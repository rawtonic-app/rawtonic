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
use tauri::Emitter;
use tauri::Manager;

pub use rawtonic_engine::lens_recipe::{
    LensLookup, resolve_lens_params_in_adjustments, stamp_loaded_lens_profile,
};

/// Off the main thread: it opens the session catalog, reads settings and
/// writes sidecars, and a busy database must not stall the interface. The
/// webview submits every write of a photograph to its one write queue and
/// reads a recipe only after the writes before (`src/utils/photoWrites.ts`),
/// so no order is lost by leaving the main thread.
#[tauri::command]
pub async fn save_metadata_and_update_thumbnail(
    path: String,
    adjustments: Value,
    update_thumbnail: Option<bool>,
    app_handle: AppHandle,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        // A caller whose loaded decode is stale (an Edit With render another
        // program saved) asks for the recipe alone.
        if update_thumbnail == Some(false) {
            return save_metadata_blocking(&path, adjustments, &app_handle);
        }
        save_metadata_and_update_thumbnail_blocking(path, adjustments, app_handle)
    })
    .await
    .map_err(|e| format!("Saving metadata failed: {e}"))?
}

/// Writes `adjustments` as the recipe of `path` (sidecar or library, and its
/// XMP when that is synchronised).
fn save_metadata_blocking(
    path: &str,
    adjustments: Value,
    app_handle: &AppHandle,
) -> Result<(), String> {
    let (source_path, sidecar_path) = parse_virtual_path(path);

    let turn = sidecar_field_edit();
    let mut metadata = crate::exif_processing::load_sidecar_with_exif(&sidecar_path, &source_path);

    let mut final_adjustments = adjustments;
    {
        let lens_db_guard = crate::lens_correction::global_lens_db();
        resolve_lens_params_in_adjustments(
            &mut final_adjustments,
            LensLookup::Keep {
                previous: Some(&metadata.adjustments),
            },
            &metadata.exif,
            lens_db_guard.as_deref(),
            &source_path,
        );
    }

    metadata.adjustments = final_adjustments;

    let json_string = serde_json::to_string_pretty(&metadata).map_err(|e| e.to_string())?;
    crate::session_bridge::write_editor_sidecar(&sidecar_path, json_string)
        .map_err(|e| e.to_string())?;
    drop(turn);

    if let Ok(settings) = load_settings(app_handle.clone())
        && settings.enable_xmp_sync.unwrap_or(false)
    {
        let create_if_missing = settings.create_xmp_if_missing.unwrap_or(false);
        sync_metadata_to_xmp(&source_path, &metadata, create_if_missing);
    }

    Ok(())
}

pub(crate) fn save_metadata_and_update_thumbnail_blocking(
    path: String,
    adjustments: Value,
    app_handle: AppHandle,
) -> Result<(), String> {
    save_metadata_blocking(&path, adjustments, &app_handle)?;
    let state = app_handle.state::<AppState>();

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
            let _turn = sidecar_field_edit();
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

/// One photograph's whole recipe before and after a batch edit: its part of
/// the edit's undo step (`replace_recipes` writes either back).
#[derive(serde::Serialize, Debug, Clone)]
pub struct RecipeChange {
    pub path: String,
    pub before: Value,
    pub after: Value,
}

/// What a batch recipe edit wrote: each written photograph's recipe before
/// and after, and the photographs not written with the first errors.
#[derive(serde::Serialize, Debug, Default)]
pub struct RecipeBatch {
    pub changes: Vec<RecipeChange>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<SaveFailure>,
}

/// `edit_sidecars` that also returns the recipe changes of the photographs
/// written (read in the same sidecar turn as the edit).
fn edit_sidecars_recorded(
    paths: &[String],
    xmp: Option<bool>,
    edit: impl Fn(&str, &Path, &Path) -> Result<ImageMetadata, String> + Sync,
) -> (Vec<RecipeChange>, Vec<(String, String)>) {
    let changes = std::sync::Mutex::new(Vec::new());
    let failures = edit_sidecars(paths, xmp, |path, source_path, sidecar_path| {
        let before = crate::exif_processing::load_sidecar(sidecar_path).adjustments;
        let metadata = edit(path, source_path, sidecar_path)?;
        changes.lock().unwrap().push(RecipeChange {
            path: path.to_string(),
            before,
            after: metadata.adjustments.clone(),
        });
        Ok(metadata)
    });
    let mut changes = changes.into_inner().unwrap();
    // An edit whose save failed was not written.
    changes.retain(|change| !failures.iter().any(|(path, _)| *path == change.path));
    (changes, failures)
}

/// `save_then_regenerate` for a recorded batch edit (`edit_sidecars_recorded`).
async fn recorded_batch(
    app_handle: AppHandle,
    paths: Vec<String>,
    work: impl FnOnce(&AppHandle, &[String]) -> (Vec<RecipeChange>, Vec<(String, String)>)
    + Send
    + 'static,
) -> RecipeBatch {
    let recorded = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = recorded.clone();
    let result = save_then_regenerate(app_handle, paths, move |app_handle, paths| {
        let (changes, failures) = work(app_handle, paths);
        *sink.lock().unwrap() = changes;
        failures
    })
    .await;
    let changes = std::mem::take(&mut *recorded.lock().unwrap());
    RecipeBatch {
        changes,
        failure: result.err(),
    }
}

/// Where pasted or synced settings come from (`apply_adjustments_to_paths`).
#[derive(serde::Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PasteFrom {
    /// The photograph (or virtual copy) they were copied from.
    pub source: String,
    /// The layers replace each photograph's own (Edit all selected) instead
    /// of joining them.
    #[serde(default)]
    pub replace_layers: bool,
    /// The processing versions a new edit gets (RAW, other): a photograph
    /// never edited takes them as its own before the settings are pasted
    /// (settings chosen in the picker carry versions only when chosen).
    #[serde(default)]
    pub new_edit: Option<NewEditResponses>,
}

/// The processing versions of a new edit (`newEditResponses` in the editor).
#[derive(serde::Deserialize, Debug, Clone, Default)]
pub struct NewEditResponses {
    pub raw: serde_json::Map<String, Value>,
    pub other: serde_json::Map<String, Value>,
}

/// A style applied from the styles list (`apply_adjustments_to_paths`): it
/// replaces the style last applied to each photograph, or with `remove`
/// takes `id` back from the photographs it is the last applied style of
/// (`rawtonic_engine::applied_style`).
#[derive(serde::Deserialize, Debug, Clone)]
pub struct StyleOp {
    pub id: String,
    #[serde(default)]
    pub remove: bool,
}

/// Applies `adjustments` to `paths`. With `paste` (copied or synced
/// settings), their layers join each photograph's own
/// (`masks::paste_layers`: same layer replaced, others added, AI detections
/// waiting for their own photograph); otherwise (presets) they replace them.
/// With `style`, each photograph's last applied style is taken back first
/// and the style recorded with that photograph's own values before it.
#[tauri::command]
pub async fn apply_adjustments_to_paths(
    paths: Vec<String>,
    adjustments: Value,
    paste: Option<PasteFrom>,
    style: Option<StyleOp>,
    app_handle: AppHandle,
) -> Result<RecipeBatch, String> {
    Ok(recorded_batch(app_handle, paths, move |app_handle, paths| {
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

        edit_sidecars_recorded(
            paths,
            xmp_sync(app_handle),
            |path, source_path, sidecar_path| {
                let (adjustments, size) = placed
                    .get(path)
                    .map_or((&adjustments, None), |(placed, size)| (placed, Some(*size)));
                pasted_metadata(
                    path,
                    source_path,
                    sidecar_path,
                    adjustments,
                    paste.as_ref(),
                    style.as_ref(),
                    size,
                    lens_db.as_deref(),
                )
            },
        )
    })
    .await)
}

/// The editor sidecar of `path` with `adjustments` applied
/// (`apply_adjustments_to_paths`): pasted keys replace the photo's, layers
/// as `paste` says; its drawn masks stay on its content and its lens data is
/// resolved again. Refused when the layers would exceed what the renderer
/// applies (it would keep but ignore the extra ones).
#[allow(clippy::too_many_arguments)]
fn pasted_metadata(
    path: &str,
    source_path: &Path,
    sidecar_path: &Path,
    adjustments: &Value,
    paste: Option<&PasteFrom>,
    style: Option<&StyleOp>,
    size: Option<(f64, f64)>,
    lens_db: Option<&rawtonic_engine::lens_correction::LensDatabase>,
) -> Result<ImageMetadata, String> {
    let mut existing_metadata =
        crate::exif_processing::load_sidecar_with_exif(sidecar_path, source_path);
    existing_metadata.adjustments = pasted_into(
        std::mem::take(&mut existing_metadata.adjustments),
        &existing_metadata.exif,
        path,
        source_path,
        adjustments,
        paste,
        style,
        size,
        lens_db,
    )?;
    Ok(existing_metadata)
}

/// `base` (a photograph's recipe) with `adjustments` pasted as
/// `apply_adjustments_to_paths` pastes them (`pasted_metadata`); the editor's
/// `pasteInto` pastes onto the open photograph by the same rules.
#[allow(clippy::too_many_arguments)]
fn pasted_into(
    base: Value,
    exif: &Option<HashMap<String, String>>,
    path: &str,
    source_path: &Path,
    adjustments: &Value,
    paste: Option<&PasteFrom>,
    style: Option<&StyleOp>,
    size: Option<(f64, f64)>,
    lens_db: Option<&rawtonic_engine::lens_correction::LensDatabase>,
) -> Result<Value, String> {
    use rawtonic_engine::applied_style;
    // Taking back a style the photograph does not have as its last: unchanged.
    if let Some(op) = style
        && op.remove
        && applied_style::applied_style_id(&base) != Some(op.id.as_str())
    {
        return Ok(base);
    }
    let mut new_adjustments = base;
    let new_edit = crate::image_processing::is_new_recipe(&new_adjustments);
    // A photo without an edit starts one: the current render
    // version, as the editor's new edits.
    crate::image_processing::apply_render_default(&mut new_adjustments);
    // Its processing versions are a new edit's own.
    if new_edit
        && let (Some(responses), Some(map)) = (
            paste.and_then(|p| p.new_edit.as_ref()),
            new_adjustments.as_object_mut(),
        )
    {
        let own = if is_raw_file(source_path) {
            &responses.raw
        } else {
            &responses.other
        };
        for (key, value) in own {
            map.entry(key.clone()).or_insert_with(|| value.clone());
        }
    }
    let previous = new_adjustments.clone();
    // The style before replaces or removes: the photograph as it was without it.
    let reverted = style.map(|_| {
        applied_style::revert(&mut new_adjustments);
        new_adjustments.clone()
    });

    if let (Some(new_map), Some(pasted_map)) =
        (new_adjustments.as_object_mut(), adjustments.as_object())
    {
        for (k, v) in pasted_map {
            // The frame of the RAW stays the photograph's own (ADR 0020).
            if k == rawtonic_engine::source_frame::KEY {
                continue;
            }
            // Copied or synced: a setting the source does not have is
            // removed here too (As Shot white balance over a custom one).
            if paste.is_some() && v.is_null() {
                new_map.remove(k);
                continue;
            }
            let v = match paste {
                Some(from) if k == "masks" => rawtonic_engine::masks::paste_layers(
                    new_map.get("masks").unwrap_or(&Value::Null),
                    v,
                    same_photograph(&from.source, path),
                    from.replace_layers,
                    &mut || uuid::Uuid::new_v4().to_string(),
                ),
                None if k == "masks" => {
                    let mut layers = v.clone();
                    rawtonic_engine::masks::prepare_layers_for_photo(&mut layers, &mut || {
                        uuid::Uuid::new_v4().to_string()
                    });
                    layers
                }
                // A style's groups join the photograph's own settings.
                _ if style.is_some() => {
                    applied_style::merged(&mut vec![k.clone()], new_map.get(k), v)
                }
                _ => v.clone(),
            };
            new_map.insert(k.clone(), v);
        }
    }
    let layers = |adjustments: &Value| {
        rawtonic_engine::masks::rendered_layers(adjustments.get("masks").unwrap_or(&Value::Null))
    };
    let (before, after) = (layers(&previous), layers(&new_adjustments));
    let limit = rawtonic_engine::image_processing::MAX_MASKS;
    if after > limit && after > before {
        return Err(format!(
            "{path}: {after} visible layers with the pasted ones; at most {limit} are applied"
        ));
    }
    // Pasted, synced or preset geometry: the photo's own drawn
    // masks stay on its content.
    crate::mask_generation::keep_drawn_masks_on_photo(&previous, &mut new_adjustments, size);

    resolve_lens_params_in_adjustments(
        &mut new_adjustments,
        LensLookup::Keep {
            previous: Some(&previous),
        },
        exif,
        lens_db,
        source_path,
    );
    if let (Some(op), Some(reverted)) = (style.filter(|op| !op.remove), reverted.as_ref()) {
        applied_style::record(reverted, &mut new_adjustments, adjustments, &op.id, 100.0);
    }
    Ok(new_adjustments)
}

