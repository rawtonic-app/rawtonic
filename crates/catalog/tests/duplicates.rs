//! Find Duplicates: identical image data despite other names, folders,
//! metadata or edits; hashes recorded per file state, dropped when the file
//! changes; the "Is duplicate" rule; the check before trashing.

use rawtonic_catalog::duplicates::{DuplicateMember, Refusal, suggested_trash};
use rawtonic_catalog::query::Rule;
use rawtonic_catalog::{Library, Mode};
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

/// A JPEG with `app` as its EXIF/XMP segment and `scan` as its image data.
fn jpeg(app: &str, scan: &[u8]) -> Vec<u8> {
    let mut out = vec![0xFF, 0xD8, 0xFF, 0xE1];
    out.extend(((app.len() + 2) as u16).to_be_bytes());
    out.extend(app.as_bytes());
    out.extend([0xFF, 0xDB, 0, 4, 1, 2, 0xFF, 0xDA, 0, 3, 9]);
    out.extend(scan);
    out.extend([0xFF, 0xD9]);
    out
}

fn session(dir: &Path, files: &[(&str, Vec<u8>)]) -> (Library, Vec<String>, Vec<PathBuf>) {
    let lib = Library::create(&dir.join("Job"), "Job", Mode::Session).unwrap();
    let paths: Vec<PathBuf> = files
        .iter()
        .map(|(name, bytes)| {
            let path = lib.directory.join("Capture").join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, bytes).unwrap();
            path
        })
        .collect();
    let ids = paths
        .iter()
        .map(|path| lib.add_asset(path).unwrap().id)
        .collect();
    (lib, ids, paths)
}

fn fill(lib: &Library) -> rawtonic_catalog::duplicates::HashPass {
    lib.fill_image_hashes(2, &AtomicBool::new(false), &mut |_, _| {})
        .unwrap()
}

fn group_names(lib: &Library) -> Vec<Vec<String>> {
    lib.duplicate_groups()
        .unwrap()
        .iter()
        .map(|group| {
            let mut names: Vec<_> = group
                .iter()
                .map(|m| {
                    m.path
                        .strip_prefix(lib.directory.join("Capture"))
                        .unwrap()
                        .to_string_lossy()
                        .into_owned()
                })
                .collect();
            names.sort();
            names
        })
        .collect()
}

fn duplicate_rule() -> Rule {
    serde_json::from_value(json!({"field": "duplicate", "op": "is", "value": true})).unwrap()
}

