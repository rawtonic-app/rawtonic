use crate::state::ai::AiState;
use crate::state::lock::lock_recover;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use base64::Engine as _;
use image::GrayImage;
use tauri::Manager;

use crate::ai_processing::{
    AI_MASKS_KEPT, AiDepthMaskParameters, AiForegroundMaskParameters, AiSkyMaskParameters,
    AiSubjectMaskParameters, CachedDepthMap, CachedLandscape, CachedMask, ImageEmbeddings,
    SamModels, SamPrompt, generate_image_embeddings, get_or_init_depth_model,
    get_or_init_sam_models, get_or_init_sky_model, get_or_init_subject_model, mask_model_installed,
    run_depth_anything_model, run_sam_decoder, run_sam_decoder_prompts, run_sky_seg_model,
    subject_selection,
};
use crate::app_state::{AiTaskGuard, AiTaskToken, AppState};
use crate::cache_utils::GEOMETRY_KEYS;
use crate::ipc::{CommandError, ErrorCode};

#[tauri::command]
pub fn cancel_ai_task(task_id: String, ai_state: tauri::State<'_, AiState>) -> Result<(), String> {
    let tasks = lock_recover(&ai_state.tasks);
    if let Some(token) = tasks.get(&task_id) {
        token.cancel();
    }
    Ok(())
}

/// The error of a mask asked for one photo while another is loaded (the
/// editor switched photos meanwhile); the editor asks again once its photo
/// is open (`useAiMasking.ts`).
/// The message of an AI task its token stopped (`cancel_ai_task`).
pub(crate) const TASK_CANCELLED: &str = "Task cancelled";

pub(crate) fn task_cancelled() -> CommandError {
    CommandError::cancelled(TASK_CANCELLED)
}

pub(crate) const PHOTO_CHANGED: &str = "The photo changed while its mask was asked for";

/// Whether `loaded` (the photo the backend holds) is the photo `asked` for.
pub(crate) fn ensure_photo(loaded: &str, asked: &str) -> Result<(), CommandError> {
    if loaded == asked {
        Ok(())
    } else {
        Err(CommandError::new(ErrorCode::Stale, PHOTO_CHANGED))
    }
}

fn encode_to_base64_png(image: &GrayImage) -> Result<String, String> {
    rawtonic_segment::mask_data_url(image)
}

#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn generate_ai_foreground_mask(
    js_adjustments: serde_json::Value,
    path: String,
    rotation: f32,
    flip_horizontal: bool,
    flip_vertical: bool,
    orientation_steps: u8,
    subject_version: Option<u32>,
    task_id: Option<String>,
    download: Option<bool>,
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<AiForegroundMaskParameters, CommandError> {
    let ai_state = app_handle.state::<AiState>();
    let _guard = task_id
        .as_ref()
        .map(|id| AiTaskGuard::new(&ai_state.tasks, id.clone()));
    let cancel_flag = _guard.as_ref().map(|g| &g.token);

    if cancel_flag.as_ref().is_some_and(|t| t.is_cancelled()) {
        return Err(task_cancelled());
    }

    ai_state.lifecycle.touch();
    let started = std::time::Instant::now();
    // The component's `subjectVersion` (absent: 1, a selection saved before
    // version 2).
    let version = subject_version.unwrap_or(1);
    let mask = subject_mask(
        &state,
        &ai_state,
        &app_handle,
        &js_adjustments,
        version,
        &path,
        download.unwrap_or(true),
        cancel_flag,
    )
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
    path: String,
    rotation: f32,
    flip_horizontal: bool,
    flip_vertical: bool,
    orientation_steps: u8,
    task_id: Option<String>,
    download: Option<bool>,
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<AiSkyMaskParameters, CommandError> {
    let ai_state = app_handle.state::<AiState>();
    let _guard = task_id
        .as_ref()
        .map(|id| AiTaskGuard::new(&ai_state.tasks, id.clone()));
    let cancel_flag = _guard.as_ref().map(|g| &g.token);

    if cancel_flag.as_ref().is_some_and(|t| t.is_cancelled()) {
        return Err(task_cancelled());
    }

    ai_state.lifecycle.touch();
    let started = std::time::Instant::now();
    let mask = sky_mask(
        &state,
        &ai_state,
        &app_handle,
        &js_adjustments,
        &path,
        download.unwrap_or(true),
        cancel_flag,
    )
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
        bitmap_grid_of: None,
    })
}

