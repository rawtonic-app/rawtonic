//! Import from Catalog (G39): the work done in an exported catalog comes
//! back; photographs are matched by asset id, else by SHA-256 (never by
//! name); one transaction, nothing applied on failure; the other catalog
//! and every original are never changed.
use rawtonic_catalog::{
    Library, Mode,
    catalog_import::{Choice, Side, Status},
};
use rawtonic_ingest::{
    catalog_export::{self, export_catalog},
    catalog_import::{NewPhotos, Options, import, open_source, review},
};
use rawtonic_metadata::Metadata;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

fn go() -> AtomicBool {
    AtomicBool::new(false)
}

fn sha(path: &Path) -> String {
    Sha256::digest(fs::read(path).unwrap())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn edit(exposure: f64) -> Value {
    json!({"version": 1, "adjustments": {"exposure": exposure}})
}

fn exposure(lib: &Library, id: &str) -> Value {
    lib.editor_state(id).unwrap().unwrap()["adjustments"]["exposure"].clone()
}

fn options(new_photos: NewPhotos) -> Options {
    Options {
        new_photos,
        changed: None,
        overrides: HashMap::new(),
        include_new: true,
        before_snapshot: "Before import".into(),
    }
}

/// A main catalog with three edited photographs and the catalog exported
/// from it (by reference) with the first two.
struct World {
    root: PathBuf,
    main: Library,
    ids: Vec<String>,
    files: Vec<PathBuf>,
    laptop: Library,
}

fn world() -> (tempfile::TempDir, World) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let main = Library::create(&root.join("Main"), "Main", Mode::Catalog).unwrap();
    let shoot = root.join("Shoot");
    fs::create_dir_all(&shoot).unwrap();
    let mut ids = Vec::new();
    let mut files = Vec::new();
    for (n, name) in ["a.arw", "b.arw", "c.arw"].iter().enumerate() {
        let file = shoot.join(name);
        fs::write(&file, format!("original {n}")).unwrap();
        let id = main.add_asset(&file).unwrap().id;
        main.save_editor_state(&id, &edit(0.1 * n as f64)).unwrap();
        ids.push(id);
        files.push(file);
    }
    let album = main.create_collection("Wedding", None).unwrap();
    main.add_to_collection(&album.id, &ids).unwrap();
    export_catalog(
        &main.directory,
        &ids[..2],
        &root.join("Laptop"),
        &catalog_export::Options {
            name: "Laptop".into(),
            include_originals: false,
            leave_out_owned_elsewhere: false,
        },
        &go(),
        |_| {},
        |_, _, _| Ok(0),
    )
    .unwrap();
    let laptop = Library::open(&root.join("Laptop")).unwrap();
    (
        tmp,
        World {
            root,
            main,
            ids,
            files,
            laptop,
        },
    )
}

