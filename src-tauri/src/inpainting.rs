use std::io::Cursor;

use base64::{Engine as _, engine::general_purpose};
use image::{DynamicImage, GenericImageView, Rgb, RgbImage, RgbaImage};
use serde_json::Value;

use crate::ai_connector;
use crate::ai_processing;
use crate::app_settings::load_settings;
use crate::app_state::AppState;
use crate::image_loader::composite_patches_on_image;
use crate::image_processing::apply_linear_to_srgb;
use crate::mask_generation::resolve_warped_image_for_masks;
use crate::mask_generation::{
    AiPatchDefinition, MaskDefinition, PhotoGeometry, generate_mask_bitmap,
};

fn prepare_source_image(
    patch_id: &str,
    current_adjustments: &Value,
    state: &tauri::State<'_, AppState>,
) -> Result<(DynamicImage, bool), String> {
    let mut source_image_adjustments = current_adjustments.clone();
    if let Some(patches) = source_image_adjustments
        .get_mut("aiPatches")
        .and_then(|v| v.as_array_mut())
    {
        patches.retain(|p| p.get("id").and_then(|id| id.as_str()) != Some(patch_id));
    }

    let is_raw = {
        let guard = state.original_image.lock().unwrap();
        guard.as_ref().map(|img| img.is_raw).unwrap_or(false)
    };

    let (base_image, _) = crate::editor_preview::get_original_image(state)?;
    let composited = composite_patches_on_image(&base_image, &source_image_adjustments)
        .map_err(|e| format!("Failed to prepare source image: {}", e))?;

    let source_image = if is_raw {
        apply_linear_to_srgb(composited)
    } else {
        composited
    };

    Ok((source_image, is_raw))
}

fn calculate_mask_bounds(
    mask_bitmap: &image::GrayImage,
) -> Result<(usize, usize, usize, usize), String> {
    let (img_w, img_h) = mask_bitmap.dimensions();
    let img_w_usize = img_w as usize;
    let img_h_usize = img_h as usize;
    let mask_raw = mask_bitmap.as_raw();

    let mut min_y = img_h_usize;
    let mut max_y = 0;

    for y in 0..img_h_usize {
        let row_start = y * img_w_usize;
        if mask_raw[row_start..row_start + img_w_usize]
            .iter()
            .any(|&p| p > 0)
        {
            min_y = y;
            break;
        }
    }

    if min_y == img_h_usize {
        return Err("Mask is empty.".to_string());
    }

    for y in (min_y..img_h_usize).rev() {
        let row_start = y * img_w_usize;
        if mask_raw[row_start..row_start + img_w_usize]
            .iter()
            .any(|&p| p > 0)
        {
            max_y = y;
            break;
        }
    }

    let mut min_x = img_w_usize;
    let mut max_x = 0;
    for y in min_y..=max_y {
        let row_start = y * img_w_usize;
        let row = &mask_raw[row_start..row_start + img_w_usize];
        if let Some(first) = row.iter().position(|&p| p > 0)
            && first < min_x
        {
            min_x = first;
        }
        if let Some(last) = row.iter().rposition(|&p| p > 0)
            && last > max_x
        {
            max_x = last;
        }
    }

    Ok((min_x, max_x, min_y, max_y))
}

#[allow(clippy::too_many_arguments)]
fn encode_patch_result(
    color_image: &RgbImage,
    mask_image: &image::GrayImage,
    offset_x: u32,
    offset_y: u32,
    width: u32,
    height: u32,
    is_srgb: bool,
    quality: u8,
    mask_as_png: bool,
    model: Option<&str>,
) -> Result<String, String> {
    let mut color_buf = Cursor::new(Vec::with_capacity(32768));
    color_image
        .write_with_encoder(image::codecs::jpeg::JpegEncoder::new_with_quality(
            &mut color_buf,
            quality,
        ))
        .map_err(|e| e.to_string())?;
    let color_base64 = general_purpose::STANDARD.encode(color_buf.get_ref());

    let mut mask_buf = Cursor::new(Vec::with_capacity(32768));
    if mask_as_png {
        mask_image
            .write_to(&mut mask_buf, image::ImageFormat::Png)
            .map_err(|e| e.to_string())?;
    } else {
        mask_image
            .write_with_encoder(image::codecs::jpeg::JpegEncoder::new_with_quality(
                &mut mask_buf,
                quality,
            ))
            .map_err(|e| e.to_string())?;
    }
    let mask_base64 = general_purpose::STANDARD.encode(mask_buf.get_ref());

    let mut result = serde_json::json!({
        "color": color_base64,
        "mask": mask_base64,
        "offsetX": offset_x,
        "offsetY": offset_y,
        "width": width,
        "height": height,
        "isSrgbEncoded": is_srgb
    });
    // The model that guided a fill, so the library can find its photographs
    // (`rawtonic_metadata::provenance::fill_model`).
    if let Some(model) = model {
        result["model"] = Value::from(model);
    }
    let result_json = result.to_string();

    Ok(result_json)
}

/// Remove's fill (docs/MODELS.md, "Object removal"): every pixel the
/// selection touches is in the hole (a hole only where it is half covered
/// left object edges and shadows that the model grew back into objects);
/// MI-GAN fills it at its 512 px working size,
/// which places structure (edges, horizons, the run of a fence) but is soft
/// and smeared when the hole is larger than that; the fill is then rebuilt
/// at full resolution from the photo's own pixels by patch-based completion
/// guided by MI-GAN's result, so texture and grain are the photo's. Returns
/// the photo with the hole replaced (no blending: the patch's mask blends it
/// when it is composited).
fn fill_removal(
    source: &DynamicImage,
    mask: &image::GrayImage,
    model: &rawtonic_onnx::Model,
) -> anyhow::Result<RgbaImage> {
    let hole_mask = binary_hole(mask);
    let guided = ai_processing::run_inpainting(source, &hole_mask, model)?;
    Ok(complete_with_guide(source, &hole_mask, guided))
}

/// Every pixel the selection touches (`mask` > 0) as 255.
fn binary_hole(mask: &image::GrayImage) -> image::GrayImage {
    let (w, h) = mask.dimensions();
    image::GrayImage::from_fn(w, h, |x, y| {
        image::Luma([if mask.get_pixel(x, y)[0] > 0 { 255 } else { 0 }])
    })
}

/// Holes at least this thick (the diameter of the largest disc inside,
/// in photo pixels) are large: MI-GAN would see them at full resolution
/// with `moebius::CONTEXT` × their size around them only up to its 512 px
/// view / 2.5. Thicker, the hole fills more than 40 % of its view's side
/// (where it painted objects back, docs/MODELS.md) or the view is scaled
/// down, and Moebius guides the fill instead. The thickness rather
/// than the longer side: a thin line is filled from its two sides, however
/// long it is.
const LARGE_HOLE_THICKNESS: f32 = crate::moebius::SIZE as f32 / crate::moebius::CONTEXT;

/// The slowest network run (one UNet step of the CFG batch) at which
/// Moebius stays on: 1.5 s, so about 15 s per removal, three times our 5 s
/// budget for a large hole. Not fitted: the GPUs measured take 0.2 s (Apple
/// M4 Pro, Core ML) and 0.4 s (RTX 3080, WebGPU); an integrated GPU may take
/// many times that (notes: benchmark-results/moebius-onnx-2026-10-27).
const MOEBIUS_MAX_SECONDS_PER_RUN: f64 = 1.5;

/// Set for the rest of the session when a removal measured Moebius slower
/// than `MOEBIUS_MAX_SECONDS_PER_RUN`: large holes go to MI-GAN.
static MOEBIUS_TOO_SLOW: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Set once Remove has said Moebius cannot run on this computer.
static MOEBIUS_UNAVAILABLE_SAID: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Moebius fills (running now, started so far). Overlapping removals wait
/// for each other's network runs (one session per model), so a fill's time
/// per run counts for `MOEBIUS_TOO_SLOW` only when it ran alone: none was
/// running when it started and none started before it ended.
static MOEBIUS_FILLS: std::sync::Mutex<(usize, u64)> = std::sync::Mutex::new((0, 0));

