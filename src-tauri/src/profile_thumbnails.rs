//! The profile browser's tiles: the open photograph with each camera
//! profile, look or black-and-white mix, rendered small from the preview
//! base the shown frame was built from (never from the file, never a
//! fabricated preview). One call renders a batch of tiles on one downscaled
//! base; a newer batch or closing the browser cancels the rest.

use crate::adjustment_utils::hydrate_adjustments;
use crate::app_state::AppState;
use crate::cache_utils::calculate_transform_hash;
use crate::editor_preview::{preview_mask_bitmaps, preview_source};
use crate::gpu_processing;
use crate::image_processing::{
    RenderRequest, downscale_f32_image, get_all_adjustments_with_masks, get_or_init_gpu_context,
    mask_definitions_from_json, resolve_tonemapper_override_from_handle,
};
use crate::lut_processing;
use image::{DynamicImage, GenericImageView};
use serde_json::Value;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tauri::Manager;

/// The largest tile edge rendered (a 160-point tile on a 2× screen is 320).
const MAX_TILE: u32 = 384;

/// The newest batch asked for or cancelled; older batches stop between tiles.
static LATEST: AtomicU64 = AtomicU64::new(0);

/// The downscaled base of the last batch: (preview base address, transform
/// hash, tile edge) → base. Hovering and the next rows reuse it.
type SmallBase = ((usize, u64, u32), Arc<DynamicImage>);
static SMALL_BASE: Mutex<Option<SmallBase>> = Mutex::new(None);

/// `variant`'s keys over `recipe`'s (a tile changes only profile fields:
/// `cameraProfile`, the look's `lut*` fields, `blackAndWhite`/`grayMixer`).
pub(crate) fn tile_recipe(recipe: &Value, variant: &Value) -> Value {
    let mut out = recipe.clone();
    if let (Some(out), Some(variant)) = (out.as_object_mut(), variant.as_object()) {
        for (key, value) in variant {
            if value.is_null() {
                out.remove(key);
            } else {
                out.insert(key.clone(), value.clone());
            }
        }
    }
    out
}

/// Frames tiles as `u32` little-endian length + JPEG each (length 0: not
/// rendered).
pub(crate) fn frame_tiles(tiles: &[Option<Vec<u8>>]) -> Vec<u8> {
    let mut out = Vec::with_capacity(tiles.iter().flatten().map(|t| t.len() + 4).sum());
    for tile in tiles {
        let bytes = tile.as_deref().unwrap_or(&[]);
        out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        out.extend_from_slice(bytes);
    }
    out
}

/// Stops batches older than or equal to `generation` (the browser closed or
/// asks for other tiles).
#[tauri::command]
pub fn cancel_profile_thumbnails(generation: u64) {
    LATEST.fetch_max(generation, Ordering::SeqCst);
}

/// Tiles of `path` (the open photograph) developed with `adjustments` and
/// each of `variants` merged over it (`tile_recipe`), at most `size` pixels
/// on the long edge, as `frame_tiles`. Empty when `path` is not the loaded
/// photo or no preview base of its geometry is cached yet (the browser asks
/// again with the next frame); a cancelled batch frames the tiles it did not
/// render as length 0.
#[tauri::command]
pub async fn profile_thumbnails(
    path: String,
    adjustments: Value,
    variants: Vec<Value>,
    size: u32,
    generation: u64,
    app_handle: tauri::AppHandle,
) -> Result<tauri::ipc::Response, String> {
    LATEST.fetch_max(generation, Ordering::SeqCst);
    tauri::async_runtime::spawn_blocking(move || {
        render_tiles(&app_handle, &path, adjustments, &variants, size, generation)
    })
    .await
    .map_err(|error| format!("Profile thumbnail task failed: {error}"))?
    .map(tauri::ipc::Response::new)
}

