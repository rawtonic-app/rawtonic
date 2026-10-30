//! Hot folder (Capture One's hot folder, Lightroom's Auto Import): which
//! files in a watched folder are complete and new. Files are taken once
//! their size and modification time have not changed for a settle time, so a
//! file still being written by tethering software or a card reader is left
//! alone. Polling keeps this portable and independent of file system events.
//!
//! Subfolders are watched when asked (`with_subfolders`); hidden files and
//! folders are skipped as in ingest, and symbolic links are never followed,
//! so the watch never leaves the watched tree. A watched folder that is gone
//! (an unplugged drive) makes `poll` fail without forgetting anything; the
//! watch goes on when it is back. The files taken can be kept
//! (`taken_file`) and given to a later watch (`restore_taken`), so files
//! that arrived in between are taken once and the earlier ones not again.
use rawtonic_catalog::{HotFolderFile, validate_relative};
use std::{
    collections::HashMap,
    fs, io,
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime},
};

/// How long a file must stay unchanged before it is taken.
pub const DEFAULT_SETTLE: Duration = Duration::from_millis(1500);

/// The pause before a file whose import failed is offered again; it doubles
/// with every further failure, up to `RETRY_MAX`.
pub const RETRY_FIRST: Duration = Duration::from_secs(5);
pub const RETRY_MAX: Duration = Duration::from_secs(300);

/// How deep subfolders are watched, against runaway trees.
pub const MAX_DEPTH: usize = 16;

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
    subfolders: bool,
    seen: HashMap<PathBuf, Seen>,
    taken: HashMap<PathBuf, (u64, Option<SystemTime>)>,
    retries: HashMap<PathBuf, Retry>,
    /// Taken files no longer in the folder, since `take_forgotten`.
    forgotten: Vec<PathBuf>,
}

impl HotFolder {
    pub fn new(folder: impl Into<PathBuf>, settle: Duration) -> Self {
        Self {
            folder: folder.into(),
            settle,
            subfolders: false,
            seen: HashMap::new(),
            taken: HashMap::new(),
            retries: HashMap::new(),
            forgotten: Vec::new(),
        }
    }

    /// Watches the folders inside the watched folder too (not hidden ones,
    /// not through symbolic links, up to `MAX_DEPTH` deep).
    pub fn with_subfolders(mut self, subfolders: bool) -> Self {
        self.subfolders = subfolders;
        self
    }

    /// Files an earlier watch of this folder took: they are not offered
    /// again unless they changed since.
    pub fn restore_taken(&mut self, files: impl IntoIterator<Item = HotFolderFile>) {
        for file in files {
            // Stored paths come back from a database row, not from a disk
            // walk: validate them as the stored strings they are (`/`
            // separators, no `\\`, no drive or UNC prefix, no `.`/`..`), then
            // push each name as one component. Pushing the raw string would
            // let a stored `nested\..\outside` leave the watched folder on
            // Windows, where `\` separates.
            if validate_relative(Path::new(&file.rel_path)).is_err()
                || file
                    .rel_path
                    .split('/')
                    .any(|part| part.is_empty() || part == "." || part == "..")
            {
                continue;
            }
            let mut path = self.folder.clone();
            for part in file.rel_path.split('/') {
                path.push(part);
            }
            self.taken
                .insert(path, (file.size, file.mtime_ns.map(from_nanos)));
        }
    }

    /// What to keep of a file `poll` returned once it is in the library (or
    /// a verified duplicate), for `restore_taken`. `None` for a path that is
    /// not valid Unicode (it is then checked as a duplicate next time).
    pub fn taken_file(&self, path: &Path) -> Option<HotFolderFile> {
        let (size, modified) = *self.taken.get(path)?;
        let relative = path.strip_prefix(&self.folder).ok()?;
        let rel_path = rawtonic_catalog::portable_relative(relative).ok()?;
        Some(HotFolderFile {
            rel_path,
            size,
            mtime_ns: modified.and_then(to_nanos),
        })
    }

