//! The corrected capture time leaves the library in the photograph's XMP
//! sidecar (`exif:DateTimeOriginal`, `photoshop:DateCreated`) and comes back
//! from it: written with the library's other metadata, removed on reset,
//! read from another library's or Lightroom's sidecar as a correction.
//! Sidecars are the real Lightroom Classic 15.6 and Capture One 16.6 packets
//! of `crates/metadata/tests/fixtures`; the photographs are synthetic bytes
//! whose SHA-256 must never change.

use rawtonic_catalog::capture_time::{CaptureChange, CaptureTime, time};
use rawtonic_catalog::{CaptureFacts, Library, Mode};
use rawtonic_metadata::{XmpDateTime, merge_xmp, read_capture_time, read_xmp};
use std::fs;
use std::path::{Path, PathBuf};

const LIGHTROOM: &str = include_str!("../../metadata/tests/fixtures/lightroom-classic-15.6-de.xmp");
const C1_KEYWORDS: &str =
    include_str!("../../metadata/tests/fixtures/capture-one-16.6-keywords.xmp");
/// What the Lightroom sidecar states as the capture time (its camera's).
const LIGHTROOM_TIME: &str = "2020-10-18T13:34:44";

fn facts(captured: &str) -> CaptureFacts {
    CaptureFacts {
        captured: Some(captured.into()),
        model: Some("Camera".into()),
        ..CaptureFacts::default()
    }
}

fn sha256(path: &Path) -> String {
    rawtonic_fs::sha256_hex(fs::read(path).unwrap())
}

/// A session with `photo` (synthetic bytes) and, when given, its XMP
/// sidecar, added with its sidecar as an import does.
fn import(dir: &Path, photo: &str, sidecar: Option<&str>) -> (Library, String, PathBuf) {
    let lib = Library::create(dir, "Job", Mode::Session).unwrap();
    let path = lib.directory.join("Capture").join(photo);
    fs::write(&path, format!("raw {photo}")).unwrap();
    if let Some(text) = sidecar {
        fs::write(path.with_extension("xmp"), text).unwrap();
    }
    lib.add_files(std::slice::from_ref(&path)).unwrap();
    let id = lib.asset_at(&path).unwrap().unwrap().id;
    (lib, id, path)
}

fn corrected() -> CaptureTime {
    time("2020-10-18 11:34:44", Some("07"), Some("+02:00")).unwrap()
}

fn set(lib: &Library, id: &str, expected: Option<CaptureTime>, new: Option<CaptureTime>) {
    let conflicts = lib
        .set_capture_times(&[CaptureChange {
            asset_id: id.into(),
            expected,
            new,
        }])
        .unwrap();
    assert!(conflicts.is_empty());
}

fn sidecar(path: &Path) -> String {
    fs::read_to_string(path.with_extension("xmp")).unwrap()
}

fn stated(path: &Path) -> Option<String> {
    read_capture_time(&sidecar(path))
        .unwrap()
        .map(|t| t.to_xmp())
}