/// Parts of the landscape (`parts`: `rawtonic_segment::LandscapePart` keys)
/// of the open photo with the caller's geometry, as one mask. Every part of
/// a photo comes from one run of the landscape model, kept per photo and
/// geometry (`AiModels::landscape_runs`): other parts answer without the
/// network. Downloads the model only with `download` (the editor's
/// automatic detection does not: `ModelsMissing` without it; opening a
/// Landscape mask's settings downloads it, `detect_landscape_parts`).
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn generate_ai_landscape_mask(
    js_adjustments: serde_json::Value,
    path: String,
    parts: Vec<String>,
    version: Option<u32>,
    download: Option<bool>,
    rotation: f32,
    flip_horizontal: bool,
    flip_vertical: bool,
    orientation_steps: u8,
    task_id: Option<String>,
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<AiSkyMaskParameters, CommandError> {
    let parts = rawtonic_segment::landscape_parts(&parts).map_err(|e| e.to_string())?;
    let ai_state = app_handle.state::<AiState>();
    let _guard = task_id
        .as_ref()
        .map(|id| AiTaskGuard::new(&ai_state.tasks, id.clone()));
    let cancel = _guard.as_ref().map(|g| &g.token);
    let cancelled = || cancel.is_some_and(|t| t.is_cancelled());
    if cancelled() {
        return Err(task_cancelled());
    }
    ai_state.lifecycle.touch();
    let started = std::time::Instant::now();
    let run = landscape_run(
        &state,
        &ai_state,
        &app_handle,
        &js_adjustments,
        &path,
        download.unwrap_or(false),
        cancel,
    )
    .await?;
    if cancelled() {
        return Err(task_cancelled());
    }
    // The network's grid with the photo's size: placing it reads the grid
    // (`mask_generation::BITMAP_GRID_OF`), a 36 MP photo's full-size mask
    // took ~50 ms to make and ~80 ms to decode for each part.
    let size = run.size;
    let data = tokio::task::spawn_blocking(move || {
        // Absent: version 1, as a component without `landscapeVersion`.
        let mask =
            rawtonic_segment::landscape_mask_grid(&run.probabilities, &parts, version.unwrap_or(1));
        encode_to_base64_png(&mask)
    })
    .await
    .map_err(|e| e.to_string())??;
    log::info!(
        "AI landscape mask: answered after {} ms",
        started.elapsed().as_millis()
    );
    Ok(AiSkyMaskParameters {
        mask_data_base64: Some(data),
        rotation: Some(rotation),
        flip_horizontal: Some(flip_horizontal),
        flip_vertical: Some(flip_vertical),
        orientation_steps: Some(orientation_steps),
        warp: Some(crate::mask_generation::mask_warp(&js_adjustments)),
        bitmap_grid_of: Some(serde_json::json!({"width": size.0, "height": size.1})),
    })
}

/// The share of the open photo (0 … 1) each landscape part covers, by part
/// key, so the editor offers the parts the photo has (as Lightroom lists
/// only the parts it found). Runs the landscape model (downloading it on
/// first use: the user opened a Landscape mask's settings) or answers from
/// its cached run.
#[tauri::command]
pub async fn detect_landscape_parts(
    js_adjustments: serde_json::Value,
    path: String,
    task_id: Option<String>,
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<Vec<(String, f32)>, CommandError> {
    let ai_state = app_handle.state::<AiState>();
    let _guard = task_id
        .as_ref()
        .map(|id| AiTaskGuard::new(&ai_state.tasks, id.clone()));
    let cancel = _guard.as_ref().map(|g| &g.token);
    ai_state.lifecycle.touch();
    let run = landscape_run(
        &state,
        &ai_state,
        &app_handle,
        &js_adjustments,
        &path,
        true,
        cancel,
    )
    .await?;
    Ok(rawtonic_segment::LandscapePart::ALL
        .into_iter()
        .zip(rawtonic_segment::landscape_shares(&run.probabilities))
        .map(|(part, share)| (part.key().to_string(), share))
        .collect())
}

/// The landscape model's run on the open photo (`asked`) with the caller's
/// geometry: cached, else computed (downloading the model on first use) and
/// cached. Cancelling `cancel` stops a run of ONNX Runtime at once.
async fn landscape_run(
    state: &tauri::State<'_, AppState>,
    ai_state: &AiState,
    app_handle: &tauri::AppHandle,
    js_adjustments: &serde_json::Value,
    asked: &str,
    download: bool,
    cancel: Option<&Arc<AiTaskToken>>,
) -> Result<CachedLandscape, CommandError> {
    let cancelled = || cancel.is_some_and(|t| t.is_cancelled());
    let _running = ai_state.lifecycle.run();
    let mut _run = ai_state.landscape_lock.lock().await;
    let loaded = crate::image_loader::original_image(&state.source).await?;
    ensure_photo(&loaded.path, asked)?;
    let key = geometry_key(&loaded, js_adjustments);
    let cached = |ai_state: &AiState| {
        lock_recover(&ai_state.models)
            .as_ref()
            .and_then(|ai| ai.landscape_runs.iter().find(|r| r.key == key).cloned())
    };
    if let Some(run) = cached(ai_state) {
        return Ok(run);
    }
    if cancelled() {
        return Err(task_cancelled());
    }
    // As `auto_mask`: not beside an idle shader compile or an export where
    // they take turns, waited for without this kind's lock. Always a run
    // the photographer asked for (a Landscape mask added, pasted or shown),
    // whether or not it may download: a compile a frame waits for starts
    // beside a run made ahead only (`memory_turns`, DVL5 review r1).
    let _turn = if rawtonic_gpu::memory_turns::takes_turns() {
        drop(_run);
        let turn = crate::ai_lifecycle::turn(true).await;
        _run = ai_state.landscape_lock.lock().await;
        if let Some(run) = cached(ai_state) {
            return Ok(run);
        }
        if cancelled() {
            return Err(task_cancelled());
        }
        Some(turn)
    } else {
        None
    };
    let warp = async {
        crate::editor_preview::cached_full_warped_image(
            app_handle,
            loaded.image.clone(),
            loaded.is_raw,
            js_adjustments.clone(),
        )
        .await
    };
    let (model, warped) = tokio::join!(
        crate::ai_processing::get_or_init_landscape_model(app_handle, ai_state, download),
        warp
    );
    let model = model.map_err(CommandError::from)?.ok_or_else(|| {
        CommandError::new(
            ErrorCode::ModelsMissing,
            "The Landscape model is not installed: open a Landscape mask's settings to download it",
        )
    })?;
    let warped = warped?;
    if cancelled() {
        return Err(task_cancelled());
    }
    let options = Arc::new(ort::session::RunOptions::new().map_err(|e| e.to_string())?);
    let watcher = cancel.map(|token| {
        let (token, options) = (token.clone(), options.clone());
        tokio::spawn(async move {
            token.wait_for_cancel().await;
            let _ = options.terminate();
        })
    });
    let size = (warped.width(), warped.height());
    let is_raw = loaded.is_raw;
    let start = std::time::Instant::now();
    let result = tokio::task::spawn_blocking(move || {
        rawtonic_segment::landscape_probabilities(warped.as_ref(), &model, is_raw, Some(&options))
            .map_err(|e| e.to_string())
    })
    .await;
    if let Some(watcher) = watcher {
        watcher.abort();
    }
    if cancelled() {
        return Err(task_cancelled());
    }
    let probabilities = result.map_err(|e| e.to_string())??;
    log::info!(
        "AI landscape: {} {}x{} (raw {is_raw}) in {} ms",
        loaded.path,
        size.0,
        size.1,
        start.elapsed().as_millis()
    );
    let run = CachedLandscape {
        key,
        size,
        probabilities: Arc::new(probabilities),
    };
    let mut ai = lock_recover(&ai_state.models);
    let runs = &mut ai.get_or_insert_with(Default::default).landscape_runs;
    runs.retain(|r| r.key != run.key);
    if runs.len() >= AI_MASKS_KEPT {
        runs.remove(0);
    }
    runs.push(run.clone());
    Ok(run)
}

#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn generate_ai_depth_mask(
    js_adjustments: serde_json::Value,
    path: String,
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
) -> Result<AiDepthMaskParameters, CommandError> {
    let ai_state = app_handle.state::<AiState>();
    let _guard = task_id
        .as_ref()
        .map(|id| AiTaskGuard::new(&ai_state.tasks, id.clone()));
    let cancel_flag = _guard.as_ref().map(|g| &g.token);

    if cancel_flag.as_ref().is_some_and(|t| t.is_cancelled()) {
        return Err(task_cancelled());
    }

    ai_state.lifecycle.touch();
    let _running = ai_state.lifecycle.run();
    // Keyed by the photo actually loaded, taken before the model loads: a
    // photo switch meanwhile cannot file one photo's depth under another.
    let loaded = crate::image_loader::original_image(&state.source).await?;
    ensure_photo(&loaded.path, &path)?;
    let path_hash = geometry_key(&loaded, &js_adjustments);
    let cached = lock_recover(&ai_state.models)
        .as_ref()
        .and_then(|ai| ai.depth_map.clone())
        .filter(|cached| cached.path_hash == path_hash);
    // A depth map to compute: not beside an idle shader compile or an
    // export where they take turns, the model's load included.
    let _turn = match cached {
        Some(_) => None,
        None => Some(crate::ai_lifecycle::turn(true).await),
    };
    let depth_model = get_or_init_depth_model(&app_handle, &ai_state)
        .await
        .map_err(CommandError::from)?;

    if cancel_flag.as_ref().is_some_and(|t| t.is_cancelled()) {
        return Err(task_cancelled());
    }

    let cached_depth = match cached {
        Some(cached) => cached,
        None => {
            let warped_image = crate::editor_preview::cached_full_warped_image(
                &app_handle,
                loaded.image.clone(),
                loaded.is_raw,
                js_adjustments.clone(),
            )
            .await?;

            if cancel_flag.as_ref().is_some_and(|t| t.is_cancelled()) {
                return Err(task_cancelled());
            }

            let is_raw = loaded.is_raw;
            let model_input = warped_image.clone();
            let depth_img = tokio::task::spawn_blocking(move || {
                run_depth_anything_model(model_input.as_ref(), &depth_model, is_raw)
                    .map_err(|e| e.to_string())
            })
            .await
            .map_err(|e| e.to_string())??;

            let new_cache = CachedDepthMap {
                path_hash,
                depth_image: depth_img,
                original_size: (warped_image.width(), warped_image.height()),
            };
            // The state may be empty: an unload while the model loaded.
            lock_recover(&ai_state.models)
                .get_or_insert_with(Default::default)
                .depth_map = Some(new_cache.clone());
            new_cache
        }
    };

    if cancel_flag.as_ref().is_some_and(|t| t.is_cancelled()) {
        return Err(task_cancelled());
    }

    let base64_data = tokio::task::spawn_blocking(move || {
        let raw_depth_fullres = image::imageops::resize(
            &cached_depth.depth_image,
            cached_depth.original_size.0,
            cached_depth.original_size.1,
            image::imageops::FilterType::Triangle,
        );
        encode_to_base64_png(&raw_depth_fullres)
    })
    .await
    .map_err(|e| e.to_string())??;

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

/// One prompt of an Object selection as the canvas made it (the recipe's
/// `prompts`, in the turned photo's pixels): a click or a box; `exclude`
/// takes it out.
#[derive(serde::Deserialize, Debug, Clone, Copy)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ObjectPrompt {
    Point {
        x: f64,
        y: f64,
        #[serde(default)]
        exclude: bool,
    },
    #[serde(rename_all = "camelCase")]
    Box {
        start_x: f64,
        start_y: f64,
        end_x: f64,
        end_y: f64,
        #[serde(default)]
        exclude: bool,
    },
}

#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn generate_ai_subject_mask(
    js_adjustments: serde_json::Value,
    path: String,
    start_point: (f64, f64),
    end_point: (f64, f64),
    // Several prompts (Shift adds, Alt excludes); without them the box or
    // click of `start_point`/`end_point`, as recipes always had it.
    prompts: Option<Vec<ObjectPrompt>>,
    rotation: f32,
    flip_horizontal: bool,
    flip_vertical: bool,
    orientation_steps: u8,
    task_id: Option<String>,
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<AiSubjectMaskParameters, CommandError> {
    let ai_state = app_handle.state::<AiState>();
    let _guard = task_id
        .as_ref()
        .map(|id| AiTaskGuard::new(&ai_state.tasks, id.clone()));
    let cancel_flag = _guard.as_ref().map(|g| &g.token);

    if cancel_flag.as_ref().is_some_and(|t| t.is_cancelled()) {
        return Err(task_cancelled());
    }

    ai_state.lifecycle.touch();
    let _running = ai_state.lifecycle.run();
    let generation = ai_state.lifecycle.generation();
    let started = std::time::Instant::now();
    let _load_turn = sam_load_turn(&ai_state, true).await;
    let models = get_or_init_sam_models(&app_handle, &ai_state)
        .await
        .map_err(CommandError::from)?;

    if cancel_flag.as_ref().is_some_and(|t| t.is_cancelled()) {
        return Err(task_cancelled());
    }

    let (embeddings, warped_image, source_is_raw) = sam_embeddings(
        &state,
        &ai_state,
        &app_handle,
        &models,
        &js_adjustments,
        generation,
        cancel_flag,
        Some(&path),
        false,
    )
    .await?;
    let ready = started.elapsed().as_millis();

    if cancel_flag.as_ref().is_some_and(|t| t.is_cancelled()) {
        return Err(task_cancelled());
    }

    let (img_w, img_h) = embeddings.original_size;

    let (coarse_rotated_w, coarse_rotated_h) = if orientation_steps % 2 == 1 {
        (img_h as f64, img_w as f64)
    } else {
        (img_w as f64, img_h as f64)
    };

    let center = (coarse_rotated_w / 2.0, coarse_rotated_h / 2.0);

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

    let un_coarse_rotate = |p: (f64, f64)| -> (f64, f64) {
        match orientation_steps {
            0 => p,
            1 => (p.1, img_h as f64 - p.0),
            2 => (img_w as f64 - p.0, img_h as f64 - p.1),
            3 => (img_w as f64 - p.1, p.0),
            _ => p,
        }
    };

    // From the turned photo to the decoded one's pixels.
    let to_photo = |p: (f64, f64)| un_coarse_rotate(unflip(unrotate(p)));
    // A box's corners there: the turned box's four corners' bounds.
    let photo_box = |start: (f64, f64), end: (f64, f64)| {
        let ucrp1 = to_photo(start);
        let ucrp2 = to_photo((start.0, end.1));
        let ucrp3 = to_photo(end);
        let ucrp4 = to_photo((end.0, start.1));
        let min_x = ucrp1.0.min(ucrp2.0).min(ucrp3.0).min(ucrp4.0);
        let min_y = ucrp1.1.min(ucrp2.1).min(ucrp3.1).min(ucrp4.1);
        let max_x = ucrp1.0.max(ucrp2.0).max(ucrp3.0).max(ucrp4.0);
        let max_y = ucrp1.1.max(ucrp2.1).max(ucrp3.1).max(ucrp4.1);
        ((min_x, min_y), (max_x, max_y))
    };

    let decode_start = std::time::Instant::now();
    let mask_bitmap = match prompts.as_deref() {
        Some(prompts) if !prompts.is_empty() => {
            let prompts: Vec<SamPrompt> = prompts
                .iter()
                .map(|prompt| match *prompt {
                    ObjectPrompt::Point { x, y, exclude } => SamPrompt::Point {
                        at: to_photo((x, y)),
                        exclude,
                    },
                    ObjectPrompt::Box {
                        start_x,
                        start_y,
                        end_x,
                        end_y,
                        exclude,
                    } => {
                        let (min, max) = photo_box((start_x, start_y), (end_x, end_y));
                        SamPrompt::Box { min, max, exclude }
                    }
                })
                .collect();
            run_sam_decoder_prompts(
                &models.decoder,
                &embeddings,
                &prompts,
                Some(warped_image.as_ref()),
                source_is_raw,
            )
        }
        _ => {
            let (unrotated_start_point, unrotated_end_point) = photo_box(start_point, end_point);
            run_sam_decoder(
                &models.decoder,
                &embeddings,
                unrotated_start_point,
                unrotated_end_point,
                Some(warped_image.as_ref()),
                source_is_raw,
            )
        }
    }
    .map_err(|e| e.to_string())?;
    let mask_ms = decode_start.elapsed().as_millis();

    if cancel_flag.as_ref().is_some_and(|t| t.is_cancelled()) {
        return Err(task_cancelled());
    }

    let png_start = std::time::Instant::now();
    let base64_data = encode_to_base64_png(&mask_bitmap)?;
    log::info!(
        "AI object mask: {path} {img_w}x{img_h} (raw {source_is_raw}), {} prompts: models and embedding ready after {ready} ms, mask {mask_ms} ms, PNG {} ms, total {} ms",
        prompts.as_ref().map_or(1, Vec::len),
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

/// Key of a photo's development with its geometry (the adjustments that
/// move pixels): what is computed for one is not used for another, nor for
/// the photo developed again (`LoadedImage::source_generation`).
pub(crate) fn geometry_key(
    loaded: &crate::state::source::LoadedImage,
    js_adjustments: &serde_json::Value,
) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(loaded.path.as_bytes());
    hasher.update(&loaded.source_generation().to_le_bytes());
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

/// Waits for `lock`; a request cancelled while it waited does not start.
async fn wait_turn<'a>(
    lock: &'a tokio::sync::Mutex<()>,
    cancel: Option<&Arc<AiTaskToken>>,
) -> Result<tokio::sync::MutexGuard<'a, ()>, CommandError> {
    let turn = lock.lock().await;
    if cancel.is_some_and(|t| t.is_cancelled()) {
        return Err(task_cancelled());
    }
    Ok(turn)
}

/// SAM's embedding of the current photo with its geometry (cached per photo
/// and geometry), the warped image it was computed from and whether that is
/// a linear RAW decode. `generation` is `AiState::lifecycle`'s generation from the
/// start of the request: an embedding whose request overlapped an unload
/// is used once, not kept. Cancelling `cancel` stops a queued request and
/// the encoder's ONNX Runtime run.
/// The turn the SAM models' load takes beside idle shader compiles and
/// exports (`ai_lifecycle::turn`), held by the caller through its run;
/// `None` when they are loaded (the embedding's run takes its own).
async fn sam_load_turn(
    ai_state: &AiState,
    asked: bool,
) -> Option<rawtonic_gpu::memory_turns::AiRunTurn> {
    let loaded = lock_recover(&ai_state.models)
        .as_ref()
        .is_some_and(|ai| ai.sam_models.is_some());
    if loaded {
        None
    } else {
        Some(crate::ai_lifecycle::turn(asked).await)
    }
}

/// `ahead`: computed ahead of a click (a photo opened), so it waits for an
/// idle shader compile where they take turns instead of stopping it.
#[allow(clippy::too_many_arguments)]
async fn sam_embeddings(
    state: &tauri::State<'_, AppState>,
    ai_state: &AiState,
    app_handle: &tauri::AppHandle,
    models: &Arc<SamModels>,
    js_adjustments: &serde_json::Value,
    generation: u64,
    cancel: Option<&Arc<AiTaskToken>>,
    asked: Option<&str>,
    ahead: bool,
) -> Result<(ImageEmbeddings, Arc<image::DynamicImage>, bool), CommandError> {
    let mut _encode = wait_turn(&ai_state.sam_encode_lock, cancel).await?;
    // Keyed by the photo actually loaded, not the caller's path: a
    // background encode that outlives a photo switch cannot file the new
    // photo's embedding under the old one. A selection asked for one photo
    // is never made on another (`PHOTO_CHANGED`).
    let loaded = crate::image_loader::original_image(&state.source).await?;
    if let Some(asked) = asked {
        ensure_photo(&loaded.path, asked)?;
    }
    let path_hash = geometry_key(&loaded, js_adjustments);
    let (mask_source, source_is_raw) = (loaded.image, loaded.is_raw);
    let cached = || {
        lock_recover(&ai_state.models)
            .as_ref()
            .and_then(|ai| ai.embeddings.clone())
            .filter(|e| e.path_hash == path_hash)
    };
    let warped_image = crate::editor_preview::cached_full_warped_image(
        app_handle,
        mask_source,
        source_is_raw,
        js_adjustments.clone(),
    )
    .await?;
    if let Some(embeddings) = cached() {
        return Ok((embeddings, warped_image, source_is_raw));
    }
    if cancel.is_some_and(|t| t.is_cancelled()) {
        return Err(task_cancelled());
    }
    // As `auto_mask`: the wait for a turn is without the encode lock.
    let _turn = if rawtonic_gpu::memory_turns::takes_turns() {
        drop(_encode);
        let turn = crate::ai_lifecycle::turn(!ahead).await;
        _encode = wait_turn(&ai_state.sam_encode_lock, cancel).await?;
        if let Some(embeddings) = cached() {
            return Ok((embeddings, warped_image, source_is_raw));
        }
        if cancel.is_some_and(|t| t.is_cancelled()) {
            return Err(task_cancelled());
        }
        Some(turn)
    } else {
        None
    };
    let options = Arc::new(ort::session::RunOptions::new().map_err(|e| e.to_string())?);
    let watcher = cancel.map(|token| {
        let (token, options) = (token.clone(), options.clone());
        tokio::spawn(async move {
            token.wait_for_cancel().await;
            let _ = options.terminate();
        })
    });
    let models = models.clone();
    let image = warped_image.clone();
    let run_options = options.clone();
    let result = tokio::task::spawn_blocking(move || {
        generate_image_embeddings(
            image.as_ref(),
            &models.encoder,
            source_is_raw,
            Some(&run_options),
        )
    })
    .await;
    if let Some(watcher) = watcher {
        watcher.abort();
    }
    if cancel.is_some_and(|t| t.is_cancelled()) {
        return Err(task_cancelled());
    }
    let mut embeddings = result
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
    embeddings.path_hash = path_hash;
    // An encoding whose request overlapped an unload is used once, not kept.
    let mut ai = lock_recover(&ai_state.models);
    if ai_state.lifecycle.keep(generation) {
        ai.get_or_insert_with(Default::default).embeddings = Some(embeddings.clone());
    }
    drop(ai);
    Ok((embeddings, warped_image, source_is_raw))
}

/// Whether the loaded photo's SAM encoding with `js_adjustments`' geometry
/// is kept (`sam_embeddings` answers it without a run).
fn sam_encoding_cached(
    state: &AppState,
    ai_state: &AiState,
    js_adjustments: &serde_json::Value,
) -> bool {
    let Some(loaded) = lock_recover(&state.source.original_image).clone() else {
        return false;
    };
    let path_hash = geometry_key(&loaded, js_adjustments);
    lock_recover(&ai_state.models)
        .as_ref()
        .and_then(|ai| ai.embeddings.as_ref())
        .is_some_and(|e| e.path_hash == path_hash)
}

#[tauri::command]
pub async fn precompute_ai_subject_mask(
    js_adjustments: serde_json::Value,
    path: String,
    background: Option<bool>,
    task_id: Option<String>,
    asked: Option<bool>,
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), CommandError> {
    // Before any wait: opening the masking tools meanwhile starts it.
    let asked_at = crate::ai_ahead::asked_count();
    let ai_state = app_handle.state::<AiState>();
    let _running = ai_state.lifecycle.run();
    let generation = ai_state.lifecycle.generation();
    // `task_id` cancels it (a photo switch, the Library, memory pressure).
    let _guard = task_id
        .as_ref()
        .map(|id| AiTaskGuard::new(&ai_state.tasks, id.clone()));
    let cancelled = || _guard.as_ref().is_some_and(|g| g.token.is_cancelled());
    // The models' load takes a turn as their run does (`sam_load_turn`).
    let mut load_turn = None;
    // Held while an encoding made ahead runs (`ai_ahead::running`).
    let mut _ahead = None;
    // `background`: asked for by opening a photo, not by the user: only
    // with a model that is already installed (no download), and while the
    // system is not short of memory (as `precompute`).
    if background == Some(true) {
        if crate::memory_pressure::under_pressure() {
            log::info!("AI object mask: not encoded ahead, the system is short of memory");
            return Ok(());
        }
        if cancelled() {
            log::info!("AI object mask: not encoded ahead, the task was cancelled");
            return Ok(());
        }
        // As Subject and Sky made ahead (`ai_ahead`): after the decodes,
        // with memory for the encoder and its run, unless the photographer
        // opened the masking tools (`asked`).
        if asked != Some(true) && !sam_encoding_cached(&state, &ai_state, &js_adjustments) {
            let job = crate::ai_ahead::AheadJob::ObjectEncoding;
            let loaded = lock_recover(&ai_state.models)
                .as_ref()
                .is_some_and(|ai| ai.sam_models.is_some());
            let need = crate::ai_ahead::estimated_peak(
                job,
                rawtonic_onnx::models_run_on_cpu_only(),
                loaded,
            );
            let asked = || crate::ai_ahead::asked_since(asked_at, task_id.as_deref());
            let admitted = crate::ai_ahead::admit(job, asked, need, || {
                if cancelled() {
                    return Some("the task was cancelled");
                }
                ahead_photo_skip(current_photo(&state.source).as_deref(), &path)
            })
            .await;
            match admitted {
                Ok(run) => _ahead = Some(run),
                Err(why) => {
                    log::info!("AI object mask: not encoded ahead, {why:?}");
                    return Ok(());
                }
            }
        }
        load_turn = sam_load_turn(&ai_state, false).await;
        crate::ai_processing::preload_sam_models(&app_handle, &ai_state)
            .await
            .map_err(|e| e.to_string())?;
        let loaded = lock_recover(&ai_state.models)
            .as_ref()
            .is_some_and(|ai| ai.sam_models.is_some());
        if !loaded {
            return Ok(());
        }
    }
    if load_turn.is_none() {
        load_turn = sam_load_turn(&ai_state, true).await;
    }
    let _load_turn = load_turn;
    let models = get_or_init_sam_models(&app_handle, &ai_state)
        .await
        .map_err(CommandError::from)?;
    // A request for a photo that is no longer open is dropped.
    let current = crate::image_loader::original_image(&state.source)
        .await
        .ok()
        .map(|l| l.path);
    if let Some(reason) = ahead_photo_skip(current.as_deref(), &path) {
        log::info!("AI object mask: not encoded ahead, {reason}");
        return Ok(());
    }
    if cancelled() {
        log::info!("AI object mask: not encoded ahead, the task was cancelled");
        return Ok(());
    }
    let start = std::time::Instant::now();
    let cancel = _guard.as_ref().map(|g| &g.token);
    match sam_embeddings(
        &state,
        &ai_state,
        &app_handle,
        &models,
        &js_adjustments,
        generation,
        cancel,
        Some(&path),
        background == Some(true),
    )
    .await
    {
        Ok(_) => log::info!(
            "AI object mask: background embedding of {path} ready after {} ms",
            start.elapsed().as_millis()
        ),
        Err(_) if cancelled() => log::info!(
            "AI object mask: background encoding of {path} cancelled after {} ms",
            start.elapsed().as_millis()
        ),
        Err(e) => return Err(e),
    }
    Ok(())
}

/// The automatic selections that are computed ahead and cached per photo
/// and geometry: Subject/Background (BiRefNet) and Sky (UperNet).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AutoMask {
    Subject,
    Sky,
}

