// SPDX-License-Identifier: AGPL-3.0-or-later
//! Super Resolution in the app: the export option that enlarges each photo
//! 2× (`rawtonic_superres`). Ticking it in the Export dialog fetches the
//! model (`prepare_super_resolution`); an export only uses an installed one
//! and fails that photo with a clear error otherwise, as exports with AI
//! selections do.

use std::sync::Arc;

use rawtonic_superres::Model;
use tauri::Manager;

use crate::ai_processing::{
    download_verified, ensure_local_runtime, get_models_dir, verify_sha256,
};
use crate::state::ai::AiState;

/// The model, loaded once: on macOS the Core ML program when its files are
/// installed (with `download`, fetched first), else the ONNX model on ONNX
/// Runtime. `Ok(None)` when it is not installed and `download` is false.
async fn model(
    app_handle: &tauri::AppHandle,
    download: bool,
) -> anyhow::Result<Option<Arc<Model>>> {
    let ai_state = app_handle.state::<AiState>();
    let models_dir = get_models_dir(app_handle)?;
    // The fp16 graph's idle GPU check passed after this model loaded, or
    // its download failed before and works now (opening Export again): the
    // next photo loads it again (exports hold the checks, so a batch does
    // not switch between photos once it has started).
    let reload_path = models_dir.join(rawtonic_superres::ONNX_FILE);
    if let Some(model) = refreshed(
        &ai_state.super_resolution,
        &REFRESH,
        async || {
            download && shared_attempt(&GPU_FETCH, async || fetch_gpu_graph(app_handle, &models_dir).await).await
        },
        async |current: Arc<Model>| {
            Ok(tokio::task::spawn_blocking(move || current.gpu_graph_ready()).await?)
        },
        async move || {
            let start = std::time::Instant::now();
            let reloaded = tokio::task::spawn_blocking(move || Model::onnx(&reload_path)).await??;
            log::info!(
                "Super Resolution: model reloaded on {} in {} ms (GPU graph fetched or its check passed)",
                reloaded.backend_name(),
                start.elapsed().as_millis()
            );
            Ok(reloaded)
        },
    )
    .await?
    {
        return Ok(Some(model));
    }
    let mut slot = ai_state.super_resolution.lock().await;
    if let Some(model) = slot.as_ref() {
        // Loaded by another call while this one waited for the lock.
        return Ok(Some(model.clone()));
    }
    let start = std::time::Instant::now();
    #[cfg(target_os = "macos")]
    if let Some(core_ml) = crate::ai_processing::load_core_ml(
        app_handle,
        &models_dir,
        rawtonic_superres::CORE_ML_PACKAGE,
        &rawtonic_superres::CORE_ML_FILES,
        download,
        "super-resolution",
        "Super Resolution Model",
    )
    .await
    {
        log::info!(
            "Super Resolution: Core ML model loaded in {} ms",
            start.elapsed().as_millis()
        );
        return Ok(Some(slot.insert(Arc::new(Model::core_ml(core_ml))).clone()));
    }
    let path = models_dir.join(rawtonic_superres::ONNX_FILE);
    if !download && !verify_sha256(&path, rawtonic_superres::ONNX_SHA256)? {
        return Ok(None);
    }
    ensure_local_runtime()?;
    if download {
        download_verified(
            app_handle,
            &models_dir,
            rawtonic_superres::ONNX_FILE,
            rawtonic_superres::ONNX_SHA256,
            "Super Resolution Model",
        )
        .await?;
        shared_attempt(&GPU_FETCH, async || {
            fetch_gpu_graph(app_handle, &models_dir).await
        })
        .await;
    }
    let mut model = tokio::task::spawn_blocking(move || Model::onnx(&path)).await??;
    if download
        && model.uses_cpu()
        && !verify_sha256(
            &models_dir.join(rawtonic_superres::CPU_ONNX_FILE),
            rawtonic_superres::CPU_ONNX_SHA256,
        )?
    {
        match download_verified(
            app_handle,
            &models_dir,
            rawtonic_superres::CPU_ONNX_FILE,
            rawtonic_superres::CPU_ONNX_SHA256,
            "Super Resolution CPU Model",
        )
        .await
        {
            Ok(_) => {
                let cpu_path = models_dir.join(rawtonic_superres::CPU_ONNX_FILE);
                // Added beside the original graph, which stays free to move
                // to a GPU in this session (the idle GPU check).
                let (with_cpu, attached) = tokio::task::spawn_blocking(move || {
                    let attached = model.attach_cpu_graph(&cpu_path);
                    (model, attached)
                })
                .await?;
                model = with_cpu;
                if let Err(error) = attached {
                    log::warn!(
                        "Optional Super Resolution CPU graph could not load; using the original graph: {error:#}"
                    );
                }
            }
            Err(error) => log::warn!(
                "Optional Super Resolution CPU graph unavailable; using the original graph: {error:#}"
            ),
        }
    }
    if download && model.uses_cpu() {
        model = attach_cpu_single(app_handle, &models_dir, model).await?;
    }
    log::info!(
        "Super Resolution: model loaded on {} in {} ms",
        model.backend_name(),
        start.elapsed().as_millis()
    );
    crate::register_exit_handler();
    Ok(Some(slot.insert(Arc::new(model)).clone()))
}

