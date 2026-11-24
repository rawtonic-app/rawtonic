//! The app's editor sidecars and virtual copies, read without the app: a
//! source's recipe lives in `<file>.rrdata` (or, for photos in a session,
//! the session database, which the caller reads); a virtual copy is
//! `<file>.<6 hex digits>.rrdata` and is addressed as `<file>?vc=<id>`.
//! Export planning and naming here are shared by the app's export queue and
//! `rawtonic export`.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use walkdir::WalkDir;

use crate::exif_processing::truncate_large_exif;
use rawtonic_engine::formats::is_supported_image_file;
use rawtonic_engine::image_processing::ImageMetadata;
use rawtonic_metadata::photo_ref::{self, PhotoRef};

/// The source file and editor sidecar of a (possibly virtual) photo path.
pub fn parse_virtual_path(virtual_path: &str) -> (PathBuf, PathBuf) {
    let photo = PhotoRef::parse(virtual_path);
    (photo.source_path().to_path_buf(), photo.sidecar_path())
}

/// The address of a source's primary photo or of one of its virtual copies.
pub fn virtual_path(source: &Path, copy_id: Option<&str>) -> String {
    PhotoRef::of_path(source, copy_id).format()
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
        if let Some((source_filename, copy_id)) = photo_ref::sidecar_name_parts(&file_name) {
            let copy_id = copy_id.map(str::to_string);

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

/// A sidecar's exact text. A missing or unreadable file can be shown as
/// default metadata, but only an absent file can be created automatically.
pub type SidecarText = Result<Option<Arc<str>>, io::ErrorKind>;

thread_local! {
    /// Sidecar texts read inside `with_sidecars_kept` on this thread (None:
    /// no scope), including reads that found no readable text.
    static KEPT: std::cell::RefCell<Option<HashMap<PathBuf, SidecarText>>> =
        const { std::cell::RefCell::new(None) };
}

/// Runs `f` with each sidecar read at most once on this thread: one job
/// (a Library tile's lookup or render) asks for its photo's and its
/// master's recipe several times, each a read over a network share
/// (PXV2/CRD6: ~19 ms per read on a LAN share). Nested scopes share the
/// outer one; nothing outlives the outermost scope, also on a panic.
pub fn with_sidecars_kept<R>(f: impl FnOnce() -> R) -> R {
    struct Scope(bool);
    impl Drop for Scope {
        fn drop(&mut self) {
            if self.0 {
                KEPT.with(|kept| *kept.borrow_mut() = None);
            }
        }
    }
    let outer = KEPT.with(|kept| {
        let mut kept = kept.borrow_mut();
        let outer = kept.is_none();
        if outer {
            *kept = Some(HashMap::new());
        }
        outer
    });
    let _scope = Scope(outer);
    f()
}

/// The text of the sidecar at `path` (None when missing or unreadable),
/// read once per `with_sidecars_kept` scope.
pub fn read_sidecar_text(path: &Path) -> Option<Arc<str>> {
    read_text(path).ok().flatten()
}

fn read_text(path: &Path) -> SidecarText {
    if let Some(kept) = KEPT.with(|kept| kept.borrow().as_ref().and_then(|m| m.get(path).cloned()))
    {
        return kept;
    }
    let text = match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(Arc::from(text))),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.kind()),
    };
    KEPT.with(|kept| {
        if let Some(map) = kept.borrow_mut().as_mut() {
            map.insert(path.to_path_buf(), text.clone());
        }
    });
    text
}

/// Forgets a kept sidecar text after the app wrote that sidecar, so the
/// rest of the scope reads what was written.
pub fn forget_kept_sidecar(path: &Path) {
    KEPT.with(|kept| {
        if let Some(map) = kept.borrow_mut().as_mut() {
            map.remove(path);
        }
    });
}

