//! Eyes per face (schema 21): readings recorded per reader version and
//! file, the photographer's call winning and surviving a new reading, the
//! Library's eye filters, and an older library taking them.

use rawtonic_catalog::face_eyes::EyeFace;
use rawtonic_catalog::query::Rule;
use rawtonic_catalog::{Library, Mode};
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};

fn session(dir: &Path, names: &[&str]) -> (Library, Vec<String>) {
    let lib = Library::create(&dir.join("Job"), "Job", Mode::Session).unwrap();
    let ids = names
        .iter()
        .map(|name| {
            let path = lib.directory.join("Capture").join(name);
            fs::write(&path, format!("raw {name}")).unwrap();
            lib.add_asset(&path).unwrap().id
        })
        .collect();
    (lib, ids)
}

fn face(x: f32, state: &str) -> EyeFace {
    EyeFace {
        bbox: [x, 0.2, 0.1, 0.15],
        state: state.into(),
        ear: Some(0.2),
        accessories: Some(0.01),
    }
}

fn eyes(value: &str) -> Rule {
    serde_json::from_value(json!({"field": "eyes", "op": "is", "value": value})).unwrap()
}

fn names(found: Vec<PathBuf>) -> Vec<String> {
    let mut names: Vec<_> = found
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn fingerprint(lib: &Library, id: &str) -> String {
    lib.asset(id).unwrap().fingerprint
}

#[test]
fn readings_wait_for_each_photo_and_version_and_a_changed_file_is_read_again() {
    let d = tempfile::tempdir().unwrap();
    let (lib, ids) = session(d.path(), &["A.ARW", "B.ARW"]);
    assert_eq!(lib.eye_backlog_count(1).unwrap(), 2);
    // Newest registered first.
    let work = lib.eye_backlog(1, 10).unwrap();
    assert_eq!(work[0].id, ids[1]);
    assert!(work[0].path.ends_with("Capture/B.ARW"));
    assert_eq!(lib.photo_eyes(&ids[0], 1).unwrap().faces, None);

    let fp = fingerprint(&lib, &ids[0]);
    lib.record_eyes(
        &ids[0],
        1,
        &fp,
        Ok(&[face(0.1, "open"), face(0.5, "closed")]),
    )
    .unwrap();
    let fp = fingerprint(&lib, &ids[1]);
    lib.record_eyes(&ids[1], 1, &fp, Err("No preview")).unwrap();
    assert_eq!(lib.eye_backlog_count(1).unwrap(), 0);
    let read = lib.photo_eyes(&ids[0], 1).unwrap();
    let faces = read.faces.unwrap();
    assert_eq!(faces.len(), 2);
    assert_eq!((faces[1].face, faces[1].effective()), (1, "closed"));
    let failed = lib.photo_eyes(&ids[1], 1).unwrap();
    assert_eq!(
        (failed.faces, failed.error.as_deref()),
        (None, Some("No preview"))
    );

    // A newer reader reads everything again; until then the old reading
    // is not handed out as its.
    assert_eq!(lib.eye_backlog_count(2).unwrap(), 2);
    assert_eq!(lib.photo_eyes(&ids[0], 2).unwrap().faces, None);
    // A file that changed (another fingerprint) is read again.
    let db = rusqlite::Connection::open(lib.directory.join("session.sqlite")).unwrap();
    db.execute(
        "UPDATE assets SET fingerprint='changed' WHERE id=?",
        [&ids[0]],
    )
    .unwrap();
    assert_eq!(lib.eye_backlog(1, 10).unwrap()[0].id, ids[0]);
    assert_eq!(lib.photo_eyes(&ids[0], 1).unwrap().faces, None);
    assert!(
        lib.record_eyes(&ids[0], 1, "x", Ok(&[face(0.1, "blink")]))
            .is_err()
    );
}

#[test]
fn the_photographers_call_wins_and_survives_a_new_reading() {
    let d = tempfile::tempdir().unwrap();
    let (lib, ids) = session(d.path(), &["A.ARW", "B.ARW", "C.ARW"]);
    let id = &ids[0];
    let fp = fingerprint(&lib, id);
    lib.record_eyes(
        id,
        1,
        &fp,
        Ok(&[face(0.1, "closed"), face(0.5, "uncertain")]),
    )
    .unwrap();
    let fp_b = fingerprint(&lib, &ids[1]);
    lib.record_eyes(&ids[1], 1, &fp_b, Ok(&[face(0.3, "open")]))
        .unwrap();
    let fp_c = fingerprint(&lib, &ids[2]);
    lib.record_eyes(&ids[2], 1, &fp_c, Ok(&[])).unwrap();
    assert_eq!(names(lib.search(&eyes("closed")).unwrap()), ["A.ARW"]);
    assert_eq!(names(lib.search(&eyes("uncertain")).unwrap()), ["A.ARW"]);

    // Wrongly called closed: open it. Then the uncertain one is closed.
    lib.set_eye_override(id, 0, Some("open")).unwrap();
    assert!(lib.search(&eyes("closed")).unwrap().is_empty());
    lib.set_eye_override(id, 1, Some("closed")).unwrap();
    assert_eq!(names(lib.search(&eyes("closed")).unwrap()), ["A.ARW"]);
    assert!(lib.search(&eyes("uncertain")).unwrap().is_empty());
    let faces = lib.photo_eyes(id, 1).unwrap().faces.unwrap();
    assert_eq!(faces[0].read.state, "closed");
    assert_eq!(faces[0].effective(), "open");

    // Never a call of "uncertain", never a face that is not there.
    assert!(lib.set_eye_override(id, 0, Some("uncertain")).is_err());
    assert!(lib.set_eye_override(id, 7, Some("open")).is_err());

    // Read again (a newer reader): boxes moved a little, one face more on
    // the left. Each call follows its face, not its index.
    let mut again = vec![face(0.0, "open"), face(0.11, "closed"), face(0.49, "open")];
    again[1].bbox[1] = 0.21;
    lib.record_eyes(id, 2, &fp, Ok(&again)).unwrap();
    let faces = lib.photo_eyes(id, 2).unwrap().faces.unwrap();
    let calls: Vec<_> = faces.iter().map(|f| f.override_state.as_deref()).collect();
    assert_eq!(calls, [None, Some("open"), Some("closed")]);
    // Cleared: the reading counts again.
    lib.set_eye_override(id, 1, None).unwrap();
    assert_eq!(
        lib.photo_eyes(id, 2).unwrap().faces.unwrap()[1].effective(),
        "closed"
    );

    // Open/none of the others: "is not closed" keeps photographs without faces.
    let not_closed: Rule =
        serde_json::from_value(json!({"field": "eyes", "op": "isNot", "value": "closed"})).unwrap();
    assert_eq!(names(lib.search(&not_closed).unwrap()), ["B.ARW", "C.ARW"]);
    let either: Rule = serde_json::from_value(
        json!({"field": "eyes", "op": "is", "value": ["notVisible", "open"]}),
    )
    .unwrap();
    assert_eq!(names(lib.search(&either).unwrap()), ["A.ARW", "B.ARW"]);
}

#[test]
fn a_library_of_schema_20_opens_unread_and_takes_readings() {
    let d = tempfile::tempdir().unwrap();
    let (lib, ids) = session(d.path(), &["A.ARW"]);
    drop(lib);
    let db = rusqlite::Connection::open(d.path().join("Job/session.sqlite")).unwrap();
    db.execute_batch(include_str!("undo_0021.sql")).unwrap();
    drop(db);
    let lib = Library::open(&d.path().join("Job")).unwrap();
    assert_eq!(lib.eye_backlog_count(1).unwrap(), 1);
    assert!(lib.search(&eyes("closed")).unwrap().is_empty());
    let fp = fingerprint(&lib, &ids[0]);
    lib.record_eyes(&ids[0], 1, &fp, Ok(&[face(0.1, "closed")]))
        .unwrap();
    assert_eq!(names(lib.search(&eyes("closed")).unwrap()), ["A.ARW"]);
    // Removing the photograph removes its eyes.
    let db = rusqlite::Connection::open(d.path().join("Job/session.sqlite")).unwrap();
    db.execute_batch("PRAGMA foreign_keys=ON").unwrap();
    db.execute("DELETE FROM assets WHERE id=?", [&ids[0]])
        .unwrap();
    let left: i64 = db
        .query_row("SELECT count(*) FROM face_eyes", [], |r| r.get(0))
        .unwrap();
    assert_eq!(left, 0);
}
