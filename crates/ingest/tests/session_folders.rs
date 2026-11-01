//! Captures and imports go to the session's Capture folder wherever it is
//! set (G38): outside the session the copy is registered as the session's
//! photograph; a Capture folder whose drive is away refuses the capture and
//! nothing is written anywhere else.
use rawtonic_catalog::session_folders::FolderRole;
use rawtonic_catalog::{Library, Mode};
use rawtonic_ingest::{destination_folder, import_capture};
use std::fs;
use std::sync::atomic::AtomicBool;

#[test]
fn a_capture_goes_to_a_capture_folder_outside_the_session() {
    let d = tempfile::tempdir().unwrap();
    let mut lib = Library::create(&d.path().join("Job"), "Job", Mode::Session).unwrap();
    let drive = d.path().join("Drive/Job Capture");
    fs::create_dir_all(&drive).unwrap();
    let drive = drive.canonicalize().unwrap();
    lib.set_session_folder(FolderRole::Capture, &drive).unwrap();
    let source = d.path().join("A.ARW");
    fs::write(&source, b"capture").unwrap();

    let result = import_capture(&lib, &source, "{orig}", 1, None, &AtomicBool::new(false)).unwrap();
    assert!(result.error.is_none(), "{result:?}");
    let copy = result.destination.unwrap();
    assert_eq!(copy, drive.join("A.ARW"));
    assert_eq!(fs::read(&copy).unwrap(), b"capture");
    let asset = lib.asset(&result.asset_id.unwrap()).unwrap();
    assert_eq!(asset.path, copy);
    assert!(
        fs::read_dir(lib.directory.join("Capture"))
            .unwrap()
            .next()
            .is_none()
    );

    // A folder below it is accepted too; one elsewhere outside is not.
    let day = drive.join("Day 2");
    fs::create_dir(&day).unwrap();
    assert!(destination_folder(&lib, Some(&day), &[]).is_ok());
    let stray = d.path().join("Stray");
    fs::create_dir(&stray).unwrap();
    assert!(destination_folder(&lib, Some(&stray), &[]).is_err());
}

#[test]
fn a_capture_folder_whose_drive_is_away_refuses_the_capture() {
    let d = tempfile::tempdir().unwrap();
    let mut lib = Library::create(&d.path().join("Job"), "Job", Mode::Session).unwrap();
    let drive = d.path().join("Drive/Capture");
    fs::create_dir_all(&drive).unwrap();
    lib.set_session_folder(FolderRole::Capture, &drive).unwrap();
    fs::remove_dir_all(d.path().join("Drive")).unwrap();
    let source = d.path().join("A.ARW");
    fs::write(&source, b"capture").unwrap();

    let error = import_capture(&lib, &source, "{orig}", 1, None, &AtomicBool::new(false))
        .unwrap_err()
        .to_string();
    assert!(error.contains("not available"), "{error}");
    assert!(!d.path().join("Drive").exists());
    assert!(
        fs::read_dir(lib.directory.join("Capture"))
            .unwrap()
            .next()
            .is_none()
    );
    assert!(lib.assets(None).unwrap().is_empty());
    assert_eq!(fs::read(&source).unwrap(), b"capture");
}
