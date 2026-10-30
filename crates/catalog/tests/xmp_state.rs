#![cfg(feature = "test-support")] // Library::fail_when_marking

//! Which photographs count as "XMP not saved" in the session bar, and
//! writing them again (O-03/C17 of the 2026-10-28 UI review).

use rawtonic_catalog::{CaptureFacts, Library, Mode, SidecarStamps, XmpProblem};
use std::fs;
use std::path::{Path, PathBuf};

fn session(dir: &Path, names: &[&str]) -> (Library, Vec<String>, Vec<PathBuf>) {
    let lib = Library::create(&dir.join("Job"), "Job", Mode::Session).unwrap();
    let paths: Vec<PathBuf> = names
        .iter()
        .map(|name| {
            let path = lib.directory.join("Capture").join(name);
            fs::write(&path, format!("raw {name}")).unwrap();
            path
        })
        .collect();
    let ids = paths
        .iter()
        .map(|path| lib.add_asset(path).unwrap().id)
        .collect();
    (lib, ids, paths)
}

#[test]
fn untouched_photographs_have_no_xmp_to_write() {
    let d = tempfile::tempdir().unwrap();
    let (lib, ids, paths) = session(d.path(), &["A.ARW", "B.NEF"]);
    lib.add_files(&[paths[1].clone()]).unwrap();
    // The camera's own rating comes from the file: nothing for the XMP either.
    let facts = CaptureFacts {
        camera_rating: Some(3),
        ..CaptureFacts::default()
    };
    lib.record_facts(&ids[1], &facts).unwrap();
    assert_eq!(lib.asset(&ids[1]).unwrap().rating, 3);
    assert_eq!(lib.summary().unwrap().xmp_pending, 0);
    assert!(lib.pending_xmp_paths().unwrap().is_empty());
    for path in &paths {
        assert!(!path.with_extension("xmp").exists());
    }
}

#[test]
fn a_saved_recipe_is_pending_until_written() {
    let d = tempfile::tempdir().unwrap();
    let (lib, ids, paths) = session(d.path(), &["A.ARW"]);
    let version = lib.primary_version(&ids[0]).unwrap();
    lib.save_recipe(
        &version,
        &serde_json::json!({"schema_version": 1, "adjustments": {"exposure": 1.0}, "layers": []}),
        None,
    )
    .unwrap();
    assert_eq!(lib.pending_xmp_paths().unwrap(), [paths[0].clone()]);
    assert_eq!(lib.write_pending_xmp().unwrap().written, 1);
    assert_eq!(lib.summary().unwrap().xmp_pending, 0);
}

#[test]
fn a_rating_not_written_to_its_xmp_yet_is_pending_until_written_again() {
    let d = tempfile::tempdir().unwrap();
    let (lib, ids, paths) = session(d.path(), &["A.ARW", "B.ARW"]);
    let original = fs::read(&paths[0]).unwrap();
    let mut meta = lib.metadata(&ids[0]).unwrap();
    meta.rating = 4;
    lib.set_metadata(&ids[0], &meta).unwrap();
    assert_eq!(lib.pending_xmp_paths().unwrap(), [paths[0].clone()]);
    let retry = lib.write_pending_xmp().unwrap();
    assert_eq!((retry.written, retry.unsaved.len()), (1, 0));
    assert_eq!(lib.summary().unwrap().xmp_pending, 0);
    let xmp = fs::read_to_string(paths[0].with_extension("xmp")).unwrap();
    assert_eq!(rawtonic_metadata::read_xmp(&xmp).unwrap().0.rating, 4);
    assert_eq!(fs::read(&paths[0]).unwrap(), original);
    assert!(!paths[1].with_extension("xmp").exists());
}

#[test]
fn a_sidecar_changed_elsewhere_or_shared_is_reported_and_left_alone() {
    let d = tempfile::tempdir().unwrap();
    let (lib, ids, paths) = session(d.path(), &["A.ARW", "B.ARW", "B.JPG"]);
    lib.sync_xmp(&ids[0]).unwrap();
    let sidecar = paths[0].with_extension("xmp");
    fs::write(&sidecar, b"another program").unwrap();
    for id in &ids {
        let mut meta = lib.metadata(id).unwrap();
        meta.rating = 2;
        lib.set_metadata(id, &meta).unwrap();
    }
    let retry = lib.write_pending_xmp().unwrap();
    assert_eq!(retry.written, 0);
    let problems: Vec<_> = retry
        .unsaved
        .iter()
        .map(|u| (u.path.clone(), u.problem.clone()))
        .collect();
    assert_eq!(
        problems,
        [
            (paths[0].clone(), XmpProblem::ChangedElsewhere),
            (paths[1].clone(), XmpProblem::SharedName),
            (paths[2].clone(), XmpProblem::SharedName),
        ]
    );
    assert_eq!(fs::read(&sidecar).unwrap(), b"another program");
    assert!(!paths[1].with_extension("xmp").exists());
    assert_eq!(lib.summary().unwrap().xmp_pending, 3);
}

