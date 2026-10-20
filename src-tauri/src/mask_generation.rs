//! Desktop glue for `rawtonic_engine::mask_generation` (ADR 0009): the Tauri commands
//! and app-state access stay here, the logic lives in the engine crate.
pub use rawtonic_engine::mask_generation::*;

use image::DynamicImage;
use serde_json::Value;
use std::sync::Arc;
use tauri::Manager;

use crate::app_state::AppState;
use crate::editor_preview::get_cached_full_warped_image;
use rawtonic_engine::luma_levels::luma_levels;

#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn generate_mask_overlay(
    mask_def: serde_json::Value,
    width: u32,
    height: u32,
    scale: f32,
    crop_offset: (f32, f32),
    js_adjustments: Option<serde_json::Value>,
    grayscale: Option<bool>,
    color: Option<[u8; 3]>,
    opacity: Option<u8>,
    app_handle: tauri::AppHandle,
) -> Result<tauri::ipc::Response, String> {
    let style = if grayscale.unwrap_or(false) {
        OverlayStyle::Grayscale
    } else {
        OverlayStyle::Colored {
            rgb: color.unwrap_or(OverlayStyle::DEFAULT_RGB),
            opacity: opacity.unwrap_or(OverlayStyle::DEFAULT_OPACITY),
        }
    };
    tauri::async_runtime::spawn_blocking(move || {
        let state = app_handle.state::<AppState>();
        generate_mask_overlay_bytes(
            mask_def,
            width,
            height,
            scale,
            crop_offset,
            js_adjustments,
            style,
            state,
        )
        .map(tauri::ipc::Response::new)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// What the luma range eyedropper gets: the tones under the sample and the
/// photo's tone at every whole percentile (`luma_levels`), from which the
/// frontend sets the range by Lightroom's rule in the photo's percentiles.
#[derive(serde::Serialize, Debug, Clone)]
pub struct LumaSampleReply {
    #[serde(flatten)]
    pub tones: LumaSample,
    pub levels: Vec<f32>,
}

/// The luma range eyedropper: the tones under a point or an area of the open
/// photo (`sample_luma_tones`) and the photo's tone levels, from the same mask
/// source the luma range masks of `js_adjustments` are made from.
#[tauri::command]
pub async fn sample_luma_range(
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    mut js_adjustments: Value,
    app_handle: tauri::AppHandle,
) -> Result<Option<LumaSampleReply>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app_handle.state::<AppState>();
        crate::adjustment_utils::hydrate_adjustments(&state, &mut js_adjustments);
        let (source, is_raw) = crate::editor_preview::get_original_image(&state)?;
        let mask_source = ParametricMaskSource::new(
            get_cached_full_warped_image(&state, &source, is_raw, &js_adjustments),
            (source.width() as f64, source.height() as f64),
        )?
        .with_geometry(&js_adjustments)
        .with_raw_curve(is_raw);
        Ok(
            sample_luma_tones(&mask_source, x, y, width, height).map(|tones| LumaSampleReply {
                tones,
                levels: luma_levels(&mask_source.image),
            }),
        )
    })
    .await
    .map_err(|e| e.to_string())?
}