#[test]
fn a_correction_goes_to_the_sidecar_comes_back_from_it_and_resets_exactly() {
    let d = tempfile::tempdir().unwrap();
    let (lib, id, photo) = import(&d.path().join("one"), "A.ORF", Some(LIGHTROOM));
    let camera_file = sha256(&photo);
    lib.record_facts(&id, &facts("2020-10-18 13:34:44"))
        .unwrap();
    // The camera's own time in the sidecar is no correction.
    assert_eq!(lib.capture_override(&id).unwrap(), None);

    // Uncorrected, a write is exactly what the writer gave before: the
    // camera's date as Lightroom wrote it, nothing added.
    let mut meta = lib.metadata(&id).unwrap();
    meta.rating = 3;
    lib.set_metadata(&id, &meta).unwrap();
    let before = sidecar(&photo);
    lib.sync_xmp(&id).unwrap();
    let plain = sidecar(&photo);
    let recipe = lib.recipe(&lib.primary_version(&id).unwrap()).unwrap();
    assert_eq!(
        plain,
        merge_xmp(Some(&before), &meta, recipe.as_ref()).unwrap()
    );
    assert!(plain.contains(r#"exif:DateTimeOriginal="2020-10-18T13:34:44""#));

    // Corrected: both dates, with fraction and offset.
    set(&lib, &id, None, Some(corrected()));
    assert_eq!(lib.summary().unwrap().xmp_pending, 1, "the sidecar follows");
    lib.write_pending_xmp().unwrap();
    let written = sidecar(&photo);
    assert_eq!(
        stated(&photo).as_deref(),
        Some("2020-10-18T11:34:44.07+02:00")
    );
    assert!(
        written.contains(
            "<photoshop:DateCreated>2020-10-18T11:34:44.07+02:00</photoshop:DateCreated>"
        )
    );
    assert_eq!(written.matches("DateTimeOriginal").count(), 2);
    // Written again unchanged: the same bytes.
    lib.sync_xmp(&id).unwrap();
    assert_eq!(sidecar(&photo), written);

    // Another library reading the photograph and its sidecar takes the
    // correction as it was written, fraction and offset kept.
    let other_dir = d.path().join("two");
    let (other, other_id, other_photo) = import(&other_dir, "A.ORF", Some(&written));
    assert_eq!(
        other.capture_override(&other_id).unwrap(),
        None,
        "file not read yet"
    );
    other
        .record_facts(&other_id, &facts("2020-10-18 13:34:44"))
        .unwrap();
    assert_eq!(
        other.capture_override(&other_id).unwrap(),
        Some(corrected())
    );
    // And a later write there keeps the sidecar as it is.
    other.sync_xmp(&other_id).unwrap();
    assert_eq!(
        stated(&other_photo).as_deref(),
        Some("2020-10-18T11:34:44.07+02:00")
    );

    // Reset: exactly the dates written go; the IPTC Date Created the
    // library holds from Lightroom comes back as an IPTC field.
    set(&lib, &id, Some(corrected()), None);
    lib.write_pending_xmp().unwrap();
    let reset = sidecar(&photo);
    assert_eq!(stated(&photo), None);
    assert_eq!(
        read_xmp(&reset).unwrap().0.iptc["dateCreated"],
        "2026-05-01T10:20:30"
    );
    // Then left alone: a further write gives the same bytes.
    lib.sync_xmp(&id).unwrap();
    assert_eq!(sidecar(&photo), reset);
    // Redo and undo stay one step each.
    set(&lib, &id, None, Some(corrected()));
    lib.write_pending_xmp().unwrap();
    assert_eq!(sidecar(&photo), written);
    set(&lib, &id, Some(corrected()), None);
    lib.write_pending_xmp().unwrap();
    assert_eq!(sidecar(&photo), reset);

    assert_eq!(
        sha256(&photo),
        camera_file,
        "the camera file is never written"
    );
}

#[test]
fn a_photo_without_a_sidecar_gets_one_only_with_a_correction() {
    let d = tempfile::tempdir().unwrap();
    let (lib, id, photo) = import(d.path(), "B.NEF", None);
    let camera_file = sha256(&photo);
    lib.record_facts(&id, &facts("2026-10-30 12:00:00"))
        .unwrap();
    assert_eq!(lib.summary().unwrap().xmp_pending, 0);
    assert!(!photo.with_extension("xmp").exists());
    let at = time("2026-10-30 10:00:00", None, None).unwrap();
    set(&lib, &id, None, Some(at.clone()));
    lib.write_pending_xmp().unwrap();
    // No offset known: none invented.
    assert_eq!(stated(&photo).as_deref(), Some("2026-10-30T10:00:00"));
    set(&lib, &id, Some(at), None);
    lib.write_pending_xmp().unwrap();
    assert_eq!(stated(&photo), None);
    assert!(!sidecar(&photo).contains("DateCreated"));
    assert_eq!(sha256(&photo), camera_file);
}

#[test]
fn lightroom_s_edited_capture_time_imports_as_a_correction_and_equal_ones_do_not() {
    let d = tempfile::tempdir().unwrap();
    // Lightroom's Edit Capture Time moved the shot two hours later.
    let shifted = LIGHTROOM.replace(LIGHTROOM_TIME, "2020-10-18T15:34:44");
    let (lib, id, photo) = import(&d.path().join("lr"), "A.ORF", Some(&shifted));
    let camera_file = sha256(&photo);
    lib.record_facts(&id, &facts("2020-10-18 13:34:44"))
        .unwrap();
    assert_eq!(
        lib.capture_override(&id).unwrap(),
        Some(time("2020-10-18 15:34:44", None, None).unwrap())
    );
    // Rereading the file's facts later changes nothing.
    lib.record_facts(&id, &facts("2020-10-18 13:34:44"))
        .unwrap();
    assert!(lib.capture_override(&id).unwrap().is_some());
    // The library owns that value now: a reset removes Lightroom's date.
    set(
        &lib,
        &id,
        Some(time("2020-10-18 15:34:44", None, None).unwrap()),
        None,
    );
    lib.write_pending_xmp().unwrap();
    assert_eq!(stated(&photo), None);
    assert_eq!(sha256(&photo), camera_file);

    // Equal to the file's time (Capture One's `Z` after local time): none.
    let (c1, c1_id, _) = import(&d.path().join("c1"), "B.CR3", Some(C1_KEYWORDS));
    c1.record_facts(&c1_id, &facts("2019-01-24 12:29:49"))
        .unwrap();
    assert_eq!(c1.capture_override(&c1_id).unwrap(), None);

    // Facts known before the sidecar is read (Use XMP of an adopted one).
    let (late, late_id, late_photo) = import(&d.path().join("late"), "C.ORF", None);
    late.record_facts(&late_id, &facts("2020-10-18 13:34:44"))
        .unwrap();
    fs::write(late_photo.with_extension("xmp"), &shifted).unwrap();
    late.adopt_xmp(&late_id).unwrap();
    assert_eq!(
        late.capture_override(&late_id).unwrap().unwrap().local,
        "2020-10-18 15:34:44"
    );
}

#[test]
fn a_sidecar_changed_elsewhere_follows_the_conflict_the_photographer_resolves() {
    let d = tempfile::tempdir().unwrap();
    let (lib, id, photo) = import(d.path(), "A.ORF", Some(LIGHTROOM));
    lib.record_facts(&id, &facts("2020-10-18 13:34:44"))
        .unwrap();
    set(&lib, &id, None, Some(corrected()));
    lib.write_pending_xmp().unwrap();
    let ours = sidecar(&photo);
    // Lightroom, which never saw the correction, writes its own later time.
    let theirs = ours.replace("2020-10-18T11:34:44.07+02:00", "2020-10-18T12:00:00");
    fs::write(photo.with_extension("xmp"), &theirs).unwrap();
    assert!(
        lib.sync_xmp(&id).is_err(),
        "a changed sidecar is a conflict"
    );

    // Use XMP: the sidecar's time replaces the library's correction ...
    let undo = lib.use_xmp(&id).unwrap();
    let lightroom = time("2020-10-18 12:00:00", None, None).unwrap();
    assert_eq!(lib.capture_override(&id).unwrap(), Some(lightroom.clone()));
    // ... and its undo brings the library's back.
    lib.undo_use_xmp(&undo).unwrap();
    assert_eq!(lib.capture_override(&id).unwrap(), Some(corrected()));

    // Keep library: the library's correction replaces the sidecar's.
    lib.keep_library_xmp(&id).unwrap();
    assert_eq!(
        stated(&photo).as_deref(),
        Some("2020-10-18T11:34:44.07+02:00")
    );

    // A sidecar back at the file's time read with Use XMP: no correction.
    let camera = sidecar(&photo).replace("2020-10-18T11:34:44.07+02:00", LIGHTROOM_TIME);
    fs::write(photo.with_extension("xmp"), &camera).unwrap();
    assert!(lib.sync_xmp(&id).is_err());
    lib.use_xmp(&id).unwrap();
    assert_eq!(lib.capture_override(&id).unwrap(), None);
    // A sidecar that states no capture time keeps the library's.
    set(&lib, &id, None, Some(lightroom.clone()));
    let mut without = read_xmp(&camera).unwrap().0;
    without.iptc.remove("dateCreated");
    let none = merge_xmp(None, &without, None).unwrap();
    assert_eq!(read_capture_time(&none).unwrap(), None::<XmpDateTime>);
    fs::write(photo.with_extension("xmp"), &none).unwrap();
    assert!(lib.sync_xmp(&id).is_err());
    lib.use_xmp(&id).unwrap();
    assert_eq!(lib.capture_override(&id).unwrap(), Some(lightroom));
}

#[test]
fn a_library_of_schema_26_opens_and_writes_corrections() {
    let d = tempfile::tempdir().unwrap();
    let (lib, id, photo) = import(d.path(), "A.ORF", Some(LIGHTROOM));
    lib.record_facts(&id, &facts("2020-10-18 13:34:44"))
        .unwrap();
    drop(lib);
    let db = rusqlite::Connection::open(d.path().join("session.sqlite")).unwrap();
    db.execute_batch(include_str!("undo_0027.sql")).unwrap();
    drop(db);
    let lib = Library::open(d.path()).unwrap();
    set(&lib, &id, None, Some(corrected()));
    lib.write_pending_xmp().unwrap();
    assert_eq!(
        stated(&photo).as_deref(),
        Some("2020-10-18T11:34:44.07+02:00")
    );
}
