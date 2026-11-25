// SPDX-License-Identifier: AGPL-3.0-or-later
//! AI Denoise: Nonlocal MatchFilter on the Bayer mosaic (rawtonic-denoise).
//!
//! The recipe's `aiDenoise` (0 … 100) and `aiDenoiseVersion` decide how
//! (`ai_denoise_settings`); the decoder runs the denoiser before
//! demosaicing (rawdecode's mosaic_denoise hook), so the photograph is
//! developed again when they change. Results are
//! cached by content in `<app data>/ai-denoise-cache`, which the
//! `rawtonic` CLI uses too.

use crate::ipc::CommandError;
use crate::state::ai::AiState;
use crate::state::lock::lock_recover;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rawtonic_engine::image_loader::FAST_AI_DENOISE_MODEL;
use rawtonic_rawdecode::mosaic_denoise::{MosaicDenoise, MosaicDenoiser};
use tauri::Manager;

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
/// The Fast network (`rawtonic_denoise::FAST_ONNX_MODEL`, CC BY-SA 4.0; in
/// the packs from pack-2026-11-25b), on ONNX Runtime on every platform.
const FAST_ONNX_SHA256: &str = "95e4e3f168d98f030b25f4f0c7d44337cc168d80d63af48227357d6f616914a6";

pub fn cache_dir(app_handle: &tauri::AppHandle) -> Result<PathBuf, String> {
    Ok(app_handle
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("ai-denoise-cache"))
}

