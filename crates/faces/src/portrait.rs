// SPDX-License-Identifier: AGPL-3.0-or-later
//! Where a face's eye openings and teeth are, from EasyPortrait's FPN +
//! ResNet-50 face parser (SberDevices, CC BY-SA 4.0 variant; converted by
//! scripts/models/export_easyportrait.py and convert_easyportrait_coreml.py,
//! docs/MODELS.md). The face retouch's Iris, Sclera and Teeth use it in
//! place of Face Mesh's lid and lip outlines and the colour models
//! (notes: benchmark-results/eye3-2026-11-21, eye4-2026-11-21).
//!
//! The network sees a square around the face box (side = the box's longer
//! side / 0.7, axis-aligned, black outside the photo) at 1024 × 1024 and its
//! mirror image, averaged inside the model. Its eye probability becomes per
//! eye two outlines, at 0.1 (the eye opening; Sclera is it minus the iris
//! circle at 0.9 of its radius) and at 0.03 (Iris is the iris circle within
//! it, the circle at `IRIS_DISK` of its radius), and its teeth probability
//! one at 0.4. Those settings were fixed on
//! EYE3's tuning set. The outlines are stored in the recipe like Face Mesh's
//! (`eye_teeth`), so the render needs no model.
//!
//! The parser's part is used only where it is plausible: an eye whose
//! opening covers less than a quarter of Face Mesh's lid opening (a missed,
//! narrowed or hair-covered eye) and a mouth without teeth keep the Face
//! Mesh outlines and colour models.

use anyhow::{Result, ensure};
use image::RgbImage;
use ndarray::Array4;
use ort::value::Tensor;
use rawtonic_onnx::{Model, ModelKind};

use crate::Face;
use crate::eye_teeth::EyeTeeth;
use crate::geometry::{Affine, sample_crop};
use crate::landmarks::LIPS_OUTER;

/// File name in the model pack and its SHA-256.
pub const FILE: &str = "easyportrait_fpn_1024.onnx";
pub const SHA256: &str = "4ddf76e62492273fa25d731722e71413f20038546be51e9ab03d3419d954219c";
/// The same network as a Core ML program (macOS): the package the app
/// assembles in the model folder and its files (pack file, place in the
/// package, SHA-256).
pub const CORE_ML_PACKAGE: &str = "easyportrait_fpn.mlpackage";
pub const CORE_ML_FILES: [(&str, &str, &str); 3] = [
    (
        "easyportrait_coreml_Manifest.json",
        "Manifest.json",
        "935b1ef0c97fe51b4999919c28b9c91151c648c72571c1d5c3c49dd6e9b3b184",
    ),
    (
        "easyportrait_coreml_model.mlmodel",
        "Data/com.apple.CoreML/model.mlmodel",
        "bee5760ef6a038aa02193b93671484add681ab65cb4680cd5bc59655ddc2fd26",
    ),
    (
        "easyportrait_coreml_weight.bin",
        "Data/com.apple.CoreML/weights/weight.bin",
        "d0d2e88f41652514494c0fd273666b26add3bc91e72ddf26187ff071e11427b9",
    ),
];

/// Where Core ML runs it (as the sky and subject models).
#[cfg(target_os = "macos")]
pub const CORE_ML_UNITS: rawtonic_denoise::coreml::ComputeUnits =
    rawtonic_denoise::coreml::ComputeUnits::CpuAndGpu;

/// Side of the network's input and output.
pub const SIZE: u32 = 1024;
/// The face box's longer side over the crop's side.
const CROP_FRACTION: f32 = 0.7;
/// Probability thresholds (EYE3, fixed on its tuning set).
const EYE: f32 = 0.1;
const IRIS_EYE: f32 = 0.03;
const TEETH: f32 = 0.4;
/// Least share of Face Mesh's lid opening the parser's eye must cover.
const MIN_EYE_SHARE: f32 = 0.25;
/// Least teeth area, network pixels.
const MIN_TEETH: usize = 16;
/// How far around an eye's lid outline (in eye widths) and a mouth's outer
/// lip outline (in mouth widths) the parser's part is looked for.
const EYE_REACH: f32 = 0.4;
const MOUTH_REACH: f32 = 0.25;
/// Outline simplification tolerance and the smallest loop kept, network
/// pixels.
const TOLERANCE: f32 = 0.2;
const MIN_LOOP_AREA: f32 = 0.5;

