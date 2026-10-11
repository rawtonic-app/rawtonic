//! Hot folder (Capture One's hot folder, Lightroom's Auto Import): which
//! files in a watched folder are complete and new. Files are taken once
//! their size and modification time have not changed for a settle time, so a
//! file still being written by tethering software or a card reader is left
//! alone. Polling keeps this portable and independent of file system events.
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime},
};

/// How long a file must stay unchanged before it is taken.
pub const DEFAULT_SETTLE: Duration = Duration::from_millis(1500);

/// The pause before a file whose import failed is offered again; it doubles
/// with every further failure, up to `RETRY_MAX`.
pub const RETRY_FIRST: Duration = Duration::from_secs(5);
pub const RETRY_MAX: Duration = Duration::from_secs(300);

#[derive(Debug, Clone, PartialEq, Eq)]
struct Seen {
    size: u64,
    modified: Option<SystemTime>,
    since: Instant,
}

/// A file whose import failed, and when it is offered again.
#[derive(Debug, Clone)]
struct Retry {
    size: u64,
    modified: Option<SystemTime>,
    failures: u32,
    at: Instant,
}

/// Remembers what a watched folder held at earlier polls.
pub struct HotFolder {
    folder: PathBuf,
    settle: Duration,
    seen: HashMap<PathBuf, Seen>,
    taken: HashMap<PathBuf, (u64, Option<SystemTime>)>,
    retries: HashMap<PathBuf, Retry>,
}

impl HotFolder {
    pub fn new(folder: impl Into<PathBuf>, settle: Duration) -> Self {
        Self {
            folder: folder.into(),
            settle,
            seen: HashMap::new(),
            taken: HashMap::new(),
            retries: HashMap::new(),
        }
    }

    /// Reports that a file `poll` returned could not be imported (say the
    /// destination was not writable): a later poll offers it again, after a
    /// pause that grows with each failure, or as a new file once it changes.
    /// Returns whether this is its first failure (so it is reported once).
    pub fn failed(&mut self, path: &Path, now: Instant) -> bool {
        let Some((size, modified)) = self.taken.remove(path) else {
            return false;
        };
        let failures = match self.retries.get(path) {
            Some(retry) if (retry.size, retry.modified) == (size, modified) => retry.failures + 1,
            _ => 1,
        };
        let pause = RETRY_FIRST
            .saturating_mul(1 << (failures - 1).min(16))
            .min(RETRY_MAX);
        self.retries.insert(
            path.to_path_buf(),
            Retry {
                size,
                modified,
                failures,
                at: now + pause,
            },
        );
        failures == 1
    }

    pub fn folder(&self) -> &Path {
        &self.folder
    }

    /// Files (directly in the folder, accepted by `wanted`) that are complete
    /// and not taken before, oldest first. Each is returned once, unless it
    /// changes afterwards (a file replaced under the same name is new) or
    /// its import `failed`.
    pub fn poll(&mut self, now: Instant, wanted: impl Fn(&Path) -> bool) -> Vec<PathBuf> {
        let Ok(entries) = fs::read_dir(&self.folder) else {
            return Vec::new();
        };
        let mut present = HashMap::new();
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.') || is_partial(&name) || !wanted(&path) {
                continue;
            }
            let Ok(meta) = entry.metadata() else { continue };
            if !meta.is_file() {
                continue;
            }
            present.insert(path, (meta.len(), meta.modified().ok()));
        }
        self.seen.retain(|path, _| present.contains_key(path));
        self.taken.retain(|path, _| present.contains_key(path));
        self.retries.retain(|path, _| present.contains_key(path));
        let mut ready = Vec::new();
        for (path, (size, modified)) in present {
            if self.taken.get(&path) == Some(&(size, modified)) {
                continue;
            }
            if let Some(retry) = self.retries.get(&path) {
                // Unchanged since it failed: it had settled; offered again
                // once its pause is over.
                if (retry.size, retry.modified) == (size, modified) {
                    if now >= retry.at {
                        ready.push((modified, path, size));
                    }
                    continue;
                }
                // Changed: a new file, which settles as usual.
                self.retries.remove(&path);
            }
            // A file is taken at the earliest on its second sighting.
            let Some(entry) = self.seen.get_mut(&path) else {
                self.seen.insert(
                    path,
                    Seen {
                        size,
                        modified,
                        since: now,
                    },
                );
                continue;
            };
            if entry.size != size || entry.modified != modified {
                *entry = Seen {
                    size,
                    modified,
                    since: now,
                };
                continue;
            }
            if size > 0 && now.duration_since(entry.since) >= self.settle {
                ready.push((modified, path, size));
            }
        }
        ready.sort();
        ready
            .into_iter()
            .map(|(modified, path, size)| {
                self.taken.insert(path.clone(), (size, modified));
                self.seen.remove(&path);
                path
            })
            .collect()
    }
}