fn names(found: Vec<PathBuf>) -> Vec<String> {
    let mut names: Vec<_> = found
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn identical_image_data_groups_whatever_the_name_folder_or_metadata() {
    let d = tempfile::tempdir().unwrap();
    let (lib, ids, _) = session(
        d.path(),
        &[
            ("A.jpg", jpeg("Exif rating 0", b"same scan")),
            ("Renamed.jpg", jpeg("Exif rating 0", b"same scan")),
            (
                "Other folder/A.jpg",
                jpeg("XMP rating 5, keywords: dog", b"same scan"),
            ),
            ("Different.jpg", jpeg("Exif rating 0", b"other scan")),
        ],
    );
    assert_eq!(lib.image_hash_backlog().unwrap(), 4);
    let pass = fill(&lib);
    assert_eq!((pass.photos, pass.hashed, pass.cancelled), (4, 4, false));
    assert!(pass.unreadable.is_empty());
    assert_eq!(
        group_names(&lib),
        [["A.jpg", "Other folder/A.jpg", "Renamed.jpg"]]
    );
    let group = &lib.duplicate_groups().unwrap()[0];
    assert!(group.iter().all(|m| m.compared == "jpeg"));
    assert_eq!(group[0].id, ids[0], "oldest import first");
    // Catalog edits (rating, keywords) change nothing.
    let mut metadata = lib.metadata(&ids[1]).unwrap();
    metadata.rating = 4;
    metadata.keywords = vec!["dog".into()];
    lib.set_metadata(&ids[1], &metadata).unwrap();
    assert_eq!(lib.image_hash_backlog().unwrap(), 0);
    let group = &lib.duplicate_groups().unwrap()[0];
    let renamed = group.iter().find(|m| m.id == ids[1]).unwrap();
    assert_eq!((renamed.rating, renamed.keywords), (4, 1));
    assert_eq!(
        names(lib.search(&duplicate_rule()).unwrap()),
        ["A.jpg", "A.jpg", "Renamed.jpg"]
    );
    let not: Rule =
        serde_json::from_value(json!({"field": "duplicate", "op": "is", "value": false})).unwrap();
    assert_eq!(names(lib.search(&not).unwrap()), ["Different.jpg"]);
    assert!(lib.rule_compares_images(&duplicate_rule()).unwrap());
    let album = lib
        .create_smart_collection("Duplicates", None, &duplicate_rule())
        .unwrap();
    assert!(lib.rule_compares_images(&Rule::album(&album.id)).unwrap());
    assert!(!lib.rule_compares_images(&Rule::text("dog")).unwrap());
    // A second pass reads nothing again.
    assert_eq!(fill(&lib).hashed, 0);
}

#[test]
fn a_changed_file_is_hashed_again_and_not_grouped_on_old_data() {
    let d = tempfile::tempdir().unwrap();
    let (lib, ids, paths) = session(
        d.path(),
        &[("A.jpg", jpeg("e", b"scan")), ("B.jpg", jpeg("e", b"scan"))],
    );
    fill(&lib);
    assert_eq!(group_names(&lib).len(), 1);
    // Another image under the same name, with another size and time.
    fs::write(&paths[1], jpeg("e", b"a new scan")).unwrap();
    let file = fs::File::options().write(true).open(&paths[1]).unwrap();
    file.set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(5))
        .unwrap();
    drop(file);
    assert!(group_names(&lib).is_empty(), "stale hash not grouped");
    assert_eq!(lib.image_hash_backlog().unwrap(), 1);
    assert_eq!(fill(&lib).hashed, 1);
    assert!(group_names(&lib).is_empty());
    let hashes = lib.image_hashes().unwrap();
    assert_ne!(hashes[&ids[0]], hashes[&ids[1]]);
    // Same size, only the time changed (same image): read again, grouped
    // again once it is.
    fs::write(&paths[1], jpeg("f", b"scan")).unwrap();
    fill(&lib);
    assert_eq!(group_names(&lib).len(), 1);
}

#[test]
fn a_new_registered_size_or_time_drops_the_hash_at_once() {
    let d = tempfile::tempdir().unwrap();
    let (lib, ids, _) = session(
        d.path(),
        &[("A.jpg", jpeg("e", b"scan")), ("B.jpg", jpeg("e", b"scan"))],
    );
    fill(&lib);
    let db = rusqlite::Connection::open(lib.directory.join("session.sqlite")).unwrap();
    db.execute("UPDATE assets SET stack=stack WHERE id=?", [&ids[0]])
        .unwrap();
    assert!(
        lib.image_hashes().unwrap()[&ids[0]].is_some(),
        "another column keeps it"
    );
    db.execute("UPDATE assets SET mtime=mtime+1 WHERE id=?", [&ids[0]])
        .unwrap();
    assert_eq!(lib.image_hashes().unwrap()[&ids[0]], None);
    assert!(lib.search(&duplicate_rule()).unwrap().is_empty());
}

#[test]
fn trashed_and_missing_photographs_are_not_duplicates() {
    let d = tempfile::tempdir().unwrap();
    let (lib, _, paths) = session(
        d.path(),
        &[
            ("A.jpg", jpeg("e", b"scan")),
            ("B.jpg", jpeg("e", b"scan")),
            ("C.jpg", jpeg("e", b"scan")),
        ],
    );
    fill(&lib);
    assert_eq!(group_names(&lib), [["A.jpg", "B.jpg", "C.jpg"]]);
    lib.set_trashed_at(&paths[2], true).unwrap();
    assert_eq!(group_names(&lib), [["A.jpg", "B.jpg"]]);
    lib.set_missing_at(&paths[1], true).unwrap();
    assert!(group_names(&lib).is_empty());
    assert!(lib.search(&duplicate_rule()).unwrap().is_empty());
    // Undo of the trash brings the pair back.
    lib.set_trashed_at(&paths[2], false).unwrap();
    assert_eq!(group_names(&lib), [["A.jpg", "C.jpg"]]);
}

