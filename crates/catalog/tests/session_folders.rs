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

fn scan(lib: &Library) -> Vec<std::path::PathBuf> {
    use rawtonic_catalog::folder_sync::{SyncOptions, is_photo};
    use std::sync::atomic::AtomicBool;
    let options = SyncOptions {
        accept: &is_photo,
        cancel: &AtomicBool::new(false),
        progress: &|_| {},
    };
    lib.scan_folder_sync(None, &options)
        .unwrap()
        .new_files
        .iter()
        .map(|f| f.path.clone())
        .collect()
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

/// A session's former Output folder that lies under a scan root (here an
/// outside Capture folder, `Drive/Capture/Delivered`) is left alone after
/// the Output changed: the exports made there never become photographs,
/// while a photograph folder beside it is still scanned. Nothing is moved or
/// deleted (review r3, G38).
#[test]
fn a_former_output_folder_under_a_scan_root_is_never_scanned() {
    let tmp = tempfile::tempdir().unwrap();
    let mut lib = session(tmp.path());
    let drive = tmp.path().join("Drive/Capture");
    fs::create_dir_all(&drive).unwrap();
    let drive = drive.canonicalize().unwrap();
    lib.set_session_folder(FolderRole::Capture, &drive).unwrap();
    let former = drive.join("Delivered");
    fs::create_dir(&former).unwrap();
    lib.set_session_folder(FolderRole::Output, &former).unwrap();
    // A delivery subfolder of the Output folder is no photograph either.
    fs::create_dir(former.join("Client")).unwrap();
    fs::write(former.join("Client/Export.jpg"), b"an export").unwrap();
    fs::write(former.join("Export.jpg"), b"an export").unwrap();
    let beside = drive.join("Shoot");
    fs::create_dir(&beside).unwrap();
    fs::write(beside.join("A.ARW"), b"a capture").unwrap();

    let elsewhere = tmp.path().join("Share/Output");
    fs::create_dir_all(&elsewhere).unwrap();
    let elsewhere = elsewhere.canonicalize().unwrap();
    lib.set_session_folder(FolderRole::Output, &elsewhere)
        .unwrap();
    assert_eq!(
        manifest(&lib)["output_folders"],
        json!(["Output", former.to_str().unwrap()])
    );
    assert!(manifest(&lib).get("former_folders").is_none());
    // Synchronizing a former Output folder is refused like the current one.
    assert!(lib.sync_scope(Some(&former)).is_err());
    assert!(
        lib.sync_scope(Some(former.join("Client").as_path()))
            .is_err()
    );

    let new = scan(&lib);
    assert!(new.contains(&beside.join("A.ARW")), "{new:?}");
    assert!(
        !new.iter()
            .any(|p| p.starts_with(&former) || p.starts_with(&elsewhere)),
        "{new:?}"
    );
    // The exports and the photograph files stay exactly where they are.
    assert!(former.join("Export.jpg").is_file());
    assert!(former.join("Client/Export.jpg").is_file());
    assert!(beside.join("A.ARW").is_file());
}

/// The same for a former Output folder inside the session, under a session
/// folder that the whole session's scan walks.
#[test]
fn a_former_output_folder_inside_under_a_scan_root_is_never_scanned() {
    let tmp = tempfile::tempdir().unwrap();
    let mut lib = session(tmp.path());
    let former = lib.directory.join("Delivered");
    fs::create_dir(&former).unwrap();
    lib.set_session_folder(FolderRole::Output, &former).unwrap();
    fs::write(former.join("Export.jpg"), b"an export").unwrap();
    let beside = lib.directory.join("Delivery");
    fs::create_dir(&beside).unwrap();
    fs::write(beside.join("B.ARW"), b"a photograph").unwrap();
    let elsewhere = tmp.path().join("Share/Output");
    fs::create_dir_all(&elsewhere).unwrap();
    lib.set_session_folder(FolderRole::Output, &elsewhere)
        .unwrap();

    let new = scan(&lib);
    assert!(new.contains(&beside.join("B.ARW")), "{new:?}");
    assert!(!new.iter().any(|p| p.starts_with(&former)), "{new:?}");
    assert!(former.join("Export.jpg").is_file());
}

/// A session written before its Output folders were recorded (before this
/// field; older sessions from schema 23 on) still leaves its default Output
/// folder alone.
#[test]
fn a_session_from_before_output_folders_were_recorded_still_skips_its_output() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = session(tmp.path());
    let dir = lib.directory.clone();
    drop(lib);
    let mut stored = manifest(&Library::open(&dir).unwrap());
    stored.as_object_mut().unwrap().remove("output_folders");
    fs::write(
        dir.join("session.json"),
        serde_json::to_vec(&stored).unwrap(),
    )
    .unwrap();
    let lib = Library::open(&dir).unwrap();
    fs::write(dir.join("Output/Old.jpg"), b"an old export").unwrap();
    let new = scan(&lib);
    assert!(
        !new.iter().any(|p| p.starts_with(dir.join("Output"))),
        "{new:?}"
    );
}

