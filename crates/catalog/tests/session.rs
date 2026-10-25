use rawtonic_catalog::{Library, Mode};
use serde_json::json;

#[test]
fn color_labels_are_standard_metadata_not_keywords() {
    let temp = tempfile::tempdir().unwrap();
    let lib = Library::create(temp.path(), "Labels", Mode::Session).unwrap();
    let source = temp.path().join("Capture/chart.jpg");
    std::fs::write(&source, b"synthetic source").unwrap();
    let asset = lib.add_asset(&source).unwrap();
    lib.save_editor_state(
        &asset.id,
        &json!({"rating":3,"tags":["portrait","color:red"],"adjustments":{}}),
    )
    .unwrap();
    let metadata = lib.metadata(&asset.id).unwrap();
    assert_eq!(metadata.label, "Red");
    assert_eq!(metadata.keywords, vec!["portrait"]);
    assert_eq!(
        lib.editor_state(&asset.id).unwrap().unwrap()["tags"],
        json!(["portrait", "color:red"])
    );
    // Match the native label command: update tags in a loaded state, which still
    // contains the previous adapter-only adjustments.colorLabel value.
    let mut state = lib.editor_state(&asset.id).unwrap().unwrap();
    state["tags"] = json!(["portrait", "color:blue"]);
    lib.save_editor_state(&asset.id, &state).unwrap();
    assert_eq!(lib.metadata(&asset.id).unwrap().label, "Blue");
    lib.sync_xmp(&asset.id).unwrap();
    let reopened = Library::open(temp.path()).unwrap();
    let mut state = reopened.editor_state(&asset.id).unwrap().unwrap();
    assert_eq!(state["tags"], json!(["portrait", "color:blue"]));
    state["tags"] = json!(null);
    reopened.save_editor_state(&asset.id, &state).unwrap();
    assert!(reopened.metadata(&asset.id).unwrap().label.is_empty());
    reopened.sync_xmp(&asset.id).unwrap();
    let (xmp, _) = rawtonic_metadata::read_xmp(
        &std::fs::read_to_string(source.with_extension("xmp")).unwrap(),
    )
    .unwrap();
    assert!(xmp.label.is_empty());
    lib.save_editor_state(&asset.id, &json!({"tags":null,"adjustments":{}}))
        .unwrap();
    assert!(lib.metadata(&asset.id).unwrap().label.is_empty());
    assert!(lib.metadata(&asset.id).unwrap().keywords.is_empty());
}

#[test]
fn typed_keywords_reach_xmp_without_the_editor_prefix() {
    let temp = tempfile::tempdir().unwrap();
    let lib = Library::create(temp.path(), "Keywords", Mode::Session).unwrap();
    let source = temp.path().join("Capture/chart.jpg");
    std::fs::write(&source, b"synthetic source").unwrap();
    let asset = lib.add_asset(&source).unwrap();
    lib.save_editor_state(
        &asset.id,
        &json!({"tags":["user:beach","dog","user:dog","color:green"],"adjustments":{}}),
    )
    .unwrap();
    assert_eq!(
        lib.metadata(&asset.id).unwrap().keywords,
        vec!["beach", "dog"]
    );
    lib.sync_xmp(&asset.id).unwrap();
    let (xmp, _) = rawtonic_metadata::read_xmp(
        &std::fs::read_to_string(source.with_extension("xmp")).unwrap(),
    )
    .unwrap();
    assert_eq!(xmp.keywords, vec!["beach", "dog"]);
    // Typed keywords come back typed; automatic tags stay automatic.
    assert_eq!(
        lib.editor_state(&asset.id).unwrap().unwrap()["tags"],
        json!(["user:beach", "dog", "color:green"])
    );
    // Keywords set outside the editor (an adopted XMP) are the user's.
    let mut meta = lib.metadata(&asset.id).unwrap();
    meta.keywords.push("harbour".into());
    meta.title = "Pier".into();
    lib.set_metadata(&asset.id, &meta).unwrap();
    let state = lib.editor_state(&asset.id).unwrap().unwrap();
    assert_eq!(
        state["tags"],
        json!(["user:beach", "dog", "user:harbour", "color:green"])
    );
    // Saving the editor state keeps the title.
    lib.save_editor_state(&asset.id, &state).unwrap();
    assert_eq!(lib.metadata(&asset.id).unwrap().title, "Pier");
}

