//! Removes what a killed app left of its writes in the library's folders
//! (G66 app kill test):
//! - temporary files of sidecar writes, exports into Output, ingest copies
//!   and cached previews: only files with Rawtonic's temporary-file prefix
//!   older than `rawtonic_metadata::temp::STALE_AGE`;
//! - those too directly in the backup folders of unfinished ingest jobs,
//!   wherever they are: a backup copy is named after the photograph being
//!   imported (`DSC_0001.ARW`, though the library's copy is `DSC_0001_1.ARW`),
//!   so a resumed job never recognises the stale leaves of a backup it kept;
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
    /// inside it), the external folders it links, and the backup folders of
    /// its unfinished ingest jobs. Symlinked folders are not followed.
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
        // The backup folders of unfinished ingest jobs, wherever they are
        // (one may be on another drive): only our stale temporary files go,
        // never what else they hold, and nothing in them is looked into. One
        // inside the library's tree may also be reached by the tree sweep
        // (not inside a nested library, which it skips); a file removed there
        // is gone here, so nothing is counted twice.
        let mut backups = Vec::new();
        for job in self.unfinished_ingest_jobs()? {
            let Some(backup) = job
                .options
                .get("backup")
                .and_then(|b| b.as_str())
                .map(PathBuf::from)
                .filter(|folder| folder.is_absolute())
            else {
                continue;
            };
            if let Ok(backup) = backup.canonicalize()
                && !backups.contains(&backup)
            {
                backups.push(backup);
            }
        }
        for folder in folders.iter().filter(|f| f.is_dir()) {
            sweep_tree(folder, &self.directory, now, cancel, &mut swept);
        }
        for backup in backups {
            if cancel.load(Ordering::Acquire) {
                break;
            }
            swept.add_temp(temp::sweep_folder(&backup, now));
        }
        Ok(swept)
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
        let swept = lib.sweep_leftovers(later, &never).unwrap();
        assert_eq!(swept.temp_files, 3);
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

    /// G66 item (4): the leaves of a job that could not be recognised (a
    /// backup copy whose name got a collision suffix, the leaves of a job
    /// that never started) go at the first open after they are stale
    /// (`temp::STALE_AGE`) — fresh ones stay, since a write may still be
    /// running; the backup's finished files and anyone else's stay, and the
    /// bytes of the temporary files alone are counted.
    #[test]
    fn the_sweep_removes_temp_files_of_a_collision_named_backup_copy_and_counts_them() {
        let dir = tempfile::tempdir().unwrap();
        let lib = Library::create(&dir.path().join("Job"), "Job", Mode::Session).unwrap();
        let card = dir.path().join("card");
        let backup = dir.path().join("Backup");
        for folder in [&card, &backup] {
            fs::create_dir_all(folder).unwrap();
        }
        fs::write(card.join("C1.ARW"), b"raw").unwrap();
        let item = [(
            crate::ingest_jobs::SourceFile::read(&card.join("C1.ARW")),
            None,
        )];
        lib.create_ingest_job(&serde_json::json!({ "backup": backup }), &item)
            .unwrap();
        let partial = |target: PathBuf, bytes: usize| {
            let mut file = temp::file_for(&target).unwrap();
            std::io::Write::write_all(&mut file, &vec![7; bytes]).unwrap();
            file.keep().unwrap().1
        };
        // What a kill left: the backup's copy at its collision name while
        // it was written (named after the photograph), a companion's, and
        // the leaves of a photograph no job item has planned yet.
        let leftovers = [
            partial(backup.join("C1_1.ARW"), 2048),
            partial(backup.join("C1_1.JPG"), 1024),
            partial(backup.join("C3_1.ARW"), 4096),
        ];
        let finished_bytes = b"a published backup copy";
        fs::write(backup.join("C1.ARW"), finished_bytes).unwrap();
        let finished = backup.join("C1.ARW");
        let foreign = backup.join(".tmpAbC123");
        fs::write(&foreign, b"another program's").unwrap();
        // An open before they are stale leaves them: a write may still run.
        let fresh = lib
            .sweep_leftovers(SystemTime::now(), &AtomicBool::new(false))
            .unwrap();
        assert_eq!((fresh.temp_files, fresh.temp_bytes), (0, 0), "{fresh:?}");
        for path in &leftovers {
            assert!(path.exists(), "{}", path.display());
        }
        // The first open after they are stale removes them.
        let later = SystemTime::now() + temp::STALE_AGE + Duration::from_secs(1);
        let swept = lib.sweep_leftovers(later, &AtomicBool::new(false)).unwrap();
        // What a resumed job removes (its own planned names) and the open
        // sweep leaves be, is not counted: the leaves here are an hour old.
        assert_eq!((swept.temp_files, swept.temp_bytes), (3, 7168), "{swept:?}");
        assert_eq!(swept.staging, staging::Removed::default());
        for path in &leftovers {
            assert!(!path.exists(), "{}", path.display());
        }
        assert_eq!(fs::read(&finished).unwrap(), finished_bytes);
        assert!(foreign.exists());
        // A job's backup folder inside the library is swept with the
        // library, once.
        let inside = lib.directory.join("Backup");
        fs::create_dir_all(&inside).unwrap();
        lib.create_ingest_job(&serde_json::json!({ "backup": inside }), &item)
            .unwrap();
        let once = partial(inside.join("C1_1.ARW"), 10);
        let swept = lib.sweep_leftovers(later, &AtomicBool::new(false)).unwrap();
        assert_eq!((swept.temp_files, swept.temp_bytes), (1, 10), "{swept:?}");
        assert!(!once.exists());
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
