// SPDX-License-Identifier: AGPL-3.0-or-later
//! AI Denoise: Nonlocal MatchFilter on the Bayer mosaic (rawtonic-denoise).
//!
//! The recipe's `aiDenoise` (0 … 100) is the amount; the decoder runs the
//! denoiser before demosaicing (rawdecode's mosaic_denoise hook), so the
//! photograph is developed again when the amount changes. Results are
//! cached by content in `<app data>/ai-denoise-cache`, which the
//! `rawtonic` CLI uses too.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rawtonic_rawdecode::mosaic_denoise::{MosaicDenoise, MosaicDenoiser};
use tauri::Manager;

use crate::app_state::AppState;

/// Files of the model pack (docs/MODELS.md). On macOS the three Core ML
/// files are put back together into the package the crate loads.
#[cfg(target_os = "macos")]
const CORE_ML_FILES: [(&str, &str, &str); 3] = [
    (
        "nlmf_coreml_Manifest.json",
        "Manifest.json",
        "434aa032d34804d4de386d2e6aeee6a0cb1b37a2e244c5819e6163238141b9d3",
    ),
    (
        "nlmf_coreml_model.mlmodel",
        "Data/com.apple.CoreML/model.mlmodel",
        "92f31612c9f526395f7c6cad010d157e94663e77394c16fff755126fb176aa9b",
    ),
    (
        "nlmf_coreml_weight.bin",
        "Data/com.apple.CoreML/weights/weight.bin",
        "a5bf7b0b9d7efc24b3141f09b8c6fc7cadd61f46eea874a41326de8216a500a0",
    ),
];
#[cfg(not(target_os = "macos"))]
const ONNX_SHA256: &str = "ba788a97f247058ea03d14bbf2f289adc8f4666039f27d0d530e6f67d23dd73c";

/// Denoised mosaics kept at most (about 100 photos of 24 MP).
const CACHE_LIMIT_BYTES: u64 = 5 << 30;

pub fn cache_dir(app_handle: &tauri::AppHandle) -> Result<PathBuf, String> {
    Ok(app_handle
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("ai-denoise-cache"))
}

/// The recipe's amount, 0 … 1.
pub fn amount(adjustments: &serde_json::Value) -> f32 {
    rawtonic_engine::image_loader::ai_denoise_amount(adjustments)
}

/// The denoiser, downloading and loading the model on first use.
pub async fn denoiser(app_handle: &tauri::AppHandle) -> Result<Arc<dyn MosaicDenoiser>, String> {
    let state = app_handle.state::<AppState>();
    if let Some(denoiser) = state.ai_denoiser.lock().unwrap().clone() {
        return Ok(denoiser);
    }
    let _guard = state.ai_init_lock.lock().await;
    if let Some(denoiser) = state.ai_denoiser.lock().unwrap().clone() {
        return Ok(denoiser);
    }
    let models = crate::ai_processing::models_dir(app_handle).map_err(|e| e.to_string())?;
    let cache = cache_dir(app_handle)?;
    let filter = load(app_handle, &models, cache).await?;
    let denoiser: Arc<dyn MosaicDenoiser> = Arc::new(filter);
    *state.ai_denoiser.lock().unwrap() = Some(denoiser.clone());
    Ok(denoiser)
}

#[cfg(target_os = "macos")]
async fn load(
    app_handle: &tauri::AppHandle,
    models: &Path,
    cache: PathBuf,
) -> Result<rawtonic_denoise::MatchFilter, String> {
    let package = models.join(rawtonic_denoise::CORE_ML_PACKAGE);
    for (file, inner, sha) in CORE_ML_FILES {
        crate::ai_processing::download_verified(app_handle, models, file, sha, "AI Denoise model")
            .await
            .map_err(|e| e.to_string())?;
        let target = package.join(inner);
        let source = models.join(file);
        let same = std::fs::metadata(&target)
            .ok()
            .zip(std::fs::metadata(&source).ok())
            .is_some_and(|(a, b)| a.len() == b.len());
        if !same {
            std::fs::create_dir_all(target.parent().unwrap()).map_err(|e| e.to_string())?;
            let staging = target.with_extension("partial");
            std::fs::copy(&source, &staging).map_err(|e| e.to_string())?;
            std::fs::rename(&staging, &target).map_err(|e| e.to_string())?;
            // A changed package needs compiling again.
            let _ = std::fs::remove_dir_all(package.with_extension("mlmodelc"));
        }
    }
    tokio::task::spawn_blocking(move || {
        rawtonic_denoise::MatchFilter::core_ml(&package, Some(cache))
            .map(|f| f.with_cache_limit(CACHE_LIMIT_BYTES))
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(not(target_os = "macos"))]
async fn load(
    app_handle: &tauri::AppHandle,
    models: &Path,
    cache: PathBuf,
) -> Result<rawtonic_denoise::MatchFilter, String> {
    crate::ai_processing::ensure_runtime().map_err(|e| e.to_string())?;
    let file = rawtonic_denoise::ONNX_MODEL;
    crate::ai_processing::download_verified(
        app_handle,
        models,
        file,
        ONNX_SHA256,
        "AI Denoise model",
    )
    .await
    .map_err(|e| e.to_string())?;
    let model = models.join(file);
    tokio::task::spawn_blocking(move || {
        rawtonic_denoise::MatchFilter::onnx(&model, Some(cache))
            .map(|f| f.with_cache_limit(CACHE_LIMIT_BYTES))
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// The decoder option for a recipe: `None` without AI Denoise.
pub async fn option_for(
    app_handle: &tauri::AppHandle,
    adjustments: &serde_json::Value,
) -> Result<Option<MosaicDenoise>, String> {
    let amount = amount(adjustments);
    if amount <= 0.0 {
        return Ok(None);
    }
    Ok(Some(MosaicDenoise::new(
        denoiser(app_handle).await?,
        amount,
    )))
}
