//! The app's own library files, `albums.json` and `presets.json`: each is
//! held by a `JsonFileStore`, read once per run, and every change to it is one
//! read-modify-write under the store's write lock, persisted with
//! `rawtonic_fs::atomic_write` (the same file, format and helper as before the
//! store) before readers see it. `AlbumsStore`, `PresetsStore` and
//! `LibraryQueue` are the three managed states here.
//!
//! Only this process writes them while the app runs: the GUI is a single
//! instance (`tauri_plugin_single_instance`), and the headless runs of the
//! same binary never write albums or presets. Changes come from the
//! frontend's commands and from the file operations' album sync, all on
//! blocking workers.
//!
//! The store alone cannot keep two frontend changes from losing one another:
//! the page reads the list (`get_albums`), changes it and could save the whole
//! snapshot, so two changes made from the same read would each save over the
//! other. `LibraryQueue` is one order for every write of both files: each
//! change takes its place the moment its command reaches the backend
//! (`ticket: Some` when the page numbered it itself, `queue_ticket`; else the
//! next place on arrival), and every change waits for the ones before it
//! before doing its read-modify-write, so they are written in the order the
//! frontend sent them, each on the list the changes before it left behind.
//! The album commands change the stored album they are given:
//! `library_change` (`add_to_album`) adds photographs to the album in the
//! file, so a list the page read earlier cannot drop an album made meanwhile.
//! The queue's lock is never held while a store's write lock is taken.
//!
//! An import parses its files before its turn: while it only reads, the
//! entries after it run (a large import never holds the album saves up). What
//! it parsed becomes one change with its ticket: after parsing it waits until
//! every lower entry not reading is done and then writes in their place, so
//! its position among changes is exact where it matters and the frontend was
//! not kept waiting by the read. `library_flush` is quitting's and Save &
//! Relaunch's drain: it records a ticket after every change the frontend
//! sent, waits for the entries below it (at most `FLUSH_TIMEOUT` for the ones
//! already running), writes what an import read but never got to write, and
//! answers with what is not written, in ticket order.
//!
//! A file that is there but cannot be read stays untouched: every change
//! fails, as it did when each change read the file itself, and it is read
//! again on the next access. A file that is not there yet is an empty list;
//! changes are written over it as before. A hand edit of the file while the
//! app runs was always overwritten by the next save; it now also stays unseen
//! until the next start.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, RwLock};
use std::time::{Duration, Instant};

use tauri::AppHandle;

use crate::file_management::{AlbumItem, PresetItem, sort_album_tree};
use crate::state::lock::{lock_recover, read_recover, write_recover};

/// How long quitting and Save & Relaunch wait for the frontend's writes
/// before they report the ones not written (`library_flush`).
pub(crate) const FLUSH_TIMEOUT: Duration = Duration::from_secs(10);

/// How long an entry waits for the entries before it (a flush is on its way;
/// nothing waits forever).
const ENTRY_GRACE: Duration = Duration::from_secs(30);

const NOT_WRITTEN: &str = "Album or preset change was not written in time";
const IMPORT_STILL_READING: &str = "Preset import still running (its presets are not written yet)";
const IMPORT_NOT_SAVED: &str = "Preset import could not be saved";

/// Hands out entry identities; entries are only recorded while the process
/// runs, so the order they were recorded in never repeats.
static NEXT_IDENTITY: AtomicU64 = AtomicU64::new(0);

/// One order for every write of the library files, as the frontend has one
/// queue of its saves (`src/store/libraryFiles.ts`). Managed by tauri
/// (`lib.rs`) and reached by the commands as `app.state::<LibraryQueue>()`.
#[derive(Default)]
pub struct LibraryQueue {
    state: Mutex<QueueState>,
    /// Signalled when an entry finishes or a write is recorded.
    changed: Condvar,
}

struct QueueState {
    /// The entries not finished yet, in ticket order; the identity keeps two
    /// entries of one ticket apart.
    entries: BTreeSet<(u64, u64)>,
    /// The tickets of entries that only read their files for now: the
    /// entries after them run meanwhile.
    reading: BTreeSet<u64>,
    /// The imports that read their files, until their change is written: a
    /// flush writes the ones that did not get their turn.
    ready: Vec<Arc<Ready>>,
    /// The next ticket handed out (`queue_ticket`).
    next: u64,
    /// The changes not written or failed yet: what a flush waits for.
    writes: BTreeSet<u64>,
    /// The changes written (or failed) once: what a flush reports.
    answered: BTreeSet<u64>,
    /// The errors of failed changes, taken by the next flush.
    errors: BTreeMap<u64, String>,
    /// The next change to start is the flush's own: nothing waits for it
    /// (`start_write` is where a change takes this).
    own: bool,
}