/// The Iris mask's disk on the parser's eyes, as a share of Face Mesh's
/// iris radius (stored per face as `irisDisk`). Face Mesh's iris circle sits
/// beside the real iris (a median 0.2 of its radius on EYE2's portraits, up
/// to 0.75) and is often a little large, so
/// the full disk lifted the eye white beside the iris; at 0.9 the Iris halo
/// fell below Capture One's on EYE2's tuning portraits with its lightening
/// kept (the smallest of 1, 0.9, 0.85, 0.8 that does) and held on the
/// hold-out (notes: benchmark-results/eye8-2026-11-22).
pub const IRIS_DISK: f32 = 0.9;

/// Closed outlines, photo pixels; a point is inside when an odd number of
/// them surround it (holes are loops inside loops).
pub type Loops = Vec<Vec<[f32; 2]>>;

/// The parser's outlines of one eye.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ParsedEye {
    /// The eye opening (probability above 0.1).
    pub eye: Loops,
    /// The wider opening the iris is taken within (above 0.03).
    pub iris: Loops,
}

/// The parser's outlines of one face, in `EyeTeeth`'s eye order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Parsed {
    /// `None` where the Face Mesh outlines stay (no eye there, or the
    /// parser's eye is implausible).
    pub eyes: Vec<Option<ParsedEye>>,
    /// Empty where the Face Mesh outline stays.
    pub teeth: Loops,
}

enum Backend {
    Onnx(Model),
    #[cfg(target_os = "macos")]
    CoreMl(rawtonic_denoise::coreml::CoreMlModel),
}

/// The eye and teeth parser.
pub struct PortraitModel {
    backend: Backend,
}

impl PortraitModel {
    /// The ONNX model at `path` (ONNX Runtime, `rawtonic-onnx`).
    pub fn onnx(path: &std::path::Path) -> Result<Self> {
        Ok(Self {
            backend: Backend::Onnx(Model::load(path, ModelKind::EyesTeeth)?),
        })
    }

    /// The Core ML program (`CORE_ML_PACKAGE`).
    #[cfg(target_os = "macos")]
    pub fn core_ml(model: rawtonic_denoise::coreml::CoreMlModel) -> Self {
        Self {
            backend: Backend::CoreMl(model),
        }
    }

    /// The model of a model folder the app filled (the CLI's `--models`):
    /// on macOS the Core ML program once the app has assembled it, else the
    /// ONNX model. Nothing is downloaded here.
    pub fn from_dir(dir: &std::path::Path) -> Result<Self> {
        let verified = |path: &std::path::Path, sha: &str| {
            std::fs::File::open(path)
                .ok()
                .and_then(|mut file| rawtonic_fs::sha256_hex_read(&mut file).ok())
                .is_some_and(|hash| hash == sha)
        };
        #[cfg(target_os = "macos")]
        if CORE_ML_FILES
            .iter()
            .all(|&(file, _, sha)| verified(&dir.join(file), sha))
        {
            let package = dir.join(CORE_ML_PACKAGE);
            anyhow::ensure!(
                CORE_ML_FILES
                    .iter()
                    .all(|&(_, inner, sha)| verified(&package.join(inner), sha)),
                "the Core ML program {} is not assembled yet: use Eyes or Teeth in the app once",
                package.display()
            );
            return Ok(Self::core_ml(rawtonic_denoise::coreml::CoreMlModel::load(
                &package,
                CORE_ML_UNITS,
            )?));
        }
        let path = dir.join(FILE);
        anyhow::ensure!(
            verified(&path, SHA256),
            "the eye and teeth model is not installed in {} (use Eyes or Teeth in the app once, or run scripts/models/install_models.sh)",
            dir.display()
        );
        Self::onnx(&path)
    }

