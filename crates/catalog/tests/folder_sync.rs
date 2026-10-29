use rawtonic_catalog::folder_sync::{
    Cancelled, SyncAction, SyncOptions, SyncPlan, SyncProgress, is_photo,
};
use rawtonic_catalog::{ImportKind, Library, Mode, Volume};
use serde_json::json;
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};

fn scan(lib: &Library, folder: Option<&Path>) -> SyncPlan {
    let cancel = AtomicBool::new(false);
    lib.scan_folder_sync(
        folder,
        &SyncOptions {
            accept: &is_photo,
            cancel: &cancel,
            progress: &|_| {},
        },
    )
    .unwrap()
}

fn fill(lib: &Library) {
    lib.fill_all_checksums(&AtomicBool::new(false)).unwrap();
}

/// Distinct RAW-sized content per name (more than the fingerprint samples).
fn photo(path: &Path, seed: u8) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut bytes = vec![seed; 300_000];
    bytes[150_000] = seed.wrapping_add(1);
    fs::write(path, bytes).unwrap();
}

/// The accepted defaults of the review dialog: verified moves and new files.
fn defaults(plan: &SyncPlan) -> Vec<SyncAction> {
    let mut actions: Vec<SyncAction> = plan.verified_moves.iter().map(SyncAction::from).collect();
    actions.extend(plan.new_files.iter().map(|f| SyncAction::Add {
        path: f.path.clone(),
        identity: f.identity.clone(),
    }));
    actions
}

fn session(dir: &Path) -> Library {
    Library::create(&dir.join("Job"), "Job", Mode::Session).unwrap()
}

#[test]
fn renames_and_moves_keep_their_photographs_with_edits_ratings_keywords_and_albums() {
    let d = tempfile::tempdir().unwrap();
    let lib = session(d.path());
    let capture = lib.directory.join("Capture");
    for (name, seed) in [("A.ARW", 1), ("B.ARW", 2), ("C.ARW", 3), ("Day 1/D.ARW", 4)] {
        photo(&capture.join(name), seed);
    }
    let report = lib
        .add_files(&[
            capture.join("A.ARW"),
            capture.join("B.ARW"),
            capture.join("C.ARW"),
            capture.join("Day 1/D.ARW"),
        ])
        .unwrap();
    let id = |name: &str| {
        report
            .added
            .iter()
            .find(|a| a.path.ends_with(name))
            .unwrap()
            .id
            .clone()
    };
    let (a, b, c, d1) = (id("A.ARW"), id("B.ARW"), id("C.ARW"), id("D.ARW"));
    lib.save_editor_state(
        &a,
        &json!({"rating":4,"tags":["user:harbour"],"adjustments":{"exposure":1.25}}),
    )
    .unwrap();
    let album = lib.create_collection("Picks", None).unwrap();
    lib.add_to_collection(&album.id, &[a.clone(), b.clone()])
        .unwrap();
    fill(&lib);
    // Outside Rawtonic: A renamed, B moved to a subfolder, C moved and
    // renamed, D's folder renamed.
    fs::rename(capture.join("A.ARW"), capture.join("A renamed.ARW")).unwrap();
    fs::create_dir_all(capture.join("Day 2")).unwrap();
    fs::rename(capture.join("B.ARW"), capture.join("Day 2/B.ARW")).unwrap();
    fs::rename(capture.join("C.ARW"), capture.join("Day 2/C final.ARW")).unwrap();
    fs::rename(capture.join("Day 1"), capture.join("Day one")).unwrap();

    let plan = scan(&lib, Some(&capture));
    assert_eq!(plan.verified_moves.len(), 4, "{plan:#?}");
    assert!(plan.new_files.is_empty() && plan.removed.is_empty() && plan.contested.is_empty());
    let applied = lib.apply_folder_sync(&defaults(&plan)).unwrap();
    assert_eq!(applied.moved.len(), 4);
    assert!(applied.skipped.is_empty());

    let moved = lib
        .asset_at(&capture.join("A renamed.ARW"))
        .unwrap()
        .unwrap();
    assert_eq!(moved.id, a);
    assert_eq!(moved.rating, 4);
    assert_eq!(lib.metadata(&a).unwrap().keywords, ["harbour"]);
    assert_eq!(
        lib.editor_state(&a).unwrap().unwrap()["adjustments"]["exposure"],
        1.25
    );
    assert_eq!(
        lib.asset_at(&capture.join("Day 2/B.ARW"))
            .unwrap()
            .unwrap()
            .id,
        b
    );
    assert_eq!(
        lib.asset_at(&capture.join("Day 2/C final.ARW"))
            .unwrap()
            .unwrap()
            .id,
        c
    );
    assert_eq!(
        lib.asset_at(&capture.join("Day one/D.ARW"))
            .unwrap()
            .unwrap()
            .id,
        d1
    );
    let in_album: Vec<_> = lib
        .collection_assets(&album.id)
        .unwrap()
        .into_iter()
        .map(|a| a.id)
        .collect();
    assert_eq!(in_album.len(), 2);
    assert!(in_album.contains(&a) && in_album.contains(&b));
    assert_eq!(lib.summary().unwrap().assets, 4);
    // Nothing left to do.
    assert!(scan(&lib, Some(&capture)).is_empty());
}