/// Names that downloaders and copy tools use while a file is incomplete.
fn is_partial(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    [".tmp", ".part", ".partial", ".crdownload", ".download", "~"]
        .iter()
        .any(|suffix| lower.ends_with(suffix))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(path: &Path) -> bool {
        path.extension().is_some_and(|e| e == "ARW")
    }

    #[test]
    fn takes_a_file_once_it_has_settled_and_only_once() {
        let dir = tempfile::tempdir().unwrap();
        let mut hot = HotFolder::new(dir.path(), Duration::from_millis(500));
        let t0 = Instant::now();
        fs::write(dir.path().join("A.ARW"), b"half").unwrap();
        assert!(hot.poll(t0, raw).is_empty(), "just appeared");
        // Still being written: it grows.
        fs::write(dir.path().join("A.ARW"), b"half and more").unwrap();
        assert!(hot.poll(t0 + Duration::from_millis(400), raw).is_empty());
        assert!(
            hot.poll(t0 + Duration::from_millis(800), raw).is_empty(),
            "changed at 400 ms, settles at 900 ms"
        );
        assert_eq!(
            hot.poll(t0 + Duration::from_millis(1000), raw),
            vec![dir.path().join("A.ARW")]
        );
        assert!(hot.poll(t0 + Duration::from_millis(2000), raw).is_empty());
    }

    #[test]
    fn ignores_hidden_partial_empty_and_unwanted_files() {
        let dir = tempfile::tempdir().unwrap();
        let mut hot = HotFolder::new(dir.path(), Duration::ZERO);
        for name in ["._A.ARW", "B.ARW.part", "C.txt", "D.ARW.tmp"] {
            fs::write(dir.path().join(name), b"x").unwrap();
        }
        fs::write(dir.path().join("E.ARW"), b"").unwrap();
        fs::create_dir(dir.path().join("F.ARW")).unwrap();
        let t0 = Instant::now();
        hot.poll(t0, raw);
        assert!(hot.poll(t0 + Duration::from_millis(10), raw).is_empty());
    }

    #[test]
    fn a_replaced_file_counts_again_and_returns_oldest_first() {
        let dir = tempfile::tempdir().unwrap();
        let mut hot = HotFolder::new(dir.path(), Duration::ZERO);
        let (a, b) = (dir.path().join("A.ARW"), dir.path().join("B.ARW"));
        fs::write(&b, b"b").unwrap();
        filetime_set(&b, 1_000);
        fs::write(&a, b"a").unwrap();
        filetime_set(&a, 2_000);
        let t0 = Instant::now();
        hot.poll(t0, raw);
        assert_eq!(hot.poll(t0, raw), vec![b.clone(), a.clone()]);
        fs::write(&a, b"a, second shot").unwrap();
        hot.poll(t0, raw);
        assert_eq!(hot.poll(t0, raw), vec![a]);
    }

    #[test]
    fn a_failed_file_is_offered_again_after_a_growing_pause() {
        let dir = tempfile::tempdir().unwrap();
        let mut hot = HotFolder::new(dir.path(), Duration::ZERO);
        let a = dir.path().join("A.ARW");
        fs::write(&a, b"a").unwrap();
        let t0 = Instant::now();
        hot.poll(t0, raw);
        assert_eq!(hot.poll(t0, raw), vec![a.clone()]);
        // The import failed (say the session was not writable).
        assert!(hot.failed(&a, t0), "the first failure is reported");
        assert!(
            hot.poll(t0 + RETRY_FIRST / 2, raw).is_empty(),
            "no hot loop"
        );
        let t1 = t0 + RETRY_FIRST;
        assert_eq!(
            hot.poll(t1, raw),
            vec![a.clone()],
            "unchanged, still retried"
        );
        assert!(!hot.failed(&a, t1), "reported once");
        assert!(
            hot.poll(t1 + RETRY_FIRST, raw).is_empty(),
            "the pause doubled"
        );
        let t2 = t1 + RETRY_FIRST * 2;
        assert_eq!(hot.poll(t2, raw), vec![a.clone()]);
        // Now it worked: taken for good.
        assert!(hot.poll(t2 + RETRY_MAX, raw).is_empty());

        // A changed file is new: it settles as usual and a failure of it is
        // reported again.
        fs::write(&a, b"a, second shot").unwrap();
        let t3 = t2 + RETRY_MAX;
        assert!(hot.poll(t3, raw).is_empty(), "first sighting");
        assert_eq!(hot.poll(t3, raw), vec![a.clone()]);
        assert!(hot.failed(&a, t3));
        fs::write(&a, b"a, third shot").unwrap();
        assert!(hot.poll(t3, raw).is_empty(), "changed: settles first");
        assert_eq!(
            hot.poll(t3, raw),
            vec![a.clone()],
            "no pause for a new file"
        );
    }

    fn filetime_set(path: &Path, secs: u64) {
        let file = fs::File::options().write(true).open(path).unwrap();
        file.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(secs))
            .unwrap();
    }
}