impl AutoMask {
    fn name(self) -> &'static str {
        match self {
            Self::Subject => "subject",
            Self::Sky => "sky",
        }
    }

    /// Serialises this kind's runs (`AiState::subject_lock`/`sky_lock`).
    fn lock(self, ai_state: &AiState) -> &tokio::sync::Mutex<()> {
        match self {
            Self::Subject => &ai_state.subject_lock,
            Self::Sky => &ai_state.sky_lock,
        }
    }

    fn cache(self, ai: &mut crate::ai_processing::AiModels) -> &mut Vec<CachedMask> {
        match self {
            Self::Subject => &mut ai.subject_masks,
            Self::Sky => &mut ai.sky_masks,
        }
    }
}

fn cached_mask(ai_state: &AiState, kind: AutoMask, key: &str) -> Option<Arc<String>> {
    let mut ai = lock_recover(&ai_state.models);
    kind.cache(ai.as_mut()?)
        .iter()
        .find(|m| m.key == key)
        .map(|m| m.mask_data_base64.clone())
}

fn store_mask(ai_state: &AiState, kind: AutoMask, key: String, data: Arc<String>) {
    let mut ai = lock_recover(&ai_state.models);
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

#[derive(Clone)]
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

    /// One run on a constant input (Core ML only); `false` when not run.
    fn warm_up(&self) -> anyhow::Result<bool> {
        match self {
            Self::Subject(model) => model.warm_up(),
            Self::Sky(model) => model.warm_up(),
        }
    }
}

