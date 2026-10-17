//! Edits applied to selected photos: adjustments, auto corrections, ratings, labels and their sidecars.
//! Part of `file_management`, split by concern (ADR 0009).
use super::add_to_thumbnail_queue;
use super::emit_thumbnail_cache_setup_error;
use super::emit_thumbnail_generated;
use super::generate_single_thumbnail_and_cache;
use super::increment_thumbnail_progress;
use super::parse_virtual_path;
use super::resolve_thumbnail_cache_dir;
use super::sync_metadata_from_xmp;
use super::sync_metadata_to_xmp;
use crate::app_state::AppState;
use crate::formats::is_raw_file;
use crate::gpu_processing;
use crate::image_loader;
use crate::image_processing::ImageMetadata;
use crate::load_settings;
use crate::tagging::COLOR_TAG_PREFIX;
use image::DynamicImage;
use rayon::prelude::*;
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::thread;
use tauri::AppHandle;
use tauri::Manager;

pub fn resolve_lens_params_in_adjustments(
    adjustments: &mut Value,
    exif_data: &Option<HashMap<String, String>>,
    lens_db: Option<&crate::lens_correction::LensDatabase>,
    source: &Path,
) {
    if let Some(map) = adjustments.as_object_mut() {
        let mode = map
            .get("lensCorrectionMode")
            .and_then(|v| v.as_str())
            .unwrap_or("manual");

        // The camera's own data: no lens database entry to resolve.
        if mode == crate::embedded_lens::CAMERA_MODE {
            let recipe = Value::Object(map.clone());
            match crate::embedded_lens::params_for_recipe(source, &recipe) {
                Some(params) => {
                    map.insert(
                        "lensDistortionParams".to_string(),
                        serde_json::to_value(params).unwrap_or_default(),
                    );
                }
                None => {
                    map.remove("lensDistortionParams");
                }
            }
            return;
        }

        if mode == "auto" {
            if let Some(exif) = exif_data {
                let exif_maker = exif.get("Make").map(|s| s.as_str()).unwrap_or("");
                let exif_model = exif.get("LensModel").map(|s| s.as_str()).unwrap_or("");
                let exif_camera_model = exif.get("Model").map(|s| s.as_str()).unwrap_or("");
                if let Some(db) = lens_db {
                    if let Some((detected_maker, detected_model)) =
                        crate::lens_correction::find_best_lens_match(
                            db,
                            exif_maker,
                            exif_model,
                            exif_camera_model,
                        )
                    {
                        map.insert(
                            "lensMaker".to_string(),
                            serde_json::to_value(&detected_maker).unwrap(),
                        );
                        map.insert(
                            "lensModel".to_string(),
                            serde_json::to_value(&detected_model).unwrap(),
                        );
                    } else {
                        map.remove("lensMaker");
                        map.remove("lensModel");
                    }
                }
            } else {
                map.remove("lensMaker");
                map.remove("lensModel");
            }
        }

        if let Some(db) = lens_db {
            let has_valid_lens = match (
                map.get("lensMaker").and_then(|v| v.as_str()),
                map.get("lensModel").and_then(|v| v.as_str()),
            ) {
                (Some(maker), Some(model)) if !maker.is_empty() && !model.is_empty() => {
                    let mut focal_length = 50.0;
                    let mut aperture = None;
                    let mut distance = None;

                    if let Some(exif) = exif_data {
                        if let Some(fl_str) = exif
                            .get("FocalLength")
                            .or(exif.get("FocalLengthIn35mmFilm"))
                            && let Ok(fl) = fl_str.replace(" mm", "").trim().parse::<f32>()
                        {
                            focal_length = fl;
                        }
                        if let Some(ap_str) = exif.get("ApertureValue").or(exif.get("FNumber"))
                            && let Ok(ap) = ap_str.replace("f/", "").trim().parse::<f32>()
                        {
                            aperture = Some(ap);
                        }
                        if let Some(dist_str) = exif.get("SubjectDistance")
                            && let Ok(dist) = dist_str.replace(" m", "").trim().parse::<f32>()
                        {
                            distance = Some(dist);
                        }
                    }

                    // The body's crop factor from the 35 mm equivalent.
                    let camera_crop = exif_data.as_ref().and_then(|exif| {
                        let number = |key: &str| {
                            exif.get(key)?
                                .replace("mm", "")
                                .trim()
                                .parse::<f32>()
                                .ok()
                                .filter(|v| *v > 0.0)
                        };
                        Some(number("FocalLengthIn35mmFilm")? / number("FocalLength")?)
                    });
                    if let Some(params) = crate::lens_correction::resolve_lens_params(
                        db,
                        maker,
                        model,
                        focal_length,
                        aperture,
                        distance,
                        camera_crop.filter(|c| (0.5..=8.0).contains(c)),
                    ) {
                        map.insert(
                            "lensDistortionParams".to_string(),
                            serde_json::to_value(params).unwrap(),
                        );
                        true
                    } else {
                        false
                    }
                }
                _ => false,
            };

            if !has_valid_lens {
                map.remove("lensDistortionParams");
            }
        }
    }
}