impl Default for QueueState {
    fn default() -> Self {
        Self {
            entries: BTreeSet::new(),
            reading: BTreeSet::new(),
            ready: Vec::new(),
            // Tickets start at 1: 0 is "none" (`library_flush(None)`).
            next: 1,
            writes: BTreeSet::new(),
            answered: BTreeSet::new(),
            errors: BTreeMap::new(),
            own: false,
        }
    }
}

impl QueueState {
    /// Whether an entry before `ticket` is still unfinished. An entry that
    /// only reads does not block: nothing it does can be written over.
    fn blocked(&self, ticket: u64) -> bool {
        self.entries
            .iter()
            .any(|(entry, _)| *entry < ticket && !self.reading.contains(entry))
    }

    /// A change a flush for `until` still has to wait for: one being
    /// written, or an entry not started that else would be dropped.
    fn pending_write(&self, until: u64) -> Option<u64> {
        let written = self.writes.range(..=until).next().copied();
        let queued = self
            .entries
            .iter()
            .map(|(entry, _)| *entry)
            .filter(|entry| *entry <= until && !self.reading.contains(entry))
            .min();
        match (written, queued) {
            (Some(written), Some(queued)) => Some(written.min(queued)),
            (written, queued) => written.or(queued),
        }
    }

    /// The imports that read their files but never wrote them.
    fn unwritten(&self, until: u64) -> Vec<Arc<Ready>> {
        self.ready
            .iter()
            .filter(|ready| ready.ticket <= until && !ready.written.load(Ordering::SeqCst))
            .cloned()
            .collect()
    }
}

impl LibraryQueue {
    fn lock(&self) -> MutexGuard<'_, QueueState> {
        lock_recover(&self.state)
    }

    fn wake(&self) {
        self.changed.notify_all();
    }

    /// Hands out the ticket of a change the frontend is about to send.
    fn ticket(&self) -> u64 {
        let mut state = self.lock();
        let ticket = state.next;
        state.next += 1;
        ticket
    }

    /// Records entry `ticket` before its change starts; `reading`: it only
    /// reads its files for now.
    fn record(&self, ticket: u64, reading: bool) -> Entry<'_> {
        let identity = NEXT_IDENTITY.fetch_add(1, Ordering::SeqCst);
        {
            let mut state = self.lock();
            state.next = state.next.max(ticket.saturating_add(1));
            state.entries.insert((ticket, identity));
            if reading {
                state.reading.insert(ticket);
            }
            drop(state);
        }
        self.wake();
        Entry {
            queue: self,
            ticket,
            identity,
        }
    }

    /// Waits until every entry before `ticket` is finished, at most
    /// `ENTRY_GRACE` (then warns: a flush was due after it and did not come).
    fn wait_before(&self, ticket: u64) {
        let until = Instant::now() + ENTRY_GRACE;
        let mut state = self.lock();
        while state.blocked(ticket) {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                log::warn!("A library write waited {ENTRY_GRACE:?} for earlier writes; going on");
                return;
            }
            state = self
                .changed
                .wait_timeout(state, left)
                .map(|(state, _)| state)
                .unwrap_or_else(|e| e.into_inner().0);
        }
    }

    /// The entry `ticket` stops only reading: the entries after it wait for
    /// its change from now on.
    fn stop_reading(&self, ticket: u64) {
        self.lock().reading.remove(&ticket);
        self.wake();
    }

    /// Records the write of `ticket` before it starts; the change a flush
    /// makes itself is not one it waits for.
    fn start_write(&self, ticket: u64) {
        let mut state = self.lock();
        if !std::mem::take(&mut state.own) {
            state.writes.insert(ticket);
        }
        drop(state);
        self.wake();
    }

    /// The change of `ticket` is written.
    fn answered(&self, ticket: u64) {
        let mut state = self.lock();
        state.writes.remove(&ticket);
        state.answered.insert(ticket);
        drop(state);
        self.wake();
    }

    /// The change of `ticket` failed: its error goes to the next flush.
    fn failed(&self, ticket: u64, error: String) {
        let mut state = self.lock();
        state.writes.remove(&ticket);
        state.answered.insert(ticket);
        state.errors.insert(ticket, error);
        drop(state);
        self.wake();
    }
}

