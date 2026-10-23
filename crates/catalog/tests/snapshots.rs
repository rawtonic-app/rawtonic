//! Named snapshots (G26, schema 12): kept beside a version's history, carried
//! by the editor state the library hands out and adopted from sidecars.
use rawtonic_catalog::{Library, Mode};
use serde_json::{Value, json};
use std::fs;

fn session(dir: &std::path::Path) -> (Library, String, std::path::PathBuf) {
    let lib = Library::create(dir, "Snaps", Mode::Session).unwrap();
    let photo = dir.join("Capture/DSC001.ARW");
    fs::write(&photo, b"synthetic original fixture").unwrap();
    let asset = lib.add_asset(&photo).unwrap();
    (lib, asset.id, photo)
}

fn names(lib: &Library, version: &str) -> Vec<String> {
    lib.snapshots(version)
        .unwrap()
        .into_iter()
        .map(|s| s.name)
        .collect()
}

/// A mask bitmap's data URL, long enough to be stored as a blob.
fn bitmap(seed: char) -> String {
    format!("data:image/png;base64,{}", seed.to_string().repeat(20_000))
}

#[test]
fn snapshots_are_taken_renamed_and_deleted_in_their_order() {
    let tmp = tempfile::tempdir().unwrap();
    let (lib, asset, _) = session(tmp.path());
    let version = lib.primary_version(&asset).unwrap();
    let first = lib
        .add_snapshot(&version, "  Warm  ", json!({"exposure": 0.5}))
        .unwrap();
    assert_eq!(first.name, "Warm");
    assert!(chrono_like(&first.created_at), "{}", first.created_at);
    let second = lib
        .add_snapshot(&version, "Cold", json!({"exposure": -0.5}))
        .unwrap();
    assert_eq!(names(&lib, &version), ["Warm", "Cold"]);
    lib.rename_snapshot(&version, &second.id, "Blue hour")
        .unwrap();
    // Updated with other settings: name, id, time and place stay.
    lib.update_snapshot(&version, &second.id, json!({"exposure": -1.0}))
        .unwrap();
    assert_eq!(lib.snapshots(&version).unwrap()[1].id, second.id);
    assert_eq!(
        lib.snapshots(&version).unwrap()[1].created_at,
        second.created_at
    );
    lib.update_snapshot(&version, &second.id, json!({"exposure": -0.5}))
        .unwrap();
    assert!(lib.update_snapshot(&version, &second.id, json!(1)).is_err());
    assert!(lib.update_snapshot(&version, "nope", json!({})).is_err());
    assert_eq!(names(&lib, &version), ["Warm", "Blue hour"]);
    assert_eq!(
        lib.snapshots(&version).unwrap()[1].adjustments,
        json!({"exposure": -0.5})
    );
    lib.delete_snapshot(&version, &first.id).unwrap();
    assert_eq!(names(&lib, &version), ["Blue hour"]);

    // Refused: empty or too long names, unknown snapshots and versions,
    // adjustments that are not an object.
    assert!(lib.add_snapshot(&version, "   ", json!({})).is_err());
    assert!(
        lib.add_snapshot(&version, &"x".repeat(201), json!({}))
            .is_err()
    );
    assert!(
        lib.add_snapshot(&version, &"é".repeat(200), json!({}))
            .is_ok()
    );
    assert!(lib.add_snapshot(&version, "List", json!([1])).is_err());
    assert!(lib.add_snapshot("no such version", "A", json!({})).is_err());
    assert!(lib.rename_snapshot(&version, &first.id, "Again").is_err());
    assert!(lib.delete_snapshot(&version, &first.id).is_err());
    assert!(lib.rename_snapshot(&version, &second.id, "").is_err());
}

fn chrono_like(text: &str) -> bool {
    text.len() == "2026-10-20T10:11:12.123Z".len() && text.ends_with('Z')
}

