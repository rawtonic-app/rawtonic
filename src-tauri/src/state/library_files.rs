//! The app's library files, albums.json and presets.json: one store per file,
//! read once and changed under one write lock with rawtonic_fs::atomic_write.
//! An unreadable file is never replaced; the next access retries the read.
//!
//! LibraryQueue orders the frontend's changes (the page sends one at a time)
//! and records pending work and the last failure per file. Imports parse and
//! merge in their one change. A flush waits only for real changes received
//! before it, at most ten seconds, and reports failures and unfinished work.
//! The queue lock and a store's locks are never held together. File operations
//! update albums directly under AlbumsStore's write lock; album commands send
//! individual edits so those updates survive.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, RwLock};
use std::time::{Duration, Instant};

use tauri::AppHandle;

use crate::file_management::{AlbumItem, PresetItem, sort_album_tree};
use crate::state::lock::{lock_recover, read_recover, write_recover};

pub(crate) const FLUSH_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum LibraryFile {
    Albums,
    Presets,
}

#[derive(Default)]
pub struct LibraryQueue {
    state: Mutex<QueueState>,
    changed: Condvar,
}

#[derive(Default)]
struct QueueState {
    next: u64,
    pending: BTreeMap<u64, String>,
    failures: BTreeMap<LibraryFile, (u64, String)>,
}

impl LibraryQueue {
    fn lock(&self) -> MutexGuard<'_, QueueState> {
        lock_recover(&self.state)
    }

    fn record(&self, file: LibraryFile, describe: &str) -> Change<'_> {
        let mut state = self.lock();
        let id = state.next;
        state.next += 1;
        state.pending.insert(id, describe.to_owned());
        Change {
            queue: self,
            id,
            file,
            finished: false,
        }
    }
}

/// Holds a real pending change until it finishes, including unwinding. A hung
/// predecessor keeps later changes ordered; it cannot hold up a quit flush
/// beyond its deadline.
struct Change<'q> {
    queue: &'q LibraryQueue,
    id: u64,
    file: LibraryFile,
    finished: bool,
}

impl Change<'_> {
    fn wait(&self) {
        let mut state = self.queue.lock();
        while state.pending.range(..self.id).next().is_some() {
            state = self
                .queue
                .changed
                .wait(state)
                .unwrap_or_else(|e| e.into_inner());
        }
    }

    fn finish(&mut self, error: Option<String>) {
        let mut state = self.queue.lock();
        let describe = state
            .pending
            .remove(&self.id)
            .expect("pending library change");
        if let Some(error) = error {
            state
                .failures
                .insert(self.file, (self.id, format!("{describe}: {error}")));
        } else {
            state.failures.remove(&self.file);
        }
        self.finished = true;
        drop(state);
        self.queue.changed.notify_all();
    }
}

impl Drop for Change<'_> {
    fn drop(&mut self) {
        if !self.finished {
            self.finish(Some("change did not finish".into()));
        }
    }
}

/// One ordered change and its outcome. A later success clears an older
/// failure of the same file, without hiding the other file's failure.
pub(crate) fn run_change<T>(
    queue: &LibraryQueue,
    file: LibraryFile,
    describe: &str,
    work: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    let mut change = queue.record(file, describe);
    change.wait();
    let result = work();
    change.finish(result.as_ref().err().cloned());
    result
}

/// Snapshot the received changes, wait for those still pending, then report
/// the failures once. A flush has no entry and cannot wait on itself or on
/// another flush. Timed-out work stays queued and can finish normally later.
pub(crate) fn flush_library(queue: &LibraryQueue, timeout: Duration) -> Vec<String> {
    let deadline = Instant::now() + timeout;
    let mut state = queue.lock();
    let until = state.next;
    while state.pending.range(..until).next().is_some() {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        state = queue
            .changed
            .wait_timeout(state, left)
            .map(|(state, _)| state)
            .unwrap_or_else(|e| e.into_inner().0);
    }
    let mut failures = Vec::new();
    state.failures.retain(|_, (id, error)| {
        if *id < until {
            failures.push((*id, error.clone()));
            false
        } else {
            true
        }
    });
    failures.extend(
        state
            .pending
            .range(..until)
            .map(|(id, describe)| (*id, format!("{describe} (still running; not written yet)"))),
    );
    failures.sort_by_key(|(id, _)| *id);
    failures.into_iter().map(|(_, error)| error).collect()
}