    pub fn backend_name(&self) -> &'static str {
        match &self.backend {
            Backend::Onnx(_) => "ONNX Runtime",
            #[cfg(target_os = "macos")]
            Backend::CoreMl(_) => "Core ML",
        }
    }

    /// Eye and teeth probabilities (`SIZE`² each) of a `SIZE`² RGB crop,
    /// values 0..255.
    fn run(&self, crop: &[[f32; 3]]) -> Result<Vec<f32>> {
        let n = SIZE as usize;
        let mut input = vec![0.0f32; 3 * n * n];
        for (i, p) in crop.iter().enumerate() {
            for c in 0..3 {
                input[c * n * n + i] = p[c];
            }
        }
        let values = match &self.backend {
            Backend::Onnx(model) => model.run(|session| {
                let input = Array4::from_shape_vec((1, 3, n, n), input.clone())?;
                let outputs = session.run(ort::inputs!["image" => Tensor::from_array(input)?])?;
                Ok(outputs["parts"]
                    .try_extract_array::<f32>()?
                    .iter()
                    .copied()
                    .collect::<Vec<f32>>())
            })?,
            #[cfg(target_os = "macos")]
            Backend::CoreMl(model) => model.predict("image", &[1, 3, n, n], &input, "parts")?.1,
        };
        ensure!(
            values.len() == 2 * n * n,
            "eye and teeth model: {} values",
            values.len()
        );
        rawtonic_onnx::ensure_finite(&values)?;
        Ok(values)
    }
}

/// The crop the network sees of `face`: crop pixels (u, v) to photo pixels.
pub fn crop_of(face: &Face) -> Affine {
    let [x, y, w, h] = face.detection.bbox;
    let side = (w.max(h) / CROP_FRACTION).round();
    let (x0, y0) = (
        (x + w / 2.0 - side / 2.0).round(),
        (y + h / 2.0 - side / 2.0).round(),
    );
    let k = side / SIZE as f32;
    Affine {
        a: k,
        b: 0.0,
        c: x0,
        d: 0.0,
        e: k,
        f: y0,
    }
}

/// A window of the network's output: samples `x0..x1` × `y0..y1`.
struct Window<'a> {
    values: &'a [f32],
    x0: usize,
    y0: usize,
    x1: usize,
    y1: usize,
}

impl<'a> Window<'a> {
    /// The samples within `reach` network pixels of the box of `points`
    /// (crop pixels).
    fn around(values: &'a [f32], points: &[[f32; 2]], reach: f32) -> Option<Self> {
        let n = SIZE as f32;
        let (mut lo, mut hi) = ([f32::INFINITY; 2], [f32::NEG_INFINITY; 2]);
        for p in points {
            for k in 0..2 {
                lo[k] = lo[k].min(p[k]);
                hi[k] = hi[k].max(p[k]);
            }
        }
        let clip = |v: f32| v.clamp(0.0, n) as usize;
        let (x0, y0) = (clip((lo[0] - reach).floor()), clip((lo[1] - reach).floor()));
        let (x1, y1) = (clip((hi[0] + reach).ceil()), clip((hi[1] + reach).ceil()));
        (x1 > x0 && y1 > y0).then_some(Self {
            values,
            x0,
            y0,
            x1,
            y1,
        })
    }

    fn get(&self, i: i64, j: i64) -> f32 {
        let (x, y) = (self.x0 as i64 + i, self.y0 as i64 + j);
        if i < 0 || j < 0 || x >= self.x1 as i64 || y >= self.y1 as i64 {
            return 0.0;
        }
        self.values[y as usize * SIZE as usize + x as usize]
    }

    fn count_above(&self, t: f32) -> usize {
        (self.y0..self.y1)
            .map(|y| {
                self.values[y * SIZE as usize + self.x0..y * SIZE as usize + self.x1]
                    .iter()
                    .filter(|&&v| v > t)
                    .count()
            })
            .sum()
    }