#[test]
fn work_done_in_the_exported_catalog_comes_back() {
    let (_tmp, w) = world();
    // On the laptop: a new edit, rating and keyword on a; b untouched; a
    // new photograph from the laptop's card with an edit, in a new album.
    w.laptop.save_editor_state(&w.ids[0], &edit(1.5)).unwrap();
    let mut meta = w.laptop.metadata(&w.ids[0]).unwrap();
    meta.rating = 5;
    meta.keywords.push("Bride".into());
    w.laptop.set_metadata(&w.ids[0], &meta).unwrap();
    let card = w.root.join("Card");
    fs::create_dir_all(&card).unwrap();
    let extra = card.join("d.arw");
    fs::write(&extra, b"shot on the laptop trip").unwrap();
    let d = w.laptop.add_asset(&extra).unwrap().id;
    w.laptop.save_editor_state(&d, &edit(0.7)).unwrap();
    w.laptop.save_editor_state(&d, &edit(0.8)).unwrap();
    let picks = w.laptop.create_collection("Laptop picks", None).unwrap();
    w.laptop
        .add_to_collection(&picks.id, &[w.ids[0].clone(), d.clone()])
        .unwrap();
    let laptop_db = sha(&w.laptop.directory.join("catalog.sqlite"));
    let originals: Vec<String> = w.files.iter().map(|f| sha(f)).collect();

    let source = open_source(&w.main, &w.laptop.directory).unwrap();
    let seen = review(&w.main, &source).unwrap();
    drop(source);
    assert_eq!((seen.new, seen.changed, seen.unchanged), (1, 1, 1));
    let a = seen.photos.iter().find(|p| p.id == w.ids[0]).unwrap();
    assert_eq!(a.status, Status::Changed);
    assert_eq!(a.matched_by, Some("id"));
    assert_eq!(a.changes, ["edit", "metadata", "albums"]);
    assert_eq!(a.side, Some(Side::There));

    let receipt = import(
        &w.main,
        &w.laptop.directory,
        &options(NewPhotos::Reference),
        &go(),
        |_| {},
    )
    .unwrap();
    assert_eq!(receipt.report.added, 1);
    assert_eq!(receipt.report.replaced, 1);
    assert_eq!(receipt.unchanged, 1);
    assert_eq!(receipt.report.albums_created, 1);
    // Merged state = the laptop's.
    assert_eq!(exposure(&w.main, &w.ids[0]), json!(1.5));
    assert_eq!(
        w.main.metadata(&w.ids[0]).unwrap(),
        w.laptop.metadata(&w.ids[0]).unwrap()
    );
    assert_eq!(exposure(&w.main, &w.ids[1]), json!(0.1));
    assert_eq!(exposure(&w.main, &d), json!(0.8));
    assert_eq!(
        w.main.asset(&d).unwrap().path,
        extra,
        "added where it is, same id"
    );
    // Right after, importing again finds nothing to do.
    let same = import(
        &w.main,
        &w.laptop.directory,
        &options(NewPhotos::Reference),
        &go(),
        |_| {},
    )
    .unwrap();
    assert_eq!(
        (same.report.added, same.report.replaced, same.unchanged),
        (0, 0, 3)
    );
    // The replaced edit stays in history; the new photo brought its own.
    assert!(
        w.main
            .undo(&w.main.primary_version(&w.ids[0]).unwrap())
            .unwrap()
    );
    assert_eq!(exposure(&w.main, &w.ids[0]), json!(0.0));
    assert!(w.main.undo(&w.main.primary_version(&d).unwrap()).unwrap());
    assert_eq!(exposure(&w.main, &d), json!(0.7));
    let albums = w.main.collections().unwrap();
    let picks_here = albums.iter().find(|c| c.name == "Laptop picks").unwrap();
    assert_eq!(picks_here.count, 2);
    assert_eq!(
        albums.iter().find(|c| c.name == "Wedding").unwrap().count,
        3
    );
    // Nothing else changed: the other catalog and every original.
    assert_eq!(sha(&w.laptop.directory.join("catalog.sqlite")), laptop_db);
    for (file, digest) in w.files.iter().zip(&originals) {
        assert_eq!(&sha(file), digest);
    }
    // Importing again: the two undone here count as changed only here
    // (their incoming edit is a later step of their history) and are kept.
    let again = import(
        &w.main,
        &w.laptop.directory,
        &options(NewPhotos::Reference),
        &go(),
        |_| {},
    )
    .unwrap();
    assert_eq!(
        (
            again.report.added,
            again.report.replaced,
            again.report.kept,
            again.unchanged
        ),
        (0, 0, 2, 1)
    );
    // No snapshot is left in the catalog's cache.
    assert_eq!(
        fs::read_dir(w.main.directory.join(".cache"))
            .unwrap()
            .count(),
        0
    );
}

#[test]
fn conflict_choices_keep_virtual_copy_and_changed_here_only() {
    let (_tmp, w) = world();
    // Both sides edited a; only here b.
    w.laptop.save_editor_state(&w.ids[0], &edit(2.0)).unwrap();
    w.main.save_editor_state(&w.ids[0], &edit(-1.0)).unwrap();
    w.main.save_editor_state(&w.ids[1], &edit(3.0)).unwrap();
    let source = open_source(&w.main, &w.laptop.directory).unwrap();
    let seen = review(&w.main, &source).unwrap();
    drop(source);
    let b = seen.photos.iter().find(|p| p.id == w.ids[1]).unwrap();
    assert_eq!(b.side, Some(Side::Here), "{b:?}");
    let a = seen.photos.iter().find(|p| p.id == w.ids[0]).unwrap();
    assert_eq!(a.side, Some(Side::Both));

    // One choice for the run: Keep current.
    let mut keep = options(NewPhotos::Reference);
    keep.changed = Some(Choice::Keep);
    let receipt = import(&w.main, &w.laptop.directory, &keep, &go(), |_| {}).unwrap();
    assert_eq!(receipt.report.kept, 2);
    assert_eq!(exposure(&w.main, &w.ids[0]), json!(-1.0));

    // Default: a (changed on both sides) and b (changed only here) are
    // kept; Replace is the photographer's choice for a.
    let receipt = import(
        &w.main,
        &w.laptop.directory,
        &options(NewPhotos::Reference),
        &go(),
        |_| {},
    )
    .unwrap();
    assert_eq!((receipt.report.replaced, receipt.report.kept), (0, 2));
    let mut replace_a = options(NewPhotos::Reference);
    replace_a
        .overrides
        .insert(w.ids[0].clone(), Choice::Replace);
    let receipt = import(&w.main, &w.laptop.directory, &replace_a, &go(), |_| {}).unwrap();
    assert_eq!((receipt.report.replaced, receipt.report.kept), (1, 1));
    assert_eq!(exposure(&w.main, &w.ids[0]), json!(2.0));
    assert_eq!(exposure(&w.main, &w.ids[1]), json!(3.0));
    assert!(
        w.main
            .undo(&w.main.primary_version(&w.ids[0]).unwrap())
            .unwrap()
    );
    assert_eq!(exposure(&w.main, &w.ids[0]), json!(-1.0));

    // Per photograph: a as a virtual copy, the edit here stays.
    let mut copy = options(NewPhotos::Reference);
    copy.overrides.insert(w.ids[0].clone(), Choice::VirtualCopy);
    copy.changed = Some(Choice::Keep);
    let receipt = import(&w.main, &w.laptop.directory, &copy, &go(), |_| {}).unwrap();
    assert_eq!(receipt.report.virtual_copies, 1);
    assert_eq!(exposure(&w.main, &w.ids[0]), json!(-1.0));
    let copies = w.main.copies(&w.ids[0]).unwrap();
    assert_eq!(copies.len(), 1);
    let state = w.main.copy_state(&w.ids[0], &copies[0]).unwrap().unwrap();
    assert_eq!(state["adjustments"]["exposure"], json!(2.0));
}