fn render_tiles(
    app_handle: &tauri::AppHandle,
    path: &str,
    mut recipe: Value,
    variants: &[Value],
    size: u32,
    generation: u64,
) -> Result<Vec<u8>, String> {
    #[cfg(feature = "performance-profiling")]
    let _stage = crate::performance::PerformanceStage::new("profile_thumbnails");
    let state = app_handle.state::<AppState>();
    let context = get_or_init_gpu_context(&state, app_handle)?;
    hydrate_adjustments(&state, &mut recipe);
    let (loaded_image, source_scale, full_dims) = preview_source(&state)?;
    if loaded_image.path != path {
        return Ok(Vec::new());
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
        return Ok(Vec::new());
    };
    let size = size.clamp(32, MAX_TILE);
    let small = small_base(&base, transform_hash, size);
    let (width, height) = small.dimensions();
    let small_scale = scale * width as f32 / base.width().max(1) as f32;
    // A distinct key: the small base is never the frame's input.
    let small_hash = transform_hash ^ (u64::from(size) << 48) ^ 0x7072_6f66_696c_6573;
    let mask_definitions = mask_definitions_from_json(&recipe);
    let mask_bitmaps = preview_mask_bitmaps(
        &state,
        &recipe,
        &mask_definitions,
        (&loaded_image, source_scale, full_dims),
        (width, height),
        small_scale,
        unscaled_crop_offset,
    )?;
    let tm_override = resolve_tonemapper_override_from_handle(app_handle, loaded_image.is_raw);
    let mut tiles = Vec::with_capacity(variants.len());
    for variant in variants {
        if LATEST.load(Ordering::SeqCst) > generation {
            tiles.push(None);
            continue;
        }
        let mut tile = tile_recipe(&recipe, variant);
        hydrate_adjustments(&state, &mut tile);
        let mut probe = get_all_adjustments_with_masks(
            &tile,
            loaded_image.is_raw,
            tm_override,
            &mask_definitions,
        );
        crate::baseline_exposure::apply_base_look(
            &mut probe.global,
            &loaded_image.path,
            loaded_image.measures.as_ref(),
        );
        probe.global.detail_scale = small_scale;
        let lut = tile["lutPath"]
            .as_str()
            .and_then(|p| lut_processing::get_or_load_lut(&state, p).ok());
        let image = gpu_processing::process_and_get_dynamic_image_with_preview_shader(
            &context,
            &state,
            &small,
            small_hash,
            RenderRequest {
                adjustments: probe,
                mask_bitmaps: &mask_bitmaps,
                lut,
                roi: None,
            },
            "profile_thumbnails",
        )?;
        let jpeg = crate::file_management::encode_thumbnail(&image, size)
            .map_err(|e| format!("Encoding a profile tile failed: {e:#}"))?;
        tiles.push(Some(jpeg));
    }
    Ok(frame_tiles(&tiles))
}

/// `base` fitted into `size` × `size`, kept for the next batch.
fn small_base(base: &Arc<DynamicImage>, transform_hash: u64, size: u32) -> Arc<DynamicImage> {
    let key = (Arc::as_ptr(base) as usize, transform_hash, size);
    let mut cached = SMALL_BASE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((k, image)) = cached.as_ref()
        && *k == key
    {
        return Arc::clone(image);
    }
    let image = Arc::new(downscale_f32_image(base, size, size));
    *cached = Some((key, Arc::clone(&image)));
    image
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_tile_changes_only_the_variant_keys() {
        let recipe = json!({"exposure": 1, "lutPath": "a.cube", "lutIntensity": 50});
        let look = tile_recipe(&recipe, &json!({"lutPath": "b.cube", "lutIntensity": 100}));
        assert_eq!(
            look,
            json!({"exposure": 1, "lutPath": "b.cube", "lutIntensity": 100})
        );
        // null removes: the Rawtonic profile and "no look".
        let none = tile_recipe(&recipe, &json!({"lutPath": null, "cameraProfile": null}));
        assert_eq!(none, json!({"exposure": 1, "lutIntensity": 50}));
        // The geometry, and so the preview base a tile renders from, stays.
        assert_eq!(
            calculate_transform_hash(&look),
            calculate_transform_hash(&recipe)
        );
    }

    #[test]
    fn tiles_are_framed_by_length() {
        let framed = frame_tiles(&[Some(vec![1, 2, 3]), None, Some(vec![9])]);
        assert_eq!(framed, [3, 0, 0, 0, 1, 2, 3, 0, 0, 0, 0, 1, 0, 0, 0, 9]);
    }
}