/// A configured folder inside the session whose parent folder was renamed
/// is reported as unavailable on open, and nothing is made at its old path
/// (which would take the work meant for the renamed one); changing the
/// setting to the renamed folder works. G38, review r3.
#[test]
fn a_folder_inside_whose_parent_was_renamed_is_unavailable_and_never_made() {
    let tmp = tempfile::tempdir().unwrap();
    let mut lib = session(tmp.path());
    let day = lib.directory.join("Shoot/Day 1");
    fs::create_dir_all(&day).unwrap();
    lib.set_session_folder(FolderRole::Capture, &day).unwrap();
    let picks = lib.directory.join("Shoot/Day 1/Picks");
    fs::create_dir(&picks).unwrap();
    lib.set_session_folder(FolderRole::Selects, &picks).unwrap();
    let output = tmp.path().join("Share/Output");
    fs::create_dir_all(&output).unwrap();
    lib.set_session_folder(FolderRole::Output, &output).unwrap();
    fs::write(day.join("A.ARW"), b"a capture").unwrap();

    fs::rename(
        lib.directory.join("Shoot"),
        lib.directory.join("Shoot 2026"),
    )
    .unwrap();
    let dir = lib.directory.clone();
    drop(lib);
    let mut lib = Library::open(&dir).unwrap();

    for role in [FolderRole::Capture, FolderRole::Selects] {
        let folder = lib.session_folder(role);
        assert!(folder.inside, "{role:?} is not inside the session");
        assert!(!folder.available, "{role:?} is reported as available");
        assert!(!folder.recreated, "{role:?} would be made again");
        let error = lib.usable_folder(role).unwrap_err().to_string();
        assert!(error.contains("not available"), "{error}");
        assert!(error.contains("renamed or moved"), "{error}");
    }
    // Nothing was made at the old paths.
    assert!(!dir.join("Shoot").exists());

    // The renamed folder is chosen as it is; the files in it are untouched.
    let renamed = dir.join("Shoot 2026/Day 1");
    let folder = lib
        .set_session_folder(FolderRole::Capture, &renamed)
        .unwrap();
    assert!(folder.inside && folder.available);
    assert_eq!(folder.stored, "Shoot 2026/Day 1");
    assert_eq!(lib.usable_folder(FolderRole::Capture).unwrap(), renamed);
    assert!(renamed.join("A.ARW").is_file());
}

/// A folder inside the session that the app made with the session (its
/// default) is still made again when someone removed it, and so is the
/// default Output of a session from before Output folders were recorded.
#[test]
fn the_folders_a_new_session_was_given_are_made_again() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = session(tmp.path());
    let dir = lib.directory.clone();
    assert!(lib.session_folders().iter().all(|f| f.recreated));
    drop(lib);
    fs::remove_dir(dir.join("Capture")).unwrap();
    fs::remove_dir(dir.join("Output")).unwrap();
    let mut stored = manifest(&Library::open(&dir).unwrap());
    stored.as_object_mut().unwrap().remove("output_folders");
    fs::write(
        dir.join("session.json"),
        serde_json::to_vec(&stored).unwrap(),
    )
    .unwrap();
    let lib = Library::open(&dir).unwrap();
    assert_eq!(
        lib.usable_folder(FolderRole::Capture).unwrap(),
        dir.join("Capture")
    );
    assert!(dir.join("Capture").is_dir());
    assert_eq!(
        lib.usable_folder(FolderRole::Output).unwrap(),
        dir.join("Output")
    );
    assert!(dir.join("Output").is_dir());
}

