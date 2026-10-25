//! Desktop glue for `rawtonic_engine::red_eye` (ADR 0009): the Tauri commands
//! and app-state access stay here, the logic lives in the engine crate.
pub use rawtonic_engine::red_eye::*;

use serde_json::Value;

/// The pupil inside the circle of `radius` around mask-frame (`x`, `y`) on
/// the open photo, found as the correction will see it (the recipe's AI
/// patches applied, its other red eye corrections and spots not), in the
/// photo as decoded (`RedEye`); None when nothing red (or for pet eyes,
/// glowing) enough is there.
#[tauri::command]
pub async fn detect_red_eye(
    x: f64,
    y: f64,
    radius: f64,
    kind: EyeKind,
    js_adjustments: Value,
    state: tauri::State<'_, crate::app_state::AppState>,
) -> Result<Option<Pupil>, String> {
    let (source, is_raw) = crate::editor_preview::original_source(&state).await?;
    tokio::task::spawn_blocking(move || {
        // A film negative's red eye is red in its positive, where the
        // correction works too (`prepare_render_base`).
        let positive = rawtonic_engine::negative::apply_for_recipe(
            std::borrow::Cow::Borrowed(source.as_ref()),
            &js_adjustments,
            is_raw,
        );
        let base = crate::image_loader::composite_ai_patches(&positive, &js_adjustments)
            .map_err(|e| e.to_string())?;
        Ok(detect(&base, &js_adjustments, x, y, radius, kind, is_raw))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Where the open photo shows each of the recipe's red eye corrections:
/// their ellipses in mask-frame pixels, in the recipe's order (None for one
/// the geometry cannot show).
#[tauri::command]
pub async fn red_eyes_shown(
    js_adjustments: Value,
    state: tauri::State<'_, crate::app_state::AppState>,
) -> Result<Vec<Option<Pupil>>, String> {
    let (source, _) = crate::editor_preview::original_source(&state).await?;
    let (width, height) = (source.width(), source.height());
    let eyes: Vec<Value> = js_adjustments["redEyes"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    Ok(eyes
        .iter()
        .map(|eye| {
            let pupil: Pupil = serde_json::from_value(eye.clone()).ok()?;
            shown(&js_adjustments, width, height, &pupil)
        })
        .collect())
}

/// Mask-frame (`x`, `y`) of the open photo in the photo as decoded, where a
/// correction dragged there is kept.
#[tauri::command]
pub async fn red_eye_point(
    x: f64,
    y: f64,
    js_adjustments: Value,
    state: tauri::State<'_, crate::app_state::AppState>,
) -> Result<(f64, f64), String> {
    let (source, _) = crate::editor_preview::original_source(&state).await?;
    Ok(image_point(
        &js_adjustments,
        source.width(),
        source.height(),
        x,
        y,
    ))
}