#[test]
fn a_failure_or_cancel_applies_nothing_and_takes_copies_back() {
    let (_tmp, w) = world();
    w.laptop.save_editor_state(&w.ids[0], &edit(9.0)).unwrap();
    let card = w.root.join("Card");
    fs::create_dir_all(&card).unwrap();
    fs::write(card.join("d.arw"), b"new").unwrap();
    let d = w.laptop.add_asset(&card.join("d.arw")).unwrap().id;
    let picks = w.laptop.create_collection("Picks", None).unwrap();
    w.laptop
        .add_to_collection(&picks.id, std::slice::from_ref(&d))
        .unwrap();
    let into = w.root.join("Imported");
    fs::create_dir_all(&into).unwrap();
    // The last step of the transaction fails (an album membership).
    let db = rusqlite::Connection::open(w.main.directory.join("catalog.sqlite")).unwrap();
    db.execute_batch(
        "CREATE TRIGGER fail_import BEFORE INSERT ON collection_assets BEGIN SELECT RAISE(FAIL,'disk full'); END;",
    )
    .unwrap();
    let error = import(
        &w.main,
        &w.laptop.directory,
        &options(NewPhotos::Copy(into.clone())),
        &go(),
        |_| {},
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("disk full"), "{error:#}");
    assert_eq!(exposure(&w.main, &w.ids[0]), json!(0.0));
    assert!(w.main.asset(&d).is_err());
    assert!(
        w.main
            .collections()
            .unwrap()
            .iter()
            .all(|c| c.name != "Picks")
    );
    assert_eq!(fs::read_dir(&into).unwrap().count(), 0, "copies taken back");
    db.execute_batch("DROP TRIGGER fail_import").unwrap();
    drop(db);
    // Cancelled while copying originals.
    let cancel = go();
    assert!(
        import(
            &w.main,
            &w.laptop.directory,
            &options(NewPhotos::Copy(into.clone())),
            &cancel,
            |_| { cancel.store(true, Ordering::Release) }
        )
        .is_err()
    );
    assert!(w.main.asset(&d).is_err());
    assert_eq!(fs::read_dir(&into).unwrap().count(), 0);
    // Then it goes through: the copy is verified and registered there.
    let receipt = import(
        &w.main,
        &w.laptop.directory,
        &options(NewPhotos::Copy(into.clone())),
        &go(),
        |_| {},
    )
    .unwrap();
    assert_eq!(receipt.originals_copied, 1);
    let path = w.main.asset(&d).unwrap().path;
    assert_eq!(path, into.join("Card/d.arw"));
    assert_eq!(fs::read(&path).unwrap(), b"new");
    assert_eq!(fs::read(card.join("d.arw")).unwrap(), b"new");
    assert_eq!(exposure(&w.main, &w.ids[0]), json!(9.0));
}

