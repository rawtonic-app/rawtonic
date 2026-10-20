use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::Cursor;
use std::sync::Arc;

use base64::{Engine as _, engine::general_purpose};
use image::{GrayImage, ImageFormat};

use crate::ai_lifecycle::LIFECYCLE;
use crate::ai_processing::{
    AI_MASKS_KEPT, AiDepthMaskParameters, AiForegroundMaskParameters, AiSkyMaskParameters,
    AiSubjectMaskParameters, CachedDepthMap, CachedMask, ImageEmbeddings, SamModels,
    generate_image_embeddings, get_or_init_depth_model, get_or_init_sam_models,
    get_or_init_sky_model, get_or_init_subject_model, mask_model_installed,
    run_depth_anything_model, run_sam_decoder, run_sky_seg_model, run_subject_model,
};
use crate::app_state::{AiTaskGuard, AiTaskToken, AppState};
use crate::cache_utils::GEOMETRY_KEYS;
use crate::editor_preview::get_cached_full_warped_image;

#[tauri::command]
pub fn cancel_ai_task(task_id: String, state: tauri::State<'_, AppState>) -> Result<(), String> {
    let tasks = state.active_ai_tasks.lock().unwrap();
    if let Some(token) = tasks.get(&task_id) {
        token.cancel();
    }
    Ok(())
}

fn encode_to_base64_png(image: &GrayImage) -> Result<String, String> {
    let mut buf = Cursor::new(Vec::new());
    image
        .write_to(&mut buf, ImageFormat::Png)
        .map_err(|e| e.to_string())?;
    let base64_str = general_purpose::STANDARD.encode(buf.get_ref());
    Ok(format!("data:image/png;base64,{}", base64_str))
}

#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn generate_ai_foreground_mask(
    js_adjustments: serde_json::Value,
    rotation: f32,
    flip_horizontal: bool,
    flip_vertical: bool,
    orientation_steps: u8,
    task_id: Option<String>,
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<AiForegroundMaskParameters, String> {
    let _guard = task_id
        .as_ref()
        .map(|id| AiTaskGuard::new(&state.active_ai_tasks, id.clone()));
    let cancel_flag = _guard.as_ref().map(|g| &g.token);

    if cancel_flag.as_ref().is_some_and(|t| t.is_cancelled()) {
        return Err("Task cancelled".to_string());
    }

    LIFECYCLE.touch();
    let started = std::time::Instant::now();
    let mask = subject_mask(&state, &app_handle, &js_adjustments, true, cancel_flag)
        .await?
        .ok_or_else(|| "The subject model is not available".to_string())?;
    log::info!(
        "AI subject mask: answered after {} ms",
        started.elapsed().as_millis()
    );

    Ok(AiForegroundMaskParameters {
        mask_data_base64: Some(mask.as_ref().clone()),
        rotation: Some(rotation),
        flip_horizontal: Some(flip_horizontal),
        flip_vertical: Some(flip_vertical),
        orientation_steps: Some(orientation_steps),
        warp: Some(crate::mask_generation::mask_warp(&js_adjustments)),
    })
}

#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn generate_ai_sky_mask(
    js_adjustments: serde_json::Value,
    rotation: f32,
    flip_horizontal: bool,
    flip_vertical: bool,
    orientation_steps: u8,
    task_id: Option<String>,
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<AiSkyMaskParameters, String> {
    let _guard = task_id
        .as_ref()
        .map(|id| AiTaskGuard::new(&state.active_ai_tasks, id.clone()));
    let cancel_flag = _guard.as_ref().map(|g| &g.token);

    if cancel_flag.as_ref().is_some_and(|t| t.is_cancelled()) {
        return Err("Task cancelled".to_string());
    }

    LIFECYCLE.touch();
    let started = std::time::Instant::now();
    let mask = sky_mask(&state, &app_handle, &js_adjustments, true, cancel_flag)
        .await?
        .ok_or_else(|| "The sky model is not available".to_string())?;
    log::info!(
        "AI sky mask: answered after {} ms",
        started.elapsed().as_millis()
    );
    let base64_data = mask.as_ref().clone();

    Ok(AiSkyMaskParameters {
        mask_data_base64: Some(base64_data),
        rotation: Some(rotation),
        flip_horizontal: Some(flip_horizontal),
        flip_vertical: Some(flip_vertical),
        orientation_steps: Some(orientation_steps),
        warp: Some(crate::mask_generation::mask_warp(&js_adjustments)),
    })
}

