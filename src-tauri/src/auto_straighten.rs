//! Desktop glue for `rawtonic_engine::auto_straighten` (ADR 0009): the Tauri
//! command stays here, the analysis and the crop reframing live in the
//! engine crate.
pub use rawtonic_engine::auto_straighten::*;

use serde::Serialize;
use tauri::Manager;

/// What Auto straighten sets on the open photograph.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoStraightenResult {
    /// The recipe's `rotation` (degrees).
    pub rotation: f64,
    /// The crop in pixels of the developed frame.
    pub crop: CropPixels,
    /// Line support (diagonals) behind the answer.
    pub support: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct CropPixels {
    pub unit: &'static str,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl From<Straightened> for AutoStraightenResult {
    fn from(result: Straightened) -> Self {
        Self {
            rotation: result.rotation,
            crop: CropPixels {
                unit: "px",
                x: result.crop.x,
                y: result.crop.y,
                width: result.crop.width,
                height: result.crop.height,
            },
            support: result.support,
        }
    }
}

/// The error of `calculate_auto_straighten` when the decode is another
/// photograph's (the selection changed): the webview drops the request.
pub const ANOTHER_PHOTO: &str = "auto straighten: another photograph is open";

/// Auto straighten for the open photograph `path` under `adjustments` (its
/// full decode): None when it has too few straight lines; `ANOTHER_PHOTO`
/// when the decode is not `path`'s. Off the main thread.
#[tauri::command]
pub async fn calculate_auto_straighten(
    path: String,
    adjustments: serde_json::Value,
    app_handle: tauri::AppHandle,
) -> Result<Option<AutoStraightenResult>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app_handle.state::<crate::app_state::AppState>();
        let loaded = crate::image_loader::wait_for_original_image(&state)?;
        if loaded.path != path {
            return Err(ANOTHER_PHOTO.to_string());
        }
        let image = loaded.image;
        let (width, height) = (image.width(), image.height());
        Ok(straighten_recipe(&image, width, height, &adjustments).map(AutoStraightenResult::from))
    })
    .await
    .map_err(|e| format!("Auto straighten failed: {e}"))?
}
