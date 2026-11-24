//! Desktop glue for `rawtonic_export::export_processing` (ADR 0009): the export
//! queue, its Tauri commands and events, sidecar and session access and the
//! metadata embedding stay here; the render and encoders live in the crate.
use rawtonic_export::existing_files::{
    self, Decision, ExistingFiles, ExportRecords, History, Outcome, Recorded,
};
pub use rawtonic_export::export_processing::*;
use rawtonic_export::file_export::require_local_tiff;
use rawtonic_export::output::Written;
use rawtonic_recipe::masks;

use crate::state::ai::AiState;
use crate::state::export::ExportState;
use crate::state::lock::lock_recover;
use std::borrow::Cow;
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use image::{DynamicImage, GenericImageView, GrayImage, ImageBuffer, Luma, imageops};
use rawtonic_recipe::effects;
use rawtonic_recipe::geometry;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::Emitter;
use tauri::Manager;

use crate::app_state::AppState;
use crate::exif_processing;
use crate::file_management::{parse_virtual_path, read_file_mapped};
use crate::formats::is_raw_file;
use crate::image_loader::load_base_image_from_bytes;
use crate::image_processing::{
    Crop, GpuContext, RenderRequest, downscale_f32_image, get_all_adjustments_from_json,
    get_or_init_gpu_context, process_and_get_dynamic_image,
    process_and_get_dynamic_image_with_precision, resolve_tonemapper_override_from_handle,
};
use crate::lut_processing::{
    convert_image_to_cube_lut, generate_identity_lut_image, get_or_load_lut,
};
use crate::mask_generation::{
    MaskBitmap, MaskDefinition, PhotoGeometry, generate_mask_bitmap, prepare_parametric_mask_source,
};

use crate::adjustment_utils::{apply_all_transformations, normalize_editor_recipe};
use crate::cache_utils::{calculate_full_job_hash, calculate_transform_hash};
use crate::editor_preview::generate_transformed_preview;
use crate::export_queue::Stage;
use crate::mask_generation::{get_cached_or_generate_mask, resolve_warped_image_for_masks};
use crate::state::develop::DevelopSession;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExportCancellationRequest {
    Requested,
    AlreadyRequested,
    NoActiveTask,
}

struct ExportTaskGuard {
    task_token: Arc<Mutex<Option<Arc<AtomicBool>>>>,
    cancellation_token: Arc<AtomicBool>,
    app_handle: Option<tauri::AppHandle>,
}

impl ExportTaskGuard {
    fn new(
        task_token: Arc<Mutex<Option<Arc<AtomicBool>>>>,
        cancellation_token: Arc<AtomicBool>,
    ) -> Self {
        Self {
            task_token,
            cancellation_token,
            app_handle: None,
        }
    }

    fn with_app_handle(
        task_token: Arc<Mutex<Option<Arc<AtomicBool>>>>,
        cancellation_token: Arc<AtomicBool>,
        app_handle: tauri::AppHandle,
    ) -> Self {
        let mut guard = Self::new(task_token, cancellation_token);
        guard.app_handle = Some(app_handle);
        guard
    }
}

impl Drop for ExportTaskGuard {
    fn drop(&mut self) {
        let app_handle = self.app_handle.clone();
        let _ = finish_export_task(
            &self.task_token,
            &self.cancellation_token,
            |cancelled| match (cancelled, app_handle) {
                (true, Some(app_handle)) => {
                    let _ = app_handle.emit("export-cancelled", ());
                }
                (false, Some(app_handle)) => {
                    let _ = app_handle.emit("export-error", "Export task terminated unexpectedly");
                }
                _ => {}
            },
        );
    }
}

fn register_export_task(
    task_token: &Mutex<Option<Arc<AtomicBool>>>,
) -> Result<Arc<AtomicBool>, String> {
    let mut active_token = task_token.lock().unwrap();
    if active_token.is_some() {
        return Err("An export is already in progress.".to_string());
    }

    let cancellation_token = Arc::new(AtomicBool::new(false));
    *active_token = Some(Arc::clone(&cancellation_token));
    Ok(cancellation_token)
}

fn request_export_cancellation<F>(
    task_token: &Mutex<Option<Arc<AtomicBool>>>,
    on_requested: F,
) -> ExportCancellationRequest
where
    F: FnOnce(),
{
    let active_token = task_token.lock().unwrap();
    let Some(cancellation_token) = active_token.as_ref() else {
        return ExportCancellationRequest::NoActiveTask;
    };

    if cancellation_token.swap(true, Ordering::SeqCst) {
        ExportCancellationRequest::AlreadyRequested
    } else {
        on_requested();
        ExportCancellationRequest::Requested
    }
}

fn finish_export_task<F>(
    task_token: &Mutex<Option<Arc<AtomicBool>>>,
    cancellation_token: &Arc<AtomicBool>,
    on_finish: F,
) -> bool
where
    F: FnOnce(bool),
{
    let mut active_token = task_token.lock().unwrap();
    let Some(current_token) = active_token.as_ref() else {
        return false;
    };
    if !Arc::ptr_eq(current_token, cancellation_token) {
        return false;
    }

    let cancelled = cancellation_token.load(Ordering::SeqCst);
    *active_token = None;

    on_finish(cancelled);
    true
}

/// Whether the open photograph's source `loaded` holds the pixels an
/// export of `adjustments` on `source` develops: the same decode variant
/// and the same AI Denoise (amount, version and so model). Anything else
/// is decoded afresh, and so is anything developed under other decode
/// preferences (linear RAW mode, preprocessing, Apple RAW 9).
fn reusable_for_export(
    loaded: &crate::state::source::LoadedImage,
    adjustments: &Value,
    source: &str,
    settings: &crate::app_settings::AppSettings,
) -> bool {
    loaded.decode == Some(crate::image_loader::decode_variant(adjustments, source))
        && loaded.decoder.as_deref()
            == Some(crate::image_loader::decode_preferences(settings).as_str())
        && loaded.ai_denoise
            == rawtonic_engine::image_loader::ai_denoise_settings(adjustments)
                .filter(|_| loaded.is_raw)
}

/// The pristine source of a full-resolution render, read from disk: RAWs
/// develop with the recipe's AI Denoise. Every export path (single images,
/// collages) loads its photos here so the same recipe develops alike; the
/// denoiser must be loaded beforehand (`ai_denoise::denoiser`), else such a
/// RAW fails.
pub(crate) fn load_export_base_image(
    ai_state: &AiState,
    source_path: &str,
    is_raw: bool,
    adjustments: &Value,
    settings: &crate::app_settings::AppSettings,
    denoise_progress: Option<rawtonic_rawdecode::mosaic_denoise::DenoiseProgress>,
) -> Result<(DynamicImage, Option<crate::image_loader::DecodeMeasures>), String> {
    let ai = rawtonic_engine::image_loader::ai_denoise_settings(adjustments).filter(|_| is_raw);
    let denoise = if let Some(ai) = ai {
        let denoiser = lock_recover(&ai_state.denoisers)[crate::ai_denoise::slot(ai.model())]
            .clone()
            .ok_or_else(|| {
                format!(
                    "{} uses AI Denoise, but its model is not available",
                    source_path
                )
            })?;
        let denoise = ai.mosaic_denoise(denoiser);
        Some(match denoise_progress {
            Some(progress) => denoise.with_progress(progress),
            None => denoise,
        })
    } else {
        None
    };
    match read_file_mapped(Path::new(source_path)) {
        Ok(mmap) => crate::image_loader::load_base_image_for_recipe_measured(
            &mmap,
            source_path,
            false,
            settings,
            None,
            denoise,
            adjustments,
        )
        .map_err(|e| export_load_error(e, "mmap")),
        Err(_) => {
            let bytes = fs::read(source_path).map_err(|e| e.to_string())?;
            crate::image_loader::load_base_image_for_recipe_measured(
                &bytes,
                source_path,
                false,
                settings,
                None,
                denoise,
                adjustments,
            )
            .map_err(|e| export_load_error(e, "bytes"))
        }
    }
}

// The persisted queue stores per-file errors as strings. Keep the codec
// message intact so the UI can localize it even after the job is reopened.
fn export_load_error(error: anyhow::Error, source: &str) -> String {
    if crate::image_loader::is_nikon_high_efficiency(&error) {
        error.to_string()
    } else {
        format!("Failed to load from {source}: {error}")
    }
}

/// A render of a region (x, y, width, height) of a prepared photograph.
pub(crate) type RenderRegion<'a> = dyn Fn([u32; 4]) -> Result<DynamicImage, String> + 'a;

/// What `render_unedited_full_size` hands its photograph's size and region
/// render to before rendering all of it.
pub(crate) type FirstRegion<'a> = dyn for<'r> Fn((u32, u32), &'r RenderRegion<'r>) + 'a;

/// A photograph not in the library yet, rendered at full size as the app
/// shows it unedited: the default recipe of a new photograph through the
/// export render (GPU), from `bytes` already read from it (a card's file is
/// read into memory, never mapped). For the import dialog's 100 % view;
/// once the photograph is ready to render, `first` may render a region of
/// it (its size, and a render of a region within it): from the same decode
/// and upload, the region's pixels are the whole render's there.
pub(crate) fn render_unedited_full_size(
    app_handle: &tauri::AppHandle,
    path: &str,
    bytes: &[u8],
    first: &FirstRegion,
) -> Result<DynamicImage, String> {
    let luts = app_handle.state::<crate::state::luts::LutState>();
    let develop = app_handle.state::<crate::state::develop::DevelopSession>();
    let gpu = app_handle.state::<crate::state::gpu::GpuState>();
    let context = get_or_init_gpu_context(&gpu, app_handle)?;
    let tonemapper_override = |is_raw| resolve_tonemapper_override_from_handle(app_handle, is_raw);
    let load_lut = |path: &str| get_or_load_lut(&luts, path).ok();
    let env = RenderEnv {
        context: &context,
        processor: &gpu.processor,
        image_cache: &develop.gpu_image_cache,
        tonemapper_override: &tonemapper_override,
        load_lut: &load_lut,
        render_inputs: None,
    };
    let unedited = decode_unedited(path, bytes, &crate::state::settings::current(app_handle))?;
    let prepared = prepare_unedited(unedited, &env)?;
    first(prepared.size(), &|region| {
        render_prepared_unedited(&prepared, Some(region), &env)
    });
    render_prepared_unedited(&prepared, None, &env)
}

/// The size `render_unedited_full_size` renders the RAW in `file` at, from
/// its header: the new recipe's crop (the camera's) of the developed frame,
/// which the decode turns upright. None where the header tells nothing.
/// Reads `file` in place (a mapping: only the metadata it needs).
pub(crate) fn unedited_size(
    path: &str,
    file: &rawtonic_engine::image_loader::SharedBytes,
) -> Option<(u32, u32)> {
    let (adjustments, _) = rawtonic_export::normalize::new_edit_from_source(
        Path::new(path),
        &rawler::rawsource::RawSource::new_from_shared(file.clone()),
    );
    let source = rawler::rawsource::RawSource::new_from_shared(file.clone()).with_path(path);
    let version = rawtonic_rawdecode::source_frame::effective(
        &source,
        rawtonic_engine::source_frame::requested(&adjustments),
    );
    let (w, h) = rawtonic_rawdecode::source_frame::frame(&source, version)?.size();
    Some(
        rawtonic_engine::image_processing::crop_rect(&adjustments[geometry::CROP.name()], w, h)
            .map_or((w, h), |(_, _, cw, ch)| (cw, ch)),
    )
}

#[cfg(test)]
fn render_unedited_full_size_with_env(
    path: &str,
    bytes: &[u8],
    settings: &crate::app_settings::AppSettings,
    env: &RenderEnv<'_>,
) -> Result<DynamicImage, String> {
    let prepared = prepare_unedited(decode_unedited(path, bytes, settings)?, env)?;
    render_prepared_unedited(&prepared, None, env)
}

/// A photograph not in the library yet, decoded for its unedited render
/// (`prepare_unedited`): the default recipe of a new photograph and the
/// source pixels, from the bytes only.
pub(crate) struct UneditedSource {
    path: String,
    is_raw: bool,
    adjustments: Value,
    versions: rawtonic_engine::recipe_versions::RecipeVersions,
    source: ExportSource,
}

pub(crate) fn decode_unedited(
    path: &str,
    bytes: &[u8],
    settings: &crate::app_settings::AppSettings,
) -> Result<UneditedSource, String> {
    let is_raw = is_raw_file(path);
    let (adjustments, normalized) =
        rawtonic_export::normalize::new_edit_from_bytes(Path::new(path), bytes);
    let versions = normalized.file_versions();
    normalized.lens?;
    // Decoded from the bytes only: the app's loader would also cache the
    // file's EXIF in sidecars beside it, and nothing may be written beside a
    // photograph that is not imported (with Apple's RAW engine preferred,
    // this view still develops with rawler).
    let frame = if is_raw {
        let source = rawler::rawsource::RawSource::new_from_slice(bytes).with_path(path);
        rawtonic_rawdecode::source_frame::effective(
            &source,
            rawtonic_engine::source_frame::requested(&adjustments),
        )
    } else {
        0
    };
    let options = crate::image_loader::base_options(
        settings,
        rawtonic_engine::image_loader::decode_variant_for_frame(&adjustments, path, frame),
    );
    let (pristine, measures) =
        crate::image_loader::load_base_image_measured(bytes, path, false, &options, None)
            .map_err(|e| e.to_string())?;
    let source = prepare_export_source(path, Arc::new(pristine), measures, &adjustments)?;
    Ok(UneditedSource {
        path: path.to_string(),
        is_raw,
        adjustments,
        versions,
        source,
    })
}

/// The export settings of the unedited render: an sRGB JPEG's pixels.
fn unedited_export_settings() -> Result<ExportSettings, String> {
    serde_json::from_value(serde_json::json!({
        "jpegQuality": 92,
        "tiffBitDepth": 16,
        "keepMetadata": false,
        "stripGps": true,
    }))
    .map_err(|e| e.to_string())
}

/// `unedited` transformed and uploaded for its renders
/// (`render_prepared_unedited`), so a region of it and then all of it
/// render from one decode, transform and upload.
pub(crate) fn prepare_unedited(
    unedited: UneditedSource,
    env: &RenderEnv<'_>,
) -> Result<PreparedExport, String> {
    prepare_export_render(
        &unedited.path,
        unedited.source,
        &unedited.adjustments,
        &unedited.versions,
        env,
        unedited.is_raw,
    )
}

/// The unedited photograph `prepared` at full size, as the app shows it
/// unedited (the export render of a new edit as a JPEG), all of it or only
/// `region` (x, y, width, height within `prepared.size()`), whose pixels
/// are those of the whole render there.
pub(crate) fn render_prepared_unedited(
    prepared: &PreparedExport,
    region: Option<[u32; 4]>,
    env: &RenderEnv<'_>,
) -> Result<DynamicImage, String> {
    let export_settings = unedited_export_settings()?;
    let image = render_prepared_export_region(
        prepared,
        env,
        "process_image_for_export",
        render_output_precision("jpeg", &export_settings),
        export_settings.color_space,
        region.map(|[x, y, width, height]| rawtonic_gpu::gpu_processing::Roi {
            x,
            y,
            width,
            height,
        }),
    )?;
    if region.is_some() {
        return Ok(image);
    }
    apply_export_geometry_and_watermark(image, &export_settings).map(|(image, _)| image)
}

/// Loads the AI Denoise models these recipes use, once each; photos
/// that need it fail one by one if it cannot be had.
pub(crate) async fn prepare_ai_denoise<V: std::borrow::Borrow<Value>>(
    app_handle: &tauri::AppHandle,
    recipes: impl IntoIterator<Item = V>,
) {
    load_denoise_models(app_handle, denoise_models(recipes)).await;
}

/// The AI Denoise models `recipes` use.
pub(crate) fn denoise_models<V: std::borrow::Borrow<Value>>(
    recipes: impl IntoIterator<Item = V>,
) -> BTreeSet<usize> {
    recipes
        .into_iter()
        .filter_map(|adjustments| {
            rawtonic_engine::image_loader::ai_denoise_settings(adjustments.borrow())
                .map(|ai| ai.model())
        })
        .collect()
}

/// Loads `models` (`denoise_models`), once each.
async fn load_denoise_models(app_handle: &tauri::AppHandle, models: BTreeSet<usize>) {
    for model in models {
        if let Err(e) = crate::ai_denoise::denoiser(app_handle, model).await {
            log::warn!("AI Denoise unavailable for this export: {e}");
        }
    }
}

/// Whether `path` (an exported photograph, `?vc=` included) is the one open in
/// the editor, whose unsaved recipe the export then uses. A virtual copy and
/// its master share a source file but not a recipe, so the whole path counts.
fn is_open_edit(path: &str, open_path: Option<&str>) -> bool {
    open_path == Some(path)
}

/// What one photo of an export renders and embeds, read once when the
/// export was submitted (`pin_photos`): a queued export renders this,
/// whichever library is open when it runs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PinnedPhoto {
    /// Its recipe (a virtual copy's own; the editor's for the open photo).
    pub adjustments: Value,
    /// The library metadata its files embed (a virtual copy's own rating,
    /// label and keywords); None outside a session or catalog.
    pub metadata: Option<rawtonic_metadata::Metadata>,
    /// The EXIF its editor state or EXIF cache holds (`read_rrexif_sidecar`):
    /// what its files embed of it, and its capture date for names and file
    /// times. None when none was read yet (a photo never decoded): the
    /// export then reads it from the original itself (`exif`).
    #[serde(default)]
    pub exif: Option<std::collections::BTreeMap<String, String>>,
    /// Without `exif`: whether its export embeds the EXIF its decode reads
    /// from the original. It does unless the library keeps the photo's
    /// editor state beside a sidecar file, where the decode's map goes to
    /// the file, which the library's state outranks (as on main).
    #[serde(default)]
    pub exif_from_original: bool,
    /// The EXIF read from the original when it is embedded, once.
    #[serde(skip)]
    pub from_original: std::sync::OnceLock<Option<std::collections::HashMap<String, String>>>,
    /// The session or catalog that owns it (None: no library), under which
    /// its export runs (`with_culling_owner`), never the library open then.
    #[serde(default)]
    pub owner: Option<String>,
    /// Its capture time as the library corrected it (Shift Capture Time),
    /// which its EXIF, names and file times take over the file's.
    #[serde(default)]
    pub capture: Option<rawtonic_catalog::capture_time::CaptureTime>,
    /// The original's file as the user saw it when the export was queued
    /// (`session_bridge::source_identity`): the export is refused when its
    /// file is another one by then. None for a job queued by an earlier
    /// build, which skips the check.
    #[serde(default)]
    pub source_identity: Option<String>,
}

impl PinnedPhoto {
    /// The EXIF of `source` as this snapshot has it, else as the original
    /// file holds it (what its first decode caches), never as a sidecar or
    /// library holds it by now.
    pub(crate) fn exif(&self, source: &Path) -> Option<std::collections::HashMap<String, String>> {
        self.corrected(self.read_exif(source))
    }

    /// `map` with the corrected capture time (`capture`), marked for the
    /// export (`CAPTURE_CORRECTED`). No map stays none: the export then
    /// reads the camera's fields from the original, which a map of dates
    /// alone would replace.
    fn corrected(
        &self,
        map: Option<std::collections::HashMap<String, String>>,
    ) -> Option<std::collections::HashMap<String, String>> {
        let Some(time) = &self.capture else {
            return map;
        };
        let mut map = map?;
        exif_processing::mark_corrected_capture(&mut map, &time.exif_fields());
        Some(map)
    }

    fn read_exif(&self, source: &Path) -> Option<std::collections::HashMap<String, String>> {
        match &self.exif {
            Some(exif) => Some(exif.clone().into_iter().collect()),
            None if !self.exif_from_original => None,
            None => self
                .from_original
                .get_or_init(|| {
                    let path = source.to_string_lossy();
                    let exif = match read_file_mapped(source) {
                        Ok(bytes) => exif_processing::read_exif_data_from_bytes(&path, &bytes),
                        Err(_) => exif_processing::read_exif_data_from_bytes(
                            &path,
                            &fs::read(source).ok()?,
                        ),
                    };
                    (!exif.is_empty()).then_some(exif)
                })
                .clone(),
        }
    }

    /// The capture date of `source` (`exif`), for its files' times.
    pub(crate) fn capture_date(&self, source: &Path) -> jiff::Timestamp {
        exif_processing::capture_date_with(source, &|| self.exif(source))
    }

    /// The capture date of `source` before it is decoded, for its names:
    /// the snapshot's, else the file's own (`capture_date_with`).
    pub(crate) fn naming_date(&self, source: &Path) -> jiff::Timestamp {
        exif_processing::capture_date_with(source, &|| {
            self.corrected(
                self.exif
                    .as_ref()
                    .map(|exif| exif.clone().into_iter().collect()),
            )
        })
    }
}

/// The photos of an export run (`paths`).
#[derive(Clone)]
pub(crate) struct ExportPhotos {
    /// The photo the editor showed, whose decoded source may be reused.
    pub open_path: Option<String>,
    /// The AI Denoise models their recipes use, loaded before the run.
    pub denoise_models: BTreeSet<usize>,
    /// What photo `i` (`paths[i]`) is exported with, read when its worker
    /// starts it, so a run holds only its photos in progress.
    pub read: Arc<dyn Fn(usize) -> Result<PinnedPhoto, String> + Send + Sync>,
}

