// SPDX-License-Identifier: AGPL-3.0-or-later
//! Preferences storage inventory and maintenance of explicit app-owned roots.
use crate::{app_state::AppState, storage_fs};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};
use tauri::Manager;

static MAINTENANCE: Mutex<()> = Mutex::new(());
static SCANS: LazyLock<Mutex<HashMap<String, Arc<AtomicBool>>>> = LazyLock::new(Default::default);
static SIZES: LazyLock<Mutex<HashMap<PathBuf, (Instant, storage_fs::Usage)>>> =
    LazyLock::new(Default::default);

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub enum Category {
    Thumbnails,
    Previews,
    Decoded,
    AiResults,
    Models,
    CompiledModels,
    Logs,
    Scratch,
    Session,
    Exif,
    SystemModels,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    category: Category,
    bytes: Option<u64>,
    files: u64,
    path: Option<String>,
    incomplete: bool,
    clearable: bool,
    pending_removal: bool,
    error: bool,
}

struct OwnedDirectory {
    root: PathBuf,
    directory: PathBuf,
}

fn app_owned(app: &tauri::AppHandle, category: Category) -> Result<Option<OwnedDirectory>, String> {
    let paths = app.path();
    let cache = paths.app_cache_dir().map_err(|e| e.to_string())?;
    let data = paths.app_data_dir().map_err(|e| e.to_string())?;
    let (root, directory) = match category {
        Category::Thumbnails => (cache.clone(), cache.join("thumbnails")),
        Category::Previews => (cache.clone(), cache.join("previews")),
        Category::AiResults => (data.clone(), data.join("ai-denoise-cache")),
        Category::Models => (data.clone(), data.join("models")),
        Category::CompiledModels => (cache.clone(), cache.join("ai-first-runs")),
        Category::Scratch => (cache.clone(), cache.join("scratch")),
        Category::Exif => (cache.clone(), cache.join("exif")),
        Category::Logs => {
            let directory = paths.app_log_dir().map_err(|e| e.to_string())?;
            let root = directory.parent().ok_or("No app log parent")?.to_path_buf();
            (root, directory)
        }
        Category::Session => {
            let Some(root) = crate::session_bridge::active_library_directory() else {
                return Ok(None);
            };
            (root.clone(), root.join(".cache"))
        }
        Category::SystemModels => {
            #[cfg(target_os = "macos")]
            {
                // Core ML owns this tree; it is inventory-only.
                (cache.clone(), cache.join("com.apple.e5rt.e5bundlecache"))
            }
            #[cfg(not(target_os = "macos"))]
            {
                return Ok(None);
            }
        }
        Category::Decoded => return Ok(None),
    };
    Ok(Some(OwnedDirectory { root, directory }))
}

fn pinned(path: &Path) -> bool {
    // Atomic writers' staging names and entries just handed to the webview or
    // read by a worker remain protected. A later maintenance pass retries.
    path.file_name()
        .is_some_and(|n| n.to_string_lossy().starts_with('.'))
        || std::fs::metadata(path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|used| used.elapsed().ok())
            .is_none_or(|age| age < Duration::from_secs(60))
}

#[tauri::command]
pub fn storage_cancel_scan(request_id: String) {
    if let Some(cancel) = SCANS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&request_id)
    {
        cancel.store(true, Ordering::Relaxed);
    }
}

