// SPDX-License-Identifier: AGPL-3.0-or-later
//! What each pixel of a person is, with MediaPipe's multiclass selfie
//! segmentation (Google, Apache-2.0), converted by
//! scripts/models/export_faces.py: background, hair, body skin, face skin,
//! clothes and accessories from a 256 × 256 RGB crop scaled to -1..1.
//!
//! The network was trained on selfies, so it runs once on the whole photo
//! (for bodies and clothes) and once per face on a crop about three times
//! the face's size, where the face fills the frame as in a selfie and the
//! skin and hair edges are resolved far more finely.

use anyhow::Result;
use image::RgbImage;
use ndarray::Array4;
use ort::session::Session;
use ort::value::Tensor;

use crate::geometry::{Affine, sample_crop};

pub const INPUT: u32 = 256;
pub const CLASSES: usize = 6;
/// Crop size around a face, relative to the face box's longer side.
pub const FACE_CROP_SCALE: f32 = 3.3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    Background = 0,
    Hair = 1,
    BodySkin = 2,
    FaceSkin = 3,
    Clothes = 4,
    Accessories = 5,
}

/// Class probabilities on a crop of the photo.
#[derive(Clone, Debug)]
pub struct ParseCrop {
    pub to_photo: Affine,
    to_crop: Affine,
    /// `INPUT²` pixels × `CLASSES` probabilities, 0..255.
    probs: Vec<u8>,
}

impl ParseCrop {
    pub fn new(to_photo: Affine, probs: Vec<u8>) -> Self {
        assert_eq!(probs.len(), (INPUT * INPUT) as usize * CLASSES);
        Self {
            to_photo,
            to_crop: to_photo.invert(),
            probs,
        }
    }

    /// The probability of `class` at photo point (x, y), bilinear, and how
    /// much this crop should count there: 1 inside, falling to 0 over the
    /// outer eighth towards its border, 0 outside.
    pub fn sample(&self, class: Class, x: f32, y: f32) -> (f32, f32) {
        let [u, v] = self.to_crop.apply(x, y);
        let n = INPUT as f32;
        if !(0.0..n).contains(&u) || !(0.0..n).contains(&v) {
            return (0.0, 0.0);
        }
        let edge = u.min(v).min(n - u).min(n - v);
        let weight = (edge / (n / 8.0)).min(1.0);
        let (u, v) = ((u - 0.5).clamp(0.0, n - 1.0), (v - 0.5).clamp(0.0, n - 1.0));
        let (u0, v0) = (u.floor() as usize, v.floor() as usize);
        let (u1, v1) = (
            (u0 + 1).min(INPUT as usize - 1),
            (v0 + 1).min(INPUT as usize - 1),
        );
        let (fu, fv) = (u - u0 as f32, v - v0 as f32);
        let at = |uu: usize, vv: usize| {
            self.probs[(vv * INPUT as usize + uu) * CLASSES + class as usize] as f32 / 255.0
        };
        let top = at(u0, v0) + (at(u1, v0) - at(u0, v0)) * fu;
        let bottom = at(u0, v1) + (at(u1, v1) - at(u0, v1)) * fu;
        (top + (bottom - top) * fv, weight)
    }

    /// Photo-space bounding box of the crop (x0, y0, x1, y1).
    pub fn bounds(&self) -> [f32; 4] {
        let n = INPUT as f32;
        let corners = [
            self.to_photo.apply(0.0, 0.0),
            self.to_photo.apply(n, 0.0),
            self.to_photo.apply(0.0, n),
            self.to_photo.apply(n, n),
        ];
        let mut b = [
            f32::INFINITY,
            f32::INFINITY,
            f32::NEG_INFINITY,
            f32::NEG_INFINITY,
        ];
        for c in corners {
            b[0] = b[0].min(c[0]);
            b[1] = b[1].min(c[1]);
            b[2] = b[2].max(c[0]);
            b[3] = b[3].max(c[1]);
        }
        b
    }
}

/// Softmax over the classes of every pixel, as 0..255.
pub fn softmax_u8(logits: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(logits.len());
    for px in logits.as_chunks::<CLASSES>().0 {
        let max = px.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        let exps: Vec<f32> = px.iter().map(|v| (v - max).exp()).collect();
        let sum: f32 = exps.iter().sum();
        out.extend(exps.iter().map(|e| (e / sum * 255.0).round() as u8));
    }
    out
}

/// Per-pixel class probabilities (`softmax_u8`); non-finite logits (a GPU
/// fault) are an error, so `Model::run` repeats the run on the CPU instead
/// of returning empty masks.
fn probabilities(logits: &[f32]) -> Result<Vec<u8>> {
    rawtonic_onnx::ensure_finite(logits)?;
    Ok(softmax_u8(logits))
}

/// Runs the network on the crop `to_photo` describes.
pub fn parse(session: &mut Session, image: &RgbImage, to_photo: Affine) -> Result<ParseCrop> {
    let crop = sample_crop(image, &to_photo, INPUT);
    let n = INPUT as usize;
    let mut input = Array4::<f32>::zeros((1, n, n, 3));
    for (i, p) in crop.iter().enumerate() {
        for c in 0..3 {
            input[[0, i / n, i % n, c]] = p[c] / 127.5 - 1.0;
        }
    }
    let outputs = session.run(ort::inputs!["image" => Tensor::from_array(input)?])?;
    let logits: Vec<f32> = outputs["classes"]
        .try_extract_array::<f32>()?
        .iter()
        .copied()
        .collect();
    Ok(ParseCrop::new(to_photo, probabilities(&logits)?))
}

/// The whole photo, centred in a square crop.
pub fn whole_photo(width: u32, height: u32) -> Affine {
    Affine::crop(
        [width as f32 / 2.0, height as f32 / 2.0],
        width.max(height) as f32,
        0.0,
        INPUT,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_finite_logits_are_an_error_for_the_cpu_fallback() {
        let mut logits = vec![0.5; CLASSES * 4];
        assert_eq!(probabilities(&logits).unwrap().len(), CLASSES * 4);
        logits[7] = f32::NAN;
        assert!(probabilities(&logits).is_err());
        logits[7] = f32::INFINITY;
        assert!(probabilities(&logits).is_err());
    }

    #[test]
    fn softmax_sums_to_one() {
        let p = softmax_u8(&[0.0, 0.0, 0.0, 10.0, 0.0, 0.0]);
        assert_eq!(p[3], 255);
        assert!(p.iter().map(|&v| v as u32).sum::<u32>() >= 254);
    }

    #[test]
    fn samples_a_class_and_fades_at_the_border() {
        let n = (INPUT * INPUT) as usize;
        let mut probs = vec![0u8; n * CLASSES];
        for px in probs.as_chunks_mut::<CLASSES>().0 {
            px[Class::FaceSkin as usize] = 255;
        }
        let crop = ParseCrop::new(whole_photo(512, 512), probs);
        let (p, w) = crop.sample(Class::FaceSkin, 256.0, 256.0);
        assert!((p - 1.0).abs() < 1e-6 && w == 1.0);
        let (_, w) = crop.sample(Class::FaceSkin, 8.0, 256.0);
        assert!(w > 0.0 && w < 0.5);
        assert_eq!(crop.sample(Class::FaceSkin, -1.0, 256.0), (0.0, 0.0));
        assert_eq!(crop.bounds(), [0.0, 0.0, 512.0, 512.0]);
    }
}
