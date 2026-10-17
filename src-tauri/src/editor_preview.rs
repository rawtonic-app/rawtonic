//! The open photograph in Develop: its decoded source and warped/patched
//! variants, the interactive preview pipeline (workers, jobs, native
//! presentation) and the commands that drive it. Moved out of `lib.rs`.
use crate::adjustment_utils::apply_all_transformations;
use crate::adjustment_utils::hydrate_adjustments;
use crate::app_settings::load_settings;
use crate::app_state::AnalyticsJob;
use crate::app_state::AppState;
use crate::app_state::CachedPreview;
use crate::app_state::LoadedImage;
use crate::app_state::PreviewJob;
use crate::cache_utils::calculate_full_job_hash;
use crate::cache_utils::calculate_geometry_hash;
use crate::cache_utils::calculate_transform_hash;
use crate::cache_utils::calculate_visual_hash;
use crate::file_management::parse_virtual_path;
use crate::file_management::read_file_mapped;
use crate::formats::is_raw_file;
use crate::gpu_processing;
use crate::image_loader::composite_patches_on_image;
use crate::image_processing;
use crate::image_processing::RenderRequest;
use crate::image_processing::apply_coarse_rotation;
use crate::image_processing::apply_flip;
use crate::image_processing::apply_geometry_warp;
use crate::image_processing::downscale_f32_image;
use crate::image_processing::get_all_adjustments_from_json;
use crate::image_processing::get_or_init_gpu_context;
use crate::image_processing::process_and_get_dynamic_image;
use crate::image_processing::resolve_tonemapper_override;
use crate::image_processing::resolve_tonemapper_override_from_handle;
use crate::image_processing::warp_image_geometry;
use crate::lut_processing;
use crate::mask_generation::MaskDefinition;
use crate::mask_generation::get_cached_or_generate_mask;
use crate::mask_generation::prepare_parametric_mask_source;
use crate::mask_generation::resolve_warped_image_for_masks;
use crate::mask_generation::{PhotoGeometry, generate_mask_bitmap};
use crate::preview_cache;
use base64::Engine as _;
use base64::engine::general_purpose;
use image::DynamicImage;
use image::GenericImageView;
#[cfg(not(target_os = "linux"))]
use imgref::ImgRef;
use mozjpeg_rs::Encoder;
use mozjpeg_rs::Preset;
use rawtonic_engine::formats;
#[cfg(not(target_os = "linux"))]
use rgb::FromSlice;
#[cfg(not(target_os = "linux"))]
use rgb::RGBA8;
use serde_json::Value;
use std::borrow::Cow;
use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use tauri::ipc::Response;
use tauri::{Emitter, Manager};

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WgpuTransformPayload {
    pub window_width: f32,
    pub window_height: f32,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub clip_x: f32,
    pub clip_y: f32,
    pub clip_width: f32,
    pub clip_height: f32,
    pub bg_primary: [f32; 4],
    pub bg_secondary: [f32; 4],
    pub pixelated: bool,
    /// Placement of the uncropped crop-editing frame (x, y, w, h) and its
    /// clockwise straighten angle in degrees.
    #[serde(default)]
    pub crop_rect: Option<[f32; 4]>,
    #[serde(default)]
    pub crop_rotation: f32,
    /// Keep showing the current frame where it is; this placement applies
    /// with the next frame bound (the next photograph's first frame).
    #[serde(default)]
    pub hold: bool,
    /// The viewer's area while the photograph is shown to fit (see
    /// `WgpuDisplay::viewport`).
    #[serde(default)]
    pub viewport: Option<[f32; 4]>,
    /// The before/after split's divider, screen x in device pixels (the unit
    /// of `x`); `None` while the split view is off.
    #[serde(default)]
    pub split: Option<f32>,
}

/// Recipe keys that place the photograph in the frame. The split's before
/// keeps them from the edit (`originalPreviewAdjustments` in
/// src/utils/beforeAfter.ts), so both sides show the same framing.
const SPLIT_FRAMING_KEYS: &[&str] = &[
    "crop",
    "rotation",
    "flipHorizontal",
    "flipVertical",
    "orientationSteps",
    "aspectRatio",
];

/// The photograph and framing of a frame: a split before is drawn only
/// beside frames with its key, never beside another crop or photograph.
pub(crate) fn split_frame_key(path: &str, adjustments: &serde_json::Value) -> u64 {
    let mut hasher = DefaultHasher::new();
    path.hash(&mut hasher);
    for key in SPLIT_FRAMING_KEYS
        .iter()
        .chain(crate::cache_utils::GEOMETRY_KEYS)
    {
        key.hash(&mut hasher);
        adjustments
            .get(*key)
            .map(|value| value.to_string())
            .unwrap_or_default()
            .hash(&mut hasher);
    }
    hasher.finish()
}

pub fn generate_transformed_preview(
    state: &tauri::State<AppState>,
    loaded_image: &LoadedImage,
    adjustments: &serde_json::Value,
    preview_dim: u32,
) -> Result<(DynamicImage, f32, (f32, f32)), String> {
    let preview = generate_shared_transformed_preview(
        state,
        loaded_image,
        adjustments,
        preview_dim,
        preview_cache::preparation_key(adjustments),
    )?;
    let image = Arc::try_unwrap(preview.image).unwrap_or_else(|image| (*image).clone());
    Ok((image, preview.scale, preview.unscaled_crop_offset))
}

pub(crate) fn source_transform_hash(source: &Arc<DynamicImage>, transform_hash: u64) -> u64 {
    let mut hash = DefaultHasher::new();
    Arc::as_ptr(source).hash(&mut hash);
    transform_hash.hash(&mut hash);
    hash.finish()
}

/// Fit-size previews need far fewer pixels than the source: below this share
/// of the source's linear size the preview is warped and transformed from a
/// reduced copy (geometry is resolution-independent), above it from the
/// full-resolution source as before.
pub(crate) const REDUCED_PREVIEW_MAX_SCALE: f32 = 0.7;

/// Resolution kept above the preview size before the last downscale, so the
/// warp's bilinear sampling does not soften the fit view.
pub(crate) const REDUCED_PREVIEW_HEADROOM: f32 = 1.25;

/// The scale (preview source px per full-resolution px) at which the cropped
/// frame still covers `preview_dim`, or None when the preview needs (nearly)
/// full resolution or has full-resolution-only steps (AI patches, spots, lens
/// blur; face retouch scales).
pub(crate) fn reduced_preview_scale(
    full_width: u32,
    full_height: u32,
    adjustments: &serde_json::Value,
    preview_dim: u32,
) -> Option<f32> {
    let has_patches = crate::image_loader::retouch_needs_full_resolution(adjustments);
    if has_patches || adjustments["lensBlurEnabled"].as_bool().unwrap_or(false) {
        return None;
    }
    let crop: Option<image_processing::Crop> =
        serde_json::from_value(adjustments["crop"].clone()).ok();
    let longest = match crop {
        Some(c) if c.width > 0.0 && c.height > 0.0 => c.width.max(c.height) as f32,
        _ => full_width.max(full_height) as f32,
    };
    let scale = preview_dim as f32 * REDUCED_PREVIEW_HEADROOM / longest.max(1.0);
    (scale <= REDUCED_PREVIEW_MAX_SCALE).then_some(scale)
}

/// A reduced preview's automatic lateral CA is the one measured on the
/// full-resolution image, as its export renders it
/// (`lateral_ca::share_estimate`).
fn share_lateral_ca(full: &DynamicImage, reduced: &DynamicImage, adjustments: &serde_json::Value) {
    let params = crate::image_processing::get_geometry_params_from_json(adjustments);
    if !params.auto_ca || params.auto_ca_version < 2 {
        return;
    }
    if let (Some(full), Some(reduced)) = (full.as_rgb32f(), reduced.as_rgb32f())
        && full.dimensions() != reduced.dimensions()
    {
        rawtonic_engine::lateral_ca::share_estimate(
            full,
            reduced,
            params.auto_ca_version,
            crate::image_processing::lens_ca_of(&params),
        );
    }
}

/// The preview base from `source`, a copy of the full-resolution image at
/// `source_scale`: resampled to `scale` (if smaller), warped, cropped and
/// rotated with the crop scaled accordingly. The returned GPU scale and crop
/// offset refer to full-resolution pixels, as for a full-resolution preview.
/// Retouching (patches, spots) and lens blur are applied at this resolution:
/// recipes with them reach this only for an offline photograph's Standard
/// preview (online they use the full decode).
pub(crate) fn reduced_preview_base(
    source: &DynamicImage,
    source_scale: f32,
    full_width: u32,
    full_height: u32,
    adjustments: &serde_json::Value,
    preview_dim: u32,
    scale: f32,
) -> Result<preview_cache::PreviewBase, String> {
    let scale = scale.min(source_scale);
    let width = ((full_width as f32 * scale).round() as u32).max(1);
    let height = ((full_height as f32 * scale).round() as u32).max(1);
    let reduced = if width < source.width() || height < source.height() {
        Cow::Owned(downscale_f32_image(source, width, height))
    } else {
        Cow::Borrowed(source)
    };
    // The actual scale after rounding to whole pixels.
    let scale = reduced.width() as f32 / full_width as f32;
    let mut scaled = adjustments.clone();
    if let Some(crop) = scaled.get_mut("crop").filter(|c| c.is_object()) {
        for key in ["x", "y", "width", "height"] {
            if let Some(value) = crop[key].as_f64() {
                crop[key] = serde_json::json!(value * scale as f64);
            }
        }
    }
    let retouched = if crate::image_loader::has_base_retouch(adjustments) {
        Cow::Owned(
            crate::image_loader::composite_patches_on_scaled_image(&reduced, adjustments, scale)
                .map_err(|e| format!("Failed to composite retouching: {e}"))?,
        )
    } else {
        reduced
    };
    if source_scale >= 1.0 {
        share_lateral_ca(source, &retouched, adjustments);
    }
    let warped = apply_geometry_warp(retouched, &scaled);
    let warped = rawtonic_engine::edge_fringe::apply_for_recipe(warped, adjustments, scale);
    let blurred = crate::lens_blur::apply_lens_blur(warped, adjustments);
    let (transformed, _) = crate::adjustment_utils::apply_spatial_transformations(blurred, &scaled);
    let transformed = transformed.into_owned();
    let unscaled_crop_offset =
        serde_json::from_value::<image_processing::Crop>(adjustments["crop"].clone())
            .map_or((0.0, 0.0), |c| (c.x as f32, c.y as f32));
    let reduced_width = transformed.width().max(1);
    let image = shared_preview_at_resolution(&Arc::new(transformed), preview_dim);
    Ok(preview_cache::PreviewBase {
        scale: image.width() as f32 * scale / reduced_width as f32,
        image,
        unscaled_crop_offset,
    })
}