/// A `.rrdata` sidecar's metadata (defaults when it is missing or not JSON;
/// a field of another type or a missing one alone takes its default,
/// `metadata_fields`) and whether over-long EXIF values were shortened,
/// which the app writes back. `library`: the library folder the photo
/// belongs to, against which its recipe's library resources
/// (`rawtonic-resource:…`) resolve; None for a photo of no library.
pub fn read_sidecar_file(sidecar_path: &Path, library: Option<&Path>) -> (ImageMetadata, bool) {
    let (metadata, healed, _) = read_sidecar_snapshot(sidecar_path, library);
    (metadata, healed)
}

/// Returns the text the metadata was parsed from for a compare-and-swap
/// rewrite. The write must still check that this text is current on disk.
pub fn read_sidecar_snapshot(
    sidecar_path: &Path,
    library: Option<&Path>,
) -> (ImageMetadata, bool, SidecarText) {
    let kept = is_kept(sidecar_path);
    let read = read_sidecar_parsed(sidecar_path, library);
    // What the caller writes back must be the sidecar as it is now, not a
    // text this thread kept while another saved the photo: read it again.
    if read.1 && kept {
        forget_kept_sidecar(sidecar_path);
        return read_sidecar_parsed(sidecar_path, library);
    }
    read
}

fn is_kept(path: &Path) -> bool {
    KEPT.with(|kept| {
        kept.borrow()
            .as_ref()
            .is_some_and(|map| map.contains_key(path))
    })
}

fn read_sidecar_parsed(
    sidecar_path: &Path,
    library: Option<&Path>,
) -> (ImageMetadata, bool, SidecarText) {
    let text = read_text(sidecar_path);
    let Ok(Some(content)) = &text else {
        return (ImageMetadata::default(), false, text);
    };

    let mut value = serde_json::from_str::<serde_json::Value>(content).unwrap_or_default();
    if let Some(root) = library {
        rawtonic_metadata::resources::resolve(&mut value, root);
    }
    let mut meta = <ImageMetadata as serde::Deserialize>::deserialize(&value)
        .unwrap_or_else(|_| metadata_fields(&value));
    let mut healed = false;

    if let Some(ref mut exif_map) = meta.exif {
        for val in exif_map.values_mut() {
            if val.len() > 500 {
                *val = truncate_large_exif(val);
                healed = true;
            }
        }
    }
    (meta, healed, text)
}