#[test]
fn snapshots_are_no_history_step_and_survive_edits_and_undo() {
    let tmp = tempfile::tempdir().unwrap();
    let (lib, asset, _) = session(tmp.path());
    let version = lib.primary_version(&asset).unwrap();
    lib.save_editor_state(
        &asset,
        &json!({"rating":0,"tags":[],"adjustments":{"exposure":1.0}}),
    )
    .unwrap();
    let head = lib.recipe(&version).unwrap();
    lib.add_snapshot(&version, "One stop", json!({"exposure": 1.0}))
        .unwrap();
    assert_eq!(lib.recipe(&version).unwrap(), head, "no new history step");

    // The editor state handed out carries them; saving it back (the editor
    // always saves what it loaded, snapshots included) stores no copy of
    // them in the history and changes nothing about them.
    let mut state = lib.editor_state(&asset).unwrap().unwrap();
    assert_eq!(state["snapshots"][0]["name"], "One stop");
    state["adjustments"]["exposure"] = json!(2.0);
    lib.save_editor_state(&asset, &state).unwrap();
    let recipe = lib.recipe(&version).unwrap().unwrap();
    assert!(recipe["editor_state"].get("snapshots").is_none());
    assert_eq!(recipe["editor_state"]["adjustments"]["exposure"], 2.0);

    // A stale state (one without them, or with others) does not delete or
    // replace them: the library's are the snapshots.
    lib.save_editor_state(&asset, &json!({"adjustments":{"exposure":3.0}}))
        .unwrap();
    lib.save_editor_state(
        &asset,
        &json!({"adjustments":{"exposure":3.0},"snapshots":[{"id":"x","name":"Other","createdAt":"2026-10-20T10:00:00.000Z","adjustments":{}}]}),
    )
    .unwrap();
    assert_eq!(names(&lib, &version), ["One stop"]);
    assert!(lib.undo(&version).unwrap());
    assert_eq!(names(&lib, &version), ["One stop"]);
}

#[test]
fn long_strings_are_stored_once_and_kept_while_a_snapshot_uses_them() {
    let tmp = tempfile::tempdir().unwrap();
    let (lib, asset, photo) = session(tmp.path());
    let version = lib.primary_version(&asset).unwrap();
    let mask = json!({"masks":[{"subMasks":[{"type":"ai-sky","parameters":{"maskDataBase64": bitmap('a')}}]}]});
    lib.save_editor_state(&asset, &json!({"adjustments": mask.clone()}))
        .unwrap();
    lib.add_snapshot(&version, "Sky", mask.clone()).unwrap();
    // The edit drops the mask; stored strings no recipe uses are pruned
    // (removing a virtual copy prunes them), the snapshot's stays.
    lib.save_editor_state(&asset, &json!({"adjustments": {}}))
        .unwrap();
    lib.save_copy_state(&asset, "abc123", &json!({"adjustments":{}}))
        .unwrap();
    assert!(lib.remove_copy(&asset, "abc123").unwrap());
    let db = rusqlite::Connection::open(tmp.path().join("session.sqlite")).unwrap();
    let stored: String = db
        .query_row("SELECT adjustments FROM snapshots", [], |r| r.get(0))
        .unwrap();
    assert!(stored.len() < 1000, "stored by reference: {stored}");
    assert_eq!(lib.snapshots(&version).unwrap()[0].adjustments, mask);
    // Updating it with other settings drops nothing the snapshot still uses.
    let id = lib.snapshots(&version).unwrap()[0].id.clone();
    lib.update_snapshot(&version, &id, mask.clone()).unwrap();
    assert_eq!(lib.snapshots(&version).unwrap()[0].adjustments, mask);
    // Deleting the snapshot drops the string nothing uses any more.
    let id = lib.snapshots(&version).unwrap()[0].id.clone();
    lib.delete_snapshot(&version, &id).unwrap();
    let blobs: i64 = db
        .query_row("SELECT count(*) FROM recipe_blobs", [], |r| r.get(0))
        .unwrap();
    assert_eq!(blobs, 1, "the history's first step still uses it");
    let _ = photo;
}

