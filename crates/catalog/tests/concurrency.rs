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
