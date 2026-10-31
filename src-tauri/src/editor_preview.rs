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
use crate::image_processing::get_or_init_gpu_context;
use crate::image_processing::process_and_get_dynamic_image;
use crate::image_processing::resolve_tonemapper_override;
use crate::image_processing::resolve_tonemapper_override_from_handle;
use crate::image_processing::warp_image_geometry;
use crate::image_processing::{
    get_all_adjustments_from_json, get_all_adjustments_with_masks, mask_definitions_from_json,
};
use crate::lut_processing;
use crate::mask_generation::MaskDefinition;
use crate::mask_generation::get_cached_or_generate_mask;
use crate::mask_generation::prepare_parametric_mask_source;
use crate::mask_generation::resolve_warped_image_for_masks;
use crate::mask_generation::{MaskBitmap, PhotoGeometry, generate_layer_mask};
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
use rawtonic_engine::scopes;
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
    /// The viewer's area at any zoom (see `WgpuDisplay::viewer_area`).
    #[serde(default)]
    pub viewer_area: Option<[f32; 4]>,
    /// The before/after split's divider, screen coordinate in device pixels (the unit
    /// of `x`); `None` while the split view is off.
    #[serde(default)]
    pub split: Option<f32>,
    #[serde(default)]
    pub split_vertical: bool,
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
    "sourceFrameVersion",
];

/// The photograph and framing of a frame: a split before is drawn only
/// beside frames with its key, never beside another crop or photograph.
/// What a rendered split before shows: its frame key, source, load,
/// size, recipe and whether the source was interim (its base then has no
/// measured lateral CA, `interim_source`).
fn split_render_key(
    frame_key: u64,
    source: &Arc<DynamicImage>,
    load_generation: usize,
    preview_dim: u32,
    adjustments: &serde_json::Value,
    interim: bool,
) -> u64 {
    let mut hasher = DefaultHasher::new();
    frame_key.hash(&mut hasher);
    (Arc::as_ptr(source) as usize).hash(&mut hasher);
    load_generation.hash(&mut hasher);
    preview_dim.hash(&mut hasher);
    adjustments.to_string().hash(&mut hasher);
    interim.hash(&mut hasher);
    hasher.finish()
}

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
            params
                .auto_ca_versus_own
                .then(|| crate::image_processing::own_tca_model(&params)),
        );
    }
}

/// Whether `reduced`, a reduced copy of a `full_dims` image, can be warped
/// without measuring its automatic lateral CA itself: the shared estimate
/// for this recipe's version and lens model is still cached, or the copy is
/// not reduced. A cached preview base whose estimate is missing (Remove CA
/// switched on, the lens model changed, or the estimate evicted) is rebuilt
/// from the full-resolution image instead.
fn lateral_ca_is_shared(
    reduced: &DynamicImage,
    full_dims: (u32, u32),
    adjustments: &serde_json::Value,
) -> bool {
    let params = crate::image_processing::get_geometry_params_from_json(adjustments);
    if !params.auto_ca || reduced.dimensions() == full_dims {
        return true;
    }
    reduced.as_rgb32f().is_none_or(|reduced| {
        rawtonic_engine::lateral_ca::is_shared(
            reduced,
            params.auto_ca_version,
            params
                .auto_ca_versus_own
                .then(|| crate::image_processing::own_tca_model(&params)),
        )
    })
}

/// The preview base from `source`, a copy of the full-resolution image at
/// `source_scale`: resampled to `scale` (if smaller), warped, cropped and
/// rotated with the crop scaled accordingly. The returned GPU scale and crop
/// offset refer to full-resolution pixels, as for a full-resolution preview.
/// Retouching (patches, spots) and lens blur are applied at this resolution:
/// recipes with them reach this only for an offline photograph's Standard
/// preview (online they use the full decode).
#[allow(clippy::too_many_arguments)]
pub(crate) fn reduced_preview_base(
    source: &DynamicImage,
    source_scale: f32,
    full_width: u32,
    full_height: u32,
    adjustments: &serde_json::Value,
    is_raw: bool,
    preview_dim: u32,
    scale: f32,
) -> Result<preview_cache::PreviewBase, String> {
    build_reduced_preview_base(
        source,
        source_scale,
        (full_width, full_height),
        adjustments,
        is_raw,
        preview_dim,
        scale,
        false,
    )
}

/// `reduced_preview_base` of a reduced `source` that stands in only until
/// the photograph at full resolution is in (`build_reduced_preview_base`
/// with `interim`).
#[allow(clippy::too_many_arguments)]
pub(crate) fn interim_preview_base(
    source: &DynamicImage,
    source_scale: f32,
    full_width: u32,
    full_height: u32,
    adjustments: &serde_json::Value,
    is_raw: bool,
    preview_dim: u32,
    scale: f32,
) -> Result<preview_cache::PreviewBase, String> {
    build_reduced_preview_base(
        source,
        source_scale,
        (full_width, full_height),
        adjustments,
        is_raw,
        preview_dim,
        scale,
        true,
    )
}

/// `reduced_preview_base` with `interim`: `source` (a reduced copy) stands
/// in only until the photograph's decode is in, so its warp does not
/// measure the automatic lateral CA on it (`warp_image_geometry_interim`).
#[allow(clippy::too_many_arguments)]
fn build_reduced_preview_base(
    source: &DynamicImage,
    source_scale: f32,
    (full_width, full_height): (u32, u32),
    adjustments: &serde_json::Value,
    is_raw: bool,
    preview_dim: u32,
    scale: f32,
    interim: bool,
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
        #[cfg(feature = "performance-profiling")]
        let _stage = crate::performance::PerformanceStage::new("preview_base_ca_share");
        share_lateral_ca(source, &retouched, adjustments);
    }
    let warped = {
        #[cfg(feature = "performance-profiling")]
        let _stage = crate::performance::PerformanceStage::new("preview_base_warp");
        if interim {
            crate::image_processing::apply_geometry_warp_interim(retouched, &scaled, is_raw)?
        } else {
            apply_geometry_warp(retouched, &scaled, is_raw)?
        }
    };
    let warped = {
        #[cfg(feature = "performance-profiling")]
        let _stage = crate::performance::PerformanceStage::new("preview_base_fringe");
        rawtonic_engine::edge_fringe::apply_for_recipe(warped, adjustments, scale)?
    };
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
    state: &AppState,
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
            loaded_image.is_raw,
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

/// The key of the patched and warped full-resolution image of
/// `adjustments`: a fringe draft's (`edge_fringe::DRAFT_KEY`, built while a
/// control is dragged) never the key of the finished image.
fn patched_warped_key(adjustments: &serde_json::Value) -> u64 {
    let key = crate::cache_utils::calculate_patched_warped_hash(adjustments);
    if rawtonic_engine::edge_fringe::is_draft(adjustments) {
        preview_cache::fringe_draft_key(key)
    } else {
        key
    }
}

pub(crate) fn compute_full_transformed_res(
    state: &AppState,
    loaded_image: &LoadedImage,
    adjustments: &serde_json::Value,
) -> Result<(Arc<DynamicImage>, (f32, f32)), String> {
    let geo_hash = source_transform_hash(&loaded_image.image, patched_warped_key(adjustments));

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

    let warped = apply_geometry_warp(patched_image, adjustments, loaded_image.is_raw)?;
    let warped = rawtonic_engine::edge_fringe::apply_for_recipe(warped, adjustments, 1.0)?;
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

/// The warped image masks are computed on (`prepare_shared_warped_mask_image`),
/// cached for the editor photo. Fails only where the warp runs on the GPU
/// and the GPU fails.
pub fn get_cached_full_warped_image(
    state: &AppState,
    source: &Arc<DynamicImage>,
    is_raw: bool,
    js_adjustments: &serde_json::Value,
) -> Result<Arc<DynamicImage>, String> {
    full_warped_image(state, source, is_raw, js_adjustments, false)
        .expect("only a prewarm skips the build")
}

/// `get_cached_full_warped_image` on a blocking worker, for async commands
/// (a full-resolution warp must not hold an async runtime thread).
pub async fn cached_full_warped_image(
    app_handle: &tauri::AppHandle,
    source: Arc<DynamicImage>,
    is_raw: bool,
    js_adjustments: serde_json::Value,
) -> Result<Arc<DynamicImage>, String> {
    let app_handle = app_handle.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let state = app_handle.state::<AppState>();
        get_cached_full_warped_image(&state, &source, is_raw, &js_adjustments)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// `get_cached_full_warped_image`; a `prewarm` builds nothing for a photo that
/// is not (or no longer) the editor photo and returns `None` then.
fn full_warped_image(
    state: &AppState,
    source: &Arc<DynamicImage>,
    is_raw: bool,
    js_adjustments: &serde_json::Value,
    prewarm: bool,
) -> Option<Result<Arc<DynamicImage>, String>> {
    let geo_hash = calculate_geometry_hash(js_adjustments);
    // Whether `source` is the editor photo, and its cached warp if so.
    let lookup = || {
        // Only the current editor source may use its full-resolution warp cache.
        // Keep this guard until the cache lookup so a source switch cannot route
        // an old job through the new editor image's cache.
        let original = state
            .original_image
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if !original
            .as_ref()
            .is_some_and(|loaded| Arc::ptr_eq(&loaded.image, source))
        {
            return (false, None);
        }
        let cache = state
            .full_warped_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        (
            true,
            cache
                .as_ref()
                .and_then(|cache| cache.get(source, is_raw, geo_hash))
                .map(Ok),
        )
    };
    let key = (Arc::as_ptr(source) as usize, is_raw, geo_hash);
    cached_or_built(&state.full_warped_build, key, lookup, prewarm, |ticket| {
        let warped = crate::mask_generation::prepare_shared_warped_mask_image(
            source,
            is_raw,
            js_adjustments,
        )?;

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
                return Ok(image);
            }
            // A build for an older geometry that ends last keeps its result
            // to itself instead of replacing the newer one.
            if state.full_warped_build.may_publish(ticket) {
                *cache = Some(crate::mask_generation::FullWarpedMaskCache::new(
                    source,
                    is_raw,
                    geo_hash,
                    Arc::clone(&warped),
                ));
            }
        }
        Ok(warped)
    })
}