#[allow(clippy::too_many_arguments)]
fn generate_mask_overlay_bytes(
    mut mask_def: serde_json::Value,
    width: u32,
    height: u32,
    scale: f32,
    crop_offset: (f32, f32),
    mut js_adjustments: Option<serde_json::Value>,
    style: OverlayStyle,
    state: tauri::State<'_, AppState>,
) -> Result<Vec<u8>, String> {
    #[cfg(feature = "performance-profiling")]
    let _stage = crate::performance::PerformanceStage::new("mask_overlay");
    if let Some(ref mut adj) = js_adjustments {
        crate::adjustment_utils::hydrate_adjustments(&state, adj);
    }

    if let Some(sub_masks) = mask_def.get_mut("subMasks").and_then(|v| v.as_array_mut()) {
        let mut cache = state.patch_cache.lock().unwrap();
        crate::adjustment_utils::hydrate_sub_masks(sub_masks, &mut cache);
    }

    let parsed_mask_def: MaskDefinition = serde_json::from_value(mask_def)
        .map_err(|e| format!("Failed to parse hydrated mask_def: {}", e))?;

    let scaled_crop_offset = (crop_offset.0 * scale, crop_offset.1 * scale);

    let warped_image =
        if parsed_mask_def.produces_bitmap() && parsed_mask_def.requires_warped_image() {
            let (source, is_raw) = crate::editor_preview::get_original_image(&state)?;
            resolve_warped_image_for_masks(
                &state,
                &source,
                is_raw,
                1.0,
                js_adjustments.as_ref().unwrap_or(&Value::Null),
                std::slice::from_ref(&parsed_mask_def),
            )?
        } else {
            None
        };

    let mut geometry =
        PhotoGeometry::from_adjustments(js_adjustments.as_ref().unwrap_or(&Value::Null));
    if let Ok((_, _, (photo_w, photo_h))) = crate::editor_preview::preview_source(&state) {
        geometry = geometry.with_photo_size(photo_w as f64, photo_h as f64);
    }
    if let Some(gray_mask) = generate_mask_bitmap(
        &parsed_mask_def,
        width,
        height,
        scale,
        scaled_crop_offset,
        &geometry,
        warped_image.as_ref(),
    ) {
        encode_mask_png(&gray_mask, style)
    } else {
        Ok(Vec::new())
    }
}

/// The layer list's mask thumbnails, as PNG data URLs by layer and component
/// id (white mask on black, in the photo's frame as the viewer shows it).
#[derive(serde::Serialize, Default)]
pub struct MaskThumbnails {
    pub layers: std::collections::HashMap<String, String>,
    pub components: std::collections::HashMap<String, String>,
}

/// Thumbnails of the layers `layer_ids` and the components `component_ids`
/// (each fitting `layer_size` / `component_size` device pixels), reduced from
/// the preview's cached masks where it has them, else from masks drawn at a
/// few times the thumbnail's size; never a full-size mask over IPC. Layers or
/// components without pixels yet (AI masks computing) are left out.
#[tauri::command]
pub async fn generate_mask_thumbnails(
    js_adjustments: serde_json::Value,
    layer_ids: Vec<String>,
    component_ids: Vec<String>,
    layer_size: (u32, u32),
    component_size: (u32, u32),
    app_handle: tauri::AppHandle,
) -> Result<MaskThumbnails, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app_handle.state::<AppState>();
        mask_thumbnails(
            &state,
            js_adjustments,
            &layer_ids,
            &component_ids,
            layer_size,
            component_size,
        )
    })
    .await
    .map_err(|e| e.to_string())?
}