/// Off the main thread: it opens the session catalog, reads settings and
/// writes sidecars, and a busy database must not stall the interface. The
/// webview chains saves per photograph and reads a recipe only after its
/// pending save finished (`debouncedSave`, `afterRecipeSaves`), so no order
/// is lost by leaving the main thread.
#[tauri::command]
pub async fn save_metadata_and_update_thumbnail(
    path: String,
    adjustments: Value,
    app_handle: AppHandle,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        save_metadata_and_update_thumbnail_blocking(path, adjustments, app_handle)
    })
    .await
    .map_err(|e| format!("Saving metadata failed: {e}"))?
}

pub(crate) fn save_metadata_and_update_thumbnail_blocking(
    path: String,
    adjustments: Value,
    app_handle: AppHandle,
) -> Result<(), String> {
    let state = app_handle.state::<AppState>();
    let (source_path, sidecar_path) = parse_virtual_path(&path);

    let mut metadata = crate::exif_processing::load_sidecar_with_exif(&sidecar_path, &source_path);

    let mut final_adjustments = adjustments;
    {
        let lens_db_guard = crate::lens_correction::global_lens_db();
        resolve_lens_params_in_adjustments(
            &mut final_adjustments,
            &metadata.exif,
            lens_db_guard.as_deref(),
            &source_path,
        );
    }

    metadata.adjustments = final_adjustments;

    let json_string = serde_json::to_string_pretty(&metadata).map_err(|e| e.to_string())?;
    crate::session_bridge::write_editor_sidecar(&sidecar_path, json_string)
        .map_err(|e| e.to_string())?;

    if let Ok(settings) = load_settings(app_handle.clone())
        && settings.enable_xmp_sync.unwrap_or(false)
    {
        let create_if_missing = settings.create_xmp_if_missing.unwrap_or(false);
        sync_metadata_to_xmp(&source_path, &metadata, create_if_missing);
    }

    let loaded_image_lock = state.original_image.lock().unwrap();
    let preloaded_image_option = if let Some(loaded_image) = loaded_image_lock.as_ref() {
        if loaded_image.path == path {
            Some(loaded_image.image.clone())
        } else {
            None
        }
    } else {
        None
    };
    drop(loaded_image_lock);

    let gpu_context = gpu_processing::get_or_init_gpu_context(&state, &app_handle).ok();
    let app_handle_clone = app_handle.clone();
    let path_clone = path.clone();

    add_to_thumbnail_queue(&state, 1, &app_handle);

    thread::spawn(move || {
        let state = app_handle_clone.state::<AppState>();
        let settings = load_settings(app_handle_clone.clone()).unwrap_or_default();

        let thumb_cache_dir = match resolve_thumbnail_cache_dir(&app_handle_clone) {
            Ok(dir) => dir,
            Err(e) => {
                log::warn!(
                    "Unable to initialize thumbnail cache directory for '{}': {}",
                    path_clone,
                    e
                );
                emit_thumbnail_cache_setup_error(&app_handle_clone, &path_clone, &e);
                increment_thumbnail_progress(&state, &app_handle_clone);
                return;
            }
        };

        let result = generate_single_thumbnail_and_cache(
            &path_clone,
            &thumb_cache_dir,
            gpu_context.as_ref(),
            preloaded_image_option.as_deref(),
            true,
            &app_handle_clone,
            &settings,
        );

        if let Some((small_path, medium_path, rating, is_edited)) = result {
            emit_thumbnail_generated(
                &app_handle_clone,
                &path_clone,
                &small_path,
                &medium_path,
                rating,
                is_edited,
            );
        }

        increment_thumbnail_progress(&state, &app_handle_clone);
    });

    Ok(())
}