#[test]
fn the_editor_sidecar_carries_snapshots_into_a_library_built_again() {
    let tmp = tempfile::tempdir().unwrap();
    let (lib, asset, photo) = session(tmp.path());
    let version = lib.primary_version(&asset).unwrap();
    lib.save_editor_state(
        &asset,
        &json!({"rating":2,"tags":[],"adjustments":{"exposure":0.3}}),
    )
    .unwrap();
    let warm = lib
        .add_snapshot(&version, "Warm", json!({"temperature": 20}))
        .unwrap();
    let mono = lib
        .add_snapshot(
            &version,
            "Mono",
            json!({"saturation": -100, "lutPath": tmp.path().canonicalize().unwrap().join("resources/luts/a.cube").to_string_lossy()}),
        )
        .unwrap();
    // The derived sidecar the app writes is the state the library hands out.
    let state = lib.editor_state(&asset).unwrap().unwrap();
    let sidecar = photo.with_file_name("DSC001.ARW.rrdata");
    fs::write(&sidecar, serde_json::to_vec_pretty(&state).unwrap()).unwrap();
    drop(lib);

    // The session's database is lost; a new one adopts the sidecar.
    fs::remove_file(tmp.path().join("session.sqlite")).unwrap();
    fs::remove_file(tmp.path().join("session.json")).unwrap();
    let lib = Library::create(tmp.path(), "Again", Mode::Session).unwrap();
    let report = lib.add_files(std::slice::from_ref(&photo)).unwrap();
    let asset = &report.added[0].id;
    let version = lib.primary_version(asset).unwrap();
    let again = lib.snapshots(&version).unwrap();
    assert_eq!(
        again,
        vec![warm, mono.clone()],
        "ids, names, times, adjustments"
    );
    // Resource paths inside the library are stored relative to it.
    let db = rusqlite::Connection::open(tmp.path().join("session.sqlite")).unwrap();
    let stored: String = db
        .query_row(
            "SELECT adjustments FROM snapshots WHERE id=?",
            [&mono.id],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        stored.contains("rawtonic-resource:resources/luts/a.cube"),
        "{stored}"
    );
    // Adopting the same sidecar again adds nothing.
    let contents = rawtonic_catalog::Contents::read_sidecars(&photo).unwrap();
    lib.apply_contents(asset, &contents).unwrap();
    assert_eq!(lib.snapshots(&version).unwrap().len(), 2);
}

#[test]
fn a_registered_unedited_photograph_adopts_its_sidecar_snapshots() {
    let tmp = tempfile::tempdir().unwrap();
    let (lib, asset, photo) = session(tmp.path());
    let sidecar = photo.with_file_name("DSC001.ARW.rrdata");
    let state = json!({"version":1,"rating":0,"tags":[],"adjustments":{"exposure":1},
    "snapshots":[
        {"id":"s1","name":"Before","createdAt":"2026-10-20T10:00:00.000Z","adjustments":{}},
        {"id":"s1","name":"Twice","createdAt":"2026-10-20T10:00:00.000Z","adjustments":{}},
        {"id":"s2","name":"","createdAt":"2026-10-20T10:00:00.000Z","adjustments":{}},
        {"id":"s3","name":"No time","createdAt":"yesterday","adjustments":{}},
        {"name":"No id","createdAt":"2026-10-20T10:00:00.000Z","adjustments":{}},
        {"id":"s4","name":"After","createdAt":"2026-10-20T11:00:00+02:00","adjustments":{"exposure":1}}
    ]});
    fs::write(&sidecar, serde_json::to_vec(&state).unwrap()).unwrap();
    let contents = rawtonic_catalog::Contents::read_sidecars(&photo).unwrap();
    assert!(lib.adopt_contents(&asset, &contents).unwrap());
    let version = lib.primary_version(&asset).unwrap();
    // Broken entries are left out; the edit and the valid ones are adopted.
    assert_eq!(names(&lib, &version), ["Before", "After"]);
    assert_eq!(
        lib.editor_state(&asset).unwrap().unwrap()["adjustments"]["exposure"],
        1
    );
}