/// The key of a full-resolution warp build: source image, raw curve, geometry.
pub(crate) type WarpBuildKey = (usize, bool, u64);

/// The editor photo's warp builds in progress, by key, so a caller waits only
/// for a build that will serve it.
#[derive(Default)]
pub(crate) struct BuildSlot<K> {
    building: std::sync::Mutex<Vec<K>>,
    done: std::sync::Condvar,
    /// Builds are numbered as they start; the latest started one published.
    started: std::sync::atomic::AtomicU64,
    published: std::sync::atomic::AtomicU64,
}

impl<K: PartialEq> BuildSlot<K> {
    fn is_building(&self, key: &K) -> bool {
        self.building
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(key)
    }

    /// Whether build `ticket` may publish its result: no build that started
    /// after it (a newer geometry) has. Call it under the lock the result is
    /// stored with, so the decision and the store are one step.
    fn may_publish(&self, ticket: u64) -> bool {
        self.published.fetch_max(ticket, Ordering::SeqCst) <= ticket
    }
}

/// Ends a build in a `BuildSlot` (also on unwind) and wakes its waiters.
struct BuildingGuard<'a, K: PartialEq> {
    slot: &'a BuildSlot<K>,
    key: K,
}

impl<K: PartialEq> Drop for BuildingGuard<'_, K> {
    fn drop(&mut self) {
        let mut building = self.slot.building.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(i) = building.iter().position(|k| *k == self.key) {
            building.swap_remove(i);
        }
        self.slot.done.notify_all();
    }
}

/// The cached value `lookup` finds (it also tells whether the photo is still
/// the editor's), else `build`'s. For the editor photo, a caller whose `key`
/// is being built waits for that build and reuses it (a stroke during a
/// prewarm); any other key builds at once, so a build for a photo or
/// geometry the user already left never holds up the current one. A photo
/// that is not (or no longer) the editor's is built without caching, or not
/// at all for a `prewarm`. `build` gets the build's ticket (`may_publish`;
/// 0 when it must not publish).
fn cached_or_built<K: PartialEq + Clone, T>(
    slot: &BuildSlot<K>,
    key: K,
    lookup: impl Fn() -> (bool, Option<T>),
    prewarm: bool,
    build: impl FnOnce(u64) -> T,
) -> Option<T> {
    let mut building = slot.building.lock().unwrap_or_else(|e| e.into_inner());
    loop {
        let (current, cached) = lookup();
        if cached.is_some() {
            return cached;
        }
        if !current {
            drop(building);
            return (!prewarm).then(|| build(0));
        }
        if !building.contains(&key) {
            break;
        }
        building = slot.done.wait(building).unwrap_or_else(|e| e.into_inner());
    }
    building.push(key.clone());
    let ticket = slot.started.fetch_add(1, Ordering::SeqCst) + 1;
    drop(building);
    let _guard = BuildingGuard { slot, key };
    Some(build(ticket))
}

/// The editor wants the Auto Mask source of `path` built ahead of the first
/// stroke (the layers panel is open with Auto Mask on; `None`: no longer).
/// Builds it now from the photo's last final preview recipe; later final
/// previews build it again when the source or its geometry changed
/// (`rewarm_mask_source`).
#[tauri::command]
pub(crate) async fn prewarm_mask_source(
    path: Option<String>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    let state = app_handle.state::<AppState>();
    *state
        .mask_source_wanted
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = path.clone();
    let recipe = state
        .mask_source_recipe
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .filter(|(recipe_path, _)| Some(recipe_path) == path.as_ref());
    let Some((path, recipe)) = recipe else {
        return Ok(());
    };
    tauri::async_runtime::spawn_blocking(move || prewarm_now(&app_handle, &path, &recipe))
        .await
        .map_err(|e| format!("Mask source prewarm failed: {e}"))
}

/// Builds the editor photo's full-resolution mask source (see
/// `get_cached_full_warped_image`) so the first Auto Mask dab does not wait
/// for it (~0.4 s at 40 MP), from a final preview's hydrated `recipe`: the
/// same image and key the stroke would build.
fn prewarm_now(app_handle: &tauri::AppHandle, path: &str, recipe: &serde_json::Value) {
    let state = app_handle.state::<AppState>();
    let Some(loaded) = state
        .original_image
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .filter(|loaded| loaded.path == path)
    else {
        return;
    };
    #[cfg(feature = "performance-profiling")]
    let _stage = crate::performance::PerformanceStage::new("mask_source_prewarm");
    full_warped_image(&state, &loaded.image, loaded.is_raw, recipe, true);
}

/// After a final preview: keeps its recipe for `prewarm_mask_source`, and
/// when the editor wants this photo's Auto Mask source and it no longer
/// matches (AI Denoise replaced the source, a lens or geometry edit), builds
/// it again in the background.
fn rewarm_mask_source(
    app_handle: &tauri::AppHandle,
    state: &AppState,
    loaded: &LoadedImage,
    recipe: serde_json::Value,
) {
    let recipe = Arc::new(recipe);
    *state
        .mask_source_recipe
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = Some((loaded.path.clone(), Arc::clone(&recipe)));
    let wanted = state
        .mask_source_wanted
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_deref()
        == Some(loaded.path.as_str());
    let editor_photo = state
        .original_image
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .is_some_and(|original| Arc::ptr_eq(&original.image, &loaded.image));
    if !wanted || !editor_photo {
        return;
    }
    let geo_hash = calculate_geometry_hash(&recipe);
    let cached = state
        .full_warped_cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .is_some_and(|cache| cache.get(&loaded.image, loaded.is_raw, geo_hash).is_some());
    // The same build already running (a prewarm or a stroke) serves it.
    let key = (Arc::as_ptr(&loaded.image) as usize, loaded.is_raw, geo_hash);
    if cached || state.full_warped_build.is_building(&key) {
        return;
    }
    let (app_handle, path) = (app_handle.clone(), loaded.path.clone());
    std::thread::spawn(move || prewarm_now(&app_handle, &path, &recipe));
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
            display.viewer_area = payload.viewer_area.or(display.viewer_area);
            display.split_x = payload.split;
            display.split_vertical = payload.split_vertical;
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
                    decode: None,
                    measures: proxy.measures,
                },
                scale,
                (proxy.full_width, proxy.full_height),
            )
        }
    })
}

/// `geometry` with the photo's size for a render from `preview_source`:
/// the full decode's own when `loaded` is it (which also places imported
/// Lightroom masks, `PhotoGeometry::with_decoded_size`), else the size a
/// preview proxy records.
pub(crate) fn source_size(
    geometry: PhotoGeometry,
    loaded: &LoadedImage,
    full_dims: (u32, u32),
) -> PhotoGeometry {
    if loaded.image.dimensions() == full_dims {
        geometry.with_decoded_size(full_dims.0, full_dims.1)
    } else {
        geometry.with_photo_size(full_dims.0 as f64, full_dims.1 as f64)
    }
}

fn is_offline(state: &AppState, loaded_image: &LoadedImage) -> bool {
    state
        .offline_source
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_deref()
        == Some(loaded_image.path.as_str())
}

/// Whether `loaded_image` at `source_scale` is shown only until the decode
/// is in: online, a reduced source (the decode's draft or a stored proxy).
/// Its base does not measure the automatic lateral CA
/// (`build_reduced_preview_base`) and is cached apart.
pub(crate) fn interim_source(
    state: &AppState,
    loaded_image: &LoadedImage,
    source_scale: f32,
) -> bool {
    source_scale < 1.0 && !is_offline(state, loaded_image)
}

/// The preview base (warped, cropped, retouched source at `build_dim`) for
/// `adjustments`, from the reusable previews or built and kept there.
/// `base_adjustments` may differ by a draft flag only.
#[allow(clippy::too_many_arguments)]
fn preview_base(
    state: &AppState,
    loaded_image: &LoadedImage,
    source_scale: f32,
    full_dims: (u32, u32),
    adjustments: &serde_json::Value,
    base_adjustments: &serde_json::Value,
    build_dim: u32,
    preparation_key: u64,
) -> Result<preview_cache::PreviewBase, String> {
    let offline = is_offline(state, loaded_image);
    // Its base is kept apart: the same proxy may turn out to be an offline
    // photograph's lasting source.
    let interim = source_scale < 1.0 && !offline;
    let cache_key = if interim {
        preview_cache::interim_key(preparation_key)
    } else {
        preparation_key
    };
    let reusable = state
        .reusable_previews
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&loaded_image.image, cache_key, build_dim);
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
        if needs_full && !offline {
            return Err("The photograph is still loading".to_string());
        }
        #[cfg(feature = "performance-profiling")]
        let _stage = crate::performance::PerformanceStage::new("preview_proxy_base");
        let scale = reduced_preview_scale(full_width, full_height, adjustments, build_dim)
            .unwrap_or(source_scale);
        build_reduced_preview_base(
            &loaded_image.image,
            source_scale,
            full_dims,
            base_adjustments,
            loaded_image.is_raw,
            build_dim,
            scale,
            interim,
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
        .insert(&loaded_image.image, cache_key, build_dim, preview.clone());
    Ok(preview)
}

