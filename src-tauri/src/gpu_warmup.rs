//! `rawtonic-app --warm-gpu-cache`: compiles the pipelines a start compiles
//! ahead into the app's own pipeline cache file (`rawtonic_gpu::
//! pipeline_cache`) and exits, without a window or webview. The Windows
//! installer runs it after an install or update (`windows/installer-hooks.nsh`),
//! as the installing user, so the first launch finds the file filled instead
//! of waiting 7–9 s for the RAW default display variant on a cold NVIDIA
//! driver cache (notes: benchmark-results/windows-gpu-vfio-2026-10-30/
//! COLD-START-OPTIONS.md B1, RESULTS-2.md W2/W4).
//!
//! The device starts exactly as the app's (`gpu_processing::
//! get_or_init_gpu_context`: the backend chosen in Preferences, the backend
//! fallback, the same cache folder and file name). Where the backend keeps no
//! pipeline cache (Metal, DX12, GL) there is nothing to fill and it exits
//! after creating the device. Bounded by `LIMIT`; whatever compiled by then
//! is in the file (each compile is written as it finishes). It logs to
//! `gpu-warmup.log` beside `app.log` and never touches the user's photos,
//! settings or the app's own log. Exit code 0 when the file is filled or
//! nothing was needed, 1 otherwise; the installer ignores it.

use std::time::{Duration, Instant};

use tauri::Manager;

use crate::app_state::AppState;

pub const ARG: &str = "--warm-gpu-cache";

/// Longer than every start pipeline takes on a cold NVIDIA driver cache in
/// the 6-vCPU Windows VM (general pipelines ≈ 47 s, compiled in parallel;
/// RESULTS-2.md W3).
const LIMIT: Duration = Duration::from_secs(120);

pub fn start(app_handle: &tauri::AppHandle) {
    let settings = crate::app_settings::load_settings(app_handle.clone()).unwrap_or_default();
    crate::startup::apply_processing_backend(settings.processing_backend.as_deref());
    crate::startup::setup_logging(app_handle, "gpu-warmup.log");
    let started = Instant::now();
    // A compile stuck in the driver must not hold the installer.
    let spawned = std::thread::Builder::new()
        .name("gpu-warmup-limit".into())
        .spawn(move || {
            std::thread::sleep(LIMIT);
            log::warn!(
                "GPU warm-up: stopped after {} s; the pipelines compiled so far are in the cache",
                LIMIT.as_secs()
            );
            crate::startup::flush_logs();
            std::process::exit(1);
        });
    if let Err(error) = spawned {
        log::warn!("GPU warm-up: no time limit ({error})");
    }
    let app_handle = app_handle.clone();
    let spawned = std::thread::Builder::new()
        .name("gpu-warmup".into())
        .spawn(move || {
            let code = match warm(&app_handle, started) {
                Ok(outcome) => {
                    log::info!(
                        "GPU warm-up: {outcome} after {:.1} s",
                        started.elapsed().as_secs_f64()
                    );
                    0
                }
                Err(error) => {
                    log::warn!(
                        "GPU warm-up failed after {:.1} s: {error}",
                        started.elapsed().as_secs_f64()
                    );
                    1
                }
            };
            crate::startup::flush_logs();
            app_handle.exit(code);
        });
    if let Err(error) = spawned {
        log::warn!("GPU warm-up failed: {error}");
        crate::startup::flush_logs();
        std::process::exit(1);
    }
}

fn warm(app_handle: &tauri::AppHandle, started: Instant) -> Result<String, String> {
    let state = app_handle.state::<AppState>();
    let context = crate::gpu_processing::get_or_init_gpu_context(&state, app_handle)?;
    if !context
        .device
        .features()
        .contains(wgpu::Features::PIPELINE_CACHE)
    {
        return Ok("nothing to do, the backend keeps no pipeline cache".into());
    }
    crate::gpu_processing::prewarm_gpu_processor(&state.gpu_processor, &context);
    {
        let processor = state
            .gpu_processor
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let processor = &processor
            .as_ref()
            .ok_or("the GPU processor could not be created")?
            .processor;
        while !processor.start_pipelines_settled() {
            if started.elapsed() >= LIMIT {
                return Err("time limit reached".into());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    match rawtonic_gpu::pipeline_cache::save_now(&context.device) {
        Some((path, bytes)) => Ok(format!(
            "pipeline cache {} holds {bytes} bytes",
            path.display()
        )),
        None => Err("the pipeline cache has no data".into()),
    }
}