struct JsonFileStore<T> {
    /// `None` until the file was read (or found missing); a read that failed
    /// is tried again on the next access.
    current: RwLock<Option<Arc<Vec<T>>>>,
    /// Held for a whole load or read-modify-write (lock order: `writer`,
    /// then `current`).
    writer: Mutex<()>,
}

impl<T> JsonFileStore<T> {
    /// A store that has read nothing yet: the app's, and a test's own.
    pub fn new() -> Self {
        Self::default()
    }
}

impl<T> Default for JsonFileStore<T> {
    fn default() -> Self {
        Self {
            current: RwLock::new(None),
            writer: Mutex::new(()),
        }
    }
}

/// The app's album list (`albums.json`), kept sorted as it is saved.
pub struct AlbumsStore(JsonFileStore<AlbumItem>);

/// The user's presets (`presets.json`).
pub struct PresetsStore(JsonFileStore<PresetItem>);

impl Default for AlbumsStore {
    fn default() -> Self {
        Self(JsonFileStore::new())
    }
}

impl Default for PresetsStore {
    fn default() -> Self {
        Self(JsonFileStore::new())
    }
}

macro_rules! json_file_store {
    ($store:ty, $path:path, $body:ty, $read:ident, $sort:ident) => {
        impl $store {
            /// The stored items, read from the file on the first call of the
            /// run (empty when it is not there). `parse` is how the file's
            /// text is read, `sort` how it is kept.
            pub fn get(&self, app_handle: &AppHandle) -> Result<Arc<Vec<$body>>, String> {
                self.0
                    .get(|| $path(app_handle), $read, &|items| $sort(items))
            }

            /// Applies `change` to the stored items and saves the result, all
            /// under the write lock, so concurrent changes never lose one
            /// another. `change` returns what to save, or `None` to save
            /// nothing. On an error neither the file nor the items change.
            pub fn update<R>(
                &self,
                app_handle: &AppHandle,
                change: impl FnOnce(&mut Vec<$body>) -> Option<R>,
            ) -> Result<Option<R>, String> {
                self.0
                    .update(|| $path(app_handle), $read, &|items| $sort(items), change)
            }
        }
    };
}

fn read_albums(text: &str) -> Result<Vec<AlbumItem>, String> {
    serde_json::from_str(text).map_err(|e| e.to_string())
}

fn read_presets(text: &str) -> Result<Vec<PresetItem>, String> {
    serde_json::from_str(text).map_err(|e| e.to_string())
}

fn no_order<T>(_: &mut Vec<T>) {}

json_file_store!(
    AlbumsStore,
    crate::file_management::get_albums_path,
    AlbumItem,
    read_albums,
    sort_album_tree
);
json_file_store!(
    PresetsStore,
    crate::file_management::get_presets_path,
    PresetItem,
    read_presets,
    no_order
);

impl<T: serde::Serialize + Clone> JsonFileStore<T> {
    /// Call with no lock held.
    fn get(
        &self,
        path: impl FnOnce() -> Result<PathBuf, String>,
        parse: impl FnOnce(&str) -> Result<Vec<T>, String>,
        sort: &dyn Fn(&mut Vec<T>),
    ) -> Result<Arc<Vec<T>>, String> {
        if let Some(items) = read_recover(&self.current).as_ref() {
            return Ok(items.clone());
        }
        let _writer = lock_recover(&self.writer);
        self.loaded(&path()?, parse, sort)
    }

