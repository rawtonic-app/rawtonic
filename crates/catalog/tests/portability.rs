use proptest::prelude::*;
use rawtonic_catalog::{Library, Mode, fingerprint, validate_relative};
use serde_json::json;
use std::{fs, path::Path};

#[test]
fn move_session_reopens_identical_identity_metadata_and_history() {
    let tmp = tempfile::tempdir().unwrap();
    let original = tmp.path().join("Job");
    let moved = tmp.path().join("Archived Job");
    let lib = Library::create(&original, "Acme", Mode::Session).unwrap();
    let photo = original.join("Capture/DSC001.ARW");
    fs::write(&photo, b"synthetic original fixture").unwrap();
    let asset = lib.add_asset(&photo).unwrap();
    let mut metadata = lib.metadata(&asset.id).unwrap();
    metadata.rating = 5;
    metadata.flag = 1;
    metadata.keywords = vec!["Acme|Portraits".into()];
    lib.set_metadata(&asset.id, &metadata).unwrap();
    let version = lib.primary_version(&asset.id).unwrap();
    let recipe = json!({"schema_version":1,"adjustments":{"exposure":0.5},"layers":[]});
    lib.save_recipe(&version, &recipe, None).unwrap();
    lib.sync_xmp(&asset.id).unwrap();
    lib.checkpoint().unwrap();
    drop(lib);
    fs::rename(&original, &moved).unwrap();
    let lib = Library::open(&moved).unwrap();
    assert_eq!(
        lib.asset(&asset.id).unwrap().path,
        moved.join("Capture/DSC001.ARW").canonicalize().unwrap()
    );
    assert_eq!(lib.metadata(&asset.id).unwrap(), metadata);
    assert_eq!(lib.recipe(&version).unwrap(), Some(recipe));
    assert_eq!(lib.assets(Some("Portraits")).unwrap().len(), 1);
    assert_eq!(
        fs::read(moved.join("Capture/DSC001.ARW")).unwrap(),
        b"synthetic original fixture"
    );
}

#[test]
fn rejects_escape_duplicate_create_and_unknown_schema() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("Session");
    let lib = Library::create(&dir, "job", Mode::Session).unwrap();
    assert!(Library::create(&dir, "other", Mode::Session).is_err());
    fs::write(tmp.path().join("outside.arw"), b"source").unwrap();
    assert!(lib.add_asset(&tmp.path().join("outside.arw")).is_err());
    assert!(validate_relative(Path::new("../outside.arw")).is_err());
    assert!(validate_relative(Path::new("/outside.arw")).is_err());
    let p = dir.join("session.json");
    let mut v: serde_json::Value = serde_json::from_slice(&fs::read(&p).unwrap()).unwrap();
    v["schema_version"] = json!(99);
    fs::write(p, serde_json::to_vec(&v).unwrap()).unwrap();
    assert!(Library::open(&dir).is_err());
}

#[test]
fn history_deduplicates_and_undo_restores_previous_step() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = Library::create(tmp.path(), "History", Mode::Session).unwrap();
    let p = tmp.path().join("Capture/a.jpg");
    fs::write(&p, b"fixture").unwrap();
    let a = lib.add_asset(&p).unwrap();
    let v = lib.primary_version(&a.id).unwrap();
    let one = json!({"schema_version":1,"adjustments":{"exposure":1}});
    let two = json!({"schema_version":1,"adjustments":{"exposure":2}});
    let id = lib.save_recipe(&v, &one, None).unwrap();
    assert_eq!(lib.save_recipe(&v, &one, None).unwrap(), id);
    lib.save_recipe(&v, &two, None).unwrap();
    assert!(lib.undo(&v).unwrap());
    assert_eq!(lib.recipe(&v).unwrap(), Some(one));
    assert!(!lib.undo(&v).unwrap());
}

#[test]
fn external_xmp_conflict_is_visible_and_does_not_lose_database_edits() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = Library::create(tmp.path(), "Conflict", Mode::Session).unwrap();
    let p = tmp.path().join("Capture/a.arw");
    fs::write(&p, b"fixture").unwrap();
    let a = lib.add_asset(&p).unwrap();
    lib.sync_xmp(&a.id).unwrap();
    fs::write(p.with_extension("xmp"), b"external edit").unwrap();
    let mut meta = lib.metadata(&a.id).unwrap();
    meta.rating = 4;
    lib.set_metadata(&a.id, &meta).unwrap();
    assert!(lib.sync_xmp(&a.id).is_err());
    assert_eq!(lib.metadata(&a.id).unwrap().rating, 4);
    assert_eq!(lib.summary().unwrap().xmp_pending, 1);
}

#[test]
fn catalog_can_reference_external_asset() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("external.jpg");
    fs::write(&p, b"external").unwrap();
    let lib = Library::create(&tmp.path().join("Main.catalog"), "Main", Mode::Catalog).unwrap();
    let a = lib.add_asset(&p).unwrap();
    assert_eq!(a.path, p.canonicalize().unwrap());
    fs::remove_file(p).unwrap();
    assert_eq!(lib.verify_paths().unwrap(), 1);
}

proptest! {
 #[test]
 fn fingerprint_is_stable_and_includes_size(bytes in prop::collection::vec(any::<u8>(),0..4096)) {
    let d=tempfile::tempdir().unwrap();let p=d.path().join("source");fs::write(&p,&bytes).unwrap();let a=fingerprint(&p).unwrap();prop_assert_eq!(&a,&fingerprint(&p).unwrap());let mut changed=bytes;changed.push(0);fs::write(&p,changed).unwrap();prop_assert_ne!(a,fingerprint(&p).unwrap());
 }
}

#[test]
fn unobserved_sidecar_is_preserved_until_adopted() {
    let d = tempfile::tempdir().unwrap();
    let l = Library::create(d.path(), "External", Mode::Session).unwrap();
    let p = d.path().join("Capture/a.arw");
    fs::write(&p, b"fixture").unwrap();
    let a = l.add_asset(&p).unwrap();
    let meta = rawtonic_metadata::Metadata {
        rating: 3,
        ..Default::default()
    };
    rawtonic_metadata::write_xmp(&p.with_extension("xmp"), &meta, None, None).unwrap();
    let original = fs::read(p.with_extension("xmp")).unwrap();
    assert!(l.sync_xmp(&a.id).is_err());
    assert_eq!(fs::read(p.with_extension("xmp")).unwrap(), original);
    assert_eq!(l.adopt_xmp(&a.id).unwrap().rating, 3);
    l.sync_xmp(&a.id).unwrap();
}