/// Libraries made before schema 18 counted every untouched photograph as
/// pending; opening them clears exactly those.
#[test]
fn opening_an_older_library_clears_only_the_false_pending_states() {
    let d = tempfile::tempdir().unwrap();
    let (lib, ids, _) = session(d.path(), &["A.ARW", "B.ARW", "C.ARW", "D.ARW"]);
    let mut meta = lib.metadata(&ids[1]).unwrap();
    meta.rating = 5;
    lib.set_metadata(&ids[1], &meta).unwrap();
    lib.save_editor_state(
        &ids[2],
        &serde_json::json!({"adjustments": {"exposure": 1.0}}),
    )
    .unwrap();
    // A recipe saved without an editor state is an edit all the same.
    let version = lib.primary_version(&ids[3]).unwrap();
    lib.save_recipe(
        &version,
        &serde_json::json!({"schema_version": 1, "adjustments": {"exposure": 1.0}, "layers": []}),
        None,
    )
    .unwrap();
    drop(lib);
    let db = rusqlite::Connection::open(d.path().join("Job/session.sqlite")).unwrap();
    db.execute_batch(include_str!("undo_0018.sql")).unwrap();
    // As schema 17 registered them.
    for id in [&ids[0], &ids[3]] {
        db.execute(
            "UPDATE metadata SET xmp_status='pending' WHERE asset_id=?",
            [id],
        )
        .unwrap();
    }
    drop(db);
    let lib = Library::open(&d.path().join("Job")).unwrap();
    let mut pending: Vec<_> = lib
        .pending_xmp_paths()
        .unwrap()
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    pending.sort();
    assert_eq!(pending, ["B.ARW", "C.ARW", "D.ARW"]);
    assert_eq!(lib.summary().unwrap().xmp_pending, 3);
}

#[test]
fn an_offline_photograph_is_not_tried() {
    let d = tempfile::tempdir().unwrap();
    let (lib, ids, paths) = session(d.path(), &["A.ARW"]);
    let mut meta = lib.metadata(&ids[0]).unwrap();
    meta.rating = 1;
    lib.set_metadata(&ids[0], &meta).unwrap();
    fs::remove_file(&paths[0]).unwrap();
    lib.check_files().unwrap();
    let retry = lib.write_pending_xmp().unwrap();
    assert_eq!(retry.unsaved[0].problem, XmpProblem::Offline);
    assert!(!paths[0].with_extension("xmp").exists());
}

#[test]
fn a_folder_with_the_sidecar_s_name_is_reported_until_it_is_gone() {
    let d = tempfile::tempdir().unwrap();
    let (lib, ids, paths) = session(d.path(), &["A.ARW"]);
    let sidecar = paths[0].with_extension("xmp");
    fs::create_dir(&sidecar).unwrap();
    let mut meta = lib.metadata(&ids[0]).unwrap();
    meta.rating = 2;
    lib.set_metadata(&ids[0], &meta).unwrap();
    let retry = lib.write_pending_xmp().unwrap();
    assert_eq!(retry.unsaved[0].problem, XmpProblem::ChangedElsewhere);
    assert!(sidecar.is_dir());
    fs::remove_dir(&sidecar).unwrap();
    assert_eq!(lib.write_pending_xmp().unwrap().written, 1);
    assert_eq!(lib.summary().unwrap().xmp_pending, 0);
}

#[test]
fn a_photograph_moved_away_since_the_last_check_gets_no_sidecar() {
    let d = tempfile::tempdir().unwrap();
    let (lib, ids, paths) = session(d.path(), &["A.ARW"]);
    let mut meta = lib.metadata(&ids[0]).unwrap();
    meta.rating = 1;
    lib.set_metadata(&ids[0], &meta).unwrap();
    fs::rename(&paths[0], d.path().join("A.ARW")).unwrap();
    let retry = lib.write_pending_xmp().unwrap();
    assert_eq!(retry.unsaved[0].problem, XmpProblem::Offline);
    assert!(!paths[0].with_extension("xmp").exists());
    assert_eq!(lib.summary().unwrap().xmp_pending, 1);
}