/// Downloads (when missing) and adds the single-rotation CPU graph
/// (`CPU_SINGLE_ONNX_FILE`, 2.4 MB) to a model that runs on the CPU, so
/// recipes from `renderVersion` 20 enlarge each tile once there. Optional:
/// without it the CPU runs the four-rotation average.
async fn attach_cpu_single(
    app_handle: &tauri::AppHandle,
    models_dir: &std::path::Path,
    model: Model,
) -> anyhow::Result<Model> {
    if let Err(error) = download_verified(
        app_handle,
        models_dir,
        rawtonic_superres::CPU_SINGLE_ONNX_FILE,
        rawtonic_superres::CPU_SINGLE_ONNX_SHA256,
        "Super Resolution CPU Model",
    )
    .await
    {
        log::warn!(
            "Optional Super Resolution single-pass CPU graph unavailable; the CPU averages four rotations: {error:#}"
        );
        return Ok(model);
    }
    let path = models_dir.join(rawtonic_superres::CPU_SINGLE_ONNX_FILE);
    let (model, attached) = tokio::task::spawn_blocking(move || {
        let mut model = model;
        let attached = model.attach_cpu_single_graph(&path);
        (model, attached)
    })
    .await?;
    if let Err(error) = attached {
        log::warn!(
            "Optional Super Resolution single-pass CPU graph could not load; the CPU averages four rotations: {error:#}"
        );
    }
    Ok(model)
}

/// Reloads of the loaded model take turns (`refreshed`), apart from the
/// model's own lock; nobody holds it during a download.
static REFRESH: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// The fp16 graph's download, shared by calls that overlap (`shared_attempt`).
static GPU_FETCH: Attempts = Attempts::new();

/// The loaded model (`None`: nothing loaded yet), replaced by `reload`'s
/// when `fetch` (the optional fp16 download; `false` for export workers)
/// installed a graph now or `ready` finds one that passed its GPU check
/// since. No lock is held while `fetch` runs, so export workers keep
/// running on the loaded model during a slow download. The check and the
/// reload take turns through `turns`: a call that finds the model replaced
/// while it waited takes the replacement, so the workers of one batch,
/// which reach Super Resolution independently, all run the graph the first
/// of them loaded. A failed reload keeps the loaded model.
async fn refreshed<M>(
    slot: &tokio::sync::Mutex<Option<Arc<M>>>,
    turns: &tokio::sync::Mutex<()>,
    fetch: impl AsyncFnOnce() -> bool,
    ready: impl AsyncFnOnce(Arc<M>) -> anyhow::Result<bool>,
    reload: impl AsyncFnOnce() -> anyhow::Result<M>,
) -> anyhow::Result<Option<Arc<M>>> {
    let Some(loaded) = slot.lock().await.clone() else {
        return Ok(None);
    };
    let fetched = fetch().await;
    let _turn = turns.lock().await;
    let Some(current) = slot.lock().await.clone() else {
        return Ok(None);
    };
    if !Arc::ptr_eq(&current, &loaded) || (!fetched && !ready(current.clone()).await?) {
        return Ok(Some(current));
    }
    let reloaded = match reload().await {
        Ok(reloaded) => Arc::new(reloaded),
        Err(error) => {
            log::warn!("Super Resolution: keeping the loaded model: {error:#}");
            return Ok(Some(current));
        }
    };
    Ok(Some(slot.lock().await.insert(reloaded).clone()))
}

/// Attempts of one optional download, one at a time; `done` counts the
/// finished ones and `last` holds the latest result.
struct Attempts {
    done: std::sync::atomic::AtomicU64,
    last: tokio::sync::Mutex<bool>,
}

impl Attempts {
    const fn new() -> Self {
        Self {
            done: std::sync::atomic::AtomicU64::new(0),
            last: tokio::sync::Mutex::const_new(false),
        }
    }
}

