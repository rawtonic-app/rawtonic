//! Edits applied to selected photos: adjustments, auto corrections, ratings, labels and their sidecars.
//! Part of `file_management`, split by concern (ADR 0009).
use super::add_to_thumbnail_queue;
use super::emit_thumbnail_cache_setup_error;
use super::emit_thumbnail_generated;
use super::generate_single_thumbnail_and_cache;
use super::increment_thumbnail_progress;
use super::parse_virtual_path;
use super::resolve_thumbnail_cache_dir;
use super::sync_metadata_to_xmp;
use super::xmp_sync::load_sidecar_with_xmp;
use crate::app_state::AppState;
use crate::batch_journal::{self, BatchKind};
use crate::formats::is_raw_file;
use crate::gpu_processing;
use crate::image_loader;
use crate::image_processing::ImageMetadata;
use crate::state::thumbnails::ThumbnailState;
use image::DynamicImage;
use rawtonic_fs::Canonical;
use rawtonic_metadata::culling::Rating;
use rawtonic_metadata::labels::EDITOR_TAG_PREFIX;
use rawtonic_metadata::photo_ref::PhotoRef;
use rawtonic_recipe::lens;
use rawtonic_recipe::masks;
use rayon::prelude::*;
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::path::Path;
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
    camera_crop_known: Option<bool>,
    app_handle: AppHandle,
) -> Result<EditorSave, String> {
    tauri::async_runtime::spawn_blocking(move || {
        // A caller whose loaded decode is stale (an Edit With render another
        // program saved) asks for the recipe alone.
        if update_thumbnail == Some(false) {
            return save_metadata_blocking(&path, adjustments, camera_crop_known, &app_handle);
        }
        save_metadata_and_update_thumbnail_blocking(
            path,
            adjustments,
            camera_crop_known,
            app_handle,
        )
    })
    .await
    .map_err(|e| format!("Saving metadata failed: {e}"))?
}

/// What an editor save tells the webview.
#[derive(Debug, Default, serde::Serialize, PartialEq)]
pub struct EditorSave {
    /// The photograph's first edit was written without the camera's crop:
    /// its original is in the cloud and nothing cached the crop
    /// (`missing_camera_crops`, as a batch apply reports it).
    #[serde(rename = "cropMissing", skip_serializing_if = "std::ops::Not::not")]
    pub crop_missing: bool,
}

/// Writes `adjustments` as the recipe of `path` (sidecar or library, and its
/// XMP when that is synchronised).
fn save_metadata_blocking(
    path: &str,
    adjustments: Value,
    camera_crop_known: Option<bool>,
    app_handle: &AppHandle,
) -> Result<EditorSave, String> {
    let (source_path, _) = parse_virtual_path(path);
    let (metadata, saved) = write_editor_save(path, adjustments, camera_crop_known)?;

    if let Ok(settings) = crate::state::settings::try_current(app_handle)
        && settings.enable_xmp_sync.unwrap_or(false)
    {
        let create_if_missing = settings.create_xmp_if_missing.unwrap_or(false);
        sync_metadata_to_xmp(&source_path, &metadata, create_if_missing);
    }

    Ok(saved)
}

/// Writes `adjustments` as the recipe of `path` (`save_metadata_blocking`)
/// and gives the editor state written; its EXIF only the user's own
/// overrides (`keep_own_exif`).
fn write_editor_save(
    path: &str,
    adjustments: Value,
    camera_crop_known: Option<bool>,
) -> Result<(ImageMetadata, EditorSave), String> {
    let (source_path, sidecar_path) = parse_virtual_path(path);

    let _turn = sidecar_field_edit();
    let (mut metadata, shown_exif) =
        crate::exif_processing::load_sidecar_for_edit(&sidecar_path, &source_path);

    let mut final_adjustments = adjustments;
    {
        let lens_db_guard = crate::lens_correction::global_lens_db();
        resolve_lens_params_in_adjustments(
            &mut final_adjustments,
            LensLookup::Keep {
                previous: Some(&metadata.adjustments),
            },
            &shown_exif,
            lens_db_guard.as_deref(),
            &source_path,
        );
    }

    // The editor starts a never-edited photograph's first edit with the
    // camera crop it was given (`get_camera_crops`); for an original in the
    // cloud without a cached crop it had none to give.
    let crop_missing = first_edit_without_camera_crop_from(
        path,
        &metadata.adjustments,
        &final_adjustments,
        camera_crop_known,
    );
    metadata.adjustments = final_adjustments;
    crate::exif_processing::keep_own_exif(&mut metadata, &source_path);

    let json_string = serde_json::to_string_pretty(&metadata).map_err(|e| e.to_string())?;
    crate::session_bridge::write_editor_sidecar(&sidecar_path, json_string)
        .map_err(|e| e.to_string())?;

    Ok((metadata, EditorSave { crop_missing }))
}

pub(crate) fn save_metadata_and_update_thumbnail_blocking(
    path: String,
    adjustments: Value,
    camera_crop_known: Option<bool>,
    app_handle: AppHandle,
) -> Result<EditorSave, String> {
    let saved = save_metadata_blocking(&path, adjustments, camera_crop_known, &app_handle)?;
    let state = app_handle.state::<AppState>();

    let loaded_image_lock = state.source.original_image.lock().unwrap();
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

    let gpu_context = gpu_processing::get_or_init_gpu_context(
        &app_handle.state::<crate::state::gpu::GpuState>(),
        &app_handle,
    )
    .ok();
    let app_handle_clone = app_handle.clone();
    let path_clone = path.clone();

    add_to_thumbnail_queue(&app_handle.state::<ThumbnailState>(), 1, &app_handle);

    rawtonic_runtime::spawn_render_thread("saved-thumbnail", move || {
        let settings = crate::state::settings::current(&app_handle_clone);

        let thumb_cache_dir = match resolve_thumbnail_cache_dir(&app_handle_clone) {
            Ok(dir) => dir,
            Err(e) => {
                log::warn!(
                    "Unable to initialize thumbnail cache directory for '{}': {}",
                    path_clone,
                    e
                );
                emit_thumbnail_cache_setup_error(&app_handle_clone, &path_clone, &e);
                increment_thumbnail_progress(
                    &app_handle_clone.state::<ThumbnailState>(),
                    &app_handle_clone,
                );
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

        increment_thumbnail_progress(
            &app_handle_clone.state::<ThumbnailState>(),
            &app_handle_clone,
        );
    });

    Ok(saved)
}

/// Regenerates the thumbnails of `paths`, whose recipes a command outside
/// the editor's batch edits wrote (a Lightroom or Capture One import), in
/// the background: counted in the thumbnail queue, each announced as it is
/// made, as after a batch edit.
pub(crate) fn refresh_thumbnails(app_handle: AppHandle, paths: Vec<String>) {
    if paths.is_empty() {
        return;
    }
    add_to_thumbnail_queue(
        &app_handle.state::<ThumbnailState>(),
        paths.len(),
        &app_handle,
    );
    regenerate_thumbnails(app_handle, paths);
}

/// Regenerates the thumbnails of `paths` in the background, after their
/// recipes were written (the thumbnail queue already counts them).
fn regenerate_thumbnails(app_handle: AppHandle, paths: Vec<String>) {
    tauri::async_runtime::spawn_blocking(move || {
        let settings = crate::state::settings::current(&app_handle);
        let thumb_cache_dir = match resolve_thumbnail_cache_dir(&app_handle) {
            Ok(dir) => dir,
            Err(e) => {
                log::warn!("Unable to initialize thumbnail cache directory: {}", e);
                for path in &paths {
                    emit_thumbnail_cache_setup_error(&app_handle, path, &e);
                }
                for _ in 0..paths.len() {
                    increment_thumbnail_progress(
                        &app_handle.state::<ThumbnailState>(),
                        &app_handle,
                    );
                }
                return;
            }
        };

        let gpu_context = gpu_processing::get_or_init_gpu_context(
            &app_handle.state::<crate::state::gpu::GpuState>(),
            &app_handle,
        )
        .ok();

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

            increment_thumbnail_progress(&app_handle.state::<ThumbnailState>(), &app_handle);
        });
    });
}

/// `Some(create if missing)` when edits are synced to XMP sidecars.
pub(crate) fn xmp_sync(app_handle: &AppHandle) -> Option<bool> {
    let settings = crate::state::settings::current(app_handle);
    settings
        .enable_xmp_sync
        .unwrap_or(false)
        .then(|| settings.create_xmp_if_missing.unwrap_or(false))
}

/// What a batch sidecar edit (`edit_sidecars`) wrote.
#[derive(Debug, Default)]
pub(crate) struct SidecarEdits {
    /// The photos not saved, with the reason (an `edit` error counts).
    pub failures: Vec<(String, String)>,
    /// The photos saved in their library whose sidecar beside the photo
    /// could not be written (a write-protected folder), with the reason:
    /// their edit holds (and its undo step), the file follows on the next
    /// save (`session_bridge::DerivedSidecarUnwritten`).
    pub unwritten: Vec<(String, String)>,
}

impl SidecarEdits {
    /// The note for the user about `unwritten` of `total` photos, if any.
    pub fn unwritten_note(&self, total: usize) -> Option<String> {
        if self.unwritten.is_empty() {
            return None;
        }
        let mut unwritten = self.unwritten.clone();
        unwritten.sort();
        let what = if total == 1 {
            "the photo".to_string()
        } else {
            format!("{} of {total} photos", unwritten.len())
        };
        Some(format!(
            "Saved in the library; the sidecar file next to {what} could not be written: {}",
            listed_errors(&unwritten)
        ))
    }
}

/// The first few of `errors` (path: error), sorted, and how many more.
fn listed_errors(errors: &[(String, String)]) -> String {
    const SHOWN: usize = 3;
    let mut listed: Vec<String> = errors
        .iter()
        .take(SHOWN)
        .map(|(path, error)| format!("{path}: {error}"))
        .collect();
    if errors.len() > SHOWN {
        listed.push(format!("and {} more", errors.len() - SHOWN));
    }
    listed.join("; ")
}

/// Writes the editor sidecar of each of `paths` as `edit` changes its
/// metadata (in parallel), and its XMP when `xmp` (`xmp_sync`).
fn edit_sidecars(
    paths: &[String],
    xmp: Option<bool>,
    edit: impl Fn(&str, &Path, &Path) -> Result<ImageMetadata, String> + Sync,
) -> SidecarEdits {
    enum Outcome {
        Saved,
        Failed(String),
        Unwritten(String),
    }
    let outcomes: Vec<(String, Outcome)> = paths
        .par_iter()
        .map(|path| {
            let _turn = sidecar_field_edit();
            let (source_path, sidecar_path) = parse_virtual_path(path);
            let edited = match conformed_frame(&source_path, &sidecar_path) {
                Some(metadata) => {
                    crate::exif_processing::reading_conformed(&sidecar_path, metadata, || {
                        edit(path, &source_path, &sidecar_path)
                    })
                }
                None => edit(path, &source_path, &sidecar_path),
            };
            let outcome = match edited {
                Err(e) => Outcome::Failed(e),
                Ok(metadata) => {
                    let outcome = match write_sidecar(path, &metadata) {
                        Ok(()) => Outcome::Saved,
                        Err(e) => match crate::session_bridge::saved_in_library(&e) {
                            Some(unwritten) => Outcome::Unwritten(unwritten.to_string()),
                            None => Outcome::Failed(e.to_string()),
                        },
                    };
                    if let (Some(create_if_missing), false) =
                        (xmp, matches!(outcome, Outcome::Failed(_)))
                    {
                        sync_metadata_to_xmp(&source_path, &metadata, create_if_missing);
                    }
                    outcome
                }
            };
            (path.clone(), outcome)
        })
        .collect();
    let mut edits = SidecarEdits::default();
    for (path, outcome) in outcomes {
        match outcome {
            Outcome::Saved => {}
            Outcome::Failed(e) => edits.failures.push((path, e)),
            Outcome::Unwritten(e) => edits.unwritten.push((path, e)),
        }
    }
    edits
}

/// A camera TIFF's edit made on its embedded thumbnail, moved to the full
/// frame (`recipe_frame::conform`), for a batch edit to change: settings
/// pasted from a RAW (its processing versions) would otherwise make it read
/// as made on the full frame. The edit reads it in memory and writes it once
/// (`exif_processing::reading_conformed`); a refused edit writes nothing.
/// None for other recipes; other files are not read.
fn conformed_frame(source_path: &Path, sidecar_path: &Path) -> Option<ImageMetadata> {
    rawtonic_engine::recipe_frame::tiff_frames(source_path)?;
    let mut metadata = crate::exif_processing::load_sidecar(sidecar_path);
    rawtonic_engine::recipe_frame::conform(&mut metadata.adjustments, source_path)
        .then_some(metadata)
}