#[test]
fn new_deleted_and_copied_files_are_found_and_removal_only_removes_the_entry() {
    let d = tempfile::tempdir().unwrap();
    let lib = session(d.path());
    let capture = lib.directory.join("Capture");
    photo(&capture.join("A.ARW"), 1);
    photo(&capture.join("B.ARW"), 2);
    let ids: Vec<String> = lib
        .add_files(&[capture.join("A.ARW"), capture.join("B.ARW")])
        .unwrap()
        .added
        .into_iter()
        .map(|a| a.id)
        .collect();
    fill(&lib);
    fs::remove_file(capture.join("B.ARW")).unwrap();
    photo(&capture.join("New.ARW"), 9);
    fs::copy(capture.join("A.ARW"), capture.join("A copy.ARW")).unwrap();
    fs::write(
        capture.join("New.xmp"),
        r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmp:Rating="3"/></rdf:RDF></x:xmpmeta>"#,
    )
    .unwrap();

    let plan = scan(&lib, None);
    assert_eq!(plan.new_files.len(), 1);
    assert_eq!(plan.copies.len(), 1);
    assert_eq!(plan.copies[0].copy_of, [capture.join("A.ARW")]);
    assert_eq!(plan.removed.len(), 1);
    assert_eq!(plan.removed[0].asset_id, ids[1]);
    assert!(plan.verified_moves.is_empty() && plan.probable_moves.is_empty());

    // Defaults: the new file only; the removal is not applied unless accepted.
    let applied = lib.apply_folder_sync(&defaults(&plan)).unwrap();
    assert_eq!(applied.added, [capture.join("New.ARW")]);
    assert_eq!(
        lib.asset_at(&capture.join("New.ARW"))
            .unwrap()
            .unwrap()
            .rating,
        3,
        "added with its XMP"
    );
    let batch = lib.previous_import().unwrap().unwrap();
    assert_eq!((batch.kind, batch.count), (ImportKind::Sync, 1));
    assert!(lib.asset(&ids[1]).is_ok(), "not removed without acceptance");

    let applied = lib
        .apply_folder_sync(&[SyncAction::Remove {
            asset_id: ids[1].clone(),
            path: capture.join("B.ARW"),
        }])
        .unwrap();
    assert_eq!(applied.removed, [capture.join("B.ARW")]);
    assert!(lib.asset(&ids[1]).is_err());
    // Files on disk are as they were.
    let mut names: Vec<_> = fs::read_dir(&capture)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, ["A copy.ARW", "A.ARW", "New.ARW", "New.xmp"]);
}

#[test]
fn a_file_matching_a_missing_and_a_present_photograph_is_the_users_choice() {
    let d = tempfile::tempdir().unwrap();
    let lib = session(d.path());
    let capture = lib.directory.join("Capture");
    // Two photographs with the same bytes (a duplicate the user kept).
    photo(&capture.join("A.ARW"), 5);
    fs::copy(capture.join("A.ARW"), capture.join("B.ARW")).unwrap();
    let report = lib
        .add_files(&[capture.join("A.ARW"), capture.join("B.ARW")])
        .unwrap();
    let a = report.added[0].id.clone();
    fill(&lib);
    fs::rename(capture.join("A.ARW"), capture.join("C.ARW")).unwrap();
    // Two files claim one missing photograph too.
    photo(&capture.join("X.ARW"), 7);
    let x = lib.add_asset(&capture.join("X.ARW")).unwrap().id;
    fill(&lib);
    fs::copy(capture.join("X.ARW"), capture.join("X1.ARW")).unwrap();
    fs::rename(capture.join("X.ARW"), capture.join("X2.ARW")).unwrap();

    let plan = scan(&lib, None);
    assert!(plan.verified_moves.is_empty(), "{plan:#?}");
    assert_eq!(plan.contested.len(), 3);
    let c = plan
        .contested
        .iter()
        .find(|c| c.path.ends_with("C.ARW"))
        .unwrap();
    assert_eq!(c.moves.len(), 1);
    assert_eq!(c.moves[0].asset_id, a);
    assert!(c.moves[0].verified);
    assert_eq!(c.copy_of, [capture.join("B.ARW")]);
    let claims: Vec<_> = plan
        .contested
        .iter()
        .filter(|c| c.moves.iter().any(|m| m.asset_id == x))
        .collect();
    assert_eq!(claims.len(), 2);
    assert!(
        plan.removed.is_empty(),
        "claimed photographs are not removed"
    );
    // The user picks: C is A moved; X2 is X, X1 a new photograph.
    let x2 = claims.iter().find(|c| c.path.ends_with("X2.ARW")).unwrap();
    let x1 = claims.iter().find(|c| c.path.ends_with("X1.ARW")).unwrap();
    let applied = lib
        .apply_folder_sync(&[
            SyncAction::from(&c.moves[0]),
            SyncAction::from(&x2.moves[0]),
            SyncAction::Add {
                path: x1.path.clone(),
                identity: x1.identity.clone(),
            },
            // Two choices for one photograph: the second is refused.
            SyncAction::from(&x1.moves[0]),
        ])
        .unwrap();
    assert_eq!(applied.moved.len(), 2);
    assert_eq!(applied.skipped.len(), 1, "{:?}", applied.skipped);
    assert_eq!(applied.added, [capture.join("X1.ARW")]);
    assert_eq!(lib.asset_at(&capture.join("C.ARW")).unwrap().unwrap().id, a);
    assert_eq!(
        lib.asset_at(&capture.join("X2.ARW")).unwrap().unwrap().id,
        x
    );
}

