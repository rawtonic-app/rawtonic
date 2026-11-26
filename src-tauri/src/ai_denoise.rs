// SPDX-License-Identifier: AGPL-3.0-or-later
//! AI Denoise: Nonlocal MatchFilter (or the Fast network) on the Bayer
//! mosaic, the X-Trans network on X-Trans mosaics (rawtonic-denoise).
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

use rawtonic_catalog::sensor_kind::SensorKind as StoredSensorKind;
use rawtonic_engine::image_loader::{FAST_AI_DENOISE_MODEL, XTRANS_AI_DENOISE_MODEL};
use rawtonic_rawdecode::mosaic_denoise::{MosaicDenoise, MosaicDenoiser};
use rawtonic_rawdecode::raw_processing::SensorKind;
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
/// The X-Trans network (`rawtonic_denoise::XTRANS_ONNX_MODEL`, CC BY-SA
/// 4.0; in the packs from pack-2026-11-25d), on ONNX Runtime on every
/// platform.
const XTRANS_ONNX_SHA256: &str = "de33162b2fc6ac8217e3f9a7becf9d6831fed6a269a4614613300f0bd4c32e2d";

pub fn cache_dir(app_handle: &tauri::AppHandle) -> Result<PathBuf, String> {
    Ok(app_handle
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("ai-denoise-cache"))
}

/// Where `AiState::denoisers` keeps the denoiser of `model`
/// (`AiDenoiseSettings::model_for`): ONNX Runtime (Windows, Linux) has one
/// MatchFilter model for every version; the Fast and the X-Trans network
/// have their own slots.
pub fn slot(model: usize) -> usize {
    if model == FAST_AI_DENOISE_MODEL || model == XTRANS_AI_DENOISE_MODEL {
        model
    } else if cfg!(target_os = "macos") {
        model.min(1)
    } else {
        0
    }
}