/// Runs the blocking `work` (which writes the recipes of `paths`), then
/// regenerates the thumbnails in the background. The command finishes once
/// every recipe is written, so the webview reads the new recipes afterwards
/// and hears about the ones not saved, and (the note) about the ones saved
/// in their library whose sidecar beside the photo could not be written.
async fn save_then_regenerate(
    app_handle: AppHandle,
    paths: Vec<String>,
    work: impl FnOnce(&AppHandle, &[String]) -> SidecarEdits + Send + 'static,
) -> (Result<(), SaveFailure>, Option<String>) {
    add_to_thumbnail_queue(
        &app_handle.state::<ThumbnailState>(),
        paths.len(),
        &app_handle,
    );
    let (app, batch) = (app_handle.clone(), paths.clone());
    let edits = tauri::async_runtime::spawn_blocking(move || work(&app, &batch))
        .await
        .unwrap_or_else(|e| SidecarEdits {
            failures: paths
                .iter()
                .map(|path| (path.clone(), format!("Saving failed: {e}")))
                .collect(),
            unwritten: Vec::new(),
        });
    let total = paths.len();
    regenerate_thumbnails(app_handle, paths);
    let note = edits.unwritten_note(total);
    (SaveFailure::check(total, edits.failures), note)
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
/// and after, the photographs not written with the first errors, and the
/// note about those saved in their library whose sidecar beside the photo
/// could not be written (they are among `changes`: their undo step holds).
#[derive(serde::Serialize, Debug, Default)]
pub struct RecipeBatch {
    pub changes: Vec<RecipeChange>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<SaveFailure>,
    /// Photographs whose first edit was written without the camera's crop
    /// because their original is in the cloud and nothing cached it
    /// (`missing_camera_crops`).
    #[serde(rename = "cropMissing", skip_serializing_if = "Vec::is_empty")]
    pub crop_missing: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unwritten: Option<String>,
}

/// The photographs among `changes` that were never edited and whose original
/// is only in the cloud, with no cached camera crop: their first edit
/// started without the crop a camera that set another aspect ratio gives
/// (the original is not downloaded for it). Told to the photographer.
fn missing_camera_crops(changes: &[RecipeChange]) -> Vec<String> {
    changes
        .iter()
        .filter(|change| {
            first_edit_without_camera_crop(&change.path, &change.before, &change.after)
        })
        .map(|change| change.path.clone())
        .collect()
}

/// Whether `after` is the first edit of the never-edited RAW at `path`
/// (`before`) whose original is only in the cloud with no cached camera
/// crop: written without the crop the camera's aspect ratio gives.
fn first_edit_without_camera_crop(path: &str, before: &Value, after: &Value) -> bool {
    first_edit_without_camera_crop_from(path, before, after, None)
}

fn first_edit_without_camera_crop_from(
    path: &str,
    before: &Value,
    after: &Value,
    camera_crop_known: Option<bool>,
) -> bool {
    use rawtonic_engine::image_processing::is_new_recipe;
    if !is_new_recipe(before) || is_new_recipe(after) {
        return false;
    }
    let (source, _) = parse_virtual_path(path);
    rawtonic_engine::formats::is_raw_file(&source)
        && crate::file_management::is_cloud_placeholder(&source)
        && !camera_crop_known
            .unwrap_or_else(|| !crate::camera_crop::raw_crops(&source, true).is_empty())
}

/// `edit_sidecars` that also returns the recipe changes of the photographs
/// written (read in the same sidecar turn as the edit).
pub(crate) fn edit_sidecars_recorded(
    paths: &[String],
    xmp: Option<bool>,
    edit: impl Fn(&str, &Path, &Path) -> Result<ImageMetadata, String> + Sync,
) -> (Vec<RecipeChange>, SidecarEdits) {
    let changes = std::sync::Mutex::new(Vec::new());
    let edits = edit_sidecars(paths, xmp, |path, source_path, sidecar_path| {
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
    // An edit whose save failed was not written (one saved in its library
    // only was: its undo step holds).
    changes.retain(|change| !edits.failures.iter().any(|(path, _)| *path == change.path));
    (changes, edits)
}

/// `save_then_regenerate` for a recorded batch edit (`edit_sidecars_recorded`).
pub(crate) async fn recorded_batch(
    app_handle: AppHandle,
    paths: Vec<String>,
    work: impl FnOnce(&AppHandle, &[String]) -> (Vec<RecipeChange>, SidecarEdits) + Send + 'static,
) -> RecipeBatch {
    let recorded = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = recorded.clone();
    let (result, unwritten) = save_then_regenerate(app_handle, paths, move |app_handle, paths| {
        let (changes, edits) = work(app_handle, paths);
        *sink.lock().unwrap() = changes;
        edits
    })
    .await;
    let changes = std::mem::take(&mut *recorded.lock().unwrap());
    RecipeBatch {
        crop_missing: missing_camera_crops(&changes),
        changes,
        failure: result.err(),
        unwritten,
    }
}

/// Where pasted or synced settings come from (`apply_adjustments_to_paths`).
#[derive(serde::Deserialize, serde::Serialize, Debug, Clone)]
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
    /// Chosen in the Copy and Sync settings picker: processing versions go
    /// along exactly as chosen there (`pasted_versions` leaves them).
    #[serde(default)]
    pub as_chosen: bool,
}

/// The processing versions of a new edit (`newEditResponses` in the editor).
#[derive(serde::Deserialize, serde::Serialize, Debug, Clone, Default)]
pub struct NewEditResponses {
    pub raw: serde_json::Map<String, Value>,
    pub other: serde_json::Map<String, Value>,
}

/// A style applied from the styles list (`apply_adjustments_to_paths`): it
/// replaces the style last applied to each photograph, or with `remove`
/// takes `id` back from the photographs it is the last applied style of
/// (`rawtonic_engine::applied_style`).
#[derive(serde::Deserialize, serde::Serialize, Debug, Clone)]
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
    open: Option<String>,
    app_handle: AppHandle,
) -> Result<RecipeBatch, String> {
    let batch = recorded_batch(app_handle, paths, move |app_handle, paths| {
        // A kill before every photograph is written is offered at the next
        // open (crate::batch_journal).
        let batch = if style.is_some() {
            BatchKind::Style
        } else if paste.is_some() {
            BatchKind::Paste
        } else {
            BatchKind::Preset
        };
        let args =
            serde_json::json!({ "adjustments": adjustments, "paste": paste, "style": style });
        // `open`: the open photograph, which the editor gives the same
        // settings itself; it belongs to the batch the photographer made.
        let members: Vec<String> = paths.iter().cloned().chain(open).collect();
        let _job = batch_journal::begin(
            app_handle,
            batch,
            "apply_adjustments_to_paths",
            args,
            None,
            &members,
            batch_journal::current,
        );
        let settings = crate::state::settings::current(app_handle);
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
                    // An evicted original is never downloaded for this
                    // (`pasted_metadata` reports the photo).
                    if crate::file_management::is_cloud_placeholder(&source_path) {
                        return None;
                    }
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
    .await;
    // The open photo is not among `paths`: the editor computes its own.
    queue_selections(&batch);
    Ok(batch)
}

/// AI selections the written photos now wait for (a preset's Sky or People
/// layer, a recipe an undo or redo brings back) are computed in the
/// background, as opening each photo would (`selection_queue`).
fn queue_selections(batch: &RecipeBatch) {
    crate::selection_queue::enqueue(
        batch
            .changes
            .iter()
            .filter(|change| crate::selection_queue::wants_detection(&change.after))
            .map(|change| change.path.clone()),
    );
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
    let (mut existing_metadata, mut shown_exif) =
        crate::exif_processing::load_sidecar_for_edit(sidecar_path, source_path);
    let in_cloud = crate::file_management::is_cloud_placeholder(source_path);
    if in_cloud {
        cloud_gate(path, source_path, adjustments, size)?;
        // The EXIF the previews kept when the catalog has none (their
        // headers only; the original is not opened).
        if shown_exif.is_none() {
            shown_exif = crate::preview_proxy::cached_camera(source_path).1;
        }
    }
    existing_metadata.adjustments = pasted_into_with(
        std::mem::take(&mut existing_metadata.adjustments),
        &shown_exif,
        path,
        source_path,
        adjustments,
        paste,
        style,
        size,
        lens_db,
        in_cloud,
    )?;
    Ok(existing_metadata)
}

/// What a batch edit of a photograph whose original is only in the cloud
/// (an evicted placeholder; reading it makes macOS download it) cannot do
/// without the file: Err naming it, so the photograph is left as it is and
/// the photographer downloads it and applies again. (A lens choice the paste
/// changes is checked once the settings are merged, `pasted_into_with`.)
fn cloud_gate(
    path: &str,
    source_path: &Path,
    adjustments: &Value,
    size: Option<(f64, f64)>,
) -> Result<(), String> {
    // A first edit starts with the camera's crop:
    // from what its previews and EXIF cached (`pasted_into_with`); without a
    // cached crop the edit is written and the crop reported as not applied
    // (`missing_camera_crops`), so no first edit waits for a download.
    // Lightroom gradients and brushes are placed in the photo's pixels.
    if size.is_none() && adjustments.to_string().contains("lrNormalized") {
        return Err(cloud_refusal(path, "a preset's gradients and brushes"));
    }
    // Whether a TIFF is a camera's RAW is read from the file; the tone
    // settings pasted into it depend on that.
    if source_path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("tif") || e.eq_ignore_ascii_case("tiff"))
    {
        return Err(cloud_refusal(path, "settings to a TIFF"));
    }
    Ok(())
}

fn cloud_refusal(path: &str, what: &str) -> String {
    format!("{path}: original is in the cloud — download it to apply {what}")
}

/// `base` (a photograph's recipe) with `adjustments` pasted as
/// `apply_adjustments_to_paths` pastes them (`pasted_metadata`); the editor's
/// `pasteInto` pastes onto the open photograph by the same rules.
#[cfg(test)]
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
    pasted_into_with(
        base,
        exif,
        path,
        source_path,
        adjustments,
        paste,
        style,
        size,
        lens_db,
        false,
    )
}

/// `pasted_into`; `in_cloud`: the original cannot be read (`cloud_gate`), so
/// a lens choice the paste changes is resolved from cached EXIF only.
#[allow(clippy::too_many_arguments)]
fn pasted_into_with(
    base: Value,
    exif: &Option<HashMap<String, String>>,
    path: &str,
    source_path: &Path,
    adjustments: &Value,
    paste: Option<&PasteFrom>,
    style: Option<&StyleOp>,
    size: Option<(f64, f64)>,
    lens_db: Option<&rawtonic_engine::lens_correction::LensDatabase>,
    in_cloud: bool,
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
    // A photo without an edit starts one: the camera's crop and every
    // version of the editor's new edit, so the paste renders as on the
    // photo opened first (a pasted crop replaces the crop; pasted versions
    // the photo's: it has nothing else that uses them).
    crate::camera_crop::start_edit(&mut new_adjustments, source_path)?;
    // The versions chosen in the picker come with the editor's new edit,
    // which adds those left to the camera's data.
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

    // In this photograph's responses: Whites carried without a tone version
    // and Blacks mean the amount they render, and a version key stays the
    // photograph's while something the paste leaves renders with it
    // (`pasted_versions`); versions chosen in the picker go along as chosen.
    let mut expressed = adjustments.clone();
    if paste.is_some_and(|from| from.as_chosen) {
        rawtonic_engine::image_processing::express_pasted_whites(&mut expressed, &new_adjustments);
        rawtonic_engine::image_processing::express_pasted_blacks(
            &mut expressed,
            &new_adjustments,
            is_raw_file(source_path),
        );
    } else {
        rawtonic_engine::pasted_versions::express_pasted(
            &mut expressed,
            &rawtonic_engine::pasted_versions::PasteTarget {
                recipe: &new_adjustments,
                is_new: new_edit,
                is_raw: is_raw_file(source_path),
                // A preset's layers join the photograph's own.
                replace_layers: paste.is_some_and(|from| from.replace_layers),
                merge_groups: style.is_some(),
            },
        );
    }
    let adjustments = &expressed;
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
                Some(from) if k == masks::MASKS.name() => rawtonic_engine::masks::paste_layers(
                    new_map.get(masks::MASKS.name()).unwrap_or(&Value::Null),
                    v,
                    same_photograph(&from.source, path),
                    from.replace_layers,
                    &mut || uuid::Uuid::new_v4().to_string(),
                ),
                // A preset's or style's layers join the photograph's own
                // (as Lightroom adds a preset's masks; coordinator decision
                // APR1 2026-11-21): a layer of the same id replaces its
                // earlier copy (applied again: updated, not doubled); their
                // AI detections wait for this photograph's.
                None if k == masks::MASKS.name() => rawtonic_engine::masks::paste_layers(
                    new_map.get(masks::MASKS.name()).unwrap_or(&Value::Null),
                    v,
                    false,
                    false,
                    &mut || uuid::Uuid::new_v4().to_string(),
                ),
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
        rawtonic_engine::masks::rendered_layers(
            adjustments.get(masks::MASKS.name()).unwrap_or(&Value::Null),
        )
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

    // An evicted original is not read for its lens: a lens choice that stays
    // is kept, a changed one needs cached EXIF (and not the camera's own
    // data, which is in the file).
    let lens_changed = rawtonic_engine::lens_recipe::lens_profile_stamp(&previous)
        != rawtonic_engine::lens_recipe::lens_profile_stamp(&new_adjustments);
    if in_cloud && lens_changed {
        let mode = new_adjustments
            .get(lens::CORRECTION_MODE.name())
            .and_then(Value::as_str);
        let named = |key: &str| {
            new_adjustments
                .get(key)
                .and_then(Value::as_str)
                .is_some_and(|name| !name.is_empty())
        };
        // Looked up (auto, or a named lens) needs the EXIF; the camera's own
        // data needs the file; turning the correction off or manual with
        // no lens only clears.
        let camera_data = mode == Some(rawtonic_engine::embedded_lens::CAMERA_MODE);
        let lookup =
            mode == Some("auto") || (named(lens::MAKER.name()) && named(lens::MODEL.name()));
        if camera_data || (lookup && exif.is_none()) {
            return Err(cloud_refusal(path, "the lens correction"));
        }
    }
    if !in_cloud || lens_changed {
        resolve_lens_params_in_adjustments(
            &mut new_adjustments,
            LensLookup::Keep {
                previous: Some(&previous),
            },
            exif,
            lens_db,
            source_path,
        );
    }
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
        lens::MAKER.name(),
        lens::MODEL.name(),
        lens::DISTORTION_PARAMS.name(),
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
        || a.canonical()
            .ok()
            .is_some_and(|a| b.canonical().ok() == Some(a))
}

/// Resets each photograph to a new edit, which starts from the user's develop
/// defaults; `neutral` (Reset to neutral) records that it gets none
/// (`develop_defaults::KEY` `[]`), so it starts neutral and stays so when the
/// defaults change.
#[tauri::command]
pub async fn reset_adjustments_for_paths(
    paths: Vec<String>,
    neutral: Option<bool>,
    app_handle: AppHandle,
) -> Result<RecipeBatch, String> {
    let neutral = neutral.unwrap_or(false);
    Ok(recorded_batch(app_handle, paths, move |app_handle, paths| {
        edit_sidecars_recorded(paths, xmp_sync(app_handle), |_, _, sidecar_path| {
            let mut existing_metadata = crate::exif_processing::load_sidecar(sidecar_path);
            existing_metadata.adjustments = reset_recipe(&existing_metadata.adjustments, neutral);
            Ok(existing_metadata)
        })
    })
    .await)
}

/// A Reset's recipe: a new edit in the frame the photograph was edited in
/// (ADR 0020), with no develop defaults when `neutral`.
fn reset_recipe(adjustments: &Value, neutral: bool) -> Value {
    let mut reset = rawtonic_engine::source_frame::reset_recipe(adjustments);
    if neutral && let Some(map) = reset.as_object_mut() {
        map.insert(
            rawtonic_engine::develop_defaults::KEY.to_string(),
            Value::Array(Vec::new()),
        );
    }
    reset
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
    let batch = recorded_batch(app_handle, paths, move |app_handle, paths| {
        edit_sidecars_recorded(paths, xmp_sync(app_handle), |path, _, sidecar_path| {
            let mut metadata = crate::exif_processing::load_sidecar(sidecar_path);
            metadata.adjustments = by_path.get(path).ok_or("no recipe")?.clone();
            Ok(metadata)
        })
    })
    .await;
    // An undo or redo of a batch apply brings back recipes from before
    // their selections were computed: computed again.
    queue_selections(&batch);
    Ok(batch)
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
                auto_lens_metadata(source_path, sidecar_path, lens_db.as_deref())
            },
        )
    })
    .await)
}

