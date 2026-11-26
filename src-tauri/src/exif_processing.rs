//! Desktop glue for `rawtonic_export::exif_processing` (ADR 0009): editor
//! sidecars and the session catalog are read here (`session_bridge`), the
//! EXIF extraction and export metadata live in the crate.
pub use rawtonic_export::exif_processing::*;

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use crate::image_processing::ImageMetadata;
use jiff::Timestamp;
use rawtonic_export::sidecars::{forget_kept_sidecar, read_sidecar_file, read_sidecar_snapshot};

thread_local! {
    /// A sidecar a batch edit on this thread reads as moved to the full
    /// frame (`reading_conformed`).
    static CONFORMED: std::cell::RefCell<Option<(std::path::PathBuf, ImageMetadata)>> =
        const { std::cell::RefCell::new(None) };
}

/// Runs `edit` with `sidecar_path` read as `metadata` on this thread: a
/// camera TIFF's edit moved from its thumbnail to the full frame in memory
/// (`file_management::edits::edit_sidecars`), written once with the edit
/// and not at all when the edit is refused.
pub(crate) fn reading_conformed<T>(
    sidecar_path: &Path,
    metadata: ImageMetadata,
    edit: impl FnOnce() -> T,
) -> T {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            CONFORMED.with(|c| *c.borrow_mut() = None);
        }
    }
    CONFORMED.with(|c| *c.borrow_mut() = Some((sidecar_path.to_path_buf(), metadata)));
    let _reset = Reset;
    edit()
}

pub fn load_sidecar(sidecar_path: &Path) -> ImageMetadata {
    load_sidecar_updating(sidecar_path, |_| false)
}

/// Loads editor metadata and optionally incorporates loose-file XMP.
pub(crate) fn load_sidecar_updating(
    sidecar_path: &Path,
    update: impl FnOnce(&mut ImageMetadata) -> bool,
) -> ImageMetadata {
    if let Some(metadata) = CONFORMED.with(|c| {
        c.borrow()
            .as_ref()
            .filter(|(path, _)| path == sidecar_path)
            .map(|(_, metadata)| metadata.clone())
    }) {
        return metadata;
    }
    if let Some(value) = crate::session_bridge::read_editor_sidecar(sidecar_path)
        && let Ok(meta) = serde_json::from_value(value)
    {
        return meta;
    }
    load_sidecar_file_updating(sidecar_path, update)
}

/// Physical fallback after the caller has already resolved database authority.
pub(crate) fn load_sidecar_file(sidecar_path: &Path) -> ImageMetadata {
    load_sidecar_file_updating(sidecar_path, |_| false)
}

fn load_sidecar_file_updating(
    sidecar_path: &Path,
    update: impl FnOnce(&mut ImageMetadata) -> bool,
) -> ImageMetadata {
    let library = crate::photo_library::library_of(sidecar_path);
    let (mut meta, healed, snapshot) = read_sidecar_snapshot(sidecar_path, library.as_deref());
    let synchronized = update(&mut meta);
    if (healed || synchronized)
        && let Ok(text) = &snapshot
    {
        let expected = text.as_deref().map(str::as_bytes);
        // Healing affects only the legacy EXIF cache in this file. Keep
        // unknown fields and library resource references in their disk form.
        let serialize = || {
            if synchronized {
                return serde_json::to_vec_pretty(&meta);
            }
            let mut value: serde_json::Value =
                serde_json::from_slice(expected.unwrap_or_default())?;
            value["exif"] = serde_json::to_value(&meta.exif)?;
            serde_json::to_vec_pretty(&value)
        };
        if let Ok(json) = serialize() {
            let written = {
                let _turn = crate::session_bridge::write_turn();
                if rawtonic_metadata::photo_ref::source_path_of_sidecar(sidecar_path).is_some_and(
                    |source| crate::session_bridge::file_replaced_since_open(&source).is_some(),
                ) {
                    return meta;
                }
                rawtonic_fs::write_if_unchanged(sidecar_path, expected, &json)
            };
            forget_kept_sidecar(sidecar_path);
            if matches!(written, Ok(false)) {
                // The edit we read has been superseded. Return the current
                // state and give any remaining repair another turn next load.
                return crate::session_bridge::read_editor_sidecar(sidecar_path)
                    .and_then(|value| serde_json::from_value(value).ok())
                    .unwrap_or_else(|| read_sidecar_file(sidecar_path, library.as_deref()).0);
            }
        }
    }
    meta
}