#[test]
fn trashing_is_confirmed_against_the_files_as_they_are_now() {
    let d = tempfile::tempdir().unwrap();
    let (lib, _, paths) = session(
        d.path(),
        &[
            ("A.jpg", jpeg("e", b"scan")),
            ("B.jpg", jpeg("e", b"scan")),
            ("C.jpg", jpeg("e", b"scan")),
        ],
    );
    fill(&lib);
    let keep = [paths[0].clone()];
    assert!(
        lib.confirm_duplicates(&keep, &paths[1..])
            .unwrap()
            .is_empty()
    );
    // C now holds another image: it is not trashed as a duplicate.
    fs::write(&paths[2], jpeg("e", b"edited elsewhere")).unwrap();
    assert_eq!(
        lib.confirm_duplicates(&keep, &paths[1..]).unwrap(),
        [(paths[2].clone(), Refusal::Changed)]
    );
    // The kept photograph is gone: nothing of its group is a duplicate.
    fs::remove_file(&paths[0]).unwrap();
    assert_eq!(
        lib.confirm_duplicates(&keep, &paths[1..2]).unwrap(),
        [(paths[1].clone(), Refusal::Changed)]
    );
}

/// A photograph another library owns (a project's file the catalog added
/// where it is) is listed with that library's name and refused for the
/// Trash; a file the catalog refers to in the user's own folder is told
/// apart from one inside the catalog's folder.
#[test]
fn a_photograph_another_library_owns_is_named_and_never_trashed() {
    let d = tempfile::tempdir().unwrap();
    let root = d.path().canonicalize().unwrap();
    let scan = jpeg("e", b"scan");
    let project = Library::create(&root.join("PROJ"), "Wedding Smith", Mode::Session).unwrap();
    let theirs = project.directory.join("Capture/a.jpg");
    fs::write(&theirs, &scan).unwrap();
    project.add_asset(&theirs).unwrap();
    let catalog = Library::create(&root.join("CAT"), "Main", Mode::Catalog).unwrap();
    let own = catalog.directory.join("Photos/a.jpg");
    let users = root.join("Shoot/a.jpg");
    for path in [&own, &users] {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, &scan).unwrap();
    }
    let own_id = catalog.add_asset(&own).unwrap().id;
    catalog.add_asset(&users).unwrap();
    catalog.add_asset(&theirs).unwrap();
    let album = catalog.create_collection("Picks", None).unwrap();
    catalog.add_to_collection(&album.id, &[own_id]).unwrap();
    fill(&catalog);

    let groups = catalog.duplicate_groups().unwrap();
    assert_eq!(groups.len(), 1);
    let at = |path: &PathBuf| groups[0].iter().find(|m| m.path == *path).unwrap();
    let owner = at(&theirs)
        .owner
        .as_ref()
        .expect("the project owns its file");
    assert_eq!(
        (owner.name.as_str(), owner.directory.clone()),
        ("Wedding Smith", project.directory.clone())
    );
    assert!(!at(&theirs).own_folder);
    assert_eq!(
        (at(&own).owner.clone(), at(&own).own_folder, at(&own).albums),
        (None, true, 1)
    );
    assert_eq!(
        (at(&users).owner.clone(), at(&users).own_folder),
        (None, false)
    );

    // Even marked by hand, the project's file stays.
    assert_eq!(
        catalog
            .confirm_duplicates(std::slice::from_ref(&own), std::slice::from_ref(&theirs))
            .unwrap(),
        [(theirs.clone(), Refusal::Owned)]
    );
    // The project's own copy, seen from the project, is its own.
    assert!(project.owner_elsewhere(&theirs).is_none());
}

fn member(name: &str, own_folder: bool) -> DuplicateMember {
    DuplicateMember {
        id: name.into(),
        path: PathBuf::from(name),
        size: 1,
        captured: None,
        imported: None,
        rating: 0,
        flag: 0,
        label: String::new(),
        keywords: 0,
        albums: 0,
        copies: 0,
        described: false,
        companion: None,
        companion_image: None,
        compared: "jpeg".into(),
        owner: None,
        own_folder,
    }
}

/// The copy with the most work stays, whatever its import order; the others
/// are suggested only inside the library's own folder.
#[test]
fn the_suggestion_keeps_the_copy_with_the_most_work() {
    let rated_in_album = DuplicateMember {
        rating: 3,
        albums: 1,
        ..member("rated", true)
    };
    let group = [
        member("oldest", true),
        rated_in_album,
        member("plain", true),
    ];
    assert_eq!(suggested_trash(&group, &[false; 3]), [true, false, true]);
    // An edit and a keyword outweigh a rating alone.
    let keyworded = DuplicateMember {
        keywords: 1,
        ..member("keyworded", true)
    };
    let rated = DuplicateMember {
        rating: 5,
        ..member("rated", true)
    };
    assert_eq!(
        suggested_trash(&[rated, keyworded], &[false, true]),
        [true, false]
    );
}

