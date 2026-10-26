//! Reviewing XMP sidecars that are not in sync: pending (offline, read-only,
//! failed writes) apart from conflicts (changed outside Rawtonic), and both
//! ways of resolving a conflict.

use rawtonic_catalog::{Library, Mode, XmpOffline, XmpState};
use serde_json::json;
use std::{fs, path::PathBuf};

struct Fixture {
    _tmp: tempfile::TempDir,
    lib: Library,
    id: String,
    photo: PathBuf,
    xmp: PathBuf,
}

/// A session photograph with a recipe and a rating, its XMP in sync.
fn synced() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let lib = Library::create(tmp.path(), "Xmp", Mode::Session).unwrap();
    let photo = tmp.path().join("Capture/a.arw");
    fs::write(&photo, b"synthetic original").unwrap();
    let id = lib.add_asset(&photo).unwrap().id;
    let mut meta = lib.metadata(&id).unwrap();
    meta.rating = 2;
    meta.flag = 1;
    meta.keywords = vec!["Library".into()];
    lib.set_metadata(&id, &meta).unwrap();
    let version = lib.primary_version(&id).unwrap();
    lib.save_recipe(
        &version,
        &json!({"schema_version": 1, "adjustments": {"exposure": 0.5}, "layers": []}),
        None,
    )
    .unwrap();
    lib.sync_xmp(&id).unwrap();
    assert_eq!(lib.summary().unwrap().xmp_pending, 0);
    let xmp = photo.with_extension("xmp");
    Fixture {
        _tmp: tmp,
        lib,
        id,
        photo,
        xmp,
    }
}

fn set_rating(f: &Fixture, rating: u8) {
    let mut meta = f.lib.metadata(&f.id).unwrap();
    meta.rating = rating;
    f.lib.set_metadata(&f.id, &meta).unwrap();
}

/// What another editor writes: a rewritten packet with its own rating and
/// keywords, a property Rawtonic does not own, and no Rawtonic flag.
fn external_packet(rating: i8) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description rdf:about="" xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:other="https://example.invalid/other/" xmp:Rating="{rating}" other:Kept="yes"><dc:subject><rdf:Bag><rdf:li>External</rdf:li></rdf:Bag></dc:subject></rdf:Description></rdf:RDF></x:xmpmeta>"#
    )
}

fn state(f: &Fixture) -> Option<(XmpState, bool)> {
    f.lib
        .xmp_unsynced()
        .unwrap()
        .into_iter()
        .find(|e| e.id == f.id)
        .map(|e| (e.state, e.offline))
}