/// Regenerates the thumbnails of `paths` in the background, after their
/// recipes were written (the thumbnail queue already counts them).
fn regenerate_thumbnails(app_handle: AppHandle, paths: Vec<String>) {
    tauri::async_runtime::spawn_blocking(move || {
        let settings = load_settings(app_handle.clone()).unwrap_or_default();
        let state = app_handle.state::<AppState>();
        let thumb_cache_dir = match resolve_thumbnail_cache_dir(&app_handle) {
            Ok(dir) => dir,
            Err(e) => {
                log::warn!("Unable to initialize thumbnail cache directory: {}", e);
                for path in &paths {
                    emit_thumbnail_cache_setup_error(&app_handle, path, &e);
                }
                for _ in 0..paths.len() {
                    increment_thumbnail_progress(&state, &app_handle);
                }
                return;
            }
        };

        let gpu_context = gpu_processing::get_or_init_gpu_context(&state, &app_handle).ok();

        paths.par_iter().for_each(|path_str| {
            let result = generate_single_thumbnail_and_cache(
                path_str,
                &thumb_cache_dir,
                gpu_context.as_ref(),
                None,
                true,
                &app_handle,
                &settings,
            );

            if let Some((small_path, medium_path, rating, is_edited)) = result {
                emit_thumbnail_generated(
                    &app_handle,
                    path_str,
                    &small_path,
                    &medium_path,
                    rating,
                    is_edited,
                );
            }

            increment_thumbnail_progress(&state, &app_handle);
        });
    });
}

/// `Some(create if missing)` when edits are synced to XMP sidecars.
fn xmp_sync(app_handle: &AppHandle) -> Option<bool> {
    let settings = load_settings(app_handle.clone()).unwrap_or_default();
    settings
        .enable_xmp_sync
        .unwrap_or(false)
        .then(|| settings.create_xmp_if_missing.unwrap_or(false))
}

/// Writes the editor sidecar of each of `paths` as `edit` changes its
/// metadata (in parallel), and its XMP when `xmp` (`xmp_sync`). Returns
/// the photos whose sidecar could not be written, with the reason; an
/// `edit` error counts as one.
fn edit_sidecars(
    paths: &[String],
    xmp: Option<bool>,
    edit: impl Fn(&str, &Path, &Path) -> Result<ImageMetadata, String> + Sync,
) -> Vec<(String, String)> {
    paths
        .par_iter()
        .filter_map(|path| {
            let (source_path, sidecar_path) = parse_virtual_path(path);
            let saved = edit(path, &source_path, &sidecar_path)
                .and_then(|metadata| save_sidecar(path, &metadata).map(|()| metadata));
            match saved {
                Ok(metadata) => {
                    if let Some(create_if_missing) = xmp {
                        sync_metadata_to_xmp(&source_path, &metadata, create_if_missing);
                    }
                    None
                }
                Err(e) => Some((path.clone(), e)),
            }
        })
        .collect()
}