#[test]
fn photographs_are_matched_by_sha256_never_by_name() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let main = Library::create(&root.join("Main"), "Main", Mode::Catalog).unwrap();
    let here = root.join("Here");
    fs::create_dir_all(&here).unwrap();
    fs::write(here.join("x.arw"), b"same photograph").unwrap();
    fs::write(here.join("y.arw"), b"one photograph").unwrap();
    let x = main.add_asset(&here.join("x.arw")).unwrap().id;
    main.add_asset(&here.join("y.arw")).unwrap();
    // Another catalog made independently: the same file under another
    // name, and another file under the name y.arw.
    let other = Library::create(&root.join("Other"), "Other", Mode::Catalog).unwrap();
    let there = root.join("There");
    fs::create_dir_all(&there).unwrap();
    fs::write(there.join("renamed.arw"), b"same photograph").unwrap();
    fs::write(there.join("y.arw"), b"another photograph").unwrap();
    let renamed = other.add_asset(&there.join("renamed.arw")).unwrap().id;
    other.save_editor_state(&renamed, &edit(0.4)).unwrap();
    let y_there = other.add_asset(&there.join("y.arw")).unwrap().id;
    let source = open_source(&main, &other.directory).unwrap();
    let seen = review(&main, &source).unwrap();
    drop(source);
    let same = seen.photos.iter().find(|p| p.id == renamed).unwrap();
    assert_eq!(same.target.as_deref(), Some(x.as_str()));
    assert_eq!(same.matched_by, Some("sha256"));
    assert_eq!(same.status, Status::Changed);
    let y = seen.photos.iter().find(|p| p.id == y_there).unwrap();
    assert_eq!(y.status, Status::New, "same name, other content");
    // Made independently: nothing records a common state, so the change
    // counts as on both sides and Keep current is the default.
    assert_eq!(same.side, Some(Side::Both));
    let mut replace = options(NewPhotos::Reference);
    replace.changed = Some(Choice::Replace);
    import(&main, &other.directory, &replace, &go(), |_| {}).unwrap();
    assert_eq!(exposure(&main, &x), json!(0.4));
    assert_eq!(main.assets(None).unwrap().len(), 3);

    // A second copy of the same file there is a photograph of its own: x is
    // matched once, the other copy comes in as new, and importing again
    // finds nothing to do (no flipping between the two copies' states).
    fs::write(there.join("again.arw"), b"same photograph").unwrap();
    let again = other.add_asset(&there.join("again.arw")).unwrap().id;
    other.save_editor_state(&again, &edit(-0.4)).unwrap();
    let receipt = import(
        &main,
        &other.directory,
        &options(NewPhotos::Reference),
        &go(),
        |_| {},
    )
    .unwrap();
    assert_eq!((receipt.report.added, receipt.report.replaced), (1, 0));
    assert_eq!(exposure(&main, &x), json!(0.4));
    assert_eq!(exposure(&main, &again), json!(-0.4));
    let receipt = import(
        &main,
        &other.directory,
        &options(NewPhotos::Reference),
        &go(),
        |_| {},
    )
    .unwrap();
    assert_eq!(
        (
            receipt.report.added,
            receipt.report.replaced,
            receipt.unchanged
        ),
        (0, 0, 3)
    );
}

#[test]
fn an_older_catalog_is_read_through_a_migrated_snapshot_and_left_as_it_is() {
    let (_tmp, w) = world();
    w.laptop.save_editor_state(&w.ids[0], &edit(4.0)).unwrap();
    let mut meta: Metadata = w.laptop.metadata(&w.ids[1]).unwrap();
    meta.title = "Older schema".into();
    w.laptop.set_metadata(&w.ids[1], &meta).unwrap();
    let dir = w.laptop.directory.clone();
    w.laptop.checkpoint().unwrap();
    drop(w.laptop);
    // Schema 20: before eye checks.
    let db = rusqlite::Connection::open(dir.join("catalog.sqlite")).unwrap();
    db.execute_batch("DROP TABLE face_eyes; DROP TABLE eye_reads; PRAGMA user_version=20; PRAGMA wal_checkpoint(TRUNCATE);")
        .unwrap();
    drop(db);
    let before = sha(&dir.join("catalog.sqlite"));
    let receipt = import(&w.main, &dir, &options(NewPhotos::Reference), &go(), |_| {}).unwrap();
    assert_eq!(receipt.report.replaced, 2);
    assert_eq!(exposure(&w.main, &w.ids[0]), json!(4.0));
    assert_eq!(w.main.metadata(&w.ids[1]).unwrap().title, "Older schema");
    assert_eq!(
        sha(&dir.join("catalog.sqlite")),
        before,
        "the other catalog is not migrated"
    );
    let version: u32 = rusqlite::Connection::open(dir.join("catalog.sqlite"))
        .unwrap()
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, 20);
}

#[test]
fn sessions_are_not_import_targets_and_a_catalog_not_its_own_source() {
    let (_tmp, w) = world();
    let session = Library::create(&w.root.join("Session"), "S", Mode::Session).unwrap();
    assert!(open_source(&session, &w.laptop.directory).is_err());
    assert!(open_source(&w.main, &w.main.directory).is_err());
}