pub(crate) fn generate_shared_transformed_preview(
    state: &tauri::State<AppState>,
    loaded_image: &LoadedImage,
    adjustments: &serde_json::Value,
    preview_dim: u32,
    preparation_key: u64,
) -> Result<preview_cache::PreviewBase, String> {
    let (full_width, full_height) = loaded_image.image.dimensions();
    // Profiling builds can force the full-resolution path for paired runs.
    #[cfg(feature = "performance-profiling")]
    let reduced_allowed = std::env::var_os("RAWTONIC_FULL_RES_PREVIEW").is_none();
    #[cfg(not(feature = "performance-profiling"))]
    let reduced_allowed = true;
    if let Some(scale) = reduced_preview_scale(full_width, full_height, adjustments, preview_dim)
        .filter(|_| reduced_allowed)
    {
        #[cfg(feature = "performance-profiling")]
        let _stage = crate::performance::PerformanceStage::new("preview_reduced_base");
        return reduced_preview_base(
            &loaded_image.image,
            1.0,
            full_width,
            full_height,
            adjustments,
            preview_dim,
            scale,
        );
    }
    let source_hash = source_transform_hash(&loaded_image.image, preparation_key);

    let (transformed_full_res, unscaled_crop_offset) = {
        let mut cache_lock = state
            .full_transformed_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some((hash, img, offset)) = cache_lock.as_ref() {
            if *hash == source_hash {
                (Arc::clone(img), *offset)
            } else {
                let (arc_img, offset) =
                    compute_full_transformed_res(state, loaded_image, adjustments)?;
                *cache_lock = Some((source_hash, Arc::clone(&arc_img), offset));
                (arc_img, offset)
            }
        } else {
            let (arc_img, offset) = compute_full_transformed_res(state, loaded_image, adjustments)?;
            *cache_lock = Some((source_hash, Arc::clone(&arc_img), offset));
            (arc_img, offset)
        }
    };

    let full_res_w = transformed_full_res.width();

    let final_preview_base = shared_preview_at_resolution(&transformed_full_res, preview_dim);

    let scale_for_gpu = if full_res_w > 0 {
        final_preview_base.width() as f32 / full_res_w as f32
    } else {
        1.0
    };

    Ok(preview_cache::PreviewBase {
        image: final_preview_base,
        scale: scale_for_gpu,
        unscaled_crop_offset,
    })
}

/// The longest side a Develop preview is built at: the requested size within
/// the GPU's texture limit. Display frames render from one texture, so a
/// photo larger than the limit (a wide panorama at 100 %) is shown
/// downsampled to it instead of failing.
pub(crate) fn preview_dim_within_limit(requested: u32, max_texture_dimension: u32) -> u32 {
    requested.min(max_texture_dimension)
}

pub(crate) fn shared_preview_at_resolution(
    source: &Arc<DynamicImage>,
    preview_dim: u32,
) -> Arc<DynamicImage> {
    if source.width() > preview_dim || source.height() > preview_dim {
        Arc::new(downscale_f32_image(source, preview_dim, preview_dim))
    } else {
        Arc::clone(source)
    }
}

pub(crate) fn prepare_interactive_preview(
    final_preview: &Arc<DynamicImage>,
    preview_dim: u32,
    divisor: f32,
    is_interactive: bool,
    cached: Option<&Arc<DynamicImage>>,
) -> Option<Arc<DynamicImage>> {
    if let Some(cached) = cached {
        return Some(Arc::clone(cached));
    }
    // A 100% zoom/final frame never uses the reduced interactive input.
    // Defer that potentially full-RAW-sized resampling until a drag needs it.
    if !is_interactive {
        return None;
    }
    if divisor <= 1.0 {
        return Some(Arc::clone(final_preview));
    }
    let target_size = (preview_dim as f32 / divisor) as u32;
    let (width, height) = final_preview.dimensions();
    let (small_width, small_height) = if width > height {
        let ratio = height as f32 / width as f32;
        (target_size, (target_size as f32 * ratio) as u32)
    } else {
        let ratio = width as f32 / height as f32;
        ((target_size as f32 * ratio) as u32, target_size)
    };
    Some(Arc::new(downscale_f32_image(
        final_preview,
        small_width,
        small_height,
    )))
}

pub(crate) fn compute_full_transformed_res(
    state: &tauri::State<AppState>,
    loaded_image: &LoadedImage,
    adjustments: &serde_json::Value,
) -> Result<(Arc<DynamicImage>, (f32, f32)), String> {
    let geo_hash = source_transform_hash(
        &loaded_image.image,
        crate::cache_utils::calculate_patched_warped_hash(adjustments),
    );

    let warped_arc = {
        let mut cache_lock = state
            .patched_warped_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        if let Some((hash, img)) = cache_lock.as_ref() {
            if *hash == geo_hash {
                Arc::clone(img)
            } else {
                let new_img = compute_patched_and_warped(loaded_image, adjustments)?;
                *cache_lock = Some((geo_hash, Arc::clone(&new_img)));
                new_img
            }
        } else {
            let new_img = compute_patched_and_warped(loaded_image, adjustments)?;
            *cache_lock = Some((geo_hash, Arc::clone(&new_img)));
            new_img
        }
    };

    let (transformed_img, offset) = crate::adjustment_utils::apply_spatial_transformations(
        Cow::Borrowed(warped_arc.as_ref()),
        adjustments,
    );

    let image = match transformed_img {
        Cow::Borrowed(_) => Arc::clone(&warped_arc),
        Cow::Owned(image) => Arc::new(image),
    };
    Ok((image, offset))
}

pub(crate) fn compute_patched_and_warped(
    loaded_image: &LoadedImage,
    adjustments: &serde_json::Value,
) -> Result<Arc<DynamicImage>, String> {
    let has_patches = crate::image_loader::has_base_retouch(adjustments);

    let patched_image = if has_patches {
        Cow::Owned(
            composite_patches_on_image(&loaded_image.image, adjustments)
                .map_err(|e| format!("Failed to composite AI patches: {}", e))?,
        )
    } else {
        Cow::Borrowed(loaded_image.image.as_ref())
    };

    let warped = apply_geometry_warp(patched_image, adjustments);
    let warped = rawtonic_engine::edge_fringe::apply_for_recipe(warped, adjustments, 1.0);
    let blurred = crate::lens_blur::apply_lens_blur(warped, adjustments);

    // No-op geometry/blur/fringe pass returns the input borrow. Retain the shared buffer
    // instead of duplicating hundreds of MB solely to populate another cache.
    Ok(match blurred {
        Cow::Borrowed(_) => Arc::clone(&loaded_image.image),
        Cow::Owned(image) => Arc::new(image),
    })
}

#[tauri::command]
pub(crate) fn cancel_thumbnail_generation(
    state: tauri::State<AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    state
        .thumbnail_cancellation_token
        .store(true, Ordering::SeqCst);

    let mut tracker = state.thumbnail_progress.lock().unwrap();
    tracker.total = 0;
    tracker.completed = 0;
    drop(tracker);

    let _ = app_handle.emit(
        "thumbnail-progress",
        serde_json::json!({ "current": 0, "total": 0 }),
    );
    Ok(())
}

pub fn get_cached_full_warped_image(
    state: &tauri::State<AppState>,
    source: &Arc<DynamicImage>,
    is_raw: bool,
    js_adjustments: &serde_json::Value,
) -> Arc<DynamicImage> {
    let geo_hash = calculate_geometry_hash(js_adjustments);
    {
        // Only the current editor source may use its full-resolution warp cache.
        // Keep this guard until the cache lookup so a source switch cannot route
        // an old job through the new editor image's cache.
        let original = state
            .original_image
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if original
            .as_ref()
            .is_some_and(|loaded| Arc::ptr_eq(&loaded.image, source))
        {
            let cache = state
                .full_warped_cache
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if let Some(image) = cache
                .as_ref()
                .and_then(|cache| cache.get(source, is_raw, geo_hash))
            {
                return image;
            }
        }
    }

    let warped =
        crate::mask_generation::prepare_shared_warped_mask_image(source, is_raw, js_adjustments);

    let original = state
        .original_image
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if original
        .as_ref()
        .is_some_and(|loaded| Arc::ptr_eq(&loaded.image, source))
    {
        let mut cache = state
            .full_warped_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        // Another preview may have prepared the same source concurrently. Reuse
        // its identity to keep downstream immutable mask caches effective.
        if let Some(image) = cache
            .as_ref()
            .and_then(|cache| cache.get(source, is_raw, geo_hash))
        {
            return image;
        }
        *cache = Some(crate::mask_generation::FullWarpedMaskCache::new(
            source,
            is_raw,
            geo_hash,
            Arc::clone(&warped),
        ));
    }
    warped
}

#[tauri::command]
pub(crate) async fn update_wgpu_transform(
    payload: WgpuTransformPayload,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    let context = match state
        .gpu_context
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
    {
        Some(c) => c.clone(),
        None => return Ok(()),
    };

    tokio::task::spawn_blocking(move || {
        #[cfg(feature = "performance-profiling")]
        log::info!(
            "[PERF] {}",
            serde_json::json!({
                "stage": "display_transform_update",
                "rect": [payload.x, payload.y, payload.width, payload.height],
                "clip": [payload.clip_x, payload.clip_y, payload.clip_width, payload.clip_height],
                "window": [payload.window_width, payload.window_height],
            })
        );
        let mut display_lock = context.display.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(display) = display_lock.as_mut() {
            let rect = [payload.x, payload.y, payload.width, payload.height];
            let geometry = gpu_processing::DisplayGeometry {
                rect,
                crop_rect: payload.crop_rect.unwrap_or(rect),
                crop_rotation: payload.crop_rotation.to_radians(),
            };
            display.viewport = payload.viewport;
            display.split_x = payload.split;
            if payload.split.is_none() {
                display.clear_split_before();
            }
            if payload.hold {
                display.pending_geometry = Some(geometry);
            } else {
                display.pending_geometry = None;
                display.geometry = geometry;
                display.apply_geometry();
            }
            display.latest_transform.clip = [
                payload.clip_x,
                payload.clip_y,
                payload.clip_width,
                payload.clip_height,
            ];
            display.latest_transform.window = [payload.window_width, payload.window_height];
            display.latest_transform.bg_primary = payload.bg_primary;
            display.latest_transform.bg_secondary = payload.bg_secondary;
            display.latest_transform.pixelated = if payload.pixelated { 1.0 } else { 0.0 };

            context.queue.write_buffer(
                &display.transform_buffer,
                0,
                bytemuck::bytes_of(&display.latest_transform),
            );
            display.render(&context.device, &context.queue);
        }
    })
    .await
    .map_err(|e| format!("Task panicked: {}", e))?;

    Ok(())
}