#[tauri::command]
pub async fn storage_usage(
    app: tauri::AppHandle,
    request_id: String,
) -> Result<Vec<Entry>, String> {
    let cancel = Arc::new(AtomicBool::new(false));
    {
        let mut scans = SCANS.lock().unwrap_or_else(|e| e.into_inner());
        if scans.len() >= 8 || scans.contains_key(&request_id) {
            return Err("Storage scan already running".into());
        }
        scans.insert(request_id.clone(), cancel.clone());
    }
    let result = tauri::async_runtime::spawn_blocking(move || {
        let mut entries = Vec::new();
        for category in [
            Category::Thumbnails,
            Category::Previews,
            Category::Decoded,
            Category::AiResults,
            Category::Models,
            Category::CompiledModels,
            Category::Logs,
            Category::Scratch,
            Category::Exif,
            Category::Session,
            Category::SystemModels,
        ] {
            if cancel.load(Ordering::Relaxed) {
                return Err("Storage scan cancelled".into());
            }
            if category == Category::Decoded {
                let bytes = app
                    .state::<AppState>()
                    .decoded_image_cache
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .retained_bytes() as u64;
                entries.push(Entry {
                    category,
                    bytes: Some(bytes),
                    files: 0,
                    path: None,
                    incomplete: false,
                    clearable: true,
                    pending_removal: false,
                    error: false,
                });
                continue;
            }
            let Some(owned) = app_owned(&app, category)? else {
                continue;
            };
            let cached = SIZES
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(&owned.directory)
                .filter(|(when, _)| when.elapsed() < Duration::from_secs(5))
                .cloned();
            let measured = if let Some((_, usage)) = cached {
                Ok(usage)
            } else {
                let result = if owned.directory.exists() {
                    storage_fs::owned_directory(&owned.root, &owned.directory)
                        .and_then(|dir| storage_fs::usage(&dir, &cancel, Duration::from_secs(2)))
                } else {
                    Ok(storage_fs::Usage::default())
                };
                if let Ok(ref usage) = result {
                    let mut sizes = SIZES.lock().unwrap_or_else(|e| e.into_inner());
                    if sizes.len() >= 32 {
                        sizes.clear();
                    }
                    sizes.insert(owned.directory.clone(), (Instant::now(), usage.clone()));
                }
                result
            };
            let error = measured.is_err();
            let usage = measured.unwrap_or_default();
            entries.push(Entry {
                category,
                bytes: (!error).then_some(usage.bytes),
                files: usage.files,
                path: Some(owned.directory.to_string_lossy().into_owned()),
                incomplete: usage.incomplete,
                clearable: category != Category::SystemModels && !error && cfg!(any(unix, windows)),
                pending_removal: category == Category::Models && removal_marker(&app)?.is_file(),
                error,
            });
        }
        Ok(entries)
    })
    .await
    .map_err(|e| e.to_string());
    SCANS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&request_id);
    result?
}

/// Pending model removal is processed before any model can be loaded on the
/// next launch. This also keeps mmap/OS model readers safe on every platform.
fn removal_marker(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    Ok(app
        .path()
        .app_cache_dir()
        .map_err(|e| e.to_string())?
        .join("remove-models-on-start"))
}

fn schedule_model_removal(marker: &Path, confirmed: bool) -> Result<(), String> {
    if !confirmed {
        return Err("Model removal requires confirmation".into());
    }
    std::fs::create_dir_all(marker.parent().ok_or("No cache root")?).map_err(|e| e.to_string())?;
    rawtonic_metadata::atomic_write(marker, b"remove at next startup").map_err(|e| e.to_string())
}

fn remove_pending_models(marker: &Path, owned: &OwnedDirectory) -> Result<(), String> {
    if !marker.is_file() {
        return Ok(());
    }
    if owned.directory.exists() {
        storage_fs::clear_tree(&owned.root, &owned.directory, &|path| {
            [
                rawtonic_onnx::Provider::Cuda,
                rawtonic_onnx::Provider::WebGpu,
            ]
            .iter()
            .filter_map(|provider| provider.library_file())
            .any(|name| path.file_name().is_some_and(|file| file == name))
        })
        .map_err(|e| e.to_string())?;
    }
    std::fs::remove_file(marker).map_err(|e| e.to_string())
}

pub(crate) fn remove_models_on_start(app: &tauri::AppHandle) -> Result<(), String> {
    let marker = removal_marker(app)?;
    let owned = app_owned(app, Category::Models)?.ok_or("No models directory")?;
    remove_pending_models(&marker, &owned)
}

#[tauri::command]
pub async fn storage_clear(
    app: tauri::AppHandle,
    category: Category,
    confirmed: bool,
    session_directory: Option<String>,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = MAINTENANCE.lock().unwrap_or_else(|e| e.into_inner());
        if category == Category::Decoded {
            app.state::<AppState>()
                .decoded_image_cache
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clear();
            return Ok(());
        }
        if category == Category::SystemModels {
            return Err("System-managed cache is read-only".into());
        }
        if category == Category::Models {
            return schedule_model_removal(&removal_marker(&app)?, confirmed);
        }
        let owned = app_owned(&app, category)?.ok_or("No owned cache directory")?;
        if category == Category::Session {
            let expected = session_directory
                .as_deref()
                .ok_or("No expected library directory")?;
            let current = &owned.root;
            if Path::new(expected)
                .canonicalize()
                .map_err(|e| e.to_string())?
                != current.canonicalize().map_err(|e| e.to_string())?
            {
                return Err("The open library changed; refresh Storage before clearing".into());
            }
        }
        if owned.directory.exists() {
            storage_fs::clear_tree(&owned.root, &owned.directory, &|path| {
                (category == Category::Logs && path.file_name().is_some_and(|n| n == "app.log"))
                    || pinned(path)
            })
            .map_err(|e| e.to_string())?;
        }
        SIZES.lock().unwrap_or_else(|e| e.into_inner()).clear();
        if matches!(category, Category::Previews | Category::Session) {
            *app.state::<AppState>()
                .proxy_memory
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = Default::default();
        }
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
}

