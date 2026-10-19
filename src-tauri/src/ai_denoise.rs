// SPDX-License-Identifier: AGPL-3.0-or-later
//! AI Denoise: Nonlocal MatchFilter on the Bayer mosaic (rawtonic-denoise).
//!
//! The recipe's `aiDenoise` (0 … 100) and `aiDenoiseVersion` decide how
//! (`ai_denoise_settings`); the decoder runs the denoiser before
//! demosaicing (rawdecode's mosaic_denoise hook), so the photograph is
//! developed again when they change. Results are
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
/// The mixed-precision 640 px package of version 3, for macOS 13 and later
/// like the app (scripts/models/convert_nlmf_coreml.py; in the packs from
/// pack-2026-10-18).
#[cfg(target_os = "macos")]
const CORE_ML_MIXED_FILES: [(&str, &str, &str); 3] = [
    (
        "nlmf_mixed640_macos13_coreml_Manifest.json",
        "Manifest.json",
        "935b1ef0c97fe51b4999919c28b9c91151c648c72571c1d5c3c49dd6e9b3b184",
    ),
    (
        "nlmf_mixed640_macos13_coreml_model.mlmodel",
        "Data/com.apple.CoreML/model.mlmodel",
        "e466563ad1dba0ee8d143b43c930dee9852b77e2cbb492f7b89cde991044c164",
    ),
    (
        "nlmf_mixed640_macos13_coreml_weight.bin",
        "Data/com.apple.CoreML/weights/weight.bin",
        "e50e91a84be8ca50f58566020eae4dc0bcd165e77c2c999bab418a4c492e3fe9",
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

/// Where `AppState::ai_denoiser` keeps the denoiser of `model`
/// (`AiDenoiseSettings::model`): ONNX Runtime (Windows, Linux) has one model
/// for every version.
pub fn slot(model: usize) -> usize {
    if cfg!(target_os = "macos") {
        model.min(1)
    } else {
        0
    }
}

/// The denoiser of AI Denoise `model` (`AiDenoiseSettings::model`: 0 the
/// first package, 1 the mixed-precision one), downloading and loading it on
/// first use. No fallback between them: a version renders with its model or
/// fails, so a saved recipe never changes with the installed models.
pub async fn denoiser(
    app_handle: &tauri::AppHandle,
    model: usize,
) -> Result<Arc<dyn MosaicDenoiser>, String> {
    let slot = slot(model);
    let state = app_handle.state::<AppState>();
    if let Some(denoiser) = state.ai_denoiser.lock().unwrap()[slot].clone() {
        return Ok(denoiser);
    }
    let _guard = state.ai_init_lock.lock().await;
    if let Some(denoiser) = state.ai_denoiser.lock().unwrap()[slot].clone() {
        return Ok(denoiser);
    }
    let models = crate::ai_processing::models_dir(app_handle).map_err(|e| e.to_string())?;
    let cache = cache_dir(app_handle)?;
    #[cfg(target_os = "macos")]
    let denoiser: Arc<dyn MosaicDenoiser> = if slot == 0 {
        Arc::new(load(app_handle, &models, cache).await?)
    } else {
        Arc::new(load_mixed(app_handle, &models, cache).await?)
    };
    #[cfg(not(target_os = "macos"))]
    let denoiser: Arc<dyn MosaicDenoiser> = Arc::new(load(app_handle, &models, cache).await?);
    state.ai_denoiser.lock().unwrap()[slot] = Some(denoiser.clone());
    Ok(denoiser)
}

/// Downloads the files of a Core ML package from the pack and puts the
/// package back together in the model folder, each file checked by its
/// SHA-256 (`ai_processing::assemble_package`).
#[cfg(target_os = "macos")]
async fn assemble_package(
    app_handle: &tauri::AppHandle,
    models: &Path,
    package: &Path,
    files: &[(&str, &str, &str)],
) -> Result<(), String> {
    crate::ai_processing::assemble_package(
        app_handle,
        models,
        package,
        files,
        true,
        "AI Denoise model",
    )
    .await
    .map(|_| ())
    .map_err(|e| e.to_string())
}

#[cfg(target_os = "macos")]
async fn load_mixed(
    app_handle: &tauri::AppHandle,
    models: &Path,
    cache: PathBuf,
) -> Result<rawtonic_denoise::MatchFilter, String> {
    let package = models.join(rawtonic_denoise::CORE_ML_MIXED_PACKAGE);
    assemble_package(app_handle, models, &package, &CORE_ML_MIXED_FILES).await?;
    tokio::task::spawn_blocking(move || {
        rawtonic_denoise::MatchFilter::core_ml_mixed(&package, Some(cache))
            .map(|f| f.with_cache_limit(CACHE_LIMIT_BYTES))
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(target_os = "macos")]
async fn load(
    app_handle: &tauri::AppHandle,
    models: &Path,
    cache: PathBuf,
) -> Result<rawtonic_denoise::MatchFilter, String> {
    let package = models.join(rawtonic_denoise::CORE_ML_PACKAGE);
    assemble_package(app_handle, models, &package, &CORE_ML_FILES).await?;
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
    let Some(ai) = rawtonic_engine::image_loader::ai_denoise_settings(adjustments) else {
        return Ok(None);
    };
    Ok(Some(
        MosaicDenoise::new(denoiser(app_handle, ai.model()).await?, ai.blend)
            .with_noise_scale(ai.noise_scale),
    ))
}

#[cfg(test)]
mod tests {
    #[test]
    fn every_model_has_a_slot_and_onnx_shares_one() {
        assert_eq!(super::slot(0), 0);
        let mixed = super::slot(1);
        assert_eq!(mixed, if cfg!(target_os = "macos") { 1 } else { 0 });
        assert!(super::slot(7) < 2);
    }
}
