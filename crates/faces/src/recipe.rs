// SPDX-License-Identifier: AGPL-3.0-or-later
//! The face retouch's recipe entries (`faceRetouch.faces`), shared by the
//! app's `analyze_face_retouch` and the CLI's `export --face-retouch`.

use std::io::Cursor;

use base64::{Engine as _, engine::general_purpose};
use image::{DynamicImage, GrayImage, ImageFormat, RgbImage};
use rayon::prelude::*;

use crate::FaceAnalysis;

/// The photo as displayed, 8-bit sRGB: what the networks were trained on.
/// `linear`: the image is a linear RAW decode.
pub fn display_rgb8(image: &DynamicImage, linear: bool) -> RgbImage {
    if !linear {
        return image.to_rgb8();
    }
    let float = image.to_rgb32f();
    let (w, h) = float.dimensions();
    let mut out = vec![0u8; (w * h * 3) as usize];
    out.par_iter_mut()
        .zip(float.as_raw().par_iter())
        .for_each(|(o, &v)| {
            let x = v.clamp(0.0, 1.0);
            let e = if x <= 0.003_130_8 {
                12.92 * x
            } else {
                1.055 * x.powf(1.0 / 2.4) - 0.055
            };
            *o = (e * 255.0).round() as u8;
        });
    RgbImage::from_raw(w, h, out).expect("buffer of the image's size")
}

fn data_url(bytes: &[u8], mime: &str) -> String {
    format!(
        "data:{mime};base64,{}",
        general_purpose::STANDARD.encode(bytes)
    )
}

fn png_data_url(image: &GrayImage) -> Result<String, String> {
    let mut png = Cursor::new(Vec::new());
    image
        .write_to(&mut png, ImageFormat::Png)
        .map_err(|e| e.to_string())?;
    Ok(data_url(png.get_ref(), "image/png"))
}

/// The recipe entries for the faces of `image`, which is the photo in the
/// mask frame (oriented, straightened, geometry corrected, not cropped):
/// crop, weights, eye distance and blemishes for the render
/// (rawtonic-engine `face_retouch`), and the face box and a small thumbnail
/// for the panel.
pub fn retouch_faces(
    image: &RgbImage,
    analysis: &FaceAnalysis,
) -> Result<Vec<serde_json::Value>, String> {
    (0..analysis.faces.len())
        .into_par_iter()
        .map(|index| {
            let face = &analysis.faces[index];
            let Some(data) = crate::retouch::retouch_face(image, analysis, index) else {
                return Ok(None);
            };
            let thumb = crate::thumbnail(image, face, 64);
            let mut jpeg = Cursor::new(Vec::new());
            thumb
                .write_to(&mut jpeg, ImageFormat::Jpeg)
                .map_err(|e| e.to_string())?;
            let c = data.crop;
            let round = |v: f32| (v as f64 * 100.0).round() / 100.0;
            Ok(Some(serde_json::json!({
                "id": format!("face-{}", index + 1),
                "crop": [c.a as f64, c.b as f64, c.c as f64, c.d as f64, c.e as f64, c.f as f64],
                "size": crate::retouch::WEIGHT_SIZE,
                "eyeDistance": round(data.eye_distance),
                "skin": png_data_url(&data.skin)?,
                "underEyes": data.under_eyes.as_ref().map(png_data_url).transpose()?,
                "neck": data.neck.as_ref().map(png_data_url).transpose()?,
                "blemishes": data.blemishes.iter().map(|b| serde_json::json!({
                    "id": b.id, "x": round(b.x), "y": round(b.y), "r": round(b.r),
                    "score": (b.score as f64 * 1000.0).round() / 1000.0,
                    "neck": b.neck,
                })).collect::<Vec<_>>(),
                "kept": [],
                "added": [],
                "settings": {},
                "enabled": true,
                "box": face.detection.bbox.map(round),
                "thumbnail": data_url(jpeg.get_ref(), "image/jpeg"),
            })))
        })
        .collect::<Result<Vec<_>, String>>()
        .map(|faces| faces.into_iter().flatten().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linear_input_is_encoded_for_the_networks() {
        let linear = DynamicImage::ImageRgb32F(image::Rgb32FImage::from_pixel(
            2,
            2,
            image::Rgb([0.18, 0.0, 1.0]),
        ));
        assert_eq!(display_rgb8(&linear, true).get_pixel(0, 0).0, [118, 0, 255]);
    }
}
