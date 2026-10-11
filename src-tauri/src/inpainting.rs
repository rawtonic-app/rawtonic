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

    let result_json = serde_json::json!({
        "color": color_base64,
        "mask": mask_base64,
        "offsetX": offset_x,
        "offsetY": offset_y,
        "width": width,
        "height": height,
        "isSrgbEncoded": is_srgb
    })
    .to_string();

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
    let (w, h) = mask.dimensions();
    let hole_mask = image::GrayImage::from_fn(w, h, |x, y| {
        image::Luma([if mask.get_pixel(x, y)[0] > 0 { 255 } else { 0 }])
    });
    let guided = ai_processing::run_inpainting(source, &hole_mask, model)?;
    let Ok((min_x, max_x, min_y, max_y)) = calculate_mask_bounds(&hole_mask) else {
        return Ok(guided);
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
    Ok(out)
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
        adjustments: serde_json::Value::Null,
        sub_masks: patch_definition.sub_masks.clone(),
    };

    let (mask_source, source_is_raw) = crate::editor_preview::get_original_image(&state)?;
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
    let patch_rgba = if use_fast_inpaint {
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

        fill_removal(&source_image, &mask_bitmap, &inpaint_model).map_err(|e| e.to_string())?
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

        if let Some(token) = cancel_flag.as_ref() {
            tokio::select! {
                res = inpaint_fut => res.map_err(|e| e.to_string())?,
                _ = token.wait_for_cancel() => {
                    return Err("Task cancelled".to_string());
                }
            }
        } else {
            inpaint_fut.await.map_err(|e| e.to_string())?
        }
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
            &color, &area_mask, x0 as u32, y0 as u32, bw as u32, bh as u32, true, 95, true,
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
