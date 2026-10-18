// SPDX-License-Identifier: AGPL-3.0-or-later
//! Times AI Denoise (MatchFilter) on a RAW's Bayer mosaic with ONNX Runtime
//! and compares the result with a reference run (G18, Linux/Windows speed).
//!
//! ```sh
//! ORT_DYLIB_PATH=…/libonnxruntime.so cargo run --release -p rawtonic-denoise \
//!   --example denoise_bench -- photo.NEF nlmf.onnx OUT_DIR [cpu|webgpu|cuda] [REFERENCE.f16]
//! ```
//!
//! The mosaic is decoded once and kept as `OUT_DIR/<name>.mosaic` (f32 plus
//! a 16-byte header), so each provider can run in its own process and its
//! peak memory (`VmHWM`) is its own. The denoised mosaic is written as
//! `OUT_DIR/<name>.<provider>.f16`, the bytes the app caches. With a
//! reference, the largest and mean absolute difference to it are printed.
//! No cache: every run computes. `BENCH_CROP=W` runs on the top-left
//! W × 2W/3 samples only; `BENCH_THREADS=N` uses a CPU session with N
//! intra-op threads instead of the app's; `BENCH_TAG` names the output.
//! Results: notes benchmark-results/ai-denoise-linux-2026-10-18.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::{Context, Result, bail};
use rawtonic_denoise::{Backend, MatchFilter, TILE};
use rawtonic_onnx::{Model, ModelKind, Provider};
use rawtonic_rawdecode::mosaic_denoise::{MosaicDenoise, MosaicDenoiser};

/// Samples, width, height and the 2 × 2 colour pattern.
type Mosaic = (Vec<f32>, usize, usize, [u8; 4]);

/// Keeps the mosaic it is given and returns it unchanged.
#[derive(Default)]
struct Capture(Mutex<Option<Mosaic>>);

impl MosaicDenoiser for Capture {
    fn denoise(&self, m: &[f32], w: usize, h: usize, p: [u8; 4]) -> Result<Vec<f32>> {
        *self.0.lock().unwrap() = Some((m.to_vec(), w, h, p));
        Ok(m.to_vec())
    }
}

fn infer(session: &mut ort::session::Session, input: &[f32]) -> Result<Vec<f32>> {
    let array = ndarray::Array4::from_shape_vec((1, 8, TILE, TILE), input.to_vec())?;
    let outputs =
        session.run(ort::inputs!["raw_with_noise" => ort::value::Tensor::from_array(array)?])?;
    Ok(outputs["denoised_raw"]
        .try_extract_array::<f32>()?
        .iter()
        .copied()
        .collect())
}

/// The model as the app loads it (`Model`, with its GPU check) or, with
/// `BENCH_THREADS`, a plain CPU session with that many intra-op threads.
enum Onnx {
    Model(Model),
    Session(Mutex<ort::session::Session>),
}

impl Backend for Onnx {
    fn run(&self, input: &[f32]) -> Result<Vec<f32>> {
        match self {
            Onnx::Model(model) => model.run_without_fallback(|s| infer(s, input)),
            Onnx::Session(session) => infer(&mut session.lock().unwrap(), input),
        }
    }
}

fn peak_rss_mb() -> f64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("VmHWM:"))
                .and_then(|l| l.split_whitespace().nth(1)?.parse::<f64>().ok())
        })
        .map_or(f64::NAN, |kb| kb / 1024.0)
}