#[test]
fn virtual_copies_have_their_own_snapshots() {
    let tmp = tempfile::tempdir().unwrap();
    let (lib, asset, _) = session(tmp.path());
    // A copy's sidecar first recorded (adopted) brings its snapshots.
    let carried = json!({"adjustments":{"exposure":1},"snapshots":[
        {"id":"c1","name":"Copy look","createdAt":"2026-10-20T10:00:00.000Z","adjustments":{"exposure":1}}]});
    lib.save_copy_state(&asset, "abc123", &carried).unwrap();
    let copy = lib.version_for(&asset, Some("abc123")).unwrap().unwrap();
    let primary = lib.version_for(&asset, None).unwrap().unwrap();
    assert_ne!(copy, primary);
    assert_eq!(names(&lib, &copy), ["Copy look"]);
    assert!(lib.snapshots(&primary).unwrap().is_empty());
    assert!(lib.version_for(&asset, Some("fff000")).unwrap().is_none());

    lib.add_snapshot(&copy, "Second", json!({})).unwrap();
    let state = lib.copy_state(&asset, "abc123").unwrap().unwrap();
    assert_eq!(state["snapshots"].as_array().unwrap().len(), 2);
    // Later saves of its state change nothing about them.
    lib.save_copy_state(&asset, "abc123", &json!({"adjustments":{}}))
        .unwrap();
    assert_eq!(names(&lib, &copy), ["Copy look", "Second"]);
    let stored: Value = {
        let db = rusqlite::Connection::open(tmp.path().join("session.sqlite")).unwrap();
        let text: String = db
            .query_row(
                "SELECT legacy_state FROM versions WHERE id=?",
                [&copy],
                |r| r.get(0),
            )
            .unwrap();
        serde_json::from_str(&text).unwrap()
    };
    assert!(stored.get("snapshots").is_none());
    // Removing the copy removes its snapshots.
    assert!(lib.remove_copy(&asset, "abc123").unwrap());
    assert!(lib.snapshots(&copy).unwrap().is_empty());
}

#[test]
fn xmp_sidecars_are_unchanged_by_snapshots() {
    let tmp = tempfile::tempdir().unwrap();
    let (lib, asset, photo) = session(tmp.path());
    let version = lib.primary_version(&asset).unwrap();
    lib.save_editor_state(
        &asset,
        &json!({"rating":4,"tags":["user:dog"],"adjustments":{"exposure":0.3}}),
    )
    .unwrap();
    // As the editor saves: the state handed out, saved back.
    let state = lib.editor_state(&asset).unwrap().unwrap();
    lib.save_editor_state(&asset, &state).unwrap();
    lib.sync_xmp(&asset).unwrap();
    let before = fs::read_to_string(photo.with_extension("xmp")).unwrap();
    lib.add_snapshot(&version, "Warm", json!({"temperature": 20}))
        .unwrap();
    let state = lib.editor_state(&asset).unwrap().unwrap();
    lib.save_editor_state(&asset, &state).unwrap();
    lib.sync_xmp(&asset).unwrap();
    let after = fs::read_to_string(photo.with_extension("xmp")).unwrap();
    assert_eq!(
        before, after,
        "the XMP's metadata and recipe are the edit's"
    );
    let (metadata, recipe) = rawtonic_metadata::read_xmp(&after).unwrap();
    assert_eq!(
        (metadata.rating, metadata.keywords),
        (4, vec!["dog".to_string()])
    );
    assert!(!recipe.unwrap().to_string().contains("Warm"));
}

