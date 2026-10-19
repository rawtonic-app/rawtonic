//! Library listings: folder contents, image metadata workers, groups, EXIF updates and disk detection.
//! Part of `file_management`, split by concern (ADR 0009).
use super::read_file_mapped;
use super::resolve_xmp_path;
use super::sync_metadata_from_xmp;
use crate::AppSettings;
use crate::app_state::AppState;
use crate::app_state::PendingMetadata;
use crate::formats::is_supported_image_file;
use crate::image_processing::ImageMetadata;
use crate::load_settings;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::thread;
use sysinfo::Disks;
use tauri::AppHandle;
use tauri::{Emitter, Manager};

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ImageFileMetadata {
    pub(crate) is_edited: bool,
    pub(crate) tags: Option<Vec<String>>,
    pub(crate) rating: u8,
    pub(crate) is_raw: bool,
}

pub(crate) fn resolve_image_metadata(
    image_path: &Path,
    sidecar_path: &Path,
    enable_xmp_sync: bool,
    settings: &AppSettings,
) -> ImageFileMetadata {
    let mut metadata = crate::exif_processing::load_sidecar(sidecar_path);

    if enable_xmp_sync
        && sync_metadata_from_xmp(image_path, &mut metadata)
        && let Ok(json) = serde_json::to_string_pretty(&metadata)
    {
        let _ = crate::session_bridge::write_editor_sidecar(sidecar_path, json);
    }

    image_file_metadata(image_path, metadata, settings)
}

pub(crate) fn image_file_metadata(
    image_path: &Path,
    metadata: ImageMetadata,
    settings: &AppSettings,
) -> ImageFileMetadata {
    let is_raw = crate::formats::is_raw_file(image_path);
    let tm_override = crate::image_processing::resolve_tonemapper_override(settings, is_raw);
    let is_edited =
        crate::image_processing::is_image_edited(&metadata.adjustments, is_raw, tm_override);
    ImageFileMetadata {
        is_edited,
        tags: metadata.tags,
        rating: metadata.rating,
        is_raw,
    }
}

pub(crate) fn listing_metadata_snapshot(
    sources: &[PathBuf],
    settings: &AppSettings,
) -> HashMap<PathBuf, ImageFileMetadata> {
    let mut snapshot = HashMap::new();
    crate::session_bridge::visit_editor_sidecars(sources, |source, sidecar, state| {
        if is_cloud_placeholder(sidecar) {
            return;
        }
        let metadata = state
            .and_then(|value| serde_json::from_value(value).ok())
            .unwrap_or_else(|| crate::exif_processing::load_sidecar_file(sidecar));
        // Managed sources deliberately do not import external XMP here; the
        // existing resolve_image_metadata path applies that same authority rule.
        snapshot.insert(
            sidecar.to_owned(),
            image_file_metadata(source, metadata, settings),
        );
    });
    snapshot
}

pub(crate) fn emit_image_metadata_loaded(
    app_handle: &AppHandle,
    path: &str,
    rating: u8,
    is_edited: bool,
    tags: &Option<Vec<String>>,
) {
    let _ = app_handle.emit(
        "image-metadata-loaded",
        serde_json::json!({ "path": path, "rating": rating, "is_edited": is_edited, "tags": tags }),
    );
}

pub(crate) fn enqueue_metadata(
    app_handle: &AppHandle,
    virtual_path: String,
    image_path: PathBuf,
    sidecar_path: PathBuf,
) {
    let state = app_handle.state::<crate::app_state::AppState>();
    let manager = &state.metadata_manager;

    let mut pending = manager.pending.lock().unwrap();
    if !pending.insert(sidecar_path.clone()) {
        return;
    }
    drop(pending);

    manager.queue.lock().unwrap().push_back(PendingMetadata {
        virtual_path,
        image_path,
        sidecar_path,
    });
    manager.cvar.notify_one();
}

pub(crate) const METADATA_WORKER_THREADS: usize = 4;

