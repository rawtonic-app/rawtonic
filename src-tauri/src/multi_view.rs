//! Develop's multi view (several selected photographs side by side, as
//! Capture One's Multi view and Lightroom's Survey show them): the webview
//! lays out the cells and sends their screen rectangles; the native display
//! draws every cell (crates/gpu multi_view.rs). The primary photograph is the
//! normal live preview. Each other cell is developed here once at its cell's
//! size, from its preview proxy (or a fast decode) and its stored recipe, with
//! the display's shader variants, and kept on the GPU until its recipe or
//! size changes. Develop pixels never cross IPC.

use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use image::{DynamicImage, GenericImageView};
use tauri::Manager;

use crate::app_settings::load_settings;
use crate::app_state::AppState;
use crate::editor_preview::{reduced_preview_base, reduced_preview_scale};
use crate::file_management::{parse_virtual_path, read_file_mapped};
use crate::formats::is_raw_file;
use crate::gpu_processing::{
    self, CellExtent, CellLayer, CellZoom, GpuContext, LatestJobSender, MultiViewCell,
    ReadbackShader, RenderOutputPrecision, RenderRequest, RenderSource,
};
use crate::image_processing::{self, get_all_adjustments_from_json};
use crate::mask_generation::{
    MaskDefinition, ParametricMaskSource, PhotoGeometry, generate_mask_bitmap,
    prepare_parametric_mask_source,
};
use crate::preview_cache::PreviewBase;

/// Photographs whose bases are prepared at the same time (decode or proxy
/// read, warp, resample); the GPU renders them one after another.
const PREPARE_THREADS: usize = 4;

/// Cell textures are rendered at a multiple of this size, so a resize of a
/// few pixels does not develop every cell again.
const SIZE_STEP: f32 = 16.0;

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CellPayload {
    pub path: String,
    /// x, y, width, height in device pixels (window coordinates).
    pub area: [f32; 4],
}

struct Job {
    generation: usize,
    primary: Option<String>,
    cells: Vec<CellPayload>,
}

/// What a photograph's base was built from.
#[derive(Clone, Copy)]
struct Facts {
    full_width: u32,
    full_height: u32,
    is_raw: bool,
}

struct Base {
    preparation_key: u64,
    dim: u32,
    base: PreviewBase,
    mask_source: Option<ParametricMaskSource>,
}

#[derive(Default)]
struct Memory {
    facts: HashMap<String, Facts>,
    bases: HashMap<String, Base>,
}

struct Worker {
    generation: AtomicUsize,
    sender: LatestJobSender<Job>,
    memory: Mutex<Memory>,
}

static WORKER: OnceLock<Arc<Worker>> = OnceLock::new();

fn worker(app_handle: &tauri::AppHandle) -> Arc<Worker> {
    WORKER
        .get_or_init(|| {
            let app = app_handle.clone();
            Arc::new(Worker {
                generation: AtomicUsize::new(0),
                sender: gpu_processing::start_latest_job_worker(move |job: Job| {
                    let worker = WORKER.get().expect("multi view worker").clone();
                    run(&app, &worker, job);
                }),
                memory: Mutex::new(Memory::default()),
            })
        })
        .clone()
}