    /// Call with no lock held; `change` runs under the write lock.
    fn update<R>(
        &self,
        path: impl FnOnce() -> Result<PathBuf, String>,
        parse: impl FnOnce(&str) -> Result<Vec<T>, String>,
        sort: &dyn Fn(&mut Vec<T>),
        change: impl FnOnce(&mut Vec<T>) -> Option<R>,
    ) -> Result<Option<R>, String> {
        let _writer = lock_recover(&self.writer);
        let path = path()?;
        let mut next = (*self.loaded(&path, parse, sort)?).clone();
        let Some(result) = change(&mut next) else {
            return Ok(None);
        };
        sort(&mut next);
        let json = serde_json::to_string_pretty(&next).map_err(|e| e.to_string())?;
        rawtonic_fs::atomic_write(&path, json).map_err(|e| e.to_string())?;
        *write_recover(&self.current) = Some(Arc::new(next));
        Ok(Some(result))
    }

    /// Call with `writer` held.
    fn loaded(
        &self,
        path: &Path,
        parse: impl FnOnce(&str) -> Result<Vec<T>, String>,
        sort: &dyn Fn(&mut Vec<T>),
    ) -> Result<Arc<Vec<T>>, String> {
        if let Some(items) = read_recover(&self.current).as_ref() {
            return Ok(items.clone());
        }
        let mut items = if path.exists() {
            parse(&fs::read_to_string(path).map_err(|e| e.to_string())?)
        } else {
            Ok(Vec::new())
        }?;
        sort(&mut items);
        let items = Arc::new(items);
        *write_recover(&self.current) = Some(items.clone());
        Ok(items)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::de::DeserializeOwned;
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread;

    fn at(path: &Path) -> impl FnOnce() -> Result<PathBuf, String> + '_ {
        || Ok(path.to_path_buf())
    }

    fn read<T: DeserializeOwned>(path: &Path) -> Vec<T> {
        serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
    }

    fn albums(names: &[&str]) -> Vec<AlbumItem> {
        names
            .iter()
            .map(|name| AlbumItem::Album {
                id: name.to_lowercase(),
                name: name.to_string(),
                icon: None,
                images: vec![format!("/photos/{name}.dng")],
            })
            .collect()
    }

    fn presets(ids: &[&str]) -> Vec<PresetItem> {
        ids.iter()
            .map(|id| {
                PresetItem::Preset(crate::file_management::Preset {
                    id: id.to_string(),
                    name: id.to_string(),
                    adjustments: serde_json::json!({ "exposure": 1 }),
                    include_masks: None,
                    include_crop_transform: None,
                    preset_type: None,
                    tool_keys: None,
                })
            })
            .collect()
    }

    fn album_names(items: &[AlbumItem]) -> Vec<String> {
        items
            .iter()
            .map(|item| match item {
                AlbumItem::Album { name, .. } => name.clone(),
                AlbumItem::Group { name, .. } => name.clone(),
            })
            .collect()
    }

    fn preset_ids(items: &[PresetItem]) -> Vec<String> {
        items
            .iter()
            .map(|item| match item {
                PresetItem::Preset(p) => p.id.clone(),
                PresetItem::Folder(f) => f.id.clone(),
            })
            .collect()
    }

    #[allow(clippy::ptr_arg)]
    fn sort(items: &mut Vec<AlbumItem>) {
        sort_album_tree(items);
    }

