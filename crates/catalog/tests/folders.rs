//! Catalog folders (where the registered photographs are, by drive) and the
//! virtual copies a catalog keeps for photographs that may go offline.
use rawtonic_catalog::{Library, Mode, Volume, folders, volume};
use std::{fs, path::Path};

fn photo(path: &Path, seed: u8) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        path,
        (0..150_000u32)
            .map(|i| (i as u8).wrapping_mul(17) ^ seed)
            .collect::<Vec<_>>(),
    )
    .unwrap();
}

#[test]
fn folders_follow_relinked_photographs_and_keep_missing_ones() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let lib = Library::create(&root.join("Main.catalog"), "Main", Mode::Catalog).unwrap();
    let card = root.join("Card/2026");
    photo(&card.join("A.NEF"), 1);
    photo(&card.join("B.NEF"), 2);
    let a = lib.add_asset(&card.join("A.NEF")).unwrap();
    lib.add_asset(&card.join("B.NEF")).unwrap();
    // A session-internal photograph is not a catalog folder.
    photo(&root.join("Main.catalog/Capture/C.NEF"), 3);
    lib.add_asset(&root.join("Main.catalog/Capture/C.NEF"))
        .unwrap();

    let records = lib.folders().unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!((records[0].photos, records[0].missing), (2, 0));

    // A moves to another folder outside Rawtonic and is relinked there; B is
    // deleted: its folder stays with one missing photograph.
    let elsewhere = root.join("Archive/2026 Trip");
    fs::create_dir_all(&elsewhere).unwrap();
    fs::rename(card.join("A.NEF"), elsewhere.join("A.NEF")).unwrap();
    fs::remove_file(card.join("B.NEF")).unwrap();
    lib.relink(&a.id, &elsewhere.join("A.NEF"), false).unwrap();
    lib.verify_paths().unwrap();
    let records = lib.folders().unwrap();
    let summary: Vec<_> = records
        .iter()
        .map(|r| (r.path.clone(), r.photos, r.missing))
        .collect();
    assert_eq!(summary, [(elsewhere.clone(), 1, 0), (card.clone(), 1, 1)]);
    assert_eq!(lib.assets(None).unwrap().len(), 3, "nothing removed");

    // Grouped by drive: both folders are on the temporary folder's drive.
    let groups = folders::by_volume(
        &records,
        &[root.join("Main.catalog")],
        &folders::SystemProbe,
    );
    let listed: usize = groups.iter().map(|g| g.folders.len()).sum();
    assert_eq!(listed, 2);
    assert!(groups.iter().all(|g| g.online));
    let card_folder = groups
        .iter()
        .flat_map(|g| &g.folders)
        .find(|f| f.path == card)
        .unwrap();
    assert!(card_folder.exists && card_folder.offline == 1);
}

#[test]
fn a_disconnected_drive_keeps_its_folders_under_its_name() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let lib = Library::create(&root.join("Main.catalog"), "Main", Mode::Catalog).unwrap();
    let mount = root.join("Card");
    photo(&mount.join("2026/A.NEF"), 1);
    lib.add_asset(&mount.join("2026/A.NEF")).unwrap();
    let card = Volume {
        kind: rawtonic_catalog::volume::VolumeKind::Local,
        id: "0A1B2C3D-0000-4000-8000-0000000000AA".into(),
        name: "Card".into(),
        mount_point: mount.clone(),
    };
    lib.set_folder_volume(&mount.join("2026"), &card).unwrap();
    fs::rename(&mount, root.join("Unplugged")).unwrap();
    lib.verify_paths().unwrap();

    let groups = folders::by_volume(&lib.folders().unwrap(), &[], &folders::SystemProbe);
    let group = groups
        .iter()
        .find(|g| g.key == format!("id:{}", card.id))
        .unwrap();
    assert_eq!(group.name, "Card");
    assert!(!group.online);
    assert_eq!(group.free_bytes, None);
    assert_eq!(group.folders[0].path, mount.join("2026"));
    assert!(!group.folders[0].exists);
    assert_eq!(group.folders[0].offline, 1);
}

