//! Develop's multi view (several selected photographs side by side, as
//! Capture One's Multi view and Lightroom's Survey show them): the webview
//! lays out the cells and sends their screen rectangles; the native display
//! draws every cell (crates/gpu multi_view.rs). The primary photograph is the
//! normal live preview. Each other cell is developed here once at its cell's
//! size, from its preview proxy (or a fast decode) and its stored recipe, with
//! the display's shader variants, and kept on the GPU until its recipe or
//! size changes. Zoomed in, each cell's visible region is developed at the
//! zoom's magnification from the photograph at full resolution; until then
//! it shows the region from the reduced source its cell was developed from,
//! marked interim (never taken for the developed region). Develop pixels
//! never cross IPC.

use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use image::{DynamicImage, GenericImageView};
use tauri::{Emitter, Manager};

use crate::app_settings::load_settings;
use crate::app_state::AppState;
use crate::editor_preview::{reduced_preview_base, reduced_preview_scale};
use crate::file_management::{parse_virtual_path, read_file_mapped};
use crate::formats::is_raw_file;
use crate::gpu_processing::{
    self, CellExtent, CellLayer, CellZoom, GpuContext, LatestJobSender, MultiViewCell,
    ReadbackShader, RenderOutputPrecision, RenderRequest, RenderSource,
};
use crate::image_processing::{self, RegionMeasures, get_all_adjustments_from_json};
use crate::mask_generation::{
    MaskBitmap, MaskDefinition, ParametricMaskSource, PhotoGeometry, generate_layer_mask,
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
    /// The measures of the decode the source is (None: a preview or a fast
    /// development), `image_loader::DecodeMeasures`.
    measures: Option<crate::image_loader::DecodeMeasures>,
}

struct Base {
    preparation_key: u64,
    dim: u32,
    base: PreviewBase,
    mask_source: Option<ParametricMaskSource>,
    /// The measures of the source it was made from.
    measures: Option<crate::image_loader::DecodeMeasures>,
}

#[derive(Default)]
struct Memory {
    facts: HashMap<String, Facts>,
    bases: HashMap<String, Base>,
    /// The reduced sources the cells were developed from (a fast decode, a
    /// Standard preview), kept within [`interim_budget`]: a zoom shows its
    /// regions from them at once, until they are developed from the
    /// photographs at full resolution.
    interims: HashMap<String, Interim>,
}

/// A cell's reduced source: the image, its pixels per full-resolution
/// pixel and the photograph's facts.
#[derive(Clone)]
struct Interim {
    image: Arc<DynamicImage>,
    scale: f32,
    facts: Facts,
}

/// Bytes of reduced sources kept for interim regions: a sixteenth of the
/// physical memory (1.5 GB of 24 GB: the half-size fast decodes of eleven
/// 20–61 MP RAWs take about 1.1 GB), none while the system is short of
/// memory.
fn interim_budget() -> usize {
    if crate::memory_pressure::level_now().is_some() {
        return 0;
    }
    static TOTAL: OnceLock<u64> = OnceLock::new();
    let total = *TOTAL.get_or_init(|| {
        let mut system = sysinfo::System::new();
        system.refresh_memory();
        system.total_memory()
    });
    (total / 16) as usize
}

/// Keeps `interim` for `path` when it fits [`interim_budget`] beside the
/// others (one already kept for `path` is replaced).
fn keep_interim(
    interims: &mut HashMap<String, Interim>,
    path: &str,
    interim: Interim,
    budget: usize,
) {
    interims.remove(path);
    let used: usize = interims.values().map(|i| i.image.as_bytes().len()).sum();
    if used + interim.image.as_bytes().len() <= budget {
        interims.insert(path.to_string(), interim);
    }
}

struct Worker {
    generation: AtomicUsize,
    sender: LatestJobSender<Job>,
    memory: Mutex<Memory>,
}

/// Edit all selected while the primary is edited (a slider drag): the
/// changes not yet written to the other photographs. The preview worker
/// develops the other cells with them between the primary's frames
/// ([`live_step`]), one cell at a time, so the primary's next frame waits at
/// most for one small cell.
#[derive(Default)]
struct Live {
    /// Copyable recipe keys with the primary's values (absolute, as paste).
    patch: Option<serde_json::Map<String, serde_json::Value>>,
    /// Round-robin position in the display's cells.
    cursor: usize,
    /// Cells checked since the patch last changed and found current (or not
    /// developable live): when all are, nothing is pending.
    settled: usize,
}

static LIVE: Mutex<Live> = Mutex::new(Live {
    patch: None,
    cursor: 0,
    settled: 0,
});
static LIVE_PENDING: AtomicBool = AtomicBool::new(false);

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
    let zoomed = {
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
        display.multi_view().is_zoomed()
    };
    if cells.is_empty() {
        *worker.memory.lock().unwrap_or_else(|e| e.into_inner()) = Memory::default();
        forget_regions(app_handle);
        crate::multi_view_prefetch::plan(app_handle, Vec::new(), None);
        return true;
    }
    // Zoomed in: regions of new cells, or of recipes that changed.
    if zoomed {
        let regions = region_worker(app_handle);
        let generation = regions.generation.fetch_add(1, Ordering::SeqCst) + 1;
        regions.sender.send(RegionJob { generation });
    }
    {
        let mut memory = worker.memory.lock().unwrap_or_else(|e| e.into_inner());
        memory
            .bases
            .retain(|path, _| cells.iter().any(|cell| &cell.path == path));
        memory
            .facts
            .retain(|path, _| cells.iter().any(|cell| &cell.path == path));
        memory
            .interims
            .retain(|path, _| cells.iter().any(|cell| &cell.path == path));
        if interim_budget() == 0 {
            memory.interims.clear();
        }
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
    let _ = rawtonic_export::exif_processing::prepare_recipe_lens(&mut recipe, &source_path);
    recipe
}

/// Developed browser cells for Linux (and the split's before), using the
/// same geometry, retouch, local masks and LUT pipeline as native cells.
/// This never activates a photo or changes the primary preview's recipe.
/// Header: shown width/height as little-endian u32, followed by JPEG.
#[tauri::command]
pub(crate) async fn render_browser_view(
    path: String,
    js_adjustments: Option<serde_json::Value>,
    target_resolution: u32,
    app_handle: tauri::AppHandle,
) -> Result<tauri::ipc::Response, String> {
    static GATE: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);
    let permit = GATE.acquire().await.map_err(|e| e.to_string())?;
    let result = tauri::async_runtime::spawn_blocking(move || {
        let state = app_handle.state::<AppState>();
        let context = gpu_processing::get_or_init_gpu_context(&state, &app_handle)?;
        let worker = worker(&app_handle);
        // At most eight prepared cells; browsing a large session must not
        // retain every decoded preview. Native display keeps its own cells.
        {
            let mut memory = worker.memory.lock().unwrap_or_else(|e| e.into_inner());
            if memory.bases.len() >= 8 && !memory.bases.contains_key(&path) {
                memory.bases.clear();
                memory.facts.clear();
            }
        }
        let recipe = js_adjustments.unwrap_or_else(|| recipe_of(&path));
        let dim = target_resolution.clamp(64, 4096);
        let key = render_key(&recipe, dim, &context);
        let prepared = prepare(
            &app_handle,
            &worker,
            &context,
            Todo {
                path,
                recipe,
                area: [0.0, 0.0, dim as f32, dim as f32],
                planned: Some((key, dim)),
            },
        )?
        .ok_or("Browser view was already presented natively")?;
        let scale = prepared.base.scale;
        let rgba = develop_rgba(
            &app_handle,
            &context,
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
        let (width, height) = shown_size(&prepared.recipe, prepared.facts);
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&(width.round() as u32).to_le_bytes());
        bytes.extend_from_slice(&(height.round() as u32).to_le_bytes());
        bytes.extend(crate::editor_preview::encode_browser_preview(&rgba, 94)?);
        Ok(tauri::ipc::Response::new(bytes))
    })
    .await
    .map_err(|e| format!("Browser view task failed: {e}"))?;
    drop(permit);
    result
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

/// The decoded-image cache key of the full decode a cell at `path` with
/// `recipe` uses: one for its decode variant (without AI Denoise).
fn decoded_key(path: &str, recipe: &serde_json::Value) -> String {
    let (source_path, _) = parse_virtual_path(path);
    let source = source_path.to_string_lossy();
    let variant = crate::image_loader::decode_variant(recipe, is_raw_file(&source_path));
    crate::image_loader::decoded_cache_key(&source, None, variant)
}

/// The full decode of `path` for `recipe` in memory, with its measures:
/// Develop's decode cache, else the one the multi view's background work
/// holds while it writes its 1:1 preview.
fn decode_in_memory(
    state: &AppState,
    path: &str,
    recipe: &serde_json::Value,
    settings: &crate::app_settings::AppSettings,
) -> Option<(
    Arc<DynamicImage>,
    Option<crate::image_loader::DecodeMeasures>,
)> {
    let key = decoded_key(path, recipe);
    let cached = state
        .decoded_image_cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_measured(&key)
        .map(|(image, _, measures)| (image, measures));
    cached.or_else(|| crate::multi_view_prefetch::held_decode(&key, settings))
}

/// What zoomed regions need from photographs' whole decodes (the automatic
/// lateral CA estimate, the edge-fringe radius: `RegionMeasures`), by the
/// pixels they were measured on ([`MeasuredOn`]), what they depend on in the
/// recipe and its geometry: a region is warped from the few rows of its 1:1
/// preview it shows only with them. Recorded wherever a whole decode is at
/// hand (the 1:1 preview being made, a decode in memory); a region from a
/// 1:1 preview without them records the 1:1 preview's own, which its whole
/// warp measured before.
type MeasuresKey = (u64, u64, u64);
/// With measures of a decode in memory, a weak reference to it: they count
/// only while it lives (it also keeps its address from being reused).
type MeasuresEntry = (RegionMeasures, Option<std::sync::Weak<DynamicImage>>);
static REGION_MEASURES: Mutex<Option<HashMap<MeasuresKey, MeasuresEntry>>> = Mutex::new(None);