/// `attempt`'s result; a call that overlaps an attempt in progress (Export
/// closed and opened again during a slow download) takes that attempt's
/// result instead of waiting through another one, and a call after it
/// has finished tries again.
async fn shared_attempt(attempts: &Attempts, attempt: impl AsyncFnOnce() -> bool) -> bool {
    use std::sync::atomic::Ordering;
    let before = attempts.done.load(Ordering::SeqCst);
    let mut last = attempts.last.lock().await;
    if attempts.done.load(Ordering::SeqCst) != before {
        return *last;
    }
    *last = attempt().await;
    attempts.done.fetch_add(1, Ordering::SeqCst);
    *last
}

/// Downloads the fp16 graph for GPU providers (1.2 MB) when ONNX Runtime's
/// GPU providers may run it (Windows and Linux, GPU AI on) and it is not
/// installed; `true` when it was downloaded now. Optional: older packs
/// lack it, and without it the original graph runs. An unreadable
/// installed copy counts as missing (replaced only after the download's
/// SHA-256 check). PRIVACY.md names this download.
async fn fetch_gpu_graph(app_handle: &tauri::AppHandle, models_dir: &std::path::Path) -> bool {
    if !cfg!(any(target_os = "windows", target_os = "linux"))
        || rawtonic_onnx::models_run_on_cpu_only()
        || verify_sha256(
            &models_dir.join(rawtonic_superres::GPU_ONNX_FILE),
            rawtonic_superres::GPU_ONNX_SHA256,
        )
        .unwrap_or(false)
    {
        return false;
    }
    match download_verified(
        app_handle,
        models_dir,
        rawtonic_superres::GPU_ONNX_FILE,
        rawtonic_superres::GPU_ONNX_SHA256,
        "Super Resolution GPU Model",
    )
    .await
    {
        Ok(()) => true,
        Err(error) => {
            log::warn!(
                "Optional Super Resolution GPU graph unavailable; using the original graph: {error:#}"
            );
            false
        }
    }
}

/// Fetches and loads the model when Super Resolution is ticked in the
/// Export dialog, so the export itself downloads nothing. Returns the
/// backend it runs on.
#[tauri::command]
pub async fn prepare_super_resolution(app_handle: tauri::AppHandle) -> Result<String, String> {
    let model = model(&app_handle, true)
        .await
        .map_err(|e| format!("Super Resolution model unavailable: {e:#}"))?
        .ok_or_else(|| "Super Resolution model unavailable".to_string())?;
    Ok(model.backend_name())
}

/// Super Resolution for the export workers (outside the async runtime),
/// with the installed model only.
pub(crate) struct ExportEnlarger<'a> {
    pub app_handle: &'a tauri::AppHandle,
}

