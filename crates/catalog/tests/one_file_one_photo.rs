//! One file is one photograph (G39, RATE). A folder registered under two
//! records — what the Windows spelling migration left when a photograph was
//! registered under both its verbatim (`\\?\C:\…`) and its plain spelling —
//! gave one file two rows: registering picked one, path lookups the other,
//! so a rating was saved to one row, read back from the other and written
//! over with the stale value by the next Develop save. Opening such a
//! catalog merges the rows (a database backup first); files are never
//! touched.
use rawtonic_catalog::{Library, Mode, layout::database_name, one_root::REPAIR_SNAPSHOT};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::{fs, path::Path, path::PathBuf};

struct World {
    _tmp: tempfile::TempDir,
    catalog: PathBuf,
    photo: PathBuf,
    /// The row under the older record (the one a path lookup found).
    old: String,
    /// The row under the record the folder's spelling names (the one
    /// registering found: ratings, labels and edits were saved there).
    new: String,
}

fn database(catalog: &Path) -> Connection {
    Connection::open(catalog.join(database_name(Mode::Catalog))).unwrap()
}

/// A catalog whose folder `Shoot` has two records with its path, each with
/// a row for `Shoot/a.jpg`, as the Windows migration left them: the older
/// record (an id from the old spelling) holds the first row. `old` and `new`
/// set up each row before the records are joined.
fn world(old: impl FnOnce(&Library, &str), new: impl FnOnce(&Library, &str)) -> World {
    let tmp = tempfile::tempdir().unwrap();
    let root = rawtonic_fs::canonicalize(tmp.path()).unwrap();
    let catalog = root.join("Main.catalog");
    let shoot = root.join("Shoot");
    let spelled = root.join("Spelled");
    fs::create_dir_all(&shoot).unwrap();
    fs::create_dir_all(&spelled).unwrap();
    let photo = shoot.join("a.jpg");
    fs::write(&photo, b"synthetic photograph").unwrap();
    // The older registration of the same file under another spelling.
    fs::write(spelled.join("a.jpg"), b"synthetic photograph").unwrap();
    let (old_id, new_id) = {
        let lib = Library::create(&catalog, "Main", Mode::Catalog).unwrap();
        let old_id = lib.add_asset(&spelled.join("a.jpg")).unwrap().id;
        old(&lib, &old_id);
        let new_id = lib.add_asset(&photo).unwrap().id;
        new(&lib, &new_id);
        (old_id, new_id)
    };
    fs::remove_file(spelled.join("a.jpg")).unwrap();
    // What `plain_stored_paths` did to the older record: the plain path,
    // its old id.
    database(&catalog)
        .execute(
            "UPDATE roots SET path=? WHERE id=(SELECT root_id FROM assets WHERE id=?)",
            rusqlite::params![shoot.to_string_lossy().as_ref(), old_id],
        )
        .unwrap();
    World {
        _tmp: tmp,
        catalog,
        photo,
        old: old_id,
        new: new_id,
    }
}

fn rows(catalog: &Path) -> i64 {
    database(catalog)
        .query_row("SELECT count(*) FROM assets", [], |r| r.get(0))
        .unwrap()
}

fn backups(catalog: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(catalog)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains("before-duplicate-repair"))
        .collect();
    names.sort();
    names
}

fn state(exposure: f64) -> Value {
    json!({"version": 1, "adjustments": {"exposure": exposure}})
}

/// What the app's commands do with a photograph's path: read its state
/// (`asset_at`, the Develop load and the culling read), change it and save
/// it to the photograph registering finds (`add_asset`, the sidecar write).
fn edit_at(lib: &Library, photo: &Path, change: impl FnOnce(&mut Value)) {
    let read = lib.asset_at(photo).unwrap().unwrap().id;
    let mut state = lib.editor_state(&read).unwrap().unwrap();
    change(&mut state);
    let write = lib.add_asset(photo).unwrap().id;
    lib.save_editor_state(&write, &state).unwrap();
}

