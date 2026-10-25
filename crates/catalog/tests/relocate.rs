use rawtonic_catalog::{Library, Mode};
use std::fs;

#[test]
fn renamed_and_moved_session_files_keep_identity_metadata_and_edits() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("Job");
    let lib = Library::create(&dir, "Job", Mode::Session).unwrap();
    let photo = dir.join("Capture/DSC001.ARW");
    fs::write(&photo, b"raw").unwrap();
    let asset = lib.add_asset(&photo).unwrap();
    let mut metadata = lib.metadata(&asset.id).unwrap();
    metadata.rating = 4;
    lib.set_metadata(&asset.id, &metadata).unwrap();
    lib.save_editor_state(
        &asset.id,
        &serde_json::json!({"adjustments":{"exposure":1}}),
    )
    .unwrap();

    // Rename.
    let renamed = dir.join("Capture/Portrait.ARW");
    fs::rename(&photo, &renamed).unwrap();
    assert_eq!(lib.relocate(&photo, &renamed).unwrap(), 1);
    let found = lib.asset_at(&renamed).unwrap().unwrap();
    assert_eq!(found.id, asset.id);
    assert_eq!(found.rating, 4);
    assert!(lib.editor_state(&asset.id).unwrap().is_some());

    // Into the session's Trash and back (undo).
    let trashed = dir.join("Trash/Capture/Portrait.ARW");
    fs::create_dir_all(trashed.parent().unwrap()).unwrap();
    fs::rename(&renamed, &trashed).unwrap();
    lib.relocate(&renamed, &trashed).unwrap();
    assert_eq!(lib.asset_at(&trashed).unwrap().unwrap().id, asset.id);
    fs::rename(&trashed, &renamed).unwrap();
    lib.relocate(&trashed, &renamed).unwrap();
    assert_eq!(lib.asset_at(&renamed).unwrap().unwrap().id, asset.id);

    // A whole folder.
    fs::create_dir_all(dir.join("Selects")).unwrap();
    let in_folder = dir.join("Capture/Day 1/B.ARW");
    fs::create_dir_all(in_folder.parent().unwrap()).unwrap();
    fs::write(&in_folder, b"b").unwrap();
    let b = lib.add_asset(&in_folder).unwrap();
    fs::rename(dir.join("Capture/Day 1"), dir.join("Capture/Day one")).unwrap();
    assert_eq!(
        lib.relocate(&dir.join("Capture/Day 1"), &dir.join("Capture/Day one"))
            .unwrap(),
        1
    );
    let moved = lib
        .asset_at(&dir.join("Capture/Day one/B.ARW"))
        .unwrap()
        .unwrap();
    assert_eq!(moved.id, b.id);
    assert_eq!(
        lib.assets(Some("Day one")).unwrap().len(),
        1,
        "search follows the path"
    );
}

