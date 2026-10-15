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
/// people the face detector misses (a profile); `None` when it finds less
/// skin than a person shows (1/300 of the frame, as `analyze_subject`).
pub fn parse_people(image: &RgbImage, models: &FaceModels) -> Result<Option<parse::ParseCrop>> {
    let (width, height) = image.dimensions();
    let crop = models
        .parser
        .run(|s| parse::parse(s, image, parse::whole_photo(width, height)))?;
    // Sample the skin on a coarse grid of the frame.
    let (steps_x, steps_y) = (96u32, 96u32);
    let skin = (0..steps_y)
        .flat_map(|j| (0..steps_x).map(move |i| (i, j)))
        .filter(|&(i, j)| {
            let x = (i as f32 + 0.5) / steps_x as f32 * width as f32;
            let y = (j as f32 + 0.5) / steps_y as f32 * height as f32;
            let face = crop.sample(parse::Class::FaceSkin, x, y).0;
            let body = crop.sample(parse::Class::BodySkin, x, y).0;
            face + body > 0.5
        })
        .count();
    Ok((skin * 300 >= (steps_x * steps_y) as usize).then_some(crop))
}