/// The editor state of `sidecar_path` with the EXIF its photo shows in
/// `exif` (`shown_exif`), for reading; an edit writes what
/// `load_sidecar_for_edit` gives it.
pub fn load_sidecar_with_exif(sidecar_path: &Path, source_path: &Path) -> ImageMetadata {
    let (mut meta, shown) = load_sidecar_for_edit(sidecar_path, source_path);
    meta.exif = shown;
    meta.exif_overrides = None;
    meta
}

/// The editor state of `sidecar_path` as an edit writes it back (only its
/// own EXIF overrides once written, `keep_own_exif`) and the EXIF its photo
/// shows (`shown_exif`), which lens detection reads.
pub fn load_sidecar_for_edit(
    sidecar_path: &Path,
    source_path: &Path,
) -> (ImageMetadata, Option<HashMap<String, String>>) {
    let meta = load_sidecar(sidecar_path);
    let shown = shown_exif(source_path, &meta);
    (meta, shown)
}

/// The EXIF `source_path` shows with the editor state `meta`
/// (`stated_exif`), with the maker note's lens; without EXIF in the state
/// the photo's (`read_rrexif_sidecar`: a virtual copy's master's edits, the
/// EXIF cache) or the original's, read now. None when nothing can be read
/// (a cloud-only file is never opened for it).
pub fn shown_exif(source_path: &Path, meta: &ImageMetadata) -> Option<HashMap<String, String>> {
    if let Some(shown) = stated_exif(source_path, meta) {
        return Some(with_maker_note_lens(source_path, shown));
    }
    if let Some(cached_exif) = read_rrexif_sidecar(source_path) {
        return Some(with_maker_note_lens(source_path, cached_exif));
    }
    if crate::file_management::is_cloud_placeholder(source_path) {
        // An original that is only in the cloud is never opened for its
        // EXIF (macOS would download it): None, as for a missing file.
        return None;
    }
    let source_path_str = source_path.to_string_lossy();
    let extracted_exif = if let Ok(mmap) = crate::file_management::read_file_mapped(source_path) {
        read_exif_data_mapped(&source_path_str, mmap)
    } else if let Ok(bytes) = std::fs::read(source_path) {
        read_exif_data(&source_path_str, &bytes)
    } else {
        HashMap::new()
    };
    (!extracted_exif.is_empty()).then_some(extracted_exif)
}

/// What an editor state with EXIF in it shows: its older copy of the read
/// map, else the original's EXIF (`original_exif`; for an original that
/// cannot be read, what its previews kept), with its own overrides on it
/// (`with_exif_overrides`). None for a state without EXIF.
fn stated_exif(source_path: &Path, meta: &ImageMetadata) -> Option<HashMap<String, String>> {
    if meta.exif.is_none() && meta.exif_overrides.is_none() {
        return None;
    }
    let base = meta
        .exif
        .clone()
        .or_else(|| original_exif(source_path))
        .or_else(|| crate::preview_proxy::cached_camera(source_path).1)
        .unwrap_or_default();
    Some(match &meta.exif_overrides {
        Some(overrides) => with_exif_overrides(overrides, &base),
        None => base,
    })
}

/// The EXIF read from `source_path` itself: the app's EXIF cache, else the
/// file's metadata (cached then). Never an editor state's map; None for a
/// cloud-only or unreadable original.
pub(crate) fn original_exif(source_path: &Path) -> Option<HashMap<String, String>> {
    if let Some(cached) = get_exif_from_rrcache(source_path) {
        return Some(cached);
    }
    if crate::file_management::is_cloud_placeholder(source_path) {
        return None;
    }
    let mmap = crate::file_management::read_file_mapped(source_path).ok()?;
    let map = rawtonic_export::exif_processing::read_exif_data_from_shared(
        &source_path.to_string_lossy(),
        std::sync::Arc::new(mmap),
    );
    if !map.is_empty() {
        save_exif_to_rrcache(source_path, map.clone());
    }
    Some(map)
}

/// `metadata` (an editor state about to be written for `source_path`)
/// with only the user's own EXIF: an older copy of the read map
/// (`exif`) becomes the overrides it holds (`overrides_of_copy`, joined
/// with any it has already) and goes (SCW1). A copy is kept as it is
/// while the original cannot be read to tell them apart.
pub(crate) fn keep_own_exif(metadata: &mut ImageMetadata, source_path: &Path) {
    let Some(copy) = metadata.exif.take() else {
        return;
    };
    let Some(own) = original_exif(source_path) else {
        metadata.exif = Some(copy);
        return;
    };
    let mut overrides = overrides_of_copy(&copy, &own);
    overrides.extend(metadata.exif_overrides.take().unwrap_or_default());
    metadata.exif_overrides = (!overrides.is_empty()).then_some(overrides);
}

