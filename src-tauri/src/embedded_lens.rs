//! Desktop glue for `rawtonic_engine::embedded_lens` (ADR 0009): the Tauri commands
//! and app-state access stay here, the logic lives in the engine crate.
pub use rawtonic_engine::embedded_lens::*;

/// The camera's own lens correction for a RAW file (Fujifilm, Sony, Panasonic,
/// Olympus / OM System, Nikon Z), for the frame that frame version `frame`
/// develops (the recipe's `sourceFrameVersion`; the newest when absent).
#[tauri::command]
pub async fn get_camera_lens_profile(
    path: String,
    frame: Option<u32>,
) -> Result<Option<CameraLensProfile>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let (source, _) = crate::file_management::parse_virtual_path(&path);
        let frame = rawtonic_engine::source_frame::effective(
            &source.to_string_lossy(),
            frame.unwrap_or(rawtonic_engine::source_frame::VERSION),
        );
        read_version(&source, CA_VERSION, GEOMETRY_VERSION, frame).and_then(|data| {
            Some(CameraLensProfile {
                maker: data.maker,
                vignetting: data.vignetting.is_some(),
                params: to_params(&data)?,
            })
        })
    })
    .await
    .map_err(|e| e.to_string())
}
