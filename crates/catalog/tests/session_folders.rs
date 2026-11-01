//! A session's configurable Capture, Selects and Output folders (G38):
//! stored relative inside the session and absolute outside it, never moving
//! files by themselves, refusing folders that hold the session's own data,
//! and reporting a folder whose drive is away instead of making one.
use rawtonic_catalog::session_folders::{FolderRole, outside_folders_of};
use rawtonic_catalog::{Library, Mode};
use serde_json::json;
use std::fs;
use std::path::Path;

fn session(dir: &Path) -> Library {
    Library::create(&dir.join("Job"), "Job", Mode::Session).unwrap()
}

fn manifest(lib: &Library) -> serde_json::Value {
    serde_json::from_slice(&fs::read(lib.directory.join("session.json")).unwrap()).unwrap()
}

#[test]
fn a_new_session_has_its_three_folders_inside_it() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = session(tmp.path());
    let folders = lib.session_folders();
    assert_eq!(
        folders
            .iter()
            .map(|f| f.stored.as_str())
            .collect::<Vec<_>>(),
        ["Capture", "Selects", "Output"]
    );
    assert!(folders.iter().all(|f| f.inside && f.available));
    assert_eq!(manifest(&lib)["selects"], "Selects");
}

#[test]
fn a_manifest_written_before_selects_was_stored_opens_with_the_default() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = session(tmp.path());
    let dir = lib.directory.clone();
    drop(lib);
    let mut old =
        serde_json::from_slice::<serde_json::Value>(&fs::read(dir.join("session.json")).unwrap())
            .unwrap();
    old.as_object_mut().unwrap().remove("selects");
    fs::write(dir.join("session.json"), serde_json::to_vec(&old).unwrap()).unwrap();
    let lib = Library::open(&dir).unwrap();
    assert_eq!(lib.folder_path(FolderRole::Selects), dir.join("Selects"));
}

#[test]
fn a_folder_inside_is_stored_relative_and_moves_with_the_session() {
    let tmp = tempfile::tempdir().unwrap();
    let mut lib = session(tmp.path());
    let day = lib.directory.join("Shoot/Day 1");
    fs::create_dir_all(&day).unwrap();
    let folder = lib.set_session_folder(FolderRole::Capture, &day).unwrap();
    assert!(folder.inside);
    assert_eq!(folder.stored, "Shoot/Day 1");
    assert_eq!(manifest(&lib)["capture"], "Shoot/Day 1");
    let old = lib.directory.clone();
    drop(lib);
    let moved = tmp.path().join("Elsewhere");
    fs::rename(&old, &moved).unwrap();
    let lib = Library::open(&moved).unwrap();
    assert_eq!(
        lib.folder_path(FolderRole::Capture),
        moved.canonicalize().unwrap().join("Shoot/Day 1")
    );
    assert!(lib.session_folder(FolderRole::Capture).available);
}

#[test]
fn a_folder_outside_is_stored_absolute_and_holds_session_photographs() {
    let tmp = tempfile::tempdir().unwrap();
    let mut lib = session(tmp.path());
    let drive = tmp.path().join("CaptureDrive/Job");
    fs::create_dir_all(&drive).unwrap();
    let drive = drive.canonicalize().unwrap();
    let folder = lib.set_session_folder(FolderRole::Capture, &drive).unwrap();
    assert!(!folder.inside);
    assert_eq!(folder.stored, drive.to_str().unwrap());
    assert_eq!(outside_folders_of(&lib.directory), vec![drive.clone()]);

    // A photograph there belongs to the session; others outside do not.
    let photo = drive.join("A.ARW");
    fs::write(&photo, b"capture").unwrap();
    let asset = lib.add_asset(&photo).unwrap();
    assert_eq!(lib.asset(&asset.id).unwrap().path, photo);
    let stray = tmp.path().join("B.ARW");
    fs::write(&stray, b"stray").unwrap();
    assert!(lib.add_asset(&stray).is_err());

    // The session folder moves; the folder outside and its photograph stay.
    let old = lib.directory.clone();
    drop(lib);
    let moved = tmp.path().join("Moved Job");
    fs::rename(&old, &moved).unwrap();
    let lib = Library::open(&moved).unwrap();
    assert_eq!(lib.folder_path(FolderRole::Capture), drive);
    let found = lib.asset(&asset.id).unwrap();
    assert_eq!(found.path, photo);
    assert!(!found.missing);
}