/// What each of `paths` is exported with, read now and once (the point of
/// no return of a queued export): its recipe (the editor's `open_adjustments`
/// for `open_path`, else the saved one, a virtual copy's its own) and the
/// metadata its files embed, both through the library that owns the photo:
/// its own session, else `submitted_from`, the catalog the export was
/// submitted from (`culling_library_for`). Never the library open later.
/// Fails when an owning library cannot be read, rather than taking a
/// sidecar file or no metadata for what it holds.
pub(crate) fn pin_photos(
    paths: &[String],
    open_path: Option<&str>,
    open_adjustments: Option<&Value>,
    submitted_from: Option<&Path>,
) -> Result<Vec<PinnedPhoto>, String> {
    use rayon::prelude::*;
    // The library it was submitted from must be there: else its photos
    // would count as in no library and lose what it holds of them.
    if let Some(library) = submitted_from {
        rawtonic_catalog::Library::open(library)
            .map_err(|e| format!("The library {} cannot be read: {e:#}", library.display()))?;
    }
    paths
        .par_iter()
        .map(|path| {
            let (source, sidecar) = parse_virtual_path(path);
            let owner = crate::session_bridge::culling_library_for(&source, submitted_from);
            if let Some(owner) = &owner {
                library_readable(owner, &source)?;
            }
            let owner = owner.map(|dir| dir.to_string_lossy().into_owned());
            crate::session_bridge::with_culling_owner(owner.as_deref(), || {
                let saved = exif_processing::load_sidecar(&sidecar);
                let metadata = crate::session_bridge::catalog_metadata(&source);
                // A virtual copy is rated, labelled and tagged on its own.
                let metadata = if rawtonic_metadata::photo_ref::is_copy(path) {
                    metadata.map(|photo| {
                        rawtonic_catalog::copy_metadata(photo, saved.rating, saved.tags.as_deref())
                    })
                } else {
                    metadata
                };
                let adjustments = match open_adjustments {
                    Some(adjustments) if is_open_edit(path, open_path) => adjustments.clone(),
                    _ => saved.adjustments,
                };
                let exif = exif_processing::read_rrexif_sidecar(&source);
                let primary = exif_processing::get_primary_sidecar_path(&source);
                let library_state = || {
                    crate::session_bridge::read_editor_sidecar(&primary).is_some_and(|state| {
                        serde_json::from_value::<crate::image_processing::ImageMetadata>(state)
                            .is_ok()
                    })
                };
                Ok(PinnedPhoto {
                    adjustments,
                    metadata,
                    exif_from_original: exif.is_none() && !(primary.exists() && library_state()),
                    exif: exif.map(|exif| exif.into_iter().collect()),
                    owner: owner.clone(),
                    from_original: Default::default(),
                    capture: crate::session_bridge::capture_override(&source),
                    source_identity: require_local_tiff(&source)
                        .ok()
                        .and_then(|()| crate::session_bridge::source_identity(&source)),
                })
            })
        })
        .collect()
}

/// Whether the library `owner` reads what it holds of `source` (its
/// recipe and metadata) without an error.
fn library_readable(owner: &Path, source: &Path) -> Result<(), String> {
    let failed = |e: rawtonic_catalog::CatalogError| {
        format!(
            "The library {} cannot be read for {}: {e:#}",
            owner.display(),
            source.display()
        )
    };
    let lib = rawtonic_catalog::Library::open(owner).map_err(failed)?;
    if let Some(asset) = lib.asset_at(source).map_err(failed)? {
        lib.metadata(&asset.id).map_err(failed)?;
        lib.editor_state(&asset.id).map_err(failed)?;
    }
    Ok(())
}

/// Why a photo gone since its export was submitted cannot be exported; it
/// fails on its own (a cloud file not downloaded yet is there: reading it
/// downloads it).
pub(crate) fn missing_source(source: &Path) -> Option<String> {
    (!source.exists()).then(|| {
        format!(
            "{} cannot be found: the original was moved, renamed or deleted, or its drive is not connected",
            source.display()
        )
    })
}

fn save_image_with_metadata(
    image: DynamicImage,
    output_path: &std::path::Path,
    source_path_str: &str,
    export_settings: &ExportSettings,
    generated: bool,
    pinned: &PinnedPhoto,
) -> Result<(), String> {
    let exif_sidecar = || pinned.exif(Path::new(source_path_str));
    let catalog = || pinned.metadata.clone();
    save_export_image(
        image,
        source_path_str,
        export_settings,
        generated,
        &ExportOutput {
            path: output_path,
            format: "",
            exif_sidecar: &exif_sidecar,
            catalog: &catalog,
            write: &write_export_bytes,
            enlarger: None,
            // Mask images carry no manifest.
            provenance: None,
        },
    )
}

/// How a recipe's file is published: new, or over `previous`, the
/// recorded previous export of `source` (Replace if changed).
fn publish_with<'a>(
    previous: Option<&'a Recorded>,
    source: &'a str,
) -> rawtonic_export::output::Publish<'a> {
    match previous {
        Some(previous) => rawtonic_export::output::Publish::Replace { previous, source },
        None => rawtonic_export::output::Publish::New,
    }
}

/// The app's record of the files its exports wrote (for Replace if
/// changed); None when it cannot be opened (exports then go on without it:
/// Replace if changed keeps both).
pub(crate) fn open_export_records(app_handle: &tauri::AppHandle) -> Option<ExportRecords> {
    let dir = app_handle.path().app_data_dir().ok()?;
    match ExportRecords::open(&existing_files::records_path(&dir)) {
        Ok(records) => Some(records),
        Err(e) => {
            log::warn!("Export records: {e:#}");
            None
        }
    }
}

/// Writes an export to a new file (never over an existing one).
fn write_export_bytes(
    output_path: &std::path::Path,
    _extension: &str,
    encoded: rawtonic_export::export_processing::EncodedExport,
) -> Result<(), String> {
    rawtonic_export::output::write_new_export(output_path, encoded)
}

#[allow(clippy::too_many_arguments)]
fn export_masks_for_image(
    base_image: &DynamicImage,
    pristine_source: &DynamicImage,
    js_adjustments: &Value,
    export_settings: &ExportSettings,
    output_path_obj: &std::path::Path,
    source_path_str: &str,
    context: &Arc<GpuContext>,
    develop: &DevelopSession,
    is_raw: bool,
    gpu: &crate::state::gpu::GpuState,
    tm_override: Option<u32>,
    lut: Option<Arc<crate::lut_processing::Lut>>,
    enlarger: Option<&dyn rawtonic_export::export_processing::Enlarger>,
    pinned: &PinnedPhoto,
    cancellation_token: &AtomicBool,
) -> Result<(), String> {
    ensure_export_not_cancelled(cancellation_token)?;
    let (transformed_image, unscaled_crop_offset) =
        apply_all_transformations(Cow::Borrowed(base_image), js_adjustments, is_raw)?;
    ensure_export_not_cancelled(cancellation_token)?;
    let (img_w, img_h) = transformed_image.dimensions();
    let mask_definitions: Vec<MaskDefinition> = js_adjustments
        .get(masks::MASKS.name())
        .and_then(|m| serde_json::from_value(m.clone()).ok())
        .unwrap_or_default();

    let warped_image = mask_definitions
        .iter()
        .any(MaskDefinition::requires_warped_image)
        .then(|| prepare_parametric_mask_source(pristine_source, is_raw, 1.0, js_adjustments))
        .transpose()?;
    let mask_geometry = PhotoGeometry::from_adjustments(js_adjustments)
        .with_decoded_size(pristine_source.width(), pristine_source.height());
    let mut mask_bitmaps = Vec::with_capacity(mask_definitions.len());
    for definition in mask_definitions
        .iter()
        .filter(|definition| definition.produces_bitmap())
        .take(crate::image_processing::MAX_MASKS)
    {
        ensure_export_not_cancelled(cancellation_token)?;
        if let Some(bitmap) = generate_mask_bitmap(
            definition,
            img_w,
            img_h,
            1.0,
            unscaled_crop_offset,
            &mask_geometry,
            warped_image.as_ref(),
        ) {
            mask_bitmaps.push(bitmap);
        }
        ensure_export_not_cancelled(cancellation_token)?;
    }

    if !mask_bitmaps.is_empty() {
        let all_adjustments = get_all_adjustments_from_json(js_adjustments, is_raw, tm_override);
        let unique_hash = calculate_full_job_hash(source_path_str, js_adjustments);
        let extension = output_path_obj
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("jpg");

        for (i, _) in mask_bitmaps.iter().enumerate() {
            ensure_export_not_cancelled(cancellation_token)?;
            let single_adjustments = build_single_mask_adjustments(&all_adjustments, i);
            let full_white_mask = ImageBuffer::from_fn(img_w, img_h, |_, _| Luma([255u8]));
            let single_bitmaps: Vec<Arc<MaskBitmap>> = vec![Arc::new(full_white_mask.into())];

            let processed = process_and_get_dynamic_image_with_precision(
                context,
                gpu,
                develop,
                transformed_image.as_ref(),
                unique_hash,
                RenderRequest {
                    adjustments: single_adjustments,
                    mask_bitmaps: &single_bitmaps,
                    lut: lut.clone(),
                    roi: None,
                },
                "export_mask_image",
                render_output_precision(extension, export_settings),
            )?;
            ensure_export_not_cancelled(cancellation_token)?;

            // The same finishing as the main file (Super Resolution included),
            // so the layer's image and alpha match its size.
            let (with_options, geometry) =
                finish_rendered_export(processed, export_settings, enlarger, &|| {
                    ensure_export_not_cancelled(cancellation_token)
                })?;

            let alpha_scaled = imageops::resize(
                &mask_bitmaps[i],
                geometry.photo_w,
                geometry.photo_h,
                imageops::FilterType::Lanczos3,
            );

            let alpha_resized: GrayImage = if geometry.is_identity() {
                alpha_scaled
            } else {
                place_photo_on_canvas(&alpha_scaled, &geometry, Luma([0u8]), None)
            };
            ensure_export_not_cancelled(cancellation_token)?;

            let [mask_image_path, mask_alpha_path] = mask_file_paths(output_path_obj, i);

            save_image_with_metadata(
                with_options,
                &mask_image_path,
                source_path_str,
                export_settings,
                rawtonic_engine::image_loader::has_generated_areas(js_adjustments),
                pinned,
            )?;
            ensure_export_not_cancelled(cancellation_token)?;

            if export_settings.preserve_timestamps {
                set_capture_file_times(
                    &mask_image_path,
                    pinned.capture_date(Path::new(source_path_str)),
                );
            }

            let alpha_bytes = encode_grayscale_to_png(&alpha_resized)?;
            ensure_export_not_cancelled(cancellation_token)?;

            crate::session_bridge::write_new_output(&mask_alpha_path, alpha_bytes)
                .map_err(|e| e.to_string())?;
            ensure_export_not_cancelled(cancellation_token)?;
        }
    }
    Ok(())
}

/// The separate files of mask layer `layer` beside a main export file:
/// the layer's rendered image and its alpha.
pub(crate) fn mask_file_paths(output_path: &Path, layer: usize) -> [std::path::PathBuf; 2] {
    let output_dir = output_path.parent().unwrap_or(output_path);
    let stem = output_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("export");
    let extension = output_path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("jpg");
    [
        output_dir.join(format!("{}_mask_{}_image.{}", stem, layer, extension)),
        output_dir.join(format!("{}_mask_{}_alpha.png", stem, layer)),
    ]
}

/// `tm_override`: the non-RAW tone-mapper override of the run's settings
/// snapshot (a LUT is the look on non-RAW pixels).
fn export_adjustments_as_lut(
    js_adjustments: &Value,
    source_path_str: &str,
    context: &Arc<GpuContext>,
    develop: &DevelopSession,
    app_handle: &tauri::AppHandle,
    cancellation_token: &AtomicBool,
    tm_override: Option<u32>,
) -> Result<Vec<u8>, String> {
    let gpu = app_handle.state::<crate::state::gpu::GpuState>();
    ensure_export_not_cancelled(cancellation_token)?;
    let lut_size = 33;
    let identity_image = generate_identity_lut_image(lut_size);

    let mut all_adjustments = get_all_adjustments_from_json(js_adjustments, false, tm_override);

    all_adjustments.global.show_clipping = 0;
    all_adjustments.global.vignette_amount = 0.0;
    all_adjustments.global.grain_amount = 0.0;
    all_adjustments.global.sharpness = 0.0;
    all_adjustments.global.clarity = 0.0;
    all_adjustments.global.dehaze = 0.0;
    all_adjustments.global.structure = 0.0;
    all_adjustments.global.centré = 0.0;
    all_adjustments.global.glow_amount = 0.0;
    all_adjustments.global.halation_amount = 0.0;
    all_adjustments.global.flare_amount = 0.0;
    all_adjustments.global.luma_noise_reduction = 0.0;
    all_adjustments.global.color_noise_reduction = 0.0;
    all_adjustments.global.chromatic_aberration_red_cyan = 0.0;
    all_adjustments.global.chromatic_aberration_blue_yellow = 0.0;

    let lut_path = js_adjustments[effects::LUT_PATH.into_name()].as_str();
    let lut = lut_path
        .and_then(|p| get_or_load_lut(&app_handle.state::<crate::state::luts::LutState>(), p).ok());
    let unique_hash = calculate_full_job_hash(source_path_str, js_adjustments);

    let processed_lut = process_and_get_dynamic_image(
        context,
        &gpu,
        develop,
        &identity_image,
        unique_hash,
        RenderRequest {
            adjustments: all_adjustments,
            mask_bitmaps: &[],
            lut,
            roi: None,
        },
        "export_lut",
    )?;
    ensure_export_not_cancelled(cancellation_token)?;

    let cube_lut = convert_image_to_cube_lut(&processed_lut, lut_size)?;
    ensure_export_not_cancelled(cancellation_token)?;
    Ok(cube_lut)
}

/// Exports keep their GPU processor (its textures and compiled pipelines)
/// between runs until the system runs short of memory (under a compile
/// limit not at all, `release_after_run`); one that a render holds stays,
/// and goes when its run ends.
pub(crate) fn release_processor_under_pressure(app_handle: &tauri::AppHandle) {
    let app_handle = app_handle.clone();
    crate::memory_pressure::watch(Arc::new(move |_| {
        if app_handle
            .state::<crate::state::gpu::GpuState>()
            .export_processor
            .take_if_idle()
            .is_some()
        {
            log::info!("Memory pressure: released the exports' GPU processor");
        }
    }));
}

/// At the end of an export run: pressure is reported once on entering it,
/// so pressure that came while a render held the processor would otherwise
/// leave it allocated for as long as memory stays short.
/// The run's prewarm goes first: while it builds the processor holds its
/// lock and looks in use, so a run that failed or was cancelled before the
/// prewarm finished would leave the processor it then stores.
async fn release_after_run<T, E>(
    processor: &crate::gpu_processing::ProcessorSlot<Option<T>>,
    prewarm: impl std::future::Future<Output = Result<(), E>>,
    short_of_memory: impl FnOnce() -> bool,
) -> bool {
    let _ = prewarm.await;
    short_of_memory() && processor.take_if_idle().is_some()
}

/// The thread pools an export's photos run in, one per export worker.
///
/// Exports ran their parallel work (decode, develop, encode) in the global
/// pool at normal priority, beside the Develop view's own work there: during
/// a batch export slider drags drew 1–2 frames a second (notes:
/// parity-gaps/G44). Here the system runs the editor first and the export
/// with the cores left over.
///
/// One pool per worker, never one shared: a pool thread holding the GPU
/// processor that waits for its parallel work may meanwhile run other jobs
/// of its pool, which would be another photo waiting for that processor.
/// So no job of a worker's pool waits for it, and its renders keep their
/// parallel work in it, off the pool the Develop view's frames use
/// (`mark_private_pool_thread`).
struct ExportLanes {
    pools: Vec<rayon::ThreadPool>,
    free: Mutex<Vec<usize>>,
    /// Their threads, on few CPUs while the user edits.
    _cpus: Arc<crate::export_cpus::ExportCpus>,
}

/// An export worker's pool, given back when its photo is done.
struct Lane {
    lanes: Arc<ExportLanes>,
    index: usize,
}

impl ExportLanes {
    fn new(workers: usize, threads: usize) -> Arc<Self> {
        // Threads inherit their creator's priority: not from an export's.
        crate::gpu_processing::start_shared_pools();
        let cpus = crate::export_cpus::ExportCpus::new();
        let run = Arc::downgrade(&cpus);
        // The render stack, not the 2 MiB default: renders on RADV
        // overflowed it (`RENDER_STACK_SIZE`, gap G74).
        let pools = rawtonic_export::workers::lane_pools(workers, threads, "export", move || {
            lower_thread_priority();
            if let Some(cpus) = run.upgrade() {
                cpus.add_current_thread();
            }
        });
        cpus.follow_editing();
        Arc::new(Self {
            free: Mutex::new((0..workers).rev().collect()),
            pools,
            _cpus: cpus,
        })
    }

    /// A free pool; there is one for each worker that may run.
    fn take(self: &Arc<Self>) -> Lane {
        let index = self
            .free
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pop()
            .expect("an export pool for every worker");
        Lane {
            lanes: Arc::clone(self),
            index,
        }
    }
}

impl Lane {
    fn install<R: Send>(&self, work: impl FnOnce() -> R + Send) -> R {
        self.lanes.pools[self.index].install(work)
    }
}

impl Drop for Lane {
    fn drop(&mut self) {
        self.lanes
            .free
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(self.index);
    }
}

