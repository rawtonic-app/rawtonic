// SPDX-License-Identifier: AGPL-3.0-or-later
//! 478 face landmarks with MediaPipe Face Mesh V2 (Google, Apache-2.0),
//! converted by scripts/models/export_faces.py.
//!
//! As MediaPipe's face landmarker does, the network sees a 256 × 256 crop
//! rotated so the eyes are level, 1.5 times the face's size, RGB 0..1. A
//! second pass recentres the crop on the first pass's landmarks. Profile
//! views mostly fail the presence test; those faces keep no landmarks.

use anyhow::Result;
use image::RgbImage;
use ndarray::Array4;
use ort::session::Session;
use ort::value::Tensor;

use crate::detect::Detection;
use crate::geometry::{Affine, sample_crop};

pub const INPUT: u32 = 256;
pub const COUNT: usize = 478;
/// Crop size relative to the face.
const CROP_SCALE: f32 = 1.5;
/// Minimum presence probability for landmarks to count.
const PRESENCE: f32 = 0.5;
/// The same for crops turned against the keypoints' roll: a turned crop of
/// the back of a child's head scored 0.79, tilted faces 0.83–0.97.
const TURNED_PRESENCE: f32 = 0.85;

// Outlines (indices into the 478 landmarks), from MediaPipe's
// face_mesh_connections. "Right"/"left" are the subject's.
pub const FACE_OVAL: [usize; 36] = [
    10, 338, 297, 332, 284, 251, 389, 356, 454, 323, 361, 288, 397, 365, 379, 378, 400, 377, 152,
    148, 176, 149, 150, 136, 172, 58, 132, 93, 234, 127, 162, 21, 54, 103, 67, 109,
];
pub const LIPS_OUTER: [usize; 20] = [
    61, 146, 91, 181, 84, 17, 314, 405, 321, 375, 291, 409, 270, 269, 267, 0, 37, 39, 40, 185,
];
pub const LIPS_INNER: [usize; 20] = [
    78, 95, 88, 178, 87, 14, 317, 402, 318, 324, 308, 415, 310, 311, 312, 13, 82, 81, 80, 191,
];
pub const LEFT_EYE: [usize; 16] = [
    263, 249, 390, 373, 374, 380, 381, 382, 362, 398, 384, 385, 386, 387, 388, 466,
];
pub const RIGHT_EYE: [usize; 16] = [
    33, 7, 163, 144, 145, 153, 154, 155, 133, 173, 157, 158, 159, 160, 161, 246,
];
pub const LEFT_BROW: [usize; 10] = [336, 296, 334, 293, 300, 276, 283, 282, 295, 285];
pub const RIGHT_BROW: [usize; 10] = [70, 63, 105, 66, 107, 55, 65, 52, 53, 46];
/// Iris centre, then four points on its rim.
pub const LEFT_IRIS: [usize; 5] = [473, 474, 475, 476, 477];
pub const RIGHT_IRIS: [usize; 5] = [468, 469, 470, 471, 472];
/// Outer eye corners (right, left), for the face's roll.
pub const EYE_CORNERS: [usize; 2] = [33, 263];

/// One pass: landmarks in photo pixels and the presence probability.
fn run(session: &mut Session, image: &RgbImage, to_photo: &Affine) -> Result<(Vec<[f32; 2]>, f32)> {
    let crop = sample_crop(image, to_photo, INPUT);
    let n = INPUT as usize;
    let mut input = Array4::<f32>::zeros((1, n, n, 3));
    for (i, p) in crop.iter().enumerate() {
        for c in 0..3 {
            input[[0, i / n, i % n, c]] = p[c] / 255.0;
        }
    }
    let outputs = session.run(ort::inputs!["image" => Tensor::from_array(input)?])?;
    let raw: Vec<f32> = outputs["landmarks"]
        .try_extract_array::<f32>()?
        .iter()
        .copied()
        .collect();
    let logit = outputs["presence"]
        .try_extract_array::<f32>()?
        .iter()
        .copied()
        .next()
        .unwrap_or(f32::NEG_INFINITY);
    let points = raw
        .as_chunks::<3>()
        .0
        .iter()
        .take(COUNT)
        .map(|p| to_photo.apply(p[0], p[1]))
        .collect();
    Ok((points, 1.0 / (1.0 + (-logit).exp())))
}

