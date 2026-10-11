//! The app's editor sidecars and virtual copies, read without the app: a
//! source's recipe lives in `<file>.rrdata` (or, for photos in a session,
//! the session database, which the caller reads); a virtual copy is
//! `<file>.<6 hex digits>.rrdata` and is addressed as `<file>?vc=<id>`.
//! Export planning and naming here are shared by the app's export queue and
//! `rawtonic export`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use walkdir::WalkDir;

use crate::exif_processing::truncate_large_exif;
use rawtonic_engine::formats::is_supported_image_file;
use rawtonic_engine::image_processing::ImageMetadata;

/// The source file and editor sidecar of a (possibly virtual) photo path.
pub fn parse_virtual_path(virtual_path: &str) -> (PathBuf, PathBuf) {
    let (source_path_str, copy_id) = if let Some((base, id)) = virtual_path.rsplit_once("?vc=") {
        (base.to_string(), Some(id.to_string()))
    } else {
        (virtual_path.to_string(), None)
    };

    let source_path = PathBuf::from(source_path_str);

    let sidecar_filename = if let Some(id) = copy_id {
        format!(
            "{}.{}.rrdata",
            source_path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy(),
            id
        )
    } else {
        format!(
            "{}.rrdata",
            source_path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
        )
    };

    let sidecar_path = source_path.with_file_name(sidecar_filename);
    (source_path, sidecar_path)
}

/// The address of a source's primary photo or of one of its virtual copies.
pub fn virtual_path(source: &Path, copy_id: Option<&str>) -> String {
    let path = source.to_string_lossy();
    match copy_id {
        Some(id) => format!("{}?vc={}", path, id),
        None => path.into_owned(),
    }
}

/// Supported images below `root` with the sidecars found next to them
/// (`None` = the primary sidecar, `Some(id)` = a virtual copy), in the
/// directory walk's order as the app's library lists them. An image
/// without sidecars counts as its primary photo.
pub fn sources_with_copies(root: &Path) -> Vec<(PathBuf, Vec<Option<String>>)> {
    let root_path = root;
    let mut images = Vec::new();

    let mut sidecars_by_path: HashMap<PathBuf, Vec<Option<String>>> = HashMap::new();

    for entry in WalkDir::new(root_path).into_iter().filter_map(Result::ok) {
        let entry_path = entry.path();
        if !entry_path.is_file() {
            continue;
        }

        let file_name = entry_path.file_name().unwrap_or_default().to_string_lossy();
        if let Some(base) = file_name.strip_suffix(".rrdata") {
            let (source_filename, copy_id) =
                if base.len() >= 7 && base.as_bytes()[base.len() - 7] == b'.' {
                    let id = &base[base.len() - 6..];
                    if id.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')) {
                        (&base[..base.len() - 7], Some(id.to_string()))
                    } else {
                        (base, None)
                    }
                } else {
                    (base, None)
                };

            if let Some(parent) = entry_path.parent() {
                sidecars_by_path
                    .entry(parent.join(source_filename))
                    .or_default()
                    .push(copy_id);
            }
        } else if is_supported_image_file(entry_path.to_string_lossy().as_ref()) {
            images.push(entry_path.to_path_buf());
        }
    }

    images
        .into_iter()
        .map(|image| {
            let sidecars = with_original(sidecars_by_path.remove(&image).unwrap_or_default());
            (image, sidecars)
        })
        .collect()
}

/// The sidecars a photograph is listed by: its original always (`None`,
/// also when it has no sidecar of its own yet, e.g. an unedited photograph
/// with a virtual copy), then its virtual copies.
pub fn with_original(mut sidecars: Vec<Option<String>>) -> Vec<Option<String>> {
    sidecars.retain(Option::is_some);
    sidecars.insert(0, None);
    sidecars
}

/// A `.rrdata` sidecar's metadata (defaults when it is missing or invalid)
/// and whether over-long EXIF values were shortened, which the app writes
/// back.
pub fn read_sidecar_file(sidecar_path: &Path) -> (ImageMetadata, bool) {
    if !sidecar_path.exists() {
        return (ImageMetadata::default(), false);
    }

    let Ok(content) = std::fs::read_to_string(sidecar_path) else {
        return (ImageMetadata::default(), false);
    };

    let mut meta = serde_json::from_str::<ImageMetadata>(&content).unwrap_or_default();
    let mut healed = false;

    if let Some(ref mut exif_map) = meta.exif {
        for val in exif_map.values_mut() {
            if val.len() > 500 {
                *val = truncate_large_exif(val);
                healed = true;
            }
        }
    }
    (meta, healed)
}