pub fn start_metadata_workers(app_handle: tauri::AppHandle) {
    let state = app_handle.state::<crate::app_state::AppState>();
    let manager = state.metadata_manager.clone();

    for _ in 0..METADATA_WORKER_THREADS {
        let app_clone = app_handle.clone();
        let manager_clone = manager.clone();

        std::thread::spawn(move || {
            loop {
                let item = {
                    let mut queue = manager_clone.queue.lock().unwrap();
                    while queue.is_empty() {
                        queue = manager_clone.cvar.wait(queue).unwrap();
                    }
                    queue.pop_front().unwrap()
                };

                let settings = load_settings(app_clone.clone()).unwrap_or_default();
                let enable_xmp_sync = settings.enable_xmp_sync.unwrap_or(false);

                let metadata = resolve_image_metadata(
                    &item.image_path,
                    &item.sidecar_path,
                    enable_xmp_sync,
                    &settings,
                );

                emit_image_metadata_loaded(
                    &app_clone,
                    &item.virtual_path,
                    metadata.rating,
                    metadata.is_edited,
                    &metadata.tags,
                );

                manager_clone
                    .pending
                    .lock()
                    .unwrap()
                    .remove(&item.sidecar_path);
            }
        });
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ImageFile {
    pub path: String,
    pub(crate) modified: u64,
    pub(crate) is_edited: bool,
    pub(crate) rating: u8,
    pub(crate) tags: Option<Vec<String>>,
    pub(crate) exif: Option<HashMap<String, String>>,
    pub(crate) is_virtual_copy: bool,
    pub(crate) is_cloud_placeholder: bool,
    pub(crate) is_raw: bool,
    pub(crate) group_id: Option<String>,
    /// The file is missing or on a disconnected drive; the library still
    /// holds the photograph (it opens on its previews).
    #[serde(default)]
    pub(crate) is_offline: bool,
    /// The disconnected drive the file is on.
    #[serde(default)]
    pub(crate) offline_volume: Option<String>,
}

/// List entries for photographs a library holds whose file is offline: their
/// rating, tags and edit state come from the library.
pub(crate) fn offline_image_files(
    entries: Vec<crate::session_bridge::OfflineEntry>,
    settings: &AppSettings,
) -> Vec<ImageFile> {
    let sources: Vec<PathBuf> = entries.iter().map(|e| e.path.clone()).collect();
    let snapshot = listing_metadata_snapshot(&sources, settings);
    let unedited = |path: &Path| ImageFileMetadata {
        is_edited: false,
        tags: None,
        rating: 0,
        is_raw: crate::formats::is_raw_file(path),
    };
    entries
        .into_iter()
        .flat_map(|entry| {
            let name = entry.path.file_name().unwrap_or_default().to_string_lossy();
            let sidecar = entry.path.with_file_name(format!("{name}.rrdata"));
            let metadata = snapshot
                .get(&sidecar)
                .cloned()
                .unwrap_or_else(|| unedited(&entry.path));
            let file =
                |path: String, is_virtual_copy: bool, metadata: ImageFileMetadata| ImageFile {
                    path,
                    modified: entry.mtime,
                    is_edited: metadata.is_edited,
                    rating: metadata.rating,
                    tags: metadata.tags,
                    exif: None,
                    is_virtual_copy,
                    is_cloud_placeholder: false,
                    is_raw: metadata.is_raw,
                    group_id: None,
                    is_offline: true,
                    offline_volume: entry.volume.clone(),
                };
            let source = entry.path.to_string_lossy().into_owned();
            // Its virtual copies, as the library keeps them, offline as well.
            let copies: Vec<ImageFile> = entry
                .copies
                .iter()
                .map(|copy| {
                    let sidecar = entry.path.with_file_name(format!("{name}.{copy}.rrdata"));
                    let metadata = crate::session_bridge::read_editor_sidecar(&sidecar)
                        .and_then(|state| serde_json::from_value::<ImageMetadata>(state).ok())
                        .map(|state| image_file_metadata(&entry.path, state, settings))
                        .unwrap_or_else(|| unedited(&entry.path));
                    file(format!("{source}?vc={copy}"), true, metadata)
                })
                .collect();
            std::iter::once(file(source.clone(), false, metadata)).chain(copies)
        })
        .collect()
}

/// The virtual copies among listed photographs (`(photograph, copy id,
/// sidecar)`), which the library then keeps (`adopt_virtual_copies`).
fn listed_copies<'a>(
    listed: impl Iterator<Item = (&'a PathBuf, &'a String, &'a Vec<Option<String>>)>,
) -> Vec<(PathBuf, String, PathBuf)> {
    listed
        .flat_map(|(path, file_name, sidecars)| {
            sidecars.iter().flatten().map(move |copy| {
                (
                    path.clone(),
                    copy.clone(),
                    path.with_file_name(format!("{file_name}.{copy}.rrdata")),
                )
            })
        })
        .collect()
}

pub(crate) fn make_group_key(source_path: &Path) -> String {
    let parent = source_path.parent().unwrap_or(Path::new(""));
    let stem = source_path.file_stem().unwrap_or_default();
    format!("{}/{}", parent.to_string_lossy(), stem.to_string_lossy())
}

pub(crate) fn assign_group_ids(
    files: &mut [ImageFile],
    settings: &crate::app_settings::AppSettings,
) {
    let require_matching_exif = settings.require_matching_exif.unwrap_or(false);
    let group_edited_files = settings.group_edited_files.unwrap_or(true);

    #[derive(Clone)]
    struct Candidate {
        index: usize,
        source_path: PathBuf,
        key: String,
    }

    let candidates: Vec<Candidate> = files
        .iter()
        .enumerate()
        .filter(|(_, file)| !file.is_virtual_copy && (group_edited_files || !file.is_edited))
        .map(|(index, file)| {
            let (source_path, _) = parse_virtual_path(&file.path);
            let key = make_group_key(&source_path);
            Candidate {
                index,
                source_path,
                key,
            }
        })
        .collect();

    let mut stem_groups: HashMap<String, Vec<Candidate>> = HashMap::new();
    for candidate in candidates {
        stem_groups
            .entry(candidate.key.clone())
            .or_default()
            .push(candidate);
    }

    if require_matching_exif {
        let groupable_paths: Vec<PathBuf> = stem_groups
            .values()
            .filter(|candidates| candidates.len() >= 2)
            .flat_map(|candidates| candidates.iter().map(|c| c.source_path.clone()))
            .collect();
        let exif_dates: HashMap<PathBuf, Option<chrono::DateTime<chrono::Utc>>> = groupable_paths
            .par_iter()
            .map(|p| {
                (
                    p.clone(),
                    crate::exif_processing::try_get_exif_creation_date(p),
                )
            })
            .collect();

        stem_groups.retain(|_, candidates| {
            if candidates.len() < 2 {
                return false;
            }
            let first = exif_dates
                .get(&candidates[0].source_path)
                .copied()
                .flatten();
            if first.is_none() {
                return false;
            }
            candidates
                .iter()
                .skip(1)
                .all(|c| exif_dates.get(&c.source_path).copied().flatten() == first)
        });
    } else {
        stem_groups.retain(|_, candidates| candidates.len() >= 2);
    }

    for (key, candidates) in stem_groups {
        for candidate in candidates {
            files[candidate.index].group_id = Some(key.clone());
        }
    }
}

pub use rawtonic_export::sidecars::parse_virtual_path;

#[tauri::command]
pub async fn read_exif_for_paths(
    paths: Vec<String>,
    state: tauri::State<'_, AppState>,
) -> Result<HashMap<String, HashMap<String, String>>, String> {
    let is_hdd = state
        .thumbnail_manager
        .rotational_disk
        .load(Ordering::Relaxed);

    tauri::async_runtime::spawn_blocking(move || {
        let process_path = |virtual_path: &String| {
            let (source_path, _) = parse_virtual_path(virtual_path);
            let source_path_str = source_path.to_string_lossy().to_string();

            let map = if let Some(sidecar_exif) =
                crate::exif_processing::read_rrexif_sidecar(&source_path)
            {
                sidecar_exif
            } else if is_cloud_placeholder(&source_path) {
                HashMap::new()
            } else if let Ok(mmap) = read_file_mapped(&source_path) {
                crate::exif_processing::read_exif_data(&source_path_str, &mmap)
            } else if let Ok(bytes) = fs::read(&source_path) {
                crate::exif_processing::read_exif_data(&source_path_str, &bytes)
            } else {
                HashMap::new()
            };

            if map.is_empty() {
                None
            } else {
                Some((virtual_path.clone(), map))
            }
        };

        let exif_data: HashMap<String, HashMap<String, String>> = if is_hdd {
            paths.iter().filter_map(process_path).collect()
        } else {
            paths.par_iter().filter_map(process_path).collect()
        };

        Ok(exif_data)
    })
    .await
    .unwrap_or_else(|e| Err(format!("Task failed: {}", e)))
}

/// The size in bytes of a photograph's file (a virtual copy's is its
/// original's), for the Metadata tool's file details. Reads only.
#[tauri::command]
pub async fn file_size(path: String) -> Result<u64, String> {
    let (source_path, _) = parse_virtual_path(&path);
    tauri::async_runtime::spawn_blocking(move || {
        fs::metadata(&source_path)
            .map(|metadata| metadata.len())
            .map_err(|e| format!("{}: {}", source_path.display(), e))
    })
    .await
    .map_err(|e| format!("Task failed: {}", e))?
}

#[tauri::command]
pub async fn update_exif_fields(
    paths: Vec<String>,
    updates: HashMap<String, String>,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let failures: Vec<String> = paths
            .par_iter()
            .filter_map(|path| update_exif_of(path, &updates).err())
            .collect();
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("; "))
        }
    })
    .await
    .map_err(|e| format!("Task failed: {}", e))?
}