#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn generate_ai_depth_mask(
    js_adjustments: serde_json::Value,
    min_depth: f32,
    max_depth: f32,
    min_fade: f32,
    max_fade: f32,
    feather: f32,
    rotation: f32,
    flip_horizontal: bool,
    flip_vertical: bool,
    orientation_steps: u8,
    task_id: Option<String>,
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<AiDepthMaskParameters, String> {
    let _guard = task_id
        .as_ref()
        .map(|id| AiTaskGuard::new(&state.active_ai_tasks, id.clone()));
    let cancel_flag = _guard.as_ref().map(|g| &g.token);

    if cancel_flag.as_ref().is_some_and(|t| t.is_cancelled()) {
        return Err("Task cancelled".to_string());
    }

    LIFECYCLE.touch();
    let _running = LIFECYCLE.run();
    // Keyed by the photo actually loaded, taken before the model loads: a
    // photo switch meanwhile cannot file one photo's depth under another.
    let loaded = crate::image_loader::wait_for_original_image(&state)?;
    let path_hash = geometry_key(&loaded.path, &js_adjustments);
    let depth_model = get_or_init_depth_model(&app_handle, &state.ai_state, &state.ai_init_lock)
        .await
        .map_err(|e| e.to_string())?;

    if cancel_flag.as_ref().is_some_and(|t| t.is_cancelled()) {
        return Err("Task cancelled".to_string());
    }

    let cached = state
        .ai_state
        .lock()
        .unwrap()
        .as_ref()
        .and_then(|ai| ai.depth_map.clone())
        .filter(|cached| cached.path_hash == path_hash);
    let cached_depth = match cached {
        Some(cached) => cached,
        None => {
            let warped_image =
                get_cached_full_warped_image(&state, &loaded.image, loaded.is_raw, &js_adjustments);

            if cancel_flag.as_ref().is_some_and(|t| t.is_cancelled()) {
                return Err("Task cancelled".to_string());
            }

            let depth_img =
                run_depth_anything_model(warped_image.as_ref(), &depth_model, loaded.is_raw)
                    .map_err(|e| e.to_string())?;

            let new_cache = CachedDepthMap {
                path_hash,
                depth_image: depth_img,
                original_size: (warped_image.width(), warped_image.height()),
            };
            // The state may be empty: an unload while the model loaded.
            state
                .ai_state
                .lock()
                .unwrap()
                .get_or_insert_with(Default::default)
                .depth_map = Some(new_cache.clone());
            new_cache
        }
    };

    if cancel_flag.as_ref().is_some_and(|t| t.is_cancelled()) {
        return Err("Task cancelled".to_string());
    }

    let raw_depth_fullres = image::imageops::resize(
        &cached_depth.depth_image,
        cached_depth.original_size.0,
        cached_depth.original_size.1,
        image::imageops::FilterType::Triangle,
    );

    let base64_data = encode_to_base64_png(&raw_depth_fullres)?;

    Ok(AiDepthMaskParameters {
        min_depth,
        max_depth,
        min_fade,
        max_fade,
        feather,
        mask_data_base64: Some(base64_data),
        rotation: Some(rotation),
        flip_horizontal: Some(flip_horizontal),
        flip_vertical: Some(flip_vertical),
        orientation_steps: Some(orientation_steps),
        warp: Some(crate::mask_generation::mask_warp(&js_adjustments)),
    })
}