/// Runs the blocking `work` (which writes the recipes of `paths` and returns
/// the failures), then regenerates the thumbnails in the background. The
/// command finishes once every recipe is written, so the webview reads the
/// new recipes afterwards and hears about the ones not saved.
async fn save_then_regenerate(
    app_handle: AppHandle,
    paths: Vec<String>,
    work: impl FnOnce(&AppHandle, &[String]) -> Vec<(String, String)> + Send + 'static,
) -> Result<(), SaveFailure> {
    let state = app_handle.state::<AppState>();
    add_to_thumbnail_queue(&state, paths.len(), &app_handle);
    let (app, batch) = (app_handle.clone(), paths.clone());
    let failures = tauri::async_runtime::spawn_blocking(move || work(&app, &batch))
        .await
        .unwrap_or_else(|e| {
            paths
                .iter()
                .map(|path| (path.clone(), format!("Saving failed: {e}")))
                .collect()
        });
    let total = paths.len();
    regenerate_thumbnails(app_handle, paths);
    SaveFailure::check(total, failures)
}

#[tauri::command]
pub async fn apply_adjustments_to_paths(
    paths: Vec<String>,
    adjustments: Value,
    app_handle: AppHandle,
) -> Result<(), SaveFailure> {
    save_then_regenerate(app_handle, paths, move |app_handle, paths| {
        let settings = load_settings(app_handle.clone()).unwrap_or_default();
        let lens_db = crate::lens_correction::global_lens_db();

        // Lightroom geometry (gradients, brushes) from an imported preset is placed in each
        // photo's pixels now, so photos that are never opened export with
        // them too. Decoded one at a time (memory), only when needed; a fast
        // decode's size is scaled back to the full frame (a pixel or two of
        // difference does not matter for gradients).
        let needs_geometry = adjustments.to_string().contains("lrNormalized");
        let placed: HashMap<String, (Value, (f64, f64))> = if needs_geometry {
            paths
                .iter()
                .filter_map(|path| {
                    let (source_path, _) = parse_virtual_path(path);
                    let bytes = fs::read(&source_path).ok()?;
                    let name = source_path.to_string_lossy();
                    let image = crate::image_loader::load_base_image_from_bytes(
                        &bytes, &name, true, &settings, None,
                    )
                    .ok()?;
                    let factor = if is_raw_file(&source_path) {
                        crate::raw_processing::get_fast_demosaic_scale_factor(
                            &bytes,
                            image.width(),
                            image.height(),
                        )
                    } else {
                        1.0
                    };
                    let mut resolved = adjustments.clone();
                    let size = (
                        image.width() as f64 / factor as f64,
                        image.height() as f64 / factor as f64,
                    );
                    crate::preset_converter::place_lightroom_geometry(
                        &mut resolved,
                        size.0,
                        size.1,
                        crate::image_loader::source_orientation(&bytes, &name),
                    );
                    Some((path.clone(), (resolved, size)))
                })
                .collect()
        } else {
            Default::default()
        };

        edit_sidecars(
            paths,
            xmp_sync(app_handle),
            |path, source_path, sidecar_path| {
                let (adjustments, size) = placed
                    .get(path)
                    .map_or((&adjustments, None), |(placed, size)| (placed, Some(*size)));
                let mut existing_metadata =
                    crate::exif_processing::load_sidecar_with_exif(sidecar_path, source_path);

                let mut new_adjustments = existing_metadata.adjustments;
                if new_adjustments.is_null() {
                    new_adjustments = serde_json::json!({});
                }
                let previous = new_adjustments.clone();

                if let (Some(new_map), Some(pasted_map)) =
                    (new_adjustments.as_object_mut(), adjustments.as_object())
                {
                    for (k, v) in pasted_map {
                        new_map.insert(k.clone(), v.clone());
                    }
                }
                // Pasted, synced or preset geometry: the photo's own drawn
                // masks stay on its content.
                crate::mask_generation::keep_drawn_masks_on_photo(
                    &previous,
                    &mut new_adjustments,
                    size,
                );

                resolve_lens_params_in_adjustments(
                    &mut new_adjustments,
                    &existing_metadata.exif,
                    lens_db.as_deref(),
                    source_path,
                );

                existing_metadata.adjustments = new_adjustments;
                Ok(existing_metadata)
            },
        )
    })
    .await
}