#[test]
fn a_schema_11_library_migrates_and_keeps_its_edits() {
    let tmp = tempfile::tempdir().unwrap();
    let (lib, asset, _) = session(tmp.path());
    lib.save_editor_state(
        &asset,
        &json!({"rating":3,"tags":[],"adjustments":{"exposure":0.7}}),
    )
    .unwrap();
    lib.save_copy_state(&asset, "abc123", &json!({"adjustments":{"exposure":-1}}))
        .unwrap();
    drop(lib);
    let db = rusqlite::Connection::open(tmp.path().join("session.sqlite")).unwrap();
    db.execute_batch(concat!(
        include_str!("undo_0014.sql"),
        include_str!("undo_0013.sql"),
        include_str!("undo_0012.sql")
    ))
    .unwrap();
    drop(db);

    let lib = Library::open(tmp.path()).unwrap();
    let db = rusqlite::Connection::open(tmp.path().join("session.sqlite")).unwrap();
    let version: u32 = db
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, rawtonic_catalog::DB_SCHEMA_VERSION);
    let state = lib.editor_state(&asset).unwrap().unwrap();
    assert_eq!(state["adjustments"]["exposure"], 0.7);
    assert!(state.get("snapshots").is_none());
    let primary = lib.primary_version(&asset).unwrap();
    assert!(lib.snapshots(&primary).unwrap().is_empty());
    lib.add_snapshot(&primary, "After the update", json!({"exposure": 0.7}))
        .unwrap();
    let copy = lib.version_for(&asset, Some("abc123")).unwrap().unwrap();
    lib.add_snapshot(&copy, "Copy", json!({})).unwrap();
    assert_eq!(names(&lib, &primary), ["After the update"]);
    drop(lib);
    // Opening again does not migrate twice; the database stays consistent.
    let lib = Library::open(tmp.path()).unwrap();
    assert_eq!(names(&lib, &copy), ["Copy"]);
    let broken: i64 = db
        .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(broken, 0);
}

#[test]
fn a_library_copy_keeps_snapshots_and_rewrites_their_resources() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("Job");
    fs::create_dir_all(&source).unwrap();
    let (lib, asset, _) = session(&source);
    let version = lib.primary_version(&asset).unwrap();
    lib.add_snapshot(&version, "Look", json!({"lutPath": "/elsewhere/look.cube"}))
        .unwrap();
    drop(lib);
    let copy = tmp.path().join("Copy");
    fs::create_dir_all(&copy).unwrap();
    let lib = Library::snapshot_from(&source, &copy).unwrap();
    let mut seen = Vec::new();
    lib.rewrite_resources(|path, used| {
        seen.push((path.to_string(), used.current));
        Ok(path.replace("/elsewhere", "/moved"))
    })
    .unwrap();
    assert!(
        seen.contains(&("/elsewhere/look.cube".to_string(), false)),
        "{seen:?}"
    );
    assert_eq!(
        lib.snapshots(&version).unwrap()[0].adjustments,
        json!({"lutPath": "/moved/look.cube"})
    );
}

#[test]
fn a_sidecar_holds_each_long_snapshot_string_once() {
    let tmp = tempfile::tempdir().unwrap();
    let (lib, asset, photo) = session(tmp.path());
    let version = lib.primary_version(&asset).unwrap();
    let mask = json!({"masks":[{"subMasks":[{"parameters":{"maskDataBase64": bitmap('b')}}]}]});
    for name in ["One", "Two", "Three"] {
        lib.add_snapshot(&version, name, mask.clone()).unwrap();
    }
    let state = lib.editor_state(&asset).unwrap().unwrap();
    let text = serde_json::to_string(&state).unwrap();
    assert_eq!(text.matches(&bitmap('b')).count(), 1, "written once");
    assert!(text.len() < 30_000, "{}", text.len());
    // Read back whole.
    let back = rawtonic_catalog::snapshots::in_state(&state);
    assert_eq!(back.len(), 3);
    assert!(back.iter().all(|s| s.adjustments == mask));
    // A sidecar that lost the string keeps the snapshots that do not need it.
    let mut broken = state.clone();
    broken["snapshotStrings"] = json!({});
    broken["snapshots"]
        .as_array_mut()
        .unwrap()
        .push(json!({"id":"plain","name":"Plain","createdAt":"2026-10-20T10:00:00.000Z","adjustments":{}}));
    let kept: Vec<String> = rawtonic_catalog::snapshots::in_state(&broken)
        .into_iter()
        .map(|s| s.name)
        .collect();
    assert_eq!(kept, ["Plain"]);
    // Edits do not store the strings in the recipe history either.
    lib.save_editor_state(&asset, &state).unwrap();
    let recipe = lib.recipe(&version).unwrap().unwrap();
    assert!(recipe["editor_state"].get("snapshotStrings").is_none());
    let _ = photo;
}
