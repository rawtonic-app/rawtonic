//! Corrected capture times: kept apart from the file's facts, found by
//! search, carried in the EXIF of the editor state, written all or none.

use rawtonic_catalog::capture_time::{CaptureChange, time};
use rawtonic_catalog::query::Rule;
use rawtonic_catalog::{CaptureFacts, Field, Library, Mode};
use serde_json::json;
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

fn facts(captured: &str) -> CaptureFacts {
    CaptureFacts {
        captured: Some(captured.into()),
        model: Some("Camera".into()),
        ..CaptureFacts::default()
    }
}

fn change(id: &str, expected: Option<&str>, new: Option<&str>) -> CaptureChange {
    let at = |t: &str| time(t, Some("05"), Some("+02:00")).unwrap();
    CaptureChange {
        asset_id: id.into(),
        expected: expected.map(at),
        new: new.map(at),
    }
}

fn names(found: Vec<PathBuf>) -> Vec<String> {
    let mut names: Vec<_> = found
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn on_day(day: &str) -> Rule {
    serde_json::from_value(json!({"field": "captureDate", "op": "is", "value": day})).unwrap()
}

#[test]
fn a_correction_outlives_a_reread_of_the_file_and_reset_gives_it_back() {
    let d = tempfile::tempdir().unwrap();
    let (lib, ids, _) = session(d.path(), &["A.ARW", "B.ARW"]);
    lib.record_facts(&ids[0], &facts("2026-10-30 23:30:00"))
        .unwrap();
    lib.record_facts(&ids[1], &facts("2026-10-30 12:00:00"))
        .unwrap();
    assert!(
        lib.set_capture_times(&[change(&ids[0], None, Some("2026-10-31 00:30:00"))])
            .unwrap()
            .is_empty()
    );
    // The facts pass (or a relink) records the file's time again.
    lib.record_facts(&ids[0], &facts("2026-10-30 23:30:00"))
        .unwrap();
    assert_eq!(
        lib.facts(&ids[0]).unwrap().unwrap().0.captured.as_deref(),
        Some("2026-10-30 23:30:00"),
        "the file's own time stays as the file says"
    );
    let kept = lib.capture_override(&ids[0]).unwrap().unwrap();
    assert_eq!(kept.local, "2026-10-31 00:30:00");
    assert_eq!(kept.subsec.as_deref(), Some("05"));
    assert_eq!(kept.offset.as_deref(), Some("+02:00"));
    // Search and value lists see the corrected day.
    assert_eq!(names(lib.search(&on_day("2026-10-31")).unwrap()), ["A.ARW"]);
    assert_eq!(names(lib.search(&on_day("2026-10-30")).unwrap()), ["B.ARW"]);
    let months = lib
        .facets(&Rule::everything(), &[Field::CaptureDate])
        .unwrap();
    assert_eq!(months[&Field::CaptureDate].len(), 1);
    // Reset: the file's time again.
    assert!(
        lib.set_capture_times(&[change(&ids[0], Some("2026-10-31 00:30:00"), None)])
            .unwrap()
            .is_empty()
    );
    assert_eq!(lib.capture_override(&ids[0]).unwrap(), None);
    assert_eq!(
        names(lib.search(&on_day("2026-10-30")).unwrap()),
        ["A.ARW", "B.ARW"]
    );
}

#[test]
fn a_changed_correction_refuses_the_whole_batch() {
    let d = tempfile::tempdir().unwrap();
    let (lib, ids, _) = session(d.path(), &["A.ARW", "B.ARW"]);
    lib.set_capture_times(&[change(&ids[1], None, Some("2026-01-01 00:00:00"))])
        .unwrap();
    // Planned when B had no correction: B conflicts, and A is not written either.
    let conflicts = lib
        .set_capture_times(&[
            change(&ids[0], None, Some("2026-02-02 00:00:00")),
            change(&ids[1], None, Some("2026-02-02 00:00:00")),
            change("gone", None, Some("2026-02-02 00:00:00")),
        ])
        .unwrap();
    assert_eq!(conflicts, [ids[1].clone(), "gone".to_string()]);
    assert_eq!(lib.capture_override(&ids[0]).unwrap(), None);
    assert_eq!(
        lib.capture_override(&ids[1]).unwrap().unwrap().local,
        "2026-01-01 00:00:00"
    );
}

#[test]
fn the_editor_state_s_exif_carries_the_correction_also_when_saved_stale() {
    let d = tempfile::tempdir().unwrap();
    let (lib, ids, _) = session(d.path(), &["A.ARW"]);
    let stale = json!({
        "version": 1, "rating": 2, "adjustments": {"exposure": 0.5},
        "exif": {"Make": "Maker", "DateTimeOriginal": "2026-10-30 23:30:00", "SubSecTimeOriginal": "05"}
    });
    lib.save_editor_state(&ids[0], &stale).unwrap();
    lib.set_capture_times(&[CaptureChange {
        asset_id: ids[0].clone(),
        expected: None,
        new: Some(time("2026-10-31 00:30:00", None, None).unwrap()),
    }])
    .unwrap();
    let read = lib.editor_state(&ids[0]).unwrap().unwrap();
    assert_eq!(read["exif"]["DateTimeOriginal"], "2026-10-31 00:30:00");
    assert!(read["exif"].get("SubSecTimeOriginal").is_none());
    assert_eq!(read["exif"]["Make"], "Maker");
    // An editor that read the state before the correction saves over it.
    lib.save_editor_state(&ids[0], &stale).unwrap();
    let stored = lib
        .recipe(&lib.primary_version(&ids[0]).unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(
        stored["editor_state"]["exif"]["DateTimeOriginal"],
        "2026-10-31 00:30:00"
    );
    assert_eq!(stored["editor_state"]["adjustments"]["exposure"], 0.5);
    assert_eq!(lib.asset(&ids[0]).unwrap().rating, 2);
}

#[test]
fn a_library_of_schema_18_opens_with_no_corrections_and_takes_them() {
    let d = tempfile::tempdir().unwrap();
    let (lib, ids, _) = session(d.path(), &["A.ARW"]);
    lib.record_facts(&ids[0], &facts("2026-10-30 12:00:00"))
        .unwrap();
    drop(lib);
    let db = rusqlite::Connection::open(d.path().join("Job/session.sqlite")).unwrap();
    db.execute_batch(include_str!("undo_0019.sql")).unwrap();
    drop(db);
    let lib = Library::open(&d.path().join("Job")).unwrap();
    assert_eq!(lib.capture_override(&ids[0]).unwrap(), None);
    assert_eq!(names(lib.search(&on_day("2026-10-30")).unwrap()), ["A.ARW"]);
    assert!(
        lib.set_capture_times(&[change(&ids[0], None, Some("2026-10-29 12:00:00"))])
            .unwrap()
            .is_empty()
    );
    assert_eq!(names(lib.search(&on_day("2026-10-29")).unwrap()), ["A.ARW"]);
}
