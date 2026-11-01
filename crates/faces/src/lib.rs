// SPDX-License-Identifier: AGPL-3.0-or-later
//! Faces in a photo, independent of Tauri: where they are (YuNet, MIT),
//! 478 landmarks each (MediaPipe Face Mesh V2, Apache-2.0) and what every
//! pixel around them is (MediaPipe multiclass selfie segmentation,
//! Apache-2.0), and masks of their parts (`parts`) after Lightroom's People
//! masks. docs/MODELS.md records the models and their measurements.
//!
//! Input is the photo as displayed (sRGB-encoded 8-bit); a linear RAW
//! decode has to be encoded first, as the networks were trained on such
//! images.

pub mod detect;
pub mod eyes;
pub mod focus;
pub mod geometry;
pub mod landmarks;
pub mod parse;
pub mod parts;
pub mod recipe;
pub mod relight;
pub mod retouch;

use std::path::Path;

use anyhow::{Context, Result};
use image::RgbImage;
use image::imageops::{self, FilterType};
use rawtonic_onnx::{Model, ModelKind};

pub use detect::Detection;
pub use parse::ParseCrop;
pub use parts::{Part, part_mask};

/// File names in the model pack.
pub const DETECTOR_FILE: &str = "face_detection_yunet_2023mar.onnx";
pub const MESH_FILE: &str = "face_mesh_v2.onnx";
pub const PARSER_FILE: &str = "selfie_multiclass_256.onnx";

/// At most this many faces per photo, the most certain first.
pub const MAX_FACES: usize = 64;
/// Least share of face skin that makes a detection without landmarks a face.
const MIN_SKIN_SHARE: f32 = 0.3;
/// Least face-skin probability under the nose or on a cheek for landmarks
/// to be kept.
const NOSE_SKIN: f32 = 0.3;

pub struct FaceModels {
    detector: Model,
    mesh: Model,
    parser: Model,
}

impl FaceModels {
    /// Loads the three models from `dir` (the app's model folder).
    pub fn load(dir: &Path) -> Result<Self> {
        let open = |name: &str, kind: ModelKind| -> Result<Model> {
            let path = dir.join(name);
            Model::load(&path, kind).with_context(|| format!("loading {}", path.display()))
        };
        Ok(Self {
            detector: open(DETECTOR_FILE, ModelKind::FaceDetector)?,
            mesh: open(MESH_FILE, ModelKind::FaceMesh)?,
            parser: open(PARSER_FILE, ModelKind::FaceParser)?,
        })
    }

    /// The faces of `image` found by the detector alone (no landmarks or
    /// parsing): where a tool must keep away from faces (Find Dust).
    pub fn detect(&self, image: &RgbImage) -> Result<Vec<Detection>> {
        self.detector.run(|s| detect::detect(s, image))
    }
}

#[derive(Clone, Debug)]
pub struct Face {
    pub detection: Detection,
    /// 478 points in photo pixels, or `None` when Face Mesh did not find
    /// the face (mostly profiles); such faces have no eye, brow, lip or
    /// teeth parts.
    pub landmarks: Option<Vec<[f32; 2]>>,
    /// Class probabilities on a crop around the face.
    pub parse: ParseCrop,
}

impl Face {
    /// Centre of the face box.
    pub fn center(&self) -> [f32; 2] {
        let [x, y, w, h] = self.detection.bbox;
        [x + w / 2.0, y + h / 2.0]
    }
}

#[derive(Clone, Debug)]
pub struct FaceAnalysis {
    pub width: u32,
    pub height: u32,
    /// Ordered left to right ("Person 1" is the leftmost face).
    pub faces: Vec<Face>,
    /// Class probabilities on the whole photo.
    pub global: ParseCrop,
}

