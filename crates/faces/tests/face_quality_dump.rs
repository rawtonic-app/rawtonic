// SPDX-License-Identifier: AGPL-3.0-or-later
//! Writes what `analyze` finds per face (box, keypoints, the 478 landmarks
//! or none) as one JSON line per photo, with the analysis time, for scoring
//! eye openness and eye sharpness outside the app (notes:
//! benchmark-results/fq-face-quality-2026-10-31/). Ignored: needs the
//! models (`FACE_MODELS`), ONNX Runtime (`ORT_DYLIB_PATH`), photos
//! (`FACE_PHOTOS`, a folder of JPEG files, one level of subfolders per
//! shoot) and the output file (`FACE_OUT`).
//!
//! cargo test -p rawtonic-faces --release --test face_quality_dump -- --ignored --nocapture

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

use image::ImageDecoder;
use rawtonic_faces::parse::Class;
use rawtonic_faces::{FaceModels, analyze};
use serde_json::json;

fn env(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).map(PathBuf::from)
}

fn jpegs(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap().filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.is_dir() {
            files.extend(jpegs(&path));
        } else if path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "jpg" | "jpeg"))
        {
            files.push(path);
        }
    }
    files.sort();
    files
}

#[test]
#[ignore]
fn dumps_faces_of_real_photos() {
    let (Some(models), Some(photos), Some(out)) =
        (env("FACE_MODELS"), env("FACE_PHOTOS"), env("FACE_OUT"))
    else {
        eprintln!("SKIP: FACE_MODELS, FACE_PHOTOS and FACE_OUT are needed");
        return;
    };
    let models = FaceModels::load(&models).unwrap();
    let mut lines = std::fs::File::create(&out).unwrap();
    for file in jpegs(&photos) {
        let start = Instant::now();
        // Upright as displayed (EXIF orientation applied).
        let mut decoder = image::ImageReader::open(&file)
            .unwrap()
            .with_guessed_format()
            .unwrap()
            .into_decoder()
            .unwrap();
        let orientation = decoder.orientation().unwrap();
        let mut image = image::DynamicImage::from_decoder(decoder).unwrap();
        image.apply_orientation(orientation);
        let image = image.into_rgb8();
        let decoded = start.elapsed();
        let start = Instant::now();
        let analysis = analyze(&image, &models).unwrap();
        let analysed = start.elapsed();
        let faces: Vec<_> = analysis
            .faces
            .iter()
            .map(|f| {
                // The parse's class probabilities at each eye (iris centre,
                // or YuNet's eye keypoint without landmarks): sunglasses
                // read as accessories, not face skin.
                let eyes = match &f.landmarks {
                    Some(p) => [p[468], p[473]],
                    None => [f.detection.keypoints[0], f.detection.keypoints[1]],
                };
                let eye_parse: Vec<Vec<f32>> = eyes
                    .iter()
                    .map(|&[x, y]| {
                        [
                            Class::Background,
                            Class::Hair,
                            Class::BodySkin,
                            Class::FaceSkin,
                            Class::Clothes,
                            Class::Accessories,
                        ]
                        .iter()
                        .map(|&c| f.parse.sample(c, x, y).0)
                        .collect()
                    })
                    .collect();
                json!({
                    "eye_parse": eye_parse,
                    "score": f.detection.score,
                    "bbox": f.detection.bbox,
                    "keypoints": f.detection.keypoints,
                    "landmarks": f.landmarks,
                })
            })
            .collect();
        let name = file.strip_prefix(&photos).unwrap().to_string_lossy();
        println!(
            "{name}: {} faces, decode {:.0} ms, analyse {:.0} ms",
            faces.len(),
            decoded.as_secs_f64() * 1e3,
            analysed.as_secs_f64() * 1e3
        );
        let line = json!({
            "file": name,
            "width": analysis.width,
            "height": analysis.height,
            "analyse_ms": analysed.as_secs_f64() * 1e3,
            "faces": faces,
        });
        writeln!(lines, "{line}").unwrap();
    }
}
