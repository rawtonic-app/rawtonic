use rawtonic_catalog::{Library, Mode};
use serde_json::json;

#[test]
fn flags_survive_reopen_and_move_preserving_metadata_recipe_and_originals() {
    let temp = tempfile::tempdir().unwrap();
    let directory = temp.path().join("job");
    let lib = Library::create(&directory, "Culling", Mode::Session).unwrap();
    let source = directory.join("Capture/chart.arw");
    std::fs::write(&source, b"synthetic original").unwrap();
    let asset = lib.add_asset(&source).unwrap();
    let state = json!({"rating":4,"tags":["portrait","color:red"],"adjustments":{"exposure":0.7}});
    lib.save_editor_state(&asset.id, &state).unwrap();
    lib.set_flags(std::slice::from_ref(&asset.id), 1).unwrap();
    lib.sync_xmp(&asset.id).unwrap();
    let metadata = lib.metadata(&asset.id).unwrap();
    assert_eq!(metadata.flag, 1);
    assert_eq!(metadata.rating, 4);
    assert_eq!(metadata.label, "Red");
    assert_eq!(metadata.keywords, vec!["portrait"]);
    assert_eq!(
        lib.editor_state(&asset.id).unwrap().unwrap()["adjustments"]["exposure"],
        0.7
    );
    let xmp = std::fs::read_to_string(source.with_extension("xmp")).unwrap();
    assert_eq!(rawtonic_metadata::read_xmp(&xmp).unwrap().0.flag, 1);
    drop(lib);

    let moved = temp.path().join("moved");
    std::fs::rename(&directory, &moved).unwrap();
    let reopened = Library::open(&moved).unwrap();
    let moved_source = reopened.directory.join("Capture/chart.arw");
    assert_eq!(reopened.flags().unwrap().get(&moved_source), Some(&1));
    reopened
        .set_flags(std::slice::from_ref(&asset.id), -1)
        .unwrap();
    assert_eq!(reopened.flags().unwrap().get(&moved_source), Some(&-1));
    reopened.set_flags(&[asset.id], 0).unwrap();
    assert!(reopened.flags().unwrap().is_empty());
    assert_eq!(std::fs::read(moved_source).unwrap(), b"synthetic original");
}

#[test]
fn invalid_batch_rolls_back_and_catalog_flags_resolve_external_paths() {
    let temp = tempfile::tempdir().unwrap();
    let lib = Library::create(&temp.path().join("catalog"), "External", Mode::Catalog).unwrap();
    let source = temp.path().join("external.nef");
    std::fs::write(&source, b"external original").unwrap();
    let asset = lib.add_asset(&source).unwrap();
    assert!(
        lib.set_flags(&[asset.id.clone(), "unknown".into()], 1)
            .is_err()
    );
    assert!(lib.flags().unwrap().is_empty());
    assert_eq!(lib.metadata(&asset.id).unwrap().flag, 0);
    assert!(lib.set_flags(std::slice::from_ref(&asset.id), 2).is_err());
    lib.set_flags(&[asset.id], -1).unwrap();
    assert_eq!(
        lib.flags().unwrap().get(&source.canonicalize().unwrap()),
        Some(&-1)
    );
    assert_eq!(std::fs::read(source).unwrap(), b"external original");
}