    /// The outlines of `value > t` (marching squares; samples sit at pixel
    /// centres, outside the window counts as 0), crop pixels.
    fn contours(&self, t: f32) -> Vec<Vec<[f32; 2]>> {
        let (w, h) = ((self.x1 - self.x0) as i64, (self.y1 - self.y0) as i64);
        // Edges: (vertical?, i, j); horizontal joins sample (i, j) and
        // (i + 1, j), vertical (i, j) and (i, j + 1).
        type Edge = (bool, i64, i64);
        let mut segments: Vec<(Edge, Edge)> = Vec::new();
        for j in -1..h {
            for i in -1..w {
                let (a, b, c, d) = (
                    self.get(i, j) > t,
                    self.get(i + 1, j) > t,
                    self.get(i + 1, j + 1) > t,
                    self.get(i, j + 1) > t,
                );
                let (top, right, bottom, left) = (
                    (false, i, j),
                    (true, i + 1, j),
                    (false, i, j + 1),
                    (true, i, j),
                );
                let case = a as u8 | (b as u8) << 1 | (c as u8) << 2 | (d as u8) << 3;
                let centre = || {
                    (self.get(i, j)
                        + self.get(i + 1, j)
                        + self.get(i + 1, j + 1)
                        + self.get(i, j + 1))
                        / 4.0
                        > t
                };
                match case {
                    0 | 15 => {}
                    5 | 10 => {
                        // Saddle: the centre decides which corners join.
                        if (case == 5) == centre() {
                            segments.push((top, right));
                            segments.push((bottom, left));
                        } else {
                            segments.push((left, top));
                            segments.push((right, bottom));
                        }
                    }
                    _ => {
                        let crossing = [
                            (top, a != b),
                            (right, b != c),
                            (bottom, d != c),
                            (left, a != d),
                        ];
                        let mut edges = crossing.iter().filter(|(_, x)| *x).map(|(e, _)| *e);
                        if let (Some(p), Some(q)) = (edges.next(), edges.next()) {
                            segments.push((p, q));
                        }
                    }
                }
            }
        }
        let point = |(vertical, i, j): Edge| -> [f32; 2] {
            let (p, q) = if vertical {
                (self.get(i, j), self.get(i, j + 1))
            } else {
                (self.get(i, j), self.get(i + 1, j))
            };
            let f = if q != p {
                ((t - p) / (q - p)).clamp(0.0, 1.0)
            } else {
                0.5
            };
            let (x, y) = (
                (self.x0 as i64 + i) as f32 + 0.5,
                (self.y0 as i64 + j) as f32 + 0.5,
            );
            if vertical { [x, y + f] } else { [x + f, y] }
        };
        // Every crossed edge joins exactly two segments: walk them into loops.
        let mut at: std::collections::HashMap<Edge, Vec<usize>> = Default::default();
        for (k, &(p, q)) in segments.iter().enumerate() {
            at.entry(p).or_default().push(k);
            at.entry(q).or_default().push(k);
        }
        let mut used = vec![false; segments.len()];
        let mut loops = Vec::new();
        for start in 0..segments.len() {
            if used[start] {
                continue;
            }
            used[start] = true;
            let (first, mut edge) = segments[start];
            let mut points = vec![point(first)];
            while edge != first {
                points.push(point(edge));
                let Some(&next) = at[&edge].iter().find(|&&k| !used[k]) else {
                    break;
                };
                used[next] = true;
                let (p, q) = segments[next];
                edge = if p == edge { q } else { p };
            }
            let points = simplify(&points, TOLERANCE);
            if points.len() >= 3 && area(&points).abs() >= MIN_LOOP_AREA {
                loops.push(points);
            }
        }
        loops
    }
}