/// Whether the network for `kind` is loaded.
fn model_loaded(ai_state: &AiState, kind: AutoMask) -> bool {
    lock_recover(&ai_state.models)
        .as_ref()
        .is_some_and(|ai| match kind {
            AutoMask::Subject => ai.subject_model.is_some(),
            AutoMask::Sky => ai.sky_model.is_some(),
        })
}

/// The loaded network for `kind`; with `download` false only an installed
/// model (`None` without one).
async fn network(
    ai_state: &AiState,
    app_handle: &tauri::AppHandle,
    kind: AutoMask,
    download: bool,
) -> Result<Option<Network>, CommandError> {
    Ok(match kind {
        AutoMask::Subject => get_or_init_subject_model(app_handle, ai_state, download)
            .await
            .map_err(CommandError::from)?
            .map(Network::Subject),
        AutoMask::Sky => get_or_init_sky_model(app_handle, ai_state, download)
            .await
            .map_err(CommandError::from)?
            .map(Network::Sky),
    })
}

/// The Subject or Sky mask (PNG data URL at the photo's size) of the photo
/// that is loaded, with the caller's geometry: cached per photo and
/// geometry, else computed and cached. `asked`: the photo a click asked
/// for (`PHOTO_CHANGED` when another is loaded); `None` for background
/// requests. `download`: the model may be downloaded (a click); otherwise
/// only an installed model is used (`Ok(None)` without one: background
/// requests, a camera TIFF's migrated selection computed again on opening).
/// Cancelling `cancel` stops a run of ONNX Runtime at once.
#[allow(clippy::too_many_arguments)]
async fn auto_mask(
    state: &tauri::State<'_, AppState>,
    ai_state: &AiState,
    app_handle: &tauri::AppHandle,
    js_adjustments: &serde_json::Value,
    kind: AutoMask,
    subject_version: u32,
    asked: Option<&str>,
    download: bool,
    cancel: Option<&Arc<AiTaskToken>>,
) -> Result<Option<Arc<String>>, CommandError> {
    let cancelled = || cancel.is_some_and(|t| t.is_cancelled());
    // Counts as running from here, so an idle unload waits for the load too.
    let _running = ai_state.lifecycle.run();
    let mut _run = kind.lock(ai_state).lock().await;
    // Keyed by the photo actually loaded, not the caller's path: a run that
    // outlives a photo switch cannot file one photo's mask under another.
    let loaded = crate::image_loader::original_image(&state.source).await?;
    if let Some(asked) = asked {
        ensure_photo(&loaded.path, asked)?;
    }
    let mut key = geometry_key(&loaded, js_adjustments);
    if kind == AutoMask::Subject {
        // Each Select Subject version is its own mask.
        key.push_str(&format!(":v{subject_version}"));
    }
    if let Some(mask) = cached_mask(ai_state, kind, &key) {
        return Ok(Some(mask));
    }
    if cancelled() {
        return Err(task_cancelled());
    }
    // Not beside an idle shader compile or an export where they take turns
    // (model load included); a cached mask above needs none. The wait is
    // without this kind's lock: a click must not queue behind a run made
    // ahead that waits for a compile (the click stops that compile).
    // Asked for is a request for the open photo (`asked`), whether or not
    // it may download (a pasted mask uses only an installed model); only
    // background requests are runs made ahead, which a compile a frame
    // waits for does not wait for (`memory_turns`, DVL5 review r1).
    let _turn = if rawtonic_gpu::memory_turns::takes_turns() {
        drop(_run);
        let turn = crate::ai_lifecycle::turn(asked.is_some()).await;
        _run = kind.lock(ai_state).lock().await;
        if let Some(mask) = cached_mask(ai_state, kind, &key) {
            return Ok(Some(mask));
        }
        if cancelled() {
            return Err(task_cancelled());
        }
        Some(turn)
    } else {
        None
    };
    // The photo's warped image is made while a model that is not loaded yet
    // loads (Core ML loads on a worker thread): a first click waits for the
    // longer of the two instead of both. Only when the model will be there:
    // asked for, or installed.
    let warp = || async {
        let start = std::time::Instant::now();
        let image = crate::editor_preview::cached_full_warped_image(
            app_handle,
            loaded.image.clone(),
            loaded.is_raw,
            js_adjustments.clone(),
        )
        .await?;
        Ok::<_, String>((image, start.elapsed().as_millis()))
    };
    let (network, warped) = if !model_loaded(ai_state, kind)
        && (download || mask_model_installed(app_handle, kind == AutoMask::Sky))
    {
        let (network, warped) = tokio::join!(network(ai_state, app_handle, kind, download), warp());
        (network?, Some(warped?))
    } else {
        (network(ai_state, app_handle, kind, download).await?, None)
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
        return Err(task_cancelled());
    }
    let start = std::time::Instant::now();
    let (warped_image, warped_ms) = match warped {
        Some(warped) => warped,
        None => warp().await?,
    };
    let (width, height) = (warped_image.width(), warped_image.height());
    // A background run on the CPU peaks 2.2-2.5 GB above the model: what
    // rebuilds on demand goes first (AIMEM: 5699 -> 5580 MB on a Mac mini).
    // A click's run keeps the caches the next edit uses; so does this one for
    // the warped image: the run holds it anyway, so releasing the cache's
    // reference saves nothing, and the next mask (Sky, SAM) rebuilds it.
    if !download && rawtonic_onnx::models_run_on_cpu_only() {
        crate::memory_pressure::release_rebuildable_memory_keeping(app_handle, true);
    }
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
                subject_selection(
                    warped_image.as_ref(),
                    model,
                    is_raw,
                    run_options.as_deref(),
                    subject_version,
                ),
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
        return Err(task_cancelled());
    }
    let (data, model_ms, backend) = result.map_err(|e| e.to_string())??;
    log::info!(
        "AI {} mask: {} {width}x{height} (raw {is_raw}) on {backend}: warped image {warped_ms} ms{during_load}, model {model_ms} ms, total {} ms",
        kind.name(),
        loaded.path,
        start.elapsed().as_millis()
    );
    let data = Arc::new(data);
    store_mask(ai_state, kind, key, data.clone());
    Ok(Some(data))
}