/// On a tie the oldest import stays; a file in the user's own folder is
/// never pre-marked, even when another copy is kept.
#[test]
fn on_a_tie_the_oldest_import_stays_and_the_users_own_folders_are_not_marked() {
    let group = [
        member("oldest", true),
        member("in the catalog", true),
        member("in the user's folder", false),
    ];
    assert_eq!(suggested_trash(&group, &[false; 3]), [false, true, false]);
    // The oldest is in the user's folder: it stays, the catalog's copy goes.
    let group = [member("user's", false), member("catalog's", true)];
    assert_eq!(suggested_trash(&group, &[false; 2]), [false, true]);
    // Only files outside: nothing is marked.
    let group = [member("a", false), member("b", false)];
    assert_eq!(suggested_trash(&group, &[false; 2]), [false, false]);
}

/// Another library's photograph is never marked and not weighed: the copy
/// with the most work among the rest stays.
#[test]
fn another_librarys_photograph_is_never_suggested() {
    let owner = Some(rawtonic_catalog::Owner {
        directory: PathBuf::from("/PROJ"),
        name: "Wedding Smith".into(),
    });
    let theirs = DuplicateMember {
        owner: owner.clone(),
        rating: 5,
        ..member("theirs", true)
    };
    let group = [theirs.clone(), member("ours", true), member("ours 2", true)];
    assert_eq!(
        suggested_trash(&group, &[true, false, false]),
        [false, false, true]
    );
    // Only owned elsewhere and one more: that one stays.
    assert_eq!(
        suggested_trash(&[theirs.clone(), member("ours", true)], &[false; 2]),
        [false, false]
    );
    assert_eq!(
        suggested_trash(&[theirs.clone(), theirs], &[false; 2]),
        [false, false]
    );
}

/// A RAW+JPEG pair goes to the Trash as one: the RAW may go only while a
/// kept photograph's companion holds the same JPEG image data.
#[test]
fn a_companion_jpeg_without_a_kept_copy_keeps_its_raw() {
    let d = tempfile::tempdir().unwrap();
    let lib = Library::create(&d.path().join("Job"), "Job", Mode::Session).unwrap();
    let capture = lib.directory.join("Capture");
    for name in ["A.RAF", "B.RAF", "C.RAF"] {
        fs::write(capture.join(name), jpeg("raw facts", b"sensor")).unwrap();
    }
    fs::write(capture.join("B.JPG"), jpeg("exif", b"camera jpeg")).unwrap();
    fs::write(capture.join("C.JPG"), jpeg("other exif", b"camera jpeg")).unwrap();
    lib.add_asset(&capture.join("A.RAF")).unwrap();
    for name in ["B", "C"] {
        let planned = lib
            .plan_add(&capture.join(format!("{name}.RAF")), Default::default())
            .unwrap()
            .with_companion(&format!("{name}.JPG"))
            .unwrap();
        lib.insert_planned(vec![planned]).unwrap();
    }
    fill(&lib);
    let group = &lib.duplicate_groups().unwrap()[0];
    let companions: Vec<_> = group.iter().map(|m| m.companion.is_some()).collect();
    assert_eq!(companions, [false, true, true]);
    let [a, b, c] = ["A", "B", "C"].map(|n| capture.join(format!("{n}.RAF")));
    // Only A (without a companion) is kept: B's JPEG has no kept copy.
    assert_eq!(
        lib.confirm_duplicates(std::slice::from_ref(&a), std::slice::from_ref(&b))
            .unwrap(),
        [(b.clone(), Refusal::Companion)]
    );
    // C keeps a companion with the same image data: B (and its JPEG) may go.
    assert!(
        lib.confirm_duplicates(&[a, c], std::slice::from_ref(&b))
            .unwrap()
            .is_empty()
    );
}

