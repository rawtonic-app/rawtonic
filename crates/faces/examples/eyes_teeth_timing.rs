// SPDX-License-Identifier: AGPL-3.0-or-later
//! Times the eye and teeth parser (`portrait`): model load and the
//! per-face parse (crop, network, outlines) on photos, as the face retouch
//! runs it. Notes: benchmark-results/eye4-2026-11-21.
//!
//! cargo run --release -p rawtonic-faces --example eyes_teeth_timing -- \
//!     MODELS_DIR (onnx|coreml) RUNS PHOTO...
//! MODELS_DIR holds the face models and the parser's files (`portrait`);
//! `coreml` takes the assembled Core ML program (macOS), `onnx` the ONNX
//! model on ONNX Runtime's CPU provider (ORT_DYLIB_PATH).

use std::path::Path;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use rawtonic_faces::portrait::{self, PortraitModel};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [dir, backend, runs, photos @ ..] = args.as_slice() else {
        bail!("usage: eyes_teeth_timing MODELS_DIR (onnx|coreml) RUNS PHOTO...");
    };
    let dir = Path::new(dir);
    let runs: usize = runs.parse().context("RUNS")?;
    let faces = rawtonic_faces::FaceModels::load(dir)?;
    let start = Instant::now();
    let model = match backend.as_str() {
        "onnx" => PortraitModel::onnx(&dir.join(portrait::FILE))?,
        #[cfg(target_os = "macos")]
        "coreml" => PortraitModel::core_ml(rawtonic_denoise::coreml::CoreMlModel::load(
            &dir.join(portrait::CORE_ML_PACKAGE),
            portrait::CORE_ML_UNITS,
        )?),
        other => bail!("unknown backend {other}"),
    };
    println!("load {} ms ({})", start.elapsed().as_millis(), model.backend_name());
    let mut times = Vec::new();
    for (k, photo) in photos.iter().enumerate() {
        let image = image::open(photo).with_context(|| photo.clone())?.to_rgb8();
        let analysis = rawtonic_faces::analyze(&image, &faces)?;
        for face in &analysis.faces {
            let features = rawtonic_faces::eye_teeth::eye_teeth(&image, face);
            for run in 0..runs {
                let t = Instant::now();
                portrait::parse(&model, &image, face, &features)?;
                let ms = t.elapsed().as_secs_f64() * 1e3;
                if k == 0 && run == 0 {
                    println!("first parse {ms:.0} ms");
                } else {
                    times.push(ms);
                }
            }
        }
    }
    times.sort_by(f64::total_cmp);
    if !times.is_empty() {
        let q = |p: f64| times[((times.len() - 1) as f64 * p).round() as usize];
        println!(
            "per face: p50 {:.0} ms, p90 {:.0} ms, n {}",
            q(0.5),
            q(0.9),
            times.len()
        );
    }
    std::mem::forget(model);
    std::mem::forget(faces);
    Ok(())
}