/// Finds the faces in `image` and parses them.
pub fn analyze(image: &RgbImage, models: &FaceModels) -> Result<FaceAnalysis> {
    let (width, height) = image.dimensions();
    let detections = models.detector.run(|s| detect::detect(s, image))?;
    let global = models
        .parser
        .run(|s| parse::parse(s, image, parse::whole_photo(width, height)))?;
    let mut faces = Vec::new();
    for detection in detections.into_iter().take(MAX_FACES) {
        let landmarks = models
            .mesh
            .run(|s| landmarks::landmarks(s, image, &detection))?;
        let angle = match &landmarks {
            Some(points) => landmarks::roll(
                points[landmarks::EYE_CORNERS[0]],
                points[landmarks::EYE_CORNERS[1]],
            ),
            None => landmarks::roll(detection.keypoints[0], detection.keypoints[1]),
        };
        let [x, y, w, h] = detection.bbox;
        let crop = geometry::Affine::crop(
            [x + w / 2.0, y + h / 2.0],
            w.max(h) * parse::FACE_CROP_SCALE,
            angle,
            parse::INPUT,
        );
        let mut parse = models.parser.run(|s| parse::parse(s, image, crop))?;
        // Landmarks belong to a face only if the parse sees face skin under
        // the nose (a turned retry can fit a face to the back of a head).
        let mut landmarks = landmarks;
        if landmarks.as_ref().is_some_and(|p| {
            [1usize, 50, 280]
                .iter()
                .map(|&i| parse.sample(parse::Class::FaceSkin, p[i][0], p[i][1]).0)
                .fold(0.0, f32::max)
                < NOSE_SKIN
        }) {
            landmarks = None;
            let angle = landmarks::roll(detection.keypoints[0], detection.keypoints[1]);
            let crop = geometry::Affine::crop(
                [x + w / 2.0, y + h / 2.0],
                w.max(h) * parse::FACE_CROP_SCALE,
                angle,
                parse::INPUT,
            );
            parse = models.parser.run(|s| parse::parse(s, image, crop))?;
        }
        // A detection is a face when Face Mesh sees one or, for profiles,
        // when the parse finds face skin in its box (YuNet alone takes a
        // round pink balloon for a face now and then).
        if landmarks.is_none() && skin_share(&parse, &detection) < MIN_SKIN_SHARE {
            continue;
        }
        faces.push(Face {
            detection,
            landmarks,
            parse,
        });
    }
    // Beside a close-up profile YuNet sometimes takes the ear for a second
    // face: a face without landmarks that overlaps a more certain face is
    // part of that head, and so is a much smaller one right behind a
    // turned face with landmarks (the ear, when the profile's box stops at
    // the cheek): on the side away from its nose. A child beside an adult
    // is kept unless it is exactly there.
    let all = faces.clone();
    faces.retain(|f| {
        let [_, _, fw, fh] = f.detection.bbox;
        let [fx, _] = f.center();
        f.landmarks.is_some()
            || !all.iter().any(|g| {
                let [gx, gy, gw, gh] = g.detection.bbox;
                let around = [gx - 0.25 * gw, gy - 0.25 * gh, 1.5 * gw, 1.5 * gh];
                let behind = g.landmarks.as_ref().is_some_and(|p| {
                    let turn = (p[1][0] - g.center()[0]) / gw;
                    turn.abs() > 0.15 && (fx - g.center()[0]) * turn < 0.0
                });
                (g.detection.score > f.detection.score
                    && detect::overlap_of_smaller(&g.detection.bbox, &f.detection.bbox) > 0.15)
                    || (behind
                        && fw * fh < 0.5 * gw * gh
                        && detect::overlap_of_smaller(&around, &f.detection.bbox) > 0.15)
            })
    });
    faces.sort_by(|a, b| a.center()[0].total_cmp(&b.center()[0]));
    Ok(FaceAnalysis {
        width,
        height,
        faces,
        global,
    })
}

/// Face skin only, for measuring the light on faces (series match): the
/// faces' boxes and their parse, without landmarks or the whole-photo
/// parse. A detection with too little face skin in its box is dropped,
/// as in [`analyze`]. At most `max` faces, the most certain first.
pub fn face_skin(
    image: &RgbImage,
    models: &FaceModels,
    max: usize,
) -> Result<Vec<(Detection, ParseCrop)>> {
    let detections = models.detector.run(|s| detect::detect(s, image))?;
    let mut faces = Vec::new();
    for detection in detections.into_iter().take(max) {
        let [x, y, w, h] = detection.bbox;
        let angle = landmarks::roll(detection.keypoints[0], detection.keypoints[1]);
        let crop = geometry::Affine::crop(
            [x + w / 2.0, y + h / 2.0],
            w.max(h) * parse::FACE_CROP_SCALE,
            angle,
            parse::INPUT,
        );
        let parse = models.parser.run(|s| parse::parse(s, image, crop))?;
        if skin_share(&parse, &detection) >= MIN_SKIN_SHARE {
            faces.push((detection, parse));
        }
    }
    Ok(faces)
}

/// Share of face skin in the middle of a detection's box (a 5 × 5 grid
/// over its central half).
fn skin_share(parse: &ParseCrop, detection: &Detection) -> f32 {
    let [x, y, w, h] = detection.bbox;
    let mut sum = 0.0;
    for j in 0..5 {
        for i in 0..5 {
            let px = x + w * (0.25 + 0.5 * i as f32 / 4.0);
            let py = y + h * (0.25 + 0.5 * j as f32 / 4.0);
            sum += parse.sample(parse::Class::FaceSkin, px, py).0;
        }
    }
    sum / 25.0
}

/// An upright square crop around the face for a list of people, `size`
/// pixels wide.
pub fn thumbnail(image: &RgbImage, face: &Face, size: u32) -> RgbImage {
    let [cx, cy] = face.center();
    let [_, _, w, h] = face.detection.bbox;
    let (iw, ih) = image.dimensions();
    let side = ((w.max(h) * 1.4) as u32).clamp(1, iw.min(ih).max(1));
    let x0 = (cx - side as f32 / 2.0).clamp(0.0, (iw - side) as f32) as u32;
    let y0 = (cy - side as f32 / 2.0).clamp(0.0, (ih - side) as f32) as u32;
    let crop = imageops::crop_imm(image, x0, y0, side, side).to_image();
    imageops::resize(&crop, size, size, FilterType::Triangle)
}