/// Signed area of a closed outline.
fn area(points: &[[f32; 2]]) -> f32 {
    let n = points.len();
    (0..n)
        .map(|i| {
            let (p, q) = (points[i], points[(i + 1) % n]);
            p[0] * q[1] - q[0] * p[1]
        })
        .sum::<f32>()
        / 2.0
}

/// Douglas-Peucker on a closed outline: points that move it by less than
/// `tolerance` dropped.
fn simplify(points: &[[f32; 2]], tolerance: f32) -> Vec<[f32; 2]> {
    let n = points.len();
    if n < 4 {
        return points.to_vec();
    }
    // Split at the point farthest from the first, then simplify both halves.
    let far = (1..n)
        .max_by(|&a, &b| {
            let d = |k: usize| (points[k][0] - points[0][0]).hypot(points[k][1] - points[0][1]);
            d(a).total_cmp(&d(b))
        })
        .unwrap_or(n / 2);
    let mut keep = vec![false; n];
    keep[0] = true;
    keep[far] = true;
    let mut stack = vec![(0, far), (far, n)];
    while let Some((s, e)) = stack.pop() {
        if e - s < 2 {
            continue;
        }
        let (p, q) = (points[s], points[e % n]);
        let (dx, dy) = (q[0] - p[0], q[1] - p[1]);
        let len = dx.hypot(dy);
        let (mut best, mut at) = (0.0f32, s);
        for (k, r) in points.iter().enumerate().take(e).skip(s + 1) {
            let d = if len > 0.0 {
                ((r[0] - p[0]) * dy - (r[1] - p[1]) * dx).abs() / len
            } else {
                (r[0] - p[0]).hypot(r[1] - p[1])
            };
            if d > best {
                best = d;
                at = k;
            }
        }
        if best > tolerance {
            keep[at] = true;
            stack.push((s, at));
            stack.push((at, e));
        }
    }
    points
        .iter()
        .zip(keep)
        .filter_map(|(p, k)| k.then_some(*p))
        .collect()
}

fn polygon_area(points: &[[f32; 2]]) -> f32 {
    area(points).abs()
}