/// Seconds per network run from the instants each run finished: the mean
/// gap after the first run (which also holds the encoder and, on the first
/// use, shader compilation). `None` with fewer than two runs.
fn seconds_per_run(finished: &[std::time::Instant]) -> Option<f64> {
    let (first, last) = (finished.first()?, finished.last()?);
    (finished.len() >= 2)
        .then(|| last.duration_since(*first).as_secs_f64() / (finished.len() - 1) as f64)
}

/// The hole's thickness (`LARGE_HOLE_THICKNESS`), within `bounds`
/// (x0, x1, y0, y1 inclusive), measured on a grid of at most 1024 cells
/// across (a cell holds the hole when any of its pixels does).
fn hole_thickness(mask: &image::GrayImage, bounds: (usize, usize, usize, usize)) -> f32 {
    let (x0, x1, y0, y1) = bounds;
    let (bw, bh) = (x1 - x0 + 1, y1 - y0 + 1);
    let cell = bw.max(bh).div_ceil(1024);
    // One empty cell around the hole, so its edge counts as outside.
    let (gw, gh) = (bw.div_ceil(cell) + 2, bh.div_ceil(cell) + 2);
    let mut grid = vec![false; gw * gh];
    for y in y0..=y1 {
        for x in x0..=x1 {
            if mask.get_pixel(x as u32, y as u32)[0] > 0 {
                grid[((y - y0) / cell + 1) * gw + (x - x0) / cell + 1] = true;
            }
        }
    }
    let radius = ai_processing::edt_2d(&grid, gw, gh)
        .into_iter()
        .fold(0.0f32, f32::max);
    2.0 * radius * cell as f32
}

/// Rebuilds the hole (`hole_mask` 255) of `source` at full resolution by
/// patch-based completion guided by `guided` (the photo with a network's
/// fill in the hole).
fn complete_with_guide(
    source: &DynamicImage,
    hole_mask: &image::GrayImage,
    guided: RgbaImage,
) -> RgbaImage {
    let (w, h) = hole_mask.dimensions();
    let Ok((min_x, max_x, min_y, max_y)) = calculate_mask_bounds(hole_mask) else {
        return guided;
    };
    // The region the completion copies from: 1.5 × the hole around it, as
    // the model's crop (at least 128 px).
    let (mw, mh) = (max_x - min_x + 1, max_y - min_y + 1);
    let pad_x = 128.max(mw * 3 / 2);
    let pad_y = 128.max(mh * 3 / 2);
    let x0 = min_x.saturating_sub(pad_x);
    let y0 = min_y.saturating_sub(pad_y);
    let x1 = (max_x + pad_x + 1).min(w as usize);
    let y1 = (max_y + pad_y + 1).min(h as usize);
    let (cw, ch) = (x1 - x0, y1 - y0);
    let rgb = source.to_rgb8();
    let to_px = |p: [u8; 3]| p.map(|v| v as f32 / 255.0);
    let mut img = Vec::with_capacity(cw * ch);
    let mut guide = Vec::with_capacity(cw * ch);
    let mut hole = Vec::with_capacity(cw * ch);
    for y in y0..y1 {
        for x in x0..x1 {
            img.push(to_px(rgb.get_pixel(x as u32, y as u32).0));
            let g = guided.get_pixel(x as u32, y as u32).0;
            guide.push(to_px([g[0], g[1], g[2]]));
            hole.push(hole_mask.get_pixel(x as u32, y as u32)[0] > 0);
        }
    }
    let filled = rawtonic_engine::patch_fill::fill(
        cw,
        ch,
        &img,
        &hole,
        Some(&guide),
        &removal_fill_params(hole.iter().filter(|&&h| h).count()),
    );
    let mut out = guided;
    for (i, v) in filled.iter().enumerate() {
        if hole[i] {
            let (x, y) = ((x0 + i % cw) as u32, (y0 + i / cw) as u32);
            let px = v.map(|c| (c * 255.0).round().clamp(0.0, 255.0) as u8);
            out.put_pixel(x, y, image::Rgba([px[0], px[1], px[2], 255]));
        }
    }
    out
}

/// The completion (`filled`) with the guide's shading and colour: below
/// the guide's own detail (a Gaussian of `sigma` pixels, the size of one of
/// its pixels; below about 0.75 px, a crop under ~384 px, the guide is at
/// full resolution and the hole takes it as it is) the hole takes the guide's values, above it the
/// completion's texture. Where the photo around a large hole has no patch
/// of the shade the guide asks for (a shadow inside a bowl), the completion
/// copies the nearest ones (the dark wall behind it) in blocks; that
/// difference is a few codes in the fill's dim base image but plain after
/// the develop settings lift it.
fn keep_guide_shading(
    mut filled: RgbaImage,
    guided: &RgbaImage,
    hole_mask: &image::GrayImage,
    sigma: f32,
) -> RgbaImage {
    let Ok((min_x, max_x, min_y, max_y)) = calculate_mask_bounds(hole_mask) else {
        return filled;
    };
    let (w, h) = hole_mask.dimensions();
    let reach = (3.0 * sigma).ceil() as usize;
    let x0 = min_x.saturating_sub(reach);
    let y0 = min_y.saturating_sub(reach);
    let x1 = (max_x + reach + 1).min(w as usize);
    let y1 = (max_y + reach + 1).min(h as usize);
    let (cw, ch) = (x1 - x0, y1 - y0);
    // Planes of the hole's weight and of its weighted difference
    // guide − completion (only hole pixels count, so the photo around it
    // does not pull the shading).
    let mut planes = vec![vec![0.0f32; cw * ch]; 4];
    for y in 0..ch {
        for x in 0..cw {
            let (px, py) = ((x0 + x) as u32, (y0 + y) as u32);
            if hole_mask.get_pixel(px, py)[0] == 0 {
                continue;
            }
            let (g, f) = (guided.get_pixel(px, py).0, filled.get_pixel(px, py).0);
            let i = y * cw + x;
            planes[3][i] = 1.0;
            for c in 0..3 {
                planes[c][i] = g[c] as f32 - f[c] as f32;
            }
        }
    }
    {
        use rayon::prelude::*;
        planes
            .par_iter_mut()
            .for_each(|plane| blur_plane(plane, cw, ch, sigma));
    }
    for y in 0..ch {
        for x in 0..cw {
            let (px, py) = ((x0 + x) as u32, (y0 + y) as u32);
            let i = y * cw + x;
            if hole_mask.get_pixel(px, py)[0] == 0 || planes[3][i] <= 1e-6 {
                continue;
            }
            let p = filled.get_pixel_mut(px, py);
            for c in 0..3 {
                let v = p[c] as f32 + planes[c][i] / planes[3][i];
                p[c] = v.round().clamp(0.0, 255.0) as u8;
            }
        }
    }
    filled
}

/// Blurs a `w` × `h` plane by about a Gaussian of `sigma` pixels: three
/// box passes along rows, then along columns (zero outside).
fn blur_plane(plane: &mut [f32], w: usize, h: usize, sigma: f32) {
    // The box radius whose three passes have the Gaussian's variance.
    let r = (((12.0 * sigma * sigma / 3.0 + 1.0).sqrt() - 1.0) / 2.0).round() as usize;
    if r == 0 {
        return;
    }
    let mut line = Vec::new();
    for _ in 0..3 {
        for row in plane.chunks_mut(w) {
            box_line(row, r, &mut line);
        }
        // Columns: running sums over rows, all columns at once.
        let mut sums = vec![0.0f32; w];
        let mut out = vec![0.0f32; w * h];
        for y in 0..h + r {
            if y < h {
                for (s, v) in sums.iter_mut().zip(&plane[y * w..(y + 1) * w]) {
                    *s += v;
                }
            }
            if y > 2 * r {
                let gone = y - 2 * r - 1;
                for (s, v) in sums.iter_mut().zip(&plane[gone * w..(gone + 1) * w]) {
                    *s -= v;
                }
            }
            if y >= r {
                let at = y - r;
                for (o, s) in out[at * w..(at + 1) * w].iter_mut().zip(&sums) {
                    *o = s / (2 * r + 1) as f32;
                }
            }
        }
        plane.copy_from_slice(&out);
    }
}