fn context_of(state: &AppState) -> Option<GpuContext> {
    state
        .gpu_context
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

/// Shows `cells` side by side in the viewer with `primary` as the live
/// photograph; no cells is the single view (every cell texture and prepared
/// base is released). Returns false when there is no native display (the
/// webview then offers no multi view).
#[tauri::command]
pub(crate) async fn set_multi_view(
    primary: Option<String>,
    cells: Vec<CellPayload>,
    app_handle: tauri::AppHandle,
) -> Result<bool, String> {
    tauri::async_runtime::spawn_blocking(move || show_cells(&app_handle, primary, cells))
        .await
        .map_err(|e| format!("Multi view task failed: {e}"))
}

fn show_cells(
    app_handle: &tauri::AppHandle,
    primary: Option<String>,
    cells: Vec<CellPayload>,
) -> bool {
    let state = app_handle.state::<AppState>();
    let Ok(context) = gpu_processing::get_or_init_gpu_context(&state, app_handle) else {
        return false;
    };
    let worker = worker(app_handle);
    let generation = worker.generation.fetch_add(1, Ordering::SeqCst) + 1;
    let zoom = {
        let mut display = context.display.lock().unwrap_or_else(|e| e.into_inner());
        let Some(display) = display.as_mut() else {
            return false;
        };
        display.set_multi_view(
            &context.device,
            &context.queue,
            primary.clone(),
            cells
                .iter()
                .map(|cell| MultiViewCell {
                    path: cell.path.clone(),
                    area: cell.area,
                })
                .collect(),
        );
        display.render(&context.device, &context.queue);
        display.multi_view().zoom()
    };
    if cells.is_empty() {
        *worker.memory.lock().unwrap_or_else(|e| e.into_inner()) = Memory::default();
        forget_regions(app_handle);
        return true;
    }
    // Zoomed in: regions of new cells, or of recipes that changed.
    if let Some(zoom) = zoom {
        let regions = region_worker(app_handle);
        let generation = regions.generation.fetch_add(1, Ordering::SeqCst) + 1;
        regions.sender.send(RegionJob { generation, zoom });
    }
    {
        let mut memory = worker.memory.lock().unwrap_or_else(|e| e.into_inner());
        memory
            .bases
            .retain(|path, _| cells.iter().any(|cell| &cell.path == path));
        memory
            .facts
            .retain(|path, _| cells.iter().any(|cell| &cell.path == path));
    }
    worker.sender.send(Job {
        generation,
        primary,
        cells,
    });
    true
}

/// CPU memory of the prepared bases (and parametric mask sources) kept so
/// that a recipe change develops a cell again without decoding.
fn base_bytes(worker: &Worker) -> usize {
    let memory = worker.memory.lock().unwrap_or_else(|e| e.into_inner());
    memory
        .bases
        .values()
        .map(|base| {
            base.base.image.as_bytes().len()
                + base
                    .mask_source
                    .as_ref()
                    .map_or(0, |source| source.image.as_bytes().len())
        })
        .sum()
}

/// The recipe a photograph is developed with: the stored one (sidecar or
/// session), else the defaults with the camera's lens data, as Develop
/// opens it.
fn recipe_of(path: &str) -> serde_json::Value {
    let (source_path, sidecar_path) = parse_virtual_path(path);
    let mut recipe = crate::exif_processing::load_sidecar(&sidecar_path).adjustments;
    if !recipe.is_object() {
        recipe = serde_json::json!({});
    }
    crate::embedded_lens::apply_default(&mut recipe, &source_path);
    recipe
}

/// The size a photograph is shown at: its crop, else the whole frame turned
/// by its orientation (as `activate_photo` places it).
fn shown_size(recipe: &serde_json::Value, facts: Facts) -> (f32, f32) {
    let crop: Option<image_processing::Crop> = serde_json::from_value(recipe["crop"].clone()).ok();
    let steps = recipe["orientationSteps"].as_u64().unwrap_or(0);
    match crop {
        Some(c) if c.width > 0.0 && c.height > 0.0 => (c.width as f32, c.height as f32),
        _ if steps % 2 == 1 => (facts.full_height as f32, facts.full_width as f32),
        _ => (facts.full_width as f32, facts.full_height as f32),
    }
}

/// The long edge a cell's frame is developed at: the photograph's size
/// contained in the cell's `area`, rounded up to [`SIZE_STEP`].
fn cell_dim(area: [f32; 4], shown: (f32, f32)) -> u32 {
    let fit = gpu_processing::multi_view::fit_rect(area, shown.0, shown.1);
    let long = fit[2].max(fit[3]).max(1.0);
    ((long / SIZE_STEP).ceil() * SIZE_STEP) as u32
}

fn render_key(recipe: &serde_json::Value, dim: u32, context: &GpuContext) -> u64 {
    let mut hash = DefaultHasher::new();
    recipe.to_string().hash(&mut hash);
    dim.hash(&mut hash);
    context.display_space.shader_code().hash(&mut hash);
    hash.finish() | 1
}

/// A photograph's source for its cell: the preview proxy kept in memory, a
/// decode still cached from Develop, the proxy on disk, else a fast decode
/// (as thumbnails). Returns the image, its pixels per full-resolution pixel
/// and its facts.
fn load_source(
    app_handle: &tauri::AppHandle,
    path: &str,
) -> Result<(Arc<DynamicImage>, f32, Facts, &'static str), String> {
    let state = app_handle.state::<AppState>();
    let (source_path, _) = parse_virtual_path(path);
    let source_str = source_path.to_string_lossy().to_string();
    let is_raw = is_raw_file(&source_path);
    if let Some(proxy) = state
        .proxy_memory
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(path)
    {
        let scale = proxy.image.width() as f32 / proxy.full_width.max(1) as f32;
        let facts = Facts {
            full_width: proxy.full_width,
            full_height: proxy.full_height,
            is_raw: proxy.is_raw,
        };
        return Ok((proxy.image, scale, facts, "proxy-memory"));
    }
    if let Some((image, _)) = state
        .decoded_image_cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&source_str)
    {
        let (full_width, full_height) = image.dimensions();
        let facts = Facts {
            full_width,
            full_height,
            is_raw,
        };
        return Ok((image, 1.0, facts, "decoded"));
    }
    let settings = load_settings(app_handle.clone()).unwrap_or_default();
    if is_raw && let Some(proxy) = crate::preview_proxy::read(&source_path, &settings) {
        let scale = proxy.image.width() as f32 / proxy.full_width.max(1) as f32;
        let facts = Facts {
            full_width: proxy.full_width,
            full_height: proxy.full_height,
            is_raw,
        };
        return Ok((Arc::new(proxy.image), scale, facts, "proxy-disk"));
    }
    decode(&source_path, &source_str, is_raw, &settings)
}