/// Tone targets' sample: the colours (linear sRGB, signed: the working gamut
/// reaches beyond sRGB) the photo's region and point curves receive in the
/// pixels around a point of the shown preview (`preview_sample_roi`), each
/// on its own so the frontend averages them in the curve's input space,
/// rendered from the preview base with those curves and everything after
/// them left out
/// (`point_curve::curve_input_probe`), so layer curves, LUTs and view states
/// do not hide it. Rendered in ProPhoto, whose cube holds the working gamut,
/// so nothing beyond sRGB is clipped. `adjustments` is the recipe (without
/// view states). None while no preview base of the current recipe's
/// geometry is cached.
#[tauri::command]
pub async fn sample_curve_input(
    path: String,
    normalized_x: f64,
    normalized_y: f64,
    adjustments: serde_json::Value,
    app_handle: tauri::AppHandle,
) -> Result<Option<Vec<[f64; 3]>>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let recipe = rawtonic_engine::point_curve::curve_input_probe(&adjustments);
        let image = render_probe(
            &app_handle,
            &path,
            recipe,
            ProbeArea::Point(normalized_x, normalized_y),
            ProbeOutput::Space(rawtonic_engine::output_color_space::OutputColorSpace::ProPhoto),
            "sample_curve_input",
        )?;
        Ok(image.map(|image| prophoto_pixels(image.to_rgba8().as_raw())))
    })
    .await
    .map_err(|error| format!("Curve input sampling task failed: {error}"))?
}

/// Levels' histogram (Capture One shows the tool's input): counts per 8-bit
/// display level of red, green and blue (every pixel, as the main histogram
/// counts; the panel smooths them as the main one) of the shown
/// preview base rendered up to Levels (the curves' probe,
/// `point_curve::curve_input_probe`, without the levels, stopped where
/// Levels act: `LEVELS_INPUT_PROBE`), the display values Levels receive. `adjustments` is the
/// recipe (without view states); its levels do not matter, so the frontend
/// asks again only when another adjustment changes. None while no preview
/// base of the recipe's geometry is cached.
#[tauri::command]
pub async fn levels_input_histogram(
    path: String,
    adjustments: serde_json::Value,
    app_handle: tauri::AppHandle,
) -> Result<Option<[Vec<u32>; 3]>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        #[cfg(feature = "performance-profiling")]
        let _stage = crate::performance::PerformanceStage::new("levels_input_histogram");
        let Some(image) = render_probe(
            &app_handle,
            &path,
            levels_input_probe(&adjustments),
            ProbeArea::Whole,
            ProbeOutput::LevelsInput,
            "levels_input_histogram",
        )?
        else {
            return Ok(None);
        };
        Ok(Some(level_counts(&image)))
    })
    .await
    .map_err(|error| format!("Levels histogram task failed: {error}"))?
}

/// The recipe Levels' input renders from: the curves' probe (curves,
/// region curve, display LUT, grain and view states left out) without the
/// levels.
fn levels_input_probe(adjustments: &serde_json::Value) -> serde_json::Value {
    let mut recipe = rawtonic_engine::point_curve::curve_input_probe(adjustments);
    if let Some(recipe) = recipe.as_object_mut() {
        recipe.remove("levels");
    }
    recipe
}

/// Per-channel counts of 8-bit levels over every pixel (as the main
/// histogram counts them).
fn level_counts(image: &image::DynamicImage) -> [Vec<u32>; 3] {
    let counts = scopes::histogram_counts(
        image,
        scopes::ScopeSpace::of(rawtonic_engine::output_color_space::OutputColorSpace::Srgb),
    );
    [
        counts.red.to_vec(),
        counts.green.to_vec(),
        counts.blue.to_vec(),
    ]
}

/// What a probe render covers: the pixels around a point of the shown
/// preview (`preview_sample_roi`) or the whole preview base.
enum ProbeArea {
    Point(f64, f64),
    Whole,
}

/// What a probe render stores: the finished colour in an output space, or
/// Levels' input (sRGB display values clipped to the cube, before any
/// output mapping: `LEVELS_INPUT_PROBE`).
enum ProbeOutput {
    Space(rawtonic_engine::output_color_space::OutputColorSpace),
    LevelsInput,
}

/// Renders `recipe` (a probe of the current recipe, without view states)
/// from the cached preview base the shown frame was built from, with the
/// layers' masks as the preview draws them, as `output` says. None when `path` is
/// not the loaded photo or no preview base of the recipe's geometry is
/// cached.
fn render_probe(
    app_handle: &tauri::AppHandle,
    path: &str,
    mut recipe: serde_json::Value,
    area: ProbeArea,
    output: ProbeOutput,
    caller_id: &str,
) -> Result<Option<image::DynamicImage>, String> {
    let state = app_handle.state::<AppState>();
    let context = get_or_init_gpu_context(&state, app_handle)?;
    hydrate_adjustments(&state, &mut recipe);
    let (loaded_image, source_scale, full_dims) = preview_source(&state)?;
    if loaded_image.path != path {
        return Ok(None);
    }
    let transform_hash = calculate_transform_hash(&recipe);
    let Some((base, scale, unscaled_crop_offset)) = state
        .cached_preview
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .filter(|c| c.source.ptr_eq(&Arc::downgrade(&loaded_image.image)))
        .filter(|c| c.transform_hash == transform_hash)
        .map(|c| (Arc::clone(&c.image), c.scale, c.unscaled_crop_offset))
    else {
        return Ok(None);
    };
    let (width, height) = base.dimensions();
    let roi = match area {
        ProbeArea::Point(x, y) => {
            let Some(roi) = gpu_processing::preview_sample_roi(width, height, x, y) else {
                return Ok(None);
            };
            Some(roi)
        }
        ProbeArea::Whole => None,
    };
    // The layers' masks as the preview frame draws them.
    let mask_definitions = mask_definitions_from_json(&recipe);
    let mask_bitmaps = preview_mask_bitmaps(
        &state,
        &recipe,
        &mask_definitions,
        (&loaded_image, source_scale, full_dims),
        (width, height),
        scale,
        unscaled_crop_offset,
    )?;
    let tm_override = resolve_tonemapper_override_from_handle(app_handle, loaded_image.is_raw);
    let mut probe = get_all_adjustments_with_masks(
        &recipe,
        loaded_image.is_raw,
        tm_override,
        &mask_definitions,
    );
    crate::baseline_exposure::apply_base_look(
        &mut probe.global,
        &loaded_image.path,
        loaded_image.measures.as_ref(),
    );
    probe.global.detail_scale = scale;
    match output {
        ProbeOutput::Space(space) => {
            probe.global.output_space[0] = space.shader_code();
            probe.global.output_matrix =
                image_processing::gpu_mat3_from_rows(space.from_linear_srgb_f32());
        }
        ProbeOutput::LevelsInput => {
            probe.global.output_space[3] = image_processing::LEVELS_INPUT_PROBE;
        }
    }
    let lut = recipe["lutPath"]
        .as_str()
        .and_then(|p| lut_processing::get_or_load_lut(&state, p).ok());
    gpu_processing::process_and_get_dynamic_image_with_preview_shader(
        &context,
        &state,
        &base,
        transform_hash,
        RenderRequest {
            adjustments: probe,
            mask_bitmaps: &mask_bitmaps,
            lut,
            roi,
        },
        caller_id,
    )
    .map(Some)
}

/// The bitmaps of `masks` (`recipe`'s) on a preview base of `size` at
/// `scale` whose crop starts at `unscaled_crop_offset`, from the photograph
/// (`source`: image, proxy pixels per full-resolution pixel, full size), as
/// the preview frame draws them.
pub(crate) fn preview_mask_bitmaps(
    state: &AppState,
    recipe: &serde_json::Value,
    masks: &[MaskDefinition],
    (loaded_image, source_scale, full_dims): (&LoadedImage, f32, (u32, u32)),
    (width, height): (u32, u32),
    scale: f32,
    unscaled_crop_offset: (f32, f32),
) -> Result<Vec<Arc<MaskBitmap>>, String> {
    let warped_image = resolve_warped_image_for_masks(
        state,
        &loaded_image.image,
        loaded_image.is_raw,
        source_scale,
        recipe,
        masks,
    )?;
    let mask_geometry = source_size(
        PhotoGeometry::from_adjustments(recipe),
        loaded_image,
        full_dims,
    );
    let crop_offset = (
        unscaled_crop_offset.0 * scale,
        unscaled_crop_offset.1 * scale,
    );
    masks
        .iter()
        .filter_map(|def| {
            get_cached_or_generate_mask(
                state,
                def,
                width,
                height,
                scale,
                crop_offset,
                &mask_geometry,
                warped_image.as_ref(),
            )
            .transpose()
        })
        .collect()
}

/// RGBA8 ProPhoto pixels as signed linear sRGB, one colour per pixel.
fn prophoto_pixels(rgba: &[u8]) -> Vec<[f64; 3]> {
    rgba.as_chunks::<4>()
        .0
        .iter()
        .map(|p| prophoto_to_linear_srgb([p[0], p[1], p[2]].map(f64::from)))
        .collect()
}