#[test]
fn new_photographs_inside_the_other_catalog_are_copied_not_referenced() {
    let (_tmp, w) = world();
    // Taken on the trip into the laptop catalog's own folder.
    let own = w.laptop.directory.join("Photos/Trip/e.arw");
    fs::create_dir_all(own.parent().unwrap()).unwrap();
    fs::write(&own, b"taken on the trip").unwrap();
    let e = w.laptop.add_asset(&own).unwrap().id;
    w.laptop.save_editor_state(&e, &edit(0.3)).unwrap();
    // And one a session owns, added to the laptop catalog where it is: its
    // latest edit is in that session, so it is left out.
    let session = Library::create(&w.root.join("Session"), "S", Mode::Session).unwrap();
    let theirs = session.directory.join("Capture/f.arw");
    fs::write(&theirs, b"a session's photo").unwrap();
    w.laptop.add_asset(&theirs).unwrap();
    let source = open_source(&w.main, &w.laptop.directory).unwrap();
    let seen = review(&w.main, &source).unwrap();
    drop(source);
    assert_eq!((seen.new, seen.left_out), (1, 1));
    let receipt = import(
        &w.main,
        &w.laptop.directory,
        &options(NewPhotos::Reference),
        &go(),
        |_| {},
    )
    .unwrap();
    assert_eq!(
        (
            receipt.report.added,
            receipt.originals_copied,
            receipt.left_out
        ),
        (1, 1, 1)
    );
    assert!(w.main.asset_at(&theirs).unwrap().is_none());
    let path = w.main.asset(&e).unwrap().path;
    assert_eq!(path, w.main.directory.join("Photos/Trip/e.arw"));
    assert_eq!(fs::read(&path).unwrap(), b"taken on the trip");
    assert_eq!(fs::read(&own).unwrap(), b"taken on the trip");
    assert_eq!(exposure(&w.main, &e), json!(0.3));
}

#[test]
fn replace_keeps_the_replaced_edit_as_a_snapshot_and_brings_snapshots_and_copies() {
    let (_tmp, w) = world();
    let a = &w.ids[0];
    // Here: a snapshot of the exported edit; there: it is renamed and a new
    // edit made; a virtual copy with a snapshot of its own is added.
    let version = w.main.primary_version(a).unwrap();
    let taken = w
        .main
        .add_snapshot(
            &version,
            "Warm",
            json!({"exposure": 0.0, "temperature": 10}),
        )
        .unwrap();
    let laptop_version = w.laptop.primary_version(a).unwrap();
    // The laptop catalog was exported before the snapshot: give it the same.
    let db = rusqlite::Connection::open(w.laptop.directory.join("catalog.sqlite")).unwrap();
    db.execute(
        "INSERT INTO snapshots(id,version_id,name,created_at,adjustments) VALUES(?,?,?,?,?)",
        rusqlite::params![
            taken.id,
            laptop_version,
            "Warm",
            taken.created_at,
            r#"{"exposure":0.0,"temperature":10}"#
        ],
    )
    .unwrap();
    drop(db);
    w.laptop
        .rename_snapshot(&laptop_version, &taken.id, "Warm (client)")
        .unwrap();
    w.laptop.save_editor_state(a, &edit(1.0)).unwrap();
    // A snapshot using a LUT outside the catalog: copied into this
    // catalog's resources, and the same snapshot after (content compared).
    let lut = w.root.join("look.cube");
    fs::write(&lut, b"LUT").unwrap();
    w.laptop
        .add_snapshot(
            &laptop_version,
            "With LUT",
            json!({"exposure": 0.2, "lutPath": lut}),
        )
        .unwrap();
    let mut copy = json!({"version": 1, "adjustments": {"exposure": 0.6}});
    rawtonic_catalog::snapshots::set_in_state(
        &mut copy,
        &[rawtonic_catalog::Snapshot::new("Copy look", json!({"exposure": 0.6})).unwrap()],
    )
    .unwrap();
    w.laptop.save_copy_state(a, "c0ffee", &copy).unwrap();

    let mut opts = options(NewPhotos::Reference);
    opts.before_snapshot = "Before import from Laptop".into();
    let receipt = import(&w.main, &w.laptop.directory, &opts, &go(), |_| {}).unwrap();
    assert_eq!(receipt.report.replaced, 1);
    assert!(receipt.report.touched.contains(a));
    assert_eq!(exposure(&w.main, a), json!(1.0));
    let names: Vec<String> = w
        .main
        .snapshots(&version)
        .unwrap()
        .into_iter()
        .map(|s| s.name)
        .collect();
    assert_eq!(
        names,
        ["Warm (client)", "Before import from Laptop", "With LUT"]
    );
    let before = w.main.snapshots(&version).unwrap().remove(1);
    assert_eq!(before.adjustments["exposure"], json!(0.0));
    assert_eq!(
        w.main.copy_state(a, "c0ffee").unwrap(),
        w.laptop.copy_state(a, "c0ffee").unwrap(),
        "the copy with its snapshot"
    );
    // Nothing left to do.
    let again = import(&w.main, &w.laptop.directory, &opts, &go(), |_| {}).unwrap();
    assert_eq!((again.report.replaced, again.unchanged), (0, 2));
}