fn decode(
    source_path: &Path,
    source_str: &str,
    is_raw: bool,
    settings: &crate::app_settings::AppSettings,
) -> Result<(Arc<DynamicImage>, f32, Facts, &'static str), String> {
    let bytes = read_file_mapped(source_path).map_err(|e| e.to_string())?;
    let image =
        crate::image_loader::load_base_image_from_bytes(&bytes, source_str, is_raw, settings, None)
            .map_err(|e| e.to_string())?;
    let (width, height) = image.dimensions();
    let scale = if is_raw {
        crate::raw_processing::get_fast_demosaic_scale_factor(&bytes, width, height)
    } else {
        1.0
    };
    let facts = Facts {
        full_width: (width as f32 / scale).round() as u32,
        full_height: (height as f32 / scale).round() as u32,
        is_raw,
    };
    Ok((Arc::new(image), scale, facts, "decode"))
}

/// A cell to develop: its recipe and the size and key it is developed for.
struct Todo {
    path: String,
    recipe: serde_json::Value,
    area: [f32; 4],
    /// Known before loading the source (from an earlier job): the key and
    /// size; otherwise worked out once the source is loaded.
    planned: Option<(u64, u32)>,
}

struct Prepared {
    path: String,
    recipe: serde_json::Value,
    key: u64,
    facts: Facts,
    base: PreviewBase,
    mask_source: Option<ParametricMaskSource>,
    source_kind: &'static str,
    prepare_ms: f64,
}

fn current_key(context: &GpuContext, path: &str) -> Option<u64> {
    context
        .display
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .and_then(|display| display.multi_view().texture_key(path))
}

fn prepare(
    app_handle: &tauri::AppHandle,
    worker: &Worker,
    context: &GpuContext,
    todo: Todo,
) -> Result<Option<Prepared>, String> {
    let start = Instant::now();
    let preparation_key = crate::preview_cache::preparation_key(&todo.recipe);
    let known = worker
        .memory
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .facts
        .get(&todo.path)
        .copied();
    let mut loaded = None;
    let facts = match known {
        Some(facts) => facts,
        None => {
            let source = load_source(app_handle, &todo.path)?;
            let facts = source.2;
            loaded = Some(source);
            worker
                .memory
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .facts
                .insert(todo.path.clone(), facts);
            facts
        }
    };
    let (key, dim) = todo.planned.unwrap_or_else(|| {
        let dim = cell_dim(todo.area, shown_size(&todo.recipe, facts));
        (render_key(&todo.recipe, dim, context), dim)
    });
    if current_key(context, &todo.path) == Some(key) {
        return Ok(None);
    }
    let cached = {
        let memory = worker.memory.lock().unwrap_or_else(|e| e.into_inner());
        memory
            .bases
            .get(&todo.path)
            .filter(|base| base.preparation_key == preparation_key && base.dim == dim)
            .map(|base| (base.base.clone(), base.mask_source.clone()))
    };
    let (base, mask_source, source_kind) = match cached {
        Some((base, mask_source)) => (base, mask_source, "base-cache"),
        None => {
            let (source, source_scale, facts, kind) = match loaded {
                Some(loaded) => loaded,
                None => load_source(app_handle, &todo.path)?,
            };
            let scale =
                reduced_preview_scale(facts.full_width, facts.full_height, &todo.recipe, dim)
                    .unwrap_or(source_scale);
            let base = reduced_preview_base(
                &source,
                source_scale,
                facts.full_width,
                facts.full_height,
                &todo.recipe,
                dim,
                scale,
            )?;
            let masks: Vec<MaskDefinition> = todo
                .recipe
                .get("masks")
                .and_then(|m| serde_json::from_value(m.clone()).ok())
                .unwrap_or_default();
            let mask_source = if masks.iter().any(MaskDefinition::requires_warped_image) {
                let mut mask_source = prepare_parametric_mask_source(
                    &source,
                    facts.is_raw,
                    source_scale,
                    &todo.recipe,
                )?;
                if mask_source.image.width().max(mask_source.image.height()) > dim {
                    let reduced =
                        image_processing::downscale_f32_image(&mask_source.image, dim, dim);
                    mask_source = mask_source.with_resized_image(reduced);
                }
                Some(mask_source)
            } else {
                None
            };
            worker
                .memory
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .bases
                .insert(
                    todo.path.clone(),
                    Base {
                        preparation_key,
                        dim,
                        base: base.clone(),
                        mask_source: mask_source.clone(),
                    },
                );
            (base, mask_source, kind)
        }
    };
    Ok(Some(Prepared {
        path: todo.path,
        recipe: todo.recipe,
        key,
        facts,
        base,
        mask_source,
        source_kind,
        prepare_ms: start.elapsed().as_secs_f64() * 1e3,
    }))
}

/// What a cell's frame is developed from: a base image (geometry, retouch
/// applied) of `path` at `scale` base px per full-resolution px whose top
/// left is `crop_offset` (base px) into the photograph's oriented frame.
struct DevelopInput<'a> {
    path: &'a str,
    recipe: &'a serde_json::Value,
    facts: Facts,
    image: &'a DynamicImage,
    scale: f32,
    crop_offset: (f32, f32),
    mask_source: Option<&'a ParametricMaskSource>,
    key: u64,
}