/// One box pass of radius `r` along `row` (zero outside); `line` is scratch.
fn box_line(row: &mut [f32], r: usize, line: &mut Vec<f32>) {
    line.clear();
    line.extend_from_slice(row);
    let n = row.len();
    let mut sum: f32 = line[..r.min(n)].iter().sum();
    for x in 0..n {
        if x + r < n {
            sum += line[x + r];
        }
        if x > r {
            sum -= line[x - r - 1];
        }
        row[x] = sum / (2 * r + 1) as f32;
    }
}

/// The completion's settings for Remove, chosen on the object-removal set
/// (docs/MODELS.md): 5 × 5 patches follow thin structure (rails, joints)
/// better than 7 × 7, a strong guide keeps MI-GAN's layout, and weighted
/// texture features keep grain from washing out. Holes above
/// `LARGE_HOLE` pixels only refine their finest levels (where the hole is
/// wider than 256 of their pixels), which keeps a car-sized removal on a
/// 45 MP photo to seconds.
fn removal_fill_params(hole_pixels: usize) -> rawtonic_engine::patch_fill::PatchFillParams {
    const LARGE_HOLE: usize = 400_000;
    rawtonic_engine::patch_fill::PatchFillParams {
        patch_radius: 2,
        guide_weight: 10.0,
        texture_weight: 3.0,
        fine_extent: if hole_pixels > LARGE_HOLE { 256 } else { 0 },
        ..Default::default()
    }
}

/// Remove's fill of a large hole: Moebius's fill guides the completion
/// instead of MI-GAN's (`moebius`). Downloads the model on first use.
/// `Ok(None)` when Moebius is not available (no download possible, Core ML
/// failed, no GPU provider on Windows and Linux, a failed run) or the hole
/// does not fit its square crop: MI-GAN fills then. Emits
/// `ai-fill-progress` ({taskId, done, total} network runs).
async fn fill_large_hole(
    app_handle: &tauri::AppHandle,
    state: &tauri::State<'_, AppState>,
    source: &DynamicImage,
    mask: &image::GrayImage,
    bounds: (usize, usize, usize, usize),
    task_id: Option<&str>,
    token: Option<std::sync::Arc<crate::app_state::AiTaskToken>>,
) -> Result<Option<RgbaImage>, String> {
    use tauri::Emitter;

    let (x0, x1, y0, y1) = bounds;
    if MOEBIUS_TOO_SLOW.load(std::sync::atomic::Ordering::Relaxed) {
        return Ok(None);
    }
    // A hole longer than the photo's shorter side does not fit the
    // network's square: MI-GAN, without downloading or loading Moebius.
    let (w, h) = source.dimensions();
    if crate::moebius::crop_square((x0 as u32, y0 as u32, x1 as u32, y1 as u32), w, h).is_none() {
        log::info!("AI fill: the hole does not fit Moebius's square crop; MI-GAN guides the fill");
        return Ok(None);
    }
    let start = std::time::Instant::now();
    // The first use downloads the model: Cancel stops waiting for it.
    let loading = ai_processing::get_or_init_moebius(app_handle, &state.ai_state);
    let loaded = match &token {
        Some(token) => tokio::select! {
            loaded = loading => loaded,
            _ = token.wait_for_cancel() => return Err("Task cancelled".to_string()),
        },
        None => loading.await,
    };
    // MI-GAN fills when Moebius cannot: the user is told why the fill is
    // weaker (`ai-fill-notice`: the download failed, or the model did not
    // run).
    let notice = |reason: &str| {
        // "unavailable" does not change within a session (no GPU provider,
        // Core ML fails): said once, not on every large removal.
        if reason == "unavailable"
            && MOEBIUS_UNAVAILABLE_SAID.swap(true, std::sync::atomic::Ordering::Relaxed)
        {
            return;
        }
        let _ = app_handle.emit(
            "ai-fill-notice",
            serde_json::json!({ "taskId": task_id, "reason": reason }),
        );
    };
    let model = match loaded {
        Ok(model) => model,
        Err(e) => {
            log::warn!("AI fill: Moebius unavailable ({e:#}); MI-GAN guides the fill");
            let offline = e.downcast_ref::<ai_processing::DownloadFailed>().is_some();
            notice(if offline { "download" } else { "unavailable" });
            return Ok(None);
        }
    };
    let load_ms = start.elapsed().as_millis();
    if token.as_ref().is_some_and(|t| t.is_cancelled()) {
        return Err("Task cancelled".to_string());
    }
    let hole_mask = binary_hole(mask);
    let rgb = source.to_rgb8();
    let app = app_handle.clone();
    let task = task_id.map(str::to_string);
    let cancel = token.clone();
    let (guided, per_run) = tokio::task::spawn_blocking(move || {
        let (alone, started) = {
            let mut fills = MOEBIUS_FILLS.lock().unwrap_or_else(|e| e.into_inner());
            fills.0 += 1;
            fills.1 += 1;
            (fills.0 == 1, fills.1)
        };
        let mut finished = Vec::new();
        let mut progress = |done: usize, total: usize| {
            if done > 0 {
                finished.push(std::time::Instant::now());
            }
            let _ = app.emit(
                "ai-fill-progress",
                serde_json::json!({ "taskId": task, "done": done, "total": total }),
            );
        };
        let cancelled = || cancel.as_ref().is_some_and(|t| t.is_cancelled());
        let guided = crate::moebius::guide(
            &*model,
            &rgb,
            &hole_mask,
            (x0 as u32, y0 as u32, x1 as u32, y1 as u32),
            &mut progress,
            &cancelled,
        )
        .map(|guided| guided.map(|g| (g, hole_mask)));
        let alone = {
            let mut fills = MOEBIUS_FILLS.lock().unwrap_or_else(|e| e.into_inner());
            fills.0 -= 1;
            // Another fill started meanwhile: this one may have waited for it.
            alone && fills.1 == started
        };
        (guided, seconds_per_run(&finished).filter(|_| alone))
    })
    .await
    .map_err(|e| e.to_string())?;
    // A GPU too slow for Moebius: this removal finishes, later ones use
    // MI-GAN for the rest of the session, and the user is told once.
    if let Some(per_run) = per_run
        && per_run > MOEBIUS_MAX_SECONDS_PER_RUN
        && !MOEBIUS_TOO_SLOW.swap(true, std::sync::atomic::Ordering::Relaxed)
    {
        log::warn!(
            "AI fill: Moebius took {per_run:.2} s per network run (limit {MOEBIUS_MAX_SECONDS_PER_RUN} s); MI-GAN guides large holes for the rest of the session"
        );
        let _ = app_handle.emit(
            "ai-fill-notice",
            serde_json::json!({ "taskId": task_id, "reason": "tooSlow" }),
        );
    }
    let guide_ms = start.elapsed().as_millis() - load_ms;
    let (guided, hole_mask) = match guided {
        Ok(Some(done)) => done,
        Ok(None) if token.as_ref().is_some_and(|t| t.is_cancelled()) => {
            return Err("Task cancelled".to_string());
        }
        Ok(None) => {
            log::info!(
                "AI fill: the hole does not fit Moebius's square crop; MI-GAN guides the fill"
            );
            return Ok(None);
        }
        Err(e) => {
            log::warn!("AI fill: Moebius failed ({e}); MI-GAN guides the fill");
            notice("unavailable");
            return Ok(None);
        }
    };
    // The guide's own detail is a pixel of its 512 px square.
    let side = crate::moebius::crop_square((x0 as u32, y0 as u32, x1 as u32, y1 as u32), w, h)
        .map_or(1.0, |(_, _, side)| {
            side as f32 / crate::moebius::SIZE as f32
        });
    let filled = complete_with_guide(source, &hole_mask, guided.clone());
    let filled = keep_guide_shading(filled, &guided, &hole_mask, side);
    log::info!(
        "AI fill: Moebius guide: load {load_ms} ms, network {guide_ms} ms, completion {} ms",
        start.elapsed().as_millis() - load_ms - guide_ms
    );
    Ok(Some(filled))
}

