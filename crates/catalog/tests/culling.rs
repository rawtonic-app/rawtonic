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
    lib.set_flags(&[asset.id.clone(), asset.id], -1).unwrap();
    assert_eq!(
        lib.flags().unwrap().get(&source.canonicalize().unwrap()),
        Some(&-1)
    );
    assert_eq!(std::fs::read(source).unwrap(), b"external original");
}

#[test]
fn mixed_flag_batch_undo_restores_every_previous_value_and_marks_xmp_pending() {
    let temp = tempfile::tempdir().unwrap();
    let lib = Library::create(temp.path(), "Undo", Mode::Session).unwrap();
    let mut ids = Vec::new();
    for (n, flag) in [0, 1, -1].into_iter().enumerate() {
        let path = temp.path().join(format!("Capture/{n}.nef"));
        std::fs::write(&path, b"synthetic original").unwrap();
        let id = lib.add_asset(&path).unwrap().id;
        lib.set_flags(std::slice::from_ref(&id), flag).unwrap();
        lib.sync_xmp(&id).unwrap();
        ids.push(id);
    }
    let values: Vec<_> = ids.iter().map(|id| (id.clone(), 1)).collect();
    let before = lib.set_flag_values(&values).unwrap();
    assert_eq!(
        before.iter().map(|(_, flag)| *flag).collect::<Vec<_>>(),
        [0, 1, -1]
    );
    for id in &ids {
        lib.sync_xmp(id).unwrap();
    }
    let redo = lib.set_flag_values(&before).unwrap();
    assert_eq!(lib.summary().unwrap().xmp_pending, 3);
    for (id, flag) in &before {
        assert_eq!(lib.metadata(id).unwrap().flag, *flag);
        lib.sync_xmp(id).unwrap();
    }
    lib.set_flag_values(&redo).unwrap();
    assert!(ids.iter().all(|id| lib.metadata(id).unwrap().flag == 1));
    // Invalid restores commit nothing, including when an earlier member was valid.
    assert!(
        lib.set_flag_values(&[(ids[0].clone(), -1), ("unknown".into(), 0)])
            .is_err()
    );
    assert_eq!(lib.metadata(&ids[0]).unwrap().flag, 1);
}
