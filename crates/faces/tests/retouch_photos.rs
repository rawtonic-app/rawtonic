// SPDX-License-Identifier: AGPL-3.0-or-later
//! Writes the face retouch's analysis of real photos: blemish circles
//! (bright: taken at Blemish 30, dim: only at 100) over the photo and the
//! skin and under-eye weights. Ignored: needs `FACE_MODELS`, `FACE_PHOTOS`,
//! `FACE_OUT` and `ORT_DYLIB_PATH` (see real_photos.rs).
//!
//! cargo test -p rawtonic-faces --release --test retouch_photos -- --ignored --nocapture

use std::path::PathBuf;
use std::time::Instant;

use image::{GrayImage, Rgb};
use rawtonic_faces::{FaceModels, analyze, retouch};

fn env(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).map(PathBuf::from)
}

/// The engine's slider mapping (rawtonic-engine face_retouch::blemish_threshold).
fn threshold(amount: f32) -> f32 {
    1.0 * (0.015f32 / 1.0).powf(amount.powf(0.25))
}

fn circle(image: &mut image::RgbImage, cx: f32, cy: f32, r: f32, colour: [u8; 3], width: f32) {
    let (w, h) = image.dimensions();
    let pad = r + width + 1.0;
    let (x0, x1) = ((cx - pad).max(0.0) as u32, ((cx + pad) as u32).min(w - 1));
    let (y0, y1) = ((cy - pad).max(0.0) as u32, ((cy + pad) as u32).min(h - 1));
    for y in y0..=y1 {
        for x in x0..=x1 {
            let d = ((x as f32 - cx).powi(2) + (y as f32 - cy).powi(2)).sqrt();
            if (d - r).abs() <= width / 2.0 {
                image.put_pixel(x, y, Rgb(colour));
            }
        }
    }
}

#[test]
#[ignore]
fn writes_retouch_analysis_of_real_photos() {
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
        .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("jpg")))
        .collect();
    files.sort();
    for file in files {
        let image = image::open(&file).unwrap().into_rgb8();
        let analysis = analyze(&image, &models).unwrap();
        let name = file.file_stem().unwrap().to_string_lossy().to_string();
        let mut marked = image.clone();
        for index in 0..analysis.faces.len() {
            let start = Instant::now();
            let face = retouch::retouch_face(&image, &analysis, index).unwrap();
            let took = start.elapsed();
            let width = (face.eye_distance / 150.0).max(1.5);
            for b in &face.blemishes {
                let colour = if b.score >= threshold(0.3) {
                    [255, 40, 40]
                } else if b.score >= threshold(0.6) {
                    [255, 200, 0]
                } else {
                    [80, 160, 255]
                };
                circle(&mut marked, b.x, b.y, b.r, colour, width);
            }
            let mut scores: Vec<f32> = face.blemishes.iter().map(|b| b.score).collect();
            scores.sort_by(|a, b| b.total_cmp(a));
            let pick = |i: usize| scores.get(i).copied().unwrap_or(0.0);
            println!(
                "  scores #1 {:.3} #5 {:.3} #10 {:.3} #20 {:.3} #50 {:.3} #100 {:.3} #200 {:.3}",
                pick(0),
                pick(4),
                pick(9),
                pick(19),
                pick(49),
                pick(99),
                pick(199)
            );
            let save = |m: &GrayImage, kind: &str| {
                m.save(out.join(format!("{name}_{index}_{kind}.png")))
                    .unwrap()
            };
            save(&face.skin, "skin");
            if let Some(u) = &face.under_eyes {
                save(u, "undereyes");
            }
            println!(
                "{name} face {index}: eye distance {:.0} px, blemishes {} (≥30: {}, ≥60: {}), {:.0} ms",
                face.eye_distance,
                face.blemishes.len(),
                face.blemishes
                    .iter()
                    .filter(|b| b.score >= threshold(0.3))
                    .count(),
                face.blemishes
                    .iter()
                    .filter(|b| b.score >= threshold(0.6))
                    .count(),
                took.as_secs_f64() * 1e3
            );
        }
        marked
            .save(out.join(format!("{name}_blemishes.jpg")))
            .unwrap();
    }
    std::mem::forget(models);
}