/// Lowers the calling thread's scheduling priority below the editor's.
/// Linux: `SCHED_IDLE`, which an unprivileged thread cannot undo, so only
/// for threads that do nothing else: they run on the time other threads
/// leave. At nice 10, slider drags during a 200-photo export on a 6-core
/// PC drew 32–34 frames a second with 200 photos selected, 44–48 with
/// this, export time unchanged (notes: parity-gaps/G44).
/// macOS keeps the default: its lower classes move threads to the
/// efficiency cores (see `proxy_worker`'s builds). Also the preview
/// proxies' prefetch and build threads (`proxy_worker`).
#[cfg(target_os = "linux")]
pub(crate) fn lower_thread_priority() {
    let param = libc::sched_param { sched_priority: 0 };
    // SAFETY: changes the calling thread's own policy (0 = this thread);
    // `param` outlives the call.
    if unsafe { libc::sched_setscheduler(0, libc::SCHED_IDLE, &param) } != 0 {
        // SAFETY: changes the calling thread's own nice value; no pointers.
        unsafe {
            libc::setpriority(libc::PRIO_PROCESS, libc::gettid() as libc::id_t, 10);
        }
    }
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn lower_thread_priority() {}

/// One recipe of an export run: its settings, where its files go and the
/// queued job that records its photos.
#[derive(Clone)]
pub(crate) struct ExportTarget {
    pub output_folder_or_file: String,
    pub is_explicit_file_path: bool,
    pub base_origin_folders: Vec<String>,
    pub export_settings: ExportSettings,
    pub output_format: String,
    pub tracking: Option<crate::export_queue::Tracking>,
}

/// Exports `paths` with every recipe in `targets`. Each photo is loaded,
/// transformed and uploaded once and every recipe renders, sizes, sharpens
/// and encodes its file from that (notes: parity-gaps/G43, F10); before, a
/// photo exported with two recipes was decoded and developed twice.
pub(crate) async fn export_images_impl(
    paths: Vec<String>,
    targets: Vec<ExportTarget>,
    photos: ExportPhotos,
    app_handle: tauri::AppHandle,
    completion_tx: Option<tokio::sync::oneshot::Sender<Result<(), usize>>>,
    max_jobs: Option<usize>,
) -> Result<(), String> {
    // No AI model's GPU verdict changes while the export runs (a check
    // finishing between two photos), so every photo of it uses the provider
    // the first one used, also after a model was unloaded and loaded again.
    // Moved into the export task below and held until its workers end.
    let same_ai_providers = rawtonic_onnx::hold_checks();
    let task_token = Arc::clone(&app_handle.state::<ExportState>().task_token);
    let cancellation_token = register_export_task(&task_token)?;
    let task_guard = ExportTaskGuard::with_app_handle(
        Arc::clone(&task_token),
        Arc::clone(&cancellation_token),
        app_handle.clone(),
    );
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    if cancellation_token.load(Ordering::SeqCst) {
        return Ok(());
    }

    #[cfg(feature = "performance-profiling")]
    let context_stage = crate::performance::PerformanceStage::new("export_gpu_context");
    let context = match get_or_init_gpu_context(
        &app_handle.state::<crate::state::gpu::GpuState>(),
        &app_handle,
    ) {
        Ok(context) => context,
        Err(_) if cancellation_token.load(Ordering::SeqCst) => return Ok(()),
        Err(error) => return Err(error),
    };

    if cancellation_token.load(Ordering::SeqCst) {
        return Ok(());
    }

    #[cfg(feature = "performance-profiling")]
    drop(context_stage);
    // Where masking computations and exports take turns (an integrated GPU
    // under a compile limit): not beside a mask computed ahead (the app
    // alone reached 7.6 GB with a JPEG export beside one on a Radeon 680M,
    // G76/WSMOKE20). Held until the export's workers end.
    let ai_turn = crate::ai_lifecycle::export_turn(&app_handle).await;
    if cancellation_token.load(Ordering::SeqCst) {
        return Ok(());
    }
    // Under a compile limit the export can wait for the shown photo's
    // display variant before allocating its textures.
    rawtonic_gpu::gpu_processing::export_of_shown_photo_starts();
    let prewarm = {
        let app_handle = app_handle.clone();
        let context = context.clone();
        tokio::task::spawn_blocking(move || {
            // Under a compile limit the processor's textures (~1 GB) are not
            // made beside a running compile: WGT21's Glow 30 export peaked
            // at 6.8 GB on a Radeon 680M, the Glow compile alone at 5.8 GB
            // (G76/WCC9).
            rawtonic_gpu::gpu_processing::wait_for_running_compiles("export processor");
            crate::gpu_processing::prewarm_gpu_processor(
                &app_handle
                    .state::<crate::state::gpu::GpuState>()
                    .export_processor,
                &context,
            );
        })
    };
    let context = Arc::new(context);
    let total_paths = paths.len();
    // Progress counts the files of every recipe; a resumed job counts the
    // photos an earlier run exported.
    let total_outputs = total_paths * targets.len();
    let progress_counter = Arc::new(AtomicUsize::new(
        targets
            .iter()
            .filter_map(|target| target.tracking.as_ref())
            .map(|t| t.already_done)
            .sum(),
    ));

    let available_cores = rawtonic_export::workers::available_cores();

    let mut sys = sysinfo::System::new();
    sys.refresh_memory();

    let available_ram_gb = sys.available_memory() as f64 / 1024.0 / 1024.0 / 1024.0;
    let num_threads = rawtonic_export::workers::export_workers(
        paths.len(),
        available_cores,
        sys.available_memory(),
    )
    .min(max_jobs.unwrap_or(usize::MAX));

    log::info!(
        "Batch Export: {} cores, {:.1} GB free RAM -> {} threads, {} recipes",
        available_cores,
        available_ram_gb,
        num_threads,
        targets.len()
    );

    let _export_task = tokio::spawn(async move {
        let _task_guard = task_guard;
        let _same_ai_providers = same_ai_providers;
        let _ai_turn = ai_turn;
        load_denoise_models(&app_handle, photos.denoise_models.clone()).await;
        let settings = crate::state::settings::current(&app_handle);
        let targets = Arc::new(targets);

        let export_items = rawtonic_export::sidecars::plan_export_items(paths);
        // Every export is recorded: the records file is set up once, before
        // photos run side by side and each opens it.
        drop(open_export_records(&app_handle));

        let used_paths = Arc::new(Mutex::new(std::collections::HashSet::new()));
        let lanes = ExportLanes::new(num_threads, available_cores);
        let semaphore = Arc::new(tokio::sync::Semaphore::new(num_threads));
        let mut join_handles = Vec::new();

        for (global_index, image_path_str, appearance_count, explicit_vc) in export_items {
            if cancellation_token.load(Ordering::SeqCst) {
                break;
            }
            // The recipes still to export this photo (a paused job, or one
            // cancelled on its own, starts no further photo).
            let wanted: Vec<usize> = (0..targets.len())
                .filter(|&i| {
                    targets[i]
                        .tracking
                        .as_ref()
                        .is_none_or(|t| t.wants(global_index) && t.goes_on())
                })
                .collect();
            if wanted.is_empty() {
                continue;
            }
            let permit = semaphore.clone().acquire_owned().await.unwrap();
            // Memory pressure: a photo starts only while the reserve is
            // free, or when no other photo is in flight.
            while semaphore.available_permits() + 1 < num_threads
                && !cancellation_token.load(Ordering::SeqCst)
                && {
                    sys.refresh_memory();
                    sys.available_memory() < rawtonic_export::workers::EXPORT_MEMORY_RESERVE
                }
            {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
            if cancellation_token.load(Ordering::SeqCst) {
                drop(permit);
                break;
            }
            // Admitted only into jobs still going on (one paused or
            // cancelled while this photo waited starts it not), reserved
            // atomically so such a job cannot settle before it is done.
            let wanted = admitted(wanted, |i| {
                targets[i].tracking.as_ref().is_none_or(|t| t.admit())
            });
            if wanted.is_empty() {
                drop(permit);
                continue;
            }

            let app_handle_clone = app_handle.clone();
            let context_clone = Arc::clone(&context);
            let progress_counter_clone = Arc::clone(&progress_counter);
            let targets = Arc::clone(&targets);
            let settings = settings.clone();
            let cancellation_token_clone = Arc::clone(&cancellation_token);
            let photos = photos.clone();
            let used_paths_clone = Arc::clone(&used_paths);
            let lane = lanes.take();

            let handle = tokio::task::spawn_blocking(move || {
                let results = lane.install(|| {
                    // Under the library the photo was submitted with: what
                    // its export still reads or stores (the EXIF its decode
                    // caches) goes there, never to the library open now.
                    let pinned = (photos.read)(global_index);
                    let owner = pinned.as_ref().ok().and_then(|p| p.owner.clone());
                    crate::session_bridge::with_culling_owner(owner.as_deref(), || {
                        // A retry exports the same snapshot of the photo
                        // into the files the first attempt reserved.
                        let planned = PlannedFiles::default();
                        // After a lost device the photo goes once more on
                        // the new one the app starts (DVL2); later photos
                        // take the newest device (`export_context`).
                        let attempt = |context: &Arc<GpuContext>, wanted: &[usize]| {
                        rawtonic_runtime::gpu_memory::retry_out_of_memory(
                            wanted,
                            |wanted| {
                                export_photo(
                                    &app_handle_clone,
                                    context,
                                    &settings,
                                    &targets,
                                    wanted,
                                    photos.open_path.as_deref(),
                                    pinned.clone(),
                                    &used_paths_clone,
                                    &planned,
                                    &cancellation_token_clone,
                                    PhotoItem {
                                        global_index,
                                        image_path_str: &image_path_str,
                                        appearance_count,
                                        explicit_vc,
                                        total_paths,
                                    },
                                )
                            },
                            |target| planned.unpublished(target),
                            |again, wait| {
                                // A lost device is no memory shortage: it goes again
                                // on the new device (`retry_after_device_loss`).
                                if rawtonic_gpu::device_start::device_lost() {
                                    return false;
                                }
                                log::warn!(
                                    "Export: the graphics card ran out of memory for {} recipe(s) of {image_path_str}; again in {wait:?}",
                                    again.len()
                                );
                                release_gpu_memory(&app_handle_clone, context);
                                wait_unless_cancelled(wait, &cancellation_token_clone)
                            },
                        )
                        };
                        rawtonic_runtime::gpu_memory::retry_after_device_loss(
                            &wanted,
                            export_context(
                                &app_handle_clone.state::<crate::state::gpu::GpuState>(),
                                &context_clone,
                            ),
                            attempt,
                            |target| planned.unpublished(target),
                            |context, error| {
                                lost_under(
                                    &app_handle_clone.state::<crate::state::gpu::GpuState>(),
                                    context,
                                    error,
                                )
                            },
                            |context| {
                                device_after_loss(
                                    &app_handle_clone.state::<crate::state::gpu::GpuState>(),
                                    context,
                                    &cancellation_token_clone,
                                    DEVICE_RECOVERY_WAIT,
                                )
                            },
                        )
                    })
                });
                drop(lane);

                let cancelled = cancellation_token_clone.load(Ordering::SeqCst);
                if !cancelled {
                    for _ in &results {
                        let current_progress =
                            progress_counter_clone.fetch_add(1, Ordering::SeqCst) + 1;
                        let _ = app_handle_clone.emit(
                            "batch-export-progress",
                            serde_json::json!({
                                "current": current_progress,
                                "total": total_outputs,
                                "path": &image_path_str
                            }),
                        );
                    }
                }

                drop(permit);
                // A photo completed before the cancel is recorded as done;
                // an unfinished one stays for a resume.
                results
                    .into_iter()
                    .map(|(target, result)| {
                        if let Some(tracking) = &targets[target].tracking
                            && (result.is_ok() || !cancelled)
                        {
                            tracking.finished(global_index, &result);
                        }
                        if cancelled {
                            Err("Export cancelled".to_string())
                        } else {
                            result
                        }
                    })
                    .collect::<Vec<_>>()
            });

            join_handles.push(handle);
        }

        let mut results = Vec::new();
        for handle in join_handles {
            match handle.await {
                Ok(res) => results.extend(res),
                Err(e) => results.push(Err(crashed_export_error(
                    &e.to_string(),
                    rawtonic_gpu::device_start::device_lost(),
                ))),
            }
        }

        if release_after_run(
            &app_handle
                .state::<crate::state::gpu::GpuState>()
                .export_processor,
            prewarm,
            // Under a compile limit the processor is not kept for the next
            // run: every later compile (4–6 GB there) would come on top of
            // its textures (G76/WCC9). The next run rebuilds it; its
            // pipelines come from the pipeline cache.
            || {
                crate::memory_pressure::under_pressure()
                    || rawtonic_gpu::gpu_processing::compiles_limited()
            },
        )
        .await
        {
            log::info!(
                "Released the exports' GPU processor after the run (memory pressure or compile limit)"
            );
        }
        *crate::state::lock::lock_recover(
            &app_handle
                .state::<crate::state::export::ExportState>()
                .last_run_end,
        ) = Some(std::time::Instant::now());
        // Render and encode buffers of the last exports.
        crate::allocator::release_freed_memory();
        let errors: Vec<String> = results.into_iter().filter_map(Result::err).collect();
        let error_count = errors.len();
        if cancellation_token.load(Ordering::SeqCst) {
            for tracking in targets.iter().filter_map(|target| target.tracking.as_ref()) {
                tracking.cancelled.store(true, Ordering::SeqCst);
            }
        }
        // A job paused or cancelled on its own leaves photos unexported:
        // the run did not complete.
        let paused = stopped_short(
            targets
                .iter()
                .filter_map(|target| target.tracking.as_ref())
                .map(|t| t.goes_on()),
        );
        let exports = app_handle.state::<ExportState>();
        let finalized = finish_export_task(&exports.task_token, &cancellation_token, |cancelled| {
            if cancelled {
                log::info!("Batch export cancelled and worker cleanup completed");
                let _ = app_handle.emit("export-cancelled", ());
                return;
            }
            // The queue panel lists the failures of a paused job.
            if paused {
                let _ = app_handle.emit("export-paused", ());
                return;
            }

            for error in &errors {
                log::error!("Export error: {}", error);
                if total_outputs == 1 {
                    let _ = app_handle.emit("export-error", error.clone());
                }
            }

            if error_count > 0 && total_outputs > 1 {
                let _ = app_handle.emit(
                    "export-error",
                    format!("{error_count} of {total_outputs} exports failed"),
                );
            } else if error_count == 0 {
                let _ = app_handle.emit(
                        "batch-export-progress",
                        serde_json::json!({ "current": total_outputs, "total": total_outputs, "path": "" }),
                    );
                let _ = app_handle.emit("export-complete", ());
            }
        });

        if !finalized {
            log::warn!("Ignoring terminal events from a stale export task");
        }

        if let Some(tx) = completion_tx {
            if error_count > 0 {
                let _ = tx.send(Err(error_count));
            } else {
                let _ = tx.send(Ok(()));
            }
        }
    });

    Ok(())
}

/// The context a photo of an export runs on: the app's current one, which
/// is a new device after a loss (DVL2, `gpu_recovery`), else the run's.
fn export_context(gpu: &crate::state::gpu::GpuState, run: &Arc<GpuContext>) -> Arc<GpuContext> {
    match gpu.context() {
        Some(current) if !Arc::ptr_eq(&current.device, &run.device) => Arc::new(current),
        _ => Arc::clone(run),
    }
}

/// Whether an export on `context` failed with `error` because its device
/// was lost: the lost error, or any error once the app replaced the device
/// (its objects fail with whatever they meet first).
fn lost_under(gpu: &crate::state::gpu::GpuState, context: &GpuContext, error: &str) -> bool {
    error.contains(rawtonic_runtime::gpu_memory::GPU_DEVICE_LOST)
        || rawtonic_gpu::device_start::device_lost()
        || gpu
            .context()
            .is_some_and(|current| !Arc::ptr_eq(&current.device, &context.device))
}

/// The new device's context once the app replaced the lost `context`'s
/// (`gpu_recovery`), waiting up to [`DEVICE_RECOVERY_WAIT`]; `None` when
/// none came (the recovery gave up) or the export was cancelled.
fn device_after_loss(
    gpu: &crate::state::gpu::GpuState,
    context: &Arc<GpuContext>,
    cancelled: &std::sync::atomic::AtomicBool,
    wait: std::time::Duration,
) -> Option<Arc<GpuContext>> {
    let started = std::time::Instant::now();
    log::warn!("Export: the graphics device was lost; waiting for the new one to export again");
    while started.elapsed() < wait {
        if cancelled.load(Ordering::SeqCst) {
            return None;
        }
        if let Some(current) = gpu.context()
            && !Arc::ptr_eq(&current.device, &context.device)
            && !rawtonic_gpu::device_start::device_lost()
        {
            log::info!(
                "Export: exporting again on the new graphics device after {:?}",
                started.elapsed()
            );
            // As at the export's start: the new device's idle compiles take
            // the shown photo's variant first, which the export waits for.
            rawtonic_gpu::gpu_processing::export_of_shown_photo_starts();
            return Some(Arc::new(current));
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    log::warn!("Export: no new graphics device within {wait:?}");
    None
}

/// How long an export waits for the app's new device after a loss: the
/// recovery's back-off (10 s) and a device start, with room.
pub(crate) const DEVICE_RECOVERY_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

/// `render` on `context`, where a panic after the device was lost (wgpu
/// panics in `Device::poll` while reporting a driver reset during a
/// readback) is the lost error, so the retry classifies it; any other
/// panic goes on.
fn render_catching_loss<T>(
    gpu: &crate::state::gpu::GpuState,
    context: &Arc<GpuContext>,
    render: &mut impl FnMut(&Arc<GpuContext>) -> Result<T, String>,
) -> Result<T, String> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| render(context))) {
        Ok(result) => result,
        // Lost for this attempt's device also when the app already started
        // a replacement (`lost_under`).
        Err(_) if lost_under(gpu, context, "") => {
            log::warn!("Export: a render panicked after the graphics device was lost");
            Err(rawtonic_runtime::gpu_memory::GPU_DEVICE_LOST.to_string())
        }
        Err(panic) => std::panic::resume_unwind(panic),
    }
}

/// One render of a single photo (a collage's cell) that goes once more on
/// the new device the app starts when the GPU device was lost under it
/// (DVL3; the queue export's retry, `retry_after_device_loss`, with the
/// same context, loss test and wait). `render` takes the context it
/// renders on and writes nothing until it succeeds.
pub(crate) fn render_after_device_loss<T>(
    gpu: &crate::state::gpu::GpuState,
    run: &Arc<GpuContext>,
    cancelled: &AtomicBool,
    wait: std::time::Duration,
    mut render: impl FnMut(&Arc<GpuContext>) -> Result<T, String>,
) -> Result<T, String> {
    let mut result = None;
    let mut results = rawtonic_runtime::gpu_memory::retry_after_device_loss(
        &[()],
        export_context(gpu, run),
        |context, _| {
            vec![(
                (),
                render_catching_loss(gpu, context, &mut render).map(|value| result = Some(value)),
            )]
        },
        |()| true,
        |context, error| lost_under(gpu, context, error),
        |context| device_after_loss(gpu, context, cancelled, wait),
    );
    match results.pop() {
        Some((_, Ok(()))) => result.ok_or_else(|| "the render gave nothing".to_string()),
        Some((_, Err(error))) => Err(error),
        None => Err("the render gave nothing".to_string()),
    }
}

/// What an export whose thread panicked reports: after a lost GPU device
/// the clear device-lost error (G76/WCC6: the panic named only an invalid
/// buffer), otherwise the panic.
fn crashed_export_error(panic: &str, device_lost: bool) -> String {
    if device_lost {
        rawtonic_runtime::gpu_memory::GPU_DEVICE_LOST.to_string()
    } else {
        format!("Thread crashed: {panic}")
    }
}

/// The recipes of `wanted` whose jobs admit the photo.
fn admitted(wanted: Vec<usize>, mut admit: impl FnMut(usize) -> bool) -> Vec<usize> {
    wanted.into_iter().filter(|&i| admit(i)).collect()
}

/// Whether a run ended short of its photos without being cancelled as a
/// whole: some job was paused or cancelled on its own (not "complete").
fn stopped_short(jobs_going_on: impl IntoIterator<Item = bool>) -> bool {
    jobs_going_on.into_iter().any(|goes_on| !goes_on)
}

/// A photo of an export run and its place in it.
pub(crate) struct PhotoItem<'a> {
    pub(crate) global_index: usize,
    pub(crate) image_path_str: &'a str,
    pub(crate) appearance_count: usize,
    pub(crate) explicit_vc: Option<u32>,
    pub(crate) total_paths: usize,
}

/// The files a photo's recipes were given, by recipe: a retry of the
/// photo exports into the same ones instead of reserving new names (which
/// would be `_1` files, its own reservation taken).
#[derive(Default)]
struct PlannedFiles(Mutex<PlannedState>);

#[derive(Default)]
struct PlannedState {
    planned: std::collections::HashMap<usize, Planned>,
    /// Recipes whose file was published (a replaced file existed before).
    published: std::collections::HashSet<usize>,
}

impl PlannedFiles {
    /// The plan of `target`: the one made before, or `plan`'s.
    fn once(
        &self,
        target: usize,
        plan: impl FnOnce() -> Result<Planned, String>,
    ) -> Result<Planned, String> {
        if let Some(planned) = self.0.lock().unwrap().planned.get(&target) {
            return Ok(planned.clone());
        }
        let planned = plan()?;
        self.0
            .lock()
            .unwrap()
            .planned
            .insert(target, planned.clone());
        Ok(planned)
    }

    fn get(&self, target: usize) -> Option<Planned> {
        self.0.lock().unwrap().planned.get(&target).cloned()
    }

    fn published(&self, target: usize) {
        self.0.lock().unwrap().published.insert(target);
    }

    /// Whether `target`'s file is not published (or not planned yet): only
    /// then can the recipe be exported again. Once its file is there its
    /// mask files may have failed; that stays a failure as before. A
    /// skipped recipe is never exported again.
    fn unpublished(&self, target: usize) -> bool {
        let state = self.0.lock().unwrap();
        if state.published.contains(&target) {
            return false;
        }
        match state.planned.get(&target) {
            None => true,
            Some(Planned::Skip { .. }) => false,
            Some(Planned::Write {
                replace: Some(_), ..
            }) => true,
            Some(Planned::Write { path, .. }) => !path.exists(),
        }
    }
}

/// Frees what the app itself holds on the graphics card and can rebuild:
/// the exports' processor when no render holds it (its textures and
/// pipelines), the scopes' uploads and the Develop view's upload of its
/// photo (the next frame uploads it again). Renders in progress keep
/// theirs. (A processor a render holds is rebuilt at its next use anyway:
/// the failed render marked it, `ProcessorSlot::scoped`.)
fn release_gpu_memory(app_handle: &tauri::AppHandle, context: &GpuContext) {
    let gpu = app_handle.state::<crate::state::gpu::GpuState>();
    let processor = gpu.export_processor.take_if_idle().is_some();
    let caches = std::iter::once(&gpu.scope_cache)
        .chain(&gpu.scope_extremes_cache)
        .chain(std::iter::once(
            &app_handle.state::<DevelopSession>().gpu_image_cache,
        ))
        .filter(|cache| {
            cache
                .try_lock()
                .is_ok_and(|mut cache| cache.take().is_some())
        })
        .count();
    // wgpu frees dropped resources once the device is polled.
    let _ = context.device.poll(wgpu::PollType::wait_indefinitely());
    log::info!(
        "Export: freed GPU memory before a retry (export processor: {processor}, uploads: {caches})"
    );
}

/// Sleeps `wait` unless the export is cancelled meanwhile (`false` then).
fn wait_unless_cancelled(wait: std::time::Duration, cancellation_token: &AtomicBool) -> bool {
    let step = std::time::Duration::from_millis(50);
    let start = std::time::Instant::now();
    while start.elapsed() < wait {
        if cancellation_token.load(Ordering::SeqCst) {
            return false;
        }
        std::thread::sleep(step.min(wait.saturating_sub(start.elapsed())));
    }
    !cancellation_token.load(Ordering::SeqCst)
}

/// The libraries the app knows besides the one managing a folder: a file
/// any of them lists is a photo, never replaced.
pub(crate) fn known_libraries(
    settings: &crate::app_settings::AppSettings,
) -> Vec<std::path::PathBuf> {
    settings
        .recent_libraries
        .iter()
        .flatten()
        .map(std::path::PathBuf::from)
        .chain(crate::session_bridge::active_library_directory())
        .collect()
}

