//! Removes what a killed app left of its writes in the library's folders
//! (G66 app kill test):
//! - temporary files of sidecar writes, exports into Output, ingest copies
//!   and cached previews: only files with Rawtonic's temporary-file prefix
//!   older than `rawtonic_metadata::temp::STALE_AGE`;
//!   (also directly in the backup folders of unfinished ingest jobs);
//! - abandoned staging folders of Import from Catalog (its copied originals
//!   and its snapshot of the other catalog): only marked ones no job holds
//!   (`rawtonic_metadata::staging`).

use crate::Library;
use anyhow::Result;
use rawtonic_metadata::{staging, temp};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::SystemTime,
};

/// What [`Library::sweep_leftovers`] removed.
#[derive(Debug, Default, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Swept {
    /// Temporary files removed, and their bytes.
    pub temp_files: usize,
    pub temp_bytes: u64,
    /// Staging folders removed, their bytes, and those left in place.
    pub staging: staging::Removed,
}

impl Library {
    /// Sweeps the library folder (its caches too, not libraries nested
    /// inside it) and the external folders it links. Symlinked folders are
    /// not followed.
    pub fn sweep_leftovers(&self, now: SystemTime, cancel: &AtomicBool) -> Result<Swept> {
        let mut folders = vec![self.directory.clone()];
        let mut roots = self
            .conn
            .prepare("SELECT path FROM roots WHERE external=1")?;
        folders.extend(
            roots
                .query_map([], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?
                .into_iter()
                .map(PathBuf::from),
        );
        let mut swept = Swept::default();
        swept.staging.add(staging::remove_abandoned(
            &self.directory.join(".cache"),
            staging::CATALOG_SNAPSHOT,
        ));
        for folder in folders.iter().filter(|f| f.is_dir()) {
            sweep_tree(folder, &self.directory, now, cancel, &mut swept);
        }
        // An interrupted ingest copies into its backup folder too: that
        // folder only (it may hold anything; only our temporary files go).
        for backup in self.ingest_backup_folders()? {
            if cancel.load(Ordering::Acquire) {
                break;
            }
            swept.add_temp(temp::sweep_folder(&backup, now));
        }
        Ok(swept)
    }

    /// The backup folders of the ingest jobs not finished yet.
    fn ingest_backup_folders(&self) -> Result<Vec<PathBuf>> {
        let mut folders: Vec<PathBuf> = self
            .unfinished_ingest_jobs()?
            .into_iter()
            .filter_map(|job| job.options.get("backup")?.as_str().map(PathBuf::from))
            .filter(|folder| folder.is_absolute())
            .collect();
        folders.sort();
        folders.dedup();
        Ok(folders)
    }
}

impl Swept {
    fn add_temp(&mut self, removed: temp::Removed) {
        self.temp_files += removed.files;
        self.temp_bytes += removed.bytes;
    }
}

fn sweep_tree(top: &Path, library: &Path, now: SystemTime, cancel: &AtomicBool, swept: &mut Swept) {
    let mut pending = vec![top.to_path_buf()];
    while let Some(folder) = pending.pop() {
        if cancel.load(Ordering::Acquire) {
            break;
        }
        if folder != library
            && (folder.join("session.json").is_file() || folder.join("catalog.json").is_file())
        {
            continue;
        }
        swept.add_temp(temp::sweep_folder(&folder, now));
        swept
            .staging
            .add(staging::remove_abandoned(&folder, staging::CATALOG_IMPORT));
        let Ok(entries) = fs::read_dir(&folder) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            // A staging folder left in place is not looked into.
            let staged = entry
                .file_name()
                .to_str()
                .is_some_and(|n| n.starts_with(staging::CATALOG_IMPORT));
            if kind.is_dir() && !staged {
                pending.push(entry.path());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Mode;
    use std::time::Duration;

    #[test]
    fn a_killed_write_leaves_nothing_after_the_next_sweep() {
        let dir = tempfile::tempdir().unwrap();
        let lib = Library::create(&dir.path().join("Job"), "Job", Mode::Session).unwrap();
        let capture = lib.directory.join("Capture");
        let output = lib.directory.join("Output").join("A");
        let previews = lib.directory.join(".cache").join("previews");
        fs::create_dir_all(&output).unwrap();
        fs::create_dir_all(&previews).unwrap();
        // What atomic_write and an export leave when the app dies before the rename.
        let sidecar = temp::file_for(&capture.join("a.nef.rrdata"))
            .unwrap()
            .keep()
            .unwrap()
            .1;
        let export = temp::file_for(&output.join("a.jpg"))
            .unwrap()
            .keep()
            .unwrap()
            .1;
        let preview = temp::file_for(&previews.join("a.rtpx"))
            .unwrap()
            .keep()
            .unwrap()
            .1;
        let foreign = capture.join(".tmpAbC123");
        fs::write(&foreign, b"another program's").unwrap();
        let never = AtomicBool::new(false);
        let fresh = lib.sweep_leftovers(SystemTime::now(), &never).unwrap();
        assert_eq!(fresh.temp_files, 0, "fresh: may still be written");
        let later = SystemTime::now() + temp::STALE_AGE + Duration::from_secs(1);
        assert_eq!(lib.sweep_leftovers(later, &never).unwrap().temp_files, 3);
        assert!(!sidecar.exists() && !export.exists() && !preview.exists());
        assert!(foreign.exists());
    }

    #[test]
    fn a_killed_ingest_leaves_nothing_in_its_backup_folder_and_the_size_is_counted() {
        let dir = tempfile::tempdir().unwrap();
        let lib = Library::create(&dir.path().join("Job"), "Job", Mode::Session).unwrap();
        let card = dir.path().join("card");
        let backup = dir.path().join("Backup");
        let finished_backup = dir.path().join("Old backup");
        for folder in [&card, &backup, &finished_backup] {
            fs::create_dir_all(folder).unwrap();
        }
        fs::write(card.join("C1.ARW"), b"raw").unwrap();
        let item = [(
            crate::ingest_jobs::SourceFile::read(&card.join("C1.ARW")),
            None,
        )];
        lib.create_ingest_job(&serde_json::json!({ "backup": backup }), &item)
            .unwrap();
        let done = lib
            .create_ingest_job(&serde_json::json!({ "backup": finished_backup }), &item)
            .unwrap();
        lib.set_ingest_job_state(&done, crate::ingest_jobs::JobState::Cancelled)
            .unwrap();
        let partial = |folder: &Path| {
            let mut file = temp::file_for(&folder.join("C1_1.ARW")).unwrap();
            std::io::Write::write_all(&mut file, &[7; 2048]).unwrap();
            file.keep().unwrap().1
        };
        let left = partial(&backup);
        let not_ours_to_sweep = partial(&finished_backup);
        let user_file = backup.join("Notes.txt");
        fs::write(&user_file, b"mine").unwrap();
        let later = SystemTime::now() + temp::STALE_AGE + Duration::from_secs(1);
        let swept = lib.sweep_leftovers(later, &AtomicBool::new(false)).unwrap();
        assert_eq!((swept.temp_files, swept.temp_bytes), (1, 2048));
        assert!(!left.exists());
        assert!(user_file.exists() && not_ours_to_sweep.exists());
    }

    #[test]
    fn a_killed_import_from_catalog_leaves_no_copies_or_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let lib = Library::create(&dir.path().join("Cat"), "Cat", Mode::Catalog).unwrap();
        let into = lib.directory.join("Photos").join("From elsewhere");
        let cache = lib.directory.join(".cache");
        // What a killed import leaves: its marked folders, no process
        // holding their locks any more.
        let mut left = Vec::new();
        for (parent, prefix) in [
            (&into, staging::CATALOG_IMPORT),
            (&cache, staging::CATALOG_SNAPSHOT),
        ] {
            let stage = parent.join(format!("{prefix}AbC123"));
            fs::create_dir_all(stage.join("Capture")).unwrap();
            fs::write(stage.join(staging::MARKER), b"Import from Catalog").unwrap();
            fs::write(stage.join("Capture/a.nef"), [1u8; 500]).unwrap();
            left.push(stage);
        }
        // An import running meanwhile keeps its own.
        let running = staging::Staging::new_in(&into, staging::CATALOG_IMPORT, "running").unwrap();
        let swept = lib
            .sweep_leftovers(SystemTime::now(), &AtomicBool::new(false))
            .unwrap();
        assert_eq!(swept.staging.folders, 2, "{swept:?}");
        assert!(swept.staging.bytes >= 1000);
        for folder in left {
            assert!(!folder.exists());
        }
        assert!(running.path().exists());
        assert_eq!(swept.staging.kept, vec![running.path().to_path_buf()]);
    }
}