#[tauri::command]
pub async fn reset_adjustments_for_paths(
    paths: Vec<String>,
    app_handle: AppHandle,
) -> Result<(), SaveFailure> {
    save_then_regenerate(app_handle, paths, |app_handle, paths| {
        edit_sidecars(paths, xmp_sync(app_handle), |_, _, sidecar_path| {
            let mut existing_metadata = crate::exif_processing::load_sidecar(sidecar_path);
            existing_metadata.adjustments = serde_json::json!({});
            Ok(existing_metadata)
        })
    })
    .await
}

#[tauri::command]
pub async fn apply_auto_lens_correction_to_paths(
    paths: Vec<String>,
    app_handle: AppHandle,
) -> Result<(), SaveFailure> {
    save_then_regenerate(app_handle, paths, |app_handle, paths| {
        let lens_db = crate::lens_correction::global_lens_db();
        edit_sidecars(
            paths,
            xmp_sync(app_handle),
            |_, source_path, sidecar_path| {
                let mut existing_metadata =
                    crate::exif_processing::load_sidecar_with_exif(sidecar_path, source_path);

                if existing_metadata.adjustments.is_null() {
                    existing_metadata.adjustments = serde_json::json!({});
                }

                if let Some(obj) = existing_metadata.adjustments.as_object_mut() {
                    obj.insert("lensCorrectionMode".to_string(), serde_json::json!("auto"));
                    obj.insert("lensDistortionEnabled".to_string(), serde_json::json!(true));
                    obj.insert("lensTcaEnabled".to_string(), serde_json::json!(true));
                    obj.insert("lensVignetteEnabled".to_string(), serde_json::json!(true));
                }

                resolve_lens_params_in_adjustments(
                    &mut existing_metadata.adjustments,
                    &existing_metadata.exif,
                    lens_db.as_deref(),
                    source_path,
                );
                Ok(existing_metadata)
            },
        )
    })
    .await
}