    #[test]
    fn a_save_then_a_change_to_one_album_keep_both() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("albums.json");
        fs::write(&path, serde_json::to_string(&albums(&["Shared"])).unwrap()).unwrap();
        let store = JsonFileStore::<AlbumItem>::new();
        let mut renamed = albums(&["Shared"]);
        if let AlbumItem::Album { name, .. } = &mut renamed[0] {
            name.push_str(" (renamed)");
        }
        store
            .update(at(&path), read_albums, &sort, move |items| {
                *items = renamed;
                Some(())
            })
            .unwrap();
        store
            .update(at(&path), read_albums, &sort, |items| {
                items.extend(albums(&["Added"]));
                Some(())
            })
            .unwrap();
        assert_eq!(
            album_names(&read::<AlbumItem>(&path)),
            ["Added", "Shared (renamed)"]
        );
        assert_eq!(
            album_names(&store.get(at(&path), read_albums, &sort).unwrap()),
            ["Added", "Shared (renamed)"]
        );
    }

    #[test]
    fn changes_from_many_threads_lose_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("presets.json");
        fs::write(&path, serde_json::to_string(&presets(&["base"])).unwrap()).unwrap();
        let store = Arc::new(JsonFileStore::<PresetItem>::new());
        let threads: Vec<_> = (0..8)
            .map(|t| {
                let (store, path) = (store.clone(), path.clone());
                thread::spawn(move || {
                    for i in 0..10 {
                        store
                            .update(at(&path), read_presets, &no_order, |items| {
                                items.extend(presets(&[&format!("t{t}-{i}")]));
                                Some(())
                            })
                            .unwrap();
                        let seen = store.get(at(&path), read_presets, &no_order).unwrap();
                        assert!(seen.iter().any(|item| matches!(item, PresetItem::Preset(p) if p.id == format!("t{t}-{i}"))));
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(preset_ids(&read::<PresetItem>(&path)).len(), 81);
        assert_eq!(
            preset_ids(&store.get(at(&path), read_presets, &no_order).unwrap()).len(),
            81
        );
    }

    #[test]
    fn a_change_that_changes_nothing_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("albums.json");
        fs::write(&path, serde_json::to_string(&albums(&["Base"])).unwrap()).unwrap();
        let store = JsonFileStore::<AlbumItem>::new();
        store.get(at(&path), read_albums, &sort).unwrap();
        fs::write(&path, b"marker").unwrap();
        let none = store
            .update(at(&path), read_albums, &sort, |_| None::<()>)
            .unwrap();
        assert!(none.is_none());
        assert_eq!(fs::read(&path).unwrap(), b"marker");
        assert_eq!(
            album_names(&store.get(at(&path), read_albums, &sort).unwrap()),
            ["Base"]
        );
    }

    #[test]
    fn a_broken_file_stays_as_it_is_until_it_reads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("albums.json");
        fs::write(&path, b"{\"not\": a list").unwrap();
        let store = JsonFileStore::<AlbumItem>::new();
        let change = |store: &JsonFileStore<AlbumItem>, name: &str| {
            store
                .update(at(&path), read_albums, &sort, |items| {
                    items.extend(albums(&[name]));
                    Some(())
                })
                .map(|r| r.is_some())
        };
        assert!(change(&store, "First").is_err());
        assert!(store.get(at(&path), read_albums, &sort).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"{\"not\": a list");
        fs::write(&path, serde_json::to_string(&albums(&["Base"])).unwrap()).unwrap();
        assert!(change(&store, "Late").unwrap());
        let mut names = album_names(&read::<AlbumItem>(&path));
        names.sort();
        assert_eq!(names, ["Base", "Late"]);
    }

    #[test]
    fn a_failed_write_changes_neither_the_file_nor_the_store() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("albums.json");
        fs::write(&path, serde_json::to_string(&albums(&["Base"])).unwrap()).unwrap();
        let store = JsonFileStore::<AlbumItem>::new();
        store.get(at(&path), read_albums, &sort).unwrap();
        // A directory in the file's place: the atomic write cannot replace it.
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        assert!(
            store
                .update(at(&path), read_albums, &sort, |items| {
                    items.extend(albums(&["Lost"]));
                    Some(())
                })
                .is_err()
        );
        assert_eq!(
            album_names(&store.get(at(&path), read_albums, &sort).unwrap()),
            ["Base"]
        );
        fs::remove_dir(&path).unwrap();
    }

    #[test]
    fn the_file_is_read_once_per_run() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("presets.json");
        fs::write(&path, serde_json::to_string(&presets(&["a"])).unwrap()).unwrap();
        let reads = AtomicUsize::new(0);
        let counted = |text: &str| {
            reads.fetch_add(1, Ordering::Relaxed);
            read_presets(text)
        };
        let store = JsonFileStore::<PresetItem>::new();
        for _ in 0..20 {
            assert_eq!(
                preset_ids(&store.get(at(&path), counted, &no_order).unwrap()),
                ["a"]
            );
        }
        fs::write(&path, serde_json::to_string(&presets(&["a", "b"])).unwrap()).unwrap();
        assert_eq!(
            preset_ids(&store.get(at(&path), counted, &no_order).unwrap()),
            ["a"]
        );
        store
            .update(at(&path), counted, &no_order, |items| {
                items.extend(presets(&["c"]));
                Some(())
            })
            .unwrap();
        assert_eq!(reads.load(Ordering::Relaxed), 1, "one read per run");
        assert_eq!(preset_ids(&read::<PresetItem>(&path)), ["a", "c"]);
    }

    #[test]
    fn a_missing_file_is_an_empty_list_and_the_first_change_makes_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("albums.json");
        let store = JsonFileStore::<AlbumItem>::new();
        assert!(store.get(at(&path), read_albums, &sort).unwrap().is_empty());
        store
            .update(at(&path), read_albums, &sort, |items| {
                items.extend(albums(&["New"]));
                Some(())
            })
            .unwrap();
        assert_eq!(album_names(&read::<AlbumItem>(&path)), ["New"]);
    }

    #[test]
    fn the_album_list_is_kept_sorted_as_it_was_saved() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("albums.json");
        fs::write(
            &path,
            serde_json::to_string(&albums(&["Zebra", "alpha"])).unwrap(),
        )
        .unwrap();
        let store = JsonFileStore::<AlbumItem>::new();
        assert_eq!(
            album_names(&store.get(at(&path), read_albums, &sort).unwrap()),
            ["alpha", "Zebra"]
        );
        store
            .update(at(&path), read_albums, &sort, |items| {
                items.extend(albums(&["Middle"]));
                Some(())
            })
            .unwrap();
        assert_eq!(
            album_names(&read::<AlbumItem>(&path)),
            ["alpha", "Middle", "Zebra"]
        );
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("[\n  {"));
    }

    #[test]
    fn album_edits_wait_for_a_file_operations_member_update() {
        use crate::file_management::{AlbumChange, apply_album_change};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("albums.json");
        fs::write(&path, serde_json::to_string(&albums(&["Shared"])).unwrap()).unwrap();
        let store = JsonFileStore::<AlbumItem>::new();
        let (updating_tx, updating_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let release_rx = Mutex::new(release_rx);
        thread::scope(|s| {
            s.spawn(|| {
                store
                    .update(at(&path), read_albums, &sort, |tree| {
                        let AlbumItem::Album { images, .. } = &mut tree[0] else {
                            unreachable!()
                        };
                        *images = vec!["renamed.dng".into()];
                        updating_tx.send(()).unwrap();
                        lock_recover(&release_rx).recv().unwrap();
                        Some(())
                    })
                    .unwrap()
            });
            updating_rx.recv().unwrap();
            s.spawn(|| {
                store
                    .update(at(&path), read_albums, &sort, |tree| {
                        assert!(
                            apply_album_change(
                                tree,
                                &AlbumChange::Rename {
                                    id: "shared".into(),
                                    name: "Renamed album".into()
                                }
                            )
                            .unwrap()
                        );
                        Some(())
                    })
                    .unwrap()
            });
            release_tx.send(()).unwrap();
        });
        let tree = read::<AlbumItem>(&path);
        assert!(matches!(&tree[0], AlbumItem::Album { name, images, .. }
            if name == "Renamed album" && images == &["renamed.dng"]));
    }

    #[test]
    fn an_unreadable_presets_file_is_never_replaced_by_a_save_or_import() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("presets.json");
        let original = b"{broken preset file";
        fs::write(&path, original).unwrap();
        let store = JsonFileStore::<PresetItem>::new();
        for _ in 0..2 {
            assert!(
                store
                    .update(at(&path), read_presets, &|_| (), |tree| {
                        tree.extend(presets(&["new"]));
                        Some(())
                    })
                    .is_err()
            );
            assert_eq!(fs::read(&path).unwrap(), original);
        }
    }
}

