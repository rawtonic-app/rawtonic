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
        download,
        async || download && fetch_gpu_graph(app_handle, &models_dir).await,
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
        fetch_gpu_graph(app_handle, &models_dir).await;
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
    log::info!(
        "Super Resolution: model loaded on {} in {} ms",
        model.backend_name(),
        start.elapsed().as_millis()
    );
    crate::register_exit_handler();
    Ok(Some(slot.insert(Arc::new(model)).clone()))
}

/// One refresh of the loaded model at a time (`refreshed`), apart from the
/// model's own lock so exports never wait for it.
static REFRESH: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// The loaded model (`None`: nothing loaded yet), replaced by `reload`'s
/// when `fetch` (the optional fp16 download) installed a graph now or
/// `ready` finds one that passed its GPU check since. `slot`'s lock is not
/// held while `fetch` and `reload` run, so export workers keep running on
/// the loaded model during a slow download; a failed reload keeps it.
/// Refreshes take turns through `refresh`: a call that `wait`s (the Export
/// dialog) runs after the one in progress, on the model it left; one that
/// does not (an export worker) keeps the loaded model meanwhile, so
/// workers never build a second session of the same graph.
async fn refreshed<M>(
    slot: &tokio::sync::Mutex<Option<Arc<M>>>,
    refresh: &tokio::sync::Mutex<()>,
    wait: bool,
    fetch: impl AsyncFnOnce() -> bool,
    ready: impl AsyncFnOnce(Arc<M>) -> anyhow::Result<bool>,
    reload: impl AsyncFnOnce() -> anyhow::Result<M>,
) -> anyhow::Result<Option<Arc<M>>> {
    let Some(loaded) = slot.lock().await.clone() else {
        return Ok(None);
    };
    let _turn = if wait {
        refresh.lock().await
    } else {
        match refresh.try_lock() {
            Ok(turn) => turn,
            Err(_) => return Ok(Some(loaded)),
        }
    };
    // The refresh this one waited for may have replaced it.
    let Some(current) = slot.lock().await.clone() else {
        return Ok(None);
    };
    if !fetch().await && !ready(current.clone()).await? {
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
        let enlarged = model.enlarge(image, cancelled)?;
        log::info!(
            "Super Resolution: {width}x{height} enlarged on {} in {} ms",
            model.backend_name(),
            started.elapsed().as_millis()
        );
        Ok(enlarged)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A slow retry of the optional fp16 download (Export opened again
    /// while a batch runs) must not hold the model's lock: export workers
    /// take the loaded model meanwhile, and the reloaded one replaces it
    /// afterwards.
    #[tokio::test]
    async fn retry_of_the_fp16_download_leaves_the_loaded_model_to_exports() {
        let slot = tokio::sync::Mutex::new(Some(Arc::new(32u32)));
        let turns = tokio::sync::Mutex::new(());
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (finish_tx, finish_rx) = tokio::sync::oneshot::channel::<()>();
        let refresh = refreshed(
            &slot,
            &turns,
            true,
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
            let during = tokio::time::timeout(std::time::Duration::from_secs(5), slot.lock())
                .await
                .expect("an export waits for the download")
                .clone();
            finish_tx.send(()).unwrap();
            during
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
            true,
            async || false,
            async |_| Ok(false),
            async || panic!("nothing to reload"),
        );
        assert_eq!(kept.await.unwrap().as_deref(), Some(&32));
        let kept = refreshed(
            &slot,
            &turns,
            true,
            async || true,
            async |_| Ok(false),
            async || anyhow::bail!("graph unreadable"),
        );
        assert_eq!(kept.await.unwrap().as_deref(), Some(&32));
        let empty = tokio::sync::Mutex::new(None::<Arc<u32>>);
        let none = refreshed(
            &empty,
            &turns,
            true,
            async || panic!("not loaded"),
            async |_| Ok(true),
            async || Ok(1),
        );
        assert!(none.await.unwrap().is_none());
    }

    /// Export opened twice during a slow retry: the second call waits for
    /// the first one's download and reload and finds nothing left to do;
    /// an export worker meanwhile keeps the loaded model without a check
    /// or a session of its own.
    #[tokio::test]
    async fn overlapping_refreshes_share_one_download_and_one_reload() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let slot = tokio::sync::Mutex::new(Some(Arc::new(32u32)));
        let turns = tokio::sync::Mutex::new(());
        let (fetches, reloads) = (AtomicUsize::new(0), AtomicUsize::new(0));
        let installed = std::sync::atomic::AtomicBool::new(false);
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (finish_tx, finish_rx) = tokio::sync::oneshot::channel::<()>();
        let dialog = |started: Option<tokio::sync::oneshot::Sender<()>>,
                      finish: Option<tokio::sync::oneshot::Receiver<()>>| {
            let (fetches, reloads, installed) = (&fetches, &reloads, &installed);
            refreshed(
                &slot,
                &turns,
                true,
                async move || {
                    fetches.fetch_add(1, Ordering::SeqCst);
                    if let (Some(started), Some(finish)) = (started, finish) {
                        started.send(()).unwrap();
                        finish.await.unwrap();
                    }
                    // Downloaded now, or already installed.
                    !installed.swap(true, Ordering::SeqCst)
                },
                async |_| Ok(false),
                async move || {
                    reloads.fetch_add(1, Ordering::SeqCst);
                    Ok(16u32)
                },
            )
        };
        let first = dialog(Some(started_tx), Some(finish_rx));
        let second = dialog(None, None);
        let worker_and_release = async {
            started_rx.await.unwrap();
            let worker = refreshed(
                &slot,
                &turns,
                false,
                async || panic!("workers do not download"),
                async |_| panic!("no check while a refresh runs"),
                async || panic!("no second session"),
            )
            .await
            .unwrap();
            finish_tx.send(()).unwrap();
            worker
        };
        let (first, second, worker) = tokio::join!(first, second, worker_and_release);
        assert_eq!(worker.as_deref(), Some(&32));
        assert_eq!(first.unwrap().as_deref(), Some(&16));
        assert_eq!(second.unwrap().as_deref(), Some(&16));
        assert_eq!(reloads.load(Ordering::SeqCst), 1);
        assert_eq!(fetches.load(Ordering::SeqCst), 2);
    }
}
