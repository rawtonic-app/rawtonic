use rawtonic_catalog::{Library, Mode};
use rawtonic_ingest::transfer::{copy_library, verify};
use serde_json::json;
use std::{
    fs,
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};

fn cancel() -> AtomicBool {
    AtomicBool::new(false)
}
fn catalog(root: &Path) -> (Library, String) {
    let lib = Library::create(&root.join("library"), "Photographs", Mode::Catalog).unwrap();
    assert_eq!(lib.manifest.capture, "Photos");
    let path = lib.directory.join("Photos/image.arw");
    fs::write(&path, b"internal original").unwrap();
    let asset = lib.add_asset(&path).unwrap();
    (lib, asset.id)
}

#[test]
fn full_backup_restores_after_all_sources_disappear_and_moves_again() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let (lib, internal) = catalog(&root);
    let ext = root.join("external");
    fs::create_dir(&ext).unwrap();
    let photo = ext.join("image.arw");
    fs::write(&photo, b"external original").unwrap();
    let external = lib.add_asset(&photo).unwrap().id;
    let lut = ext.join("look.cube");
    fs::write(&lut, b"LUT fixture").unwrap();
    let profile = ext.join("camera.dcp");
    fs::write(&profile, b"profile fixture").unwrap();
    let state = json!({"version": 1, "rating": 4, "tags": ["user:Travel"], "adjustments": {
        "exposure": 1, "lutPath": lut, "cameraProfile": profile,
        "masks": [{"data": "embedded mask", "adjustments": {"lutPath": lut}}]
    }});
    lib.save_editor_state(&external, &state).unwrap();
    lib.save_copy_state(&external, "abcdef", &state).unwrap();
    let mut later = state.clone();
    later["adjustments"]["exposure"] = json!(2);
    lib.save_editor_state(&external, &later).unwrap();
    let album = lib.create_collection("Travel", None).unwrap();
    lib.add_to_collection(&album.id, &[internal.clone(), external.clone()])
        .unwrap();
    fs::write(photo.with_extension("xmp"), b"keep foreign XMP exactly").unwrap();
    fs::create_dir_all(lib.directory.join("previews")).unwrap();
    fs::write(lib.directory.join("previews/disposable.jpg"), b"cache").unwrap();
    let backup = root.join("backup");
    let receipt = copy_library(&lib.directory, &backup, "backup", &cancel(), |_| {}).unwrap();
    assert_eq!(receipt.photographs, 2);
    assert!(!backup.join("previews/disposable.jpg").exists());
    // Snapshot works while the source connection is open and includes WAL.
    assert_eq!(
        lib.editor_state(&external).unwrap().unwrap()["adjustments"]["lutPath"],
        json!(lut)
    );
    drop(lib);
    fs::remove_dir_all(root.join("library")).unwrap();
    fs::remove_dir_all(&ext).unwrap();
    verify(&backup, &cancel(), |_| {}).unwrap();
    let before = fs::read(backup.join("catalog.sqlite")).unwrap();
    let restored = root.join("restored");
    copy_library(&backup, &restored, "restore", &cancel(), |_| {}).unwrap();
    assert_eq!(fs::read(backup.join("catalog.sqlite")).unwrap(), before);
    verify(&backup, &cancel(), |_| {}).unwrap();
    fs::remove_dir_all(&backup).unwrap();
    let moved = root.join("another computer");
    fs::rename(restored, &moved).unwrap();
    let lib = Library::open(&moved).unwrap();
    assert_eq!(lib.verify_paths().unwrap(), 0);
    assert_eq!(lib.collection_assets(&album.id).unwrap().len(), 2);
    assert_eq!(
        fs::read(lib.asset(&internal).unwrap().path).unwrap(),
        b"internal original"
    );
    let external_path = lib.asset(&external).unwrap().path;
    assert_eq!(fs::read(&external_path).unwrap(), b"external original");
    assert_eq!(
        fs::read(external_path.with_extension("xmp")).unwrap(),
        b"keep foreign XMP exactly"
    );
    let actual = lib.editor_state(&external).unwrap().unwrap();
    assert_eq!(actual["rating"], 4);
    assert_eq!(actual["adjustments"]["exposure"], 2);
    for (field, expected) in [
        ("lutPath", b"LUT fixture".as_slice()),
        ("cameraProfile", b"profile fixture".as_slice()),
    ] {
        let path = Path::new(actual["adjustments"][field].as_str().unwrap());
        assert!(path.starts_with(&moved));
        assert_eq!(fs::read(path).unwrap(), expected);
    }
    let copy = lib.copy_state(&external, "abcdef").unwrap().unwrap();
    assert_eq!(copy["adjustments"]["exposure"], 1);
    assert_eq!(
        copy["adjustments"]["lutPath"],
        actual["adjustments"]["lutPath"]
    );
    assert!(lib.undo(&lib.primary_version(&external).unwrap()).unwrap());
    assert_eq!(
        lib.editor_state(&external).unwrap().unwrap()["adjustments"]["exposure"],
        1
    );
    // Saving resolved resource paths must not break the next move.
    lib.save_editor_state(&external, &actual).unwrap();
    lib.save_copy_state(&external, "abcdef", &copy).unwrap();
    let stored = lib
        .recipe(&lib.primary_version(&external).unwrap())
        .unwrap()
        .unwrap();
    assert!(
        stored["editor_state"]["adjustments"]["lutPath"]
            .as_str()
            .unwrap()
            .starts_with("rawtonic-resource:")
    );
}