#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn generate_ai_subject_mask(
    js_adjustments: serde_json::Value,
    path: String,
    start_point: (f64, f64),
    end_point: (f64, f64),
    rotation: f32,
    flip_horizontal: bool,
    flip_vertical: bool,
    orientation_steps: u8,
    task_id: Option<String>,
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<AiSubjectMaskParameters, String> {
    let _guard = task_id
        .as_ref()
        .map(|id| AiTaskGuard::new(&state.active_ai_tasks, id.clone()));
    let cancel_flag = _guard.as_ref().map(|g| &g.token);

    if cancel_flag.as_ref().is_some_and(|t| t.is_cancelled()) {
        return Err("Task cancelled".to_string());
    }

    LIFECYCLE.touch();
    let _running = LIFECYCLE.run();
    let started = std::time::Instant::now();
    let models = get_or_init_sam_models(&app_handle, &state.ai_state, &state.ai_init_lock)
        .await
        .map_err(|e| e.to_string())?;

    if cancel_flag.as_ref().is_some_and(|t| t.is_cancelled()) {
        return Err("Task cancelled".to_string());
    }

    let (embeddings, warped_image, source_is_raw) =
        sam_embeddings(&state, &models, &js_adjustments).await?;
    let ready = started.elapsed().as_millis();

    if cancel_flag.as_ref().is_some_and(|t| t.is_cancelled()) {
        return Err("Task cancelled".to_string());
    }

    let (img_w, img_h) = embeddings.original_size;

    let (coarse_rotated_w, coarse_rotated_h) = if orientation_steps % 2 == 1 {
        (img_h as f64, img_w as f64)
    } else {
        (img_w as f64, img_h as f64)
    };

    let center = (coarse_rotated_w / 2.0, coarse_rotated_h / 2.0);

    let p1 = start_point;
    let p2 = (start_point.0, end_point.1);
    let p3 = end_point;
    let p4 = (end_point.0, start_point.1);

    let angle_rad = (rotation as f64).to_radians();
    let cos_a = angle_rad.cos();
    let sin_a = angle_rad.sin();

    let unrotate = |p: (f64, f64)| {
        let px = p.0 - center.0;
        let py = p.1 - center.1;
        let new_px = px * cos_a + py * sin_a + center.0;
        let new_py = -px * sin_a + py * cos_a + center.1;
        (new_px, new_py)
    };

    let up1 = unrotate(p1);
    let up2 = unrotate(p2);
    let up3 = unrotate(p3);
    let up4 = unrotate(p4);

    let unflip = |p: (f64, f64)| {
        let mut new_px = p.0;
        let mut new_py = p.1;
        if flip_horizontal {
            new_px = coarse_rotated_w - p.0;
        }
        if flip_vertical {
            new_py = coarse_rotated_h - p.1;
        }
        (new_px, new_py)
    };

    let ufp1 = unflip(up1);
    let ufp2 = unflip(up2);
    let ufp3 = unflip(up3);
    let ufp4 = unflip(up4);

    let un_coarse_rotate = |p: (f64, f64)| -> (f64, f64) {
        match orientation_steps {
            0 => p,
            1 => (p.1, img_h as f64 - p.0),
            2 => (img_w as f64 - p.0, img_h as f64 - p.1),
            3 => (img_w as f64 - p.1, p.0),
            _ => p,
        }
    };

    let ucrp1 = un_coarse_rotate(ufp1);
    let ucrp2 = un_coarse_rotate(ufp2);
    let ucrp3 = un_coarse_rotate(ufp3);
    let ucrp4 = un_coarse_rotate(ufp4);

    let min_x = ucrp1.0.min(ucrp2.0).min(ucrp3.0).min(ucrp4.0);
    let min_y = ucrp1.1.min(ucrp2.1).min(ucrp3.1).min(ucrp4.1);
    let max_x = ucrp1.0.max(ucrp2.0).max(ucrp3.0).max(ucrp4.0);
    let max_y = ucrp1.1.max(ucrp2.1).max(ucrp3.1).max(ucrp4.1);

    let unrotated_start_point = (min_x, min_y);
    let unrotated_end_point = (max_x, max_y);

    let decode_start = std::time::Instant::now();
    let mask_bitmap = run_sam_decoder(
        &models.decoder,
        &embeddings,
        unrotated_start_point,
        unrotated_end_point,
        Some(warped_image.as_ref()),
        source_is_raw,
    )
    .map_err(|e| e.to_string())?;
    let mask_ms = decode_start.elapsed().as_millis();

    if cancel_flag.as_ref().is_some_and(|t| t.is_cancelled()) {
        return Err("Task cancelled".to_string());
    }

    let png_start = std::time::Instant::now();
    let base64_data = encode_to_base64_png(&mask_bitmap)?;
    log::info!(
        "AI object mask: {path} {img_w}x{img_h} (raw {source_is_raw}): models and embedding ready after {ready} ms, mask {mask_ms} ms, PNG {} ms, total {} ms",
        png_start.elapsed().as_millis(),
        started.elapsed().as_millis()
    );

    Ok(AiSubjectMaskParameters {
        start_x: start_point.0,
        start_y: start_point.1,
        end_x: end_point.0,
        end_y: end_point.1,
        mask_data_base64: Some(base64_data),
        rotation: Some(rotation),
        flip_horizontal: Some(flip_horizontal),
        flip_vertical: Some(flip_vertical),
        orientation_steps: Some(orientation_steps),
        warp: Some(crate::mask_generation::mask_warp(&js_adjustments)),
    })
}

/// Key of a photo with its geometry (the adjustments that move pixels):
/// what is computed for one is not used for another.
pub(crate) fn geometry_key(path: &str, js_adjustments: &serde_json::Value) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(path.as_bytes());
    let mut geo_hasher = DefaultHasher::new();
    for key in GEOMETRY_KEYS {
        if let Some(val) = js_adjustments.get(key) {
            key.hash(&mut geo_hasher);
            val.to_string().hash(&mut geo_hasher);
        }
    }
    hasher.update(&geo_hasher.finish().to_le_bytes());
    hasher.finalize().to_hex().to_string()
}

