//! The session bar's XMP review: which photographs' XMP sidecars are
//! pending or in conflict, "Sync now" for the pending ones, and resolving
//! conflicts for the library ("Keep library") or for the sidecar ("Use
//! XMP", undoable). The catalog does the work (`rawtonic_catalog::xmp_review`)
//! in a blocking worker under the library's write lock; originals are never
//! touched.

use crate::session_bridge::with_library;
use rawtonic_catalog::{Library, XmpOffline, XmpState, XmpUndo};
use rawtonic_metadata::XmpConflict;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};

/// A photograph in the review, with its virtual copies (the grid lists them too).
#[derive(Debug, Clone, Serialize)]
pub struct XmpReviewEntry {
    pub path: String,
    pub state: XmpState,
    pub offline: bool,
    pub volume: Option<String>,
    pub copies: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct XmpFailure {
    pub path: String,
    pub reason: String,
}

/// What "Sync now" or resolving did, by photograph path.
#[derive(Debug, Clone, Default, Serialize)]
pub struct XmpReport {
    /// Written (or read, for "Use XMP") and in sync now.
    pub done: Vec<String>,
    /// Offline photographs: still pending, written when they are back.
    pub offline: Vec<String>,
    /// Their sidecar changed outside Rawtonic: left for review.
    pub conflicts: Vec<String>,
    pub failed: Vec<XmpFailure>,
    /// For "Use XMP": what puts the library's metadata back.
    pub undo: Vec<XmpUndo>,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum XmpChoice {
    /// The library's metadata replaces the sidecar's.
    Library,
    /// The sidecar's metadata is read into the library.
    Xmp,
}

pub fn review(lib: &Library) -> anyhow::Result<Vec<XmpReviewEntry>> {
    lib.xmp_unsynced()?
        .into_iter()
        .map(|entry| {
            Ok(XmpReviewEntry {
                copies: lib.copies(&entry.id)?,
                path: entry.path.to_string_lossy().into_owned(),
                state: entry.state,
                offline: entry.offline,
                volume: entry.volume,
            })
        })
        .collect()
}

impl XmpReport {
    fn note(&mut self, path: String, result: anyhow::Result<()>) {
        match result {
            Ok(()) => self.done.push(path),
            Err(e) if e.is::<XmpOffline>() => self.offline.push(path),
            Err(e) if e.is::<XmpConflict>() => self.conflicts.push(path),
            Err(e) => self.failed.push(XmpFailure {
                path,
                reason: e.to_string(),
            }),
        }
    }
}

/// Writes every pending sidecar; conflicts are left for review.
pub fn sync_pending(lib: &Library) -> anyhow::Result<XmpReport> {
    let mut report = XmpReport::default();
    for entry in lib.xmp_unsynced()? {
        let path = entry.path.to_string_lossy().into_owned();
        match entry.state {
            XmpState::Conflict => report.conflicts.push(path),
            XmpState::Pending => {
                let result = lib.sync_xmp(&entry.id);
                report.note(path, result);
            }
        }
    }
    Ok(report)
}

/// Resolves the conflicts of `paths` (library paths; virtual copies count
/// as their photograph) one way. Photographs not in conflict are skipped.
pub fn resolve(lib: &Library, paths: &[String], keep: XmpChoice) -> anyhow::Result<XmpReport> {
    let conflicts: HashMap<String, _> = lib
        .xmp_unsynced()?
        .into_iter()
        .filter(|entry| entry.state == XmpState::Conflict)
        .map(|entry| (entry.id.clone(), entry))
        .collect();
    let mut seen = BTreeSet::new();
    let mut report = XmpReport::default();
    for path in paths {
        let source = crate::file_management::parse_virtual_path(path).0;
        let Some(asset) = lib.asset_at(&source)? else {
            continue;
        };
        if !seen.insert(asset.id.clone()) || !conflicts.contains_key(&asset.id) {
            continue;
        }
        let shown = source.to_string_lossy().into_owned();
        let result = match keep {
            XmpChoice::Library => lib.keep_library_xmp(&asset.id),
            // The editor sidecar (`.rrdata`) is left as it is: the library's
            // state is what the app reads for a library photograph, and an
            // undo back to a never-edited photograph finds it unchanged.
            XmpChoice::Xmp => lib.use_xmp(&asset.id).and_then(|undo| {
                // Read: undoable even when writing the sidecar back fails.
                report.undo.push(undo);
                lib.sync_xmp(&asset.id).map_err(|e| {
                    anyhow::anyhow!("Read from the XMP sidecar; writing it back failed: {e}")
                })
            }),
        };
        report.note(shown, result);
    }
    Ok(report)
}

/// Undoes "Use XMP": puts each photograph's metadata back unless it was
/// edited since, newest step first (a photograph read twice goes back to
/// what it held before the first). Steps that could not be undone are
/// returned in `undo`, in their order, so they can be tried again.
pub fn undo(lib: &Library, undo: &[XmpUndo]) -> XmpReport {
    let mut report = XmpReport::default();
    for step in undo.iter().rev() {
        let path = lib
            .asset(&step.asset)
            .map(|a| a.path.to_string_lossy().into_owned())
            .unwrap_or_else(|_| step.asset.clone());
        match lib.undo_use_xmp(step) {
            Ok(()) => report.done.push(path),
            Err(e) => {
                report.undo.push(step.clone());
                report.failed.push(XmpFailure {
                    path,
                    reason: e.to_string(),
                });
            }
        }
    }
    report.undo.reverse();
    report
}

#[tauri::command]
pub async fn session_xmp_review(directory: String) -> Result<Vec<XmpReviewEntry>, String> {
    with_library(directory, false, |lib| {
        review(lib).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn session_xmp_sync(directory: String) -> Result<XmpReport, String> {
    with_library(directory, true, |lib| {
        sync_pending(lib).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn session_xmp_resolve(
    directory: String,
    paths: Vec<String>,
    keep: XmpChoice,
) -> Result<XmpReport, String> {
    with_library(directory, true, move |lib| {
        resolve(lib, &paths, keep).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn session_xmp_undo(directory: String, steps: Vec<XmpUndo>) -> Result<XmpReport, String> {
    with_library(directory, true, move |lib| Ok(undo(lib, &steps)))
        .await
        .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;
    use rawtonic_catalog::Mode;
    use std::{
        fs,
        path::{Path, PathBuf},
    };

    const EXTERNAL: &str = r#"<?xml version="1.0" encoding="UTF-8"?><x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description rdf:about="" xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmp:Rating="5"/></rdf:RDF></x:xmpmeta>"#;

    /// A session with three synced photographs, each rated 2 then 3 in the
    /// library: `a` offline, `b` with a read-only sidecar, `c` edited
    /// outside Rawtonic. Plus `d`, simply pending.
    fn session() -> (tempfile::TempDir, Library, Vec<PathBuf>) {
        let tmp = tempfile::tempdir().unwrap();
        let lib = Library::create(tmp.path(), "Review", Mode::Session).unwrap();
        let mut photos = Vec::new();
        for name in ["a", "b", "c", "d"] {
            let photo = tmp.path().join(format!("Capture/{name}.nef"));
            fs::write(&photo, b"synthetic original").unwrap();
            let id = lib.add_asset(&photo).unwrap().id;
            let mut meta = lib.metadata(&id).unwrap();
            meta.rating = 2;
            lib.set_metadata(&id, &meta).unwrap();
            lib.sync_xmp(&id).unwrap();
            meta.rating = 3;
            lib.set_metadata(&id, &meta).unwrap();
            photos.push(photo.canonicalize().unwrap());
        }
        fs::rename(&photos[0], tmp.path().join("a-moved.nef")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let xmp = photos[1].with_extension("xmp");
            fs::set_permissions(xmp, fs::Permissions::from_mode(0o444)).unwrap();
        }
        fs::write(photos[2].with_extension("xmp"), EXTERNAL).unwrap();
        (tmp, lib, photos)
    }

    fn shown(path: &Path) -> String {
        path.to_string_lossy().into_owned()
    }

    #[cfg(unix)]
    #[test]
    fn sync_now_writes_pending_and_tells_offline_failed_and_conflicts_apart() {
        let (_tmp, lib, photos) = session();
        let before = review(&lib).unwrap();
        assert_eq!(before.len(), 4);
        assert!(before.iter().all(|e| e.state == XmpState::Pending));
        assert!(
            before
                .iter()
                .any(|e| e.offline && e.path == shown(&photos[0]))
        );

        let report = sync_pending(&lib).unwrap();
        assert_eq!(report.done, vec![shown(&photos[3])]);
        assert_eq!(report.offline, vec![shown(&photos[0])]);
        assert_eq!(report.conflicts, vec![shown(&photos[2])]);
        assert_eq!(report.failed.len(), 1);
        assert_eq!(report.failed[0].path, shown(&photos[1]));
        assert!(
            report.failed[0].reason.contains("read-only"),
            "{:?}",
            report.failed
        );
        assert_eq!(
            fs::read_to_string(photos[2].with_extension("xmp")).unwrap(),
            EXTERNAL
        );

        let states: Vec<_> = review(&lib)
            .unwrap()
            .into_iter()
            .map(|e| (e.path, e.state))
            .collect();
        assert_eq!(
            states,
            vec![
                (shown(&photos[0]), XmpState::Pending),
                (shown(&photos[1]), XmpState::Pending),
                (shown(&photos[2]), XmpState::Conflict),
            ]
        );
        assert_eq!(lib.summary().unwrap().xmp_pending, 3);
        for photo in &photos[1..] {
            assert_eq!(fs::read(photo).unwrap(), b"synthetic original");
        }
    }

    #[test]
    fn keep_library_resolves_a_conflict_and_skips_what_is_not_one() {
        let (_tmp, lib, photos) = session();
        sync_pending(&lib).unwrap();
        let paths = vec![
            shown(&photos[2]),
            format!("{}?vc=copy", shown(&photos[2])),
            shown(&photos[1]),
        ];
        let report = resolve(&lib, &paths, XmpChoice::Library).unwrap();
        assert_eq!(
            report.done,
            vec![shown(&photos[2])],
            "once per photograph, conflicts only"
        );
        assert!(report.undo.is_empty());
        let xmp = fs::read_to_string(photos[2].with_extension("xmp")).unwrap();
        assert!(xmp.contains(r#"xmp:Rating="3""#), "{xmp}");
    }

    #[test]
    fn use_xmp_reads_the_sidecar_into_the_library_and_undoes() {
        let (_tmp, lib, photos) = session();
        sync_pending(&lib).unwrap();
        let id = lib.asset_at(&photos[2]).unwrap().unwrap().id;
        let report = resolve(&lib, &[shown(&photos[2])], XmpChoice::Xmp).unwrap();
        assert_eq!(report.done, vec![shown(&photos[2])]);
        assert_eq!(lib.metadata(&id).unwrap().rating, 5);
        // What the app reads for the photograph: the library's state.
        let sidecar = photos[2].with_file_name("c.nef.rrdata");
        assert_eq!(
            crate::session_bridge::read_editor_sidecar(&sidecar).unwrap()["rating"],
            5
        );
        assert!(
            review(&lib)
                .unwrap()
                .iter()
                .all(|e| e.path != shown(&photos[2]))
        );

        let undone = undo(&lib, &report.undo);
        assert_eq!(undone.done, vec![shown(&photos[2])]);
        assert_eq!(lib.metadata(&id).unwrap().rating, 3);
        assert_eq!(
            crate::session_bridge::read_editor_sidecar(&sidecar).unwrap()["rating"],
            3
        );
        let entry = review(&lib)
            .unwrap()
            .into_iter()
            .find(|e| e.path == shown(&photos[2]))
            .unwrap();
        assert_eq!(entry.state, XmpState::Conflict);
        assert_eq!(fs::read(&photos[2]).unwrap(), b"synthetic original");
    }

    #[test]
    fn an_offline_conflict_waits_for_its_photograph() {
        let (tmp, lib, photos) = session();
        fs::rename(tmp.path().join("a-moved.nef"), &photos[0]).unwrap();
        fs::write(photos[0].with_extension("xmp"), EXTERNAL).unwrap();
        sync_pending(&lib).unwrap();
        fs::rename(&photos[0], tmp.path().join("a-moved.nef")).unwrap();
        for keep in [XmpChoice::Library, XmpChoice::Xmp] {
            let report = resolve(&lib, &[shown(&photos[0])], keep).unwrap();
            assert_eq!(report.offline, vec![shown(&photos[0])]);
        }
        assert_eq!(
            fs::read_to_string(photos[0].with_extension("xmp")).unwrap(),
            EXTERNAL
        );
    }

    #[test]
    fn use_xmp_and_its_undo_leave_an_editor_sidecar_as_it_is() {
        let tmp = tempfile::tempdir().unwrap();
        let lib = Library::create(tmp.path(), "Fresh", Mode::Session).unwrap();
        let photo = tmp.path().join("Capture/e.nef");
        fs::write(&photo, b"synthetic original").unwrap();
        let id = lib.add_asset(&photo).unwrap().id;
        let photo = photo.canonicalize().unwrap();
        // Edits that appeared beside it after it was registered (not adopted).
        let rrdata = photo.with_file_name("e.nef.rrdata");
        let edits = br#"{"version":1,"rating":0,"tags":[],"adjustments":{"exposure":1.25}}"#;
        fs::write(&rrdata, edits).unwrap();
        // A sidecar another application wrote, never adopted: a conflict.
        fs::write(photo.with_extension("xmp"), EXTERNAL).unwrap();
        assert!(lib.sync_xmp(&id).is_err());
        let report = resolve(&lib, &[shown(&photo)], XmpChoice::Xmp).unwrap();
        assert_eq!(report.done, vec![shown(&photo)]);
        assert_eq!(lib.metadata(&id).unwrap().rating, 5);
        assert_eq!(fs::read(&rrdata).unwrap(), edits);
        assert_eq!(undo(&lib, &report.undo).done, vec![shown(&photo)]);
        assert_eq!(lib.metadata(&id).unwrap().rating, 0);
        assert_eq!(
            fs::read(&rrdata).unwrap(),
            edits,
            "the edits are still there"
        );
        // Without one, none is made.
        let other = photo.with_file_name("f.nef");
        fs::write(&other, b"synthetic original").unwrap();
        let other_id = lib.add_asset(&other).unwrap().id;
        fs::write(other.with_extension("xmp"), EXTERNAL).unwrap();
        assert!(lib.sync_xmp(&other_id).is_err());
        resolve(&lib, &[shown(&other)], XmpChoice::Xmp).unwrap();
        assert!(!other.with_file_name("f.nef.rrdata").exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_read_only_conflict_reports_why_it_stays() {
        use std::os::unix::fs::PermissionsExt;
        let (_tmp, lib, photos) = session();
        sync_pending(&lib).unwrap();
        let xmp = photos[2].with_extension("xmp");
        fs::set_permissions(&xmp, fs::Permissions::from_mode(0o444)).unwrap();
        let path = shown(&photos[2]);

        let kept = resolve(&lib, std::slice::from_ref(&path), XmpChoice::Library).unwrap();
        assert!(kept.done.is_empty() && kept.conflicts.is_empty());
        assert_eq!(kept.failed.len(), 1);
        assert!(
            kept.failed[0].reason.contains("read-only"),
            "{:?}",
            kept.failed
        );

        // Read into the library, but the sidecar cannot take the recipe:
        // reported, still undoable, and the photograph is pending.
        let read = resolve(&lib, std::slice::from_ref(&path), XmpChoice::Xmp).unwrap();
        assert_eq!(read.undo.len(), 1);
        assert!(
            read.failed[0].reason.contains("writing it back failed"),
            "{:?}",
            read.failed
        );
        let id = lib.asset_at(&photos[2]).unwrap().unwrap().id;
        assert_eq!(lib.metadata(&id).unwrap().rating, 5);
        let entry = review(&lib)
            .unwrap()
            .into_iter()
            .find(|e| e.path == path)
            .unwrap();
        assert_eq!(entry.state, XmpState::Pending);
        assert_eq!(fs::read_to_string(&xmp).unwrap(), EXTERNAL);
    }

    #[test]
    fn undo_goes_back_through_two_readings_of_one_photo() {
        let (_tmp, lib, photos) = session();
        sync_pending(&lib).unwrap();
        let path = shown(&photos[2]);
        let id = lib.asset_at(&photos[2]).unwrap().unwrap().id;
        let first = resolve(&lib, std::slice::from_ref(&path), XmpChoice::Xmp).unwrap();
        assert_eq!(lib.metadata(&id).unwrap().rating, 5);
        // Changed outside again, read again.
        fs::write(
            photos[2].with_extension("xmp"),
            EXTERNAL.replace(r#"Rating="5""#, r#"Rating="1""#),
        )
        .unwrap();
        assert!(lib.sync_xmp(&id).is_err());
        let second = resolve(&lib, std::slice::from_ref(&path), XmpChoice::Xmp).unwrap();
        assert_eq!(lib.metadata(&id).unwrap().rating, 1);
        let steps: Vec<_> = first.undo.into_iter().chain(second.undo).collect();
        // A step that cannot be undone yet is handed back for another try.
        let blocked = undo(&lib, &steps[..1]);
        assert_eq!(blocked.undo, steps[..1].to_vec());
        let report = undo(&lib, &steps);
        assert!(report.failed.is_empty(), "{:?}", report.failed);
        assert!(report.undo.is_empty());
        assert_eq!(
            lib.metadata(&id).unwrap().rating,
            3,
            "back to before the first reading"
        );
    }
}
