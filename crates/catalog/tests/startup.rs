use rawtonic_catalog::{Library, Mode};
use rusqlite::Connection;
use serde_json::json;
use std::fs;

#[test]
fn path_verification_writes_only_missing_transitions_and_preserves_edits() {
    let temp = tempfile::tempdir().unwrap();
    let lib = Library::create(temp.path(), "Startup", Mode::Session).unwrap();
    let source = lib.directory.join("Capture/photo.arw");
    fs::write(&source, b"fixture original").unwrap();
    let asset = lib.add_asset(&source).unwrap();
    let state = json!({"rating":4,"tags":["kept"],"adjustments":{"exposure":0.5}});
    lib.save_editor_state(&asset.id, &state).unwrap();
    let saved = lib.editor_state(&asset.id).unwrap();
    let version = lib.primary_version(&asset.id).unwrap();
    let recipe = lib.recipe(&version).unwrap();
    let observer = Connection::open(lib.directory.join("session.sqlite")).unwrap();
    let data_version = || {
        observer
            .query_row("PRAGMA data_version", [], |r| r.get::<_, i64>(0))
            .unwrap()
    };

    let before = data_version();
    assert_eq!(lib.verify_paths().unwrap(), 0);
    assert_eq!(
        data_version(),
        before,
        "unchanged startup must not write SQLite"
    );

    fs::remove_file(&source).unwrap();
    assert_eq!(lib.verify_paths().unwrap(), 1);
    assert!(lib.asset(&asset.id).unwrap().missing);
    assert_ne!(data_version(), before);
    let missing_version = data_version();
    assert_eq!(lib.verify_paths().unwrap(), 1);
    assert_eq!(
        data_version(),
        missing_version,
        "unchanged missing state must not write"
    );

    fs::write(&source, b"fixture original").unwrap();
    assert_eq!(lib.verify_paths().unwrap(), 0);
    let restored = lib.asset(&asset.id).unwrap();
    assert!(!restored.missing);
    assert_eq!(restored.fingerprint, asset.fingerprint);
    assert_eq!(lib.editor_state(&asset.id).unwrap(), saved);
    assert_eq!(lib.recipe(&version).unwrap(), recipe);
    assert_eq!(fs::read(source).unwrap(), b"fixture original");
}

#[test]
fn startup_index_preserves_adoption_candidates_and_explicit_metadata_clears() {
    let temp = tempfile::tempdir().unwrap();
    let lib = Library::create(temp.path(), "Adoption", Mode::Session).unwrap();
    let mut entries = Vec::new();
    for name in ["untouched", "recipe_only", "edited", "explicit_clear"] {
        let path = lib.directory.join(format!("Capture/{name}.arw"));
        fs::write(&path, b"fixture").unwrap();
        entries.push(lib.add_asset(&path).unwrap());
    }
    let version = lib.primary_version(&entries[1].id).unwrap();
    lib.save_recipe(
        &version,
        &json!({"schema_version":1,"adjustments":{"exposure":0.5}}),
        None,
    )
    .unwrap();
    lib.save_editor_state(&entries[2].id, &json!({"rating":3,"adjustments":{}}))
        .unwrap();
    lib.set_metadata(&entries[3].id, &Default::default())
        .unwrap();

    let index = lib.index_entries().unwrap();
    assert_eq!(index.len(), entries.len());
    for (asset, expected) in entries.iter().zip([true, true, false, false]) {
        let entry = index.get(&asset.path).unwrap();
        assert_eq!(entry.id, asset.id);
        assert_eq!(entry.needs_metadata_adoption, expected);
    }
}