/// Where `AiState::denoisers` keeps the denoiser of `model`
/// (`AiDenoiseSettings::model`): ONNX Runtime (Windows, Linux) has one
/// MatchFilter model for every version; the Fast network has its own slot.
pub fn slot(model: usize) -> usize {
    if model == FAST_AI_DENOISE_MODEL {
        FAST_AI_DENOISE_MODEL
    } else if cfg!(target_os = "macos") {
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
) -> Result<Arc<dyn MosaicDenoiser>, CommandError> {
    let ai_state = app_handle.state::<AiState>();
    let slot = slot(model);
    if let Some(denoiser) = lock_recover(&ai_state.denoisers)[slot].clone() {
        return Ok(denoiser);
    }
    let _guard = ai_state.init_lock.lock().await;
    if let Some(denoiser) = lock_recover(&ai_state.denoisers)[slot].clone() {
        return Ok(denoiser);
    }
    let models = crate::ai_processing::models_dir(app_handle).map_err(|e| e.to_string())?;
    let cache = cache_dir(app_handle)?;
    let denoiser: Arc<dyn MosaicDenoiser> = if slot == FAST_AI_DENOISE_MODEL {
        Arc::new(load_fast(app_handle, &models, cache).await?)
    } else {
        #[cfg(target_os = "macos")]
        let denoiser: Arc<dyn MosaicDenoiser> = if slot == 0 {
            Arc::new(load(app_handle, &models, cache).await?)
        } else {
            Arc::new(load_mixed(app_handle, &models, cache).await?)
        };
        #[cfg(not(target_os = "macos"))]
        let denoiser: Arc<dyn MosaicDenoiser> = Arc::new(load(app_handle, &models, cache).await?);
        denoiser
    };
    lock_recover(&ai_state.denoisers)[slot] = Some(denoiser.clone());
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
        None,
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
            .map(|filter| {
                filter
                    .with_cache_use_observer(crate::storage_fs::mark_used)
                    .with_cache_access(|path| Box::new(crate::storage_fs::pin_file(path)))
            })
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
            .map(|filter| {
                filter
                    .with_cache_use_observer(crate::storage_fs::mark_used)
                    .with_cache_access(|path| Box::new(crate::storage_fs::pin_file(path)))
            })
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
) -> Result<rawtonic_denoise::MatchFilter, CommandError> {
    crate::ai_processing::ensure_runtime()?;
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
            .map(|filter| {
                filter
                    .with_cache_use_observer(crate::storage_fs::mark_used)
                    .with_cache_access(|path| Box::new(crate::storage_fs::pin_file(path)))
            })
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(CommandError::from)
}

/// The Fast network on ONNX Runtime (every platform), downloaded from the
/// pack and checked by its SHA-256 on first use.
async fn load_fast(
    app_handle: &tauri::AppHandle,
    models: &Path,
    cache: PathBuf,
) -> Result<rawtonic_denoise::MatchFilter, CommandError> {
    crate::ai_processing::ensure_runtime()?;
    let file = rawtonic_denoise::FAST_ONNX_MODEL;
    crate::ai_processing::download_verified(
        app_handle,
        models,
        file,
        FAST_ONNX_SHA256,
        "AI Denoise model (Fast)",
    )
    .await
    .map_err(|e| e.to_string())?;
    let model = models.join(file);
    tokio::task::spawn_blocking(move || {
        rawtonic_denoise::MatchFilter::fast_onnx(&model, Some(cache))
            .map(|filter| {
                filter
                    .with_cache_use_observer(crate::storage_fs::mark_used)
                    .with_cache_access(|path| Box::new(crate::storage_fs::pin_file(path)))
            })
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(CommandError::from)
}

/// The AI Denoise quality (`aiDenoiseQuality`) a photo gets when AI Denoise
/// is first turned on, by the preference (`ai_denoise_default_quality`):
/// "full" or "fast" as chosen; otherwise Fast where MatchFilter would run
/// on the processor (`matchfilter_on_cpu`; a 24 MP export takes about 8 min
/// on a 6-core desktop and 19–25 min on a Radeon 680M laptop, against
/// about 1.3 min with Fast; notes: benchmark-results/dst1-2026-11-25), else
/// Full. The choice is stored in the recipe, so exports never depend on the
/// machine.
pub fn default_quality(
    preference: Option<&str>,
    matchfilter_on_cpu: impl FnOnce() -> bool,
) -> &'static str {
    use rawtonic_engine::recipe_versions::{AI_DENOISE_FAST, AI_DENOISE_FULL};
    match preference {
        Some(AI_DENOISE_FAST) => AI_DENOISE_FAST,
        Some(AI_DENOISE_FULL) => AI_DENOISE_FULL,
        _ if matchfilter_on_cpu() => AI_DENOISE_FAST,
        _ => AI_DENOISE_FULL,
    }
}

/// Whether MatchFilter runs on the processor in this process: never on
/// macOS (Core ML, on the GPU); elsewhere when every model stays on the CPU
/// (`keep_models_on_cpu`, GPU AI off, a forced CPU) or no GPU provider
/// offers a device for it.
fn matchfilter_on_cpu() -> bool {
    if cfg!(target_os = "macos") {
        return false;
    }
    rawtonic_onnx::models_run_on_cpu_only()
        || !rawtonic_onnx::has_gpu_for(rawtonic_onnx::ModelKind::Denoise)
}

/// `default_quality` for this machine and preference.
#[tauri::command]
pub async fn ai_denoise_default_quality(app_handle: tauri::AppHandle) -> Result<String, String> {
    let preference = crate::state::settings::current(&app_handle)
        .ai_denoise_default_quality
        .clone();
    tokio::task::spawn_blocking(move || {
        default_quality(preference.as_deref(), matchfilter_on_cpu).to_string()
    })
    .await
    .map_err(|e| e.to_string())
}

/// The decoder option for a recipe: `None` without AI Denoise.
pub async fn option_for(
    app_handle: &tauri::AppHandle,
    adjustments: &serde_json::Value,
) -> Result<Option<MosaicDenoise>, CommandError> {
    let Some(ai) = rawtonic_engine::image_loader::ai_denoise_settings(adjustments) else {
        return Ok(None);
    };
    Ok(Some(
        ai.mosaic_denoise(denoiser(app_handle, ai.model()).await?),
    ))
}

#[cfg(test)]
mod tests {
    #[test]
    fn new_edits_get_fast_where_matchfilter_runs_on_the_cpu_unless_chosen() {
        use super::default_quality;
        assert_eq!(default_quality(None, || true), "fast");
        assert_eq!(default_quality(None, || false), "full");
        assert_eq!(default_quality(Some("auto"), || true), "fast");
        // A chosen quality wins and never asks for the machine.
        let unasked = || panic!("the machine is not asked");
        assert_eq!(default_quality(Some("full"), unasked), "full");
        assert_eq!(default_quality(Some("fast"), unasked), "fast");
    }

    #[test]
    fn every_model_has_a_slot_and_onnx_shares_one() {
        assert_eq!(super::slot(0), 0);
        let mixed = super::slot(1);
        assert_eq!(mixed, if cfg!(target_os = "macos") { 1 } else { 0 });
        assert!(super::slot(7) < 2);
        let fast = rawtonic_engine::image_loader::FAST_AI_DENOISE_MODEL;
        assert_eq!(super::slot(fast), fast, "the Fast network has its own slot");
        assert!(fast < rawtonic_engine::image_loader::AI_DENOISE_MODELS);
    }
}
