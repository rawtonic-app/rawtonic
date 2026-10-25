//! Desktop glue for `rawtonic_engine::spot_removal` (ADR 0009): the Tauri commands
//! and app-state access stay here, the logic lives in the engine crate.
pub use rawtonic_engine::spot_removal::*;

use serde_json::Value;

/// The automatic source for a spot about to be placed on the open photo:
/// mask-frame centre and radius in, mask-frame source centre out (the
/// recipe's existing patches and spots already applied). With `points`
/// (two or more), a brush stroke along them: where its first point copies
/// from.
#[tauri::command]
pub async fn find_spot_source(
    x: f64,
    y: f64,
    radius: f64,
    points: Option<Vec<StrokePoint>>,
    js_adjustments: Value,
    state: tauri::State<'_, crate::app_state::AppState>,
) -> Result<(f64, f64), String> {
    let (source, is_raw) = crate::editor_preview::original_source(&state).await?;
    tokio::task::spawn_blocking(move || {
        // Spots heal a film negative's positive (`prepare_render_base`).
        let base = rawtonic_engine::image_loader::composite_patches_on_source(
            &source,
            &js_adjustments,
            is_raw,
        )
        .map_err(|e| e.to_string())?;
        match points.filter(|p| p.len() >= 2) {
            Some(points) => find_stroke_source(&base, &js_adjustments, &points, radius),
            None => find_source(&base, &js_adjustments, x, y, radius),
        }
        .ok_or_else(|| "No source area fits around this spot".to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Visualize Spots for the open photo: a JPEG of the band-pass view of the
/// displayed (transformed, cropped) photo with its spots applied, at most
/// `max_edge` pixels long. The band-pass is cached per photo state, so the
/// sensitivity slider only re-thresholds.
#[tauri::command]
pub async fn visualize_spots(
    sensitivity: f64,
    max_edge: u32,
    js_adjustments: Value,
    state: tauri::State<'_, crate::app_state::AppState>,
) -> Result<tauri::ipc::Response, String> {
    let (source, is_raw) = crate::editor_preview::original_source(&state).await?;
    let key = {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        (std::sync::Arc::as_ptr(&source) as usize).hash(&mut hasher);
        crate::cache_utils::calculate_transform_hash(&js_adjustments).hash(&mut hasher);
        crate::cache_utils::calculate_patched_warped_hash(&js_adjustments).hash(&mut hasher);
        max_edge.hash(&mut hasher);
        hasher.finish()
    };
    let cached = band_cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .filter(|(k, _)| *k == key)
        .map(|(_, band)| band.clone());
    let band = match cached {
        Some(band) => band,
        None => {
            let warped = crate::editor_preview::get_cached_full_warped_image(
                &state,
                &source,
                is_raw,
                &js_adjustments,
            );
            let adjustments = js_adjustments.clone();
            let band = tokio::task::spawn_blocking(move || {
                let (transformed, _) = crate::adjustment_utils::apply_spatial_transformations(
                    warped.as_ref(),
                    &adjustments,
                );
                std::sync::Arc::new(spot_band_pass(&transformed, max_edge))
            })
            .await
            .map_err(|e| e.to_string())?;
            *band_cache().lock().unwrap_or_else(|e| e.into_inner()) = Some((key, band.clone()));
            band
        }
    };
    let view = visualize(&band.0, band.1, sensitivity);
    let mut jpeg = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageLuma8(view)
        .write_to(&mut jpeg, image::ImageFormat::Jpeg)
        .map_err(|e| e.to_string())?;
    Ok(tauri::ipc::Response::new(jpeg.into_inner()))
}