/// What decides a photo's exports besides each recipe, for Replace if
/// changed: the photo as queued (its recipe, library metadata, corrected
/// capture time and the original's file), the preferences the render reads
/// and the LUT file it reads besides the photo. Not the EXIF cache: it is
/// the original's own EXIF (the file is in `sourceIdentity`), cached only
/// after a first decode, which would count as a change.
pub(crate) fn photo_inputs(
    pinned: &PinnedPhoto,
    settings: &crate::app_settings::AppSettings,
    source: &Path,
    is_raw: bool,
) -> Value {
    use rawtonic_engine::develop_defaults;
    // A saved, untouched photo still follows its camera's Develop defaults.
    // Include the resolved values the render uses, including a default LUT.
    let defaults = settings.develop_defaults.as_ref().and_then(|defaults| {
        if defaults.is_empty() || !develop_defaults::follows_defaults(&pinned.adjustments) {
            return None;
        }
        let camera = (!defaults.cameras.is_empty() && !rawtonic_fs::is_cloud_placeholder(source))
            .then(|| develop_defaults::camera_of(source))
            .flatten();
        develop_defaults::resolve(defaults, is_raw, camera.as_ref())
    });
    let lut_key = effects::LUT_PATH.into_name();
    let lut_file = defaults
        .as_ref()
        .and_then(|defaults| defaults.settings.get(lut_key))
        .unwrap_or(&pinned.adjustments[lut_key])
        .as_str()
        .filter(|path| !path.is_empty())
        .map(|path| existing_files::file_fingerprint(Path::new(path)));
    let mut parts = serde_json::json!({
        "metadata": &pinned.metadata,
        "capture": &pinned.capture,
        "sourceIdentity": &pinned.source_identity,
        "lutFile": lut_file,
        "decode": crate::image_loader::decode_preferences(settings),
        // The photo's own (RAW or not), and the non-RAW one a LUT export uses.
        "toneMapper": [
            crate::image_processing::resolve_tonemapper_override(settings, is_raw),
            crate::image_processing::resolve_tonemapper_override(settings, false),
        ],
    });
    parts["adjustments"] = existing_files::adjustments_for_hash(&pinned.adjustments, source);
    if let Some(defaults) = defaults.filter(|defaults| !defaults.settings.is_empty()) {
        parts["developDefaults"] = Value::Object(defaults.settings);
    }
    parts
}

/// A recipe's inputs for one photo (`photo_inputs`): the recipe's settings
/// and format, the watermark image it reads, under the app's version.
pub(crate) fn target_inputs_hash(photo_inputs: &Value, target: &ExportTarget) -> String {
    let export_settings = &target.export_settings;
    let watermark_file = export_settings
        .watermark
        .as_ref()
        .filter(|watermark| watermark.text.is_none() && !watermark.path.is_empty())
        .map(|watermark| existing_files::file_fingerprint(Path::new(&watermark.path)));
    existing_files::inputs_hash(
        env!("CARGO_PKG_VERSION"),
        &serde_json::json!({
            "photo": photo_inputs,
            "format": target.output_format.to_lowercase(),
            "settings": existing_files::settings_for_hash(export_settings),
            "watermarkFile": watermark_file,
        }),
    )
}

/// What a recipe does for a photo, decided before anything is rendered
/// (the recipe's "If a file exists" choice, `rawtonic_export::existing_files`).
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Planned {
    /// Write the file at `path`: a new one, or over the previous export of
    /// the photo recorded as `replace`. `kept`: why Replace if changed
    /// kept another file at the recipe's name and numbered this one
    /// ([`Outcome::KeptBoth`], [`Outcome::KeptOriginal`]).
    Write {
        path: std::path::PathBuf,
        slot: std::path::PathBuf,
        replace: Option<Recorded>,
        kept: Option<Outcome>,
    },
    /// Not exported: the file at `path` stays as it is.
    Skip {
        path: std::path::PathBuf,
        outcome: Outcome,
    },
}

impl Planned {
    pub(crate) fn path(&self) -> &Path {
        match self {
            Planned::Write { path, .. } | Planned::Skip { path, .. } => path,
        }
    }
    /// How a successful export of this plan is reported.
    pub(crate) fn outcome(&self) -> Outcome {
        match self {
            Planned::Skip { outcome, .. } => *outcome,
            Planned::Write {
                replace: Some(_), ..
            } => Outcome::Replaced,
            Planned::Write {
                kept: Some(kept), ..
            } => *kept,
            Planned::Write { .. } => Outcome::Written,
        }
    }
}

/// The file a recipe writes for a photo: its name from the recipe's
/// template, in the recipe's folder, never over another output of this run
/// and over an existing file only as the recipe's "If a file exists"
/// choice allows (`History::decide`: by default a numbered name).
/// With separate mask files the name holds for its `mask_layers` layers'
/// files too, which are reserved with it: recipes of photos exported side
/// by side would otherwise take the name of a mask file not written yet.
/// `source` is the photo (a virtual copy's path), `inputs` this export's
/// `existing_files::inputs_hash`, and `history` reads previous exports.
#[allow(clippy::too_many_arguments)]
pub(crate) fn plan_output_path<'a>(
    target: &ExportTarget,
    item: &PhotoItem,
    source_path: &Path,
    file_date: &jiff::Timestamp,
    capture: Option<&rawtonic_naming::Capture>,
    mask_layers: usize,
    used_paths: &Mutex<std::collections::HashSet<std::path::PathBuf>>,
    source: &str,
    inputs: impl Fn() -> &'a str,
    history: &History<'_>,
    create_folders: bool,
) -> Result<Planned, String> {
    let export_settings = &target.export_settings;
    let output_folder_path = Path::new(&target.output_folder_or_file);
    let total_paths = item.total_paths;
    let (sequence, last_sequence) =
        export_settings.filename_counter(item.global_index, total_paths);
    let naming = export_settings.naming_context(
        source_path,
        sequence,
        last_sequence,
        file_date,
        &target.output_format,
        capture,
    );
    let new_stem = rawtonic_export::sidecars::export_stem(
        &export_settings.naming_template(),
        &naming,
        item.appearance_count,
        item.explicit_vc,
    )?;

    let new_filename = format!("{}.{}", new_stem, target.output_format);

    let beside_original = export_settings.destination_type.as_deref() == Some("originalFolder");
    let explicit = target.is_explicit_file_path && total_paths == 1;
    let slot = if !beside_original && explicit {
        output_folder_path.to_path_buf()
    } else {
        let dir = export_directory(
            source_path,
            output_folder_path,
            export_settings,
            &target.base_origin_folders,
            &naming,
        )?;
        if create_folders
            && dir != output_folder_path
            && let Err(e) = std::fs::create_dir_all(&dir)
        {
            return Err(format!(
                "Failed to create export subdirectory '{}': {}",
                dir.display(),
                e
            ));
        }
        dir.join(&new_filename)
    };

    let extension = target.output_format.to_lowercase();
    let policy = export_settings.existing_files;
    let mask_layers = if export_settings.export_masks {
        mask_layers
    } else {
        0
    };
    let family = |output_path: &Path| {
        std::iter::once(output_path.to_path_buf())
            .chain((0..mask_layers).flat_map(|layer| mask_file_paths(output_path, layer)))
            .collect::<Vec<_>>()
    };

    let mut used = used_paths.lock().unwrap();
    let taken = |path: &Path| family(path).iter().any(|path| used.contains(path));
    // Save As checks the file the user named. Numbered names still reserve
    // their entire family, as they do for folder exports.
    let exists = if explicit {
        slot.exists()
    } else {
        family(&slot).iter().any(|path| path.exists())
    };
    let (mut output_path, decision) = history.decide(
        policy,
        &slot,
        exists,
        !explicit && taken(&slot),
        mask_layers > 0,
        source,
        inputs,
        taken,
    );
    let (replace, kept) = match decision {
        Decision::Skip(outcome) => {
            if outcome == Outcome::SkippedUnchanged && output_path != slot {
                used.extend(family(&output_path));
            }
            return Ok(Planned::Skip {
                path: output_path,
                outcome,
            });
        }
        Decision::Write => (None, None),
        Decision::Replace(recorded) => (Some(recorded), None),
        Decision::Number { reported } => {
            let (stem, extension) = if explicit {
                (
                    slot.file_stem().unwrap_or_default().to_string_lossy(),
                    slot.extension()
                        .map_or_else(|| extension.as_str().into(), |ext| ext.to_string_lossy()),
                )
            } else {
                (new_stem.as_str().into(), extension.as_str().into())
            };
            let mut counter = 1;
            while family(&output_path)
                .iter()
                .any(|path| path.exists() || used.contains(path))
            {
                output_path = slot.with_file_name(format!("{stem}_{counter}.{extension}"));
                counter += 1;
            }
            (None, reported)
        }
    };
    if !explicit || output_path != slot {
        used.extend(family(&output_path));
    }
    Ok(Planned::Write {
        path: output_path,
        slot,
        replace,
        kept,
    })
}

/// Keeps the selections an export (or the selection queue) computed in the
/// photo's stored recipe, as opening it in Develop would
/// (`ai_selections::keep_detected`), so the next export or the editor uses
/// the same mask. Only components still as the export found them, in the
/// same geometry, take it. Returns whether the recipe changed. The photo open in Develop is left
/// to the editor, which computes and saves its own.
///
/// Only into a stored recipe that `accepts` (read in the same turn as the
/// write: a recipe changed since the detection began, e.g. given AI Denoise,
/// is checked as it is now).
pub(crate) fn keep_detected_selections(
    path: &str,
    detected: &[rawtonic_engine::ai_selections::Detected],
    accepts: impl Fn(&Value) -> bool,
) -> Result<bool, String> {
    if detected.is_empty() {
        return Ok(false);
    }
    let (source_path, sidecar_path) = parse_virtual_path(path);
    let _turn = crate::file_management::sidecar_field_edit();
    let mut metadata = crate::exif_processing::load_sidecar_with_exif(&sidecar_path, &source_path);
    if !accepts(&metadata.adjustments) {
        return Ok(false);
    }
    // An edit still to be moved from a camera TIFF's thumbnail to its full
    // frame (`recipe_frame::conform`, done when the user saves): a bitmap of
    // the full frame in it would read as made on the full frame, so nothing
    // is kept; its export computes the selections again.
    if needs_frame_migration(&metadata.adjustments, &source_path) {
        return Ok(false);
    }
    let frame = detection_frame(&metadata.adjustments, &source_path);
    if !rawtonic_engine::ai_selections::keep_detected_in(
        &mut metadata.adjustments,
        &frame,
        detected,
    ) {
        return Ok(false);
    }
    let json = serde_json::to_string_pretty(&metadata).map_err(|e| e.to_string())?;
    crate::session_bridge::write_editor_sidecar(&sidecar_path, json)
        .map_err(|e| format!("the selections could not be kept: {e}"))?;
    Ok(true)
}

/// Keeps an export's selections only while the saved recipe still asks for
/// the same effective AI Denoise settings as the decode used by the export.
pub(crate) fn keep_export_detected_selections(
    path: &str,
    detected: &[rawtonic_engine::ai_selections::Detected],
    exported_recipe: &Value,
) -> Result<bool, String> {
    use rawtonic_engine::image_loader::ai_denoise_settings;
    let denoise = ai_denoise_settings(exported_recipe);
    keep_detected_selections(path, detected, |now| ai_denoise_settings(now) == denoise)
}

/// The geometry of `adjustments` as a detection saw it: prepared for a
/// render as the export and the selection queue prepare it (version keys,
/// the resolved lens), which a never-opened recipe may not hold yet.
pub(crate) fn detection_frame(adjustments: &Value, source_path: &Path) -> Value {
    let mut frame = adjustments.clone();
    let _ = rawtonic_export::normalize::normalize(
        &mut frame,
        &rawtonic_export::normalize::RecipeSource::file(source_path),
        rawtonic_export::normalize::Bitmaps::None,
        rawtonic_export::normalize::Defaults::Render,
    );
    frame
}

/// Whether `adjustments` is an edit of a camera TIFF still made on its
/// thumbnail, which opening or rendering moves to the full frame
/// (`recipe_frame::conform`) without writing it.
pub(crate) fn needs_frame_migration(adjustments: &Value, source: &Path) -> bool {
    rawtonic_engine::recipe_frame::conform(&mut adjustments.clone(), source)
}

/// What the naming tokens read about `original` for its export and rename
/// names: its header's facts and its XMP sidecar's (`source_facts`, as an
/// import reads them), with the rating, label, title, creator and copyright
/// the library holds (`metadata`) over the file's.
pub(crate) fn naming_capture(
    original: &Path,
    metadata: Option<&rawtonic_metadata::Metadata>,
) -> rawtonic_naming::Capture {
    rawtonic_ingest::naming_capture(original, metadata)
}

/// A photo's render `groups` (their recipes consecutive in `images`, the
/// order the recipes are saved in) reordered so that a group whose first
/// TIFF is a full-size one (`full_size`) renders for it: that TIFF is saved
/// first, its file written while the group renders, so no TIFF render is
/// held whole beside a JPEG's (`streaming_tiff_first`; notes:
/// benchmark-results/mem15-2026-11-17). It passes only files of other
/// formats, so every name stays; only in a group whose recipes all have the
/// same destination (a file could otherwise take the name of another
/// recipe's subfolder), and never when a recipe writes separate mask files
/// (mask files of any recipe may take each other's names).
fn render_groups_for_streaming_tiffs(
    targets: &[ExportTarget],
    images: &mut [usize],
    groups: &mut [Vec<usize>],
    full_size: impl Fn(usize) -> bool,
) {
    if images
        .iter()
        .any(|&target| targets[target].export_settings.export_masks)
    {
        return;
    }
    let is_tiff = |target: usize| {
        matches!(
            targets[target].output_format.to_ascii_lowercase().as_str(),
            "tif" | "tiff"
        )
    };
    let destination = |target: usize| {
        let (t, settings) = (&targets[target], &targets[target].export_settings);
        (
            &t.output_folder_or_file,
            &t.base_origin_folders,
            &settings.destination_type,
            &settings.subfolder,
            settings.preserve_folders,
        )
    };
    for group in groups {
        if group
            .iter()
            .any(|&target| destination(target) != destination(group[0]))
        {
            continue;
        }
        let Some(start) = images.iter().position(|&target| target == group[0]) else {
            continue;
        };
        streaming_tiff_first(group, is_tiff, |target| {
            is_tiff(target) && full_size(target)
        });
        images[start..start + group.len()].copy_from_slice(group);
    }
}

/// Each of `targets` in turn: its file is planned right before it is
/// exported, so it sees the files the recipes before it wrote (their mask
/// files too), as when each recipe was an export of its own.
fn export_in_turn(
    targets: impl IntoIterator<Item = usize>,
    mut plan: impl FnMut(usize) -> Result<Option<std::path::PathBuf>, String>,
    mut export: impl FnMut(usize, &Path) -> Result<(), String>,
) -> Vec<(usize, Result<(), String>)> {
    targets
        .into_iter()
        .map(|target| {
            let result = plan(target).and_then(|path| match path {
                Some(path) => export(target, &path),
                // Skipped: nothing to write.
                None => Ok(()),
            });
            (target, result)
        })
        .collect()
}