#[tauri::command]
pub async fn invoke_generative_replace_with_mask_def(
    path: String,
    patch_definition: AiPatchDefinition,
    current_adjustments: Value,
    use_fast_inpaint: bool,
    task_id: Option<String>,
    app_handle: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    let _guard = task_id
        .as_ref()
        .map(|id| crate::app_state::AiTaskGuard::new(&state.active_ai_tasks, id.clone()));
    let cancel_flag = _guard.as_ref().map(|g| &g.token);

    if cancel_flag.as_ref().is_some_and(|t| t.is_cancelled()) {
        return Err("Task cancelled".to_string());
    }

    let settings = load_settings(app_handle.clone()).unwrap_or_default();
    // MI-GAN follows the masking models' lifecycle (ai_lifecycle).
    crate::ai_lifecycle::LIFECYCLE.touch();
    let _running = crate::ai_lifecycle::LIFECYCLE.run();

    let started = std::time::Instant::now();
    let (source_image, is_raw) =
        prepare_source_image(&patch_definition.id, &current_adjustments, &state)?;
    let source_ms = started.elapsed().as_millis();
    let (img_w, img_h) = source_image.dimensions();

    let orientation_steps = current_adjustments
        .get("orientationSteps")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u8;
    let (trans_w, trans_h) = if orientation_steps % 2 == 1 {
        (img_h, img_w)
    } else {
        (img_w, img_h)
    };

    let mask_def_for_generation = MaskDefinition {
        id: patch_definition.id.clone(),
        name: patch_definition.name.clone(),
        visible: patch_definition.visible,
        invert: patch_definition.invert,
        opacity: 100.0,
        mask_version: None,
        refine_edge: None,
        adjustments: serde_json::Value::Null,
        sub_masks: patch_definition.sub_masks.clone(),
    };

    let (mask_source, source_is_raw) = crate::editor_preview::original_source(&state).await?;
    let warped_image = resolve_warped_image_for_masks(
        &state,
        &mask_source,
        source_is_raw,
        1.0,
        &current_adjustments,
        std::slice::from_ref(&mask_def_for_generation),
    )?;

    let mask_bitmap = generate_mask_bitmap(
        &mask_def_for_generation,
        trans_w,
        trans_h,
        1.0,
        (0.0, 0.0),
        &PhotoGeometry::from_adjustments(&current_adjustments)
            .with_photo_size(img_w as f64, img_h as f64),
        warped_image.as_ref(),
    )
    .ok_or("Failed to generate mask bitmap for AI replace")?;

    let mask_bitmap =
        crate::image_processing::inverse_transform_mask(mask_bitmap, &current_adjustments);

    let (min_x, max_x, min_y, max_y) = calculate_mask_bounds(&mask_bitmap)?;
    let mask_ms = started.elapsed().as_millis() - source_ms;

    if cancel_flag.as_ref().is_some_and(|t| t.is_cancelled()) {
        return Err("Task cancelled".to_string());
    }

    let fill_start = std::time::Instant::now();
    let large_fill = if use_fast_inpaint
        && hole_thickness(&mask_bitmap, (min_x, max_x, min_y, max_y)) >= LARGE_HOLE_THICKNESS
    {
        fill_large_hole(
            &app_handle,
            &state,
            &source_image,
            &mask_bitmap,
            (min_x, max_x, min_y, max_y),
            task_id.as_deref(),
            _guard.as_ref().map(|g| g.token.clone()),
        )
        .await?
    } else {
        None
    };
    use rawtonic_metadata::provenance::fill_model;
    let (patch_rgba, model) = if let Some(filled) = large_fill {
        (filled, fill_model::MOEBIUS)
    } else if use_fast_inpaint {
        let inpaint_model = ai_processing::get_or_init_inpaint_model(
            &app_handle,
            &state.ai_state,
            &state.ai_init_lock,
        )
        .await
        .map_err(|e| e.to_string())?;

        if cancel_flag.as_ref().is_some_and(|t| t.is_cancelled()) {
            return Err("Task cancelled".to_string());
        }

        let filled =
            fill_removal(&source_image, &mask_bitmap, &inpaint_model).map_err(|e| e.to_string())?;
        (filled, fill_model::MI_GAN)
    } else if settings.ai_provider.as_deref() == Some("ai-connector")
        && let Some(address) = settings.ai_connector_address
    {
        let base_url = format!("http://{}", address);

        let mut rgba_mask = RgbaImage::new(img_w, img_h);
        for (src_val, dst_chunk) in mask_bitmap.as_raw().iter().zip(rgba_mask.chunks_mut(4)) {
            let intensity = *src_val;
            dst_chunk[0] = intensity;
            dst_chunk[1] = intensity;
            dst_chunk[2] = intensity;
            dst_chunk[3] = 255;
        }
        let mask_image_dynamic = DynamicImage::ImageRgba8(rgba_mask);

        let (real_path_buf, _) = crate::file_management::parse_virtual_path(&path);
        let real_path_str = real_path_buf.to_string_lossy().to_string();

        let inpaint_fut = ai_connector::process_inpainting(
            &base_url,
            &real_path_str,
            &source_image,
            &mask_image_dynamic,
            patch_definition.prompt,
            None,
        );

        let filled = if let Some(token) = cancel_flag.as_ref() {
            tokio::select! {
                res = inpaint_fut => res.map_err(|e| e.to_string())?,
                _ = token.wait_for_cancel() => {
                    return Err("Task cancelled".to_string());
                }
            }
        } else {
            inpaint_fut.await.map_err(|e| e.to_string())?
        };
        (filled, fill_model::EXTERNAL)
    } else {
        return Err(
            "No generative backend configured or connection invalid. Please check your AI settings."
                .to_string(),
        );
    };

    if cancel_flag.as_ref().is_some_and(|t| t.is_cancelled()) {
        return Err("Task cancelled".to_string());
    }

    let (patch_w, patch_h) = patch_rgba.dimensions();
    let final_patch = if patch_w != img_w || patch_h != img_h {
        image::imageops::resize(
            &patch_rgba,
            img_w,
            img_h,
            image::imageops::FilterType::Lanczos3,
        )
    } else {
        patch_rgba.clone()
    };

    let min_x_u32 = min_x as u32;
    let min_y_u32 = min_y as u32;
    let crop_w = (max_x - min_x + 1) as u32;
    let crop_h = (max_y - min_y + 1) as u32;

    let mut color_image = RgbImage::new(crop_w, crop_h);

    for y in min_y..=max_y {
        for x in min_x..=max_x {
            let px_x = x as u32;
            let px_y = y as u32;
            let mask_value = mask_bitmap.get_pixel(px_x, px_y)[0];

            let out_x = px_x - min_x_u32;
            let out_y = px_y - min_y_u32;

            let px = if mask_value > 0 {
                *final_patch.get_pixel(px_x, px_y)
            } else {
                source_image.get_pixel(px_x, px_y)
            };
            color_image.put_pixel(out_x, out_y, Rgb([px[0], px[1], px[2]]));
        }
    }

    let output_mask =
        image::imageops::crop_imm(&mask_bitmap, min_x_u32, min_y_u32, crop_w, crop_h).to_image();
    let fill_ms = fill_start.elapsed().as_millis();

    let result = encode_patch_result(
        &color_image,
        &output_mask,
        min_x_u32,
        min_y_u32,
        crop_w,
        crop_h,
        is_raw,
        95,
        true,
        Some(model),
    );
    log::info!(
        "AI fill ({}): {img_w}x{img_h}, area {crop_w}x{crop_h}: source {source_ms} ms, mask {mask_ms} ms, fill {fill_ms} ms, total {} ms",
        patch_definition.name,
        started.elapsed().as_millis()
    );
    result
}