#[cfg(test)]
mod queue_tests {
    use super::*;
    use std::{sync::mpsc, thread};

    const ALBUMS: &str = "Album list could not be saved";
    const PRESETS: &str = "Preset list could not be saved";

    #[test]
    fn changes_run_in_received_order() {
        let queue = LibraryQueue::default();
        let order = Mutex::new(Vec::new());
        let (started_tx, started_rx) = mpsc::channel();
        let (finish_tx, finish_rx) = mpsc::channel();
        let finish_rx = Mutex::new(finish_rx);
        thread::scope(|s| {
            s.spawn(|| {
                run_change(&queue, LibraryFile::Albums, ALBUMS, || {
                    started_tx.send(()).unwrap();
                    lock_recover(&finish_rx).recv().unwrap();
                    lock_recover(&order).push(1);
                    Ok(())
                })
                .unwrap()
            });
            started_rx.recv().unwrap();
            s.spawn(|| {
                run_change(&queue, LibraryFile::Presets, PRESETS, || {
                    lock_recover(&order).push(2);
                    Ok(())
                })
                .unwrap()
            });
            while queue.lock().pending.len() < 2 {
                thread::yield_now();
            }
            assert!(lock_recover(&order).is_empty());
            finish_tx.send(()).unwrap();
        });
        assert_eq!(*lock_recover(&order), [1, 2]);
    }

