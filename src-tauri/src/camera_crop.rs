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
        let crops = editor_camera_crops(
            &source_str,
            crate::file_management::is_cloud_placeholder(&source),
        );
        crops
            .into_iter()
            .map(|crop| crop.map(|crop| rawtonic_engine::camera_crop::recipe_values(crop).0))
            .collect()
    })
    .await
    .map_err(|e| e.to_string())
}

fn editor_camera_crops(
    source: &str,
    placeholder: bool,
) -> Vec<Option<rawtonic_engine::camera_crop::CameraCrop>> {
    // The editor's proxy can be older than the newest cached header.
    // Facts from before RAF aspect ratios were read cannot answer it.
    use rawtonic_engine::camera_crop;
    let cached = rawtonic_engine::source_facts::registered(source)
        .and_then(|facts| camera_crop::crops_from_facts(&facts, source));
    if let Some(crops) = cached {
        crops
    } else if placeholder {
        Vec::new()
    } else if rawtonic_engine::formats::is_raw_file(source) {
        camera_crop::crops_from_raw_file(source)
    } else {
        camera_crop::crops(source)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn obsolete_camera_facts_do_not_hide_the_answer_of_a_local_raw() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("camera.dng");
        std::fs::write(
            &path,
            include_bytes!("../../third_party/rawler/src/decoders/dng_lossy_jpeg_test.dng"),
        )
        .unwrap();
        let source = path.to_str().unwrap();
        let expected = editor_camera_crops(source, false);
        assert!(!expected.is_empty());
        rawtonic_engine::source_facts::register(
            source,
            rawtonic_engine::source_facts::SourceFacts::default(),
        );
        assert!(editor_camera_crops(source, true).is_empty());
        assert_eq!(editor_camera_crops(source, false), expected);
        rawtonic_engine::source_facts::forget(source);
    }
}