/// The crop around a set of points: centred on their bounding box in the
/// frame rotated by `angle`, `CROP_SCALE` times its longer side.
fn crop_around(points: &[[f32; 2]], angle: f32) -> Affine {
    let (s, c) = angle.sin_cos();
    let rotate = |p: &[f32; 2]| [p[0] * c + p[1] * s, -p[0] * s + p[1] * c];
    let mut min = [f32::INFINITY; 2];
    let mut max = [f32::NEG_INFINITY; 2];
    for p in points {
        let r = rotate(p);
        for k in 0..2 {
            min[k] = min[k].min(r[k]);
            max[k] = max[k].max(r[k]);
        }
    }
    let mid = [(min[0] + max[0]) / 2.0, (min[1] + max[1]) / 2.0];
    // Back from the rotated frame.
    let center = [mid[0] * c - mid[1] * s, mid[0] * s + mid[1] * c];
    let size = (max[0] - min[0]).max(max[1] - min[1]) * CROP_SCALE;
    Affine::crop(center, size, angle, INPUT)
}

/// Roll of the line from the subject's right eye to their left eye.
pub fn roll(right_eye: [f32; 2], left_eye: [f32; 2]) -> f32 {
    (left_eye[1] - right_eye[1]).atan2(left_eye[0] - right_eye[0])
}

/// The face's landmarks in photo pixels, or `None` when the network finds
/// no face in the crop (mostly profiles).
pub fn landmarks(
    session: &mut Session,
    image: &RgbImage,
    face: &Detection,
) -> Result<Option<Vec<[f32; 2]>>> {
    let angle = roll(face.keypoints[0], face.keypoints[1]);
    let [x, y, w, h] = face.bbox;
    let k = &face.keypoints;
    let centroid = [
        k.iter().map(|p| p[0]).sum::<f32>() / 5.0,
        k.iter().map(|p| p[1]).sum::<f32>() / 5.0,
    ];
    // MediaPipe's crop from the detection box first; faces looking down or
    // away sit off its centre (the box includes the top of the head), so
    // crops centred on the eyes, nose and mouth follow, a little larger.
    // Tilted heads (a child's head on one side) and faces looking down
    // far, whose keypoints give a poor roll, are found in crops turned by
    // 25° or 45° either way; analyze() keeps such landmarks only where the
    // parse sees face skin under the nose.
    let box_centre = [x + w / 2.0, y + h / 2.0];
    let mut crops = vec![
        (box_centre, CROP_SCALE, 0.0f32),
        (centroid, CROP_SCALE, 0.0),
        (centroid, CROP_SCALE * 1.25, 0.0),
    ];
    for turn in [-25.0f32, 25.0, -45.0, 45.0] {
        for centre in [box_centre, centroid] {
            crops.push((centre, CROP_SCALE, turn.to_radians()));
        }
    }
    let mut found = None;
    for (centre, scale, turn) in crops {
        let crop = Affine::crop(centre, w.max(h) * scale, angle + turn, INPUT);
        let (points, presence) = run(session, image, &crop)?;
        if presence
            >= if turn == 0.0 {
                PRESENCE
            } else {
                TURNED_PRESENCE
            }
        {
            found = Some(points);
            break;
        }
    }
    let Some(points) = found else {
        return Ok(None);
    };
    let angle = roll(points[EYE_CORNERS[0]], points[EYE_CORNERS[1]]);
    let second = crop_around(&points, angle);
    let (refined, presence) = run(session, image, &second)?;
    Ok((presence >= PRESENCE).then_some(refined).or(Some(points)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crop_around_points_is_centred_and_scaled() {
        let points = [[10.0, 20.0], [30.0, 20.0], [20.0, 60.0]];
        let t = crop_around(&points, 0.0);
        let centre = t.apply(128.0, 128.0);
        assert!((centre[0] - 20.0).abs() < 1e-3 && (centre[1] - 40.0).abs() < 1e-3);
        assert!((t.scale() * INPUT as f32 - 60.0).abs() < 1e-3);
        // Rotated by 90°, the same points span 40 along the crop's x axis.
        let t = crop_around(&points, std::f32::consts::FRAC_PI_2);
        let centre = t.apply(128.0, 128.0);
        assert!((centre[0] - 20.0).abs() < 1e-3 && (centre[1] - 40.0).abs() < 1e-3);
    }

    #[test]
    fn roll_follows_the_eye_line() {
        assert_eq!(roll([0.0, 0.0], [10.0, 0.0]), 0.0);
        assert!((roll([0.0, 0.0], [0.0, 10.0]) - std::f32::consts::FRAC_PI_2).abs() < 1e-6);
    }
}