/// Develops `input` with the display's shader variants (masks, LUT, the
/// base look) into display-encoded RGBA8.
fn develop_rgba(
    app_handle: &tauri::AppHandle,
    context: &GpuContext,
    input: DevelopInput,
) -> Result<image::RgbaImage, String> {
    let state = app_handle.state::<AppState>();
    let settings = load_settings(app_handle.clone()).unwrap_or_default();
    let recipe = input.recipe;
    let (width, height) = input.image.dimensions();
    let masks: Vec<MaskDefinition> = recipe
        .get("masks")
        .and_then(|m| serde_json::from_value(m.clone()).ok())
        .unwrap_or_default();
    let geometry = PhotoGeometry::from_adjustments(recipe).with_photo_size(
        input.facts.full_width as f64,
        input.facts.full_height as f64,
    );
    let mask_bitmaps: Vec<Arc<image::GrayImage>> = masks
        .iter()
        .filter_map(|def| {
            generate_mask_bitmap(
                def,
                width,
                height,
                input.scale,
                input.crop_offset,
                &geometry,
                input.mask_source,
            )
            .map(Arc::new)
        })
        .collect();
    let is_raw = input.facts.is_raw;
    let tm_override = image_processing::resolve_tonemapper_override(&settings, is_raw);
    let mut adjustments = get_all_adjustments_from_json(recipe, is_raw, tm_override);
    crate::baseline_exposure::apply_base_look(&mut adjustments.global, input.path);
    adjustments.global.detail_scale = input.scale;
    adjustments.global.show_clipping = 0;
    // Encoded for the window, as the live frame beside it.
    let space = context.display_space;
    adjustments.global.output_space[0] = space.shader_code();
    adjustments.global.output_matrix =
        image_processing::gpu_mat3_from_rows(space.from_linear_srgb_f32());
    let lut = recipe["lutPath"]
        .as_str()
        .and_then(|p| crate::lut_processing::get_or_load_lut(&state, p).ok());
    let developed = gpu_processing::process_and_get_dynamic_image_inner(
        context,
        &state.gpu_processor,
        &state.gpu_image_cache,
        RenderSource::Image {
            image: input.image,
            transform_hash: input.key,
        },
        RenderRequest {
            adjustments,
            mask_bitmaps: &mask_bitmaps,
            lut,
            roi: None,
        },
        "multi_view",
        RenderOutputPrecision::EightBit,
        false,
        None,
        None,
        ReadbackShader::Display,
    )?;
    Ok(developed.to_rgba8())
}

/// Keeps a developed frame of `path` on the display and shows it.
fn show_frame(
    context: &GpuContext,
    path: &str,
    key: u64,
    extent: CellExtent,
    layer: CellLayer,
    rgba: &image::RgbaImage,
) {
    let mut display = context.display.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(display) = display.as_mut() {
        display.upload_cell(
            &context.device,
            &context.queue,
            path,
            key,
            extent,
            layer,
            rgba.width(),
            rgba.height(),
            rgba.as_raw(),
        );
        display.render(&context.device, &context.queue);
    }
}

/// Develops a prepared cell's whole photograph and keeps it on the display.
fn develop(
    app_handle: &tauri::AppHandle,
    context: &GpuContext,
    prepared: Prepared,
) -> Result<f64, String> {
    let start = Instant::now();
    let scale = prepared.base.scale;
    let rgba = develop_rgba(
        app_handle,
        context,
        DevelopInput {
            path: &prepared.path,
            recipe: &prepared.recipe,
            facts: prepared.facts,
            image: &prepared.base.image,
            scale,
            crop_offset: (
                prepared.base.unscaled_crop_offset.0 * scale,
                prepared.base.unscaled_crop_offset.1 * scale,
            ),
            mask_source: prepared.mask_source.as_ref(),
            key: prepared.key,
        },
    )?;
    let (w, h) = shown_size(&prepared.recipe, prepared.facts);
    let extent = CellExtent {
        shown: [w, h],
        region: None,
    };
    show_frame(
        context,
        &prepared.path,
        prepared.key,
        extent,
        CellLayer::Whole,
        &rgba,
    );
    Ok(start.elapsed().as_secs_f64() * 1e3)
}