impl rawtonic_export::export_processing::Enlarger for ExportEnlarger<'_> {
    fn enlarge_2x(
        &self,
        image: image::DynamicImage,
        one_pass_on_cpu: bool,
        cancelled: &dyn Fn() -> Result<(), String>,
    ) -> Result<image::DynamicImage, String> {
        let model = tauri::async_runtime::block_on(model(self.app_handle, false))
            .map_err(|e| format!("Super Resolution model unavailable: {e:#}"))?
            .ok_or_else(|| {
                "the Super Resolution model is not installed (tick Super Resolution in the Export dialog again to download it)"
                    .to_string()
            })?;
        let started = std::time::Instant::now();
        let (width, height) = (image.width(), image.height());
        let rotations = rawtonic_superres::Rotations::of(one_pass_on_cpu);
        let enlarged = model.enlarge(image, rotations, cancelled)?;
        log::info!(
            "Super Resolution: {width}x{height} enlarged on {}{} in {} ms",
            model.backend_name(),
            if model.runs_one_pass(rotations) {
                ", one pass"
            } else {
                ""
            },
            started.elapsed().as_millis()
        );
        Ok(enlarged)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::oneshot;

    /// A slow retry of the optional fp16 download (Export opened again
    /// while a batch runs) must not hold the model's lock or the reload
    /// turns: export workers take the loaded model meanwhile, and the
    /// reloaded one replaces it afterwards.
    #[tokio::test]
    async fn retry_of_the_fp16_download_leaves_the_loaded_model_to_exports() {
        let slot = tokio::sync::Mutex::new(Some(Arc::new(32u32)));
        let turns = tokio::sync::Mutex::new(());
        let (started_tx, started_rx) = oneshot::channel();
        let (finish_tx, finish_rx) = oneshot::channel::<()>();
        let refresh = refreshed(
            &slot,
            &turns,
            async || {
                started_tx.send(()).unwrap();
                finish_rx.await.unwrap();
                true
            },
            async |_| panic!("a fetched graph needs no check"),
            async || Ok(16u32),
        );
        let export = async {
            started_rx.await.unwrap();
            let worker = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                refreshed(
                    &slot,
                    &turns,
                    async || false,
                    async |_| Ok(false),
                    async || panic!("nothing to reload"),
                ),
            )
            .await
            .expect("an export worker waits for the download")
            .unwrap();
            finish_tx.send(()).unwrap();
            worker
        };
        let (refreshed, during) = tokio::join!(refresh, export);
        assert_eq!(during.as_deref(), Some(&32));
        assert_eq!(refreshed.unwrap().as_deref(), Some(&16));
        assert_eq!(slot.lock().await.as_deref(), Some(&16));
    }

    #[tokio::test]
    async fn nothing_new_keeps_the_model_and_a_failed_reload_too() {
        let slot = tokio::sync::Mutex::new(Some(Arc::new(32u32)));
        let turns = tokio::sync::Mutex::new(());
        let kept = refreshed(
            &slot,
            &turns,
            async || false,
            async |_| Ok(false),
            async || panic!("nothing to reload"),
        );
        assert_eq!(kept.await.unwrap().as_deref(), Some(&32));
        let kept = refreshed(
            &slot,
            &turns,
            async || true,
            async |_| Ok(false),
            async || anyhow::bail!("graph unreadable"),
        );
        assert_eq!(kept.await.unwrap().as_deref(), Some(&32));
        let empty = tokio::sync::Mutex::new(None::<Arc<u32>>);
        let none = refreshed(
            &empty,
            &turns,
            async || false,
            async |_| Ok(true),
            async || Ok(1),
        );
        assert!(none.await.unwrap().is_none());
    }

    /// Two export workers reach Super Resolution while the first one
    /// reloads the model for the fp16 graph (its check passed while Export
    /// was open): the second waits for that reload and runs the same
    /// graph, so one batch never mixes graphs.
    #[tokio::test]
    async fn workers_of_one_batch_run_the_graph_the_first_one_loaded() {
        let slot = tokio::sync::Mutex::new(Some(Arc::new(32u32)));
        let turns = tokio::sync::Mutex::new(());
        let reloads = AtomicUsize::new(0);
        let (started_tx, started_rx) = oneshot::channel();
        let (finish_tx, finish_rx) = oneshot::channel::<()>();
        let first = refreshed(
            &slot,
            &turns,
            async || false,
            async |_| Ok(true),
            async || {
                reloads.fetch_add(1, Ordering::SeqCst);
                started_tx.send(()).unwrap();
                finish_rx.await.unwrap();
                Ok(16u32)
            },
        );
        let second = async {
            started_rx.await.unwrap();
            let second = refreshed(
                &slot,
                &turns,
                async || false,
                async |_| Ok(true),
                async || {
                    reloads.fetch_add(1, Ordering::SeqCst);
                    Ok(99u32)
                },
            );
            let releaser = async {
                tokio::task::yield_now().await;
                finish_tx.send(()).unwrap();
            };
            tokio::join!(second, releaser).0
        };
        let (first, second) = tokio::join!(first, second);
        assert_eq!(first.unwrap().as_deref(), Some(&16));
        assert_eq!(second.unwrap().as_deref(), Some(&16));
        assert_eq!(reloads.load(Ordering::SeqCst), 1);
    }

    /// Export opened again while a slow download fails: the second call
    /// takes the first one's result instead of a second slow attempt; a
    /// later, separate call tries again.
    #[tokio::test]
    async fn an_overlapping_call_shares_the_attempt_in_progress() {
        let attempts = Attempts::new();
        let tries = AtomicUsize::new(0);
        let (started_tx, started_rx) = oneshot::channel();
        let (finish_tx, finish_rx) = oneshot::channel::<()>();
        let first = shared_attempt(&attempts, async || {
            tries.fetch_add(1, Ordering::SeqCst);
            started_tx.send(()).unwrap();
            finish_rx.await.unwrap();
            false
        });
        let second = async {
            started_rx.await.unwrap();
            let second = shared_attempt(&attempts, async || {
                tries.fetch_add(1, Ordering::SeqCst);
                true
            });
            let releaser = async {
                tokio::task::yield_now().await;
                finish_tx.send(()).unwrap();
            };
            tokio::join!(second, releaser).0
        };
        let (first, second) = tokio::join!(first, second);
        assert_eq!((first, second), (false, false));
        assert_eq!(tries.load(Ordering::SeqCst), 1);
        let later = shared_attempt(&attempts, async || {
            tries.fetch_add(1, Ordering::SeqCst);
            true
        });
        assert!(later.await);
        assert_eq!(tries.load(Ordering::SeqCst), 2);
    }
}