fn load_primary_metadata(image_path: &Path) -> ImageMetadata {
    let primary = get_primary_sidecar_path(image_path);
    load_sidecar(&primary)
}

/// Fill the EXIF cache in the physical sidecar of an image the app made
/// itself (`write_rrexif_sidecar`), leaving all edit fields and any EXIF
/// already saved there alone. A photograph's own sidecar never takes it:
/// reading EXIF keeps it in the app's cache (`save_exif_to_rrcache`), so a
/// sidecar beside an original changes only with an edit (WQA7: the cache
/// grew a rated photo's sidecar from 72 B to 12 KB).
fn cache_primary_exif(image_path: &Path, exif: &HashMap<String, String>) -> anyhow::Result<bool> {
    let primary = get_primary_sidecar_path(image_path);
    let snapshot = rawtonic_fs::read_optional(&primary)?;
    let mut state: serde_json::Value = match &snapshot {
        Some(bytes) => serde_json::from_slice(bytes)?,
        None => serde_json::to_value(ImageMetadata::default())?,
    };
    let Some(fields) = state.as_object_mut() else {
        return Ok(false);
    };
    if fields.get("exif").is_some_and(serde_json::Value::is_object)
        || fields.contains_key("exifOverrides")
    {
        return Ok(true);
    }
    fields.insert("exif".into(), serde_json::to_value(exif)?);
    let bytes = serde_json::to_vec_pretty(&state)?;
    let _turn = crate::session_bridge::write_turn();
    forget_kept_sidecar(&primary);
    rawtonic_fs::write_if_unchanged(&primary, snapshot.as_deref(), &bytes)
}