/// Prefix of preview errors where rendering itself failed (for example an
/// invalid GPU shader), as opposed to a superseded or not-yet-loaded job.
/// The webview matches it to tell the user instead of keeping a stale frame.
pub(crate) const PREVIEW_RENDER_FAILED: &str = "Preview rendering failed";

/// The image Develop renders from: the full decode, or until it is in the
/// photograph's preview proxy (preview_proxy.rs), with `source_scale` proxy
/// pixels per full-resolution pixel and the full-resolution size.
pub(crate) fn preview_source(state: &AppState) -> Result<(LoadedImage, f32, (u32, u32)), String> {
    let full_source = state
        .original_image
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    Ok(match full_source {
        Some(loaded) => {
            let dims = loaded.image.dimensions();
            (loaded, 1.0_f32, dims)
        }
        None => {
            let current = state
                .current_source
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            let proxy = state
                .proxy_image
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
                .filter(|proxy| proxy.path == current)
                .ok_or("No original image loaded")?;
            let scale = proxy.image.width() as f32 / proxy.full_width.max(1) as f32;
            (
                LoadedImage {
                    path: proxy.path,
                    image: proxy.image,
                    is_raw: proxy.is_raw,
                    multiscale: None,
                },
                scale,
                (proxy.full_width, proxy.full_height),
            )
        }
    })
}

/// The preview base (warped, cropped, retouched source at `build_dim`) for
/// `adjustments`, from the reusable previews or built and kept there.
/// `base_adjustments` may differ by a draft flag only.
#[allow(clippy::too_many_arguments)]
fn preview_base(
    state: &tauri::State<AppState>,
    loaded_image: &LoadedImage,
    source_scale: f32,
    full_dims: (u32, u32),
    adjustments: &serde_json::Value,
    base_adjustments: &serde_json::Value,
    build_dim: u32,
    preparation_key: u64,
) -> Result<preview_cache::PreviewBase, String> {
    let reusable = state
        .reusable_previews
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&loaded_image.image, preparation_key, build_dim);
    if let Some(preview) = reusable {
        return Ok(preview);
    }
    let preview = if source_scale < 1.0 {
        // Steps that need the full image wait for it (the frontend
        // renders again when the decode is in). An offline
        // photograph gets no decode: its Standard preview is
        // retouched and blurred at its own resolution.
        let (full_width, full_height) = full_dims;
        let needs_full = crate::image_loader::has_base_retouch(adjustments)
            || adjustments["lensBlurEnabled"].as_bool().unwrap_or(false);
        let offline = state
            .offline_source
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_deref()
            == Some(loaded_image.path.as_str());
        if needs_full && !offline {
            return Err("The photograph is still loading".to_string());
        }
        #[cfg(feature = "performance-profiling")]
        let _stage = crate::performance::PerformanceStage::new("preview_proxy_base");
        let scale = reduced_preview_scale(full_width, full_height, adjustments, build_dim)
            .unwrap_or(source_scale);
        reduced_preview_base(
            &loaded_image.image,
            source_scale,
            full_width,
            full_height,
            base_adjustments,
            build_dim,
            scale,
        )?
    } else {
        generate_shared_transformed_preview(
            state,
            loaded_image,
            base_adjustments,
            build_dim,
            preparation_key,
        )?
    };
    state
        .reusable_previews
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(
            &loaded_image.image,
            preparation_key,
            build_dim,
            preview.clone(),
        );
    Ok(preview)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn process_preview_job(
    app_handle: &tauri::AppHandle,
    state: tauri::State<AppState>,
    mut adjustments_json: serde_json::Value,
    is_interactive: bool,
    target_resolution: Option<u32>,
    roi: Option<(f32, f32, f32, f32)>,
    request_analytics: bool,
    compute_waveform: bool,
    active_waveform_channel: Option<&str>,
    mask_overlay: Option<&crate::app_state::MaskOverlayRequest>,
    crop_edit: bool,
    render_generation: usize,
) -> Result<Vec<u8>, String> {
    let fn_start = std::time::Instant::now();
    #[cfg(feature = "performance-profiling")]
    let setup_stage = crate::performance::PerformanceStage::new("preview_setup");
    let context = get_or_init_gpu_context(&state, app_handle)?;
    hydrate_adjustments(&state, &mut adjustments_json);
    let adjustments_clone = adjustments_json;

    let (loaded_image, source_scale, full_dims) = preview_source(&state)?;
    let display_identity = gpu_processing::DisplayFrameIdentity {
        path: loaded_image.path.clone(),
        source: Arc::downgrade(&loaded_image.image),
        load_generation: state.load_image_generation.load(Ordering::SeqCst),
        render_generation,
    };

    let new_transform_hash = calculate_transform_hash(&adjustments_clone);
    let settings = load_settings(app_handle.clone()).unwrap_or_default();
    let live_quality = settings.live_preview_quality.as_deref().unwrap_or("high");

    let default_preview_dim = settings.editor_preview_resolution.unwrap_or(1920);
    let preview_dim = preview_dim_within_limit(
        target_resolution.unwrap_or(default_preview_dim),
        context.limits.max_texture_dimension_2d,
    );
    if roi.is_none() && !is_interactive {
        // The fit view's size decides the size of new preview proxies.
        state.last_preview_dim.store(preview_dim, Ordering::Relaxed);
    }
    #[cfg(not(target_os = "linux"))]
    let use_wgpu_renderer = settings.use_wgpu_renderer.unwrap_or(true);
    #[cfg(target_os = "linux")]
    let use_wgpu_renderer = false;

    let has_roi = roi.is_some();
    let (interactive_divisor, interactive_quality) = match live_quality {
        "full" => (1.0_f32, 85_u8),
        "performance" => (if has_roi { 1.8_f32 } else { 1.5_f32 }, 65_u8),
        _ => (if has_roi { 1.4_f32 } else { 1.0_f32 }, 75_u8),
    };
    #[cfg(target_os = "linux")]
    let interactive_divisor =
        browser_interactive_divisor(preview_dim, live_quality, interactive_divisor);

    let mut cached_preview_lock = state
        .cached_preview
        .lock()
        .unwrap_or_else(|e| e.into_inner());

    // Face retouch is the slow step of a preview base (about 70 ms for a
    // face filling a fit view): while a control is dragged the base is
    // built at half the size, as Capture One shows a quick preview until
    // the drag ends; the release renders it at full size.
    let draft_dim = (is_interactive
        && rawtonic_engine::face_retouch::is_active(&adjustments_clone))
    .then(|| (preview_dim / 2).max(256));
    let build_dim = draft_dim.unwrap_or(preview_dim);
    let base_valid = cached_preview_lock.as_ref().is_some_and(|c| {
        c.source.ptr_eq(&Arc::downgrade(&loaded_image.image))
            && c.transform_hash == new_transform_hash
            && (c.preview_dim == preview_dim || Some(c.preview_dim) == draft_dim)
    });
    let small_valid = base_valid
        && cached_preview_lock.as_ref().is_some_and(|c| {
            c.interactive_divisor == interactive_divisor && c.small_image.is_some()
        });

    let (final_preview_base, scale_for_gpu, unscaled_crop_offset, base_dim) = if base_valid {
        let cached = cached_preview_lock.as_ref().unwrap();
        (
            Arc::clone(&cached.image),
            cached.scale,
            cached.unscaled_crop_offset,
            cached.preview_dim,
        )
    } else {
        *state
            .gpu_image_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = None;

        let preparation_key = preview_cache::preparation_key(&adjustments_clone);
        // The quick preview heals crowded blemishes once (draft).
        let draft_adjustments;
        let base_adjustments = if draft_dim.is_some() {
            let mut draft = adjustments_clone.clone();
            draft["faceRetouch"]["draft"] = serde_json::json!(true);
            draft_adjustments = draft;
            &draft_adjustments
        } else {
            &adjustments_clone
        };
        let preview = preview_base(
            &state,
            &loaded_image,
            source_scale,
            full_dims,
            &adjustments_clone,
            base_adjustments,
            build_dim,
            preparation_key,
        )?;
        (
            preview.image,
            preview.scale,
            preview.unscaled_crop_offset,
            build_dim,
        )
    };

    let small_preview_base = prepare_interactive_preview(
        &final_preview_base,
        base_dim,
        interactive_divisor,
        is_interactive,
        if small_valid {
            cached_preview_lock.as_ref().unwrap().small_image.as_ref()
        } else {
            None
        },
    );
    *cached_preview_lock = Some(CachedPreview {
        source: Arc::downgrade(&loaded_image.image),
        image: Arc::clone(&final_preview_base),
        small_image: small_preview_base.clone(),
        transform_hash: new_transform_hash,
        scale: scale_for_gpu,
        unscaled_crop_offset,
        preview_dim: base_dim,
        interactive_divisor,
    });

    drop(cached_preview_lock);

    let (processing_image, effective_scale, jpeg_quality) = if is_interactive {
        let small_preview_base = small_preview_base.expect("interactive input was prepared");
        let orig_w = final_preview_base.width() as f32;
        let small_w = small_preview_base.width() as f32;
        let scale_factor = if orig_w > 0.0 { small_w / orig_w } else { 1.0 };
        let new_scale = scale_for_gpu * scale_factor;
        (small_preview_base, new_scale, interactive_quality)
    } else {
        (final_preview_base, scale_for_gpu, 94)
    };

    let (preview_width, preview_height) = processing_image.dimensions();

    let pixel_roi = if is_interactive {
        roi.map(|(nx, ny, nw, nh)| crate::gpu_processing::Roi {
            x: (nx * preview_width as f32).round() as u32,
            y: (ny * preview_height as f32).round() as u32,
            width: (nw * preview_width as f32).round() as u32,
            height: (nh * preview_height as f32).round() as u32,
        })
    } else {
        None
    };

    #[cfg(feature = "performance-profiling")]
    drop(setup_stage);
    #[cfg(feature = "performance-profiling")]
    let mask_stage = crate::performance::PerformanceStage::new("preview_mask_bitmaps");
    let mask_definitions: Vec<MaskDefinition> = adjustments_clone
        .get("masks")
        .and_then(|m| serde_json::from_value(m.clone()).ok())
        .unwrap_or_default();

    let scaled_crop_offset = (
        unscaled_crop_offset.0 * effective_scale,
        unscaled_crop_offset.1 * effective_scale,
    );

    let warped_image = resolve_warped_image_for_masks(
        &state,
        &loaded_image.image,
        loaded_image.is_raw,
        source_scale,
        &adjustments_clone,
        &mask_definitions,
    )?;
    let mask_geometry = PhotoGeometry::from_adjustments(&adjustments_clone)
        .with_photo_size(full_dims.0 as f64, full_dims.1 as f64);
    // The native display draws the edited layer's mask over this frame. Like
    // Capture One, it shows the mask itself: layer opacity scales the
    // adjustment, not the overlay. Taken first: below 100% the layer's own
    // bitmap is then derived from it instead of rasterized a second time.
    let display_overlay = match mask_overlay.filter(|_| use_wgpu_renderer) {
        Some(request) => match mask_definitions
            .iter()
            .find(|def| def.id == request.mask_id && def.produces_bitmap())
        {
            Some(def) => get_cached_or_generate_mask(
                &state,
                &MaskDefinition {
                    opacity: 100.0,
                    ..def.clone()
                },
                preview_width,
                preview_height,
                effective_scale,
                scaled_crop_offset,
                &mask_geometry,
                warped_image.as_ref(),
            )?
            .map(|mask| gpu_processing::DisplayMaskOverlay {
                mask,
                style: if request.grayscale {
                    crate::mask_generation::OverlayStyle::Grayscale
                } else {
                    crate::mask_generation::OverlayStyle::Colored {
                        rgb: request.color,
                        opacity: request.opacity.min(100),
                    }
                },
            }),
            None => None,
        },
        None => None,
    };
    if use_wgpu_renderer
        && let Some(display) = context
            .display
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_mut()
    {
        display.overlay = display_overlay;
        display.next_frame_uncropped = crop_edit;
        display.next_frame_split_key = split_frame_key(&loaded_image.path, &adjustments_clone);
    }
    let mask_bitmaps: Vec<Arc<image::GrayImage>> = mask_definitions
        .iter()
        .filter_map(|def| {
            get_cached_or_generate_mask(
                &state,
                def,
                preview_width,
                preview_height,
                effective_scale,
                scaled_crop_offset,
                &mask_geometry,
                warped_image.as_ref(),
            )
            .transpose()
        })
        .collect::<Result<_, _>>()?;
    #[cfg(feature = "performance-profiling")]
    drop(mask_stage);
    #[cfg(feature = "performance-profiling")]
    let adjustments_stage = crate::performance::PerformanceStage::new("preview_adjustments");

    let is_raw = loaded_image.is_raw;
    let tm_override = resolve_tonemapper_override_from_handle(app_handle, is_raw);
    let mut final_adjustments =
        get_all_adjustments_from_json(&adjustments_clone, is_raw, tm_override);
    crate::baseline_exposure::apply_base_look(&mut final_adjustments.global, &loaded_image.path);
    final_adjustments.global.detail_scale = effective_scale;
    let lut_path = adjustments_clone["lutPath"].as_str();
    let lut = lut_path.and_then(|p| lut_processing::get_or_load_lut(&state, p).ok());

    let wants_analytics = !(is_interactive && pixel_roi.is_some()) && request_analytics;
    let channel_filter = if is_interactive {
        active_waveform_channel.map(|s| s.to_string())
    } else {
        None
    };

    let analytics_config = if wants_analytics {
        state.analytics_worker_tx.lock().unwrap().clone().map(|tx| {
            crate::app_state::AnalyticsConfig {
                path: loaded_image.path.clone(),
                compute_waveform,
                active_waveform_channel: channel_filter,
                sender: tx,
            }
        })
    } else {
        None
    };

    #[cfg(feature = "performance-profiling")]
    drop(adjustments_stage);
    let final_processed_image_result =
        crate::image_processing::process_and_get_dynamic_image_with_analytics(
            &context,
            &state,
            &processing_image,
            new_transform_hash,
            RenderRequest {
                adjustments: final_adjustments,
                mask_bitmaps: &mask_bitmaps,
                lut,
                roi: pixel_roi,
            },
            "apply_adjustments",
            use_wgpu_renderer,
            analytics_config,
            Some(display_identity),
        );

    let final_processed_image = match final_processed_image_result {
        Ok(image) => image,
        Err(error) => {
            log::error!(
                "[process_preview_job] processing failed after {:.2?}: {}",
                fn_start.elapsed(),
                error
            );
            return Err(format!("{PREVIEW_RENDER_FAILED}: {error}"));
        }
    };
    if use_wgpu_renderer {
        {
            #[cfg(feature = "performance-profiling")]
            let _stage = crate::performance::PerformanceStage::new("preview_gpu_wait");
            let _ = context.device.poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: Some(std::time::Duration::from_millis(500)),
            });
        }
        let _ = app_handle.emit(
            "wgpu-frame-ready",
            serde_json::json!({ "path": loaded_image.path }),
        );
        #[cfg(feature = "performance-profiling")]
        log::info!(
            "[PERF] {}",
            serde_json::json!({
                "stage": "wgpu_preview", "ms": fn_start.elapsed().as_secs_f64() * 1000.0,
                "interactive": is_interactive, "width": preview_width, "height": preview_height
            })
        );
        return Ok(b"WGPU_RENDER".to_vec());
    }

    let final_processed_image = Arc::new(final_processed_image);
    let final_rgba_image = match &*final_processed_image {
        DynamicImage::ImageRgba8(img) => img,
        _ => return Err("Expected Rgba8 image from GPU for encoding".to_string()),
    };

    let step_start = std::time::Instant::now();

    // Linux presents previews through WebKit rather than a native surface.
    // Read RGBA directly with libjpeg-turbo, avoiding a full RGB copy and the
    // Rust encoder's extra latency on every slider/zoom frame.
    #[cfg(target_os = "linux")]
    let encode_result = encode_browser_preview(final_rgba_image, jpeg_quality);

    #[cfg(not(target_os = "linux"))]
    let encode_result = {
        let raw_bytes: &[u8] = final_rgba_image.as_raw();
        let rgba8_pixels: &[RGBA8] = raw_bytes.as_rgba();

        let img_ref = ImgRef::new(
            rgba8_pixels,
            final_rgba_image.width() as usize,
            final_rgba_image.height() as usize,
        );

        Encoder::new(Preset::BaselineFastest)
            .quality(jpeg_quality)
            .fast_color(true)
            .encode_imgref(img_ref)
    };

    match encode_result {
        Ok(jpeg_bytes) => {
            if is_interactive {
                let (roi_w, roi_h) = final_rgba_image.dimensions();
                let (rx, ry) = if let Some(r) = pixel_roi {
                    (r.x, r.y)
                } else {
                    (0, 0)
                };

                let mut response = Vec::with_capacity(24 + jpeg_bytes.len());
                response.extend_from_slice(&rx.to_le_bytes());
                response.extend_from_slice(&ry.to_le_bytes());
                response.extend_from_slice(&roi_w.to_le_bytes());
                response.extend_from_slice(&roi_h.to_le_bytes());
                response.extend_from_slice(&preview_width.to_le_bytes());
                response.extend_from_slice(&preview_height.to_le_bytes());
                response.extend_from_slice(&jpeg_bytes);

                log::info!(
                    "[process_preview_job] interactive ROI {}x{} encode in {:.2?}, total {:.2?}",
                    roi_w,
                    roi_h,
                    step_start.elapsed(),
                    fn_start.elapsed()
                );
                Ok(response)
            } else {
                let (width, height) = final_rgba_image.dimensions();
                log::info!(
                    "[process_preview_job] full {}x{} q={} encode in {:.2?}, total {:.2?}",
                    width,
                    height,
                    jpeg_quality,
                    step_start.elapsed(),
                    fn_start.elapsed()
                );
                Ok(jpeg_bytes)
            }
        }
        Err(e) => Err(format!("Failed to encode preview: {}", e)),
    }
}

