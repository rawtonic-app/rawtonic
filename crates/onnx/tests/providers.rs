// SPDX-License-Identifier: AGPL-3.0-or-later
//! GPU providers against the CPU on the real models. Ignored: they need
//! ONNX Runtime (`ORT_DYLIB_PATH`, 1.24.4 or later for plugins), the plugin
//! library in `RAWTONIC_ORT_PLUGIN_DIR` (or beside the runtime) and the
//! model pack in `AI_MODELS`. `RAWTONIC_AI_PROVIDER` picks the GPU provider
//! (default WebGPU); `AI_ONLY` limits the files (comma-separated prefixes).
//!
//! cargo test -p rawtonic-onnx --test providers -- --ignored --nocapture

use std::path::PathBuf;

use rawtonic_onnx::bench::{Picture, bench_file, model_files};
use rawtonic_onnx::{Model, ModelKind, Provider, default_plugin_dirs, forced_provider};

fn models() -> PathBuf {
    PathBuf::from(std::env::var("AI_MODELS").expect("AI_MODELS"))
}

fn gpu() -> Provider {
    forced_provider()
        .filter(|p| *p != Provider::Cpu)
        .unwrap_or(Provider::WebGpu)
}

#[test]
#[ignore = "needs ORT_DYLIB_PATH, a GPU plugin and AI_MODELS"]
fn every_model_on_the_gpu_matches_the_cpu() {
    let only: Vec<String> = std::env::var("AI_ONLY")
        .map(|v| v.split(',').map(str::to_string).collect())
        .unwrap_or_default();
    let picture = Picture::synthetic(1536, 1024);
    let dirs = default_plugin_dirs();
    let mut failures = Vec::new();
    for path in model_files(&models(), false).unwrap() {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        if !only.is_empty() && !only.iter().any(|o| name.starts_with(o.as_str())) {
            continue;
        }
        let report = bench_file(&path, gpu(), 2, &dirs, &picture).unwrap();
        println!(
            "{name}: CPU load {:.0} ms, first {:.0} ms, median {:.0} ms",
            report.cpu.load_ms,
            report.cpu.first_run_ms,
            report.cpu.median_ms().unwrap_or(0.0)
        );
        match (&report.gpu, &report.gpu_error) {
            (Some(timing), _) => println!(
                "{name}: {} ({}) load {:.0} ms, first {:.0} ms, median {:.0} ms",
                report.gpu_provider,
                report.gpu_device.as_deref().unwrap_or("?"),
                timing.load_ms,
                timing.first_run_ms,
                timing.median_ms().unwrap_or(0.0)
            ),
            (None, Some(e)) => failures.push(format!("{name}: no GPU run: {e}")),
            (None, None) => failures.push(format!("{name}: no GPU run")),
        }
        for a in &report.agreement {
            println!(
                "{name}: {} max abs {:.3e}, mean abs {:.3e}, max/range {:.3e}{}",
                a.output,
                a.max_abs,
                a.mean_abs,
                a.max_rel_range,
                a.iou.map(|v| format!(", IoU {v:.5}")).unwrap_or_default()
            );
            // Masks must agree; numbers within 1 % of the output's range
            // (MI-GAN's uint8 output: a few levels).
            let bad = !a.finite
                || a.iou.is_some_and(|v| v < 0.99)
                || (report.kind != ModelKind::Inpaint && a.max_rel_range > 1e-2)
                || (report.kind == ModelKind::Inpaint && a.mean_abs > 2.0);
            if bad {
                failures.push(format!("{name}: {} disagrees: {a:?}", a.output));
            }
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// Loads every model as the app does (the GPU first, checked against the
/// CPU once) and prints where it landed; a model whose GPU output differs
/// must land on the CPU.
#[test]
#[ignore = "needs ORT_DYLIB_PATH, a GPU plugin and AI_MODELS"]
fn models_run_on_the_gpu_only_where_it_matches_the_cpu() {
    let dirs = default_plugin_dirs();
    for path in model_files(&models(), false).unwrap() {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let kind = ModelKind::from_file_name(&name).unwrap();
        let model = Model::load_on(&path, kind, &[gpu(), Provider::Cpu], &dirs).unwrap();
        println!("{name}: runs on {}", model.provider());
    }
}

#[test]
#[ignore = "needs ORT_DYLIB_PATH, a GPU plugin and AI_MODELS"]
fn a_failed_gpu_run_moves_the_model_to_the_cpu() {
    let path = models().join("face_detection_yunet_2023mar.onnx");
    let model = Model::load_on(
        &path,
        ModelKind::FaceDetector,
        &[gpu(), Provider::Cpu],
        &default_plugin_dirs(),
    )
    .unwrap();
    assert_eq!(model.provider(), gpu(), "the model should load on the GPU");
    let mut calls = 0;
    let value = model
        .run(|_session| {
            calls += 1;
            if calls == 1 {
                Err(anyhow::anyhow!("simulated device loss"))
            } else {
                Ok(calls)
            }
        })
        .unwrap();
    assert_eq!(value, 2);
    assert_eq!(model.provider(), Provider::Cpu);
}

#[test]
#[ignore = "needs ORT_DYLIB_PATH and AI_MODELS"]
fn a_provider_without_its_library_falls_back_to_the_cpu() {
    let path = models().join("face_detection_yunet_2023mar.onnx");
    let empty = std::env::temp_dir().join("rawtonic-onnx-empty-plugin-dir");
    std::fs::create_dir_all(&empty).unwrap();
    let model = Model::load_on(
        &path,
        ModelKind::FaceDetector,
        &[Provider::Cuda, Provider::Cpu],
        &[empty],
    )
    .unwrap();
    assert_eq!(model.provider(), Provider::Cpu);
}