/// A session written before `output_folders` existed may still have a
/// custom Output setting (origin/main supported it without that field).
/// Such a folder that is missing or whose parent was renamed is reported
/// unavailable and is never made again — a folder outside the session that
/// held the exports is not made on its drive either. Review r4, G38.
#[test]
fn a_legacy_session_never_makes_a_custom_output_folder_again() {
    // Custom Output inside the session whose parent was renamed.
    let tmp = tempfile::tempdir().unwrap();
    let mut lib = session(tmp.path());
    let deliveries = lib.directory.join("Deliveries/2026");
    fs::create_dir_all(&deliveries).unwrap();
    lib.set_session_folder(FolderRole::Output, &deliveries)
        .unwrap();
    fs::write(deliveries.join("Export.jpg"), b"an export").unwrap();
    fs::rename(
        lib.directory.join("Deliveries"),
        lib.directory.join("Deliveries 2026"),
    )
    .unwrap();
    let dir = lib.directory.clone();
    drop(lib);
    let legacy = |dir: &Path| {
        let mut stored = manifest(&Library::open(dir).unwrap());
        stored.as_object_mut().unwrap().remove("output_folders");
        fs::write(
            dir.join("session.json"),
            serde_json::to_vec(&stored).unwrap(),
        )
        .unwrap();
    };
    legacy(&dir);
    let lib = Library::open(&dir).unwrap();
    let output = lib.session_folder(FolderRole::Output);
    assert!(output.inside && !output.available, "{output:?}");
    assert!(!output.recreated, "{output:?}");
    let error = lib
        .usable_folder(FolderRole::Output)
        .unwrap_err()
        .to_string();
    assert!(error.contains("not available"), "{error}");
    assert!(error.contains("renamed or moved"), "{error}");
    // Nothing was made at the old path; the exports stay where they are.
    assert!(!dir.join("Deliveries").exists());
    assert!(dir.join("Deliveries 2026/2026/Export.jpg").is_file());

    // Custom Output outside the session, missing: reported, never made.
    let tmp = tempfile::tempdir().unwrap();
    let mut lib = session(tmp.path());
    let share = tmp.path().join("Share/Exports");
    fs::create_dir_all(&share).unwrap();
    lib.set_session_folder(FolderRole::Output, &share).unwrap();
    let dir = lib.directory.clone();
    drop(lib);
    legacy(&dir);
    fs::remove_dir_all(tmp.path().join("Share")).unwrap();
    let lib = Library::open(&dir).unwrap();
    let output = lib.session_folder(FolderRole::Output);
    assert!(!output.inside && !output.available && !output.recreated);
    let error = lib
        .usable_folder(FolderRole::Output)
        .unwrap_err()
        .to_string();
    assert!(error.contains("not available"), "{error}");
    assert!(!share.exists(), "the folder outside was made again");

    // The default Output of such a session is still made again when removed.
    let tmp = tempfile::tempdir().unwrap();
    let lib = session(tmp.path());
    let dir = lib.directory.clone();
    drop(lib);
    legacy(&dir);
    fs::remove_dir(dir.join("Output")).unwrap();
    let lib = Library::open(&dir).unwrap();
    assert!(lib.session_folder(FolderRole::Output).recreated);
    assert_eq!(
        lib.usable_folder(FolderRole::Output).unwrap(),
        dir.join("Output")
    );
    assert!(dir.join("Output").is_dir());
}