#[test]
fn a_photograph_back_since_the_last_check_gets_its_sidecar() {
    let d = tempfile::tempdir().unwrap();
    let (lib, ids, paths) = session(d.path(), &["A.ARW"]);
    let mut meta = lib.metadata(&ids[0]).unwrap();
    meta.rating = 1;
    lib.set_metadata(&ids[0], &meta).unwrap();
    let away = d.path().join("A.ARW");
    fs::rename(&paths[0], &away).unwrap();
    lib.check_files().unwrap();
    assert!(lib.asset(&ids[0]).unwrap().missing);
    fs::rename(&away, &paths[0]).unwrap();
    assert_eq!(lib.write_pending_xmp().unwrap().written, 1);
    assert!(paths[0].with_extension("xmp").is_file());
}

/// Rates the photographs and writes their sidecars: in sync, as after an edit.
fn rated_and_written(lib: &Library, ids: &[String]) {
    for id in ids {
        let mut meta = lib.metadata(id).unwrap();
        meta.rating = 3;
        lib.set_metadata(id, &meta).unwrap();
    }
    assert_eq!(lib.write_pending_xmp().unwrap().written, ids.len());
    assert_eq!(lib.summary().unwrap().xmp_pending, 0);
}

fn problems(lib: &Library) -> Vec<(PathBuf, Option<XmpProblem>)> {
    lib.xmp_unsynced()
        .unwrap()
        .into_iter()
        .map(|entry| (entry.path, entry.problem))
        .collect()
}

#[test]
fn a_sidecar_changed_elsewhere_is_found_without_any_write() {
    let d = tempfile::tempdir().unwrap();
    let (lib, ids, paths) = session(d.path(), &["A.ARW", "B.ARW", "C.ARW", "D.ARW"]);
    rated_and_written(&lib, &ids[..3]);
    let originals: Vec<_> = paths.iter().map(|p| fs::read(p).unwrap()).collect();
    // Another program rewrites A's sidecar, deletes B's and writes one
    // beside D, which the library never had one for; C is left alone.
    let other = "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"><rdf:Description rdf:about=\"\" xmlns:xmp=\"http://ns.adobe.com/xap/1.0/\" xmp:Rating=\"5\"/></rdf:RDF></x:xmpmeta>";
    fs::write(paths[0].with_extension("xmp"), other).unwrap();
    fs::remove_file(paths[1].with_extension("xmp")).unwrap();
    fs::write(paths[3].with_extension("xmp"), other).unwrap();
    let kept_c = fs::read(paths[2].with_extension("xmp")).unwrap();

    let mut seen = SidecarStamps::default();
    let check = lib.check_xmp_sidecars(None, &mut seen).unwrap();
    assert_eq!(
        check.changed,
        [ids[0].clone(), ids[1].clone(), ids[3].clone()]
    );
    assert_eq!(lib.mark_changed_elsewhere(&check.changed).unwrap(), 3);

    assert_eq!(lib.summary().unwrap().xmp_pending, 3);
    assert_eq!(
        problems(&lib),
        [
            (paths[0].clone(), Some(XmpProblem::ChangedElsewhere)),
            (paths[1].clone(), Some(XmpProblem::ChangedElsewhere)),
            (paths[3].clone(), Some(XmpProblem::ChangedElsewhere)),
        ]
    );
    // Found, not written: the sidecars are as the other program left them.
    assert_eq!(
        fs::read_to_string(paths[0].with_extension("xmp")).unwrap(),
        other
    );
    assert!(!paths[1].with_extension("xmp").exists());
    assert_eq!(fs::read(paths[2].with_extension("xmp")).unwrap(), kept_c);
    for (path, bytes) in paths.iter().zip(&originals) {
        assert_eq!(&fs::read(path).unwrap(), bytes);
    }
    // Marked once: a second check finds nothing new.
    let again = lib.check_xmp_sidecars(None, &mut seen).unwrap();
    assert!(again.changed.is_empty());
    assert_eq!(lib.mark_changed_elsewhere(&check.changed).unwrap(), 0);
}

#[test]
fn an_unchanged_sidecar_is_read_once_and_then_only_looked_at() {
    let d = tempfile::tempdir().unwrap();
    let (lib, ids, paths) = session(d.path(), &["A.ARW", "B.ARW"]);
    rated_and_written(&lib, &ids);
    let mut seen = SidecarStamps::default();
    let first = lib.check_xmp_sidecars(None, &mut seen).unwrap();
    assert_eq!((first.checked, first.read, first.changed.len()), (2, 2, 0));
    let second = lib.check_xmp_sidecars(None, &mut seen).unwrap();
    assert_eq!(
        (second.checked, second.read, second.changed.len()),
        (2, 0, 0)
    );
    // Rewritten by another program: read again and found.
    let sidecar = paths[1].with_extension("xmp");
    let mut text = fs::read_to_string(&sidecar).unwrap();
    text.push_str("\n<!-- touched -->");
    fs::write(&sidecar, text).unwrap();
    let third = lib.check_xmp_sidecars(None, &mut seen).unwrap();
    assert_eq!((third.read, &third.changed), (1, &vec![ids[1].clone()]));
    // The library writing it again (Keep library) is in sync again.
    lib.mark_changed_elsewhere(&third.changed).unwrap();
    lib.keep_library_xmp(&ids[1]).unwrap();
    let fourth = lib.check_xmp_sidecars(None, &mut seen).unwrap();
    assert_eq!((fourth.read, fourth.changed.len()), (1, 0));
}

