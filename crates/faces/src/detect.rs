// SPDX-License-Identifier: AGPL-3.0-or-later
//! Face detection with YuNet (Wu, Peng and Yu 2023; OpenCV Zoo
//! `face_detection_yunet_2023mar.onnx`, MIT).
//!
//! The photo is scaled to fit 640 × 640 (top-left, black padding) and fed as
//! BGR 0..255. Each stride (8, 16, 32) predicts per grid cell a class and an
//! objectness score, a box (centre offset in cells, log size) and five
//! keypoints; the score is √(class · objectness), as OpenCV's
//! FaceDetectorYN decodes it.

use anyhow::Result;
use image::RgbImage;
use image::imageops::{self, FilterType};
use ndarray::Array4;
use ort::session::Session;
use ort::value::Tensor;

pub const INPUT: u32 = 640;
const STRIDES: [usize; 3] = [8, 16, 32];
/// Faces below this score are dropped. Faces turned 90° (portrait photos
/// without orientation) score about 0.6.
pub const THRESHOLD: f32 = 0.6;
const NMS_IOU: f32 = 0.3;

#[derive(Clone, Debug, PartialEq)]
pub struct Detection {
    pub score: f32,
    /// x, y, width, height in photo pixels.
    pub bbox: [f32; 4],
    /// Right eye, left eye, nose tip, right and left mouth corner (the
    /// subject's sides), in photo pixels.
    pub keypoints: [[f32; 2]; 5],
}

/// One stride's outputs, flattened: `cls`, `obj` one value per cell,
/// `bbox` four, `kps` ten.
pub struct StrideOutput<'a> {
    pub stride: usize,
    pub cls: &'a [f32],
    pub obj: &'a [f32],
    pub bbox: &'a [f32],
    pub kps: &'a [f32],
}

/// Detections of all strides above `threshold`, in photo pixels (`scale`:
/// network pixels per photo pixel), after non-maximum suppression.
pub fn decode(outputs: &[StrideOutput], scale: f32, threshold: f32) -> Vec<Detection> {
    suppress(candidates(outputs, scale, threshold))
}

fn candidates(outputs: &[StrideOutput], scale: f32, threshold: f32) -> Vec<Detection> {
    let mut found = Vec::new();
    for out in outputs {
        let s = out.stride as f32;
        let cols = INPUT as usize / out.stride;
        for i in 0..out.cls.len() {
            let score = (out.cls[i].clamp(0.0, 1.0) * out.obj[i].clamp(0.0, 1.0)).sqrt();
            if score < threshold {
                continue;
            }
            let (row, col) = ((i / cols) as f32, (i % cols) as f32);
            let b = &out.bbox[i * 4..i * 4 + 4];
            let (cx, cy) = ((col + b[0]) * s, (row + b[1]) * s);
            let (w, h) = (b[2].exp() * s, b[3].exp() * s);
            let k = &out.kps[i * 10..i * 10 + 10];
            let mut keypoints = [[0.0; 2]; 5];
            for (n, point) in keypoints.iter_mut().enumerate() {
                *point = [
                    (k[2 * n] + col) * s / scale,
                    (k[2 * n + 1] + row) * s / scale,
                ];
            }
            found.push(Detection {
                score,
                bbox: [
                    (cx - w / 2.0) / scale,
                    (cy - h / 2.0) / scale,
                    w / scale,
                    h / scale,
                ],
                keypoints,
            });
        }
    }
    found
}

/// Non-maximum suppression: the best of overlapping detections.
fn suppress(mut found: Vec<Detection>) -> Vec<Detection> {
    found.sort_by(|a, b| b.score.total_cmp(&a.score));
    let mut kept: Vec<Detection> = Vec::new();
    for d in found {
        if kept
            .iter()
            .all(|k| iou(&k.bbox, &d.bbox) < NMS_IOU && overlap_of_smaller(&k.bbox, &d.bbox) < 0.6)
        {
            kept.push(d);
        }
    }
    kept
}

/// Intersection over the smaller box: a face found again inside a much
/// larger box (the two passes) has a small IoU but is the same face.
pub(crate) fn overlap_of_smaller(a: &[f32; 4], b: &[f32; 4]) -> f32 {
    let iw = ((a[0] + a[2]).min(b[0] + b[2]) - a[0].max(b[0])).max(0.0);
    let ih = ((a[1] + a[3]).min(b[1] + b[3]) - a[1].max(b[1])).max(0.0);
    iw * ih / (a[2] * a[3]).min(b[2] * b[3]).max(1e-6)
}