/// The automatic selections an export, a collage or the selection queue
/// computes for a photo that was never opened (`ai_selections::
/// detect_missing`), with the app's models as the editor runs them: an
/// installed model only (nothing is downloaded; one that is not installed
/// fails that photo, naming the way to get it), one run of each kind at a
/// time, kept from an idle unload while it runs. Called from worker
/// threads, outside the async runtime.
pub(crate) struct ExportSelections<'a> {
    pub app_handle: &'a tauri::AppHandle,
}

/// The error of a model that is not installed for a selection.
fn not_installed(what: &str) -> String {
    format!(
        "the {what} model is not installed (open the photo in Develop once to download it, then try again)"
    )
}

impl rawtonic_engine::ai_selections::SelectionDetector for ExportSelections<'_> {
    fn detect(
        &self,
        selection: rawtonic_engine::ai_selections::AutoSelection,
        parameters: &serde_json::Value,
        warped: &image::DynamicImage,
        linear: bool,
    ) -> Result<String, String> {
        use rawtonic_engine::ai_selections::{self as sel, AutoSelection};
        let ai_state = self.app_handle.state::<AiState>();
        ai_state.lifecycle.touch();
        let _running = ai_state.lifecycle.run();
        let started = std::time::Instant::now();
        let logged = |data: String| {
            log::info!(
                "AI {} mask for a closed photo: {}x{} in {} ms",
                selection.name(),
                warped.width(),
                warped.height(),
                started.elapsed().as_millis()
            );
            Ok(data)
        };
        let kind = match selection {
            AutoSelection::Sky => AutoMask::Sky,
            AutoSelection::Subject => AutoMask::Subject,
            AutoSelection::People => {
                let models =
                    tauri::async_runtime::block_on(crate::ai_faces::models(self.app_handle, false))
                        .map_err(|e| {
                            if e.message == crate::ai_faces::FACE_MODELS_MISSING {
                                not_installed("face")
                            } else {
                                e.message
                            }
                        })?;
                let mask = rawtonic_faces::recipe::people_selection(
                    warped,
                    linear,
                    &models,
                    &sel::people_parts(parameters),
                    sel::people_person(parameters),
                )?;
                return encode_to_base64_png(&mask).and_then(logged);
            }
            AutoSelection::Depth => {
                if !crate::ai_processing::depth_model_installed(self.app_handle) {
                    return Err(not_installed("Depth"));
                }
                let model = tauri::async_runtime::block_on(
                    crate::ai_processing::get_or_init_depth_model(self.app_handle, &ai_state),
                )
                .map_err(|e| e.to_string())?;
                // As the editor stores it (`generate_ai_depth_mask`): the
                // network's depth at the warped photo's size.
                let depth =
                    run_depth_anything_model(warped, &model, linear).map_err(|e| e.to_string())?;
                let full = image::imageops::resize(
                    &depth,
                    warped.width(),
                    warped.height(),
                    image::imageops::FilterType::Triangle,
                );
                return encode_to_base64_png(&full).and_then(logged);
            }
            AutoSelection::Landscape => {
                let parts = sel::landscape_parts(parameters)
                    .ok_or_else(|| "a Landscape selection without known parts".to_string())?;
                return self
                    .detect_landscape(&[parts], warped, linear)
                    .map(|mut masks| masks.remove(0));
            }
        };
        let _run = kind.lock(&ai_state).blocking_lock();
        let network =
            tauri::async_runtime::block_on(network(&ai_state, self.app_handle, kind, false))?
                .ok_or_else(|| not_installed(selection.name()))?;
        let version = sel::subject_version(parameters);
        let mask = match &network {
            Network::Subject(model) => subject_selection(warped, model, linear, None, version),
            Network::Sky(model) => run_sky_seg_model(warped, model, linear, None),
        }
        .map_err(|e| e.to_string())?;
        encode_to_base64_png(&mask).and_then(logged)
    }

    fn detect_landscape(
        &self,
        sets: &[rawtonic_engine::ai_selections::LandscapeParts],
        warped: &image::DynamicImage,
        linear: bool,
    ) -> Result<Vec<String>, String> {
        let ai_state = self.app_handle.state::<AiState>();
        let sets = sets
            .iter()
            .map(|set| {
                rawtonic_segment::landscape_parts(&set.keys()).map(|parts| (parts, set.version))
            })
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        ai_state.lifecycle.touch();
        let _running = ai_state.lifecycle.run();
        let _run = ai_state.landscape_lock.blocking_lock();
        let model = tauri::async_runtime::block_on(
            crate::ai_processing::get_or_init_landscape_model(self.app_handle, &ai_state, false),
        )
        .map_err(|e| e.to_string())?
        .ok_or_else(|| {
            "the Landscape model is not installed (open a Landscape mask's settings in Develop once to download it, then export again)".to_string()
        })?;
        let started = std::time::Instant::now();
        let probabilities = rawtonic_segment::landscape_probabilities(warped, &model, linear, None)
            .map_err(|e| e.to_string())?;
        log::info!(
            "AI landscape for an export: {}x{} in {} ms",
            warped.width(),
            warped.height(),
            started.elapsed().as_millis()
        );
        sets.iter()
            .map(|(parts, version)| {
                encode_to_base64_png(&rawtonic_segment::landscape_mask_grid(
                    &probabilities,
                    parts,
                    *version,
                ))
            })
            .collect()
    }
}

#[allow(clippy::too_many_arguments)]
async fn subject_mask(
    state: &tauri::State<'_, AppState>,
    ai_state: &AiState,
    app_handle: &tauri::AppHandle,
    js_adjustments: &serde_json::Value,
    version: u32,
    asked: &str,
    download: bool,
    cancel: Option<&Arc<AiTaskToken>>,
) -> Result<Option<Arc<String>>, CommandError> {
    auto_mask(
        state,
        ai_state,
        app_handle,
        js_adjustments,
        AutoMask::Subject,
        version,
        Some(asked),
        download,
        cancel,
    )
    .await
}

async fn sky_mask(
    state: &tauri::State<'_, AppState>,
    ai_state: &AiState,
    app_handle: &tauri::AppHandle,
    js_adjustments: &serde_json::Value,
    asked: &str,
    download: bool,
    cancel: Option<&Arc<AiTaskToken>>,
) -> Result<Option<Arc<String>>, CommandError> {
    auto_mask(
        state,
        ai_state,
        app_handle,
        js_adjustments,
        AutoMask::Sky,
        1,
        Some(asked),
        download,
        cancel,
    )
    .await
}

