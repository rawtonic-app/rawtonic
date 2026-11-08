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
async fn model(app_handle: &tauri::AppHandle, download: bool) -> anyhow::Result<Option<Arc<Model>>> {
    let ai_state = app_handle.state::<AiState>();
    let mut slot = ai_state.super_resolution.lock().await;
    if let Some(model) = slot.as_ref() {
        return Ok(Some(model.clone()));
    }
    let models_dir = get_models_dir(app_handle)?;
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
    }
    let model = tokio::task::spawn_blocking(move || Model::onnx(&path)).await??;
    log::info!(
        "Super Resolution: model loaded on {} in {} ms",
        model.backend_name(),
        start.elapsed().as_millis()
    );
    crate::register_exit_handler();
    Ok(Some(slot.insert(Arc::new(model)).clone()))
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