/// The lens data a lens choice pasted onto the open photograph `path` means
/// there (`adjustments`, marked unresolved by the editor), resolved as its
/// save resolves it (`pasted_lens_data`): with the camera facts registered
/// for an offline photograph, its EXIF from the sidecar or the file.
#[tauri::command]
pub async fn resolve_pasted_lens(path: String, adjustments: Value) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let (source_path, sidecar_path) = parse_virtual_path(&path);
        let exif = crate::exif_processing::load_sidecar_with_exif(&sidecar_path, &source_path).exif;
        let lens_db = crate::lens_correction::global_lens_db();
        pasted_lens_data(adjustments, &exif, lens_db.as_deref(), &source_path)
    })
    .await
    .map_err(|e| e.to_string())
}

/// The lens keys of `adjustments` once its lens choice is resolved for
/// `source` (`null` for a key it does not have); `lensProfileFor` stays
/// `LENS_UNRESOLVED` while it cannot be resolved yet.
fn pasted_lens_data(
    mut adjustments: Value,
    exif: &Option<HashMap<String, String>>,
    lens_db: Option<&rawtonic_engine::lens_correction::LensDatabase>,
    source: &Path,
) -> Value {
    resolve_lens_params_in_adjustments(
        &mut adjustments,
        LensLookup::Keep { previous: None },
        exif,
        lens_db,
        source,
    );
    let keys = [
        "lensMaker",
        "lensModel",
        "lensDistortionParams",
        rawtonic_engine::lens_recipe::LENS_PROFILE_FOR,
    ];
    keys.iter()
        .map(|key| {
            let value = adjustments.get(*key).cloned().unwrap_or(Value::Null);
            (key.to_string(), value)
        })
        .collect::<serde_json::Map<_, _>>()
        .into()
}

/// Whether two photograph paths (virtual copies included) are of one file.
fn same_photograph(a: &str, b: &str) -> bool {
    let (a, _) = parse_virtual_path(a);
    let (b, _) = parse_virtual_path(b);
    a == b
        || a.canonicalize()
            .ok()
            .is_some_and(|a| b.canonicalize().ok() == Some(a))
}

#[tauri::command]
pub async fn reset_adjustments_for_paths(
    paths: Vec<String>,
    app_handle: AppHandle,
) -> Result<RecipeBatch, String> {
    Ok(recorded_batch(app_handle, paths, |app_handle, paths| {
        edit_sidecars_recorded(paths, xmp_sync(app_handle), |_, _, sidecar_path| {
            let mut existing_metadata = crate::exif_processing::load_sidecar(sidecar_path);
            // A new edit in the frame the photograph was edited in (ADR 0020).
            existing_metadata.adjustments =
                rawtonic_engine::source_frame::reset_recipe(&existing_metadata.adjustments);
            Ok(existing_metadata)
        })
    })
    .await)
}

/// One photograph's whole recipe for `replace_recipes`.
#[derive(serde::Deserialize, Debug, Clone)]
pub struct RecipeReplacement {
    pub path: String,
    pub adjustments: Value,
}

/// Writes each photograph's whole recipe back as it was (the undo and redo
/// of a batch edit, `RecipeChange`). Writing it again is harmless, so a
/// partly failed undo can be retried.
#[tauri::command]
pub async fn replace_recipes(
    changes: Vec<RecipeReplacement>,
    app_handle: AppHandle,
) -> Result<RecipeBatch, String> {
    let paths: Vec<String> = changes.iter().map(|c| c.path.clone()).collect();
    let by_path: HashMap<String, Value> = changes
        .into_iter()
        .map(|c| (c.path, c.adjustments))
        .collect();
    Ok(recorded_batch(app_handle, paths, move |app_handle, paths| {
        edit_sidecars_recorded(paths, xmp_sync(app_handle), |path, _, sidecar_path| {
            let mut metadata = crate::exif_processing::load_sidecar(sidecar_path);
            metadata.adjustments = by_path.get(path).ok_or("no recipe")?.clone();
            Ok(metadata)
        })
    })
    .await)
}

#[tauri::command]
pub async fn apply_auto_lens_correction_to_paths(
    paths: Vec<String>,
    app_handle: AppHandle,
) -> Result<RecipeBatch, String> {
    Ok(recorded_batch(app_handle, paths, |app_handle, paths| {
        let lens_db = crate::lens_correction::global_lens_db();
        edit_sidecars_recorded(
            paths,
            xmp_sync(app_handle),
            |_, source_path, sidecar_path| {
                let mut existing_metadata =
                    crate::exif_processing::load_sidecar_with_exif(sidecar_path, source_path);

                crate::image_processing::apply_render_default(&mut existing_metadata.adjustments);

                if let Some(obj) = existing_metadata.adjustments.as_object_mut() {
                    obj.insert("lensCorrectionMode".to_string(), serde_json::json!("auto"));
                    obj.insert("lensDistortionEnabled".to_string(), serde_json::json!(true));
                    obj.insert("lensTcaEnabled".to_string(), serde_json::json!(true));
                    obj.insert("lensVignetteEnabled".to_string(), serde_json::json!(true));
                }

                // Detected again on the user's request: from the current database.
                resolve_lens_params_in_adjustments(
                    &mut existing_metadata.adjustments,
                    LensLookup::Fresh,
                    &existing_metadata.exif,
                    lens_db.as_deref(),
                    source_path,
                );
                Ok(existing_metadata)
            },
        )
    })
    .await)
}

/// Top-level recipe values of one photo (`set_recipe_values`): a value of
/// `null` removes the key.
#[derive(serde::Deserialize, serde::Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct RecipeValues {
    pub path: String,
    pub values: serde_json::Map<String, Value>,
}

/// What `set_recipe_values` wrote: each written photo's values before
/// (`null` for keys it did not have), and the photos not written with the
/// first errors.
#[derive(serde::Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct RecipeWrite {
    pub before: Vec<RecipeValues>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<SaveFailure>,
}

/// Sets per photo its own top-level recipe values (series match, and the
/// undo and redo of it). Writing the same values again is harmless, so a
/// partly failed undo can be retried.
#[tauri::command]
pub async fn set_recipe_values(
    changes: Vec<RecipeValues>,
    app_handle: AppHandle,
) -> Result<RecipeWrite, String> {
    let paths: Vec<String> = changes.iter().map(|c| c.path.clone()).collect();
    let by_path: HashMap<String, serde_json::Map<String, Value>> =
        changes.into_iter().map(|c| (c.path, c.values)).collect();
    let before = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let written = before.clone();
    let result = save_then_regenerate(app_handle, paths, move |app_handle, paths| {
        edit_sidecars(paths, xmp_sync(app_handle), |path, _, sidecar_path| {
            let values = by_path.get(path).ok_or("no values")?;
            let mut metadata = crate::exif_processing::load_sidecar(sidecar_path);
            crate::image_processing::apply_render_default(&mut metadata.adjustments);
            let old = set_values(&mut metadata.adjustments, values)?;
            written.lock().unwrap().push(RecipeValues {
                path: path.to_string(),
                values: old,
            });
            Ok(metadata)
        })
    })
    .await;
    let mut before = std::mem::take(&mut *before.lock().unwrap());
    let failure = result.err();
    // A photo whose edit succeeded but whose save failed was not written.
    if let Some(failure) = &failure {
        before.retain(|b| !failure.failed.contains(&b.path));
    }
    Ok(RecipeWrite { before, failure })
}

/// Sets `values` in `recipe` (`null` removes a key) and returns the values
/// they replaced (`null` for keys it did not have).
fn set_values(
    recipe: &mut Value,
    values: &serde_json::Map<String, Value>,
) -> Result<serde_json::Map<String, Value>, String> {
    let recipe = recipe.as_object_mut().ok_or("recipe is not an object")?;
    let mut old = serde_json::Map::new();
    for (key, value) in values {
        old.insert(key.clone(), recipe.get(key).cloned().unwrap_or(Value::Null));
        if value.is_null() {
            recipe.remove(key);
        } else {
            recipe.insert(key.clone(), value.clone());
        }
    }
    Ok(old)
}

/// One photograph of a batch command whose write of its recipe, sidecar or
/// XMP is finished (`batch_writes`). The webview answers the quit request
/// (`quit_saving`) for each, so quitting waits for a long batch photo by
/// photo instead of dropping it after the save timeout, and takes a failed
/// one into the unsaved-changes prompt.
pub const BATCH_WRITE_SETTLED: &str = "batch-write-settled";

/// A photograph of a batch command that finished: whether its write
/// succeeded, and why it did not.
#[derive(Debug, Clone, serde::Serialize)]
pub struct BatchWriteSettled {
    /// The command's `batch` tag (the webview's write queue entry).
    pub batch: String,
    pub path: String,
    pub ok: Option<String>,
    /// Photos this command writes besides `path` (a failed one may not know
    /// which), which also count as settled.
    pub also: Vec<String>,
}

/// Tells the webview that the photograph `path` of batch command `batch` is
/// written (`ok` false: it is not, the user hears it when quitting).
pub(crate) fn emit_batch_write_settled(
    app_handle: &AppHandle,
    batch: &str,
    path: &str,
    ok: Option<&str>,
    also: &[String],
) {
    let _ = app_handle.emit(
        BATCH_WRITE_SETTLED,
        BatchWriteSettled {
            batch: batch.to_string(),
            path: path.to_string(),
            ok: ok.map(str::to_string),
            also: also.to_vec(),
        },
    );
}

/// The `SaveFailure` of a face retouch that did not write `path`
/// (`ai_faces::apply_face_retouch_to_paths` reports each photo with it).
pub(crate) fn face_retouch_failure(path: &str, reason: &str) -> SaveFailure {
    SaveFailure {
        message: format!("Face retouch of {path} failed: {reason}"),
        failed: vec![path.to_string()],
        culling: Vec::new(),
    }
}