#[test]
fn virtual_copies_are_kept_with_their_photograph() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let lib = Library::create(&root.join("Main.catalog"), "Main", Mode::Catalog).unwrap();
    let path = root.join("Card/A.NEF");
    photo(&path, 1);
    let asset = lib.add_asset(&path).unwrap();
    lib.save_editor_state(
        &asset.id,
        &serde_json::json!({"rating": 2, "adjustments": {}}),
    )
    .unwrap();

    let copy = serde_json::json!({"rating": 4, "adjustments": {"exposure": 1.0}});
    lib.save_copy_state(&asset.id, "a1b2c3", &copy).unwrap();
    lib.save_copy_state(&asset.id, "0f0f0f", &serde_json::json!({"rating": 1}))
        .unwrap();
    let edited = serde_json::json!({"rating": 5, "adjustments": {"exposure": 2.0}});
    lib.save_copy_state(&asset.id, "a1b2c3", &edited).unwrap();
    assert!(lib.save_copy_state(&asset.id, "../x", &copy).is_err());
    assert_eq!(lib.copies(&asset.id).unwrap(), ["0f0f0f", "a1b2c3"]);
    assert_eq!(lib.copy_state(&asset.id, "a1b2c3").unwrap(), Some(edited));
    // The master's own state is untouched.
    assert_eq!(lib.editor_state(&asset.id).unwrap().unwrap()["rating"], 2);

    // They move with the photograph.
    let moved = root.join("Archive/A.NEF");
    fs::create_dir_all(moved.parent().unwrap()).unwrap();
    fs::rename(&path, &moved).unwrap();
    lib.relocate(&path, &moved).unwrap();
    let again = lib.asset_at(&moved).unwrap().unwrap();
    assert_eq!(again.id, asset.id);
    assert_eq!(lib.copies(&again.id).unwrap().len(), 2);

    assert!(lib.remove_copy(&asset.id, "0f0f0f").unwrap());
    assert!(!lib.remove_copy(&asset.id, "0f0f0f").unwrap());
    assert_eq!(lib.copies(&asset.id).unwrap(), ["a1b2c3"]);
}

#[test]
fn the_mount_list_is_watched_by_system_notifications_where_there_are_any() {
    let key = volume::mounts_key();
    if cfg!(any(target_os = "macos", target_os = "linux")) {
        assert!(!key.is_empty(), "the system disk is mounted");
    }
    let kind = volume::watch_mounts(|| {});
    if cfg!(any(target_os = "macos", target_os = "linux")) {
        assert_eq!(kind, volume::WatchKind::Notifications);
    } else {
        assert_eq!(kind, volume::WatchKind::Polling);
    }
}

/// Mounts a small file system and unmounts it again, measuring how soon the
/// watcher reports each. Needs privileges (`hdiutil` on macOS, root for
/// `mount` on Linux): `cargo test -p rawtonic-catalog --test folders -- --ignored --nocapture`.
#[test]
#[ignore = "mounts a file system"]
fn mounting_and_ejecting_is_reported_at_once() {
    use std::process::Command;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};
    let (tx, rx) = mpsc::channel::<Instant>();
    volume::watch_mounts(move || {
        let _ = tx.send(Instant::now());
    });
    let tmp = tempfile::tempdir().unwrap();
    let point = tmp.path().join("point");
    fs::create_dir_all(&point).unwrap();
    let image = tmp.path().join("watch.dmg");
    let (mount, unmount): (Vec<String>, Vec<String>) = if cfg!(target_os = "macos") {
        let status = Command::new("hdiutil")
            .args([
                "create",
                "-size",
                "8m",
                "-fs",
                "APFS",
                "-volname",
                "WatchTest",
            ])
            .arg(&image)
            .status()
            .unwrap();
        assert!(status.success());
        (
            vec![
                "hdiutil".into(),
                "attach".into(),
                "-mountpoint".into(),
                point.display().to_string(),
                image.display().to_string(),
            ],
            vec![
                "hdiutil".into(),
                "detach".into(),
                point.display().to_string(),
            ],
        )
    } else {
        // A drive (a loop device), not a virtual file system: those are
        // left out of the mount list on purpose.
        fs::write(&image, vec![0u8; 16 << 20]).unwrap();
        let status = Command::new("mkfs.ext4")
            .arg("-q")
            .arg(&image)
            .status()
            .unwrap();
        assert!(status.success());
        (
            vec![
                "mount".into(),
                "-o".into(),
                "loop".into(),
                image.display().to_string(),
                point.display().to_string(),
            ],
            vec!["umount".into(), point.display().to_string()],
        )
    };
    for (what, command) in [("mount", &mount), ("unmount", &unmount)] {
        while rx.try_recv().is_ok() {}
        let started = Instant::now();
        let status = Command::new(&command[0])
            .args(&command[1..])
            .status()
            .unwrap();
        let returned = Instant::now();
        assert!(status.success(), "{what} failed");
        let seen = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("no change seen");
        let after_start = seen.duration_since(started).as_secs_f64() * 1000.0;
        let command_ms = returned.duration_since(started).as_secs_f64() * 1000.0;
        println!(
            "{what}: command took {command_ms:.0} ms; reported {after_start:.0} ms after it started \
             ({:+.0} ms relative to its return)",
            after_start - command_ms
        );
    }
}