/// A session photograph the app moves out of the session folder (to
/// another drive, ADR 0016) stays the same photograph there, online, with
/// its rating, edits and albums; moving it back records it in the session
/// again and drops the outside folder. One moved into another session is
/// kept as missing: that session's records would not see it.
#[test]
fn session_assets_moved_out_of_the_session_stay_the_same_photographs() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("Job");
    let lib = Library::create(&dir, "Job", Mode::Session).unwrap();
    let photo = dir.join("Capture/A.ARW");
    fs::write(&photo, b"a").unwrap();
    let asset = lib.add_asset(&photo).unwrap();
    let mut metadata = lib.metadata(&asset.id).unwrap();
    metadata.rating = 4;
    lib.set_metadata(&asset.id, &metadata).unwrap();
    lib.save_editor_state(
        &asset.id,
        &serde_json::json!({"adjustments":{"exposure":1}}),
    )
    .unwrap();
    let album = lib.create_collection("Picks", None).unwrap();
    lib.add_to_collection(&album.id, std::slice::from_ref(&asset.id))
        .unwrap();

    let outside = tmp
        .path()
        .canonicalize()
        .unwrap()
        .join("Drive/Shoots/A.ARW");
    fs::create_dir_all(outside.parent().unwrap()).unwrap();
    fs::rename(&photo, &outside).unwrap();
    assert_eq!(lib.relocate(&photo, &outside).unwrap(), 1);
    let moved = lib.asset_at(&outside).unwrap().unwrap();
    assert_eq!(moved.id, asset.id);
    assert!(!moved.missing);
    assert_eq!(moved.path, outside);
    assert_eq!(moved.rating, 4);
    assert!(lib.editor_state(&asset.id).unwrap().is_some());
    assert_eq!(lib.collection_assets(&album.id).unwrap()[0].id, asset.id);
    assert!(lib.asset_at(&photo).unwrap().is_none());
    // Still there after a check of the files, and listed as a folder.
    assert_eq!(lib.verify_paths().unwrap(), 0);
    let folders: Vec<_> = lib.folders().unwrap().into_iter().map(|f| f.path).collect();
    assert_eq!(folders, [outside.parent().unwrap()]);
    // Found again where it lives when its drive was away and came back.
    fs::rename(&outside, tmp.path().join("away")).unwrap();
    assert_eq!(lib.verify_paths().unwrap(), 1);
    fs::rename(tmp.path().join("away"), &outside).unwrap();
    assert_eq!(lib.verify_paths().unwrap(), 0);

    // Moving back finds the same asset again, in the session.
    fs::rename(&outside, &photo).unwrap();
    lib.relocate(&outside, &photo).unwrap();
    let back = lib.asset_at(&photo).unwrap().unwrap();
    assert_eq!(back.id, asset.id);
    assert!(!back.missing);
    assert!(lib.folders().unwrap().is_empty(), "no outside folder left");

    // Into another session: kept as missing.
    let other = tmp.path().join("Other");
    Library::create(&other, "Other", Mode::Session).unwrap();
    let there = other.join("Capture/A.ARW");
    fs::rename(&photo, &there).unwrap();
    lib.relocate(&photo, &there).unwrap();
    assert!(lib.asset(&asset.id).unwrap().missing);
    fs::rename(&there, &photo).unwrap();
    lib.relocate(&there, &photo).unwrap();
    assert!(!lib.asset_at(&photo).unwrap().unwrap().missing);
}

/// A folder holding a session's photographs outside the session moves on
/// (Move Folder): the records follow it like a catalog folder.
#[test]
fn an_outside_folder_of_session_photographs_moves_with_its_records() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("Job");
    let lib = Library::create(&dir, "Job", Mode::Session).unwrap();
    let photo = dir.join("Capture/A.ARW");
    fs::write(&photo, b"a").unwrap();
    let asset = lib.add_asset(&photo).unwrap();
    let drive = tmp.path().canonicalize().unwrap().join("Drive");
    let outside = drive.join("Shoots/A.ARW");
    fs::create_dir_all(outside.parent().unwrap()).unwrap();
    fs::rename(&photo, &outside).unwrap();
    lib.relocate(&photo, &outside).unwrap();
    fs::create_dir(drive.join("Archive")).unwrap();
    fs::rename(drive.join("Shoots"), drive.join("Archive/Shoots")).unwrap();
    assert_eq!(
        lib.relocate(&drive.join("Shoots"), &drive.join("Archive/Shoots"))
            .unwrap(),
        1
    );
    let moved = lib
        .asset_at(&drive.join("Archive/Shoots/A.ARW"))
        .unwrap()
        .unwrap();
    assert_eq!(moved.id, asset.id);
    assert!(!moved.missing);

    // Offline and moved meanwhile: it may be relinked outside again, as it
    // lives outside the session; a photograph inside may not.
    fs::create_dir(drive.join("New")).unwrap();
    fs::rename(drive.join("Archive/Shoots/A.ARW"), drive.join("New/A.ARW")).unwrap();
    lib.relink(&asset.id, &drive.join("New/A.ARW"), false)
        .unwrap();
    assert_eq!(
        lib.asset_at(&drive.join("New/A.ARW")).unwrap().unwrap().id,
        asset.id
    );
    let inside = dir.join("Capture/B.ARW");
    fs::write(&inside, b"b").unwrap();
    let b = lib.add_asset(&inside).unwrap();
    fs::rename(&inside, drive.join("New/B.ARW")).unwrap();
    assert!(lib.relink(&b.id, &drive.join("New/B.ARW"), false).is_err());
}

