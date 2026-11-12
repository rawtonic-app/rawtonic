//! Catalogs written on Windows before G77 stored their folders verbatim
//! (`\\?\C:\…`); opened now, they find their photographs under the plain
//! paths the app uses, keep ids, edits and drives, and do not register a
//! photograph twice.
#![cfg(windows)]
use rawtonic_catalog::{Library, Mode, layout::database_name};
use rusqlite::Connection;
use serde_json::json;
use std::fs;

#[test]
fn a_catalog_with_verbatim_folders_finds_its_photographs_under_plain_paths() {
    let tmp = tempfile::tempdir().unwrap();
    let root = rawtonic_fs::canonicalize(tmp.path()).unwrap();
    let catalog = root.join("Main.catalog");
    let shoot = root.join("Shoot");
    let empty = root.join("Empty");
    fs::create_dir_all(&shoot).unwrap();
    fs::create_dir_all(&empty).unwrap();
    let photo = shoot.join("a.jpg");
    fs::write(&photo, b"synthetic photograph").unwrap();
    let trashed = shoot.join("b.jpg");
    fs::write(&trashed, b"another synthetic photograph").unwrap();
    let id = {
        let lib = Library::create(&catalog, "Main", Mode::Catalog).unwrap();
        let id = lib.add_asset(&photo).unwrap().id;
        lib.add_asset(&trashed).unwrap();
        lib.save_editor_state(
            &id,
            &json!({"version": 1, "adjustments": {"exposure": 0.7}}),
        )
        .unwrap();
        id
    };
    // What the earlier app wrote: the folder's and its drive's paths verbatim,
    // its id from that spelling; a photograph moved to the system Trash; a
    // folder with no photograph yet.
    let recorded: Option<String> = {
        let db = Connection::open(catalog.join(database_name(Mode::Catalog))).unwrap();
        let drive = db
            .query_row("SELECT volume_id FROM roots WHERE external=1", [], |r| {
                r.get(0)
            })
            .unwrap();
        db.execute_batch("PRAGMA foreign_keys=OFF").unwrap();
        db.execute(
            "UPDATE roots SET id='external-old', path=?, volume_path='\\\\?\\' || volume_path WHERE external=1",
            [format!(r"\\?\{}", shoot.display())],
        )
        .unwrap();
        db.execute("UPDATE assets SET root_id='external-old'", [])
            .unwrap();
        db.execute(
            "UPDATE assets SET trashed=1, missing=1 WHERE rel_path='b.jpg'",
            [],
        )
        .unwrap();
        db.execute(
            "INSERT INTO roots(id,path,external) VALUES('external-empty',?,1)",
            [format!(r"\\?\{}", empty.display())],
        )
        .unwrap();
        drive
    };
    fs::remove_file(&trashed).unwrap();
    let lib = Library::open(&catalog).unwrap();
    let found = lib
        .asset_at(&photo)
        .unwrap()
        .expect("found under the plain path");
    assert_eq!(found.id, id);
    assert_eq!(found.path, photo);
    assert_eq!(
        lib.add_asset(&photo).unwrap().id,
        id,
        "not registered twice"
    );
    assert!(lib.asset_at(&trashed).unwrap().is_some_and(|a| a.id != id));
    assert_eq!(
        lib.editor_state(&id).unwrap().unwrap()["adjustments"]["exposure"],
        json!(0.7)
    );
    drop(lib);
    let db = Connection::open(catalog.join(database_name(Mode::Catalog))).unwrap();
    let rows: Vec<(String, Option<String>, Option<String>)> = db
        .prepare("SELECT path,volume_id,volume_path FROM roots WHERE external=1 ORDER BY path")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert!(
        rows.iter()
            .all(|(path, _, volume)| !path.starts_with(r"\\?\")
                && !volume.as_deref().unwrap_or("").starts_with(r"\\?\")),
        "{rows:?}"
    );
    let shoot_row = rows
        .iter()
        .find(|row| row.0 == shoot.display().to_string())
        .unwrap();
    assert_eq!(shoot_row.1, recorded, "the recorded drive is kept");
    let state: (i64, i64) = db
        .query_row(
            "SELECT trashed,missing FROM assets WHERE rel_path='b.jpg'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(state, (1, 1), "a trashed photograph stays trashed");
    // A folder without photographs yet keeps its record, now plain.
    assert!(
        rows.iter().any(|row| row.0 == empty.display().to_string()),
        "{rows:?}"
    );
    // Opening again changes nothing.
    drop(db);
    let lib = Library::open(&catalog).unwrap();
    assert_eq!(lib.asset_at(&photo).unwrap().unwrap().id, id);
}