fn ahead_photo_skip(current: Option<&str>, requested: &str) -> Option<&'static str> {
    match current {
        None => Some("the photo is not ready"),
        Some(current) if current != requested => Some("the photo has changed"),
        Some(_) => None,
    }
}

/// What a mask made ahead (`precompute`) came to, for the webview.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum AheadMask {
    /// The mask is there (computed or cached).
    Ready,
    /// Not computed, and asking again for this photo would not change that:
    /// its model is not installed, the photo changed, the run was cancelled.
    Unavailable,
    /// Not computed for now: held too long for the photo's decodes or the
    /// memory, or the system was short of memory. Opening the masking tools
    /// asks for it again.
    Dropped,
}

/// The path of the photo whose full decode is loaded, without waiting.
fn current_photo(source: &crate::state::source::SourceState) -> Option<String> {
    lock_recover(&source.original_image)
        .as_ref()
        .map(|loaded| loaded.path.clone())
}

/// Whether the loaded photo's `kind` mask with `js_adjustments`' geometry is
/// cached (`auto_mask` answers it without a model).
fn auto_mask_cached(
    state: &AppState,
    ai_state: &AiState,
    kind: AutoMask,
    js_adjustments: &serde_json::Value,
    subject_version: u32,
) -> bool {
    let Some(loaded) = lock_recover(&state.source.original_image).clone() else {
        return false;
    };
    let mut key = geometry_key(&loaded, js_adjustments);
    if kind == AutoMask::Subject {
        key.push_str(&format!(":v{subject_version}"));
    }
    cached_mask(ai_state, kind, &key).is_some()
}

/// The photographer opened the masking tools while the masks made ahead for
/// the open photo wait for their start (`ai_ahead`): they start now, also
/// `task_id` (the run's current task) if its command has not reached its
/// admission yet.
#[tauri::command]
pub async fn admit_ai_ahead_work(task_id: Option<String>) {
    crate::ai_ahead::ask_now(task_id);
}

/// Computes the open photo's Subject or Sky mask in the background, so the
/// selection answers at once. Only with a model that is already installed
/// (nothing is downloaded) and while the system is not short of memory;
/// dropped when the photo is no longer open; `task_id` cancels it (a photo
/// switch).
#[allow(clippy::too_many_arguments)]
async fn precompute(
    kind: AutoMask,
    js_adjustments: serde_json::Value,
    path: String,
    task_id: Option<String>,
    asked: bool,
    state: tauri::State<'_, AppState>,
    ai_state: tauri::State<'_, AiState>,
    app_handle: tauri::AppHandle,
) -> Result<AheadMask, String> {
    // Before any wait: opening the masking tools meanwhile starts it.
    let asked_at = crate::ai_ahead::asked_count();
    let _guard = task_id
        .as_ref()
        .map(|id| AiTaskGuard::new(&ai_state.tasks, id.clone()));
    let cancel = _guard.as_ref().map(|g| &g.token);
    let current = crate::image_loader::original_image(&state.source)
        .await
        .ok()
        .map(|l| l.path);
    if let Some(reason) = ahead_photo_skip(current.as_deref(), &path) {
        log::info!("AI {} mask: not computed ahead, {reason}", kind.name());
        return Ok(AheadMask::Unavailable);
    }
    // Ahead of a click only with memory to spare: a run holds about 1.6 GB
    // on Core ML (and loads the model if it is not loaded).
    if crate::memory_pressure::under_pressure() {
        log::info!(
            "AI {} mask: not computed ahead, the system is short of memory",
            kind.name()
        );
        return Ok(AheadMask::Dropped);
    }
    // Ahead of a click on a new selection: the current Subject version.
    let version = rawtonic_segment::SUBJECT_VERSION;
    // Not beside the full-resolution decodes, and only with memory for its
    // peak (`ai_ahead`); a mask computed already needs neither. Opening the
    // masking tools (`asked`) starts it at once.
    let mut _ahead = None;
    if !asked && !auto_mask_cached(&state, &ai_state, kind, &js_adjustments, version) {
        let job = match kind {
            AutoMask::Subject => crate::ai_ahead::AheadJob::Subject,
            AutoMask::Sky => crate::ai_ahead::AheadJob::Sky,
        };
        let need = crate::ai_ahead::estimated_peak(
            job,
            rawtonic_onnx::models_run_on_cpu_only(),
            model_loaded(&ai_state, kind),
        );
        let asked = || crate::ai_ahead::asked_since(asked_at, task_id.as_deref());
        let admitted = crate::ai_ahead::admit(job, asked, need, || {
            if cancel.is_some_and(|t| t.is_cancelled()) {
                return Some("the task was cancelled");
            }
            ahead_photo_skip(current_photo(&state.source).as_deref(), &path)
        })
        .await;
        match admitted {
            Ok(run) => _ahead = Some(run),
            Err(why) => {
                log::info!("AI {} mask: not computed ahead, {why:?}", kind.name());
                return Ok(match why {
                    crate::ai_ahead::NotStarted::TimedOut(_) => AheadMask::Dropped,
                    crate::ai_ahead::NotStarted::GaveUp(_) => AheadMask::Unavailable,
                });
            }
        }
    }
    let start = std::time::Instant::now();
    match auto_mask(
        &state,
        &ai_state,
        &app_handle,
        &js_adjustments,
        kind,
        version,
        None,
        false,
        cancel,
    )
    .await
    {
        Ok(Some(_)) => {
            log::info!(
                "AI {} mask: background mask of {path} ready after {} ms",
                kind.name(),
                start.elapsed().as_millis()
            );
            Ok(AheadMask::Ready)
        }
        Ok(None) => Ok(AheadMask::Unavailable),
        Err(_) if cancel.is_some_and(|t| t.is_cancelled()) => {
            log::info!(
                "AI {} mask: background run of {path} cancelled after {} ms",
                kind.name(),
                start.elapsed().as_millis()
            );
            Ok(AheadMask::Unavailable)
        }
        Err(e) => {
            log::warn!("AI {} mask: ahead computation failed: {e}", kind.name());
            Err(e.into())
        }
    }
}

/// Subject/Background ahead of the click (`precompute`): asked for when the
/// masking tools are open, or when a photo opens after they were used.
/// What it came to (`AheadMask`): a dropped one is asked for again when the
/// masking tools open.
#[tauri::command]
pub async fn precompute_ai_foreground_mask(
    js_adjustments: serde_json::Value,
    path: String,
    task_id: Option<String>,
    asked: Option<bool>,
    state: tauri::State<'_, AppState>,
    ai_state: tauri::State<'_, AiState>,
    app_handle: tauri::AppHandle,
) -> Result<AheadMask, String> {
    precompute(
        AutoMask::Subject,
        js_adjustments,
        path,
        task_id,
        asked == Some(true),
        state,
        ai_state,
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
    asked: Option<bool>,
    state: tauri::State<'_, AppState>,
    ai_state: tauri::State<'_, AiState>,
    app_handle: tauri::AppHandle,
) -> Result<AheadMask, String> {
    precompute(
        AutoMask::Sky,
        js_adjustments,
        path,
        task_id,
        asked == Some(true),
        state,
        ai_state,
        app_handle,
    )
    .await
}

/// `preload_ai_mask_models`: a model that waited for its start was loaded
/// meanwhile (by the photo's mask made ahead).
const LOADED_MEANWHILE: &str = "the model was loaded meanwhile";

/// The models `preload_ai_mask_models` loads: both, or on the CPU only
/// Subject. On the CPU a model's weights and the run's transient add up in
/// the app's own memory (Subject settles at about 1 GB with a 2.2-2.5 GB run
/// on top); Sky next to it was 0.2-0.33 GB of the peak (5912 against 5699
/// MB on a Mac mini), and it loads in 0.14-0.37 s on first use (AIMEM,
/// 2026-11-14).
fn preload_kinds(on_cpu: bool) -> &'static [AutoMask] {
    if on_cpu {
        &[AutoMask::Subject]
    } else {
        &[AutoMask::Subject, AutoMask::Sky]
    }
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
    ai_state: tauri::State<'_, AiState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    if crate::memory_pressure::under_pressure() {
        log::info!("AI masks: models not loaded ahead, the system is short of memory");
        return Ok(());
    }
    // Before any wait: an unload meanwhile (the Library) stops it. Never
    // asked for: opening the masking tools starts the photo's masks, which
    // load the model themselves.
    let generation = ai_state.lifecycle.generation();
    let _running = ai_state.lifecycle.run();
    let on_cpu = rawtonic_onnx::models_run_on_cpu_only();
    if on_cpu {
        log::info!("AI sky mask: model not loaded ahead, AI is running on the CPU");
    }
    // Asked for as Develop opens: the photo's full decode first (it waits
    // up to a few seconds; a longer decode holds the admission below).
    let _ = crate::image_loader::original_image(&state.source).await;
    for &kind in preload_kinds(on_cpu) {
        let name = kind.name();
        if model_loaded(&ai_state, kind) {
            continue;
        }
        // A model is loaded ahead for the run after it (`precompute`): not
        // beside the full-resolution decodes, and only with memory for both
        // (`ai_ahead`).
        let job = match kind {
            AutoMask::Subject => crate::ai_ahead::AheadJob::Subject,
            AutoMask::Sky => crate::ai_ahead::AheadJob::Sky,
        };
        let need = crate::ai_ahead::estimated_peak(job, on_cpu, false);
        let admitted = crate::ai_ahead::admit(
            job,
            || false,
            need,
            || {
                if ai_state.lifecycle.generation() != generation {
                    return Some("the models were unloaded");
                }
                // The photo's mask made ahead loaded it meanwhile.
                model_loaded(&ai_state, kind).then_some(LOADED_MEANWHILE)
            },
        )
        .await;
        let _ahead = match admitted {
            Ok(run) => run,
            Err(crate::ai_ahead::NotStarted::GaveUp(LOADED_MEANWHILE)) => continue,
            Err(why) => {
                log::info!("AI {name} mask: model not loaded ahead, {why:?}");
                return Ok(());
            }
        };
        let _turn = crate::ai_lifecycle::turn(false).await;
        // Unloaded while it waited for its turn: nothing is loaded again.
        if ai_state.lifecycle.generation() != generation {
            log::info!("AI {name} mask: model not loaded ahead, the models were unloaded");
            return Ok(());
        }
        let start = std::time::Instant::now();
        let loaded = network(&ai_state, &app_handle, kind, false)
            .await
            .inspect_err(|e| log::warn!("AI {name} mask: ahead model load failed: {e}"))?;
        // Unloaded while the loader waited (it reads the generation once it
        // runs, after another load of the model ended): what it kept for
        // this preload goes again.
        if ai_state.lifecycle.generation() != generation {
            if let Some(network) = &loaded
                && let Some(ai) = lock_recover(&ai_state.models).as_mut()
            {
                release_network(ai, network);
            }
            log::info!("AI {name} mask: model not kept ahead, the models were unloaded");
            return Ok(());
        }
        if loaded.is_some() {
            log::info!(
                "AI {name} mask: model ready ahead after {} ms",
                start.elapsed().as_millis()
            );
        }
    }
    Ok(())
}