#[cfg(test)]
mod removal_tests {
    /// Remove's fill on a striped photo with grain and a soft-edged hole:
    /// only the hole changes, the stripes continue through it and the grain
    /// is kept (MI-GAN alone leaves the hole smooth). Needs the model in
    /// `RAWTONIC_INPAINT_MODEL` and ONNX Runtime in `ORT_DYLIB_PATH`.
    #[test]
    #[ignore]
    fn removal_fill_continues_texture_and_keeps_the_rest() {
        let Some(model) = std::env::var_os("RAWTONIC_INPAINT_MODEL") else {
            return;
        };
        let session = rawtonic_onnx::Model::load(
            std::path::Path::new(&model),
            rawtonic_onnx::ModelKind::Inpaint,
        )
        .unwrap();
        let (w, h) = (900u32, 700u32);
        let mut state = 12345u64;
        let mut grain = || {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((state >> 33) % 21) as i32 - 10
        };
        let photo = image::RgbImage::from_fn(w, h, |x, _| {
            let base = if (x / 24).is_multiple_of(2) { 70 } else { 180 };
            let v = (base + grain()).clamp(0, 255) as u8;
            image::Rgb([v, v, v])
        });
        // A disc of radius 90, feathered over 20 px.
        let mask = image::GrayImage::from_fn(w, h, |x, y| {
            let d = ((x as f32 - 450.0).powi(2) + (y as f32 - 350.0).powi(2)).sqrt();
            image::Luma([(255.0 * ((110.0 - d) / 20.0).clamp(0.0, 1.0)) as u8])
        });
        let out = super::fill_removal(
            &image::DynamicImage::ImageRgb8(photo.clone()),
            &mask,
            &session,
        )
        .unwrap();
        crate::register_exit_handler();
        let mut hole_dev = Vec::new();
        for (x, y, p) in out.enumerate_pixels() {
            let m = mask.get_pixel(x, y)[0];
            if m == 0 {
                assert_eq!(p[0], photo.get_pixel(x, y)[0], "kept at ({x}, {y})");
            } else if ((x as f32 - 450.0).powi(2) + (y as f32 - 350.0).powi(2)).sqrt() < 80.0 {
                let base = if (x / 24).is_multiple_of(2) {
                    70.0
                } else {
                    180.0
                };
                hole_dev.push((p[0] as f32 - base).abs());
            }
        }
        hole_dev.sort_by(f32::total_cmp);
        let median = hole_dev[hole_dev.len() / 2];
        // The stripes continue: most hole pixels near their stripe's level.
        assert!(
            median < 30.0,
            "median distance from the stripe level {median}"
        );
        // The grain is kept: the hole is not flat.
        let row: Vec<f32> = (430..470)
            .map(|x| out.get_pixel(x, 350)[0] as f32)
            .collect();
        assert!(
            row.windows(2).any(|p| (p[0] - p[1]).abs() >= 3.0),
            "no grain in the hole"
        );
    }
}

#[cfg(test)]
mod removal_timing {
    /// Times Remove's fill on real photos: `RAWTONIC_REMOVAL_CASES` lists
    /// `photo;mask` lines (8-bit or 16-bit PNG photo, grey mask of the same
    /// size as Remove's patch mask). Prints milliseconds per case. Needs the
    /// model in `RAWTONIC_INPAINT_MODEL` and ONNX Runtime in `ORT_DYLIB_PATH`.
    #[test]
    #[ignore]
    fn print_removal_fill_times() {
        let (Some(model), Some(cases)) = (
            std::env::var_os("RAWTONIC_INPAINT_MODEL"),
            std::env::var_os("RAWTONIC_REMOVAL_CASES"),
        ) else {
            return;
        };
        let session = rawtonic_onnx::Model::load(
            std::path::Path::new(&model),
            rawtonic_onnx::ModelKind::Inpaint,
        )
        .unwrap();
        for line in std::fs::read_to_string(cases).unwrap().lines() {
            let Some((photo, mask)) = line.split_once(';') else {
                continue;
            };
            let photo = image::DynamicImage::ImageRgb8(image::open(photo).unwrap().to_rgb8());
            let mask = image::open(mask).unwrap().to_luma8();
            let started = std::time::Instant::now();
            let guide_only = crate::ai_processing::run_inpainting(&photo, &mask, &session).unwrap();
            let migan_ms = started.elapsed().as_millis();
            drop(guide_only);
            let started = std::time::Instant::now();
            super::fill_removal(&photo, &mask, &session).unwrap();
            println!(
                "{}x{} {line}: MI-GAN alone {migan_ms} ms, Remove's fill {} ms",
                photo.width(),
                photo.height(),
                started.elapsed().as_millis()
            );
        }
        crate::register_exit_handler();
    }

    /// Remove's fill on RAW photos as the app runs it (sRGB-encoded decode):
    /// `RAWTONIC_REMOVAL_RAW_CASES` lists `raw;mask;x;y;out` lines (the grey
    /// mask PNG of a patch at `x`, `y`, e.g. a recipe's `patchData.mask`)
    /// and writes `out`-source/guide/fill PNGs of the patch's area; a
    /// `raw;out.png` line writes the photo as the fill sees it (input for
    /// `scripts/models/eval_inpaint.py`).
    #[test]
    #[ignore]
    fn write_removal_fill_on_raws() {
        let (Some(model), Some(cases)) = (
            std::env::var_os("RAWTONIC_INPAINT_MODEL"),
            std::env::var_os("RAWTONIC_REMOVAL_RAW_CASES"),
        ) else {
            return;
        };
        let session = rawtonic_onnx::Model::load(
            std::path::Path::new(&model),
            rawtonic_onnx::ModelKind::Inpaint,
        )
        .unwrap();
        // Never over an existing file (the outputs sit beside QA copies).
        let save = |img: image::DynamicImage, path: String| {
            let mut png = Vec::new();
            img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
                .unwrap();
            rawtonic_export::output::write_new_output(&path, png)
                .unwrap_or_else(|e| panic!("{path}: {e}"));
        };
        for line in std::fs::read_to_string(cases).unwrap().lines() {
            let f: Vec<&str> = line.split(';').collect();
            if f.len() != 2 && f.len() != 5 {
                continue;
            }
            let bytes = std::fs::read(f[0]).unwrap();
            let base = crate::image_loader::load_base_image_from_bytes(
                &bytes,
                f[0],
                false,
                &Default::default(),
                None,
            )
            .unwrap();
            let photo = super::apply_linear_to_srgb(base);
            if f.len() == 2 {
                save(
                    image::DynamicImage::ImageRgb8(photo.to_rgb8()),
                    f[1].to_string(),
                );
                continue;
            }
            let patch = image::open(f[1]).unwrap().to_luma8();
            let (px, py): (u32, u32) = (f[2].parse().unwrap(), f[3].parse().unwrap());
            let mut mask = image::GrayImage::new(photo.width(), photo.height());
            image::imageops::replace(&mut mask, &patch, px as i64, py as i64);
            let guide = crate::ai_processing::run_inpainting(&photo, &mask, &session).unwrap();
            let started = std::time::Instant::now();
            let fill = super::fill_removal(&photo, &mask, &session).unwrap();
            let (w, h) = patch.dimensions();
            let crop =
                |img: &image::RgbaImage| image::imageops::crop_imm(img, px, py, w, h).to_image();
            let source = crop(&photo.to_rgba8());
            for (name, img) in [
                ("source", source),
                ("guide", crop(&guide)),
                ("fill", crop(&fill)),
            ] {
                save(
                    image::DynamicImage::ImageRgba8(img),
                    format!("{}-{name}.png", f[4]),
                );
            }
            println!("{line}: {} ms", started.elapsed().as_millis());
        }
        crate::register_exit_handler();
    }