/// Applies `updates` to one photo's EXIF fields in its sidecar (an empty value
/// removes the field). Takes its turn with the other sidecar field edits, so a
/// rating or recipe saved meanwhile is not overwritten.
fn update_exif_of(path: &str, updates: &HashMap<String, String>) -> Result<(), String> {
    let original_path = Path::new(path);
    let primary_path = crate::exif_processing::get_primary_sidecar_path(original_path);
    let _turn = super::sidecar_field_edit();
    let mut metadata = crate::exif_processing::load_sidecar(&primary_path);

    let mut exif_data = metadata.exif.take().unwrap_or_else(|| {
        if let Some(existing) = crate::exif_processing::read_rrexif_sidecar(original_path) {
            existing
        } else if let Ok(mmap) = read_file_mapped(original_path) {
            crate::exif_processing::read_exif_data_from_bytes(path, &mmap)
        } else if let Ok(bytes) = fs::read(original_path) {
            crate::exif_processing::read_exif_data_from_bytes(path, &bytes)
        } else {
            HashMap::new()
        }
    });

    for (k, v) in updates {
        let trimmed = v.trim();
        if trimmed.is_empty() {
            exif_data.remove(k);
        } else {
            exif_data.insert(k.clone(), trimmed.to_string());
        }
    }

    metadata.exif = Some(exif_data);
    let json = serde_json::to_string_pretty(&metadata).map_err(|e| format!("{path}: {e}"))?;
    crate::session_bridge::write_editor_sidecar(&primary_path, json)
        .map_err(|e| format!("{path}: {e}"))
}

