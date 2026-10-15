// SPDX-License-Identifier: AGPL-3.0-or-later
//! Persistable facial relief, inferred once. Coordinates use the displayed
//! frame: x/width, y/height, and camera-facing z/short side. MediaPipe z has
//! the same scale as crop x, negative towards the camera; it is not metres.
use crate::{FaceModels, detect, geometry, landmarks, parse};
use anyhow::Result;
use image::RgbImage;

pub fn analyze(image: &RgbImage, models: &FaceModels) -> Result<Vec<Vec<[f32; 4]>>> {
    let detections = models.detector.run(|s| detect::detect(s, image))?;
    let mut faces = Vec::new();
    let short = image.width().min(image.height()) as f32;
    // Tiny faces cannot provide a dependable shading surface. Keep the cost
    // bounded in group photographs, retaining the most certain detections.
    for detection in detections.into_iter().take(8) {
        let [x, y, w, h] = detection.bbox;
        if w.min(h) < 48. {
            continue;
        }
        let Some(points) = models
            .mesh
            .run(|s| landmarks::landmarks3d(s, image, &detection))?
        else {
            continue;
        };
        if points.len() < 468 || points.iter().flatten().any(|v| !v.is_finite()) {
            continue;
        }
        let angle = landmarks::roll(
            [points[33][0], points[33][1]],
            [points[263][0], points[263][1]],
        );
        let crop = geometry::Affine::crop(
            [x + w * 0.5, y + h * 0.5],
            w.max(h) * parse::FACE_CROP_SCALE,
            angle,
            parse::INPUT,
        );
        let parsed = models.parser.run(|s| parse::parse(s, image, crop))?;
        if [1, 50, 280]
            .into_iter()
            .map(|i| {
                parsed
                    .sample(parse::Class::FaceSkin, points[i][0], points[i][1])
                    .0
            })
            .fold(0., f32::max)
            < 0.3
        {
            continue;
        }
        faces.push(
            points
                .into_iter()
                .take(468)
                .map(|p| {
                    let confidence = geometry::smoothstep(
                        0.3,
                        0.8,
                        parsed.sample(parse::Class::FaceSkin, p[0], p[1]).0,
                    );
                    [
                        p[0] / image.width() as f32,
                        p[1] / image.height() as f32,
                        -p[2] / short,
                        confidence,
                    ]
                })
                .collect(),
        );
    }
    Ok(faces)
}

/// The whole photo's people parse (skin, hair, clothes), for photos whose
/// people the face detector misses (a profile).
pub fn parse_people(image: &RgbImage, models: &FaceModels) -> Result<parse::ParseCrop> {
    let (width, height) = image.dimensions();
    models
        .parser
        .run(|s| parse::parse(s, image, parse::whole_photo(width, height)))
}