/// Builds Core ML's GPU kernels for Subject, then Sky, once per app and
/// macOS build, so the first click on Subject no longer waits for them: on
/// a fresh install Metal compiles them on the model's first run (Subject
/// 11-14 s, Sky 7 s on an M5 Pro; Lightroom's first Subject took 7.9 s).
/// Asked for when a photo is ready in Develop for someone who has not
/// masked yet (for others the photo's masks computed ahead do the same).
/// Installed models only, not under memory pressure, and only for a model
/// that has not run here yet (`core_ml_first_run_pending`; always false
/// away from macOS): two file checks afterwards. The run is on a constant
/// input on a utility-QoS thread (`at_utility_qos`); the model is dropped
/// afterwards unless the masking tools were used meanwhile. Cancelling
/// `task_id` (the Library, memory pressure) stops it before a model's run;
/// Core ML's run itself cannot be stopped, and a click meanwhile waits for
/// it (it would build the same kernels).
#[tauri::command]
pub async fn warm_up_ai_mask_models(
    task_id: String,
    ai_state: tauri::State<'_, AiState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    if ai_state
        .warm_up_fell_back
        .load(std::sync::atomic::Ordering::Relaxed)
    {
        return Ok(());
    }
    let guard = AiTaskGuard::new(&ai_state.tasks, task_id);
    let stop = || {
        if guard.token.is_cancelled() {
            Some("cancelled")
        } else if crate::memory_pressure::under_pressure() {
            Some("the system is short of memory")
        } else {
            None
        }
    };
    // Asked on the first Develop visit, which a cold start's display
    // variant compile may still hold.
    tauri::async_runtime::spawn_blocking(crate::gpu_processing::after_first_display_variant)
        .await
        .map_err(|e| e.to_string())?;
    // Once for both models: masking use during Subject's warm-up keeps Sky
    // too.
    let uses = ai_state.lifecycle.uses();
    for kind in [AutoMask::Subject, AutoMask::Sky] {
        if !crate::ai_processing::core_ml_first_run_pending(&app_handle, kind == AutoMask::Sky) {
            continue;
        }
        // As the masks made ahead (`ai_ahead`): after the decodes, with
        // memory for the model and its run.
        let job = match kind {
            AutoMask::Subject => crate::ai_ahead::AheadJob::Subject,
            AutoMask::Sky => crate::ai_ahead::AheadJob::Sky,
        };
        let need = crate::ai_ahead::estimated_peak(
            job,
            rawtonic_onnx::models_run_on_cpu_only(),
            model_loaded(&ai_state, kind),
        );
        let _ahead = match crate::ai_ahead::admit(job, || false, need, &stop).await {
            Ok(run) => run,
            Err(why) => {
                log::info!(
                    "AI {} mask: GPU kernels not built ahead, {why:?}",
                    kind.name()
                );
                return Ok(());
            }
        };
        let _running = ai_state.lifecycle.run();
        let outcome = warm_up_model(
            async {
                network(&ai_state, &app_handle, kind, false)
                    .await
                    .map_err(String::from)
            },
            stop,
            async |network: Network| {
                tauri::async_runtime::spawn_blocking(move || {
                    crate::ai_processing::at_utility_qos(|| network.warm_up())
                })
                .await
                .map_err(|e| e.to_string())?
                .map_err(|e| e.to_string())
            },
            |network: &Network| {
                let mut ai = lock_recover(&ai_state.models);
                crate::ai_lifecycle::release_after_warm_up(
                    &ai_state.lifecycle,
                    &mut ai,
                    uses,
                    |ai| release_network(ai, network),
                );
            },
        )
        .await?;
        let name = kind.name();
        match outcome {
            WarmUp::Built { load_ms, run_ms } => log::info!(
                "AI {name} mask: GPU kernels built ahead (load {load_ms} ms, run {run_ms} ms)"
            ),
            WarmUp::Stopped(reason) => {
                log::info!("AI {name} mask: GPU kernels not built ahead, {reason}");
                return Ok(());
            }
            WarmUp::Failed(e) => log::warn!("AI {name} mask: building ahead failed: {e}"),
            WarmUp::NotInstalled => {}
            // Core ML failed and ONNX Runtime was loaded instead: no first
            // run is recorded, so nothing more is loaded for a warm-up
            // until the app starts again.
            WarmUp::NotRun => {
                ai_state
                    .warm_up_fell_back
                    .store(true, std::sync::atomic::Ordering::Relaxed);
                return Ok(());
            }
        }
    }
    Ok(())
}

