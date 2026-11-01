//! Reviewing XMP sidecars that are not in sync: pending (offline, read-only,
//! failed writes) apart from conflicts (changed outside Rawtonic), and both
//! ways of resolving a conflict.

use rawtonic_catalog::{Library, Mode, XmpOffline, XmpProblem};
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

/// Whether the photograph is listed, and why its XMP is not written.
fn state(f: &Fixture) -> Option<Option<XmpProblem>> {
    f.lib
        .xmp_unsynced()
        .unwrap()
        .into_iter()
        .find(|e| e.id == f.id)
        .map(|e| e.problem)
}

const PENDING: Option<Option<XmpProblem>> = Some(None);
const CONFLICT: Option<Option<XmpProblem>> = Some(Some(XmpProblem::ChangedElsewhere));

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
    assert_eq!(state(&f), Some(Some(XmpProblem::Offline)));
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
    assert_eq!(state(&f), PENDING);
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
    assert_eq!(state(&f), CONFLICT);
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
fn an_external_edit_is_read_with_use_xmp_written_back_and_undone() {
    let f = synced();
    fs::write(&f.xmp, external_packet(5)).unwrap();
    set_rating(&f, 3);
    assert!(f.lib.sync_xmp(&f.id).is_err());
    let version = f.lib.primary_version(&f.id).unwrap();
    let recipe = f.lib.recipe(&version).unwrap();

    let undo = f.lib.use_xmp(&f.id).unwrap();
    assert_eq!(state(&f), PENDING, "the recipe is still to be written");
    f.lib.sync_xmp(&f.id).unwrap();
    let meta = f.lib.metadata(&f.id).unwrap();
    assert_eq!(meta.rating, 5);
    assert_eq!(meta.keywords, vec!["External".to_string()]);
    assert_eq!(meta.flag, 1, "a packet without a flag keeps the library's");
    assert_eq!(
        f.lib.recipe(&version).unwrap(),
        recipe,
        "the edit is the library's"
    );
    assert!(state(&f).is_none(), "in sync after writing it back");
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
    assert_eq!(state(&f), CONFLICT, "a conflict again");
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
    assert_eq!(state(&f), CONFLICT);
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

#[test]
fn an_undo_never_overwrites_an_edit_from_another_connection() {
    let f = synced();
    fs::write(&f.xmp, external_packet(5)).unwrap();
    assert!(f.lib.sync_xmp(&f.id).is_err());
    let undo = f.lib.use_xmp(&f.id).unwrap();
    let other = Library::open(&f.lib.directory).unwrap();
    let mut meta = other.metadata(&f.id).unwrap();
    meta.title = "Edited in another window".into();
    other.set_metadata(&f.id, &meta).unwrap();
    assert!(f.lib.undo_use_xmp(&undo).is_err());
    assert_eq!(f.lib.metadata(&f.id).unwrap(), meta);
}

#[test]
fn keep_library_removes_iptc_the_library_has_empty_and_keeps_unmodelled_properties() {
    let f = synced();
    let mut metadata = f.lib.metadata(&f.id).unwrap();
    metadata.iptc.insert("city".into(), String::new());
    metadata.iptc.insert("creatorEmail".into(), String::new());
    f.lib.set_metadata(&f.id, &metadata).unwrap();
    f.lib.sync_xmp(&f.id).unwrap();
    // Another editor adds a city and a creator e-mail beside a property
    // Rawtonic does not model.
    let text = fs::read_to_string(&f.xmp).unwrap().replace(
        "</rdf:RDF>",
        r#"<rdf:Description rdf:about="" xmlns:photoshop="http://ns.adobe.com/photoshop/1.0/" xmlns:Iptc4xmpCore="http://iptc.org/std/Iptc4xmpCore/1.0/xmlns/" xmlns:other="https://example.invalid/other/" photoshop:City="Elsewhere" other:Kept="yes"><Iptc4xmpCore:CreatorContactInfo rdf:parseType="Resource"><Iptc4xmpCore:CiEmailWork>someone@example.invalid</Iptc4xmpCore:CiEmailWork></Iptc4xmpCore:CreatorContactInfo></rdf:Description></rdf:RDF>"#,
    );
    fs::write(&f.xmp, &text).unwrap();
    set_rating(&f, 3);
    assert!(f.lib.sync_xmp(&f.id).is_err());
    f.lib.keep_library_xmp(&f.id).unwrap();
    let written = fs::read_to_string(&f.xmp).unwrap();
    let (meta, _) = rawtonic_metadata::read_xmp(&written).unwrap();
    assert!(meta.iptc.is_empty(), "{:?}", meta.iptc);
    assert!(
        !written.contains("Elsewhere") && !written.contains("someone@"),
        "{written}"
    );
    assert!(written.contains(r#"other:Kept="yes""#), "{written}");
    assert_eq!(meta.rating, 3);
}

#[test]
fn use_xmp_takes_an_outside_rejection_of_a_rawtonic_packet() {
    let f = synced();
    // The packet Rawtonic wrote holds the pick (`rawtonic:Flag="1"`);
    // another editor changes only the rating to rejected.
    let text = fs::read_to_string(&f.xmp).unwrap();
    assert!(text.contains(r#"xmp:Rating="2""#) && text.contains(r#"rawtonic:Flag="1""#));
    fs::write(
        &f.xmp,
        text.replace(r#"xmp:Rating="2""#, r#"xmp:Rating="-1""#),
    )
    .unwrap();
    assert!(f.lib.sync_xmp(&f.id).is_err());
    f.lib.use_xmp(&f.id).unwrap();
    assert_eq!(f.lib.metadata(&f.id).unwrap().flag, -1);
    f.lib.sync_xmp(&f.id).unwrap();
    assert!(
        fs::read_to_string(&f.xmp)
            .unwrap()
            .contains(r#"rawtonic:Flag="-1""#)
    );
}

#[test]
fn use_xmp_refuses_a_sidecar_that_is_no_xmp_packet() {
    let f = synced();
    fs::write(&f.xmp, "<oops/>").unwrap();
    set_rating(&f, 3);
    assert!(f.lib.sync_xmp(&f.id).is_err());
    let before = f.lib.metadata(&f.id).unwrap();
    let listed = f.lib.xmp_unsynced().unwrap();
    assert!(f.lib.use_xmp(&f.id).is_err());
    assert_eq!(f.lib.metadata(&f.id).unwrap(), before, "metadata unchanged");
    assert_eq!(state(&f), CONFLICT);
    assert_eq!(f.lib.xmp_unsynced().unwrap().len(), listed.len());
    assert_eq!(fs::read_to_string(&f.xmp).unwrap(), "<oops/>");
}

/// A photograph registered unedited, whose edit then appeared beside it in
/// an editor sidecar (`.rrdata`) the library has not adopted, with a
/// sidecar another application wrote (a conflict).
fn unadopted_edit(external: &str) -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let lib = Library::create(tmp.path(), "Xmp", Mode::Session).unwrap();
    let photo = tmp.path().join("Capture/e.arw");
    fs::write(&photo, b"synthetic original").unwrap();
    let id = lib.add_asset(&photo).unwrap().id;
    let photo = photo.canonicalize().unwrap();
    fs::write(
        photo.with_file_name("e.arw.rrdata"),
        br#"{"version":1,"rating":3,"tags":["color:red"],"adjustments":{"exposure":1.25}}"#,
    )
    .unwrap();
    let xmp = photo.with_extension("xmp");
    fs::write(&xmp, external).unwrap();
    assert!(lib.editor_state(&id).unwrap().is_none(), "not adopted yet");
    Fixture {
        _tmp: tmp,
        lib,
        id,
        photo,
        xmp,
    }
}

fn exposure(f: &Fixture) -> Option<f64> {
    f.lib.editor_state(&f.id).unwrap()?["adjustments"]["exposure"].as_f64()
}

/// A packet that clears what the edit beside it carries: no rating, no
/// label, no keywords.
const CLEARING: &str = r#"<?xml version="1.0" encoding="UTF-8"?><x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description rdf:about="" xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmp:Rating="0"/></rdf:RDF></x:xmpmeta>"#;

#[test]
fn use_xmp_clearing_the_last_rating_keeps_the_edit_beside_the_photo() {
    let f = unadopted_edit(CLEARING);
    assert!(f.lib.sync_xmp(&f.id).is_err());
    let undo = f.lib.use_xmp(&f.id).unwrap();
    // The library holds the edit now, so the app never falls back to the
    // editor sidecar's (its rating and label included).
    assert_eq!(exposure(&f), Some(1.25));
    let state = f.lib.editor_state(&f.id).unwrap().unwrap();
    assert_eq!(state["rating"], 0);
    assert_eq!(state["tags"], json!([]));
    f.lib.undo_use_xmp(&undo).unwrap();
    let meta = f.lib.metadata(&f.id).unwrap();
    assert_eq!(
        (meta.rating, meta.label.as_str()),
        (3, "Red"),
        "back to what the photo showed"
    );
    assert_eq!(exposure(&f), Some(1.25), "the edit stays");
}

#[test]
fn every_metadata_write_adopts_an_edit_only_beside_the_photo_first() {
    let keep = unadopted_edit(&external_packet(5));
    keep.lib.keep_library_xmp(&keep.id).unwrap();
    assert_eq!(exposure(&keep), Some(1.25));
    let written = fs::read_to_string(&keep.xmp).unwrap();
    assert!(
        written.contains(r#"xmp:Rating="3""#),
        "the rating the photo showed: {written}"
    );
    assert!(written.contains("rawtonic:Recipe"));

    let flag = unadopted_edit(&external_packet(5));
    flag.lib
        .set_flags(std::slice::from_ref(&flag.id), 1)
        .unwrap();
    assert_eq!(exposure(&flag), Some(1.25));
    assert_eq!(flag.lib.metadata(&flag.id).unwrap().rating, 3);

    let title = unadopted_edit(&external_packet(5));
    title.lib.adopt_editor_sidecar(&title.id).unwrap();
    let mut meta = title.lib.metadata(&title.id).unwrap();
    meta.title = "Harbour".into();
    title.lib.set_metadata(&title.id, &meta).unwrap();
    assert_eq!(exposure(&title), Some(1.25));
    assert_eq!(title.lib.metadata(&title.id).unwrap().title, "Harbour");

    // An editor sidecar the editor cannot read shows no edit: there is no
    // look to keep, so the write goes on and the file is left as it is.
    let broken = unadopted_edit(&external_packet(5));
    let rrdata = broken.photo.with_file_name("e.arw.rrdata");
    fs::write(&rrdata, b"{not json").unwrap();
    broken
        .lib
        .set_flags(std::slice::from_ref(&broken.id), 1)
        .unwrap();
    assert_eq!(broken.lib.metadata(&broken.id).unwrap().flag, 1);
    assert_eq!(exposure(&broken), None);
    assert_eq!(fs::read(&rrdata).unwrap(), b"{not json");
    assert_eq!(fs::read(&broken.photo).unwrap(), b"synthetic original");
}

#[test]
fn uppercase_sidecar_is_discovered_adopted_checked_and_updated_in_place() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = Library::create(tmp.path(), "Uppercase", Mode::Session).unwrap();
    let photo = tmp.path().join("Capture/upper.arw");
    let upper = photo.with_extension("XMP");
    let lower = photo.with_extension("xmp");
    fs::write(&photo, b"synthetic original").unwrap();
    fs::write(&upper, external_packet(3)).unwrap();
    // This regression exercises case-sensitive filesystems specifically.
    if lower.exists() {
        return;
    }
    let contents = rawtonic_catalog::add::Contents::read_sidecars(&photo).unwrap();
    assert_eq!(contents.metadata.unwrap().rating, 3);
    let id = lib.add_asset(&photo).unwrap().id;
    assert_eq!(lib.adopt_xmp(&id).unwrap().rating, 3);
    let mut meta = lib.metadata(&id).unwrap();
    meta.rating = 5;
    lib.set_metadata(&id, &meta).unwrap();
    lib.sync_xmp(&id).unwrap();
    assert!(!lower.exists());
    assert_eq!(
        rawtonic_metadata::read_xmp(&fs::read_to_string(&upper).unwrap())
            .unwrap()
            .0
            .rating,
        5
    );
    let mut stamps = rawtonic_catalog::SidecarStamps::default();
    assert!(
        lib.check_xmp_sidecars(None, &mut stamps)
            .unwrap()
            .changed
            .is_empty()
    );
    fs::write(&upper, external_packet(2)).unwrap();
    let checked = lib.check_xmp_sidecars(None, &mut stamps).unwrap();
    assert_eq!(checked.changed.as_slice(), std::slice::from_ref(&id));
    lib.mark_changed_elsewhere(&checked.changed).unwrap();
    lib.use_xmp(&id).unwrap();
    assert_eq!(lib.metadata(&id).unwrap().rating, 2);
    // A RAW/JPEG pair still cannot own a single stem sidecar.
    let jpeg = photo.with_extension("jpg");
    fs::write(&jpeg, b"synthetic jpeg").unwrap();
    lib.add_asset(&jpeg).unwrap();
    let before = fs::read(&upper).unwrap();
    assert!(lib.sync_xmp(&id).is_err());
    assert_eq!(fs::read(&upper).unwrap(), before);
    assert!(!lower.exists());
}

#[test]
fn keep_library_preserves_unheld_real_iptc_and_applies_explicit_core_and_extension_clears() {
    for packet in [
        include_str!("../../metadata/tests/fixtures/lightroom-classic-15.6-de.xmp"),
        include_str!("../../metadata/tests/fixtures/capture-one-16.6-iptc.xmp"),
    ] {
        for extension in ["xmp", "XMP"] {
            let tmp = tempfile::tempdir().unwrap();
            let lib = Library::create(tmp.path(), "Legacy", Mode::Session).unwrap();
            let photo = tmp.path().join("Capture/legacy.arw");
            fs::write(&photo, b"synthetic original").unwrap();
            let id = lib.add_asset(&photo).unwrap().id;
            // A pre-existing catalog never read IPTC (also models an external
            // editor writing fields after the photo was first registered).
            assert!(lib.metadata(&id).unwrap().iptc.is_empty());
            let path = photo.with_extension(extension);
            fs::write(&path, packet).unwrap();
            let mut stamps = rawtonic_catalog::SidecarStamps::default();
            assert_eq!(
                lib.check_xmp_sidecars(None, &mut stamps)
                    .unwrap()
                    .changed
                    .as_slice(),
                std::slice::from_ref(&id)
            );
            lib.keep_library_xmp(&id).unwrap();
            let written = fs::read_to_string(&path).unwrap();
            let original = rawtonic_metadata::read_xmp(packet).unwrap().0;
            assert_eq!(
                rawtonic_metadata::read_xmp(&written).unwrap().0.iptc,
                original.iptc
            );
            // Core absence (City) no longer means clear. Explicit empties
            // clear both Core and Extension, and remain clear on later writes.
            let mut metadata = lib.metadata(&id).unwrap();
            metadata.iptc.insert("city".into(), String::new());
            metadata.iptc.insert("personInImage".into(), String::new());
            lib.set_metadata(&id, &metadata).unwrap();
            lib.keep_library_xmp(&id).unwrap();
            lib.sync_xmp(&id).unwrap();
            let read = rawtonic_metadata::read_xmp(&fs::read_to_string(&path).unwrap())
                .unwrap()
                .0;
            assert!(!read.iptc.contains_key("city"));
            assert!(!read.iptc.contains_key("personInImage"));
            assert_eq!(read.iptc["event"], original.iptc["event"]);
            assert_eq!(read.iptc["dateCreated"], original.iptc["dateCreated"]);
            if extension == "XMP" {
                // By name: on a case-insensitive file system (macOS) the
                // lower-case path finds the upper-case sidecar.
                let lower = photo.with_extension("xmp");
                let lower = lower.file_name().unwrap();
                assert!(
                    fs::read_dir(photo.parent().unwrap())
                        .unwrap()
                        .all(|entry| entry.unwrap().file_name() != lower)
                );
            }
            assert_eq!(fs::read(&photo).unwrap(), b"synthetic original");
        }
    }
}
