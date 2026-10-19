use rawtonic_catalog::{GROUP, Library, Mode};

#[test]
fn albums_hold_assets_survive_a_moved_session_and_never_delete_photos() {
    let temp = tempfile::tempdir().unwrap();
    let directory = temp.path().join("job");
    let lib = Library::create(&directory, "Albums", Mode::Session).unwrap();
    let mut ids = Vec::new();
    for name in ["b", "a", "c"] {
        let path = lib.directory.join(format!("Capture/{name}.arw"));
        std::fs::write(&path, format!("original {name}")).unwrap();
        ids.push(lib.add_asset(&path).unwrap().id);
    }
    // All metadata in one query, by path.
    let mut meta = lib.metadata(&ids[0]).unwrap();
    meta.title = "Pier".into();
    lib.set_metadata(&ids[0], &meta).unwrap();
    let all = lib.all_metadata().unwrap();
    assert_eq!(all.len(), 3);
    let titled: Vec<_> = all
        .iter()
        .filter(|(_, m)| m.title == "Pier")
        .map(|(p, _)| p.clone())
        .collect();
    assert_eq!(titled, [lib.directory.join("Capture/b.arw")]);
    assert!(lib.create_collection("  ", None).is_err());
    let picks = lib.create_collection(" Picks ", None).unwrap();
    assert_eq!(picks.name, "Picks");
    let client = lib
        .create_collection_of_kind("Client", None, GROUP)
        .unwrap();
    let web = lib.create_collection("Web", Some(&client.id)).unwrap();
    assert!(lib.create_collection("Orphan", Some("missing")).is_err());
    // Albums hold photographs, groups hold albums.
    assert!(lib.create_collection("Inside", Some(&picks.id)).is_err());
    assert!(lib.add_to_collection(&client.id, &ids).is_err());
    lib.add_to_collection(&picks.id, &ids[..2]).unwrap();
    lib.add_to_collection(&picks.id, &ids[..1]).unwrap();
    lib.add_to_collection(&web.id, &ids[2..]).unwrap();
    assert!(lib.add_to_collection("missing", &ids).is_err());
    let names = |lib: &Library| {
        lib.collections()
            .unwrap()
            .into_iter()
            .map(|c| (c.name, c.count))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        names(&lib),
        [("Picks".into(), 2), ("Web".into(), 1), ("Client".into(), 0)]
    );
    let paths: Vec<_> = lib
        .collection_assets(&picks.id)
        .unwrap()
        .into_iter()
        .map(|a| a.rel_path)
        .collect();
    assert_eq!(paths, ["Capture/a.arw", "Capture/b.arw"]);
    lib.rename_collection(&picks.id, "Selects").unwrap();
    assert!(lib.rename_collection(&picks.id, "").is_err());
    drop(lib);

    // Albums store asset ids, so a moved session keeps them.
    let moved = temp.path().join("moved");
    std::fs::rename(&directory, &moved).unwrap();
    let lib = Library::open(&moved).unwrap();
    assert_eq!(
        names(&lib),
        [
            ("Selects".into(), 2),
            ("Web".into(), 1),
            ("Client".into(), 0)
        ]
    );
    assert!(
        lib.collection_assets(&picks.id).unwrap()[0]
            .path
            .starts_with(lib.directory.join("Capture"))
    );
    lib.remove_from_collection(&picks.id, &ids[1..2]).unwrap();
    assert_eq!(lib.collection_assets(&picks.id).unwrap().len(), 1);
    // Deleting a group removes the albums inside it, never photographs.
    lib.delete_collection(&client.id).unwrap();
    assert_eq!(names(&lib), [("Selects".into(), 1)]);
    lib.delete_collection(&picks.id).unwrap();
    assert!(lib.collections().unwrap().is_empty());
    assert!(lib.delete_collection(&picks.id).is_err());
    assert_eq!(lib.assets(None).unwrap().len(), 3);
    for name in ["a", "b", "c"] {
        assert_eq!(
            std::fs::read_to_string(moved.join(format!("Capture/{name}.arw"))).unwrap(),
            format!("original {name}")
        );
    }
}