/// A ticket for a command the frontend did not send one with: it comes after
/// every change sent so far.
pub(crate) fn next_after_sent(queue: &LibraryQueue) -> u64 {
    queue.ticket()
}

/// Runs change `ticket` in ticket order and records its outcome for the next
/// flush; `describe` names what could not be saved in the error a flush
/// reports (the command gets its own error unchanged).
pub(crate) fn run_change<T>(
    queue: &LibraryQueue,
    ticket: u64,
    describe: &str,
    work: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    let entry = queue.record(ticket, false);
    queue.wait_before(ticket);
    queue.start_write(ticket);
    let result = work();
    match &result {
        Ok(_) => queue.answered(ticket),
        Err(e) => queue.failed(ticket, format!("{describe}: {e}")),
    }
    drop(entry);
    result
}

/// A change's place in the order, until dropped: dropping it frees the
/// entries after it, also when its change panicked or returned early.
struct Entry<'q> {
    queue: &'q LibraryQueue,
    /// Told to `run_import` for the import's `Ready`.
    ticket: u64,
    identity: u64,
}

impl Drop for Entry<'_> {
    fn drop(&mut self) {
        let mut state = self.queue.lock();
        state.entries.remove(&(self.ticket, self.identity));
        state.reading.remove(&self.ticket);
        drop(state);
        self.queue.wake();
    }
}

/// What an import parsed, until its change is written.
struct Ready {
    /// The import's ticket.
    ticket: u64,
    /// The entry that read the files (`bring_back` takes it off the queue).
    identity: u64,
    /// Taken by whoever writes it: the import's change or a flush.
    items: Mutex<Option<Vec<PresetItem>>>,
    /// Written by either of them (its change is then answered).
    written: AtomicBool,
}

/// Runs a preset import in ticket order: `read` parses the files (the slow
/// part, while the entries after the import run) and says what to merge,
/// `merge` then adds it to the presets in one change. A failed read has
/// nothing to write; its error is told to the next flush.
pub(crate) fn run_import<R, T>(
    queue: &LibraryQueue,
    ticket: u64,
    read: impl FnOnce() -> Result<(Vec<PresetItem>, R), String>,
    merge: impl FnOnce(Vec<PresetItem>) -> Result<T, String>,
) -> Result<(T, R), String> {
    let entry = queue.record(ticket, true);
    let (items, report) = match read() {
        Ok(parsed) => parsed,
        Err(e) => {
            queue.failed(ticket, format!("Preset import could not be read: {e}"));
            return Err(e);
        }
    };
    let ready = Arc::new(Ready {
        ticket,
        identity: entry.identity,
        items: Mutex::new(Some(items)),
        written: AtomicBool::new(false),
    });
    queue.lock().ready.push(ready.clone());
    queue.wake();
    // The import's exact turn: every lower entry that is not only reading is
    // done, and from here the entries after it wait for its change.
    queue.wait_before(ticket);
    queue.stop_reading(ticket);
    // A flush that ran out of its wait may write it meanwhile: it takes the
    // items first, so one of the two writes them.
    let written = match lock_recover(&ready.items).take() {
        Some(items) => {
            queue.start_write(ticket);
            let written = merge(items);
            match &written {
                Ok(_) => queue.answered(ticket),
                Err(e) => queue.failed(ticket, format!("{IMPORT_NOT_SAVED}: {e}")),
            }
            written
        }
        // The flush wrote them: the library holds them already.
        None => merge(Vec::new()),
    };
    ready.written.store(true, Ordering::SeqCst);
    queue
        .lock()
        .ready
        .retain(|other| !Arc::ptr_eq(other, &ready));
    drop(entry);
    written.map(|written| (written, report))
}