pub(crate) fn match_disk_kind(disks: &Disks, canonical: &Path) -> Option<bool> {
    let mut best_match: Option<(&Path, bool)> = None;

    for disk in disks.list() {
        let mount_point = disk.mount_point();
        if canonical.starts_with(mount_point) {
            let is_longer_match = best_match
                .map(|(current, _)| mount_point.as_os_str().len() > current.as_os_str().len())
                .unwrap_or(true);
            if is_longer_match {
                best_match = Some((mount_point, disk.kind() == sysinfo::DiskKind::HDD));
            }
        }
    }

    best_match.map(|(_, is_hdd)| is_hdd)
}

pub(crate) fn update_rotational_disk_flag(path: &str, app_handle: &AppHandle) {
    let state = app_handle.state::<crate::app_state::AppState>();
    let Ok(canonical) = Path::new(path).canonicalize() else {
        return;
    };

    let cached_match = {
        let cache = state.disks_cache.lock().unwrap();
        cache
            .as_ref()
            .and_then(|disks| match_disk_kind(disks, &canonical))
    };

    match cached_match {
        Some(is_hdd) => {
            state
                .thumbnail_manager
                .rotational_disk
                .store(is_hdd, Ordering::Relaxed);
        }
        None => {
            if !state.disks_cache_refreshing.swap(true, Ordering::Relaxed) {
                let refresh_app_handle = app_handle.clone();
                thread::spawn(move || {
                    let disks = Disks::new_with_refreshed_list();
                    let state = refresh_app_handle.state::<crate::app_state::AppState>();
                    *state.disks_cache.lock().unwrap() = Some(disks);
                    state.disks_cache_refreshing.store(false, Ordering::Relaxed);
                });
            }
        }
    }
}