pub(crate) fn apply_limits(app: &tauri::AppHandle) -> Result<(), String> {
    let settings = crate::app_settings::load_settings(app.clone())?;
    let _guard = MAINTENANCE.lock().unwrap_or_else(|e| e.into_inner());
    for (category, gb, extension) in [
        (
            Category::Thumbnails,
            settings.thumbnail_cache_limit_gb.unwrap_or(2),
            "jpg",
        ),
        (
            Category::AiResults,
            settings.ai_cache_limit_gb.unwrap_or(5),
            "f16",
        ),
    ] {
        if gb == 0 {
            continue;
        }
        let Some(owned) = app_owned(app, category)? else {
            continue;
        };
        if !owned.directory.exists() {
            continue;
        }
        storage_fs::evict(
            &owned.root,
            &owned.directory,
            u64::from(gb) * 1_000_000_000,
            |p| p.extension().is_some_and(|ext| ext == extension),
            pinned,
        )
        .map_err(|e| e.to_string())?;
    }
    SIZES.lock().unwrap_or_else(|e| e.into_inner()).clear();
    Ok(())
}

#[tauri::command]
pub async fn storage_apply_limits(app: tauri::AppHandle) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || apply_limits(&app))
        .await
        .map_err(|e| e.to_string())?
}

pub(crate) fn start_maintenance(app: tauri::AppHandle) {
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(60)).await;
            let app = app.clone();
            let _ = tauri::async_runtime::spawn_blocking(move || {
                if let Err(error) = apply_limits(&app) {
                    log::debug!("Cache limits: {error}");
                }
            })
            .await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn models_need_confirmation_and_are_removed_only_on_the_next_start() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let models = root.join("models");
        fs::create_dir_all(models.join("compiled/weights")).unwrap();
        fs::write(models.join("network.onnx"), b"weights").unwrap();
        let provider = rawtonic_onnx::Provider::Cuda.library_file().unwrap();
        fs::write(models.join(provider), b"installed plugin").unwrap();
        fs::write(models.join("compiled/weights/bin"), b"compiled").unwrap();
        let photo = root.join("original.RAW");
        fs::write(&photo, b"original").unwrap();
        let marker = root.join("cache/remove-models-on-start");
        assert!(schedule_model_removal(&marker, false).is_err());
        assert!(!marker.exists());
        let owned = OwnedDirectory {
            root,
            directory: models.clone(),
        };
        remove_pending_models(&marker, &owned).unwrap();
        assert!(models.join("network.onnx").exists());
        schedule_model_removal(&marker, true).unwrap();
        assert!(
            models.join("network.onnx").exists(),
            "confirmation schedules, it does not unlink a loaded model"
        );
        remove_pending_models(&marker, &owned).unwrap();
        assert!(!models.join("network.onnx").exists());
        assert!(!models.join("compiled/weights/bin").exists());
        assert_eq!(
            fs::read(models.join(provider)).unwrap(),
            b"installed plugin"
        );
        assert!(!marker.exists());
        assert_eq!(fs::read(photo).unwrap(), b"original");
    }

    #[cfg(unix)]
    #[test]
    fn redirected_model_root_is_refused_and_pending_removal_is_preserved() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let photographs = root.join("photographs");
        fs::create_dir(&photographs).unwrap();
        fs::write(photographs.join("original.RAW"), b"original").unwrap();
        let models = root.join("models");
        symlink(&photographs, &models).unwrap();
        let marker = root.join("cache/remove-models-on-start");
        schedule_model_removal(&marker, true).unwrap();
        assert!(
            remove_pending_models(
                &marker,
                &OwnedDirectory {
                    root,
                    directory: models
                }
            )
            .is_err()
        );
        assert!(marker.exists());
        assert_eq!(
            fs::read(photographs.join("original.RAW")).unwrap(),
            b"original"
        );
    }
}