/// Decodes each photo for its auto adjustments, so it waits until every
/// recipe is written (or failed) and makes the thumbnails from the decoded
/// photos meanwhile.
#[tauri::command]
pub async fn apply_auto_adjustments_to_paths(
    paths: Vec<String>,
    app_handle: AppHandle,
) -> Result<(), SaveFailure> {
    let state = app_handle.state::<AppState>();
    add_to_thumbnail_queue(&state, paths.len(), &app_handle);
    let total = paths.len();
    let all = paths.clone();
    let failures = tauri::async_runtime::spawn_blocking(move || {
        let settings = load_settings(app_handle.clone()).unwrap_or_default();
        let enable_xmp_sync = settings.enable_xmp_sync.unwrap_or(false);
        let create_xmp_if_missing = settings.create_xmp_if_missing.unwrap_or(false);

        let state = app_handle.state::<AppState>();
        let thumb_cache_dir = resolve_thumbnail_cache_dir(&app_handle);
        if let Err(e) = &thumb_cache_dir {
            log::warn!("Unable to initialize thumbnail cache directory: {}", e);
        }

        let gpu_context = gpu_processing::get_or_init_gpu_context(&state, &app_handle).ok();

        paths
            .par_iter()
            .filter_map(|path| {
                let applied = (|| -> Result<DynamicImage, String> {
                    let (source_path, sidecar_path) = parse_virtual_path(path);
                    let source_path_str = source_path.to_string_lossy().to_string();

                    let file_bytes = fs::read(&source_path).map_err(|e| e.to_string())?;
                    let image = image_loader::load_base_image_from_bytes(
                        &file_bytes,
                        &source_path_str,
                        true,
                        &settings,
                        None,
                    )
                    .map_err(|e| e.to_string())?;

                    let mut existing_metadata = crate::exif_processing::load_sidecar(&sidecar_path);
                    let tone_mapper = existing_metadata
                        .adjustments
                        .get("toneMapper")
                        .and_then(|value| value.as_str())
                        .map(str::to_string);
                    let base_curve = existing_metadata
                        .adjustments
                        .get("baseCurve")
                        .and_then(|value| value.as_str())
                        .map(str::to_string);
                    let auto_adjustments_json = crate::image_processing::auto_adjustments_json(
                        &image,
                        &source_path_str,
                        crate::formats::is_raw_file(&source_path_str),
                        tone_mapper.as_deref(),
                        base_curve.as_deref(),
                    );

                    let previous = existing_metadata.adjustments.clone();
                    crate::auto_adjust::merge_auto_adjustments(
                        &mut existing_metadata.adjustments,
                        &auto_adjustments_json,
                    );
                    // Should Auto ever set the geometry, drawn masks stay
                    // on the photo's content (the decode may be reduced:
                    // frames take the photo's size when drawn).
                    crate::mask_generation::keep_drawn_masks_on_photo(
                        &previous,
                        &mut existing_metadata.adjustments,
                        None,
                    );

                    save_sidecar(path, &existing_metadata)?;

                    if enable_xmp_sync {
                        sync_metadata_to_xmp(
                            &source_path,
                            &existing_metadata,
                            create_xmp_if_missing,
                        );
                    }
                    Ok(image)
                })();
                if let Err(e) = &applied {
                    log::warn!("Failed to apply auto adjustments to {}: {}", path, e);
                }

                match &thumb_cache_dir {
                    Ok(thumb_cache_dir) => {
                        let result = generate_single_thumbnail_and_cache(
                            path,
                            thumb_cache_dir,
                            gpu_context.as_ref(),
                            applied.as_ref().ok(),
                            true,
                            &app_handle,
                            &settings,
                        );
                        if let Some((small_path, medium_path, rating, is_edited)) = result {
                            emit_thumbnail_generated(
                                &app_handle,
                                path,
                                &small_path,
                                &medium_path,
                                rating,
                                is_edited,
                            );
                        }
                    }
                    Err(e) => emit_thumbnail_cache_setup_error(&app_handle, path, e),
                }
                increment_thumbnail_progress(&state, &app_handle);

                applied.err().map(|e| (path.clone(), e))
            })
            .collect::<Vec<_>>()
    })
    .await
    .map_err(|e| SaveFailure {
        message: format!("Auto adjustments failed: {e}"),
        failed: all,
    })?;
    SaveFailure::check(total, failures)
}

/// A batch save that failed for some photos: which ones, and why. The
/// others were saved.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveFailure {
    pub message: String,
    pub failed: Vec<String>,
}

impl SaveFailure {
    /// `Ok` when nothing failed; otherwise the failed paths (as given) and a
    /// message naming the first few with their errors.
    pub fn check(total: usize, mut failures: Vec<(String, String)>) -> Result<(), SaveFailure> {
        if failures.is_empty() {
            return Ok(());
        }
        failures.sort();
        const SHOWN: usize = 3;
        let mut listed: Vec<String> = failures
            .iter()
            .take(SHOWN)
            .map(|(path, error)| format!("{path}: {error}"))
            .collect();
        if failures.len() > SHOWN {
            listed.push(format!("and {} more", failures.len() - SHOWN));
        }
        let what = if total == 1 {
            "the photo".to_string()
        } else {
            format!("{} of {total} photos", failures.len())
        };
        Err(SaveFailure {
            message: format!("Could not save {what}: {}", listed.join("; ")),
            failed: failures.into_iter().map(|(path, _)| path).collect(),
        })
    }
}

/// Writes `metadata` as the editor sidecar of the virtual path `path`.
fn save_sidecar(path: &str, metadata: &ImageMetadata) -> Result<(), String> {
    let (_, sidecar_path) = parse_virtual_path(path);
    let json_string = serde_json::to_string_pretty(metadata).map_err(|e| e.to_string())?;
    crate::session_bridge::write_editor_sidecar(&sidecar_path, json_string)
        .map_err(|e| e.to_string())
}

