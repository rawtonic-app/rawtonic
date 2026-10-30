use rawtonic_catalog::{Library, Mode};
use serde_json::json;
use std::sync::{Arc, Barrier, mpsc};
use std::time::{Duration, Instant};

#[test]
fn opening_an_existing_library_enables_wal_without_losing_its_photographs() {
    let temp = tempfile::tempdir().unwrap();
    let photo = temp.path().join("Capture/frame.jpg");
    let id = {
        let lib = Library::create(temp.path(), "Existing", Mode::Session).unwrap();
        std::fs::write(&photo, b"synthetic source").unwrap();
        lib.add_asset(&photo).unwrap().id
    };
    let db = temp.path().join("session.sqlite");
    {
        let conn = rusqlite::Connection::open(&db).unwrap();
        let mode: String = conn
            .query_row("PRAGMA journal_mode=DELETE", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "delete");
    }
    let lib = Library::open(temp.path()).unwrap();
    assert_eq!(lib.asset_at(&photo).unwrap().unwrap().id, id);
    let conn = rusqlite::Connection::open(&db).unwrap();
    let mode: String = conn
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
}

/// The editor opens a short-lived connection per sidecar read, from the main
/// thread and from every thumbnail worker at once. SQLite 3.51.1 could
/// deadlock when one thread closed the last connection of a WAL database
/// (unixLock took the inode mutex, then the global VFS mutex) while another
/// closed its handle (global mutex, then inode mutex); the app hung at
/// startup. Many threads opening, reading and closing must keep finishing.
#[test]
fn concurrent_short_lived_connections_never_deadlock() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("Capture/frame.jpg");
    {
        let lib = Library::create(temp.path(), "Concurrency", Mode::Session).unwrap();
        std::fs::write(&source, b"synthetic source").unwrap();
        let asset = lib.add_asset(&source).unwrap();
        lib.save_editor_state(&asset.id, &json!({"rating": 2, "adjustments": {}}))
            .unwrap();
    }
    // Threads close their connections together, so that the last close of
    // the WAL database races with the others.
    const THREADS: usize = 4;
    const ROUNDS: u32 = 1500;
    let (done, finished) = mpsc::channel();
    let barrier = Arc::new(Barrier::new(THREADS));
    for _ in 0..THREADS {
        let directory = temp.path().to_path_buf();
        let source = source.clone();
        let done = done.clone();
        let barrier = barrier.clone();
        std::thread::spawn(move || {
            for _ in 0..ROUNDS {
                let lib = Library::open(&directory).unwrap();
                let asset = lib.asset_at(&source).unwrap().unwrap();
                assert!(lib.editor_state(&asset.id).unwrap().is_some());
                barrier.wait();
                drop(lib);
            }
            done.send(ROUNDS).unwrap();
        });
    }
    drop(done);
    let mut total = 0;
    let started = Instant::now();
    for _ in 0..THREADS {
        total += finished
            .recv_timeout(Duration::from_secs(30))
            .expect("a connection thread hung (SQLite VFS deadlock)");
    }
    assert_eq!(total, THREADS as u32 * ROUNDS);
    assert!(started.elapsed() < Duration::from_secs(60));
}

/// Exercise the Windows VFS with literal path components and a path longer
/// than MAX_PATH. These names must not be rewritten to avoid WAL contention.
#[cfg(windows)]
#[test]
fn concurrent_wal_reads_preserve_long_verbatim_and_literal_paths() {
    let temp = tempfile::tempdir().unwrap();
    let mut directory = temp.path().canonicalize().unwrap();
    for _ in 0..5 {
        directory.push("a_long_directory_component_for_the_wal_path_regression");
    }
    std::fs::create_dir_all(&directory).unwrap();
    let ordinary = directory.join("literal");
    let literal = directory.join("literal. ");
    std::fs::create_dir(&ordinary).unwrap();
    std::fs::create_dir(&literal).unwrap();
    let db = literal.join("database.sqlite");
    {
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE t(x); INSERT INTO t VALUES(42);")
            .unwrap();
    }
    let (done, finished) = mpsc::channel();
    let barrier = Arc::new(Barrier::new(4));
    for _ in 0..4 {
        let db = db.clone();
        let done = done.clone();
        let barrier = barrier.clone();
        std::thread::spawn(move || {
            for _ in 0..1500 {
                let conn = rusqlite::Connection::open(&db).unwrap();
                conn.busy_timeout(Duration::from_secs(5)).unwrap();
                let value: i64 = conn
                    .query_row("SELECT x FROM t", [], |row| row.get(0))
                    .unwrap();
                assert_eq!(value, 42);
                barrier.wait();
                drop(conn);
            }
            done.send(()).unwrap();
        });
    }
    drop(done);
    for _ in 0..4 {
        finished.recv_timeout(Duration::from_secs(30)).unwrap();
    }
    assert!(!ordinary.join("database.sqlite").exists());
    let conn = rusqlite::Connection::open(&db).unwrap();
    let integrity: String = conn
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .unwrap();
    assert_eq!(integrity, "ok");
}