/// The suggestion never marks a RAW+JPEG pair whose JPEG has no kept copy:
/// accepting it as it comes passes the check before trashing.
#[test]
fn the_suggestion_passes_the_check_with_raw_jpeg_pairs() {
    let d = tempfile::tempdir().unwrap();
    let lib = Library::create(&d.path().join("Job"), "Job", Mode::Session).unwrap();
    let capture = lib.directory.join("Capture");
    for name in ["A.RAF", "B.RAF", "C.RAF"] {
        fs::write(capture.join(name), jpeg("raw facts", b"sensor")).unwrap();
    }
    for name in ["B.JPG", "C.JPG"] {
        fs::write(capture.join(name), jpeg("exif", b"camera jpeg")).unwrap();
    }
    let a = lib.add_asset(&capture.join("A.RAF")).unwrap().id;
    let mut ids = vec![a.clone()];
    for name in ["B", "C"] {
        let planned = lib
            .plan_add(&capture.join(format!("{name}.RAF")), Default::default())
            .unwrap()
            .with_companion(&format!("{name}.JPG"))
            .unwrap();
        ids.push(lib.insert_planned(vec![planned]).unwrap().pop().unwrap().0);
    }
    let rate = |id: &str| {
        lib.set_metadata(
            id,
            &rawtonic_metadata::Metadata {
                rating: 3,
                ..Default::default()
            },
        )
        .unwrap()
    };
    let accept = |lib: &Library| {
        fill(lib);
        let group = &lib.duplicate_groups().unwrap()[0];
        let marked = suggested_trash(group, &vec![false; group.len()]);
        let (trash, keep): (Vec<_>, Vec<_>) =
            group.iter().zip(&marked).partition(|(_, marked)| **marked);
        let paths = |list: Vec<(&DuplicateMember, &bool)>| -> Vec<PathBuf> {
            list.into_iter().map(|(m, _)| m.path.clone()).collect()
        };
        let (trash, keep) = (paths(trash), paths(keep));
        assert!(lib.confirm_duplicates(&keep, &trash).unwrap().is_empty());
        names(trash)
    };
    // The RAW alone is rated: it stays, and the pairs stay with it.
    rate(&a);
    assert!(accept(&lib).is_empty());
    // A pair is rated: the other pair goes (their JPEGs are identical), the
    // RAW alone too.
    rate(&ids[1]);
    lib.set_metadata(&a, &Default::default()).unwrap();
    assert_eq!(accept(&lib), ["A.RAF", "C.RAF"]);
    // C's JPEG is another image (reprocessed): C stays with it.
    fs::write(capture.join("C.JPG"), jpeg("exif", b"reprocessed jpeg")).unwrap();
    assert_eq!(accept(&lib), ["A.RAF"]);
}

#[test]
fn text_metadata_is_reported_with_the_photograph() {
    let d = tempfile::tempdir().unwrap();
    let (lib, ids, _) = session(
        d.path(),
        &[("A.jpg", jpeg("e", b"scan")), ("B.jpg", jpeg("e", b"scan"))],
    );
    let mut metadata = lib.metadata(&ids[1]).unwrap();
    metadata.description = "Bride and groom, first dance".into();
    lib.set_metadata(&ids[1], &metadata).unwrap();
    fill(&lib);
    let described: Vec<_> = lib.duplicate_groups().unwrap()[0]
        .iter()
        .map(|m| (m.id.clone(), m.described))
        .collect();
    assert_eq!(described, [(ids[0].clone(), false), (ids[1].clone(), true)]);
}

#[test]
fn a_cancelled_pass_keeps_what_it_recorded() {
    let d = tempfile::tempdir().unwrap();
    let (lib, _, _) = session(
        d.path(),
        &[("A.jpg", jpeg("e", b"scan")), ("B.jpg", jpeg("e", b"scan"))],
    );
    let pass = lib
        .fill_image_hashes(1, &AtomicBool::new(true), &mut |_, _| {})
        .unwrap();
    assert!(pass.cancelled);
    assert_eq!(pass.hashed, 0);
    assert_eq!(lib.image_hash_backlog().unwrap(), 2);
}

#[test]
fn opening_a_schema_19_library_adds_the_image_hashes() {
    let d = tempfile::tempdir().unwrap();
    let (lib, ids, _) = session(
        d.path(),
        &[("A.jpg", jpeg("e", b"scan")), ("B.jpg", jpeg("e", b"scan"))],
    );
    drop(lib);
    let db = rusqlite::Connection::open(d.path().join("Job/session.sqlite")).unwrap();
    db.execute_batch(include_str!("undo_0020.sql")).unwrap();
    drop(db);
    let lib = Library::open(&d.path().join("Job")).unwrap();
    assert_eq!(lib.image_hashes().unwrap()[&ids[0]], None);
    assert_eq!(fill(&lib).hashed, 2);
    assert_eq!(group_names(&lib), [["A.jpg", "B.jpg"]]);
}