/// An 8-bit ProPhoto colour (0–255) as signed linear sRGB.
fn prophoto_to_linear_srgb(rgb: [f64; 3]) -> [f64; 3] {
    let space = rawtonic_engine::output_color_space::OutputColorSpace::ProPhoto;
    let linear = rgb.map(|v| space.transfer().to_linear(v / 255.0));
    let m = space.to_linear_srgb();
    std::array::from_fn(|r| (0..3).map(|c| m[r][c] * linear[c]).sum())
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
    // The edge-fringe pass's chromatic-aberration stage (20–110 ms at
    // preview sizes) waits for the drag's end too: a base rebuilt during a
    // drag (a geometry control) is built without it, and the frame after
    // the drag builds it again with it.
    let fringe_draft =
        is_interactive && rawtonic_engine::edge_fringe::has_draft(&adjustments_clone);
    let interim = interim_source(&state, &loaded_image, source_scale);
    // A lasting frame checks the whole preparation too (`serves_frame`): a
    // recipe shown in place of the edit (a snapshot in the held peek, undo)
    // can differ from the base's in inputs the transform hash takes by size.
    let base_valid = cached_preview_lock.as_ref().is_some_and(|c| {
        c.serves_frame(
            &loaded_image.image,
            new_transform_hash,
            interim,
            is_interactive,
            || preview_cache::preparation_key(&adjustments_clone),
        ) && (c.preview_dim == preview_dim || Some(c.preview_dim) == draft_dim)
    });
    let small_valid = base_valid
        && cached_preview_lock.as_ref().is_some_and(|c| {
            c.interactive_divisor == interactive_divisor && c.small_image.is_some()
        });

    let (
        final_preview_base,
        scale_for_gpu,
        unscaled_crop_offset,
        base_dim,
        base_fringe_draft,
        base_preparation_key,
    ) = if base_valid {
        let cached = cached_preview_lock.as_ref().unwrap();
        (
            Arc::clone(&cached.image),
            cached.scale,
            cached.unscaled_crop_offset,
            cached.preview_dim,
            cached.fringe_draft,
            cached.preparation_key,
        )
    } else {
        *state
            .gpu_image_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = None;

        let preparation_key = preview_cache::preparation_key(&adjustments_clone);
        let preparation_key = if fringe_draft {
            preview_cache::fringe_draft_key(preparation_key)
        } else {
            preparation_key
        };
        // The quick preview heals crowded blemishes once (draft), and
        // leaves out the fringe pass's chromatic-aberration stage.
        let draft_adjustments;
        let base_adjustments = if draft_dim.is_some() || fringe_draft {
            let mut draft = adjustments_clone.clone();
            if draft_dim.is_some() {
                draft["faceRetouch"]["draft"] = serde_json::json!(true);
            }
            if fringe_draft {
                draft[rawtonic_engine::edge_fringe::DRAFT_KEY] = serde_json::json!(true);
            }
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
            fringe_draft,
            preparation_key,
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
        interim,
        fringe_draft: base_fringe_draft,
        preparation_key: base_preparation_key,
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
    let mut mask_definitions = mask_definitions_from_json(&adjustments_clone);

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
    let mask_geometry = source_size(
        PhotoGeometry::from_adjustments(&adjustments_clone),
        &loaded_image,
        full_dims,
    );
    // The native display draws the edited layer's mask over this frame. Like
    // Capture One, it shows the mask itself: layer opacity scales the
    // adjustment, not the overlay. Taken first: below 100% the layer's own
    // bitmap is then derived from it instead of rasterized a second time.
    let display_overlay = match mask_overlay.filter(|_| use_wgpu_renderer) {
        Some(request) => match mask_definitions
            .iter_mut()
            .find(|def| def.id == request.mask_id && def.produces_bitmap())
        {
            // The layer at full opacity, set in place: a copy would copy
            // every stroke of a painted layer for each frame.
            Some(def) => {
                let opacity = std::mem::replace(&mut def.opacity, 100.0);
                let overlay = get_cached_or_generate_mask(
                    &state,
                    def,
                    preview_width,
                    preview_height,
                    effective_scale,
                    scaled_crop_offset,
                    &mask_geometry,
                    warped_image.as_ref(),
                );
                def.opacity = opacity;
                overlay?.map(|mask| gpu_processing::DisplayMaskOverlay {
                    mask,
                    style: if request.grayscale {
                        crate::mask_generation::OverlayStyle::Grayscale
                    } else {
                        crate::mask_generation::OverlayStyle::Colored {
                            rgb: request.color,
                            opacity: request.opacity.min(100),
                        }
                    },
                })
            }
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
    let mask_bitmaps: Vec<Arc<MaskBitmap>> = mask_definitions
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
        get_all_adjustments_with_masks(&adjustments_clone, is_raw, tm_override, &mask_definitions);
    crate::baseline_exposure::apply_base_look(
        &mut final_adjustments.global,
        &loaded_image.path,
        loaded_image.measures.as_ref(),
    );
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
    state.rendered_source.mark(&loaded_image.image);
    if !is_interactive {
        rewarm_mask_source(app_handle, &state, &loaded_image, adjustments_clone);
    }
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
    crate::preview_jpeg::encode_rgbx_444(image, quality)
}

pub(crate) fn start_analytics_worker(app_handle: tauri::AppHandle) {
    let state = app_handle.state::<AppState>();
    let worker_app = app_handle.clone();
    let sender = gpu_processing::start_latest_job_worker(move |job: AnalyticsJob| {
        #[cfg(feature = "performance-profiling")]
        let _stage = crate::performance::PerformanceStage::new("analytics_compute");
        let histogram_data = scopes::calculate_histogram_from_image(
            &job.image,
            scopes::ScopeSpace::of(job.space),
            job.clipped,
        )
        .ok();
        let waveform_data = if job.compute_waveform {
            scopes::calculate_waveform_from_image(
                &job.image,
                job.space,
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
    // Named for thread lists and profiles.
    let spawned = std::thread::Builder::new().name("preview-worker".into()).spawn(move || {
        let jobs = &app_handle.state::<AppState>().inner().preview_jobs;
        let mut newest_sequence = None;
        let mut waiting: Option<PreviewJob> = None;
        let mut live = crate::preview_slot::LiveCells::default();
        let live_step = || crate::multi_view::live_step(&app_handle);
        loop {
            // A request arriving meanwhile has already replaced (and
            // rejected) the one waiting: the job taken is the newest received.
            let job = match waiting.take() {
                Some(job) => job,
                None => live.next_job(jobs, crate::multi_view::live_pending, &live_step),
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
            let warm = draft_warmup_of(&job);
            let interactive = job.is_interactive;
            let responder = job.responder;
            let frame = crate::multi_view_prefetch::PreviewFrame::start(job.is_interactive);
            crate::proxy_worker::note_preview(job.is_interactive);
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
            let rendered = result.is_ok();
            let _ = responder.send(result);
            drop(frame);
            if let Some(warm) = warm.filter(|_| rendered && !crate::multi_view::live_pending()) {
                // A request within the pause goes first; the warm-up waits
                // for the next final frame then.
                match jobs.take_timeout(DRAFT_WARMUP_IDLE) {
                    Some(next) => waiting = Some(next),
                    None => warm_draft_source(&app_handle, warm),
                }
                continue;
            }
            waiting = live.after_frame(jobs, interactive, crate::multi_view::live_pending, &live_step);
        }
    });
    if let Err(e) = spawned {
        log::error!("Failed to start the preview worker: {}", e);
    }
}

/// A fit view's final frame, to render once more as a drag draft.
struct DraftWarmup {
    adjustments: serde_json::Value,
    target_resolution: Option<u32>,
    generation: usize,
}

/// Quiet time after a final frame before the drag-draft source is prepared.
const DRAFT_WARMUP_IDLE: std::time::Duration = std::time::Duration::from_millis(300);

/// Linux (browser drafts): a drag's first draft built its smaller source —
/// downscale, upload and chroma pyramid, 64–90 ms on the Linux PC — so the
/// first move after opening a photograph lagged. Once a fit view's final
/// frame is done and the worker stays idle, a discarded draft render builds
/// that source ahead (the final one stays as the GPU cache's spare). Only
/// while none is prepared, so drags and their release frames do not repeat
/// it (notes: parity-gaps/G55-performance.md).
fn draft_warmup_of(job: &PreviewJob) -> Option<DraftWarmup> {
    (cfg!(target_os = "linux")
        && !job.is_interactive
        && job.roi.is_none()
        && job.mask_overlay.is_none()
        && !job.crop_edit)
        .then(|| DraftWarmup {
            adjustments: job.adjustments.clone(),
            target_resolution: job.target_resolution,
            generation: job.generation,
        })
}

fn warm_draft_source(app_handle: &tauri::AppHandle, warm: DraftWarmup) {
    let state = app_handle.state::<AppState>();
    let prepared = state
        .cached_preview
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .is_none_or(|cached| cached.small_image.is_some());
    if prepared {
        return;
    }
    let started = std::time::Instant::now();
    let result = process_preview_job(
        app_handle,
        state,
        warm.adjustments,
        true,
        warm.target_resolution,
        None,
        false,
        false,
        None,
        None,
        false,
        warm.generation,
    );
    match result {
        Ok(_) => log::info!("Drag draft source prepared in {:.1?}", started.elapsed()),
        Err(e) => log::warn!("Preparing the drag draft source failed: {}", e),
    }
}

/// What `activate_photo` put on screen.
#[derive(serde::Serialize, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) enum Activation {
    /// Nothing (not a RAW, the viewer not at fit, no proxy or thumbnail).
    None,
    /// A frame developed from the photograph's preview proxy is on its way.
    Developed,
    /// Its cached thumbnail, until its first developed frame (a preview).
    Still,
}

/// Shows the photograph at `path` at once, natively (Capture One swaps its
/// preview files the same way): its preview proxy (in memory or on disk) is
/// developed with its recipe (`adjustments` from the editor's cache, else
/// the stored one, else the defaults) and presented in the fit placement,
/// before the webview has updated its state. The editor then loads the
/// photograph as usual and renders the same frame again. Without a proxy
/// (never opened, previews not built) its cached thumbnail is shown instead
/// (`show_still`), so a step never keeps the previous photograph on screen
/// while the RAW decodes. Returns what was put on screen.
#[tauri::command]
pub(crate) async fn activate_photo(
    path: String,
    adjustments: Option<serde_json::Value>,
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<Activation, String> {
    #[cfg(feature = "performance-profiling")]
    let received = crate::performance::epoch_ms();
    let (source_path, sidecar_path) = parse_virtual_path(&path);
    if !formats::is_raw_file(&source_path) {
        return Ok(Activation::None);
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
        return Ok(Activation::None);
    };
    // Only previews made for the recipe's decode variant stand in (a virtual
    // copy's may differ from its master's, `preview_proxy::stands_for`).
    let wanted = crate::image_loader::decode_variant(
        &adjustments
            .clone()
            .filter(|a| a.is_object())
            .unwrap_or_else(|| crate::exif_processing::load_sidecar(&sidecar_path).adjustments),
        &source_path.to_string_lossy(),
    );
    if wanted != crate::image_loader::stored_decode_variant(&source_path.to_string_lossy()) {
        #[cfg(feature = "performance-profiling")]
        log::info!(
            "[PERF] {}",
            serde_json::json!({ "stage": "activate_photo_skipped", "reason": "other decode variant" })
        );
        return Ok(Activation::None);
    }
    crate::image_loader::open_source(&state, &path);
    release_still_for(&state, &path);
    let remembered = state
        .proxy_memory
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&path, wanted);
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
                let never_edited = crate::embedded_still::never_edited(
                    &adjustments
                        .clone()
                        .filter(|a| a.is_object())
                        .unwrap_or_else(|| {
                            crate::exif_processing::load_sidecar(&sidecar_path).adjustments
                        }),
                );
                return Ok(show_still(&app_handle, &path, viewport, never_edited).await);
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
        return Ok(Activation::None);
    }
    let mut recipe = adjustments
        .filter(|a| a.is_object())
        .unwrap_or_else(|| crate::exif_processing::load_sidecar(&sidecar_path).adjustments);
    if recipe.is_null() {
        recipe = serde_json::json!({});
    }
    let _ = rawtonic_export::exif_processing::prepare_recipe_lens(&mut recipe, &source_path);
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
    Ok(Activation::Developed)
}

/// A real image of the photograph presented natively in the fit placement
/// of `viewport` until its first developed frame: instead of the previous
/// photograph at once, then refined in the same place. A never-edited RAW
/// (`never_edited`) shows its camera's embedded JPEG at the size shown
/// (`embedded_still`): at once when it was prepared ahead, else after its
/// cached medium thumbnail (the same camera JPEG, smaller), which it then
/// replaces in the same placement; an edited one, and a RAW without an
/// embedded JPEG, its cached thumbnail (developed with its stored recipe,
/// already turned and cropped as it is shown). Nothing when there is
/// neither, another photograph was asked for meanwhile, or a developed
/// frame of it is already shown.
async fn show_still(
    app_handle: &tauri::AppHandle,
    path: &str,
    viewport: [f32; 4],
    never_edited: bool,
) -> Activation {
    let state = app_handle.state::<AppState>();
    let Some(context) = state
        .gpu_context
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
    else {
        return Activation::None;
    };
    let (source, _) = parse_virtual_path(path);
    let view = (viewport[2].max(1.0) as u32, viewport[3].max(1.0) as u32);
    let space = context.display_space;
    let embedded = never_edited
        .then(|| crate::embedded_still::kept(&source, view, space))
        .flatten();
    if let Some(still) = embedded {
        return match present_still_of(
            app_handle, &context, path, viewport, &still, "embedded", false,
        ) {
            true => Activation::Still,
            false => Activation::None,
        };
    }
    let (app, wanted, raw) = (app_handle.clone(), path.to_string(), source.clone());
    let thumbnail = tokio::task::spawn_blocking(move || {
        let file = crate::file_management::cached_medium_thumbnail(&app, &wanted)?;
        let bytes = std::fs::read(file).ok()?;
        let mut image = image::load_from_memory_with_format(&bytes, image::ImageFormat::Jpeg)
            .ok()?
            .to_rgba8();
        gpu_processing::srgb_to_display(space, &mut image);
        // A never-edited RAW's thumbnail is its camera JPEG: framed like the
        // development, as the embedded JPEG replacing it will be (header
        // tags of the memory-mapped file only; never a cloud file not
        // downloaded yet).
        let developed = (never_edited && crate::preview_proxy::source_available(&raw))
            .then(|| crate::embedded_still::frame_of(&raw))
            .flatten()
            .and_then(|frame| frame.size);
        Some(crate::embedded_still::framed(image, developed))
    })
    .await
    .ok()
    .flatten();
    let shown = thumbnail.is_some_and(|still| {
        present_still_of(
            app_handle,
            &context,
            path,
            viewport,
            &still,
            "thumbnail",
            false,
        )
    });
    if never_edited {
        // The camera's JPEG at the size shown replaces the thumbnail (or
        // stands in for a missing one) while no developed frame is in.
        let (app, wanted) = (app_handle.clone(), path.to_string());
        let upgrade = async move {
            let decoded = tokio::task::spawn_blocking(move || {
                crate::embedded_still::still_for(&source, view, space)
            })
            .await
            .ok()
            .flatten()?;
            Some(present_still_of(
                &app, &context, &wanted, viewport, &decoded, "embedded", true,
            ))
        };
        if !shown {
            return match upgrade.await {
                Some(true) => Activation::Still,
                _ => Activation::None,
            };
        }
        tokio::spawn(upgrade);
    }
    if shown {
        Activation::Still
    } else {
        Activation::None
    }
}

/// Presents `still` as the frame of `path` (`present_if_current`; with
/// `replace_still` also over a still of it already shown). Whether it was.
fn present_still_of(
    app_handle: &tauri::AppHandle,
    context: &image_processing::GpuContext,
    path: &str,
    viewport: [f32; 4],
    still: &crate::embedded_still::Still,
    _source: &str,
    replace_still: bool,
) -> bool {
    let state = app_handle.state::<AppState>();
    let rect = gpu_processing::fit_placement(viewport, still.shown.0 as f32, still.shown.1 as f32);
    let shown = present_if_current(
        &context.display,
        &state.current_source,
        path,
        viewport,
        replace_still,
        |display| display.present_still(&context.device, &context.queue, &still.image, path, rect),
    );
    #[cfg(feature = "performance-profiling")]
    if shown {
        let (width, height) = still.image.dimensions();
        log::info!(
            "[PERF] {}",
            serde_json::json!({ "stage": "still_preview_presented", "source": _source,
                "width": width, "height": height, "rect": rect,
                "epoch_ms": crate::performance::epoch_ms() })
        );
    }
    shown
}

/// A photograph is asked for (`current_source` set to `path`): a still of
/// another one no longer holds back developed frames
/// (`WgpuDisplay::release_still_for`).
pub(crate) fn release_still_for(state: &AppState, path: &str) {
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
        display.release_still_for(path);
    }
}

/// What a still is presented on: the display, or a test's.
trait StillDisplay {
    fn frame_path(&self) -> Option<&str>;
    fn shows_still_of(&self, path: &str) -> bool;
    fn viewport(&self) -> Option<[f32; 4]>;
}

impl StillDisplay for gpu_processing::WgpuDisplay {
    fn frame_path(&self) -> Option<&str> {
        gpu_processing::WgpuDisplay::frame_path(self)
    }
    fn shows_still_of(&self, path: &str) -> bool {
        gpu_processing::WgpuDisplay::shows_still_of(self, path)
    }
    fn viewport(&self) -> Option<[f32; 4]> {
        self.viewport
    }
}

/// Presents a still of `path` (`present`) unless another photograph was
/// asked for meanwhile (`current`), a developed frame of it is shown
/// already, or the viewer left the fit `viewport`. Checked while the
/// display is locked, so no developed frame of a photograph asked for later
/// can be presented between the check and the still (it follows the still
/// and replaces it). Whether it was presented.
fn present_if_current<D: StillDisplay>(
    display: &std::sync::Mutex<Option<D>>,
    current: &std::sync::Mutex<String>,
    path: &str,
    viewport: [f32; 4],
    replace_still: bool,
    present: impl FnOnce(&mut D) -> bool,
) -> bool {
    let mut display = display.lock().unwrap_or_else(|e| e.into_inner());
    let Some(display) = display.as_mut() else {
        return false;
    };
    let asked_for = *current.lock().unwrap_or_else(|e| e.into_inner()) == path;
    asked_for
        && (display.frame_path() != Some(path) || (replace_still && display.shows_still_of(path)))
        && display.viewport() == Some(viewport)
        && present(display)
}

/// Renders the split view's before side (`js_adjustments`: the unedited
/// recipe with the edit's framing, or a snapshot compared with, framed and
/// developed as the edit; view states included) for the photograph at `path`
/// and keeps it on the GPU beside the edited frames, as Capture One's
/// before/after split shows both renders. Pixels stay native: the
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
    #[cfg(feature = "performance-profiling")]
    let _stage = crate::performance::PerformanceStage::new("split_before");
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
    let interim = interim_source(&state, &loaded_image, source_scale);
    let render_key = split_render_key(
        frame_key,
        &loaded_image.image,
        load_generation,
        preview_dim,
        &adjustments,
        interim,
    );
    if current_key == Some(render_key) {
        return Ok(true);
    }

    let tm_override = resolve_tonemapper_override(&settings, loaded_image.is_raw);
    let rendered = develop_split_before(
        &state,
        &context,
        (&loaded_image, source_scale, full_dims),
        &adjustments,
        preview_dim,
        interim,
        tm_override,
    )?;

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

/// The split's before of `adjustments` (hydrated, with the edit's framing)
/// from `source` (image, proxy pixels per full-resolution pixel, full size;
/// `interim`: shown only until the decode is in) at `preview_dim`: the
/// display shader's readback, encoded for the window layer as display frames
/// are, local adjustments included.
#[allow(clippy::too_many_arguments)]
fn develop_split_before(
    state: &AppState,
    context: &gpu_processing::GpuContext,
    (loaded_image, source_scale, full_dims): (&LoadedImage, f32, (u32, u32)),
    adjustments: &serde_json::Value,
    preview_dim: u32,
    interim: bool,
    tm_override: Option<u32>,
) -> Result<image::RgbaImage, String> {
    // The edit's preview base when the before shares it (same framing and
    // retouch); else its own, without touching the edit's caches.
    let transform_hash = calculate_transform_hash(adjustments);
    let preparation_key = preview_cache::preparation_key(adjustments);
    let cached = state
        .cached_preview
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .filter(|c| {
            c.serves_recipe(
                &loaded_image.image,
                (transform_hash, preparation_key),
                interim,
            ) && c.preview_dim == preview_dim
        })
        .map(|c| (Arc::clone(&c.image), c.scale, c.unscaled_crop_offset));
    let (base, scale, unscaled_crop_offset) = match cached {
        Some(cached) => cached,
        None => {
            let preview = preview_base(
                state,
                loaded_image,
                source_scale,
                full_dims,
                adjustments,
                adjustments,
                preview_dim,
                preparation_key,
            )?;
            (preview.image, preview.scale, preview.unscaled_crop_offset)
        }
    };
    // A snapshot's local adjustments, as the preview frame draws them.
    let mask_definitions = mask_definitions_from_json(adjustments);
    let mask_bitmaps = preview_mask_bitmaps(
        state,
        adjustments,
        &mask_definitions,
        (loaded_image, source_scale, full_dims),
        base.dimensions(),
        scale,
        unscaled_crop_offset,
    )?;

    let is_raw = loaded_image.is_raw;
    let mut all_adjustments =
        get_all_adjustments_with_masks(adjustments, is_raw, tm_override, &mask_definitions);
    crate::baseline_exposure::apply_base_look(
        &mut all_adjustments.global,
        &loaded_image.path,
        loaded_image.measures.as_ref(),
    );
    all_adjustments.global.detail_scale = scale;
    // Encoded for the window layer, as display frames are.
    let space = context.display_space;
    all_adjustments.global.output_space[0] = space.shader_code();
    all_adjustments.global.output_matrix =
        image_processing::gpu_mat3_from_rows(space.from_linear_srgb_f32());
    let lut = adjustments["lutPath"]
        .as_str()
        .and_then(|p| lut_processing::get_or_load_lut(state, p).ok());
    let rendered = gpu_processing::process_and_get_dynamic_image_with_display_shader(
        context,
        state,
        &base,
        transform_hash,
        RenderRequest {
            adjustments: all_adjustments,
            mask_bitmaps: &mask_bitmaps,
            lut,
            roi: None,
        },
        "split_before",
    )
    .map_err(|error| format!("{PREVIEW_RENDER_FAILED}: {error}"))?;
    Ok(rendered.into_rgba8())
}

#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub(crate) async fn apply_adjustments(
    preview_channel: Option<tauri::ipc::JavaScriptChannelId>,
    webview: tauri::Webview,
    js_adjustments: Option<serde_json::Value>,
    live_stroke: Option<crate::live_stroke::LiveStroke>,
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
    // A live brush frame may carry only the points its stroke gained.
    let mut js_adjustments = state
        .live_stroke
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .resolve(js_adjustments, live_stroke, sequence)?;
    // On receipt: a later request may refer to these strokes even if this
    // job is superseded before it runs.
    rawtonic_engine::mask_parameters::hydrate(&mut js_adjustments)?;
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

    let loaded_image = crate::image_loader::original_image(&state).await?;

    tokio::task::spawn_blocking(move || {
        let state = app_handle.state::<AppState>();
        let path = loaded_image.path.clone();
        let is_raw = loaded_image.is_raw;
        let visual_hash = calculate_visual_hash(&path, &adjustments_clone);

        let pre_geometry_base = {
            let mut cache = state.geometry_cache.lock().unwrap();
            // The visual hash leaves the geometry out, so a hit may lack the
            // full-resolution lateral CA estimate the export uses.
            let full_dims = loaded_image.image.dimensions();
            if let Some(cached) = cache
                .get(&visual_hash)
                .filter(|cached| lateral_ca_is_shared(cached, full_dims, &adjustments_clone))
                .cloned()
            {
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

        let params = crate::image_processing::geometry_params_for(&adjustments_clone, is_raw);
        let mut adjusted_params = params;
        adjusted_params.lens_vignette_amount *= if is_raw { 0.4 } else { 0.8 };

        let warped_image = rawtonic_engine::edge_fringe::apply_for_recipe(
            Cow::Owned(warp_image_geometry(&pre_geometry_base, adjusted_params)?),
            &adjustments_clone,
            scale_for_gpu,
        )?
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
            .with_decoded_size(loaded_image.image.width(), loaded_image.image.height());
        let mask_bitmaps: Vec<Arc<MaskBitmap>> = mask_definitions
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
        crate::baseline_exposure::apply_base_look(
            &mut uncropped_adjustments.global,
            &path,
            loaded_image.measures.as_ref(),
        );
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

/// `get_original_image` for async commands (`image_loader::original_image`).
pub async fn original_source(
    state: &AppState,
) -> Result<(std::sync::Arc<image::DynamicImage>, bool), String> {
    let loaded_image = crate::image_loader::original_image(state).await?;
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

        let (pristine_source, measures) = match read_file_mapped(&source_path) {
            Ok(mmap) => crate::image_loader::load_base_image_for_recipe_measured(
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
                crate::image_loader::load_base_image_for_recipe_measured(
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
            apply_all_transformations(base_image, &js_adjustments, is_raw)?;
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
        let mask_geometry = PhotoGeometry::from_adjustments(&js_adjustments)
            .with_decoded_size(pristine_source.width(), pristine_source.height());
        let mask_bitmaps: Vec<Arc<MaskBitmap>> = mask_definitions
            .iter()
            .filter_map(|def| {
                generate_layer_mask(
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
        crate::baseline_exposure::apply_base_look(
            &mut all_adjustments.global,
            &source_path_str,
            measures.as_ref(),
        );
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
    use super::{split_frame_key, split_render_key};
    use serde_json::json;

    #[test]
    fn a_split_before_of_an_interim_source_is_rendered_again_once_it_lasts() {
        // The same proxy, load and recipe: only the photograph turned out
        // offline (its before then needs the measured lateral CA).
        let proxy = std::sync::Arc::new(image::DynamicImage::new_rgb32f(4, 4));
        let recipe = json!({"autoCaVersion": 3});
        let key = |interim| split_render_key(1, &proxy, 2, 3, &recipe, interim);
        assert_ne!(key(true), key(false));
        assert_eq!(key(false), key(false));
    }

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

    /// A snapshot compared with shows its local adjustments in the split's
    /// before: a radial layer brightens its part and leaves the rest (the
    /// before once rendered without mask bitmaps, as for the unedited one).
    #[test]
    #[ignore = "needs a GPU"]
    fn the_split_before_shows_a_snapshots_local_adjustments() {
        use crate::app_state::{AppState, LoadedImage};
        use crate::gpu_processing::GpuContext;
        use image::{DynamicImage, Rgb32FImage};
        use std::sync::Arc;

        let context = GpuContext::headless().expect("a GPU");
        let state = AppState::default();
        let (width, height) = (384, 256);
        let loaded = LoadedImage {
            path: "/split-before.tif".into(),
            image: Arc::new(DynamicImage::ImageRgb32F(Rgb32FImage::from_pixel(
                width,
                height,
                image::Rgb([0.18, 0.18, 0.18]),
            ))),
            is_raw: false,
            decode: None,
            measures: None,
        };
        let layer = json!({"id": "layer", "name": "Layer", "visible": true, "invert": false,
            "opacity": 100, "adjustments": {"exposure": 2.0},
            "subMasks": [{"id": "radial", "type": "radial", "visible": true, "mode": "additive",
                "parameters": {"centerX": 96.0, "centerY": 128.0, "radiusX": 60.0,
                    "radiusY": 60.0, "rotation": 0.0, "feather": 0.0}}]});
        let render = |recipe: &serde_json::Value| {
            super::develop_split_before(
                &state,
                &context,
                (&loaded, 1.0, (width, height)),
                recipe,
                width,
                false,
                None,
            )
            .expect("a before")
        };
        let plain = render(&json!({}));
        let snapshot = render(&json!({"masks": [layer]}));
        assert_eq!(snapshot.dimensions(), plain.dimensions());
        let (inside, outside) = ((96, 128), (300, 128));
        let luma = |image: &image::RgbaImage, (x, y): (u32, u32)| image.get_pixel(x, y)[1];
        assert!(
            luma(&snapshot, inside) > luma(&plain, inside) + 40,
            "the layer brightens its part: {} vs {}",
            luma(&snapshot, inside),
            luma(&plain, inside)
        );
        assert_eq!(
            snapshot.get_pixel(outside.0, outside.1),
            plain.get_pixel(outside.0, outside.1)
        );
    }
}

#[cfg(test)]
mod fringe_draft_tests {
    use super::patched_warped_key;
    use serde_json::json;

    #[test]
    fn a_fringe_draft_never_shares_the_finished_full_resolution_image() {
        // A geometry drag at full resolution builds the warped image without
        // the fringe pass's chromatic-aberration stage; the frame after the
        // drag must not take it from the cache.
        let settled = json!({"fringeVersion": 4, "rotation": 1.5});
        let mut draft = settled.clone();
        draft[rawtonic_engine::edge_fringe::DRAFT_KEY] = json!(true);
        assert_ne!(patched_warped_key(&draft), patched_warped_key(&settled));
        // Finished images keep the key they had.
        assert_eq!(
            patched_warped_key(&settled),
            crate::cache_utils::calculate_patched_warped_hash(&settled)
        );
    }
}

#[cfg(test)]
mod preview_memory_tests {
    use super::*;

    #[test]
    fn only_final_fit_frames_warm_the_drag_draft_source() {
        let job =
            |is_interactive: bool, roi: Option<(f32, f32, f32, f32)>, crop_edit: bool| PreviewJob {
                generation: 3,
                sequence: None,
                adjustments: serde_json::json!({"exposure": 1.0}),
                is_interactive,
                target_resolution: Some(1920),
                roi,
                request_analytics: true,
                compute_waveform: false,
                active_waveform_channel: None,
                mask_overlay: None,
                crop_edit,
                responder: tokio::sync::oneshot::channel().0,
            };
        let warm = draft_warmup_of(&job(false, None, false));
        assert_eq!(warm.is_some(), cfg!(target_os = "linux"));
        if let Some(warm) = warm {
            assert_eq!(warm.adjustments, serde_json::json!({"exposure": 1.0}));
            assert_eq!((warm.target_resolution, warm.generation), (Some(1920), 3));
        }
        assert!(draft_warmup_of(&job(true, None, false)).is_none());
        assert!(draft_warmup_of(&job(false, Some((0.0, 0.0, 0.5, 0.5)), false)).is_none());
        assert!(draft_warmup_of(&job(false, None, true)).is_none());
    }

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
        let warped = apply_geometry_warp(source, adjustments, true).unwrap();
        let warped =
            rawtonic_engine::edge_fringe::apply_for_recipe(warped, adjustments, 1.0).unwrap();
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
            let base = reduced_preview_base(&proxy, 0.2, 3000, 2000, &adjustments, true, 640, 0.2)
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
                reduced_preview_base(&source, 1.0, 3000, 2000, &adjustments, true, 640, scale)
                    .unwrap();
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
            let _ =
                reduced_preview_base(&source, 1.0, w, h, &adjustments, true, dim, scale).unwrap();
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
            decode: None,
            measures: None,
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
            true,
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

#[cfg(test)]
mod lateral_ca_sharing_tests {
    use super::*;
    use rawtonic_engine::lateral_ca;
    use serde_json::json;

    /// Rings and spokes with red rendered 0.8 ‰ larger than green.
    fn photo(w: u32, h: u32) -> image::Rgb32FImage {
        let (cx, cy) = ((w as f32 - 1.0) / 2.0, (h as f32 - 1.0) / 2.0);
        let value = |x: f32, y: f32| {
            let (dx, dy) = (x - cx, y - cy);
            let rings = (((dx * dx + dy * dy).sqrt() / 23.0).sin() > 0.0) as u8 as f32;
            let spokes = ((dy.atan2(dx) * 40.0).sin() > 0.0) as u8 as f32;
            0.05 + 0.3 * rings + 0.15 * spokes
        };
        image::Rgb32FImage::from_fn(w, h, |x, y| {
            let (x, y) = (x as f32, y as f32);
            let red = value(cx + (x - cx) / 1.0008, cy + (y - cy) / 1.0008);
            image::Rgb([red, value(x, y), value(x, y) * 0.8])
        })
    }

    #[test]
    fn a_cached_preview_base_warps_with_the_exports_estimate() {
        let full = DynamicImage::ImageRgb32F(photo(3000, 2000));
        let reduced = downscale_f32_image(&full, 1500, 1500);
        let full_dims = full.dimensions();
        let off = json!({"autoCaVersion": 2, "autoChromaticAberration": false});
        let on = json!({"autoCaVersion": 2, "autoChromaticAberration": true});
        // The geometry cache's hash is the same for both recipes.
        assert_eq!(
            calculate_visual_hash("/a.RW2", &off),
            calculate_visual_hash("/a.RW2", &on)
        );
        // Built with Remove CA off, the base has no shared estimate: a hit
        // with it on must rebuild rather than measure the reduced copy.
        share_lateral_ca(&full, &reduced, &off);
        assert!(lateral_ca_is_shared(&reduced, full_dims, &off));
        assert!(!lateral_ca_is_shared(&reduced, full_dims, &on));
        share_lateral_ca(&full, &reduced, &on);
        assert!(lateral_ca_is_shared(&reduced, full_dims, &on));
        // The preview's warp then uses the export's full-resolution estimate.
        let (full_rgb, reduced_rgb) = (full.as_rgb32f().unwrap(), reduced.as_rgb32f().unwrap());
        assert!(lateral_ca::estimate_for(full_rgb, 2, None).is_some());
        assert_eq!(
            lateral_ca::estimate_for(reduced_rgb, 2, None),
            lateral_ca::estimate_for(full_rgb, 2, None)
        );
        // From version 3 a change of the lens data's own CA is another estimate.
        let lens = |vr: f64| json!({"autoCaVersion": 3, "lensDistortionParams": {"tca_vr": vr, "tca_vb": 1.0}});
        share_lateral_ca(&full, &reduced, &lens(1.0003));
        assert!(lateral_ca_is_shared(&reduced, full_dims, &lens(1.0003)));
        assert!(!lateral_ca_is_shared(&reduced, full_dims, &lens(1.0005)));
        // Not reduced: the warp measures the image the export measures.
        assert!(lateral_ca_is_shared(&reduced, reduced.dimensions(), &on));
    }

    #[test]
    fn an_interim_preview_base_does_not_measure_its_reduced_copy() {
        // A decode's draft, shown until the decode is in.
        let full = DynamicImage::ImageRgb32F(photo(3000, 2000));
        // (A size no other test's reduced copy has: the estimates are cached
        // per pixels for the whole process.)
        let draft = downscale_f32_image(&full, 1400, 1400);
        let scale = draft.width() as f32 / 3000.0;
        let on = json!({"autoCaVersion": 2, "autoChromaticAberration": true});
        let off = json!({"autoCaVersion": 2, "autoChromaticAberration": false});
        let base = |recipe: &serde_json::Value, interim: bool| {
            build_reduced_preview_base(
                &draft,
                scale,
                (3000, 2000),
                recipe,
                true,
                1400,
                scale,
                interim,
            )
            .unwrap()
            .image
            .to_rgb32f()
        };
        // Without the whole decode's estimate the draft warps as with an
        // estimate that found nothing (here: no correction) ...
        let interim = base(&on, true);
        assert_eq!(interim, base(&off, true));
        assert!(!lateral_ca_is_shared(&draft, (3000, 2000), &on));
        // ... where measuring the draft itself would correct it.
        assert_ne!(interim, base(&on, false));
        // Once the decode shared its estimate, the draft warps with that.
        share_lateral_ca(&full, &draft, &on);
        assert_eq!(base(&on, true), base(&on, false));
    }
}

#[cfg(test)]
mod mask_source_build_tests {
    use super::{BuildSlot, cached_or_built};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex, mpsc};
    use std::time::Duration;

    /// The editor photo, its geometry and the cached build (photo, geometry),
    /// as `full_warped_image` sees them; a build key is (photo, geometry).
    #[derive(Default)]
    struct Editor {
        view: Mutex<(u32, u32)>,
        cached: Mutex<Option<(u32, u32)>>,
        slot: BuildSlot<(u32, u32)>,
        builds: AtomicUsize,
    }

    type Gate = Option<mpsc::Receiver<()>>;

    impl Editor {
        fn new(photo: u32) -> Arc<Self> {
            let editor = Self::default();
            *editor.view.lock().unwrap() = (photo, 0);
            Arc::new(editor)
        }

        fn show(&self, photo: u32, geometry: u32) {
            *self.view.lock().unwrap() = (photo, geometry);
        }

        /// The source of `key`, built after `gate` releases it; publishes
        /// like `full_warped_image`.
        fn get(&self, key: (u32, u32), prewarm: bool, gate: Gate) -> Option<(u32, u32)> {
            let lookup = || {
                let current = self.view.lock().unwrap().0 == key.0;
                let cached = *self.cached.lock().unwrap();
                (current, cached.filter(|&c| current && c == key))
            };
            cached_or_built(&self.slot, key, lookup, prewarm, |ticket| {
                self.builds.fetch_add(1, Ordering::SeqCst);
                if let Some(gate) = gate {
                    gate.recv().unwrap();
                }
                let mut cached = self.cached.lock().unwrap();
                if self.view.lock().unwrap().0 == key.0 && self.slot.may_publish(ticket) {
                    *cached = Some(key);
                }
                key
            })
        }

        fn spawn(
            self: &Arc<Self>,
            key: (u32, u32),
            prewarm: bool,
            gate: Gate,
        ) -> std::thread::JoinHandle<Option<(u32, u32)>> {
            let editor = Arc::clone(self);
            std::thread::spawn(move || editor.get(key, prewarm, gate))
        }

        fn wait_for_builds(&self, n: usize) {
            for _ in 0..500 {
                if self.builds.load(Ordering::SeqCst) >= n {
                    return;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            panic!("build {n} did not start");
        }
    }

    #[test]
    fn a_stroke_during_a_prewarm_waits_for_it_and_reuses_it() {
        let editor = Editor::new(1);
        let (release, gate) = mpsc::channel();
        let prewarm = editor.spawn((1, 0), true, Some(gate));
        editor.wait_for_builds(1);
        let stroke = editor.spawn((1, 0), false, None);
        std::thread::sleep(Duration::from_millis(30));
        assert!(!stroke.is_finished());
        release.send(()).unwrap();
        assert_eq!(prewarm.join().unwrap(), Some((1, 0)));
        assert_eq!(stroke.join().unwrap(), Some((1, 0)));
        assert_eq!(editor.builds.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_new_photo_builds_at_once_while_an_old_build_runs() {
        let editor = Editor::new(1);
        let (release, gate) = mpsc::channel();
        let old = editor.spawn((1, 0), true, Some(gate));
        editor.wait_for_builds(1);
        editor.show(2, 0);
        assert_eq!(editor.get((2, 0), true, None), Some((2, 0)));
        assert_eq!(editor.get((2, 0), false, None), Some((2, 0)), "cached");
        release.send(()).unwrap();
        old.join().unwrap();
        assert_eq!(editor.builds.load(Ordering::SeqCst), 2);
        assert_eq!(*editor.cached.lock().unwrap(), Some((2, 0)));
    }

    #[test]
    fn an_older_geometry_ending_last_does_not_replace_the_newer_one() {
        let editor = Editor::new(1);
        let (release, gate) = mpsc::channel();
        let old = editor.spawn((1, 0), true, Some(gate));
        editor.wait_for_builds(1);
        // A lens edit on the same photo: a new key, built at once.
        editor.show(1, 1);
        assert_eq!(editor.get((1, 1), true, None), Some((1, 1)));
        release.send(()).unwrap();
        assert_eq!(old.join().unwrap(), Some((1, 0)));
        assert_eq!(*editor.cached.lock().unwrap(), Some((1, 1)));
        assert_eq!(editor.get((1, 1), false, None), Some((1, 1)));
        assert_eq!(editor.builds.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn a_prewarm_waiting_for_a_photo_no_longer_edited_builds_nothing() {
        let editor = Editor::new(1);
        let (release, gate) = mpsc::channel();
        let first = editor.spawn((1, 0), true, Some(gate));
        editor.wait_for_builds(1);
        let stale = editor.spawn((1, 0), true, None);
        std::thread::sleep(Duration::from_millis(30));
        editor.show(2, 0);
        release.send(()).unwrap();
        first.join().unwrap();
        assert_eq!(stale.join().unwrap(), None);
        assert_eq!(editor.builds.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn other_photos_build_without_caching_and_never_for_a_prewarm() {
        let editor = Editor::new(2);
        assert_eq!(editor.get((1, 0), false, None), Some((1, 0)));
        assert_eq!(editor.get((1, 0), true, None), None);
        assert_eq!(editor.builds.load(Ordering::SeqCst), 1);
        assert_eq!(*editor.cached.lock().unwrap(), None);
    }

    #[test]
    fn waiters_wake_when_a_build_panics() {
        let editor = Editor::new(1);
        let failed = {
            let editor = Arc::clone(&editor);
            std::thread::spawn(move || {
                cached_or_built(
                    &editor.slot,
                    (1, 0),
                    || (true, None),
                    false,
                    |_| -> (u32, u32) { panic!("build failed") },
                )
            })
        };
        assert!(failed.join().is_err());
        assert_eq!(editor.get((1, 0), false, None), Some((1, 0)));
    }
}

#[cfg(test)]
mod curve_input_tests {
    use super::{prophoto_pixels, prophoto_to_linear_srgb};
    use rawtonic_engine::output_color_space::OutputColorSpace;

    #[test]
    fn the_curve_input_keeps_colours_beyond_srgb() {
        let space = OutputColorSpace::ProPhoto;
        let encoded = |linear: [f64; 3]| linear.map(|v| space.transfer().from_linear(v) * 255.0);
        // A deep red beyond sRGB: its green is negative in sRGB, which an
        // sRGB sample would have clipped to zero.
        let srgb = prophoto_to_linear_srgb(encoded([0.4, 0.05, 0.02]));
        assert!(srgb[1] < -0.02, "{srgb:?}");
        let back = space.from_linear_srgb();
        for (r, row) in back.iter().enumerate() {
            let v: f64 = (0..3).map(|c| row[c] * srgb[c]).sum();
            assert!((v - [0.4, 0.05, 0.02][r]).abs() < 1e-9, "{r}: {v}");
        }
        // Grey stays grey.
        let grey = prophoto_to_linear_srgb([128.0; 3]);
        assert!((grey[0] - grey[1]).abs() < 1e-6 && (grey[1] - grey[2]).abs() < 1e-6);
        // Each pixel on its own (averaged by the frontend in the curve's
        // input space), black and white apart.
        let pixels = prophoto_pixels(&[255, 255, 255, 255, 0, 0, 0, 255]);
        assert_eq!(pixels.len(), 2);
        assert!(
            pixels[0].iter().all(|v| (v - 1.0).abs() < 1e-6),
            "{pixels:?}"
        );
        assert!(pixels[1].iter().all(|v| v.abs() < 1e-9), "{pixels:?}");
    }
}

#[cfg(test)]
mod still_tests {
    use super::{StillDisplay, present_if_current};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    const FIT: [f32; 4] = [0.0, 0.0, 100.0, 80.0];

    struct Shown {
        frame: Option<String>,
        /// The frame is a still (no developed frame of it yet).
        still: bool,
        viewport: Option<[f32; 4]>,
    }

    impl StillDisplay for Shown {
        fn frame_path(&self) -> Option<&str> {
            self.frame.as_deref()
        }
        fn shows_still_of(&self, path: &str) -> bool {
            self.still && self.frame.as_deref() == Some(path)
        }
        fn viewport(&self) -> Option<[f32; 4]> {
            self.viewport
        }
    }

    fn present(display: &mut Shown) -> bool {
        display.frame = Some("B".into());
        display.still = true;
        true
    }

    #[test]
    fn a_still_shows_only_while_its_photograph_is_wanted_and_undeveloped_at_fit() {
        let display = Mutex::new(Some(Shown {
            frame: Some("A".into()),
            still: false,
            viewport: Some(FIT),
        }));
        let current = Mutex::new("C".to_string());
        assert!(!present_if_current(
            &display, &current, "B", FIT, false, present
        ));
        *current.lock().unwrap() = "B".into();
        assert!(!present_if_current(
            &display, &current, "B", [0.0; 4], false, present
        ));
        display.lock().unwrap().as_mut().unwrap().frame = Some("B".into());
        assert!(!present_if_current(
            &display, &current, "B", FIT, false, present
        ));
        display.lock().unwrap().as_mut().unwrap().frame = Some("A".into());
        assert!(present_if_current(
            &display, &current, "B", FIT, false, present
        ));
        let shown = display.lock().unwrap();
        assert_eq!(shown.as_ref().unwrap().frame.as_deref(), Some("B"));
        assert!(shown.as_ref().unwrap().still);
    }

    /// The camera JPEG at the size shown replaces the photograph's
    /// thumbnail still, never its developed frame nor another's still.
    #[test]
    fn a_better_still_replaces_only_a_still_of_its_photograph() {
        let display = Mutex::new(Some(Shown {
            frame: Some("B".into()),
            still: true,
            viewport: Some(FIT),
        }));
        let current = Mutex::new("B".to_string());
        assert!(!present_if_current(
            &display, &current, "B", FIT, false, present
        ));
        assert!(present_if_current(
            &display, &current, "B", FIT, true, present
        ));
        // B's developed frame is in.
        display.lock().unwrap().as_mut().unwrap().still = false;
        assert!(!present_if_current(
            &display, &current, "B", FIT, true, present
        ));
        // A still of A is shown while B is asked for: B's still replaces it
        // as any frame of another photograph.
        {
            let mut shown = display.lock().unwrap();
            let shown = shown.as_mut().unwrap();
            shown.frame = Some("A".into());
            shown.still = true;
        }
        *current.lock().unwrap() = "A".into();
        assert!(!present_if_current(
            &display, &current, "B", FIT, true, present
        ));
    }

    #[test]
    fn a_still_never_covers_a_frame_of_a_photograph_asked_for_after_it() {
        let display = Arc::new(Mutex::new(Some(Shown {
            frame: Some("A".into()),
            still: false,
            viewport: Some(FIT),
        })));
        let current = Arc::new(Mutex::new("B".to_string()));
        // The developed frame of C is being presented while B's still waits
        // for the display.
        let presenting = display.lock().unwrap();
        let still = {
            let (display, current) = (Arc::clone(&display), Arc::clone(&current));
            std::thread::spawn(move || {
                present_if_current(&display, &current, "B", FIT, false, present)
            })
        };
        std::thread::sleep(Duration::from_millis(50));
        *current.lock().unwrap() = "C".into();
        let mut presenting = presenting;
        presenting.as_mut().unwrap().frame = Some("C".into());
        drop(presenting);
        assert!(!still.join().unwrap());
        assert_eq!(
            display.lock().unwrap().as_ref().unwrap().frame.as_deref(),
            Some("C")
        );
    }
}

#[cfg(test)]
mod levels_input_tests {
    use super::{level_counts, levels_input_probe};
    use serde_json::json;

    #[test]
    fn the_probe_leaves_out_levels_and_what_follows_them() {
        let recipe = json!({
            "exposure": 0.5,
            "levels": {"rgb": {"inBlack": 40}},
            "curves": {"luma": [{"x": 0, "y": 20}, {"x": 255, "y": 255}]},
            "regionCurve": {"highlights": 30},
            "grainAmount": 20,
            "lutPath": "/look.cube",
        });
        let probe = levels_input_probe(&recipe);
        assert_eq!(probe, json!({"exposure": 0.5}));
    }

    #[test]
    fn counts_every_pixel_per_channel() {
        let mut image = image::RgbaImage::new(4, 1);
        for (x, value) in [10u8, 20, 30, 40].into_iter().enumerate() {
            image.put_pixel(x as u32, 0, image::Rgba([value, 255 - value, 7, 255]));
        }
        let counts = level_counts(&image::DynamicImage::ImageRgba8(image));
        assert_eq!(counts[0][10], 1);
        assert_eq!(counts[0][30], 1);
        assert_eq!(counts[0][20], 1);
        assert_eq!(counts[1][245], 1);
        assert_eq!(counts[2][7], 4);
        assert_eq!(
            counts
                .iter()
                .map(|c| c.iter().sum::<u32>())
                .collect::<Vec<_>>(),
            [4, 4, 4]
        );
    }
}
