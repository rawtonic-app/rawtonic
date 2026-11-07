//! Content search's index (schema 25, G37): what the background pass is
//! given to read, what it records, how far it is, and the ranking.
use rawtonic_catalog::content::{ContentStatus, DIM};
use rawtonic_catalog::{Library, Mode};
use std::{fs, path::Path};

const MODEL: &str = "vision-a";

/// A unit vector pointing mostly along axis `axis`.
fn along(axis: usize) -> Vec<f32> {
    let mut v = vec![0.01f32; DIM];
    v[axis] = 1.0;
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    v.iter().map(|x| x / norm).collect()
}

fn library(dir: &Path, names: &[&str]) -> (Library, Vec<String>) {
    let lib = Library::create(&dir.join("Job"), "Job", Mode::Session).unwrap();
    let capture = lib.directory.join("Capture");
    let ids = names
        .iter()
        .map(|name| {
            fs::write(capture.join(name), name).unwrap();
            lib.add_asset(&capture.join(name)).unwrap().id
        })
        .collect();
    (lib, ids)
}

fn names(ranked: &[(std::path::PathBuf, f32)]) -> Vec<String> {
    ranked
        .iter()
        .map(|(p, _)| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect()
}

#[test]
fn the_pass_reads_each_photograph_once_and_ranks_by_match() {
    let d = tempfile::tempdir().unwrap();
    let (lib, ids) = library(d.path(), &["a.jpg", "b.jpg", "c.jpg"]);
    assert_eq!(
        lib.content_status(MODEL).unwrap(),
        ContentStatus {
            indexed: 0,
            failed: 0,
            total: 3
        }
    );
    let work = lib.content_backlog(MODEL, 10).unwrap();
    assert_eq!(work.len(), 3);
    // Newest registered first.
    assert_eq!(work[0].id, ids[2]);
    for (i, item) in work.iter().enumerate() {
        if item.id == ids[1] {
            lib.record_content(&item.id, MODEL, &item.fingerprint, Err("unreadable"))
                .unwrap();
        } else {
            lib.record_content(&item.id, MODEL, &item.fingerprint, Ok(&along(i)))
                .unwrap();
        }
    }
    assert_eq!(lib.content_backlog_count(MODEL).unwrap(), 0);
    assert_eq!(
        lib.content_status(MODEL).unwrap(),
        ContentStatus {
            indexed: 2,
            failed: 1,
            total: 3
        }
    );
    assert!(lib.has_content(&ids[0], MODEL).unwrap());
    // c.jpg was read along axis 0, a.jpg along axis 2; the failed b.jpg is
    // not ranked.
    let ranked = lib.content_ranking(MODEL, &along(2)).unwrap();
    assert_eq!(names(&ranked), ["a.jpg", "c.jpg"]);
    assert!(ranked[0].1 > 0.95 && ranked[1].1 < 0.5, "{ranked:?}");
    assert_eq!(
        names(&lib.content_ranking(MODEL, &along(0)).unwrap()),
        ["c.jpg", "a.jpg"]
    );
    assert!(lib.content_ranking(MODEL, &[0.0; 3]).is_err());
}

#[test]
fn another_model_or_a_changed_file_is_read_again() {
    let d = tempfile::tempdir().unwrap();
    let (lib, ids) = library(d.path(), &["a.jpg", "b.jpg"]);
    for item in lib.content_backlog(MODEL, 10).unwrap() {
        lib.record_content(&item.id, MODEL, &item.fingerprint, Ok(&along(1)))
            .unwrap();
    }
    // A new model: nothing it made yet, everything to read, no ranking.
    assert_eq!(lib.content_backlog_count("vision-b").unwrap(), 2);
    assert_eq!(lib.content_status("vision-b").unwrap().indexed, 0);
    assert!(lib.content_ranking("vision-b", &along(1)).unwrap().is_empty());
    // A file changed since it was read: read again, not ranked meanwhile.
    let db = rusqlite::Connection::open(d.path().join("Job/session.sqlite")).unwrap();
    db.execute(
        "UPDATE assets SET fingerprint='xxh3:changed' WHERE id=?",
        [&ids[0]],
    )
    .unwrap();
    let work = lib.content_backlog(MODEL, 10).unwrap();
    assert_eq!(work.len(), 1);
    assert_eq!(work[0].id, ids[0]);
    assert_eq!(work[0].fingerprint, "xxh3:changed");
    assert!(!lib.has_content(&ids[0], MODEL).unwrap());
    assert_eq!(
        names(&lib.content_ranking(MODEL, &along(1)).unwrap()),
        ["b.jpg"]
    );
    lib.record_content(&ids[0], MODEL, "xxh3:changed", Ok(&along(1)))
        .unwrap();
    assert_eq!(lib.content_status(MODEL).unwrap().indexed, 2);
    // A removed photograph takes its row along; a recording for it after
    // that is ignored.
    db.execute_batch("PRAGMA foreign_keys=ON").unwrap();
    db.execute("DELETE FROM assets WHERE id=?", [&ids[1]]).unwrap();
    let rows: i64 = db
        .query_row("SELECT count(*) FROM content_embeddings", [], |r| r.get(0))
        .unwrap();
    assert_eq!(rows, 1);
    lib.record_content(&ids[1], MODEL, "x", Ok(&along(1)))
        .unwrap();
    let rows: i64 = db
        .query_row("SELECT count(*) FROM content_embeddings", [], |r| r.get(0))
        .unwrap();
    assert_eq!(rows, 1);
}

#[test]
fn a_bad_embedding_is_refused() {
    let d = tempfile::tempdir().unwrap();
    let (lib, ids) = library(d.path(), &["a.jpg"]);
    let fingerprint = lib.content_backlog(MODEL, 1).unwrap()[0].fingerprint.clone();
    assert!(lib
        .record_content(&ids[0], MODEL, &fingerprint, Ok(&[0.5; 10]))
        .is_err());
    assert!(lib
        .record_content(&ids[0], MODEL, &fingerprint, Ok(&vec![0.0; DIM]))
        .is_err());
    assert_eq!(lib.content_backlog_count(MODEL).unwrap(), 1);
}

/// A library of schema 24 opens with the index table created and its
/// photographs to read; taken back to 24 (`undo_0025.sql`) it opens again
/// the same way, so the step is safe to run twice.
#[test]
fn an_older_library_gets_the_index() {
    let d = tempfile::tempdir().unwrap();
    let (lib, ids) = library(d.path(), &["a.jpg"]);
    let fingerprint = lib.content_backlog(MODEL, 1).unwrap()[0].fingerprint.clone();
    lib.record_content(&ids[0], MODEL, &fingerprint, Ok(&along(0)))
        .unwrap();
    drop(lib);
    let db_path = d.path().join("Job/session.sqlite");
    let db = rusqlite::Connection::open(&db_path).unwrap();
    db.execute_batch(include_str!("undo_0025.sql")).unwrap();
    let version: u32 = db.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
    assert_eq!(version, 24);
    drop(db);
    let lib = Library::open(&d.path().join("Job")).unwrap();
    assert_eq!(
        lib.content_status(MODEL).unwrap(),
        ContentStatus {
            indexed: 0,
            failed: 0,
            total: 1
        }
    );
    assert_eq!(lib.content_backlog_count(MODEL).unwrap(), 1);
}