/// Measures kept (a few bytes each; a session of many thousands of photos
/// starts over rather than grow without bound).
const REGION_MEASURES_KEPT: usize = 4096;

/// The pixels measures were taken on.
#[derive(Clone, Copy)]
enum MeasuredOn<'a> {
    /// A decode kept in memory (the decode cache's image).
    Decode(&'a Arc<DynamicImage>),
    /// The photograph's 1:1 preview with this header key (what it was made
    /// from: file size, modification time, decoder settings, engine
    /// version), as read: what its regions are cut from and what its whole
    /// warp measured, so a region gets the same whether its measures were
    /// recorded before or are taken now, and a preview rebuilt from other
    /// settings or another file never finds those of the last one.
    Preview(&'a crate::preview_proxy::ProxyKey),
    /// A full decode used once (not kept): measured, not recorded.
    Fresh,
}

fn source_identity(path: &str, recipe: &serde_json::Value, on: MeasuredOn) -> Option<u64> {
    let mut hash = DefaultHasher::new();
    let (source_path, _) = parse_virtual_path(path);
    match on {
        MeasuredOn::Decode(image) => {
            decoded_key(path, recipe).hash(&mut hash);
            (Arc::as_ptr(image) as usize).hash(&mut hash);
            image.dimensions().hash(&mut hash);
        }
        MeasuredOn::Fresh => return None,
        MeasuredOn::Preview(key) => {
            source_path.hash(&mut hash);
            (key.file_size, key.modified_ns, &key.decoder, key.engine).hash(&mut hash);
        }
    }
    Some(hash.finish())
}

fn measures_key(path: &str, recipe: &serde_json::Value, on: MeasuredOn) -> Option<MeasuresKey> {
    Some((
        source_identity(path, recipe, on)?,
        RegionMeasures::inputs_key(recipe)?,
        crate::preview_cache::preparation_key(recipe),
    ))
}

/// The measures recorded for `path` with `recipe` on `on`, if any.
fn known_measures(
    path: &str,
    recipe: &serde_json::Value,
    on: MeasuredOn,
) -> Option<RegionMeasures> {
    let key = measures_key(path, recipe, on)?;
    let all = REGION_MEASURES.lock().unwrap_or_else(|e| e.into_inner());
    let (measures, decode) = all.as_ref()?.get(&key)?;
    let current = match (on, decode) {
        (MeasuredOn::Decode(image), Some(weak)) => {
            weak.strong_count() > 0 && std::sync::Weak::ptr_eq(weak, &Arc::downgrade(image))
        }
        (MeasuredOn::Preview(_), None) => true,
        _ => false,
    };
    current.then_some(*measures)
}

/// Records the measures of the current 1:1 preview of `path` for `recipe`
/// unless known (the multi view's background work, after making or finding
/// the preview), so that the first zoom warps regions from its rows.
pub(crate) fn measure_preview(
    path: &str,
    recipe: &serde_json::Value,
    settings: &crate::app_settings::AppSettings,
) {
    if RegionMeasures::inputs_key(recipe).is_none() {
        return;
    }
    let (source, _) = parse_virtual_path(path);
    // A recipe of another decode variant is not shown from the previews.
    if !crate::preview_proxy::stands_for(&source, recipe) {
        return;
    }
    if crate::preview_proxy::full_key(&source, settings)
        .is_some_and(|key| known_measures(path, recipe, MeasuredOn::Preview(&key)).is_some())
    {
        return;
    }
    let Some(proxy) =
        crate::preview_proxy::read_kind(&source, crate::preview_proxy::ProxyKind::Full, settings)
    else {
        return;
    };
    if proxy.image.width() == proxy.full_width {
        let on = MeasuredOn::Preview(&proxy.key);
        note_measures(path, recipe, &proxy.image, on, true);
    }
}

/// Records the measures of `image`, the full decode of `path` held in
/// memory, for `recipe` unless known (the multi view's background work).
pub(crate) fn measure_decode(path: &str, recipe: &serde_json::Value, image: &Arc<DynamicImage>) {
    note_measures(path, recipe, image, MeasuredOn::Decode(image), true);
}

/// The stored recipe of `path` (for the multi view's background work).
pub(crate) fn stored_recipe(path: &str) -> serde_json::Value {
    recipe_of(path)
}

/// The measures of `image` (the pixels `on` stands for) for `recipe`:
/// recorded ones when `reuse` (or None when the recipe needs none), else
/// measured and recorded.
fn note_measures(
    path: &str,
    recipe: &serde_json::Value,
    image: &DynamicImage,
    on: MeasuredOn,
    reuse: bool,
) -> Option<RegionMeasures> {
    RegionMeasures::inputs_key(recipe)?;
    if reuse && let Some(known) = known_measures(path, recipe, on) {
        return Some(known);
    }
    let measures = RegionMeasures::of(image.as_rgb32f()?, recipe);
    if let Some(key) = measures_key(path, recipe, on) {
        let mut all = REGION_MEASURES.lock().unwrap_or_else(|e| e.into_inner());
        let all = all.get_or_insert_with(HashMap::new);
        if all.len() >= REGION_MEASURES_KEPT {
            all.clear();
        }
        let decode = match on {
            MeasuredOn::Decode(image) => Some(Arc::downgrade(image)),
            _ => None,
        };
        all.insert(key, (measures, decode));
    }
    Some(measures)
}

/// A photograph's source for its cell: the preview proxy kept in memory, a
/// decode still cached from Develop, the proxy on disk, else a fast decode
/// (as thumbnails). Returns the image, its pixels per full-resolution pixel
/// and its facts.
fn load_source(
    app_handle: &tauri::AppHandle,
    path: &str,
    recipe: &serde_json::Value,
) -> Result<(Arc<DynamicImage>, f32, Facts, &'static str), String> {
    let state = app_handle.state::<AppState>();
    let (source_path, _) = parse_virtual_path(path);
    let source_str = source_path.to_string_lossy().to_string();
    let is_raw = is_raw_file(&source_path);
    if let Some((image, _, measures)) = state
        .decoded_image_cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_measured(&decoded_key(path, recipe))
    {
        let (full_width, full_height) = image.dimensions();
        let facts = Facts {
            full_width,
            full_height,
            is_raw,
            measures,
        };
        return Ok((image, 1.0, facts, "decoded"));
    }
    // The previews stand for the master recipe's decode variant
    // (`preview_proxy::stands_for`); a cell of another one decodes.
    let proxy_fits = !is_raw || crate::preview_proxy::stands_for(&source_path, recipe);
    if let Some(proxy) = state
        .proxy_memory
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(path, crate::image_loader::decode_variant(recipe, true))
        .filter(|_| proxy_fits)
    {
        let scale = proxy.image.width() as f32 / proxy.full_width.max(1) as f32;
        let facts = Facts {
            full_width: proxy.full_width,
            full_height: proxy.full_height,
            is_raw: proxy.is_raw,
            measures: proxy.measures,
        };
        return Ok((proxy.image, scale, facts, "proxy-memory"));
    }
    let settings = load_settings(app_handle.clone()).unwrap_or_default();
    if is_raw
        && proxy_fits
        && let Some(proxy) = crate::preview_proxy::read(&source_path, &settings)
    {
        let scale = proxy.image.width() as f32 / proxy.full_width.max(1) as f32;
        let facts = Facts {
            full_width: proxy.full_width,
            full_height: proxy.full_height,
            is_raw,
            measures: proxy.measures,
        };
        return Ok((Arc::new(proxy.image), scale, facts, "proxy-disk"));
    }
    decode(&source_path, &source_str, is_raw, &settings, recipe)
}

fn decode(
    source_path: &Path,
    source_str: &str,
    is_raw: bool,
    settings: &crate::app_settings::AppSettings,
    recipe: &serde_json::Value,
) -> Result<(Arc<DynamicImage>, f32, Facts, &'static str), String> {
    let bytes = read_file_mapped(source_path).map_err(|e| e.to_string())?;
    let image = crate::image_loader::load_base_image_for_recipe(
        &bytes, source_str, is_raw, settings, None, None, recipe,
    )
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
        measures: None,
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

/// What a prepared base of `path` depends on: the recipe's preparation
/// (geometry, retouch) and the source it was made from (a full decode
/// arriving after a proxy invalidates it).
fn base_key(app_handle: &tauri::AppHandle, path: &str, recipe: &serde_json::Value) -> u64 {
    let state = app_handle.state::<AppState>();
    let mut hash = DefaultHasher::new();
    crate::preview_cache::preparation_key(recipe).hash(&mut hash);
    if let Some((image, _)) = state
        .decoded_image_cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&decoded_key(path, recipe))
    {
        (Arc::as_ptr(&image) as usize).hash(&mut hash);
    }
    hash.finish()
}

fn prepare(
    app_handle: &tauri::AppHandle,
    worker: &Worker,
    context: &GpuContext,
    todo: Todo,
) -> Result<Option<Prepared>, String> {
    let start = Instant::now();
    let preparation_key = base_key(app_handle, &todo.path, &todo.recipe);
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
            let source = load_source(app_handle, &todo.path, &todo.recipe)?;
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
            .map(|base| (base.base.clone(), base.mask_source.clone(), base.measures))
    };
    let (base, mask_source, measures, source_kind) = match cached {
        Some((base, mask_source, measures)) => (base, mask_source, measures, "base-cache"),
        None => {
            let (source, source_scale, facts, kind) = match loaded {
                Some(loaded) => loaded,
                None => load_source(app_handle, &todo.path, &todo.recipe)?,
            };
            if source_scale < 1.0 {
                keep_interim(
                    &mut worker
                        .memory
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .interims,
                    &todo.path,
                    Interim {
                        image: Arc::clone(&source),
                        scale: source_scale,
                        facts,
                    },
                    interim_budget(),
                );
            }
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
                facts.is_raw,
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
                        measures: facts.measures,
                    },
                );
            (base, mask_source, facts.measures, kind)
        }
    };
    Ok(Some(Prepared {
        path: todo.path,
        recipe: todo.recipe,
        key,
        // The size known for the photograph, the measures of the base's source.
        facts: Facts { measures, ..facts },
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
    let mask_bitmaps: Vec<Arc<MaskBitmap>> = masks
        .iter()
        .filter_map(|def| {
            generate_layer_mask(
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
    crate::baseline_exposure::apply_base_look(
        &mut adjustments.global,
        input.path,
        input.facts.measures.as_ref(),
    );
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

/// A secondary photograph's shown size (cropped and turned, full-resolution
/// px), for the webview's per-cell zoom (fit and 100 % of each cell).
#[derive(Clone, serde::Serialize)]
struct CellSize {
    path: String,
    width: f32,
    height: f32,
}

impl CellSize {
    fn new(path: &str, (width, height): (f32, f32)) -> Self {
        Self {
            path: path.to_string(),
            width,
            height,
        }
    }
}

fn emit_sizes(app_handle: &tauri::AppHandle, sizes: Vec<CellSize>) {
    if !sizes.is_empty() {
        let _ = app_handle.emit("multi-view-cell-sizes", sizes);
    }
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
    let mut sizes = Vec::new();
    for cell in &job.cells {
        if job.primary.as_deref() == Some(cell.path.as_str()) {
            continue;
        }
        let recipe = with_live_patch(recipe_of(&cell.path));
        let known = worker
            .memory
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .facts
            .get(&cell.path)
            .copied();
        let planned = known.map(|facts| {
            let shown = shown_size(&recipe, facts);
            sizes.push(CellSize::new(&cell.path, shown));
            let dim = cell_dim(cell.area, shown);
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
        emit_sizes(app_handle, sizes);
        plan_prefetch(app_handle, &job);
        return;
    }
    let count = todos.len();
    // The 1:1 previews' background work waits for the cells.
    let _busy = crate::multi_view_prefetch::RegionsBusy::cells();
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
                    let size = CellSize::new(&path, shown_size(&prepared.recipe, prepared.facts));
                    sizes.retain(|known: &CellSize| known.path != size.path);
                    sizes.push(size);
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
    emit_sizes(app_handle, sizes);
    // The decodes and bases freed above go back to the system now (as after
    // a full develop), not with the allocator's delay or never.
    rawtonic_runtime::allocator::release_freed_memory();
    if current() {
        plan_prefetch(app_handle, &job);
    }
}

/// Once the cells are shown: their photographs' 1:1 previews are made in
/// the background, so that a zoom finds them (multi_view_prefetch).
fn plan_prefetch(app_handle: &tauri::AppHandle, job: &Job) {
    let cells: Vec<String> = job.cells.iter().map(|cell| cell.path.clone()).collect();
    crate::multi_view_prefetch::plan(
        app_handle,
        crate::multi_view_prefetch::order(&cells, job.primary.as_deref()),
        job.primary.as_deref(),
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

/// Full-resolution sources decoded at the same time for zoomed regions whose
/// photographs have no 1:1 preview yet: two, one when the system is short
/// of memory. A decode alone takes up to about 2.5 GB while it runs (61 MP);
/// the region is cut from its rows, so no transformed whole-frame copy is
/// added (2026-10-12, 11 cells of 20–61 MP, six of them decoded: two at a
/// time 3.3 s instead of 4.1 s for the first locked 100 %, process peak
/// about 0.2 GB higher).
fn region_threads() -> usize {
    if crate::memory_pressure::level_now().is_none() {
        2
    } else {
        1
    }
}

/// Regions developed at the same time from a ready source (the decode in
/// memory or the bands of a 1:1 preview): a few megabytes each.
const READY_REGION_THREADS: usize = 4;

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

/// [`quantized_scale`], with magnifications within 1/2000 of 100 % at 100 %:
/// the webview's zoom arithmetic sends 0.99995 for 100 % now and then,
/// which developed every region from a whole photograph resampled by
/// 0.01 % instead of its pixels.
fn snapped_scale(scale: f32) -> f32 {
    if (scale - 1.0).abs() < 5e-4 {
        1.0
    } else {
        quantized_scale(scale)
    }
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

/// A zoom from the webview as the display keeps it: `scale` quantized so a
/// pan is not a new magnification (regions and their bases are kept per
/// magnification); None when not usable.
fn cell_zoom(z: ZoomPayload) -> Option<CellZoom> {
    (z.scale.is_finite() && z.scale > 0.0).then(|| CellZoom {
        scale: snapped_scale(z.scale),
        center: [z.center[0].clamp(0.0, 1.0), z.center[1].clamp(0.0, 1.0)],
    })
}

/// Zooms the other cells of the multi view: all to the primary's zoom
/// (`zoom`, Lightroom's zoom lock; None: fit), or, without the lock, each in
/// `cells` to its own zoom and pan (the others to fit). The cells'
/// photographs are enlarged at once; the visible regions are then developed
/// at their magnification.
#[tauri::command]
pub(crate) async fn set_multi_view_zoom(
    zoom: Option<ZoomPayload>,
    cells: Option<HashMap<String, ZoomPayload>>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app_handle.state::<AppState>();
        let Some(context) = context_of(&state) else {
            return;
        };
        let shared = zoom.and_then(cell_zoom);
        let individual: HashMap<String, CellZoom> = cells
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(path, z)| Some((path, cell_zoom(z)?)))
            .collect();
        let zooms: HashMap<String, CellZoom> = {
            let mut display = context.display.lock().unwrap_or_else(|e| e.into_inner());
            let Some(display) = display.as_mut() else {
                return;
            };
            let multi = display.multi_view();
            if !multi.is_active() || (multi.zoom() == shared && multi.cell_zooms() == &individual) {
                return;
            }
            display.set_multi_view_zooms(shared, individual);
            display.render(&context.device, &context.queue);
            let multi = display.multi_view();
            multi
                .cells()
                .iter()
                .filter_map(|cell| Some((cell.path.clone(), multi.zoom_of(&cell.path)?)))
                .collect()
        };
        if zooms.is_empty() {
            forget_regions(&app_handle);
            return;
        }
        let worker = region_worker(&app_handle);
        // Bases at another magnification of their cell are of no use any more.
        worker
            .bases
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|path, base| zooms.get(path).is_some_and(|z| z.scale == base.zoom_scale));
        let generation = worker.generation.fetch_add(1, Ordering::SeqCst) + 1;
        worker.sender.send(RegionJob { generation });
    })
    .await
    .map_err(|e| format!("Multi view zoom task failed: {e}"))
}

/// A full-resolution source: the image, its pixels per full-resolution pixel,
/// its facts, what it is and, for a 1:1 preview, its header key.
type FullSource = (
    Arc<DynamicImage>,
    f32,
    Facts,
    &'static str,
    Option<crate::preview_proxy::ProxyKey>,
);

/// How long a zoom waits for the background decode of a photograph under
/// way (`multi_view_prefetch::decoding_now`) before decoding it itself.
const PENDING_DECODE_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// A photograph at full resolution for zoomed regions: a decode still cached
/// from Develop or held (or under way) in the background, its 1:1 preview,
/// else a full decode, which is then held for the background's previews
/// (`multi_view_prefetch::offer_decode`; `generation` is the decodes'
/// generation read before `settings`). Returns the image, its pixels per
/// full-resolution pixel and its facts.
fn load_full_source(
    app_handle: &tauri::AppHandle,
    path: &str,
    recipe: &serde_json::Value,
    settings: &crate::app_settings::AppSettings,
    generation: usize,
) -> Result<FullSource, String> {
    let state = app_handle.state::<AppState>();
    let (source_path, _) = parse_virtual_path(path);
    let source_str = source_path.to_string_lossy().to_string();
    let is_raw = is_raw_file(&source_path);
    let key = decoded_key(path, recipe);
    let in_memory = decode_in_memory(&state, path, recipe, settings).or_else(|| {
        crate::multi_view_prefetch::held_or_pending_decode(&key, settings, PENDING_DECODE_WAIT)
    });
    if let Some((image, measures)) = in_memory {
        let (full_width, full_height) = image.dimensions();
        let facts = Facts {
            full_width,
            full_height,
            is_raw,
            measures,
        };
        return Ok((image, 1.0, facts, "decoded", None));
    }
    if is_raw
        && crate::preview_proxy::stands_for(&source_path, recipe)
        && let Some(proxy) = crate::preview_proxy::read_kind(
            &source_path,
            crate::preview_proxy::ProxyKind::Full,
            settings,
        )
    {
        let scale = proxy.image.width() as f32 / proxy.full_width.max(1) as f32;
        let facts = Facts {
            full_width: proxy.full_width,
            full_height: proxy.full_height,
            is_raw,
            measures: proxy.measures,
        };
        return Ok((
            Arc::new(proxy.image),
            scale,
            facts,
            "proxy-1:1",
            Some(proxy.key),
        ));
    }
    let bytes = read_file_mapped(&source_path).map_err(|e| e.to_string())?;
    let (image, measures) = crate::image_loader::load_base_image_for_recipe_measured(
        &bytes,
        &source_str,
        false,
        settings,
        None,
        None,
        recipe,
    )
    .map_err(|e| e.to_string())?;
    let (full_width, full_height) = image.dimensions();
    let facts = Facts {
        full_width,
        full_height,
        is_raw,
        measures,
    };
    let image = Arc::new(image);
    let variant = crate::image_loader::decode_variant(recipe, is_raw);
    let held = crate::multi_view_prefetch::offer_decode(
        path, &key, &image, measures, variant, generation, settings,
    );
    let kind = if held {
        "full-decode-held"
    } else {
        "full-decode"
    };
    Ok((image, 1.0, facts, kind, None))
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
    match prepare_region_from_rows(app_handle, path, area, zoom, recipe) {
        Ok(fast) => return Ok(fast),
        Err(reason) => {
            log::info!("[multi_view] {path}: region from the whole photograph ({reason})")
        }
    }
    let loading = Instant::now();
    // Before the settings: a change after makes a decode made now stale.
    let generation = crate::multi_view_prefetch::decodes_generation();
    let settings = load_settings(app_handle.clone()).unwrap_or_default();
    let (source, source_scale, facts, source_kind, preview_key) =
        load_full_source(app_handle, path, recipe, &settings, generation)?;
    log::info!(
        "[multi_view] {path}: {source_kind} source in {:.1} ms",
        loading.elapsed().as_secs_f64() * 1e3
    );
    if let Some(base) = region_from_full_decode(
        path,
        &source,
        source_scale,
        source_kind,
        preview_key.as_ref(),
        facts,
        area,
        zoom,
        recipe,
    ) {
        let kind = match source_kind {
            "full-decode" | "full-decode-held" => "full-decode-rows",
            "decoded" => "decoded-rows",
            _ => "1:1-rows",
        };
        return Ok((base, kind));
    }
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
        facts.is_raw,
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

/// The facts of `path` the cell worker found (its full size).
fn known_facts(path: &str) -> Option<Facts> {
    WORKER.get().and_then(|worker| {
        worker
            .memory
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .facts
            .get(path)
            .copied()
    })
}

/// The full size of `path` the cell worker found, if known.
pub(crate) fn full_size_of(path: &str) -> Option<(u32, u32)> {
    known_facts(path).map(|facts| (facts.full_width, facts.full_height))
}

/// Whether a region of `recipe` at full resolution can be made from the
/// rows of the source it shows (`image_processing::plan_transformed_region`
/// says the rest): retouching and masks that read the warped photograph
/// need all of it.
fn region_from_rows_possible(recipe: &serde_json::Value) -> bool {
    if crate::image_loader::has_base_retouch(recipe)
        || crate::image_loader::retouch_needs_full_resolution(recipe)
    {
        return false;
    }
    let masks: Vec<MaskDefinition> = recipe
        .get("masks")
        .and_then(|m| serde_json::from_value(m.clone()).ok())
        .unwrap_or_default();
    !masks.iter().any(MaskDefinition::requires_warped_image)
}

/// The base of the region of `path` its cell shows at `zoom` (100 % or
/// more), made from only the source rows it shows: from the decode still in
/// memory, else from the bands of its 1:1 preview that hold them (made
/// ahead by multi_view_prefetch). The pixels are those `prepare_region`
/// cuts from the whole photograph, bit for bit; None when this way does not
/// apply (no 1:1 preview yet, below 100 %, a recipe that needs the whole
/// photograph), with the reason.
fn prepare_region_from_rows(
    app_handle: &tauri::AppHandle,
    path: &str,
    area: [f32; 4],
    zoom: CellZoom,
    recipe: &serde_json::Value,
) -> Result<(RegionBase, &'static str), &'static str> {
    if zoom.scale < 1.0 {
        return Err("below 100 %");
    }
    if !region_from_rows_possible(recipe) {
        return Err("retouching or masks read the whole photograph");
    }
    let known = known_facts(path).ok_or("size not known yet")?;
    let start = Instant::now();
    let state = app_handle.state::<AppState>();
    let (source_path, _) = parse_virtual_path(path);
    let settings = load_settings(app_handle.clone()).unwrap_or_default();
    // The source and its true size (the cells' facts may come from a fast
    // decode, whose size is rounded).
    let decoded = decode_in_memory(&state, path, recipe, &settings)
        .filter(|(image, _)| image.as_rgb32f().is_some());
    let decoded_measures = decoded.as_ref().and_then(|(_, measures)| *measures);
    let decoded = decoded.map(|(image, _)| image);
    let (full_w, full_h) = match &decoded {
        Some(image) => image.dimensions(),
        None if !known.is_raw => return Err("not a RAW"),
        None if !crate::preview_proxy::stands_for(&source_path, recipe) => {
            return Err("the 1:1 preview is of another decode variant");
        }
        None => crate::preview_proxy::full_size(&source_path, &settings)
            .ok_or("no current 1:1 preview")?,
    };
    let mut facts = Facts {
        full_width: full_w,
        full_height: full_h,
        is_raw: known.is_raw,
        measures: decoded_measures,
    };
    // The measures of the decode in memory, else those recorded for the
    // 1:1 preview's photograph.
    let mut preview_key = None;
    let measures = match &decoded {
        Some(decoded) => note_measures(path, recipe, decoded, MeasuredOn::Decode(decoded), true),
        None => crate::preview_proxy::full_key(&source_path, &settings).and_then(|key| {
            let measures = known_measures(path, recipe, MeasuredOn::Preview(&key));
            preview_key = Some(key);
            measures
        }),
    };
    let plan = region_plan(facts, area, zoom, recipe, measures)?;
    let (image, kind, read_ms) = match decoded {
        Some(decoded) => {
            let rgb = decoded.as_rgb32f().ok_or("not float RGB")?;
            let rows = image_processing::SourceRows::whole(rgb.as_raw(), full_w, full_h);
            (
                image_processing::transformed_region(&rows, recipe, &plan)
                    .ok_or("rows do not cover the region")?,
                "decoded-rows",
                0.0,
            )
        }
        None => {
            let read =
                crate::preview_proxy::read_full_rows(&source_path, &settings, plan.rows, plan.cols)
                    .ok_or("no current 1:1 preview")?;
            if !rows_fit_measures(measures.is_some(), preview_key.as_ref(), &read.key) {
                return Err("1:1 preview changed while planning");
            }
            if (read.width, read.height) != (full_w, full_h) {
                return Err("1:1 preview of another size");
            }
            // Developed with the measures of the decode the preview was made from.
            facts.measures = read.measures;
            let read_ms = start.elapsed().as_secs_f64() * 1e3;
            let rows = image_processing::SourceRows {
                pixels: &read.pixels,
                width: read.width,
                height: read.height,
                row0: read.row0,
                col0: read.col0,
                cols: read.cols,
            };
            (
                image_processing::transformed_region(&rows, recipe, &plan)
                    .ok_or("rows do not cover the region")?,
                "proxy-1:1-rows",
                read_ms,
            )
        }
    };
    log::info!(
        "[multi_view] {path}: region base from rows {}..{}, columns {}..{} in {:.1} ms (read and decode {read_ms:.1} ms)",
        plan.rows.0,
        plan.rows.1,
        plan.cols.0,
        plan.cols.1,
        start.elapsed().as_secs_f64() * 1e3
    );
    Ok((region_base(image, &plan, facts, zoom, recipe), kind))
}

/// Whether rows read from a 1:1 preview with `read` key may be warped with
/// the measures planned (of the preview with `planned` key): not when the
/// preview was rebuilt in between.
fn rows_fit_measures(
    measured: bool,
    planned: Option<&crate::preview_proxy::ProxyKey>,
    read: &crate::preview_proxy::ProxyKey,
) -> bool {
    !measured || planned == Some(read)
}

/// How the region of a photograph of `facts` its cell `area` shows at
/// `zoom` (with BASE_MARGIN) is made from the rows of its source.
fn region_plan(
    facts: Facts,
    area: [f32; 4],
    zoom: CellZoom,
    recipe: &serde_json::Value,
    measures: Option<RegionMeasures>,
) -> Result<image_processing::RegionPlan, &'static str> {
    let (shown_w, shown_h) = shown_size(recipe, facts);
    let wanted =
        gpu_processing::multi_view::zoomed_region(area, [shown_w, shown_h], zoom, BASE_MARGIN);
    image_processing::plan_transformed_region_with(
        facts.full_width,
        facts.full_height,
        recipe,
        whole_pixels(wanted),
        measures,
    )
    .ok_or("lens blur, or whole-photograph measures (lateral CA, fringes) not known yet")
}

/// The region base of `image`, the pixels `plan` asks for.
fn region_base(
    image: DynamicImage,
    plan: &image_processing::RegionPlan,
    facts: Facts,
    zoom: CellZoom,
    recipe: &serde_json::Value,
) -> RegionBase {
    let [rx, ry, rw, rh] = plan.rect;
    let unscaled_crop_offset =
        serde_json::from_value::<image_processing::Crop>(recipe["crop"].clone())
            .map_or((0.0, 0.0), |c| (c.x as f32, c.y as f32));
    RegionBase {
        preparation_key: crate::preview_cache::preparation_key(recipe),
        zoom_scale: zoom.scale,
        region: [rx as f32, ry as f32, rw as f32, rh as f32],
        image: Arc::new(image),
        scale: 1.0,
        crop_offset: (
            unscaled_crop_offset.0 + rx as f32,
            unscaled_crop_offset.1 + ry as f32,
        ),
        facts,
        mask_source: None,
    }
}

/// From a full-resolution decode made now (no 1:1 preview yet): the
/// region's rows only, instead of warping the whole photograph and cutting
/// the region out (the same pixels; about a second less for a 61 MP
/// photograph with lens corrections, and no whole-frame copy).
#[allow(clippy::too_many_arguments)]
fn region_from_full_decode(
    path: &str,
    source: &Arc<DynamicImage>,
    source_scale: f32,
    source_kind: &str,
    preview_key: Option<&crate::preview_proxy::ProxyKey>,
    facts: Facts,
    area: [f32; 4],
    zoom: CellZoom,
    recipe: &serde_json::Value,
) -> Option<RegionBase> {
    if zoom.scale < 1.0 || source_scale != 1.0 || !region_from_rows_possible(recipe) {
        return None;
    }
    let rgb = source.as_rgb32f()?;
    let (full_w, full_h) = rgb.dimensions();
    let facts = Facts {
        full_width: full_w,
        full_height: full_h,
        ..facts
    };
    // A decode in memory (or held from now on) or a 1:1 preview: its own,
    // measured once. A full decode used once: its own, not recorded.
    let on = match (source_kind, preview_key) {
        ("decoded" | "full-decode-held", _) => MeasuredOn::Decode(source),
        (_, Some(key)) => MeasuredOn::Preview(key),
        _ => MeasuredOn::Fresh,
    };
    let measuring = Instant::now();
    let measures = note_measures(path, recipe, source, on, true);
    let measured_ms = measuring.elapsed().as_secs_f64() * 1e3;
    let plan = region_plan(facts, area, zoom, recipe, measures).ok()?;
    let rows = image_processing::SourceRows::whole(rgb.as_raw(), full_w, full_h);
    let image = image_processing::transformed_region(&rows, recipe, &plan)?;
    log::info!(
        "[multi_view] {path}: measured {measured_ms:.1} ms, region rows warped {:.1} ms",
        measuring.elapsed().as_secs_f64() * 1e3 - measured_ms
    );
    Some(region_base(image, &plan, facts, zoom, recipe))
}

/// The whole pixels covering `wanted` (x, y, w, h), as `prepare_region`
/// cuts them from a base at full resolution.
fn whole_pixels(wanted: [f32; 4]) -> [u32; 4] {
    let x = wanted[0].floor().max(0.0) as u32;
    let y = wanted[1].floor().max(0.0) as u32;
    let w = ((wanted[0] + wanted[2]).ceil().max(0.0) as u32)
        .saturating_sub(x)
        .max(1);
    let h = ((wanted[1] + wanted[3]).ceil().max(0.0) as u32)
        .saturating_sub(y)
        .max(1);
    [x, y, w, h]
}

/// Whether the region of `path` can be developed from a source that is
/// ready (the decode in memory or a 1:1 preview), not a RAW decoded now.
fn region_source_ready(
    app_handle: &tauri::AppHandle,
    path: &str,
    zoom: CellZoom,
    recipe: &serde_json::Value,
) -> bool {
    let state = app_handle.state::<AppState>();
    let (source_path, _) = parse_virtual_path(path);
    let settings = load_settings(app_handle.clone()).unwrap_or_default();
    let in_memory = decode_in_memory(&state, path, recipe, &settings).is_some();
    in_memory
        || (zoom.scale >= 1.0
            && crate::preview_proxy::stands_for(&source_path, recipe)
            && crate::preview_proxy::has_full(&source_path, &settings))
}

/// The kept region base of `path` that still covers what its cell `area`
/// shows at `zoom` (with REGION_MARGIN), at this magnification and
/// geometry: its region is cut from it in milliseconds.
fn kept_base(
    worker: &RegionWorker,
    path: &str,
    area: [f32; 4],
    zoom: CellZoom,
    recipe: &serde_json::Value,
) -> Option<RegionBase> {
    let preparation_key = crate::preview_cache::preparation_key(recipe);
    worker
        .bases
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(path)
        .filter(|base| {
            let (w, h) = shown_size(recipe, base.facts);
            base.preparation_key == preparation_key
                && base.zoom_scale == zoom.scale
                && contains(
                    base.region,
                    gpu_processing::multi_view::zoomed_region(area, [w, h], zoom, REGION_MARGIN),
                )
        })
        .cloned()
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
    // The texture covers the visible part and REGION_MARGIN around it; the
    // kept base reaches further (BASE_MARGIN), so a pan is cut from it.
    let wanted_in = |facts: Facts| {
        let (w, h) = shown_size(&recipe, facts);
        gpu_processing::multi_view::zoomed_region(area, [w, h], zoom, REGION_MARGIN)
    };
    let kept = kept_base(worker, path, area, zoom, &recipe);
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

/// Whether the cell of `path` is a secondary shown at magnification `scale`
/// with the recipe a region of render key `key` was developed with
/// (`region_key`).
fn still_wanted(context: &GpuContext, path: &str, key: u64, scale: f32) -> bool {
    let zoom = {
        let display = context.display.lock().unwrap_or_else(|e| e.into_inner());
        display.as_ref().and_then(|display| {
            let multi = display.multi_view();
            (multi.cell(path).is_some() && multi.primary() != Some(path))
                .then(|| multi.zoom_of(path))
                .flatten()
        })
    };
    zoom.is_some_and(|zoom| {
        zoom.scale == scale && region_key(&with_live_patch(recipe_of(path)), zoom, context) == key
    })
}

/// Interim regions developed at the same time from the cells' reduced
/// sources (a warp of a few megapixels each, ~10–40 ms).
const INTERIM_REGION_THREADS: usize = 4;

/// How long interim regions may hold back the decodes for the regions:
/// those not begun by then are left out.
const INTERIM_TIME: std::time::Duration = std::time::Duration::from_millis(300);

/// Shows interim regions (`develop_interim_region`) of `interims` (path,
/// cell area, zoom, recipe, region key, reduced source) while `current`,
/// several at a time, beginning none after [`INTERIM_TIME`].
fn show_interim_regions(
    app_handle: &tauri::AppHandle,
    context: &GpuContext,
    current: &(dyn Fn() -> bool + Sync),
    interims: Vec<(String, [f32; 4], CellZoom, serde_json::Value, u64, Interim)>,
) {
    let until = Instant::now() + INTERIM_TIME;
    let count = interims.len();
    let interims = Mutex::new(interims.into_iter());
    std::thread::scope(|scope| {
        for _ in 0..INTERIM_REGION_THREADS.min(count) {
            scope.spawn(|| {
                while current() && Instant::now() < until {
                    let Some((path, area, zoom, recipe, key, interim)) =
                        interims.lock().unwrap_or_else(|e| e.into_inner()).next()
                    else {
                        return;
                    };
                    let region = match develop_interim_region(
                        app_handle, context, &path, area, zoom, &recipe, key, &interim,
                    ) {
                        Ok(region) => region,
                        Err(e) => {
                            log::info!("[multi_view] {path}: no interim region: {e}");
                            continue;
                        }
                    };
                    if !current() {
                        return;
                    }
                    show_frame(
                        context,
                        &region.path,
                        region.key,
                        region.extent,
                        CellLayer::Region,
                        &region.rgba,
                    );
                    #[cfg(feature = "performance-profiling")]
                    log::info!(
                        "[PERF] {}",
                        serde_json::json!({
                            "stage": "multi_view_region_interim", "source": region.source_kind,
                            "ms": region.ms, "end_epoch_ms": crate::performance::epoch_ms()
                        })
                    );
                    log::info!(
                        "[multi_view] {path}: interim region {}x{} at {:.4}x in {:.1} ms",
                        region.rgba.width(),
                        region.rgba.height(),
                        zoom.scale,
                        region.ms
                    );
                }
            });
        }
    });
}

/// The render key of an interim region (`develop_interim_region`) of the
/// region whose key is `key`: never a region's own key (those are odd), so
/// the region is still developed and replaces it.
fn interim_key(key: u64) -> u64 {
    key & !1
}

/// Whether a region of `recipe` may be shown from a cell's reduced source
/// until it is developed at full resolution: not with retouching, lens blur
/// or masks that read the warped photograph (they need it at full
/// resolution, and an interim region would show them otherwise).
fn interim_region_possible(recipe: &serde_json::Value) -> bool {
    region_from_rows_possible(recipe) && !recipe["lensBlurEnabled"].as_bool().unwrap_or(false)
}

/// The region of `path` its cell `area` shows at `zoom`, from the cell's
/// reduced source (`interim`) at that source's resolution (enlarged on the
/// display), with an interim key: shown only until the region developed at
/// full resolution replaces it, never in its place.
#[allow(clippy::too_many_arguments)]
fn develop_interim_region(
    app_handle: &tauri::AppHandle,
    context: &GpuContext,
    path: &str,
    area: [f32; 4],
    zoom: CellZoom,
    recipe: &serde_json::Value,
    key: u64,
    interim: &Interim,
) -> Result<Region, String> {
    let start = Instant::now();
    let facts = interim.facts;
    let scale = zoom.scale.min(interim.scale).min(1.0);
    let dim = ((facts.full_width.max(facts.full_height) as f32 * scale).ceil() as u32).max(1) + 2;
    let base = crate::editor_preview::interim_preview_base(
        &interim.image,
        interim.scale,
        facts.full_width,
        facts.full_height,
        recipe,
        dim,
        scale,
        facts.is_raw,
    )?;
    let (shown_w, shown_h) = shown_size(recipe, facts);
    let wanted =
        gpu_processing::multi_view::zoomed_region(area, [shown_w, shown_h], zoom, REGION_MARGIN);
    let (base_w, base_h) = base.image.dimensions();
    let s = base.scale;
    let x = ((wanted[0] * s).floor().max(0.0) as u32).min(base_w.saturating_sub(1));
    let y = ((wanted[1] * s).floor().max(0.0) as u32).min(base_h.saturating_sub(1));
    let w = (((wanted[0] + wanted[2]) * s).ceil().max(0.0) as u32).clamp(x + 1, base_w) - x;
    let h = (((wanted[1] + wanted[3]) * s).ceil().max(0.0) as u32).clamp(y + 1, base_h) - y;
    let image = base.image.crop_imm(x, y, w, h);
    let key = interim_key(key);
    let rgba = develop_rgba(
        app_handle,
        context,
        DevelopInput {
            path,
            recipe,
            facts,
            image: &image,
            scale: s,
            crop_offset: (
                base.unscaled_crop_offset.0 * s + x as f32,
                base.unscaled_crop_offset.1 * s + y as f32,
            ),
            mask_source: None,
            key,
        },
    )?;
    Ok(Region {
        path: path.to_string(),
        key,
        extent: CellExtent {
            shown: [shown_w, shown_h],
            region: Some([x as f32 / s, y as f32 / s, w as f32 / s, h as f32 / s]),
        },
        rgba,
        source_kind: "interim",
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

/// The order in which the regions whose RAWs must be decoded are taken:
/// the largest photographs first (the decodes, a few at a time, end
/// together), those decoded in the background already (`facts` says
/// whether that decode is under way, and the photograph's pixels) last,
/// when that decode is nearly done.
fn heavy_order<T>(
    mut heavy: Vec<(String, [f32; 4], CellZoom, serde_json::Value, T)>,
    facts: impl Fn(&str, &serde_json::Value) -> (bool, u64),
) -> Vec<(String, [f32; 4], CellZoom, serde_json::Value, T)> {
    heavy.sort_by_cached_key(|(path, _, _, recipe, _)| {
        let (pending, pixels) = facts(path, recipe);
        (pending, std::cmp::Reverse(pixels))
    });
    heavy
}

/// A region to develop: the cell's path, area and zoom, its recipe and
/// region render key.
type RegionTodo = (String, [f32; 4], CellZoom, serde_json::Value, u64);
/// A region developed (with its magnification), or why not.
type RegionResult = (String, Result<(Region, f32), String>);

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
    // The secondaries whose shown region is not developed at their zoom yet.
    let todo: Vec<(String, [f32; 4], CellZoom, serde_json::Value, u64)> = {
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
                let zoom = multi.zoom_of(&cell.path)?;
                let recipe = with_live_patch(recipe_of(&cell.path));
                let key = region_key(&recipe, zoom, &context);
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
                                    zoom,
                                )
                            })
                        })
                });
                (!covered).then(|| (cell.path.clone(), cell.area, zoom, recipe, key))
            })
            .collect()
    };
    if todo.is_empty() {
        return;
    }
    let _busy = crate::multi_view_prefetch::RegionsBusy::start();
    let count = todo.len();
    // Short of memory: the reduced sources kept for interim regions go.
    if interim_budget() == 0
        && let Some(cells) = WORKER.get()
    {
        cells
            .memory
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .interims
            .clear();
    }
    // Regions from ready sources (a kept region base, the decode in memory,
    // a 1:1 preview) several at a time; RAWs that must be decoded now one
    // at a time beside them.
    let (ready, heavy): (Vec<_>, Vec<_>) =
        todo.into_iter().partition(|(path, area, zoom, recipe, _)| {
            kept_base(worker, path, *area, *zoom, recipe).is_some()
                || (region_from_rows_possible(recipe)
                    && region_source_ready(app_handle, path, *zoom, recipe))
        });
    let heavy = heavy_order(heavy, |path, recipe| {
        let pending = crate::multi_view_prefetch::decoding_now(&decoded_key(path, recipe));
        let pixels = facts
            .as_ref()
            .and_then(|f| f.get(path))
            .map_or(0, |f| f.full_width as u64 * f.full_height as u64);
        (pending, pixels)
    });
    // Those regions from the cells' reduced sources at once, before the
    // decodes take the cores (the ready ones go on meanwhile).
    let interims: Vec<_> = {
        let cells = WORKER.get().map(|w| {
            w.memory
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .interims
                .clone()
        });
        heavy
            .iter()
            .filter(|(_, _, _, recipe, _)| interim_region_possible(recipe))
            .filter_map(|(path, area, zoom, recipe, key)| {
                let interim = cells.as_ref()?.get(path)?.clone();
                Some((path.clone(), *area, *zoom, recipe.clone(), *key, interim))
            })
            .collect()
    };
    let ready_count = ready.len();
    let ready = Mutex::new(ready.into_iter());
    let heavy = Mutex::new(heavy.into_iter());
    let (sender, receiver) = std::sync::mpsc::channel();
    let mut shown = 0;
    // Develops the regions of `queue` one after another.
    let work = |queue: &Mutex<std::vec::IntoIter<RegionTodo>>,
                sender: std::sync::mpsc::Sender<RegionResult>| {
        loop {
            if worker.generation.load(Ordering::SeqCst) != job.generation {
                return;
            }
            let Some((path, area, zoom, recipe, key)) =
                queue.lock().unwrap_or_else(|e| e.into_inner()).next()
            else {
                return;
            };
            let result =
                develop_region(app_handle, &context, worker, &path, area, zoom, recipe, key)
                    .map(|region| (region, zoom.scale));
            if sender.send((path, result)).is_err() {
                return;
            }
        }
    };
    let (work, ready, heavy, current_ref) = (&work, &ready, &heavy, &current);
    std::thread::scope(|scope| {
        for _ in 0..READY_REGION_THREADS.min(ready_count) {
            let sender = sender.clone();
            scope.spawn(move || work(ready, sender));
        }
        // The decodes once the interim regions are shown (within
        // INTERIM_TIME).
        let heavy_sender = sender.clone();
        let interim_context = &context;
        scope.spawn(move || {
            show_interim_regions(app_handle, interim_context, current_ref, interims);
            std::thread::scope(|decodes| {
                for _ in 0..region_threads() {
                    let sender = heavy_sender.clone();
                    decodes.spawn(move || work(heavy, sender));
                }
            });
        });
        drop(sender);
        for (path, result) in receiver {
            // A job after this one (cells shown again, a pan) still wants a
            // region it finished when the cell is at that magnification
            // with that recipe: shown, so that it is not developed again.
            let wanted = |region: &Region, scale: f32| {
                current() || still_wanted(&context, &region.path, region.key, scale)
            };
            match result {
                Ok((region, scale)) if !wanted(&region, scale) => {}
                Ok((region, scale)) => {
                    show_frame(
                        &context,
                        &region.path,
                        region.key,
                        region.extent,
                        CellLayer::Region,
                        &region.rgba,
                    );
                    shown += 1;
                    #[cfg(feature = "performance-profiling")]
                    log::info!(
                        "[PERF] {}",
                        serde_json::json!({
                            "stage": "multi_view_region", "source": region.source_kind,
                            "ms": region.ms, "end_epoch_ms": crate::performance::epoch_ms()
                        })
                    );
                    log::info!(
                        "[multi_view] {path}: region {}x{} at {:.4}x from {} in {:.1} ms",
                        region.rgba.width(),
                        region.rgba.height(),
                        scale,
                        region.source_kind,
                        region.ms
                    );
                }
                Err(_) if !current() => {}
                Err(e) => log::warn!("[multi_view] {path}: region not developed: {e}"),
            }
        }
    });
    log::info!(
        "[multi_view] {shown} of {count} zoomed regions developed in {:.1} ms",
        start.elapsed().as_secs_f64() * 1e3
    );
    #[cfg(feature = "performance-profiling")]
    log::info!(
        "[PERF] {}",
        serde_json::json!({
            "stage": "multi_view_regions", "shown": shown, "count": count,
            "ms": start.elapsed().as_secs_f64() * 1e3, "end_epoch_ms": crate::performance::epoch_ms()
        })
    );
    // Full-resolution decodes made for the regions are freed: return them.
    rawtonic_runtime::allocator::release_freed_memory();
}

/// The recipe a secondary is shown with during Edit all selected: its stored
/// recipe with the changes not yet written to it.
fn with_live_patch(mut recipe: serde_json::Value) -> serde_json::Value {
    let live = LIVE.lock().unwrap_or_else(|e| e.into_inner());
    if let (Some(patch), Some(object)) = (live.patch.as_ref(), recipe.as_object_mut()) {
        for (key, value) in patch {
            object.insert(key.clone(), value.clone());
        }
    }
    recipe
}

/// Edit all selected: the primary's changes that are not yet written to the
/// other photographs (`None` once they are, or when it ends). The other cells
/// follow them live, between the primary's frames.
#[tauri::command]
pub(crate) fn set_multi_view_live(
    patch: Option<serde_json::Value>,
    state: tauri::State<'_, AppState>,
) {
    if set_live(patch) {
        // The preview worker may be waiting for the primary's next frame,
        // which a typed value never asks for.
        state.preview_jobs.wake();
    }
}

/// Keeps `patch` as the live changes; returns whether cells have to follow.
fn set_live(patch: Option<serde_json::Value>) -> bool {
    let patch = patch.and_then(|patch| match patch {
        serde_json::Value::Object(map) if !map.is_empty() => Some(map),
        _ => None,
    });
    let mut live = LIVE.lock().unwrap_or_else(|e| e.into_inner());
    // Cleared once the changes are written: the written recipes re-develop
    // the cells through `set_multi_view`.
    let pending = patch.is_some();
    live.patch = patch;
    live.settled = 0;
    LIVE_PENDING.store(pending, Ordering::Release);
    pending
}

/// Whether [`live_step`] has a cell to develop (cheap; the preview worker
/// asks after each frame).
pub(crate) fn live_pending() -> bool {
    LIVE_PENDING.load(Ordering::Acquire)
}

/// Develops the next cell that does not show the live recipe yet, if its
/// prepared base fits (a change of geometry waits for the written recipe).
/// Called by the preview worker between the primary's frames; returns
/// whether a cell was developed.
pub(crate) fn live_step(app_handle: &tauri::AppHandle) -> bool {
    if !live_pending() {
        return false;
    }
    let Some(worker) = WORKER.get().cloned() else {
        LIVE_PENDING.store(false, Ordering::Release);
        return false;
    };
    let state = app_handle.state::<AppState>();
    let Some(context) = context_of(&state) else {
        return false;
    };
    let (cells, primary) = {
        let display = context.display.lock().unwrap_or_else(|e| e.into_inner());
        match display.as_ref() {
            Some(display) if display.multi_view().is_active() => (
                display
                    .multi_view()
                    .cells()
                    .iter()
                    .map(|cell| cell.path.clone())
                    .collect::<Vec<_>>(),
                display.multi_view().primary().map(str::to_string),
            ),
            _ => (Vec::new(), None),
        }
    };
    let secondaries: Vec<String> = cells
        .into_iter()
        .filter(|path| primary.as_deref() != Some(path.as_str()))
        .collect();
    let count = secondaries.len();
    let start = Instant::now();
    for _ in 0..count {
        let (path, patch_active) = {
            let mut live = LIVE.lock().unwrap_or_else(|e| e.into_inner());
            if live.patch.is_none() || live.settled >= count {
                // Every cell shows the live recipe (or cannot follow it live).
                LIVE_PENDING.store(false, Ordering::Release);
                return false;
            }
            let path = secondaries[live.cursor % count].clone();
            live.cursor = (live.cursor + 1) % count;
            live.settled += 1;
            (path, live.patch.is_some())
        };
        let recipe = with_live_patch(recipe_of(&path));
        // A zoomed cell shows its sharp region over the enlarged photograph:
        // that region follows the change (from its kept region base).
        let zoomed = context
            .display
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .and_then(|display| {
                let multi = display.multi_view();
                Some((multi.zoom_of(&path)?, multi.cell(&path)?.area))
            });
        if let Some((zoom, area)) = zoomed {
            match live_region(app_handle, &context, &path, area, zoom, &recipe) {
                LiveRegion::Developed(ms) => {
                    LIVE.lock().unwrap_or_else(|e| e.into_inner()).settled = 0;
                    #[cfg(feature = "performance-profiling")]
                    log::info!(
                        "[PERF] {}",
                        serde_json::json!({
                            "stage": "multi_view_live_cell", "ms": start.elapsed().as_secs_f64() * 1e3,
                            "develop_ms": ms, "end_epoch_ms": crate::performance::epoch_ms(),
                            "patch": patch_active, "region": true
                        })
                    );
                    #[cfg(not(feature = "performance-profiling"))]
                    let _ = ms;
                    return true;
                }
                // The region shows it already; the hidden whole photograph
                // follows from the written recipe.
                LiveRegion::Current => continue,
                LiveRegion::Unavailable => {}
            }
        }
        let base = {
            let memory = worker.memory.lock().unwrap_or_else(|e| e.into_inner());
            memory
                .facts
                .get(&path)
                .copied()
                .zip(memory.bases.get(&path).map(|b| {
                    (
                        b.preparation_key,
                        b.dim,
                        b.base.clone(),
                        b.mask_source.clone(),
                        b.measures,
                    )
                }))
        };
        // Not prepared yet (the cell worker brings it), or the change needs
        // another base (geometry): shown once the recipe is written.
        let Some((facts, (preparation_key, dim, base, mask_source, measures))) = base else {
            continue;
        };
        let facts = Facts { measures, ..facts };
        if preparation_key != base_key(app_handle, &path, &recipe) {
            continue;
        }
        let key = render_key(&recipe, dim, &context);
        if current_key(&context, &path) == Some(key) {
            continue;
        }
        let prepared = Prepared {
            path: path.clone(),
            recipe,
            key,
            facts,
            base,
            mask_source,
            source_kind: "live",
            prepare_ms: 0.0,
        };
        match develop(app_handle, &context, prepared) {
            Ok(gpu_ms) => {
                // Developed: the others may still be behind.
                LIVE.lock().unwrap_or_else(|e| e.into_inner()).settled = 0;
                #[cfg(feature = "performance-profiling")]
                log::info!(
                    "[PERF] {}",
                    serde_json::json!({
                        "stage": "multi_view_live_cell", "ms": start.elapsed().as_secs_f64() * 1e3,
                        "develop_ms": gpu_ms, "end_epoch_ms": crate::performance::epoch_ms(),
                        "patch": patch_active
                    })
                );
                #[cfg(not(feature = "performance-profiling"))]
                let _ = (gpu_ms, patch_active, start);
                return true;
            }
            Err(e) => {
                log::warn!("[multi_view] live {path} not developed: {e}");
            }
        }
    }
    if LIVE.lock().unwrap_or_else(|e| e.into_inner()).settled >= count {
        LIVE_PENDING.store(false, Ordering::Release);
    }
    false
}

/// What [`live_step`] could do for a zoomed cell's region.
enum LiveRegion {
    /// Developed with the live recipe and shown (ms).
    Developed(f64),
    /// It shows the live recipe already.
    Current,
    /// No kept region base at this magnification covers it (or the change
    /// needs another base): left to the region worker once written.
    Unavailable,
}

/// The region of a zoomed cell developed with the live recipe from its kept
/// region base; never decodes (that would hold the preview worker).
fn live_region(
    app_handle: &tauri::AppHandle,
    context: &GpuContext,
    path: &str,
    area: [f32; 4],
    zoom: CellZoom,
    recipe: &serde_json::Value,
) -> LiveRegion {
    let Some(regions) = REGIONS.get() else {
        return LiveRegion::Unavailable;
    };
    let key = region_key(recipe, zoom, context);
    let shown_key = context
        .display
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .and_then(|display| display.multi_view().region_of(path).map(|(key, _)| key));
    if shown_key == Some(key) {
        return LiveRegion::Current;
    }
    if kept_base(regions, path, area, zoom, recipe).is_none() {
        return LiveRegion::Unavailable;
    }
    match develop_region(
        app_handle,
        context,
        regions,
        path,
        area,
        zoom,
        recipe.clone(),
        key,
    ) {
        Ok(region) => {
            show_frame(
                context,
                &region.path,
                region.key,
                region.extent,
                CellLayer::Region,
                &region.rgba,
            );
            LiveRegion::Developed(region.ms)
        }
        Err(e) => {
            log::warn!("[multi_view] live region of {path} not developed: {e}");
            LiveRegion::Unavailable
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tests of the shared measures store run one at a time.
    static MEASURES_TESTS: Mutex<()> = Mutex::new(());

    /// A session with photograph `A.RAF` (`content`) in a temporary folder.
    fn session_photo(name: &str, content: &[u8]) -> (std::path::PathBuf, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("rt-mv-{name}-{}", std::process::id()));
        let session = dir.join("Job");
        std::fs::create_dir_all(session.join("Capture")).unwrap();
        std::fs::write(session.join("session.json"), "{}").unwrap();
        std::fs::write(session.join("session.sqlite"), "").unwrap();
        let photo = session.join("Capture").join("A.RAF");
        std::fs::write(&photo, content).unwrap();
        (dir, photo)
    }

    #[test]
    fn region_measures_follow_the_preview_they_were_taken_on() {
        let _one = MEASURES_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        let (dir, photo) = session_photo("follow", b"first");
        let path = photo.to_string_lossy().to_string();
        // The never-edited master's decode variant (render version 3).
        let recipe = serde_json::json!({"fringeVersion": 2, "renderVersion": 3});
        let image = DynamicImage::ImageRgb32F(image::ImageBuffer::from_fn(64, 48, |x, y| {
            image::Rgb([x as f32 / 64.0, y as f32 / 48.0, 0.5])
        }));
        let settings = crate::app_settings::AppSettings::default();
        let other = crate::app_settings::AppSettings {
            raw_highlight_compression: Some(1.0),
            ..Default::default()
        };
        let current = |settings: &crate::app_settings::AppSettings| {
            crate::preview_proxy::full_key(&photo, settings)
                .and_then(|key| known_measures(&path, &recipe, MeasuredOn::Preview(&key)))
        };
        // The preview as the master's stored recipe has it.
        let variant = crate::image_loader::stored_decode_variant(&path);
        crate::preview_proxy::write_full(&photo, &image, &settings, variant, None).unwrap();
        assert!(crate::preview_proxy::has_full(&photo, &settings));
        let first_key = crate::preview_proxy::full_key(&photo, &settings).unwrap();
        measure_preview(&path, &recipe, &settings);
        assert!(current(&settings).is_some_and(|m| m.fringe.is_some()));
        // Rebuilt with other decoder settings: the current preview has none.
        crate::preview_proxy::write_full(&photo, &image, &other, variant, None).unwrap();
        assert_eq!(current(&other), None);
        assert_eq!(current(&settings), None);
        // Rows read after a rebuild do not fit measures planned before it.
        let rows = crate::preview_proxy::read_full_rows(&photo, &other, (0, 4), (0, 4)).unwrap();
        assert!(!rows_fit_measures(true, Some(&first_key), &rows.key));
        assert!(rows_fit_measures(true, Some(&rows.key), &rows.key));
        assert!(rows_fit_measures(false, None, &rows.key));
        // Another file at the same path: its old preview is not current.
        crate::preview_proxy::write_full(&photo, &image, &settings, variant, None).unwrap();
        measure_preview(&path, &recipe, &settings);
        assert!(current(&settings).is_some());
        std::fs::write(&photo, b"second, longer").unwrap();
        assert_eq!(current(&settings), None);
        // A decode in memory: measured once, then found.
        let decode = Arc::new(image);
        let on = MeasuredOn::Decode(&decode);
        assert_eq!(known_measures(&path, &recipe, on), None);
        let first = note_measures(&path, &recipe, &decode, on, true);
        assert_eq!(known_measures(&path, &recipe, on), first);
        // Evicted and decoded again (another allocation): measured anew.
        let image = (*decode).clone();
        drop(decode);
        let again = Arc::new(image);
        assert_eq!(
            known_measures(&path, &recipe, MeasuredOn::Decode(&again)),
            None
        );
        // A recipe that needs none records none.
        let on = MeasuredOn::Decode(&again);
        assert_eq!(
            note_measures(&path, &serde_json::json!({}), &again, on, true),
            None
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_region_gets_the_same_measures_from_a_warm_or_a_cold_store() {
        let _one = MEASURES_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        let (dir, photo) = session_photo("cold", b"raw");
        let path = photo.to_string_lossy().to_string();
        let settings = crate::app_settings::AppSettings::default();
        // A decode with a fringed edge, stored lossily as the 1:1 preview.
        let decode = DynamicImage::ImageRgb32F(image::ImageBuffer::from_fn(160, 120, |x, y| {
            let edge = x as i64 - 80 - y as i64 / 9;
            let v = 0.1 + if edge >= 0 { 0.6 } else { 0.0 };
            let red = if (-1..2).contains(&edge) { 0.25 } else { 0.0 };
            image::Rgb([v + red, v, v + 0.002 * (x % 3) as f32])
        }));
        let variant = crate::image_loader::stored_decode_variant(&path);
        crate::preview_proxy::write_full(&photo, &decode, &settings, variant, None).unwrap();
        assert!(crate::preview_proxy::has_full(&photo, &settings));
        let recipe = serde_json::json!({"fringeVersion": 2, "autoCaVersion": 2, "renderVersion": 3,
            "lensDistortionParams": {"k1": -0.05}, "colorNoiseReduction": 25, "colorNoiseVersion": 9});
        // Warm: measured by the background work after the preview was made.
        measure_preview(&path, &recipe, &settings);
        let key = crate::preview_proxy::full_key(&photo, &settings).unwrap();
        let warm = known_measures(&path, &recipe, MeasuredOn::Preview(&key)).expect("measured");
        // Cold (restart): a region measures the preview it reads.
        REGION_MEASURES
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_mut()
            .unwrap()
            .clear();
        let proxy = crate::preview_proxy::read_kind(
            &photo,
            crate::preview_proxy::ProxyKind::Full,
            &settings,
        )
        .unwrap();
        assert_eq!(proxy.key, key);
        let on = MeasuredOn::Preview(&proxy.key);
        let cold = note_measures(&path, &recipe, &proxy.image, on, true);
        assert_eq!(cold, Some(warm));
        // A full decode used once is measured but never recorded.
        let fresh = note_measures(&path, &recipe, &decode, MeasuredOn::Fresh, true);
        assert!(fresh.is_some());
        assert_eq!(
            known_measures(&path, &recipe, MeasuredOn::Preview(&key)),
            Some(warm)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_magnification_keeps_its_value_while_panning() {
        assert_eq!(quantized_scale(1.000_000_2), quantized_scale(0.999_999_8));
        assert_eq!(quantized_scale(1.0), 1.0);
        assert_eq!(quantized_scale(0.123_456), 0.1235);
        assert_ne!(quantized_scale(1.0), quantized_scale(1.01));
        assert!((quantized_scale(2.345_678) - 2.346).abs() < 1e-6);
    }

    #[test]
    fn a_hair_below_100_percent_is_100_percent() {
        assert_eq!(snapped_scale(0.999_95), 1.0);
        assert_eq!(snapped_scale(1.000_2), 1.0);
        assert_eq!(snapped_scale(0.998), quantized_scale(0.998));
        assert_eq!(snapped_scale(2.0), 2.0);
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
            measures: None,
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

    /// The base `prepare_region` cuts from the whole photograph at 100 %
    /// and the one made from the rows it shows are the same pixels at the
    /// same place.
    #[test]
    fn a_region_from_rows_matches_the_region_cut_from_the_whole_photograph() {
        let (full_w, full_h) = (640u32, 480u32);
        let source =
            DynamicImage::ImageRgb32F(image::ImageBuffer::from_fn(full_w, full_h, |x, y| {
                let v = |c: u32| ((x * 31 + y * 17 + c * 7 + (x ^ y) % 13) % 199) as f32 / 101.0;
                image::Rgb([v(0), v(1), v(2)])
            }));
        let facts = Facts {
            full_width: full_w,
            full_height: full_h,
            is_raw: true,
            measures: None,
        };
        let lens =
            serde_json::json!({"k1": -0.08, "k2": 0.01, "k3": 0.0, "model": 0, "tca_vr": 1.001});
        for recipe in [
            serde_json::json!({}),
            serde_json::json!({"lensDistortionParams": lens, "exposure": 0.5}),
            serde_json::json!({"lensDistortionParams": lens, "orientationSteps": 1,
                "crop": {"x": 12.0, "y": 40.5, "width": 400.0, "height": 560.0}}),
            serde_json::json!({"rotation": 2.5, "crop": {"x": 30.0, "y": 20.0, "width": 560.0, "height": 420.0}}),
        ] {
            assert!(region_from_rows_possible(&recipe));
            let (shown_w, shown_h) = shown_size(&recipe, facts);
            for (area, center, scale) in [
                ([0.0, 0.0, 200.0, 150.0], [0.4, 0.6], 1.0),
                ([10.0, 300.0, 120.0, 90.0], [0.02, 0.97], 1.0),
                ([0.0, 0.0, 160.0, 160.0], [0.5, 0.5], 2.0),
            ] {
                let zoom = CellZoom { scale, center };
                // As prepare_region: the whole photograph's base, cut.
                let dim = full_w.max(full_h) + 2;
                let base =
                    reduced_preview_base(&source, 1.0, full_w, full_h, &recipe, dim, 1.0, true)
                        .unwrap();
                let wanted = gpu_processing::multi_view::zoomed_region(
                    area,
                    [shown_w, shown_h],
                    zoom,
                    BASE_MARGIN,
                );
                let (base_w, base_h) = base.image.dimensions();
                let s = base.scale;
                assert_eq!(s, 1.0);
                let x = ((wanted[0] * s).floor() as u32).min(base_w.saturating_sub(1));
                let y = ((wanted[1] * s).floor() as u32).min(base_h.saturating_sub(1));
                let w = (((wanted[0] + wanted[2]) * s).ceil() as u32).clamp(x + 1, base_w) - x;
                let h = (((wanted[1] + wanted[3]) * s).ceil() as u32).clamp(y + 1, base_h) - y;
                let cut = base.image.crop_imm(x, y, w, h);
                // From the rows.
                let plan = image_processing::plan_transformed_region(
                    full_w,
                    full_h,
                    &recipe,
                    whole_pixels(wanted),
                )
                .unwrap();
                assert_eq!(plan.rect, [x, y, w, h], "{recipe}");
                assert_eq!(plan.frame, (base_w, base_h), "{recipe}");
                let raw = source.as_rgb32f().unwrap().as_raw();
                let stride = full_w as usize * 3;
                let ((c0, c1), (r0, r1)) = (plan.cols, plan.rows);
                let part: Vec<f32> = (r0..r1)
                    .flat_map(|y| {
                        let row = y as usize * stride;
                        raw[row + c0 as usize * 3..row + c1 as usize * 3].to_vec()
                    })
                    .collect();
                let rows = image_processing::SourceRows {
                    pixels: &part,
                    width: full_w,
                    height: full_h,
                    row0: r0,
                    col0: c0,
                    cols: c1 - c0,
                };
                let region = image_processing::transformed_region(&rows, &recipe, &plan).unwrap();
                assert_eq!(
                    region.to_rgba32f().as_raw(),
                    cut.to_rgba32f().as_raw(),
                    "{recipe}"
                );
            }
        }
        // Retouching and parametric masks need the whole photograph.
        assert!(!region_from_rows_possible(&serde_json::json!({
            "aiPatches": [{"id": "p", "visible": true}]
        })));
    }

    #[test]
    fn live_changes_show_over_the_stored_recipe_until_cleared() {
        let stored = serde_json::json!({"exposure": 0.0, "contrast": 10, "crop": null});
        set_live(Some(serde_json::json!({"exposure": 1.25})));
        assert!(live_pending());
        let shown = with_live_patch(stored.clone());
        assert_eq!(shown["exposure"], 1.25);
        assert_eq!(shown["contrast"], 10);
        // Written: the stored recipe alone again, nothing left to develop.
        set_live(None);
        assert!(!live_pending());
        assert_eq!(with_live_patch(stored.clone()), stored);
        // An empty patch is no patch.
        set_live(Some(serde_json::json!({})));
        assert!(!live_pending());
    }

    #[test]
    fn regions_to_decode_take_the_largest_first_and_those_decoded_in_the_background_last() {
        let zoom = CellZoom {
            scale: 1.0,
            center: [0.5, 0.5],
        };
        let item = |path: &str| (path.to_string(), [0.0; 4], zoom, serde_json::json!({}), ());
        // (decode under way in the background, pixels)
        let facts = |path: &str, _: &serde_json::Value| match path {
            "small" => (false, 20),
            "large" => (false, 61),
            "pending" => (true, 45),
            _ => (false, 0),
        };
        let order: Vec<String> = heavy_order(
            vec![
                item("pending"),
                item("small"),
                item("unknown"),
                item("large"),
            ],
            facts,
        )
        .into_iter()
        .map(|(path, ..)| path)
        .collect();
        assert_eq!(order, ["large", "small", "unknown", "pending"]);
    }

    #[test]
    fn interim_regions_never_stand_for_the_developed_region() {
        for key in [1u64, 3, 0xffff_ffff_ffff_ffff, 0x1234_5679] {
            // Region keys are odd; the interim key is another.
            assert_ne!(interim_key(key), key);
        }
        // Not where the full resolution changes the pixels beyond detail.
        assert!(interim_region_possible(&serde_json::json!({})));
        assert!(!interim_region_possible(
            &serde_json::json!({"lensBlurEnabled": true})
        ));
    }

    #[test]
    fn reduced_sources_are_kept_within_their_budget() {
        let facts = Facts {
            full_width: 2,
            full_height: 2,
            is_raw: true,
            measures: None,
        };
        // 1024 x 1024 float RGB: 12 MiB.
        let interim = || Interim {
            image: Arc::new(DynamicImage::new_rgb32f(1024, 1024)),
            scale: 0.5,
            facts,
        };
        const UNIT: usize = 12 << 20;
        let mut kept = HashMap::new();
        keep_interim(&mut kept, "a", interim(), 2 * UNIT);
        keep_interim(&mut kept, "b", interim(), 2 * UNIT);
        // Full: not kept.
        keep_interim(&mut kept, "c", interim(), 2 * UNIT);
        assert!(kept.contains_key("a") && kept.contains_key("b") && !kept.contains_key("c"));
        // Replacing one of its own photograph fits.
        keep_interim(&mut kept, "a", interim(), 2 * UNIT);
        assert_eq!(kept.len(), 2);
        // No budget (short of memory): nothing more.
        keep_interim(&mut kept, "d", interim(), 0);
        assert!(!kept.contains_key("d"));
    }

    #[test]
    fn a_kept_region_base_serves_a_pan_within_it_at_its_magnification() {
        let worker = RegionWorker {
            generation: AtomicUsize::new(0),
            sender: gpu_processing::start_latest_job_worker(|_: RegionJob| {}),
            bases: Mutex::new(HashMap::new()),
        };
        let recipe = serde_json::json!({});
        let facts = Facts {
            full_width: 4000,
            full_height: 3000,
            is_raw: true,
            measures: None,
        };
        let zoom = |x: f32| CellZoom {
            scale: 1.0,
            center: [x, 0.5],
        };
        let area = [0.0, 0.0, 400.0, 300.0];
        assert!(kept_base(&worker, "a", area, zoom(0.5), &recipe).is_none());
        // A base reaching 400 px around the visible 400 x 300 at the centre.
        worker.bases.lock().unwrap().insert(
            "a".into(),
            RegionBase {
                preparation_key: crate::preview_cache::preparation_key(&recipe),
                zoom_scale: 1.0,
                region: [1400.0, 1050.0, 1200.0, 900.0],
                image: Arc::new(DynamicImage::new_rgb32f(1, 1)),
                scale: 1.0,
                crop_offset: (0.0, 0.0),
                facts,
                mask_source: None,
            },
        );
        assert!(kept_base(&worker, "a", area, zoom(0.5), &recipe).is_some());
        // Panned 100 px: still inside it.
        assert!(kept_base(&worker, "a", area, zoom(0.525), &recipe).is_some());
        // Panned beyond it, another magnification or another geometry: not.
        assert!(kept_base(&worker, "a", area, zoom(0.8), &recipe).is_none());
        let double = CellZoom {
            scale: 2.0,
            ..zoom(0.5)
        };
        assert!(kept_base(&worker, "a", area, double, &recipe).is_none());
        let rotated = serde_json::json!({"rotation": 1.5});
        assert!(kept_base(&worker, "a", area, zoom(0.5), &rotated).is_none());
        assert!(kept_base(&worker, "b", area, zoom(0.5), &recipe).is_none());
    }
}
