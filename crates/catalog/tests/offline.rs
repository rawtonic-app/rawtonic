//! Offline photographs: missing files and disconnected drives stay in the
//! library with their edits, metadata and albums; relinking finds them again
//! without touching any file.
use rawtonic_catalog::{Library, Mode, Volume};
use std::{fs, path::Path};

fn bytes(seed: u8, len: usize) -> Vec<u8> {
    (0..len)
        .map(|i| (i as u8).wrapping_mul(31) ^ seed)
        .collect()
}

/// A catalog with three photographs in `Card/2026` (one in a subfolder),
/// rated and edited, in an album.
fn catalog(tmp: &Path) -> (Library, Vec<String>) {
    let lib = Library::create(&tmp.join("Main.catalog"), "Main", Mode::Catalog).unwrap();
    let folder = tmp.join("Card/2026");
    fs::create_dir_all(folder.join("Day 2")).unwrap();
    let mut ids = Vec::new();
    for (seed, name) in [(1u8, "A.NEF"), (2, "B.NEF"), (3, "Day 2/C.NEF")] {
        let path = folder.join(name);
        // Larger than the fingerprint's two 64 KiB samples.
        fs::write(&path, bytes(seed, 200_000)).unwrap();
        let asset = lib.add_asset(&path).unwrap();
        let mut meta = lib.metadata(&asset.id).unwrap();
        meta.rating = seed;
        lib.set_metadata(&asset.id, &meta).unwrap();
        lib.save_editor_state(
            &asset.id,
            &serde_json::json!({"adjustments": {"exposure": seed}}),
        )
        .unwrap();
        ids.push(asset.id);
    }
    let album = lib.create_collection("Trip", None).unwrap();
    lib.add_to_collection(&album.id, &ids).unwrap();
    (lib, ids)
}

fn snapshot(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut files: Vec<_> = walk(dir)
        .into_iter()
        .map(|p| {
            (
                p.strip_prefix(dir).unwrap().to_string_lossy().into_owned(),
                fs::read(&p).unwrap(),
            )
        })
        .collect();
    files.sort();
    files
}

fn walk(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(walk(&path));
        } else {
            out.push(path);
        }
    }
    out
}

#[test]
fn a_vanished_folder_keeps_its_photographs_edits_and_albums() {
    let tmp = tempfile::tempdir().unwrap();
    let (lib, ids) = catalog(tmp.path());
    fs::rename(tmp.path().join("Card"), tmp.path().join("Elsewhere")).unwrap();

    assert_eq!(lib.verify_paths().unwrap(), 3);
    // Reopening again changes nothing and removes nothing.
    drop(lib);
    let lib = Library::open(&tmp.path().join("Main.catalog")).unwrap();
    assert_eq!(lib.verify_paths().unwrap(), 3);
    assert_eq!(lib.assets(None).unwrap().len(), 3);
    assert_eq!(lib.summary().unwrap().missing, 3);
    for (i, id) in ids.iter().enumerate() {
        assert_eq!(lib.metadata(id).unwrap().rating, i as u8 + 1);
        assert!(lib.editor_state(id).unwrap().is_some());
    }
    let album = &lib.collections().unwrap()[0];
    assert_eq!(album.count, 3);
    assert_eq!(lib.collection_assets(&album.id).unwrap().len(), 3);

    let offline = lib.offline_assets().unwrap();
    assert_eq!(offline.len(), 3);
    assert!(
        offline.iter().all(|a| a.volume.is_none()),
        "the drive is there"
    );
    let folder = tmp.path().canonicalize().unwrap().join("Card/2026");
    assert_eq!(lib.offline_in(&folder, false).unwrap().len(), 2);
    assert_eq!(lib.offline_in(&folder, true).unwrap().len(), 3);
    // A missing file is still found by its registered path.
    let a = lib.asset_at(&folder.join("A.NEF")).unwrap().unwrap();
    assert_eq!(a.id, ids[0]);
    assert!(a.missing);
}

