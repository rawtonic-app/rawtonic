//! Which photographs count as "XMP not saved" in the session bar, and
//! writing them again (O-03/C17 of the 2026-10-28 UI review).

use rawtonic_catalog::{CaptureFacts, Library, Mode, XmpProblem};
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
