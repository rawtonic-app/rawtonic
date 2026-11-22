//! Desktop glue for `rawtonic_engine::camera_crop` (ADR 0020, option C): the
//! Tauri command; the rule lives in the engine and rawdecode crates.

/// The camera's crop of a RAW for each frame version (index), as recipe
/// `crop` values (`camera_crop::recipe_values`); None where the camera set
/// no other aspect ratio. Empty when neither the file nor, offline, its
/// previews tell it. The editor gives it to new recipes
/// (src/utils/neverEdited.ts).
#[tauri::command]
pub async fn get_camera_crops(path: String) -> Result<Vec<Option<serde_json::Value>>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let (source, _) = crate::file_management::parse_virtual_path(&path);
        let source_str = source.to_string_lossy();
        // Offline: what its previews keep (registered on its first read).
        if rawtonic_engine::source_facts::registered(&source_str).is_none() {
            crate::preview_proxy::prepare_offline(&source);
        }
        // The editor's proxy can be older than the newest cached header.
        // Facts from before RAF aspect ratios were read cannot answer it.
        let crops = if let Some(facts) = rawtonic_engine::source_facts::registered(&source_str) {
            rawtonic_engine::camera_crop::crops_from_facts(&facts, &source_str).unwrap_or_default()
        } else if crate::file_management::is_cloud_placeholder(&source) {
            Vec::new()
        } else {
            rawtonic_engine::camera_crop::crops(&source_str)
        };
        crops
            .into_iter()
            .map(|crop| crop.map(|crop| rawtonic_engine::camera_crop::recipe_values(crop).0))
            .collect()
    })
    .await
    .map_err(|e| e.to_string())
}