#[test]
fn changing_a_folder_moves_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let mut lib = session(tmp.path());
    let photo = lib.directory.join("Capture/A.ARW");
    fs::write(&photo, b"a").unwrap();
    let asset = lib.add_asset(&photo).unwrap();
    let other = tmp.path().join("Other");
    fs::create_dir(&other).unwrap();
    lib.set_session_folder(FolderRole::Capture, &other).unwrap();
    assert!(photo.is_file());
    assert_eq!(fs::read_dir(&other).unwrap().count(), 0);
    assert_eq!(lib.asset(&asset.id).unwrap().path, photo);
    assert!(lib.directory.join("Capture").is_dir());
}

#[test]
fn a_photograph_moved_into_a_selects_folder_outside_keeps_its_record() {
    let tmp = tempfile::tempdir().unwrap();
    let mut lib = session(tmp.path());
    let selects = tmp.path().join("Share/Selects");
    fs::create_dir_all(&selects).unwrap();
    let selects = selects.canonicalize().unwrap();
    lib.set_session_folder(FolderRole::Selects, &selects)
        .unwrap();
    let photo = lib.directory.join("Capture/A.ARW");
    fs::write(&photo, b"a").unwrap();
    let asset = lib.add_asset(&photo).unwrap();
    let version = lib.primary_version(&asset.id).unwrap();
    let recipe = json!({"schema_version": 1, "adjustments": {"exposure": 0.7}, "layers": []});
    lib.save_recipe(&version, &recipe, None).unwrap();

    let target = selects.join("A.ARW");
    fs::rename(&photo, &target).unwrap();
    lib.relocate(&photo, &target).unwrap();
    let moved = lib.asset(&asset.id).unwrap();
    assert_eq!(moved.path, target);
    assert!(!moved.missing);
    assert_eq!(lib.recipe(&version).unwrap(), Some(recipe));
    // And back (an undo).
    fs::rename(&target, &photo).unwrap();
    lib.relocate(&target, &photo).unwrap();
    assert_eq!(lib.asset(&asset.id).unwrap().path, photo);
}

#[test]
fn folders_holding_the_sessions_own_data_or_another_library_are_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let mut lib = session(tmp.path());
    let dir = lib.directory.clone();
    fs::create_dir_all(dir.join(".cache/x")).unwrap();
    fs::create_dir_all(dir.join("Selects/.hidden")).unwrap();
    let other = Library::create(&tmp.path().join("Other"), "Other", Mode::Session).unwrap();
    for refused in [
        dir.clone(),
        tmp.path().to_path_buf(),
        dir.join("Trash"),
        dir.join("masks"),
        dir.join(".cache/x"),
        dir.join("Selects/.hidden"),
        dir.join("Selects"),
        other.directory.join("Capture"),
        tmp.path().join("not there"),
    ] {
        assert!(
            lib.set_session_folder(FolderRole::Capture, &refused)
                .is_err(),
            "{} accepted",
            refused.display()
        );
    }
    assert_eq!(lib.manifest.capture, "Capture");
    assert_eq!(manifest(&lib)["capture"], "Capture");

    let mut catalog = Library::create(&tmp.path().join("Cat"), "Cat", Mode::Catalog).unwrap();
    let folder = tmp.path().join("Out");
    fs::create_dir(&folder).unwrap();
    assert!(
        catalog
            .set_session_folder(FolderRole::Output, &folder)
            .is_err()
    );
}