#[test]
fn replacing_with_an_unedited_photo_applies_it_and_keeps_the_edit() {
    let (_tmp, w) = world();
    // Exported unedited, edited here afterwards; the untouched exported
    // catalog imported with Replace.
    let file = w.root.join("Shoot/e.arw");
    fs::write(&file, b"never edited").unwrap();
    let e = w.main.add_asset(&file).unwrap().id;
    let plain = w.root.join("Plain");
    export_catalog(
        &w.main.directory,
        std::slice::from_ref(&e),
        &plain,
        &catalog_export::Options {
            name: "Plain".into(),
            include_originals: false,
            leave_out_owned_elsewhere: false,
        },
        &go(),
        |_| {},
        |_, _, _| Ok(0),
    )
    .unwrap();
    w.main.save_editor_state(&e, &edit(2.5)).unwrap();
    let mut replace = options(NewPhotos::Reference);
    replace.changed = Some(Choice::Replace);
    let receipt = import(&w.main, &plain, &replace, &go(), |_| {}).unwrap();
    assert_eq!(receipt.report.replaced, 1);
    let state = w.main.editor_state(&e).unwrap().unwrap();
    assert!(state["adjustments"].is_null(), "{state}");
    let version = w.main.primary_version(&e).unwrap();
    let kept = w.main.snapshots(&version).unwrap();
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].adjustments["exposure"], json!(2.5));
    let again = import(&w.main, &plain, &replace, &go(), |_| {}).unwrap();
    assert_eq!((again.report.replaced, again.unchanged), (0, 1));
}

#[test]
fn copies_never_go_into_another_library() {
    let (_tmp, w) = world();
    let card = w.root.join("Card");
    fs::create_dir_all(&card).unwrap();
    fs::write(card.join("d.arw"), b"new").unwrap();
    w.laptop.add_asset(&card.join("d.arw")).unwrap();
    let session = Library::create(&w.root.join("Session"), "S", Mode::Session).unwrap();
    let error = import(
        &w.main,
        &w.laptop.directory,
        &options(NewPhotos::Copy(session.directory.join("Capture"))),
        &go(),
        |_| {},
    )
    .unwrap_err()
    .to_string();
    assert!(
        error.contains("choose a folder outside other projects"),
        "{error}"
    );
    assert_eq!(
        fs::read_dir(session.directory.join("Capture"))
            .unwrap()
            .count(),
        0
    );
}