fn run(app_handle: &tauri::AppHandle, worker: &Worker, job: Job) {
    let start = Instant::now();
    let state = app_handle.state::<AppState>();
    let Some(context) = context_of(&state) else {
        return;
    };
    let current = || worker.generation.load(Ordering::SeqCst) == job.generation;
    if !current() {
        return;
    }
    // Plan: the recipe of every secondary, and what is already on screen.
    let mut todos = Vec::new();
    for cell in &job.cells {
        if job.primary.as_deref() == Some(cell.path.as_str()) {
            continue;
        }
        let recipe = recipe_of(&cell.path);
        let known = worker
            .memory
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .facts
            .get(&cell.path)
            .copied();
        let planned = known.map(|facts| {
            let dim = cell_dim(cell.area, shown_size(&recipe, facts));
            (render_key(&recipe, dim, &context), dim)
        });
        if planned.is_some_and(|(key, _)| current_key(&context, &cell.path) == Some(key)) {
            continue;
        }
        todos.push(Todo {
            path: cell.path.clone(),
            recipe,
            area: cell.area,
            planned,
        });
    }
    if todos.is_empty() {
        return;
    }
    let count = todos.len();
    let queue = Mutex::new(todos.into_iter());
    let (sender, receiver) = std::sync::mpsc::channel();
    let mut developed = 0;
    std::thread::scope(|scope| {
        for _ in 0..PREPARE_THREADS.min(count) {
            let sender = sender.clone();
            let (queue, context) = (&queue, &context);
            scope.spawn(move || {
                loop {
                    if worker.generation.load(Ordering::SeqCst) != job.generation {
                        return;
                    }
                    let Some(todo) = queue.lock().unwrap_or_else(|e| e.into_inner()).next() else {
                        return;
                    };
                    let path = todo.path.clone();
                    let result = prepare(app_handle, worker, context, todo);
                    if sender.send((path, result)).is_err() {
                        return;
                    }
                }
            });
        }
        drop(sender);
        for (path, result) in receiver {
            if !current() {
                continue;
            }
            match result {
                Ok(Some(prepared)) => {
                    let (kind, prepare_ms) = (prepared.source_kind, prepared.prepare_ms);
                    match develop(app_handle, &context, prepared) {
                        Ok(gpu_ms) => {
                            developed += 1;
                            log::info!(
                                "[multi_view] {path}: {kind} prepared in {prepare_ms:.1} ms, developed in {gpu_ms:.1} ms"
                            );
                        }
                        Err(e) => log::warn!("[multi_view] {path} not developed: {e}"),
                    }
                }
                Ok(None) => {}
                Err(e) => log::warn!("[multi_view] {path} not prepared: {e}"),
            }
        }
    });
    let bytes = context
        .display
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .map_or(0, |display| display.multi_view().texture_bytes());
    log::info!(
        "[multi_view] {developed} of {count} cells developed in {:.1} ms; cell textures {:.1} MB (GPU), prepared bases {:.1} MB (CPU)",
        start.elapsed().as_secs_f64() * 1e3,
        bytes as f64 / 1e6,
        base_bytes(worker) as f64 / 1e6
    );
}

/// Zoomed in, each cell's region is developed with this much of the visible
/// size around it on each side, so that panning stays sharp a while.
const REGION_MARGIN: f32 = 0.25;

/// The base a region is cut from reaches this much of the visible size
/// around it on each side: a pan further than REGION_MARGIN develops the new
/// region from memory (milliseconds) instead of decoding the photograph
/// again. About 21 MB per cell for a 544 × 362 px cell at 1:1.
const BASE_MARGIN: f32 = 1.0;

/// How long a zoom must stay before its regions are developed.
const REGION_SETTLE: std::time::Duration = std::time::Duration::from_millis(100);

/// Full-resolution sources decoded at the same time for zoomed regions: one.
/// A decode and its transformed copy take several hundred megabytes; two at
/// a time raised the process peak by about 2 GB (11 cells, 24–45 MP).
const REGION_THREADS: usize = 1;

#[derive(Clone, Copy, Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ZoomPayload {
    /// Device px per full-resolution px of the shown photograph.
    pub scale: f32,
    /// The photograph point (0–1) at each cell's centre.
    pub center: [f32; 2],
}

struct RegionJob {
    generation: usize,
    zoom: CellZoom,
}

struct RegionWorker {
    generation: AtomicUsize,
    sender: LatestJobSender<RegionJob>,
    /// Region bases of the shown cells at the current magnification.
    bases: Mutex<HashMap<String, RegionBase>>,
}

static REGIONS: OnceLock<Arc<RegionWorker>> = OnceLock::new();

fn region_worker(app_handle: &tauri::AppHandle) -> Arc<RegionWorker> {
    REGIONS
        .get_or_init(|| {
            let app = app_handle.clone();
            Arc::new(RegionWorker {
                generation: AtomicUsize::new(0),
                bases: Mutex::new(HashMap::new()),
                sender: gpu_processing::start_latest_job_worker(move |job: RegionJob| {
                    let worker = REGIONS.get().expect("multi view region worker").clone();
                    run_regions(&app, &worker, job);
                }),
            })
        })
        .clone()
}

/// `scale` to 1/1000 of itself: the webview's zoom arithmetic varies in the
/// last digits between frames of one magnification.
fn quantized_scale(scale: f32) -> f32 {
    let step = 10f32.powi(scale.log10().floor() as i32 - 3);
    (scale / step).round() * step
}

/// Stops region work and releases the kept region bases (back to fit, or
/// the multi view ended).
fn forget_regions(app_handle: &tauri::AppHandle) {
    let worker = region_worker(app_handle);
    worker.generation.fetch_add(1, Ordering::SeqCst);
    worker
        .bases
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
}