static SIDECAR_FIELD_EDITS: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Rating, colour label and keyword edits read a photo's sidecar, change
/// one field and write it back. They run on blocking workers (no longer one
/// after the other on the main thread), so they take turns through this
/// guard: none overwrites another's change.
pub(crate) fn sidecar_field_edit() -> std::sync::MutexGuard<'static, ()> {
    SIDECAR_FIELD_EDITS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Every photo of `paths` unsaved: the worker saving them stopped (`error`).
fn unsaved(paths: &[String], error: &str) -> Result<(), SaveFailure> {
    SaveFailure::check(
        paths.len(),
        paths
            .iter()
            .map(|path| (path.clone(), format!("Saving failed: {error}")))
            .collect(),
    )
}

/// Off the main thread: the save waits while the catalog is locked.
#[tauri::command]
pub async fn set_color_label_for_paths(
    paths: Vec<String>,
    color: Option<String>,
    app_handle: AppHandle,
) -> Result<(), SaveFailure> {
    let all = paths.clone();
    super::blocking(move || set_color_label_for_paths_blocking(paths, color, app_handle))
        .await
        .unwrap_or_else(|e| unsaved(&all, &e))
}

fn set_color_label_for_paths_blocking(
    paths: Vec<String>,
    color: Option<String>,
    app_handle: AppHandle,
) -> Result<(), SaveFailure> {
    let _turn = sidecar_field_edit();
    let settings = load_settings(app_handle.clone()).unwrap_or_default();
    let enable_xmp_sync = settings.enable_xmp_sync.unwrap_or(false);
    let create_xmp_if_missing = settings.create_xmp_if_missing.unwrap_or(false);

    let failures: Vec<(String, String)> = paths
        .par_iter()
        .filter_map(|path| {
            let (_, sidecar_path) = parse_virtual_path(path);

            let mut metadata = crate::exif_processing::load_sidecar(&sidecar_path);

            let mut tags = metadata.tags.unwrap_or_default();
            tags.retain(|tag| !tag.starts_with(COLOR_TAG_PREFIX));

            if let Some(c) = &color
                && !c.is_empty()
            {
                tags.push(format!("{}{}", COLOR_TAG_PREFIX, c));
            }

            if tags.is_empty() {
                metadata.tags = None;
            } else {
                metadata.tags = Some(tags);
            }

            if let Err(e) = save_sidecar(path, &metadata) {
                return Some((path.clone(), e));
            }

            if enable_xmp_sync {
                let source_path = parse_virtual_path(path).0;
                sync_metadata_to_xmp(&source_path, &metadata, create_xmp_if_missing);
            }
            None
        })
        .collect();

    SaveFailure::check(paths.len(), failures)
}

/// Off the main thread: the save waits while the catalog is locked.
#[tauri::command]
pub async fn set_rating_for_paths(
    paths: Vec<String>,
    rating: u8,
    app_handle: AppHandle,
) -> Result<(), SaveFailure> {
    let all = paths.clone();
    super::blocking(move || set_rating_for_paths_blocking(paths, rating, app_handle))
        .await
        .unwrap_or_else(|e| unsaved(&all, &e))
}

fn set_rating_for_paths_blocking(
    paths: Vec<String>,
    rating: u8,
    app_handle: AppHandle,
) -> Result<(), SaveFailure> {
    let _turn = sidecar_field_edit();
    let settings = load_settings(app_handle.clone()).unwrap_or_default();
    let enable_xmp_sync = settings.enable_xmp_sync.unwrap_or(false);
    let create_xmp_if_missing = settings.create_xmp_if_missing.unwrap_or(false);

    let failures: Vec<(String, String)> = paths
        .par_iter()
        .filter_map(|path| {
            let (_, sidecar_path) = parse_virtual_path(path);

            let mut metadata = crate::exif_processing::load_sidecar(&sidecar_path);

            metadata.rating = rating;

            if let Err(e) = save_sidecar(path, &metadata) {
                return Some((path.clone(), e));
            }

            if enable_xmp_sync {
                let source_path = parse_virtual_path(path).0;
                sync_metadata_to_xmp(&source_path, &metadata, create_xmp_if_missing);
            }
            None
        })
        .collect();

    SaveFailure::check(paths.len(), failures)
}