#[test]
fn locating_one_photograph_relinks_its_neighbours_and_touches_no_file() {
    let tmp = tempfile::tempdir().unwrap();
    let (lib, ids) = catalog(tmp.path());
    let moved = tmp.path().join("Archive/2026 trip");
    fs::create_dir_all(moved.parent().unwrap()).unwrap();
    fs::rename(tmp.path().join("Card/2026"), &moved).unwrap();
    // Locating under another name works when the content is the same.
    fs::rename(moved.join("A.NEF"), moved.join("Renamed.NEF")).unwrap();
    lib.verify_paths().unwrap();
    let before = snapshot(tmp.path().join("Archive").as_path());

    let report = lib
        .relink(&ids[0], &moved.join("Renamed.NEF"), true)
        .unwrap();
    assert_eq!(report.relinked.len(), 3, "{report:?}");
    assert_eq!(report.not_found, 0);
    assert_eq!(lib.verify_paths().unwrap(), 0);
    assert_eq!(lib.summary().unwrap().missing, 0);
    let moved = moved.canonicalize().unwrap();
    for (id, path) in ids.iter().zip(["Renamed.NEF", "B.NEF", "Day 2/C.NEF"]) {
        let asset = lib.asset(id).unwrap();
        assert_eq!(asset.path, moved.join(path));
        assert!(!asset.missing);
        assert!(lib.editor_state(id).unwrap().is_some(), "edits kept");
    }
    assert_eq!(lib.collections().unwrap()[0].count, 3, "albums kept");
    assert_eq!(
        snapshot(tmp.path().join("Archive").as_path()),
        before,
        "files untouched"
    );
}

#[test]
fn another_photograph_with_the_same_name_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let (lib, ids) = catalog(tmp.path());
    let folder = tmp.path().join("Card/2026");
    fs::rename(&folder, tmp.path().join("Gone")).unwrap();
    let other = tmp.path().join("Other");
    fs::create_dir_all(other.join("Day 2")).unwrap();
    // Same name and size, other content.
    fs::write(other.join("A.NEF"), bytes(9, 200_000)).unwrap();
    fs::copy(tmp.path().join("Gone/B.NEF"), other.join("B.NEF")).unwrap();
    lib.verify_paths().unwrap();

    let error = lib.relink(&ids[0], &other.join("A.NEF"), true).unwrap_err();
    assert!(error.to_string().contains("not this photograph"), "{error}");
    assert_eq!(lib.summary().unwrap().missing, 3, "nothing relinked");

    let report = lib.relink_folder(&folder, &other).unwrap();
    assert_eq!(report.relinked.len(), 1, "{report:?}");
    assert_eq!(report.mismatched.len(), 1);
    assert_eq!(report.not_found, 1, "Day 2/C.NEF is not there");
    assert_eq!(lib.summary().unwrap().missing, 2);
    assert!(lib.asset(&ids[0]).unwrap().missing);
    assert!(!lib.asset(&ids[1]).unwrap().missing);
}

#[test]
fn a_present_photograph_or_a_place_another_one_holds_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let (lib, ids) = catalog(tmp.path());
    let folder = tmp.path().join("Card/2026");
    assert!(
        lib.relink(&ids[0], &folder.join("A.NEF"), false).is_err(),
        "not missing"
    );

    // B is gone; a copy of it is registered as another photograph.
    let copy = tmp.path().join("Copy/B.NEF");
    fs::create_dir_all(copy.parent().unwrap()).unwrap();
    fs::copy(folder.join("B.NEF"), &copy).unwrap();
    let other = lib.add_asset(&copy).unwrap();
    fs::remove_file(folder.join("B.NEF")).unwrap();
    lib.verify_paths().unwrap();
    let error = lib.relink(&ids[1], &copy, false).unwrap_err();
    assert!(
        error.to_string().contains("already another photograph"),
        "{error}"
    );
    assert_eq!(
        lib.asset(&other.id).unwrap().path,
        copy.canonicalize().unwrap()
    );
}

