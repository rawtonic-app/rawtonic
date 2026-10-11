// SPDX-License-Identifier: AGPL-3.0-or-later
//! Runs the face models on real photos and writes every part's mask as an
//! overlay, with timings. Ignored: needs the models (`FACE_MODELS`, the
//! model folder), ONNX Runtime (`ORT_DYLIB_PATH`), photos (`FACE_PHOTOS`,
//! a folder of JPEG/PNG files) and an output folder (`FACE_OUT`).
//!
//! cargo test -p rawtonic-faces --release --test real_photos -- --ignored --nocapture

use std::path::PathBuf;
use std::time::Instant;

use image::{Rgb, RgbImage};
use rawtonic_faces::{FaceModels, Part, analyze, part_mask};

fn env(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).map(PathBuf::from)
}

#[test]
#[ignore]
fn writes_part_masks_of_real_photos() {
    let (Some(models), Some(photos), Some(out)) =
        (env("FACE_MODELS"), env("FACE_PHOTOS"), env("FACE_OUT"))
    else {
        eprintln!("SKIP: FACE_MODELS, FACE_PHOTOS and FACE_OUT are needed");
        return;
    };
    std::fs::create_dir_all(&out).unwrap();
    let models = FaceModels::load(&models).unwrap();
    let mut files: Vec<_> = std::fs::read_dir(&photos)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "jpg" | "jpeg" | "png"))
        })
        .collect();
    files.sort();
    let colours: [(Part, [u8; 3]); 10] = [
        (Part::FaceSkin, [0, 200, 255]),
        (Part::BodySkin, [255, 0, 255]),
        (Part::Hair, [255, 200, 0]),
        (Part::Clothes, [0, 255, 0]),
        (Part::Eyebrows, [255, 120, 0]),
        (Part::Sclera, [255, 255, 255]),
        (Part::Iris, [0, 0, 255]),
        (Part::Lips, [255, 0, 0]),
        (Part::Teeth, [255, 255, 0]),
        (Part::EntirePerson, [128, 128, 128]),
    ];
    for file in files {
        let image = image::open(&file).unwrap().into_rgb8();
        let start = Instant::now();
        let analysis = analyze(&image, &models).unwrap();
        let analysed = start.elapsed();
        // FACE_STRIPS_ONLY: just the faces found, for a quick check of many
        // photos.
        if std::env::var_os("FACE_STRIPS_ONLY").is_some() {
            let name = file.file_stem().unwrap().to_string_lossy().to_string();
            for (i, f) in analysis.faces.iter().enumerate() {
                let thumb = rawtonic_faces::thumbnail(&image, f, 96);
                let tag = if f.landmarks.is_some() { "m" } else { "p" };
                thumb
                    .save(out.join(format!("{name}_{i}{tag}.jpg")))
                    .unwrap();
            }
            println!(
                "{name}: faces {} analyse {:.0} ms",
                analysis.faces.len(),
                analysed.as_secs_f64() * 1e3
            );
            continue;
        }
        let start = Instant::now();
        let mut overlay = image.clone();
        for (part, colour) in colours.iter().take(9) {
            let mask = part_mask(&image, &analysis, &[*part], None);
            for (p, m) in overlay.pixels_mut().zip(mask.pixels()) {
                let a = m[0] as f32 / 255.0 * 0.6;
                *p = Rgb(std::array::from_fn(|c| {
                    (p[c] as f32 * (1.0 - a) + colour[c] as f32 * a).round() as u8
                }));
            }
        }
        let masked = start.elapsed() / 9;
        let name = file.file_stem().unwrap().to_string_lossy().to_string();
        overlay.save(out.join(format!("{name}_parts.jpg"))).unwrap();
        if !analysis.faces.is_empty() {
            let skin = part_mask(&image, &analysis, &[Part::FaceSkin], Some(0));
            skin.save(out.join(format!("{name}_person1_skin.png")))
                .unwrap();
        }
        let thumbs: Vec<RgbImage> = analysis
            .faces
            .iter()
            .map(|f| rawtonic_faces::thumbnail(&image, f, 96))
            .collect();
        if !thumbs.is_empty() {
            let mut strip = RgbImage::new(96 * thumbs.len() as u32, 96);
            for (i, t) in thumbs.iter().enumerate() {
                image::imageops::replace(&mut strip, t, i as i64 * 96, 0);
            }
            strip.save(out.join(format!("{name}_people.jpg"))).unwrap();
        }
        let start = Instant::now();
        let _ = part_mask(
            &image,
            &analysis,
            &[Part::FaceSkin, Part::Lips, Part::Eyebrows],
            None,
        );
        let combined = start.elapsed();
        println!(
            "{name}: {}x{} faces {} (landmarks {}) analyse {:.0} ms, one part mask {:.0} ms, skin+lips+brows {:.0} ms",
            image.width(),
            image.height(),
            analysis.faces.len(),
            analysis
                .faces
                .iter()
                .filter(|f| f.landmarks.is_some())
                .count(),
            analysed.as_secs_f64() * 1e3,
            masked.as_secs_f64() * 1e3,
            combined.as_secs_f64() * 1e3,
        );
    }
    // ONNX Runtime aborts when its environment is torn down at exit.
    std::mem::forget(models);
}