#[test]
fn without_a_recorded_hash_a_move_is_only_probable() {
    let d = tempfile::tempdir().unwrap();
    let lib = session(d.path());
    let capture = lib.directory.join("Capture");
    photo(&capture.join("A.ARW"), 1);
    let a = lib.add_asset(&capture.join("A.ARW")).unwrap().id;
    fs::rename(capture.join("A.ARW"), capture.join("B.ARW")).unwrap();
    let plan = scan(&lib, None);
    assert_eq!(plan.probable_moves.len(), 1);
    assert!(!plan.probable_moves[0].verified);
    assert!(plan.verified_moves.is_empty());
    // Not accepted by default: nothing happens.
    assert!(
        lib.apply_folder_sync(&defaults(&plan))
            .unwrap()
            .moved
            .is_empty()
    );
    let applied = lib
        .apply_folder_sync(&[SyncAction::from(&plan.probable_moves[0])])
        .unwrap();
    assert_eq!(applied.moved.len(), 1);
    assert_eq!(lib.asset_at(&capture.join("B.ARW")).unwrap().unwrap().id, a);

    // A hash on record that differs from the file: not this photograph.
    photo(&capture.join("C.ARW"), 3);
    let c = lib.add_asset(&capture.join("C.ARW")).unwrap().id;
    fill(&lib);
    fs::remove_file(capture.join("C.ARW")).unwrap();
    // Same size and sampled parts, different middle.
    let mut bytes = vec![3u8; 300_000];
    bytes[150_000] = 4;
    bytes[100_000] = 9;
    fs::write(capture.join("D.ARW"), bytes).unwrap();
    let plan = scan(&lib, None);
    assert!(plan.verified_moves.is_empty() && plan.probable_moves.is_empty());
    assert_eq!(plan.new_files.len(), 1);
    assert_eq!(plan.removed[0].asset_id, c);
}

#[test]
fn photographs_on_a_disconnected_drive_are_unavailable_never_moved_or_removed() {
    let d = tempfile::tempdir().unwrap();
    let root = d.path().canonicalize().unwrap();
    let lib = Library::create(&root.join("Main.catalog"), "Main", Mode::Catalog).unwrap();
    let mount = root.join("Volumes/Photos");
    let folder = mount.join("2026");
    photo(&folder.join("A.ARW"), 1);
    photo(&folder.join("B.ARW"), 2);
    let a = lib.add_asset(&folder.join("A.ARW")).unwrap().id;
    lib.add_asset(&folder.join("B.ARW")).unwrap();
    fill(&lib);
    lib.set_folder_volume(
        &folder,
        &Volume {
            kind: rawtonic_catalog::volume::VolumeKind::Local,
            id: "PHOTOS-UUID".into(),
            name: "Photos".into(),
            mount_point: mount.clone(),
        },
    )
    .unwrap();
    // The drive is ejected: its mount point is gone. A's bytes are on
    // another drive meanwhile (a backup copy).
    let elsewhere = root.join("Desk");
    fs::create_dir_all(&elsewhere).unwrap();
    fs::copy(folder.join("A.ARW"), elsewhere.join("A.ARW")).unwrap();
    fs::rename(&mount, root.join("ejected")).unwrap();

    let plan = scan(&lib, Some(&folder));
    assert_eq!(plan.unavailable.len(), 2);
    assert_eq!(plan.unavailable[0].volume.as_deref(), Some("Photos"));
    assert!(plan.removed.is_empty() && plan.verified_moves.is_empty());

    let whole = scan(&lib, None);
    assert_eq!(whole.unavailable.len(), 2);
    assert!(whole.removed.is_empty() && whole.verified_moves.is_empty());
    assert!(whole.probable_moves.is_empty() && whole.contested.is_empty());
    // The backup copy is a copy of an unavailable photograph, never its move.
    let desk = scan(&lib, Some(&elsewhere));
    assert!(desk.verified_moves.is_empty());
    assert_eq!(desk.copies.len(), 1);
    // A removal sent anyway is refused while the drive is away.
    let applied = lib
        .apply_folder_sync(&[SyncAction::Remove {
            asset_id: a.clone(),
            path: folder.join("A.ARW"),
        }])
        .unwrap();
    assert!(applied.removed.is_empty());
    assert_eq!(applied.skipped.len(), 1);
    assert!(lib.asset(&a).is_ok());
    // A folder that is simply not there is an error, not a removal.
    assert!(
        lib.scan_folder_sync(
            Some(&root.join("Nowhere")),
            &SyncOptions {
                accept: &is_photo,
                cancel: &AtomicBool::new(false),
                progress: &|_| {},
            },
        )
        .is_err()
    );
}