#[test]
fn albums_move_between_groups_in_order_but_never_into_themselves() {
    let temp = tempfile::tempdir().unwrap();
    let lib = Library::create(temp.path(), "Order", Mode::Session).unwrap();
    let a = lib.create_collection("A", None).unwrap();
    let b = lib.create_collection("B", None).unwrap();
    let group = lib.create_collection_of_kind("Group", None, GROUP).unwrap();
    let inner = lib
        .create_collection_of_kind("Inner", Some(&group.id), GROUP)
        .unwrap();
    let order = |lib: &Library, parent: Option<&str>| {
        lib.collections()
            .unwrap()
            .into_iter()
            .filter(|c| c.parent.as_deref() == parent)
            .map(|c| c.name)
            .collect::<Vec<_>>()
    };
    assert_eq!(order(&lib, None), ["A", "B", "Group"]);
    lib.move_collection(&group.id, None, 0).unwrap();
    assert_eq!(order(&lib, None), ["Group", "A", "B"]);
    lib.move_collection(&b.id, Some(&group.id), 0).unwrap();
    assert_eq!(order(&lib, Some(&group.id)), ["B", "Inner"]);
    assert_eq!(order(&lib, None), ["Group", "A"]);
    // A group cannot go into itself or its own groups; albums contain nothing.
    assert!(lib.move_collection(&group.id, Some(&inner.id), 0).is_err());
    assert!(lib.move_collection(&group.id, Some(&group.id), 0).is_err());
    assert!(lib.move_collection(&b.id, Some(&a.id), 0).is_err());
    // An index past the end appends.
    lib.move_collection(&a.id, Some(&inner.id), 99).unwrap();
    assert_eq!(order(&lib, Some(&inner.id)), ["A"]);
}

#[test]
fn version_one_databases_are_migrated_once_and_newer_ones_refused() {
    let temp = tempfile::tempdir().unwrap();
    let lib = Library::create(temp.path(), "Old", Mode::Session).unwrap();
    let album = lib.create_collection("Kept", None).unwrap();
    drop(lib);
    // Turn the database back into schema 1 (no kind/position columns, no
    // volume columns of schema 3, no import batches of schema 4).
    let db = rusqlite::Connection::open(temp.path().join("session.sqlite")).unwrap();
    db.execute_batch(
        "ALTER TABLE assets DROP COLUMN sha256; DROP TABLE asset_imports; DROP TABLE import_batches;
         ALTER TABLE roots DROP COLUMN volume_name; ALTER TABLE roots DROP COLUMN volume_path;
         CREATE TABLE c1(id TEXT PRIMARY KEY, name TEXT NOT NULL, parent TEXT REFERENCES collections(id));
         INSERT INTO c1 SELECT id,name,parent FROM collections;
         PRAGMA foreign_keys=OFF; DROP TABLE collections; ALTER TABLE c1 RENAME TO collections;
         PRAGMA user_version=1;",
    )
    .unwrap();
    drop(db);
    let lib = Library::open(temp.path()).unwrap();
    let albums = lib.collections().unwrap();
    assert_eq!(
        (albums[0].id.as_str(), albums[0].kind.as_str()),
        (album.id.as_str(), "album")
    );
    lib.create_collection_of_kind("New group", None, GROUP)
        .unwrap();
    drop(lib);
    // Opening again does not migrate twice.
    Library::open(temp.path()).unwrap();
    let db = rusqlite::Connection::open(temp.path().join("session.sqlite")).unwrap();
    db.execute_batch("PRAGMA user_version=10;").unwrap();
    drop(db);
    let error = Library::open(temp.path()).err().unwrap().to_string();
    assert!(error.contains("Unsupported database schema 10"), "{error}");
}
