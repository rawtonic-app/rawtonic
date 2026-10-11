//! Desktop glue for `rawtonic_export::exif_processing` (ADR 0009): editor
//! sidecars and the session catalog are read here (`session_bridge`), the
//! EXIF extraction and export metadata live in the crate.
pub use rawtonic_export::exif_processing::*;

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use crate::image_processing::ImageMetadata;
use chrono::{DateTime, Utc};

pub fn load_sidecar(sidecar_path: &Path) -> ImageMetadata {
    if let Some(value) = crate::session_bridge::read_editor_sidecar(sidecar_path)
        && let Ok(meta) = serde_json::from_value(value)
    {
        return meta;
    }
    load_sidecar_file(sidecar_path)
}

/// Physical fallback after the caller has already resolved database authority.
pub(crate) fn load_sidecar_file(sidecar_path: &Path) -> ImageMetadata {
    let (meta, healed) = rawtonic_export::sidecars::read_sidecar_file(sidecar_path);
    if healed && let Ok(json) = serde_json::to_string_pretty(&meta) {
        let _ = crate::session_bridge::write_editor_sidecar(sidecar_path, json);
        log::info!(
            "Auto-healed bloated sidecar for: {}",
            sidecar_path.display()
        );
    }
    meta
}

pub fn load_sidecar_with_exif(sidecar_path: &Path, source_path: &Path) -> ImageMetadata {
    let mut meta = load_sidecar(sidecar_path);

    if meta.exif.is_none() {
        if let Some(cached_exif) = read_rrexif_sidecar(source_path) {
            meta.exif = Some(cached_exif);
        } else {
            let source_path_str = source_path.to_string_lossy();
            let extracted_exif =
                if let Ok(mmap) = crate::file_management::read_file_mapped(source_path) {
                    read_exif_data(&source_path_str, &mmap)
                } else if let Ok(bytes) = std::fs::read(source_path) {
                    read_exif_data(&source_path_str, &bytes)
                } else {
                    std::collections::HashMap::new()
                };

            if !extracted_exif.is_empty() {
                meta.exif = Some(extracted_exif);
            }
        }
    }

    meta
}

fn load_primary_metadata(image_path: &Path) -> ImageMetadata {
    let primary = get_primary_sidecar_path(image_path);
    load_sidecar(&primary)
}

fn save_primary_metadata(image_path: &Path, metadata: &ImageMetadata) -> std::io::Result<()> {
    let primary = get_primary_sidecar_path(image_path);
    let json = serde_json::to_string_pretty(metadata).map_err(std::io::Error::other)?;
    rawtonic_metadata::atomic_write(&primary, json).map_err(std::io::Error::other)
}

pub fn read_rrexif_sidecar(image_path: &Path) -> Option<HashMap<String, String>> {
    let primary = get_primary_sidecar_path(image_path);

    if primary.exists() {
        let metadata = load_primary_metadata(image_path);
        if let Some(exif) = metadata.exif {
            return Some(exif);
        }
    }

    if let Some(exif) = get_exif_from_rrcache(image_path) {
        return Some(exif);
    }

    let legacy = get_rrexif_path(image_path);
    if legacy.exists()
        && let Ok(content) = fs::read_to_string(&legacy)
        && let Ok(map) = serde_json::from_str::<HashMap<String, String>>(&content)
    {
        save_exif_to_rrcache(image_path, map.clone());
        let _ = fs::remove_file(&legacy);
        return Some(map);
    }

    None
}

pub fn try_get_exif_creation_date(path: &Path) -> Option<DateTime<Utc>> {
    try_capture_date_with(path, &|| read_rrexif_sidecar(path))
}

pub fn get_creation_date_from_path(path: &Path) -> DateTime<Utc> {
    capture_date_with(path, &|| read_rrexif_sidecar(path))
}

pub fn read_exif_data(path: &str, file_bytes: &[u8]) -> HashMap<String, String> {
    let source_path = Path::new(path);

    if let Some(cached_exif) = read_rrexif_sidecar(source_path) {
        return cached_exif;
    }

    let exif_map = read_exif_data_from_bytes(path, file_bytes);
    if !exif_map.is_empty() {
        let primary = get_primary_sidecar_path(source_path);
        if primary.exists() {
            let mut metadata = load_primary_metadata(source_path);
            metadata.exif = Some(exif_map.clone());
            let _ = save_primary_metadata(source_path, &metadata);
        } else {
            save_exif_to_rrcache(source_path, exif_map.clone());
        }
    }

    exif_map
}

pub fn persist_exif_if_missing(source_path: &Path, source_path_str: &str, file_bytes: &[u8]) {
    if read_rrexif_sidecar(source_path).is_some() {
        return;
    }

    let exif_map = read_exif_data_from_bytes(source_path_str, file_bytes);
    if exif_map.is_empty() {
        return;
    }

    let primary = get_primary_sidecar_path(source_path);

    if primary.exists() {
        let mut metadata = load_primary_metadata(source_path);
        if metadata.exif.is_none() {
            metadata.exif = Some(exif_map);
            let _ = save_primary_metadata(source_path, &metadata);
        }
    } else {
        save_exif_to_rrcache(source_path, exif_map);
    }
}

pub fn write_rrexif_sidecar(source_path_str: &str, target_image_path: &Path) -> Result<(), String> {
    let source_path = Path::new(source_path_str);

    let exif_data = if let Some(existing) = read_rrexif_sidecar(source_path) {
        existing
    } else if let Ok(bytes) = fs::read(source_path) {
        read_exif_data_from_bytes(source_path_str, &bytes)
    } else {
        return Ok(());
    };

    if exif_data.is_empty() {
        return Ok(());
    }

    let mut metadata = load_primary_metadata(target_image_path);
    metadata.exif = Some(exif_data);
    save_primary_metadata(target_image_path, &metadata)
        .map_err(|e| format!("Failed to write sidecar: {}", e))
}

#[cfg(test)]
mod sidecar_write_tests {
    use super::*;

    #[test]
    fn primary_sidecar_is_replaced_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let photo = dir.path().join("photo.raf");
        fs::write(&photo, b"original").unwrap();
        let mut metadata = ImageMetadata {
            rating: 2,
            ..Default::default()
        };
        save_primary_metadata(&photo, &metadata).unwrap();
        metadata.rating = 4;
        save_primary_metadata(&photo, &metadata).unwrap();

        let sidecar = get_primary_sidecar_path(&photo);
        let saved: ImageMetadata =
            serde_json::from_str(&fs::read_to_string(&sidecar).unwrap()).unwrap();
        assert_eq!(saved.rating, 4);
        assert_eq!(fs::read(&photo).unwrap(), b"original");
        let mut names: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, ["photo.raf", "photo.raf.rrdata"]);
    }
}