/// The editor state of a photo with Auto lens correction turned on and
/// detected again (`apply_auto_lens_correction_to_paths`).
fn auto_lens_metadata(
    source_path: &Path,
    sidecar_path: &Path,
    lens_db: Option<&rawtonic_engine::lens_correction::LensDatabase>,
) -> Result<ImageMetadata, String> {
    let (mut existing_metadata, shown_exif) =
        crate::exif_processing::load_sidecar_for_edit(sidecar_path, source_path);

    crate::camera_crop::start_edit(&mut existing_metadata.adjustments, source_path)?;

    if let Some(obj) = existing_metadata.adjustments.as_object_mut() {
        obj.insert(
            lens::CORRECTION_MODE.name().to_string(),
            serde_json::json!("auto"),
        );
        obj.insert(
            lens::DISTORTION_ENABLED.name().to_string(),
            serde_json::json!(true),
        );
        obj.insert(
            lens::TCA_ENABLED.name().to_string(),
            serde_json::json!(true),
        );
        obj.insert(
            lens::VIGNETTE_ENABLED.name().to_string(),
            serde_json::json!(true),
        );
    }

    // Detected again on the user's request: from the current database.
    resolve_lens_params_in_adjustments(
        &mut existing_metadata.adjustments,
        LensLookup::Fresh,
        &shown_exif,
        lens_db,
        source_path,
    );
    Ok(existing_metadata)
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
    /// As [`RecipeBatch::unwritten`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unwritten: Option<String>,
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
    let (result, unwritten) = save_then_regenerate(app_handle, paths, move |app_handle, paths| {
        edit_sidecars(
            paths,
            xmp_sync(app_handle),
            |path, source_path, sidecar_path| {
                let values = by_path.get(path).ok_or("no values")?;
                let mut metadata = crate::exif_processing::load_sidecar(sidecar_path);
                let old = set_photo_values(&mut metadata.adjustments, source_path, values)?;
                written.lock().unwrap().push(RecipeValues {
                    path: path.to_string(),
                    values: old,
                });
                Ok(metadata)
            },
        )
    })
    .await;
    let mut before = std::mem::take(&mut *before.lock().unwrap());
    let failure = result.err();
    // A photo whose edit succeeded but whose save failed was not written.
    if let Some(failure) = &failure {
        before.retain(|b| !failure.failed.contains(&b.path));
    }
    Ok(RecipeWrite {
        before,
        failure,
        unwritten,
    })
}

fn set_photo_values(
    recipe: &mut Value,
    source: &Path,
    values: &serde_json::Map<String, Value>,
) -> Result<serde_json::Map<String, Value>, String> {
    crate::camera_crop::start_edit(recipe, source)?;
    set_values(recipe, values)
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
        keywords: Vec::new(),
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
    add_to_thumbnail_queue(
        &app_handle.state::<ThumbnailState>(),
        paths.len(),
        &app_handle,
    );
    let total = paths.len();
    let all = paths.clone();
    let failures = tauri::async_runtime::spawn_blocking(move || {
        let settle = |path: &str, why: Option<&str>| {
            if let Some(batch) = batch.as_deref() {
                emit_batch_write_settled(&app_handle, batch, path, why, &[]);
            }
        };
        let settings = crate::state::settings::current(&app_handle);
        let enable_xmp_sync = settings.enable_xmp_sync.unwrap_or(false);
        let create_xmp_if_missing = settings.create_xmp_if_missing.unwrap_or(false);

        let thumb_cache_dir = resolve_thumbnail_cache_dir(&app_handle);
        if let Err(e) = &thumb_cache_dir {
            log::warn!("Unable to initialize thumbnail cache directory: {}", e);
        }

        let gpu_context = gpu_processing::get_or_init_gpu_context(
            &app_handle.state::<crate::state::gpu::GpuState>(),
            &app_handle,
        )
        .ok();

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
                        crate::image_processing::effective_render_version(&recipe),
                    );

                    // Read again for the write: the photo may have been
                    // edited or tagged while it was decoded and measured.
                    let _turn = sidecar_field_edit();
                    let mut existing_metadata = crate::exif_processing::load_sidecar(&sidecar_path);
                    crate::camera_crop::start_edit(
                        &mut existing_metadata.adjustments,
                        &source_path,
                    )?;
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
                increment_thumbnail_progress(&app_handle.state::<ThumbnailState>(), &app_handle);
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
        keywords: Vec::new(),
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
    /// A keyword batch: what each photograph that saved gained and lost.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub keywords: Vec<KeywordChange>,
}

/// What a keyword write did to one photograph's keywords: the stored tags it
/// gained and lost (colour labels are not keywords). Undo removes `added` and
/// puts `removed` back; Redo the other way (`restore_keywords`), so a keyword
/// the photograph gained meanwhile stays.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct KeywordChange {
    pub path: String,
    pub added: Vec<String>,
    pub removed: Vec<String>,
}

