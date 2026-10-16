//! A library's hot folder (G47), kept in the library so watching resumes
//! when it is opened again: the watched folder and its settings, the last
//! capture (for Copy from last capture), and the files the hot folder has
//! taken, so a later run picks up only files that arrived since (while the
//! app was closed, say) and does not hash the ones taken before again.
use crate::Library;
use anyhow::Result;
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde_json::Value;
use std::path::PathBuf;

/// What a library's hot folder watches and how.
#[derive(Debug, Clone, PartialEq)]
pub struct HotFolderConfig {
    /// The watched folder (absolute; it is outside the library).
    pub folder: PathBuf,
    /// The filename template captures are copied under.
    pub template: String,
    pub include_subfolders: bool,
    /// The app's further settings (next-capture adjustments and metadata),
    /// stored as they are.
    pub settings: Value,
    /// The asset id of the last capture.
    pub last_capture: Option<String>,
}

/// A file the hot folder took: its path relative to the watched folder
/// (`/`-separated), size and modification time (nanoseconds since the Unix
/// epoch, when known). A file that changed since is new.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HotFolderFile {
    pub rel_path: String,
    pub size: u64,
    pub mtime_ns: Option<i64>,
}

impl Library {
    /// The hot folder watched for this library, if any.
    pub fn hot_folder(&self) -> Result<Option<HotFolderConfig>> {
        self.conn
            .query_row(
                "SELECT folder,template,include_subfolders,settings_json,last_capture
                 FROM hot_folder WHERE id=1",
                [],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, bool>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, Option<String>>(4)?,
                    ))
                },
            )
            .optional()?
            .map(|(folder, template, include_subfolders, settings, last)| {
                Ok(HotFolderConfig {
                    folder: folder.into(),
                    template,
                    include_subfolders,
                    settings: serde_json::from_str(&settings)?,
                    last_capture: last,
                })
            })
            .transpose()
    }

    /// Starts watching `config` (`None`: stops) in place of any earlier hot
    /// folder; the files an earlier one took are forgotten, so a new start
    /// considers everything in the folder (verified duplicates are still
    /// skipped by the import).
    pub fn set_hot_folder(&self, config: Option<&HotFolderConfig>) -> Result<()> {
        let tx = rusqlite::Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        tx.execute("DELETE FROM hot_folder_files", [])?;
        tx.execute("DELETE FROM hot_folder", [])?;
        if let Some(config) = config {
            let folder = config
                .folder
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("The hot folder's path is not valid Unicode"))?;
            tx.execute(
                "INSERT INTO hot_folder(id,folder,template,include_subfolders,settings_json,last_capture)
                 VALUES(1,?,?,?,?,?)",
                params![
                    folder,
                    config.template,
                    config.include_subfolders,
                    serde_json::to_string(&config.settings)?,
                    config.last_capture
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Remembers the last capture (Copy from last capture after a restart).
    pub fn set_hot_folder_last_capture(&self, asset: &str) -> Result<()> {
        self.conn
            .execute("UPDATE hot_folder SET last_capture=? WHERE id=1", [asset])?;
        Ok(())
    }

    /// The files the hot folder took.
    pub fn hot_folder_files(&self) -> Result<Vec<HotFolderFile>> {
        let mut statement = self
            .conn
            .prepare("SELECT rel_path,size,mtime_ns FROM hot_folder_files")?;
        let files = statement
            .query_map([], |r| {
                Ok(HotFolderFile {
                    rel_path: r.get(0)?,
                    size: r.get::<_, i64>(1)?.max(0) as u64,
                    mtime_ns: r.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(files)
    }

    /// Records that the hot folder took `file` (imported it or found it a
    /// verified duplicate). Only while a hot folder is set.
    pub fn record_hot_folder_file(&self, file: &HotFolderFile) -> Result<()> {
        self.conn.execute(
            "INSERT INTO hot_folder_files(rel_path,size,mtime_ns)
             SELECT ?1,?2,?3 WHERE EXISTS(SELECT 1 FROM hot_folder WHERE id=1)
             ON CONFLICT(rel_path) DO UPDATE SET size=excluded.size,mtime_ns=excluded.mtime_ns",
            params![file.rel_path, i64::try_from(file.size)?, file.mtime_ns],
        )?;
        Ok(())
    }

    /// Forgets taken files that are no longer in the watched folder.
    pub fn forget_hot_folder_files(&self, rel_paths: &[String]) -> Result<()> {
        if rel_paths.is_empty() {
            return Ok(());
        }
        let tx = rusqlite::Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        for rel in rel_paths {
            tx.execute("DELETE FROM hot_folder_files WHERE rel_path=?", [rel])?;
        }
        tx.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Mode;
    use serde_json::json;

    fn config(folder: &str) -> HotFolderConfig {
        HotFolderConfig {
            folder: folder.into(),
            template: "{orig}".into(),
            include_subfolders: true,
            settings: json!({"nextCapture": {"mode": "copyFromLast"}}),
            last_capture: None,
        }
    }

    #[test]
    fn the_hot_folder_and_its_taken_files_survive_reopening() {
        let d = tempfile::tempdir().unwrap();
        let lib = Library::create(&d.path().join("Job"), "Job", Mode::Session).unwrap();
        assert_eq!(lib.hot_folder().unwrap(), None);
        // Nothing is recorded without a hot folder.
        let file = HotFolderFile {
            rel_path: "day 1/A.ARW".into(),
            size: 12,
            mtime_ns: Some(1_700_000_000_123_456_789),
        };
        lib.record_hot_folder_file(&file).unwrap();
        assert!(lib.hot_folder_files().unwrap().is_empty());

        lib.set_hot_folder(Some(&config("/Volumes/Tether/Hot")))
            .unwrap();
        lib.record_hot_folder_file(&file).unwrap();
        let changed = HotFolderFile {
            size: 13,
            ..file.clone()
        };
        lib.record_hot_folder_file(&changed).unwrap();
        let photo = lib.directory.join("Capture/A.ARW");
        std::fs::write(&photo, b"a").unwrap();
        let asset = lib.add_asset(&photo).unwrap();
        lib.set_hot_folder_last_capture(&asset.id).unwrap();
        drop(lib);

        let lib = Library::open(&d.path().join("Job")).unwrap();
        let restored = lib.hot_folder().unwrap().unwrap();
        assert_eq!(
            restored,
            HotFolderConfig {
                last_capture: Some(asset.id.clone()),
                ..config("/Volumes/Tether/Hot")
            }
        );
        assert_eq!(
            lib.hot_folder_files().unwrap(),
            std::slice::from_ref(&changed)
        );
        lib.forget_hot_folder_files(std::slice::from_ref(&changed.rel_path))
            .unwrap();
        assert!(lib.hot_folder_files().unwrap().is_empty());

        // A new start forgets what the earlier one took; stopping forgets all.
        lib.record_hot_folder_file(&file).unwrap();
        lib.set_hot_folder(Some(&config("/elsewhere"))).unwrap();
        assert!(lib.hot_folder_files().unwrap().is_empty());
        lib.set_hot_folder(None).unwrap();
        assert_eq!(lib.hot_folder().unwrap(), None);
    }

    /// A transferred or restored copy does not watch the source's folder.
    #[test]
    fn a_copy_of_the_library_does_not_watch() {
        let d = tempfile::tempdir().unwrap();
        let lib = Library::create(&d.path().join("Job"), "Job", Mode::Session).unwrap();
        lib.set_hot_folder(Some(&config("/hot"))).unwrap();
        lib.record_hot_folder_file(&HotFolderFile {
            rel_path: "A.ARW".into(),
            size: 1,
            mtime_ns: None,
        })
        .unwrap();
        lib.internalize_assets(&[]).unwrap();
        assert_eq!(lib.hot_folder().unwrap(), None);
        assert!(lib.hot_folder_files().unwrap().is_empty());
    }

    /// A schema 7 library (before hot folders were kept) opens with no hot
    /// folder and keeps one after the migration.
    #[test]
    fn schema_7_libraries_migrate_to_kept_hot_folders() {
        let d = tempfile::tempdir().unwrap();
        let lib = Library::create(&d.path().join("Job"), "Job", Mode::Session).unwrap();
        let photo = lib.directory.join("Capture/A.ARW");
        std::fs::write(&photo, b"a").unwrap();
        let asset = lib.add_asset(&photo).unwrap();
        lib.conn
            .execute_batch(
                "DROP TABLE hot_folder; DROP TABLE hot_folder_files; PRAGMA user_version=7;",
            )
            .unwrap();
        drop(lib);
        let lib = Library::open(&d.path().join("Job")).unwrap();
        let version: u32 = lib
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, crate::DB_SCHEMA_VERSION);
        assert_eq!(lib.hot_folder().unwrap(), None);
        assert_eq!(lib.asset(&asset.id).unwrap().id, asset.id);
        lib.set_hot_folder(Some(&HotFolderConfig {
            last_capture: Some(asset.id.clone()),
            ..config("/hot")
        }))
        .unwrap();
        // The last capture goes when its photograph does.
        lib.conn
            .execute("DELETE FROM assets WHERE id=?", [&asset.id])
            .unwrap();
        assert_eq!(lib.hot_folder().unwrap().unwrap().last_capture, None);
    }
}