    /// Taken files that left the folder since the last call, as relative
    /// paths of `taken_file` (to forget them where they are kept).
    pub fn take_forgotten(&mut self) -> Vec<String> {
        let folder = self.folder.clone();
        std::mem::take(&mut self.forgotten)
            .into_iter()
            .filter_map(|path| {
                let relative = path.strip_prefix(&folder).ok()?;
                rawtonic_catalog::portable_relative(relative).ok()
            })
            .collect()
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

    /// The files in `dir` (and, with subfolders, below it) `wanted` accepts.
    /// Folders that cannot be read are listed in `unreadable`.
    fn list(
        &self,
        dir: &Path,
        depth: usize,
        wanted: &dyn Fn(&Path) -> bool,
        present: &mut HashMap<PathBuf, (u64, Option<SystemTime>)>,
        unreadable: &mut Vec<PathBuf>,
    ) -> io::Result<()> {
        for entry in fs::read_dir(dir)?.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.') {
                continue;
            }
            // Does not follow symbolic links.
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let path = entry.path();
            if kind.is_dir() {
                if self.subfolders
                    && depth < MAX_DEPTH
                    && self
                        .list(&path, depth + 1, wanted, present, unreadable)
                        .is_err()
                {
                    unreadable.push(path);
                }
                continue;
            }
            if !kind.is_file() || is_partial(&name) || !wanted(&path) {
                continue;
            }
            let Ok(meta) = fs::symlink_metadata(&path) else {
                continue;
            };
            if meta.is_file() {
                present.insert(path, (meta.len(), meta.modified().ok()));
            }
        }
        Ok(())
    }

    /// Files (accepted by `wanted`) that are complete and not taken before,
    /// oldest first. Each is returned once, unless it changes afterwards (a
    /// file replaced under the same name is new) or its import `failed`.
    /// Fails, forgetting nothing, when the watched folder cannot be read
    /// (it is gone, say its drive was unplugged).
    pub fn poll(
        &mut self,
        now: Instant,
        wanted: impl Fn(&Path) -> bool,
    ) -> io::Result<Vec<PathBuf>> {
        let mut present = HashMap::new();
        let mut unreadable = Vec::new();
        let folder = self.folder.clone();
        self.list(&folder, 0, &wanted, &mut present, &mut unreadable)?;
        // What is in a folder that could not be read this time is kept.
        let keep = |path: &PathBuf| {
            present.contains_key(path) || unreadable.iter().any(|dir| path.starts_with(dir))
        };
        self.seen.retain(|path, _| keep(path));
        let forgotten: Vec<PathBuf> = self.taken.keys().filter(|p| !keep(p)).cloned().collect();
        for path in &forgotten {
            self.taken.remove(path);
        }
        self.forgotten.extend(forgotten);
        self.retries.retain(|path, _| keep(path));
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
        Ok(ready
            .into_iter()
            .map(|(modified, path, size)| {
                self.taken.insert(path.clone(), (size, modified));
                self.seen.remove(&path);
                path
            })
            .collect())
    }
}

fn to_nanos(time: SystemTime) -> Option<i64> {
    match time.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(after) => i64::try_from(after.as_nanos()).ok(),
        Err(before) => i64::try_from(before.duration().as_nanos()).ok().map(|n| -n),
    }
}

