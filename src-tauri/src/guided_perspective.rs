//! Desktop glue for `rawtonic_engine::guided_perspective` (ADR 0009): the Tauri commands
//! and app-state access stay here, the logic lives in the engine crate.
pub use rawtonic_engine::guided_perspective::*;

#[tauri::command]
pub fn calculate_guided_perspective(
    lines: Vec<GuideLine>,
    width: f64,
    height: f64,
    fill: Option<bool>,
) -> GuidedResultJson {
    match guided_homography(&lines, width, height, fill.unwrap_or(false)) {
        Some(result) => GuidedResultJson {
            forward_h: flatten_row_major(&result.forward_h),
            crop: compute_max_inscribed_crop(&result.forward_h, width, height),
            valid: true,
            debug_vp: result.debug_vp,
        },
        None => GuidedResultJson {
            forward_h: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
            crop: [0.0, 0.0, 1.0, 1.0],
            valid: false,
            debug_vp: [None, None],
        },
    }
}

/// The manual Transform sliders' homography on centred pixel coordinates
/// (row-major, source to output), so overlays use the engine's own model.
#[tauri::command]
pub fn calculate_manual_transform(
    adjustments: serde_json::Value,
    width: f64,
    height: f64,
) -> [f64; 9] {
    let params = rawtonic_engine::image_processing::get_geometry_params_from_json(&adjustments);
    let m = rawtonic_engine::image_processing::manual_transform(&params, width, height);
    [
        m[0][0], m[0][1], m[0][2], m[1][0], m[1][1], m[1][2], m[2][0], m[2][1], m[2][2],
    ]
}