pub fn read_rrexif_sidecar(image_path: &Path) -> Option<HashMap<String, String>> {
    let primary = get_primary_sidecar_path(image_path);

    if primary.exists() {
        let metadata = load_primary_metadata(image_path);
        if let Some(shown) = stated_exif(image_path, &metadata) {
            return Some(shown);
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

pub fn try_get_exif_creation_date(path: &Path) -> Option<Timestamp> {
    try_capture_date_with(path, &|| read_rrexif_sidecar(path))
}

pub fn get_creation_date_from_path(path: &Path) -> Timestamp {
    capture_date_with(path, &|| read_rrexif_sidecar(path))
}

pub fn read_exif_data(path: &str, file_bytes: &[u8]) -> HashMap<String, String> {
    cached_exif_data(path, || read_exif_data_from_bytes(path, file_bytes))
}

/// `read_exif_data` of a mapped file, read in place
/// (`read_exif_data_from_shared`): a cold RAW is read only where its
/// metadata lies, not in full.
pub fn read_exif_data_mapped(path: &str, file: memmap2::Mmap) -> HashMap<String, String> {
    cached_exif_data(path, || {
        rawtonic_export::exif_processing::read_exif_data_from_shared(
            path,
            std::sync::Arc::new(file),
        )
    })
}

/// An EXIF map cached (sidecar, EXIF cache) before the maker note's lens
/// was read with it, of a RAW whose EXIF lens name does not identify the
/// lens: the maker note's lens added now, so lens detection sees it
/// (`needs_maker_note_lens`). Only the maker note is read; nothing is
/// written back, and a cloud-only original is not opened.
fn with_maker_note_lens(
    source_path: &Path,
    mut exif: HashMap<String, String>,
) -> HashMap<String, String> {
    if !rawtonic_export::exif_processing::needs_maker_note_lens(
        &source_path.to_string_lossy(),
        &exif,
    ) || crate::file_management::is_cloud_placeholder(source_path)
    {
        return exif;
    }
    if let Ok(mmap) = crate::file_management::read_file_mapped(source_path) {
        rawtonic_export::exif_processing::add_maker_note_lens_from_shared(
            &mut exif,
            std::sync::Arc::new(mmap),
        );
    }
    exif
}

fn cached_exif_data(
    path: &str,
    read: impl FnOnce() -> HashMap<String, String>,
) -> HashMap<String, String> {
    let source_path = Path::new(path);

    if let Some(cached_exif) = read_rrexif_sidecar(source_path) {
        return with_maker_note_lens(source_path, cached_exif);
    }

    let exif_map = read();
    if !exif_map.is_empty() {
        save_exif_to_rrcache(source_path, exif_map.clone());
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

    save_exif_to_rrcache(source_path, exif_map);
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

    cache_primary_exif(target_image_path, &exif_data)
        .map(|_| ())
        .map_err(|e| format!("Failed to write sidecar: {}", e))
}

#[cfg(test)]
mod sidecar_write_tests {
    use super::*;

    fn legacy_sidecar(path: &Path) {
        let legacy = serde_json::json!({
            "version": 1, "rating": 2, "adjustments": {"exposure": 1},
            "exif": {"Comment": "x".repeat(850)}, "unknown": {"future": true}
        });
        rawtonic_fs::atomic_write(path, legacy.to_string()).unwrap();
    }

    #[test]
    fn sidecar_heal_keeps_a_save_committed_after_the_read() {
        let dir = tempfile::tempdir().unwrap();
        let sidecar = dir.path().join("photo.raf.rrdata");
        legacy_sidecar(&sidecar);
        let newer = br#"{"version":1,"rating":5,"adjustments":{"exposure":2}}"#;

        let loaded = load_sidecar_file_updating(&sidecar, |_| {
            crate::session_bridge::write_editor_sidecar(&sidecar, newer).unwrap();
            false
        });

        assert_eq!(fs::read(&sidecar).unwrap(), newer);
        assert_eq!(loaded.rating, 5);
    }

    #[test]
    fn sidecar_heal_shortens_exif_when_the_file_is_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let sidecar = dir.path().join("photo.raf.rrdata");
        legacy_sidecar(&sidecar);

        rawtonic_export::sidecars::with_sidecars_kept(|| {
            let read = || rawtonic_export::sidecars::read_sidecar_text(&sidecar).unwrap();
            let before = read();
            load_sidecar_file(&sidecar);
            let after = read();
            assert_ne!(before, after);
            assert_eq!(&*after, fs::read_to_string(&sidecar).unwrap());
        });

        let saved: serde_json::Value =
            serde_json::from_slice(&fs::read(&sidecar).unwrap()).unwrap();
        assert_eq!(saved["rating"], 2);
        assert_eq!(saved["adjustments"]["exposure"], 1);
        assert_eq!(saved["unknown"], serde_json::json!({"future": true}));
        assert!(saved["exif"]["Comment"].as_str().unwrap().len() <= 500);
    }

    /// WQA7: reading a rated photo's EXIF wrote the whole map (maker note,
    /// the RAW's own XMP packet) into its sidecar beside the original.
    #[test]
    fn reading_exif_leaves_the_photos_sidecar_as_it_was() {
        let dir = tempfile::tempdir().unwrap();
        let photo = dir.path().join("photo.nef");
        fs::write(&photo, b"original").unwrap();
        let sidecar = get_primary_sidecar_path(&photo);
        let rated = br#"{"version":1,"rating":3,"adjustments":null,"tags":null}"#;
        rawtonic_fs::atomic_write(&sidecar, rated).unwrap();
        let exif = HashMap::from([("MakerNote".to_string(), "0x4e696b6f6e".repeat(400))]);

        let path = photo.to_string_lossy();
        assert_eq!(cached_exif_data(&path, || exif.clone()), exif);
        // Read again from the app's cache, not from the file.
        assert_eq!(cached_exif_data(&path, || unreachable!("read twice")), exif);

        assert_eq!(fs::read(&sidecar).unwrap(), rated);
        assert_eq!(fs::read(&photo).unwrap(), b"original");
    }

    #[test]
    fn exif_cache_preserves_edit_fields_in_the_primary_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        let photo = dir.path().join("photo.raf");
        fs::write(&photo, b"original").unwrap();
        let sidecar = get_primary_sidecar_path(&photo);
        rawtonic_fs::atomic_write(&sidecar, br#"{"rating":4,"extra":{"kept":true}}"#).unwrap();
        let exif = HashMap::from([("Model".into(), "Camera".into())]);
        assert!(cache_primary_exif(&photo, &exif).unwrap());

        let saved: serde_json::Value =
            serde_json::from_slice(&fs::read(&sidecar).unwrap()).unwrap();
        assert_eq!(
            saved,
            serde_json::json!({
                "rating": 4, "extra": {"kept": true}, "exif": {"Model": "Camera"}
            })
        );
        assert_eq!(fs::read(&photo).unwrap(), b"original");
        let mut names: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, ["photo.raf", "photo.raf.rrdata"]);
    }
}