fn from_nanos(nanos: i64) -> SystemTime {
    let magnitude = Duration::from_nanos(nanos.unsigned_abs());
    if nanos >= 0 {
        SystemTime::UNIX_EPOCH + magnitude
    } else {
        SystemTime::UNIX_EPOCH - magnitude
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
        assert!(hot.poll(t0, raw).unwrap().is_empty(), "just appeared");
        // Still being written: it grows.
        fs::write(dir.path().join("A.ARW"), b"half and more").unwrap();
        assert!(
            hot.poll(t0 + Duration::from_millis(400), raw)
                .unwrap()
                .is_empty()
        );
        assert!(
            hot.poll(t0 + Duration::from_millis(800), raw)
                .unwrap()
                .is_empty(),
            "changed at 400 ms, settles at 900 ms"
        );
        assert_eq!(
            hot.poll(t0 + Duration::from_millis(1000), raw).unwrap(),
            vec![dir.path().join("A.ARW")]
        );
        assert!(
            hot.poll(t0 + Duration::from_millis(2000), raw)
                .unwrap()
                .is_empty()
        );
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
        hot.poll(t0, raw).unwrap();
        assert!(
            hot.poll(t0 + Duration::from_millis(10), raw)
                .unwrap()
                .is_empty()
        );
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
        hot.poll(t0, raw).unwrap();
        assert_eq!(hot.poll(t0, raw).unwrap(), vec![b.clone(), a.clone()]);
        fs::write(&a, b"a, second shot").unwrap();
        hot.poll(t0, raw).unwrap();
        assert_eq!(hot.poll(t0, raw).unwrap(), vec![a]);
    }

    #[test]
    fn a_failed_file_is_offered_again_after_a_growing_pause() {
        let dir = tempfile::tempdir().unwrap();
        let mut hot = HotFolder::new(dir.path(), Duration::ZERO);
        let a = dir.path().join("A.ARW");
        fs::write(&a, b"a").unwrap();
        let t0 = Instant::now();
        hot.poll(t0, raw).unwrap();
        assert_eq!(hot.poll(t0, raw).unwrap(), vec![a.clone()]);
        // The import failed (say the session was not writable).
        assert!(hot.failed(&a, t0), "the first failure is reported");
        assert!(
            hot.poll(t0 + RETRY_FIRST / 2, raw).unwrap().is_empty(),
            "no hot loop"
        );
        let t1 = t0 + RETRY_FIRST;
        assert_eq!(
            hot.poll(t1, raw).unwrap(),
            vec![a.clone()],
            "unchanged, still retried"
        );
        assert!(!hot.failed(&a, t1), "reported once");
        assert!(
            hot.poll(t1 + RETRY_FIRST, raw).unwrap().is_empty(),
            "the pause doubled"
        );
        let t2 = t1 + RETRY_FIRST * 2;
        assert_eq!(hot.poll(t2, raw).unwrap(), vec![a.clone()]);
        // Now it worked: taken for good.
        assert!(hot.poll(t2 + RETRY_MAX, raw).unwrap().is_empty());

        // A changed file is new: it settles as usual and a failure of it is
        // reported again.
        fs::write(&a, b"a, second shot").unwrap();
        let t3 = t2 + RETRY_MAX;
        assert!(hot.poll(t3, raw).unwrap().is_empty(), "first sighting");
        assert_eq!(hot.poll(t3, raw).unwrap(), vec![a.clone()]);
        assert!(hot.failed(&a, t3));
        fs::write(&a, b"a, third shot").unwrap();
        assert!(
            hot.poll(t3, raw).unwrap().is_empty(),
            "changed: settles first"
        );
        assert_eq!(
            hot.poll(t3, raw).unwrap(),
            vec![a.clone()],
            "no pause for a new file"
        );
    }

    #[test]
    fn subfolders_are_watched_only_when_asked_and_hidden_ones_never() {
        let dir = tempfile::tempdir().unwrap();
        let day = dir.path().join("2026-10-16");
        fs::create_dir_all(day.join("card")).unwrap();
        fs::create_dir_all(dir.path().join(".hidden")).unwrap();
        fs::write(dir.path().join("A.ARW"), b"a").unwrap();
        fs::write(day.join("B.ARW"), b"b").unwrap();
        fs::write(day.join("card/C.ARW"), b"c").unwrap();
        fs::write(day.join("._C.ARW"), b"x").unwrap();
        fs::write(dir.path().join(".hidden/D.ARW"), b"d").unwrap();
        let t0 = Instant::now();

        let mut flat = HotFolder::new(dir.path(), Duration::ZERO);
        flat.poll(t0, raw).unwrap();
        assert_eq!(flat.poll(t0, raw).unwrap(), [dir.path().join("A.ARW")]);

        let mut deep = HotFolder::new(dir.path(), Duration::ZERO).with_subfolders(true);
        deep.poll(t0, raw).unwrap();
        let mut taken = deep.poll(t0, raw).unwrap();
        taken.sort();
        assert_eq!(
            taken,
            [
                day.join("B.ARW"),
                day.join("card/C.ARW"),
                dir.path().join("A.ARW")
            ]
        );
        assert_eq!(
            deep.taken_file(&day.join("card/C.ARW")).unwrap().rel_path,
            "2026-10-16/card/C.ARW"
        );
    }

    #[cfg(unix)]
    #[test]
    fn symbolic_links_are_never_followed() {
        let dir = tempfile::tempdir().unwrap();
        let watched = dir.path().join("Hot");
        let outside = dir.path().join("Elsewhere");
        fs::create_dir_all(&watched).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("X.ARW"), b"outside").unwrap();
        std::os::unix::fs::symlink(&outside, watched.join("link")).unwrap();
        std::os::unix::fs::symlink(outside.join("X.ARW"), watched.join("Y.ARW")).unwrap();
        // A loop back into the watched tree.
        std::os::unix::fs::symlink(&watched, watched.join("loop")).unwrap();
        fs::write(watched.join("A.ARW"), b"a").unwrap();
        let mut hot = HotFolder::new(&watched, Duration::ZERO).with_subfolders(true);
        let t0 = Instant::now();
        hot.poll(t0, raw).unwrap();
        assert_eq!(hot.poll(t0, raw).unwrap(), [watched.join("A.ARW")]);
    }

    /// An unplugged drive: polls fail and nothing is forgotten, so nothing
    /// taken before is offered again once it is back; what arrived while it
    /// was away is taken then.
    #[test]
    fn a_folder_that_is_gone_forgets_nothing_and_resumes() {
        let dir = tempfile::tempdir().unwrap();
        let watched = dir.path().join("Card");
        let away = dir.path().join("Unplugged");
        fs::create_dir(&watched).unwrap();
        fs::write(watched.join("A.ARW"), b"a").unwrap();
        let mut hot = HotFolder::new(&watched, Duration::ZERO);
        let t0 = Instant::now();
        hot.poll(t0, raw).unwrap();
        assert_eq!(hot.poll(t0, raw).unwrap(), [watched.join("A.ARW")]);
        fs::rename(&watched, &away).unwrap();
        assert!(hot.poll(t0, raw).is_err());
        assert!(hot.poll(t0, raw).is_err());
        assert!(hot.take_forgotten().is_empty());
        fs::write(away.join("B.ARW"), b"b").unwrap();
        fs::rename(&away, &watched).unwrap();
        assert!(hot.poll(t0, raw).unwrap().is_empty(), "B: first sighting");
        assert_eq!(hot.poll(t0, raw).unwrap(), [watched.join("B.ARW")]);
    }

    /// A later watch given what an earlier one took (a restart) takes only
    /// the files that arrived or changed in between, each once.
    #[test]
    fn a_restored_watch_takes_only_what_arrived_since() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();
        let (a, b, c) = (
            dir.path().join("A.ARW"),
            dir.path().join("sub/B.ARW"),
            dir.path().join("C.ARW"),
        );
        fs::write(&a, b"a").unwrap();
        fs::write(&b, b"b").unwrap();
        let t0 = Instant::now();
        let mut first = HotFolder::new(dir.path(), Duration::ZERO).with_subfolders(true);
        first.poll(t0, raw).unwrap();
        let taken: Vec<HotFolderFile> = first
            .poll(t0, raw)
            .unwrap()
            .iter()
            .map(|p| first.taken_file(p).unwrap())
            .collect();
        assert_eq!(taken.len(), 2);
        drop(first);

        // While the app was closed: C arrived, B was replaced, A stayed.
        fs::write(&c, b"c").unwrap();
        fs::write(&b, b"b, second shot").unwrap();
        let mut second = HotFolder::new(dir.path(), Duration::ZERO).with_subfolders(true);
        second.restore_taken(taken.clone());
        second.poll(t0, raw).unwrap();
        let mut again = second.poll(t0, raw).unwrap();
        again.sort();
        assert_eq!(again, [c.clone(), b.clone()]);
        assert!(second.poll(t0, raw).unwrap().is_empty(), "once");

        // A file gone is forgotten (to be dropped where it is kept).
        fs::remove_file(&a).unwrap();
        second.poll(t0, raw).unwrap();
        assert_eq!(second.take_forgotten(), ["A.ARW"]);

        // Stored paths never lead out of the folder.
        let mut third = HotFolder::new(dir.path(), Duration::ZERO);
        third.restore_taken([HotFolderFile {
            rel_path: "../C.ARW".into(),
            size: 1,
            mtime_ns: None,
        }]);
        assert!(third.taken.is_empty());
    }

    /// Stored paths come from the database, so `restore_taken` treats them as
    /// untrusted: a backslash (which on Windows separates components) or a
    /// drive/UNC prefix never becomes a path outside the watched folder.
    /// Legitimate stored paths, nested ones included, still restore.
    #[test]
    fn a_restored_watch_ignores_stored_paths_that_lead_out_of_the_folder() {
        let parent = tempfile::tempdir().unwrap();
        let folder = parent.path().join("Watched");
        fs::create_dir_all(&folder).unwrap();
        // Something outside the watched folder a stored path could name.
        fs::write(parent.path().join("outside.ARW"), b"outside").unwrap();
        let file = |rel_path: &str| HotFolderFile {
            rel_path: rel_path.into(),
            size: 1,
            mtime_ns: None,
        };
        let mut hot = HotFolder::new(&folder, Duration::ZERO).with_subfolders(true);
        hot.restore_taken([
            // A backslash would push a Windows component out of the folder.
            file(r"nested\..\outside.ARW"),
            file(r"..\outside.ARW"),
            file(r"C:\outside.ARW"),
            file(r"\\server\share\outside.ARW"),
            // Empty, dot and parent parts were refused before, too.
            file(""),
            file("."),
            file(".."),
            file("nested/../../outside.ARW"),
            file("nested//A.ARW"),
            file("nested/./A.ARW"),
            file("/outside.ARW"),
        ]);
        assert!(hot.taken.is_empty(), "{:?}", hot.taken.keys());
        assert!(
            hot.taken
                .keys()
                .all(|path| path.starts_with(&folder) && path != &folder)
        );
        // Legitimate restoration stays.
        hot.restore_taken([file("A.ARW"), file("nested/Day 2/B.ARW")]);
        assert_eq!(
            hot.taken
                .keys()
                .map(|path| path.strip_prefix(&folder).unwrap().to_path_buf())
                .collect::<std::collections::BTreeSet<_>>(),
            [
                PathBuf::from("A.ARW"),
                PathBuf::from("nested").join("Day 2").join("B.ARW")
            ]
            .into_iter()
            .collect()
        );
    }

    #[test]
    fn modification_times_survive_being_kept() {
        for time in [
            SystemTime::UNIX_EPOCH + Duration::new(1_760_000_000, 123_456_789),
            SystemTime::UNIX_EPOCH - Duration::new(10, 5),
        ] {
            assert_eq!(from_nanos(to_nanos(time).unwrap()), time);
        }
    }

    fn filetime_set(path: &Path, secs: u64) {
        let file = fs::File::options().write(true).open(path).unwrap();
        file.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(secs))
            .unwrap();
    }
}