#[test]
fn changes_between_scan_and_apply_are_skipped() {
    let d = tempfile::tempdir().unwrap();
    let lib = session(d.path());
    let capture = lib.directory.join("Capture");
    for (name, seed) in [("A.ARW", 1), ("B.ARW", 2), ("C.ARW", 3)] {
        photo(&capture.join(name), seed);
    }
    let ids: Vec<String> = lib
        .add_files(&[
            capture.join("A.ARW"),
            capture.join("B.ARW"),
            capture.join("C.ARW"),
        ])
        .unwrap()
        .added
        .into_iter()
        .map(|a| a.id)
        .collect();
    fill(&lib);
    fs::rename(capture.join("A.ARW"), capture.join("A2.ARW")).unwrap();
    fs::rename(capture.join("B.ARW"), capture.join("B2.ARW")).unwrap();
    fs::remove_file(capture.join("C.ARW")).unwrap();
    photo(&capture.join("N.ARW"), 8);
    let plan = scan(&lib, None);
    assert_eq!(plan.verified_moves.len(), 2);
    // Meanwhile: A2 is edited by another program (other bytes), B is moved
    // back, C comes back, N is replaced.
    let mut changed = fs::read(capture.join("A2.ARW")).unwrap();
    changed.push(0);
    fs::write(capture.join("A2.ARW"), changed).unwrap();
    fs::rename(capture.join("B2.ARW"), capture.join("B.ARW")).unwrap();
    photo(&capture.join("C.ARW"), 3);
    photo(&capture.join("N.ARW"), 6);
    let mut actions = defaults(&plan);
    actions.push(SyncAction::Remove {
        asset_id: ids[2].clone(),
        path: capture.join("C.ARW"),
    });
    let applied = lib.apply_folder_sync(&actions).unwrap();
    assert!(applied.moved.is_empty() && applied.added.is_empty() && applied.removed.is_empty());
    assert_eq!(applied.skipped.len(), 4, "{:?}", applied.skipped);
    assert_eq!(lib.asset(&ids[0]).unwrap().path, capture.join("A.ARW"));
    assert_eq!(lib.asset(&ids[1]).unwrap().path, capture.join("B.ARW"));
    assert!(lib.asset(&ids[2]).is_ok());
    assert!(lib.asset_at(&capture.join("N.ARW")).unwrap().is_none());
}

#[test]
fn an_xmp_that_moved_with_its_photograph_is_kept_one_left_behind_is_not_claimed() {
    let d = tempfile::tempdir().unwrap();
    let lib = session(d.path());
    let capture = lib.directory.join("Capture");
    photo(&capture.join("A.ARW"), 1);
    photo(&capture.join("B.ARW"), 2);
    let a = lib.add_asset(&capture.join("A.ARW")).unwrap().id;
    let b = lib.add_asset(&capture.join("B.ARW")).unwrap().id;
    for id in [&a, &b] {
        let mut meta = lib.metadata(id).unwrap();
        meta.rating = 5;
        lib.set_metadata(id, &meta).unwrap();
        lib.sync_xmp(id).unwrap();
    }
    fill(&lib);
    fs::create_dir_all(capture.join("Moved")).unwrap();
    // A moves with its XMP; B without.
    fs::rename(capture.join("A.ARW"), capture.join("Moved/A.ARW")).unwrap();
    fs::rename(capture.join("A.xmp"), capture.join("Moved/A.xmp")).unwrap();
    fs::rename(capture.join("B.ARW"), capture.join("Moved/B.ARW")).unwrap();
    let plan = scan(&lib, None);
    lib.apply_folder_sync(&defaults(&plan)).unwrap();
    assert_eq!(
        lib.summary().unwrap().xmp_pending,
        1,
        "B's XMP is to be written anew"
    );
    // A's sidecar is still the library's: written again without conflict.
    lib.sync_xmp(&a).unwrap();
    lib.sync_xmp(&b).unwrap();
    assert!(capture.join("Moved/B.xmp").is_file());
    assert!(
        capture.join("B.xmp").is_file(),
        "the old sidecar is never deleted"
    );
}

#[test]
fn a_scan_can_be_cancelled_and_reports_progress() {
    let d = tempfile::tempdir().unwrap();
    let lib = session(d.path());
    for i in 0..3u8 {
        photo(&lib.directory.join(format!("Capture/{i}.ARW")), i + 1);
    }
    let cancel = AtomicBool::new(true);
    let error = lib
        .scan_folder_sync(
            None,
            &SyncOptions {
                accept: &is_photo,
                cancel: &cancel,
                progress: &|_| {},
            },
        )
        .unwrap_err();
    assert!(error.downcast_ref::<Cancelled>().is_some());
    let seen = std::sync::Mutex::new(Vec::<SyncProgress>::new());
    let plan = lib
        .scan_folder_sync(
            None,
            &SyncOptions {
                accept: &is_photo,
                cancel: &AtomicBool::new(false),
                progress: &|p| seen.lock().unwrap().push(p),
            },
        )
        .unwrap();
    assert_eq!(plan.new_files.len(), 3);
    let last = *seen.lock().unwrap().last().unwrap();
    assert_eq!((last.done, last.total), (3, 3));
    // A session folder outside the session is refused.
    assert!(lib.sync_scope(Some(d.path())).is_err());
}