pub fn list_images_in_dir(path: String, app_handle: AppHandle) -> Result<Vec<ImageFile>, String> {
    #[cfg(feature = "performance-profiling")]
    let performance_start = std::time::Instant::now();
    let settings = load_settings(app_handle.clone()).unwrap_or_default();
    let enable_xmp_sync = settings.enable_xmp_sync.unwrap_or(false);

    update_rotational_disk_flag(&path, &app_handle);

    // Photographs the library holds here whose file is gone (or whose
    // drive is disconnected) stay listed.
    let offline = crate::session_bridge::offline_in_folder(Path::new(&path), false);
    let entries = match fs::read_dir(&path) {
        Ok(entries) => entries,
        Err(_) if !offline.is_empty() => return Ok(offline_image_files(offline, &settings)),
        Err(e) => return Err(e.to_string()),
    };
    let mut images = Vec::new();
    let mut sidecars_by_filename: HashMap<String, Vec<Option<String>>> = HashMap::new();

    for entry in entries.filter_map(Result::ok) {
        let entry_path = entry.path();
        let file_name = entry
            .file_name()
            .into_string()
            .unwrap_or_else(|os| os.to_string_lossy().into_owned());

        if file_name.ends_with(".rrdata") {
            let base = &file_name[..file_name.len() - 7];

            let (source_filename, copy_id) =
                if base.len() >= 7 && base.as_bytes()[base.len() - 7] == b'.' {
                    let id = &base[base.len() - 6..];
                    if id.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')) {
                        (&base[..base.len() - 7], Some(id.to_string()))
                    } else {
                        (base, None)
                    }
                } else {
                    (base, None)
                };

            sidecars_by_filename
                .entry(source_filename.to_string())
                .or_default()
                .push(copy_id);
        } else if is_supported_image_file(&file_name) {
            images.push((file_name, entry_path));
        }
    }

    let metadata_snapshot = listing_metadata_snapshot(
        &images
            .iter()
            .map(|(_, path)| path.clone())
            .collect::<Vec<_>>(),
        &settings,
    );
    let tasks: Vec<_> = images
        .into_iter()
        .map(|(file_name, path_buf)| {
            let sidecars = rawtonic_export::sidecars::with_original(
                sidecars_by_filename.remove(&file_name).unwrap_or_default(),
            );
            let path_str = path_buf.to_string_lossy().into_owned();
            (path_str, file_name, path_buf, sidecars)
        })
        .collect();
    crate::session_bridge::adopt_virtual_copies(&listed_copies(
        tasks
            .iter()
            .map(|(_, file_name, path, sidecars)| (path, file_name, sidecars)),
    ));

    let mut result_list: Vec<ImageFile> = tasks
        .into_par_iter()
        .flat_map(|(path_str, file_name, path_buf, sidecars)| {
            let modified = fs::metadata(&path_buf)
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);

            let is_cloud_placeholder = is_cloud_placeholder(&path_buf);

            let mut file_results = Vec::with_capacity(sidecars.len());

            for copy_id_opt in sidecars {
                let (virtual_path, is_virtual_copy, sidecar_filename) = match copy_id_opt {
                    Some(id) => (
                        format!("{}?vc={}", path_str, id),
                        true,
                        format!("{}.{}.rrdata", file_name, id),
                    ),
                    None => (path_str.clone(), false, format!("{}.rrdata", file_name)),
                };

                let sidecar_path = path_buf.with_file_name(sidecar_filename);

                let xmp_is_placeholder = enable_xmp_sync
                    && resolve_xmp_path(&path_buf)
                        .is_some_and(|p| crate::file_management::is_cloud_placeholder(&p));

                let metadata = if crate::file_management::is_cloud_placeholder(&sidecar_path)
                    || xmp_is_placeholder
                {
                    enqueue_metadata(
                        &app_handle,
                        virtual_path.clone(),
                        path_buf.clone(),
                        sidecar_path.clone(),
                    );
                    ImageFileMetadata {
                        is_edited: false,
                        tags: None,
                        rating: 0,
                        is_raw: crate::formats::is_raw_file(&path_buf),
                    }
                } else if let Some(metadata) = metadata_snapshot.get(&sidecar_path) {
                    metadata.clone()
                } else {
                    resolve_image_metadata(&path_buf, &sidecar_path, enable_xmp_sync, &settings)
                };

                file_results.push(ImageFile {
                    path: virtual_path,
                    modified,
                    is_edited: metadata.is_edited,
                    tags: metadata.tags,
                    exif: None,
                    is_virtual_copy,
                    is_raw: metadata.is_raw,
                    group_id: None,
                    rating: metadata.rating,
                    is_cloud_placeholder,
                    is_offline: false,
                    offline_volume: None,
                });
            }

            file_results
        })
        .collect();

    result_list.extend(offline_image_files(offline, &settings));
    assign_group_ids(&mut result_list, &settings);
    #[cfg(feature = "performance-profiling")]
    log::info!(
        "[PERF] {}",
        serde_json::json!({
            "stage": "list_images", "ms": performance_start.elapsed().as_secs_f64() * 1000.0,
            "assets": result_list.len()
        })
    );
    Ok(result_list)
}

