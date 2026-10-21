//! Desktop glue for `rawtonic_engine::embedded_lens` (ADR 0009): the Tauri commands
//! and app-state access stay here, the logic lives in the engine crate.
pub use rawtonic_engine::embedded_lens::*;

/// The camera's own lens correction for a RAW file (Fujifilm, Sony, Panasonic,
/// Olympus / OM System, Nikon Z).
#[tauri::command]
pub async fn get_camera_lens_profile(
    path: String,
    ca_version: Option<u64>,
) -> Result<Option<CameraLensProfile>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let (source, _) = crate::file_management::parse_virtual_path(&path);
        read(&source).and_then(|data| {
            // A recipe's own camera CA version (`cameraCaVersion`, pasted),
            // as its save and export read it (`params_for_recipe`).
            let params = match ca_version {
                Some(version) => {
                    params_for_recipe(&source, &serde_json::json!({ CA_VERSION_KEY: version }))?
                }
                None => to_params(&data)?,
            };
            Some(CameraLensProfile {
                maker: data.maker,
                vignetting: data.vignetting.is_some(),
                params,
            })
        })
    })
    .await
    .map_err(|e| e.to_string())
}