/// Zooms every cell of the multi view to the primary's zoom (Lightroom's
/// zoom lock; None: fit). The cells' photographs are enlarged at once; the
/// visible regions are then developed at that magnification.
#[tauri::command]
pub(crate) async fn set_multi_view_zoom(
    zoom: Option<ZoomPayload>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app_handle.state::<AppState>();
        let Some(context) = context_of(&state) else {
            return;
        };
        let zoom = zoom
            .filter(|z| z.scale.is_finite() && z.scale > 0.0)
            .map(|z| CellZoom {
                // A pan must not count as a new magnification (regions and
                // their bases are kept per magnification).
                scale: quantized_scale(z.scale),
                center: [z.center[0].clamp(0.0, 1.0), z.center[1].clamp(0.0, 1.0)],
            });
        {
            let mut display = context.display.lock().unwrap_or_else(|e| e.into_inner());
            let Some(display) = display.as_mut() else {
                return;
            };
            if !display.multi_view().is_active() || display.multi_view().zoom() == zoom {
                return;
            }
            display.set_multi_view_zoom(zoom);
            display.render(&context.device, &context.queue);
        }
        let Some(zoom) = zoom else {
            forget_regions(&app_handle);
            return;
        };
        let worker = region_worker(&app_handle);
        // Bases at another magnification are of no use any more.
        worker
            .bases
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|_, base| base.zoom_scale == zoom.scale);
        let generation = worker.generation.fetch_add(1, Ordering::SeqCst) + 1;
        worker.sender.send(RegionJob { generation, zoom });
    })
    .await
    .map_err(|e| format!("Multi view zoom task failed: {e}"))
}

/// A photograph at full resolution for zoomed regions: a decode still cached
/// from Develop, its 1:1 preview, else a full decode. Returns the image, its
/// pixels per full-resolution pixel and its facts.
fn load_full_source(
    app_handle: &tauri::AppHandle,
    path: &str,
) -> Result<(Arc<DynamicImage>, f32, Facts, &'static str), String> {
    let state = app_handle.state::<AppState>();
    let (source_path, _) = parse_virtual_path(path);
    let source_str = source_path.to_string_lossy().to_string();
    let is_raw = is_raw_file(&source_path);
    if let Some((image, _)) = state
        .decoded_image_cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&source_str)
    {
        let (full_width, full_height) = image.dimensions();
        let facts = Facts {
            full_width,
            full_height,
            is_raw,
        };
        return Ok((image, 1.0, facts, "decoded"));
    }
    let settings = load_settings(app_handle.clone()).unwrap_or_default();
    if is_raw
        && let Some(proxy) = crate::preview_proxy::read_kind(
            &source_path,
            crate::preview_proxy::ProxyKind::Full,
            &settings,
        )
    {
        let scale = proxy.image.width() as f32 / proxy.full_width.max(1) as f32;
        let facts = Facts {
            full_width: proxy.full_width,
            full_height: proxy.full_height,
            is_raw,
        };
        return Ok((Arc::new(proxy.image), scale, facts, "proxy-1:1"));
    }
    let bytes = read_file_mapped(&source_path).map_err(|e| e.to_string())?;
    let image = crate::image_loader::load_base_image_from_bytes(
        &bytes,
        &source_str,
        false,
        &settings,
        None,
    )
    .map_err(|e| e.to_string())?;
    let (full_width, full_height) = image.dimensions();
    let facts = Facts {
        full_width,
        full_height,
        is_raw,
    };
    Ok((Arc::new(image), 1.0, facts, "full-decode"))
}

fn region_key(recipe: &serde_json::Value, zoom: CellZoom, context: &GpuContext) -> u64 {
    let mut hash = DefaultHasher::new();
    recipe.to_string().hash(&mut hash);
    zoom.scale.to_bits().hash(&mut hash);
    context.display_space.shader_code().hash(&mut hash);
    hash.finish() | 1
}

/// A developed region ready for the display.
struct Region {
    path: String,
    key: u64,
    extent: CellExtent,
    rgba: image::RgbaImage,
    source_kind: &'static str,
    ms: f64,
}

/// A zoomed region's base (geometry and retouching applied), kept so that a
/// recipe change (Edit all selected while zoomed) develops it again without
/// decoding the full-resolution source.
#[derive(Clone)]
struct RegionBase {
    preparation_key: u64,
    zoom_scale: f32,
    /// The region in full-resolution px of the shown photograph.
    region: [f32; 4],
    image: Arc<DynamicImage>,
    /// Base px per full-resolution px.
    scale: f32,
    /// The region's top left in base px of the photograph's oriented frame.
    crop_offset: (f32, f32),
    facts: Facts,
    mask_source: Option<ParametricMaskSource>,
}

/// Prepares the base of the region of `path` its cell `area` shows at
/// `zoom` (with a margin) from the full-resolution source: geometry and
/// retouching at the zoom's magnification (at most the source's), cut to
/// the region.
fn prepare_region(
    app_handle: &tauri::AppHandle,
    path: &str,
    area: [f32; 4],
    zoom: CellZoom,
    recipe: &serde_json::Value,
) -> Result<(RegionBase, &'static str), String> {
    let (source, source_scale, facts, source_kind) = load_full_source(app_handle, path)?;
    let (shown_w, shown_h) = shown_size(recipe, facts);
    let scale = zoom.scale.min(source_scale).min(1.0);
    let dim = ((facts.full_width.max(facts.full_height) as f32 * scale).ceil() as u32).max(1) + 2;
    let base = reduced_preview_base(
        &source,
        source_scale,
        facts.full_width,
        facts.full_height,
        recipe,
        dim,
        scale,
    )?;
    let wanted =
        gpu_processing::multi_view::zoomed_region(area, [shown_w, shown_h], zoom, BASE_MARGIN);
    let (base_w, base_h) = base.image.dimensions();
    let s = base.scale;
    let x = ((wanted[0] * s).floor() as u32).min(base_w.saturating_sub(1));
    let y = ((wanted[1] * s).floor() as u32).min(base_h.saturating_sub(1));
    let w = (((wanted[0] + wanted[2]) * s).ceil() as u32).clamp(x + 1, base_w) - x;
    let h = (((wanted[1] + wanted[3]) * s).ceil() as u32).clamp(y + 1, base_h) - y;
    let image = Arc::new(base.image.crop_imm(x, y, w, h));
    drop(base.image);
    let masks: Vec<MaskDefinition> = recipe
        .get("masks")
        .and_then(|m| serde_json::from_value(m.clone()).ok())
        .unwrap_or_default();
    let mask_source = masks
        .iter()
        .any(MaskDefinition::requires_warped_image)
        .then(|| prepare_parametric_mask_source(&source, facts.is_raw, source_scale, recipe))
        .transpose()?;
    Ok((
        RegionBase {
            preparation_key: crate::preview_cache::preparation_key(recipe),
            zoom_scale: zoom.scale,
            region: [x as f32 / s, y as f32 / s, w as f32 / s, h as f32 / s],
            image,
            scale: s,
            crop_offset: (
                base.unscaled_crop_offset.0 * s + x as f32,
                base.unscaled_crop_offset.1 * s + y as f32,
            ),
            facts,
            mask_source,
        },
        source_kind,
    ))
}