/// A case-only rename on a case-insensitive volume is the photograph's own
/// file under a new name: a verified move. The file system resolves both
/// names to one file (on Windows by its file number, whatever the letter
/// case), so it must never be taken for a copy of the photograph that is
/// still at its case-variant name, which would leave both listed.
///
/// A file number identifies a file on its volume; a file system mounted on
/// a file image made in memory (a Windows RAM disk) may report 0 for every
/// file, which identifies none. Only that check is left out there.
#[test]
fn a_case_only_rename_is_followed_on_a_case_insensitive_volume() {
    #[cfg(windows)]
    fn identifies_files(capture: &Path) -> bool {
        let first = capture.join("a.arw");
        let second = capture.join("B.ARW");
        let index = |path: &Path| {
            use std::os::windows::fs::MetadataExt;
            fs::metadata(path).unwrap().file_index()
        };
        let a = index(&first);
        a.is_some_and(|a| a != 0 && index(&second).is_some_and(|b| a != b))
    }
    #[cfg(not(windows))]
    fn identifies_files(_: &Path) -> bool {
        true
    }
    let d = tempfile::tempdir().unwrap();
    let lib = session(d.path());
    let capture = lib.directory.join("Capture");
    photo(&capture.join("a.arw"), 1);
    photo(&capture.join("B.ARW"), 2);
    let can_tell_files = identifies_files(&capture);
    if !capture.join("A.ARW").exists() {
        // A case-sensitive file system: a case rename is an ordinary
        // rename. Its file is identified, so no copy of a photograph still
        // there can be the same file.
        if can_tell_files {
            let a = lib.add_asset(&capture.join("a.arw")).unwrap().id;
            fs::rename(capture.join("a.arw"), capture.join("A.ARW")).unwrap();
            let plan = scan(&lib, None);
            assert_eq!(plan.verified_moves.len(), 1, "{plan:#?}");
            assert!(plan.copies.is_empty() && plan.new_files.is_empty());
            lib.apply_folder_sync(&defaults(&plan)).unwrap();
            let listed: Vec<PathBuf> = lib
                .assets(None)
                .unwrap()
                .into_iter()
                .map(|x| x.path)
                .collect();
            assert_eq!(listed, [capture.join("A.ARW")]);
            assert_eq!(lib.asset(&a).unwrap().path, capture.join("A.ARW"));
        }
        return;
    }
    let a = lib.add_asset(&capture.join("a.arw")).unwrap().id;
    let b = lib.add_asset(&capture.join("B.ARW")).unwrap().id;
    fs::rename(capture.join("a.arw"), capture.join("A.ARW")).unwrap();
    let plan = scan(&lib, None);
    // Before the fix on Windows no file identity was known, so the new name
    // was `copies` of the photograph still listed as `a.arw`, and
    // `verified_moves` stayed empty.
    assert!(
        plan.copies.is_empty() || !can_tell_files,
        "a case variant is the photograph's own file: {plan:#?}"
    );
    if can_tell_files {
        assert_eq!(plan.verified_moves.len(), 1, "{plan:#?}");
        assert_eq!(plan.verified_moves[0].asset_id, a);
        // A native place: stored as portable relative path by the library
        // (`portable_relative` refuses an unstored `\`), so a plan that
        // shows it portably is turned back before it is applied.
        assert_eq!(plan.verified_moves[0].to, capture.join("A.ARW"));
        assert!(plan.new_files.is_empty());
        let applied = lib.apply_folder_sync(&defaults(&plan)).unwrap();
        assert_eq!(applied.moved.len(), 1, "{applied:#?}");
    }
    // Once followed, it is at its new name, listed once.
    let listed: Vec<PathBuf> = lib
        .assets(None)
        .unwrap()
        .into_iter()
        .map(|x| x.path)
        .collect();
    assert_eq!(listed, [capture.join("A.ARW"), capture.join("B.ARW")]);
    assert_eq!(lib.asset(&a).unwrap().path, capture.join("A.ARW"));
    assert_eq!(lib.asset(&b).unwrap().path, capture.join("B.ARW"), "B is B");
    assert!(scan(&lib, None).is_empty(), "reported once");
}

#[cfg(unix)]
#[test]
fn a_hard_link_to_a_photograph_that_is_still_there_is_a_copy_not_a_move() {
    let d = tempfile::tempdir().unwrap();
    let lib = session(d.path());
    let capture = lib.directory.join("Capture");
    photo(&capture.join("A.ARW"), 1);
    let a = lib.add_asset(&capture.join("A.ARW")).unwrap().id;
    fill(&lib);
    fs::hard_link(capture.join("A.ARW"), capture.join("A link.ARW")).unwrap();
    let plan = scan(&lib, None);
    assert!(plan.verified_moves.is_empty(), "{plan:#?}");
    assert_eq!(plan.copies.len(), 1);
    assert_eq!(plan.copies[0].path, capture.join("A link.ARW"));
    assert_eq!(plan.copies[0].copy_of, [capture.join("A.ARW")]);
    lib.apply_folder_sync(&defaults(&plan)).unwrap();
    assert_eq!(lib.asset(&a).unwrap().path, capture.join("A.ARW"));
}