#[cfg(any(target_os = "linux", test))]
fn browser_interactive_divisor(preview_dim: u32, quality: &str, divisor: f32) -> f32 {
    // A zoomed or HiDPI frame can otherwise encode millions of pixels on
    // every pointer move. Refine at the requested resolution on release;
    // the explicit Full quality mode still renders every pixel while dragging.
    let limit = match quality {
        "full" => return divisor,
        "performance" => 1024.0,
        _ => 1536.0,
    };
    divisor.max(preview_dim as f32 / limit)
}

pub(crate) fn encode_browser_preview(
    image: &image::RgbaImage,
    quality: u8,
) -> Result<Vec<u8>, String> {
    let mut compressor = turbojpeg::Compressor::new().map_err(|e| e.to_string())?;
    compressor
        .set_quality(i32::from(quality.clamp(1, 100)))
        .map_err(|e| e.to_string())?;
    // Keep colour detail in the final frame and in small interactive patches.
    compressor
        .set_subsamp(turbojpeg::Subsamp::None)
        .map_err(|e| e.to_string())?;
    compressor
        .compress_to_vec(turbojpeg::Image {
            pixels: image.as_raw().as_slice(),
            width: image.width() as usize,
            pitch: image.width() as usize * 4,
            height: image.height() as usize,
            format: turbojpeg::PixelFormat::RGBX,
        })
        .map_err(|e| e.to_string())
}

pub(crate) fn start_analytics_worker(app_handle: tauri::AppHandle) {
    let state = app_handle.state::<AppState>();
    let worker_app = app_handle.clone();
    let sender = gpu_processing::start_latest_job_worker(move |job: AnalyticsJob| {
        #[cfg(feature = "performance-profiling")]
        let _stage = crate::performance::PerformanceStage::new("analytics_compute");
        let histogram_data = image_processing::calculate_histogram_from_image(&job.image).ok();
        let waveform_data = if job.compute_waveform {
            image_processing::calculate_waveform_from_image(
                &job.image,
                job.active_waveform_channel.as_deref(),
            )
            .ok()
        } else {
            None
        };
        if histogram_data.is_some() || waveform_data.is_some() {
            let _ = worker_app.emit(
                "analytics-update",
                serde_json::json!({
                    "path": job.path,
                    "histogram": histogram_data,
                    "waveform": waveform_data,
                }),
            );
        }
    });
    *state.analytics_worker_tx.lock().unwrap() = Some(sender);
}

