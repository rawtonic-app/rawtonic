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
) -> Result<DynamicImage, String> {
    let ai = rawtonic_engine::image_loader::ai_denoise_settings(adjustments).filter(|_| is_raw);
    let denoise = if let Some(ai) = ai {
        let denoiser = state.ai_denoiser.lock().unwrap()[ai.model()]
            .clone()
            .ok_or_else(|| {
                format!(
                    "{} uses AI Denoise, but its model is not available",
                    source_path
                )
            })?;
        Some(
            rawtonic_rawdecode::mosaic_denoise::MosaicDenoise::new(denoiser, ai.blend)
                .with_noise_scale(ai.noise_scale),
        )
    } else {
        None
    };
    match read_file_mapped(Path::new(source_path)) {
        Ok(mmap) => crate::image_loader::load_base_image_for_recipe(
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
            crate::image_loader::load_base_image_for_recipe(
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
    let mask_geometry = PhotoGeometry::from_adjustments(js_adjustments).with_photo_size(
        pristine_source.width() as f64,
        pristine_source.height() as f64,
    );
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
        let output_dir = output_path_obj.parent().unwrap_or(output_path_obj);
        let stem = output_path_obj
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("export");
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

            let mask_image_path =
                output_dir.join(format!("{}_mask_{}_image.{}", stem, i, extension));
            let mask_alpha_path = output_dir.join(format!("{}_mask_{}_alpha.png", stem, i));

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

#[allow(clippy::too_many_arguments)]
pub(crate) async fn export_images_impl(
    paths: Vec<String>,
    output_folder_or_file: String,
    is_explicit_file_path: bool,
    base_origin_folders: Vec<String>,
    export_settings: ExportSettings,
    output_format: String,
    adjustments_mode: ExportAdjustmentsMode,
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
    completion_tx: Option<tokio::sync::oneshot::Sender<Result<(), usize>>>,
    max_jobs: Option<usize>,
    tracking: Option<crate::export_queue::Tracking>,
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
                &app_handle.state::<crate::app_state::AppState>(),
                &context,
            );
        });
    }
    let context = Arc::new(context);
    // A resumed job counts the photos an earlier run exported.
    let progress_counter = Arc::new(AtomicUsize::new(
        tracking.as_ref().map_or(0, |t| t.already_done),
    ));

    let available_cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);

    let mut sys = sysinfo::System::new();
    sys.refresh_memory();

    let available_ram_gb = sys.available_memory() as f64 / 1024.0 / 1024.0 / 1024.0;
    let ram_based_limit = (available_ram_gb / 4.0).floor() as usize;

    let num_threads = if paths.len() == 1 {
        1
    } else {
        available_cores
            .min(ram_based_limit)
            .clamp(1, 4)
            .min(max_jobs.unwrap_or(usize::MAX))
    };

    log::info!(
        "Batch Export: {} cores, {:.1} GB free RAM -> {} threads",
        available_cores,
        available_ram_gb,
        num_threads
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
        let output_folder_path = std::path::Path::new(&output_folder_or_file);
        let total_paths = paths.len();
        let settings = load_settings(app_handle.clone()).unwrap_or_default();

        let export_items = rawtonic_export::sidecars::plan_export_items(paths);

        let used_paths = Arc::new(Mutex::new(std::collections::HashSet::new()));
        let semaphore = Arc::new(tokio::sync::Semaphore::new(num_threads));
        let mut join_handles = Vec::new();

        for (global_index, image_path_str, appearance_count, explicit_vc) in export_items {
            if cancellation_token.load(Ordering::SeqCst) {
                break;
            }
            if tracking.as_ref().is_some_and(|t| !t.wants(global_index)) {
                continue;
            }
            let permit = semaphore.clone().acquire_owned().await.unwrap();
            if cancellation_token.load(Ordering::SeqCst) {
                drop(permit);
                break;
            }

            let app_handle_clone = app_handle.clone();
            let context_clone = Arc::clone(&context);
            let progress_counter_clone = Arc::clone(&progress_counter);
            let output_folder_path = output_folder_path.to_path_buf();
            let base_origin_folders = base_origin_folders.clone();
            let export_settings = export_settings.clone();
            let output_format = output_format.clone();
            let settings = settings.clone();
            let cancellation_token_clone = Arc::clone(&cancellation_token);
            let adjustments_mode = adjustments_mode.clone();
            let used_paths_clone = Arc::clone(&used_paths);
            let tracking_clone = tracking.clone();

            let handle = tokio::task::spawn_blocking(move || {
                ensure_export_not_cancelled(&cancellation_token_clone)?;

                let state = app_handle_clone.state::<AppState>();
                let (source_path, sidecar_path) = parse_virtual_path(&image_path_str);
                let source_path_str = source_path.to_string_lossy().to_string();

                let is_current_edit = match &adjustments_mode {
                    ExportAdjustmentsMode::UseSidecars { active_path, .. } => {
                        Some(&source_path_str) == active_path.as_ref()
                    }
                    ExportAdjustmentsMode::GlobalOverride(_) => false,
                };

                let mut js_adjustments = match &adjustments_mode {
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
                crate::embedded_lens::apply_default(&mut js_adjustments, &source_path);
                let is_raw = is_raw_file(&source_path_str);
                let original_path = std::path::Path::new(&source_path_str);
                let file_date = exif_processing::get_creation_date_from_path(original_path);

                let filename_template = export_settings
                    .filename_template
                    .as_deref()
                    .unwrap_or("{original_filename}_edited");

                let (sequence, last_sequence) =
                    export_settings.filename_counter(global_index, total_paths);
                let new_stem = rawtonic_export::sidecars::export_stem(
                    filename_template,
                    original_path,
                    sequence,
                    last_sequence,
                    &file_date,
                    appearance_count,
                    explicit_vc,
                );

                let new_filename = format!("{}.{}", new_stem, output_format);

                let beside_original =
                    export_settings.destination_type.as_deref() == Some("originalFolder");
                let mut output_path =
                    if !beside_original && is_explicit_file_path && total_paths == 1 {
                        output_folder_path.clone()
                    } else {
                        let dir = export_directory(
                            source_path.as_path(),
                            &output_folder_path,
                            &export_settings,
                            &base_origin_folders,
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

                let extension = output_format.to_lowercase();

                if !(is_explicit_file_path && total_paths == 1) {
                    let mut used = used_paths_clone.lock().unwrap();
                    let parent_dir = output_path
                        .parent()
                        .unwrap_or(std::path::Path::new(""))
                        .to_path_buf();
                    let mut counter = 1;

                    while output_path.exists() || used.contains(&output_path) {
                        let incremented_filename =
                            format!("{}_{}.{}", new_stem, counter, extension);
                        output_path = parent_dir.join(&incremented_filename);
                        counter += 1;
                    }
                    used.insert(output_path.clone());
                }
                if let Some(tracking) = &tracking_clone {
                    tracking.started(global_index, &output_path);
                }

                let result: Result<(), String> = (|| {
                    if extension == "cube" {
                        let cube_bytes = export_adjustments_as_lut(
                            &js_adjustments,
                            &source_path_str,
                            &context_clone,
                            &state,
                            &app_handle_clone,
                            &cancellation_token_clone,
                        )?;
                        ensure_export_not_cancelled(&cancellation_token_clone)?;
                        crate::session_bridge::write_new_output(&output_path, cube_bytes)
                            .map_err(|e| e.to_string())?;
                        ensure_export_not_cancelled(&cancellation_token_clone)?;
                        return Ok(());
                    }

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
                            .filter(|loaded| {
                                offline_source.as_deref() != Some(loaded.path.as_str())
                            })
                            // Developed for the colour noise filter exported.
                            .filter(|loaded| {
                                loaded.multiscale
                                    == Some(rawtonic_engine::color_noise::multiscale(
                                        &js_adjustments,
                                        is_raw,
                                    ))
                            })
                            .map(|loaded| Arc::clone(&loaded.image))
                    } else {
                        None
                    };
                    #[cfg(feature = "performance-profiling")]
                    let source_stage =
                        crate::performance::PerformanceStage::new("export_load_source");
                    let pristine_source = if let Some(source) = cached_source {
                        source
                    } else {
                        Arc::new(load_export_base_image(
                            &state,
                            &source_path_str,
                            is_raw,
                            &js_adjustments,
                            &settings,
                        )?)
                    };
                    #[cfg(feature = "performance-profiling")]
                    drop(source_stage);
                    let source =
                        prepare_export_source(&source_path_str, pristine_source, &js_adjustments)?;
                    // Only separate mask files need source pixels after rendering.
                    let mask_source = export_settings.export_masks.then(|| source.clone());
                    ensure_export_not_cancelled(&cancellation_token_clone)?;

                    let tonemapper_override =
                        |is_raw| resolve_tonemapper_override_from_handle(&app_handle_clone, is_raw);
                    let load_lut = |path: &str| get_or_load_lut(&state, path).ok();
                    let env = RenderEnv {
                        context: &context_clone,
                        processor: &state.gpu_processor,
                        image_cache: &state.gpu_image_cache,
                        tonemapper_override: &tonemapper_override,
                        load_lut: &load_lut,
                    };
                    let source_file = Path::new(&source_path_str);
                    let exif_sidecar = || exif_processing::read_rrexif_sidecar(source_file);
                    let catalog = || crate::session_bridge::catalog_metadata(source_file);
                    render_and_save(
                        &source_path_str,
                        source,
                        &js_adjustments,
                        &export_settings,
                        &env,
                        is_raw,
                        &ExportOutput {
                            path: &output_path,
                            format: output_format.as_str(),
                            exif_sidecar: &exif_sidecar,
                            catalog: &catalog,
                            write: &write_export_bytes,
                        },
                        &|| ensure_export_not_cancelled(&cancellation_token_clone),
                    )?;
                    ensure_export_not_cancelled(&cancellation_token_clone)?;

                    if export_settings.preserve_timestamps {
                        set_timestamps_from_exif(Path::new(&source_path_str), &output_path);
                    }
                    ensure_export_not_cancelled(&cancellation_token_clone)?;

                    if let Some(source) = mask_source.as_ref() {
                        export_masks_for_image(
                            source.base(),
                            &source.pristine,
                            &js_adjustments,
                            &export_settings,
                            &output_path,
                            &source_path_str,
                            &context_clone,
                            &state,
                            is_raw,
                            &app_handle_clone,
                            &cancellation_token_clone,
                        )?;
                    }

                    Ok(())
                })();

                if !cancellation_token_clone.load(Ordering::SeqCst) {
                    let current_progress =
                        progress_counter_clone.fetch_add(1, Ordering::SeqCst) + 1;
                    let _ = app_handle_clone.emit(
                        "batch-export-progress",
                        serde_json::json!({
                            "current": current_progress,
                            "total": total_paths,
                            "path": &image_path_str
                        }),
                    );
                }

                drop(permit);
                if cancellation_token_clone.load(Ordering::SeqCst) {
                    Err("Export cancelled".to_string())
                } else {
                    if let Some(tracking) = &tracking_clone {
                        tracking.finished(global_index, &result);
                    }
                    result
                }
            });

            join_handles.push(handle);
        }

        let mut results = Vec::new();
        for handle in join_handles {
            match handle.await {
                Ok(res) => results.push(res),
                Err(e) => results.push(Err(format!("Thread crashed: {}", e))),
            }
        }

        // Render and encode buffers of the last exports.
        crate::allocator::release_freed_memory();
        let errors: Vec<String> = results.into_iter().filter_map(Result::err).collect();
        let error_count = errors.len();
        if let Some(tracking) = &tracking
            && cancellation_token.load(Ordering::SeqCst)
        {
            tracking.cancelled.store(true, Ordering::SeqCst);
        }
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

                for error in &errors {
                    log::error!("Export error: {}", error);
                    if total_paths == 1 {
                        let _ = app_handle.emit("export-error", error.clone());
                    }
                }

                if error_count > 0 && total_paths > 1 {
                    let _ = app_handle.emit(
                        "export-error",
                        format!("{error_count} of {total_paths} exports failed"),
                    );
                } else if error_count == 0 {
                    let _ = app_handle.emit(
                        "batch-export-progress",
                        serde_json::json!({ "current": total_paths, "total": total_paths, "path": "" }),
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
        session.output,
        is_explicit_file_path,
        vec![session.source],
        export_settings,
        session.format,
        mode,
        state.clone(),
        app_handle.clone(),
        Some(tx),
        session.max_jobs,
        None,
    )
    .await?;

    match rx.await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(errors)) => Err(format!("Export completed with {} errors.", errors)),
        Err(_) => Err("Export task panicked or was cancelled.".to_string()),
    }
}

#[tauri::command]
pub fn cancel_export(
    state: tauri::State<AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    // The queue waits after a cancel until the next export or resume.
    crate::export_queue::hold();
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

#[tauri::command]
pub async fn estimate_export_sizes(
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
    let is_current_edit = Some(&source_path_str) == current_edit_path.as_ref();
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
        let mask_geometry = PhotoGeometry::from_adjustments(&adjustments_clone).with_photo_size(
            loaded_image.image.width() as f64,
            loaded_image.image.height() as f64,
        );
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
        crate::baseline_exposure::apply_base_look(&mut all_adjustments.global, &source_path_str);
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
        crate::embedded_lens::apply_default(&mut js_adjustments, Path::new(&source_path_str));

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
        crate::baseline_exposure::apply_base_look(&mut all_adjustments.global, &source_path_str);
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
