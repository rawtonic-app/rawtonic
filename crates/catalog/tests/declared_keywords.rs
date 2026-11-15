//! Keywords without photographs (G36/G37, schema 29): made in the Keyword
//! List, kept until deleted, every change returning what Undo needs.
use rawtonic_catalog::{Library, Mode};

fn library(dir: &std::path::Path) -> Library {
    Library::create(dir, "Keywords", Mode::Session).unwrap()
}

#[test]
fn a_declared_keyword_stays_until_deleted_and_is_unique_ignoring_case() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = library(tmp.path());
    let first = lib.declare_keyword(" Animals | Dogs ").unwrap();
    assert_eq!(first.added, ["Animals|Dogs"]);
    assert!(lib.declare_keyword("animals|DOGS").unwrap().is_empty());
    drop(lib);
    let lib = Library::open(tmp.path()).unwrap();
    assert_eq!(lib.declared_keywords().unwrap(), ["Animals|Dogs"]);
    let gone = lib.delete_declared_keyword("Animals|Dogs").unwrap();
    assert_eq!(gone.removed, ["Animals|Dogs"]);
    assert!(lib.declared_keywords().unwrap().is_empty());
}

#[test]
fn renaming_moves_declared_keywords_below_it_and_merges_into_declared_ones() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = library(tmp.path());
    for path in ["Animals|Dogs", "Animals|Dogs|Pugs", "Pets", "Cats"] {
        lib.declare_keyword(path).unwrap();
    }
    let change = lib
        .rename_declared_keywords("Animals|Dogs", "Pets")
        .unwrap();
    assert_eq!(change.removed, ["Animals|Dogs", "Animals|Dogs|Pugs"]);
    assert_eq!(change.added, ["Pets|Pugs"], "Pets exists: merged");
    assert_eq!(
        lib.declared_keywords().unwrap(),
        ["Cats", "Pets", "Pets|Pugs"]
    );
    // Undo is the same change the other way.
    lib.apply_declared_keywords(&change.added, &change.removed)
        .unwrap();
    assert_eq!(
        lib.declared_keywords().unwrap(),
        ["Animals|Dogs", "Animals|Dogs|Pugs", "Cats", "Pets"]
    );
}

#[test]
fn deleting_a_declared_keyword_moves_those_below_it_up() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = library(tmp.path());
    for path in ["Animals", "Animals|Dogs", "Cats"] {
        lib.declare_keyword(path).unwrap();
    }
    let change = lib.delete_declared_keyword("animals").unwrap();
    assert_eq!(change.removed, ["Animals", "Animals|Dogs"]);
    assert_eq!(change.added, ["Dogs"]);
    assert_eq!(lib.declared_keywords().unwrap(), ["Cats", "Dogs"]);
}

#[test]
fn a_schema_28_library_migrates_without_declared_keywords() {
    let tmp = tempfile::tempdir().unwrap();
    {
        let lib = library(tmp.path());
        lib.declare_keyword("Old").unwrap();
    }
    let db = rusqlite::Connection::open(tmp.path().join("session.sqlite")).unwrap();
    db.execute_batch(include_str!("undo_0029.sql")).unwrap();
    drop(db);
    let lib = Library::open(tmp.path()).unwrap();
    assert!(lib.declared_keywords().unwrap().is_empty());
}