#[test]
fn a_folder_check_looks_only_at_that_folder() {
    let d = tempfile::tempdir().unwrap();
    let (lib, ids, paths) = session(d.path(), &["A.ARW"]);
    let selects = lib.directory.join("Selects");
    fs::create_dir_all(&selects).unwrap();
    let b = selects.join("B.ARW");
    fs::write(&b, "raw B").unwrap();
    let ids = [ids[0].clone(), lib.add_asset(&b).unwrap().id];
    rated_and_written(&lib, &ids);
    fs::write(paths[0].with_extension("xmp"), "another program").unwrap();
    fs::write(b.with_extension("xmp"), "another program").unwrap();
    let mut seen = SidecarStamps::default();
    let check = lib.check_xmp_sidecars(Some(&selects), &mut seen).unwrap();
    assert_eq!((check.checked, check.changed), (1, vec![ids[1].clone()]));
}

/// G60 (Codex on #451): the photographs one check found are marked in one
/// transaction: either every one of them is marked or none is, so the
/// session bar never shows half of what one check found.
#[test]
fn one_check_marks_its_photographs_in_one_transaction() {
    let d = tempfile::tempdir().unwrap();
    let (lib, ids, paths) = session(d.path(), &["A.ARW", "B.ARW", "C.ARW", "D.ARW"]);
    rated_and_written(&lib, &ids);
    for path in &paths {
        fs::write(path.with_extension("xmp"), "another program").unwrap();
    }
    let mut seen = SidecarStamps::default();
    let check = lib.check_xmp_sidecars(None, &mut seen).unwrap();
    assert_eq!(check.changed, ids);
    let conflicting = |lib: &Library| {
        lib.xmp_unsynced()
            .unwrap()
            .into_iter()
            .filter(|e| e.problem == Some(XmpProblem::ChangedElsewhere))
            .count()
    };
    assert_eq!(conflicting(&lib), 0, "in sync until marked");

    // The second of four marks fails, as a full disk would fail it: the
    // first is made, the three after it are never reached. With a commit per
    // photograph the first would be marked.
    let marks = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = marks.clone();
    lib.fail_when_marking(Box::new(move || {
        if count.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 1 {
            anyhow::bail!("injected write failure")
        }
        Ok(())
    }));
    assert!(lib.mark_changed_elsewhere(&check.changed).is_err());
    assert_eq!(conflicting(&lib), 0, "none marked: the check rolled back");
    assert_eq!(lib.summary().unwrap().xmp_pending, 0);

    // Without the failure the same check marks all four.
    let lib = Library::open(&lib.directory).unwrap();
    assert_eq!(lib.mark_changed_elsewhere(&check.changed).unwrap(), 4);
    assert_eq!(conflicting(&lib), 4);
    assert_eq!(lib.summary().unwrap().xmp_pending, 4);
    // The photographs themselves are never written.
    for (path, name) in paths.iter().zip(["A.ARW", "B.ARW", "C.ARW", "D.ARW"]) {
        assert_eq!(fs::read(path).unwrap(), format!("raw {name}").into_bytes());
    }
}

#[test]
fn only_what_is_still_changed_elsewhere_is_marked() {
    let d = tempfile::tempdir().unwrap();
    let (lib, ids, paths) = session(d.path(), &["A.ARW", "B.ARW", "C.ARW", "C.JPG"]);
    rated_and_written(&lib, &ids[..2]);
    let a = paths[0].with_extension("xmp");
    let written = fs::read(&a).unwrap();
    fs::write(&a, "another program").unwrap();
    fs::write(paths[1].with_extension("xmp"), "another program").unwrap();
    fs::write(paths[2].with_extension("xmp"), "another program").unwrap();
    let mut seen = SidecarStamps::default();
    let check = lib.check_xmp_sidecars(None, &mut seen).unwrap();
    assert_eq!(check.changed, [ids[0].clone(), ids[1].clone()]);
    // Meanwhile A's sidecar is put back and B's photograph goes offline:
    // neither is a sidecar to review now. C and its JPEG share theirs and
    // are never written, so not found either.
    fs::write(&a, written).unwrap();
    fs::remove_file(&paths[1]).unwrap();
    assert_eq!(lib.mark_changed_elsewhere(&check.changed).unwrap(), 0);
    assert_eq!(lib.summary().unwrap().xmp_pending, 0);
}