/// What `warm_up_model` did with one model.
#[derive(Debug, PartialEq)]
enum WarmUp {
    /// The model is not installed.
    NotInstalled,
    /// Stopped before the run (cancelled, short of memory).
    Stopped(&'static str),
    /// Not a model that needs it (ONNX Runtime).
    NotRun,
    Built {
        load_ms: u128,
        run_ms: u128,
    },
    Failed(String),
}

/// One model's warm-up: `load`, then its `run` unless `stop` gives a
/// reason before the load or after it (a load takes seconds on a fresh
/// install). Whatever happens once the model is loaded, `release` gets it
/// afterwards.
async fn warm_up_model<M>(
    load: impl Future<Output = Result<Option<M>, String>>,
    stop: impl Fn() -> Option<&'static str>,
    run: impl AsyncFnOnce(M) -> Result<bool, String>,
    release: impl FnOnce(&M),
) -> Result<WarmUp, String>
where
    M: Clone,
{
    if let Some(reason) = stop() {
        return Ok(WarmUp::Stopped(reason));
    }
    let start = std::time::Instant::now();
    let Some(model) = load.await? else {
        return Ok(WarmUp::NotInstalled);
    };
    let load_ms = start.elapsed().as_millis();
    let outcome = match stop() {
        Some(reason) => WarmUp::Stopped(reason),
        None => match run(model.clone()).await {
            Ok(true) => WarmUp::Built {
                load_ms,
                run_ms: start.elapsed().as_millis() - load_ms,
            },
            Ok(false) => WarmUp::NotRun,
            Err(e) => WarmUp::Failed(e),
        },
    };
    release(&model);
    Ok(outcome)
}

/// Drops `network` from the state when it is the loaded one.
fn release_network(ai: &mut crate::ai_processing::AiModels, network: &Network) {
    match network {
        Network::Subject(model) => {
            if ai
                .subject_model
                .as_ref()
                .is_some_and(|m| Arc::ptr_eq(m, model))
            {
                ai.subject_model = None;
            }
        }
        Network::Sky(model) => {
            if ai.sky_model.as_ref().is_some_and(|m| Arc::ptr_eq(m, model)) {
                ai.sky_model = None;
            }
        }
    }
}

/// Analyze only on request; subsequent light edits reuse the persisted depth.
#[tauri::command]
pub async fn generate_relight_depth(
    js_adjustments: serde_json::Value,
    state: tauri::State<'_, AppState>,
    ai_state: tauri::State<'_, AiState>,
    app_handle: tauri::AppHandle,
) -> Result<serde_json::Value, String> {
    // Capture the source before awaiting model initialization.
    let (source, is_raw) = crate::editor_preview::original_source(&state).await?;
    ai_state.lifecycle.touch();
    let running = ai_state.lifecycle.run();
    let _turn = crate::ai_lifecycle::turn(true).await;
    let model = crate::ai_processing::get_or_init_relight_model(&app_handle, &ai_state)
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
        )?;
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
    let subject_model =
        crate::ai_processing::get_or_init_subject_model(&app_handle, &ai_state, true)
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
        crate::ai_processing::get_or_init_people_normals_model(&app_handle, &ai_state)
            .await
            .inspect_err(|e| log::warn!("Relight: continuing without people normals: {e}"))
            .ok()
    };
    // The sky: a new light does not reach it.
    let sky_model = crate::ai_processing::get_or_init_sky_model(&app_handle, &ai_state, true)
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
                        let center = subject_center(&matte);
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
                        Ok((analysis.png, analysis.light, analysis.inverse, center))
                    })
                    .inspect(|(_, light, ..)| {
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
    if let Some((subject, light, inverse, center)) = subject {
        result["subject"] = encode(subject).into();
        if let Some(center) = center {
            result["subjectCenter"] = serde_json::json!(center);
        }
        if let Some(light) = light {
            result["sourceLight"] = serde_json::json!(light);
        }
        if let Some(inverse) = inverse {
            result["subjectDepth"] = serde_json::json!(inverse);
        }
    }
    Ok(result)
}

/// The centre of the subject matte's opaque part, x/y normalized to the
/// frame (where the editor places Relight presets on photos without a face).
fn subject_center(matte: &image::GrayImage) -> Option<[f32; 2]> {
    let (mut x, mut y, mut n) = (0f64, 0f64, 0f64);
    for (px, py, v) in matte.enumerate_pixels() {
        if v[0] >= 128 {
            x += f64::from(px) + 0.5;
            y += f64::from(py) + 0.5;
            n += 1.;
        }
    }
    (n > 0.).then(|| {
        [
            (x / n / f64::from(matte.width())) as f32,
            (y / n / f64::from(matte.height())) as f32,
        ]
    })
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
    #[test]
    fn masks_made_ahead_tell_the_webview_which_to_ask_for_again() {
        // `useAiMasking` asks again for 'dropped' only.
        for (outcome, name) in [
            (super::AheadMask::Ready, "ready"),
            (super::AheadMask::Unavailable, "unavailable"),
            (super::AheadMask::Dropped, "dropped"),
        ] {
            assert_eq!(serde_json::to_value(outcome).unwrap(), name);
        }
    }

    #[test]
    fn ahead_masks_report_why_the_requested_photo_is_unavailable() {
        assert_eq!(
            super::ahead_photo_skip(None, "a"),
            Some("the photo is not ready")
        );
        assert_eq!(
            super::ahead_photo_skip(Some("b"), "a"),
            Some("the photo has changed")
        );
        assert_eq!(super::ahead_photo_skip(Some("a"), "a"), None);
    }

    use super::{AutoMask, WarmUp, preload_kinds, warm_up_model};
    use std::cell::{Cell, RefCell};

    #[test]
    fn cpu_preloads_only_subject() {
        assert_eq!(preload_kinds(true), &[AutoMask::Subject]);
        assert_eq!(preload_kinds(false), &[AutoMask::Subject, AutoMask::Sky]);
    }

    #[test]
    fn subject_center_is_the_opaque_matte_centroid() {
        let matte = image::GrayImage::from_fn(10, 4, |x, y| {
            image::Luma([if (6..10).contains(&x) && y < 2 {
                255
            } else {
                40
            }])
        });
        assert_eq!(super::subject_center(&matte), Some([0.8, 0.25]));
        assert_eq!(super::subject_center(&image::GrayImage::new(3, 3)), None);
    }

    /// Runs `warm_up_model` on a fake model 7: `stops` are the answers of
    /// `stop` in turn, `run` the run's result. Returns the outcome, whether
    /// it ran and what was released.
    async fn warm_up(
        stops: &[Option<&'static str>],
        installed: bool,
        run: Result<bool, String>,
    ) -> (WarmUp, bool, Vec<u32>) {
        let asked = Cell::new(0);
        let ran = Cell::new(false);
        let released = RefCell::new(Vec::new());
        let outcome = warm_up_model(
            async { Ok(installed.then_some(7u32)) },
            || {
                asked.set(asked.get() + 1);
                stops.get(asked.get() - 1).copied().flatten()
            },
            async |_model: u32| {
                ran.set(true);
                run
            },
            |model: &u32| released.borrow_mut().push(*model),
        )
        .await
        .unwrap();
        (outcome, ran.get(), released.into_inner())
    }

    #[tokio::test]
    async fn a_warm_up_runs_and_releases_its_model() {
        let (outcome, ran, released) = warm_up(&[None, None], true, Ok(true)).await;
        assert!(matches!(outcome, WarmUp::Built { .. }) && ran);
        assert_eq!(released, [7]);
    }

    #[tokio::test]
    async fn a_warm_up_stopped_during_the_load_does_not_run_but_releases() {
        let (outcome, ran, released) = warm_up(&[None, Some("cancelled")], true, Ok(true)).await;
        assert_eq!(outcome, WarmUp::Stopped("cancelled"));
        assert!(!ran);
        assert_eq!(released, [7]);
    }

    #[tokio::test]
    async fn a_warm_up_stopped_before_the_load_loads_nothing() {
        let (outcome, ran, released) = warm_up(&[Some("short")], true, Ok(true)).await;
        assert_eq!(outcome, WarmUp::Stopped("short"));
        assert!(!ran && released.is_empty());
    }

    #[tokio::test]
    async fn a_model_not_run_or_failing_is_released_too() {
        let (outcome, _, released) = warm_up(&[None, None], true, Ok(false)).await;
        assert_eq!(outcome, WarmUp::NotRun);
        assert_eq!(released, [7]);
        let (outcome, _, released) = warm_up(&[None, None], true, Err("gpu".into())).await;
        assert_eq!(outcome, WarmUp::Failed("gpu".into()));
        assert_eq!(released, [7]);
        let (outcome, ran, released) = warm_up(&[None, None], false, Ok(true)).await;
        assert_eq!(outcome, WarmUp::NotInstalled);
        assert!(!ran && released.is_empty());
    }

    /// A request cancelled while it waits for the encoder (another photo's
    /// encoding runs) does not start; one that was not cancelled does.
    #[tokio::test]
    async fn a_request_cancelled_while_queued_does_not_start() {
        let lock = tokio::sync::Mutex::new(());
        let running = lock.lock().await;
        let token = std::sync::Arc::new(crate::app_state::AiTaskToken::new());
        let queued = {
            let token = token.clone();
            let lock = &lock;
            async move { super::wait_turn(lock, Some(&token)).await.map(drop) }
        };
        let other = std::sync::Arc::new(crate::app_state::AiTaskToken::new());
        let kept = {
            let lock = &lock;
            let other = other.clone();
            async move { super::wait_turn(lock, Some(&other)).await.map(drop) }
        };
        let (queued, kept, ()) = tokio::join!(queued, kept, async {
            token.cancel();
            drop(running);
        });
        assert_eq!(queued, Err(task_cancelled()));
        assert_eq!(task_cancelled().code, ErrorCode::Cancelled);
        assert_eq!(kept, Ok(()));
    }

    use super::*;

    #[test]
    fn geometry_key_follows_the_development_and_geometry_only() {
        let loaded =
            |path: &str, image: &Arc<image::DynamicImage>| crate::state::source::LoadedImage {
                path: path.into(),
                image: image.clone(),
                is_raw: true,
                decode: None,
                ai_denoise: None,
                decoder: None,
                measures: None,
            };
        let pixels = || Arc::new(image::DynamicImage::new_rgb8(2, 1));
        let decode = pixels();
        let x = loaded("x.raf", &decode);
        let a = serde_json::json!({"exposure": 1.0, "transformRotate": 0.0});
        let b = serde_json::json!({"exposure": 2.0, "transformRotate": 0.0});
        let c = serde_json::json!({"exposure": 1.0, "transformRotate": 5.0});
        assert_eq!(geometry_key(&x, &a), geometry_key(&x, &b));
        assert_ne!(geometry_key(&x, &a), geometry_key(&x, &c));
        assert_ne!(
            geometry_key(&x, &a),
            geometry_key(&loaded("y.raf", &decode), &a)
        );
        // The same development (back from the decoded-image cache): its masks.
        assert_eq!(
            geometry_key(&x, &a),
            geometry_key(&loaded("x.raf", &decode), &a)
        );
        // Developed again (AI Denoise, decode options): computed again.
        let denoised = pixels();
        assert_ne!(
            geometry_key(&x, &a),
            geometry_key(&loaded("x.raf", &denoised), &a)
        );
        // A generation is never handed to a later development.
        let first = x.source_generation();
        drop((x, decode));
        assert_ne!(loaded("x.raf", &pixels()).source_generation(), first);
    }

    #[test]
    fn a_mask_is_only_for_the_photo_asked_for() {
        assert_eq!(ensure_photo("/a.raf", "/a.raf"), Ok(()));
        assert_eq!(
            ensure_photo("/b.raf", "/a.raf"),
            Err(CommandError::new(ErrorCode::Stale, PHOTO_CHANGED))
        );
        // A virtual copy is another photo of the same file.
        assert!(ensure_photo("/a.raf?vc=1", "/a.raf").is_err());
    }
}