/// A folder that was the Output folder holds exports, so Capture and
/// Selects refuse it and any folder inside it, current or former: a scan of
/// the Capture folder would otherwise take the exports for photographs
/// again. Nothing is moved or deleted. Review r4, G38.
#[test]
fn photograph_folders_inside_an_output_folder_are_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let mut lib = session(tmp.path());
    let deliveries = lib.directory.join("Deliveries");
    fs::create_dir(&deliveries).unwrap();
    lib.set_session_folder(FolderRole::Output, &deliveries)
        .unwrap();
    fs::write(deliveries.join("Export.jpg"), b"an export").unwrap();
    let elsewhere = tmp.path().join("Share/Output");
    fs::create_dir_all(&elsewhere).unwrap();
    let elsewhere = elsewhere.canonicalize().unwrap();
    lib.set_session_folder(FolderRole::Output, &elsewhere)
        .unwrap();

    // Deliveries is a former Output folder now: still no Capture folder.
    let error = lib
        .set_session_folder(FolderRole::Capture, &deliveries)
        .unwrap_err()
        .to_string();
    assert!(error.contains("Output folder"), "{error}");
    assert!(
        error.contains(&format!(
            "rawtonic:inOutputFolder:capture:{}:{}",
            deliveries.display(),
            deliveries.display()
        )),
        "{error}"
    );
    for role in [FolderRole::Capture, FolderRole::Selects] {
        assert!(
            lib.set_session_folder(role, &deliveries).is_err(),
            "{role:?} took a former Output folder"
        );
        let inside = deliveries.join("Client");
        fs::create_dir_all(&inside).unwrap();
        assert!(
            lib.set_session_folder(role, &inside).is_err(),
            "{role:?} took a folder inside a former Output folder"
        );
        // The current Output folder and a folder inside it, too.
        assert!(lib.set_session_folder(role, &elsewhere).is_err());
        // Also reached by a path that steps into it and back.
        assert!(
            lib.set_session_folder(role, &elsewhere.join("../Output"))
                .is_err()
        );
    }
    let current = lib.directory.join("Output");
    assert!(current.is_dir());
    assert!(
        lib.set_session_folder(FolderRole::Capture, &current)
            .is_err()
    );
    assert_eq!(lib.manifest.capture, "Capture");
    assert_eq!(manifest(&lib)["capture"], "Capture");
    // Nothing was moved or deleted.
    assert!(deliveries.join("Export.jpg").is_file());
    assert!(deliveries.join("Client").is_dir());
    // Output itself may still be set to any folder.
    let output = lib.directory.join("Renders");
    fs::create_dir(&output).unwrap();
    lib.set_session_folder(FolderRole::Output, &output).unwrap();
    // And a folder that never was an Output folder is a Capture folder.
    let shoot = lib.directory.join("Shoot");
    fs::create_dir(&shoot).unwrap();
    lib.set_session_folder(FolderRole::Capture, &shoot).unwrap();
    assert_eq!(lib.manifest.capture, "Shoot");
}

/// The same for an Output folder outside the session: a Capture folder in it
/// would register its exports as photographs of the drive.
#[test]
fn a_capture_folder_inside_an_output_folder_outside_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let mut lib = session(tmp.path());
    let share = tmp.path().join("Share/Output");
    fs::create_dir_all(&share).unwrap();
    let share = share.canonicalize().unwrap();
    lib.set_session_folder(FolderRole::Output, &share).unwrap();
    let client = share.join("Client");
    fs::create_dir(&client).unwrap();
    let error = lib
        .set_session_folder(FolderRole::Capture, &client)
        .unwrap_err()
        .to_string();
    assert!(
        error.contains(&format!(
            "rawtonic:inOutputFolder:capture:{}:{}",
            client.display(),
            share.display()
        )),
        "{error}"
    );
    assert!(error.contains("Output folder"), "{error}");
    assert_eq!(lib.manifest.capture, "Capture");
    // A link to the Output folder leads into it as well.
    #[cfg(unix)]
    {
        let link = lib.directory.join("Link");
        std::os::unix::fs::symlink(&share, &link).unwrap();
        assert!(lib.set_session_folder(FolderRole::Capture, &link).is_err());
    }
}