/// The parser's outlines of `face` in `image` (the photo the analysis was
/// made of), for its eyes and mouth as `features` has them.
pub fn parse(
    model: &PortraitModel,
    image: &RgbImage,
    face: &Face,
    features: &EyeTeeth,
) -> Result<Parsed> {
    let Some(points) = face.landmarks.as_deref() else {
        return Ok(Parsed::default());
    };
    let to_photo = crop_of(face);
    let to_crop = to_photo.invert();
    let values = model.run(&sample_crop(image, &to_photo, SIZE))?;
    let n2 = (SIZE * SIZE) as usize;
    let (eye_p, teeth_p) = values.split_at(n2);
    let crop_points =
        |p: &[[f32; 2]]| -> Vec<[f32; 2]> { p.iter().map(|q| to_crop.apply(q[0], q[1])).collect() };
    let photo_loops = |loops: Vec<Vec<[f32; 2]>>| -> Loops {
        loops
            .into_iter()
            .map(|l| l.into_iter().map(|q| to_photo.apply(q[0], q[1])).collect())
            .collect()
    };
    let eyes = features
        .eyes
        .iter()
        .map(|eye| {
            let eye = eye.as_ref()?;
            let lid = crop_points(&eye.lid);
            let width = lid
                .iter()
                .flat_map(|a| lid.iter().map(move |b| (a[0] - b[0]).hypot(a[1] - b[1])))
                .fold(0.0f32, f32::max);
            let window = Window::around(eye_p, &lid, EYE_REACH * width)?;
            let opening = window.count_above(EYE) as f32;
            (opening >= MIN_EYE_SHARE * polygon_area(&lid)).then(|| ParsedEye {
                eye: photo_loops(window.contours(EYE)),
                iris: photo_loops(window.contours(IRIS_EYE)),
            })
        })
        .collect();
    let lips = crop_points(&LIPS_OUTER.map(|i| points[i]));
    let mouth_width = lips
        .iter()
        .flat_map(|a| lips.iter().map(move |b| (a[0] - b[0]).hypot(a[1] - b[1])))
        .fold(0.0f32, f32::max);
    let teeth = Window::around(teeth_p, &lips, MOUTH_REACH * mouth_width)
        .filter(|w| w.count_above(TEETH) >= MIN_TEETH)
        .map(|w| photo_loops(w.contours(TEETH)))
        .unwrap_or_default();
    Ok(Parsed { eyes, teeth })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window_of(values: &[f32]) -> Window<'_> {
        Window {
            values,
            x0: 0,
            y0: 0,
            x1: SIZE as usize,
            y1: SIZE as usize,
        }
    }

    fn disk(cx: f32, cy: f32, r: f32) -> Vec<f32> {
        (0..SIZE * SIZE)
            .map(|i| {
                let (x, y) = ((i % SIZE) as f32 + 0.5, (i / SIZE) as f32 + 0.5);
                let d = (x - cx).hypot(y - cy);
                (1.0 - (d - r) / 2.0).clamp(0.0, 1.0)
            })
            .collect()
    }

    #[test]
    fn a_disk_gives_one_round_loop() {
        let values = disk(100.0, 120.0, 30.0);
        let window = Window::around(&values, &[[100.0, 120.0]], 50.0).unwrap();
        let loops = window.contours(0.5);
        assert_eq!(loops.len(), 1);
        for p in &loops[0] {
            let d = (p[0] - 100.0).hypot(p[1] - 120.0);
            assert!((d - 31.0).abs() < 0.3, "{p:?} at {d}");
        }
        let a = area(&loops[0]).abs();
        let expected = std::f32::consts::PI * 31.0 * 31.0;
        assert!((a - expected).abs() / expected < 0.01, "{a} vs {expected}");
    }

    #[test]
    fn a_ring_gives_two_loops() {
        let outer = disk(200.0, 200.0, 40.0);
        let inner = disk(200.0, 200.0, 15.0);
        let ring: Vec<f32> = outer.iter().zip(&inner).map(|(o, i)| o - i).collect();
        let loops = window_of(&ring).contours(0.5);
        assert_eq!(loops.len(), 2);
    }

    #[test]
    fn parts_at_the_window_edge_close() {
        let values = vec![1.0f32; (SIZE * SIZE) as usize];
        let window = Window::around(&values, &[[10.0, 10.0], [30.0, 20.0]], 0.0).unwrap();
        let loops = window.contours(0.5);
        assert_eq!(loops.len(), 1);
        assert_eq!(window.count_above(0.5), 20 * 10);
    }

    #[test]
    fn simplify_keeps_corners() {
        let mut square = Vec::new();
        for k in 0..10 {
            square.push([k as f32, 0.0]);
        }
        for k in 0..10 {
            square.push([10.0, k as f32]);
        }
        for k in 0..10 {
            square.push([10.0 - k as f32, 10.0]);
        }
        for k in 0..10 {
            square.push([0.0, 10.0 - k as f32]);
        }
        let s = simplify(&square, 0.2);
        assert_eq!(s.len(), 4, "{s:?}");
        assert!((area(&s).abs() - 100.0).abs() < 1e-3);
    }

    #[test]
    fn the_crop_is_square_around_the_box() {
        let face = Face {
            detection: crate::Detection {
                score: 1.0,
                bbox: [100.0, 50.0, 140.0, 120.0],
                keypoints: [[0.0; 2]; 5],
            },
            landmarks: None,
            parse: crate::ParseCrop::new(
                crate::parse::whole_photo(10, 10),
                vec![
                    0;
                    (crate::parse::INPUT * crate::parse::INPUT) as usize * crate::parse::CLASSES
                ],
            ),
        };
        let c = crop_of(&face);
        // Side 140 / 0.7 = 200 around the box centre (170, 110).
        assert_eq!(c.apply(0.0, 0.0), [70.0, 10.0]);
        assert_eq!(c.apply(SIZE as f32, SIZE as f32), [270.0, 210.0]);
    }
}
