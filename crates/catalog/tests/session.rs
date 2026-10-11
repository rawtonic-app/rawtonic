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