/// Serialises SAM encodes: a click that arrives while the photo is being
/// encoded in the background waits for that encode instead of starting a
/// second one.
static SAM_ENCODE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// SAM's embedding of the current photo with its geometry (cached per photo
/// and geometry), the warped image it was computed from and whether that is
/// a linear RAW decode.
async fn sam_embeddings(
    state: &tauri::State<'_, AppState>,
    models: &Arc<SamModels>,
    js_adjustments: &serde_json::Value,
) -> Result<(ImageEmbeddings, Arc<image::DynamicImage>, bool), String> {
    let _encode = SAM_ENCODE_LOCK.lock().await;
    // Keyed by the photo actually loaded, not the caller's path: a
    // background encode that outlives a photo switch cannot file the new
    // photo's embedding under the old one.
    let loaded = crate::image_loader::wait_for_original_image(state)?;
    let (mask_source, source_is_raw) = (loaded.image, loaded.is_raw);
    let path_hash = geometry_key(&loaded.path, js_adjustments);
    let cached = || {
        state
            .ai_state
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|ai| ai.embeddings.clone())
            .filter(|e| e.path_hash == path_hash)
    };
    let warped_image =
        get_cached_full_warped_image(state, &mask_source, source_is_raw, js_adjustments);
    if let Some(embeddings) = cached() {
        return Ok((embeddings, warped_image, source_is_raw));
    }
    let models = models.clone();
    let image = warped_image.clone();
    let generation = LIFECYCLE.generation();
    let mut embeddings = tokio::task::spawn_blocking(move || {
        generate_image_embeddings(image.as_ref(), &models.encoder, source_is_raw)
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())?;
    embeddings.path_hash = path_hash;
    // An encoding that outlived an unload is used once, not kept.
    let mut ai = state.ai_state.lock().unwrap();
    if LIFECYCLE.keep(generation) {
        ai.get_or_insert_with(Default::default).embeddings = Some(embeddings.clone());
    }
    drop(ai);
    Ok((embeddings, warped_image, source_is_raw))
}

#[tauri::command]
pub async fn precompute_ai_subject_mask(
    js_adjustments: serde_json::Value,
    path: String,
    background: Option<bool>,
    task_id: Option<String>,
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    let _running = LIFECYCLE.run();
    // `task_id` cancels it (a photo switch, the Library, memory pressure).
    let _guard = task_id
        .as_ref()
        .map(|id| AiTaskGuard::new(&state.active_ai_tasks, id.clone()));
    let cancelled = || _guard.as_ref().is_some_and(|g| g.token.is_cancelled());
    // `background`: asked for by opening a photo, not by the user: only
    // with a model that is already installed (no download), and while the
    // system is not short of memory (as `precompute`).
    if background == Some(true) {
        if crate::memory_pressure::under_pressure() {
            log::info!("AI object mask: not encoded ahead, the system is short of memory");
            return Ok(());
        }
        if cancelled() {
            return Ok(());
        }
        crate::ai_processing::preload_sam_models(&app_handle, &state.ai_state, &state.ai_init_lock)
            .await
            .map_err(|e| e.to_string())?;
        let loaded = state
            .ai_state
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|ai| ai.sam_models.is_some());
        if !loaded {
            return Ok(());
        }
    }
    let models = get_or_init_sam_models(&app_handle, &state.ai_state, &state.ai_init_lock)
        .await
        .map_err(|e| e.to_string())?;
    // A request for a photo that is no longer open is dropped.
    let current = crate::image_loader::wait_for_original_image(&state)
        .ok()
        .map(|l| l.path);
    if current.as_deref() != Some(path.as_str()) || cancelled() {
        return Ok(());
    }
    let start = std::time::Instant::now();
    sam_embeddings(&state, &models, &js_adjustments).await?;
    log::info!(
        "AI object mask: background embedding of {path} ready after {} ms",
        start.elapsed().as_millis()
    );
    Ok(())
}

/// The automatic selections that are computed ahead and cached per photo
/// and geometry: Subject/Background (BiRefNet) and Sky (UperNet).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AutoMask {
    Subject,
    Sky,
}

/// Serialises Subject runs: a click on Subject or Background that arrives
/// while the photo is computed in the background waits for that run
/// instead of starting a second one.
static SUBJECT_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
/// The same for Sky.
static SKY_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

impl AutoMask {
    fn name(self) -> &'static str {
        match self {
            Self::Subject => "subject",
            Self::Sky => "sky",
        }
    }

    fn lock(self) -> &'static tokio::sync::Mutex<()> {
        match self {
            Self::Subject => &SUBJECT_LOCK,
            Self::Sky => &SKY_LOCK,
        }
    }

    fn cache(self, ai: &mut crate::ai_processing::AiState) -> &mut Vec<CachedMask> {
        match self {
            Self::Subject => &mut ai.subject_masks,
            Self::Sky => &mut ai.sky_masks,
        }
    }
}

fn cached_mask(state: &AppState, kind: AutoMask, key: &str) -> Option<Arc<String>> {
    let mut ai = state.ai_state.lock().unwrap();
    kind.cache(ai.as_mut()?)
        .iter()
        .find(|m| m.key == key)
        .map(|m| m.mask_data_base64.clone())
}

