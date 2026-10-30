//! The session bar's XMP status (`SessionXmpStatus.tsx`): which photographs'
//! XMP sidecars do not hold what the library holds and why
//! (`rawtonic_catalog::XmpProblem`), and resolving those changed by another
//! program for the library ("Keep library") or for the sidecar ("Use XMP",
//! one step of the app's Undo). Writing the others again is "Try again"
//! (`session_bridge::session_write_pending_xmp`). The catalog does the work
//! (`rawtonic_catalog::xmp_review`) in a blocking worker under the library's
//! write lock; an edit only in a photograph's editor sidecar is adopted
//! before any of it (`Library::adopt_editor_sidecar`), and originals are
//! never touched.

use crate::session_bridge::with_library;
use rawtonic_catalog::{Library, XmpOffline, XmpProblem, XmpUndo};
use rawtonic_metadata::XmpConflict;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};

/// A photograph of the status panel's list.
#[derive(Debug, Clone, Serialize)]
pub struct XmpReviewEntry {
    pub path: String,
    /// None: pending, written by Try again.
    pub problem: Option<XmpProblem>,
    pub volume: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct XmpFailure {
    pub path: String,
    pub reason: String,
}

/// What resolving or its undo did, by photograph path.
#[derive(Debug, Clone, Default, Serialize)]
pub struct XmpReport {
    /// Resolved (or undone) and in sync or pending now.
    pub done: Vec<String>,
    /// Offline photographs: left until they are back.
    pub offline: Vec<String>,
    /// Their sidecar changed again meanwhile: still to review.
    pub conflicts: Vec<String>,
    pub failed: Vec<XmpFailure>,
    /// For "Use XMP": what puts the library's metadata back. For its undo:
    /// the steps that could not be undone.
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
    Ok(lib
        .xmp_unsynced()?
        .into_iter()
        .map(|entry| XmpReviewEntry {
            path: entry.path.to_string_lossy().into_owned(),
            problem: entry.problem,
            volume: entry.volume,
        })
        .collect())
}

impl XmpReport {
    fn note(&mut self, path: String, result: anyhow::Result<()>) {
        match result {
            Ok(()) => self.done.push(path),
            Err(e) if e.is::<XmpOffline>() => self.offline.push(path),
            Err(e) if e.is::<XmpConflict>() => self.conflicts.push(path),
            Err(e) => self.failed.push(XmpFailure {
                path,
                reason: format!("{e:#}"),
            }),
        }
    }
}

/// Resolves the sidecars of `paths` changed by another program (library
/// paths; virtual copies count as their photograph) one way. An offline one
/// is reported offline; photographs without such a sidecar are skipped.
pub fn resolve(lib: &Library, paths: &[String], keep: XmpChoice) -> anyhow::Result<XmpReport> {
    let listed: HashMap<String, Option<XmpProblem>> = lib
        .xmp_unsynced()?
        .into_iter()
        .map(|entry| (entry.id, entry.problem))
        .collect();
    let mut seen = BTreeSet::new();
    let mut report = XmpReport::default();
    for path in paths {
        let source = crate::file_management::parse_virtual_path(path).0;
        let Some(asset) = lib.asset_at(&source)? else {
            continue;
        };
        if !seen.insert(asset.id.clone()) {
            continue;
        }
        if !matches!(
            listed.get(&asset.id),
            Some(Some(XmpProblem::ChangedElsewhere | XmpProblem::Offline))
        ) {
            continue;
        }
        let shown = source.to_string_lossy().into_owned();
        let result = match keep {
            XmpChoice::Library => lib.keep_library_xmp(&asset.id),
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

    /// A session with four synced photographs, each rated 2 then 3 in the
    /// library: `a` offline, `b` with a read-only sidecar, `c` edited
    /// outside Rawtonic, `d` simply pending.
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

    fn problems(lib: &Library) -> Vec<(String, Option<XmpProblem>)> {
        review(lib)
            .unwrap()
            .into_iter()
            .map(|e| (e.path, e.problem))
            .collect()
    }

    #[cfg(unix)]
    #[test]
    fn the_list_says_why_each_xmp_is_behind_as_try_again_does() {
        let (_tmp, lib, photos) = session();
        assert_eq!(
            problems(&lib),
            vec![
                (shown(&photos[0]), Some(XmpProblem::Offline)),
                (shown(&photos[1]), None),
                (shown(&photos[2]), Some(XmpProblem::ChangedElsewhere)),
                (shown(&photos[3]), None),
            ]
        );
        let retry = lib.write_pending_xmp().unwrap();
        assert_eq!(retry.written, 1);
        assert_eq!(
            problems(&lib),
            vec![
                (shown(&photos[0]), Some(XmpProblem::Offline)),
                (shown(&photos[1]), None),
                (shown(&photos[2]), Some(XmpProblem::ChangedElsewhere)),
            ]
        );
        assert_eq!(lib.summary().unwrap().xmp_pending, 3);
        assert_eq!(
            fs::read_to_string(photos[2].with_extension("xmp")).unwrap(),
            EXTERNAL
        );
        for photo in &photos[1..] {
            assert_eq!(fs::read(photo).unwrap(), b"synthetic original");
        }
    }

    #[test]
    fn keep_library_resolves_a_changed_sidecar_and_skips_what_is_not_one() {
        let (_tmp, lib, photos) = session();
        let paths = vec![
            shown(&photos[2]),
            format!("{}?vc=copy", shown(&photos[2])),
            shown(&photos[3]),
        ];
        let report = resolve(&lib, &paths, XmpChoice::Library).unwrap();
        assert_eq!(
            report.done,
            vec![shown(&photos[2])],
            "once per photograph, changed sidecars only"
        );
        assert!(report.undo.is_empty());
        let xmp = fs::read_to_string(photos[2].with_extension("xmp")).unwrap();
        assert!(xmp.contains(r#"xmp:Rating="3""#), "{xmp}");
    }

    #[test]
    fn use_xmp_reads_the_sidecar_into_the_library_and_undoes() {
        let (_tmp, lib, photos) = session();
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
        assert!(problems(&lib).iter().all(|(p, _)| p != &shown(&photos[2])));

        let undone = undo(&lib, &report.undo);
        assert_eq!(undone.done, vec![shown(&photos[2])]);
        assert_eq!(lib.metadata(&id).unwrap().rating, 3);
        assert_eq!(
            crate::session_bridge::read_editor_sidecar(&sidecar).unwrap()["rating"],
            3
        );
        assert!(
            problems(&lib).contains(&(shown(&photos[2]), Some(XmpProblem::ChangedElsewhere))),
            "to review again"
        );
        assert_eq!(fs::read(&photos[2]).unwrap(), b"synthetic original");
    }

    #[test]
    fn an_offline_conflict_waits_for_its_photograph() {
        let (tmp, lib, photos) = session();
        fs::rename(tmp.path().join("a-moved.nef"), &photos[0]).unwrap();
        fs::write(photos[0].with_extension("xmp"), EXTERNAL).unwrap();
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

    /// XS r9: Use XMP clearing the last rating of a photograph with no
    /// recipe but an edit in the editor sidecar beside it must not make the
    /// app fall back to that file's edit (and its rating): the edit is
    /// adopted first, so the look stays and the rating is the sidecar's.
    #[test]
    fn use_xmp_on_a_photo_whose_edit_is_only_beside_it_keeps_its_look() {
        let tmp = tempfile::tempdir().unwrap();
        let lib = Library::create(tmp.path(), "Fresh", Mode::Session).unwrap();
        let photo = tmp.path().join("Capture/e.nef");
        fs::write(&photo, b"synthetic original").unwrap();
        let id = lib.add_asset(&photo).unwrap().id;
        let photo = photo.canonicalize().unwrap();
        let rrdata = photo.with_file_name("e.nef.rrdata");
        let edits = br#"{"version":1,"rating":4,"tags":[],"adjustments":{"exposure":1.25}}"#;
        fs::write(&rrdata, edits).unwrap();
        let cleared = EXTERNAL.replace(r#"Rating="5""#, r#"Rating="0""#);
        fs::write(photo.with_extension("xmp"), &cleared).unwrap();
        // Another application's sidecar, never adopted: a conflict.
        assert!(lib.sync_xmp(&id).is_err());
        let shows =
            |key: &str| crate::exif_processing::load_sidecar(&rrdata).adjustments[key].clone();
        let rating = || crate::exif_processing::load_sidecar(&rrdata).rating;
        assert_eq!((shows("exposure"), rating()), (1.25.into(), 4));

        let report = resolve(&lib, &[shown(&photo)], XmpChoice::Xmp).unwrap();
        assert_eq!(report.done, vec![shown(&photo)]);
        assert_eq!(shows("exposure"), 1.25, "the look stays");
        assert_eq!(
            rating(),
            0,
            "the sidecar's rating, not the editor sidecar's"
        );
        assert_eq!(
            fs::read(&rrdata).unwrap(),
            edits,
            "editor sidecar untouched"
        );

        undo(&lib, &report.undo);
        assert_eq!((shows("exposure"), rating()), (1.25.into(), 4));
        assert_eq!(fs::read(&photo).unwrap(), b"synthetic original");
    }

    #[cfg(unix)]
    #[test]
    fn a_read_only_conflict_reports_why_it_stays() {
        use std::os::unix::fs::PermissionsExt;
        let (_tmp, lib, photos) = session();
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
        assert!(problems(&lib).contains(&(path, None)));
        assert_eq!(fs::read_to_string(&xmp).unwrap(), EXTERNAL);
    }

    #[test]
    fn undo_goes_back_through_two_readings_of_one_photo() {
        let (_tmp, lib, photos) = session();
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