    #[test]
    fn a_later_success_clears_only_the_older_failure_of_the_same_file() {
        let queue = LibraryQueue::default();
        let _ = run_change(&queue, LibraryFile::Albums, ALBUMS, || {
            Err::<(), _>("album failure".into())
        });
        let _ = run_change(&queue, LibraryFile::Presets, PRESETS, || {
            Err::<(), _>("preset failure".into())
        });
        run_change(&queue, LibraryFile::Presets, PRESETS, || Ok(())).unwrap();
        assert_eq!(
            flush_library(&queue, Duration::from_millis(30)),
            ["Album list could not be saved: album failure"]
        );
        assert!(flush_library(&queue, Duration::from_millis(30)).is_empty());
    }

    #[test]
    fn only_the_latest_failure_per_file_is_reported_in_change_order() {
        let queue = LibraryQueue::default();
        let _ = run_change(&queue, LibraryFile::Albums, ALBUMS, || {
            Err::<(), _>("old".into())
        });
        let _ = run_change(&queue, LibraryFile::Presets, PRESETS, || {
            Err::<(), _>("presets".into())
        });
        let _ = run_change(&queue, LibraryFile::Albums, ALBUMS, || {
            Err::<(), _>("new".into())
        });
        assert_eq!(
            flush_library(&queue, Duration::ZERO),
            [
                "Preset list could not be saved: presets",
                "Album list could not be saved: new"
            ]
        );
    }