/// Develops the region of `path` its cell shows at `zoom`, from a kept base
/// when one still covers it at this magnification and geometry, else from
/// a new one; then the region alone through the display's shader variants.
#[allow(clippy::too_many_arguments)]
fn develop_region(
    app_handle: &tauri::AppHandle,
    context: &GpuContext,
    worker: &RegionWorker,
    path: &str,
    area: [f32; 4],
    zoom: CellZoom,
    recipe: serde_json::Value,
    key: u64,
) -> Result<Region, String> {
    let start = Instant::now();
    let preparation_key = crate::preview_cache::preparation_key(&recipe);
    // The texture covers the visible part and REGION_MARGIN around it; the
    // kept base reaches further (BASE_MARGIN), so a pan is cut from it.
    let wanted_in = |facts: Facts| {
        let (w, h) = shown_size(&recipe, facts);
        gpu_processing::multi_view::zoomed_region(area, [w, h], zoom, REGION_MARGIN)
    };
    let kept = worker
        .bases
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(path)
        .filter(|base| {
            base.preparation_key == preparation_key
                && base.zoom_scale == zoom.scale
                && contains(base.region, wanted_in(base.facts))
        })
        .cloned();
    let (base, source_kind) = match kept {
        Some(base) => (base, "region-cache"),
        None => {
            let (base, kind) = prepare_region(app_handle, path, area, zoom, &recipe)?;
            worker
                .bases
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(path.to_string(), base.clone());
            (base, kind)
        }
    };
    let wanted = wanted_in(base.facts);
    let s = base.scale;
    let (base_w, base_h) = base.image.dimensions();
    let x = (((wanted[0] - base.region[0]) * s).floor().max(0.0) as u32).min(base_w - 1);
    let y = (((wanted[1] - base.region[1]) * s).floor().max(0.0) as u32).min(base_h - 1);
    let w = ((((wanted[0] + wanted[2] - base.region[0]) * s)
        .ceil()
        .max(0.0) as u32)
        .clamp(x + 1, base_w))
        - x;
    let h = ((((wanted[1] + wanted[3] - base.region[1]) * s)
        .ceil()
        .max(0.0) as u32)
        .clamp(y + 1, base_h))
        - y;
    let image = base.image.crop_imm(x, y, w, h);
    let rgba = develop_rgba(
        app_handle,
        context,
        DevelopInput {
            path,
            recipe: &recipe,
            facts: base.facts,
            image: &image,
            scale: s,
            crop_offset: (base.crop_offset.0 + x as f32, base.crop_offset.1 + y as f32),
            mask_source: base.mask_source.as_ref(),
            key,
        },
    )?;
    let (shown_w, shown_h) = shown_size(&recipe, base.facts);
    Ok(Region {
        path: path.to_string(),
        key,
        extent: CellExtent {
            shown: [shown_w, shown_h],
            region: Some([
                base.region[0] + x as f32 / s,
                base.region[1] + y as f32 / s,
                w as f32 / s,
                h as f32 / s,
            ]),
        },
        rgba,
        source_kind,
        ms: start.elapsed().as_secs_f64() * 1e3,
    })
}

/// Whether `outer` holds all of `inner` (x, y, w, h), within half a pixel.
fn contains(outer: [f32; 4], inner: [f32; 4]) -> bool {
    let eps = 0.5;
    outer[0] <= inner[0] + eps
        && outer[1] <= inner[1] + eps
        && outer[0] + outer[2] + eps >= inner[0] + inner[2]
        && outer[1] + outer[3] + eps >= inner[1] + inner[3]
}

