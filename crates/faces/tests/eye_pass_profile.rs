// SPDX-License-Identifier: AGPL-3.0-or-later
//! Times the eye check's steps per photo (decode, the detector's two
//! resizes, detection, the whole analysis) on a folder of JPEG files
//! (`FACE_PHOTOS`) with the models in `FACE_MODELS` (ONNX Runtime from
//! `ORT_DYLIB_PATH`). Prints one tab-separated line per photo.
//!
//! cargo test -p rawtonic-faces --release --test eye_pass_profile -- --ignored --nocapture

use std::path::PathBuf;
use std::time::Instant;

use image::imageops::{self, FilterType};
use image::{ImageDecoder, ImageFormat};
use rawtonic_faces::{FaceModels, analyze};

#[test]
#[ignore]
fn profiles_the_eye_check() {
    let env = |n: &str| std::env::var_os(n).map(PathBuf::from);
    let (Some(models), Some(photos)) = (env("FACE_MODELS"), env("FACE_PHOTOS")) else {
        eprintln!("SKIP: FACE_MODELS and FACE_PHOTOS are needed");
        return;
    };
    let t = Instant::now();
    let models = FaceModels::load(&models).unwrap();
    println!("load\t{:.0}", t.elapsed().as_secs_f64() * 1e3);
    let mut files: Vec<PathBuf> = std::fs::read_dir(&photos)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("jpg")))
        .collect();
    files.sort();
    let limit: usize = std::env::var("FACE_LIMIT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(usize::MAX);
    println!("file\tread\tdecode\tresize1\tresize045\tdetect\tanalyze\tfaces");
    let ms = |t: Instant| t.elapsed().as_secs_f64() * 1e3;
    for file in files.into_iter().take(limit) {
        let t = Instant::now();
        let bytes = std::fs::read(&file).unwrap();
        let read = ms(t);
        let t = Instant::now();
        let mut decoder =
            image::ImageReader::with_format(std::io::Cursor::new(bytes), ImageFormat::Jpeg)
                .into_decoder()
                .unwrap();
        let orientation = decoder.orientation().unwrap();
        let mut image = image::DynamicImage::from_decoder(decoder).unwrap();
        image.apply_orientation(orientation);
        let image = image.into_rgb8();
        let decode = ms(t);
        let (w, h) = image.dimensions();
        let mut rs = [0.0; 2];
        for (k, share) in [1.0f32, 0.45].into_iter().enumerate() {
            let fit = (640.0 * share).round();
            let s = fit / w.max(h) as f32;
            let t = Instant::now();
            let _ = imageops::resize(
                &image,
                ((w as f32 * s).round() as u32).clamp(1, 640),
                ((h as f32 * s).round() as u32).clamp(1, 640),
                FilterType::Triangle,
            );
            rs[k] = ms(t);
        }
        let t = Instant::now();
        let _ = models.detect(&image).unwrap();
        let detect = ms(t);
        let t = Instant::now();
        let analysis = analyze(&image, &models).unwrap();
        let total = ms(t);
        println!(
            "{}\t{read:.0}\t{decode:.0}\t{:.0}\t{:.0}\t{detect:.0}\t{total:.0}\t{}",
            file.file_name().unwrap().to_string_lossy(),
            rs[0],
            rs[1],
            analysis.faces.len()
        );
    }
}