    /// Remove's large-hole fill as the app runs it (`fill_large_hole`
    /// without the app): Moebius from the pack's files in
    /// `RAWTONIC_MOEBIUS_MODELS`, on `RAWTONIC_MOEBIUS_CASES` lines
    /// `photo;mask;out` (8-bit sRGB photo, a mask of its size). Writes
    /// `out`-guide.png (the network's square) and `out`-fill.png (the crop
    /// `eval_inpaint.py` scores, 1.5 × the hole around it), never over a
    /// file; prints the hole's thickness, the times and the process's
    /// memory (current / peak: the physical footprint as Activity Monitor
    /// shows it on macOS, the resident set elsewhere). Windows and Linux
    /// also need ONNX Runtime in `ORT_DYLIB_PATH` with a GPU plugin beside
    /// it.
    #[test]
    #[ignore]
    fn moebius_fill_times() {
        let (Some(models), Some(cases)) = (
            std::env::var_os("RAWTONIC_MOEBIUS_MODELS"),
            std::env::var_os("RAWTONIC_MOEBIUS_CASES"),
        ) else {
            return;
        };
        let models = std::path::Path::new(&models);
        #[cfg(target_os = "macos")]
        let footprint = || {
            // SAFETY: plain data the kernel fills for this process.
            let mut info: libc::rusage_info_v4 = unsafe { std::mem::zeroed() };
            unsafe {
                libc::proc_pid_rusage(
                    std::process::id() as libc::c_int,
                    libc::RUSAGE_INFO_V4,
                    (&mut info as *mut libc::rusage_info_v4).cast(),
                )
            };
            format!(
                "{:.2} / {:.2} GB",
                info.ri_phys_footprint as f64 / 1e9,
                info.ri_lifetime_max_phys_footprint as f64 / 1e9
            )
        };
        #[cfg(not(target_os = "macos"))]
        let footprint = || {
            let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
            let kb = |key: &str| {
                status
                    .lines()
                    .find_map(|l| l.strip_prefix(key))
                    .and_then(|v| v.split_whitespace().next()?.parse::<f64>().ok())
                    .map_or("?".to_string(), |v| format!("{:.2}", v * 1024.0 / 1e9))
            };
            format!("{} / {} GB", kb("VmRSS:"), kb("VmHWM:"))
        };
        // The packages put together from the pack's flat files, as the app does.
        #[cfg(target_os = "macos")]
        let dir = {
            let dir = tempfile::tempdir().unwrap();
            std::fs::copy(
                models.join(crate::moebius::EMBEDDINGS_FILE.0),
                dir.path().join(crate::moebius::EMBEDDINGS_FILE.0),
            )
            .unwrap();
            for (package, files) in &crate::moebius::coreml::PACKAGES {
                for (file, inner, _) in files {
                    let to = dir.path().join(package).join(inner);
                    std::fs::create_dir_all(to.parent().unwrap()).unwrap();
                    std::fs::hard_link(models.join(file), &to).unwrap();
                }
            }
            dir
        };
        println!("before load: footprint {}", footprint());
        let started = std::time::Instant::now();
        #[cfg(target_os = "macos")]
        let model = crate::moebius::coreml::Moebius::load(dir.path()).unwrap();
        #[cfg(not(target_os = "macos"))]
        let model =
            crate::moebius::onnx::Moebius::load(models, &crate::moebius::onnx::gpu_providers())
                .unwrap();
        #[cfg(not(target_os = "macos"))]
        println!("on {}", model.provider());
        println!(
            "load (compile): {} ms, footprint {}",
            started.elapsed().as_millis(),
            footprint()
        );
        let save = |img: image::DynamicImage, path: String| {
            let mut png = Vec::new();
            img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
                .unwrap();
            rawtonic_export::output::write_new_output(&path, png)
                .unwrap_or_else(|e| panic!("{path}: {e}"));
        };
        for line in std::fs::read_to_string(cases).unwrap().lines() {
            let f: Vec<&str> = line.split(';').collect();
            if f.len() != 3 {
                continue;
            }
            let photo = image::open(f[0]).unwrap();
            let mask = image::open(f[1]).unwrap().to_luma8();
            let started = std::time::Instant::now();
            let (min_x, max_x, min_y, max_y) = super::calculate_mask_bounds(&mask).unwrap();
            let thickness = super::hole_thickness(&mask, (min_x, max_x, min_y, max_y));
            let hole_mask = super::binary_hole(&mask);
            let rgb = photo.to_rgb8();
            let measured = started.elapsed();
            let mut finished = Vec::new();
            let guided = crate::moebius::guide(
                &model,
                &rgb,
                &hole_mask,
                (min_x as u32, min_y as u32, max_x as u32, max_y as u32),
                &mut |done, _| {
                    if done > 0 {
                        finished.push(std::time::Instant::now());
                    }
                },
                &|| false,
            )
            .unwrap();
            let Some(guided) = guided else {
                println!("{}: the hole does not fit the square", f[2]);
                continue;
            };
            let guide_done = started.elapsed();
            let (_, _, side) = crate::moebius::crop_square(
                (min_x as u32, min_y as u32, max_x as u32, max_y as u32),
                photo.width(),
                photo.height(),
            )
            .unwrap();
            let fill = super::complete_with_guide(&photo, &hole_mask, guided.clone());
            let fill = super::keep_guide_shading(
                fill,
                &guided,
                &hole_mask,
                side as f32 / crate::moebius::SIZE as f32,
            );
            let total = started.elapsed();
            let (w, h) = mask.dimensions();
            let side = crate::moebius::crop_square(
                (min_x as u32, min_y as u32, max_x as u32, max_y as u32),
                w,
                h,
            )
            .unwrap();
            let square =
                image::imageops::crop_imm(&guided, side.0, side.1, side.2, side.2).to_image();
            save(
                image::DynamicImage::ImageRgba8(square),
                format!("{}-guide.png", f[2]),
            );
            let (mw, mh) = (max_x - min_x + 1, max_y - min_y + 1);
            let (px, py) = (128.max(mw * 3 / 2), 128.max(mh * 3 / 2));
            let (x0, y0) = (min_x.saturating_sub(px), min_y.saturating_sub(py));
            let (x1, y1) = (
                (max_x + px + 1).min(w as usize),
                (max_y + py + 1).min(h as usize),
            );
            let crop = image::imageops::crop_imm(
                &fill,
                x0 as u32,
                y0 as u32,
                (x1 - x0) as u32,
                (y1 - y0) as u32,
            );
            save(
                image::DynamicImage::ImageRgba8(crop.to_image()),
                format!("{}-fill.png", f[2]),
            );
            println!(
                "{}: {w}x{h}, hole {mw}x{mh}, thickness {thickness:.0} px: mask {} ms, Moebius {} ms ({} runs; to the first run's end {} ms, then {:.0} ms per run), completion {} ms, total {} ms; footprint {}",
                f[2],
                measured.as_millis(),
                (guide_done - measured).as_millis(),
                finished.len(),
                (finished[0] - started - measured).as_millis(),
                super::seconds_per_run(&finished).unwrap_or(0.0) * 1000.0,
                (total - guide_done).as_millis(),
                total.as_millis(),
                footprint()
            );
        }
    }

    /// A completion that copied a darker block into a smooth guide takes
    /// the guide's shade back but keeps its own fine texture; nothing
    /// outside the hole changes.
    #[test]
    fn keeps_the_guides_shading_and_the_completions_texture() {
        let (w, h) = (200u32, 160u32);
        let hole = image::GrayImage::from_fn(w, h, |x, y| {
            image::Luma([if (40..160).contains(&x) && (30..130).contains(&y) {
                255
            } else {
                0
            }])
        });
        let guide = image::RgbaImage::from_pixel(w, h, image::Rgba([60, 80, 50, 255]));
        // Grain ±4 everywhere; a block 20 codes darker in the hole.
        let grain = |x: u32, y: u32| if (x + y).is_multiple_of(2) { 4i32 } else { -4 };
        let filled = image::RgbaImage::from_fn(w, h, |x, y| {
            let dark = if (60..110).contains(&x) && (50..100).contains(&y) {
                20
            } else {
                0
            };
            let v = |c: i32| (c - dark + grain(x, y)) as u8;
            image::Rgba([v(60), v(80), v(50), 255])
        });
        let out = super::keep_guide_shading(filled.clone(), &guide, &hole, 4.0);
        // The block's centre is back at the guide's shade, grain kept.
        let p = out.get_pixel(85, 75).0;
        assert!((p[1] as i32 - 80 - grain(85, 75)).abs() <= 2, "{p:?}");
        let q = out.get_pixel(86, 75).0;
        assert!(
            (p[1] as i32 - q[1] as i32).abs() >= 6,
            "grain kept: {p:?} {q:?}"
        );
        for (x, y) in [(5, 5), (199, 159), (39, 80)] {
            assert_eq!(out.get_pixel(x, y), filled.get_pixel(x, y));
        }
    }

