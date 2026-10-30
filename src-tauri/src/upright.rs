//! Desktop glue for `rawtonic_engine::upright` (ADR 0009): the Tauri command
//! stays here, the line detection and Upright logic live in the engine crate.
pub use rawtonic_engine::upright::*;

/// Guide lines for the currently loaded photo: detected on its lens-corrected
/// image (only lens settings of `adjustments`), in the photo's pixel
/// coordinates before orientation, which guided lines use.
/// Off the main thread (the decode wait and the line detection would freeze
/// the window).
#[tauri::command]
pub async fn calculate_upright(
    mode: String,
    adjustments: serde_json::Value,
    app_handle: tauri::AppHandle,
) -> Result<Option<UprightResult>, String> {
    use tauri::Manager;
    let mode = match mode.as_str() {
        "auto" => UprightMode::Auto,
        "level" => UprightMode::Level,
        "vertical" => UprightMode::Vertical,
        "full" => UprightMode::Full,
        other => return Err(format!("Unknown upright mode {other}")),
    };
    let state = app_handle.state::<crate::app_state::AppState>();
    let image = crate::image_loader::original_image(&state).await?.image;
    tauri::async_runtime::spawn_blocking(move || upright_guides(&image, mode, &adjustments))
        .await
        .map_err(|e| format!("Upright failed: {e}"))
}

fn upright_guides(
    image: &image::DynamicImage,
    mode: UprightMode,
    adjustments: &serde_json::Value,
) -> Option<UprightResult> {
    // Only the lens correction: lines are drawn on the lens-corrected photo.
    let mut lens_only = serde_json::json!({});
    for key in [
        "lensCorrectionMode",
        "lensDistortionParams",
        "lensDistortionAmount",
        "lensDistortionEnabled",
    ] {
        if let Some(value) = adjustments.get(key) {
            lens_only[key] = value.clone();
        }
    }
    let (gray, sw, sh) = if lens_only
        .get("lensDistortionParams")
        .is_some_and(|p| !p.is_null())
    {
        let (_, sw, sh) = analysis_image(image, 1400);
        let small = crate::image_processing::thumbnail(image, sw as u32, sh as u32);
        // The lines do not move with the shading's encoding (`is_raw`).
        let corrected = crate::image_processing::apply_geometry_warp(&small, &lens_only, true)
            .inspect_err(|e| log::warn!("Upright: {e}"))
            .ok()?;
        analysis_image(&corrected, 1400)
    } else {
        analysis_image(image, 1400)
    };
    let segments = detect_segments(&gray, sw, sh, (sw as f64).hypot(sh as f64) * 0.02);
    // Guided lines are stored in 0–1 coordinates of the photo.
    let portrait_on_sensor = adjustments["orientationSteps"].as_u64().unwrap_or(0) % 2 == 1;
    upright_lines(&segments, sw as f64, sh as f64, mode, portrait_on_sensor).map(|mut result| {
        for line in &mut result.lines {
            for p in [&mut line.p1, &mut line.p2] {
                p.x /= sw as f64;
                p.y /= sh as f64;
            }
        }
        result
    })
}