impl KeywordChange {
    /// The change from the tags `before` to those `after` (None: no tags).
    pub fn between(path: &str, before: Option<&[String]>, after: Option<&[String]>) -> Self {
        let keywords = |tags: Option<&[String]>| -> Vec<String> {
            tags.unwrap_or_default()
                .iter()
                .filter(|tag| !rawtonic_metadata::labels::is_editor_tag(tag))
                .cloned()
                .collect()
        };
        let (before, after) = (keywords(before), keywords(after));
        Self {
            path: path.to_string(),
            added: after
                .iter()
                .filter(|tag| !before.contains(tag))
                .cloned()
                .collect(),
            removed: before
                .iter()
                .filter(|tag| !after.contains(tag))
                .cloned()
                .collect(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty()
    }
}

impl SaveFailure {
    /// `Ok` when nothing failed; otherwise the failed paths (as given) and a
    /// message naming the first few with their errors.
    pub fn check(total: usize, mut failures: Vec<(String, String)>) -> Result<(), SaveFailure> {
        if failures.is_empty() {
            return Ok(());
        }
        failures.sort();
        let listed = listed_errors(&failures);
        let what = if total == 1 {
            "the photo".to_string()
        } else {
            format!("{} of {total} photos", failures.len())
        };
        Err(SaveFailure {
            message: format!("Could not save {what}: {listed}"),
            failed: failures.into_iter().map(|(path, _)| path).collect(),
            culling: Vec::new(),
            keywords: Vec::new(),
        })
    }
}

/// Writes `metadata` as the editor sidecar of the virtual path `path`.
fn save_sidecar(path: &str, metadata: &ImageMetadata) -> Result<(), String> {
    write_sidecar(path, metadata).map_err(|e| e.to_string())
}

/// `save_sidecar` with the write's error as it is (`saved_in_library`).
fn write_sidecar(path: &str, metadata: &ImageMetadata) -> std::io::Result<()> {
    let (source_path, sidecar_path) = parse_virtual_path(path);
    let mut metadata = metadata.clone();
    crate::exif_processing::keep_own_exif(&mut metadata, &source_path);
    let json_string = serde_json::to_string_pretty(&metadata).map_err(std::io::Error::other)?;
    crate::session_bridge::write_editor_sidecar(&sidecar_path, json_string)
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
    let job = (
        BatchKind::Color {
            color: color.clone(),
        },
        "set_color_label_for_paths",
        serde_json::json!({ "color": color, "directory": directory }),
    );
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
        Some(job),
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
    let job = (
        BatchKind::Rating { rating },
        "set_rating_for_paths",
        serde_json::json!({ "rating": rating, "directory": directory }),
    );
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
        Some(job),
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
    resolve_culling_targets_with(values, directory, |owner| {
        rawtonic_catalog::Library::open(owner).map_err(|e| e.to_string())
    })
}

fn resolve_culling_targets_with(
    values: Vec<CullingWrite>,
    directory: Option<&str>,
    mut open: impl FnMut(&Path) -> Result<rawtonic_catalog::Library, String>,
) -> (Vec<CullingWrite>, Vec<(String, String)>) {
    let mut libraries = HashMap::new();
    partition_culling_targets(values, |write| {
        if let Some(id) = &write.asset_id {
            let owner = write
                .directory
                .as_deref()
                .or(directory)
                .ok_or_else(|| "Undo needs its original library".to_string())?;
            let lib = libraries
                .entry(owner.to_string())
                .or_insert_with(|| open(Path::new(owner)))
                .as_ref()
                .map_err(Clone::clone)?;
            let asset = lib.asset(id).map_err(|e| e.to_string())?;
            let photo = PhotoRef::parse(&write.path);
            let copy = photo.copy_id();
            if let Some(copy) = copy {
                lib.version_for(id, Some(copy))
                    .map_err(|e| e.to_string())?
                    .ok_or_else(|| "This virtual copy no longer exists".to_string())?;
            }
            write.path = PhotoRef::of_path(&asset.path, copy).format();
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
    prepare_culling_targets_with(values, selected, |owner| {
        rawtonic_catalog::Library::open(owner).map_err(|e| e.to_string())
    })
}

fn prepare_culling_targets_with(
    values: Vec<CullingWrite>,
    selected: Option<&str>,
    mut open: impl FnMut(&Path) -> Result<rawtonic_catalog::Library, String>,
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
            let lib = libraries
                .entry(owner.clone())
                .or_insert_with(|| open(&owner))
                .as_ref()
                .map_err(Clone::clone)?;
            write.asset_id = lib
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

/// `job`: the batch recorded before the first photograph is written
/// (`crate::batch_journal`), with its command and arguments.
async fn culling_worker(
    values: Vec<CullingWrite>,
    directory: Option<String>,
    app_handle: AppHandle,
    job: Option<(BatchKind, &'static str, Value)>,
) -> Result<Vec<CullingChange>, SaveFailure> {
    let paths: Vec<_> = values.iter().map(|write| write.path.clone()).collect();
    let recorded = paths.clone();
    super::blocking(move || {
        let _job = job.and_then(|(batch, command, args)| {
            batch_journal::begin(
                &app_handle,
                batch,
                command,
                args,
                directory.as_deref().map(Path::new),
                &recorded,
                batch_journal::current,
            )
        });
        write_culling(values, directory, &app_handle, Vec::new())
    })
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
            Rating::try_from(*rating).map_err(|e| e.to_string())?;
            CullingValue::Rating(std::mem::replace(&mut metadata.rating, *rating))
        }
        CullingValue::Color(color) => {
            let mut tags = metadata.tags.take().unwrap_or_default();
            let before = tags
                .iter()
                .find_map(|tag| tag.strip_prefix(EDITOR_TAG_PREFIX).map(str::to_string));
            tags.retain(|tag| !tag.starts_with(EDITOR_TAG_PREFIX));
            if let Some(color) = color.as_ref().filter(|color| !color.is_empty()) {
                tags.push(format!("{EDITOR_TAG_PREFIX}{color}"));
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
    let settings = crate::state::settings::current(&app_handle);
    let enable_xmp_sync = settings.enable_xmp_sync.unwrap_or(false);

    let (source_path, sidecar_path) = parse_virtual_path(&path);
    let mut metadata = load_sidecar_with_xmp(&source_path, &sidecar_path, enable_xmp_sync);

    // For the editor only (not written): its history carries it (see
    // LensLookup::Keep).
    stamp_loaded_lens_profile(&mut metadata.adjustments);
    // A camera TIFF's edit made on its embedded thumbnail, moved to the
    // full frame (`recipe_frame`; saved with the next edit).
    rawtonic_engine::recipe_frame::conform(&mut metadata.adjustments, &source_path);
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
    fn reset_follows_the_develop_defaults_and_reset_to_neutral_records_none() {
        let edited = serde_json::json!({"exposure": 1.0, "sourceFrameVersion": 1});
        let reset = reset_recipe(&edited, false);
        assert_eq!(reset, serde_json::json!({"sourceFrameVersion": 1}));
        assert!(rawtonic_engine::develop_defaults::follows_defaults(&reset));
        let neutral = reset_recipe(&edited, true);
        assert_eq!(
            neutral,
            serde_json::json!({"sourceFrameVersion": 1, "developDefaults": []})
        );
        assert!(rawtonic_engine::image_processing::is_new_recipe(&neutral));
        assert!(!rawtonic_engine::develop_defaults::follows_defaults(
            &neutral
        ));
        // A Reset of a neutral reset follows the defaults again.
        assert!(rawtonic_engine::develop_defaults::follows_defaults(
            &reset_recipe(&neutral, false)
        ));
        assert_eq!(
            reset_recipe(&serde_json::Value::Null, true),
            serde_json::json!({"developDefaults": []})
        );
    }

    #[test]
    fn culling_batches_open_each_unavailable_owner_only_once() {
        let temp = tempfile::tempdir().unwrap();
        let library = rawtonic_catalog::Library::create(
            temp.path(),
            "Owner",
            rawtonic_catalog::Mode::Session,
        )
        .unwrap();
        let owner = library.directory.to_string_lossy().into_owned();
        let paths: Vec<_> = (0..3)
            .map(|i| {
                library
                    .directory
                    .join(format!("Capture/{i}.jpg"))
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        drop(library);
        for resolving in [false, true] {
            let mut calls = 0;
            let open = |_: &Path| {
                calls += 1;
                Err("owner unavailable".to_string())
            };
            let values = paths
                .iter()
                .map(|path| CullingWrite {
                    path: path.clone(),
                    value: CullingValue::Rating(3),
                    asset_id: resolving.then(|| "asset-id".into()),
                    directory: Some(owner.clone()),
                })
                .collect();
            let (ready, failures) = if resolving {
                resolve_culling_targets_with(values, Some(&owner), open)
            } else {
                prepare_culling_targets_with(values, Some(&owner), open)
            };
            assert!(ready.is_empty());
            assert_eq!(
                failures,
                paths
                    .iter()
                    .map(|path| (path.clone(), "owner unavailable".into()))
                    .collect::<Vec<_>>()
            );
            assert_eq!(calls, 1, "resolving={resolving}");
        }
    }

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
        let (first, edits) = edit_sidecars_recorded(&paths, None, edited(0.5));
        assert!(edits.failures.is_empty(), "{edits:?}");
        assert_eq!(first.len(), 2);
        let (changes, edits) = edit_sidecars_recorded(&paths, None, edited(1.5));
        assert_eq!(edits.failures.len(), 1);
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
        let (restored, edits) = edit_sidecars_recorded(&paths[..1], None, |_, _, sidecar_path| {
            let mut metadata = crate::exif_processing::load_sidecar(sidecar_path);
            metadata.adjustments = change.before.clone();
            Ok(metadata)
        });
        assert!(edits.failures.is_empty());
        assert_eq!(restored[0].after, change.before);
        let (_, sidecar_path) = parse_virtual_path(&paths[0]);
        assert_eq!(
            crate::exif_processing::load_sidecar(&sidecar_path).adjustments,
            change.before
        );
    }

    /// A batch edit (a Library reset or paste on a closed photo) in a
    /// library whose photo folder is write-protected: the library commits
    /// it, the sidecar beside the photo cannot be written. The edit is
    /// saved, so it keeps its undo step and is reported as saved with the
    /// sidecar unwritten, not as a failure; the original is untouched.
    #[cfg(unix)]
    #[test]
    fn a_batch_edit_saved_in_the_library_keeps_its_undo_when_the_folder_is_write_protected() {
        use rawtonic_catalog::{Library, Mode};
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let lib = Library::create(temp.path(), "Write-protected", Mode::Session).unwrap();
        let capture = temp.path().join("Capture");
        let source = capture.join("a.jpg");
        fs::write(&source, b"synthetic original").unwrap();
        let path = source.to_string_lossy().into_owned();
        let paths = vec![path.clone()];
        let edited = |exposure: f64| {
            move |_: &str, _: &Path, sidecar: &Path| {
                let mut metadata = crate::exif_processing::load_sidecar(sidecar);
                metadata.adjustments = serde_json::json!({"exposure": exposure});
                Ok(metadata)
            }
        };
        let (_, edits) = edit_sidecars_recorded(&paths, None, edited(0.5));
        assert!(
            edits.failures.is_empty() && edits.unwritten.is_empty(),
            "{edits:?}"
        );
        let (_, sidecar) = parse_virtual_path(&path);
        let sidecar_before = fs::read(&sidecar).unwrap();

        fs::set_permissions(&capture, fs::Permissions::from_mode(0o555)).unwrap();
        let (changes, edits) = edit_sidecars_recorded(&paths, None, edited(1.5));
        fs::set_permissions(&capture, fs::Permissions::from_mode(0o755)).unwrap();

        assert!(edits.failures.is_empty(), "{edits:?}");
        assert_eq!(edits.unwritten.len(), 1, "{edits:?}");
        assert_eq!(changes.len(), 1, "its undo step");
        assert_eq!(changes[0].before["exposure"], 0.5);
        assert_eq!(changes[0].after["exposure"], 1.5);
        let asset = lib.asset_at(&source).unwrap().unwrap();
        assert_eq!(
            lib.editor_state(&asset.id).unwrap().unwrap()["adjustments"]["exposure"],
            1.5
        );
        let note = edits.unwritten_note(1).unwrap();
        assert!(
            note.starts_with(
                "Saved in the library; the sidecar file next to the photo could not be written: "
            ),
            "{note}"
        );
        assert_eq!(fs::read(&source).unwrap(), b"synthetic original");
        assert_eq!(fs::read(&sidecar).unwrap(), sidecar_before);
    }

    /// A virtual copy in a write-protected folder: while the photograph is
    /// there its recipe is read from its sidecar file (`read_editor_sidecar`),
    /// so an edit the file did not get is not saved, whatever the library
    /// holds: it fails and records no undo step; what export reads stays
    /// the recipe before.
    #[cfg(unix)]
    #[test]
    fn a_batch_edit_of_a_virtual_copy_the_folder_refuses_is_not_saved() {
        use rawtonic_catalog::{Library, Mode};
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let lib = Library::create(temp.path(), "Write-protected copy", Mode::Session).unwrap();
        let capture = temp.path().join("Capture");
        let source = capture.join("a.jpg");
        fs::write(&source, b"synthetic original").unwrap();
        let copy = format!("{}?vc=c0ffee", source.to_string_lossy());
        let paths = vec![copy.clone()];
        let edited = |exposure: f64| {
            move |_: &str, _: &Path, sidecar: &Path| {
                let mut metadata = crate::exif_processing::load_sidecar(sidecar);
                metadata.adjustments = serde_json::json!({"exposure": exposure});
                Ok(metadata)
            }
        };
        let (_, edits) = edit_sidecars_recorded(&paths, None, edited(0.5));
        assert!(
            edits.failures.is_empty() && edits.unwritten.is_empty(),
            "{edits:?}"
        );

        fs::set_permissions(&capture, fs::Permissions::from_mode(0o555)).unwrap();
        let (changes, edits) = edit_sidecars_recorded(&paths, None, edited(1.5));
        fs::set_permissions(&capture, fs::Permissions::from_mode(0o755)).unwrap();

        assert_eq!(edits.failures.len(), 1, "{edits:?}");
        assert!(edits.unwritten.is_empty(), "{edits:?}");
        assert!(changes.is_empty(), "no undo step for an edit not saved");
        let (_, sidecar) = parse_virtual_path(&copy);
        assert_eq!(
            crate::exif_processing::load_sidecar(&sidecar).adjustments["exposure"],
            0.5
        );
        // The library holds the recipe before too: a relink writes the
        // copy's sidecar from it (`rederive_copies`).
        let asset = lib.asset_at(&source).unwrap().unwrap();
        assert_eq!(
            lib.copy_state(&asset.id, "c0ffee").unwrap().unwrap()["adjustments"]["exposure"],
            0.5
        );
        assert_eq!(fs::read(&source).unwrap(), b"synthetic original");
    }

    /// A virtual copy in a folder that takes writes but cannot be read (so
    /// it cannot be synced after the replace): the sidecar holds the edit,
    /// with its undo step, and the library holds the same recipe. Relinking
    /// the photo recreates the copy sidecar for the export pin.
    #[cfg(unix)]
    #[test]
    fn a_virtual_copy_written_into_a_folder_that_cannot_be_read_is_saved() {
        let temp = tempfile::tempdir().unwrap();
        let (_, pinned) = relink_copy_saved_in_unreadable_folder(temp.path());
        assert_eq!(
            pinned.adjustments,
            serde_json::json!({"exposure": 1.5, "saturation": -60}),
            "the relinked copy's export pin"
        );
    }

    #[cfg(unix)]
    fn relink_copy_saved_in_unreadable_folder(
        directory: &Path,
    ) -> (std::path::PathBuf, crate::export_processing::PinnedPhoto) {
        use rawtonic_catalog::{Library, Mode};
        use std::os::unix::fs::PermissionsExt;
        let lib = Library::create(directory, "Unreadable folder", Mode::Session).unwrap();
        let capture = directory.join("Capture");
        let source = capture.join("a.png");
        let photo = image::RgbImage::from_pixel(32, 24, image::Rgb([72, 96, 120]));
        let mut encoded = std::io::Cursor::new(Vec::new());
        photo
            .write_to(&mut encoded, image::ImageFormat::Png)
            .unwrap();
        let original = encoded.into_inner();
        rawtonic_fs::atomic_write(&source, &original).unwrap();
        let copy = format!("{}?vc=c0ffee", source.to_string_lossy());
        let paths = vec![copy.clone()];
        let edited = |exposure: f64, saturation: i32| {
            move |_: &str, _: &Path, sidecar: &Path| {
                let mut metadata = crate::exif_processing::load_sidecar(sidecar);
                metadata.adjustments =
                    serde_json::json!({"exposure": exposure, "saturation": saturation});
                Ok(metadata)
            }
        };
        let (_, edits) = edit_sidecars_recorded(&paths, None, edited(0.5, 20));
        assert!(
            edits.failures.is_empty() && edits.unwritten.is_empty(),
            "{edits:?}"
        );

        fs::set_permissions(&capture, fs::Permissions::from_mode(0o300)).unwrap();
        let (changes, edits) = edit_sidecars_recorded(&paths, None, edited(1.5, -60));
        fs::set_permissions(&capture, fs::Permissions::from_mode(0o755)).unwrap();

        let (_, sidecar) = parse_virtual_path(&copy);
        let asset = lib.asset_at(&source).unwrap().unwrap();
        let file = crate::exif_processing::load_sidecar(&sidecar).adjustments;
        let library = &lib.copy_state(&asset.id, "c0ffee").unwrap().unwrap()["adjustments"];
        assert_eq!(
            &file, library,
            "the file and the library hold the same recipe"
        );
        assert!(edits.failures.is_empty(), "{edits:?}");
        assert_eq!(
            file,
            serde_json::json!({"exposure": 1.5, "saturation": -60})
        );
        assert_eq!(changes.len(), 1, "its undo step");

        // Move only the photo. Relink must recreate the copy sidecar at
        // the new location from the state saved in the catalog.
        let relocated = directory.join("Selects/relocated.png");
        rawtonic_fs::rename_no_clobber(&source, &relocated).unwrap();
        lib.verify_paths().unwrap();
        let report = lib.relink(&asset.id, &relocated, false).unwrap();
        crate::session_bridge::after_relink(&lib, &report);
        assert_eq!(fs::read(&relocated).unwrap(), original);

        let copy = format!("{}?vc=c0ffee", relocated.display());
        let mut pinned =
            crate::export_processing::pin_photos(&[copy], None, None, Some(directory)).unwrap();
        (relocated, pinned.remove(0))
    }

    #[cfg(unix)]
    #[test]
    #[ignore = "requires a GPU; run explicitly on native QA hosts"]
    fn a_relinked_virtual_copy_exports_its_saved_edit() {
        use rawtonic_export::file_export::{FileExport, FileExporter};
        use rawtonic_export::output::Publish;

        let temp = tempfile::tempdir().unwrap();
        let (source, pinned) = relink_copy_saved_in_unreadable_folder(temp.path());
        let exporter =
            FileExporter::new(&Path::new(env!("CARGO_MANIFEST_DIR")).join("lensfun_db")).unwrap();
        let export = |name: &str, recipe: Option<Value>| {
            let output = temp.path().join(name);
            exporter
                .export(&FileExport {
                    source: &source,
                    output: &output,
                    format: "png",
                    recipe,
                    settings: serde_json::from_value(serde_json::json!({
                        "jpegQuality": 90, "keepMetadata": false, "stripGps": false
                    }))
                    .unwrap(),
                    base: Default::default(),
                    catalog: None,
                    capture: None,
                    library: Some(temp.path()),
                    publish: Publish::New,
                    source_generated: false,
                })
                .unwrap();
            image::open(output).unwrap().to_rgb8()
        };

        let actual = export("pinned.png", Some(pinned.adjustments));
        let expected = export(
            "direct.png",
            Some(serde_json::json!({"exposure": 1.5, "saturation": -60})),
        );
        let master = export("master.png", None);
        assert_ne!(expected, master, "the edit must visibly change the export");
        assert_eq!(
            actual, expected,
            "the relinked copy must export its saved edit"
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

    /// Whites copied without a tone version carry the amount they render:
    /// pasted onto a version-16 photograph they keep that amount in its own
    /// response, its tone version and its layers' Whites stay as they were.
    #[test]
    fn pasted_whites_keep_their_amount_and_the_photographs_tone_version() {
        let source = Path::new("/nowhere/photo.CR3");
        let paste = |base: Value, pasted: Value| {
            pasted_into(
                base,
                &None,
                "/nowhere/photo.CR3",
                source,
                &pasted,
                None,
                None,
                None,
                None,
            )
            .unwrap()
        };
        let layer = serde_json::json!([{"id": "l", "adjustments": {"whites": 50.0}}]);
        let v16 = serde_json::json!({"toneVersion": 16, "whites": 0.0, "masks": layer});
        // Whites +50 copied from a version-17 photograph: amount 74.
        let pasted = paste(v16.clone(), serde_json::json!({"whites": 74.0}));
        assert_eq!(pasted["toneVersion"], 16);
        assert_eq!(pasted["whites"], 74.0);
        assert_eq!(pasted["masks"][0]["adjustments"]["whites"], 50.0);
        // Onto a version-17 photograph the amount is its slider's +50.
        let v17 = serde_json::json!({"toneVersion": 17, "whites": 0.0});
        assert_eq!(
            paste(v17.clone(), serde_json::json!({"whites": 74.0}))["whites"],
            50.0
        );
        // Settings that name their tone version mean its values: on an
        // edited photograph (which renders with its tone version also where
        // no slider is moved) the amount they render, in its response.
        let named = paste(v16, serde_json::json!({"whites": 50.0, "toneVersion": 17}));
        assert_eq!(
            (named["whites"].as_f64(), named["toneVersion"].as_u64()),
            (Some(74.0), Some(16))
        );
    }

    /// A pasted tool's version key goes along only when nothing the paste
    /// leaves on the photograph renders with it: a layer using Structure
    /// keeps the photograph's travel (and its own value); without one the
    /// paste takes the source's (`pasted_versions`).
    #[test]
    fn a_pasted_structure_keeps_the_travel_a_layer_of_the_photograph_uses() {
        let source = Path::new("/nowhere/photo.CR3");
        let paste = |base: Value| {
            pasted_into(
                base,
                &None,
                "/nowhere/photo.CR3",
                source,
                &serde_json::json!({"structure": 50.0, "structureVersion": 3}),
                None,
                None,
                None,
                None,
            )
            .unwrap()
        };
        let layer = serde_json::json!([{"id": "l", "adjustments": {"structure": 40.0}}]);
        let kept = paste(serde_json::json!({"exposure": 0.5, "masks": layer}));
        // Stated as the photograph renders it (no key: version 1).
        assert_eq!(kept["structureVersion"], 1, "{kept}");
        assert_eq!(
            kept["structure"].as_f64(),
            Some(rawtonic_engine::image_processing::structure_amount_v2(50.0))
        );
        assert_eq!(kept["masks"][0]["adjustments"]["structure"], 40.0);
        let taken = paste(serde_json::json!({"exposure": 0.5}));
        assert_eq!(taken["structureVersion"], 3);
        assert_eq!(taken["structure"], 50.0);
    }

    /// Editing a photograph without an edit (or after a Library Reset) from
    /// the batch tools starts it as a new edit: the camera's crop of its
    /// frame with the new edit's versions, as the editor's first edit does.
    #[test]
    fn a_batch_edit_starts_a_new_recipe_with_the_cameras_crop_and_versions() {
        use rawtonic_engine::source_facts::{SourceFacts, VERSION, forget, register};
        use rawtonic_engine::source_frame::KEY;
        let path = "/offline/batch-camera-crop.CR3";
        register(
            path,
            SourceFacts {
                version: VERSION,
                frames: Some(vec![0, 1]),
                camera_crop: Some(vec![0, 376, 4000, 2248]),
                ..Default::default()
            },
        );
        let paste = |base: Value| {
            pasted_into(
                base,
                &None,
                path,
                Path::new(path),
                &serde_json::json!({"exposure": 1.0}),
                None,
                None,
                None,
                None,
            )
            .unwrap()
        };
        for new in [
            Value::Null,
            serde_json::json!({}),
            serde_json::json!({KEY: 1}),
        ] {
            // A Reset's frame stays; a never-edited photo takes the newest.
            let frame = new
                .get(KEY)
                .cloned()
                .unwrap_or(serde_json::json!(rawtonic_engine::source_frame::VERSION));
            let recipe = paste(new);
            assert_eq!(recipe["exposure"], 1.0);
            assert_eq!(recipe["crop"]["y"], 376);
            assert_eq!(recipe["aspectRatio"], 4000.0 / 2248.0);
            assert_eq!(recipe[KEY], frame);
            assert!(recipe["renderVersion"].is_u64());
        }
        // A pasted crop replaces it; an edited photograph keeps its own.
        let own = serde_json::json!({"exposure": 0.2, "renderVersion": 4});
        assert!(paste(own).get("crop").is_none());
        let mut values = Value::Null;
        crate::camera_crop::apply_default(&mut values, Path::new(path));
        assert_eq!(values["crop"]["height"], 2248);
        let before = values.clone();
        crate::camera_crop::apply_default(&mut values, Path::new(path));
        assert_eq!(values, before, "applied once");
        forget(path);
    }

    #[test]
    fn shared_camera_crop_ignores_obsolete_registered_framing_when_the_raw_is_local() {
        use rawtonic_engine::source_facts::{SourceFacts, VERSION, forget, register};

        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("camera.dng");
        fs::write(
            &source,
            include_bytes!("../../../third_party/rawler/src/decoders/dng_lossy_jpeg_test.dng"),
        )
        .unwrap();
        let path = source.to_str().unwrap();
        register(
            path,
            SourceFacts {
                version: VERSION - 1,
                camera_crop: Some(vec![0, 376, 4000, 2248]),
                ..Default::default()
            },
        );
        let recipe = pasted_into(
            Value::Null,
            &None,
            path,
            &source,
            &serde_json::json!({"exposure": 1}),
            None,
            None,
            None,
            None,
        )
        .unwrap();
        forget(path);
        // DNG develops its own default frame. A registered preview crop
        // from an earlier original at this path must not be saved.
        assert!(recipe["crop"].is_null(), "{recipe}");
        assert_eq!(recipe["exposure"], 1);
    }

    /// A batch edit of a photograph never opened (Edit all selected, paste,
    /// sync, preset, style) gives it the same edit the photograph opened
    /// first gets: every tool response of the editor's new edit, so Contrast
    /// −100 renders with today's tone response on both, not the legacy one
    /// a recipe without `toneVersion` keeps (notes: benchmark-results/
    /// uxv-slider-use-test-2026-11-10 FINDINGS.md "Fresh unopened batch
    /// target"). Rendered alike: the uniforms of both are identical.
    #[test]
    fn a_batch_edit_of_a_never_opened_photo_matches_the_one_opened_first() {
        let path = "/offline/fbv-never-opened.NEF";
        let source = Path::new(path);
        let opened: Value = serde_json::from_str(include_str!(
            "../../../crates/engine/tests/fixtures/new-edit-recipe.raw.json"
        ))
        .unwrap();
        let edit = serde_json::json!({"contrast": -100.0});
        let render = |mut recipe: Value| {
            rawtonic_export::normalize::normalize(
                &mut recipe,
                &rawtonic_export::normalize::RecipeSource::file(source),
                rawtonic_export::normalize::Bitmaps::None,
                rawtonic_export::normalize::Defaults::Render,
            );
            let prepared = rawtonic_engine::image_processing::get_all_adjustments_from_json(
                &recipe, true, None,
            );
            bytemuck::bytes_of(&*prepared.uniforms).to_vec()
        };
        let pastes = [
            (
                "edit all selected",
                Some(PasteFrom {
                    source: "/offline/opened.NEF".into(),
                    replace_layers: true,
                    new_edit: None,
                    as_chosen: false,
                }),
                None,
            ),
            (
                "paste or sync",
                Some(PasteFrom {
                    source: "/offline/opened.NEF".into(),
                    replace_layers: false,
                    new_edit: None,
                    as_chosen: false,
                }),
                None,
            ),
            ("preset", None, None),
            (
                "style",
                None,
                Some(StyleOp {
                    id: "s".into(),
                    remove: false,
                }),
            ),
        ];
        for (name, paste, style) in &pastes {
            let paste_onto = |base: Value| {
                pasted_into(
                    base,
                    &None,
                    path,
                    source,
                    &edit,
                    paste.as_ref(),
                    style.as_ref(),
                    None,
                    None,
                )
                .unwrap()
            };
            let a = paste_onto(opened.clone());
            for never_opened in [Value::Null, serde_json::json!({})] {
                let b = paste_onto(never_opened);
                assert_eq!(b["contrast"], -100.0, "{name}");
                for (key, value) in opened.as_object().unwrap() {
                    // The camera's lens versions need its data (none offline).
                    if key.ends_with("Version")
                        && key != "cameraCaVersion"
                        && key != "cameraGeometryVersion"
                    {
                        assert_eq!(b.get(key), Some(value), "{name}: {key} in {b}");
                    }
                }
                assert!(render(a.clone()) == render(b), "{name}: renders differ");
            }
        }
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
    fn rating_undo_preserves_reject_without_a_flag_receipt() {
        use rawtonic_catalog::{Library, Mode};
        let temp = tempfile::tempdir().unwrap();
        let root = rawtonic_fs::canonicalize(temp.path()).unwrap();
        let lib = Library::create(&root, "Rating undo", Mode::Session).unwrap();
        let source = root.join("Capture/source.nef");
        fs::write(&source, b"synthetic original").unwrap();
        let asset = lib.add_asset(&source).unwrap();
        let path = source.to_string_lossy().into_owned();
        crate::session_bridge::write_flag_values(root.to_str().unwrap(), vec![(path.clone(), -1)])
            .unwrap();
        // Count actual metadata writes, including same-value flag writes.
        let audit = rusqlite::Connection::open(root.join("session.sqlite")).unwrap();
        audit
            .execute_batch(
                "CREATE TABLE culling_write_audit (rating INTEGER);
            CREATE TRIGGER audit_culling_write AFTER UPDATE OF data ON metadata
            BEGIN INSERT INTO culling_write_audit VALUES (NEW.rating); END;",
            )
            .unwrap();
        let rate = write_culling_batch(
            vec![CullingWrite {
                path,
                value: CullingValue::Rating(3),
                asset_id: None,
                directory: None,
            }],
            Some(root.to_string_lossy().into_owned()),
            None,
            Vec::new(),
        )
        .unwrap();
        assert_eq!(rate.len(), 1);
        assert_eq!(rate[0].before, CullingValue::Rating(0));
        assert_eq!(lib.metadata(&asset.id).unwrap().rating, 3);
        let undo = write_culling_batch(
            vec![CullingWrite {
                path: rate[0].path.clone(),
                value: rate[0].before.clone(),
                asset_id: rate[0].asset_id.clone(),
                directory: rate[0].directory.clone(),
            }],
            Some(root.to_string_lossy().into_owned()),
            None,
            Vec::new(),
        )
        .unwrap();
        assert_eq!(undo.len(), 1);
        assert_eq!(undo[0].after, CullingValue::Rating(0));
        assert_eq!(lib.metadata(&asset.id).unwrap().rating, 0);
        assert_eq!(lib.flags().unwrap().get(&source), Some(&-1));
        let writes: Vec<u8> = audit
            .prepare("SELECT rating FROM culling_write_audit")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            writes,
            vec![3, 0],
            "one write per rating change, no flag resubmission"
        );
    }

    #[test]
    fn culling_undo_follows_asset_identity_after_a_move_and_refuses_missing_unmanaged_files() {
        use rawtonic_catalog::{Library, Mode};
        let temp = tempfile::tempdir().unwrap();
        // The library and the resolved paths are canonical: on macOS the
        // temporary folder /var/… is /private/var/….
        let root = temp.path().canonical().unwrap();
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

    /// A sidecar the typed reader cannot take whole (no `adjustments`, or a
    /// field of another type, as a hand edit or another program may leave)
    /// keeps its rating, keywords and recipe through a rating change, a
    /// colour label and a batch recipe edit: each writes only its own part.
    #[test]
    fn writes_keep_what_an_untyped_sidecar_holds() {
        let temp = tempfile::tempdir().unwrap();
        let read = |sidecar: &Path| -> Value {
            serde_json::from_slice(&fs::read(sidecar).unwrap()).unwrap()
        };
        let photo = |name: &str, stored: Value| {
            let source = temp.path().join(name);
            fs::write(&source, b"synthetic original").unwrap();
            let sidecar = temp.path().join(format!("{name}.rrdata"));
            fs::write(&sidecar, stored.to_string()).unwrap();
            (source.to_string_lossy().into_owned(), sidecar)
        };
        let culling = |path: &str, value: CullingValue| {
            write_culling_sidecar(
                CullingWrite {
                    path: path.to_string(),
                    value,
                    asset_id: None,
                    directory: None,
                },
                None,
            )
            .unwrap();
        };

        // No `adjustments` (never edited): a colour label keeps rating and keywords.
        let (path, sidecar) = photo(
            "no-recipe.jpg",
            serde_json::json!({"version": 1, "rating": 4, "tags": ["user:a"]}),
        );
        culling(&path, CullingValue::Color(Some("red".into())));
        let written = read(&sidecar);
        assert_eq!(written["rating"], 4);
        assert_eq!(written["tags"], serde_json::json!(["user:a", "color:red"]));
        assert_eq!(written["adjustments"], Value::Null);

        // A field of another type (an EXIF number, a keyword that is no
        // string): a rating change keeps the recipe and the keywords.
        let (path, sidecar) = photo(
            "mistyped.jpg",
            serde_json::json!({
                "version": 1, "rating": 4, "tags": ["user:a", 7],
                "adjustments": {"exposure": 1.5}, "exif": {"ISO": 100, "Make": "Maker"},
            }),
        );
        culling(&path, CullingValue::Rating(2));
        let written = read(&sidecar);
        assert_eq!(written["rating"], 2);
        assert_eq!(written["tags"], serde_json::json!(["user:a"]));
        assert_eq!(written["adjustments"], serde_json::json!({"exposure": 1.5}));
        assert_eq!(written["exif"], serde_json::json!({"Make": "Maker"}));

        // A batch recipe edit (paste, preset, Auto: `edit_sidecars`) keeps
        // rating and keywords.
        let (path, sidecar) = photo(
            "batch.jpg",
            serde_json::json!({"version": 1, "rating": "5", "tags": ["user:b"], "adjustments": {}}),
        );
        let failures = edit_sidecars(std::slice::from_ref(&path), None, |_, _, sidecar| {
            let mut metadata = crate::exif_processing::load_sidecar(sidecar);
            metadata.adjustments = serde_json::json!({"exposure": 0.5});
            Ok(metadata)
        })
        .failures;
        assert!(failures.is_empty(), "{failures:?}");
        let written = read(&sidecar);
        assert_eq!(written["tags"], serde_json::json!(["user:b"]));
        assert_eq!(written["adjustments"], serde_json::json!({"exposure": 0.5}));
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

    /// A camera TIFF's edit made on its thumbnail is moved to the full frame
    /// when a batch edit changes it: Processing Versions pasted from a RAW
    /// would otherwise make it read as made on the full frame (TIFR2 open
    /// case 2). A refused edit writes nothing.
    #[test]
    fn a_batch_edit_moves_a_thumbnail_edit_first() {
        use rawtonic_engine::recipe_frame;
        let (_temp, paths) = session_with_photos(1);
        let (source, sidecar) = parse_virtual_path(&paths[0]);
        recipe_frame::pretend_frames(
            &source,
            recipe_frame::TiffFrames {
                thumbnail: (288, 192),
                full: (2496, 1662),
            },
        );
        let legacy = serde_json::json!({"version": 1, "rating": 0, "tags": [], "adjustments": {
            "exposure": 0.1, "crop": {"x": 10.0, "y": 10.0, "width": 100.0, "height": 80.0}}});
        crate::session_bridge::write_editor_sidecar(&sidecar, serde_json::to_vec(&legacy).unwrap())
            .unwrap();
        let failed = edit_sidecars(&paths, None, |_, _, sidecar| {
            let mut metadata = crate::exif_processing::load_sidecar(sidecar);
            metadata.adjustments["fringeVersion"] = serde_json::json!(6);
            Ok(metadata)
        })
        .failures;
        assert!(failed.is_empty(), "{failed:?}");
        let saved = crate::exif_processing::load_sidecar(&sidecar).adjustments;
        assert_eq!(saved["fringeVersion"], 6);
        assert_eq!(recipe_frame::stamped(&saved), Some((2496.0, 1662.0)));
        let width = saved["crop"]["width"].as_f64().unwrap();
        assert!((width - 100.0 * 2496.0 / 288.0).abs() < 1e-6, "{width}");
        // A refused edit leaves the sidecar as it was, not moved.
        crate::session_bridge::write_editor_sidecar(&sidecar, serde_json::to_vec(&legacy).unwrap())
            .unwrap();
        let failed =
            edit_sidecars(&paths, None, |_, _, _| Err("too many layers".to_string())).failures;
        assert_eq!(failed.len(), 1);
        let kept = crate::exif_processing::load_sidecar(&sidecar).adjustments;
        assert_eq!(kept["crop"], legacy["adjustments"]["crop"]);
        assert_eq!(recipe_frame::stamped(&kept), None);
    }

    /// G50S decision 4: the Lens group of a photo with an imported polynomial
    /// distortion (Capture One's Generic profile) is copied, pasted and
    /// synced with its parameters, over the destination's lens profile; and a
    /// lens-off paste clears it.
    #[test]
    fn a_pasted_imported_polynomial_distortion_replaces_the_destinations_lens_and_is_kept() {
        let (_temp, paths) = session_with_photos(2);
        let polynomial = serde_json::json!({
            "model": 0, "k1": -0.036, "k2": 0.0, "k3": 0.0, "zoom": 1.0, "exact_zoom": true
        });
        let paste = PasteFrom {
            source: paths[0].clone(),
            replace_layers: false,
            as_chosen: false,
            new_edit: None,
        };
        let run = |pasted: &Value| {
            let failed = edit_sidecars(&paths[1..], None, |path, source, sidecar| {
                pasted_metadata(
                    path,
                    source,
                    sidecar,
                    pasted,
                    Some(&paste),
                    None,
                    None,
                    None,
                )
            })
            .failures;
            assert!(failed.is_empty(), "{failed:?}");
            let (_, sidecar) = parse_virtual_path(&paths[1]);
            crate::exif_processing::load_sidecar(&sidecar).adjustments
        };
        let pasted = serde_json::json!({
            "lensCorrectionMode": "manual", "lensMaker": null, "lensModel": null,
            "lensDistortionAmount": 60, "lensDistortionParams": polynomial,
        });
        let after = run(&pasted);
        assert_eq!(after["lensDistortionParams"], polynomial);
        assert_eq!(after["lensDistortionAmount"], 60);
        assert!(after.get("lensMaker").is_none());
        // Lens correction off pasted over it: the polynomial goes.
        let off = serde_json::json!({
            "lensCorrectionMode": "manual", "lensMaker": null, "lensModel": null,
            "lensDistortionParams": null,
        });
        let cleared = run(&off);
        assert!(
            cleared
                .get("lensDistortionParams")
                .is_none_or(Value::is_null)
        );
    }

    #[test]
    fn synced_layers_join_each_photographs_own_with_their_own_detection() {
        let (_temp, paths) = session_with_photos(2);
        let pasted =
            serde_json::json!({"contrast": 20, "masks": [sky_layer("synced", "data:source")]});
        let paste = PasteFrom {
            source: paths[0].clone(),
            replace_layers: false,
            as_chosen: false,
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
        })
        .failures;
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
        )
        .failures;
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
        // The editor's map adds what the engine's new edit leaves to the
        // camera's data (`image_processing::start_new_edit`).
        let mut raw = serde_json::Map::new();
        raw.insert("toneVersion".into(), serde_json::json!(2));
        raw.insert("cameraGeometryVersion".into(), serde_json::json!(10));
        let paste = PasteFrom {
            source: paths[0].clone(),
            replace_layers: false,
            as_chosen: true,
            new_edit: Some(NewEditResponses {
                raw,
                other: serde_json::Map::new(),
            }),
        };
        // Blacks −100 from a version-17 RAW, carried as its amount.
        let pasted = serde_json::json!({"shadows": 80, "blacks": -100.0});
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
        })
        .failures;
        assert!(failed.is_empty(), "{failed:?}");
        let new = crate::exif_processing::load_sidecar(&sidecar).adjustments;
        assert_eq!(
            new["toneVersion"],
            rawtonic_engine::image_processing::TONE_VERSION,
            "a new edit's versions"
        );
        assert_eq!(new["cameraGeometryVersion"], 10);
        // In the new edit's tone response, as the open photograph pastes it
        // (useEditorActions.test.ts: "expresses carried Blacks … Reset photo").
        let blacks = new["blacks"].as_f64().unwrap();
        assert!((blacks - -71.551_724).abs() < 1e-5, "{blacks}");
        // Carried Shadows are amounts too: version 20's +80 is 21's +71.2.
        let shadows = new["shadows"].as_f64().unwrap();
        assert!((shadows - 71.211_231).abs() < 1e-5, "{shadows}");
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
            as_chosen: false,
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
        })
        .failures;
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
            as_chosen: false,
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
        })
        .failures;
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
        })
        .failures;
        assert!(failed.is_empty(), "{failed:?}");
    }

    /// A JPEG whose only EXIF is Make "Cloud", for the EXIF reads of a photo.
    fn jpeg_with_make() -> Vec<u8> {
        let mut tiff = b"II*\0\x08\0\0\0\x01\0".to_vec();
        tiff.extend([0x0f, 0x01, 2, 0, 6, 0, 0, 0, 0x1a, 0, 0, 0, 0, 0, 0, 0]);
        tiff.extend(b"Cloud\0");
        let mut app1 = b"Exif\0\0".to_vec();
        app1.extend(tiff);
        let mut jpeg = vec![0xff, 0xd8, 0xff, 0xe1];
        jpeg.extend(((app1.len() + 2) as u16).to_be_bytes());
        jpeg.extend(app1);
        jpeg.extend([0xff, 0xd9]);
        jpeg
    }

    /// A linked RAW (a little-endian TIFF `.dng` outside any library) with
    /// Make, Artist and Copyright in IFD0 and DateTimeOriginal in its Exif
    /// IFD, without a sidecar.
    fn linked_raw() -> (tempfile::TempDir, std::path::PathBuf) {
        let ascii = |text: &str| {
            let mut bytes = text.as_bytes().to_vec();
            bytes.push(0);
            bytes
        };
        // (tag, value) of IFD0 and of the Exif IFD; values after the IFDs.
        let ifd0 = [
            (0x010f_u16, ascii("Maker")),
            (0x013b, ascii("Camera Owner")),
            (0x8298, ascii("Studio")),
        ];
        let exif = [(0x9003_u16, ascii("2026:10:30 23:30:00"))];
        let ifd_len = |n: usize| 2 + 12 * n as u32 + 4;
        let ifd0_at = 8;
        let exif_at = ifd0_at + ifd_len(ifd0.len() + 1);
        let mut data_at = exif_at + ifd_len(exif.len());
        let mut tiff = b"II*\0".to_vec();
        tiff.extend(ifd0_at.to_le_bytes());
        let mut data: Vec<u8> = Vec::new();
        let mut ifd = |tiff: &mut Vec<u8>, entries: &[(u16, Vec<u8>)], pointer: Option<u32>| {
            let count = entries.len() + usize::from(pointer.is_some());
            tiff.extend((count as u16).to_le_bytes());
            for (tag, value) in entries {
                tiff.extend(tag.to_le_bytes());
                tiff.extend(2u16.to_le_bytes());
                tiff.extend((value.len() as u32).to_le_bytes());
                tiff.extend(data_at.to_le_bytes());
                data_at += value.len() as u32;
                data.extend(value);
            }
            if let Some(pointer) = pointer {
                tiff.extend(0x8769u16.to_le_bytes());
                tiff.extend(4u16.to_le_bytes());
                tiff.extend(1u32.to_le_bytes());
                tiff.extend(pointer.to_le_bytes());
            }
            tiff.extend(0u32.to_le_bytes());
        };
        ifd(&mut tiff, &ifd0, Some(exif_at));
        ifd(&mut tiff, &exif, None);
        tiff.extend(data);
        let temp = tempfile::tempdir().unwrap();
        let raw = temp.path().join("linked.dng");
        fs::write(&raw, tiff).unwrap();
        (temp, raw)
    }

    /// The `exif` of the sidecar file beside `raw`, as written.
    fn sidecar_exif(raw: &Path) -> Option<Value> {
        let sidecar = crate::exif_processing::get_primary_sidecar_path(raw);
        let state: Value = serde_json::from_slice(&fs::read(sidecar).unwrap()).unwrap();
        state.get("exif").cloned()
    }

    /// Each writer of an actual edit (editor save, paste, preset, auto lens,
    /// AI selection) on a linked RAW whose EXIF the app has read: the sidecar
    /// gets the edit and no EXIF map; after a user EXIF edit just that field
    /// (SCW1).
    #[test]
    fn edits_of_a_linked_raw_write_the_recipe_and_only_the_users_exif() {
        use rawtonic_engine::ai_selections::{AutoSelection, Detected, detected_parameters};
        let pending_sky = serde_json::json!({"masks": [{"id": "l", "visible": true,
            "subMasks": [{"id": "s", "type": "ai-sky", "visible": true,
                "parameters": {"maskDataBase64": null, "detectPending": true}}]}]});
        type Writer = Box<dyn Fn(&str) -> Value>;
        let writers: Vec<(&str, Writer)> = vec![
            (
                "editor save",
                Box::new(|path| {
                    write_editor_save(path, serde_json::json!({"exposure": 1.5}), Some(true))
                        .unwrap();
                    serde_json::json!({"exposure": 1.5})
                }),
            ),
            (
                "paste",
                Box::new(|path| {
                    let paste = PasteFrom {
                        source: "/elsewhere/other.dng".into(),
                        replace_layers: false,
                        as_chosen: false,
                        new_edit: None,
                    };
                    let pasted = serde_json::json!({"contrast": 20});
                    let edits =
                        edit_sidecars(&[path.to_string()], None, |path, source, sidecar| {
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
                    assert!(edits.failures.is_empty(), "{:?}", edits.failures);
                    serde_json::json!({"contrast": 20})
                }),
            ),
            (
                "preset",
                Box::new(|path| {
                    let preset = serde_json::json!({"saturation": -10});
                    let edits =
                        edit_sidecars(&[path.to_string()], None, |path, source, sidecar| {
                            pasted_metadata(path, source, sidecar, &preset, None, None, None, None)
                        });
                    assert!(edits.failures.is_empty(), "{:?}", edits.failures);
                    serde_json::json!({"saturation": -10})
                }),
            ),
            (
                "auto lens",
                Box::new(|path| {
                    let edits = edit_sidecars(&[path.to_string()], None, |_, source, sidecar| {
                        auto_lens_metadata(source, sidecar, None)
                    });
                    assert!(edits.failures.is_empty(), "{:?}", edits.failures);
                    serde_json::json!({"lensCorrectionMode": "auto"})
                }),
            ),
            (
                "AI selection",
                Box::new(move |path| {
                    let (source, _) = parse_virtual_path(path);
                    let mut recipe =
                        crate::exif_processing::load_sidecar(&parse_virtual_path(path).1);
                    recipe.adjustments = pending_sky.clone();
                    // Its pending detection is in the recipe (as saved by
                    // an editor of the same build).
                    save_sidecar(path, &recipe).unwrap();
                    let frame = crate::export_processing::detection_frame(&pending_sky, &source);
                    let found = vec![Detected {
                        layer_id: "l".into(),
                        component_id: "s".into(),
                        selection: AutoSelection::Sky,
                        parameters: detected_parameters(
                            &pending_sky["masks"][0]["subMasks"][0]["parameters"],
                            &frame,
                            "data:sky".into(),
                        ),
                    }];
                    assert!(
                        crate::export_processing::keep_detected_selections(path, &found, |_| true)
                            .unwrap()
                    );
                    serde_json::json!({"masks": [{"subMasks": [{"parameters":
                        {"maskDataBase64": "data:sky"}}]}]})
                }),
            ),
        ];
        // Whether `wanted`'s values are in `recipe` (objects and arrays in part).
        fn holds(recipe: &Value, wanted: &Value) -> bool {
            match (recipe, wanted) {
                (Value::Object(r), Value::Object(w)) => {
                    w.iter().all(|(k, v)| r.get(k).is_some_and(|r| holds(r, v)))
                }
                (Value::Array(r), Value::Array(w)) => {
                    w.iter().zip(r).all(|(w, r)| holds(r, w)) && r.len() >= w.len()
                }
                _ => recipe == wanted,
            }
        }
        for (name, write) in &writers {
            for user_edit in [false, true] {
                let (_temp, raw) = linked_raw();
                let path = raw.to_string_lossy().into_owned();
                // The app has read and shown its EXIF (cache warm).
                let shown = crate::exif_processing::read_exif_data(&path, &fs::read(&raw).unwrap());
                assert_eq!(shown["Make"], "Maker", "fixture EXIF");
                if user_edit {
                    let updates = HashMap::from([("Artist".to_string(), "Ana".to_string())]);
                    crate::file_management::update_exif_of(&path, &updates).unwrap();
                    assert_eq!(
                        sidecar_exif(&raw),
                        Some(serde_json::json!({"Artist": "Ana"})),
                        "the Metadata tool keeps only the user's field"
                    );
                }
                let wanted = write(&path);
                let sidecar = crate::exif_processing::get_primary_sidecar_path(&raw);
                let state: Value = serde_json::from_slice(&fs::read(&sidecar).unwrap()).unwrap();
                assert!(holds(&state["adjustments"], &wanted), "{name}: {state}");
                let expected = user_edit.then(|| serde_json::json!({"Artist": "Ana"}));
                assert_eq!(
                    state.get("exif").cloned(),
                    expected,
                    "{name} (user edit {user_edit})"
                );
                // What the photo shows is still the file's EXIF with the edit.
                let shown = crate::exif_processing::read_rrexif_sidecar(&raw).unwrap();
                assert_eq!(shown["Make"], "Maker", "{name}");
                let artist = if user_edit { "Ana" } else { "Camera Owner" };
                assert_eq!(shown["Artist"], artist, "{name}");
            }
        }
    }

    /// A sidecar an older build wrote with a copy of the read EXIF map is
    /// left alone until the photo is edited; its next edit keeps only what
    /// differs from the original (the user's overrides): the copy goes, a
    /// changed, a removed field and a corrected capture time stay (SCW1).
    #[test]
    fn an_edit_reduces_a_copied_exif_map_to_the_users_overrides() {
        let (_temp, raw) = linked_raw();
        let path = raw.to_string_lossy().into_owned();
        let read =
            crate::exif_processing::read_exif_data_from_bytes(&path, &fs::read(&raw).unwrap());
        let sidecar = crate::exif_processing::get_primary_sidecar_path(&raw);
        let write_copy = |exif: &HashMap<String, String>| {
            let state = serde_json::json!({"version": 1, "rating": 2, "tags": [],
                "adjustments": {"exposure": 0.5}, "exif": exif});
            fs::write(&sidecar, serde_json::to_vec_pretty(&state).unwrap()).unwrap();
        };

        // A plain copy: nothing kept on the next edit.
        write_copy(&read);
        let before = fs::read(&sidecar).unwrap();
        let shown = crate::exif_processing::load_sidecar_with_exif(&sidecar, &raw)
            .exif
            .unwrap();
        assert_eq!(shown["Make"], "Maker");
        assert_eq!(
            fs::read(&sidecar).unwrap(),
            before,
            "reading leaves it alone"
        );
        write_editor_save(&path, serde_json::json!({"exposure": 1.0}), Some(true)).unwrap();
        assert_eq!(sidecar_exif(&raw), None, "the copy goes with the edit");

        // A copy holding the user's edits keeps just them.
        let mut edited = read.clone();
        edited.insert("Artist".into(), "Ana".into());
        edited.remove("Copyright");
        edited.insert("DateTimeOriginal".into(), "2026-10-31 00:30:00".into());
        write_copy(&edited);
        // What an export embeds over the original's EXIF (`PinnedPhoto::exif`).
        let export_edits = || {
            let stored = crate::exif_processing::load_sidecar(&sidecar).exif.unwrap();
            crate::exif_processing::exif_edits(&stored, &read)
        };
        let before_edits = export_edits();
        write_editor_save(&path, serde_json::json!({"exposure": 1.0}), Some(true)).unwrap();
        assert_eq!(
            sidecar_exif(&raw),
            Some(serde_json::json!({
                "Artist": "Ana",
                "Copyright": "",
                "DateTimeOriginal": "2026-10-31 00:30:00",
                "SubSecTimeOriginal": "",
                "OffsetTimeOriginal": "",
            }))
        );
        // The export reads the same edits from the overrides as from the copy.
        assert_eq!(export_edits(), before_edits);
        assert_eq!(before_edits["Artist"], "Ana");
        assert_eq!(before_edits["Copyright"], "");
        // What the photo shows: the file's fields with the user's.
        let shown = crate::exif_processing::read_rrexif_sidecar(&raw).unwrap();
        assert_eq!(shown["Make"], "Maker");
        assert_eq!(shown["Artist"], "Ana");
        assert!(!shown.contains_key("Copyright"));
        assert_eq!(shown["DateTimeOriginal"], "2026-10-31 00:30:00");
    }

    fn in_cloud(path: &str) {
        crate::file_management::simulate_cloud_placeholder(&parse_virtual_path(path).0);
    }

    #[test]
    fn batch_apply_reads_the_exif_of_an_online_original_but_not_of_an_evicted_one() {
        let (_temp, paths) = session_with_photos(2);
        for path in &paths {
            let (source, _) = parse_virtual_path(path);
            fs::write(&source, jpeg_with_make()).unwrap();
        }
        in_cloud(&paths[1]);
        let preset = serde_json::json!({"exposure": 1});
        let apply = |path: &String| {
            let (source, sidecar) = parse_virtual_path(path);
            pasted_metadata(path, &source, &sidecar, &preset, None, None, None, None).unwrap()
        };
        let online = apply(&paths[0]);
        // Read for its lens (now in the EXIF cache), never copied into the
        // editor state (SCW1).
        let (source, _) = parse_virtual_path(&paths[0]);
        assert_eq!(
            crate::exif_processing::get_exif_from_rrcache(&source)
                .as_ref()
                .and_then(|e| e.get("Make"))
                .map(String::as_str),
            Some("Cloud"),
            "fixture has no readable EXIF"
        );
        assert!(online.exif.is_none(), "{:?}", online.exif);
        // The evicted one is written with its recipe, without opening the file.
        let evicted = apply(&paths[1]);
        assert!(evicted.exif.is_none(), "{:?}", evicted.exif);
        assert_eq!(evicted.adjustments["exposure"], 1);
    }

    #[test]
    fn batch_apply_reports_what_needs_an_evicted_original_instead_of_downloading_it() {
        let (_temp, paths) = session_with_photos(1);
        let (source, sidecar) = parse_virtual_path(&paths[0]);
        in_cloud(&paths[0]);
        let before = crate::exif_processing::load_sidecar(&sidecar).adjustments;
        // Lightroom geometry is placed in the photo's pixels.
        let gradient = serde_json::json!({"exposure": 1, "lrNormalized": true});
        let error = pasted_metadata(
            &paths[0], &source, &sidecar, &gradient, None, None, None, None,
        )
        .unwrap_err();
        assert!(
            error.contains("original is in the cloud — download it to apply"),
            "{error}"
        );
        assert!(error.contains("gradients"), "{error}");
        // A camera's own lens data comes from the file.
        let lens = serde_json::json!({"lensCorrectionMode": "camera"});
        let error = pasted_metadata(&paths[0], &source, &sidecar, &lens, None, None, None, None)
            .unwrap_err();
        assert!(error.contains("original is in the cloud"), "{error}");
        assert!(error.contains("lens"), "{error}");
        // Nothing was written for the photo.
        assert_eq!(
            crate::exif_processing::load_sidecar(&sidecar).adjustments,
            before
        );
    }

    #[cfg(unix)]
    #[test]
    fn evicted_original_is_kept_open_for_a_lens_amount_a_saved_choice_and_a_batch_save() {
        let (_temp, paths) = session_with_photos(1);
        let (source, sidecar) = parse_virtual_path(&paths[0]);
        // An edited photo with the camera's lens data resolved and stamped.
        let mut lens = serde_json::json!({"exposure": 0.1, "lensCorrectionMode": "camera",
            "lensDistortionParams": {"k1": 0.1}});
        lens[LENS_PROFILE_FOR] = serde_json::json!(lens_profile_stamp(&lens));
        let mut saved = crate::exif_processing::load_sidecar(&sidecar);
        saved.adjustments = lens.clone();
        save_sidecar(&paths[0], &saved).unwrap();
        // Opened before (its fingerprint is remembered), evicted since.
        assert!(!crate::session_bridge::note_file_opened(&source));
        in_cloud(&paths[0]);
        // Unreadable: any open of the original would fail the batch.
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&source, fs::Permissions::from_mode(0o000)).unwrap();
        let amount =
            serde_json::json!({"lensCorrectionMode": "camera", "lensDistortionAmount": 50});
        let failed = edit_sidecars(&paths, None, |path, source, sidecar| {
            pasted_metadata(path, source, sidecar, &amount, None, None, None, None)
        })
        .failures;
        fs::set_permissions(&source, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(failed.is_empty(), "{failed:?}");
        let after = crate::exif_processing::load_sidecar(&sidecar).adjustments;
        assert_eq!(after["lensDistortionAmount"], 50);
        // Lens correction off (manual, no lens) only clears: no EXIF needed.
        let off = serde_json::json!({"lensCorrectionMode": "manual", "lensMaker": null, "lensModel": null});
        let failed = edit_sidecars(&paths, None, |path, source, sidecar| {
            pasted_metadata(path, source, sidecar, &off, None, None, None, None)
        })
        .failures;
        assert!(failed.is_empty(), "{failed:?}");
        assert_eq!(after[LENS_PROFILE_FOR], lens[LENS_PROFILE_FOR]);
    }

    #[cfg(unix)]
    #[test]
    fn first_edit_of_an_evicted_never_edited_raw_uses_the_cached_crop_without_reading_it() {
        use std::os::unix::fs::PermissionsExt;
        let (_temp, paths) = session_with_photos(1);
        let (source, sidecar) = parse_virtual_path(&paths[0]);
        save_sidecar(&paths[0], &ImageMetadata::default()).unwrap();
        in_cloud(&paths[0]);
        // What its previews cached: the camera's 16:9 crop.
        crate::preview_proxy::tests::cache_camera_crop(&source, vec![0, 376, 4000, 2248]);
        // Unreadable: any open of the original fails the edit.
        fs::set_permissions(&source, fs::Permissions::from_mode(0o000)).unwrap();
        let result = pasted_metadata(
            &paths[0],
            &source,
            &sidecar,
            &serde_json::json!({"exposure": 1}),
            None,
            None,
            None,
            None,
        );
        fs::set_permissions(&source, fs::Permissions::from_mode(0o644)).unwrap();
        let edited = result.unwrap();
        assert_eq!(edited.adjustments["exposure"], 1);
        assert_eq!(edited.adjustments["crop"]["y"], 376);
        assert_eq!(edited.adjustments["crop"]["height"], 2248);
        assert!(
            missing_camera_crops(&[RecipeChange {
                path: paths[0].clone(),
                before: serde_json::Value::Null,
                after: edited.adjustments,
            }])
            .is_empty()
        );
    }

    #[test]
    fn first_edit_of_an_evicted_raw_without_a_cached_crop_is_written_and_reported() {
        let (_temp, paths) = session_with_photos(1);
        let (source, sidecar) = parse_virtual_path(&paths[0]);
        save_sidecar(&paths[0], &ImageMetadata::default()).unwrap();
        in_cloud(&paths[0]);
        let edited = pasted_metadata(
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
        // The edit is there, started as a first edit (versions), without a crop.
        assert_eq!(edited.adjustments["exposure"], 1);
        assert!(edited.adjustments.get("renderVersion").is_some());
        assert!(edited.adjustments.get("crop").is_none());
        let notices = missing_camera_crops(&[RecipeChange {
            path: paths[0].clone(),
            before: serde_json::Value::Null,
            after: edited.adjustments,
        }]);
        assert_eq!(notices, vec![paths[0].clone()]);
        // The webview reads it as `cropMissing`.
        let batch = RecipeBatch {
            crop_missing: notices,
            ..Default::default()
        };
        assert!(
            serde_json::to_value(&batch)
                .unwrap()
                .get("cropMissing")
                .is_some()
        );
    }

    /// The editor's own first edit of an evicted never-edited RAW (the
    /// Library's active photo given a preset, a slider in Develop) is told
    /// like a batch's when no crop was cached: the editor had none to apply.
    #[test]
    fn the_editors_first_edit_of_an_evicted_raw_without_a_cached_crop_is_reported() {
        let (_temp, paths) = session_with_photos(1);
        let (source, _) = parse_virtual_path(&paths[0]);
        let never_edited = serde_json::Value::Null;
        let edited = serde_json::json!({"exposure": 1, "renderVersion": 5});
        // Online: the editor read the crop from the file.
        assert!(!first_edit_without_camera_crop(
            &paths[0],
            &never_edited,
            &edited
        ));
        in_cloud(&paths[0]);
        assert!(first_edit_without_camera_crop(
            &paths[0],
            &never_edited,
            &edited
        ));
        // Not a first edit, or nothing edited yet.
        assert!(!first_edit_without_camera_crop(&paths[0], &edited, &edited));
        assert!(!first_edit_without_camera_crop(
            &paths[0],
            &never_edited,
            &serde_json::json!({})
        ));
        // Cached with its previews: the editor applied it.
        crate::preview_proxy::tests::cache_camera_crop(&source, vec![0, 376, 4000, 2248]);
        assert!(!first_edit_without_camera_crop(
            &paths[0],
            &never_edited,
            &edited
        ));
        // The webview reads it as `cropMissing`.
        assert_eq!(
            serde_json::to_value(EditorSave { crop_missing: true }).unwrap(),
            serde_json::json!({"cropMissing": true})
        );
        assert_eq!(
            serde_json::to_value(EditorSave::default()).unwrap(),
            serde_json::json!({})
        );
    }

    /// A recipe given AI Denoise while the queue computed its selection on
    /// the decode without it takes none (its export detects on the denoised
    /// decode); the check reads the recipe as it is at the write.
    #[test]
    fn queued_selections_are_not_kept_in_a_recipe_given_ai_denoise_meanwhile() {
        use rawtonic_engine::ai_selections::{AutoSelection, Detected, detected_parameters};
        let (_temp, paths) = session_with_photos(1);
        let (source, sidecar) = parse_virtual_path(&paths[0]);
        let recipe = serde_json::json!({"aiDenoise": 60, "masks": [{"id": "l", "visible": true,
            "subMasks": [{"id": "s", "type": "ai-sky", "visible": true,
                "parameters": {"maskDataBase64": null, "detectPending": true}}]}]});
        save_sidecar(
            &paths[0],
            &ImageMetadata {
                adjustments: recipe.clone(),
                ..Default::default()
            },
        )
        .unwrap();
        let frame = crate::export_processing::detection_frame(&recipe, &source);
        let found = vec![Detected {
            layer_id: "l".into(),
            component_id: "s".into(),
            selection: AutoSelection::Sky,
            parameters: detected_parameters(
                &recipe["masks"][0]["subMasks"][0]["parameters"],
                &frame,
                "data:sky".into(),
            ),
        }];
        let undenoised = |now: &serde_json::Value| {
            rawtonic_engine::image_loader::ai_denoise_settings(now).is_none()
        };
        assert!(
            !crate::export_processing::keep_detected_selections(&paths[0], &found, undenoised)
                .unwrap()
        );
        let stored = crate::exif_processing::load_sidecar(&sidecar).adjustments;
        assert!(stored["masks"][0]["subMasks"][0]["parameters"]["maskDataBase64"].is_null());
        // Without AI Denoise it is kept.
        assert!(
            crate::export_processing::keep_detected_selections(&paths[0], &found, |_| true)
                .unwrap()
        );
    }

    /// A RAF whose previews were made before a RAF's aspect ratio was read
    /// cached an empty crop: not an answer, so its first edit is reported.
    #[test]
    fn the_editors_first_edit_of_an_evicted_raf_with_older_previews_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let fixture = dir.path().join("fixture.RAF");
        fs::write(&fixture, b"raf bytes").unwrap();
        let (_temp, paths) = session_of(1, Some(&fixture));
        let (source, _) = parse_virtual_path(&paths[0]);
        in_cloud(&paths[0]);
        let edited = serde_json::json!({"exposure": 1, "renderVersion": 5});
        crate::preview_proxy::tests::cache_camera_crop_in_facts(
            &source,
            Vec::new(),
            rawtonic_engine::source_facts::RAF_CROP_VERSION - 1,
        );
        assert!(first_edit_without_camera_crop(
            &paths[0],
            &serde_json::Value::Null,
            &edited
        ));
        // Previews made since: their crop (none set in the camera) is known.
        crate::preview_proxy::tests::cache_camera_crop_in_facts(
            &source,
            Vec::new(),
            rawtonic_engine::source_facts::RAF_CROP_VERSION,
        );
        assert!(!first_edit_without_camera_crop(
            &paths[0],
            &serde_json::Value::Null,
            &edited
        ));
        // The editor opened on an older preview. Rebuilding Standard has
        // since supplied newer facts the editor did not open with.
        crate::preview_proxy::tests::cache_camera_crop(&source, vec![0, 376, 4000, 2248]);
        assert!(first_edit_without_camera_crop_from(
            &paths[0],
            &serde_json::Value::Null,
            &edited,
            Some(false),
        ));
        assert!(!first_edit_without_camera_crop_from(
            &paths[0],
            &serde_json::Value::Null,
            &edited,
            Some(true),
        ));
    }

    #[test]
    fn export_selections_stay_with_the_effective_amount_and_model_used_on_export() {
        use rawtonic_engine::ai_selections::{AutoSelection, Detected, detected_parameters};
        let (_temp, paths) = session_with_photos(1);
        let path = &paths[0];
        let (source, sidecar) = parse_virtual_path(path);
        let exported = serde_json::json!({"aiDenoise": 60, "aiDenoiseVersion": 3, "masks": [{
            "id": "l", "visible": true, "subMasks": [{"id": "s", "type": "ai-sky", "visible": true,
                "parameters": {"maskDataBase64": null, "detectPending": true}}]}]});
        let frame = crate::export_processing::detection_frame(&exported, &source);
        let detected = [Detected {
            layer_id: "l".into(),
            component_id: "s".into(),
            selection: AutoSelection::Sky,
            parameters: detected_parameters(
                &exported["masks"][0]["subMasks"][0]["parameters"],
                &frame,
                "data:sky".into(),
            ),
        }];
        for (amount, model, accepted) in [(70, 3, false), (60, 4, false), (60, 3, true)] {
            let mut saved = exported.clone();
            saved["aiDenoise"] = serde_json::json!(amount);
            saved["aiDenoiseVersion"] = serde_json::json!(model);
            save_sidecar(
                path,
                &ImageMetadata {
                    adjustments: saved,
                    ..Default::default()
                },
            )
            .unwrap();
            assert_eq!(
                crate::export_processing::keep_export_detected_selections(
                    path, &detected, &exported
                )
                .unwrap(),
                accepted
            );
            let stored = crate::exif_processing::load_sidecar(&sidecar).adjustments;
            let expected = if accepted {
                serde_json::json!("data:sky")
            } else {
                Value::Null
            };
            assert_eq!(
                stored["masks"][0]["subMasks"][0]["parameters"]["maskDataBase64"],
                expected
            );
        }
    }

    #[test]
    fn series_match_refuses_a_first_edit_when_cloud_hides_its_develop_defaults() {
        // The defaults are process-wide, so keep this fixture out of
        // concurrent tests in a regular cargo test run.
        const CHILD: &str = "RAWTONIC_CLOUD_SERIES_MATCH_TEST_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let test = concat!(
                "file_management::edits::tests::",
                "series_match_refuses_a_first_edit_when_cloud_hides_its_develop_defaults"
            );
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", test])
                .env(CHILD, "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stdout)
            );
            return;
        }
        rawtonic_engine::develop_defaults::set_current(
            serde_json::from_value(serde_json::json!({
                "global": {"settings": {"exposure": 0.5}}
            }))
            .unwrap(),
        );
        let (_temp, paths) = session_with_photos(1);
        let (source, _) = parse_virtual_path(&paths[0]);
        in_cloud(&paths[0]);
        let mut recipe = Value::Null;
        let values = serde_json::Map::from_iter([("exposure".into(), serde_json::json!(1.0))]);
        let error = set_photo_values(&mut recipe, &source, &values).unwrap_err();
        assert!(error.contains("download it to apply"), "{error}");
        assert!(recipe.is_null());

        // An edit already has its defaults and can keep working offline.
        let mut edited = serde_json::json!({"exposure": 0.5, "renderVersion": 5});
        let old = set_photo_values(&mut edited, &source, &values).unwrap();
        assert_eq!(old["exposure"], 0.5);
        assert_eq!(edited["exposure"], 1.0);

        let mut neutral = serde_json::json!({(rawtonic_engine::develop_defaults::KEY): []});
        set_photo_values(&mut neutral, &source, &values).unwrap();
        assert_eq!(neutral["exposure"], 1.0);

        let jpeg = source.with_extension("jpg");
        fs::write(&jpeg, b"evicted JPEG").unwrap();
        crate::file_management::simulate_cloud_placeholder(&jpeg);
        let mut raster = Value::Null;
        set_photo_values(&mut raster, &jpeg, &values).unwrap();
        assert_eq!(raster["exposure"], 1.0);
    }

    #[test]
    fn evicted_camera_tiff_is_reported_for_a_paste() {
        let error = cloud_gate(
            "/p/a.tif",
            Path::new("/p/a.tif"),
            &serde_json::json!({"blacks": -20}),
            None,
        )
        .unwrap_err();
        assert!(error.contains("original is in the cloud"), "{error}");
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
            })
            .failures;
            assert!(failed.is_empty(), "{failed:?}");
            let mut ids = Vec::new();
            for path in &paths {
                let saved =
                    crate::exif_processing::load_sidecar(&parse_virtual_path(path).1).adjustments;
                assert_eq!(saved.get("lutPath"), Some(&Value::Null));
                assert_eq!(saved.get("lutData"), Some(&Value::Null));
                assert_eq!(saved.get("crop"), Some(&Value::Null));
                // Joined to the photo's own layers, once (applied again: replaced).
                let preset_layers: Vec<&Value> = saved["masks"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|layer| layer["id"] == "preset-sky")
                    .collect();
                assert_eq!(preset_layers.len(), 1);
                let sub = &preset_layers[0]["subMasks"][0];
                assert!(sub["parameters"]["maskDataBase64"].is_null());
                assert_eq!(sub["parameters"]["detectPending"], true);
                assert_ne!(sub["id"], "preset-sky-c");
                ids.push(sub["id"].clone());
            }
            assert_ne!(ids[0], ids[1]);
            assert_eq!(preset["masks"][0]["subMasks"][0]["id"], "preset-sky-c");
        }
    }