pub fn iou(a: &[f32; 4], b: &[f32; 4]) -> f32 {
    let iw = ((a[0] + a[2]).min(b[0] + b[2]) - a[0].max(b[0])).max(0.0);
    let ih = ((a[1] + a[3]).min(b[1] + b[3]) - a[1].max(b[1])).max(0.0);
    let inter = iw * ih;
    inter / (a[2] * a[3] + b[2] * b[3] - inter)
}

/// Sizes the photo is fitted to within the network's input, as a share of
/// it: YuNet was trained on faces of about 10 to 300 pixels, so a face
/// filling a portrait is found in the smaller pass.
const PASSES: [f32; 2] = [1.0, 0.45];

/// Faces in `image` (display-encoded sRGB), best first.
pub fn detect(session: &mut Session, image: &RgbImage) -> Result<Vec<Detection>> {
    let mut found = Vec::new();
    for share in PASSES {
        found.extend(pass(session, image, share)?);
    }
    Ok(suppress(found))
}

fn pass(session: &mut Session, image: &RgbImage, share: f32) -> Result<Vec<Detection>> {
    let (w, h) = image.dimensions();
    let fit = (INPUT as f32 * share).round();
    let scale = fit / w.max(h) as f32;
    let (sw, sh) = (
        ((w as f32 * scale).round() as u32).clamp(1, INPUT),
        ((h as f32 * scale).round() as u32).clamp(1, INPUT),
    );
    let small = imageops::resize(image, sw, sh, FilterType::Triangle);
    let n = INPUT as usize;
    let mut input = Array4::<f32>::zeros((1, 3, n, n));
    for (x, y, p) in small.enumerate_pixels() {
        for c in 0..3 {
            // BGR
            input[[0, c, y as usize, x as usize]] = p[2 - c] as f32;
        }
    }
    let outputs = session.run(ort::inputs!["input" => Tensor::from_array(input)?])?;
    let mut arrays = Vec::new();
    for stride in STRIDES {
        let get = |name: &str| -> Result<Vec<f32>> {
            Ok(outputs[format!("{name}_{stride}").as_str()]
                .try_extract_array::<f32>()?
                .iter()
                .copied()
                .collect())
        };
        arrays.push((stride, get("cls")?, get("obj")?, get("bbox")?, get("kps")?));
    }
    let strides: Vec<StrideOutput> = arrays
        .iter()
        .map(|(stride, cls, obj, bbox, kps)| StrideOutput {
            stride: *stride,
            cls,
            obj,
            bbox,
            kps,
        })
        .collect();
    Ok(candidates(&strides, sw as f32 / w as f32, THRESHOLD))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_a_cell_and_suppresses_its_neighbour() {
        let cells = (INPUT as usize / 32).pow(2);
        let mut cls = vec![0.0; cells];
        let mut obj = vec![0.0; cells];
        let mut bbox = vec![0.0; cells * 4];
        let kps = vec![0.5; cells * 10];
        // Cell (row 2, col 3): a 32 px face centred at (3.5, 2.5) cells.
        let i = 2 * 20 + 3;
        cls[i] = 0.9;
        obj[i] = 0.9;
        bbox[i * 4..i * 4 + 4].copy_from_slice(&[0.5, 0.5, 0.0, 0.0]);
        // Its neighbour sees nearly the same face, a little less sure.
        let j = i + 1;
        cls[j] = 0.8;
        obj[j] = 0.8;
        bbox[j * 4..j * 4 + 4].copy_from_slice(&[-0.4, 0.5, 0.0, 0.0]);
        let out = StrideOutput {
            stride: 32,
            cls: &cls,
            obj: &obj,
            bbox: &bbox,
            kps: &kps,
        };
        // Network pixels are half the photo's.
        let found = decode(&[out], 0.5, 0.6);
        assert_eq!(found.len(), 1);
        let d = &found[0];
        assert!((d.score - 0.9).abs() < 1e-6);
        assert_eq!(
            d.bbox,
            [(112.0 - 16.0) * 2.0, (80.0 - 16.0) * 2.0, 64.0, 64.0]
        );
        assert_eq!(d.keypoints[0], [3.5 * 32.0 * 2.0, 2.5 * 32.0 * 2.0]);
    }
}