/// Waits for the changes up to `until` (at most `timeout`), then answers what
/// is not written, in ticket order. The imports up to `until` that read their
/// files but did not get their turn are written after the wait, in their
/// place (`merge`).
///
/// Nothing is waited for forever: a change that hangs makes the flush report
/// it (the frontend's quit gives up at `FLUSH_TIMEOUT` too).
pub(crate) fn flush_state(
    queue: &LibraryQueue,
    until: u64,
    timeout: Duration,
    mut merge: impl FnMut(Vec<PresetItem>) -> Result<Vec<PresetItem>, String>,
) -> Vec<String> {
    let deadline = Instant::now() + timeout;
    // Recorded after the other entries were found not to hold `own`: its
    // change is not one this flush waits for.
    let own_entry = {
        let entry = queue.record(until, false);
        queue.lock().own = true;
        entry
    };
    while queue.lock().pending_write(until).is_some() {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        let state = queue.lock();
        // Every change of the queue notifies; the small timeout only covers a
        // waiter that was notified for something else.
        let (state, _) = queue
            .changed
            .wait_timeout_while(state, left, |_| Instant::now() < deadline)
            .unwrap_or_else(|e| e.into_inner());
        drop(state);
    }
    let mut failures: Vec<(u64, String)> = Vec::new();
    {
        let mut state = queue.lock();
        // The answers (and their errors) up to `until`, told once.
        let told: Vec<u64> = state.answered.range(..=until).copied().collect();
        for ticket in told {
            state.answered.remove(&ticket);
            if let Some(error) = state.errors.remove(&ticket) {
                failures.push((ticket, error));
            }
        }
        // What was not written: a change still running (the flush's own
        // entries are not changes).
        let unwritten = state.writes.range(..=until).next().is_some();
        let still_reading = state
            .entries
            .iter()
            .any(|(entry, _)| *entry <= until && state.reading.contains(entry));
        if still_reading {
            failures.push((until, IMPORT_STILL_READING.to_string()));
        }
        if unwritten {
            failures.push((until, NOT_WRITTEN.to_string()));
        }
    };
    drop(own_entry);
    failures.extend(bring_back(queue, until, &mut merge));
    failures.sort_by_key(|(ticket, _)| *ticket);
    failures.into_iter().map(|(_, error)| error).collect()
}

