//! The camera names photographs' files gave (`develop_defaults::camera_of`),
//! kept on disk in the app's own cache folder so a Library of never-edited
//! RAWs does not read each file's tags again on every start while develop
//! defaults are set (CRD6: 95–219 ms per RAW over a LAN share; notes:
//! benchmark-results/crd6-2026-11-23/REPORT.md).
//!
//! Only the names the file itself carries are kept (make, model, serial as
//! the decoder or the EXIF reads them); the camera's identity is made from
//! them again on every load; names read with another decoder's camera
//! catalogue (`develop_defaults::naming_fingerprint`) are not used. An
//! entry stands for its file only while the path, size, modification time
//! and (where the system has one) file id are the ones it was read at. A
//! missing, damaged or older store is no store: the files are read again.
//! Nothing is ever written beside the user's files.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

/// How the kept names were read; a change in what `camera_of` reads from a
/// file makes a new version, and older stores are ignored.
const VERSION: u32 = 1;
const FILE_NAME: &str = "camera-names-v1.json";
/// The most entries kept; the least recently used go first beyond it.
const LIMIT: usize = 20_000;
/// New entries are written this long after the first of them (batched).
const WRITE_EVERY: Duration = Duration::from_secs(2);

/// What identifies the state of a file the names were read from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Stamp {
    pub size: u64,
    pub modified_ns: i128,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_id: Option<(u64, u64)>,
}

impl Stamp {
    pub(crate) fn of(path: &Path) -> Option<Self> {
        let metadata = std::fs::metadata(path).ok()?;
        let modified = metadata.modified().ok()?;
        let modified_ns = match modified.duration_since(SystemTime::UNIX_EPOCH) {
            Ok(after) => after.as_nanos() as i128,
            Err(before) => -(before.duration().as_nanos() as i128),
        };
        Some(Self {
            size: metadata.len(),
            modified_ns,
            file_id: file_id(&metadata),
        })
    }
}

#[cfg(unix)]
fn file_id(metadata: &std::fs::Metadata) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    Some((metadata.dev(), metadata.ino()))
}

#[cfg(not(unix))]
fn file_id(_metadata: &std::fs::Metadata) -> Option<(u64, u64)> {
    None
}

/// A camera's names as one file gives them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct FileNames {
    pub make: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub serial: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
struct Entry {
    stamp: Stamp,
    names: FileNames,
    /// Order of last use (larger is newer), for eviction.
    used: u64,
}

#[derive(Default, Serialize, Deserialize)]
struct Stored {
    version: u32,
    /// The decoder's naming data the names were read with
    /// (`develop_defaults::naming_fingerprint`).
    #[serde(default)]
    naming: String,
    entries: HashMap<PathBuf, Entry>,
}

#[derive(Default)]
struct Store {
    path: Option<PathBuf>,
    entries: HashMap<PathBuf, Entry>,
    /// The naming data the loaded entries were read with, until compared
    /// with this build's on first use (None once compared).
    unchecked_naming: Option<String>,
    clock: u64,
    dirty: bool,
}

impl Store {
    /// Drops entries read with another decoder's naming data. Compared on
    /// first use, not at start-up (the catalogue loads then).
    fn check_naming(&mut self) {
        if let Some(naming) = self.unchecked_naming.take()
            && naming != crate::develop_defaults::naming_fingerprint()
        {
            self.entries.clear();
        }
    }
}

fn store() -> &'static Mutex<Store> {
    static STORE: OnceLock<Mutex<Store>> = OnceLock::new();
    STORE.get_or_init(Default::default)
}

/// Keeps the names in `dir` (the app's cache folder) from now on, with
/// what an earlier run kept there. Without it they are only kept in memory.
pub fn set_dir(dir: &Path) {
    let path = dir.join(FILE_NAME);
    let (naming, entries) = std::fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Stored>(&bytes).ok())
        .filter(|stored| stored.version == VERSION)
        .map(|stored| (stored.naming, stored.entries))
        .unwrap_or_default();
    let mut store = store().lock().unwrap_or_else(|e| e.into_inner());
    store.clock = entries.values().map(|e| e.used).max().unwrap_or(0);
    store.entries = entries;
    store.unchecked_naming = Some(naming);
    store.path = Some(path);
    store.dirty = false;
}

/// The names kept for `path` when they were read at `stamp`.
pub(crate) fn get(path: &Path, stamp: &Stamp) -> Option<FileNames> {
    let mut store = store().lock().unwrap_or_else(|e| e.into_inner());
    store.check_naming();
    store.clock += 1;
    let clock = store.clock;
    let entry = store.entries.get_mut(path).filter(|e| e.stamp == *stamp)?;
    entry.used = clock;
    Some(entry.names.clone())
}

