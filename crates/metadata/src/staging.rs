//! Staging folders of jobs that build something before publishing it
//! (Export as Catalog's catalog, Import from Catalog's copied originals and
//! its snapshot of the other catalog).
//!
//! A job that is killed leaves its staging folder behind; it holds only what
//! the job copied, but that can be many gigabytes. Each staging folder made
//! here carries a marker file ([`MARKER`]: who made it) that its job holds an
//! exclusive lock on for as long as it runs. [`remove_abandoned`] removes a
//! staging folder only when all of this holds (user decision 2026-10-31,
//! G66 app kill test):
//! - its name has the job's prefix and it carries the marker (a folder
//!   without one, e.g. from an older version, is left and reported);
//! - the marker's lock can be taken: no job, in this or another process,
//!   is using it;
//! - everything in it is a plain file or folder on the same drive (no
//!   symlink is followed or removed; anything else keeps the whole folder).

use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

/// The marker file inside every staging folder.
pub const MARKER: &str = ".rawtonic-staging";
/// Export as Catalog: the catalog being built, beside its destination.
pub const CATALOG_EXPORT: &str = ".rawtonic-export-";
/// Import from Catalog: the copied originals, inside the folder they go to.
pub const CATALOG_IMPORT: &str = ".rawtonic-import-";
/// Import from Catalog: the snapshot of the other catalog, in the `.cache`
/// folder of the catalog imported into.
pub const CATALOG_SNAPSHOT: &str = "import-";

/// A staging folder of a running job; removed when dropped unless published.
pub struct Staging {
    dir: Option<tempfile::TempDir>,
    lock: Option<fs::File>,
}

impl Staging {
    /// Makes a staging folder in `parent` named `<prefix>XXXXXX`, marked
    /// with `owner` (the job and library, for whoever finds it) and locked.
    pub fn new_in(parent: &Path, prefix: &str, owner: &str) -> io::Result<Self> {
        let dir = tempfile::Builder::new().prefix(prefix).tempdir_in(parent)?;
        let mut marker = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(dir.path().join(MARKER))?;
        marker.try_lock().map_err(io::Error::other)?;
        marker.write_all(owner.as_bytes())?;
        marker.sync_all()?;
        Ok(Self {
            dir: Some(dir),
            lock: Some(marker),
        })
    }

    pub fn path(&self) -> &Path {
        self.dir.as_ref().expect("staging folder").path()
    }

    /// Before the folder itself is published (renamed into place): the
    /// marker goes, so the published folder carries none. Returns the
    /// folder, still removed when dropped.
    pub fn unmark(mut self) -> io::Result<tempfile::TempDir> {
        let dir = self.dir.take().expect("staging folder");
        drop(self.lock.take());
        fs::remove_file(dir.path().join(MARKER))?;
        Ok(dir)
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        // The lock goes before the folder (a locked file cannot be removed
        // on Windows); the TempDir then removes everything.
        drop(self.lock.take());
        drop(self.dir.take());
    }
}

/// What [`remove_abandoned`] did.
#[derive(Debug, Default, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Removed {
    /// Staging folders removed.
    pub folders: usize,
    /// Bytes they held.
    pub bytes: u64,
    /// Folders with the prefix that were left: no marker, in use, or holding
    /// something other than plain files and folders on the same drive.
    pub kept: Vec<PathBuf>,
}

impl Removed {
    pub fn add(&mut self, other: Removed) {
        self.folders += other.folders;
        self.bytes += other.bytes;
        self.kept.extend(other.kept);
    }
}

/// Removes the abandoned staging folders named `<prefix>…` directly in
/// `parent` (see the module documentation for when one counts as abandoned).
pub fn remove_abandoned(parent: &Path, prefix: &str) -> Removed {
    let mut removed = Removed::default();
    let Ok(entries) = fs::read_dir(parent) else {
        return removed;
    };
    for entry in entries.flatten() {
        if !entry
            .file_name()
            .to_str()
            .is_some_and(|n| n.starts_with(prefix))
        {
            continue;
        }
        let path = entry.path();
        match remove_if_abandoned(&path) {
            Ok(Some(bytes)) => {
                removed.folders += 1;
                removed.bytes += bytes;
            }
            Ok(None) | Err(_) => removed.kept.push(path),
        }
    }
    removed
}