/// Exports one photo with the recipes `wanted` (indices into `targets`):
/// the source is loaded once and developed once per kind of main file (with
/// or without its mask layers), the recipes of each kind render together
/// (one main pass, encoded per recipe), then each recipe sizes, sharpens
/// and writes its file. Returns each recipe's result.
#[allow(clippy::too_many_arguments)]
fn export_photo(
    app_handle: &tauri::AppHandle,
    context: &Arc<GpuContext>,
    settings: &crate::app_settings::AppSettings,
    targets: &[ExportTarget],
    wanted: &[usize],
    open_path: Option<&str>,
    pinned: Result<PinnedPhoto, String>,
    used_paths: &Mutex<std::collections::HashSet<std::path::PathBuf>>,
    planned: &PlannedFiles,
    cancellation_token: &AtomicBool,
    item: PhotoItem,
) -> Vec<(usize, Result<(), String>)> {
    let fail_all = |error: String| {
        wanted
            .iter()
            .map(|&target| (target, Err(error.clone())))
            .collect::<Vec<_>>()
    };
    if let Err(e) = ensure_export_not_cancelled(cancellation_token) {
        return fail_all(e);
    }
    let trackings: Vec<_> = wanted
        .iter()
        .filter_map(|&target| targets[target].tracking.clone())
        .collect();
    for tracking in &trackings {
        tracking.began(item.global_index);
    }

    let state = app_handle.state::<AppState>();
    let gpu = app_handle.state::<crate::state::gpu::GpuState>();
    let develop = app_handle.state::<DevelopSession>();
    let image_path_str = item.image_path_str;
    let (source_path, _) = parse_virtual_path(image_path_str);
    let source_path_str = source_path.to_string_lossy().to_string();
    let is_current_edit = is_open_edit(image_path_str, open_path);
    // Only what the export was submitted with (`pin_photos`): no recipe or
    // metadata is read from a library now.
    let mut pinned = match pinned {
        Ok(pinned) => pinned,
        Err(e) => return fail_all(e),
    };
    let is_raw = is_raw_file(&source_path_str);
    let original_path = std::path::Path::new(&source_path_str);

    // What decides each recipe's file, for Replace if changed: the photo as
    // queued (its recipe, library metadata, corrected capture time and the
    // original's file), the preferences the render reads, the files it
    // reads besides the photo (a LUT, a watermark image) and the recipe's
    // settings, under the app's version. Not the EXIF cache: it is the
    // original's own EXIF (the file is in `sourceIdentity`), cached only
    // after a first decode, which would count as a change.
    let pinned_inputs = photo_inputs(&pinned, settings, original_path, is_raw);
    // Once per recipe, before it renders: the render reads these inputs
    // (the watermark file included) after this, and its record names them.
    let hash_inputs = |target: usize| target_inputs_hash(&pinned_inputs, &targets[target]);
    let inputs_of: Vec<std::cell::OnceCell<String>> =
        targets.iter().map(|_| std::cell::OnceCell::new()).collect();
    let inputs = |target: usize| {
        inputs_of[target]
            .get_or_init(|| hash_inputs(target))
            .as_str()
    };
    // The photo as its exports are recorded: its path and its original's
    // file, so another photo put at the same path is never taken for it.
    let photo_key = existing_files::photo_key(image_path_str, pinned.source_identity.as_deref());
    // A replacement is planned only with a key (`plan_target`).
    let photo = photo_key.as_deref().unwrap_or_default();
    // The libraries the app knows besides the one managing a folder: a file
    // any of them lists is a photo, never replaced.
    let known_libraries = known_libraries(settings);
    let records = std::cell::OnceCell::new();
    let records = || {
        records
            .get_or_init(|| open_export_records(app_handle))
            .as_ref()
    };
    let history = History {
        records: &records,
        is_original: &|path| crate::session_bridge::is_library_photo(path, &known_libraries),
    };
    // Named as before for every choice: the name may need the capture date
    // or facts (read from the original when the snapshot has none).
    let file_date = pinned.naming_date(original_path);
    let capture = wanted
        .iter()
        .any(|&t| targets[t].export_settings.naming_needs_capture())
        .then(|| naming_capture(original_path, pinned.metadata.as_ref()));

    // Each recipe's file is named (and recorded) right before it is
    // written, as when each recipe was its own export.
    let mask_layers =
        rawtonic_engine::image_processing::mask_definitions_from_json(&pinned.adjustments)
            .iter()
            .filter(|definition| definition.produces_bitmap())
            .take(crate::image_processing::MAX_MASKS)
            .count();
    let plan_target = |target: usize| {
        planned.once(target, || {
            let inputs = inputs(target);
            plan_output_path(
                &targets[target],
                &item,
                original_path,
                &file_date,
                capture.as_ref(),
                mask_layers,
                used_paths,
                photo,
                || inputs,
                &history,
                true,
            )
        })
    };
    let report_outcome = |target: usize, outcome: Outcome, path: &Path| {
        if let Some(tracking) = &targets[target].tracking {
            tracking.outcome(item.global_index, outcome, path);
        } else {
            log::debug!("Export outcome {}: {}", outcome.as_str(), path.display());
        }
    };
    let plan = |target: usize| {
        let planned = plan_target(target)?;
        let tracking = targets[target].tracking.as_ref();
        match &planned {
            Planned::Skip { path, outcome } => {
                report_outcome(target, *outcome, path);
                return Ok(None);
            }
            // The queue records the file only once it is replaced: a
            // restart before that must not take the old file for this
            // export's (`ExportQueue::recover`).
            Planned::Write {
                replace: Some(_), ..
            } => {
                if let Some(tracking) = tracking {
                    tracking.stage(item.global_index, Stage::Rendering, None);
                }
            }
            Planned::Write { path, .. } => {
                if let Some(tracking) = tracking {
                    tracking.started(item.global_index, path);
                }
            }
        }
        Ok(Some(planned.path().to_path_buf()))
    };
    // The previous export a recipe's file replaces, as planned.
    let replacing = |target: usize| match planned.get(target) {
        Some(Planned::Write { replace, .. }) => replace,
        _ => None,
    };
    // After every file of a recipe is written (its mask files too): the
    // main file recorded for a later Replace if changed, and how it ended
    // reported.
    let published = |target: usize, output_path: &Path, written: Option<Written>| {
        planned.published(target);
        let plan = planned.get(target);
        if let Some(Planned::Write { slot, .. }) = &plan
            && let (Some(records), Some(photo), Some(written)) =
                (records(), photo_key.as_deref(), written.as_ref())
            && let Err(e) =
                records.record_written(output_path, slot, photo, inputs(target), written)
        {
            log::warn!(
                "Export records: recording {} failed: {e}",
                output_path.display()
            );
        }
        let outcome = plan.map_or(Outcome::Written, |planned| planned.outcome());
        if outcome != Outcome::Written {
            report_outcome(target, outcome, output_path);
        }
    };
    // Recipes that skip their photo are decided before its source is
    // checked or decoded: a photo every recipe skips is not decoded (an
    // offline drive does not fail it). Only its name may read the
    // original's capture date or facts, as before.
    let mut results = Vec::new();
    let mut wanted = wanted.to_vec();
    wanted.retain(|&target| {
        if matches!(
            targets[target].export_settings.existing_files,
            ExistingFiles::KeepBoth | ExistingFiles::Ask
        ) {
            return true;
        }
        match plan_target(target) {
            Ok(Planned::Skip { .. }) => {
                results.push((target, plan(target).map(|_| ())));
                false
            }
            Ok(Planned::Write { .. }) => true,
            Err(e) => {
                results.push((target, Err(e)));
                false
            }
        }
    });
    if wanted.is_empty() {
        return results;
    }
    let fail_rest = |mut results: Vec<(usize, Result<(), String>)>, error: String| {
        results.extend(wanted.iter().map(|&target| (target, Err(error.clone()))));
        results
    };
    if let Some(error) = missing_source(&source_path) {
        return fail_rest(results, error);
    }
    if let Err(error) = require_local_tiff(&source_path) {
        return fail_rest(results, error);
    }
    // The file is another one than the user saw when the export was queued
    // (a sync tool replaced it): the recipe of the old image is not
    // exported from the new one. The rest of the queue goes on.
    if let Some(identity) = &pinned.source_identity
        && !crate::session_bridge::file_is(&source_path, identity)
    {
        return fail_rest(results, crate::session_bridge::FILE_REPLACED.to_string());
    }
    let mut js_adjustments = std::mem::take(&mut pinned.adjustments);

    // A never-edited photo exports with the camera's lens data,
    // as Develop shows it (profile corrections on by default).
    let normalized = rawtonic_export::normalize::normalize(
        &mut js_adjustments,
        &rawtonic_export::normalize::RecipeSource::file(&source_path),
        rawtonic_export::normalize::Bitmaps::Queued(&develop.patch_cache),
        rawtonic_export::normalize::Defaults::Render,
    );
    let versions = normalized.file_versions();
    if let Err(e) = normalized.lens {
        return fail_rest(results, e);
    }
    let is_cube = |target: usize| targets[target].output_format.to_lowercase() == "cube";
    results.extend(export_in_turn(
        wanted.iter().copied().filter(|&t| is_cube(t)),
        plan,
        |target, output_path| {
            let cube_bytes = export_adjustments_as_lut(
                &js_adjustments,
                &source_path_str,
                context,
                &develop,
                app_handle,
                cancellation_token,
                crate::image_processing::resolve_tonemapper_override(settings, false),
            )?;
            ensure_export_not_cancelled(cancellation_token)?;
            let previous = replacing(target);
            let written = publish_with(previous.as_ref(), photo)
                .write_with(output_path, |file| {
                    std::io::Write::write_all(file, &cube_bytes)
                })
                .map_err(|e| e.to_string())?;
            published(target, output_path, Some(written));
            Ok(())
        },
    ));
    let images: Vec<usize> = wanted.iter().copied().filter(|&t| !is_cube(t)).collect();
    if images.is_empty() {
        return results;
    }

    let source = (|| {
        // An offline RAW is shown from its preview proxy; exports
        // are made from the RAW only, never from preview data.
        // (A cloud file not downloaded yet is read, which
        // downloads it.)
        if is_raw && !crate::preview_proxy::source_readable(&source_path) {
            return Err(format!(
                "{} is offline: exporting needs the RAW file",
                source_path_str
            ));
        }
        // A queued export may outlive an editor image switch. Reuse
        // only the pristine pixels of this export's actual source
        // (not an offline photograph's 1:1 preview proxy).
        let offline_source = state
            .source
            .offline_source
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let cached_source = if is_current_edit {
            state
                .source
                .original_image
                .lock()
                .unwrap()
                .as_ref()
                .filter(|loaded| parse_virtual_path(&loaded.path).0 == source_path)
                .filter(|loaded| offline_source.as_deref() != Some(loaded.path.as_str()))
                // Developed for the decode variant and the AI Denoise
                // exported: the open photograph's may have changed since
                // this export was queued.
                .filter(|loaded| {
                    reusable_for_export(loaded, &js_adjustments, &source_path_str, settings)
                })
                .map(|loaded| (Arc::clone(&loaded.image), loaded.measures))
        } else {
            None
        };
        #[cfg(feature = "performance-profiling")]
        let source_stage = crate::performance::PerformanceStage::new("export_load_source");
        let (pristine_source, measures) = if let Some(source) = cached_source {
            source
        } else {
            // AI Denoise takes up to a minute per photo without a
            // GPU; the Export button says what it waits for.
            if is_raw
                && rawtonic_engine::image_loader::ai_denoise_settings(&js_adjustments).is_some()
            {
                let _ = app_handle.emit("export-denoising", image_path_str);
            }
            let (image, measures) = load_export_base_image(
                &app_handle.state::<AiState>(),
                &source_path_str,
                is_raw,
                &js_adjustments,
                settings,
                (!trackings.is_empty()).then(|| {
                    crate::export_queue::denoise_progress(trackings.clone(), item.global_index)
                }),
            )?;
            (Arc::new(image), measures)
        };
        #[cfg(feature = "performance-profiling")]
        drop(source_stage);
        Ok((pristine_source, measures))
    })();
    // AI selections the photo holds without their bitmap (it was never
    // opened: an import, a preset, pasted settings) are computed as the
    // editor computes them when it opens, and kept in its recipe.
    let source = source.and_then(|(pristine_source, measures)| {
        let detected = rawtonic_engine::ai_selections::detect_missing(
            &mut js_adjustments,
            &pristine_source,
            is_raw,
            Some(&crate::ai_commands::ExportSelections { app_handle }),
            &mut || {
                for tracking in &trackings {
                    tracking.stage(
                        item.global_index,
                        crate::export_queue::Stage::Selecting,
                        None,
                    );
                }
                let _ = app_handle.emit("export-selecting", image_path_str);
            },
        )
        .map_err(|e| format!("{source_path_str}: not exported, {e}"))?;
        ensure_export_not_cancelled(cancellation_token)?;
        // Only into a recipe still developed with the AI Denoise the
        // selections were computed on (one pasted onto it while this export
        // waited detects again on its own decode).
        if !is_current_edit
            && let Err(e) =
                keep_export_detected_selections(image_path_str, &detected, &js_adjustments)
        {
            // The export stands; its next export computes them again.
            log::warn!("Export: keeping the AI selections of {image_path_str} failed: {e}");
        }
        let source =
            prepare_export_source(&source_path_str, pristine_source, measures, &js_adjustments)?
                .in_library(crate::photo_library::library_of_str(&source_path_str));
        ensure_export_not_cancelled(cancellation_token)?;
        Ok(source)
    });
    let source = match source {
        Ok(source) => source,
        Err(e) => {
            results.extend(images.into_iter().map(|target| (target, Err(e.clone()))));
            return results;
        }
    };

    // The preferences of the run's snapshot (`settings`), which its records
    // name too, not ones changed while it runs.
    let tonemapper_override =
        |is_raw| crate::image_processing::resolve_tonemapper_override(settings, is_raw);
    let load_lut = |path: &str| {
        get_or_load_lut(&app_handle.state::<crate::state::luts::LutState>(), path).ok()
    };
    let env = RenderEnv {
        context,
        processor: &gpu.export_processor,
        image_cache: &develop.gpu_image_cache,
        tonemapper_override: &tonemapper_override,
        load_lut: &load_lut,
        render_inputs: None,
    };
    let enlarger = crate::super_resolution::ExportEnlarger { app_handle };
    // Loaded only when a recipe of this photo asks for a manifest.
    let provenance = targets
        .iter()
        .any(|target| target.export_settings.provenance.is_some())
        .then(|| crate::provenance::identity_for_export(app_handle))
        .flatten();
    // The develop of each kind of main file the recipes want: with its
    // mask layers, or without them when they go to separate files.
    let with_mask_files = |target: usize| targets[target].export_settings.export_masks;
    let mut prepared: [Option<Result<PreparedExport, String>>; 2] = [None, None];
    // Only separate mask files need source pixels after rendering; else the
    // last develop takes the source, so it is freed once on the GPU.
    let wants_mask_source = images.iter().any(|&target| with_mask_files(target));
    let first_of_kind = |index: usize| {
        images
            .iter()
            .position(|&other| with_mask_files(other) == with_mask_files(images[index]))
            == Some(index)
    };
    let last_develop = (0..images.len()).rev().find(|&index| first_of_kind(index));
    let mut source = Some(source);
    for (index, &target) in images.iter().enumerate() {
        let masks = with_mask_files(target);
        if prepared[masks as usize].is_none() {
            #[cfg(feature = "performance-profiling")]
            let _stage = crate::performance::PerformanceStage::new("export_prepare");
            let this_source = if !wants_mask_source && Some(index) == last_develop {
                source.take()
            } else {
                source.clone()
            }
            .expect("the source until the last develop");
            prepared[masks as usize] = Some(prepare_export_render(
                &source_path_str,
                this_source,
                &main_export_adjustments(&js_adjustments, &targets[target].export_settings),
                &versions,
                &env,
                is_raw,
            ));
        }
    }
    // Recipes of one develop render together: one main pass, encoded for
    // each recipe's colour space and precision (notes: parity-gaps/G43).
    // A file the user named takes its precision from its own extension,
    // known only once it is planned: it renders on its own. Groups are
    // consecutive in the order the recipes are saved and render right
    // before their first recipe is saved: a photo keeps at most one group
    // (`render_groups`, `GroupRenders`).
    let alone = |target: usize| targets[target].is_explicit_file_path && item.total_paths == 1;
    let together =
        |a: usize, b: usize| with_mask_files(a) == with_mask_files(b) && !alone(a) && !alone(b);
    let precision = |target: usize| {
        render_output_precision(
            &targets[target].output_format,
            &targets[target].export_settings,
        )
    };
    let mut groups: Vec<Vec<usize>> = Vec::new();
    for run in images.chunk_by(|&a, &b| together(a, b)) {
        let Some(Ok(prepared)) = &prepared[with_mask_files(run[0]) as usize] else {
            continue;
        };
        if alone(run[0]) {
            continue;
        }
        let precisions: Vec<_> = run.iter().map(|&target| precision(target)).collect();
        groups.extend(
            render_groups(prepared.size(), &precisions)
                .into_iter()
                .filter(|group| group.len() > 1)
                .map(|group| run[group].to_vec()),
        );
    }
    let mut images = images;
    render_groups_for_streaming_tiffs(targets, &mut images, &mut groups, |target| {
        matches!(
            &prepared[with_mask_files(target) as usize],
            Some(Ok(prepared))
                if render_is_file(&targets[target].export_settings, prepared.size())
        )
    });
    let mut rendered = GroupRenders::new(groups);
    let mask_source = source.filter(|_| wants_mask_source);

    let exif_sidecar = || pinned.exif(original_path);
    let catalog = || pinned.metadata.clone();
    results.extend(export_in_turn(images, plan, |target, output_path| {
        let export_settings = &targets[target].export_settings;
        let previous = replacing(target);
        // What was written, as its record certifies it.
        let written = Mutex::new(None);
        let write_export =
            |path: &Path,
             _extension: &str,
             encoded: rawtonic_export::export_processing::EncodedExport| {
                let file = publish_with(previous.as_ref(), photo).write_export(path, encoded)?;
                *lock_recover(&written) = Some(file);
                Ok(())
            };
        {
            let prepared = prepared[export_settings.export_masks as usize]
                .as_ref()
                .expect("prepared for every wanted recipe")
                .as_ref()
                .map_err(Clone::clone)?;
            ensure_export_not_cancelled(cancellation_token)?;
            #[cfg(feature = "qa-control")]
            crate::qa_crash::gpu_out_of_memory()?;
            let output = ExportOutput {
                path: output_path,
                format: targets[target].output_format.as_str(),
                exif_sidecar: &exif_sidecar,
                catalog: &catalog,
                write: &write_export,
                enlarger: Some(&enlarger),
                provenance: provenance.as_deref(),
            };
            let cancelled = || ensure_export_not_cancelled(cancellation_token);
            // A group renders at the first of its recipes still to be
            // saved (earlier ones may have been skipped), whose file is
            // then written while it renders when that file is a TIFF of the
            // render unchanged (`save_group_rendering`).
            let render = rendered.take(target, |group| {
                let outputs: Vec<_> = group
                    .iter()
                    .map(|&target| {
                        (
                            precision(target),
                            targets[target].export_settings.color_space,
                        )
                    })
                    .collect();
                #[cfg(feature = "performance-profiling")]
                let _stage = crate::performance::PerformanceStage::new("export_render");
                let own = group
                    .iter()
                    .position(|&member| member == target)
                    .unwrap_or(group.len());
                save_group_rendering(
                    &source_path_str,
                    prepared,
                    &js_adjustments,
                    export_settings,
                    &env,
                    &output,
                    &cancelled,
                    "process_image_for_export",
                    &outputs,
                    own,
                )
            });
            match render {
                Some(processed) => match processed? {
                    GroupRender::Saved => {}
                    GroupRender::Failed(e) => return Err(e),
                    GroupRender::Image(processed) => save_rendered_export(
                        processed,
                        &source_path_str,
                        &js_adjustments,
                        export_settings,
                        &output,
                        &cancelled,
                    )?,
                },
                None => render_prepared_and_save(
                    &source_path_str,
                    prepared,
                    &js_adjustments,
                    export_settings,
                    &env,
                    &output,
                    &cancelled,
                )?,
            }

            // The photo's file is published: its other outputs are
            // finished too, cancelled or not, so the photo is either
            // complete or not exported (a resume would otherwise
            // export it again under a new name or miss its masks).
            let finish_photo = AtomicBool::new(false);
            if export_settings.preserve_timestamps {
                set_capture_file_times(output_path, pinned.capture_date(original_path));
            }

            if export_settings.export_masks
                && let Some(source) = mask_source.as_ref()
            {
                if let Some(tracking) = &targets[target].tracking {
                    tracking.stage(item.global_index, Stage::Masks, None);
                }
                export_masks_for_image(
                    source.base(),
                    &source.pristine,
                    &js_adjustments,
                    export_settings,
                    output_path,
                    &source_path_str,
                    context,
                    &develop,
                    is_raw,
                    &app_handle.state::<crate::state::gpu::GpuState>(),
                    crate::image_processing::resolve_tonemapper_override(settings, is_raw),
                    js_adjustments[effects::LUT_PATH.into_name()]
                        .as_str()
                        .and_then(|path| {
                            get_or_load_lut(
                                &app_handle.state::<crate::state::luts::LutState>(),
                                path,
                            )
                            .ok()
                        }),
                    Some(&crate::super_resolution::ExportEnlarger { app_handle }),
                    &pinned,
                    &finish_photo,
                )?;
            }
            // Recorded only now: a mask file that failed leaves no record,
            // so a retry never takes the photo for unchanged.
            published(target, output_path, lock_recover(&written).take());
            Ok(())
        }
    }));
    results
}

/// The installed font families for text watermarks.
#[tauri::command]
pub async fn list_font_families() -> Vec<String> {
    tauri::async_runtime::spawn_blocking(rawtonic_export::text_watermark::font_families)
        .await
        .unwrap_or_default()
}

#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn export_images(
    paths: Vec<String>,
    output_folder_or_file: String,
    is_explicit_file_path: bool,
    base_origin_folders: Vec<String>,
    export_settings: ExportSettings,
    output_format: String,
    current_edit_path: Option<String>,
    current_edit_adjustments: Option<Value>,
    library: crate::export_queue::SubmittedFrom,
    app_handle: tauri::AppHandle,
) -> Result<String, String> {
    if paths.is_empty() {
        return Err("Nothing to export.".to_string());
    }
    // Queued behind a running export; persisted so it survives a restart.
    crate::export_queue::enqueue(
        &app_handle,
        crate::export_queue::ExportRequest {
            paths,
            display_label: None,
            output_folder_or_file,
            is_explicit_file_path,
            base_origin_folders,
            export_settings,
            output_format,
            current_edit_path,
            current_edit_adjustments,
            library,
            pinned: None,
            edit_with: None,
            edit_target: None,
            proof_sheet: None,
        },
    )
    .await
}

/// What a recipe's name and subfolder templates give `path` (the first of
/// `total` photos): the Export dialog's live example and a single photo's
/// file name. The photo is read as its export reads it (`pin_photos`
/// through `library`): its corrected capture date and the library's
/// rating, label, title, creator and copyright.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportNamePreview {
    /// The subfolder below the destination ("" for none), `/`-separated.
    pub folder: String,
    pub name: String,
}

#[tauri::command]
pub async fn export_naming_preview(
    path: String,
    export_settings: ExportSettings,
    output_format: String,
    total: usize,
    library: crate::export_queue::SubmittedFrom,
) -> Result<ExportNamePreview, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let (source, _) = parse_virtual_path(&path);
        require_local_tiff(&source)?;
        let pinned =
            pin_photos(std::slice::from_ref(&path), None, None, library.directory())?.remove(0);
        let date = pinned.naming_date(&source);
        let capture = export_settings
            .naming_needs_capture()
            .then(|| naming_capture(&source, pinned.metadata.as_ref()));
        let (sequence, last) = export_settings.filename_counter(0, total.max(1));
        let naming = export_settings.naming_context(
            &source,
            sequence,
            last,
            &date,
            &output_format,
            capture.as_ref(),
        );
        let stem = rawtonic_export::sidecars::export_stem(
            &export_settings.naming_template(),
            &naming,
            1,
            None,
        )?;
        let folder = match export_settings.subfolder.as_deref() {
            Some(sub) => rawtonic_naming::render_folder(sub.trim(), &naming)
                .map_err(|e| e.to_string())?
                .iter()
                .map(|part| part.to_string_lossy())
                .collect::<Vec<_>>()
                .join("/"),
            None => String::new(),
        };
        Ok(ExportNamePreview {
            folder,
            name: format!("{stem}.{output_format}"),
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// The exports of a photo set's ticked recipes, queued together so they run
/// as one: each photo is developed once for all of them.
#[tauri::command]
pub async fn export_images_batch(
    mut requests: Vec<crate::export_queue::ExportRequest>,
    app_handle: tauri::AppHandle,
) -> Result<Vec<String>, String> {
    if requests.iter().any(|request| request.paths.is_empty()) || requests.is_empty() {
        return Err("Nothing to export.".to_string());
    }
    for request in &mut requests {
        // Pinned here, from the library, never as the interface says.
        request.pinned = None;
    }
    crate::export_queue::enqueue_all(&app_handle, requests).await
}

/// Fails before any render when exports could not be written to `folder`
/// (permissions or a read-only volume such as an AppImage mount).
pub(crate) fn ensure_writable_folder(folder: &std::path::Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        let folder = if folder.as_os_str().is_empty() {
            std::path::Path::new(".")
        } else {
            folder
        };
        let path = rawtonic_fs::c_path(folder)
            .map_err(|_| format!("Invalid output folder: {}", folder.display()))?;
        // access() also reports a read-only file system (EROFS).
        if unsafe { libc::access(path.as_ptr(), libc::W_OK | libc::X_OK) } != 0 {
            return Err(format!(
                "Output folder {} is not writable: {}",
                folder.display(),
                std::io::Error::last_os_error()
            ));
        }
    }
    #[cfg(not(unix))]
    let _ = folder;
    Ok(())
}

pub async fn run_headless_export(
    session: crate::launch_request::HeadlessExportSession,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    println!("Starting headless export...");

    let source_path = std::path::Path::new(&session.source);
    if !source_path.exists() {
        return Err(format!("Source path does not exist: {}", session.source));
    }

    let mut paths = Vec::new();
    if source_path.is_dir() {
        let images = crate::file_management::list_images_recursive(
            session.source.clone(),
            app_handle.clone(),
        )?;
        paths = images.into_iter().map(|img| img.path).collect();
    } else {
        paths.push(session.source.clone());
    }

    if paths.is_empty() {
        return Err("No supported images found at the source path.".to_string());
    }

    let output_path = std::path::Path::new(&session.output);
    // Recipes write into the output folder (their subfolders).
    let is_explicit_file_path = session.recipes.is_none()
        && paths.len() == 1
        && output_path.extension().is_some()
        && !output_path.is_dir();

    let output_folder = if is_explicit_file_path {
        let parent = output_path.parent().unwrap_or(std::path::Path::new(""));
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create output parent directory: {}", e))?;
        parent
    } else {
        std::fs::create_dir_all(&session.output)
            .map_err(|e| format!("Failed to create output directory: {}", e))?;
        output_path
    };
    // Recipes choose their own folders (subfolders, the originals' folder);
    // without them the output folder is where every file goes.
    if session.recipes.is_none() {
        ensure_writable_folder(output_folder)?;
    }

    println!("Found {} images to export. Processing...", paths.len());

    let mut custom_adjustments = None;
    if let Some(adj_path) = &session.adjustments_override {
        let content = std::fs::read_to_string(adj_path)
            .map_err(|e| format!("Failed to read adjustments file: {}", e))?;
        let json: serde_json::Value = serde_json::from_str(&content)
            .map_err(|e| format!("Failed to parse adjustments JSON: {}", e))?;
        custom_adjustments = Some(json);
        println!(
            "Loaded custom adjustments to override sidecars from: {}",
            adj_path
        );
    }

    let (tx, rx) = tokio::sync::oneshot::channel();

    // Each photo is read when it is exported, through the library open now.
    let library = crate::session_bridge::active_library_directory();
    let read = {
        let paths = paths.clone();
        Arc::new(move |index: usize| {
            let mut photo =
                pin_photos(&paths[index..=index], None, None, library.as_deref())?.remove(0);
            if let Some(adjustments) = &custom_adjustments {
                photo.adjustments = adjustments.clone();
            }
            Ok(photo)
        })
    };
    let denoise_models = {
        let (read, count) = (Arc::clone(&read), paths.len());
        tauri::async_runtime::spawn_blocking(move || {
            denoise_models((0..count).filter_map(|i| read(i).ok().map(|p| p.adjustments)))
        })
        .await
        .map_err(|e| e.to_string())?
    };
    let photos = ExportPhotos {
        open_path: None,
        denoise_models,
        read,
    };

    let targets = headless_targets(&session, is_explicit_file_path)?;

    export_images_impl(
        paths,
        targets,
        photos,
        app_handle.clone(),
        Some(tx),
        session.max_jobs,
    )
    .await?;

    match rx.await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(errors)) => Err(format!("Export completed with {} errors.", errors)),
        Err(_) => Err("Export task panicked or was cancelled.".to_string()),
    }
}

