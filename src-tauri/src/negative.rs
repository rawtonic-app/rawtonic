//! Tauri commands of the Negative tool (`rawtonic_engine::negative`): the
//! automatic film base and highlight, and the film base picked on the photo.

use std::sync::Arc;

use rawtonic_engine::negative::{self, Analysis};
use tauri::Manager;

use crate::app_state::AppState;

/// The loaded decode of `path` and whether it is a RAW; None when another
/// photo is loaded.
async fn loaded(
    state: &AppState,
    path: &str,
) -> Result<Option<(Arc<image::DynamicImage>, bool)>, String> {
    let loaded = crate::image_loader::original_image(state).await?;
    Ok((loaded.path == path).then_some((loaded.image, loaded.is_raw)))
}

/// The film base and the frame's highlight of the loaded photo `path`
/// (`negative::analyze` of `negative::analysis_image`: the scan with the
/// geometry of `js_adjustments`, its crop as the frame). None when `path` is
/// not the loaded photo.
#[tauri::command]
pub(crate) async fn analyze_negative(
    path: String,
    js_adjustments: serde_json::Value,
    app_handle: tauri::AppHandle,
) -> Result<Option<Analysis>, String> {
    let state = app_handle.state::<AppState>();
    let Some((image, is_raw)) = loaded(&state, &path).await? else {
        return Ok(None);
    };
    tauri::async_runtime::spawn_blocking(move || {
        let (scan, frame) = negative::analysis_image(&image, &js_adjustments);
        Ok(negative::analyze(&scan, is_raw, frame))
    })
    .await
    .map_err(|error| format!("Negative analysis failed: {error}"))?
}

/// The film base under the film base picker: mean linear RGB (11 × 11
/// preview pixels, as the white balance picker samples) at a normalised
/// position of the shown photo, from its base before the negative
/// conversion and retouching (`js_adjustments` gives the geometry and
/// crop that place the point).
#[tauri::command]
pub(crate) async fn sample_film_base(
    path: String,
    normalized_x: f64,
    normalized_y: f64,
    js_adjustments: serde_json::Value,
    app_handle: tauri::AppHandle,
) -> Result<Option<[f64; 3]>, String> {
    let state = app_handle.state::<AppState>();
    let Some((image, is_raw)) = loaded(&state, &path).await? else {
        return Ok(None);
    };
    tauri::async_runtime::spawn_blocking(move || {
        let mut recipe = js_adjustments;
        if let Some(object) = recipe.as_object_mut() {
            for key in [
                negative::KEY,
                "aiPatches",
                "spots",
                "redEyes",
                "faceRetouch",
                "lensBlurEnabled",
            ] {
                object.remove(key);
            }
        }
        const SAMPLE_DIM: u32 = 1024;
        let (w, h) = (image.width(), image.height());
        let scale =
            crate::editor_preview::reduced_preview_scale(w, h, &recipe, SAMPLE_DIM).unwrap_or(1.0);
        let base = crate::editor_preview::reduced_preview_base(
            &image, 1.0, w, h, &recipe, SAMPLE_DIM, scale, is_raw,
        )?;
        Ok(crate::gpu_processing::mean_linear_rgb(
            &base.image,
            is_raw,
            normalized_x,
            normalized_y,
        ))
    })
    .await
    .map_err(|error| format!("Film base sampling failed: {error}"))?
}