/// Writes the imports up to `until` that read their files but whose change
/// the flush did not see written: it was still waiting for lower entries, or
/// its wait ran out. The import itself then finds its items gone and reports
/// them written. Best effort: an import whose change took its items to write
/// them is left to itself.
fn bring_back(
    queue: &LibraryQueue,
    until: u64,
    merge: &mut impl FnMut(Vec<PresetItem>) -> Result<Vec<PresetItem>, String>,
) -> Vec<(u64, String)> {
    let mut failures = Vec::new();
    loop {
        let Some(ready) = queue.lock().unwritten(until).pop() else {
            break;
        };
        // Written meanwhile by its own change: it is not written twice.
        let Some(items) = lock_recover(&ready.items).take() else {
            continue;
        };
        queue.record(ready.ticket, false);
        queue.start_write(ready.ticket);
        match merge(items) {
            Ok(_) => queue.answered(ready.ticket),
            Err(e) => {
                let error = format!("{IMPORT_NOT_SAVED}: {e}");
                queue.failed(ready.ticket, error.clone());
                failures.push((ready.ticket, error));
            }
        }
        let mut state = queue.lock();
        state.entries.remove(&(ready.ticket, ready.identity));
        state.ready.retain(|other| !Arc::ptr_eq(other, &ready));
        drop(state);
        queue.wake();
    }
    failures
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
    use std::sync::atomic::AtomicUsize;
    use std::sync::mpsc;
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

    // The queue itself: the order, an import's reading, failures, the flush.

    fn record_order(order: &Arc<Mutex<Vec<u64>>>, ticket: u64) {
        order.lock().unwrap().push(ticket);
    }

    #[test]
    fn changes_run_in_ticket_order_even_when_the_later_one_starts_first() {
        let queue = LibraryQueue::default();
        let order = Arc::new(Mutex::new(Vec::new()));
        let (first_running_tx, first_running_rx) = mpsc::channel::<()>();
        let (let_first_finish_tx, let_first_finish_rx) = mpsc::channel::<()>();
        let let_first_finish_rx = Arc::new(Mutex::new(let_first_finish_rx));
        thread::scope(|s| {
            s.spawn(|| {
                run_change(&queue, 1, "Album or preset change", || {
                    first_running_tx.send(()).unwrap();
                    let_first_finish_rx.lock().unwrap().recv().unwrap();
                    record_order(&order, 1);
                    Ok(())
                })
                .unwrap();
            });
            first_running_rx.recv().unwrap();
            s.spawn(|| {
                // Recorded while change 1 runs: it waits for it.
                run_change(&queue, 2, "Album or preset change", || {
                    record_order(&order, 2);
                    Ok(())
                })
                .unwrap();
            });
            while queue.lock().entries.len() < 2 {
                thread::yield_now();
            }
            let_first_finish_tx.send(()).unwrap();
        });
        assert_eq!(*order.lock().unwrap(), [1, 2]);
    }

    /// Every command asks for its ticket when it arrives (`ticket:
    /// None`): the arrival order is the ticket order.
    #[test]
    fn changes_without_a_ticket_run_in_the_order_they_arrive() {
        let queue = LibraryQueue::default();
        let order = Arc::new(Mutex::new(Vec::new()));
        let (first_running_tx, first_running_rx) = mpsc::channel::<()>();
        let (let_first_finish_tx, let_first_finish_rx) = mpsc::channel::<()>();
        let let_first_finish_rx = Arc::new(Mutex::new(let_first_finish_rx));
        thread::scope(|s| {
            s.spawn(|| {
                let ticket = next_after_sent(&queue);
                run_change(&queue, ticket, "Album or preset change", || {
                    first_running_tx.send(()).unwrap();
                    let_first_finish_rx.lock().unwrap().recv().unwrap();
                    record_order(&order, 1);
                    Ok(())
                })
                .unwrap();
            });
            first_running_rx.recv().unwrap();
            // The second change arrives while the first still runs.
            let ticket = next_after_sent(&queue);
            let (second, q) = (Arc::clone(&order), &queue);
            s.spawn(move || {
                run_change(q, ticket, "Album or preset change", || {
                    record_order(&second, 2);
                    Ok(())
                })
                .unwrap();
            });
            while queue.lock().entries.len() < 2 {
                thread::yield_now();
            }
            let_first_finish_tx.send(()).unwrap();
        });
        assert_eq!(*order.lock().unwrap(), [1, 2]);
    }

    #[test]
    fn a_flush_reports_a_failed_change_once() {
        let queue = LibraryQueue::default();
        assert!(
            run_change(&queue, 1, "Album list could not be saved", || {
                Err::<(), String>("disk full".to_string())
            })
            .is_err()
        );
        let merge = |items: Vec<PresetItem>| Ok(items);
        assert_eq!(
            flush_state(&queue, 1, Duration::from_millis(10), merge),
            ["Album list could not be saved: disk full"]
        );
        assert!(
            flush_state(&queue, 1, Duration::from_millis(10), merge).is_empty(),
            "an error is told once"
        );
    }

    #[test]
    fn an_import_that_reads_does_not_block_a_later_change_which_is_written_first() {
        let queue = LibraryQueue::default();
        let written = Arc::new(Mutex::new(Vec::new()));
        let (reading_tx, reading_rx) = mpsc::channel::<()>();
        let (read_done_tx, read_done_rx) = mpsc::channel::<()>();
        let read_done_rx = Arc::new(Mutex::new(read_done_rx));
        let order = written.clone();
        thread::scope(|s| {
            s.spawn(|| {
                let read = || {
                    reading_tx.send(()).unwrap();
                    read_done_rx.lock().unwrap().recv().unwrap();
                    Ok((presets(&["imported"]), ()))
                };
                run_import(&queue, 1, read, |items| {
                    record_order(&order, 1);
                    Ok(items.len())
                })
                .unwrap();
            });
            reading_rx.recv().unwrap();
            // The import is reading: the later change runs meanwhile.
            run_change(&queue, 2, "Preset list could not be saved", || {
                record_order(&written, 2);
                Ok(())
            })
            .unwrap();
            read_done_tx.send(()).unwrap();
        });
        assert_eq!(
            *written.lock().unwrap(),
            [2, 1],
            "the import writes after change 2"
        );
        let merge = |items: Vec<PresetItem>| Ok(items);
        assert!(flush_state(&queue, 2, Duration::from_millis(10), merge).is_empty());
    }

    #[test]
    fn a_flush_waiting_on_a_hung_change_reports_it_not_written() {
        let queue = LibraryQueue::default();
        let (running_tx, running_rx) = mpsc::channel::<()>();
        let (finish_tx, finish_rx) = mpsc::channel::<()>();
        let finish_rx = Arc::new(Mutex::new(finish_rx));
        let report = thread::scope(|s| {
            s.spawn(|| {
                let _ = run_change(&queue, 2, "Album or preset change", || {
                    running_tx.send(()).unwrap();
                    let _ = finish_rx.lock().unwrap().recv();
                    Ok(())
                });
            });
            running_rx.recv().unwrap();
            let report = flush_state(&queue, 3, Duration::from_millis(20), Ok);
            let _ = finish_tx.send(());
            report
        });
        assert!(
            report.iter().any(|error| error == NOT_WRITTEN),
            "{report:?}"
        );
    }
}