fn headless_targets(
    session: &crate::launch_request::HeadlessExportSession,
    is_explicit_file_path: bool,
) -> Result<Vec<ExportTarget>, String> {
    let export_settings = ExportSettings {
        jpeg_quality: session.quality,
        tiff_bit_depth: session.tiff_bit_depth,
        resize: None,
        border: None,
        pad: None,
        keep_metadata: session.keep_metadata,
        color_space: session.color_space,
        preserve_timestamps: true,
        strip_gps: false,
        keep_creator_contact: session.keep_creator_contact,
        filename_template: None,
        name_template: None,
        job_name: None,
        recipe_name: None,
        sequence_start: None,
        watermark: None,
        export_masks: false,
        preserve_folders: true,
        destination_type: None,
        subfolder: None,
        output_sharpening: None,
        super_resolution: false,
        provenance: None,
        existing_files: session.existing_files,
    };

    let recipes = match &session.recipes {
        Some(file) => headless_recipes(file)?,
        None => vec![HeadlessRecipe {
            format: session.format.clone(),
            settings: export_settings,
        }],
    };
    Ok(recipes
        .into_iter()
        .map(|recipe| ExportTarget {
            output_folder_or_file: session.output.clone(),
            is_explicit_file_path,
            base_origin_folders: vec![session.source.clone()],
            export_settings: recipe.settings,
            output_format: recipe.format,
            tracking: None,
        })
        .collect())
}

/// A recipe of a headless export with `--recipes`: the format and the
/// settings an export recipe saves (sizes, metadata, subfolder, …).
#[derive(serde::Deserialize)]
struct HeadlessRecipe {
    format: String,
    settings: ExportSettings,
}

/// The recipes of a `--recipes` file: a JSON array of `{format, settings}`.
fn headless_recipes(file: &str) -> Result<Vec<HeadlessRecipe>, String> {
    let content =
        std::fs::read_to_string(file).map_err(|e| format!("Failed to read recipes file: {}", e))?;
    let recipes: Vec<HeadlessRecipe> = serde_json::from_str(&content)
        .map_err(|e| format!("Failed to parse recipes file: {}", e))?;
    if recipes.is_empty() {
        return Err("The recipes file lists no recipe.".to_string());
    }
    Ok(recipes)
}

#[tauri::command]
pub fn cancel_export(app_handle: tauri::AppHandle) -> Result<(), String> {
    // The queue waits after a cancel until the next export or resume.
    crate::export_queue::hold(&app_handle.state::<ExportState>());
    cancel_running_export(&app_handle)
}

/// Stops the running export at the next checkpoint (the queue goes on).
pub(crate) fn cancel_running_export(app_handle: &tauri::AppHandle) -> Result<(), String> {
    let exports = app_handle.state::<ExportState>();
    match request_export_cancellation(&exports.task_token, || {
        let _ = app_handle.emit("export-cancelling", ());
    }) {
        ExportCancellationRequest::Requested => {
            log::info!("Export cancellation requested; workers will stop at the next checkpoint");
        }
        ExportCancellationRequest::AlreadyRequested => {
            log::info!("Export cancellation was already requested");
        }
        ExportCancellationRequest::NoActiveTask => {
            return Err("No export task is currently running.".to_string());
        }
    }
    Ok(())
}

/// The precision a size estimate renders at: always 8 bits. TIFF is written
/// uncompressed (`encode_image_to_bytes`), so a 16-bit TIFF's size does not
/// depend on its pixel values and an 8-bit render encoded at 16 bits has
/// the same byte count; every other format writes 8 bits.
const ESTIMATE_PRECISION: rawtonic_gpu::gpu_processing::RenderOutputPrecision =
    rawtonic_gpu::gpu_processing::RenderOutputPrecision::EightBit;

/// Renders the estimate on a blocking worker, not an async runtime thread.
#[tauri::command]
pub async fn estimate_export_sizes(
    paths: Vec<String>,
    export_settings: ExportSettings,
    output_format: String,
    current_edit_path: Option<String>,
    current_edit_adjustments: Option<Value>,
    app_handle: tauri::AppHandle,
) -> Result<usize, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let app = app_handle.clone();
        estimate_sizes(
            paths,
            export_settings,
            output_format,
            current_edit_path,
            current_edit_adjustments,
            app.state::<AppState>(),
            app_handle,
        )
    })
    .await
    .map_err(|e| format!("Export size estimate failed: {e}"))?
}