/// A session keeps its photographs inside its folder with relative paths:
/// moves between its own folders are followed, its working folders (Trash,
/// Output, caches) and libraries inside it are not scanned, a photograph
/// moved out of it is only missing (never followed outside), and a session
/// moved as a whole is still in sync.
#[test]
fn a_session_follows_moves_between_its_folders_and_stays_inside_itself() {
    let d = tempfile::tempdir().unwrap();
    let root = d.path().canonicalize().unwrap();
    let lib = session(&root);
    let capture = lib.directory.join("Capture");
    let selects = lib.directory.join("Selects");
    photo(&capture.join("A.ARW"), 1);
    photo(&capture.join("B.ARW"), 2);
    let a = lib.add_asset(&capture.join("A.ARW")).unwrap().id;
    let b = lib.add_asset(&capture.join("B.ARW")).unwrap().id;
    let mut meta = lib.metadata(&a).unwrap();
    meta.rating = 5;
    lib.set_metadata(&a, &meta).unwrap();
    fill(&lib);
    // Outside Rawtonic: A into Selects, B out of the session; files that
    // belong to no photograph in the session's working folders and in
    // another session inside this one.
    fs::rename(capture.join("A.ARW"), selects.join("A pick.ARW")).unwrap();
    fs::rename(capture.join("B.ARW"), root.join("B.ARW")).unwrap();
    for working in ["Trash", "Output", ".cache", "masks"] {
        photo(&lib.directory.join(working).join("W.ARW"), 7);
    }
    let inner = Library::create(&lib.directory.join("Inner"), "Inner", Mode::Session).unwrap();
    photo(&inner.directory.join("Capture/I.ARW"), 8);

    let plan = scan(&lib, None);
    assert_eq!(plan.folders, std::slice::from_ref(&lib.directory));
    assert_eq!(plan.verified_moves.len(), 1, "{plan:#?}");
    assert_eq!(plan.verified_moves[0].to, selects.join("A pick.ARW"));
    assert!(
        plan.new_files.is_empty() && plan.copies.is_empty(),
        "{plan:#?}"
    );
    assert_eq!(plan.removed.len(), 1);
    assert_eq!(plan.removed[0].asset_id, b);
    // A scan of a folder outside the session is refused.
    assert!(lib.sync_scope(Some(&root)).is_err());
    lib.apply_folder_sync(&defaults(&plan)).unwrap();
    let moved = lib.asset(&a).unwrap();
    assert_eq!(moved.rel_path, "Selects/A pick.ARW");
    assert_eq!(moved.rating, 5);
    // A move out of the session sent anyway is refused.
    let identity = rawtonic_catalog::folder_sync::FileIdentity::read(&root.join("B.ARW")).unwrap();
    let applied = lib
        .apply_folder_sync(&[SyncAction::Move {
            asset_id: b.clone(),
            from: capture.join("B.ARW"),
            to: root.join("B.ARW"),
            identity,
        }])
        .unwrap();
    assert!(applied.moved.is_empty());
    assert_eq!(applied.skipped.len(), 1, "{:?}", applied.skipped);
    assert_eq!(lib.asset(&b).unwrap().path, capture.join("B.ARW"));
    fs::rename(root.join("B.ARW"), capture.join("B.ARW")).unwrap();
    // The whole session moved elsewhere: its relative paths still hold.
    drop(lib);
    drop(inner);
    fs::rename(root.join("Job"), root.join("Job moved")).unwrap();
    let lib = Library::open(&root.join("Job moved")).unwrap();
    assert!(scan(&lib, None).is_empty(), "{:#?}", scan(&lib, None));
    assert_eq!(
        lib.asset(&a).unwrap().path,
        lib.directory.join("Selects/A pick.ARW")
    );
}

/// A drive or network share without a drive identity (Windows drives, SMB
/// and NAS shares on macOS) that is not connected only shows as its folders
/// being gone: its photographs are unavailable, never offered or approved
/// for removal, whether or not an empty mount point folder is left behind.
#[test]
fn photographs_of_a_folder_that_is_not_there_are_never_removed_without_a_drive_identity() {
    let d = tempfile::tempdir().unwrap();
    let root = d.path().canonicalize().unwrap();
    let lib = Library::create(&root.join("Main.catalog"), "Main", Mode::Catalog).unwrap();
    // A share with photographs in a folder of it, and one with photographs
    // right at its mount point.
    let share = root.join("Volumes/NAS");
    let folder = share.join("2026");
    photo(&folder.join("A.ARW"), 1);
    photo(&folder.join("B.ARW"), 2);
    let mount = root.join("mnt/card");
    photo(&mount.join("C.ARW"), 3);
    let ids: Vec<String> = ["A.ARW", "B.ARW"]
        .iter()
        .map(|name| lib.add_asset(&folder.join(name)).unwrap().id)
        .chain([lib.add_asset(&mount.join("C.ARW")).unwrap().id])
        .collect();
    fill(&lib);
    let removals = || -> Vec<SyncAction> {
        ids.iter()
            .map(|id| SyncAction::Remove {
                asset_id: id.clone(),
                path: lib.asset(id).unwrap().path,
            })
            .collect()
    };
    // Unmounted: the share's mount point goes, the other one stays empty.
    fs::rename(&share, root.join("unmounted")).unwrap();
    fs::remove_file(mount.join("C.ARW")).unwrap();

    let whole = scan(&lib, None);
    assert!(whole.removed.is_empty(), "{whole:#?}");
    assert_eq!(whole.unavailable.len(), 3, "{whole:#?}");
    assert!(whole.unavailable.iter().all(|m| m.volume.is_none()));
    assert!(whole.is_empty(), "nothing to act on: {whole:#?}");
    let single = scan(&lib, Some(&mount));
    assert!(single.removed.is_empty() && single.unavailable.len() == 1);
    // Removals sent anyway are refused; the photographs keep their entries.
    let applied = lib.apply_folder_sync(&removals()).unwrap();
    assert!(applied.removed.is_empty(), "{applied:#?}");
    assert_eq!(applied.skipped.len(), 3);
    assert!(ids.iter().all(|id| lib.asset(id).is_ok()));

    // Connected again (and the card's file back): nothing changed.
    fs::rename(root.join("unmounted"), &share).unwrap();
    photo(&mount.join("C.ARW"), 3);
    let again = scan(&lib, None);
    assert!(
        again.is_empty() && again.unavailable.is_empty(),
        "{again:#?}"
    );
}