pub fn list_images_recursive(
    path: String,
    app_handle: AppHandle,
) -> Result<Vec<ImageFile>, String> {
    #[cfg(feature = "performance-profiling")]
    let _stage = crate::performance::PerformanceStage::new("list_images_recursive");
    let settings = load_settings(app_handle.clone()).unwrap_or_default();
    let enable_xmp_sync = settings.enable_xmp_sync.unwrap_or(false);

    update_rotational_disk_flag(&path, &app_handle);

    let sources = rawtonic_export::sidecars::sources_with_copies(Path::new(&path));
    let images: Vec<PathBuf> = sources.iter().map(|(image, _)| image.clone()).collect();
    let mut sidecars_by_path: HashMap<PathBuf, Vec<Option<String>>> = sources.into_iter().collect();

    let metadata_snapshot = listing_metadata_snapshot(&images, &settings);
    let tasks: Vec<_> = images
        .into_iter()
        .map(|path_buf| {
            let sidecars = rawtonic_export::sidecars::with_original(
                sidecars_by_path.remove(&path_buf).unwrap_or_default(),
            );
            let path_str = path_buf.to_string_lossy().into_owned();
            let file_name = path_buf
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            (path_str, file_name, path_buf, sidecars)
        })
        .collect();
    crate::session_bridge::adopt_virtual_copies(&listed_copies(
        tasks
            .iter()
            .map(|(_, file_name, path, sidecars)| (path, file_name, sidecars)),
    ));

    let mut result_list: Vec<ImageFile> = tasks
        .into_par_iter()
        .flat_map(|(path_str, file_name, path_buf, sidecars)| {
            let modified = fs::metadata(&path_buf)
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);

            let is_cloud_placeholder = is_cloud_placeholder(&path_buf);

            let mut file_results = Vec::with_capacity(sidecars.len());

            for copy_id_opt in sidecars {
                let (virtual_path, is_virtual_copy, sidecar_filename) = match copy_id_opt {
                    Some(id) => (
                        format!("{}?vc={}", path_str, id),
                        true,
                        format!("{}.{}.rrdata", file_name, id),
                    ),
                    None => (path_str.clone(), false, format!("{}.rrdata", file_name)),
                };

                let sidecar_path = path_buf.with_file_name(sidecar_filename);

                let xmp_is_placeholder = enable_xmp_sync
                    && resolve_xmp_path(&path_buf)
                        .is_some_and(|p| crate::file_management::is_cloud_placeholder(&p));

                let metadata = if crate::file_management::is_cloud_placeholder(&sidecar_path)
                    || xmp_is_placeholder
                {
                    enqueue_metadata(
                        &app_handle,
                        virtual_path.clone(),
                        path_buf.clone(),
                        sidecar_path.clone(),
                    );
                    ImageFileMetadata {
                        is_edited: false,
                        tags: None,
                        rating: 0,
                        is_raw: crate::formats::is_raw_file(&path_buf),
                    }
                } else if let Some(metadata) = metadata_snapshot.get(&sidecar_path) {
                    metadata.clone()
                } else {
                    resolve_image_metadata(&path_buf, &sidecar_path, enable_xmp_sync, &settings)
                };

                file_results.push(ImageFile {
                    path: virtual_path,
                    modified,
                    is_edited: metadata.is_edited,
                    tags: metadata.tags,
                    exif: None,
                    is_virtual_copy,
                    is_raw: metadata.is_raw,
                    group_id: None,
                    rating: metadata.rating,
                    is_cloud_placeholder,
                    is_offline: false,
                    offline_volume: None,
                });
            }

            file_results
        })
        .collect();

    result_list.extend(offline_image_files(
        crate::session_bridge::offline_in_folder(Path::new(&path), true),
        &settings,
    ));
    assign_group_ids(&mut result_list, &settings);
    Ok(result_list)
}