fn store_mask(state: &AppState, kind: AutoMask, key: String, data: Arc<String>) {
    let mut ai = state.ai_state.lock().unwrap();
    let masks = kind.cache(ai.get_or_insert_with(Default::default));
    masks.retain(|m| m.key != key);
    if masks.len() >= AI_MASKS_KEPT {
        masks.remove(0);
    }
    masks.push(CachedMask {
        key,
        mask_data_base64: data,
    });
}

enum Network {
    Subject(Arc<crate::ai_processing::SubjectModel>),
    Sky(Arc<crate::ai_processing::SkyModel>),
}

impl Network {
    fn on_onnx(&self) -> bool {
        match self {
            Self::Subject(model) => model.on_onnx(),
            Self::Sky(model) => model.on_onnx(),
        }
    }
}

/// Whether the network for `kind` is loaded.
fn model_loaded(state: &AppState, kind: AutoMask) -> bool {
    state
        .ai_state
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(|ai| match kind {
            AutoMask::Subject => ai.subject_model.is_some(),
            AutoMask::Sky => ai.sky_model.is_some(),
        })
}

/// The loaded network for `kind`; with `download` false only an installed
/// model (`None` without one).
async fn network(
    state: &AppState,
    app_handle: &tauri::AppHandle,
    kind: AutoMask,
    download: bool,
) -> Result<Option<Network>, String> {
    Ok(match kind {
        AutoMask::Subject => {
            get_or_init_subject_model(app_handle, &state.ai_state, &state.ai_init_lock, download)
                .await
                .map_err(|e| e.to_string())?
                .map(Network::Subject)
        }
        AutoMask::Sky => get_or_init_sky_model(app_handle, &state.ai_state, download)
            .await
            .map_err(|e| e.to_string())?
            .map(Network::Sky),
    })
}

/// The Subject or Sky mask (PNG data URL at the photo's size) of the photo
/// that is loaded, with the caller's geometry: cached per photo and
/// geometry, else computed and cached. `download`: false for background
/// requests, which use an installed model only (`Ok(None)` without one).
/// Cancelling `cancel` stops a run of ONNX Runtime at once.
async fn auto_mask(
    state: &tauri::State<'_, AppState>,
    app_handle: &tauri::AppHandle,
    js_adjustments: &serde_json::Value,
    kind: AutoMask,
    download: bool,
    cancel: Option<&Arc<AiTaskToken>>,
) -> Result<Option<Arc<String>>, String> {
    let cancelled = || cancel.is_some_and(|t| t.is_cancelled());
    // Counts as running from here, so an idle unload waits for the load too.
    let _running = LIFECYCLE.run();
    let _run = kind.lock().lock().await;
    // Keyed by the photo actually loaded, not the caller's path: a run that
    // outlives a photo switch cannot file one photo's mask under another.
    let loaded = crate::image_loader::wait_for_original_image(state)?;
    let key = geometry_key(&loaded.path, js_adjustments);
    if let Some(mask) = cached_mask(state, kind, &key) {
        return Ok(Some(mask));
    }
    if cancelled() {
        return Err("Task cancelled".to_string());
    }
    // The photo's warped image is made while a model that is not loaded yet
    // loads (Core ML loads on a worker thread): a first click waits for the
    // longer of the two instead of both. Only when the model will be there:
    // asked for, or installed.
    let warp = || {
        let start = std::time::Instant::now();
        let image =
            get_cached_full_warped_image(state, &loaded.image, loaded.is_raw, js_adjustments);
        (image, start.elapsed().as_millis())
    };
    let (network, warped) = if !model_loaded(state, kind)
        && (download || mask_model_installed(app_handle, kind == AutoMask::Sky))
    {
        let (network, warped) =
            tokio::join!(network(state, app_handle, kind, download), async { warp() });
        (network?, Some(warped))
    } else {
        (network(state, app_handle, kind, download).await?, None)
    };
    let during_load = if warped.is_some() {
        " (while the model loaded)"
    } else {
        ""
    };
    let Some(network) = network else {
        return Ok(None);
    };
    if cancelled() {
        return Err("Task cancelled".to_string());
    }
    let start = std::time::Instant::now();
    let (warped_image, warped_ms) = warped.unwrap_or_else(warp);
    let (width, height) = (warped_image.width(), warped_image.height());
    // Only an ONNX Runtime run can be stopped; Core ML needs no ONNX Runtime
    // at all (without one, creating run options would fail).
    let options = if network.on_onnx() {
        Some(Arc::new(
            ort::session::RunOptions::new().map_err(|e| e.to_string())?,
        ))
    } else {
        None
    };
    let watcher = cancel.zip(options.clone()).map(|(token, options)| {
        let token = token.clone();
        tokio::spawn(async move {
            token.wait_for_cancel().await;
            let _ = options.terminate();
        })
    });
    let is_raw = loaded.is_raw;
    let run_options = options.clone();
    let result = tokio::task::spawn_blocking(move || {
        let run = std::time::Instant::now();
        let qos = crate::ai_processing::thread_qos();
        let (mask, backend) = match &network {
            Network::Subject(model) => (
                run_subject_model(warped_image.as_ref(), model, is_raw, run_options.as_deref()),
                model.backend_name(),
            ),
            Network::Sky(model) => (
                run_sky_seg_model(warped_image.as_ref(), model, is_raw, run_options.as_deref()),
                model.backend_name(),
            ),
        };
        let mask = mask.map_err(|e| e.to_string())?;
        let model_ms = run.elapsed().as_millis();
        let png = std::time::Instant::now();
        let data = encode_to_base64_png(&mask)?;
        log::info!(
            "AI {} mask: PNG {} ms",
            kind.name(),
            png.elapsed().as_millis()
        );
        Ok::<_, String>((data, model_ms, format!("{backend}, QoS {qos}")))
    })
    .await;
    if let Some(watcher) = watcher {
        watcher.abort();
    }
    if cancelled() {
        return Err("Task cancelled".to_string());
    }
    let (data, model_ms, backend) = result.map_err(|e| e.to_string())??;
    log::info!(
        "AI {} mask: {} {width}x{height} (raw {is_raw}) on {backend}: warped image {warped_ms} ms{during_load}, model {model_ms} ms, total {} ms",
        kind.name(),
        loaded.path,
        start.elapsed().as_millis()
    );
    let data = Arc::new(data);
    store_mask(state, kind, key, data.clone());
    Ok(Some(data))
}

