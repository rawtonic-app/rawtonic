//! Camera framing for the desktop's first edits and thumbnails (ADR 0020).

use std::path::Path;

use rawtonic_engine::{camera_crop, develop_defaults, formats, source_facts};
use serde_json::Value;

use crate::{file_management::is_cloud_placeholder, preview_proxy};

/// The camera's crop of a RAW for each frame version (index), as recipe
/// `crop` values (`camera_crop::recipe_values`); None where the camera set
/// no other aspect ratio. Empty when neither the file nor, offline, its
/// previews tell it. The editor gives it to new recipes
/// (src/utils/neverEdited.ts).
#[tauri::command]
pub async fn get_camera_crops(path: String) -> Result<Vec<Option<Value>>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let (source, _) = crate::file_management::parse_virtual_path(&path);
        crops(&source)
            .into_iter()
            .map(|crop| crop.map(|crop| camera_crop::recipe_values(crop).0))
            .collect()
    })
    .await
    .map_err(|e| e.to_string())
}

pub(crate) fn crops(source: &Path) -> Vec<Option<camera_crop::CameraCrop>> {
    if formats::is_raw_file(source) {
        raw_crops(source, is_cloud_placeholder(source))
    } else {
        vec![None; rawtonic_engine::source_frame::VERSION as usize + 1]
    }
}

/// Prepare the framing of a new recipe for a thumbnail or preview.
pub(crate) fn apply_default(adjustments: &mut Value, source: &Path) {
    if !rawtonic_engine::image_processing::is_new_recipe(adjustments) {
        return;
    }
    apply_new_edit(adjustments, source, is_cloud_placeholder(source));
}

/// Start a saved edit only when its Develop defaults can be resolved.
pub(crate) fn start_edit(adjustments: &mut Value, source: &Path) -> Result<(), String> {
    if !rawtonic_engine::image_processing::is_new_recipe(adjustments) {
        return Ok(());
    }
    let placeholder = is_cloud_placeholder(source);
    if placeholder && develop_defaults::follows_defaults(adjustments) {
        let defaults = develop_defaults::current();
        // A TIFF's RAW status needs the file. Formats such as JPEG can
        // still start an edit offline while defaults are limited to RAWs.
        let may_be_raw = || {
            source
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(formats::is_tiff_extension)
                || formats::is_raw_file(source)
        };
        if !defaults.is_empty() && (!defaults.raw_only || may_be_raw()) {
            return Err(format!(
                "{}: original is in the cloud — download it to apply the develop defaults of its first edit",
                source.display()
            ));
        }
    }
    apply_new_edit(adjustments, source, placeholder);
    Ok(())
}

fn apply_new_edit(adjustments: &mut Value, source: &Path, placeholder: bool) {
    camera_crop::apply_default_with(adjustments, source, !placeholder, |is_raw| {
        if is_raw {
            raw_crops(source, placeholder)
        } else {
            Vec::new()
        }
    });
}

pub(crate) fn raw_crops(source: &Path, placeholder: bool) -> Vec<Option<camera_crop::CameraCrop>> {
    // Preview facts can outlive a reconnect. A local file's header gives
    // the crops of all its frames and is cached by file size and mtime.
    let path = source.to_string_lossy();
    if !placeholder {
        let crops = camera_crop::crops_from_raw_file(&path);
        if !crops.is_empty() {
            return crops;
        }
    }
    // For an evicted original, only a preview of the same file can answer.
    // A rebuilt Standard may know more than the proxy that Develop opened.
    if let Some(crops) = preview_proxy::cached_camera(source).0 {
        return crops;
    }
    if placeholder {
        return Vec::new();
    }
    // A disconnected file may have no metadata to match against the proxy.
    // Keep the framing available from its offline preview.
    if source_facts::registered(&path).is_none() {
        preview_proxy::prepare_offline(source);
    }
    source_facts::registered(&path)
        .and_then(|facts| camera_crop::crops_from_facts(&facts, &path))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_camera_crop_uses_rebuilt_standard_for_an_evicted_original() {
        let dir = tempfile::tempdir().unwrap();
        let session =
            rawtonic_catalog::Library::create(dir.path(), "Crop", rawtonic_catalog::Mode::Session)
                .unwrap();
        let source = session.directory.join("Capture").join("camera.RAF");
        std::fs::write(&source, b"evicted RAF").unwrap();
        crate::preview_proxy::tests::cache_camera_crop(&source, vec![0, 376, 4000, 2248]);
        crate::file_management::simulate_cloud_placeholder(&source);
        let path = source.to_str().unwrap();
        rawtonic_engine::source_facts::register(
            path,
            rawtonic_engine::source_facts::SourceFacts {
                version: rawtonic_engine::source_facts::RAF_CROP_VERSION - 1,
                camera_crop: Some(Vec::new()),
                ..Default::default()
            },
        );
        let crops = tauri::async_runtime::block_on(get_camera_crops(path.to_owned())).unwrap();
        rawtonic_engine::source_facts::forget(path);
        assert_eq!(
            crops.last().and_then(Option::as_ref).map(|c| &c["height"]),
            Some(&serde_json::json!(2248)),
        );
    }

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
        let expected = raw_crops(&path, false);
        assert!(!expected.is_empty());
        rawtonic_engine::source_facts::register(
            source,
            rawtonic_engine::source_facts::SourceFacts::default(),
        );
        assert!(raw_crops(&path, true).is_empty());
        assert_eq!(raw_crops(&path, false), expected);
        rawtonic_engine::source_facts::forget(source);
    }
}