#[test]
fn session_photographs_relink_inside_the_session_only() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("Job");
    let lib = Library::create(&dir, "Job", Mode::Session).unwrap();
    let photo = dir.join("Capture/DSC001.ARW");
    fs::write(&photo, bytes(4, 70_000)).unwrap();
    let asset = lib.add_asset(&photo).unwrap();
    // Moved out of the session in Finder.
    let outside = tmp.path().join("DSC001.ARW");
    fs::rename(&photo, &outside).unwrap();
    assert_eq!(lib.verify_paths().unwrap(), 1);
    assert_eq!(lib.offline_assets().unwrap()[0].id, asset.id);
    let error = lib.relink(&asset.id, &outside, false).unwrap_err();
    assert!(error.to_string().contains("inside the session"), "{error}");
    assert!(lib.relink_folder(&dir.join("Capture"), tmp.path()).is_err());

    // Moved to another folder of the session instead.
    let inside = dir.join("Selects/DSC001.ARW");
    fs::rename(&outside, &inside).unwrap();
    let report = lib.relink(&asset.id, &inside, false).unwrap();
    assert_eq!(report.relinked.len(), 1);
    assert_eq!(lib.asset(&asset.id).unwrap().rel_path, "Selects/DSC001.ARW");
    assert_eq!(lib.verify_paths().unwrap(), 0);
}

#[test]
fn a_drive_mounted_elsewhere_is_followed_and_a_disconnected_one_is_named() {
    let tmp = tempfile::tempdir().unwrap();
    let (lib, ids) = catalog(tmp.path());
    let mount = tmp.path().canonicalize().unwrap().join("Card");
    let volume = Volume {
        id: "0A1B2C3D-0000-4000-8000-000000000001".into(),
        name: "Photos".into(),
        mount_point: mount.clone(),
    };
    lib.set_folder_volume(&mount.join("2026"), &volume).unwrap();
    lib.set_folder_volume(&mount.join("2026/Day 2"), &volume)
        .unwrap();

    // Disconnected: the mount point is gone.
    fs::rename(&mount, tmp.path().join("Unplugged")).unwrap();
    lib.verify_paths().unwrap();
    let offline = lib.offline_assets().unwrap();
    assert_eq!(offline.len(), 3);
    assert!(
        offline
            .iter()
            .all(|a| a.volume.as_deref() == Some("Photos")),
        "{offline:?}"
    );

    // Plugged in again under another mount point ("Photos 1").
    let remounted = tmp.path().canonicalize().unwrap().join("Card 1");
    fs::rename(tmp.path().join("Unplugged"), &remounted).unwrap();
    // B was deleted meanwhile: it stays missing at its old place.
    fs::remove_file(remounted.join("2026/B.NEF")).unwrap();
    let now = Volume {
        mount_point: remounted.clone(),
        name: "Photos 1".into(),
        ..volume.clone()
    };
    assert_eq!(lib.follow_volumes(std::slice::from_ref(&now)).unwrap(), 2);
    lib.verify_paths().unwrap();
    assert_eq!(
        lib.asset(&ids[0]).unwrap().path,
        remounted.join("2026/A.NEF")
    );
    assert_eq!(
        lib.asset(&ids[2]).unwrap().path,
        remounted.join("2026/Day 2/C.NEF")
    );
    assert!(lib.asset(&ids[1]).unwrap().missing);
    assert_eq!(lib.assets(None).unwrap().len(), 3, "nothing removed");
    // Another drive with the same layout is not taken for it.
    let stranger = Volume {
        id: "FFFFFFFF-0000-4000-8000-000000000002".into(),
        ..now
    };
    assert_eq!(lib.follow_volumes(&[stranger]).unwrap(), 0);
}

#[test]
fn an_older_database_is_migrated_with_its_photographs() {
    let tmp = tempfile::tempdir().unwrap();
    let (lib, _) = catalog(tmp.path());
    drop(lib);
    let db = tmp.path().join("Main.catalog/catalog.sqlite");
    let conn = rusqlite::Connection::open(&db).unwrap();
    // Back to schema 2 (no volume columns, no import batches).
    conn.execute_batch(
        "DROP TABLE asset_imports; DROP TABLE import_batches; ALTER TABLE roots DROP COLUMN volume_name; ALTER TABLE roots DROP COLUMN volume_path; PRAGMA user_version=2;",
    )
    .unwrap();
    drop(conn);
    let lib = Library::open(&tmp.path().join("Main.catalog")).unwrap();
    assert_eq!(lib.verify_paths().unwrap(), 0);
    assert_eq!(lib.summary().unwrap().missing, 0);
    assert_eq!(lib.assets(None).unwrap().len(), 3);
}