async fn subject_mask(
    state: &tauri::State<'_, AppState>,
    app_handle: &tauri::AppHandle,
    js_adjustments: &serde_json::Value,
    download: bool,
    cancel: Option<&Arc<AiTaskToken>>,
) -> Result<Option<Arc<String>>, String> {
    auto_mask(
        state,
        app_handle,
        js_adjustments,
        AutoMask::Subject,
        download,
        cancel,
    )
    .await
}

async fn sky_mask(
    state: &tauri::State<'_, AppState>,
    app_handle: &tauri::AppHandle,
    js_adjustments: &serde_json::Value,
    download: bool,
    cancel: Option<&Arc<AiTaskToken>>,
) -> Result<Option<Arc<String>>, String> {
    auto_mask(
        state,
        app_handle,
        js_adjustments,
        AutoMask::Sky,
        download,
        cancel,
    )
    .await
}

/// Computes the open photo's Subject or Sky mask in the background, so the
/// selection answers at once. Only with a model that is already installed
/// (nothing is downloaded) and while the system is not short of memory;
/// dropped when the photo is no longer open; `task_id` cancels it (a photo
/// switch).
async fn precompute(
    kind: AutoMask,
    js_adjustments: serde_json::Value,
    path: String,
    task_id: Option<String>,
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    let _guard = task_id
        .as_ref()
        .map(|id| AiTaskGuard::new(&state.active_ai_tasks, id.clone()));
    let cancel = _guard.as_ref().map(|g| &g.token);
    let current = crate::image_loader::wait_for_original_image(&state)
        .ok()
        .map(|l| l.path);
    if current.as_deref() != Some(path.as_str()) {
        return Ok(());
    }
    // Ahead of a click only with memory to spare: a run holds about 1.6 GB
    // on Core ML (and loads the model if it is not loaded).
    if crate::memory_pressure::under_pressure() {
        log::info!(
            "AI {} mask: not computed ahead, the system is short of memory",
            kind.name()
        );
        return Ok(());
    }
    let start = std::time::Instant::now();
    match auto_mask(&state, &app_handle, &js_adjustments, kind, false, cancel).await {
        Ok(Some(_)) => {
            log::info!(
                "AI {} mask: background mask of {path} ready after {} ms",
                kind.name(),
                start.elapsed().as_millis()
            );
            Ok(())
        }
        Ok(None) => Ok(()),
        Err(_) if cancel.is_some_and(|t| t.is_cancelled()) => {
            log::info!(
                "AI {} mask: background run of {path} cancelled after {} ms",
                kind.name(),
                start.elapsed().as_millis()
            );
            Ok(())
        }
        Err(e) => Err(e),
    }
}

/// Subject/Background ahead of the click (`precompute`): asked for when the
/// masking tools are open, or when a photo opens after they were used.
#[tauri::command]
pub async fn precompute_ai_foreground_mask(
    js_adjustments: serde_json::Value,
    path: String,
    task_id: Option<String>,
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    precompute(
        AutoMask::Subject,
        js_adjustments,
        path,
        task_id,
        state,
        app_handle,
    )
    .await
}