fn mosaic(raw: &Path, out: &Path) -> Result<Mosaic> {
    let stem = raw.file_name().unwrap().to_string_lossy().to_string();
    let file = out.join(format!("{stem}.mosaic"));
    if let Ok(bytes) = std::fs::read(&file) {
        let word = |i: usize| u32::from_le_bytes(bytes[i * 4..i * 4 + 4].try_into().unwrap());
        let (w, h) = (word(0) as usize, word(1) as usize);
        let pattern = word(2).to_le_bytes();
        let samples = bytes[16..]
            .as_chunks::<4>()
            .0
            .iter()
            .map(|&b| f32::from_le_bytes(b))
            .collect();
        return Ok((samples, w, h, pattern));
    }
    let capture = Arc::new(Capture::default());
    let started = Instant::now();
    rawtonic_rawdecode::raw_processing::develop_raw_image_with(
        &std::fs::read(raw).with_context(|| format!("reading {}", raw.display()))?,
        false,
        2.5,
        "auto".to_string(),
        None,
        Some(&MosaicDenoise::new(capture.clone(), 1.0)),
    )?;
    eprintln!("decoded in {:.1} s", started.elapsed().as_secs_f32());
    let Some((samples, w, h, pattern)) = capture.0.lock().unwrap().take() else {
        bail!("{} has no Bayer mosaic", raw.display());
    };
    let mut bytes = Vec::with_capacity(16 + samples.len() * 4);
    for word in [w as u32, h as u32, u32::from_le_bytes(pattern), 0] {
        bytes.extend_from_slice(&word.to_le_bytes());
    }
    for v in &samples {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    std::fs::write(&file, bytes)?;
    Ok((samples, w, h, pattern))
}

fn read_f16(path: &Path) -> Result<Vec<f32>> {
    Ok(std::fs::read(path)?
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&b| half::f16::from_le_bytes(b).to_f32())
        .collect())
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [raw, model, out, rest @ ..] = &args[..] else {
        bail!("usage: denoise_bench RAW MODEL OUT_DIR [cpu|webgpu|cuda] [REFERENCE.f16]");
    };
    let (raw, model, out) = (PathBuf::from(raw), PathBuf::from(model), PathBuf::from(out));
    let provider = rest
        .first()
        .map(|p| Provider::parse(p).context("provider: cpu, webgpu or cuda"))
        .transpose()?
        .unwrap_or(Provider::Cpu);
    std::fs::create_dir_all(&out)?;
    let (mut samples, mut w, mut h, pattern) = mosaic(&raw, &out)?;
    let env = |name: &str| {
        std::env::var(name)
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
    };
    // BENCH_CROP=W: the top-left W × 2W/3 samples only (even), for quick runs.
    let crop = env("BENCH_CROP").map(|cw| cw.min(w) & !1);
    if let Some(cw) = crop {
        let ch = (cw * 2 / 3).min(h) & !1;
        samples = (0..ch)
            .flat_map(|y| samples[y * w..y * w + cw].to_vec())
            .collect();
        (w, h) = (cw, ch);
    }
    eprintln!(
        "{}: {w}x{h} ({:.1} MP)",
        raw.display(),
        (w * h) as f64 / 1e6
    );

    let threads = env("BENCH_THREADS");
    let started = Instant::now();
    let backend = match threads {
        Some(n) if provider == Provider::Cpu => {
            rawtonic_onnx::ensure_runtime()?;
            Onnx::Session(Mutex::new(
                ort::session::Session::builder()?
                    .with_intra_threads(n)
                    .map_err(|e| anyhow::anyhow!("{e}"))?
                    .commit_from_file(&model)?,
            ))
        }
        _ => Onnx::Model(Model::load_on(
            &model,
            ModelKind::Denoise,
            &[provider],
            &rawtonic_onnx::default_plugin_dirs(),
        )?),
    };
    let load_s = started.elapsed().as_secs_f64();
    let filter = MatchFilter::new(Box::new(backend), None);
    let started = Instant::now();
    let denoised = filter.denoise(&samples, w, h, pattern)?;
    let run_s = started.elapsed().as_secs_f64();
    let stem = raw.file_name().unwrap().to_string_lossy().to_string();
    let tag = crop.map(|c| format!(".crop{c}")).unwrap_or_default();
    let target = out.join(format!(
        "{stem}{tag}.{}{}.f16",
        provider.name().to_lowercase(),
        std::env::var("BENCH_TAG")
            .map(|t| format!(".{t}"))
            .unwrap_or_default()
    ));
    let mut bytes = Vec::with_capacity(denoised.len() * 2);
    for &v in &denoised {
        bytes.extend_from_slice(&half::f16::from_f32(v).to_le_bytes());
    }
    std::fs::write(&target, bytes)?;
    let mut line = format!(
        "{stem}\t{provider}\tthreads {threads:?}\t{w}x{h}\tload {load_s:.2} s\tdenoise {run_s:.2} s\tpeak RSS {:.0} MB",
        peak_rss_mb()
    );
    if let Some(reference) = rest.get(1) {
        let reference = read_f16(Path::new(reference))?;
        anyhow::ensure!(reference.len() == denoised.len(), "reference size differs");
        let (mut max, mut sum, mut changed) = (0.0f64, 0.0f64, 0usize);
        for (a, b) in denoised.iter().zip(&reference) {
            let d = (*a as f64 - *b as f64).abs();
            max = max.max(d);
            sum += d;
            changed += (d > 0.0) as usize;
        }
        line += &format!(
            "\tvs reference: max {max:.2e} mean {:.2e} differing {:.2} %",
            sum / denoised.len() as f64,
            100.0 * changed as f64 / denoised.len() as f64
        );
    }
    println!("{line}");
    Ok(())
}
