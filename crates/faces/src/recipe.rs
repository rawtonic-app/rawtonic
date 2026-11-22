// SPDX-License-Identifier: AGPL-3.0-or-later
//! The face retouch's recipe entries (`faceRetouch.faces`), shared by the
//! app's `analyze_face_retouch` and the CLI's `export --face-retouch`.

use std::io::Cursor;

use base64::{Engine as _, engine::general_purpose};
use image::{DynamicImage, GrayImage, ImageFormat, RgbImage};
use rayon::prelude::*;

use crate::FaceAnalysis;
use crate::eye_teeth::EyeTeeth;
use crate::portrait::{Parsed, PortraitModel};

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
/// crop, weights, eye distance, blemishes, eyes and mouth for the render
/// (rawtonic-engine `face_retouch`), and the face box and a small thumbnail
/// for the panel. With `portrait` the eye and teeth parser's outlines too
/// (`parsedEye`, `parsedIris` per eye it found, `parsedTeeth` on every face,
/// empty where Face Mesh's outlines stay; `portrait`).
pub fn retouch_faces(
    image: &RgbImage,
    analysis: &FaceAnalysis,
    portrait: Option<&PortraitModel>,
) -> Result<Vec<serde_json::Value>, String> {
    let features: Vec<EyeTeeth> = analysis
        .faces
        .par_iter()
        .map(|face| crate::eye_teeth::eye_teeth(image, face))
        .collect();
    // One face at a time: the network is large and runs on all cores.
    let parsed: Vec<Option<Parsed>> = match portrait {
        Some(model) => analysis
            .faces
            .iter()
            .zip(&features)
            .map(|(face, features)| crate::portrait::parse(model, image, face, features).map(Some))
            .collect::<anyhow::Result<_>>()
            .map_err(|e| format!("{e:#}"))?,
        None => vec![None; analysis.faces.len()],
    };
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
            let points = |p: &[[f32; 2]]| {
                p.iter()
                    .map(|q| [round(q[0]), round(q[1])])
                    .collect::<Vec<_>>()
            };
            let features = &features[index];
            let parsed = parsed[index].as_ref();
            let loops = |l: &[Vec<[f32; 2]>]| l.iter().map(|p| points(p)).collect::<Vec<_>>();
            let eyes: Vec<serde_json::Value> = features
                .eyes
                .iter()
                .enumerate()
                .map(|(side, e)| {
                    let Some(e) = e else {
                        return serde_json::Value::Null;
                    };
                    let mut eye = serde_json::json!({
                        "iris": e.iris.map(round),
                        "lid": points(&e.lid),
                    });
                    if let Some(p) = parsed
                        .and_then(|p| p.eyes.get(side))
                        .and_then(Option::as_ref)
                    {
                        eye["parsedEye"] = serde_json::json!(loops(&p.eye));
                        eye["parsedIris"] = serde_json::json!(loops(&p.iris));
                    }
                    eye
                })
                .collect();
            let mut entry = serde_json::json!({
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
                "eyes": eyes,
                "mouth": features.mouth.as_deref().map(points),
                "kept": [],
                "added": [],
                "settings": {},
                "enabled": true,
                "box": face.detection.bbox.map(round),
                "thumbnail": data_url(jpeg.get_ref(), "image/jpeg"),
            });
            if let Some(p) = parsed {
                entry["parsedTeeth"] = serde_json::json!(loops(&p.teeth));
                entry["irisDisk"] = serde_json::json!(crate::portrait::IRIS_DISK);
            }
            Ok(Some(entry))
        })
        .collect::<Result<Vec<_>, String>>()
        .map(|faces| faces.into_iter().flatten().collect())
}

/// A People selection (`ai-people`) of `parts` (`Part::name`) of everyone
/// or of `person` on `warped` (the photo with its lens and transform
/// geometry, linear light when `linear`), as the editor's People mask
/// computes it: at `warped`'s size. A person the photo does not have is an
/// error (the editor refuses it too); a photo without faces selects nothing.
pub fn people_selection(
    warped: &DynamicImage,
    linear: bool,
    models: &crate::FaceModels,
    parts: &[String],
    person: Option<usize>,
) -> Result<GrayImage, String> {
    let selected: Vec<crate::Part> = parts
        .iter()
        .filter_map(|p| crate::Part::from_name(p))
        .collect();
    if selected.is_empty() {
        return Err("no person parts selected".into());
    }
    let image = display_rgb8(warped, linear);
    let analysis = crate::analyze(&image, models).map_err(|e| format!("{e:#}"))?;
    if let Some(i) = person
        && i >= analysis.faces.len()
    {
        return Err(format!("there is no person {} in this photo", i + 1));
    }
    Ok(crate::part_mask(&image, &analysis, &selected, person))
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