/// A photograph deleted from a library folder that is there, or with a
/// folder deleted inside one, is still found removed; a folder renamed
/// inside one is still followed.
#[test]
fn photographs_deleted_inside_a_library_folder_that_is_there_are_removed() {
    let d = tempfile::tempdir().unwrap();
    let root = d.path().canonicalize().unwrap();
    let lib = Library::create(&root.join("Main.catalog"), "Main", Mode::Catalog).unwrap();
    let photos = root.join("Photos");
    let names = ["A.ARW", "B.ARW", "Day/C.ARW", "Trip/D.ARW"];
    for (seed, name) in names.iter().enumerate() {
        photo(&photos.join(name), seed as u8 + 1);
    }
    let ids: Vec<String> = names
        .iter()
        .map(|name| lib.add_asset(&photos.join(name)).unwrap().id)
        .collect();
    fill(&lib);
    fs::remove_file(photos.join("B.ARW")).unwrap();
    fs::remove_dir_all(photos.join("Day")).unwrap();
    fs::rename(photos.join("Trip"), photos.join("Trip 2026")).unwrap();

    let plan = scan(&lib, None);
    let mut removed: Vec<&str> = plan.removed.iter().map(|m| m.asset_id.as_str()).collect();
    removed.sort();
    let mut expected = [ids[1].as_str(), ids[2].as_str()];
    expected.sort();
    assert_eq!(removed, expected, "{plan:#?}");
    assert!(plan.unavailable.is_empty(), "{plan:#?}");
    assert_eq!(plan.verified_moves.len(), 1);
    assert_eq!(plan.verified_moves[0].to, photos.join("Trip 2026/D.ARW"));
    let mut actions = defaults(&plan);
    actions.extend(plan.removed.iter().map(|m| SyncAction::Remove {
        asset_id: m.asset_id.clone(),
        path: m.path.clone(),
    }));
    let applied = lib.apply_folder_sync(&actions).unwrap();
    assert_eq!(applied.removed.len(), 2, "{applied:#?}");
    assert_eq!(applied.moved.len(), 1);
    assert!(lib.asset(&ids[1]).is_err() && lib.asset(&ids[2]).is_err());
    assert_eq!(
        lib.asset(&ids[3]).unwrap().path,
        photos.join("Trip 2026/D.ARW")
    );
}

/// `scan_files_sync` classifies only the files given: it lists no folder,
/// hashes only files matching a photograph and reports nothing removed.
#[test]
fn a_scan_of_given_files_follows_their_moves_without_listing_folders() {
    let d = tempfile::tempdir().unwrap();
    let root = d.path().canonicalize().unwrap();
    let lib = Library::create(&root.join("Main.catalog"), "Main", Mode::Catalog).unwrap();
    let photos = root.join("Photos");
    photo(&photos.join("A.ARW"), 1);
    photo(&photos.join("B.ARW"), 2);
    let a = lib.add_asset(&photos.join("A.ARW")).unwrap().id;
    lib.add_asset(&photos.join("B.ARW")).unwrap();
    fill(&lib);
    fs::rename(photos.join("A.ARW"), photos.join("A renamed.ARW")).unwrap();
    fs::remove_file(photos.join("B.ARW")).unwrap();
    photo(&photos.join("Unlisted.ARW"), 5);
    let inside = lib.directory.join("Photos/N.ARW");
    photo(&inside, 9);
    let plan = lib
        .scan_files_sync(
            &[photos.join("A renamed.ARW"), inside.clone()],
            &SyncOptions {
                accept: &is_photo,
                cancel: &AtomicBool::new(false),
                progress: &|_| {},
            },
        )
        .unwrap();
    assert_eq!(plan.files, 2);
    assert_eq!(plan.verified_moves.len(), 1);
    assert_eq!(plan.verified_moves[0].asset_id, a);
    assert_eq!(plan.new_files.len(), 1);
    assert_eq!(plan.new_files[0].path, inside);
    assert!(plan.removed.is_empty() && plan.unavailable.is_empty() && plan.folders.is_empty());
}

/// When adding the new files fails, the moves and removals applied before
/// stay applied and are reported, with the files left out.
#[test]
fn a_failed_addition_still_reports_the_moves_and_removals_applied() {
    let d = tempfile::tempdir().unwrap();
    let lib = session(d.path());
    let capture = lib.directory.join("Capture");
    photo(&capture.join("A.ARW"), 1);
    photo(&capture.join("B.ARW"), 2);
    let a = lib.add_asset(&capture.join("A.ARW")).unwrap().id;
    let b = lib.add_asset(&capture.join("B.ARW")).unwrap().id;
    fill(&lib);
    fs::rename(capture.join("A.ARW"), capture.join("A2.ARW")).unwrap();
    fs::remove_file(capture.join("B.ARW")).unwrap();
    photo(&capture.join("N.ARW"), 9);
    let plan = scan(&lib, None);
    let mut actions = defaults(&plan);
    actions.push(SyncAction::Remove {
        asset_id: b.clone(),
        path: capture.join("B.ARW"),
    });
    // Adding fails as a whole (as a full disk would), after the moves and
    // removals committed.
    rusqlite::Connection::open(lib.directory.join("session.sqlite"))
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER no_room BEFORE INSERT ON assets BEGIN SELECT RAISE(ABORT, 'disk full'); END;",
        )
        .unwrap();
    let applied = lib.apply_folder_sync(&actions).unwrap();
    assert_eq!(
        applied.moved,
        [(capture.join("A.ARW"), capture.join("A2.ARW"))]
    );
    assert_eq!(applied.removed, [capture.join("B.ARW")]);
    assert!(applied.added.is_empty());
    assert!(applied.add_error.is_some(), "{applied:#?}");
    assert_eq!(applied.skipped.len(), 1);
    assert_eq!(applied.skipped[0].path, capture.join("N.ARW"));
    assert_eq!(lib.asset(&a).unwrap().path, capture.join("A2.ARW"));
    assert!(lib.asset(&b).is_err());
    assert!(lib.asset_at(&capture.join("N.ARW")).unwrap().is_none());
}

