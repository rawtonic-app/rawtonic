// SPDX-License-Identifier: AGPL-3.0-or-later
//! A process for watching ONNX Runtime's network use from outside (PRIVACY.md:
//! the runtime must send nothing). It starts the runtime the app's way, loads
//! and runs `PROBE_MODEL` (default the YuNet face detector) with the app's
//! provider choice and the CPU, then stays alive `PROBE_SECS` seconds (default
//! 360) so a capture by PID (`nettop`, `lsof`, `log stream`, `ss`, Process
//! Monitor) covers any batched upload. With `RAWTONIC_AI_PROVIDER` set to a
//! GPU provider the model must load and run there (a plugin library carries
//! its own copy of the uploader on Linux). Ignored: needs ONNX Runtime
//! (`ORT_DYLIB_PATH`) and the model pack (`AI_MODELS`).
//!
//! cargo test -p rawtonic-onnx --test network_probe -- --ignored --nocapture

use std::path::PathBuf;
use std::time::{Duration, Instant};

use rawtonic_onnx::bench::{Picture, bench_file};
use rawtonic_onnx::{Model, Provider, ensure_runtime, forced_provider};

#[test]
#[ignore = "needs ORT_DYLIB_PATH and AI_MODELS; stays alive PROBE_SECS seconds"]
fn runtime_stays_alive_for_a_network_capture() {
    let models = PathBuf::from(std::env::var("AI_MODELS").expect("AI_MODELS"));
    let name = std::env::var("PROBE_MODEL")
        .unwrap_or_else(|_| "face_detection_yunet_2023mar.onnx".to_string());
    let seconds: u64 = std::env::var("PROBE_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(360);
    let path = models.join(&name);
    println!("probe pid {}", std::process::id());

    let started = Instant::now();
    ensure_runtime().expect("ONNX Runtime");
    println!(
        "probe runtime ready after {} ms",
        started.elapsed().as_millis()
    );

    let model = Model::load_file(&path).expect("model load");
    println!("probe {name} loaded on {}", model.provider().name());
    let gpu = forced_provider().unwrap_or(Provider::Cpu);
    if gpu != Provider::Cpu {
        assert_eq!(model.provider(), gpu, "the forced provider did not load");
    }
    let report = bench_file(&path, gpu, 1, &[], &Picture::synthetic(640, 480)).expect("inference");
    assert!(report.gpu_error.is_none(), "{:?}", report.gpu_error);
    println!(
        "probe {} CPU inference {:.1} ms{}; waiting {seconds} s",
        report.file,
        report.cpu.first_run_ms,
        report
            .gpu
            .as_ref()
            .map(|t| format!(", {} {:.1} ms", gpu.name(), t.first_run_ms))
            .unwrap_or_default()
    );
    std::thread::sleep(Duration::from_secs(seconds));
    drop(model);
    println!("probe done after {} s", started.elapsed().as_secs());
}
