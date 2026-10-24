//! Desktop glue for `rawtonic_export::export_processing` (ADR 0009): the export
//! queue, its Tauri commands and events, sidecar and session access and the
//! metadata embedding stay here; the render and encoders live in the crate.
pub use rawtonic_export::export_processing::*;

use std::borrow::Cow;
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use image::{DynamicImage, GenericImageView, GrayImage, ImageBuffer, Luma, imageops};
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

use crate::adjustment_utils::{apply_all_transformations, hydrate_adjustments};
use crate::app_settings::load_settings;
use crate::cache_utils::{calculate_full_job_hash, calculate_transform_hash};
use crate::editor_preview::generate_transformed_preview;
use crate::export_queue::Stage;
use crate::mask_generation::{get_cached_or_generate_mask, resolve_warped_image_for_masks};

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

/// The pristine source of a full-resolution render, read from disk: RAWs
/// develop with the recipe's AI Denoise. Every export path (single images,
/// collages) loads its photos here so the same recipe develops alike; the
/// denoiser must be loaded beforehand (`ai_denoise::denoiser`), else such a
/// RAW fails.
pub(crate) fn load_export_base_image(
    state: &AppState,
    source_path: &str,
    is_raw: bool,
    adjustments: &Value,
    settings: &crate::app_settings::AppSettings,
    denoise_progress: Option<rawtonic_rawdecode::mosaic_denoise::DenoiseProgress>,
) -> Result<(DynamicImage, Option<crate::image_loader::DecodeMeasures>), String> {
    let ai = rawtonic_engine::image_loader::ai_denoise_settings(adjustments).filter(|_| is_raw);
    let denoise = if let Some(ai) = ai {
        let denoiser = state.ai_denoiser.lock().unwrap()[crate::ai_denoise::slot(ai.model())]
            .clone()
            .ok_or_else(|| {
                format!(
                    "{} uses AI Denoise, but its model is not available",
                    source_path
                )
            })?;
        let denoise = rawtonic_rawdecode::mosaic_denoise::MosaicDenoise::new(denoiser, ai.blend)
            .with_noise_scale(ai.noise_scale);
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
        .map_err(|e| format!("Failed to load from mmap: {}", e)),
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
            .map_err(|e| format!("Failed to load from bytes: {}", e))
        }
    }
}

/// Loads the AI Denoise models these recipes use, once each; photos
/// that need it fail one by one if it cannot be had.
pub(crate) async fn prepare_ai_denoise<V: std::borrow::Borrow<Value>>(
    app_handle: &tauri::AppHandle,
    recipes: impl IntoIterator<Item = V>,
) {
    let models: std::collections::BTreeSet<usize> = recipes
        .into_iter()
        .filter_map(|adjustments| {
            rawtonic_engine::image_loader::ai_denoise_settings(adjustments.borrow())
                .map(|ai| ai.model())
        })
        .collect();
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

fn set_timestamps_from_exif(src: &Path, dst: &Path) {
    set_capture_file_times(dst, exif_processing::get_creation_date_from_path(src));
}

fn save_image_with_metadata(
    image: DynamicImage,
    output_path: &std::path::Path,
    source_path_str: &str,
    export_settings: &ExportSettings,
    generated: bool,
) -> Result<(), String> {
    let source_file = Path::new(source_path_str);
    let exif_sidecar = || exif_processing::read_rrexif_sidecar(source_file);
    let catalog = || crate::session_bridge::catalog_metadata(source_file);
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
        },
    )
}