/// A scan of the whole library, its accepted defaults applied, then a copy
/// and a removal accepted (no hash on record, so the renamed file is only a
/// probable move): the library follows it all and the next scan is empty.
/// Run for both library modes, whose paths are stored and compared
/// differently (relative inside the session, an external root for a
/// catalog).
#[test]
fn everything_a_scan_finds_is_applied_and_reported_once() {
    for mode in [Mode::Session, Mode::Catalog] {
        for hash in [false, true] {
            let d = tempfile::tempdir().unwrap();
            let lib = match mode {
                Mode::Session => session(d.path()),
                Mode::Catalog => {
                    Library::create(&d.path().join("Main.catalog"), "Main", mode).unwrap()
                }
            };
            let outer = match mode {
                Mode::Session => lib.directory.clone(),
                Mode::Catalog => d.path().to_path_buf(),
            };
            let capture = outer.join("Capture");
            let day = capture.join("Day 1");
            photo(&capture.join("A.ARW"), 1);
            photo(&day.join("B.ARW"), 2);
            photo(&day.join("Gone.ARW"), 3);
            let report = lib
                .add_files(&[
                    capture.join("A.ARW"),
                    day.join("B.ARW"),
                    day.join("Gone.ARW"),
                ])
                .unwrap();
            let id = |name: &str| {
                report
                    .added
                    .iter()
                    .find(|a| a.path.ends_with(name))
                    .unwrap()
                    .id
                    .clone()
            };
            let (a, b, gone) = (id("A.ARW"), id("B.ARW"), id("Gone.ARW"));
            if hash {
                fill(&lib);
            }
            // On disk: A renamed, B moved with its folder, Gone deleted, a
            // copy of A beside it.
            fs::rename(capture.join("A.ARW"), capture.join("A final.ARW")).unwrap();
            fs::rename(capture.join("Day 1"), capture.join("Day one")).unwrap();
            fs::remove_file(capture.join("Day one/Gone.ARW")).unwrap();
            fs::copy(capture.join("A final.ARW"), capture.join("A copy.ARW")).unwrap();

            let plan = scan(&lib, None);
            let folders = match mode {
                Mode::Session => vec![lib.directory.clone()],
                // The library's own folder and the catalog folder.
                Mode::Catalog => vec![d.path().canonicalize().unwrap(), lib.directory.clone()],
            };
            assert_eq!(plan.folders, folders, "{plan:#?}");
            let moves = hash
                .then_some(&plan.verified_moves)
                .unwrap_or(&plan.probable_moves);
            assert_eq!(moves.len(), 2, "{mode:?} hashed {hash}: {plan:#?}");
            assert!(
                moves
                    .iter()
                    .any(|m| m.asset_id == a && m.to == capture.join("A final.ARW"))
            );
            assert!(
                moves
                    .iter()
                    .any(|m| m.asset_id == b && m.to == capture.join("Day one/B.ARW"))
            );
            assert_eq!(plan.copies.len(), 1, "{plan:#?}");
            assert_eq!(plan.copies[0].copy_of, [capture.join("A final.ARW")]);
            assert_eq!(plan.removed.len(), 1, "{plan:#?}");
            assert_eq!(plan.removed[0].asset_id, gone);
            assert!(
                plan.new_files.is_empty() && plan.contested.is_empty() && plan.errors.is_empty()
            );

            let mut actions: Vec<SyncAction> = plan
                .verified_moves
                .iter()
                .chain(&plan.probable_moves)
                .map(SyncAction::from)
                .collect();
            actions.push(SyncAction::Add {
                path: plan.copies[0].path.clone(),
                identity: plan.copies[0].identity.clone(),
            });
            actions.push(SyncAction::Remove {
                asset_id: gone.clone(),
                path: plan.removed[0].path.clone(),
            });
            let applied = lib.apply_folder_sync(&actions).unwrap();
            assert!(applied.skipped.is_empty(), "{applied:#?}");
            assert_eq!(applied.moved.len(), 2);
            assert_eq!(applied.added, [capture.join("A copy.ARW")]);
            assert_eq!(applied.removed, [capture.join("Day one/Gone.ARW")]);
            assert_eq!(lib.asset(&a).unwrap().path, capture.join("A final.ARW"));
            assert_eq!(lib.asset(&b).unwrap().path, capture.join("Day one/B.ARW"));
            assert!(lib.asset(&gone).is_err());
            assert_eq!(lib.assets(None).unwrap().len(), 3);
            assert!(scan(&lib, None).is_empty(), "{mode:?} hashed {hash}");
            // Stored relative to the session, `/` between folders: the
            // follow registered it under the folder's new name on disk.
            if let Ok(relative) = capture.strip_prefix(&lib.directory) {
                let day = relative.with_file_name("Day one");
                assert_eq!(
                    lib.asset(&b).unwrap().rel_path,
                    format!("{}/B.ARW", day.display()).replace('\\', "/")
                );
            }
        }
    }
}