    #[test]
    fn a_flush_waits_for_a_running_import_to_finish_reading_and_merging() {
        let queue = LibraryQueue::default();
        let (started_tx, started_rx) = mpsc::channel();
        let started_rx = Mutex::new(started_rx);
        let (finish_tx, finish_rx) = mpsc::channel();
        let finish_rx = Mutex::new(finish_rx);
        let merged = Mutex::new(false);
        thread::scope(|s| {
            s.spawn(|| {
                lock_recover(&started_rx).recv().unwrap();
                let failures = flush_library(&queue, Duration::from_millis(200));
                finish_tx.send(()).unwrap();
                assert!(failures.is_empty(), "{failures:?}");
                assert!(*lock_recover(&merged));
            });
            run_change(&queue, LibraryFile::Presets, "Preset import", || {
                started_tx.send(()).unwrap();
                assert!(
                    lock_recover(&finish_rx)
                        .recv_timeout(Duration::from_millis(40))
                        .is_err(),
                    "flush returned while the import was still reading"
                );
                *lock_recover(&merged) = true;
                Ok(())
            })
            .unwrap();
        });
    }

    #[test]
    fn an_import_after_a_timeout_still_finishes_once_and_keeps_its_turn() {
        let queue = LibraryQueue::default();
        let order = Mutex::new(Vec::new());
        let (started_tx, started_rx) = mpsc::channel();
        let (finish_tx, finish_rx) = mpsc::channel();
        let finish_rx = Mutex::new(finish_rx);
        thread::scope(|s| {
            s.spawn(|| {
                run_change(&queue, LibraryFile::Presets, "Preset import", || {
                    started_tx.send(()).unwrap();
                    lock_recover(&finish_rx).recv().unwrap();
                    lock_recover(&order).push("import");
                    Ok(())
                })
                .unwrap()
            });
            started_rx.recv().unwrap();
            s.spawn(|| {
                run_change(&queue, LibraryFile::Albums, ALBUMS, || {
                    lock_recover(&order).push("album");
                    Ok(())
                })
                .unwrap()
            });
            while queue.lock().pending.len() < 2 {
                thread::yield_now();
            }
            let report = flush_library(&queue, Duration::from_millis(20));
            assert_eq!(
                report,
                [
                    "Preset import (still running; not written yet)",
                    "Album list could not be saved (still running; not written yet)"
                ]
            );
            assert!(lock_recover(&order).is_empty());
            finish_tx.send(()).unwrap();
        });
        assert_eq!(*lock_recover(&order), ["import", "album"]);
        assert!(flush_library(&queue, Duration::ZERO).is_empty());
    }

    #[test]
    fn an_idle_flush_returns_without_waiting_on_itself() {
        let queue = LibraryQueue::default();
        let start = Instant::now();
        assert!(flush_library(&queue, Duration::from_secs(2)).is_empty());
        assert!(start.elapsed() < Duration::from_secs(1));
        assert!(queue.lock().pending.is_empty());
    }

    #[test]
    fn a_flush_wakes_when_the_last_write_finishes() {
        let queue = LibraryQueue::default();
        let mut change = queue.record(LibraryFile::Albums, ALBUMS);
        let (done_tx, done_rx) = mpsc::channel();
        thread::scope(|s| {
            s.spawn(|| {
                done_tx
                    .send(flush_library(&queue, Duration::from_secs(3)))
                    .unwrap();
            });
            assert!(done_rx.recv_timeout(Duration::from_millis(20)).is_err());
            change.finish(None);
            assert!(
                done_rx
                    .recv_timeout(Duration::from_secs(1))
                    .unwrap()
                    .is_empty()
            );
        });
    }

    #[test]
    fn an_unwinding_change_reports_failure_and_does_not_leave_pending_work() {
        let queue = LibraryQueue::default();
        assert!(
            std::panic::catch_unwind(|| run_change(
                &queue,
                LibraryFile::Albums,
                ALBUMS,
                || -> Result<(), String> { panic!("worker panic") }
            ))
            .is_err()
        );
        assert_eq!(
            flush_library(&queue, Duration::ZERO),
            ["Album list could not be saved: change did not finish"]
        );
        run_change(&queue, LibraryFile::Albums, ALBUMS, || Ok(())).unwrap();
        assert!(flush_library(&queue, Duration::ZERO).is_empty());
    }
}