#[test]
fn a_rating_survives_a_develop_edit_and_its_undo() {
    let w = world(
        |lib, id| {
            let mut meta = lib.metadata(id).unwrap();
            meta.rating = 2;
            lib.set_metadata(id, &meta).unwrap();
        },
        |lib, id| {
            let mut meta = lib.metadata(id).unwrap();
            meta.rating = 2;
            lib.set_metadata(id, &meta).unwrap();
        },
    );
    let lib = Library::open(&w.catalog).unwrap();
    // Rate 5, label and keyword in the Library, flag it, then an exposure
    // edit in Develop and its Undo.
    edit_at(&lib, &w.photo, |s| s["rating"] = json!(5));
    edit_at(&lib, &w.photo, |s| {
        s["tags"] = json!(["color:red", "Bride"]);
    });
    let flagged = lib.asset_at(&w.photo).unwrap().unwrap().id;
    lib.set_flags(&[flagged], 1).unwrap();
    edit_at(&lib, &w.photo, |s| {
        s["adjustments"]["exposure"] = json!(0.5)
    });
    edit_at(&lib, &w.photo, |s| s["adjustments"]["exposure"] = json!(0));
    drop(lib);
    let lib = Library::open(&w.catalog).unwrap();
    let photo = lib.asset_at(&w.photo).unwrap().unwrap();
    assert_eq!(photo.rating, 5, "the rating set last");
    let meta = lib.metadata(&photo.id).unwrap();
    assert_eq!(meta.label, "Red");
    assert_eq!(meta.keywords, ["Bride"]);
    assert_eq!(meta.flag, 1);
    assert_eq!(lib.add_asset(&w.photo).unwrap().id, photo.id);
    assert_eq!(rows(&w.catalog), 1, "one file, one photograph");
}

#[test]
fn two_records_of_one_folder_become_one_photograph_keeping_the_work_of_both() {
    let original = b"synthetic photograph".to_vec();
    let w = world(
        |lib, id| {
            // The row path lookups found: its flag and caption were set
            // there; its rating and keywords are old ones; an album, an
            // earlier edit and a virtual copy.
            lib.save_editor_state(id, &state(0.3)).unwrap();
            let mut meta = lib.metadata(id).unwrap();
            meta.rating = 2;
            meta.flag = 1;
            meta.keywords = vec!["Bride".into()];
            meta.title = "First dance".into();
            meta.iptc.insert("city".into(), "Lyon".into());
            lib.set_metadata(id, &meta).unwrap();
            let album = lib.create_collection("Wedding", None).unwrap();
            lib.add_to_collection(&album.id, &[id.to_string()]).unwrap();
            lib.save_copy_state(id, "c0ffee", &state(-1.0)).unwrap();
        },
        |lib, id| {
            // The row registering found: the latest rating, label, keywords
            // and edit were saved there; its caption is an old one.
            lib.save_editor_state(id, &state(0.7)).unwrap();
            let mut meta = lib.metadata(id).unwrap();
            meta.rating = 4;
            meta.label = "red".into();
            meta.keywords = vec!["Bride".into(), "Groom".into()];
            meta.title = "Our dance".into();
            lib.set_metadata(id, &meta).unwrap();
        },
    );
    assert_eq!(rows(&w.catalog), 2);
    assert!(backups(&w.catalog).is_empty());

    let lib = Library::open(&w.catalog).unwrap();
    assert_eq!(rows(&w.catalog), 1);
    let kept = lib.asset_at(&w.photo).unwrap().unwrap();
    assert_eq!(kept.id, w.new, "the row ratings and edits were saved to");
    assert_eq!(lib.add_asset(&w.photo).unwrap().id, w.new);
    let meta = lib.metadata(&w.new).unwrap();
    // Each field from the row its writes went to.
    assert_eq!(meta.rating, 4);
    assert_eq!(meta.label, "red");
    assert_eq!(meta.keywords, ["Bride", "Groom"]);
    assert_eq!(meta.flag, 1);
    assert_eq!(meta.title, "First dance");
    assert_eq!(meta.iptc.get("city").map(String::as_str), Some("Lyon"));
    // Its edit stays; the other's is a snapshot of it.
    let edit = lib.editor_state(&w.new).unwrap().unwrap();
    assert_eq!(edit["adjustments"]["exposure"], json!(0.7));
    let snapshots = lib
        .snapshots(&lib.primary_version(&w.new).unwrap())
        .unwrap();
    assert_eq!(snapshots.len(), 1);
    assert_eq!(snapshots[0].name, REPAIR_SNAPSHOT);
    assert_eq!(snapshots[0].adjustments["exposure"], json!(0.3));
    // Albums and virtual copies come along.
    let wedding = lib
        .collections()
        .unwrap()
        .into_iter()
        .find(|c| c.name == "Wedding")
        .unwrap();
    assert_eq!(wedding.count, 1);
    assert_eq!(lib.copies(&w.new).unwrap(), ["c0ffee"]);
    assert_eq!(
        lib.copy_state(&w.new, "c0ffee").unwrap().unwrap()["adjustments"]["exposure"],
        json!(-1.0)
    );
    assert!(lib.asset(&w.old).is_err(), "the merged row is gone");
    lib.check_integrity().unwrap();
    // Saved before, said once, done once.
    assert_eq!(backups(&w.catalog).len(), 1);
    let backup = Connection::open(w.catalog.join(&backups(&w.catalog)[0])).unwrap();
    let before: i64 = backup
        .query_row("SELECT count(*) FROM assets", [], |r| r.get(0))
        .unwrap();
    assert_eq!(before, 2, "the backup holds the catalog as it was");
    let report = lib.take_duplicate_repair().unwrap().unwrap();
    assert_eq!(report.photos.len(), 1);
    assert_eq!(report.photos[0].path, w.photo);
    assert_eq!(report.photos[0].merged, std::slice::from_ref(&w.old));
    assert_eq!(report.photos[0].snapshots, 1);
    assert!(lib.take_duplicate_repair().unwrap().is_none());
    drop(lib);
    let again = Library::open(&w.catalog).unwrap();
    assert_eq!(rows(&w.catalog), 1);
    assert_eq!(backups(&w.catalog).len(), 1, "nothing to merge: no backup");
    assert!(again.take_duplicate_repair().unwrap().is_none());
    // The photograph itself is never touched.
    assert_eq!(fs::read(&w.photo).unwrap(), original);
}