/// Decodes each photo for its auto adjustments, so it waits until every
/// recipe is written (or failed) and makes the thumbnails from the decoded
/// photos meanwhile. Each photo's finished write is announced
/// (`BATCH_WRITE_SETTLED`, tagged `batch`).
#[tauri::command]
pub async fn apply_auto_adjustments_to_paths(
    paths: Vec<String>,
    batch: Option<String>,
    app_handle: AppHandle,
) -> Result<(), SaveFailure> {
    let state = app_handle.state::<AppState>();
    add_to_thumbnail_queue(&state, paths.len(), &app_handle);
    let total = paths.len();
    let all = paths.clone();
    let failures = tauri::async_runtime::spawn_blocking(move || {
        let settle = |path: &str, why: Option<&str>| {
            if let Some(batch) = batch.as_deref() {
                emit_batch_write_settled(&app_handle, batch, path, why, &[]);
            }
        };
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

                    let recipe = crate::exif_processing::load_sidecar(&sidecar_path).adjustments;
                    let tone_mapper = recipe
                        .get("toneMapper")
                        .and_then(|value| value.as_str())
                        .map(str::to_string);
                    let base_curve = recipe
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

                    // Read again for the write: the photo may have been
                    // edited or tagged while it was decoded and measured.
                    let _turn = sidecar_field_edit();
                    let mut existing_metadata = crate::exif_processing::load_sidecar(&sidecar_path);
                    crate::image_processing::apply_render_default(
                        &mut existing_metadata.adjustments,
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
                // Its recipe is on disk, or failed: after the thumbnail.
                settle(path, applied.as_ref().err().map(String::as_str));

                applied.err().map(|e| (path.clone(), e))
            })
            .collect::<Vec<_>>()
    })
    .await
    .map_err(|e| SaveFailure {
        message: format!("Auto adjustments failed: {e}"),
        failed: all,
        culling: Vec::new(),
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
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub culling: Vec<CullingChange>,
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
            culling: Vec::new(),
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

/// Rating, colour label, keyword and recipe edits, batch Auto and background
/// AI tagging read a photo's sidecar, change their part and write it back.
/// They run on blocking workers and rayon tasks, so they take turns through
/// this guard: none overwrites another's change. It covers one photo's read,
/// change and write and is never held while waiting for other work (a
/// batch's `par_iter`, a decode), so a rayon task may wait for it.
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

/// An acknowledged, field-only catalog change. Receipts are used by Undo;
/// replay reads the latest sidecar again so edits and keywords stay intact.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
#[serde(tag = "kind", content = "value", rename_all = "camelCase")]
pub enum CullingValue {
    Rating(u8),
    Color(Option<String>),
    Flag(i8),
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CullingChange {
    pub path: String,
    pub asset_id: Option<String>,
    pub directory: Option<String>,
    pub before: CullingValue,
    pub after: CullingValue,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CullingWrite {
    pub path: String,
    pub value: CullingValue,
    pub asset_id: Option<String>,
    pub directory: Option<String>,
}

#[tauri::command]
pub async fn set_color_label_for_paths(
    paths: Vec<String>,
    color: Option<String>,
    directory: Option<String>,
    app_handle: AppHandle,
) -> Result<Vec<CullingChange>, SaveFailure> {
    let directory = directory.or_else(|| {
        crate::session_bridge::active_library_directory()
            .map(|dir| dir.to_string_lossy().into_owned())
    });
    culling_worker(
        paths
            .into_iter()
            .map(|path| CullingWrite {
                path,
                value: CullingValue::Color(color.clone()),
                asset_id: None,
                directory: None,
            })
            .collect(),
        directory,
        app_handle,
    )
    .await
}

#[tauri::command]
pub async fn set_rating_for_paths(
    paths: Vec<String>,
    rating: u8,
    directory: Option<String>,
    app_handle: AppHandle,
) -> Result<Vec<CullingChange>, SaveFailure> {
    let directory = directory.or_else(|| {
        crate::session_bridge::active_library_directory()
            .map(|dir| dir.to_string_lossy().into_owned())
    });
    culling_worker(
        paths
            .into_iter()
            .map(|path| CullingWrite {
                path,
                value: CullingValue::Rating(rating),
                asset_id: None,
                directory: None,
            })
            .collect(),
        directory,
        app_handle,
    )
    .await
}

#[tauri::command]
pub async fn restore_culling(
    values: Vec<CullingWrite>,
    directory: Option<String>,
    app_handle: AppHandle,
) -> Result<Vec<CullingChange>, SaveFailure> {
    let failures: Vec<_> = values.iter().map(|write| write.path.clone()).collect();
    super::blocking(move || {
        let (values, failures) = resolve_culling_targets(values, directory.as_deref());
        write_culling(values, directory, &app_handle, failures)
    })
    .await
    .unwrap_or_else(|error| unsaved(&failures, &error).map(|()| Vec::new()))
}

/// Keep successful targets and report validation failures per photograph.
fn partition_culling_targets(
    values: Vec<CullingWrite>,
    mut prepare: impl FnMut(&mut CullingWrite) -> Result<(), String>,
) -> (Vec<CullingWrite>, Vec<(String, String)>) {
    let mut ready = Vec::new();
    let mut failures = Vec::new();
    for mut write in values {
        let path = write.path.clone();
        match prepare(&mut write) {
            Ok(()) => ready.push(write),
            Err(error) => failures.push((path, error)),
        }
    }
    (ready, failures)
}

/// Undo follows a managed photo's catalog identity through rename, trash and
/// relink. An unmanaged file that disappeared is never recreated at an old path.
fn resolve_culling_targets(
    values: Vec<CullingWrite>,
    directory: Option<&str>,
) -> (Vec<CullingWrite>, Vec<(String, String)>) {
    let mut libraries = HashMap::new();
    partition_culling_targets(values, |write| {
        if let Some(id) = &write.asset_id {
            let owner = write
                .directory
                .as_deref()
                .or(directory)
                .ok_or_else(|| "Undo needs its original library".to_string())?;
            if !libraries.contains_key(owner) {
                libraries.insert(
                    owner.to_string(),
                    rawtonic_catalog::Library::open(Path::new(owner)).map_err(|e| e.to_string())?,
                );
            }
            let lib = &libraries[owner];
            let asset = lib.asset(id).map_err(|e| e.to_string())?;
            let copy = write.path.rsplit_once("?vc=").map(|(_, copy)| copy);
            if let Some(copy) = copy {
                lib.version_for(id, Some(copy))
                    .map_err(|e| e.to_string())?
                    .ok_or_else(|| "This virtual copy no longer exists".to_string())?;
            }
            write.path = format!(
                "{}{}",
                asset.path.to_string_lossy(),
                copy.map(|id| format!("?vc={id}")).unwrap_or_default()
            );
            write.directory = Some(owner.to_string());
        } else if !parse_virtual_path(&write.path).0.is_file() {
            return Err("The photograph is no longer at its original location".into());
        }
        Ok(())
    })
}

/// Read target ids with one connection per owning library. Connections stay
/// on this blocking worker; only strings pass to the parallel sidecar jobs.
fn prepare_culling_targets(
    values: Vec<CullingWrite>,
    selected: Option<&str>,
) -> (Vec<CullingWrite>, Vec<(String, String)>) {
    let mut libraries = HashMap::new();
    partition_culling_targets(values, |write| {
        if write.asset_id.is_some() {
            // Already validated from an undo receipt.
            return Ok(());
        }
        let source = parse_virtual_path(&write.path).0;
        let owner = crate::session_bridge::culling_library_for(&source, selected.map(Path::new));
        write.directory = owner.as_ref().map(|dir| dir.to_string_lossy().into_owned());
        if let Some(owner) = owner {
            if !libraries.contains_key(&owner) {
                libraries.insert(
                    owner.clone(),
                    rawtonic_catalog::Library::open(&owner).map_err(|e| e.to_string())?,
                );
            }
            write.asset_id = libraries[&owner]
                .asset_at(&source)
                .map_err(|e| e.to_string())?
                .map(|asset| asset.id);
        }
        if write.asset_id.is_none() && !source.is_file() {
            return Err(
                "The photograph is no longer registered or available at this location".into(),
            );
        }
        Ok(())
    })
}

fn write_prepared_culling(write: CullingWrite, xmp: Option<bool>) -> Result<CullingChange, String> {
    let owner = write.directory.clone();
    crate::session_bridge::with_culling_owner(owner.as_deref(), || {
        write_culling_sidecar(write, xmp)
    })
}

async fn culling_worker(
    values: Vec<CullingWrite>,
    directory: Option<String>,
    app_handle: AppHandle,
) -> Result<Vec<CullingChange>, SaveFailure> {
    let paths: Vec<_> = values.iter().map(|write| write.path.clone()).collect();
    super::blocking(move || write_culling(values, directory, &app_handle, Vec::new()))
        .await
        .unwrap_or_else(|error| unsaved(&paths, &error).map(|()| Vec::new()))
}

fn write_culling(
    values: Vec<CullingWrite>,
    directory: Option<String>,
    app_handle: &AppHandle,
    failures: Vec<(String, String)>,
) -> Result<Vec<CullingChange>, SaveFailure> {
    write_culling_batch(values, directory, xmp_sync(app_handle), failures)
}

fn write_culling_batch(
    values: Vec<CullingWrite>,
    directory: Option<String>,
    xmp: Option<bool>,
    mut failures: Vec<(String, String)>,
) -> Result<Vec<CullingChange>, SaveFailure> {
    let total = values.len() + failures.len();
    let (values, preparation_failures) = prepare_culling_targets(values, directory.as_deref());
    failures.extend(preparation_failures);
    let mut changed = Vec::new();
    let mut flags: HashMap<String, Vec<(String, i8)>> = HashMap::new();
    for write in &values {
        if let CullingValue::Flag(flag) = write.value {
            if let Some(owner) = write.directory.as_ref().or(directory.as_ref()) {
                flags
                    .entry(owner.clone())
                    .or_default()
                    .push((write.path.clone(), flag));
            } else {
                failures.push((write.path.clone(), "Flag Undo needs its library".into()));
            }
        }
    }
    for (owner, flags) in flags {
        match crate::session_bridge::write_flag_values(&owner, flags.clone()) {
            Ok(receipts) => changed.extend(receipts),
            Err(error) => failures.extend(flags.into_iter().map(|(path, _)| (path, error.clone()))),
        }
    }
    let mut seen = std::collections::HashSet::new();
    let sidecars: Vec<_> = values
        .into_iter()
        .filter(|write| {
            !matches!(write.value, CullingValue::Flag(_)) && seen.insert(write.path.clone())
        })
        .collect();
    // Preserve the old parallel batch path. Each task installs its own resolved
    // owner scope; the existing field-edit mutex still protects read/modify/write.
    for (path, result) in sidecars
        .into_par_iter()
        .map(|write| {
            let path = write.path.clone();
            (path, write_prepared_culling(write, xmp))
        })
        .collect::<Vec<_>>()
    {
        match result {
            Ok(receipt) => changed.push(receipt),
            Err(error) => failures.push((path, error)),
        }
    }
    SaveFailure::check(total, failures).map_err(|mut error| {
        error.culling = changed.clone();
        error
    })?;
    Ok(changed)
}

/// The normal atomic editor/catalog write, changing only the culling field.
fn write_culling_sidecar(write: CullingWrite, xmp: Option<bool>) -> Result<CullingChange, String> {
    let _turn = sidecar_field_edit();
    let (source, sidecar) = parse_virtual_path(&write.path);
    let mut metadata = crate::exif_processing::load_sidecar(&sidecar);
    let before = match &write.value {
        CullingValue::Rating(rating) => {
            if *rating > 5 {
                return Err("Rating must be 0–5".into());
            }
            CullingValue::Rating(std::mem::replace(&mut metadata.rating, *rating))
        }
        CullingValue::Color(color) => {
            let mut tags = metadata.tags.take().unwrap_or_default();
            let before = tags
                .iter()
                .find_map(|tag| tag.strip_prefix(COLOR_TAG_PREFIX).map(str::to_string));
            tags.retain(|tag| !tag.starts_with(COLOR_TAG_PREFIX));
            if let Some(color) = color.as_ref().filter(|color| !color.is_empty()) {
                tags.push(format!("{COLOR_TAG_PREFIX}{color}"));
            }
            metadata.tags = (!tags.is_empty()).then_some(tags);
            CullingValue::Color(before)
        }
        CullingValue::Flag(_) => return Err("Flags must be written as a catalog batch".into()),
    };
    save_sidecar(&write.path, &metadata)?;
    if let Some(create) = xmp {
        sync_metadata_to_xmp(&source, &metadata, create);
    }
    let owner = crate::session_bridge::library_dir_for(&source);
    let asset_id = write.asset_id.or_else(|| {
        owner
            .as_ref()
            .and_then(|dir| rawtonic_catalog::Library::open(dir).ok())
            .and_then(|lib| lib.asset_at(&source).ok().flatten())
            .map(|asset| asset.id)
    });
    Ok(CullingChange {
        path: write.path,
        asset_id,
        directory: owner.map(|dir| dir.to_string_lossy().into_owned()),
        before,
        after: write.value,
    })
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

    // For the editor only (not written): its history carries it (see
    // LensLookup::Keep).
    stamp_loaded_lens_profile(&mut metadata.adjustments);
    // Saved while the photo could not be read: resolved now if it can be,
    // so the editor shows the correction (saved with the next edit).
    let _ = rawtonic_export::exif_processing::resolve_unresolved_lens(
        &mut metadata.adjustments,
        &source_path,
    );
    Ok(metadata)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rawtonic_engine::lens_recipe::{LENS_PROFILE_FOR, LENS_UNRESOLVED, lens_profile_stamp};

    #[test]
    fn set_values_returns_what_it_replaced_and_null_removes() {
        let mut recipe = serde_json::json!({"exposure": 0.5, "contrast": 10});
        let values = serde_json::json!({"exposure": 1.25, "wbTemperature": 4200, "contrast": null});
        let old = set_values(&mut recipe, values.as_object().unwrap()).unwrap();
        assert_eq!(
            recipe,
            serde_json::json!({"exposure": 1.25, "wbTemperature": 4200})
        );
        assert_eq!(
            Value::Object(old.clone()),
            serde_json::json!({"exposure": 0.5, "wbTemperature": null, "contrast": 10})
        );
        // Writing the old values back restores the recipe exactly.
        set_values(&mut recipe, &old).unwrap();
        assert_eq!(recipe, serde_json::json!({"exposure": 0.5, "contrast": 10}));
    }

    /// A batch edit returns each written photograph's whole recipe before
    /// and after; the photographs it failed on are not part of it, and
    /// writing the recipe before back restores the sidecar exactly.
    #[test]
    fn a_recorded_batch_edit_returns_whole_recipes_and_writes_them_back() {
        let temp = tempfile::tempdir().unwrap();
        let paths: Vec<String> = ["a.jpg", "b.jpg"]
            .iter()
            .map(|name| {
                let path = temp.path().join(name);
                fs::write(&path, b"photo").unwrap();
                path.to_string_lossy().into_owned()
            })
            .collect();
        let edited = |exposure: f64| {
            move |path: &str, _: &Path, sidecar_path: &Path| {
                if path.ends_with("b.jpg") && exposure > 1.0 {
                    return Err("refused".to_string());
                }
                let mut metadata = crate::exif_processing::load_sidecar(sidecar_path);
                metadata.adjustments = serde_json::json!({"exposure": exposure, "contrast": 5});
                Ok(metadata)
            }
        };
        let (first, failures) = edit_sidecars_recorded(&paths, None, edited(0.5));
        assert!(failures.is_empty(), "{failures:?}");
        assert_eq!(first.len(), 2);
        let (changes, failures) = edit_sidecars_recorded(&paths, None, edited(1.5));
        assert_eq!(failures.len(), 1);
        assert_eq!(changes.len(), 1);
        let change = &changes[0];
        assert_eq!(change.path, paths[0]);
        assert_eq!(
            change.before,
            serde_json::json!({"exposure": 0.5, "contrast": 5})
        );
        assert_eq!(
            change.after,
            serde_json::json!({"exposure": 1.5, "contrast": 5})
        );
        let (restored, failures) =
            edit_sidecars_recorded(&paths[..1], None, |_, _, sidecar_path| {
                let mut metadata = crate::exif_processing::load_sidecar(sidecar_path);
                metadata.adjustments = change.before.clone();
                Ok(metadata)
            });
        assert!(failures.is_empty());
        assert_eq!(restored[0].after, change.before);
        let (_, sidecar_path) = parse_virtual_path(&paths[0]);
        assert_eq!(
            crate::exif_processing::load_sidecar(&sidecar_path).adjustments,
            change.before
        );
    }

    fn successful_targets(
        (values, failures): (Vec<CullingWrite>, Vec<(String, String)>),
    ) -> Vec<CullingWrite> {
        assert!(failures.is_empty(), "{failures:?}");
        values
    }

    /// A paste, preset or style onto a closed photograph never changes its
    /// frame (ADR 0020, "new photos only"): an edit keeps its own, a
    /// never-edited photograph starts its edit with the newest.
    #[test]
    fn a_paste_keeps_the_photographs_frame() {
        use rawtonic_engine::source_frame::{KEY, VERSION};
        let source = Path::new("/nowhere/photo.CR3");
        let paste = |base: Value| {
            pasted_into(
                base,
                &None,
                "/nowhere/photo.CR3",
                source,
                &serde_json::json!({"exposure": 1.0, KEY: 1}),
                None,
                None,
                None,
                None,
            )
            .unwrap()
        };
        let legacy = paste(serde_json::json!({"exposure": 0.2, "renderVersion": 4}));
        assert_eq!(legacy["exposure"], 1.0);
        assert_eq!(legacy.get(KEY), None);
        assert_eq!(paste(serde_json::json!({"exposure": 0.2, KEY: 0}))[KEY], 0);
        // A Reset's recipe keeps its frame through the paste after it.
        assert_eq!(paste(serde_json::json!({KEY: 0}))[KEY], 0);
        assert_eq!(paste(serde_json::json!({}))[KEY], VERSION);
    }

    /// A style replaces the photograph's last applied style with that
    /// photograph's own values before it; applying it again removes it.
    #[test]
    fn a_style_replaces_the_last_one_per_photograph_and_toggles_off() {
        use rawtonic_engine::applied_style::APPLIED_STYLE;
        let source = Path::new("/nowhere/photo.jpg");
        let apply = |base: Value, values: Value, id: &str, remove: bool| {
            let op = StyleOp {
                id: id.into(),
                remove,
            };
            pasted_into(
                base,
                &None,
                "/nowhere/photo.jpg",
                source,
                &values,
                None,
                Some(&op),
                None,
                None,
            )
            .unwrap()
        };
        let own = serde_json::json!({"renderVersion": 6, "exposure": 0.2, "contrast": 5, "saturation": 0});
        let a = apply(
            own.clone(),
            serde_json::json!({"exposure": 1.0, "saturation": 20}),
            "a",
            false,
        );
        assert_eq!(a[APPLIED_STYLE]["id"], "a");
        // The user changes saturation after A; B then replaces A.
        let mut edited = a.clone();
        edited["saturation"] = serde_json::json!(35);
        edited["contrast"] = serde_json::json!(9);
        let b = apply(edited, serde_json::json!({"contrast": 30}), "b", false);
        assert_eq!(
            b["exposure"], 0.2,
            "A's exposure goes back to the photograph's own"
        );
        assert_eq!(b["saturation"], 35, "the user's later change stays");
        assert_eq!(b["contrast"], 30);
        assert_eq!(
            b[APPLIED_STYLE]["changes"],
            serde_json::json!([{"path": ["contrast"], "from": 9, "to": 30}])
        );
        // Removing a style the photograph does not have changes nothing.
        assert_eq!(apply(b.clone(), serde_json::json!({}), "a", true), b);
        let removed = apply(b, serde_json::json!({}), "b", true);
        assert_eq!(removed.get(APPLIED_STYLE), None);
        assert_eq!(removed["contrast"], 9);
        assert_eq!(removed["exposure"], 0.2);
        assert_eq!(removed["saturation"], 35);
        // A style that sets one HSL slider leaves the others; taken back,
        // the group is as it was.
        let own = serde_json::json!({"renderVersion": 6, "hsl": {"reds": {"hue": 0, "saturation": 4}, "blues": {"hue": 22}}});
        let red = apply(
            own.clone(),
            serde_json::json!({"hsl": {"reds": {"hue": 5}}}),
            "red",
            false,
        );
        assert_eq!(
            red["hsl"],
            serde_json::json!({"reds": {"hue": 5, "saturation": 4}, "blues": {"hue": 22}})
        );
        let mut back = apply(red, serde_json::json!({}), "red", true);
        // The lens a style sets is the photograph's own: taking the style
        // back leaves it.
        let manual =
            serde_json::json!({"renderVersion": 6, "lensCorrectionMode": "manual", "exposure": 0});
        let auto = apply(
            manual,
            serde_json::json!({"lensCorrectionMode": "auto", "exposure": 1}),
            "auto",
            false,
        );
        let off = apply(auto, serde_json::json!({}), "auto", true);
        assert_eq!(off["lensCorrectionMode"], "auto");
        assert_eq!(off["exposure"], 0);
        // (Every save stamps the lens choice.)
        back.as_object_mut().unwrap().remove(LENS_PROFILE_FOR);
        assert_eq!(back, own);
    }

    #[test]
    fn culling_receipts_restore_exact_values_without_reverting_later_edits() {
        use rawtonic_catalog::{Library, Mode};
        let temp = tempfile::tempdir().unwrap();
        let lib = Library::create(temp.path(), "Undo", Mode::Session).unwrap();
        let mut receipts = Vec::new();
        let mut ids = Vec::new();
        for (n, (rating, color)) in [(0, None), (2, Some("red")), (5, Some("blue"))]
            .into_iter()
            .enumerate()
        {
            let source = temp.path().join(format!("Capture/{n}.nef"));
            fs::write(&source, b"synthetic original").unwrap();
            let asset = lib.add_asset(&source).unwrap();
            let tags = color
                .map(|color| vec![format!("color:{color}")])
                .unwrap_or_default();
            lib.save_editor_state(&asset.id, &serde_json::json!({"version": 1, "rating": rating, "tags": tags, "adjustments": {"exposure": 1}})).unwrap();
            let path = source.to_string_lossy().into_owned();
            for value in [
                CullingValue::Rating(4),
                CullingValue::Color(Some("green".into())),
            ] {
                receipts.push(
                    write_culling_sidecar(
                        CullingWrite {
                            path: path.clone(),
                            value,
                            asset_id: None,
                            directory: None,
                        },
                        None,
                    )
                    .unwrap(),
                );
            }
            // A later unrelated edit must survive both restores.
            let mut state = lib.editor_state(&asset.id).unwrap().unwrap();
            state["adjustments"]["exposure"] = serde_json::json!(2);
            state["tags"]
                .as_array_mut()
                .unwrap()
                .push(serde_json::json!("user:later"));
            lib.save_editor_state(&asset.id, &state).unwrap();
            lib.set_flags(std::slice::from_ref(&asset.id), -1).unwrap();
            lib.sync_xmp(&asset.id).unwrap();
            ids.push((asset.id, rating, color));
            // Offline Undo writes SQLite and leaves XMP pending, like a normal edit.
            fs::rename(&source, temp.path().join(format!("offline-{n}.nef"))).unwrap();
        }
        assert_eq!(lib.summary().unwrap().xmp_pending, 0);
        for receipt in receipts.iter().rev() {
            let values = successful_targets(resolve_culling_targets(
                vec![CullingWrite {
                    path: receipt.path.clone(),
                    value: receipt.before.clone(),
                    asset_id: receipt.asset_id.clone(),
                    directory: receipt.directory.clone(),
                }],
                temp.path().to_str(),
            ));
            write_culling_sidecar(values.into_iter().next().unwrap(), None).unwrap();
        }
        assert_eq!(lib.summary().unwrap().xmp_pending, 3);
        for (id, rating, color) in ids {
            let meta = lib.metadata(&id).unwrap();
            assert_eq!(meta.rating, rating);
            assert_eq!(meta.label.to_lowercase(), color.unwrap_or_default());
            assert_eq!(meta.flag, -1);
            assert!(meta.keywords.contains(&"later".into()));
            assert_eq!(
                lib.editor_state(&id).unwrap().unwrap()["adjustments"]["exposure"],
                2
            );
        }
    }

    #[test]
    fn culling_undo_follows_asset_identity_after_a_move_and_refuses_missing_unmanaged_files() {
        use rawtonic_catalog::{Library, Mode};
        let temp = tempfile::tempdir().unwrap();
        // The library and the resolved paths are canonical: on macOS the
        // temporary folder /var/… is /private/var/….
        let root = temp.path().canonicalize().unwrap();
        let lib = Library::create(&root, "Undo move", Mode::Session).unwrap();
        let source = root.join("Capture/source.nef");
        fs::write(&source, b"synthetic original").unwrap();
        let asset = lib.add_asset(&source).unwrap();
        let receipt = write_culling_sidecar(
            CullingWrite {
                path: source.to_string_lossy().into_owned(),
                value: CullingValue::Rating(4),
                asset_id: None,
                directory: None,
            },
            None,
        )
        .unwrap();
        let moved = root.join("Selects/moved.nef");
        fs::rename(&source, &moved).unwrap();
        lib.relocate(&source, &moved).unwrap();
        let values = successful_targets(resolve_culling_targets(
            vec![CullingWrite {
                path: receipt.path.clone(),
                value: receipt.before.clone(),
                asset_id: receipt.asset_id,
                directory: receipt.directory,
            }],
            root.to_str(),
        ));
        assert_eq!(values[0].path, moved.to_string_lossy());
        write_culling_sidecar(values.into_iter().next().unwrap(), None).unwrap();
        assert_eq!(lib.metadata(&asset.id).unwrap().rating, 0);
        assert_eq!(
            resolve_culling_targets(
                vec![CullingWrite {
                    path: receipt.path,
                    value: receipt.before,
                    asset_id: None,
                    directory: None,
                }],
                None
            )
            .1
            .len(),
            1
        );
    }

    #[test]
    fn culling_receipts_keep_virtual_copy_ratings_and_labels_separate() {
        use rawtonic_catalog::{Library, Mode};
        let temp = tempfile::tempdir().unwrap();
        let lib = Library::create(temp.path(), "Copy undo", Mode::Session).unwrap();
        let source = temp.path().join("Capture/source.nef");
        fs::write(&source, b"synthetic original").unwrap();
        let asset = lib.add_asset(&source).unwrap();
        lib.save_editor_state(
            &asset.id,
            &serde_json::json!({"version": 1, "rating": 4, "tags": ["color:red"], "adjustments": {}}),
        )
        .unwrap();
        crate::session_bridge::write_editor_sidecar(
            source.with_file_name("source.nef.a1b2c3.rrdata"),
            serde_json::to_vec(
                &serde_json::json!({"version": 1, "rating": 2, "tags": [], "adjustments": {}}),
            )
            .unwrap(),
        )
        .unwrap();
        let path = format!("{}?vc=a1b2c3", source.to_string_lossy());
        let mut receipts = Vec::new();
        for value in [
            CullingValue::Rating(5),
            CullingValue::Color(Some("blue".into())),
        ] {
            receipts.push(
                write_culling_sidecar(
                    CullingWrite {
                        path: path.clone(),
                        value,
                        asset_id: None,
                        directory: None,
                    },
                    None,
                )
                .unwrap(),
            );
        }
        for receipt in receipts.into_iter().rev() {
            let values = successful_targets(resolve_culling_targets(
                vec![CullingWrite {
                    path: receipt.path,
                    value: receipt.before,
                    asset_id: receipt.asset_id,
                    directory: receipt.directory,
                }],
                temp.path().to_str(),
            ));
            write_culling_sidecar(values.into_iter().next().unwrap(), None).unwrap();
        }
        assert_eq!(
            lib.copy_state(&asset.id, "a1b2c3").unwrap().unwrap()["rating"],
            2
        );
        assert!(lib.copy_state(&asset.id, "a1b2c3").unwrap().unwrap()["tags"].is_null());
        assert_eq!(lib.metadata(&asset.id).unwrap().rating, 4);
        assert_eq!(lib.metadata(&asset.id).unwrap().label, "Red");
    }

    #[test]
    fn queued_culling_write_and_undo_keep_their_original_catalog() {
        use crate::session_bridge::with_culling_owner;
        use rawtonic_catalog::{Library, Mode};
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("external.nef");
        fs::write(&source, b"synthetic original").unwrap();
        let first = Library::create(&temp.path().join("first"), "First", Mode::Catalog).unwrap();
        let second = Library::create(&temp.path().join("second"), "Second", Mode::Catalog).unwrap();
        let a = first.add_asset(&source).unwrap();
        let b = second.add_asset(&source).unwrap();
        for (lib, id, rating) in [(&first, &a.id, 1), (&second, &b.id, 3)] {
            lib.save_editor_state(
                id,
                &serde_json::json!({"version": 1, "rating": rating, "tags": [], "adjustments": {}}),
            )
            .unwrap();
        }
        let directory = first.directory.to_str();
        let receipt = with_culling_owner(directory, || {
            // A nested job choosing another library restores the caller's target.
            with_culling_owner(second.directory.to_str(), || {
                assert_eq!(
                    crate::session_bridge::library_dir_for(&source),
                    Some(second.directory.clone())
                )
            });
            write_culling_sidecar(
                CullingWrite {
                    path: source.to_string_lossy().into_owned(),
                    value: CullingValue::Rating(4),
                    asset_id: None,
                    directory: None,
                },
                None,
            )
            .unwrap()
        });
        assert_eq!(receipt.before, CullingValue::Rating(1));
        assert_eq!(first.metadata(&a.id).unwrap().rating, 4);
        assert_eq!(second.metadata(&b.id).unwrap().rating, 3);
        with_culling_owner(directory, || {
            let values = successful_targets(resolve_culling_targets(
                vec![CullingWrite {
                    path: receipt.path,
                    value: receipt.before,
                    asset_id: receipt.asset_id,
                    directory: receipt.directory,
                }],
                directory,
            ));
            write_culling_sidecar(values.into_iter().next().unwrap(), None).unwrap();
        });
        assert_eq!(first.metadata(&a.id).unwrap().rating, 1);
        assert_eq!(second.metadata(&b.id).unwrap().rating, 3);
    }

    #[test]
    fn culling_batch_preflight_keeps_available_targets_when_one_is_missing() {
        let temp = tempfile::tempdir().unwrap();
        let available = temp.path().join("available.nef");
        fs::write(&available, b"synthetic original").unwrap();
        let missing = temp.path().join("missing.nef");
        let make = |path: &Path| CullingWrite {
            path: path.to_string_lossy().into_owned(),
            value: CullingValue::Rating(3),
            asset_id: None,
            directory: None,
        };
        for (ready, failures) in [
            prepare_culling_targets(vec![make(&missing), make(&available)], None),
            resolve_culling_targets(vec![make(&missing), make(&available)], None),
        ] {
            assert_eq!(ready.len(), 1);
            assert_eq!(failures.len(), 1);
            assert_eq!(failures[0].0, missing.to_string_lossy());
            let receipt = write_prepared_culling(ready.into_iter().next().unwrap(), None).unwrap();
            assert_eq!(receipt.path, available.to_string_lossy());
            let failure = SaveFailure::check(2, failures).unwrap_err();
            assert_eq!(failure.failed, vec![missing.to_string_lossy()]);
            assert!(!parse_virtual_path(&missing.to_string_lossy()).1.exists());
            assert_eq!(fs::read(&available).unwrap(), b"synthetic original");
        }
    }

    #[test]
    fn normal_culling_batch_saves_available_photos_and_returns_only_missing_failures() {
        let temp = tempfile::tempdir().unwrap();
        let available = temp.path().join("available.nef");
        let missing = temp.path().join("missing.nef");
        fs::write(&available, b"synthetic original").unwrap();
        for value in [
            CullingValue::Rating(4),
            CullingValue::Color(Some("red".into())),
        ] {
            let make = |path: &Path| CullingWrite {
                path: path.to_string_lossy().into_owned(),
                value: value.clone(),
                asset_id: None,
                directory: None,
            };
            let failure = write_culling_batch(
                vec![make(&missing), make(&available)],
                None,
                None,
                Vec::new(),
            )
            .unwrap_err();
            assert_eq!(failure.failed, vec![missing.to_string_lossy()]);
            assert_eq!(failure.culling.len(), 1);
            assert_eq!(failure.culling[0].path, available.to_string_lossy());
            assert_eq!(failure.culling[0].after, value);
            let metadata = crate::exif_processing::load_sidecar_file(
                &parse_virtual_path(&available.to_string_lossy()).1,
            );
            match value {
                CullingValue::Rating(rating) => assert_eq!(metadata.rating, rating),
                CullingValue::Color(_) => {
                    assert!(metadata.tags.unwrap().contains(&"color:red".into()))
                }
                CullingValue::Flag(_) => unreachable!(),
            }
            assert!(!parse_virtual_path(&missing.to_string_lossy()).1.exists());
            assert_eq!(fs::read(&available).unwrap(), b"synthetic original");
        }
    }

    #[test]
    fn culling_restore_reports_an_unavailable_owner_without_losing_other_targets() {
        let temp = tempfile::tempdir().unwrap();
        let available = temp.path().join("available.nef");
        fs::write(&available, b"synthetic original").unwrap();
        let blocked_owner = temp.path().join("not-a-library");
        fs::write(&blocked_owner, b"cannot open a library here").unwrap();
        let (ready, failures) = resolve_culling_targets(
            vec![
                CullingWrite {
                    path: "unavailable.nef".into(),
                    value: CullingValue::Rating(0),
                    asset_id: Some("unavailable-asset".into()),
                    directory: Some(blocked_owner.to_string_lossy().into_owned()),
                },
                CullingWrite {
                    path: available.to_string_lossy().into_owned(),
                    value: CullingValue::Rating(0),
                    asset_id: None,
                    directory: None,
                },
            ],
            None,
        );
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].path, available.to_string_lossy());
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].0, "unavailable.nef");
    }

    #[test]
    fn culling_resolves_real_owners_before_pinning_the_selected_library() {
        use rawtonic_catalog::{Library, Mode};
        let temp = tempfile::tempdir().unwrap();
        let session =
            Library::create(&temp.path().join("session"), "Session", Mode::Session).unwrap();
        let catalog =
            Library::create(&temp.path().join("catalog"), "Catalog", Mode::Catalog).unwrap();
        let outside = temp.path().join("outside.nef");
        fs::write(&outside, b"synthetic original").unwrap();
        let inside = session.directory.join("Capture/inside.nef");
        fs::write(&inside, b"synthetic original").unwrap();
        let make = |path: &Path| CullingWrite {
            path: path.to_string_lossy().into_owned(),
            value: CullingValue::Rating(3),
            asset_id: None,
            directory: None,
        };
        let writes = successful_targets(prepare_culling_targets(
            vec![make(&outside)],
            session.directory.to_str(),
        ));
        let receipt = write_prepared_culling(writes.into_iter().next().unwrap(), None).unwrap();
        assert!(receipt.asset_id.is_none());
        assert_eq!(session.summary().unwrap().assets, 0);
        assert_eq!(
            crate::exif_processing::load_sidecar_file(
                &parse_virtual_path(&outside.to_string_lossy()).1
            )
            .rating,
            3
        );
        let writes = successful_targets(prepare_culling_targets(
            vec![make(&inside)],
            catalog.directory.to_str(),
        ));
        write_prepared_culling(writes.into_iter().next().unwrap(), None).unwrap();
        assert!(session.asset_at(&inside).unwrap().is_some());
        assert!(catalog.asset_at(&inside).unwrap().is_none());
        let vanished = temp.path().join("vanished.nef");
        assert_eq!(
            prepare_culling_targets(vec![make(&vanished)], session.directory.to_str())
                .1
                .len(),
            1
        );
    }

    #[test]
    fn culling_receipt_keeps_implicit_owner_when_no_library_is_selected() {
        use rawtonic_catalog::{Library, Mode};
        let temp = tempfile::tempdir().unwrap();
        let lib = Library::create(temp.path(), "Implicit owner", Mode::Session).unwrap();
        let source = temp.path().join("Capture/source.nef");
        fs::write(&source, b"synthetic original").unwrap();
        let asset = lib.add_asset(&source).unwrap();
        let receipt = write_culling_sidecar(
            CullingWrite {
                path: source.to_string_lossy().into_owned(),
                value: CullingValue::Rating(4),
                asset_id: None,
                directory: None,
            },
            None,
        )
        .unwrap();
        assert_eq!(receipt.directory.as_deref(), temp.path().to_str());
        let values = successful_targets(resolve_culling_targets(
            vec![CullingWrite {
                path: receipt.path,
                value: receipt.before,
                asset_id: receipt.asset_id,
                directory: receipt.directory,
            }],
            None,
        ));
        write_prepared_culling(values.into_iter().next().unwrap(), None).unwrap();
        assert_eq!(lib.metadata(&asset.id).unwrap().rating, 0);
    }

    #[test]
    #[ignore = "manual metadata writer comparison; prints timings, not a timing gate"]
    fn measure_culling_batch_receipt_overhead() {
        use rawtonic_catalog::{Library, Mode};
        let temp = tempfile::tempdir().unwrap();
        let lib = Library::create(temp.path(), "Metadata timing", Mode::Session).unwrap();
        let paths: Vec<_> = (0..80)
            .map(|n| {
                let source = temp.path().join(format!("Capture/{n}.nef"));
                fs::write(&source, b"synthetic original").unwrap();
                let asset = lib.add_asset(&source).unwrap();
                lib.save_editor_state(
                    &asset.id,
                    &serde_json::json!({"version":1,"rating":0,"tags":[],"adjustments":{}}),
                )
                .unwrap();
                source.to_string_lossy().into_owned()
            })
            .collect();
        // Baseline is the previous rating writer: the same parallel loop,
        // field guard, latest sidecar load and normal atomic catalog/XMP save.
        let baseline = |rating| {
            paths.par_iter().for_each(|path| {
                let _turn = sidecar_field_edit();
                let mut metadata =
                    crate::exif_processing::load_sidecar(&parse_virtual_path(path).1);
                metadata.rating = rating;
                save_sidecar(path, &metadata).unwrap();
            });
        };
        let receipts = |rating| {
            let values = paths
                .iter()
                .map(|path| CullingWrite {
                    path: path.clone(),
                    value: CullingValue::Rating(rating),
                    asset_id: None,
                    directory: None,
                })
                .collect();
            let values = successful_targets(prepare_culling_targets(values, temp.path().to_str()));
            values.into_par_iter().for_each(|write| {
                write_prepared_culling(write, None).unwrap();
            });
        };
        baseline(0);
        receipts(1);
        for (name, rating) in [
            ("baseline", 2),
            ("receipts", 3),
            ("receipts", 4),
            ("baseline", 5),
        ] {
            let start = std::time::Instant::now();
            if name == "baseline" {
                baseline(rating);
            } else {
                receipts(rating);
            }
            println!(
                "CULLING_BENCH {name} photos=80 ms={:.3}",
                start.elapsed().as_secs_f64() * 1000.0
            );
            assert!(
                lib.all_metadata()
                    .unwrap()
                    .iter()
                    .all(|(_, metadata)| metadata.rating == rating)
            );
        }
    }

    fn sky_layer(id: &str, data: &str) -> Value {
        serde_json::json!({"id": id, "name": "Sky", "visible": true, "invert": false, "opacity": 100,
            "adjustments": {"exposure": -0.5},
            "subMasks": [{"id": format!("{id}-c"), "type": "ai-sky", "visible": true, "invert": false,
                "opacity": 100, "mode": "additive", "parameters": {"maskDataBase64": data}}]})
    }

    /// Writes `count` photographs of a session, each with its own edit, an
    /// own layer and its own sky detection. With `fixture` (a RAW file),
    /// they are hard links to it (never written: only sidecars are).
    fn session_with_photos(count: usize) -> (tempfile::TempDir, Vec<String>) {
        session_of(count, None)
    }

    fn session_of(count: usize, fixture: Option<&Path>) -> (tempfile::TempDir, Vec<String>) {
        // Beside the fixture, so the links need no copies.
        let temp = match fixture.and_then(Path::parent) {
            Some(folder) => tempfile::tempdir_in(folder).unwrap(),
            None => tempfile::tempdir().unwrap(),
        };
        let lib =
            rawtonic_catalog::Library::create(temp.path(), "Sync", rawtonic_catalog::Mode::Session)
                .unwrap();
        let day = lib.directory.join("Capture");
        let paths = (0..count)
            .map(|i| {
                let extension = fixture
                    .and_then(|f| f.extension())
                    .map_or("ARW".into(), |e| e.to_string_lossy().into_owned());
                let photo = day.join(format!("A{i:03}.{extension}"));
                match fixture {
                    Some(fixture) => fs::hard_link(fixture, &photo)
                        .or_else(|_| fs::copy(fixture, &photo).map(|_| ()))
                        .unwrap(),
                    None => fs::write(&photo, format!("sync fixture {i}")).unwrap(),
                }
                let state = serde_json::json!({"version": 1, "rating": 0, "tags": [], "adjustments": {
                    "exposure": 0.1, "masks": [sky_layer("own", &format!("data:own-{i}"))]}});
                crate::session_bridge::write_editor_sidecar(
                    photo.with_file_name(format!("A{i:03}.{extension}.rrdata")),
                    serde_json::to_vec(&state).unwrap(),
                )
                .unwrap();
                photo.to_string_lossy().into_owned()
            })
            .collect();
        (temp, paths)
    }

    #[test]
    fn synced_layers_join_each_photographs_own_with_their_own_detection() {
        let (_temp, paths) = session_with_photos(2);
        let pasted =
            serde_json::json!({"contrast": 20, "masks": [sky_layer("synced", "data:source")]});
        let paste = PasteFrom {
            source: paths[0].clone(),
            replace_layers: false,
            new_edit: None,
        };
        let failed = edit_sidecars(&paths[1..], None, |path, source, sidecar| {
            pasted_metadata(
                path,
                source,
                sidecar,
                &pasted,
                Some(&paste),
                None,
                None,
                None,
            )
        });
        assert!(failed.is_empty(), "{failed:?}");
        let (_, sidecar) = parse_virtual_path(&paths[1]);
        let saved = crate::exif_processing::load_sidecar(&sidecar).adjustments;
        assert_eq!(saved["contrast"], 20);
        assert_eq!(saved["exposure"], 0.1, "its own edit stays");
        let layers = saved["masks"].as_array().unwrap();
        assert_eq!(
            layers[0]["subMasks"][0]["parameters"]["maskDataBase64"],
            "data:own-1"
        );
        let synced = &layers[1]["subMasks"][0]["parameters"];
        assert_eq!(synced["maskDataBase64"], Value::Null);
        assert_eq!(synced["detectPending"], true);
        // Its virtual copy shares its pixels: the bitmap is kept there.
        let copy = format!("{}?vc=c0ffee", paths[0]);
        let failed = edit_sidecars(
            std::slice::from_ref(&copy),
            None,
            |path, source, sidecar| {
                pasted_metadata(
                    path,
                    source,
                    sidecar,
                    &pasted,
                    Some(&paste),
                    None,
                    None,
                    None,
                )
            },
        );
        assert!(failed.is_empty(), "{failed:?}");
        let (_, sidecar) = parse_virtual_path(&copy);
        let saved = crate::exif_processing::load_sidecar(&sidecar).adjustments;
        assert_eq!(
            saved["masks"][0]["subMasks"][0]["parameters"]["maskDataBase64"],
            "data:source"
        );
    }

    #[test]
    fn a_never_edited_photograph_takes_a_new_edits_versions_as_its_own() {
        let (_temp, paths) = session_with_photos(2);
        // The second photograph was never edited.
        let (_, sidecar) = parse_virtual_path(&paths[1]);
        crate::session_bridge::write_editor_sidecar(
            &sidecar,
            br#"{"version":1,"rating":0,"tags":[],"adjustments":null}"#,
        )
        .unwrap();
        let mut raw = serde_json::Map::new();
        raw.insert("toneVersion".into(), serde_json::json!(2));
        let paste = PasteFrom {
            source: paths[0].clone(),
            replace_layers: false,
            new_edit: Some(NewEditResponses {
                raw,
                other: serde_json::Map::new(),
            }),
        };
        let pasted = serde_json::json!({"shadows": 80});
        let failed = edit_sidecars(&paths, None, |path, source, sidecar| {
            pasted_metadata(
                path,
                source,
                sidecar,
                &pasted,
                Some(&paste),
                None,
                None,
                None,
            )
        });
        assert!(failed.is_empty(), "{failed:?}");
        let new = crate::exif_processing::load_sidecar(&sidecar).adjustments;
        assert_eq!(new["toneVersion"], 2, "a new edit's versions");
        assert_eq!(new["shadows"], 80);
        // An edited photograph keeps its own (here none: an older edit).
        let (_, edited) = parse_virtual_path(&paths[0]);
        let old = crate::exif_processing::load_sidecar(&edited).adjustments;
        assert!(old.get("toneVersion").is_none());
    }

    #[test]
    fn a_setting_the_source_does_not_have_is_taken_away() {
        let (_temp, paths) = session_with_photos(1);
        let (_, sidecar) = parse_virtual_path(&paths[0]);
        crate::session_bridge::write_editor_sidecar(
            &sidecar,
            br#"{"version":1,"rating":0,"tags":[],"adjustments":{"whiteBalance":"custom","wbTemperature":2850,"temperature":5}}"#,
        )
        .unwrap();
        let paste = PasteFrom {
            source: "/elsewhere/B.ARW".into(),
            replace_layers: false,
            new_edit: None,
        };
        // As Shot: no custom balance.
        let pasted =
            serde_json::json!({"whiteBalance": null, "wbTemperature": null, "temperature": 0});
        let failed = edit_sidecars(&paths, None, |path, source, sidecar| {
            pasted_metadata(
                path,
                source,
                sidecar,
                &pasted,
                Some(&paste),
                None,
                None,
                None,
            )
        });
        assert!(failed.is_empty(), "{failed:?}");
        let saved = crate::exif_processing::load_sidecar(&sidecar).adjustments;
        assert!(saved.get("whiteBalance").is_none());
        assert!(saved.get("wbTemperature").is_none());
        assert_eq!(saved["temperature"], 0);
    }

    #[test]
    fn a_paste_beyond_the_rendered_layer_limit_is_refused() {
        let (_temp, paths) = session_with_photos(1);
        let layers: Vec<Value> = (0..32)
            .map(|i| sky_layer(&format!("l{i}"), "data:x"))
            .collect();
        let paste = PasteFrom {
            source: "/elsewhere/B.ARW".into(),
            replace_layers: false,
            new_edit: None,
        };
        // The photograph has one layer; 32 more would be 33.
        let pasted = serde_json::json!({ "masks": layers });
        let failed = edit_sidecars(&paths, None, |path, source, sidecar| {
            pasted_metadata(
                path,
                source,
                sidecar,
                &pasted,
                Some(&paste),
                None,
                None,
                None,
            )
        });
        assert_eq!(failed.len(), 1);
        assert!(failed[0].1.contains("at most 32"), "{failed:?}");
        let (_, sidecar) = parse_virtual_path(&paths[0]);
        let saved = crate::exif_processing::load_sidecar(&sidecar).adjustments;
        assert_eq!(
            saved["masks"].as_array().unwrap().len(),
            1,
            "nothing written"
        );
        // 31 fit.
        let pasted = serde_json::json!({ "masks": layers[..31] });
        let failed = edit_sidecars(&paths, None, |path, source, sidecar| {
            pasted_metadata(
                path,
                source,
                sidecar,
                &pasted,
                Some(&paste),
                None,
                None,
                None,
            )
        });
        assert!(failed.is_empty(), "{failed:?}");
    }

    #[test]
    fn preset_without_layers_preserves_the_photos_own_mask_bitmap() {
        let (_temp, paths) = session_with_photos(1);
        let (source, sidecar) = parse_virtual_path(&paths[0]);
        let before = crate::exif_processing::load_sidecar(&sidecar).adjustments;
        let after = pasted_metadata(
            &paths[0],
            &source,
            &sidecar,
            &serde_json::json!({"exposure": 1}),
            None,
            None,
            None,
            None,
        )
        .unwrap();
        assert_eq!(after.adjustments["masks"], before["masks"]);
    }

    #[test]
    fn preset_ai_masks_wait_for_each_target_photo_even_with_legacy_bitmaps() {
        let (_temp, paths) = session_with_photos(2);
        for bitmap in [Value::Null, serde_json::json!("data:source")] {
            let mut sky = sky_layer("preset-sky", "data:source");
            sky["subMasks"][0]["parameters"]["maskDataBase64"] = bitmap;
            let preset =
                serde_json::json!({"masks": [sky], "lutPath": null, "lutData": null, "crop": null});
            let failed = edit_sidecars(&paths, None, |path, source, sidecar| {
                pasted_metadata(path, source, sidecar, &preset, None, None, None, None)
            });
            assert!(failed.is_empty(), "{failed:?}");
            let mut ids = Vec::new();
            for path in &paths {
                let saved =
                    crate::exif_processing::load_sidecar(&parse_virtual_path(path).1).adjustments;
                assert_eq!(saved.get("lutPath"), Some(&Value::Null));
                assert_eq!(saved.get("lutData"), Some(&Value::Null));
                assert_eq!(saved.get("crop"), Some(&Value::Null));
                let sub = &saved["masks"][0]["subMasks"][0];
                assert!(sub["parameters"]["maskDataBase64"].is_null());
                assert_eq!(sub["parameters"]["detectPending"], true);
                assert_ne!(sub["id"], "preset-sky-c");
                ids.push(sub["id"].clone());
            }
            assert_ne!(ids[0], ids[1]);
            assert_eq!(preset["masks"][0]["subMasks"][0]["id"], "preset-sky-c");
        }
    }

    /// Timing of a sync of 100 library photographs (the recipe writes
    /// `apply_adjustments_to_paths` waits for, without the thumbnails
    /// regenerated afterwards in the background): 25 settings and a layer
    /// with a 300 KB sky bitmap onto photographs with their own layer.
    /// `RAWTONIC_SYNC_FIXTURE=<a RAW> cargo test --workspace --lib
    /// sync_timing -- --ignored --nocapture`: the first run reads each
    /// photograph's EXIF (then kept in its sidecar), the others do not.
    /// Without a fixture the files are not photographs and every run reads
    /// (fails to read) their EXIF again.
    #[test]
    #[ignore]
    fn sync_timing() {
        let fixture = std::env::var_os("RAWTONIC_SYNC_FIXTURE").map(std::path::PathBuf::from);
        let (_temp, paths) = session_of(101, fixture.as_deref());
        let mut pasted = serde_json::Map::new();
        for i in 0..25 {
            pasted.insert(format!("setting{i}"), serde_json::json!(i));
        }
        pasted.insert(
            "masks".into(),
            serde_json::json!([sky_layer("synced", &"s".repeat(300_000))]),
        );
        let pasted = Value::Object(pasted);
        let paste = PasteFrom {
            source: paths[0].clone(),
            replace_layers: false,
            new_edit: None,
        };
        let mut runs = Vec::new();
        for _ in 0..5 {
            let start = std::time::Instant::now();
            let failed = edit_sidecars(&paths[1..], None, |path, source, sidecar| {
                pasted_metadata(
                    path,
                    source,
                    sidecar,
                    &pasted,
                    Some(&paste),
                    None,
                    None,
                    None,
                )
            });
            assert!(failed.is_empty());
            runs.push(start.elapsed().as_secs_f64() * 1000.0);
        }
        let first = runs[0];
        let mut later = runs[1..].to_vec();
        later.sort_by(f64::total_cmp);
        // The same with settings only (no layer).
        let settings_only = Value::Object(
            pasted
                .as_object()
                .unwrap()
                .iter()
                .filter(|(k, _)| *k != "masks")
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        );
        let start = std::time::Instant::now();
        edit_sidecars(&paths[1..], None, |path, source, sidecar| {
            pasted_metadata(
                path,
                source,
                sidecar,
                &settings_only,
                Some(&paste),
                None,
                None,
                None,
            )
        });
        let plain = start.elapsed().as_secs_f64() * 1000.0;
        eprintln!(
            "sync of 100 photographs ({}): first {:.0} ms, median of the next four {:.0} ms; settings only {plain:.0} ms",
            fixture
                .as_deref()
                .map_or("not photographs".into(), |f| f.display().to_string()),
            first,
            later[later.len() / 2],
        );
    }

    /// Batch edits take the sidecar turn per photo inside their `par_iter`
    /// and batch Auto inside rayon tasks: a worker of one batch that steals
    /// a task of the other while it waits must not stall either.
    #[test]
    fn batches_taking_the_sidecar_turn_in_one_pool_finish() {
        use std::sync::mpsc::channel;
        use std::time::Duration;
        let pool = std::sync::Arc::new(
            rayon::ThreadPoolBuilder::new()
                .num_threads(2)
                .build()
                .unwrap(),
        );
        let (waited, waits) = channel();
        for _ in 0..8 {
            let waited = waited.clone();
            pool.spawn(move || {
                let _turn = sidecar_field_edit();
                std::thread::sleep(Duration::from_millis(2));
                waited.send(()).unwrap();
            });
        }
        let (finished, done) = channel();
        std::thread::spawn({
            let pool = pool.clone();
            move || {
                let edited = pool.install(|| {
                    (0..64u32)
                        .into_par_iter()
                        .map(|i| {
                            let _turn = sidecar_field_edit();
                            i
                        })
                        .sum::<u32>()
                });
                finished.send(edited).unwrap();
            }
        });
        assert_eq!(done.recv_timeout(Duration::from_secs(10)), Ok(2016));
        for _ in 0..8 {
            waits.recv_timeout(Duration::from_secs(10)).unwrap();
        }
    }

    #[test]
    fn an_offline_save_from_stale_facts_keeps_the_cameras_ca_correction() {
        use rawtonic_engine::source_facts::{SourceFacts, forget, register};
        // A Panasonic preview made before the CA data was read (facts
        // version 2: no camera lens data for a CA-only TZ81).
        let source = Path::new("/offline/volume/Job/Capture/P1000456.RW2");
        let path = source.to_string_lossy();
        register(
            &path,
            SourceFacts {
                version: 2,
                ..Default::default()
            },
        );
        let params = serde_json::json!({"tca_vr": 1.0004, "tca_vb": 0.9997});
        let mut adjustments = serde_json::json!({
            "lensCorrectionMode": "camera", "cameraCaVersion": 1,
            "lensDistortionParams": params,
        });
        resolve_lens_params_in_adjustments(
            &mut adjustments,
            LensLookup::Fresh,
            &None,
            None,
            source,
        );
        assert_eq!(adjustments["lensDistortionParams"], params);
        // Recipes from before the CA data are rewritten from the facts.
        adjustments["cameraCaVersion"] = serde_json::json!(0);
        resolve_lens_params_in_adjustments(
            &mut adjustments,
            LensLookup::Fresh,
            &None,
            None,
            source,
        );
        assert!(adjustments.get("lensDistortionParams").is_none());
        // The camera data's CA version is part of the lens choice: an
        // editor save that changes it resolves (here: facts without camera
        // data), one that keeps it keeps the parameters.
        let mut kept = serde_json::json!({"lensCorrectionMode": "camera", "cameraCaVersion": 1,
            "lensDistortionParams": params});
        kept[LENS_PROFILE_FOR] = serde_json::json!(lens_profile_stamp(&kept));
        let mut unrelated = kept.clone();
        resolve_lens_params_in_adjustments(
            &mut unrelated,
            LensLookup::Keep { previous: None },
            &None,
            None,
            source,
        );
        assert_eq!(unrelated["lensDistortionParams"], params);
        let mut version = kept.clone();
        version["cameraCaVersion"] = serde_json::json!(0);
        resolve_lens_params_in_adjustments(
            &mut version,
            LensLookup::Keep { previous: None },
            &None,
            None,
            source,
        );
        assert!(version.get("lensDistortionParams").is_none());
        forget(&path);
    }

    /// A lens chosen under a name the database has since extended (Lensfun
    /// renamed the Sony E PZ 16-50 by appending "(Vers. I/II)") keeps its
    /// correction and takes the current name.
    #[test]
    fn a_lens_named_as_before_a_database_rename_keeps_its_correction() {
        use crate::lens_correction::{lenses_for_maker, load_lensfun_dir};
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("lenses.xml"),
            r#"<lensdatabase><lens>
                <maker>Canon</maker><model>Canon RF 28mm F2.8 STM</model><mount>Canon RF</mount>
                <cropfactor>1.0</cropfactor>
                <calibration><distortion model="ptlens" focal="28" a="0.023" b="-0.084" c="0.053"/></calibration>
            </lens></lensdatabase>"#,
        )
        .unwrap();
        let db = load_lensfun_dir(tmp.path());
        let lenses = lenses_for_maker(&db, "Canon");
        let current_name = lenses[0].get_display_name(&lenses);
        let exif = Some(HashMap::from([(
            "FocalLength".to_string(),
            "28 mm".to_string(),
        )]));
        let mut recipe = serde_json::json!({"lensCorrectionMode": "manual", "lensMaker": "Canon",
            "lensModel": "RF 28mm F2.8"});
        resolve_lens_params_in_adjustments(
            &mut recipe,
            LensLookup::Fresh,
            &exif,
            Some(&db),
            Path::new("/p/IMG.CR3"),
        );
        assert_eq!(recipe["lensModel"], serde_json::json!(current_name));
        assert!(recipe["lensDistortionParams"].is_object());
    }

    /// A Lensfun update (here: a re-measured profile) reaches a saved edit
    /// only through a new lens choice: an unrelated save or an undo keeps
    /// the parameters, or their absence, they were resolved with; a changed
    /// choice (lens tool, paste, preset Amount, the camera data's version)
    /// and Auto lens correction resolve again.
    #[test]
    fn a_saved_edit_keeps_its_lens_parameters_until_its_lens_choice_changes() {
        use crate::lens_correction::{lenses_for_maker, load_lensfun_dir};
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("lenses.xml"),
            r#"<lensdatabase><lens>
                <maker>Canon</maker><model>Canon RF 28mm F2.8 STM</model><mount>Canon RF</mount>
                <cropfactor>1.0</cropfactor>
                <calibration><distortion model="ptlens" focal="28" a="0.023" b="-0.084" c="0.053"/></calibration>
            </lens><lens>
                <maker>Canon</maker><model>Canon RF 50mm F1.8 STM</model><mount>Canon RF</mount>
                <cropfactor>1.0</cropfactor>
                <calibration><distortion model="ptlens" focal="50" a="0.01" b="-0.02" c="0.01"/></calibration>
            </lens></lensdatabase>"#,
        )
        .unwrap();
        let db = load_lensfun_dir(tmp.path());
        let lenses = lenses_for_maker(&db, "Canon");
        let rf28 = lenses
            .iter()
            .map(|l| l.get_display_name(&lenses))
            .find(|n| n.contains("28"))
            .unwrap();
        let rf50 = lenses
            .iter()
            .map(|l| l.get_display_name(&lenses))
            .find(|n| n.contains("50"))
            .unwrap();
        let exif = Some(HashMap::from([
            ("Make".to_string(), "Canon".to_string()),
            ("LensModel".to_string(), "RF28mm F2.8 STM".to_string()),
            ("FocalLength".to_string(), "28 mm".to_string()),
        ]));
        let source = Path::new("/photos/IMG_0001.CR3");
        let current = |model: &str| {
            let params = crate::lens_correction::resolve_lens_params(
                &db, "Canon", model, 28.0, None, None, None,
            );
            serde_json::to_value(params.unwrap()).unwrap()
        };
        let resolve = |recipe: &mut Value, lookup: LensLookup| {
            resolve_lens_params_in_adjustments(recipe, lookup, &exif, Some(&db), source)
        };
        // Resolved with an older database: other numbers than today's.
        let old = serde_json::json!({"k1": 0.02, "k2": -0.08, "k3": 0.05, "model": 1,
            "tca_vr": 1, "tca_vb": 1, "vig_k1": 0, "vig_k2": 0, "vig_k3": 0});
        let saved = serde_json::json!({"lensCorrectionMode": "manual", "lensMaker": "Canon",
            "lensModel": rf28, "lensDistortionParams": old});
        // A recipe from before the key: an unrelated save keeps them and
        // records what they were resolved for.
        let mut edit = saved.clone();
        edit["exposure"] = serde_json::json!(0.3);
        resolve(
            &mut edit,
            LensLookup::Keep {
                previous: Some(&saved),
            },
        );
        assert_eq!(edit["lensDistortionParams"], old);
        assert!(edit[LENS_PROFILE_FOR].is_string());
        // From then on: unrelated saves and undos keep them, whatever the
        // recipe saved last.
        let mut undone = edit.clone();
        resolve(&mut undone, LensLookup::Keep { previous: None });
        assert_eq!(undone["lensDistortionParams"], old);
        // A "no profile found" stays so after the database learns the lens.
        let mut none_found = serde_json::json!({"lensCorrectionMode": "auto"});
        none_found[LENS_PROFILE_FOR] = serde_json::json!(lens_profile_stamp(&none_found));
        let mut later = none_found.clone();
        later["exposure"] = serde_json::json!(0.3);
        resolve(&mut later, LensLookup::Keep { previous: None });
        assert!(later.get("lensDistortionParams").is_none());
        assert!(later.get("lensModel").is_none());
        // A recipe saved before the key, loaded into the editor (stamped
        // there), switched off and saved, then undone: kept.
        let mut loaded = saved.clone();
        stamp_loaded_lens_profile(&mut loaded);
        let off = serde_json::json!({"lensCorrectionMode": "manual", "lensMaker": null,
            "lensModel": null, "lensDistortionParams": null});
        let mut restored = loaded.clone();
        resolve(
            &mut restored,
            LensLookup::Keep {
                previous: Some(&off),
            },
        );
        assert_eq!(restored["lensDistortionParams"], old);
        // Parameters cleared while the choice stays (a paste) are resolved.
        let mut cleared = edit.clone();
        cleared["lensDistortionParams"] = Value::Null;
        resolve(
            &mut cleared,
            LensLookup::Keep {
                previous: Some(&edit),
            },
        );
        assert_eq!(cleared["lensDistortionParams"], current(&rf28));
        // Another lens chosen (a preset's Amount restoring the base's
        // parameters with the new choice): resolved for it.
        let mut switched = edit.clone();
        switched["lensModel"] = serde_json::json!(rf50);
        resolve(
            &mut switched,
            LensLookup::Keep {
                previous: Some(&edit),
            },
        );
        assert_eq!(switched["lensDistortionParams"], current(&rf50));
        // Before the key, a choice changed from the saved one is resolved.
        let mut chosen = saved.clone();
        chosen["lensModel"] = serde_json::json!(rf50);
        resolve(
            &mut chosen,
            LensLookup::Keep {
                previous: Some(&saved),
            },
        );
        assert_eq!(chosen["lensDistortionParams"], current(&rf50));
        // A lookup that could not run (no database yet) is tried again on
        // every later save, also after reopening, until it succeeds.
        let mut deferred = serde_json::json!({"lensCorrectionMode": "manual", "lensMaker": "Canon",
            "lensModel": rf50});
        resolve_lens_params_in_adjustments(
            &mut deferred,
            LensLookup::Keep { previous: None },
            &exif,
            None,
            source,
        );
        assert_eq!(
            deferred[LENS_PROFILE_FOR],
            serde_json::json!(LENS_UNRESOLVED)
        );
        let mut reopened = deferred.clone();
        stamp_loaded_lens_profile(&mut reopened);
        let mut again = reopened.clone();
        resolve_lens_params_in_adjustments(
            &mut again,
            LensLookup::Keep {
                previous: Some(&deferred),
            },
            &exif,
            None,
            source,
        );
        assert_eq!(again[LENS_PROFILE_FOR], serde_json::json!(LENS_UNRESOLVED));
        resolve(
            &mut again,
            LensLookup::Keep {
                previous: Some(&deferred),
            },
        );
        assert_eq!(again["lensDistortionParams"], current(&rf50));
        assert_ne!(again[LENS_PROFILE_FOR], serde_json::json!(LENS_UNRESOLVED));
        // Consecutive saves of the same unstamped recipe while it cannot be
        // resolved keep retrying (the first one vouches for nothing).
        let unstamped = serde_json::json!({"lensCorrectionMode": "manual", "lensMaker": "Canon",
            "lensModel": rf50});
        let mut first = unstamped.clone();
        resolve_lens_params_in_adjustments(
            &mut first,
            LensLookup::Keep { previous: None },
            &exif,
            None,
            source,
        );
        let mut second = unstamped.clone();
        resolve(
            &mut second,
            LensLookup::Keep {
                previous: Some(&first),
            },
        );
        assert_eq!(second["lensDistortionParams"], current(&rf50));
        // Auto without EXIF to detect from (offline) is not "no profile
        // found": unresolved, detected once the EXIF is there.
        let mut offline = serde_json::json!({"lensCorrectionMode": "auto"});
        resolve_lens_params_in_adjustments(
            &mut offline,
            LensLookup::Fresh,
            &None,
            Some(&db),
            source,
        );
        assert_eq!(
            offline[LENS_PROFILE_FOR],
            serde_json::json!(LENS_UNRESOLVED)
        );
        let previous = offline.clone();
        resolve(
            &mut offline,
            LensLookup::Keep {
                previous: Some(&previous),
            },
        );
        assert_eq!(offline["lensModel"], serde_json::json!(rf28));
        // Auto lens correction detects and resolves afresh.
        let mut fresh = edit.clone();
        fresh["lensCorrectionMode"] = serde_json::json!("auto");
        resolve(&mut fresh, LensLookup::Fresh);
        assert_eq!(fresh["lensModel"], serde_json::json!(rf28));
        assert_eq!(fresh["lensDistortionParams"], current(&rf28));
    }

    #[test]
    fn a_manual_lens_on_an_offline_photo_resolves_once_its_exif_is_there() {
        use crate::lens_correction::{lenses_for_maker, load_lensfun_dir};
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("lenses.xml"),
            r#"<lensdatabase><lens>
                <maker>Canon</maker><model>Canon RF 24-70mm F2.8L IS USM</model><mount>Canon RF</mount>
                <cropfactor>1.0</cropfactor>
                <calibration>
                    <distortion model="ptlens" focal="24" a="0.03" b="-0.09" c="0.06"/>
                    <distortion model="ptlens" focal="70" a="0.002" b="0.004" c="-0.003"/>
                </calibration>
            </lens></lensdatabase>"#,
        )
        .unwrap();
        let db = load_lensfun_dir(tmp.path());
        let lenses = lenses_for_maker(&db, "Canon");
        let zoom = lenses[0].get_display_name(&lenses);
        let at = |focal: f32| {
            let params = crate::lens_correction::resolve_lens_params(
                &db, "Canon", &zoom, focal, None, None, None,
            );
            serde_json::to_value(params.unwrap()).unwrap()
        };
        assert_ne!(at(24.0), at(50.0));
        let choice = serde_json::json!({
            "lensCorrectionMode": "manual",
            "lensMaker": "Canon",
            "lensModel": zoom,
        });

        // Offline, no cached EXIF: marked unresolved, not resolved at 50 mm.
        let offline = Path::new("/offline/volume/Job/Capture/IMG_0002.CR3");
        let mut saved = choice.clone();
        resolve_lens_params_in_adjustments(
            &mut saved,
            LensLookup::Keep { previous: None },
            &None,
            Some(&db),
            offline,
        );
        assert_eq!(saved[LENS_PROFILE_FOR], serde_json::json!(LENS_UNRESOLVED));
        assert!(saved.get("lensDistortionParams").is_none());

        // Back online: the next save resolves at the photo's focal length.
        let exif = Some(HashMap::from([(
            "FocalLength".to_string(),
            "24 mm".to_string(),
        )]));
        let previous = saved.clone();
        resolve_lens_params_in_adjustments(
            &mut saved,
            LensLookup::Keep {
                previous: Some(&previous),
            },
            &exif,
            Some(&db),
            offline,
        );
        assert_eq!(saved["lensDistortionParams"], at(24.0));
        assert_ne!(saved[LENS_PROFILE_FOR], serde_json::json!(LENS_UNRESOLVED));

        // A readable file without EXIF never gets a focal length: the
        // fallback is resolved and kept.
        let no_exif = tmp.path().join("scan.tif");
        std::fs::write(&no_exif, b"no exif").unwrap();
        let mut scan = choice.clone();
        resolve_lens_params_in_adjustments(
            &mut scan,
            LensLookup::Keep { previous: None },
            &None,
            Some(&db),
            &no_exif,
        );
        assert_eq!(scan["lensDistortionParams"], at(50.0));
        assert_ne!(scan[LENS_PROFILE_FOR], serde_json::json!(LENS_UNRESOLVED));
    }

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
    /// A camera correction pasted onto an offline photograph (shown from its
    /// preview proxy) resolves from the camera facts registered for it, as
    /// its save does; without them it stays unresolved for export to retry.
    #[test]
    fn a_pasted_camera_correction_resolves_offline_from_registered_facts() {
        let source = Path::new("/offline/pasted-camera-lens/DSCF0001.RAF");
        let pasted = serde_json::json!({
            "lensCorrectionMode": "camera", "cameraCaVersion": 1,
            "lensDistortionParams": null, LENS_PROFILE_FOR: LENS_UNRESOLVED,
        });
        let unregistered = pasted_lens_data(pasted.clone(), &None, None, source);
        assert_eq!(
            unregistered[LENS_PROFILE_FOR],
            serde_json::json!(LENS_UNRESOLVED)
        );
        assert!(unregistered["lensDistortionParams"].is_null());

        let params = serde_json::json!({"k1": -0.02, "k2": 0.004, "k3": 0.0, "model": 0,
            "tca_vr": 1.0002, "tca_vb": 0.9998, "vig_k1": 0.1, "vig_k2": 0.0, "vig_k3": 0.0});
        rawtonic_engine::source_facts::register(
            &source.to_string_lossy(),
            rawtonic_engine::source_facts::SourceFacts {
                version: rawtonic_engine::source_facts::VERSION,
                camera_lens: Some(serde_json::from_value(params.clone()).unwrap()),
                ..Default::default()
            },
        );
        // An export of the editor's recipe started before that resolves it too.
        let mut exported = pasted.clone();
        rawtonic_export::exif_processing::prepare_recipe_lens(&mut exported, source).unwrap();
        assert_eq!(exported["lensDistortionParams"], params);
        let resolved = pasted_lens_data(pasted, &None, None, source);
        assert_eq!(resolved["lensDistortionParams"], params);
        assert_ne!(
            resolved[LENS_PROFILE_FOR],
            serde_json::json!(LENS_UNRESOLVED)
        );
        assert!(resolved["lensMaker"].is_null() && resolved["lensModel"].is_null());
        rawtonic_engine::source_facts::forget(&source.to_string_lossy());
    }
}