#[test]
fn bulk_editor_states_match_primary_heads_metadata_overrides_and_path_scope() {
    let temp = tempfile::tempdir().unwrap();
    let lib = Library::create(temp.path(), "Listing", Mode::Session).unwrap();
    let mut assets = Vec::new();
    for name in ["edited", "neutral", "outside_scope"] {
        let path = lib.directory.join(format!("Capture/{name}.arw"));
        fs::write(&path, b"fixture").unwrap();
        assets.push(lib.add_asset(&path).unwrap());
    }
    lib.save_editor_state(
        &assets[0].id,
        &json!({"version":1,"rating":4,"tags":["old"],"adjustments":{"exposure":0.5}}),
    )
    .unwrap();
    lib.set_metadata(&assets[0].id, &Default::default())
        .unwrap();
    // A non-primary recipe must not replace or duplicate the primary state.
    let db = Connection::open(lib.directory.join("session.sqlite")).unwrap();
    // The library's own connections index versions' AI facts (schema 17).
    db.create_scalar_function(
        "rawtonic_ai_facts",
        1,
        rusqlite::functions::FunctionFlags::SQLITE_UTF8,
        |_| Ok(0),
    )
    .unwrap();
    db.execute(
        "INSERT INTO versions(id,asset_id,name,is_primary) VALUES('copy',?,'Copy',0)",
        [&assets[0].id],
    )
    .unwrap();
    lib.save_recipe("copy", &json!({"schema_version":1,"editor_state":{"version":1,"rating":5,"adjustments":{"exposure":2}}}), None).unwrap();
    let paths = assets[..2].iter().map(|asset| asset.path.clone()).collect();
    let mut seen = std::collections::HashMap::new();
    lib.visit_editor_states(&paths, |path, state| {
        assert!(seen.insert(path.to_owned(), state).is_none());
    })
    .unwrap();
    assert_eq!(seen.len(), 2);
    for asset in &assets[..2] {
        assert_eq!(seen[&asset.path], lib.editor_state(&asset.id).unwrap());
    }
    assert!(seen[&assets[1].path].is_none());
    let state = seen[&assets[0].path].as_ref().unwrap();
    assert_eq!(state["rating"], 0);
    assert_eq!(state["tags"], json!([]));
    assert_eq!(state["adjustments"]["exposure"], 0.5);
}

/// A rating, keywords or label the library holds for a photograph never
/// edited (`rawtonic rate`, an adopted XMP) reach the app through the editor
/// state, without turning the photo into an edited one.
#[test]
fn library_metadata_of_an_unedited_photo_is_in_its_editor_state() {
    let temp = tempfile::tempdir().unwrap();
    let lib = Library::create(temp.path(), "Rated", Mode::Session).unwrap();
    let mut assets = Vec::new();
    for name in ["rated.arw", "plain.arw"] {
        let source = lib.directory.join("Capture").join(name);
        fs::write(&source, name).unwrap();
        assets.push(lib.add_asset(&source).unwrap());
    }
    let mut meta = lib.metadata(&assets[0].id).unwrap();
    meta.rating = 4;
    meta.label = "Green".into();
    meta.keywords = vec!["studio".into()];
    lib.set_metadata(&assets[0].id, &meta).unwrap();
    let mut flagged = lib.metadata(&assets[1].id).unwrap();
    flagged.flag = 1;
    lib.set_metadata(&assets[1].id, &flagged).unwrap();

    let state = lib.editor_state(&assets[0].id).unwrap().unwrap();
    assert_eq!(state["rating"], 4);
    assert_eq!(state["tags"], json!(["user:studio", "color:green"]));
    assert!(
        state["adjustments"].is_null(),
        "still opens as never edited"
    );
    assert!(lib.editor_state(&assets[1].id).unwrap().is_none());

    let paths = assets.iter().map(|asset| asset.path.clone()).collect();
    let mut seen = std::collections::HashMap::new();
    lib.visit_editor_states(&paths, |path, state| {
        seen.insert(path.to_owned(), state);
    })
    .unwrap();
    assert_eq!(seen[&assets[0].path].as_ref(), Some(&state));
    assert!(seen[&assets[1].path].is_none());
}
