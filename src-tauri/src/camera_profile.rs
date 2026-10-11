//! Desktop glue for `rawtonic_engine::camera_profile` (ADR 0009): the Tauri commands
//! and app-state access stay here, the logic lives in the engine crate.
pub use rawtonic_engine::camera_profile::*;

use std::path::Path;

/// The photo's "As Shot" white balance in Kelvin and tint (`None` for
/// files without usable calibrations, e.g. JPEGs).
#[tauri::command]
pub async fn white_balance_info(image_path: String) -> Result<Option<WhiteBalanceInfo>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        as_shot_white_balance(&image_path).map(|(temperature, tint)| WhiteBalanceInfo {
            temperature: temperature.round(),
            tint: tint.round(),
        })
    })
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn white_balance_from_sample(
    image_path: String,
    rgb: [f32; 3],
) -> Result<Option<WhiteBalanceInfo>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        white_balance_for_sample(&image_path, rgb).map(|(temperature, tint)| WhiteBalanceInfo {
            temperature: temperature.clamp(2000.0, 50_000.0).round(),
            tint: tint.clamp(-150.0, 150.0).round(),
        })
    })
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn import_camera_profile(
    source: String,
    image_path: String,
    app_handle: tauri::AppHandle,
) -> Result<ProfileEntry, String> {
    use tauri::Manager;
    let app_data = app_handle.path().app_data_dir().ok();
    tauri::async_runtime::spawn_blocking(move || {
        import(
            Path::new(&source),
            Path::new(&image_path),
            app_data.as_deref(),
        )
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn list_camera_profiles(
    image_path: String,
    app_handle: tauri::AppHandle,
) -> Result<Vec<ProfileEntry>, String> {
    use tauri::Manager;
    let app_data = app_handle.path().app_data_dir().ok();
    tauri::async_runtime::spawn_blocking(move || list(Path::new(&image_path), app_data.as_deref()))
        .await
        .map_err(|e| e.to_string())
}
