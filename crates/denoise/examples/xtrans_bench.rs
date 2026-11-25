// SPDX-License-Identifier: AGPL-3.0-or-later
//! Runs AI Denoise's X-Trans network (`XTransNet`) on a mosaic file and
//! writes the result, to compare it with the network as it was scored and
//! to time it (G18, task XTSH1).
//!
//! ```sh
//! ORT_DYLIB_PATH=…/libonnxruntime.so RAWTONIC_AI_PROVIDER=cpu \
//!   cargo run --release -p rawtonic-denoise --example xtrans_bench -- \
//!   MOSAIC.f32 WIDTH HEIGHT xtrans_denoise_v1.onnx OUT.f32 [NOISE_SCALE]
//! ```
//!
//! `MOSAIC.f32` holds `WIDTH × HEIGHT` little-endian f32 samples
//! (black-subtracted, white 1), the canonical X-Trans phase at the origin
//! unless `XTRANS_PHASE=dy,dx` names the sample that holds it. No cache:
//! every run computes. `OUT.f32` (a new file; an existing one is never
//! replaced) gets the result in the same layout; the noise profile and the
//! time are printed.

use std::path::Path;
use std::time::Instant;

use anyhow::{Context, Result};
use rawtonic_denoise::XTransNet;
use rawtonic_denoise::xtrans::CANONICAL;
use rawtonic_rawdecode::mosaic_denoise::MosaicDenoiser;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    anyhow::ensure!(
        args.len() >= 5,
        "usage: MOSAIC.f32 WIDTH HEIGHT MODEL.onnx OUT.f32 [NOISE_SCALE]"
    );
    let (width, height): (usize, usize) = (args[1].parse()?, args[2].parse()?);
    let scale: f32 = args.get(5).map_or(Ok(1.0), |v| v.parse())?;
    let bytes = std::fs::read(&args[0]).context("mosaic")?;
    let mosaic: Vec<f32> = bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|&b| f32::from_le_bytes(b))
        .collect();
    anyhow::ensure!(mosaic.len() == width * height, "{} samples", mosaic.len());
    let (dy, dx) = match std::env::var("XTRANS_PHASE") {
        Ok(v) => {
            let (a, b) = v.split_once(',').context("XTRANS_PHASE=dy,dx")?;
            (a.parse::<usize>()?, b.parse::<usize>()?)
        }
        Err(_) => (0, 0),
    };
    let pattern: [[u8; 6]; 6] = std::array::from_fn(|y| {
        std::array::from_fn(|x| CANONICAL[(y + 6 - dy) % 6][(x + 6 - dx) % 6])
    });
    let loaded = Instant::now();
    let net = XTransNet::onnx(Path::new(&args[3]), None)?;
    eprintln!("loaded in {:.2} s", loaded.elapsed().as_secs_f32());
    let started = Instant::now();
    let out = net
        .denoise_xtrans(&mosaic, width, height, &pattern, scale, &|done, total| {
            eprintln!(
                "tile {done}/{total} at {:.1} s",
                started.elapsed().as_secs_f32()
            )
        })
        .context("not an X-Trans denoiser")??;
    println!(
        "{{\"width\": {width}, \"height\": {height}, \"seconds\": {:.3}}}",
        started.elapsed().as_secs_f64()
    );
    let mut bytes = Vec::with_capacity(out.len() * 4);
    for v in out {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    // Never over an existing file (a mistyped path must not replace the
    // mosaic or a RAW): a synced temporary file beside it, published only if
    // the name is free (rawtonic-fs), then the folder synced.
    let target = Path::new(&args[4]);
    let dir = target
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut staged = tempfile::NamedTempFile::new_in(dir)?;
    std::io::Write::write_all(&mut staged, &bytes)?;
    staged.as_file().sync_all()?;
    rawtonic_fs::no_clobber::persist_atomic(staged, target)
        .with_context(|| format!("{} not written", target.display()))?;
    rawtonic_fs::sync_dir(dir)?;
    Ok(())
}