#[test]
fn an_offline_photo_stays_pending_and_nothing_is_written_beside_it() {
    let f = synced();
    let before = fs::read(&f.xmp).unwrap();
    fs::rename(&f.photo, f.photo.with_file_name("elsewhere.arw")).unwrap();
    set_rating(&f, 5);
    let error = f.lib.sync_xmp(&f.id).unwrap_err();
    assert!(error.is::<XmpOffline>(), "{error}");
    assert_eq!(
        fs::read(&f.xmp).unwrap(),
        before,
        "nothing written beside a missing photograph"
    );
    assert_eq!(state(&f), Some((XmpState::Pending, true)));
    assert!(
        f.lib
            .keep_library_xmp(&f.id)
            .unwrap_err()
            .is::<XmpOffline>()
    );
    assert!(f.lib.use_xmp(&f.id).unwrap_err().is::<XmpOffline>());
    assert_eq!(f.lib.summary().unwrap().xmp_pending, 1);
    // Back again: the pending write goes through.
    fs::rename(f.photo.with_file_name("elsewhere.arw"), &f.photo).unwrap();
    f.lib.sync_xmp(&f.id).unwrap();
    assert_eq!(f.lib.summary().unwrap().xmp_pending, 0);
    assert!(
        fs::read_to_string(&f.xmp)
            .unwrap()
            .contains(r#"xmp:Rating="5""#)
    );
}

#[cfg(unix)]
#[test]
fn a_read_only_sidecar_is_not_replaced_and_stays_pending() {
    use std::os::unix::fs::PermissionsExt;
    let f = synced();
    fs::set_permissions(&f.xmp, fs::Permissions::from_mode(0o444)).unwrap();
    let before = fs::read(&f.xmp).unwrap();
    set_rating(&f, 4);
    assert!(f.lib.sync_xmp(&f.id).is_err());
    assert_eq!(
        fs::read(&f.xmp).unwrap(),
        before,
        "a locked sidecar stays as it is"
    );
    assert_eq!(state(&f), Some((XmpState::Pending, false)));
    // Unlocked, the next sync writes it.
    fs::set_permissions(&f.xmp, fs::Permissions::from_mode(0o644)).unwrap();
    f.lib.sync_xmp(&f.id).unwrap();
    assert!(state(&f).is_none());
    assert_eq!(fs::read(&f.photo).unwrap(), b"synthetic original");
}

#[test]
fn an_external_edit_is_a_conflict_and_keep_library_writes_over_it() {
    let f = synced();
    fs::write(&f.xmp, external_packet(5)).unwrap();
    set_rating(&f, 3);
    assert!(f.lib.sync_xmp(&f.id).is_err());
    assert_eq!(state(&f), Some((XmpState::Conflict, false)));
    assert_eq!(fs::read_to_string(&f.xmp).unwrap(), external_packet(5));

    f.lib.keep_library_xmp(&f.id).unwrap();
    assert!(state(&f).is_none());
    let written = fs::read_to_string(&f.xmp).unwrap();
    assert!(written.contains(r#"xmp:Rating="3""#), "{written}");
    assert!(written.contains("Library") && !written.contains("<rdf:li>External"));
    assert!(written.contains(r#"other:Kept="yes""#), "unowned XMP kept");
    assert_eq!(f.lib.metadata(&f.id).unwrap().rating, 3);
    // In sync again: the next edit writes without a conflict.
    set_rating(&f, 1);
    f.lib.sync_xmp(&f.id).unwrap();
    assert_eq!(fs::read(&f.photo).unwrap(), b"synthetic original");
}

#[test]
fn an_external_edit_is_read_with_use_xmp_and_undone() {
    let f = synced();
    fs::write(&f.xmp, external_packet(5)).unwrap();
    set_rating(&f, 3);
    assert!(f.lib.sync_xmp(&f.id).is_err());
    let version = f.lib.primary_version(&f.id).unwrap();
    let recipe = f.lib.recipe(&version).unwrap();

    let undo = f.lib.use_xmp(&f.id).unwrap();
    let meta = f.lib.metadata(&f.id).unwrap();
    assert_eq!(meta.rating, 5);
    assert_eq!(meta.keywords, vec!["External".to_string()]);
    assert_eq!(meta.flag, 1, "a packet without a flag keeps the library's");
    assert_eq!(
        f.lib.recipe(&version).unwrap(),
        recipe,
        "the edit is the library's"
    );
    assert!(state(&f).is_none(), "in sync after reading it");
    let written = fs::read_to_string(&f.xmp).unwrap();
    assert!(written.contains(r#"xmp:Rating="5""#) && written.contains(r#"other:Kept="yes""#));
    assert!(
        written.contains("rawtonic:Recipe"),
        "the library's recipe is written back"
    );

    f.lib.undo_use_xmp(&undo).unwrap();
    let meta = f.lib.metadata(&f.id).unwrap();
    assert_eq!(
        (meta.rating, meta.keywords),
        (3, vec!["Library".to_string()])
    );
    assert_eq!(
        state(&f),
        Some((XmpState::Conflict, false)),
        "a conflict again"
    );
    assert_eq!(fs::read(&f.photo).unwrap(), b"synthetic original");
}

#[test]
fn undoing_use_xmp_leaves_a_later_edit_alone() {
    let f = synced();
    fs::write(&f.xmp, external_packet(5)).unwrap();
    assert!(f.lib.sync_xmp(&f.id).is_err());
    let undo = f.lib.use_xmp(&f.id).unwrap();
    set_rating(&f, 1);
    assert!(f.lib.undo_use_xmp(&undo).is_err());
    assert_eq!(f.lib.metadata(&f.id).unwrap().rating, 1);
}

#[test]
fn a_rejected_rating_from_another_editor_rejects() {
    let f = synced();
    fs::write(&f.xmp, external_packet(-1)).unwrap();
    assert!(f.lib.sync_xmp(&f.id).is_err());
    f.lib.use_xmp(&f.id).unwrap();
    assert_eq!(f.lib.metadata(&f.id).unwrap().flag, -1);
}

#[test]
fn a_sidecar_removed_outside_is_a_conflict_that_keep_library_writes_again() {
    let f = synced();
    fs::remove_file(&f.xmp).unwrap();
    set_rating(&f, 4);
    assert!(f.lib.sync_xmp(&f.id).is_err());
    assert_eq!(state(&f), Some((XmpState::Conflict, false)));
    assert!(f.lib.use_xmp(&f.id).is_err(), "nothing to read");
    f.lib.keep_library_xmp(&f.id).unwrap();
    assert!(
        fs::read_to_string(&f.xmp)
            .unwrap()
            .contains(r#"xmp:Rating="4""#)
    );
    assert!(state(&f).is_none());
}

#[test]
fn the_review_lists_what_the_session_bar_counts() {
    let f = synced();
    set_rating(&f, 4);
    fs::remove_file(&f.xmp).unwrap();
    assert_eq!(f.lib.xmp_unsynced().unwrap().len(), 1);
    assert_eq!(f.lib.summary().unwrap().xmp_pending, 1);
}