#[test]
fn a_folder_lists_its_files_deleted_since_the_last_check() {
    let tmp = tempfile::tempdir().unwrap();
    let (lib, ids) = catalog(tmp.path());
    let folder = tmp.path().canonicalize().unwrap().join("Card/2026");
    fs::remove_file(folder.join("B.NEF")).unwrap();
    // No verify_paths yet: the listing still sees B as offline.
    let offline = lib.offline_in(&folder, false).unwrap();
    assert_eq!(offline.len(), 1);
    assert_eq!(offline[0].id, ids[1]);
    assert!(
        lib.offline_in(&folder.join("Day 2"), false)
            .unwrap()
            .is_empty()
    );
    assert_eq!(lib.offline_in(tmp.path(), true).unwrap().len(), 1);

    // In a session too, from its root folder down.
    let dir = tmp.path().join("Job");
    let session = Library::create(&dir, "Job", Mode::Session).unwrap();
    let photo = dir.join("Capture/Day_1/DSC001.ARW");
    fs::create_dir_all(photo.parent().unwrap()).unwrap();
    fs::write(&photo, bytes(5, 1000)).unwrap();
    session.add_asset(&photo).unwrap();
    fs::remove_file(&photo).unwrap();
    let dir = dir.canonicalize().unwrap();
    assert_eq!(session.offline_in(&dir, true).unwrap().len(), 1);
    assert_eq!(
        session
            .offline_in(&dir.join("Capture/Day_1"), false)
            .unwrap()
            .len(),
        1
    );
    // "_" is literal, not a LIKE wildcard.
    assert!(
        session
            .offline_in(&dir.join("Capture/DayX1"), true)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn a_folder_moved_since_the_last_check_is_relinked() {
    let tmp = tempfile::tempdir().unwrap();
    let (lib, ids) = catalog(tmp.path());
    let moved = tmp.path().join("Archive/2026");
    fs::create_dir_all(moved.parent().unwrap()).unwrap();
    fs::rename(tmp.path().join("Card/2026"), &moved).unwrap();
    // No verify_paths: nothing is flagged missing yet (before the fix the
    // relink found nothing: "0 photographs relinked").
    assert_eq!(lib.summary().unwrap().missing, 0);
    let folder = tmp.path().canonicalize().unwrap().join("Card/2026");
    let report = lib.relink_folder(&folder, &moved).unwrap();
    assert_eq!(report.relinked.len(), 3, "{report:?}");
    let moved = moved.canonicalize().unwrap();
    assert_eq!(lib.asset(&ids[2]).unwrap().path, moved.join("Day 2/C.NEF"));
    // Nearby photographs of a located one, too.
    fs::rename(&moved, tmp.path().join("Archive/Again")).unwrap();
    let again = tmp.path().canonicalize().unwrap().join("Archive/Again");
    let report = lib.relink(&ids[0], &again.join("A.NEF"), true).unwrap();
    assert_eq!(report.relinked.len(), 3, "{report:?}");
    assert_eq!(lib.verify_paths().unwrap(), 0);
}

#[test]
fn a_drive_whose_mount_point_folder_stays_is_named_as_disconnected() {
    let tmp = tempfile::tempdir().unwrap();
    if rawtonic_catalog::volume::volume_of(tmp.path()).is_none() {
        return; // No volume identities on this platform.
    }
    let (lib, _) = catalog(tmp.path());
    // The drive was mounted on a folder (hdiutil -mountpoint, /mnt/photos on
    // Linux) that stays, empty, when it is unmounted.
    let mount = tmp.path().canonicalize().unwrap().join("Card");
    let volume = Volume {
        id: "0A1B2C3D-0000-4000-8000-000000000009".into(),
        name: "Card".into(),
        mount_point: mount.clone(),
    };
    lib.set_folder_volume(&mount.join("2026"), &volume).unwrap();
    lib.set_folder_volume(&mount.join("2026/Day 2"), &volume)
        .unwrap();
    fs::rename(mount.join("2026"), tmp.path().join("Elsewhere")).unwrap();
    assert!(mount.is_dir(), "the mount point folder is still there");
    lib.verify_paths().unwrap();
    let offline = lib.offline_assets().unwrap();
    assert_eq!(offline.len(), 3);
    // Before the fix: None ("the file is missing"), as the folder exists.
    assert!(
        offline.iter().all(|a| a.volume.as_deref() == Some("Card")),
        "{offline:?}"
    );
}

/// A session photograph with a rating and a written XMP.
fn rated_with_xmp(lib: &Library, path: &Path, seed: u8, rating: u8) -> String {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes(seed, 70_000)).unwrap();
    let asset = lib.add_asset(path).unwrap();
    let mut meta = lib.metadata(&asset.id).unwrap();
    meta.rating = rating;
    lib.set_metadata(&asset.id, &meta).unwrap();
    lib.sync_xmp(&asset.id).unwrap();
    asset.id
}

fn xmp_rating(path: &Path) -> u8 {
    let text = fs::read_to_string(path).unwrap();
    rawtonic_metadata::read_xmp(&text).unwrap().0.rating
}

#[test]
fn a_photograph_relinked_without_its_xmp_gets_a_new_one_and_a_foreign_xmp_is_kept() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("Job");
    let lib = Library::create(&dir, "Job", Mode::Session).unwrap();
    let day = dir.join("Capture/Day1");
    let a = rated_with_xmp(&lib, &day.join("A.ARW"), 5, 3);
    let b = rated_with_xmp(&lib, &day.join("B.ARW"), 6, 4);
    // Only the photographs are moved (Finder, another tool); their XMPs stay.
    let moved = dir.join("Capture/Moved");
    fs::create_dir_all(&moved).unwrap();
    fs::rename(day.join("A.ARW"), moved.join("A.ARW")).unwrap();
    fs::rename(day.join("B.ARW"), moved.join("B.ARW")).unwrap();
    // An XMP of something else already lies beside B at the new place.
    let foreign = "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"></x:xmpmeta>";
    fs::write(moved.join("B.xmp"), foreign).unwrap();
    assert_eq!(lib.verify_paths().unwrap(), 2);

    let report = lib.relink(&a, &moved.join("A.ARW"), true).unwrap();
    assert_eq!(report.relinked.len(), 2, "{report:?}");
    // A's XMP is written at its new place (before the fix: "XMP conflict:
    // external changes must be reviewed", for good).
    lib.sync_xmp(&a).unwrap();
    assert_eq!(xmp_rating(&moved.join("A.xmp")), 3);
    // B's foreign XMP is not replaced: it stays a conflict to review.
    assert!(lib.sync_xmp(&b).is_err());
    assert_eq!(fs::read_to_string(moved.join("B.xmp")).unwrap(), foreign);
    // The XMPs left at the old place are untouched.
    assert_eq!(xmp_rating(&day.join("A.xmp")), 3);
    assert_eq!(xmp_rating(&day.join("B.xmp")), 4);
}

#[test]
fn a_photograph_moved_with_its_xmp_keeps_syncing_it() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("Job");
    let lib = Library::create(&dir, "Job", Mode::Session).unwrap();
    let photo = dir.join("Capture/A.ARW");
    let a = rated_with_xmp(&lib, &photo, 7, 2);
    let away = tmp.path().join("away");
    fs::create_dir_all(&away).unwrap();
    fs::rename(&photo, away.join("A.ARW")).unwrap();
    fs::rename(dir.join("Capture/A.xmp"), away.join("A.xmp")).unwrap();
    assert_eq!(lib.verify_paths().unwrap(), 1);
    // Back in the session with its XMP: that XMP is still the one synced.
    let back = dir.join("Selects");
    fs::rename(away.join("A.ARW"), back.join("A.ARW")).unwrap();
    fs::rename(away.join("A.xmp"), back.join("A.xmp")).unwrap();
    lib.relink(&a, &back.join("A.ARW"), false).unwrap();
    let mut meta = lib.metadata(&a).unwrap();
    meta.rating = 5;
    lib.set_metadata(&a, &meta).unwrap();
    lib.sync_xmp(&a).unwrap();
    assert_eq!(xmp_rating(&back.join("A.xmp")), 5);
}