#[test]
fn a_folder_outside_whose_drive_is_away_is_reported_and_never_made() {
    let tmp = tempfile::tempdir().unwrap();
    let mut lib = session(tmp.path());
    let drive = tmp.path().join("Drive/Output");
    fs::create_dir_all(&drive).unwrap();
    lib.set_session_folder(FolderRole::Output, &drive).unwrap();
    fs::remove_dir_all(tmp.path().join("Drive")).unwrap();

    let dir = lib.directory.clone();
    drop(lib);
    let lib = Library::open(&dir).unwrap();
    let output = lib.session_folder(FolderRole::Output);
    assert!(!output.available && !output.inside);
    let error = lib
        .usable_folder(FolderRole::Output)
        .unwrap_err()
        .to_string();
    assert!(error.contains("not available"), "{error}");
    assert!(!drive.exists());

    // A folder inside the session that was removed is simply made again.
    fs::remove_dir(dir.join("Selects")).unwrap();
    assert_eq!(
        lib.usable_folder(FolderRole::Selects).unwrap(),
        dir.join("Selects")
    );
    assert!(dir.join("Selects").is_dir());
}

#[test]
fn a_session_with_a_folder_on_another_systems_drive_still_opens() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = session(tmp.path());
    let dir = lib.directory.clone();
    drop(lib);
    let mut stored: serde_json::Value =
        serde_json::from_slice(&fs::read(dir.join("session.json")).unwrap()).unwrap();
    let foreign = if cfg!(windows) {
        "/Volumes/Capture/Job"
    } else {
        "D:\\Capture\\Job"
    };
    stored["capture"] = json!(foreign);
    fs::write(
        dir.join("session.json"),
        serde_json::to_vec(&stored).unwrap(),
    )
    .unwrap();
    let lib = Library::open(&dir).unwrap();
    let capture = lib.session_folder(FolderRole::Capture);
    assert!(!capture.inside && !capture.available);
    assert!(lib.usable_folder(FolderRole::Capture).is_err());
}

#[test]
fn traversal_in_a_stored_relative_folder_is_still_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = session(tmp.path());
    let dir = lib.directory.clone();
    drop(lib);
    let mut stored: serde_json::Value =
        serde_json::from_slice(&fs::read(dir.join("session.json")).unwrap()).unwrap();
    stored["selects"] = json!("../Elsewhere");
    fs::write(
        dir.join("session.json"),
        serde_json::to_vec(&stored).unwrap(),
    )
    .unwrap();
    assert!(Library::open(&dir).is_err());
}

#[test]
fn a_folder_outside_left_behind_keeps_its_photographs_and_takes_an_undo_back() {
    let tmp = tempfile::tempdir().unwrap();
    let mut lib = session(tmp.path());
    let a = tmp.path().join("Drive/A");
    let b = tmp.path().join("Drive/B");
    fs::create_dir_all(&a).unwrap();
    fs::create_dir_all(&b).unwrap();
    let (a, b) = (a.canonicalize().unwrap(), b.canonicalize().unwrap());
    lib.set_session_folder(FolderRole::Capture, &a).unwrap();
    let kept = a.join("Kept.ARW");
    let moving = a.join("Moving.ARW");
    fs::write(&kept, b"kept").unwrap();
    fs::write(&moving, b"moving").unwrap();
    let kept_id = lib.add_asset(&kept).unwrap().id;
    let moving_id = lib.add_asset(&moving).unwrap().id;

    // Capture changes to B: A is remembered, its photographs stay the session's.
    lib.set_session_folder(FolderRole::Capture, &b).unwrap();
    assert_eq!(
        manifest(&lib)["former_folders"],
        json!([a.to_str().unwrap()])
    );
    assert_eq!(
        outside_folders_of(&lib.directory),
        vec![a.clone(), b.clone()]
    );
    assert_eq!(lib.asset_at(&kept).unwrap().unwrap().id, kept_id);

    // "Move contents" and its Undo: the record follows there and back.
    let target = b.join("Moving.ARW");
    fs::rename(&moving, &target).unwrap();
    lib.relocate(&moving, &target).unwrap();
    assert_eq!(lib.asset(&moving_id).unwrap().path, target);
    fs::rename(&target, &moving).unwrap();
    lib.relocate(&target, &moving).unwrap();
    let back = lib.asset(&moving_id).unwrap();
    assert_eq!(back.path, moving);
    assert!(!back.missing);

    // Choosing A again takes it off the former folders.
    lib.set_session_folder(FolderRole::Capture, &a).unwrap();
    assert_eq!(
        manifest(&lib)["former_folders"],
        json!([b.to_str().unwrap()])
    );
}