/// Sky ahead of the click (`precompute`), asked for after Subject.
#[tauri::command]
pub async fn precompute_ai_sky_mask(
    js_adjustments: serde_json::Value,
    path: String,
    task_id: Option<String>,
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    precompute(
        AutoMask::Sky,
        js_adjustments,
        path,
        task_id,
        state,
        app_handle,
    )
    .await
}

/// Loads the Subject and Sky models (installed ones only; nothing is
/// downloaded), asked for when Develop opens for someone who used the
/// masking tools before, so the load overlaps the photo's decoding. Not
/// while the system is short of memory (the photo's masks are then computed
/// on a click only).
///
/// No warm-up run: Core ML's first run after a load is slower (BiRefNet
/// fp16 on an M5 Pro 0.53-0.9 s in the app against 0.27-0.29 s later), but
/// one on a constant input right after the load, which makes the next run
/// fast in a tool (0.17-0.31 s), did not shorten the photo's first run in
/// the app (0.36-0.77 s after it, 2026-10-11) and cost another ~0.6 s of
/// GPU while the photo opened. The first photo of a Develop visit gets its
/// masks computed at once instead (`useAiMasking`), so that run is the
/// warm-up and its result is used (notes: reference/birefnet-fp16.md).
#[tauri::command]
pub async fn preload_ai_mask_models(
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    if crate::memory_pressure::under_pressure() {
        log::info!("AI masks: models not loaded ahead, the system is short of memory");
        return Ok(());
    }
    let _running = LIFECYCLE.run();
    let start = std::time::Instant::now();
    for kind in [AutoMask::Subject, AutoMask::Sky] {
        network(&state, &app_handle, kind, false).await?;
    }
    log::info!(
        "AI masks: models loaded ahead after {} ms",
        start.elapsed().as_millis()
    );
    Ok(())
}