pub(crate) fn start_preview_worker(app_handle: tauri::AppHandle) {
    std::thread::spawn(move || {
        let jobs = &app_handle.state::<AppState>().inner().preview_jobs;
        let mut newest_sequence = None;
        let mut waiting: Option<PreviewJob> = None;
        loop {
            // A request arriving meanwhile has already replaced (and
            // rejected) the one waiting: the job taken is the newest received.
            let job = match waiting.take() {
                Some(job) => job,
                None if crate::multi_view::live_pending() => {
                    // Edit all selected: while no frame of the primary is
                    // asked for, the other cells catch up one at a time.
                    match jobs.take_timeout(LIVE_IDLE) {
                        Some(job) => job,
                        None => {
                            crate::multi_view::live_step(&app_handle);
                            continue;
                        }
                    }
                }
                None => jobs.take(),
            };
            // Dropping the responder rejects the superseded invoke.
            if crate::app_state::preview_job_is_stale(job.sequence, newest_sequence) {
                #[cfg(feature = "performance-profiling")]
                log::info!(
                    "[PERF] {}",
                    serde_json::json!({ "stage": "preview_job_stale" })
                );
                continue;
            }
            newest_sequence = job.sequence.or(newest_sequence);

            let state = app_handle.state::<AppState>();
            let responder = job.responder;
            crate::multi_view_prefetch::note_preview(job.is_interactive);
            #[cfg(feature = "performance-profiling")]
            let (job_start_epoch, job_interactive, job_sequence) = (
                crate::performance::epoch_ms(),
                job.is_interactive,
                job.sequence,
            );
            #[cfg(feature = "performance-profiling")]
            if let Some(delay) = crate::preview_slot::debug_render_delay() {
                std::thread::sleep(delay);
            }
            let result = process_preview_job(
                &app_handle,
                state,
                job.adjustments,
                job.is_interactive,
                job.target_resolution,
                job.roi,
                job.request_analytics,
                job.compute_waveform,
                job.active_waveform_channel.as_deref(),
                job.mask_overlay.as_ref(),
                job.crop_edit,
                job.generation,
            );
            #[cfg(feature = "performance-profiling")]
            log::info!(
                "[PERF] {}",
                serde_json::json!({
                    "stage": "preview_job", "start_epoch_ms": job_start_epoch,
                    "end_epoch_ms": crate::performance::epoch_ms(), "interactive": job_interactive,
                    "sequence": job_sequence
                })
            );
            if let Err(e) = &result {
                log::error!("Preview worker error: {}", e);
            }
            let _ = responder.send(result);
            // Edit all selected: one other cell after each frame of the
            // primary, unless its next frame is already asked for.
            if crate::multi_view::live_pending() {
                match jobs.try_take() {
                    Some(next) => waiting = Some(next),
                    None => {
                        crate::multi_view::live_step(&app_handle);
                    }
                }
            }
        }
    });
}

/// How long the preview worker waits for the primary's next frame before
/// developing another multi view cell live.
const LIVE_IDLE: std::time::Duration = std::time::Duration::from_millis(25);

/// Shows the photograph at `path` at once, natively (Capture One swaps its
/// preview files the same way): its preview proxy (in memory or on disk) is
/// developed with its recipe (`adjustments` from the editor's cache, else
/// the stored one, else the defaults) and presented in the fit placement,
/// before the webview has updated its state. The editor then loads the
/// photograph as usual and renders the same frame again. Returns whether a
/// frame was started (false: no proxy, or the viewer is not at fit).
#[tauri::command]
pub(crate) async fn activate_photo(
    path: String,
    adjustments: Option<serde_json::Value>,
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<bool, String> {
    #[cfg(feature = "performance-profiling")]
    let received = crate::performance::epoch_ms();
    let (source_path, sidecar_path) = parse_virtual_path(&path);
    if !formats::is_raw_file(&source_path) {
        return Ok(false);
    }
    let Some(viewport) = state
        .gpu_context
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .and_then(|c| {
            let display = c.display.lock().unwrap_or_else(|e| e.into_inner());
            let display = display.as_ref()?;
            // Multi view: the next primary is shown in its own cell.
            match display.multi_view().cell(&path) {
                Some(cell) => Some(cell.area),
                None => display.viewport,
            }
        })
    else {
        #[cfg(feature = "performance-profiling")]
        log::info!(
            "[PERF] {}",
            serde_json::json!({ "stage": "activate_photo_skipped", "reason": "no viewport" })
        );
        return Ok(false);
    };
    *state
        .current_source
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = path.clone();
    let remembered = state
        .proxy_memory
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&path);
    let proxy = match remembered {
        Some(proxy) => proxy,
        None => {
            let settings = load_settings(app_handle.clone()).unwrap_or_default();
            let read_source = source_path.clone();
            let Some(proxy) = tokio::task::spawn_blocking(move || {
                crate::preview_proxy::read(&read_source, &settings)
            })
            .await
            .map_err(|e| e.to_string())?
            else {
                #[cfg(feature = "performance-profiling")]
                log::info!(
                    "[PERF] {}",
                    serde_json::json!({ "stage": "activate_photo_skipped", "reason": "no proxy" })
                );
                return Ok(false);
            };
            state
                .proxy_memory
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remember(&path, proxy)
        }
    };
    // Asked for another photograph meanwhile.
    if *state
        .current_source
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        != path
    {
        return Ok(false);
    }
    let mut recipe = adjustments
        .filter(|a| a.is_object())
        .unwrap_or_else(|| crate::exif_processing::load_sidecar(&sidecar_path).adjustments);
    if recipe.is_null() {
        recipe = serde_json::json!({});
    }
    crate::embedded_lens::apply_default(&mut recipe, &source_path);
    // Shown size: the crop, else the photograph turned by its orientation.
    let crop: Option<image_processing::Crop> = serde_json::from_value(recipe["crop"].clone()).ok();
    let steps = recipe["orientationSteps"].as_u64().unwrap_or(0);
    let (shown_w, shown_h) = match crop {
        Some(c) if c.width > 0.0 && c.height > 0.0 => (c.width as f32, c.height as f32),
        _ if steps % 2 == 1 => (proxy.full_height as f32, proxy.full_width as f32),
        _ => (proxy.full_width as f32, proxy.full_height as f32),
    };
    let rect = gpu_processing::fit_placement(viewport, shown_w, shown_h);
    // The preview job renders the proxy, not the previous photograph's decode.
    {
        let mut original = state
            .original_image
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if original.as_ref().is_some_and(|o| o.path != path) {
            *original = None;
        }
    }
    *state.proxy_image.lock().unwrap_or_else(|e| e.into_inner()) = Some(proxy);
    if let Some(context) = state
        .gpu_context
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        && let Some(display) = context
            .display
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_mut()
    {
        display.pending_geometry = Some(gpu_processing::DisplayGeometry {
            rect,
            crop_rect: rect,
            crop_rotation: 0.0,
        });
    }
    let preview_dim = match state.last_preview_dim.load(Ordering::Relaxed) {
        0 => None,
        dim => Some(dim),
    };
    let (tx, rx) = tokio::sync::oneshot::channel();
    {
        state.preview_jobs.submit(PreviewJob {
            generation: state.preview_generation.fetch_add(1, Ordering::SeqCst) + 1,
            sequence: None,
            adjustments: recipe,
            is_interactive: false,
            target_resolution: preview_dim,
            roi: None,
            request_analytics: false,
            compute_waveform: false,
            active_waveform_channel: None,
            mask_overlay: None,
            crop_edit: false,
            responder: tx,
        });
    }
    #[cfg(feature = "performance-profiling")]
    log::info!(
        "[PERF] {}",
        serde_json::json!({ "stage": "activate_photo", "received_epoch_ms": received,
            "queued_epoch_ms": crate::performance::epoch_ms() })
    );
    // The job presents natively; its answer only matters for errors.
    tokio::spawn(async move {
        let _ = rx.await;
    });
    Ok(true)
}

/// Renders the split view's before side (`js_adjustments`: the unedited
/// recipe with the edit's framing, view states included) for the photograph
/// at `path` and keeps it on the GPU beside the edited frames, as Capture
/// One's before/after split shows both renders. Pixels stay native: the
/// display-shader readback (exactly the display frame's pixels) is uploaded
/// to the display's before texture. Returns whether a before is in place;
/// an unchanged request renders nothing.
#[tauri::command]
pub(crate) async fn render_split_before(
    path: String,
    js_adjustments: serde_json::Value,
    target_resolution: Option<u32>,
    app_handle: tauri::AppHandle,
) -> Result<bool, String> {
    tauri::async_runtime::spawn_blocking(move || {
        render_split_before_blocking(&app_handle, &path, js_adjustments, target_resolution)
    })
    .await
    .map_err(|e| format!("Split before task failed: {e}"))?
}