#[test]
fn catalog_files_move_between_external_folders_without_duplicates() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = Library::create(&tmp.path().join("Catalog"), "Catalog", Mode::Catalog).unwrap();
    let photos = tmp.path().join("Photos");
    fs::create_dir_all(photos.join("2026")).unwrap();
    fs::create_dir_all(photos.join("Best")).unwrap();
    let photo = photos.join("2026/A.NEF");
    fs::write(&photo, b"a").unwrap();
    let asset = lib.add_asset(&photo).unwrap();
    let target = photos.join("Best/A.NEF");
    fs::rename(&photo, &target).unwrap();
    lib.relocate(&photo, &target).unwrap();
    // Registering the new place again finds the same asset.
    assert_eq!(lib.add_asset(&target).unwrap().id, asset.id);
    assert_eq!(lib.assets(None).unwrap().len(), 1);

    // Renaming the folder holding it re-keys its root.
    fs::rename(photos.join("Best"), photos.join("Selects")).unwrap();
    lib.relocate(&photos.join("Best"), &photos.join("Selects"))
        .unwrap();
    let moved = photos.join("Selects/A.NEF");
    assert_eq!(lib.add_asset(&moved).unwrap().id, asset.id);
    assert_eq!(lib.assets(None).unwrap().len(), 1);
}

#[test]
fn a_stale_row_at_the_new_place_keeps_its_edits_aside() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("Job");
    let lib = Library::create(&dir, "Job", Mode::Session).unwrap();
    let old = dir.join("Capture/B.ARW");
    fs::write(&old, b"old").unwrap();
    let stale = lib.add_asset(&old).unwrap();
    fs::remove_file(&old).unwrap(); // gone outside Rawtonic
    let photo = dir.join("Capture/A.ARW");
    fs::write(&photo, b"a").unwrap();
    let asset = lib.add_asset(&photo).unwrap();
    fs::rename(&photo, &old).unwrap();
    lib.relocate(&photo, &old).unwrap();
    assert_eq!(lib.asset_at(&old).unwrap().unwrap().id, asset.id);
    let kept = lib.asset(&stale.id).unwrap();
    assert!(kept.missing);
}

#[test]
fn trashed_assets_are_marked_missing_and_restored() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("Job");
    let lib = Library::create(&dir, "Job", Mode::Session).unwrap();
    let photo = dir.join("Capture/A.ARW");
    fs::write(&photo, b"a").unwrap();
    let asset = lib.add_asset(&photo).unwrap();
    fs::rename(&photo, tmp.path().join("system-trash")).unwrap();
    assert_eq!(lib.set_missing_at(&photo, true).unwrap(), 1);
    assert!(lib.asset(&asset.id).unwrap().missing);
    fs::rename(tmp.path().join("system-trash"), &photo).unwrap();
    assert_eq!(lib.set_missing_at(&photo, false).unwrap(), 1);
    assert!(!lib.asset(&asset.id).unwrap().missing);
}

#[test]
fn a_trashed_folder_marks_everything_below_it_missing() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("Job");
    let lib = Library::create(&dir, "Job", Mode::Session).unwrap();
    let a = dir.join("Capture/Day_1/A.ARW");
    let b = dir.join("Capture/Day_10/B.ARW");
    for p in [&a, &b] {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, b"x").unwrap();
    }
    let (ida, idb) = (lib.add_asset(&a).unwrap().id, lib.add_asset(&b).unwrap().id);
    fs::rename(dir.join("Capture/Day_1"), tmp.path().join("gone")).unwrap();
    // "_" is literal: Day_10 is not below Day_1.
    assert_eq!(
        lib.set_missing_at(&dir.join("Capture/Day_1"), true)
            .unwrap(),
        1
    );
    assert!(lib.asset(&ida).unwrap().missing);
    assert!(!lib.asset(&idb).unwrap().missing);
}