/// Analyze only on request; subsequent light edits reuse the persisted depth.
#[tauri::command]
pub async fn generate_relight_depth(
    js_adjustments: serde_json::Value,
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<serde_json::Value, String> {
    // Capture the source before awaiting model initialization.
    let (source, is_raw) = crate::editor_preview::get_original_image(&state)?;
    LIFECYCLE.touch();
    let running = LIFECYCLE.run();
    let model = crate::ai_processing::get_or_init_relight_model(
        &app_handle,
        &state.ai_state,
        &state.ai_init_lock,
    )
    .await
    .map_err(|e| e.to_string())?;
    // Facial relief only refines the depth; without the face models (or if
    // they fail on this photo) the scene depth alone still lights it.
    let face_models = crate::ai_faces::models(&app_handle, true)
        .await
        .inspect_err(|e| log::warn!("Relight: continuing without face models: {e}"))
        .ok();
    let running = std::sync::Arc::new(running);
    let first = running.clone();
    let adjustments = js_adjustments.clone();
    let (depth, camera, small, faces, parse) = tauri::async_runtime::spawn_blocking(move || {
        let _running = first;
        let warped = rawtonic_engine::mask_generation::prepare_shared_warped_mask_image(
            &source,
            is_raw,
            &adjustments,
        );
        let (displayed, _) = rawtonic_engine::adjustment_utils::apply_spatial_transformations(
            warped.as_ref(),
            &adjustments,
        );
        let (depth, camera) = crate::ai_processing::run_relight_model(&displayed, &model, is_raw)
            .map_err(|e| e.to_string())?;
        let small = if displayed.width().max(displayed.height()) > 1600 {
            crate::image_processing::thumbnail(&displayed, 1600, 1600)
        } else {
            displayed.into_owned()
        };
        let face_input = rawtonic_faces::recipe::display_rgb8(&small, is_raw);
        let faces: Vec<Vec<[f32; 4]>> = face_models
            .as_ref()
            .and_then(|models| {
                rawtonic_faces::relight::analyze(&face_input, models)
                    .inspect_err(|e| log::warn!("Relight: face analysis failed: {e}"))
                    .ok()
            })
            .unwrap_or_default();
        // People the face detector misses (a profile) still show as skin.
        let parse = face_models.filter(|_| faces.is_empty()).and_then(|models| {
            rawtonic_faces::relight::parse_people(&face_input, &models)
                .inspect_err(|e| log::warn!("Relight: people parse failed: {e}"))
                .ok()
                .flatten()
        });
        Ok::<_, String>((depth, camera, small, faces, parse))
    })
    .await
    .map_err(|e| e.to_string())??;
    // The subject's matte separates it from its background (version five);
    // people's normals light them better than the scene depth and let a new
    // light take the photo's own lighting off them. People normals load only
    // for photos with faces; without these models the scene depth alone
    // still lights the photo.
    let subject_model = crate::ai_processing::get_or_init_subject_model(
        &app_handle,
        &state.ai_state,
        &state.ai_init_lock,
        true,
    )
    .await
    .inspect_err(|e| log::warn!("Relight: continuing without the subject model: {e}"))
    .ok()
    .flatten();
    // Faces, or a parse with skin, and a subject to give them normals in:
    // only then is the people-normal model worth loading (449 MB).
    let maybe_people = subject_model.is_some() && (!faces.is_empty() || parse.is_some());
    let people_model = if !maybe_people {
        None
    } else {
        crate::ai_processing::get_or_init_people_normals_model(
            &app_handle,
            &state.ai_state,
            &state.ai_init_lock,
        )
        .await
        .inspect_err(|e| log::warn!("Relight: continuing without people normals: {e}"))
        .ok()
    };
    // The sky: a new light does not reach it.
    let sky_model = crate::ai_processing::get_or_init_sky_model(&app_handle, &state.ai_state, true)
        .await
        .inspect_err(|e| log::warn!("Relight: continuing without the sky model: {e}"))
        .ok()
        .flatten();
    let subject = match subject_model {
        Some(subject_model) => {
            let (small, faces, depth) = (small.clone(), faces.clone(), depth.clone());
            let parse = parse.clone();
            let second = running.clone();
            tauri::async_runtime::spawn_blocking(move || {
                let _running = second;
                let start = std::time::Instant::now();
                let sky = sky_model.as_ref().and_then(|model| {
                    crate::ai_processing::run_sky_seg_model(&small, model, is_raw, None)
                        .inspect_err(|e| log::warn!("Relight: sky analysis failed: {e}"))
                        .ok()
                });
                crate::ai_processing::run_subject_model(&small, &subject_model, is_raw, None)
                    .and_then(|matte| {
                        let analysis = crate::ai_processing::analyze_subject(
                            &small,
                            is_raw,
                            &matte,
                            crate::ai_processing::SubjectContext {
                                faces: &faces,
                                parse: parse.as_ref(),
                                people_model: people_model.as_deref(),
                                scene: Some((&depth, camera)),
                                sky: sky.as_ref(),
                            },
                        )?;
                        Ok((analysis.png, analysis.light, analysis.inverse))
                    })
                    .inspect(|(_, light, _)| {
                        log::info!(
                            "Relight: subject analyzed in {} ms, own lighting {}",
                            start.elapsed().as_millis(),
                            if light.is_some() {
                                "fitted"
                            } else {
                                "not fitted"
                            }
                        )
                    })
                    .inspect_err(|e| log::info!("Relight: no subject: {e}"))
                    .ok()
            })
            .await
            .map_err(|e| e.to_string())?
        }
        None => None,
    };
    drop(running);
    let mut png = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageLuma16(depth)
        .write_to(&mut png, image::ImageFormat::Png)
        .map_err(|e| e.to_string())?;
    let encode = |bytes: Vec<u8>| {
        format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(bytes)
        )
    };
    let mut result = serde_json::json!({
        "data": encode(png.into_inner()),
        "geometry": rawtonic_engine::relight::geometry(&js_adjustments),
        "faces": faces,
        "version": 5,
        "camera": camera,
    });
    if let Some((subject, light, inverse)) = subject {
        result["subject"] = encode(subject).into();
        if let Some(light) = light {
            result["sourceLight"] = serde_json::json!(light);
        }
        if let Some(inverse) = inverse {
            result["subjectDepth"] = serde_json::json!(inverse);
        }
    }
    Ok(result)
}

/// Reads an image the user chose as a Relight pattern and returns it as the
/// recipe stores it (`relight::pattern_png`, a PNG data URL). Read only.
#[tauri::command]
pub async fn load_relight_pattern(path: String) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let image = image::ImageReader::open(&path)
            .and_then(|reader| reader.with_guessed_format())
            .map_err(|e| e.to_string())?
            .decode()
            .map_err(|e| e.to_string())?;
        let png = rawtonic_engine::relight::pattern_png(&image)?;
        Ok(format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(png)
        ))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn geometry_key_follows_path_and_geometry_only() {
        let a = serde_json::json!({"exposure": 1.0, "transformRotate": 0.0});
        let b = serde_json::json!({"exposure": 2.0, "transformRotate": 0.0});
        let c = serde_json::json!({"exposure": 1.0, "transformRotate": 5.0});
        assert_eq!(geometry_key("x.raf", &a), geometry_key("x.raf", &b));
        assert_ne!(geometry_key("x.raf", &a), geometry_key("x.raf", &c));
        assert_ne!(geometry_key("x.raf", &a), geometry_key("y.raf", &a));
    }
}