fn render_split_before_blocking(
    app_handle: &tauri::AppHandle,
    path: &str,
    mut adjustments: serde_json::Value,
    target_resolution: Option<u32>,
) -> Result<bool, String> {
    let state = app_handle.state::<AppState>();
    let settings = load_settings(app_handle.clone()).unwrap_or_default();
    #[cfg(not(target_os = "linux"))]
    let use_wgpu_renderer = settings.use_wgpu_renderer.unwrap_or(true);
    #[cfg(target_os = "linux")]
    let use_wgpu_renderer = false;
    if !use_wgpu_renderer {
        return Ok(false);
    }
    let context = get_or_init_gpu_context(&state, app_handle)?;
    let display_key = |context: &gpu_processing::GpuContext| {
        context
            .display
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|display| display.split_before_key())
    };
    let Some(current_key) = display_key(&context) else {
        return Ok(false);
    };
    hydrate_adjustments(&state, &mut adjustments);
    let load_generation = state.load_image_generation.load(Ordering::SeqCst);
    let (loaded_image, source_scale, full_dims) = preview_source(&state)?;
    if loaded_image.path != path {
        return Ok(false);
    }
    let preview_dim = preview_dim_within_limit(
        target_resolution.unwrap_or(settings.editor_preview_resolution.unwrap_or(1920)),
        context.limits.max_texture_dimension_2d,
    );
    let frame_key = split_frame_key(path, &adjustments);
    let render_key = {
        let mut hasher = DefaultHasher::new();
        frame_key.hash(&mut hasher);
        (Arc::as_ptr(&loaded_image.image) as usize).hash(&mut hasher);
        load_generation.hash(&mut hasher);
        preview_dim.hash(&mut hasher);
        adjustments.to_string().hash(&mut hasher);
        hasher.finish()
    };
    if current_key == Some(render_key) {
        return Ok(true);
    }

    // The edit's preview base when the before shares it (no retouch, same
    // framing); else its own, without touching the edit's caches.
    let transform_hash = calculate_transform_hash(&adjustments);
    let cached = state
        .cached_preview
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .filter(|c| {
            c.source.ptr_eq(&Arc::downgrade(&loaded_image.image))
                && c.transform_hash == transform_hash
                && c.preview_dim == preview_dim
        })
        .map(|c| (Arc::clone(&c.image), c.scale));
    let (base, scale) = match cached {
        Some(cached) => cached,
        None => {
            let preview = preview_base(
                &state,
                &loaded_image,
                source_scale,
                full_dims,
                &adjustments,
                &adjustments,
                preview_dim,
                preview_cache::preparation_key(&adjustments),
            )?;
            (preview.image, preview.scale)
        }
    };

    let is_raw = loaded_image.is_raw;
    let tm_override = resolve_tonemapper_override(&settings, is_raw);
    let mut all_adjustments = get_all_adjustments_from_json(&adjustments, is_raw, tm_override);
    crate::baseline_exposure::apply_base_look(&mut all_adjustments.global, &loaded_image.path);
    all_adjustments.global.detail_scale = scale;
    // Encoded for the window layer, as display frames are.
    let space = context.display_space;
    all_adjustments.global.output_space[0] = space.shader_code();
    all_adjustments.global.output_matrix =
        image_processing::gpu_mat3_from_rows(space.from_linear_srgb_f32());
    let lut = adjustments["lutPath"]
        .as_str()
        .and_then(|p| lut_processing::get_or_load_lut(&state, p).ok());
    // The unedited recipe has no local adjustments: no mask bitmaps.
    let rendered = gpu_processing::process_and_get_dynamic_image_with_display_shader(
        &context,
        &state,
        &base,
        transform_hash,
        RenderRequest {
            adjustments: all_adjustments,
            mask_bitmaps: &[],
            lut,
            roi: None,
        },
        "split_before",
    )
    .map_err(|error| format!("{PREVIEW_RENDER_FAILED}: {error}"))?;
    let rendered = rendered.into_rgba8();

    // Another photograph or decode meanwhile: this before is not wanted.
    let still_current = state.load_image_generation.load(Ordering::SeqCst) == load_generation
        && preview_source(&state).is_ok_and(|(now, _, _)| {
            now.path == path && Arc::ptr_eq(&now.image, &loaded_image.image)
        });
    if !still_current {
        return Ok(false);
    }
    let mut display_lock = context.display.lock().unwrap_or_else(|e| e.into_inner());
    let Some(display) = display_lock.as_mut() else {
        return Ok(false);
    };
    // Kept even if the divider is not set yet (the webview sends it with its
    // next placement); the display draws the before only beside a divider.
    display.set_split_before(
        &context.device,
        &context.queue,
        &rendered,
        render_key,
        frame_key,
    );
    display.render(&context.device, &context.queue);
    Ok(true)
}

#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub(crate) async fn apply_adjustments(
    preview_channel: Option<tauri::ipc::JavaScriptChannelId>,
    webview: tauri::Webview,
    js_adjustments: serde_json::Value,
    is_interactive: bool,
    target_resolution: Option<u32>,
    roi: Option<(f32, f32, f32, f32)>,
    request_analytics: bool,
    compute_waveform: bool,
    active_waveform_channel: Option<String>,
    sequence: Option<u64>,
    mask_overlay: Option<crate::app_state::MaskOverlayRequest>,
    crop_edit: Option<bool>,
    browser_preview_uri: Option<bool>,
    state: tauri::State<'_, AppState>,
) -> Result<Response, String> {
    let preview_channel: Option<tauri::ipc::Channel<Response>> =
        preview_channel.map(|id| id.channel_on(webview));
    #[cfg(feature = "performance-profiling")]
    log::info!(
        "[PERF] {}",
        serde_json::json!({
            "stage": "preview_command_received", "epoch_ms": crate::performance::epoch_ms(),
            "interactive": is_interactive
        })
    );
    let (tx, rx) = tokio::sync::oneshot::channel();

    let kept = state.preview_jobs.submit(PreviewJob {
        generation: state.preview_generation.fetch_add(1, Ordering::SeqCst) + 1,
        sequence,
        adjustments: js_adjustments,
        is_interactive,
        target_resolution,
        roi,
        request_analytics,
        compute_waveform,
        active_waveform_channel,
        mask_overlay,
        crop_edit: crop_edit.unwrap_or(false),
        responder: tx,
    });
    #[cfg(feature = "performance-profiling")]
    if !kept {
        log::info!(
            "[PERF] {}",
            serde_json::json!({ "stage": "preview_job_stale", "sequence": sequence })
        );
    }
    #[cfg(not(feature = "performance-profiling"))]
    let _ = kept;

    match rx.await {
        Ok(Ok(bytes)) => {
            #[cfg(target_os = "linux")]
            let use_uri = is_interactive && browser_preview_uri.unwrap_or(false);
            #[cfg(all(target_os = "linux", feature = "performance-profiling"))]
            let use_uri =
                use_uri && std::env::var("RAWTONIC_PREVIEW_TRANSPORT").as_deref() != Ok("bytes");
            #[cfg(target_os = "linux")]
            let bytes = if use_uri {
                crate::browser_preview_transport::interactive_reply(bytes)
            } else {
                bytes
            };
            #[cfg(not(target_os = "linux"))]
            let _ = browser_preview_uri;
            #[cfg(feature = "performance-profiling")]
            log::info!(
                "[PERF] {}",
                serde_json::json!({
                    "stage": "preview_reply_ready", "epoch_ms": crate::performance::epoch_ms(),
                    "sequence": sequence, "bytes": bytes.len(), "interactive": is_interactive
                })
            );
            #[cfg(target_os = "linux")]
            {
                #[cfg(not(feature = "performance-profiling"))]
                let use_channel = true;
                #[cfg(feature = "performance-profiling")]
                let use_channel =
                    std::env::var("RAWTONIC_PREVIEW_TRANSPORT").as_deref() != Ok("uri-reply");
                // Only the tiny URI packet uses direct channel delivery.
                // Large binary channel messages would incur another fetch.
                if use_channel
                    && bytes.starts_with(b"JPEG_URI___")
                    && let Some(channel) = preview_channel
                    && channel.send(Response::new(bytes.clone())).is_ok()
                {
                    return Ok(Response::new(Vec::new()));
                }
            }
            #[cfg(not(target_os = "linux"))]
            let _ = preview_channel;
            Ok(Response::new(bytes))
        }
        Ok(Err(error)) => Err(error),
        Err(_) => Err("Superseded or worker failed".to_string()),
    }
}