#[test]
fn failed_or_cancelled_copies_never_publish_and_never_overwrite() {
    let tmp = tempfile::tempdir().unwrap();
    let (lib, id) = catalog(tmp.path());
    let to = tmp.path().join("copy");
    let cancelled = cancel();
    assert!(
        copy_library(&lib.directory, &to, "transfer", &cancelled, |_| cancelled
            .store(true, Ordering::Release))
        .is_err()
    );
    assert!(!to.exists());
    assert!(!fs::read_dir(tmp.path()).unwrap().any(|e| {
        e.unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".rawtonic-copy-")
    }));
    lib.save_editor_state(
        &id,
        &json!({"adjustments":{"lutPath": tmp.path().join("missing.cube")}}),
    )
    .unwrap();
    assert!(
        copy_library(&lib.directory, &to, "backup", &cancel(), |_| {})
            .unwrap_err()
            .to_string()
            .contains("resource is missing")
    );
    assert!(!to.exists());
    fs::remove_file(lib.asset(&id).unwrap().path).unwrap();
    assert!(
        copy_library(&lib.directory, &to, "backup", &cancel(), |_| {})
            .unwrap_err()
            .to_string()
            .contains("Originals unavailable")
    );
    fs::create_dir(&to).unwrap();
    fs::write(to.join("keep"), b"existing destination").unwrap();
    assert!(copy_library(&lib.directory, &to, "transfer", &cancel(), |_| {}).is_err());
    assert_eq!(fs::read(to.join("keep")).unwrap(), b"existing destination");
}

#[test]
fn corrupted_or_opened_backup_is_refused_before_database_open() {
    let tmp = tempfile::tempdir().unwrap();
    let (lib, _) = catalog(tmp.path());
    let backup = tmp.path().join("backup");
    copy_library(&lib.directory, &backup, "backup", &cancel(), |_| {}).unwrap();
    let target = tmp.path().join("restored");
    fs::write(backup.join("catalog.sqlite-wal"), b"unverified WAL").unwrap();
    assert!(
        copy_library(&backup, &target, "restore", &cancel(), |_| {})
            .unwrap_err()
            .to_string()
            .contains("unverified file")
    );
    assert!(!target.exists());
    fs::remove_file(backup.join("catalog.sqlite-wal")).unwrap();
    fs::write(backup.join("Photos/image.arw"), b"corrupted").unwrap();
    assert!(
        copy_library(&backup, &target, "restore", &cancel(), |_| {})
            .unwrap_err()
            .to_string()
            .contains("verification failed")
    );
    assert!(!target.exists());
}