/// Off the main thread (see `save_metadata_and_update_thumbnail`).
#[tauri::command]
pub async fn load_metadata(path: String, app_handle: AppHandle) -> Result<ImageMetadata, String> {
    #[cfg(feature = "performance-profiling")]
    let received = std::time::SystemTime::now();
    let result =
        tauri::async_runtime::spawn_blocking(move || load_metadata_blocking(path, app_handle))
            .await
            .map_err(|e| format!("Loading metadata failed: {e}"))?;
    #[cfg(feature = "performance-profiling")]
    log::info!(
        "[PERF] {}",
        serde_json::json!({
            "stage": "load_metadata",
            "ms": received.elapsed().map_or(0.0, |d| d.as_secs_f64() * 1000.0),
            "start_epoch_ms": received
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0.0, |d| d.as_secs_f64() * 1000.0),
        })
    );
    result
}

pub(crate) fn load_metadata_blocking(
    path: String,
    app_handle: AppHandle,
) -> Result<ImageMetadata, String> {
    let settings = load_settings(app_handle).unwrap_or_default();
    let enable_xmp_sync = settings.enable_xmp_sync.unwrap_or(false);

    let (source_path, sidecar_path) = parse_virtual_path(&path);
    let mut metadata = crate::exif_processing::load_sidecar(&sidecar_path);

    if enable_xmp_sync
        && sync_metadata_from_xmp(&source_path, &mut metadata)
        && let Ok(json) = serde_json::to_string_pretty(&metadata)
    {
        let _ = crate::session_bridge::write_editor_sidecar(&sidecar_path, json);
    }

    Ok(metadata)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_batch_save_reports_every_failed_photo() {
        assert!(SaveFailure::check(3, vec![]).is_ok());
        let failures = (1..=5)
            .map(|n| (format!("/p/{n}.jpg"), "database is locked".to_string()))
            .rev()
            .collect();
        let failure = SaveFailure::check(8, failures).unwrap_err();
        assert_eq!(failure.failed.len(), 5);
        assert_eq!(failure.failed[0], "/p/1.jpg");
        assert_eq!(
            failure.message,
            "Could not save 5 of 8 photos: /p/1.jpg: database is locked; \
             /p/2.jpg: database is locked; /p/3.jpg: database is locked; and 2 more"
        );
        let one = SaveFailure::check(1, vec![("/p/a.jpg".into(), "denied".into())]);
        assert_eq!(
            one.unwrap_err().message,
            "Could not save the photo: /p/a.jpg: denied"
        );
    }

    /// The batch edits (paste, reset, auto lens) report each photo whose
    /// recipe could not be written; the others are written.
    #[test]
    fn batch_edits_report_the_sidecars_not_written() {
        let tmp = tempfile::tempdir().unwrap();
        let ok = tmp.path().join("A.jpg");
        let blocked = tmp.path().join("B.jpg");
        std::fs::write(&ok, b"a").unwrap();
        std::fs::write(&blocked, b"b").unwrap();
        // A folder where B's sidecar would go: it cannot be written.
        std::fs::create_dir(tmp.path().join("B.jpg.rrdata")).unwrap();
        let paths: Vec<String> = [&ok, &blocked]
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        let failures = edit_sidecars(&paths, None, |_, _, sidecar| {
            let mut metadata = crate::exif_processing::load_sidecar(sidecar);
            metadata.adjustments = serde_json::json!({ "exposure": 1.5 });
            Ok(metadata)
        });
        assert_eq!(failures.len(), 1, "{failures:?}");
        assert_eq!(failures[0].0, paths[1]);
        let written = crate::exif_processing::load_sidecar(&tmp.path().join("A.jpg.rrdata"));
        assert_eq!(written.adjustments["exposure"], 1.5);
        assert!(tmp.path().join("B.jpg.rrdata").is_dir());
        let failure = SaveFailure::check(paths.len(), failures).unwrap_err();
        assert_eq!(failure.failed, [paths[1].clone()]);
    }
}