/// The export queue's items: (index, photo path, how often its source has
/// appeared so far, a virtual-copy number written in the path).
pub fn plan_export_items(paths: Vec<String>) -> Vec<(usize, String, usize, Option<u32>)> {
    let mut base_path_counts: HashMap<String, usize> = HashMap::new();
    let total_paths = paths.len();
    let mut export_items = Vec::with_capacity(total_paths);

    for (i, path_str) in paths.into_iter().enumerate() {
        let (source_path, _) = parse_virtual_path(&path_str);
        let source_str = source_path.to_string_lossy().to_string();
        let count = base_path_counts.entry(source_str.clone()).or_insert(0);
        *count += 1;

        let mut explicit_vc = None;
        if let Some(idx) = path_str.rfind("vc=") {
            let id_str = path_str[idx + 3..].split('&').next().unwrap_or("");
            if let Ok(id) = id_str.parse::<u32>() {
                explicit_vc = Some(id);
            }
        }
        if explicit_vc.is_none() {
            let lower = path_str.to_lowercase();
            if let Some(idx) = lower.rfind("_vc") {
                let id_str: String = lower[idx + 3..]
                    .chars()
                    .take_while(|c| c.is_ascii_digit())
                    .collect();
                if let Ok(id) = id_str.parse::<u32>() {
                    explicit_vc = Some(id);
                }
            }
        }
        export_items.push((i, path_str, *count, explicit_vc));
    }
    export_items
}

/// An export file's stem: the template applied to the source, then `_VCnn`
/// for a virtual copy (its number in the path, else its appearance).
pub fn export_stem(
    filename_template: &str,
    original_path: &Path,
    sequence: usize,
    total_paths: usize,
    file_date: &chrono::DateTime<chrono::Utc>,
    appearance_count: usize,
    explicit_vc: Option<u32>,
) -> String {
    let mut new_stem = crate::export_processing::generate_filename_from_template(
        filename_template,
        original_path,
        sequence,
        total_paths,
        file_date,
    );

    if let Some(vc_id) = explicit_vc {
        new_stem = format!("{}_VC{:02}", new_stem, vc_id);
    } else if appearance_count > 1 {
        new_stem = format!("{}_VC{:02}", new_stem, appearance_count - 1);
    }
    new_stem
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn virtual_copies_are_found_next_to_their_source() {
        let dir = tempfile::tempdir().unwrap();
        let raw = dir.path().join("a.raf");
        std::fs::write(&raw, b"x").unwrap();
        std::fs::write(dir.path().join("a.raf.rrdata"), "{}").unwrap();
        std::fs::write(dir.path().join("a.raf.0f1e2d.rrdata"), "{}").unwrap();
        // Not a copy id (upper case, wrong length): read as other files' sidecars.
        std::fs::write(dir.path().join("a.raf.ABCDEF.rrdata"), "{}").unwrap();
        std::fs::write(dir.path().join("b.jpg"), b"x").unwrap();
        std::fs::write(dir.path().join("notes.txt"), b"x").unwrap();

        let mut found = sources_with_copies(dir.path());
        found.sort();
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].0, raw);
        let mut copies = found[0].1.clone();
        copies.sort();
        assert_eq!(copies, [None, Some("0f1e2d".to_string())]);
        assert_eq!(found[1].1, [None]);

        let (source, sidecar) = parse_virtual_path(&virtual_path(&raw, Some("0f1e2d")));
        assert_eq!(source, raw);
        assert_eq!(sidecar, dir.path().join("a.raf.0f1e2d.rrdata"));

        // An unedited photograph with a virtual copy: listed itself too.
        std::fs::write(dir.path().join("b.jpg.123abc.rrdata"), "{}").unwrap();
        let mut found = sources_with_copies(dir.path());
        found.sort();
        assert_eq!(found[1].1, [None, Some("123abc".to_string())]);
    }

    #[test]
    fn copies_are_numbered_as_the_app_names_them() {
        let items = plan_export_items(vec![
            "/p/a.raf".into(),
            "/p/a.raf?vc=0f1e2d".into(),
            "/p/a.raf?vc=123456".into(),
            "/p/b.raf".into(),
        ]);
        let date = chrono::DateTime::<chrono::Utc>::from_timestamp(0, 0).unwrap();
        let stems: Vec<String> = items
            .iter()
            .map(|(i, photo, appearance, vc)| {
                let (path, _) = parse_virtual_path(photo);
                export_stem(
                    "{original_filename}_edited",
                    &path,
                    i + 1,
                    4,
                    &date,
                    *appearance,
                    *vc,
                )
            })
            .collect();
        assert_eq!(
            stems,
            ["a_edited", "a_edited_VC01", "a_edited_VC123456", "b_edited"]
        );
    }

    #[test]
    fn sidecar_files_read_without_writing() {
        let dir = tempfile::tempdir().unwrap();
        let sidecar = dir.path().join("a.raf.rrdata");
        let long = "x".repeat(600);
        let json = format!(
            r#"{{"version":1,"rating":3,"adjustments":{{"exposure":1.0}},"exif":{{"Note":"{long}"}}}}"#
        );
        std::fs::write(&sidecar, &json).unwrap();
        let (meta, healed) = read_sidecar_file(&sidecar);
        assert!(healed);
        assert_eq!(meta.rating, 3);
        assert_eq!(meta.adjustments["exposure"], 1.0);
        assert_eq!(std::fs::read_to_string(&sidecar).unwrap(), json);
        assert_eq!(
            read_sidecar_file(&dir.path().join("missing.rrdata"))
                .0
                .rating,
            0
        );
    }
}