#[test]
fn synchronize_folder_covers_folders_outside_and_leaves_the_output_folder_out() {
    use rawtonic_catalog::folder_sync::{SyncOptions, is_photo};
    use std::sync::atomic::AtomicBool;
    let tmp = tempfile::tempdir().unwrap();
    let mut lib = session(tmp.path());
    let drive = tmp.path().join("Drive/Capture");
    fs::create_dir_all(&drive).unwrap();
    let drive = drive.canonicalize().unwrap();
    lib.set_session_folder(FolderRole::Capture, &drive).unwrap();
    let delivered = lib.directory.join("Delivered");
    fs::create_dir(&delivered).unwrap();
    lib.set_session_folder(FolderRole::Output, &delivered)
        .unwrap();
    fs::write(drive.join("New.ARW"), b"new capture").unwrap();
    fs::write(delivered.join("Export.jpg"), b"an export").unwrap();
    fs::write(lib.directory.join("Selects/Pick.ARW"), b"pick").unwrap();

    assert_eq!(lib.sync_scope(Some(&drive)).unwrap(), vec![drive.clone()]);
    assert!(lib.sync_scope(Some(tmp.path())).is_err());
    let options = SyncOptions {
        accept: &is_photo,
        cancel: &AtomicBool::new(false),
        progress: &|_| {},
    };
    let plan = lib.scan_folder_sync(None, &options).unwrap();
    let new: Vec<_> = plan.new_files.iter().map(|f| f.path.clone()).collect();
    assert!(new.contains(&drive.join("New.ARW")), "{new:?}");
    assert!(
        new.contains(&lib.directory.join("Selects/Pick.ARW")),
        "{new:?}"
    );
    assert!(!new.iter().any(|p| p.starts_with(&delivered)), "{new:?}");
}

#[test]
fn an_output_folder_outside_is_never_scanned_now_or_after_it_changed() {
    use rawtonic_catalog::folder_sync::{SyncOptions, is_photo};
    use std::sync::atomic::AtomicBool;
    let tmp = tempfile::tempdir().unwrap();
    let mut lib = session(tmp.path());
    let a = tmp.path().join("Share/A");
    let b = tmp.path().join("Share/B");
    fs::create_dir_all(&a).unwrap();
    fs::create_dir_all(&b).unwrap();
    let (a, b) = (a.canonicalize().unwrap(), b.canonicalize().unwrap());
    lib.set_session_folder(FolderRole::Output, &a).unwrap();
    fs::write(a.join("Render.jpg"), b"render a").unwrap();
    // Synchronizing the Output folder itself is refused.
    assert!(lib.sync_scope(Some(&a)).is_err());

    lib.set_session_folder(FolderRole::Output, &b).unwrap();
    fs::write(b.join("Render.jpg"), b"render b").unwrap();
    assert!(manifest(&lib).get("former_folders").is_none());
    let options = SyncOptions {
        accept: &is_photo,
        cancel: &AtomicBool::new(false),
        progress: &|_| {},
    };
    let plan = lib.scan_folder_sync(None, &options).unwrap();
    assert!(
        !plan
            .new_files
            .iter()
            .any(|f| f.path.starts_with(&a) || f.path.starts_with(&b)),
        "{:?}",
        plan.new_files.iter().map(|f| &f.path).collect::<Vec<_>>()
    );
}