#[test]
fn changed_original_cannot_silently_receive_another_photos_edits() {
    let tmp = tempfile::tempdir().unwrap();
    let (lib, id) = catalog(tmp.path());
    fs::write(lib.asset(&id).unwrap().path, b"replaced original").unwrap();
    let to = tmp.path().join("copy");
    assert!(
        copy_library(&lib.directory, &to, "transfer", &cancel(), |_| {})
            .unwrap_err()
            .to_string()
            .contains("Original has changed")
    );
    assert!(!to.exists());
}

#[test]
fn catalogs_sort_default_imports_by_date_but_honor_explicit_folders() {
    let tmp = tempfile::tempdir().unwrap();
    let (lib, _) = catalog(tmp.path());
    let source = tmp.path().join("source.arw");
    fs::write(&source, b"date fallback fixture").unwrap();
    let modified = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_704_196_800); // 2024-01-02 12:00 UTC
    fs::File::options()
        .write(true)
        .open(&source)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(modified))
        .unwrap();
    let result = rawtonic_ingest::import_one(&lib, &source, "{orig}", 1, None, &cancel()).unwrap();
    assert_eq!(
        result
            .destination
            .unwrap()
            .strip_prefix(&lib.directory)
            .unwrap(),
        Path::new("Photos/2024/2024-01-02/source.arw")
    );
    fs::write(&source, b"explicit folder fixture").unwrap();
    let capture = lib.directory.join("Photos");
    let destination = rawtonic_ingest::destination_folder(&lib, Some(&capture), &[]).unwrap();
    let result = rawtonic_ingest::import_one_into(
        &lib,
        &source,
        &destination,
        "{orig}",
        2,
        1,
        None,
        &cancel(),
    )
    .unwrap();
    assert_eq!(result.destination.unwrap(), capture.join("source.arw"));
}

#[test]
fn exif_date_takes_precedence_over_file_modification_date() {
    let tmp = tempfile::tempdir().unwrap();
    let (lib, _) = catalog(tmp.path());
    let source = tmp.path().join("camera.tiff");
    // Little-endian TIFF with a single ASCII DateTime field in its primary IFD.
    let mut tiff = b"II\x2a\x00\x08\x00\x00\x00\x01\x00\x32\x01\x02\x00\x14\x00\x00\x00\x1a\x00\x00\x00\x00\x00\x00\x00".to_vec();
    tiff.extend_from_slice(b"2023:07:14 18:30:00\0");
    fs::write(&source, tiff).unwrap();
    let result = rawtonic_ingest::import_one(&lib, &source, "{orig}", 1, None, &cancel()).unwrap();
    assert_eq!(
        result.destination.unwrap(),
        lib.directory.join("Photos/2023/2023-07-14/camera.tiff")
    );
}

#[test]
fn project_layout_and_file_dates_survive_full_backup_restore() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = Library::create(&tmp.path().join("project"), "Shoot", Mode::Session).unwrap();
    let photo = lib.directory.join("Capture/a.arw");
    fs::write(&photo, b"project original").unwrap();
    let modified = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_704_196_800);
    fs::File::options()
        .write(true)
        .open(&photo)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(modified))
        .unwrap();
    let asset = lib.add_asset(&photo).unwrap();
    let backup = tmp.path().join("backup");
    copy_library(&lib.directory, &backup, "backup", &cancel(), |_| {}).unwrap();
    let restore = tmp.path().join("restored");
    copy_library(&backup, &restore, "restore", &cancel(), |_| {}).unwrap();
    let restored = Library::open(&restore).unwrap();
    assert_eq!(restored.manifest.mode, Mode::Session);
    assert_eq!(restored.manifest.capture, "Capture");
    let path = restored.asset(&asset.id).unwrap().path;
    assert_eq!(path, restored.directory.join("Capture/a.arw"));
    assert_eq!(fs::metadata(path).unwrap().modified().unwrap(), modified);
}