/// The metadata of a sidecar the typed reader does not take whole (one
/// written by hand or by another program: no `adjustments`, a keyword that
/// is no string, a number in the EXIF): each field read on its own, so a
/// later write (a rating, a label, a recipe) keeps what the others hold
/// instead of writing every field's default. Keywords and EXIF values keep
/// their strings.
fn metadata_fields(value: &serde_json::Value) -> ImageMetadata {
    use serde_json::Value;
    let field = |key: &str| value.get(key).filter(|v| !v.is_null());
    let strings = |key: &str| {
        field(key).and_then(Value::as_array).map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
    };
    let default = ImageMetadata::default();
    ImageMetadata {
        version: field("version")
            .and_then(Value::as_u64)
            .and_then(|v| u32::try_from(v).ok())
            .unwrap_or(default.version),
        rating: field("rating")
            .and_then(Value::as_u64)
            .and_then(|v| u8::try_from(v).ok())
            .unwrap_or(default.rating),
        adjustments: value.get("adjustments").cloned().unwrap_or(Value::Null),
        tags: strings("tags"),
        exif: field("exif").and_then(Value::as_object).map(|map| {
            map.iter()
                .filter_map(|(key, v)| Some((key.clone(), v.as_str()?.to_string())))
                .collect()
        }),
        snapshots: field("snapshots")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
        snapshot_strings: field("snapshotStrings")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default(),
    }
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

        // The copy number is read from the file name only (a folder such as
        // `Shoot_VC2024` is not one).
        let name = &path_str[path_str.rfind(['/', '\\']).map_or(0, |i| i + 1)..];
        let mut explicit_vc = None;
        if let Some(idx) = name.rfind("vc=") {
            let id_str = name[idx + 3..].split('&').next().unwrap_or("");
            if let Ok(id) = id_str.parse::<u32>() {
                explicit_vc = Some(id);
            }
        }
        if explicit_vc.is_none() {
            let lower = name.to_lowercase();
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

/// An export file's stem: `filename_template` (naming tokens) filled from
/// `naming` (`ExportSettings::naming_context`), then `_VCnn` for a virtual
/// copy (its number in the path, else its appearance). A template its
/// tokens leave empty for this photo gives the original's name
/// (`render_or_original`). Fails on a template the naming tokens refuse or
/// a name no file can have.
pub fn export_stem(
    filename_template: &str,
    naming: &rawtonic_naming::Context,
    appearance_count: usize,
    explicit_vc: Option<u32>,
) -> Result<String, String> {
    let mut new_stem = rawtonic_naming::render_or_original(filename_template, naming)
        .map_err(|e| format!("The file name template '{filename_template}' cannot be used: {e}"))?;

    if let Some(vc_id) = explicit_vc {
        new_stem = format!("{}_VC{:02}", new_stem, vc_id);
    } else if appearance_count > 1 {
        new_stem = format!("{}_VC{:02}", new_stem, appearance_count - 1);
    }
    Ok(new_stem)
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
            "/p/Shoot_VC2024/c.raf".into(),
            "/p/c_vc3.raf".into(),
        ]);
        let date = jiff::Timestamp::UNIX_EPOCH;
        let stems: Vec<String> = items
            .iter()
            .map(|(i, photo, appearance, vc)| {
                let (path, _) = parse_virtual_path(photo);
                let settings: crate::export_processing::ExportSettings =
                    serde_json::from_value(serde_json::json!({
                        "jpegQuality": 90, "keepMetadata": true, "stripGps": false,
                    }))
                    .unwrap();
                let naming = settings.naming_context(&path, i + 1, 6, &date, "jpg", None);
                export_stem(&settings.naming_template(), &naming, *appearance, *vc).unwrap()
            })
            .collect();
        assert_eq!(
            stems,
            [
                "a_edited",
                "a_edited_VC01",
                "a_edited_VC123456",
                "b_edited",
                "c_edited",
                "c_vc3_edited_VC03"
            ]
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
        let (meta, healed) = read_sidecar_file(&sidecar, None);
        assert!(healed);
        assert_eq!(meta.rating, 3);
        assert_eq!(meta.adjustments["exposure"], 1.0);
        assert_eq!(std::fs::read_to_string(&sidecar).unwrap(), json);
        assert_eq!(
            read_sidecar_file(&dir.path().join("missing.rrdata"), None)
                .0
                .rating,
            0
        );
    }

    #[test]
    fn copied_catalog_sidecars_resolve_resources_from_their_current_folder() {
        let dir = tempfile::tempdir().unwrap();
        let photos = dir.path().join("Photos");
        std::fs::create_dir(&photos).unwrap();
        let sidecar = photos.join("a.arw.abcdef.rrdata");
        let state = r#"{"version":1,"rating":4,"adjustments":{"lutPath":"rawtonic-resource:resources/look.cube","cameraProfile":"rawtonic-resource:resources/camera.dcp"}}"#;
        std::fs::write(&sidecar, state).unwrap();
        let (meta, healed) = read_sidecar_file(&sidecar, Some(dir.path()));
        assert!(!healed);
        assert_eq!(meta.rating, 4);
        assert_eq!(
            meta.adjustments["lutPath"],
            dir.path()
                .join("resources/look.cube")
                .to_string_lossy()
                .as_ref()
        );
        assert_eq!(
            meta.adjustments["cameraProfile"],
            dir.path()
                .join("resources/camera.dcp")
                .to_string_lossy()
                .as_ref()
        );
        assert_eq!(std::fs::read_to_string(sidecar).unwrap(), state);
    }

    #[test]
    fn sidecars_of_photos_a_session_keeps_outside_resolve_its_resources() {
        let dir = tempfile::tempdir().unwrap();
        let session = dir.path().join("Job");
        let drive = dir.path().join("Drive");
        std::fs::create_dir(&drive).unwrap();
        let photo = drive.join("a.nef");
        std::fs::write(&photo, b"x").unwrap();
        let sidecar = drive.join("a.nef.rrdata");
        let state = r#"{"version":1,"rating":0,"adjustments":{"lutPath":"rawtonic-resource:resources/look.cube","masks":[{"raster_path":"rawtonic-resource:resources/m.png"}]}}"#;
        std::fs::write(&sidecar, state).unwrap();
        // A photo of no library keeps the names as stored.
        let (loose, _) = read_sidecar_file(&sidecar, None);
        assert_eq!(
            loose.adjustments["lutPath"],
            "rawtonic-resource:resources/look.cube"
        );
        // The session that keeps it outside its folder resolves them, not
        // the folder the file is in.
        let (meta, _) = read_sidecar_file(&sidecar, Some(&session));
        let at = |p: &str| session.join(p).to_string_lossy().into_owned();
        assert_eq!(meta.adjustments["lutPath"], at("resources/look.cube"));
        assert_eq!(
            meta.adjustments["masks"][0]["raster_path"],
            at("resources/m.png")
        );
        assert_eq!(std::fs::read_to_string(sidecar).unwrap(), state);
    }
}