#[test]
fn iptc_fields_are_stored_and_searchable() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("Job");
    let lib = Library::create(&dir, "Job", Mode::Session).unwrap();
    let photo = dir.join("Capture/A.ARW");
    fs::write(&photo, b"a").unwrap();
    let asset = lib.add_asset(&photo).unwrap();
    let mut meta = lib.metadata(&asset.id).unwrap();
    meta.set_iptc("city", "Ascona");
    lib.set_metadata(&asset.id, &meta).unwrap();
    assert_eq!(
        lib.metadata(&asset.id)
            .unwrap()
            .iptc
            .get("city")
            .map(String::as_str),
        Some("Ascona")
    );
    assert_eq!(lib.assets(Some("Ascona")).unwrap().len(), 1);
}

/// Emptying the session Trash: the photographs deleted for good are
/// forgotten with their edits and album memberships; the rest stay, paths
/// outside the session are refused, and a second run finds nothing.
#[test]
fn forget_at_drops_permanently_deleted_photographs_only() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("Job");
    let lib = Library::create(&dir, "Job", Mode::Session).unwrap();
    let add = |rel: &str| {
        let path = dir.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, rel).unwrap();
        lib.add_asset(&path).unwrap()
    };
    let trashed = add("Trash/Capture/A.ARW");
    let numbered = add("Trash/Capture/1/A.ARW");
    let loose = add("Trash/B_1%.ARW");
    let kept = add("Capture/A.ARW");
    // A look-alike name next to the Trash must not match its pattern.
    let lookalike = add("Trash2/C.ARW");
    lib.save_editor_state(
        &trashed.id,
        &serde_json::json!({"adjustments":{"exposure":1}}),
    )
    .unwrap();
    let album = lib.create_collection("Picks", None).unwrap();
    lib.add_to_collection(&album.id, &[trashed.id.clone(), kept.id.clone()])
        .unwrap();

    // The files are gone (deleted for good) before the records follow.
    fs::remove_dir_all(dir.join("Trash/Capture")).unwrap();
    fs::remove_file(dir.join("Trash/B_1%.ARW")).unwrap();
    assert_eq!(lib.forget_at(&dir.join("Trash/Capture")).unwrap(), 2);
    assert_eq!(lib.forget_at(&dir.join("Trash/B_1%.ARW")).unwrap(), 1);
    for gone in [&trashed, &numbered, &loose] {
        assert!(lib.asset(&gone.id).is_err(), "{:?} still known", gone.path);
    }
    assert!(
        !matches!(lib.editor_state(&trashed.id), Ok(Some(_))),
        "edits gone"
    );
    let in_album: Vec<String> = lib
        .collection_assets(&album.id)
        .unwrap()
        .into_iter()
        .map(|a| a.id)
        .collect();
    assert_eq!(in_album, std::slice::from_ref(&kept.id));
    assert_eq!(
        lib.asset_at(&dir.join("Capture/A.ARW"))
            .unwrap()
            .unwrap()
            .id,
        kept.id
    );
    assert_eq!(
        lib.asset_at(&dir.join("Trash2/C.ARW")).unwrap().unwrap().id,
        lookalike.id
    );
    // Again: nothing left.
    assert_eq!(lib.forget_at(&dir.join("Trash/Capture")).unwrap(), 0);
    // Outside the session, or the session itself: refused.
    assert!(lib.forget_at(&tmp.path().join("Elsewhere/A.ARW")).is_err());
    assert!(lib.forget_at(&dir).is_err());
}