fn mask_thumbnails(
    state: &tauri::State<'_, AppState>,
    mut adjustments: serde_json::Value,
    layer_ids: &[String],
    component_ids: &[String],
    layer_size: (u32, u32),
    component_size: (u32, u32),
) -> Result<MaskThumbnails, String> {
    use rawtonic_engine::mask_thumbnail::{
        MAX_THUMBNAIL_SIDE, MaskFrame, THUMBNAIL_OVERSAMPLING, ThumbnailSources,
        component_thumbnail_definition, layer_thumbnail_definition,
    };
    #[cfg(feature = "performance-profiling")]
    let _stage = crate::performance::PerformanceStage::new("mask_thumbnails");
    let started = std::time::Instant::now();
    let clamp = |(w, h): (u32, u32)| {
        (
            w.clamp(1, MAX_THUMBNAIL_SIDE),
            h.clamp(1, MAX_THUMBNAIL_SIDE),
        )
    };
    let (layer_size, component_size) = (clamp(layer_size), clamp(component_size));
    crate::adjustment_utils::hydrate_adjustments(state, &mut adjustments);
    let masks: Vec<MaskDefinition> = adjustments
        .get("masks")
        .and_then(|m| serde_json::from_value(m.clone()).ok())
        .unwrap_or_default();

    let mut layers = Vec::new();
    let mut components = Vec::new();
    for mask in &masks {
        if layer_ids.contains(&mask.id) {
            layers.push((mask.id.clone(), layer_thumbnail_definition(mask)));
        }
        for sub in &mask.sub_masks {
            if component_ids.contains(&sub.id) {
                components.push((sub.id.clone(), component_thumbnail_definition(mask, sub)));
            }
        }
    }
    let mut thumbnails = MaskThumbnails::default();
    if layers.is_empty() && components.is_empty() {
        return Ok(thumbnails);
    }

    let (loaded, source_scale, full_dims) = crate::editor_preview::preview_source(state)?;
    let wanted: Vec<MaskDefinition> = layers
        .iter()
        .chain(&components)
        .map(|(_, def)| def.clone())
        .collect();
    let warped = resolve_warped_image_for_masks(
        state,
        &loaded.image,
        loaded.is_raw,
        source_scale,
        &adjustments,
        &wanted,
    )?;
    // The preview's frame, while it shows these adjustments' geometry: its
    // masks are cached under it.
    let preview = state
        .cached_preview
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .filter(|c| {
            c.source.ptr_eq(&Arc::downgrade(&loaded.image))
                && c.transform_hash == crate::cache_utils::calculate_transform_hash(&adjustments)
        })
        .map(|c| MaskFrame {
            width: c.image.width(),
            height: c.image.height(),
            scale: c.scale,
            crop_offset: (
                c.unscaled_crop_offset.0 * c.scale,
                c.unscaled_crop_offset.1 * c.scale,
            ),
        });
    let longest = layer_size
        .0
        .max(layer_size.1)
        .max(component_size.0.max(component_size.1))
        * THUMBNAIL_OVERSAMPLING;
    let small =
        MaskFrame::fitting(&adjustments, full_dims, longest).ok_or("The photograph has no size")?;
    let geometry = PhotoGeometry::from_adjustments(&adjustments)
        .with_photo_size(full_dims.0 as f64, full_dims.1 as f64);
    let sources = ThumbnailSources {
        preview: preview.map(|frame| (frame, &state.mask_cache)),
        small,
        small_cache: &state.mask_thumbnail_cache,
        warped: warped.as_ref(),
        geometry: &geometry,
    };
    for (id, def) in layers {
        if let Some(url) = sources.thumbnail(&def, layer_size.0, layer_size.1)? {
            thumbnails.layers.insert(id, url);
        }
    }
    for (id, def) in components {
        if let Some(url) = sources.thumbnail(&def, component_size.0, component_size.1)? {
            thumbnails.components.insert(id, url);
        }
    }
    log::debug!(
        "Mask thumbnails: {} layers, {} components in {:.1} ms",
        thumbnails.layers.len(),
        thumbnails.components.len(),
        started.elapsed().as_secs_f64() * 1000.0
    );
    Ok(thumbnails)
}

pub fn resolve_warped_image_for_masks(
    state: &tauri::State<AppState>,
    source: &Arc<DynamicImage>,
    is_raw: bool,
    source_scale: f32,
    adjustments: &serde_json::Value,
    masks: &[MaskDefinition],
) -> Result<Option<ParametricMaskSource>, String> {
    if masks
        .iter()
        .any(|m| m.produces_bitmap() && m.requires_warped_image())
    {
        validate_source_scale(source_scale)?;
        Ok(Some(
            ParametricMaskSource::new(
                get_cached_full_warped_image(state, source, is_raw, adjustments),
                (
                    source.width() as f64 / source_scale as f64,
                    source.height() as f64 / source_scale as f64,
                ),
            )?
            .with_geometry(adjustments)
            .with_raw_curve(is_raw),
        ))
    } else {
        Ok(None)
    }
}

#[allow(clippy::too_many_arguments)]
pub fn get_cached_or_generate_mask(
    state: &tauri::State<AppState>,
    def: &MaskDefinition,
    width: u32,
    height: u32,
    scale: f32,
    crop_offset: (f32, f32),
    geometry: &PhotoGeometry,
    warped_image: Option<&ParametricMaskSource>,
) -> Result<Option<Arc<MaskBitmap>>, String> {
    cached_or_generated_mask(
        &state.mask_cache,
        def,
        width,
        height,
        scale,
        crop_offset,
        geometry,
        warped_image,
    )
}