/// Removes `dir` if it is an abandoned staging folder; `Some(bytes)` if it
/// was removed, `None` if it is not one (or not abandoned).
pub fn remove_if_abandoned(dir: &Path) -> io::Result<Option<u64>> {
    let meta = fs::symlink_metadata(dir)?;
    if !meta.file_type().is_dir() {
        return Ok(None);
    }
    let marker = dir.join(MARKER);
    if !fs::symlink_metadata(&marker).is_ok_and(|m| m.file_type().is_file()) {
        return Ok(None);
    }
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&marker)?;
    if lock.try_lock().is_err() {
        return Ok(None);
    }
    let mut files = Vec::new();
    let mut folders = Vec::new();
    let mut bytes = 0;
    let mut pending = vec![dir.to_path_buf()];
    while let Some(folder) = pending.pop() {
        for entry in fs::read_dir(&folder)? {
            let entry = entry?;
            let path = entry.path();
            let meta = fs::symlink_metadata(&path)?;
            if !same_drive(&meta, dir)? {
                return Ok(None);
            }
            let kind = meta.file_type();
            if kind.is_dir() {
                pending.push(path.clone());
                folders.push(path);
            } else if kind.is_file() {
                bytes += meta.len();
                if path != marker {
                    files.push(path);
                }
            } else {
                return Ok(None);
            }
        }
    }
    for file in files {
        fs::remove_file(file)?;
    }
    // Deepest first.
    folders.sort_by_key(|f| std::cmp::Reverse(f.components().count()));
    for folder in folders {
        fs::remove_dir(folder)?;
    }
    drop(lock);
    fs::remove_file(&marker)?;
    fs::remove_dir(dir)?;
    Ok(Some(bytes))
}

#[cfg(unix)]
fn same_drive(meta: &fs::Metadata, dir: &Path) -> io::Result<bool> {
    use std::os::unix::fs::MetadataExt;
    Ok(meta.dev() == fs::symlink_metadata(dir)?.dev())
}

#[cfg(not(unix))]
fn same_drive(_meta: &fs::Metadata, _dir: &Path) -> io::Result<bool> {
    // A mount point inside the folder is a reparse point there, not a
    // plain folder, so the file-type check already keeps it.
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fill(dir: &Path) {
        fs::create_dir_all(dir.join("Photos/2026")).unwrap();
        fs::write(dir.join("Photos/2026/a.nef"), [7u8; 1000]).unwrap();
        fs::write(dir.join("catalog.sqlite"), [1u8; 24]).unwrap();
    }

    #[test]
    fn a_killed_jobs_staging_folder_is_removed_and_a_running_ones_kept() {
        let parent = tempfile::tempdir().unwrap();
        // What a killed job leaves: its folder, the marker, no lock.
        let killed = Staging::new_in(parent.path(), ".rawtonic-export-", "test").unwrap();
        fill(killed.path());
        let left = killed.dir.as_ref().unwrap().path().to_path_buf();
        let mut killed = killed;
        drop(killed.lock.take());
        std::mem::forget(killed.dir.take().unwrap()); // as SIGKILL: nothing cleaned up
        let running = Staging::new_in(parent.path(), ".rawtonic-export-", "test").unwrap();
        fill(running.path());
        let removed = remove_abandoned(parent.path(), ".rawtonic-export-");
        assert_eq!(removed.folders, 1);
        assert!(removed.bytes >= 1024);
        assert!(!left.exists());
        assert!(
            running.path().join("Photos/2026/a.nef").exists(),
            "in use: kept"
        );
        assert_eq!(removed.kept, vec![running.path().to_path_buf()]);
    }

    #[test]
    fn a_folder_without_marker_or_with_other_things_is_kept() {
        let parent = tempfile::tempdir().unwrap();
        let unmarked = parent.path().join(".rawtonic-import-old123");
        fill(&unmarked);
        let marked = parent.path().join(".rawtonic-import-lnk456");
        fill(&marked);
        fs::write(marked.join(MARKER), b"test").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(parent.path(), marked.join("elsewhere")).unwrap();
        let removed = remove_abandoned(parent.path(), ".rawtonic-import-");
        assert_eq!(removed.folders, if cfg!(unix) { 0 } else { 1 });
        assert!(unmarked.join("Photos/2026/a.nef").exists());
        #[cfg(unix)]
        assert!(
            marked.join("Photos/2026/a.nef").exists(),
            "never through a symlink"
        );
    }

    #[test]
    fn a_published_folder_carries_no_marker() {
        let parent = tempfile::tempdir().unwrap();
        let staging = Staging::new_in(parent.path(), ".rawtonic-export-", "test").unwrap();
        fill(staging.path());
        let dir = staging.unmark().unwrap();
        let published = parent.path().join("Catalog");
        fs::rename(dir.path(), &published).unwrap();
        let _ = dir.keep();
        assert!(!published.join(MARKER).exists());
        assert!(published.join("catalog.sqlite").exists());
        assert_eq!(remove_abandoned(parent.path(), "Catalog").folders, 0);
    }
}