#[test]
fn large_recipe_values_are_stored_once_across_history_steps() {
    let temp = tempfile::tempdir().unwrap();
    let lib = Library::create(temp.path(), "Blobs", Mode::Session).unwrap();
    let source = temp.path().join("Capture/portrait.jpg");
    std::fs::write(&source, b"synthetic source").unwrap();
    let asset = lib.add_asset(&source).unwrap();
    // A mask bitmap / Relight analysis as the editor stores them.
    let bitmap = format!("data:image/png;base64,{}", "A".repeat(400_000));
    for exposure in 0..20 {
        lib.save_editor_state(
            &asset.id,
            &json!({"adjustments":{"exposure": exposure, "relightDepth":{"data": bitmap, "version":5}}}),
        )
        .unwrap();
    }
    // The state reads back whole.
    let state = lib.editor_state(&asset.id).unwrap().unwrap();
    assert_eq!(state["adjustments"]["relightDepth"]["data"], json!(bitmap));
    assert_eq!(state["adjustments"]["exposure"], json!(19));
    drop(lib);
    // Twenty history steps hold the bitmap once, not twenty times.
    let conn = rusqlite::Connection::open(temp.path().join("session.sqlite")).unwrap();
    let (rows, total): (i64, i64) = conn
        .query_row(
            "SELECT COUNT(*), SUM(LENGTH(payload)) FROM recipes",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert!(rows >= 20);
    assert!(total < 100_000, "history payloads {total} bytes");
    let blobs: i64 = conn
        .query_row("SELECT COUNT(*) FROM recipe_blobs", [], |r| r.get(0))
        .unwrap();
    assert_eq!(blobs, 1);
    // A reopened library reads the same state.
    let lib = Library::open(temp.path()).unwrap();
    assert_eq!(
        lib.editor_state(&asset.id).unwrap().unwrap()["adjustments"]["relightDepth"]["data"],
        json!(bitmap)
    );
}

#[test]
fn xmp_ownership_is_scoped_to_the_exact_folder_and_complete_stem() {
    let temp = tempfile::tempdir().unwrap();
    let lib = Library::create(&temp.path().join("Catalog"), "Ownership", Mode::Catalog).unwrap();
    let capture = lib.directory.join("Capture");
    std::fs::create_dir_all(capture.join("nested")).unwrap();
    let external = temp.path().join("External");
    std::fs::create_dir_all(&external).unwrap();
    let photo = capture.join("name.multi.NEF");
    for path in [
        &photo,
        &capture.join("name.multi.extra.JPG"),
        &capture.join("nested/name.multi.JPG"),
        &external.join("name.multi.JPG"),
    ] {
        std::fs::write(path, b"photograph").unwrap();
        lib.add_asset(path).unwrap();
    }
    let asset = lib.asset_at(&photo).unwrap().unwrap();
    lib.sync_xmp(&asset.id).unwrap();
    let xmp = photo.with_extension("xmp");
    let before = std::fs::read(&xmp).unwrap();
    // A real sibling, including one without an extension, must still block.
    let sibling = capture.join("name.multi.JPG");
    std::fs::write(&sibling, b"independent photograph").unwrap();
    lib.add_asset(&sibling).unwrap();
    assert!(
        lib.sync_xmp(&asset.id)
            .unwrap_err()
            .to_string()
            .contains("pair resolution")
    );
    assert_eq!(std::fs::read(&xmp).unwrap(), before);

    let bare = external.join("bare");
    let extended = external.join("bare.JPG");
    std::fs::write(&bare, b"bare").unwrap();
    std::fs::write(&extended, b"extended").unwrap();
    let bare_id = lib.add_asset(&bare).unwrap().id;
    lib.add_asset(&extended).unwrap();
    assert!(
        lib.sync_xmp(&bare_id)
            .unwrap_err()
            .to_string()
            .contains("pair resolution")
    );
    assert!(!bare.with_extension("xmp").exists());
}
