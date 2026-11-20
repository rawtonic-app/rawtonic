// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tool: times one AI Denoise tile on Core ML per compute unit (macOS).
//!
//! `cargo run --release -p rawtonic-denoise --example tile_bench -- PACKAGE UNITS [N] [lowacc]`
//! UNITS: all | gpu | ane | cpu. Runs N (default 6) tiles of a synthetic
//! noisy input after one warm-up run and prints the load, first-run and
//! per-tile times and a checksum of the last output.

#[cfg(target_os = "macos")]
fn main() -> anyhow::Result<()> {
    use rawtonic_denoise::coreml::{ComputeUnits, CoreMlModel};
    use std::time::Instant;
    let args: Vec<String> = std::env::args().collect();
    let package = std::path::PathBuf::from(&args[1]);
    let units = match args[2].as_str() {
        "all" => ComputeUnits::All,
        "gpu" => ComputeUnits::CpuAndGpu,
        "ane" => ComputeUnits::CpuAndNeuralEngine,
        _ => ComputeUnits::CpuOnly,
    };
    let n: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(6);
    let low = args.get(4).is_some_and(|s| s == "lowacc");
    let t = Instant::now();
    let model = CoreMlModel::load_with(&package, units, low)?;
    let load = t.elapsed().as_secs_f64();
    let shape = model.input_shape("raw_with_noise")?;
    let tile = shape[2];
    let mut seed = 1u64;
    let mut rnd = || {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (seed >> 40) as f32 / (1u64 << 24) as f32 - 0.5
    };
    let mut input = vec![0.0f32; 8 * tile * tile];
    for c in 0..4 {
        for i in 0..tile * tile {
            let x = (i % tile) as f32 / tile as f32;
            input[c * tile * tile + i] = 0.05 + 0.2 * x + 0.03 * rnd();
            input[(c + 4) * tile * tile + i] = 0.03;
        }
    }
    let t = Instant::now();
    model.predict("raw_with_noise", &shape, &input, "denoised_raw")?;
    let first = t.elapsed().as_secs_f64();
    let t = Instant::now();
    let mut sum = 0.0f64;
    for _ in 0..n {
        let (_, out) = model.predict("raw_with_noise", &shape, &input, "denoised_raw")?;
        sum = out.iter().map(|&v| v as f64).sum();
    }
    let per = t.elapsed().as_secs_f64() / n as f64;
    println!(
        "{} {} tile {tile} load {load:.2} s first {first:.2} s per tile {per:.3} s ({:.2} µs/sample) checksum {sum:.4}",
        package.display(),
        args[2],
        per * 1e6 / (tile * tile) as f64
    );
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn main() {}