    #[test]
    fn preset_layers_join_the_photos_own_and_replace_their_earlier_copy() {
        // A built-in adaptive style on a photo with its own layer, twice:
        // the photo keeps its layer, the style's layer is there once and
        // waits for this photo's Sky selection (the selection queue).
        let (_temp, paths) = session_with_photos(1);
        let (_, sidecar) = parse_virtual_path(&paths[0]);
        let own = crate::exif_processing::load_sidecar(&sidecar).adjustments["masks"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert!(!own.is_empty(), "the fixture photo has a layer of its own");
        let builtins = super::super::presets::parse_builtin_presets(include_str!(
            "../../resources/presets/builtin.json"
        ))
        .unwrap();
        let sky = builtins
            .iter()
            .find(|p| p.id == "adaptive-sky-deeper")
            .unwrap();
        for _ in 0..2 {
            let failed = edit_sidecars(&paths, None, |path, source, sidecar| {
                pasted_metadata(
                    path,
                    source,
                    sidecar,
                    &sky.adjustments,
                    None,
                    None,
                    None,
                    None,
                )
            })
            .failures;
            assert!(failed.is_empty(), "{failed:?}");
        }
        let saved = crate::exif_processing::load_sidecar(&sidecar).adjustments;
        let masks = saved["masks"].as_array().unwrap();
        assert_eq!(masks.len(), own.len() + 1);
        assert_eq!(&masks[..own.len()], own.as_slice());
        assert_eq!(masks[own.len()]["id"], "adaptive-sky");
        assert_eq!(
            masks[own.len()]["subMasks"][0]["parameters"]["detectPending"],
            true
        );
        assert!(crate::selection_queue::wants_detection(&saved));
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
            as_chosen: false,
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
            })
            .failures;
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
        })
        .failures;
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

    /// Recipe codec fixtures (notes: plans/architecture-cleanup-2026-11.md,
    /// step 0.3b): `crates/engine/tests/fixtures/recipe-codec.json` and the
    /// editor's new-edit recipe.
    fn codec_fixtures() -> Vec<(String, Value)> {
        let read = |name: &str| -> Value {
            let path = format!(
                "{}/../crates/engine/tests/fixtures/{name}",
                env!("CARGO_MANIFEST_DIR")
            );
            serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
        };
        let mut out: Vec<(String, Value)> = read("recipe-codec.json")
            .as_object()
            .unwrap()
            .iter()
            .map(|(name, recipe)| (name.clone(), recipe.clone()))
            .collect();
        out.push(("new-edit".into(), read("new-edit-recipe.raw.json")));
        out
    }

    /// The app's load and save of a sidecar without an edit (`edit_sidecars`
    /// with `load_sidecar`, as every batch edit reads and writes) and the
    /// pass-through of `replace_recipes` keep each recipe's JSON value: key
    /// presence, explicit nulls, unknown and future keys. Outside a library
    /// (plain sidecar files), in a session (the database is authoritative,
    /// the sidecar derived) and for a session photograph's virtual copy.
    #[test]
    fn a_recipe_loaded_and_saved_without_an_edit_keeps_its_json_value() {
        let temp = tempfile::tempdir().unwrap();
        let plain = temp.path().join("Plain");
        fs::create_dir_all(&plain).unwrap();
        let lib = rawtonic_catalog::Library::create(
            &temp.path().join("Session"),
            "Codec",
            rawtonic_catalog::Mode::Session,
        )
        .unwrap();
        for (index, (name, recipe)) in codec_fixtures().into_iter().enumerate() {
            let state = serde_json::json!({
                "version": 1, "rating": 2, "tags": ["kept"], "adjustments": recipe,
            });
            let plain_photo = plain.join(format!("photo{index}.jpg"));
            let session_photo = lib.directory.join(format!("Capture/photo{index}.jpg"));
            let photos = [
                ("plain", plain_photo.to_string_lossy().into_owned()),
                ("session", session_photo.to_string_lossy().into_owned()),
                (
                    "session copy",
                    format!("{}?vc=0a0b0c", session_photo.to_string_lossy()),
                ),
            ];
            for (_, path) in &photos {
                let (source, sidecar) = parse_virtual_path(path);
                fs::write(&source, format!("synthetic original {index}")).unwrap();
                crate::session_bridge::write_editor_sidecar(&sidecar, state.to_string()).unwrap();
            }
            for (place, path) in &photos {
                let (_, sidecar) = parse_virtual_path(path);
                // A session photograph's recipe is read from the library
                // with its colour label written into the recipe
                // (`colorLabel`, `""` without one; crates/catalog
                // `apply_editor_metadata`). Not a develop setting: a recipe
                // new as saved (`{}` of a Library Reset, frame-only,
                // `lensProfileFor`-only) stays new as read
                // (`is_new_recipe`), so it renders as a never-edited photo.
                // Plain sidecars and a present virtual copy's sidecar keep
                // the recipe as saved.
                let read = match (*place, recipe.as_object()) {
                    ("session", Some(map)) => {
                        let mut map = map.clone();
                        map.insert("colorLabel".into(), serde_json::json!(""));
                        Value::Object(map)
                    }
                    _ => recipe.clone(),
                };
                assert_eq!(
                    rawtonic_engine::image_processing::is_new_recipe(&read),
                    rawtonic_engine::image_processing::is_new_recipe(&recipe),
                    "{name} {place}: as new as saved"
                );
                let failures = edit_sidecars(std::slice::from_ref(path), None, |_, _, sidecar| {
                    Ok(crate::exif_processing::load_sidecar(sidecar))
                })
                .failures;
                assert!(failures.is_empty(), "{name} {place}: {failures:?}");
                let loaded = crate::exif_processing::load_sidecar(&sidecar);
                assert_eq!(loaded.adjustments, read, "{name} {place}: loaded");
                let file: Value = serde_json::from_slice(&fs::read(&sidecar).unwrap()).unwrap();
                assert_eq!(file["adjustments"], read, "{name} {place}: sidecar file");

                // `replace_recipes` (undo/redo of a batch edit) writing the
                // recipe it read.
                let (changes, failures) =
                    edit_sidecars_recorded(std::slice::from_ref(path), None, |_, _, sidecar| {
                        let mut metadata = crate::exif_processing::load_sidecar(sidecar);
                        metadata.adjustments = recipe.clone();
                        Ok(metadata)
                    });
                let failures = failures.failures;
                assert!(failures.is_empty(), "{name} {place}: {failures:?}");
                assert_eq!(changes[0].before, read, "{name} {place}: before");
                assert_eq!(changes[0].after, recipe, "{name} {place}: after");
                assert_eq!(
                    crate::exif_processing::load_sidecar(&sidecar).adjustments,
                    read,
                    "{name} {place}: replaced"
                );
            }
        }
    }
}