#[cfg(test)]
mod kept_sidecar_tests {
    use super::*;

    #[test]
    fn a_scope_reads_each_sidecar_once_and_sees_its_own_writes() {
        let dir = tempfile::tempdir().unwrap();
        let sidecar = dir.path().join("a.ARW.rrdata");
        std::fs::write(&sidecar, "{\"rating\":1}").unwrap();
        with_sidecars_kept(|| {
            assert_eq!(
                read_sidecar_text(&sidecar).as_deref(),
                Some("{\"rating\":1}")
            );
            std::fs::write(&sidecar, "{\"rating\":2}").unwrap();
            // Nested scopes share the outer one: still the first read.
            with_sidecars_kept(|| {
                assert_eq!(
                    read_sidecar_text(&sidecar).as_deref(),
                    Some("{\"rating\":1}")
                )
            });
            forget_kept_sidecar(&sidecar);
            assert_eq!(
                read_sidecar_text(&sidecar).as_deref(),
                Some("{\"rating\":2}")
            );
        });
        std::fs::write(&sidecar, "{\"rating\":3}").unwrap();
        // Nothing outlives the scope.
        assert_eq!(
            read_sidecar_text(&sidecar).as_deref(),
            Some("{\"rating\":3}")
        );
        assert_eq!(read_sidecar_text(&dir.path().join("missing.rrdata")), None);
    }

    #[test]
    fn a_sidecar_to_heal_is_read_again_not_taken_from_the_scope() {
        let dir = tempfile::tempdir().unwrap();
        let sidecar = dir.path().join("b.ARW.rrdata");
        let long = "x".repeat(600);
        let with = |rating: u8| {
            serde_json::json!({ "rating": rating, "exif": { "Long": long } }).to_string()
        };
        std::fs::write(&sidecar, with(1)).unwrap();
        with_sidecars_kept(|| {
            assert!(read_sidecar_text(&sidecar).is_some());
            // Saved on another thread meanwhile (its forget does not reach
            // this thread's scope).
            std::fs::write(&sidecar, with(5)).unwrap();
            let (meta, healed) = read_sidecar_file(&sidecar, None);
            assert!(healed);
            // What the caller writes back is the save, not the kept text.
            assert_eq!(meta.rating, 5);
        });
    }
}