/// The denoiser of AI Denoise `model` (`AiDenoiseSettings::model_for`: 0 the
/// first package, 1 the mixed-precision one, then the Fast and the X-Trans
/// network), downloading and loading it on
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
    } else if slot == XTRANS_AI_DENOISE_MODEL {
        Arc::new(load_xtrans(app_handle, &models, cache).await?)
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

/// The X-Trans network on ONNX Runtime (every platform), downloaded from
/// the pack and checked by its SHA-256 on first use.
async fn load_xtrans(
    app_handle: &tauri::AppHandle,
    models: &Path,
    cache: PathBuf,
) -> Result<rawtonic_denoise::XTransNet, CommandError> {
    crate::ai_processing::ensure_runtime()?;
    let file = rawtonic_denoise::XTRANS_ONNX_MODEL;
    crate::ai_processing::download_verified(
        app_handle,
        models,
        file,
        XTRANS_ONNX_SHA256,
        "AI Denoise model (X-Trans)",
    )
    .await
    .map_err(|e| e.to_string())?;
    let model = models.join(file);
    tokio::task::spawn_blocking(move || {
        rawtonic_denoise::XTransNet::onnx(&model, Some(cache))
            .map(|net| {
                net.with_cache_use_observer(crate::storage_fs::mark_used)
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
/// on the processor (`matchfilter_on_cpu`; its denoising took 664 s of a
/// 24 MP export on a loaded 6-core desktop against 43–51 s with Fast, and a
/// Radeon 680M laptop exports in 19–25 min, about 1.3 min with Fast
/// projected; notes: benchmark-results/dst1-2026-11-25, dst2-2026-11-25),
/// else Full. The choice is stored in the recipe, so every machine exports
/// the photo with the same network.
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

/// Whether MatchFilter (`models/nlmf.onnx`) runs on the processor in this
/// process: never on macOS (Core ML, on the GPU); elsewhere when every
/// model stays on the CPU (`keep_models_on_cpu`, GPU AI off, a forced CPU)
/// or it would not run on a GPU (`rawtonic_onnx::runs_on_gpu`: no GPU
/// provider offers a device, or MatchFilter's check there rejected it).
fn matchfilter_on_cpu(models: &Path) -> bool {
    on_cpu_of(
        cfg!(target_os = "macos"),
        rawtonic_onnx::models_run_on_cpu_only(),
        || {
            rawtonic_onnx::runs_on_gpu(
                &models.join(rawtonic_denoise::ONNX_MODEL),
                rawtonic_onnx::ModelKind::Denoise,
            )
        },
    )
}

/// `matchfilter_on_cpu` from its parts: models kept on the CPU by policy
/// (`keep_models_on_cpu`, e.g. a Radeon 680M under Windows; GPU AI off; a
/// forced CPU) answer at once, without asking for a GPU or a check that
/// never runs there.
fn on_cpu_of(macos: bool, models_on_cpu: bool, runs_on_gpu: impl FnOnce() -> bool) -> bool {
    if macos {
        return false;
    }
    models_on_cpu || !runs_on_gpu()
}

/// `default_quality` of Automatic on this machine. The editor resolves a
/// chosen preference itself (it holds the choice before it is saved).
#[tauri::command]
pub async fn ai_denoise_default_quality(app_handle: tauri::AppHandle) -> Result<String, String> {
    let models = crate::ai_processing::models_dir(&app_handle).map_err(|e| e.to_string())?;
    tokio::task::spawn_blocking(move || {
        default_quality(None, || matchfilter_on_cpu(&models)).to_string()
    })
    .await
    .map_err(|e| e.to_string())
}

/// Prepare the denoiser for this recipe. With an unknown sensor, the full
/// decode will choose which of the two models it needs.
pub async fn option_for(
    app_handle: &tauri::AppHandle,
    adjustments: &serde_json::Value,
    source: &Path,
) -> Result<Option<MosaicDenoise>, CommandError> {
    let Some(ai) = rawtonic_engine::image_loader::ai_denoise_settings(adjustments) else {
        return Ok(None);
    };
    let sensor = if ai.denoises_xtrans() {
        sensor_kind(source).await
    } else {
        SensorKind::Unknown
    };
    let mut models = std::collections::BTreeMap::new();
    let mut error = None;
    for model in ai.models_for_sensor(sensor).into_iter().flatten() {
        match denoiser(app_handle, model).await {
            Ok(denoiser) => {
                models.insert(model, denoiser);
            }
            Err(e) => {
                error.get_or_insert(e);
            }
        }
    }
    match ai.mosaic_denoise_for(sensor, |model| models.remove(&model)) {
        Some(denoise) => Ok(Some(denoise)),
        None => Err(error.unwrap_or_else(|| "AI Denoise model is not available".into())),
    }
}

/// Probe the sensor of a photo, including virtual copies of it, for
/// Develop's controls. A readable file is read, and its session or catalog
/// remembers the answer (in the background, so the answer does not wait for
/// the write); an offline file is not read, and the kind remembered for it
/// answers (Unknown without one). Renders and exports never use the
/// remembered kind (`sensor_kind`).
#[tauri::command]
pub async fn raw_sensor_kind(path: String) -> SensorKind {
    let (source, _) = crate::file_management::parse_virtual_path(&path);
    tokio::task::spawn_blocking(move || {
        let probe = rawtonic_rawdecode::raw_processing::sensor_kind_at;
        let library = crate::session_bridge::library_dir_for(&source);
        if !crate::preview_proxy::source_available(&source) {
            return library.map_or(SensorKind::Unknown, |library| {
                remembered_sensor_kind(&library, &source)
            });
        }
        // A photograph no library manages is read once, with nothing to
        // remember (no fingerprint reads).
        let Some(library) = library.filter(|library| registered(library, &source)) else {
            return probe(&source);
        };
        let (kind, read) = read_sensor_kind(&source, probe);
        if let Some(read) = read {
            tauri::async_runtime::spawn_blocking(move || {
                if let Err(e) = remember_sensor_kind(&library, &source, kind, &read) {
                    log::warn!("Sensor of {}: not remembered: {e}", source.display());
                }
            });
        }
        kind
    })
    .await
    .unwrap_or(SensorKind::Unknown)
}

/// Whether the library at `library` has a photograph at `source`.
fn registered(library: &Path, source: &Path) -> bool {
    rawtonic_catalog::Library::open(library)
        .ok()
        .is_some_and(|lib| matches!(lib.asset_at(source), Ok(Some(_))))
}

/// A sensor read to remember: the fingerprint of the file it read, and its
/// place among the reads of that file (a later read wins, whichever of
/// their background writes runs first).
struct SensorRead {
    fingerprint: String,
    ticket: u64,
}

/// The newest sensor read of each file read this run (one number per file).
static NEWEST_SENSOR_READS: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<PathBuf, u64>>,
> = std::sync::LazyLock::new(Default::default);

/// `probe`'s answer for the file at `source`, with what is needed to
/// remember it when the file was the same before and after the read (else
/// the answer is not remembered).
fn read_sensor_kind(
    source: &Path,
    probe: impl FnOnce(&Path) -> SensorKind,
) -> (SensorKind, Option<SensorRead>) {
    static TICKETS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let ticket = TICKETS.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
    lock_recover(&NEWEST_SENSOR_READS).insert(source.to_path_buf(), ticket);
    let before = rawtonic_catalog::fingerprint(source).ok();
    let kind = probe(source);
    let read = before
        .filter(|before| rawtonic_catalog::fingerprint(source).ok().as_ref() == Some(before))
        .map(|fingerprint| SensorRead {
            fingerprint,
            ticket,
        });
    (kind, read)
}

/// Records what a read of the photograph at `source` gave, from the file
/// `read` names, in the library at `library`: a Bayer or X-Trans answer
/// replaces the recorded one; an Unknown answer keeps a kind read from the
/// same file (the read failed this time) and forgets one of another file.
/// Writes nothing when nothing changes (a photo opened again) or when a
/// later read of the file was made meanwhile (its own write decides).
/// Runs in the background, after the answer went to Develop.
fn remember_sensor_kind(
    library: &Path,
    source: &Path,
    kind: SensorKind,
    read: &SensorRead,
) -> Result<(), String> {
    let fingerprint = read.fingerprint.as_str();
    let verified = match kind {
        SensorKind::Bayer => Some(StoredSensorKind::Bayer),
        SensorKind::XTrans => Some(StoredSensorKind::XTrans),
        SensorKind::Unknown => None,
    };
    let unchanged = |lib: &rawtonic_catalog::Library| -> Result<Option<String>, String> {
        let Some(asset) = lib.asset_at(source).map_err(|e| e.to_string())? else {
            return Ok(None);
        };
        let recorded = lib.sensor_read(&asset.id).map_err(|e| e.to_string())?;
        let same_file = recorded
            .as_ref()
            .is_some_and(|(_, read)| read == fingerprint);
        let done = match verified {
            Some(kind) => same_file && recorded.is_some_and(|(stored, _)| stored == kind),
            None => recorded.is_none() || same_file,
        };
        Ok((!done).then_some(asset.id))
    };
    // Whether no later read of the file was made meanwhile.
    let newest = || lock_recover(&NEWEST_SENSOR_READS).get(source) == Some(&read.ticket);
    // The whole decision under the write lock, so no later read can pass
    // between this read's check and its write (the photo's answer never
    // waits for this).
    crate::session_bridge::with_library_blocking(library, |lib| {
        if !newest() {
            return Ok(());
        }
        let Some(id) = unchanged(lib)? else {
            return Ok(());
        };
        match verified {
            Some(kind) => lib.record_sensor_kind(&id, kind, fingerprint).map(drop),
            None => lib.forget_sensor_kind(&id),
        }
        .map_err(|e| e.to_string())
    })
}

/// The kind the library at `library` remembered for the photograph at
/// `source`, as the frontend reads it (Unknown without one).
fn remembered_sensor_kind(library: &Path, source: &Path) -> SensorKind {
    let stored = rawtonic_catalog::Library::open(library)
        .ok()
        .and_then(|lib| lib.sensor_kind(&lib.asset_at(source).ok()??.id).ok()?);
    match stored {
        Some(StoredSensorKind::Bayer) => SensorKind::Bayer,
        Some(StoredSensorKind::XTrans) => SensorKind::XTrans,
        None => SensorKind::Unknown,
    }
}

/// Run the sensor read off the async workers.
pub async fn sensor_kind(source: &Path) -> SensorKind {
    let source = source.to_path_buf();
    tokio::task::spawn_blocking(move || rawtonic_rawdecode::raw_processing::sensor_kind_at(&source))
        .await
        .unwrap_or(SensorKind::Unknown)
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_verified_read_is_remembered_for_the_photo_offline_and_a_failed_one_is_not() {
        use super::{SensorKind, read_sensor_kind, remember_sensor_kind, remembered_sensor_kind};
        use rawtonic_catalog::{Library, Mode};
        let d = tempfile::tempdir().unwrap();
        let lib = Library::create(&d.path().join("Job"), "Job", Mode::Session).unwrap();
        let capture = lib.directory.join("Capture");
        std::fs::create_dir_all(&capture).unwrap();
        let (xtrans, bayer, unread, replaced) = (
            capture.join("A.RAF"),
            capture.join("B.NEF"),
            capture.join("C.RAF"),
            capture.join("D.DNG"),
        );
        for path in [&xtrans, &bayer, &unread, &replaced] {
            std::fs::write(path, path.to_string_lossy().as_bytes()).unwrap();
            lib.add_asset(path).unwrap();
        }
        let directory = lib.directory.clone();
        drop(lib);
        let read = |path: &std::path::Path, answer: SensorKind| {
            let (kind, read) = read_sensor_kind(path, |_| answer);
            assert_eq!(kind, answer);
            remember_sensor_kind(&directory, path, kind, &read.unwrap()).unwrap();
        };
        read(&xtrans, SensorKind::XTrans);
        read(&bayer, SensorKind::Bayer);
        read(&unread, SensorKind::Unknown);
        read(&replaced, SensorKind::XTrans);
        // A failed read of the same file keeps what was read before.
        read(&xtrans, SensorKind::Unknown);
        // The file replaced in place (the library has not registered it):
        // its own read counts, and one that cannot tell forgets the old kind.
        std::fs::write(&bayer, b"edited elsewhere, now X-Trans").unwrap();
        read(&bayer, SensorKind::XTrans);
        std::fs::write(&replaced, b"replaced by an unreadable file").unwrap();
        read(&replaced, SensorKind::Unknown);
        // A read during which the file changed is not remembered.
        let (_, changed) = read_sensor_kind(&unread, |path| {
            std::fs::write(path, b"replaced while it was read").unwrap();
            SensorKind::Bayer
        });
        assert!(changed.is_none());
        // Two reads across a replacement: the later one wins although the
        // earlier one's write runs last.
        let (first, earlier) = read_sensor_kind(&xtrans, |_| SensorKind::XTrans);
        std::fs::write(&xtrans, b"now a Bayer file").unwrap();
        let (second, later) = read_sensor_kind(&xtrans, |_| SensorKind::Bayer);
        remember_sensor_kind(&directory, &xtrans, second, &later.unwrap()).unwrap();
        remember_sensor_kind(&directory, &xtrans, first, &earlier.unwrap()).unwrap();
        assert_eq!(
            remembered_sensor_kind(&directory, &xtrans),
            SensorKind::Bayer
        );
        std::fs::write(&xtrans, b"back to X-Trans").unwrap();
        read(&xtrans, SensorKind::XTrans);
        // Offline: the files are gone, the library answers.
        for path in [&xtrans, &bayer, &unread, &replaced] {
            std::fs::remove_file(path).unwrap();
        }
        let offline = |path: &std::path::Path| remembered_sensor_kind(&directory, path);
        assert_eq!(offline(&xtrans), SensorKind::XTrans);
        assert_eq!(offline(&bayer), SensorKind::XTrans);
        assert_eq!(offline(&unread), SensorKind::Unknown);
        assert_eq!(offline(&replaced), SensorKind::Unknown);
        assert_eq!(
            offline(&capture.join("unregistered.RAF")),
            SensorKind::Unknown
        );
        // The answer reaches the frontend in its contract's shape.
        let wire: Vec<String> =
            serde_json::from_str(include_str!("../../src/contracts/rawSensorKind.json")).unwrap();
        assert_eq!(serde_json::to_value(offline(&xtrans)).unwrap(), wire[1]);
    }

    #[test]
    fn raw_sensor_kind_matches_the_frontend_contract() {
        use super::SensorKind;
        let wire: [serde_json::Value; 3] =
            serde_json::from_str(include_str!("../../src/contracts/rawSensorKind.json")).unwrap();
        let kinds = [SensorKind::Bayer, SensorKind::XTrans, SensorKind::Unknown];
        for (value, kind) in wire.into_iter().zip(kinds) {
            assert_eq!(
                serde_json::from_value::<SensorKind>(value.clone()).unwrap(),
                kind
            );
            assert_eq!(serde_json::to_value(kind).unwrap(), value);
        }
    }

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
    fn models_kept_on_the_cpu_resolve_automatic_to_fast_at_once() {
        use super::{default_quality, on_cpu_of};
        let unasked = || -> bool { panic!("no GPU and no check is asked for") };
        // keep_models_on_cpu (Radeon 680M under Windows), GPU AI off, forced CPU.
        assert!(on_cpu_of(false, true, unasked));
        assert_eq!(
            default_quality(None, || on_cpu_of(false, true, unasked)),
            "fast"
        );
        // A usable GPU, verdict unknown or passed: Full; no GPU or a rejected check: Fast.
        assert_eq!(
            default_quality(None, || on_cpu_of(false, false, || true)),
            "full"
        );
        assert_eq!(
            default_quality(None, || on_cpu_of(false, false, || false)),
            "fast"
        );
        // macOS runs MatchFilter on Core ML.
        assert_eq!(
            default_quality(None, || on_cpu_of(true, true, unasked)),
            "full"
        );
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
        let xtrans = rawtonic_engine::image_loader::XTRANS_AI_DENOISE_MODEL;
        assert_eq!(super::slot(xtrans), xtrans, "so has the X-Trans network");
        assert!(xtrans < rawtonic_engine::image_loader::AI_DENOISE_MODELS);
    }
}
