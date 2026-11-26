//! The verified sensor layout of each RAW (schema 38): recorded only for the
//! file the library has registered, dropped when that file changes, and
//! added to older libraries by their upgrade.

use rawtonic_catalog::sensor_kind::SensorKind;
use rawtonic_catalog::{Library, Mode, fingerprint};
use std::fs;
use std::path::{Path, PathBuf};

fn session(dir: &Path, names: &[&str]) -> (Library, Vec<String>, Vec<PathBuf>) {
    let lib = Library::create(&dir.join("Job"), "Job", Mode::Session).unwrap();
    let paths: Vec<PathBuf> = names
        .iter()
        .map(|name| {
            let path = lib.directory.join("Capture").join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, format!("raw bytes of {name}")).unwrap();
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
fn a_kind_read_from_the_registered_file_is_kept() {
    let d = tempfile::tempdir().unwrap();
    let (lib, ids, paths) = session(d.path(), &["A.RAF", "B.NEF"]);
    assert_eq!(lib.sensor_kind(&ids[0]).unwrap(), None);
    let a = fingerprint(&paths[0]).unwrap();
    let b = fingerprint(&paths[1]).unwrap();
    assert!(
        lib.record_sensor_kind(&ids[0], SensorKind::XTrans, &a)
            .unwrap()
    );
    assert!(
        lib.record_sensor_kind(&ids[1], SensorKind::Bayer, &b)
            .unwrap()
    );
    assert_eq!(lib.sensor_kind(&ids[0]).unwrap(), Some(SensorKind::XTrans));
    assert_eq!(lib.sensor_kind(&ids[1]).unwrap(), Some(SensorKind::Bayer));
    // Read again, the file's own answer replaces the record.
    assert!(
        lib.record_sensor_kind(&ids[0], SensorKind::Bayer, &a)
            .unwrap()
    );
    assert_eq!(lib.sensor_kind(&ids[0]).unwrap(), Some(SensorKind::Bayer));
    // Kept across reopening (a later offline Develop).
    drop(lib);
    let lib = Library::open(&d.path().join("Job")).unwrap();
    assert_eq!(lib.sensor_kind(&ids[1]).unwrap(), Some(SensorKind::Bayer));
}

#[test]
fn a_read_of_the_file_changed_in_place_replaces_the_kind() {
    let d = tempfile::tempdir().unwrap();
    let (lib, ids, paths) = session(d.path(), &["A.DNG"]);
    let old = fingerprint(&paths[0]).unwrap();
    assert!(
        lib.record_sensor_kind(&ids[0], SensorKind::XTrans, &old)
            .unwrap()
    );
    // Another file at the same name; the library has not registered it yet.
    fs::write(&paths[0], b"another camera's file").unwrap();
    let now = fingerprint(&paths[0]).unwrap();
    assert!(
        lib.record_sensor_kind(&ids[0], SensorKind::Bayer, &now)
            .unwrap()
    );
    assert_eq!(
        lib.sensor_read(&ids[0]).unwrap(),
        Some((SensorKind::Bayer, now))
    );
    // A replacement whose read cannot tell is forgotten, not kept as the old one.
    lib.forget_sensor_kind(&ids[0]).unwrap();
    assert_eq!(lib.sensor_kind(&ids[0]).unwrap(), None);
    assert!(
        !lib.record_sensor_kind("no-such-asset", SensorKind::Bayer, &old)
            .unwrap()
    );
}

#[test]
fn a_relink_drops_a_kind_read_from_another_file_than_the_registered_one() {
    let d = tempfile::tempdir().unwrap();
    let (lib, ids, paths) = session(d.path(), &["A.DNG", "B.DNG"]);
    let originals: Vec<Vec<u8>> = paths.iter().map(|p| fs::read(p).unwrap()).collect();
    // A: read from its registered file. B: read from a Bayer file that took
    // its place, which the library has not registered.
    let a = fingerprint(&paths[0]).unwrap();
    assert!(
        lib.record_sensor_kind(&ids[0], SensorKind::XTrans, &a)
            .unwrap()
    );
    fs::write(&paths[1], b"a Bayer file took its place").unwrap();
    let replacement = fingerprint(&paths[1]).unwrap();
    assert!(
        lib.record_sensor_kind(&ids[1], SensorKind::Bayer, &replacement)
            .unwrap()
    );
    // Both originals are found on a backup and relinked (Locate Photo).
    let backup = lib.directory.join("Capture/Backup");
    fs::create_dir_all(&backup).unwrap();
    for ((id, path), bytes) in ids.iter().zip(&paths).zip(&originals) {
        fs::remove_file(path).unwrap();
        let found = backup.join(path.file_name().unwrap());
        fs::write(&found, bytes).unwrap();
        lib.relink(id, &found, false).unwrap();
    }
    assert_eq!(lib.sensor_kind(&ids[0]).unwrap(), Some(SensorKind::XTrans));
    assert_eq!(lib.sensor_kind(&ids[1]).unwrap(), None);
}

#[test]
fn a_new_registered_file_drops_the_kind() {
    let d = tempfile::tempdir().unwrap();
    let (lib, ids, paths) = session(d.path(), &["A.RAF", "B.RAF"]);
    for (id, path) in ids.iter().zip(&paths) {
        let read = fingerprint(path).unwrap();
        assert!(
            lib.record_sensor_kind(id, SensorKind::XTrans, &read)
                .unwrap()
        );
    }
    // Another column of the asset keeps it.
    let db = rusqlite::Connection::open(lib.directory.join("session.sqlite")).unwrap();
    db.execute("UPDATE assets SET stack=stack WHERE id=?", [&ids[1]])
        .unwrap();
    assert_eq!(lib.sensor_kind(&ids[1]).unwrap(), Some(SensorKind::XTrans));
    // A save in place registers the new file: the kind is read again.
    fs::write(&paths[0], b"saved by another program, other size").unwrap();
    lib.file_saved(&ids[0]).unwrap();
    assert_eq!(lib.sensor_kind(&ids[0]).unwrap(), None);
    let rows: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM sensor_kinds WHERE asset_id=?",
            [&ids[0]],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(rows, 0, "the stale row is gone, not only hidden");
    assert_eq!(lib.sensor_kind(&ids[1]).unwrap(), Some(SensorKind::XTrans));
}

#[test]
fn an_older_library_takes_the_table_and_upgrading_again_changes_nothing() {
    let d = tempfile::tempdir().unwrap();
    let (lib, ids, paths) = session(d.path(), &["A.RAF"]);
    drop(lib);
    let db = rusqlite::Connection::open(d.path().join("Job/session.sqlite")).unwrap();
    db.execute_batch(include_str!("undo_0038.sql")).unwrap();
    drop(db);
    let lib = Library::open(&d.path().join("Job")).unwrap();
    assert_eq!(lib.sensor_kind(&ids[0]).unwrap(), None);
    let read = fingerprint(&paths[0]).unwrap();
    assert!(
        lib.record_sensor_kind(&ids[0], SensorKind::Bayer, &read)
            .unwrap()
    );
    drop(lib);
    // Opening the upgraded library again keeps what it holds.
    let lib = Library::open(&d.path().join("Job")).unwrap();
    assert_eq!(lib.sensor_kind(&ids[0]).unwrap(), Some(SensorKind::Bayer));
}

#[test]
fn the_kind_is_never_unknown_on_the_wire() {
    assert_eq!(serde_json::to_value(SensorKind::XTrans).unwrap(), "xTrans");
    assert_eq!(serde_json::to_value(SensorKind::Bayer).unwrap(), "bayer");
    assert!(serde_json::from_str::<SensorKind>("\"unknown\"").is_err());
}
