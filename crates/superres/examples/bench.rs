// SPDX-License-Identifier: AGPL-3.0-or-later
//! Measurement tool (G75): enlarges one 8-bit image with the model folder's
//! Super Resolution as the CLI/app would pick it (`RAWTONIC_AI_PROVIDER`
//! forces a provider), prints the provider, the load and enlarge times, a
//! tile-seam check, and compares with an earlier dump when given.
//!
//! `cargo run --release --example bench -p rawtonic-superres -- MODELS IN.tif [OUT.rgb8] [REFERENCE.rgb8]`

use std::time::Instant;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let models = std::path::PathBuf::from(&args[1]);
    // IDLE_PROMOTE=FILE: the app's way of loading (model on the CPU at once, GPU check queued for when
    // idle, verdicts in FILE); after loading, waits up to 180 s for the model to move to the GPU.
    let idle_file = std::env::var_os("IDLE_PROMOTE").map(std::path::PathBuf::from);
    rawtonic_onnx::configure(rawtonic_onnx::Settings {
        checks_file: idle_file
            .clone()
            .or_else(|| models.parent().map(|p| p.join("ai-gpu-checks.tsv"))),
        check_when_idle: idle_file.is_some(),
        ..rawtonic_onnx::settings()
    });
    let mut image = image::open(&args[2])?;
    // CROP=x,y,w,h: enlarge only this window (a CPU/GPU parity check without the CPU's minutes per photo)
    if let Ok(crop) = std::env::var("CROP") {
        let v: Vec<u32> = crop.split(',').map(|n| n.parse().unwrap()).collect();
        image = image.crop_imm(v[0], v[1], v[2], v[3]);
    }
    let (w, h) = (image.width(), image.height());
    let started = Instant::now();
    let model = rawtonic_superres::Model::from_dir(&models)?;
    println!(
        "backend={} load_ms={}",
        model.backend_name(),
        started.elapsed().as_millis()
    );
    if idle_file.is_some() {
        let waited = Instant::now();
        while waited.elapsed().as_secs() < 180 && model.backend_name().contains("CPU") {
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
        println!(
            "after {} s idle: backend={}",
            waited.elapsed().as_secs(),
            model.backend_name()
        );
    }
    let started = Instant::now();
    let out = model
        .enlarge(image, &|| Ok(()))
        .map_err(anyhow::Error::msg)?
        .to_rgb8();
    println!(
        "enlarge {w}x{h} -> {}x{} ms={}",
        out.width(),
        out.height(),
        started.elapsed().as_millis()
    );
    let (ow, oh) = (out.width() as usize, out.height() as usize);
    let raw = out.as_raw();
    // Seam check: mean |column step| at tile joins vs all other columns, same for rows.
    let step = 2 * (rawtonic_superres::TILE - 2 * rawtonic_superres::MARGIN);
    let col_step = |x: usize| -> f64 {
        let mut sum = 0u64;
        for y in (0..oh).step_by(7) {
            for c in 0..3 {
                let a = raw[(y * ow + x) * 3 + c] as i32;
                let b = raw[(y * ow + x - 1) * 3 + c] as i32;
                sum += (a - b).unsigned_abs() as u64;
            }
        }
        sum as f64 / ((oh / 7 + 1) * 3) as f64
    };
    let row_step = |y: usize| -> f64 {
        let mut sum = 0u64;
        for x in (0..ow).step_by(7) {
            for c in 0..3 {
                let a = raw[(y * ow + x) * 3 + c] as i32;
                let b = raw[((y - 1) * ow + x) * 3 + c] as i32;
                sum += (a - b).unsigned_abs() as u64;
            }
        }
        sum as f64 / ((ow / 7 + 1) * 3) as f64
    };
    let n_c = (ow.saturating_sub(1)) / step;
    let n_r = (oh.saturating_sub(1)) / step;
    let seam_c: Vec<f64> = (1..=n_c).map(|k| col_step(k * step)).collect();
    let seam_r: Vec<f64> = (1..=n_r).map(|k| row_step(k * step)).collect();
    // Control: columns/rows half a tile step away from every join.
    let other_c: Vec<f64> = (0..n_c).map(|k| col_step(k * step + step / 2)).collect();
    let other_r: Vec<f64> = (0..n_r).map(|k| row_step(k * step + step / 2)).collect();
    let mean = |v: &[f64]| v.iter().sum::<f64>() / v.len().max(1) as f64;
    println!(
        "seam col {:.4} vs other {:.4} (ratio {:.3}); seam row {:.4} vs other {:.4} (ratio {:.3})",
        mean(&seam_c),
        mean(&other_c),
        mean(&seam_c) / mean(&other_c),
        mean(&seam_r),
        mean(&other_r),
        mean(&seam_r) / mean(&other_r)
    );
    if let Some(reference) = args.get(4) {
        let reference = std::fs::read(reference)?;
        anyhow::ensure!(reference.len() == raw.len(), "reference size differs");
        let (mut max, mut sum, mut over) = (0u8, 0u64, 0u64);
        for (a, b) in raw.iter().zip(&reference) {
            let d = a.abs_diff(*b);
            max = max.max(d);
            sum += d as u64;
            over += (d > 3) as u64; // 0.01 of 255 = 2.55 codes
        }
        println!(
            "vs reference: max {} codes ({:.4}), mean {:.5} codes, values > 3 codes: {}",
            max,
            max as f64 / 255.0,
            sum as f64 / raw.len() as f64,
            over
        );
    }
    if let Some(path) = args.get(3).filter(|p| p.as_str() != "-") {
        std::fs::write(path, raw)?;
    }
    Ok(())
}