/// Writes export bytes to a new file (never over an existing one).
fn write_export_bytes(
    output_path: &std::path::Path,
    _extension: &str,
    image_bytes: Vec<u8>,
) -> Result<(), String> {
    crate::session_bridge::write_new_output(output_path, image_bytes)
        .map_err(|e| format!("Failed to write file to '{}': {}", output_path.display(), e))
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
    state: &tauri::State<AppState>,
    is_raw: bool,
    app_handle: &tauri::AppHandle,
    cancellation_token: &AtomicBool,
) -> Result<(), String> {
    ensure_export_not_cancelled(cancellation_token)?;
    let (transformed_image, unscaled_crop_offset) =
        apply_all_transformations(Cow::Borrowed(base_image), js_adjustments);
    ensure_export_not_cancelled(cancellation_token)?;
    let (img_w, img_h) = transformed_image.dimensions();
    let mask_definitions: Vec<MaskDefinition> = js_adjustments
        .get("masks")
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
        let tm_override = resolve_tonemapper_override_from_handle(app_handle, is_raw);
        let all_adjustments = get_all_adjustments_from_json(js_adjustments, is_raw, tm_override);
        let lut_path = js_adjustments["lutPath"].as_str();
        let lut = lut_path.and_then(|p| get_or_load_lut(state, p).ok());
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
                state,
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

            let (with_options, geometry) =
                apply_export_geometry_and_watermark(processed, export_settings)?;

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
            )?;
            ensure_export_not_cancelled(cancellation_token)?;

            if export_settings.preserve_timestamps {
                set_timestamps_from_exif(Path::new(source_path_str), &mask_image_path);
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
fn mask_file_paths(output_path: &Path, layer: usize) -> [std::path::PathBuf; 2] {
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

fn export_adjustments_as_lut(
    js_adjustments: &Value,
    source_path_str: &str,
    context: &Arc<GpuContext>,
    state: &tauri::State<AppState>,
    app_handle: &tauri::AppHandle,
    cancellation_token: &AtomicBool,
) -> Result<Vec<u8>, String> {
    ensure_export_not_cancelled(cancellation_token)?;
    let lut_size = 33;
    let identity_image = generate_identity_lut_image(lut_size);

    let tm_override = resolve_tonemapper_override_from_handle(app_handle, false);
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

    let lut_path = js_adjustments["lutPath"].as_str();
    let lut = lut_path.and_then(|p| get_or_load_lut(state, p).ok());
    let unique_hash = calculate_full_job_hash(source_path_str, js_adjustments);

    let processed_lut = process_and_get_dynamic_image(
        context,
        state,
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

/// Export workers at most. The GPU renders one photo at a time and is the
/// slowest stage; four keep it busy while they load, upload and encode the
/// other photos. Measured on 20 RAWs with two recipes (M5 Pro): 4 workers
/// 23.5 s, 6 23.7 s, 8 26.1 s; with one render for both recipes 4 workers
/// 16.4 s, 6 18.6 s, 8 19.1 s (notes: parity-gaps/G43, F10).
const MAX_EXPORT_WORKERS: usize = 4;
/// Memory an export worker takes, measured: four workers on 24–45 MP RAWs
/// with two recipes peaked 7.8 GB above the app's footprint before the
/// export.
const EXPORT_WORKER_GB: f64 = 2.0;
/// Free memory a batch export leaves: no further photo starts below it.
const EXPORT_MEMORY_RESERVE: u64 = 2 << 30;

/// Exports keep their GPU processor (its textures and compiled pipelines)
/// between runs until the system runs short of memory; one that a render
/// holds stays, and goes when its run ends (`release_if_short_of_memory`).
pub(crate) fn release_processor_under_pressure(app_handle: &tauri::AppHandle) {
    let app_handle = app_handle.clone();
    crate::memory_pressure::watch(Arc::new(move |_| {
        let state = app_handle.state::<AppState>();
        if state.export_gpu_processor.take_if_idle().is_some() {
            log::info!("Memory pressure: released the exports' GPU processor");
        }
    }));
}

/// At the end of an export run: pressure is reported once on entering it,
/// so pressure that came while a render held the processor would otherwise
/// leave it allocated for as long as memory stays short.
fn release_if_short_of_memory<T>(
    processor: &crate::gpu_processing::ProcessorSlot<Option<T>>,
    short_of_memory: impl FnOnce() -> bool,
) -> bool {
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
/// So no job of a worker's pool waits for it, and renders hand their work
/// to other pools as before (`mark_private_pool_thread`).
struct ExportLanes {
    pools: Vec<rayon::ThreadPool>,
    free: Mutex<Vec<usize>>,
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
        let pools = (0..workers)
            .map(|lane| {
                rayon::ThreadPoolBuilder::new()
                    .num_threads(threads)
                    .thread_name(move |i| format!("export-{lane}-{i}"))
                    .start_handler(|_| {
                        lower_thread_priority();
                        crate::gpu_processing::mark_private_pool_thread();
                    })
                    .build()
                    .expect("export thread pool")
            })
            .collect();
        Arc::new(Self {
            free: Mutex::new((0..workers).rev().collect()),
            pools,
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
/// Linux: nice 10, which an unprivileged thread cannot undo, so only for
/// threads that do nothing else. macOS keeps the default: its lower classes
/// move threads to the efficiency cores (see `proxy_worker`'s builds).
#[cfg(target_os = "linux")]
fn lower_thread_priority() {
    // SAFETY: changes the calling thread's own nice value; no pointers.
    unsafe {
        libc::setpriority(libc::PRIO_PROCESS, libc::gettid() as libc::id_t, 10);
    }
}

#[cfg(not(target_os = "linux"))]
fn lower_thread_priority() {}

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
    adjustments_mode: ExportAdjustmentsMode,
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
    completion_tx: Option<tokio::sync::oneshot::Sender<Result<(), usize>>>,
    max_jobs: Option<usize>,
) -> Result<(), String> {
    let cancellation_token = register_export_task(&state.export_task_token)?;
    let task_guard = ExportTaskGuard::with_app_handle(
        Arc::clone(&state.export_task_token),
        Arc::clone(&cancellation_token),
        app_handle.clone(),
    );
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    if cancellation_token.load(Ordering::SeqCst) {
        return Ok(());
    }

    #[cfg(feature = "performance-profiling")]
    let context_stage = crate::performance::PerformanceStage::new("export_gpu_context");
    let context = match get_or_init_gpu_context(&state, &app_handle) {
        Ok(context) => context,
        Err(_) if cancellation_token.load(Ordering::SeqCst) => return Ok(()),
        Err(error) => return Err(error),
    };

    if cancellation_token.load(Ordering::SeqCst) {
        return Ok(());
    }

    #[cfg(feature = "performance-profiling")]
    drop(context_stage);
    {
        let app_handle = app_handle.clone();
        let context = context.clone();
        std::thread::spawn(move || {
            crate::gpu_processing::prewarm_gpu_processor(
                &app_handle
                    .state::<crate::app_state::AppState>()
                    .export_gpu_processor,
                &context,
            );
        });
    }
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

    let available_cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);

    let mut sys = sysinfo::System::new();
    sys.refresh_memory();

    let available_ram_gb = sys.available_memory() as f64 / 1024.0 / 1024.0 / 1024.0;
    let ram_based_limit = (available_ram_gb / EXPORT_WORKER_GB).floor() as usize;

    let num_threads = if paths.len() == 1 {
        1
    } else {
        available_cores
            .min(ram_based_limit)
            .clamp(1, MAX_EXPORT_WORKERS)
            .min(max_jobs.unwrap_or(usize::MAX))
    };

    log::info!(
        "Batch Export: {} cores, {:.1} GB free RAM -> {} threads, {} recipes",
        available_cores,
        available_ram_gb,
        num_threads,
        targets.len()
    );

    let _export_task = tokio::spawn(async move {
        let _task_guard = task_guard;
        let recipes = paths.iter().map(|path| match &adjustments_mode {
            ExportAdjustmentsMode::GlobalOverride(adj) => adj.clone(),
            ExportAdjustmentsMode::UseSidecars { .. } => {
                crate::exif_processing::load_sidecar(&parse_virtual_path(path).1).adjustments
            }
        });
        prepare_ai_denoise(&app_handle, recipes).await;
        let settings = load_settings(app_handle.clone()).unwrap_or_default();
        let targets = Arc::new(targets);

        let export_items = rawtonic_export::sidecars::plan_export_items(paths);

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
                    sys.available_memory() < EXPORT_MEMORY_RESERVE
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
            let adjustments_mode = adjustments_mode.clone();
            let used_paths_clone = Arc::clone(&used_paths);
            let lane = lanes.take();

            let handle = tokio::task::spawn_blocking(move || {
                let results = lane.install(|| {
                    export_photo(
                        &app_handle_clone,
                        &context_clone,
                        &settings,
                        &targets,
                        &wanted,
                        &adjustments_mode,
                        &used_paths_clone,
                        &cancellation_token_clone,
                        PhotoItem {
                            global_index,
                            image_path_str: &image_path_str,
                            appearance_count,
                            explicit_vc,
                            total_paths,
                        },
                    )
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
                Err(e) => results.push(Err(format!("Thread crashed: {}", e))),
            }
        }

        if release_if_short_of_memory(
            &app_handle.state::<AppState>().export_gpu_processor,
            crate::memory_pressure::under_pressure,
        ) {
            log::info!("Memory pressure: released the exports' GPU processor after the run");
        }
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
        let export_state = app_handle.state::<AppState>();
        let finalized = finish_export_task(
            &export_state.export_task_token,
            &cancellation_token,
            |cancelled| {
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
            },
        );

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
struct PhotoItem<'a> {
    global_index: usize,
    image_path_str: &'a str,
    appearance_count: usize,
    explicit_vc: Option<u32>,
    total_paths: usize,
}

/// The file a recipe writes for a photo: its name from the recipe's
/// template, in the recipe's folder, never over an existing file or another
/// output of this run. With separate mask files the name holds for its
/// `mask_layers` layers' files too, which are reserved with it: recipes of
/// photos exported side by side would otherwise take the name of a mask
/// file not written yet.
fn plan_output_path(
    target: &ExportTarget,
    item: &PhotoItem,
    source_path: &Path,
    file_date: &chrono::DateTime<chrono::Utc>,
    mask_layers: usize,
    used_paths: &Mutex<std::collections::HashSet<std::path::PathBuf>>,
) -> Result<std::path::PathBuf, String> {
    let export_settings = &target.export_settings;
    let output_folder_path = Path::new(&target.output_folder_or_file);
    let total_paths = item.total_paths;
    let filename_template = export_settings
        .filename_template
        .as_deref()
        .unwrap_or("{original_filename}_edited");

    let (sequence, last_sequence) =
        export_settings.filename_counter(item.global_index, total_paths);
    let new_stem = rawtonic_export::sidecars::export_stem(
        filename_template,
        source_path,
        sequence,
        last_sequence,
        file_date,
        item.appearance_count,
        item.explicit_vc,
    );

    let new_filename = format!("{}.{}", new_stem, target.output_format);

    let beside_original = export_settings.destination_type.as_deref() == Some("originalFolder");
    let mut output_path = if !beside_original && target.is_explicit_file_path && total_paths == 1 {
        output_folder_path.to_path_buf()
    } else {
        let dir = export_directory(
            source_path,
            output_folder_path,
            export_settings,
            &target.base_origin_folders,
        )?;
        if dir != output_folder_path
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

    if !(target.is_explicit_file_path && total_paths == 1) {
        let mut used = used_paths.lock().unwrap();
        let parent_dir = output_path
            .parent()
            .unwrap_or(std::path::Path::new(""))
            .to_path_buf();
        let mut counter = 1;
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

        while family(&output_path)
            .iter()
            .any(|path| path.exists() || used.contains(path))
        {
            let incremented_filename = format!("{}_{}.{}", new_stem, counter, extension);
            output_path = parent_dir.join(&incremented_filename);
            counter += 1;
        }
        used.extend(family(&output_path));
    }
    Ok(output_path)
}

/// Each of `targets` in turn: its file is planned right before it is
/// exported, so it sees the files the recipes before it wrote (their mask
/// files too), as when each recipe was an export of its own.
fn export_in_turn(
    targets: impl IntoIterator<Item = usize>,
    mut plan: impl FnMut(usize) -> Result<std::path::PathBuf, String>,
    mut export: impl FnMut(usize, &Path) -> Result<(), String>,
) -> Vec<(usize, Result<(), String>)> {
    targets
        .into_iter()
        .map(|target| (target, plan(target).and_then(|path| export(target, &path))))
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
    adjustments_mode: &ExportAdjustmentsMode,
    used_paths: &Mutex<std::collections::HashSet<std::path::PathBuf>>,
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
    let image_path_str = item.image_path_str;
    let (source_path, sidecar_path) = parse_virtual_path(image_path_str);
    let source_path_str = source_path.to_string_lossy().to_string();

    let is_current_edit = match adjustments_mode {
        ExportAdjustmentsMode::UseSidecars { active_path, .. } => {
            is_open_edit(image_path_str, active_path.as_deref())
        }
        ExportAdjustmentsMode::GlobalOverride(_) => false,
    };

    let mut js_adjustments = match adjustments_mode {
        ExportAdjustmentsMode::UseSidecars {
            active_adjustments, ..
        } => {
            if is_current_edit {
                if let Some(adj) = active_adjustments {
                    adj.clone()
                } else {
                    crate::exif_processing::load_sidecar(&sidecar_path).adjustments
                }
            } else {
                crate::exif_processing::load_sidecar(&sidecar_path).adjustments
            }
        }
        ExportAdjustmentsMode::GlobalOverride(adj) => adj.clone(),
    };

    hydrate_adjustments(&state, &mut js_adjustments);
    // A never-edited photo exports with the camera's lens data,
    // as Develop shows it (profile corrections on by default).
    if let Err(e) =
        rawtonic_export::exif_processing::prepare_recipe_lens(&mut js_adjustments, &source_path)
    {
        return fail_all(e);
    }
    let is_raw = is_raw_file(&source_path_str);
    let original_path = std::path::Path::new(&source_path_str);
    let file_date = exif_processing::get_creation_date_from_path(original_path);

    // Each recipe's file is named (and recorded) right before it is
    // written, as when each recipe was its own export.
    let mask_layers =
        rawtonic_engine::image_processing::mask_definitions_from_json(&js_adjustments)
            .iter()
            .filter(|definition| definition.produces_bitmap())
            .take(crate::image_processing::MAX_MASKS)
            .count();
    let plan = |target: usize| {
        let output_path = plan_output_path(
            &targets[target],
            &item,
            original_path,
            &file_date,
            mask_layers,
            used_paths,
        )?;
        if let Some(tracking) = &targets[target].tracking {
            tracking.started(item.global_index, &output_path);
        }
        Ok(output_path)
    };
    let is_cube = |target: usize| targets[target].output_format.to_lowercase() == "cube";
    let mut results = export_in_turn(
        wanted.iter().copied().filter(|&t| is_cube(t)),
        plan,
        |_, output_path| {
            let cube_bytes = export_adjustments_as_lut(
                &js_adjustments,
                &source_path_str,
                context,
                &state,
                app_handle,
                cancellation_token,
            )?;
            ensure_export_not_cancelled(cancellation_token)?;
            crate::session_bridge::write_new_output(output_path, cube_bytes)
                .map_err(|e| e.to_string())
        },
    );
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
            .offline_source
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let cached_source = if is_current_edit {
            state
                .original_image
                .lock()
                .unwrap()
                .as_ref()
                .filter(|loaded| parse_virtual_path(&loaded.path).0 == source_path)
                .filter(|loaded| offline_source.as_deref() != Some(loaded.path.as_str()))
                // Developed for the decode variant exported.
                .filter(|loaded| {
                    loaded.decode
                        == Some(crate::image_loader::decode_variant(&js_adjustments, is_raw))
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
                &state,
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
        let source =
            prepare_export_source(&source_path_str, pristine_source, measures, &js_adjustments)?;
        ensure_export_not_cancelled(cancellation_token)?;
        Ok(source)
    })();
    let source = match source {
        Ok(source) => source,
        Err(e) => {
            results.extend(images.into_iter().map(|target| (target, Err(e.clone()))));
            return results;
        }
    };

    let tonemapper_override = |is_raw| resolve_tonemapper_override_from_handle(app_handle, is_raw);
    let load_lut = |path: &str| get_or_load_lut(&state, path).ok();
    let env = RenderEnv {
        context,
        processor: &state.export_gpu_processor,
        image_cache: &state.gpu_image_cache,
        tonemapper_override: &tonemapper_override,
        load_lut: &load_lut,
    };
    // The develop of each kind of main file the recipes want: with its
    // mask layers, or without them when they go to separate files.
    let with_mask_files = |target: usize| targets[target].export_settings.export_masks;
    let mut prepared: [Option<Result<PreparedExport, String>>; 2] = [None, None];
    for &target in &images {
        let masks = with_mask_files(target);
        if prepared[masks as usize].is_none() {
            #[cfg(feature = "performance-profiling")]
            let _stage = crate::performance::PerformanceStage::new("export_prepare");
            prepared[masks as usize] = Some(prepare_export_render(
                &source_path_str,
                source.clone(),
                &main_export_adjustments(&js_adjustments, &targets[target].export_settings),
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
    let precision = |target: usize| {
        render_output_precision(
            &targets[target].output_format,
            &targets[target].export_settings,
        )
    };
    let mut groups: Vec<Vec<usize>> = Vec::new();
    for run in
        images.chunk_by(|&a, &b| with_mask_files(a) == with_mask_files(b) && !alone(a) && !alone(b))
    {
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
    let mut rendered = GroupRenders::new(groups);
    // Only separate mask files need source pixels after rendering.
    let mask_source = images
        .iter()
        .any(|&target| with_mask_files(target))
        .then_some(source);

    let source_file = Path::new(&source_path_str);
    let exif_sidecar = || exif_processing::read_rrexif_sidecar(source_file);
    let catalog = || crate::session_bridge::catalog_metadata(source_file);
    results.extend(export_in_turn(images, plan, |target, output_path| {
        let export_settings = &targets[target].export_settings;
        {
            let prepared = prepared[export_settings.export_masks as usize]
                .as_ref()
                .expect("prepared for every wanted recipe")
                .as_ref()
                .map_err(Clone::clone)?;
            ensure_export_not_cancelled(cancellation_token)?;
            let output = ExportOutput {
                path: output_path,
                format: targets[target].output_format.as_str(),
                exif_sidecar: &exif_sidecar,
                catalog: &catalog,
                write: &write_export_bytes,
            };
            let cancelled = || ensure_export_not_cancelled(cancellation_token);
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
                render_prepared_exports(prepared, &env, "process_image_for_export", &outputs)
            });
            match render {
                Some(processed) => save_rendered_export(
                    processed?,
                    &source_path_str,
                    &js_adjustments,
                    export_settings,
                    &output,
                    &cancelled,
                )?,
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
                set_timestamps_from_exif(Path::new(&source_path_str), output_path);
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
                    &state,
                    is_raw,
                    app_handle,
                    &finish_photo,
                )?;
            }
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
            output_folder_or_file,
            is_explicit_file_path,
            base_origin_folders,
            export_settings,
            output_format,
            current_edit_path,
            current_edit_adjustments,
        },
    )
}

/// The exports of a photo set's ticked recipes, queued together so they run
/// as one: each photo is developed once for all of them.
#[tauri::command]
pub async fn export_images_batch(
    requests: Vec<crate::export_queue::ExportRequest>,
    app_handle: tauri::AppHandle,
) -> Result<Vec<String>, String> {
    if requests.iter().any(|request| request.paths.is_empty()) || requests.is_empty() {
        return Err("Nothing to export.".to_string());
    }
    crate::export_queue::enqueue_all(&app_handle, requests)
}

pub async fn run_headless_export(
    session: crate::launch_request::HeadlessExportSession,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    println!("Starting headless export...");
    let state = app_handle.state::<crate::app_state::AppState>();

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
    let is_explicit_file_path =
        paths.len() == 1 && output_path.extension().is_some() && !output_path.is_dir();

    if is_explicit_file_path {
        if let Some(parent) = output_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create output parent directory: {}", e))?;
        }
    } else {
        std::fs::create_dir_all(&session.output)
            .map_err(|e| format!("Failed to create output directory: {}", e))?;
    }

    println!("Found {} images to export. Processing...", paths.len());

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
        strip_creator_contact: session.strip_creator_contact,
        filename_template: None,
        sequence_start: None,
        watermark: None,
        export_masks: false,
        preserve_folders: true,
        destination_type: None,
        subfolder: None,
        output_sharpening: None,
    };

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

    let mode = if let Some(adj) = custom_adjustments {
        ExportAdjustmentsMode::GlobalOverride(adj)
    } else {
        ExportAdjustmentsMode::UseSidecars {
            active_path: None,
            active_adjustments: None,
        }
    };

    export_images_impl(
        paths,
        vec![ExportTarget {
            output_folder_or_file: session.output,
            is_explicit_file_path,
            base_origin_folders: vec![session.source],
            export_settings,
            output_format: session.format,
            tracking: None,
        }],
        mode,
        state.clone(),
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

#[tauri::command]
pub fn cancel_export(app_handle: tauri::AppHandle) -> Result<(), String> {
    // The queue waits after a cancel until the next export or resume.
    crate::export_queue::hold();
    cancel_running_export(&app_handle)
}

/// Stops the running export at the next checkpoint (the queue goes on).
pub(crate) fn cancel_running_export(app_handle: &tauri::AppHandle) -> Result<(), String> {
    let state = app_handle.state::<AppState>();
    match request_export_cancellation(&state.export_task_token, || {
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
    if output_format.to_lowercase() == "cube" {
        return Ok(1_050_000 * paths.len());
    }

    if paths.is_empty() {
        return Ok(0);
    }

    let first_path = &paths[0];
    let (source_path, sidecar_path) = parse_virtual_path(first_path);
    let source_path_str = source_path.to_string_lossy().to_string();

    let context = get_or_init_gpu_context(&state, &app_handle)?;
    let is_current_edit = is_open_edit(first_path, current_edit_path.as_deref());
    let is_raw = is_raw_file(&source_path_str);
    let settings = load_settings(app_handle.clone()).unwrap_or_default();

    let single_image_extrapolated_size: usize = if is_current_edit
        && current_edit_adjustments.is_some()
    {
        let loaded_image = state
            .original_image
            .lock()
            .unwrap()
            .clone()
            .ok_or("No original image loaded")?;
        let mut adjustments_clone = current_edit_adjustments.clone().unwrap();
        hydrate_adjustments(&state, &mut adjustments_clone);

        let new_transform_hash = calculate_transform_hash(&adjustments_clone);
        let cached_preview_lock = state.cached_preview.lock().unwrap();
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
                    &state,
                    &loaded_image,
                    &adjustments_clone,
                    preview_dim,
                )?
            }
        } else {
            drop(cached_preview_lock);
            generate_transformed_preview(&state, &loaded_image, &adjustments_clone, preview_dim)?
        };

        let (img_w, img_h) = preview_image.dimensions();
        let mask_definitions: Vec<MaskDefinition> = adjustments_clone
            .get("masks")
            .and_then(|m| serde_json::from_value(m.clone()).ok())
            .unwrap_or_default();

        let scaled_crop_offset = (
            unscaled_crop_offset.0 * scale,
            unscaled_crop_offset.1 * scale,
        );

        let warped_image = resolve_warped_image_for_masks(
            &state,
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
                    &state,
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
        );
        all_adjustments.global.detail_scale = scale;
        all_adjustments.global.show_clipping = 0;

        let lut = adjustments_clone["lutPath"]
            .as_str()
            .and_then(|p| get_or_load_lut(&state, p).ok());
        let unique_hash =
            calculate_full_job_hash(&loaded_image.path, &adjustments_clone).wrapping_add(1);

        let processed_preview = process_and_get_dynamic_image_with_precision(
            &context,
            &state,
            &preview_image,
            unique_hash,
            RenderRequest {
                adjustments: all_adjustments,
                mask_bitmaps: &mask_bitmaps,
                lut,
                roi: None,
            },
            "estimate_export_size",
            render_output_precision(&output_format, &export_settings),
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
            apply_all_transformations(&loaded_image.image, &adjustments_clone);
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
        let _ = rawtonic_export::exif_processing::prepare_recipe_lens(
            &mut js_adjustments,
            Path::new(&source_path_str),
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

        if let Some(crop_val) = js_adjustments.get_mut("crop")
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
            apply_all_transformations(Cow::Borrowed(&original_image), &js_adjustments);
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
            .get("masks")
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
                    &state,
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
        );
        all_adjustments.global.detail_scale = total_scale;
        all_adjustments.global.show_clipping = 0;

        let lut = js_adjustments["lutPath"]
            .as_str()
            .and_then(|p| get_or_load_lut(&state, p).ok());
        let unique_hash =
            calculate_full_job_hash(&source_path_str, &js_adjustments).wrapping_add(1);

        let processed_preview = process_and_get_dynamic_image_with_precision(
            &context,
            &state,
            &preview_base,
            unique_hash,
            RenderRequest {
                adjustments: all_adjustments,
                mask_bitmaps: &mask_bitmaps,
                lut,
                roi: None,
            },
            "estimate_batch_export_size",
            render_output_precision(&output_format, &export_settings),
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
    use super::{ExportTarget, PhotoItem, export_in_turn, is_open_edit, plan_output_path};

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
    #[test]
    fn the_end_of_a_run_frees_a_processor_pressure_found_in_use() {
        use crate::gpu_processing::ProcessorSlot;
        let processor = ProcessorSlot::background(Some("processor"));
        let render = processor.lock().unwrap();
        assert_eq!(processor.take_if_idle(), None, "the watch leaves it");
        drop(render);
        assert!(!super::release_if_short_of_memory(&processor, || false));
        assert!(
            processor.lock().unwrap().is_some(),
            "kept with memory to spare"
        );
        assert!(super::release_if_short_of_memory(&processor, || true));
        assert!(processor.lock().unwrap().is_none());
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
        let date = chrono::Utc::now();
        let mut written = Vec::new();
        let results = export_in_turn(
            [0, 1],
            |t| {
                plan_output_path(
                    &targets[t],
                    &item,
                    Path::new("/photos/foo.NEF"),
                    &date,
                    1,
                    &used,
                )
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
        let date = chrono::Utc::now();
        let plan = |t: &ExportTarget, index, path: &str| {
            let planned =
                plan_output_path(t, &item(index, path), Path::new(path), &date, 2, &used).unwrap();
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