fn estimate_sizes(
    paths: Vec<String>,
    export_settings: ExportSettings,
    output_format: String,
    current_edit_path: Option<String>,
    current_edit_adjustments: Option<Value>,
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<usize, String> {
    let develop = tauri::Manager::state::<DevelopSession>(&app_handle);
    if output_format.to_lowercase() == "cube" {
        return Ok(1_050_000 * paths.len());
    }

    if paths.is_empty() {
        return Ok(0);
    }

    let first_path = &paths[0];
    let (source_path, sidecar_path) = parse_virtual_path(first_path);
    require_local_tiff(&source_path)?;
    let source_path_str = source_path.to_string_lossy().to_string();

    let gpu = app_handle.state::<crate::state::gpu::GpuState>();

    let context = get_or_init_gpu_context(&gpu, &app_handle)?;
    let is_current_edit = is_open_edit(first_path, current_edit_path.as_deref());
    let is_raw = is_raw_file(&source_path_str);
    let settings = crate::state::settings::current(&app_handle);

    let single_image_extrapolated_size: usize = if is_current_edit
        && current_edit_adjustments.is_some()
    {
        let loaded_image = state
            .source
            .original_image
            .lock()
            .unwrap()
            .clone()
            .ok_or("No original image loaded")?;
        let mut adjustments_clone = current_edit_adjustments.clone().unwrap();
        normalize_editor_recipe(&develop, &mut adjustments_clone);

        let new_transform_hash = calculate_transform_hash(&adjustments_clone);
        let cached_preview_lock = develop.cached_preview.lock().unwrap();
        let preview_dim = settings.editor_preview_resolution.unwrap_or(1920);

        let (preview_image, scale, unscaled_crop_offset) = if let Some(cached) =
            &*cached_preview_lock
        {
            if cached.source.ptr_eq(&Arc::downgrade(&loaded_image.image))
                && cached.transform_hash == new_transform_hash
                && cached.preview_dim == preview_dim
            {
                let img = Arc::clone(&cached.image);
                let s = cached.scale;
                let offset = cached.unscaled_crop_offset;
                drop(cached_preview_lock);
                let owned_img = Arc::try_unwrap(img).unwrap_or_else(|arc| (*arc).clone());
                (owned_img, s, offset)
            } else {
                drop(cached_preview_lock);
                generate_transformed_preview(
                    &develop,
                    &loaded_image,
                    &adjustments_clone,
                    preview_dim,
                )?
            }
        } else {
            drop(cached_preview_lock);
            generate_transformed_preview(&develop, &loaded_image, &adjustments_clone, preview_dim)?
        };

        let (img_w, img_h) = preview_image.dimensions();
        let mask_definitions: Vec<MaskDefinition> = adjustments_clone
            .get(masks::MASKS.name())
            .and_then(|m| serde_json::from_value(m.clone()).ok())
            .unwrap_or_default();

        let scaled_crop_offset = (
            unscaled_crop_offset.0 * scale,
            unscaled_crop_offset.1 * scale,
        );

        let warped_image = resolve_warped_image_for_masks(
            &state,
            &develop,
            &loaded_image.image,
            loaded_image.is_raw,
            1.0,
            &adjustments_clone,
            &mask_definitions,
        )?;
        let mask_geometry = PhotoGeometry::from_adjustments(&adjustments_clone)
            .with_decoded_size(loaded_image.image.width(), loaded_image.image.height());
        let mask_bitmaps: Vec<Arc<MaskBitmap>> = mask_definitions
            .iter()
            .filter_map(|def| {
                get_cached_or_generate_mask(
                    &develop,
                    def,
                    img_w,
                    img_h,
                    scale,
                    scaled_crop_offset,
                    &mask_geometry,
                    warped_image.as_ref(),
                )
                .transpose()
            })
            .collect::<Result<_, _>>()?;

        let tm_override = resolve_tonemapper_override_from_handle(&app_handle, is_raw);
        let mut all_adjustments =
            get_all_adjustments_from_json(&adjustments_clone, is_raw, tm_override);
        crate::baseline_exposure::apply_base_look(
            &mut all_adjustments.global,
            &source_path_str,
            loaded_image.measures.as_ref(),
            crate::photo_library::library_of_str(&source_path_str).as_deref(),
        );
        all_adjustments.global.detail_scale = scale;
        all_adjustments.global.show_clipping = 0;

        let lut = adjustments_clone[effects::LUT_PATH.into_name()]
            .as_str()
            .and_then(|p| {
                get_or_load_lut(&app_handle.state::<crate::state::luts::LutState>(), p).ok()
            });
        let unique_hash =
            calculate_full_job_hash(&loaded_image.path, &adjustments_clone).wrapping_add(1);

        let processed_preview = process_and_get_dynamic_image_with_precision(
            &context,
            &gpu,
            &develop,
            &preview_image,
            unique_hash,
            RenderRequest {
                adjustments: all_adjustments,
                mask_bitmaps: &mask_bitmaps,
                lut,
                roi: None,
            },
            "estimate_export_size",
            ESTIMATE_PRECISION,
        )?;

        let preview_bytes = encode_image_to_bytes(
            &processed_preview,
            &output_format,
            export_settings.jpeg_quality,
            export_settings.tiff_bit_depth,
            export_settings.color_space,
            None,
        )?;
        let preview_byte_size = preview_bytes.len();

        let (transformed_full_res, _) =
            apply_all_transformations(&loaded_image.image, &adjustments_clone, is_raw)?;
        let (full_w, full_h) = transformed_full_res.dimensions();

        let (final_full_w, final_full_h) =
            compute_export_output_size(full_w, full_h, &export_settings);

        let (processed_preview_w, processed_preview_h) = processed_preview.dimensions();
        let pixel_ratio = if processed_preview_w > 0 && processed_preview_h > 0 {
            (final_full_w as f64 * final_full_h as f64)
                / (processed_preview_w as f64 * processed_preview_h as f64)
        } else {
            1.0
        };

        (preview_byte_size as f64 * pixel_ratio) as usize
    } else {
        let metadata = crate::exif_processing::load_sidecar(&sidecar_path);
        let mut js_adjustments = metadata.adjustments;
        // An estimate: a lens that cannot be resolved yet only leaves it
        // without the correction.
        let _ = rawtonic_export::normalize::normalize(
            &mut js_adjustments,
            &rawtonic_export::normalize::RecipeSource::file(Path::new(&source_path_str)),
            rawtonic_export::normalize::Bitmaps::None,
            rawtonic_export::normalize::Defaults::Render,
        );

        const ESTIMATE_DIM: u32 = 1280;

        let file_slice: Vec<u8>;
        let mmap_guard;
        let file_data: &[u8] = match read_file_mapped(Path::new(&source_path_str)) {
            Ok(mmap) => {
                mmap_guard = Some(mmap);
                mmap_guard.as_ref().unwrap()
            }
            Err(_) => {
                file_slice = fs::read(&source_path_str).map_err(|io_err| io_err.to_string())?;
                &file_slice
            }
        };

        let original_image =
            load_base_image_from_bytes(file_data, &source_path_str, true, &settings, None)
                .map_err(|e| e.to_string())?;

        let raw_scale_factor = if is_raw {
            crate::raw_processing::get_fast_demosaic_scale_factor(
                file_data,
                original_image.width(),
                original_image.height(),
            )
        } else {
            1.0
        };

        if let Some(crop_val) = js_adjustments.get_mut(geometry::CROP.name())
            && let Ok(c) = serde_json::from_value::<Crop>(crop_val.clone())
        {
            *crop_val = serde_json::to_value(Crop {
                x: c.x * raw_scale_factor as f64,
                y: c.y * raw_scale_factor as f64,
                width: c.width * raw_scale_factor as f64,
                height: c.height * raw_scale_factor as f64,
            })
            .unwrap_or(serde_json::Value::Null);
        }

        let (transformed_shrunk_res, unscaled_crop_offset) =
            apply_all_transformations(Cow::Borrowed(&original_image), &js_adjustments, is_raw)?;
        let (shrunk_w, shrunk_h) = transformed_shrunk_res.dimensions();

        let preview_base = if shrunk_w > ESTIMATE_DIM || shrunk_h > ESTIMATE_DIM {
            downscale_f32_image(transformed_shrunk_res.as_ref(), ESTIMATE_DIM, ESTIMATE_DIM)
        } else {
            transformed_shrunk_res.into_owned()
        };

        let (preview_w, preview_h) = preview_base.dimensions();
        let gpu_scale = if shrunk_w > 0 {
            preview_w as f32 / shrunk_w as f32
        } else {
            1.0
        };
        let total_scale = gpu_scale * raw_scale_factor;

        let mask_definitions: Vec<MaskDefinition> = js_adjustments
            .get(masks::MASKS.name())
            .and_then(|m| serde_json::from_value(m.clone()).ok())
            .unwrap_or_default();
        let scaled_crop_offset = (
            unscaled_crop_offset.0 * gpu_scale,
            unscaled_crop_offset.1 * gpu_scale,
        );

        let warped_image = mask_definitions
            .iter()
            .any(MaskDefinition::requires_warped_image)
            .then(|| {
                prepare_parametric_mask_source(
                    &original_image,
                    is_raw,
                    raw_scale_factor,
                    &js_adjustments,
                )
            })
            .transpose()?;
        let mask_geometry = PhotoGeometry::from_adjustments(&js_adjustments).with_photo_size(
            original_image.width() as f64 / raw_scale_factor as f64,
            original_image.height() as f64 / raw_scale_factor as f64,
        );
        let mask_bitmaps: Vec<Arc<MaskBitmap>> = mask_definitions
            .iter()
            .filter_map(|def| {
                get_cached_or_generate_mask(
                    &develop,
                    def,
                    preview_w,
                    preview_h,
                    total_scale,
                    scaled_crop_offset,
                    &mask_geometry,
                    warped_image.as_ref(),
                )
                .transpose()
            })
            .collect::<Result<_, _>>()?;

        let tm_override = resolve_tonemapper_override_from_handle(&app_handle, is_raw);
        let mut all_adjustments =
            get_all_adjustments_from_json(&js_adjustments, is_raw, tm_override);
        crate::baseline_exposure::apply_base_look(
            &mut all_adjustments.global,
            &source_path_str,
            None,
            crate::photo_library::library_of_str(&source_path_str).as_deref(),
        );
        all_adjustments.global.detail_scale = total_scale;
        all_adjustments.global.show_clipping = 0;

        let lut = js_adjustments[effects::LUT_PATH.into_name()]
            .as_str()
            .and_then(|p| {
                get_or_load_lut(&app_handle.state::<crate::state::luts::LutState>(), p).ok()
            });
        let unique_hash =
            calculate_full_job_hash(&source_path_str, &js_adjustments).wrapping_add(1);

        let processed_preview = process_and_get_dynamic_image_with_precision(
            &context,
            &gpu,
            &develop,
            &preview_base,
            unique_hash,
            RenderRequest {
                adjustments: all_adjustments,
                mask_bitmaps: &mask_bitmaps,
                lut,
                roi: None,
            },
            "estimate_batch_export_size",
            ESTIMATE_PRECISION,
        )?;

        let preview_bytes = encode_image_to_bytes(
            &processed_preview,
            &output_format,
            export_settings.jpeg_quality,
            export_settings.tiff_bit_depth,
            export_settings.color_space,
            None,
        )?;
        let single_image_estimated_size = preview_bytes.len();

        let full_w = (shrunk_w as f32 / raw_scale_factor).round() as u32;
        let full_h = (shrunk_h as f32 / raw_scale_factor).round() as u32;

        let (final_full_w, final_full_h) =
            compute_export_output_size(full_w, full_h, &export_settings);

        let (processed_preview_w, processed_preview_h) = processed_preview.dimensions();
        let pixel_ratio = if processed_preview_w > 0 && processed_preview_h > 0 {
            (final_full_w as f64 * final_full_h as f64)
                / (processed_preview_w as f64 * processed_preview_h as f64)
        } else {
            1.0
        };

        (single_image_estimated_size as f64 * pixel_ratio) as usize
    };

    Ok(single_image_extrapolated_size * paths.len())
}

#[cfg(test)]
mod tests {
    use super::{
        ExportTarget, History, PhotoItem, PlannedFiles, crashed_export_error, export_in_turn,
        is_open_edit, render_groups_for_streaming_tiffs,
    };

    /// The file `plan_output_path` names for a recipe whose choice writes
    /// (Keep both, or nothing recorded).
    fn plan_output_path(
        target: &ExportTarget,
        item: &PhotoItem,
        source: &std::path::Path,
        date: &jiff::Timestamp,
        capture: Option<&rawtonic_naming::Capture>,
        mask_layers: usize,
        used: &std::sync::Mutex<std::collections::HashSet<std::path::PathBuf>>,
    ) -> Result<std::path::PathBuf, String> {
        super::plan_output_path(
            target,
            item,
            source,
            date,
            capture,
            mask_layers,
            used,
            item.image_path_str,
            || "inputs",
            &History {
                records: &|| None,
                is_original: &|_| false,
            },
            true,
        )
        .map(|planned| planned.path().to_path_buf())
    }

    /// A queued export of the open photograph reuses its pixels only when
    /// they were developed with the export's AI Denoise: after the user
    /// turned it off, moved the slider or changed its version, the export
    /// decodes afresh (an export queued at 50 is denoised at 50).
    #[test]
    fn open_source_is_reused_only_for_its_own_ai_denoise() {
        use super::reusable_for_export;
        use crate::state::source::LoadedImage;
        use rawtonic_engine::image_loader::{ai_denoise_settings, decode_variant};
        let source = "/no-such-dir/open.raf";
        let settings = crate::app_settings::AppSettings::default();
        let recipe = |amount: f64, version: u8| serde_json::json!({ "aiDenoise": amount, "aiDenoiseVersion": version });
        let open = |recipe: &serde_json::Value, is_raw: bool| LoadedImage {
            path: source.to_string(),
            image: std::sync::Arc::new(image::DynamicImage::new_rgb8(2, 1)),
            is_raw,
            decode: Some(decode_variant(recipe, source)),
            ai_denoise: ai_denoise_settings(recipe).filter(|_| is_raw),
            decoder: Some(crate::image_loader::decode_preferences(&settings)),
            measures: None,
        };
        let queued = recipe(50.0, 4);
        assert!(reusable_for_export(
            &open(&queued, true),
            &queued,
            source,
            &settings
        ));
        for changed in [
            recipe(0.0, 4),
            recipe(30.0, 4),
            recipe(50.0, 3),
            recipe(50.0, 1),
        ] {
            assert!(
                !reusable_for_export(&open(&changed, true), &queued, source, &settings),
                "open at {changed}, export queued at {queued}"
            );
            assert!(!reusable_for_export(
                &open(&queued, true),
                &changed,
                source,
                &settings
            ));
        }
        // An export develops the stored amount whether the photograph is
        // open or not: an old fine drag's 50.4 is not the open 50.
        assert!(!reusable_for_export(
            &open(&queued, true),
            &recipe(50.4, 4),
            source,
            &settings
        ));
        // The model could not be had when the photograph opened: its
        // pixels are undenoised, so an export with AI Denoise decodes.
        let undenoised = LoadedImage {
            ai_denoise: None,
            decoder: None,
            ..open(&queued, true)
        };
        assert!(!reusable_for_export(
            &undenoised,
            &queued,
            source,
            &settings
        ));
        // Without AI Denoise either way the pixels are the same.
        let off = recipe(0.0, 4);
        assert!(reusable_for_export(
            &open(&off, true),
            &off,
            source,
            &settings
        ));
        // Other formats never run it: the slider does not matter.
        let tiff = LoadedImage {
            path: "/no-such-dir/open.tif".to_string(),
            decode: Some(decode_variant(&queued, "/no-such-dir/open.tif")),
            ..open(&queued, false)
        };
        assert!(reusable_for_export(
            &tiff,
            &queued,
            "/no-such-dir/open.tif",
            &settings
        ));
        // The open pixels were decoded under other preferences (linear RAW
        // mode, preprocessing, Apple RAW 9): the export decodes afresh.
        let open_now = open(&queued, true);
        for changed in [
            crate::app_settings::AppSettings {
                linear_raw_mode: "linear".to_string(),
                ..settings.clone()
            },
            crate::app_settings::AppSettings {
                raw_preprocessing_sharpening: Some(0.9),
                ..settings.clone()
            },
            crate::app_settings::AppSettings {
                use_apple_raw9: Some(true),
                ..settings.clone()
            },
        ] {
            assert!(!reusable_for_export(&open_now, &queued, source, &changed));
            // Of every format: the export never depends on which photo is open.
            let tiff_open = LoadedImage {
                path: "/no-such-dir/open.tif".to_string(),
                decode: Some(decode_variant(&queued, "/no-such-dir/open.tif")),
                ..open(&queued, false)
            };
            assert!(!reusable_for_export(
                &tiff_open,
                &queued,
                "/no-such-dir/open.tif",
                &changed
            ));
        }
        // Preview data carries no preferences.
        let preview = LoadedImage {
            decoder: None,
            ..open(&queued, true)
        };
        assert!(!reusable_for_export(&preview, &queued, source, &settings));
    }

    /// Exercise the app's layer export, including real PNG encoding and
    /// no-clobber file publication, with an upscaler accepted by production.
    #[test]
    #[ignore = "requires a GPU; run explicitly on native QA hosts"]
    fn super_resolution_main_mask_and_alpha_files_share_dimensions() {
        use super::*;
        use rawtonic_export::export_processing::Enlarger;
        struct Doubler;
        impl Enlarger for Doubler {
            fn enlarge_2x(
                &self,
                image: DynamicImage,
                cancelled: &dyn Fn() -> Result<(), String>,
            ) -> Result<DynamicImage, String> {
                cancelled()?;
                Ok(image.resize_exact(
                    image.width() * 2,
                    image.height() * 2,
                    imageops::FilterType::Nearest,
                ))
            }
        }
        let context = Arc::new(GpuContext::headless().expect("native QA requires a GPU"));
        let gpu = crate::state::gpu::GpuState::default();
        let develop = DevelopSession::default();
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source.png");
        let photo =
            DynamicImage::ImageRgb8(ImageBuffer::from_pixel(30, 20, image::Rgb([80, 100, 120])));
        let mut bytes = std::io::Cursor::new(Vec::new());
        photo.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
        crate::session_bridge::write_new_output(&source, bytes.into_inner()).unwrap();
        let original = std::fs::read(&source).unwrap();
        let adjustments = serde_json::json!({"masks": [{
            "id": "layer", "name": "Gradient", "visible": true, "invert": false,
            "adjustments": {"exposure": 0.5}, "subMasks": [{
                "id": "linear", "type": "linear", "visible": true, "mode": "additive",
                "parameters": {"startX": 0, "startY": 10, "endX": 30, "endY": 10, "range": 10}
            }]
        }]});
        let pinned: PinnedPhoto = serde_json::from_value(
            serde_json::json!({"adjustments": adjustments, "metadata": null}),
        )
        .unwrap();
        for enabled in [false, true] {
            let mut settings: ExportSettings = serde_json::from_value(serde_json::json!({
                "jpegQuality": 90, "keepMetadata": false, "stripGps": false,
                "filenameTemplate": null, "watermark": null, "resize": null
            }))
            .unwrap();
            settings.super_resolution = enabled;
            settings.export_masks = true;
            let output = temp.path().join(format!("main-{enabled}.png"));
            let (main, _) =
                finish_rendered_export(photo.clone(), &settings, Some(&Doubler), &|| Ok(()))
                    .unwrap();
            save_image_with_metadata(
                main,
                &output,
                source.to_str().unwrap(),
                &settings,
                false,
                &pinned,
            )
            .unwrap();
            export_masks_for_image(
                &photo,
                &photo,
                &adjustments,
                &settings,
                &output,
                source.to_str().unwrap(),
                &context,
                &develop,
                false,
                &gpu,
                None,
                None,
                Some(&Doubler),
                &pinned,
                &AtomicBool::new(false),
            )
            .unwrap();
            let main = image::open(&output).unwrap();
            let [layer, alpha] = mask_file_paths(&output, 0);
            let layer = image::open(layer).unwrap();
            let alpha = image::open(alpha).unwrap();
            let expected = if enabled { (60, 40) } else { (30, 20) };
            assert_eq!(main.dimensions(), expected);
            assert_eq!(layer.dimensions(), main.dimensions());
            assert_eq!(alpha.dimensions(), main.dimensions());
        }
        assert_eq!(std::fs::read(source).unwrap(), original);
    }

    /// The exact full-size RAW render used by the import dialog, including
    /// normalization, decode and GPU processing, must not write beside a card.
    #[test]
    #[ignore = "requires a GPU; run explicitly on native QA hosts"]
    fn import_full_size_render_keeps_source_and_all_sidecars_unchanged() {
        use super::{RenderEnv, render_unedited_full_size_with_env};
        use rawtonic_gpu::gpu_processing::{GpuContext, GpuProcessorSlot};
        use std::collections::BTreeMap;
        use std::sync::Mutex;
        let context = GpuContext::headless().expect("native QA requires a GPU");
        let processor = GpuProcessorSlot::new(None);
        let cache = Mutex::new(None);
        let env = RenderEnv {
            context: &context,
            processor: &processor,
            image_cache: &cache,
            tonemapper_override: &|_| None,
            load_lut: &|_| None,
            render_inputs: None,
        };
        let pixels = image::Rgb32FImage::from_fn(64, 48, |x, y| {
            image::Rgb([0.05 + x as f32 / 256.0, 0.1 + y as f32 / 192.0, 0.15])
        });
        let bytes = rawtonic_merge::dng::encode_linear_dng(
            &pixels,
            &rawtonic_merge::dng::DngMeta {
                color: None,
                metadata: None,
                keep_lens: false,
                baseline_exposure: None,
                preview: None,
                software: "Rawtonic test",
            },
        )
        .unwrap();
        let temp = tempfile::tempdir().unwrap();
        let photo = temp.path().join("source.DNG");
        std::fs::write(&photo, &bytes).unwrap();
        let snapshot = || -> BTreeMap<_, _> {
            std::fs::read_dir(temp.path())
                .unwrap()
                .map(|entry| {
                    let entry = entry.unwrap();
                    (entry.file_name(), std::fs::read(entry.path()).unwrap())
                })
                .collect()
        };
        // Both clean media and media with existing edits/metadata: also
        // catch unexpected sidecar names, and preserve the RAW byte for byte.
        for existing in [false, true] {
            if existing {
                for name in [
                    "source.DNG.rrdata",
                    "source.DNG.rrexif",
                    "source.xmp",
                    "source.DNG.extra",
                ] {
                    let contents: &[u8] = match name {
                        "source.DNG.rrdata" => {
                            br#"{"rating":4,"tags":["keep"],"adjustments":{"exposure":0.75}}"#
                        }
                        "source.DNG.rrexif" => {
                            br#"{"Make":"Keep this metadata","Model":"Unchanged"}"#
                        }
                        "source.xmp" => {
                            br#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><keep/></x:xmpmeta>"#
                        }
                        _ => b"existing private metadata\0\xff",
                    };
                    std::fs::write(temp.path().join(name), contents).unwrap();
                }
            }
            let before = snapshot();
            let full = crate::file_management::source_full_size_jpeg(&photo, &|bytes| {
                Some(
                    render_unedited_full_size_with_env(
                        photo.to_str().unwrap(),
                        bytes,
                        &crate::app_settings::AppSettings::default(),
                        &env,
                    )
                    .unwrap(),
                )
            })
            .expect("full-size RAW must render");
            assert_eq!(full.1, crate::file_management::FullSizeSource::Decoded);
            assert_eq!(image::load_from_memory(&full.0).unwrap().width(), 64);
            assert_eq!(image::load_from_memory(&full.0).unwrap().height(), 48);
            assert_eq!(
                snapshot(),
                before,
                "the original and every sidecar stay unchanged; none created"
            );
        }
        // Use a fresh path that has never existed, so path-keyed render
        // caches cannot hide a reopen after normalization. The full render
        // must use the same bytes, and leave adjacent sidecars absent.
        let detached = temp.path().join("never-written.DNG");
        let before_detached = snapshot();
        let full = render_unedited_full_size_with_env(
            detached.to_str().unwrap(),
            &bytes,
            &crate::app_settings::AppSettings::default(),
            &env,
        )
        .unwrap();
        assert_eq!((full.width(), full.height()), (64, 48));
        assert_eq!(snapshot(), before_detached);
    }

    /// Measurement for the import dialog's 100 % view (notes:
    /// benchmark-results/impd4-import-zoom): decode, whole render and a
    /// viewport-sized region render of each RAW in
    /// `RAWTONIC_IMPORT_REGION_FILES` (`:`-separated), and how the region's
    /// pixels compare with the whole render's.
    #[test]
    #[ignore = "requires a GPU and private RAW copies"]
    fn import_region_render_timings() {
        use super::{RenderEnv, decode_unedited, prepare_unedited, render_prepared_unedited};
        use rawtonic_gpu::gpu_processing::{GpuContext, GpuProcessorSlot};
        use std::sync::Mutex;
        use std::time::Instant;
        let Ok(files) = std::env::var("RAWTONIC_IMPORT_REGION_FILES") else {
            eprintln!("SKIP: RAWTONIC_IMPORT_REGION_FILES not set");
            return;
        };
        let context = GpuContext::headless().expect("requires a GPU");
        let processor = GpuProcessorSlot::new(None);
        let cache = Mutex::new(None);
        let env = RenderEnv {
            context: &context,
            processor: &processor,
            image_cache: &cache,
            tonemapper_override: &|_| None,
            load_lut: &|_| None,
            render_inputs: None,
        };
        let settings = crate::app_settings::AppSettings::default();
        let (vw, vh) = (1920u32, 1200u32);
        let ms = |started: Instant| started.elapsed().as_millis();
        for (round, path) in files.split(':').enumerate().flat_map(|(i, p)| {
            // The first file twice: its first render warms the pipelines.
            if i == 0 {
                vec![(0, p), (1, p)]
            } else {
                vec![(1, p)]
            }
        }) {
            let bytes = std::fs::read(path).unwrap();
            let started = Instant::now();
            let unedited = decode_unedited(path, &bytes, &settings).unwrap();
            let decode = ms(started);
            let started = Instant::now();
            let prepared = prepare_unedited(unedited, &env).unwrap();
            let prepare = ms(started);
            let (w, h) = prepared.size();
            let shared: rawtonic_engine::image_loader::SharedBytes =
                std::sync::Arc::new(bytes.clone());
            assert_eq!(super::unedited_size(path, &shared), Some((w, h)), "{path}");
            let (rw, rh) = (vw.min(w), vh.min(h));
            let (rx, ry) = ((w - rw) / 2, (h - rh) / 2);
            let started = Instant::now();
            let region = render_prepared_unedited(&prepared, Some([rx, ry, rw, rh]), &env)
                .unwrap()
                .to_rgb8();
            let region_ms = ms(started);
            let started = Instant::now();
            let full = render_prepared_unedited(&prepared, None, &env)
                .unwrap()
                .to_rgb8();
            let full_ms = ms(started);
            let started = Instant::now();
            let whole = super::render_unedited_full_size_with_env(path, &bytes, &settings, &env)
                .unwrap()
                .to_rgb8();
            let whole_ms = ms(started);
            assert_eq!(full.dimensions(), (w, h));
            assert!(whole == full, "the same render either way");
            let (mut max, mut differing) = (0u8, 0usize);
            for y in 0..rh {
                for x in 0..rw {
                    let a = full.get_pixel(rx + x, ry + y).0;
                    let b = region.get_pixel(x, y).0;
                    let d = (0..3).map(|c| a[c].abs_diff(b[c])).max().unwrap();
                    max = max.max(d);
                    differing += usize::from(d > 0);
                }
            }
            println!(
                "IMPD4 round={round} file={} out={w}x{h} decode_ms={decode} prepare_ms={prepare} \
                 region_ms={region_ms} full_after_region_ms={full_ms} whole_ms={whole_ms} \
                 region_max={max} region_diff={differing}",
                path.rsplit('/').next().unwrap(),
            );
        }
    }

    /// The size the import dialog's large view enlarges its preview to is
    /// that of the 100 % view that replaces it, for each file in
    /// `RAWTONIC_IMPORT_REGION_FILES` (renders and camera JPEGs alike).
    #[test]
    #[ignore = "requires a GPU and private RAW copies"]
    fn import_full_size_dimensions_are_those_of_the_full_size_view() {
        use super::{RenderEnv, render_unedited_full_size_with_env, unedited_size};
        use rawtonic_gpu::gpu_processing::{GpuContext, GpuProcessorSlot};
        use std::sync::Mutex;
        let Ok(files) = std::env::var("RAWTONIC_IMPORT_REGION_FILES") else {
            eprintln!("SKIP: RAWTONIC_IMPORT_REGION_FILES not set");
            return;
        };
        let context = GpuContext::headless().expect("requires a GPU");
        let processor = GpuProcessorSlot::new(None);
        let cache = Mutex::new(None);
        let env = RenderEnv {
            context: &context,
            processor: &processor,
            image_cache: &cache,
            tonemapper_override: &|_| None,
            load_lut: &|_| None,
            render_inputs: None,
        };
        let settings = crate::app_settings::AppSettings::default();
        for path in files.split(':') {
            let file = std::path::Path::new(path);
            let predicted = crate::file_management::source_full_size_dimensions(file, &|bytes| {
                unedited_size(path, bytes)
            });
            let (jpeg, source) = crate::file_management::source_full_size_jpeg(file, &|bytes| {
                render_unedited_full_size_with_env(path, bytes, &settings, &env).ok()
            })
            .unwrap();
            let view = image::load_from_memory(&jpeg).unwrap();
            println!(
                "IMPD4 size file={} source={source:?} predicted={predicted:?} view={}x{}",
                path.rsplit('/').next().unwrap(),
                view.width(),
                view.height()
            );
            assert_eq!(predicted, Some((view.width(), view.height())), "{path}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_folder_exports_cannot_be_written_to_fails_before_rendering() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("rawtonic-readonly-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(super::ensure_writable_folder(&dir), Ok(()));
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
        let result = super::ensure_writable_folder(&dir);
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::remove_dir(&dir).unwrap();
        let error = result.expect_err("a read-only folder");
        assert!(
            error.contains("not writable") && error.contains(&*dir.to_string_lossy()),
            "{error}"
        );
    }

    #[test]
    fn a_recipes_file_lists_each_recipes_format_and_settings() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("recipes.json");
        std::fs::write(
            &file,
            r#"[
              {"format": "jpg", "settings": {"jpegQuality": 80, "resize": {"mode": "width", "value": 2048, "dontEnlarge": true}, "keepMetadata": false, "stripGps": true, "filenameTemplate": "{original_filename}_web", "subfolder": "web"}},
              {"format": "tiff", "settings": {"jpegQuality": 90, "tiffBitDepth": 16, "keepMetadata": true, "stripGps": false, "colorSpace": "adobeRgb", "subfolder": "print"}}
            ]"#,
        )
        .unwrap();
        let recipes = super::headless_recipes(file.to_str().unwrap()).unwrap();
        assert_eq!(recipes.len(), 2);
        assert_eq!(recipes[0].format, "jpg");
        assert_eq!(recipes[0].settings.subfolder.as_deref(), Some("web"));
        assert_eq!(
            recipes[1].settings.tiff_bit_depth,
            super::TiffBitDepth::Sixteen
        );
        std::fs::write(&file, "[]").unwrap();
        assert!(super::headless_recipes(file.to_str().unwrap()).is_err());
    }

    #[test]
    fn headless_targets_use_each_saved_policy_instead_of_the_command_line() {
        use crate::launch_request::{LaunchRequest, parse_launch_args};
        use rawtonic_export::existing_files::ExistingFiles;

        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("recipes.json");
        std::fs::write(
            &file,
            r#"[
              {"format": "jpg", "settings": {"jpegQuality": 80, "keepMetadata": false, "stripGps": true}},
              {"format": "tiff", "settings": {"jpegQuality": 90, "keepMetadata": true, "stripGps": false, "existingFiles": "replaceIfChanged"}}
            ]"#,
        )
        .unwrap();
        let args = [
            "export",
            "photo.png",
            "--output",
            "out",
            "--on-conflict",
            "skip",
            "--recipes",
            file.to_str().unwrap(),
        ]
        .map(str::to_owned);
        let LaunchRequest::HeadlessExport(mut session) = parse_launch_args(&args, None) else {
            panic!("expected a headless export");
        };
        let policies = |session: &crate::launch_request::HeadlessExportSession| {
            super::headless_targets(session, false)
                .unwrap()
                .into_iter()
                .map(|target| target.export_settings.existing_files)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            policies(&session),
            [ExistingFiles::KeepBoth, ExistingFiles::ReplaceIfChanged]
        );
        session.recipes = None;
        assert_eq!(policies(&session), [ExistingFiles::Skip]);
    }

    #[test]
    fn a_job_paused_or_cancelled_while_its_photo_waited_does_not_start_it() {
        // Recipes 0 and 2 were wanted when the photo began to wait; job 2
        // was paused meanwhile.
        let going_on = [true, true, false];
        assert_eq!(super::admitted(vec![0, 2], |i| going_on[i]), vec![0]);
        assert!(super::admitted(vec![2], |i| going_on[i]).is_empty());
    }

    /// Pressure reported while a render held the exports' processor left
    /// it; the end of the run frees it if memory is still short.
    #[tokio::test]
    async fn the_end_of_a_run_frees_a_processor_pressure_found_in_use() {
        use crate::gpu_processing::ProcessorSlot;
        let processor = ProcessorSlot::background(Some("processor"));
        let render = processor.lock().unwrap();
        assert_eq!(processor.take_if_idle(), None, "the watch leaves it");
        drop(render);
        let prewarmed = || std::future::ready(Ok::<(), ()>(()));
        assert!(!super::release_after_run(&processor, prewarmed(), || false).await);
        assert!(
            processor.lock().unwrap().is_some(),
            "kept with memory to spare"
        );
        assert!(super::release_after_run(&processor, prewarmed(), || true).await);
        assert!(processor.lock().unwrap().is_none());
    }

    /// A run that fails or is cancelled while its prewarm still builds the
    /// processor: the prewarm holds the lock, so the end of the run waits
    /// for it and then frees what it stored.
    #[tokio::test]
    async fn the_end_of_a_run_frees_a_processor_its_prewarm_was_still_building() {
        use crate::gpu_processing::ProcessorSlot;
        use std::future::Future;
        use std::task::{Context, Poll, Waker};
        let processor = std::sync::Arc::new(ProcessorSlot::background(None::<&str>));
        let (locked_tx, locked_rx) = std::sync::mpsc::channel();
        let (built_tx, built_rx) = std::sync::mpsc::channel::<()>();
        let prewarm = {
            let processor = processor.clone();
            tokio::task::spawn_blocking(move || {
                let mut slot = processor.lock().unwrap();
                locked_tx.send(()).unwrap();
                built_rx.recv().unwrap();
                *slot = Some("processor");
            })
        };
        locked_rx.recv().unwrap();
        let mut end = Box::pin(super::release_after_run(&processor, prewarm, || true));
        assert_eq!(
            end.as_mut().poll(&mut Context::from_waker(Waker::noop())),
            Poll::Pending,
            "waits for the prewarm"
        );
        built_tx.send(()).unwrap();
        assert!(end.await);
        assert!(processor.lock().unwrap().is_none());
    }

    #[test]
    fn an_export_crashed_after_a_lost_gpu_device_says_so() {
        assert_eq!(
            crashed_export_error("task 840 panicked", true),
            rawtonic_runtime::gpu_memory::GPU_DEVICE_LOST
        );
        assert_eq!(
            crashed_export_error("task 840 panicked", false),
            "Thread crashed: task 840 panicked"
        );
    }

    /// Export threads run only on the time the editor's threads leave.
    #[cfg(target_os = "linux")]
    #[test]
    fn export_threads_run_on_idle_time() {
        let policy = std::thread::spawn(|| {
            super::lower_thread_priority();
            // SAFETY: reads the calling thread's own policy.
            unsafe { libc::sched_getscheduler(0) }
        })
        .join()
        .unwrap();
        assert_eq!(policy, libc::SCHED_IDLE);
        // SAFETY: as above, for this thread, which keeps its policy.
        assert_eq!(unsafe { libc::sched_getscheduler(0) }, libc::SCHED_OTHER);
    }

    #[test]
    fn a_run_with_a_job_stopped_on_its_own_is_not_complete() {
        assert!(!super::stopped_short([true, true]));
        assert!(
            super::stopped_short([true, false]),
            "one recipe cancelled alone"
        );
        assert!(!super::stopped_short([]));
    }
    use std::path::Path;

    /// A photo's full-size TIFF is saved first within its render group
    /// (it then streams), but never where that could change a file's name
    /// or folder (MEM15 review round 3): a JPEG whose subfolder `{orig}` is
    /// `Photo.tiff/` before a TIFF named `{orig_base}` keeps today's order,
    /// so the JPEG makes its folder and the TIFF becomes `Photo_1.tiff`.
    #[test]
    fn a_full_size_tiff_goes_first_only_where_names_and_folders_stay() {
        let dir = tempfile::tempdir().unwrap();
        let target = |format: &str, settings: serde_json::Value| ExportTarget {
            output_folder_or_file: dir.path().to_string_lossy().into_owned(),
            is_explicit_file_path: false,
            base_origin_folders: Vec::new(),
            export_settings: serde_json::from_value(settings).unwrap(),
            output_format: format.to_string(),
            tracking: None,
        };
        let settings = |extra: serde_json::Value| {
            let mut settings = serde_json::json!({
                "jpegQuality": 90, "keepMetadata": false, "stripGps": false,
                "tiffBitDepth": 16,
            });
            settings
                .as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            settings
        };
        let order = |targets: &[ExportTarget]| {
            let mut images = vec![0, 1];
            let mut groups = vec![vec![0, 1]];
            render_groups_for_streaming_tiffs(targets, &mut images, &mut groups, |_| true);
            assert_eq!(groups[0], images, "the group renders in the saved order");
            images
        };

        // Same folder: the TIFF goes first.
        let same = [
            target("jpg", settings(serde_json::json!({}))),
            target("tiff", settings(serde_json::json!({}))),
        ];
        assert_eq!(order(&same), [1, 0]);
        // Mask files: the asked order.
        let masks = [
            target("jpg", settings(serde_json::json!({"exportMasks": true}))),
            target("tiff", settings(serde_json::json!({}))),
        ];
        assert_eq!(order(&masks), [0, 1]);

        // Round 3's case: today's order and today's names.
        let r3 = [
            target("jpg", settings(serde_json::json!({"subfolder": "{orig}"}))),
            target(
                "tiff",
                settings(serde_json::json!({"filenameTemplate": "{orig_base}"})),
            ),
        ];
        let saved = order(&r3);
        assert_eq!(saved, [0, 1]);
        let item = PhotoItem {
            global_index: 0,
            image_path_str: "/photos/Photo.tiff",
            appearance_count: 1,
            explicit_vc: None,
            total_paths: 2,
        };
        let used = std::sync::Mutex::new(std::collections::HashSet::new());
        let date = jiff::Timestamp::now();
        let mut written = Vec::new();
        let results = export_in_turn(
            saved,
            |t| {
                plan_output_path(
                    &r3[t],
                    &item,
                    Path::new("/photos/Photo.tiff"),
                    &date,
                    None,
                    0,
                    &used,
                )
                .map(Some)
            },
            |_, path| {
                std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(path)
                    .map_err(|e| format!("{}: {e}", path.display()))?;
                written.push(path.strip_prefix(dir.path()).unwrap().to_path_buf());
                Ok(())
            },
        );
        assert!(results.iter().all(|(_, r)| r.is_ok()), "{results:?}");
        assert_eq!(written[0].parent().unwrap(), Path::new("Photo.tiff"));
        assert_eq!(written[1], Path::new("Photo_1.tiff"));
    }

    /// A later recipe of the same photo names its file after the earlier
    /// ones wrote theirs, mask files included (UT batch export, F10 review):
    /// planned all at once, the second recipe took the name of the first
    /// one's mask file and failed its no-clobber write.
    #[test]
    fn each_recipe_is_named_after_the_files_of_the_recipes_before_it() {
        let dir = tempfile::tempdir().unwrap();
        let target = |template: &str| ExportTarget {
            output_folder_or_file: dir.path().to_string_lossy().into_owned(),
            is_explicit_file_path: false,
            base_origin_folders: Vec::new(),
            export_settings: serde_json::from_value(serde_json::json!({
                "jpegQuality": 90, "keepMetadata": false, "stripGps": false,
                "filenameTemplate": template, "exportMasks": true,
            }))
            .unwrap(),
            output_format: "jpg".to_string(),
            tracking: None,
        };
        let targets = [
            target("{original_filename}"),
            target("{original_filename}_mask_0_image"),
        ];
        let item = PhotoItem {
            global_index: 0,
            image_path_str: "/photos/foo.NEF",
            appearance_count: 1,
            explicit_vc: None,
            total_paths: 2,
        };
        let used = std::sync::Mutex::new(std::collections::HashSet::new());
        let date = jiff::Timestamp::now();
        let mut written = Vec::new();
        let results = export_in_turn(
            [0, 1],
            |t| {
                plan_output_path(
                    &targets[t],
                    &item,
                    Path::new("/photos/foo.NEF"),
                    &date,
                    None,
                    1,
                    &used,
                )
                .map(Some)
            },
            |t, path| {
                let mut files = vec![path.to_path_buf()];
                if t == 0 {
                    files.push(dir.path().join("foo_mask_0_image.jpg"));
                }
                for file in files {
                    std::fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(&file)
                        .map_err(|e| format!("{}: {e}", file.display()))?;
                    written.push(file.file_name().unwrap().to_string_lossy().into_owned());
                }
                Ok(())
            },
        );
        assert!(results.iter().all(|(_, r)| r.is_ok()), "{results:?}");
        assert_eq!(
            written,
            ["foo.jpg", "foo_mask_0_image.jpg", "foo_mask_0_image_1.jpg"]
        );
    }

    /// Photos export side by side: a recipe of a later photo may be named
    /// before an earlier photo's recipe wrote its mask files. Their names are
    /// reserved with the main file, so neither takes the other's (review of
    /// F10: recipe B's `Photo_2_mask_0_image` against recipe A's `Photo_2`
    /// with separate masks).
    #[test]
    fn a_retry_exports_into_the_file_its_first_attempt_reserved() {
        let dir = tempfile::tempdir().unwrap();
        let used = std::sync::Mutex::new(std::collections::HashSet::new());
        let target = ExportTarget {
            output_folder_or_file: dir.path().to_string_lossy().into_owned(),
            is_explicit_file_path: false,
            base_origin_folders: Vec::new(),
            export_settings: serde_json::from_value(serde_json::json!({
                "jpegQuality": 90, "keepMetadata": false, "stripGps": false,
            }))
            .unwrap(),
            output_format: "jpg".to_string(),
            tracking: None,
        };
        let source = dir.path().join("a.CR3");
        let item = PhotoItem {
            global_index: 0,
            image_path_str: "a.CR3",
            appearance_count: 1,
            explicit_vc: None,
            total_paths: 2,
        };
        let date = jiff::Timestamp::now();
        let planned = PlannedFiles::default();
        let plan = || {
            super::plan_output_path(
                &target,
                &item,
                &source,
                &date,
                None,
                0,
                &used,
                "a.CR3",
                || "inputs",
                &History {
                    records: &|| None,
                    is_original: &|_| false,
                },
                true,
            )
        };
        let first = planned.once(0, plan).unwrap();
        let again = planned.once(0, plan).unwrap();
        assert_eq!(first, again, "no _1 name for the retry");
        let first = first.path().to_path_buf();
        assert_eq!(first.file_name().unwrap(), "a_edited.jpg");
        assert!(planned.unpublished(0) && planned.unpublished(1));
        std::fs::write(&first, b"published").unwrap();
        assert!(!planned.unpublished(0), "its file is there: not again");
        let other = plan_output_path(&target, &item, &source, &date, None, 0, &used).unwrap();
        assert_ne!(other, first, "the reservation still holds for others");
    }

    #[test]
    fn exports_with_colliding_templates_preserve_other_originals_and_raw_jpeg_partners() {
        use rawtonic_export::export_processing::encode_export;

        let dir = tempfile::tempdir().unwrap();
        let raw = dir.path().join("a.CR3");
        let partner = dir.path().join("a.jpg");
        let other = dir.path().join("b.jpg");
        let originals: [(&Path, &[u8]); 3] = [
            (&raw, b"original RAW"),
            (&partner, b"camera JPEG"),
            (&other, b"another JPEG"),
        ];
        for (path, contents) in originals {
            rawtonic_fs::atomic_write(path, contents).unwrap();
        }

        let mut target = ExportTarget {
            output_folder_or_file: dir.path().to_string_lossy().into_owned(),
            is_explicit_file_path: false,
            base_origin_folders: Vec::new(),
            export_settings: serde_json::from_value(serde_json::json!({
                "jpegQuality": 90, "keepMetadata": false, "stripGps": false,
                "filenameTemplate": "b", "destinationType": "originalFolder",
            }))
            .unwrap(),
            output_format: "jpg".to_string(),
            tracking: None,
        };
        let used = std::sync::Mutex::new(std::collections::HashSet::new());
        let date = jiff::Timestamp::now();
        let plan = |target: &ExportTarget, source: &Path, global_index, total_paths| {
            let item = PhotoItem {
                global_index,
                image_path_str: source.to_str().unwrap(),
                appearance_count: 1,
                explicit_vc: None,
                total_paths,
            };
            plan_output_path(target, &item, source, &date, None, 0, &used).unwrap()
        };
        let write = |target: &ExportTarget, path: &Path| {
            let image = image::DynamicImage::new_rgb8(2, 2);
            let encoded = encode_export(image, "jpg", &target.export_settings, None).unwrap();
            super::write_export_bytes(path, "jpg", encoded)
        };
        for (index, (source, expected)) in [(&raw, "b_1.jpg"), (&other, "b_2.jpg")]
            .into_iter()
            .enumerate()
        {
            let output = plan(&target, source, index, 2);
            assert_eq!(output, dir.path().join(expected));
            write(&target, &output).unwrap();
        }

        // With only the RAW selected, the camera JPEG in the same folder is also protected.
        target.export_settings = serde_json::from_value(serde_json::json!({
            "jpegQuality": 90, "keepMetadata": false, "stripGps": false,
            "filenameTemplate": "a", "destinationType": "originalFolder",
        }))
        .unwrap();
        let output = plan(&target, &raw, 0, 1);
        assert_eq!(output, dir.path().join("a_1.jpg"));
        write(&target, &output).unwrap();

        // An explicit single-file destination on an original is numbered
        // beside it before rendering (never written over, never a failure
        // at publication).
        target.is_explicit_file_path = true;
        target.export_settings.destination_type = None;
        for (original, expected) in [(&partner, "a_2.jpg"), (&other, "b_3.jpg")] {
            target.output_folder_or_file = original.to_str().unwrap().to_owned();
            let output = plan(&target, &raw, 0, 1);
            assert_eq!(output, dir.path().join(expected));
            write(&target, &output).unwrap();
        }
        for (path, contents) in originals {
            assert_eq!(std::fs::read(path).unwrap(), contents);
        }
    }

    /// "If a file exists": Skip is decided from the folder alone; Replace
    /// if changed replaces only the recorded, unchanged export of the same
    /// photo, and never an original, its camera JPEG or another file.
    #[test]
    fn existing_files_skip_or_replace_only_the_photos_own_previous_export() {
        use super::{Planned, plan_output_path as plan_with};
        use rawtonic_export::existing_files::{ExistingFiles, ExportRecords, Outcome};

        let dir = tempfile::tempdir().unwrap();
        let raw = dir.path().join("a.CR3");
        let partner = dir.path().join("a.jpg");
        let other_raw = dir.path().join("b.CR3");
        for (path, contents) in [
            (&raw, &b"original RAW"[..]),
            (&partner, b"camera JPEG"),
            (&other_raw, b"other RAW"),
        ] {
            rawtonic_fs::atomic_write(path, contents).unwrap();
        }
        let records = ExportRecords::open(&dir.path().join("records.sqlite")).unwrap();
        let target = |choice: &str, template: &str| ExportTarget {
            output_folder_or_file: dir.path().to_string_lossy().into_owned(),
            is_explicit_file_path: false,
            base_origin_folders: Vec::new(),
            export_settings: serde_json::from_value(serde_json::json!({
                "jpegQuality": 90, "keepMetadata": false, "stripGps": false,
                "nameTemplate": template, "destinationType": "originalFolder",
                "existingFiles": choice,
            }))
            .unwrap(),
            output_format: "jpg".to_string(),
            tracking: None,
        };
        let date = jiff::Timestamp::now();
        let plan = |target: &ExportTarget, source: &Path, inputs: &str| {
            let used = std::sync::Mutex::new(std::collections::HashSet::new());
            let item = PhotoItem {
                global_index: 0,
                image_path_str: source.to_str().unwrap(),
                appearance_count: 1,
                explicit_vc: None,
                total_paths: 1,
            };
            let history = History {
                records: &|| Some(&records),
                is_original: &|_| false,
            };
            plan_with(
                target,
                &item,
                source,
                &date,
                None,
                0,
                &used,
                item.image_path_str,
                || inputs,
                &history,
                true,
            )
            .unwrap()
        };
        let raw_key = raw.to_str().unwrap();

        // Skip: the camera JPEG at the name is left alone, nothing rendered.
        assert_eq!(
            plan(&target("skip", "{orig_base}"), &raw, "v1"),
            Planned::Skip {
                path: partner.clone(),
                outcome: Outcome::SkippedExists
            }
        );
        // Replace if changed: the camera JPEG is no export of ours.
        let replace = target("replaceIfChanged", "{orig_base}");
        let planned = plan(&replace, &raw, "v1");
        assert_eq!(planned.outcome(), Outcome::KeptBoth);
        assert_eq!(planned.path(), dir.path().join("a_1.jpg"));
        let numbered = planned.path().to_path_buf();
        rawtonic_fs::atomic_write(&numbered, b"numbered v1").unwrap();
        records
            .record_written(
                &numbered,
                &partner,
                raw_key,
                "v1",
                &super::Written {
                    size: b"numbered v1".len() as u64,
                    sha256: rawtonic_fs::sha256_hex(b"numbered v1"),
                },
            )
            .unwrap();
        assert_eq!(
            plan(&replace, &raw, "v1"),
            Planned::Skip {
                path: numbered.clone(),
                outcome: Outcome::SkippedUnchanged,
            }
        );
        let changed = plan(&replace, &raw, "v2");
        assert_eq!(changed.outcome(), Outcome::Replaced);
        assert_eq!(changed.path(), numbered);

        // Our own export, recorded: unchanged → skipped, changed → replaced.
        let export = target("replaceIfChanged", "{orig_base}-edited");
        let own = dir.path().join("a-edited.jpg");
        assert_eq!(
            plan(&export, &raw, "v1"),
            Planned::Write {
                path: own.clone(),
                slot: own.clone(),
                replace: None,
                kept: None
            }
        );
        rawtonic_fs::atomic_write(&own, b"export v1").unwrap();
        records.record(&own, raw_key, "v1").unwrap();
        assert_eq!(
            plan(&export, &raw, "v1"),
            Planned::Skip {
                path: own.clone(),
                outcome: Outcome::SkippedUnchanged
            }
        );
        let planned = plan(&export, &raw, "v2");
        assert_eq!(planned.outcome(), Outcome::Replaced);
        assert_eq!(planned.path(), own);
        // Published as the export publishes it, with the key it was planned
        // with, and recorded as written: the next run finds it unchanged.
        let Planned::Write {
            replace: Some(previous),
            ..
        } = &planned
        else {
            unreachable!()
        };
        let written = super::publish_with(Some(previous), raw_key)
            .write_with(&own, |file| std::io::Write::write_all(file, b"export v2"))
            .unwrap();
        assert_eq!(std::fs::read(&own).unwrap(), b"export v2");
        records
            .record_written(&own, &own, raw_key, "v2", &written)
            .unwrap();
        assert_eq!(
            plan(&export, &raw, "v2").outcome(),
            Outcome::SkippedUnchanged
        );
        // A key of another photo at that path publishes nothing.
        assert!(
            super::publish_with(Some(&records.get(&own).unwrap().unwrap()), "other\nkey")
                .write_with(&own, |file| std::io::Write::write_all(file, b"x"))
                .is_err()
        );
        assert_eq!(std::fs::read(&own).unwrap(), b"export v2");
        // Another photo named onto it: kept both.
        let planned = plan(&target("replaceIfChanged", "a-edited"), &other_raw, "v2");
        assert_eq!(planned.outcome(), Outcome::KeptBoth);
        assert_eq!(planned.path(), dir.path().join("a-edited_1.jpg"));
        // Retouched in another program since: kept both.
        rawtonic_fs::atomic_write(&own, b"retouched").unwrap();
        assert_eq!(plan(&export, &raw, "v2").outcome(), Outcome::KeptBoth);
        // Keep both (the default) as before: numbered, never read records.
        let keep = target("keepBoth", "{orig_base}-edited");
        assert_eq!(
            plan(&keep, &raw, "v2").path(),
            dir.path().join("a-edited_1.jpg")
        );
        // Save As onto another program's file: numbered beside it, as the
        // choice promises; Skip leaves it.
        let mut save_as = target("replaceIfChanged", "{orig_base}");
        save_as.is_explicit_file_path = true;
        save_as.export_settings.destination_type = None;
        save_as.output_folder_or_file = partner.to_string_lossy().into_owned();
        let planned = plan(&save_as, &raw, "v1");
        assert_eq!(planned.outcome(), Outcome::SkippedUnchanged);
        assert_eq!(planned.path(), dir.path().join("a_1.jpg"));
        save_as.export_settings.existing_files = ExistingFiles::Skip;
        assert_eq!(plan(&save_as, &raw, "v1").outcome(), Outcome::SkippedExists);
        // Keep both (and Ask, which reaches an export as Keep both) on a
        // taken Save As name numbers a free sibling before anything renders,
        // instead of failing at the write after the render.
        for choice in [ExistingFiles::KeepBoth, ExistingFiles::Ask] {
            save_as.export_settings.existing_files = choice;
            let planned = plan(&save_as, &raw, "v1");
            assert_eq!(planned.path(), dir.path().join("a_2.jpg"), "{choice:?}");
            assert_eq!(planned.outcome(), Outcome::Written);
        }
        // A free Save As name is used as it is.
        save_as.output_folder_or_file = dir.path().join("new.jpg").to_string_lossy().into_owned();
        assert_eq!(
            plan(&save_as, &raw, "v1").path(),
            dir.path().join("new.jpg")
        );
        assert_eq!(std::fs::read(&raw).unwrap(), b"original RAW");
        assert_eq!(std::fs::read(&partner).unwrap(), b"camera JPEG");
    }

    #[test]
    fn mask_file_names_are_reserved_with_their_main_file() {
        let dir = tempfile::tempdir().unwrap();
        let target = |template: &str, start: u32, masks: bool| ExportTarget {
            output_folder_or_file: dir.path().to_string_lossy().into_owned(),
            is_explicit_file_path: false,
            base_origin_folders: Vec::new(),
            export_settings: serde_json::from_value(serde_json::json!({
                "jpegQuality": 90, "keepMetadata": false, "stripGps": false,
                "filenameTemplate": template, "sequenceStart": start, "exportMasks": masks,
            }))
            .unwrap(),
            output_format: "jpg".to_string(),
            tracking: None,
        };
        let a = target("Photo_{sequence}", 2, true);
        let b = target("Photo_{sequence}_mask_0_image", 1, false);
        fn item(global_index: usize, path: &str) -> PhotoItem<'_> {
            PhotoItem {
                global_index,
                image_path_str: path,
                appearance_count: 1,
                explicit_vc: None,
                total_paths: 2,
            }
        }
        let used = std::sync::Mutex::new(std::collections::HashSet::new());
        let date = jiff::Timestamp::now();
        let plan = |t: &ExportTarget, index, path: &str| {
            let planned = plan_output_path(
                t,
                &item(index, path),
                Path::new(path),
                &date,
                None,
                2,
                &used,
            )
            .unwrap();
            planned.file_name().unwrap().to_string_lossy().into_owned()
        };
        // Photo 2 reaches recipe B first; then photo 1, recipe A (2 layers).
        assert_eq!(plan(&b, 1, "/p/2.NEF"), "Photo_2_mask_0_image.jpg");
        assert_eq!(plan(&a, 0, "/p/1.NEF"), "Photo_2_1.jpg");
        assert_eq!(
            used.lock().unwrap().len(),
            6,
            "B's file, A's file and A's 2 × 2 mask files"
        );
        // The other way round: A's mask files are reserved before B names.
        used.lock().unwrap().clear();
        assert_eq!(plan(&a, 0, "/p/1.NEF"), "Photo_2.jpg");
        assert_eq!(plan(&b, 1, "/p/2.NEF"), "Photo_2_mask_0_image_1.jpg");
        assert!(
            used.lock()
                .unwrap()
                .contains(&dir.path().join("Photo_2_mask_1_alpha.png"))
        );
    }

    #[test]
    fn a_virtual_copy_is_not_its_open_master_nor_the_reverse() {
        assert!(is_open_edit("/p/a.RAF", Some("/p/a.RAF")));
        assert!(is_open_edit(
            "/p/a.RAF?vc=1a2b3c",
            Some("/p/a.RAF?vc=1a2b3c")
        ));
        assert!(!is_open_edit("/p/a.RAF?vc=1a2b3c", Some("/p/a.RAF")));
        assert!(!is_open_edit("/p/a.RAF", Some("/p/a.RAF?vc=1a2b3c")));
        assert!(!is_open_edit("/p/a.RAF", None));
    }
}