fn run_regions(app_handle: &tauri::AppHandle, worker: &RegionWorker, job: RegionJob) {
    let start = Instant::now();
    let current = || worker.generation.load(Ordering::SeqCst) == job.generation;
    // A zoom animates over a few frames and a pan sends one zoom per frame:
    // only a zoom that stays for a moment is worth a full-resolution decode.
    std::thread::sleep(REGION_SETTLE);
    if !current() {
        return;
    }
    let state = app_handle.state::<AppState>();
    let Some(context) = context_of(&state) else {
        return;
    };
    let facts = WORKER.get().map(|w| {
        w.memory
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .facts
            .clone()
    });
    // The secondaries whose shown region is not developed at this zoom yet.
    let todo: Vec<(String, [f32; 4], serde_json::Value, u64)> = {
        let display = context.display.lock().unwrap_or_else(|e| e.into_inner());
        let Some(display) = display.as_ref() else {
            return;
        };
        let multi = display.multi_view();
        multi
            .cells()
            .iter()
            .filter(|cell| multi.primary() != Some(cell.path.as_str()))
            .filter_map(|cell| {
                let recipe = recipe_of(&cell.path);
                let key = region_key(&recipe, job.zoom, &context);
                let shown = facts
                    .as_ref()
                    .and_then(|f| f.get(&cell.path))
                    .map(|f| shown_size(&recipe, *f));
                let covered = multi.region_of(&cell.path).is_some_and(|(k, extent)| {
                    k == key
                        && shown.is_some_and(|(w, h)| {
                            extent.region.is_some_and(|region| {
                                gpu_processing::multi_view::region_covers(
                                    region,
                                    cell.area,
                                    [w, h],
                                    job.zoom,
                                )
                            })
                        })
                });
                (!covered).then(|| (cell.path.clone(), cell.area, recipe, key))
            })
            .collect()
    };
    if todo.is_empty() {
        return;
    }
    let count = todo.len();
    let queue = Mutex::new(todo.into_iter());
    let (sender, receiver) = std::sync::mpsc::channel();
    let mut shown = 0;
    std::thread::scope(|scope| {
        for _ in 0..REGION_THREADS.min(count) {
            let sender = sender.clone();
            let (queue, context) = (&queue, &context);
            scope.spawn(move || {
                loop {
                    if worker.generation.load(Ordering::SeqCst) != job.generation {
                        return;
                    }
                    let Some((path, area, recipe, key)) =
                        queue.lock().unwrap_or_else(|e| e.into_inner()).next()
                    else {
                        return;
                    };
                    let result = develop_region(
                        app_handle, context, worker, &path, area, job.zoom, recipe, key,
                    );
                    if sender.send((path, result)).is_err() {
                        return;
                    }
                }
            });
        }
        drop(sender);
        for (path, result) in receiver {
            if !current() {
                continue;
            }
            match result {
                Ok(region) => {
                    show_frame(
                        &context,
                        &region.path,
                        region.key,
                        region.extent,
                        CellLayer::Region,
                        &region.rgba,
                    );
                    shown += 1;
                    log::info!(
                        "[multi_view] {path}: region {}x{} at {:.4}x from {} in {:.1} ms",
                        region.rgba.width(),
                        region.rgba.height(),
                        job.zoom.scale,
                        region.source_kind,
                        region.ms
                    );
                }
                Err(e) => log::warn!("[multi_view] {path}: region not developed: {e}"),
            }
        }
    });
    log::info!(
        "[multi_view] {shown} of {count} zoomed regions developed in {:.1} ms",
        start.elapsed().as_secs_f64() * 1e3
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_magnification_keeps_its_value_while_panning() {
        assert_eq!(quantized_scale(1.000_000_2), quantized_scale(0.999_999_8));
        assert_eq!(quantized_scale(1.0), 1.0);
        assert_eq!(quantized_scale(0.123_456), 0.1235);
        assert_ne!(quantized_scale(1.0), quantized_scale(1.01));
        assert!((quantized_scale(2.345_678) - 2.346).abs() < 1e-6);
    }

    #[test]
    fn regions_must_hold_what_the_cell_shows() {
        assert!(contains([0.0, 0.0, 100.0, 100.0], [10.0, 10.0, 50.0, 50.0]));
        assert!(contains(
            [0.0, 0.0, 100.0, 100.0],
            [-0.4, 0.0, 100.3, 100.0]
        ));
        assert!(!contains(
            [0.0, 0.0, 100.0, 100.0],
            [60.0, 10.0, 50.0, 50.0]
        ));
    }

    #[test]
    fn cells_are_developed_at_their_shown_size() {
        let facts = Facts {
            full_width: 6000,
            full_height: 4000,
            is_raw: true,
        };
        let plain = serde_json::json!({});
        // 3:2 in a 400 × 600 cell: 400 × 267 → long edge 400.
        assert_eq!(
            cell_dim([0.0, 0.0, 400.0, 600.0], shown_size(&plain, facts)),
            400
        );
        // Turned a quarter: 2:3 → 400 × 600, rounded up to 608.
        let turned = serde_json::json!({"orientationSteps": 1});
        assert_eq!(
            cell_dim([0.0, 0.0, 400.0, 600.0], shown_size(&turned, facts)),
            608
        );
        // A square crop in a wide cell: its height, rounded up to 16.
        let square = serde_json::json!({"crop": {"x": 0, "y": 0, "width": 3000, "height": 3000}});
        assert_eq!(
            cell_dim([0.0, 0.0, 900.0, 301.0], shown_size(&square, facts)),
            304
        );
    }
}