#[test]
fn an_unedited_row_takes_the_edit_of_the_row_merged_into_it() {
    let w = world(
        |lib, id| lib.save_editor_state(id, &state(0.9)).unwrap(),
        |_, _| {},
    );
    let lib = Library::open(&w.catalog).unwrap();
    assert_eq!(
        lib.editor_state(&w.new).unwrap().unwrap()["adjustments"]["exposure"],
        json!(0.9)
    );
    assert!(
        lib.snapshots(&lib.primary_version(&w.new).unwrap())
            .unwrap()
            .is_empty()
    );
}

#[test]
fn a_folder_with_two_records_and_one_row_keeps_it_under_one_record() {
    let tmp = tempfile::tempdir().unwrap();
    let root = rawtonic_fs::canonicalize(tmp.path()).unwrap();
    let catalog = root.join("Main.catalog");
    let shoot = root.join("Shoot");
    fs::create_dir_all(&shoot).unwrap();
    let photo = shoot.join("a.jpg");
    fs::write(&photo, b"synthetic photograph").unwrap();
    let id = {
        let lib = Library::create(&catalog, "Main", Mode::Catalog).unwrap();
        lib.add_asset(&photo).unwrap().id
    };
    {
        let db = database(&catalog);
        db.execute(
            "INSERT INTO roots(id,path,external) SELECT 'external-old',path,1 FROM roots WHERE external=1",
            [],
        )
        .unwrap();
        db.execute("UPDATE assets SET root_id='external-old'", [])
            .unwrap();
    }
    let lib = Library::open(&catalog).unwrap();
    let roots: i64 = database(&catalog)
        .query_row("SELECT count(*) FROM roots WHERE external=1", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(roots, 1);
    assert_eq!(lib.asset_at(&photo).unwrap().unwrap().id, id);
    assert_eq!(
        lib.add_asset(&photo).unwrap().id,
        id,
        "not registered twice"
    );
    assert_eq!(rows(&catalog), 1);
    assert!(lib.take_duplicate_repair().unwrap().is_none());
}

/// A catalog folder copied elsewhere and its records pointed there (as a
/// relinked folder or a moved catalog keeps its records' ids): the record
/// is found by its path, also written with a trailing separator, and its
/// photographs are not registered again.
#[test]
fn a_record_pointed_at_another_folder_is_found_by_its_path() {
    let tmp = tempfile::tempdir().unwrap();
    let root = rawtonic_fs::canonicalize(tmp.path()).unwrap();
    let catalog = root.join("Main.catalog");
    let (old, new) = (root.join("Old"), root.join("New"));
    fs::create_dir_all(&old).unwrap();
    fs::create_dir_all(&new).unwrap();
    fs::write(old.join("a.jpg"), b"synthetic photograph").unwrap();
    fs::copy(old.join("a.jpg"), new.join("a.jpg")).unwrap();
    let id = {
        let lib = Library::create(&catalog, "Main", Mode::Catalog).unwrap();
        let id = lib.add_asset(&old.join("a.jpg")).unwrap().id;
        let mut meta = lib.metadata(&id).unwrap();
        meta.rating = 2;
        lib.set_metadata(&id, &meta).unwrap();
        id
    };
    database(&catalog)
        .execute(
            "UPDATE roots SET path=? WHERE external=1",
            [format!("{}{}", new.display(), std::path::MAIN_SEPARATOR)],
        )
        .unwrap();
    let lib = Library::open(&catalog).unwrap();
    let photo = new.join("a.jpg");
    assert_eq!(lib.asset_at(&photo).unwrap().unwrap().id, id);
    edit_at(&lib, &photo, |s| s["rating"] = json!(5));
    edit_at(&lib, &photo, |s| s["adjustments"]["exposure"] = json!(0.5));
    assert_eq!(rows(&catalog), 1, "not registered again");
    assert_eq!(lib.asset_at(&photo).unwrap().unwrap().rating, 5);
}

/// The Windows library of the report: the folder copied twice, each copy's
/// records pointed at the new folder (one with a trailing separator), each
/// with its own row for the file. One photograph is left, in the record
/// the folder's own path names.
#[test]
fn three_records_of_one_folder_leave_one_photograph() {
    let tmp = tempfile::tempdir().unwrap();
    let root = rawtonic_fs::canonicalize(tmp.path()).unwrap();
    let catalog = root.join("Main.catalog");
    let folders: Vec<PathBuf> = ["raws", "r1", "r"].iter().map(|n| root.join(n)).collect();
    let mut ids = Vec::new();
    {
        let lib = Library::create(&catalog, "Main", Mode::Catalog).unwrap();
        for (n, folder) in folders.iter().enumerate() {
            fs::create_dir_all(folder).unwrap();
            fs::write(folder.join("Canon_R6.CR3"), b"synthetic raw").unwrap();
            let id = lib.add_asset(&folder.join("Canon_R6.CR3")).unwrap().id;
            let mut meta = lib.metadata(&id).unwrap();
            meta.rating = [2, 2, 5][n];
            lib.set_metadata(&id, &meta).unwrap();
            ids.push(id);
        }
    }
    let last = folders[2].to_string_lossy().into_owned();
    database(&catalog)
        .execute(
            "UPDATE roots SET path=CASE WHEN path LIKE '%r1' THEN ?1 || ?2 ELSE ?1 END WHERE external=1",
            rusqlite::params![last, std::path::MAIN_SEPARATOR.to_string()],
        )
        .unwrap();
    let lib = Library::open(&catalog).unwrap();
    assert_eq!(rows(&catalog), 1);
    let photo = folders[2].join("Canon_R6.CR3");
    let kept = lib.asset_at(&photo).unwrap().unwrap();
    assert_eq!(kept.id, ids[2]);
    assert_eq!(kept.rating, 5);
    assert_eq!(
        lib.take_duplicate_repair().unwrap().unwrap().photos[0]
            .merged
            .len(),
        2
    );
    lib.check_integrity().unwrap();
}

/// A clear is a value: a flag cleared through the path lookup and a rating
/// cleared through registering stay cleared, though the other row still
/// has the earlier value.
#[test]
fn a_cleared_flag_and_a_cleared_rating_stay_cleared() {
    let w = world(
        |lib, id| {
            // Looked up: the flag was cleared here; an old rating.
            let mut meta = lib.metadata(id).unwrap();
            meta.rating = 3;
            meta.flag = 0;
            meta.label = "Blue".into();
            lib.set_metadata(id, &meta).unwrap();
        },
        |lib, id| {
            // Registering: the rating and label were cleared here; an old flag.
            let mut meta = lib.metadata(id).unwrap();
            meta.rating = 0;
            meta.flag = 1;
            meta.description = "kept from the camera".into();
            lib.set_metadata(id, &meta).unwrap();
        },
    );
    let lib = Library::open(&w.catalog).unwrap();
    let meta = lib.metadata(&w.new).unwrap();
    assert_eq!(meta.rating, 0, "cleared through registering");
    assert_eq!(meta.label, "", "cleared through registering");
    assert_eq!(meta.flag, 0, "cleared through the path lookup");
    assert_eq!(meta.description, "", "the looked-up row's caption");
    let report = lib.take_duplicate_repair().unwrap().unwrap();
    let taken = &report.photos[0].taken;
    assert!(taken.iter().any(|t| t.starts_with("flag <- ")), "{taken:?}");
    assert!(!taken.iter().any(|t| t.starts_with("rating")), "{taken:?}");
}

/// A virtual copy both rows have: the kept row's copy keeps its edit, the
/// other's different edit is a snapshot of it; a copy only the other row
/// has comes over with its stack.
#[test]
fn virtual_copies_and_their_stacks_come_along() {
    use rawtonic_catalog::stacks::Member;
    let tmp_other = tempfile::tempdir().unwrap();
    let other_photo = rawtonic_fs::canonicalize(tmp_other.path())
        .unwrap()
        .join("b.jpg");
    fs::write(&other_photo, b"another photograph").unwrap();
    let other_photo_for_old = other_photo.clone();
    let third_photo = other_photo.with_file_name("c.jpg");
    fs::write(&third_photo, b"a third photograph").unwrap();
    let w = world(
        move |lib, id| {
            lib.save_copy_state(id, "c0ffee", &state(-1.0)).unwrap();
            lib.save_copy_state(id, "beef01", &state(0.25)).unwrap();
            let b = lib.add_asset(&other_photo_for_old).unwrap().id;
            lib.group_stack(&[
                Member {
                    asset_id: id.to_string(),
                    copy: "beef01".into(),
                },
                Member::photo(b),
            ])
            .unwrap();
        },
        move |lib, id| {
            lib.save_copy_state(id, "c0ffee", &state(1.0)).unwrap();
            // The kept photograph is in a stack of its own.
            let c = lib.add_asset(&third_photo).unwrap().id;
            lib.group_stack(&[Member::photo(id), Member::photo(c)])
                .unwrap();
        },
    );
    let lib = Library::open(&w.catalog).unwrap();
    assert_eq!(lib.copies(&w.new).unwrap(), ["beef01", "c0ffee"]);
    assert_eq!(
        lib.copy_state(&w.new, "c0ffee").unwrap().unwrap()["adjustments"]["exposure"],
        json!(1.0)
    );
    let copy = lib.version_for(&w.new, Some("c0ffee")).unwrap().unwrap();
    let snapshots = lib.snapshots(&copy).unwrap();
    assert_eq!(snapshots.len(), 1);
    assert_eq!(snapshots[0].name, REPAIR_SNAPSHOT);
    assert_eq!(snapshots[0].adjustments["exposure"], json!(-1.0));
    let stack = lib
        .stack_of(&Member {
            asset_id: w.new.clone(),
            copy: "beef01".into(),
        })
        .unwrap()
        .expect("the copy's stack stays");
    assert_eq!(stack.members.len(), 2);
    let own = lib
        .stack_of(&Member::photo(w.new.clone()))
        .unwrap()
        .expect("its own stack stays");
    assert_ne!(own.id, stack.id);
    lib.check_integrity().unwrap();
}

/// Three records of one folder that hold different files: the path lookup
/// found each file in the oldest record holding it, so that row's cleared
/// flag stays cleared, though an older record holds only another file.
#[test]
fn the_looked_up_row_is_found_per_file() {
    let tmp = tempfile::tempdir().unwrap();
    let root = rawtonic_fs::canonicalize(tmp.path()).unwrap();
    let catalog = root.join("Main.catalog");
    let folders: Vec<PathBuf> = ["old", "middle", "r"]
        .iter()
        .map(|n| root.join(n))
        .collect();
    for folder in &folders {
        fs::create_dir_all(folder).unwrap();
    }
    fs::write(folders[0].join("b.jpg"), b"b").unwrap();
    for folder in &folders[1..] {
        fs::write(folder.join("a.jpg"), b"a").unwrap();
    }
    let (middle, kept) = {
        let lib = Library::create(&catalog, "Main", Mode::Catalog).unwrap();
        lib.add_asset(&folders[0].join("b.jpg")).unwrap();
        // Looked up: the flag was cleared here.
        let middle = lib.add_asset(&folders[1].join("a.jpg")).unwrap().id;
        // Registering: an old flag.
        let kept = lib.add_asset(&folders[2].join("a.jpg")).unwrap().id;
        let mut meta = lib.metadata(&kept).unwrap();
        meta.flag = 1;
        lib.set_metadata(&kept, &meta).unwrap();
        (middle, kept)
    };
    fs::write(folders[2].join("b.jpg"), b"b").unwrap();
    database(&catalog)
        .execute(
            "UPDATE roots SET path=? WHERE external=1",
            [folders[2].to_string_lossy().as_ref()],
        )
        .unwrap();
    let lib = Library::open(&catalog).unwrap();
    assert_eq!(rows(&catalog), 2, "a.jpg and b.jpg");
    let a = lib.asset_at(&folders[2].join("a.jpg")).unwrap().unwrap();
    assert_eq!(a.id, kept);
    assert_eq!(lib.metadata(&kept).unwrap().flag, 0, "cleared on {middle}");
    assert!(lib.asset_at(&folders[2].join("b.jpg")).unwrap().is_some());
}
