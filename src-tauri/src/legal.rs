// SPDX-License-Identifier: AGPL-3.0-or-later
//! The license texts the app ships, for About.

use tauri::Manager;

/// THIRD_PARTY_LICENSES.txt from the app's licenses folder, written by
/// `scripts/third_party_licenses.py` before every `tauri build`.
#[tauri::command]
pub fn third_party_licenses(app_handle: tauri::AppHandle) -> Result<String, String> {
    let path = app_handle
        .path()
        .resolve(
            "resources/licenses/THIRD_PARTY_LICENSES.txt",
            tauri::path::BaseDirectory::Resource,
        )
        .map_err(|e| e.to_string())?;
    std::fs::read_to_string(&path).map_err(|_| {
        "The license texts are generated for release builds (npm run licenses).".to_string()
    })
}