#[tauri::command]
pub(crate) async fn generate_uncropped_preview(
    js_adjustments: serde_json::Value,
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<String, String> {
    let context = get_or_init_gpu_context(&state, &app_handle)?;
    let mut adjustments_clone = js_adjustments.clone();
    hydrate_adjustments(&state, &mut adjustments_clone);

    let loaded_image = crate::image_loader::wait_for_original_image(&state)?;

    tokio::task::spawn_blocking(move || {
        let state = app_handle.state::<AppState>();
        let path = loaded_image.path.clone();
        let is_raw = loaded_image.is_raw;
        let visual_hash = calculate_visual_hash(&path, &adjustments_clone);

        let pre_geometry_base = {
            let mut cache = state.geometry_cache.lock().unwrap();
            if let Some(cached) = cache.get(&visual_hash).cloned() {
                cached
            } else {
                let has_patches = crate::image_loader::has_base_retouch(&adjustments_clone);
                let patched_image = if has_patches {
                    Cow::Owned(
                        composite_patches_on_image(&loaded_image.image, &adjustments_clone)
                            .unwrap_or_else(|_| loaded_image.image.as_ref().clone()),
                    )
                } else {
                    Cow::Borrowed(loaded_image.image.as_ref())
                };

                let blurred_image =
                    crate::lens_blur::apply_lens_blur(patched_image, &adjustments_clone);

                let settings = load_settings(app_handle.clone()).unwrap_or_default();
                let target_dim = (settings.editor_preview_resolution.unwrap_or(1920) as f32) as u32;

                let downscaled = downscale_f32_image(&blurred_image, target_dim, target_dim);
                share_lateral_ca(&blurred_image, &downscaled, &adjustments_clone);

                if cache.len() > 5 {
                    cache.clear();
                }
                cache.insert(visual_hash, downscaled.clone());
                downscaled
            }
        };

        let scale_for_gpu = if loaded_image.image.width() > 0 {
            pre_geometry_base.width() as f32 / loaded_image.image.width() as f32
        } else {
            1.0
        };

        let params = crate::image_processing::get_geometry_params_from_json(&adjustments_clone);
        let mut adjusted_params = params;
        adjusted_params.lens_vignette_amount *= if is_raw { 0.4 } else { 0.8 };

        let warped_image = rawtonic_engine::edge_fringe::apply_for_recipe(
            Cow::Owned(warp_image_geometry(&pre_geometry_base, adjusted_params)),
            &adjustments_clone,
            scale_for_gpu,
        )
        .into_owned();
        let orientation_steps = adjustments_clone["orientationSteps"].as_u64().unwrap_or(0) as u8;
        let flipped_image = apply_flip(
            apply_coarse_rotation(Cow::Owned(warped_image), orientation_steps),
            adjustments_clone["flipHorizontal"]
                .as_bool()
                .unwrap_or(false),
            adjustments_clone["flipVertical"].as_bool().unwrap_or(false),
        )
        .into_owned();

        let (preview_width, preview_height) = flipped_image.dimensions();

        let mask_definitions: Vec<MaskDefinition> = adjustments_clone
            .get("masks")
            .and_then(|m| serde_json::from_value(m.clone()).ok())
            .unwrap_or_default();

        let warped_mask_source = resolve_warped_image_for_masks(
            &state,
            &loaded_image.image,
            is_raw,
            1.0,
            &adjustments_clone,
            &mask_definitions,
        )?;
        // This frame is oriented and flipped but not straightened.
        let mask_geometry = PhotoGeometry::from_adjustments(&adjustments_clone)
            .unrotated()
            .with_photo_size(
                loaded_image.image.width() as f64,
                loaded_image.image.height() as f64,
            );
        let mask_bitmaps: Vec<Arc<image::GrayImage>> = mask_definitions
            .iter()
            .filter_map(|def| {
                get_cached_or_generate_mask(
                    &state,
                    def,
                    preview_width,
                    preview_height,
                    scale_for_gpu,
                    (0.0, 0.0),
                    &mask_geometry,
                    warped_mask_source.as_ref(),
                )
                .transpose()
            })
            .collect::<Result<_, _>>()?;

        let tm_override = resolve_tonemapper_override_from_handle(&app_handle, is_raw);
        let mut uncropped_adjustments =
            get_all_adjustments_from_json(&adjustments_clone, is_raw, tm_override);
        crate::baseline_exposure::apply_base_look(&mut uncropped_adjustments.global, &path);
        uncropped_adjustments.global.detail_scale = scale_for_gpu;
        uncropped_adjustments.global.show_clipping = 0;
        let lut_path = adjustments_clone["lutPath"].as_str();
        let lut = lut_path.and_then(|p| lut_processing::get_or_load_lut(&state, p).ok());

        let full_hash = calculate_full_job_hash(&path, &adjustments_clone);

        if let Ok(processed_image) = process_and_get_dynamic_image(
            &context,
            &state,
            &flipped_image,
            full_hash,
            RenderRequest {
                adjustments: uncropped_adjustments,
                mask_bitmaps: &mask_bitmaps,
                lut,
                roi: None,
            },
            "generate_uncropped_preview",
        ) {
            let (width, height) = processed_image.dimensions();
            let rgb_pixels = processed_image.to_rgb8().into_vec();

            if let Ok(bytes) = Encoder::new(Preset::BaselineFastest)
                .quality(80)
                .encode_rgb(&rgb_pixels, width, height)
            {
                let base64_str = general_purpose::STANDARD.encode(&bytes);
                let data_url = format!("data:image/jpeg;base64,{}", base64_str);
                return Ok(data_url);
            }
        }

        Err("Failed to process uncropped preview".to_string())
    })
    .await
    .map_err(|e| format!("Task execution failed: {}", e))?
}

pub fn get_original_image(
    state: &tauri::State<AppState>,
) -> Result<(std::sync::Arc<image::DynamicImage>, bool), String> {
    let loaded_image = crate::image_loader::wait_for_original_image(state)?;
    Ok((loaded_image.image, loaded_image.is_raw))
}

#[tauri::command]
pub(crate) async fn generate_preview_for_path(
    path: String,
    js_adjustments: Value,
    app_handle: tauri::AppHandle,
) -> Result<Response, String> {
    tokio::task::spawn_blocking(move || {
        let state = app_handle.state::<AppState>();
        let context = get_or_init_gpu_context(&state, &app_handle)?;
        let (source_path, _) = parse_virtual_path(&path);
        let source_path_str = source_path.to_string_lossy().to_string();
        let is_raw = is_raw_file(&source_path_str);
        let settings = load_settings(app_handle.clone()).unwrap_or_default();

        let pristine_source = match read_file_mapped(&source_path) {
            Ok(mmap) => crate::image_loader::load_base_image_for_recipe(
                &mmap,
                &source_path_str,
                false,
                &settings,
                None,
                None,
                &js_adjustments,
            )
            .map_err(|e| e.to_string())?,
            Err(e) => {
                log::warn!(
                    "Failed to memory-map file '{}': {}. Falling back to standard read.",
                    source_path_str,
                    e
                );
                let bytes = fs::read(&source_path).map_err(|io_err| io_err.to_string())?;
                crate::image_loader::load_base_image_for_recipe(
                    &bytes,
                    &source_path_str,
                    false,
                    &settings,
                    None,
                    None,
                    &js_adjustments,
                )
                .map_err(|e| e.to_string())?
            }
        };

        let base_image =
            crate::image_loader::composite_patches_for_render(&pristine_source, &js_adjustments)
                .map_err(|e| e.to_string())?;

        let (transformed_image, unscaled_crop_offset) =
            apply_all_transformations(base_image, &js_adjustments);
        let (img_w, img_h) = transformed_image.dimensions();
        let mask_definitions: Vec<MaskDefinition> = js_adjustments
            .get("masks")
            .and_then(|m| serde_json::from_value(m.clone()).ok())
            .unwrap_or_default();

        let warped_image = mask_definitions
            .iter()
            .any(MaskDefinition::requires_warped_image)
            .then(|| prepare_parametric_mask_source(&pristine_source, is_raw, 1.0, &js_adjustments))
            .transpose()?;
        let mask_geometry = PhotoGeometry::from_adjustments(&js_adjustments).with_photo_size(
            pristine_source.width() as f64,
            pristine_source.height() as f64,
        );
        let mask_bitmaps: Vec<Arc<image::GrayImage>> = mask_definitions
            .iter()
            .filter_map(|def| {
                generate_mask_bitmap(
                    def,
                    img_w,
                    img_h,
                    1.0,
                    unscaled_crop_offset,
                    &mask_geometry,
                    warped_image.as_ref(),
                )
                .map(Arc::new)
            })
            .collect();

        let tm_override = resolve_tonemapper_override(&settings, is_raw);
        let mut all_adjustments =
            get_all_adjustments_from_json(&js_adjustments, is_raw, tm_override);
        crate::baseline_exposure::apply_base_look(&mut all_adjustments.global, &source_path_str);
        all_adjustments.global.show_clipping = 0;
        let lut_path = js_adjustments["lutPath"].as_str();
        let lut = lut_path.and_then(|p| lut_processing::get_or_load_lut(&state, p).ok());
        let unique_hash = calculate_full_job_hash(&source_path_str, &js_adjustments);

        let final_image = process_and_get_dynamic_image(
            &context,
            &state,
            transformed_image.as_ref(),
            unique_hash,
            RenderRequest {
                adjustments: all_adjustments,
                mask_bitmaps: &mask_bitmaps,
                lut,
                roi: None,
            },
            "generate_preview_for_path",
        )?;

        let (width, height) = final_image.dimensions();
        let rgb_pixels = final_image.to_rgb8().into_vec();

        let bytes = Encoder::new(Preset::BaselineFastest)
            .quality(92)
            .encode_rgb(&rgb_pixels, width, height)
            .map_err(|e| format!("Failed to encode with mozjpeg-rs: {}", e))?;

        Ok(Response::new(bytes))
    })
    .await
    .map_err(|e| format!("Task execution failed: {}", e))?
}

#[cfg(test)]
mod split_view_tests {
    use super::split_frame_key;
    use serde_json::json;

    #[test]
    fn the_split_before_matches_frames_of_the_same_photograph_and_framing() {
        let edit = json!({"exposure": 1.5, "crop": {"x": 10, "y": 20}, "rotation": 2.0,
            "lensDistortionAmount": 80, "masks": [{"id": "a"}]});
        let before = json!({"exposure": 0, "crop": {"x": 10, "y": 20}, "rotation": 2.0,
            "lensDistortionAmount": 80, "masks": []});
        let key = split_frame_key("/a.NEF", &edit);
        assert_eq!(key, split_frame_key("/a.NEF", &before));
        assert_ne!(key, split_frame_key("/b.NEF", &before));
        let mut recropped = before.clone();
        recropped["crop"]["x"] = json!(11);
        assert_ne!(key, split_frame_key("/a.NEF", &recropped));
        let mut perspective = before.clone();
        perspective["transformVertical"] = json!(-10);
        assert_ne!(key, split_frame_key("/a.NEF", &perspective));
    }
}

#[cfg(test)]
mod preview_memory_tests {
    use super::*;

    #[test]
    fn browser_drag_limits_work_without_reducing_explicit_full_quality() {
        assert_eq!(browser_interactive_divisor(6000, "full", 1.0), 1.0);
        assert_eq!(
            browser_interactive_divisor(6000, "high", 1.4),
            6000.0 / 1536.0
        );
        assert_eq!(browser_interactive_divisor(4096, "performance", 1.8), 4.0);
        assert_eq!(browser_interactive_divisor(1024, "high", 1.0), 1.0);
        assert_eq!(browser_interactive_divisor(1024, "high", 1.4), 1.4);
    }

    #[test]
    fn browser_preview_keeps_odd_dimensions_rgb_order_and_ignores_alpha() {
        let image = image::RgbaImage::from_fn(17, 9, |x, _| {
            image::Rgba(if x < 8 {
                [220, 30, 60, 0]
            } else {
                [25, 70, 210, 255]
            })
        });
        for quality in [65, 75, 85, 94] {
            let bytes = encode_browser_preview(&image, quality).unwrap();
            assert_eq!(
                turbojpeg::read_header(&bytes).unwrap().subsamp,
                turbojpeg::Subsamp::None
            );
            let decoded = image::load_from_memory(&bytes).unwrap().to_rgb8();
            assert_eq!(decoded.dimensions(), (17, 9));
            for (x, expected) in [(2, [220u8, 30, 60]), (14, [25, 70, 210])] {
                for (actual, expected) in decoded.get_pixel(x, 4).0.into_iter().zip(expected) {
                    assert!(
                        actual.abs_diff(expected) <= 8,
                        "RGB channel changed: {actual} vs {expected}"
                    );
                }
            }
        }
    }

    /// The full-resolution path: warp and transform the source, then
    /// downscale (what fit-size previews did before the reduced path).
    fn full_resolution_preview(
        source: &DynamicImage,
        adjustments: &serde_json::Value,
        preview_dim: u32,
    ) -> preview_cache::PreviewBase {
        let warped = apply_geometry_warp(source, adjustments);
        let warped = rawtonic_engine::edge_fringe::apply_for_recipe(warped, adjustments, 1.0);
        let (transformed, offset) =
            crate::adjustment_utils::apply_spatial_transformations(warped, adjustments);
        let full_width = transformed.width();
        let image = shared_preview_at_resolution(&Arc::new(transformed.into_owned()), preview_dim);
        preview_cache::PreviewBase {
            scale: image.width() as f32 / full_width as f32,
            image,
            unscaled_crop_offset: offset,
        }
    }

    #[test]
    fn an_offline_standard_preview_is_retouched_at_its_resolution() {
        // A 600 x 400 Standard preview of a 3000 x 2000 photograph with a
        // dark dot at full-resolution (1500, 1000).
        let proxy = DynamicImage::ImageRgb32F(image::Rgb32FImage::from_fn(600, 400, |x, y| {
            let d = ((x as f32 - 300.0).powi(2) + (y as f32 - 200.0).powi(2)).sqrt();
            let v = if d < 4.0 {
                0.02
            } else {
                0.3 + x as f32 / 6000.0
            };
            image::Rgb([v, v, v])
        }));
        let healed = serde_json::json!({"spots": [
            {"x": 1500.0, "y": 1000.0, "radius": 40.0, "sourceX": 1200.0, "sourceY": 1000.0}
        ]});
        for (adjustments, dot) in [(serde_json::json!({}), true), (healed, false)] {
            let base = reduced_preview_base(&proxy, 0.2, 3000, 2000, &adjustments, 640, 0.2)
                .unwrap()
                .image
                .to_rgb32f();
            assert_eq!(base.dimensions(), (600, 400));
            assert_eq!(base.get_pixel(300, 200)[0] < 0.1, dot);
        }
    }

    #[test]
    fn reduced_previews_match_full_resolution_previews() {
        // Smooth detail with a sharp edge, 3000 × 2000.
        let source = DynamicImage::ImageRgb32F(image::Rgb32FImage::from_fn(3000, 2000, |x, y| {
            let fx = x as f32 / 3000.0;
            let fy = y as f32 / 2000.0;
            let edge = if x > 1500 { 0.8 } else { 0.2 };
            image::Rgb([fx, fy, edge * (0.5 + 0.5 * (fx * 9.0).sin())])
        }));
        let lens = serde_json::json!({
            "lensDistortionEnabled": true,
            "lensVignetteEnabled": true,
            "lensDistortionParams": {"k1": -0.04, "k2": 0.01, "k3": 0.0, "model": 0,
                "vig_k1": -0.3, "vig_k2": 0.05, "vig_k3": 0.0},
            "lensDistortionAmount": 100,
            "lensVignetteAmount": 100,
        });
        let mut cropped = lens.clone();
        cropped["crop"] =
            serde_json::json!({"x": 300.5, "y": 210.0, "width": 2100.0, "height": 1400.0});
        cropped["rotation"] = serde_json::json!(2.5);
        for adjustments in [serde_json::json!({}), lens, cropped] {
            let scale = reduced_preview_scale(3000, 2000, &adjustments, 640).expect("reduced");
            let reduced =
                reduced_preview_base(&source, 1.0, 3000, 2000, &adjustments, 640, scale).unwrap();
            let full = full_resolution_preview(&source, &adjustments, 640);
            // Sizes may differ by a rounding pixel (resampled twice).
            let (rw, rh) = reduced.image.dimensions();
            let (fw, fh) = full.image.dimensions();
            assert!(
                rw.abs_diff(fw) <= 1 && rh.abs_diff(fh) <= 1,
                "{rw}×{rh} vs {fw}×{fh}"
            );
            assert!(
                (reduced.scale - full.scale).abs() / full.scale < 0.01,
                "{} vs {}",
                reduced.scale,
                full.scale
            );
            assert_eq!(reduced.unscaled_crop_offset, full.unscaled_crop_offset);
            let (a, b) = (reduced.image.to_rgb32f(), full.image.to_rgb32f());
            let (w, h) = (rw.min(fw), rh.min(fh));
            let mut sum = 0.0;
            for y in 0..h {
                for x in 0..w {
                    let (p, q) = (a.get_pixel(x, y).0, b.get_pixel(x, y).0);
                    sum += (0..3).map(|c| (p[c] - q[c]).abs()).sum::<f32>() / 3.0;
                }
            }
            let mean = sum / (w * h) as f32;
            assert!(mean < 0.01, "mean difference {mean} for {adjustments}");
        }
    }

    /// Timing of a fit-view preview with face retouch on a 51 MP source:
    /// `FACE_RECIPE` = a render harness JSON (ai_faces'
    /// renders_face_retouch_of_real_photos) of a 4000 px wide photo,
    /// `FACE_SETTINGS` the sliders. Prints each step's time.
    #[test]
    #[ignore]
    fn times_face_retouch_previews() {
        let Some(path) = std::env::var_os("FACE_RECIPE") else {
            eprintln!("SKIP: FACE_RECIPE is needed");
            return;
        };
        let data: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let (w, h) = (8256u32, 6192u32);
        let mut adjustments = serde_json::json!({"faceRetouch": data["faceRetouch"].clone()});
        adjustments["faceRetouch"]["settings"] = std::env::var("FACE_SETTINGS")
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or(serde_json::json!({"blemishes": 60, "evenSkin": 60}));
        let adjustments =
            rawtonic_engine::face_retouch::scaled_recipe(&adjustments, 8256.0 / 4000.0);
        let source = DynamicImage::ImageRgb32F(image::Rgb32FImage::from_fn(w, h, |x, y| {
            let t = (((x * 7 + y * 13) % 5) as f32 - 2.0) * 0.004;
            image::Rgb([0.24 + t, 0.18 + t, 0.15 + t])
        }));
        let dim: u32 = std::env::var("PREVIEW_DIM")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1024);
        let scale = reduced_preview_scale(w, h, &adjustments, dim).expect("reduced");
        for round in 0..3 {
            let start = std::time::Instant::now();
            let reduced = downscale_f32_image(
                &source,
                (w as f32 * scale).round() as u32,
                (h as f32 * scale).round() as u32,
            );
            let downscale = start.elapsed();
            let start = std::time::Instant::now();
            let s = reduced.width() as f32 / w as f32;
            let _ =
                crate::image_loader::composite_patches_on_scaled_image(&reduced, &adjustments, s)
                    .unwrap();
            let retouch = start.elapsed();
            let start = std::time::Instant::now();
            let _ = reduced_preview_base(&source, 1.0, w, h, &adjustments, dim, scale).unwrap();
            let whole = start.elapsed();
            println!(
                "round {round}: preview {}×{}: downscale {:.0} ms, face retouch {:.0} ms, whole reduced base {:.0} ms",
                reduced.width(),
                reduced.height(),
                downscale.as_secs_f64() * 1e3,
                retouch.as_secs_f64() * 1e3,
                whole.as_secs_f64() * 1e3
            );
        }
    }

    #[test]
    fn previews_close_to_full_resolution_keep_the_full_path() {
        assert!(reduced_preview_scale(3000, 2000, &serde_json::json!({}), 2560).is_none());
        let tight = serde_json::json!({"crop": {"x": 0, "y": 0, "width": 900, "height": 600}});
        assert!(reduced_preview_scale(3000, 2000, &tight, 640).is_none());
        let patched = serde_json::json!({"aiPatches": [{"id": "p"}]});
        assert!(reduced_preview_scale(3000, 2000, &patched, 640).is_none());
        // Face retouch scales with the preview.
        let faces = serde_json::json!({"faceRetouch": {"settings": {"evenSkin": 50},
            "faces": [{"crop": [1, 0, 0, 0, 1, 0], "size": 8, "eyeDistance": 100,
                "skin": ""}]}});
        assert!(reduced_preview_scale(3000, 2000, &faces, 640).is_some());
    }

    #[test]
    fn no_op_geometry_reuses_the_original_pixel_buffer() {
        let loaded = LoadedImage {
            path: "memory-fixture".into(),
            image: Arc::new(DynamicImage::ImageRgb32F(image::Rgb32FImage::new(12, 8))),
            is_raw: true,
            multiscale: None,
        };
        let result = compute_patched_and_warped(&loaded, &serde_json::json!({})).unwrap();
        assert!(Arc::ptr_eq(&loaded.image, &result));
    }

    #[test]
    fn previews_of_photos_over_the_texture_limit_are_downsampled_to_it() {
        // A 100 % view of a 40 000 px panorama asks for its full size.
        let limit = 64;
        let dim = preview_dim_within_limit(400, limit);
        assert_eq!(dim, limit);
        assert_eq!(preview_dim_within_limit(48, limit), 48);
        let panorama = Arc::new(DynamicImage::ImageRgb32F(image::Rgb32FImage::new(400, 90)));
        let preview = shared_preview_at_resolution(&panorama, dim);
        assert!(preview.width() <= limit && preview.height() <= limit);
        assert_eq!(preview.width(), limit);
        let reduced = reduced_preview_scale(400, 90, &serde_json::json!({}), dim).unwrap();
        let base = reduced_preview_base(
            &panorama,
            1.0,
            400,
            90,
            &serde_json::json!({}),
            dim,
            reduced,
        )
        .unwrap();
        assert!(base.image.width() <= limit && base.image.height() <= limit);
    }

    #[test]
    fn native_resolution_preview_shares_pixels_without_upsampling() {
        let original = Arc::new(DynamicImage::ImageRgb32F(image::Rgb32FImage::from_fn(
            12,
            8,
            |x, y| image::Rgb([x as f32 / 12.0, y as f32 / 8.0, 0.25]),
        )));
        for resolution in [12, 24] {
            let preview = shared_preview_at_resolution(&original, resolution);
            assert!(Arc::ptr_eq(&original, &preview));
            assert_eq!(preview.dimensions(), (12, 8));
        }
        let reduced = shared_preview_at_resolution(&original, 6);
        assert_eq!(reduced.dimensions(), (6, 4));
        assert_eq!(
            reduced.as_bytes(),
            downscale_f32_image(&original, 6, 6).as_bytes()
        );
    }
    #[test]
    fn final_preview_defers_interactive_resampling_and_preserves_prepared_pixels() {
        let original = Arc::new(DynamicImage::ImageRgb32F(image::Rgb32FImage::new(12, 8)));
        assert!(prepare_interactive_preview(&original, 12, 1.4, false, None).is_none());
        let full = prepare_interactive_preview(&original, 12, 1.0, true, None).unwrap();
        assert!(Arc::ptr_eq(&original, &full));
        let small = prepare_interactive_preview(&original, 12, 1.4, true, None).unwrap();
        assert_eq!(
            small.as_bytes(),
            downscale_f32_image(&original, 8, 5).as_bytes()
        );
        for interactive in [false, true] {
            let cached =
                prepare_interactive_preview(&original, 12, 1.4, interactive, Some(&small)).unwrap();
            assert!(Arc::ptr_eq(&small, &cached));
        }
    }
}
