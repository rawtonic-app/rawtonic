//! Desktop glue for `rawtonic_engine::develop_defaults`: the command the
//! editor asks a photograph's develop defaults with; the logic lives in the
//! engine crate, the settings in `AppSettings::develop_defaults`.
pub use rawtonic_engine::develop_defaults::*;

use serde::Serialize;

/// What the editor needs of a photograph's defaults: the camera it was
/// taken with (for "Set as default for …") and the defaults it gets now.
#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PhotoDefaults {
    pub camera: Option<CameraIdentity>,
    pub resolved: Option<Resolved>,
}

/// The develop defaults of the photograph at `path` (a virtual copy's are
/// its source file's), as its never-edited renders get them.
#[tauri::command]
pub async fn develop_defaults_for(path: String) -> Result<PhotoDefaults, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let (source, _) = rawtonic_export::sidecars::parse_virtual_path(&path);
        let camera = camera_of(&source);
        let resolved = resolve(
            &current(),
            rawtonic_engine::formats::is_raw_file(&source),
            camera.as_ref(),
        );
        PhotoDefaults { camera, resolved }
    })
    .await
    .map_err(|e| e.to_string())
}