/// UXR3 (notes reference/ux-local-tools-2026-10-31, D2): a rating changed
/// here after Export as Catalog was offered as "changed", Replace by
/// default, which would have put the older rating back. Each photograph is
/// compared with its state at the export: changed there → Replace, changed
/// here → Keep current, changed on both sides → Keep current (the
/// photographer chooses); the replaced metadata can be put back.
#[test]
fn the_side_that_changed_since_the_export_decides_the_default() {
    let (_tmp, w) = world();
    // Here: a's rating. There: b's rating. Both: c is not exported; a's
    // keyword there as well as a's rating here make a "both" for photo 0
    // in the second round below.
    let mut here = w.main.metadata(&w.ids[0]).unwrap();
    here.rating = 4;
    w.main.set_metadata(&w.ids[0], &here).unwrap();
    let mut there = w.laptop.metadata(&w.ids[1]).unwrap();
    there.rating = 2;
    w.laptop.set_metadata(&w.ids[1], &there).unwrap();

    let source = open_source(&w.main, &w.laptop.directory).unwrap();
    let seen = review(&w.main, &source).unwrap();
    drop(source);
    let a = seen.photos.iter().find(|p| p.id == w.ids[0]).unwrap();
    assert_eq!(
        (a.changes.clone(), a.side),
        (vec!["metadata"], Some(Side::Here))
    );
    let b = seen.photos.iter().find(|p| p.id == w.ids[1]).unwrap();
    assert_eq!(
        (b.changes.clone(), b.side),
        (vec!["metadata"], Some(Side::There))
    );

    let receipt = import(
        &w.main,
        &w.laptop.directory,
        &options(NewPhotos::Reference),
        &go(),
        |_| {},
    )
    .unwrap();
    assert_eq!((receipt.report.replaced, receipt.report.kept), (1, 1));
    assert_eq!(
        w.main.metadata(&w.ids[0]).unwrap().rating,
        4,
        "the newer rating here stays"
    );
    assert_eq!(
        w.main.metadata(&w.ids[1]).unwrap().rating,
        2,
        "the newer rating there comes"
    );
    // What Replace replaced can be put back (Undo), and done again (Redo).
    assert_eq!(receipt.report.undo.len(), 1);
    w.main.travel_import(&receipt.report.undo, true).unwrap();
    assert_eq!(w.main.metadata(&w.ids[1]).unwrap().rating, 0);
    w.main.travel_import(&receipt.report.undo, false).unwrap();
    assert_eq!(w.main.metadata(&w.ids[1]).unwrap().rating, 2);

    // After the import, its state is the new common ground: a later change
    // there alone is "there" again, not "both".
    let mut there = w.laptop.metadata(&w.ids[1]).unwrap();
    there.rating = 3;
    w.laptop.set_metadata(&w.ids[1], &there).unwrap();
    // And a changed on both sides now.
    let mut there_a = w.laptop.metadata(&w.ids[0]).unwrap();
    there_a.keywords.push("Bride".into());
    w.laptop.set_metadata(&w.ids[0], &there_a).unwrap();
    let source = open_source(&w.main, &w.laptop.directory).unwrap();
    let seen = review(&w.main, &source).unwrap();
    drop(source);
    let b = seen.photos.iter().find(|p| p.id == w.ids[1]).unwrap();
    assert_eq!(b.side, Some(Side::There));
    let a = seen.photos.iter().find(|p| p.id == w.ids[0]).unwrap();
    assert_eq!(a.side, Some(Side::Both));
    let receipt = import(
        &w.main,
        &w.laptop.directory,
        &options(NewPhotos::Reference),
        &go(),
        |_| {},
    )
    .unwrap();
    assert_eq!((receipt.report.replaced, receipt.report.kept), (1, 1));
    assert_eq!(w.main.metadata(&w.ids[0]).unwrap().rating, 4);
}

/// A catalog exported before the export recorded its state (or from
/// another catalog) has nothing to compare with: every changed photograph
/// counts as changed on both sides, and Keep current is the default.
#[test]
fn without_a_record_of_the_export_every_change_is_on_both_sides() {
    let (_tmp, w) = world();
    fs::remove_file(
        w.laptop
            .directory
            .join(rawtonic_catalog::catalog_import::EXPORT_BASELINE),
    )
    .unwrap();
    let mut there = w.laptop.metadata(&w.ids[1]).unwrap();
    there.rating = 2;
    w.laptop.set_metadata(&w.ids[1], &there).unwrap();
    let source = open_source(&w.main, &w.laptop.directory).unwrap();
    let seen = review(&w.main, &source).unwrap();
    drop(source);
    let b = seen.photos.iter().find(|p| p.id == w.ids[1]).unwrap();
    assert_eq!(b.side, Some(Side::Both));
    let receipt = import(
        &w.main,
        &w.laptop.directory,
        &options(NewPhotos::Reference),
        &go(),
        |_| {},
    )
    .unwrap();
    assert_eq!((receipt.report.replaced, receipt.report.kept), (0, 1));
}

/// Review round 1: a snapshot the two catalogs had in common, renamed
/// differently on each side, is a change on both sides (Replace would
/// overwrite the name here).
#[test]
fn a_common_snapshot_changed_on_both_sides_is_a_conflict() {
    let (_tmp, w) = world();
    let a = &w.ids[0];
    let there = w.laptop.primary_version(a).unwrap();
    let snap = w
        .laptop
        .add_snapshot(&there, "Warm", json!({"exposure": 0.0}))
        .unwrap();
    import(
        &w.main,
        &w.laptop.directory,
        &options(NewPhotos::Reference),
        &go(),
        |_| {},
    )
    .unwrap();
    let here = w.main.primary_version(a).unwrap();
    assert!(
        w.main
            .snapshots(&here)
            .unwrap()
            .iter()
            .any(|s| s.name == "Warm")
    );
    w.main
        .rename_snapshot(&here, &snap.id, "Warm (mine)")
        .unwrap();
    w.laptop
        .rename_snapshot(&there, &snap.id, "Warm (client)")
        .unwrap();
    let source = open_source(&w.main, &w.laptop.directory).unwrap();
    let seen = review(&w.main, &source).unwrap();
    drop(source);
    let photo = seen.photos.iter().find(|p| p.id == *a).unwrap();
    assert_eq!(
        (photo.changes.clone(), photo.side),
        (vec!["snapshots"], Some(Side::Both))
    );
    import(
        &w.main,
        &w.laptop.directory,
        &options(NewPhotos::Reference),
        &go(),
        |_| {},
    )
    .unwrap();
    assert!(
        w.main
            .snapshots(&here)
            .unwrap()
            .iter()
            .any(|s| s.name == "Warm (mine)")
    );
}