/// Keeps the names `path` gave at `stamp`; written to disk by a background
/// flush shortly after, never on the caller's (a Library tile's) thread.
pub(crate) fn put(path: &Path, stamp: Stamp, names: FileNames) {
    {
        let mut store = store().lock().unwrap_or_else(|e| e.into_inner());
        store.check_naming();
        store.clock += 1;
        let used = store.clock;
        store
            .entries
            .insert(path.to_path_buf(), Entry { stamp, names, used });
        evict(&mut store.entries);
        store.dirty = true;
    }
    flush_later();
}

/// One flush after `WRITE_EVERY`, for entries a throttled `put` left.
fn flush_later() {
    use std::sync::atomic::{AtomicBool, Ordering};
    static PENDING: AtomicBool = AtomicBool::new(false);
    if store()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .path
        .is_none()
        || PENDING.swap(true, Ordering::AcqRel)
    {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("camera-names".into())
        .spawn(|| {
            std::thread::sleep(WRITE_EVERY);
            PENDING.store(false, Ordering::Release);
            flush();
        });
    if spawned.is_err() {
        PENDING.store(false, Ordering::Release);
    }
}

fn evict(entries: &mut HashMap<PathBuf, Entry>) {
    if entries.len() <= LIMIT {
        return;
    }
    // Down to 90 % at once, so eviction does not run on every new entry.
    let mut used: Vec<u64> = entries.values().map(|e| e.used).collect();
    used.sort_unstable();
    let cut = used[entries.len() - LIMIT * 9 / 10];
    entries.retain(|_, e| e.used >= cut);
}

/// Writes entries not yet on disk (atomically: a temporary file in the same
/// folder, then renamed over the store).
pub fn flush() {
    static WRITING: Mutex<()> = Mutex::new(());
    let _writing = WRITING.lock().unwrap_or_else(|e| e.into_inner());
    let (path, bytes) = {
        let mut store = store().lock().unwrap_or_else(|e| e.into_inner());
        let Some(path) = store.path.clone().filter(|_| store.dirty) else {
            return;
        };
        store.check_naming();
        let stored = Stored {
            version: VERSION,
            naming: crate::develop_defaults::naming_fingerprint().to_string(),
            entries: store.entries.clone(),
        };
        let Ok(bytes) = serde_json::to_vec(&stored) else {
            return;
        };
        store.dirty = false;
        (path, bytes)
    };
    if let Err(error) = write_atomic(&path, &bytes) {
        log::warn!("Camera names not kept in {}: {error}", path.display());
        store().lock().unwrap_or_else(|e| e.into_inner()).dirty = true;
    }
}

fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    rawtonic_fs::atomic_write(path, bytes).map_err(std::io::Error::other)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(model: &str) -> FileNames {
        FileNames {
            make: "Make".into(),
            model: model.into(),
            serial: None,
        }
    }

    #[test]
    fn evicts_the_least_recently_used() {
        let mut entries = HashMap::new();
        let stamp = Stamp {
            size: 1,
            modified_ns: 1,
            file_id: None,
        };
        for i in 0..=LIMIT as u64 {
            entries.insert(
                PathBuf::from(format!("/p/{i}")),
                Entry {
                    stamp,
                    names: names("m"),
                    used: i,
                },
            );
        }
        evict(&mut entries);
        assert!(entries.len() <= LIMIT * 9 / 10 + 1);
        assert!(entries.contains_key(Path::new(&format!("/p/{LIMIT}"))));
        assert!(!entries.contains_key(Path::new("/p/0")));
    }

    #[test]
    fn a_file_gives_its_kept_names_until_it_changes_and_they_survive_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let photo = dir.path().join("photo.jpg");
        std::fs::write(&photo, b"no exif here").unwrap();
        let store_dir = dir.path().join("cache");
        set_dir(&store_dir);
        let stamp = Stamp::of(&photo).unwrap();
        put(&photo, stamp, names("Kept"));
        // The file itself has no camera: the identity comes from the store.
        let camera = crate::develop_defaults::camera_of(&photo).unwrap();
        assert_eq!(camera.model, "Kept");
        flush();
        // A new run reads what was kept.
        set_dir(&store_dir);
        assert_eq!(get(&photo, &stamp), Some(names("Kept")));
        // Another state of the file is not the one the names were read at.
        let other = Stamp {
            size: stamp.size + 1,
            ..stamp
        };
        assert_eq!(get(&photo, &other), None);
        // Names read with another decoder's naming data are not used.
        let mut stored: serde_json::Value =
            serde_json::from_slice(&std::fs::read(store_dir.join(FILE_NAME)).unwrap()).unwrap();
        assert_eq!(
            stored["naming"],
            crate::develop_defaults::naming_fingerprint()
        );
        stored["naming"] = "another catalogue".into();
        std::fs::write(store_dir.join(FILE_NAME), stored.to_string()).unwrap();
        set_dir(&store_dir);
        assert_eq!(get(&photo, &stamp), None);
        // A damaged store is no store.
        std::fs::write(store_dir.join(FILE_NAME), b"{not json").unwrap();
        set_dir(&store_dir);
        assert_eq!(get(&photo, &stamp), None);
        store().lock().unwrap().path = None;
    }
}