    #[test]
    fn seconds_per_run_leaves_out_the_first_run() {
        let start = std::time::Instant::now();
        let at = |ms: u64| start + std::time::Duration::from_millis(ms);
        assert_eq!(super::seconds_per_run(&[]), None);
        assert_eq!(super::seconds_per_run(&[at(900)]), None);
        // The first run ends at 0.9 s (encoder, compilation); then 0.4 s each.
        let per_run = super::seconds_per_run(&[at(900), at(1300), at(1700), at(2100)]).unwrap();
        assert!((per_run - 0.4).abs() < 1e-9, "{per_run}");
    }

    #[test]
    fn hole_thickness_is_the_widest_disc_inside() {
        // A disc of radius 150 and a 4000 px long, 20 px wide line.
        let mut mask = image::GrayImage::new(5000, 1000);
        for y in 0..1000u32 {
            for x in 0..5000u32 {
                let (dx, dy) = (x as f32 - 400.0, y as f32 - 400.0);
                if dx * dx + dy * dy <= 150.0 * 150.0
                    || ((900..4900).contains(&x) && (800..820).contains(&y))
                {
                    mask.put_pixel(x, y, image::Luma([255]));
                }
            }
        }
        let bounds = super::calculate_mask_bounds(&mask).unwrap();
        // The grid is 5 px coarse here (4650 px across in at most 1024 cells).
        let t = super::hole_thickness(&mask, bounds);
        assert!((t - 300.0).abs() <= 12.0, "{t}");
        let line = image::imageops::crop_imm(&mask, 600, 0, 4400, 1000).to_image();
        let t = super::hole_thickness(&line, super::calculate_mask_bounds(&line).unwrap());
        assert!((t - 20.0).abs() <= 10.0, "{t}");
    }

    /// Fill edges on an uncropped panorama DNG (`RAWTONIC_FILL_PANO`; its
    /// empty edges are black): writes the panorama with the edge patches
    /// composited as at export to `RAWTONIC_FILL_OUT` (PNG) and prints the
    /// patches and time.
    #[test]
    #[ignore]
    fn fill_edges_on_a_panorama() {
        let (Some(pano), Some(out)) = (
            std::env::var_os("RAWTONIC_FILL_PANO"),
            std::env::var_os("RAWTONIC_FILL_OUT"),
        ) else {
            return;
        };
        let frame =
            rawtonic_merge::frame::load_frame(std::path::Path::new(&pano), &Default::default())
                .unwrap();
        // The DNG's empty edges decode near, not exactly, zero; the app
        // passes the stitch's own coverage instead.
        let covered: Vec<bool> = frame
            .image
            .pixels()
            .map(|p| p.0.iter().any(|v| v.abs() > 1e-4))
            .collect();
        let started = std::time::Instant::now();
        let patches = super::fill_edge_patches(&frame.image, &covered).unwrap();
        println!(
            "{} patches, {} empty pixels of {}, {} ms",
            patches.len(),
            covered.iter().filter(|c| !**c).count(),
            covered.len(),
            started.elapsed().as_millis()
        );
        let filled = rawtonic_engine::image_loader::composite_ai_patches(
            &image::DynamicImage::ImageRgb32F(frame.image),
            &serde_json::json!({ "aiPatches": patches }),
        )
        .unwrap();
        assert!(!rawtonic_engine::image_loader::has_generated_areas(
            &serde_json::json!({ "aiPatches": patches })
        ));
        // `RAWTONIC_FILL_SIZE`: the long side of the PNG (default 1600).
        let size = std::env::var("RAWTONIC_FILL_SIZE")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(1600);
        let preview = rawtonic_merge::hdr::preview(&filled.into_rgb32f(), size);
        preview.save(std::path::Path::new(&out)).unwrap();
        crate::register_exit_handler();
    }
}

/// Fill edges of a panorama (merge dialog): the empty areas a panorama
/// keeps without auto crop, completed from the panorama's own pixels
/// (patch-based, no model), one patch per connected empty area so the fill
/// stays editable. No model makes the pixels, so the patches are marked
/// `"generated": false` and exports carry no generated-content marking.
/// `image` is the panorama in linear working
/// RGB, `covered` its covered pixels. Returns the patches for the recipe's
/// `aiPatches`.
pub(crate) fn fill_edge_patches(
    image: &image::Rgb32FImage,
    covered: &[bool],
) -> anyhow::Result<Vec<Value>> {
    let (w, h) = (image.width() as usize, image.height() as usize);
    // The outermost pixels of a warped frame are darkened by interpolation
    // with the empty canvas: they are filled too.
    let empty: Vec<bool> = covered.iter().map(|c| !c).collect();
    let covered: Vec<bool> = near_mask(w, h, &empty, 2).iter().map(|n| !n).collect();
    let covered = &covered[..];
    // Empty areas on a coarse grid, 4-connected.
    let f = (w.max(h) / 1000).max(1);
    let (gw, gh) = (w.div_ceil(f), h.div_ceil(f));
    let mut empty = vec![false; gw * gh];
    for y in 0..h {
        for x in 0..w {
            if !covered[y * w + x] {
                empty[(y / f) * gw + x / f] = true;
            }
        }
    }
    let mut label = vec![usize::MAX; gw * gh];
    let mut boxes: Vec<(usize, usize, usize, usize)> = Vec::new();
    for start in 0..gw * gh {
        if !empty[start] || label[start] != usize::MAX {
            continue;
        }
        let id = boxes.len();
        let mut b = (usize::MAX, usize::MAX, 0, 0);
        let mut stack = vec![start];
        label[start] = id;
        while let Some(k) = stack.pop() {
            let (x, y) = (k % gw, k / gw);
            b = (b.0.min(x), b.1.min(y), b.2.max(x), b.3.max(y));
            let mut push = |n: usize| {
                if empty[n] && label[n] == usize::MAX {
                    label[n] = id;
                    stack.push(n);
                }
            };
            if x > 0 {
                push(k - 1);
            }
            if x + 1 < gw {
                push(k + 1);
            }
            if y > 0 {
                push(k - gw);
            }
            if y + 1 < gh {
                push(k + gw);
            }
        }
        boxes.push(b);
    }
    let to_srgb8 = |v: f32| {
        let v = v.clamp(0.0, 1.0);
        let e = if v <= 0.003_130_8 {
            v * 12.92
        } else {
            1.055 * v.powf(1.0 / 2.4) - 0.055
        };
        (e * 255.0).round() as u8
    };
    // Each area is completed from the panorama's own pixels around it
    // (patch-based, unguided: MI-GAN's guide turned dark and blotchy at the
    // image border, where it has to extend outwards); every empty pixel is
    // a hole, so an empty area never serves as the source of another.
    let raw: &[f32] = image;
    let source = RgbImage::from_fn(w as u32, h as u32, |x, y| {
        let p = &raw[(y as usize * w + x as usize) * 3..][..3];
        Rgb([to_srgb8(p[0]), to_srgb8(p[1]), to_srgb8(p[2])])
    });
    let mut patches = Vec::new();
    for (id, (gx0, gy0, gx1, gy1)) in boxes.into_iter().enumerate() {
        let (x0, y0) = (gx0 * f, gy0 * f);
        let (x1, y1) = (((gx1 + 1) * f).min(w), ((gy1 + 1) * f).min(h));
        let (bw, bh) = (x1 - x0, y1 - y0);
        let in_area = |x: usize, y: usize| !covered[y * w + x] && label[(y / f) * gw + x / f] == id;
        // Sources: known pixels at most `band` from the area (¾ of its
        // thickness), so an edge continues what is next to it. A bounding
        // box took in soft background further away and filled a sharp
        // meadow smooth; a band of 128 px on a 500 px thick edge left the
        // coarse levels too few sources and copied jeans into the grass.
        let band = (bw.min(bh) * 3 / 4).clamp(64, 384);
        let pad = band + 8;
        let (rx0, ry0) = (x0.saturating_sub(pad), y0.saturating_sub(pad));
        let (rx1, ry1) = ((x1 + pad).min(w), (y1 + pad).min(h));
        let (rw, rh) = (rx1 - rx0, ry1 - ry0);
        let mut region = Vec::with_capacity(rw * rh);
        let mut hole = Vec::with_capacity(rw * rh);
        for y in ry0..ry1 {
            for x in rx0..rx1 {
                region.push(
                    source
                        .get_pixel(x as u32, y as u32)
                        .0
                        .map(|v| v as f32 / 255.0),
                );
                hole.push(!covered[y * w + x]);
            }
        }
        let near = near_mask(rw, rh, &hole, band);
        let sources: Vec<bool> = near.iter().zip(&hole).map(|(&n, &h)| n && !h).collect();
        let filled = rawtonic_engine::patch_fill::fill_from(
            rw,
            rh,
            &region,
            &hole,
            Some(&sources),
            None,
            &rawtonic_engine::patch_fill::PatchFillParams {
                patch_radius: 4,
                texture_weight: 10.0,
                fine_extent: 0,
                sharp_finish: true,
                ..Default::default()
            },
        );
        let filled = blend_into_surroundings(rw, rh, &region, &hole, filled);
        let color = RgbImage::from_fn(bw as u32, bh as u32, |x, y| {
            let (cx, cy) = (x0 + x as usize, y0 + y as usize);
            if in_area(cx, cy) {
                let v = filled[(cy - ry0) * rw + cx - rx0];
                Rgb(v.map(|c| (c * 255.0).round().clamp(0.0, 255.0) as u8))
            } else {
                *source.get_pixel(cx as u32, cy as u32)
            }
        });
        let area_mask = image::GrayImage::from_fn(bw as u32, bh as u32, |x, y| {
            image::Luma([if in_area(x0 + x as usize, y0 + y as usize) {
                255
            } else {
                0
            }])
        });
        let data = encode_patch_result(
            &color, &area_mask, x0 as u32, y0 as u32, bw as u32, bh as u32, true, 95, true, None,
        )
        .map_err(anyhow::Error::msg)?;
        patches.push(serde_json::json!({
            "id": uuid::Uuid::new_v4().to_string(),
            "name": "Fill edges",
            "generated": false,
            "visible": true,
            "invert": false,
            "prompt": "",
            "isLoading": false,
            "subMasks": [],
            "patchData": serde_json::from_str::<Value>(&data)?,
        }));
    }
    Ok(patches)
}