/// Checks if the given path exists and is an iCloud placeholder file on macOS.
#[cfg(target_os = "macos")]
pub fn is_cloud_placeholder(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    const SF_DATALESS: u32 = 0x4000_0000;

    let c_path = match std::ffi::CString::new(path.as_os_str().as_bytes()) {
        Ok(p) => p,
        Err(_) => return false,
    };
    let mut stat_buf: libc::stat = unsafe { std::mem::zeroed() };
    let ret = unsafe { libc::lstat(c_path.as_ptr(), &mut stat_buf) };
    ret == 0 && (stat_buf.st_flags & SF_DATALESS) != 0
}

#[cfg(not(target_os = "macos"))]
pub fn is_cloud_placeholder(_path: &Path) -> bool {
    false
}

#[cfg(test)]
mod listing_metadata_tests {
    use super::*;
    use rawtonic_catalog::{Library, Mode};

    fn write_sidecar(source: &Path, rating: u8) -> PathBuf {
        let sidecar = source.with_file_name(format!(
            "{}.rrdata",
            source.file_name().unwrap().to_string_lossy()
        ));
        fs::write(
            &sidecar,
            serde_json::to_vec(&serde_json::json!({
                "version":1,"rating":rating,"tags":["physical"],"adjustments":{}
            }))
            .unwrap(),
        )
        .unwrap();
        sidecar
    }

    #[test]
    fn exif_field_edits_are_saved_and_a_failed_save_is_reported() {
        let temp = tempfile::tempdir().unwrap();
        let photo = temp.path().join("a.jpg");
        let sidecar = write_sidecar(&photo, 3);
        let updates = HashMap::from([("Artist".to_string(), " Ana ".to_string())]);
        tauri::async_runtime::block_on(update_exif_fields(
            vec![photo.to_string_lossy().into()],
            updates.clone(),
        ))
        .unwrap();
        let saved = crate::exif_processing::load_sidecar(&sidecar);
        assert_eq!(saved.exif.unwrap()["Artist"], "Ana");
        assert_eq!(saved.rating, 3);

        // The sidecar cannot be written into a folder that does not exist.
        let missing = temp.path().join("gone").join("b.jpg");
        let result = tauri::async_runtime::block_on(update_exif_fields(
            vec![missing.to_string_lossy().into()],
            updates,
        ));
        assert!(result.is_err(), "{result:?}");
    }