/// Review round 1: another catalog's id is never used as a path.
#[test]
fn an_id_that_is_a_path_is_never_written_to() {
    let (_tmp, w) = world();
    let manifest = w.laptop.directory.join("catalog.json");
    let mut json: Value = serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
    json["id"] = json!("../catalog");
    fs::write(&manifest, serde_json::to_vec_pretty(&json).unwrap()).unwrap();
    let ours = fs::read(w.main.directory.join("catalog.json")).unwrap();
    import(
        &w.main,
        &w.laptop.directory,
        &options(NewPhotos::Reference),
        &go(),
        |_| {},
    )
    .unwrap();
    assert_eq!(
        fs::read(w.main.directory.join("catalog.json")).unwrap(),
        ours
    );
    Library::open(&w.main.directory).unwrap();
}

/// Review round 1: Undo and Redo write the XMP again (pending) and keep
/// the checksum of the sidecar as it is now, so no outside change is told.
#[test]
fn travel_marks_the_xmp_pending_and_keeps_its_checksum() {
    let (_tmp, w) = world();
    let mut there = w.laptop.metadata(&w.ids[1]).unwrap();
    there.rating = 2;
    w.laptop.set_metadata(&w.ids[1], &there).unwrap();
    let receipt = import(
        &w.main,
        &w.laptop.directory,
        &options(NewPhotos::Reference),
        &go(),
        |_| {},
    )
    .unwrap();
    let db = rusqlite::Connection::open(w.main.directory.join("catalog.sqlite")).unwrap();
    let row = || -> (Option<String>, String) {
        db.query_row(
            "SELECT xmp_hash,xmp_status FROM metadata WHERE asset_id=?",
            [&w.ids[1]],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap()
    };
    db.execute(
        "UPDATE metadata SET xmp_hash='now',xmp_status='synced' WHERE asset_id=?",
        [&w.ids[1]],
    )
    .unwrap();
    w.main.travel_import(&receipt.report.undo, true).unwrap();
    assert_eq!(row(), (Some("now".into()), "pending".into()));
    db.execute(
        "UPDATE metadata SET xmp_status='synced' WHERE asset_id=?",
        [&w.ids[1]],
    )
    .unwrap();
    w.main.travel_import(&receipt.report.undo, false).unwrap();
    assert_eq!(row(), (Some("now".into()), "pending".into()));
}

/// Review round 2: after an import that replaced an edit (keeping it as a
/// snapshot here), the common state still advances, so a snapshot that
/// came in and is then renamed on both sides is a conflict, not "there".
#[test]
fn the_common_state_advances_past_a_replaced_edit() {
    let (_tmp, w) = world();
    let a = &w.ids[0];
    let there = w.laptop.primary_version(a).unwrap();
    w.laptop.save_editor_state(a, &edit(1.2)).unwrap();
    let snap = w
        .laptop
        .add_snapshot(&there, "S", json!({"exposure": 0.3}))
        .unwrap();
    let receipt = import(
        &w.main,
        &w.laptop.directory,
        &options(NewPhotos::Reference),
        &go(),
        |_| {},
    )
    .unwrap();
    assert_eq!(receipt.report.replaced, 1);
    let here = w.main.primary_version(a).unwrap();
    assert!(
        w.main
            .snapshots(&here)
            .unwrap()
            .iter()
            .any(|s| s.name == "Before import")
    );
    w.main.rename_snapshot(&here, &snap.id, "S (mine)").unwrap();
    w.laptop
        .rename_snapshot(&there, &snap.id, "S (client)")
        .unwrap();
    let source = open_source(&w.main, &w.laptop.directory).unwrap();
    let seen = review(&w.main, &source).unwrap();
    drop(source);
    let photo = seen.photos.iter().find(|p| p.id == *a).unwrap();
    assert_eq!(photo.side, Some(Side::Both), "{photo:?}");
    import(
        &w.main,
        &w.laptop.directory,
        &options(NewPhotos::Reference),
        &go(),
        |_| {},
    )
    .unwrap();
    assert!(
        w.main
            .snapshots(&here)
            .unwrap()
            .iter()
            .any(|s| s.name == "S (mine)")
    );
}