/// Pixels within `d` (Chebyshev distance) of a set pixel of `mask`.
fn near_mask(w: usize, h: usize, mask: &[bool], d: usize) -> Vec<bool> {
    let mut sum = vec![0u32; (w + 1) * (h + 1)];
    for y in 0..h {
        let mut row = 0u32;
        for x in 0..w {
            row += mask[y * w + x] as u32;
            sum[(y + 1) * (w + 1) + x + 1] = sum[y * (w + 1) + x + 1] + row;
        }
    }
    (0..w * h)
        .map(|i| {
            let (x, y) = (i % w, i / w);
            let (xa, ya) = (x.saturating_sub(d), y.saturating_sub(d));
            let (xb, yb) = ((x + d + 1).min(w), (y + d + 1).min(h));
            sum[yb * (w + 1) + xb] + sum[ya * (w + 1) + xa]
                > sum[ya * (w + 1) + xb] + sum[yb * (w + 1) + xa]
        })
        .collect()
}

/// Width, height, weighted values and weights of one push-pull level.
type PyramidLevel = (usize, usize, Vec<[f32; 3]>, Vec<f32>);

/// Removes the step where a completed hole meets its surroundings: the
/// difference between each hole pixel on the border and its known
/// neighbours is spread smoothly over the hole (push-pull interpolation)
/// and added, as seamless cloning does.
fn blend_into_surroundings(
    w: usize,
    h: usize,
    known: &[[f32; 3]],
    hole: &[bool],
    mut filled: Vec<[f32; 3]>,
) -> Vec<[f32; 3]> {
    // Mismatch samples on the hole's border.
    let mut value = vec![[0.0f32; 3]; w * h];
    let mut weight = vec![0.0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            let k = y * w + x;
            if !hole[k] {
                continue;
            }
            let (mut sum, mut n) = ([0.0f32; 3], 0.0f32);
            for (dx, dy) in [(-1i64, 0i64), (1, 0), (0, -1), (0, 1)] {
                let (nx, ny) = (x as i64 + dx, y as i64 + dy);
                if nx < 0 || ny < 0 || nx >= w as i64 || ny >= h as i64 {
                    continue;
                }
                let q = ny as usize * w + nx as usize;
                if !hole[q] {
                    for c in 0..3 {
                        sum[c] += known[q][c] - filled[k][c];
                    }
                    n += 1.0;
                }
            }
            if n > 0.0 {
                value[k] = sum.map(|v| v / n);
                weight[k] = 1.0;
            }
        }
    }
    // Push-pull: average down a pyramid, fill the gaps from coarser levels.
    let mut levels: Vec<PyramidLevel> = vec![(w, h, value, weight)];
    while {
        let (lw, lh, _, _) = &levels[levels.len() - 1];
        *lw > 1 || *lh > 1
    } {
        let (lw, lh, v, wt) = &levels[levels.len() - 1];
        let (nw, nh) = (lw.div_ceil(2), lh.div_ceil(2));
        let mut nv = vec![[0.0f32; 3]; nw * nh];
        let mut nwt = vec![0.0f32; nw * nh];
        for y in 0..*lh {
            for x in 0..*lw {
                let (k, q) = (y * lw + x, (y / 2) * nw + x / 2);
                for c in 0..3 {
                    nv[q][c] += v[k][c] * wt[k];
                }
                nwt[q] += wt[k];
            }
        }
        for q in 0..nw * nh {
            if nwt[q] > 0.0 {
                nv[q] = nv[q].map(|c| c / nwt[q]);
                nwt[q] = nwt[q].min(1.0);
            }
        }
        levels.push((nw, nh, nv, nwt));
    }
    for l in (0..levels.len() - 1).rev() {
        let (upper, lower) = levels.split_at_mut(l + 1);
        let (lw, lh, v, wt) = &mut upper[l];
        let (cw, ch, cv, _) = &lower[0];
        let (cw, ch) = (*cw, *ch);
        let sample = |fx: f32, fy: f32| -> [f32; 3] {
            let fx = fx.clamp(0.0, (cw - 1) as f32);
            let fy = fy.clamp(0.0, (ch - 1) as f32);
            let (x0, y0) = (fx.floor() as usize, fy.floor() as usize);
            let (x1, y1) = ((x0 + 1).min(cw - 1), (y0 + 1).min(ch - 1));
            let (tx, ty) = (fx - x0 as f32, fy - y0 as f32);
            let at = |x: usize, y: usize| cv[y * cw + x];
            std::array::from_fn(|c| {
                (at(x0, y0)[c] * (1.0 - tx) + at(x1, y0)[c] * tx) * (1.0 - ty)
                    + (at(x0, y1)[c] * (1.0 - tx) + at(x1, y1)[c] * tx) * ty
            })
        };
        for y in 0..*lh {
            for x in 0..*lw {
                let k = y * *lw + x;
                let coarse = sample((x as f32 + 0.5) / 2.0 - 0.5, (y as f32 + 0.5) / 2.0 - 0.5);
                let a = wt[k].min(1.0);
                for c in 0..3 {
                    v[k][c] = v[k][c] * a + coarse[c] * (1.0 - a);
                }
                wt[k] = 1.0;
            }
        }
    }
    let field = &levels[0].2;
    for (k, p) in filled.iter_mut().enumerate() {
        if hole[k] {
            for c in 0..3 {
                p[c] = (p[c] + field[k][c]).clamp(0.0, 1.0);
            }
        }
    }
    filled
}