    #[test]
    fn listing_snapshot_preserves_db_clears_virtual_sidecars_and_real_fallbacks() {
        let temp = tempfile::tempdir().unwrap();
        let lib = Library::create(temp.path(), "Listing", Mode::Session).unwrap();
        let settings = AppSettings::default();
        let mut sources = Vec::new();
        for name in ["edited", "fallback", "unregistered", "neutral"] {
            let path = lib.directory.join(format!("Capture/{name}.arw"));
            fs::write(&path, b"fixture original").unwrap();
            if name != "unregistered" {
                lib.add_asset(&path).unwrap();
            }
            sources.push(path);
        }
        let edited = lib.asset_at(&sources[0]).unwrap().unwrap();
        lib.save_editor_state(
            &edited.id,
            &serde_json::json!({
                "version":1,"rating":4,"tags":["old"],"adjustments":{"exposure":0.5}
            }),
        )
        .unwrap();
        lib.set_metadata(&edited.id, &Default::default()).unwrap();
        let primary = write_sidecar(&sources[0], 5);
        let fallback = write_sidecar(&sources[1], 3);
        let unregistered = write_sidecar(&sources[2], 2);
        let virtual_sidecar = sources[0].with_file_name("edited.arw.abcdef.rrdata");
        fs::write(
            &virtual_sidecar,
            serde_json::to_vec(&serde_json::json!({
                "version":1,"rating":1,"tags":["virtual"],"adjustments":{}
            }))
            .unwrap(),
        )
        .unwrap();
        let before = fs::read(&primary).unwrap();
        let snapshot = listing_metadata_snapshot(&sources, &settings);
        assert_eq!(snapshot.len(), sources.len());
        for (source, sidecar) in sources[..3]
            .iter()
            .zip([&primary, &fallback, &unregistered])
        {
            assert_eq!(
                snapshot[sidecar],
                resolve_image_metadata(source, sidecar, true, &settings)
            );
        }
        assert_eq!(snapshot[&primary].rating, 0);
        assert_eq!(snapshot[&primary].tags, Some(Vec::new()));
        assert!(snapshot[&primary].is_edited);
        assert_eq!(snapshot[&fallback].rating, 3);
        assert_eq!(snapshot[&unregistered].rating, 2);
        assert!(!snapshot.contains_key(&virtual_sidecar));
        assert_eq!(
            resolve_image_metadata(&sources[0], &virtual_sidecar, true, &settings).rating,
            1
        );
        assert_eq!(fs::read(primary).unwrap(), before);
        for source in sources {
            assert_eq!(fs::read(source).unwrap(), b"fixture original");
        }
    }

    #[test]
    fn listing_snapshot_resolves_nested_sessions_and_leaves_standalone_fallbacks() {
        let temp = tempfile::tempdir().unwrap();
        let outer = Library::create(&temp.path().join("outer"), "Outer", Mode::Session).unwrap();
        let nested = Library::create(
            &outer.directory.join("Capture/nested"),
            "Nested",
            Mode::Session,
        )
        .unwrap();
        let mut sources = Vec::new();
        for (lib, rating) in [(&outer, 2), (&nested, 4)] {
            let path = lib.directory.join("Capture/same.arw");
            fs::write(&path, b"fixture").unwrap();
            let asset = lib.add_asset(&path).unwrap();
            lib.save_editor_state(
                &asset.id,
                &serde_json::json!({
                    "version":1,"rating":rating,"adjustments":{}
                }),
            )
            .unwrap();
            sources.push(path);
        }
        let standalone = temp.path().join("standalone.arw");
        fs::write(&standalone, b"fixture").unwrap();
        let standalone_sidecar = write_sidecar(&standalone, 5);
        sources.push(standalone.clone());
        let settings = AppSettings::default();
        let snapshot = listing_metadata_snapshot(&sources, &settings);
        assert_eq!(snapshot.len(), 2);
        for (source, rating) in sources[..2].iter().zip([2, 4]) {
            let sidecar = source.with_file_name("same.arw.rrdata");
            assert_eq!(snapshot[&sidecar].rating, rating);
            assert_eq!(
                snapshot[&sidecar],
                resolve_image_metadata(source, &sidecar, false, &settings)
            );
        }
        assert!(!snapshot.contains_key(&standalone_sidecar));
        assert_eq!(
            resolve_image_metadata(&standalone, &standalone_sidecar, false, &settings).rating,
            5
        );
    }
}
