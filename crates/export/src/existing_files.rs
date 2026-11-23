//! What an export does when its file name is taken ("If a file exists").
//!
//! * **Keep both** (the default): the new file gets a free numbered name
//!   (`photo_1.jpg`); nothing is ever written over.
//! * **Skip**: the photo is not exported for that recipe; decided before
//!   its source is decoded, and reported.
//! * **Replace if changed**: the taken file is replaced only when it is a
//!   previous Rawtonic export of the same photo at the same path, still
//!   byte for byte as Rawtonic wrote it, and the export's inputs (recipe,
//!   library metadata, export settings, app version) differ from the ones
//!   recorded for it; unchanged, the export is skipped. Any other file
//!   (an original, a camera JPEG, another program's file, one edited since)
//!   is never replaced: the export falls back to Keep both and says so.
//!
//! The record of what was exported where lives in a small SQLite file of
//! the app (or the one the CLI names), never beside the exported files: a
//! synced or web gallery folder gets no extra files.
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::{ErrorKind, Read};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ExistingFiles {
    #[default]
    KeepBoth,
    Skip,
    ReplaceIfChanged,
}

impl ExistingFiles {
    /// The CLI's spelling (`keep-both`, `skip`, `replace-if-changed`).
    pub fn parse(text: &str) -> Result<Self, String> {
        match text {
            "keep-both" => Ok(Self::KeepBoth),
            "skip" => Ok(Self::Skip),
            "replace-if-changed" => Ok(Self::ReplaceIfChanged),
            other => Err(format!(
                "unknown value {other:?} (keep-both, skip, replace-if-changed)"
            )),
        }
    }
}

/// How one output of an export ended, as its result reports it.
#[derive(Serialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Outcome {
    /// Written under its own name or, by Keep both, a numbered one.
    Written,
    /// Skip: a file of its name exists.
    SkippedExists,
    /// Replace if changed: its previous export has the same inputs.
    SkippedUnchanged,
    /// Replace if changed: its previous export was replaced.
    Replaced,
    /// Replace if changed: the file of its name is not a previous export of
    /// this photo (or has mask files beside it), so it was kept and the new
    /// one got a numbered name.
    KeptBoth,
    /// Replace if changed: the file of its name is a photo of a library
    /// (someone's original, even if Rawtonic once exported it), so it was
    /// kept and the new one got a numbered name.
    KeptOriginal,
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::Written => "written",
            Outcome::SkippedExists => "skippedExists",
            Outcome::SkippedUnchanged => "skippedUnchanged",
            Outcome::Replaced => "replaced",
            Outcome::KeptBoth => "keptBoth",
            Outcome::KeptOriginal => "keptOriginal",
        }
    }
    pub fn parse(text: &str) -> Option<Self> {
        [
            Outcome::Written,
            Outcome::SkippedExists,
            Outcome::SkippedUnchanged,
            Outcome::Replaced,
            Outcome::KeptBoth,
            Outcome::KeptOriginal,
        ]
        .into_iter()
        .find(|outcome| outcome.as_str() == text)
    }
    pub fn is_skip(self) -> bool {
        matches!(self, Outcome::SkippedExists | Outcome::SkippedUnchanged)
    }
}

/// The photo an export is of, as its record names it: its path (a virtual
/// copy's own) and the fingerprint of its original's file, so another
/// photo put at the same path later (a reused camera file name) is never
/// taken for it. `None` without a fingerprint: such an export is never
/// replaced.
pub fn photo_key(photo: &str, original_fingerprint: Option<&str>) -> Option<String> {
    original_fingerprint.map(|fingerprint| format!("{photo}\n{fingerprint}"))
}

/// What was recorded for a file an export wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recorded {
    /// The photo it was exported from ([`photo_key`]).
    pub source: String,
    /// [`inputs_hash`] of the export.
    pub inputs: String,
    pub size: u64,
    pub sha256: String,
}

impl Recorded {
    /// Whether the file at `path` is this record's export of `source`,
    /// unchanged since it was written: a regular file (not a symlink) of
    /// the recorded size and SHA-256.
    pub fn is_export_of(&self, path: &Path, source: &str) -> bool {
        if self.source != source {
            return false;
        }
        let Ok(meta) = std::fs::symlink_metadata(path) else {
            return false;
        };
        if !meta.is_file() || meta.len() != self.size {
            return false;
        }
        std::fs::File::open(path)
            .and_then(|mut file| rawtonic_fs::sha256_hex_read(&mut file))
            .is_ok_and(|digest| digest == self.sha256)
    }
}

/// The fingerprint of everything that decides an export's file: `parts`
/// (the recipe, the library metadata, the preferences the render reads, the
/// files it reads besides the photo and the export settings the caller
/// passes, without the "If a file exists" choice) and `app_version`, the
/// released app's version, so an update re-exports once.
pub fn inputs_hash(app_version: &str, parts: &serde_json::Value) -> String {
    let mut bytes = format!("rawtonic-export {app_version}\n").into_bytes();
    bytes.extend(serde_json::to_vec(parts).unwrap_or_default());
    rawtonic_fs::sha256_hex(bytes)
}

/// The content fingerprint of a file an export reads besides the photo (a
/// watermark image, a LUT), for [`inputs_hash`]: replaced in place, it
/// changes the export. `None` for a file that cannot be read.
pub fn file_fingerprint(path: &Path) -> Option<String> {
    let mut file = std::fs::File::open(path).ok()?;
    rawtonic_fs::sha256_hex_read(&mut file).ok()
}

/// The export settings as [`inputs_hash`] takes them: without the
/// `existingFiles` choice, which changes no file.
pub fn settings_for_hash(settings: &crate::export_processing::ExportSettings) -> serde_json::Value {
    let mut value = serde_json::to_value(settings).unwrap_or_default();
    if let Some(map) = value.as_object_mut() {
        map.remove("existingFiles");
    }
    value
}

/// What to do with one output whose own name is taken or free.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Write a new file under its own name.
    Write,
    /// Give it the next free numbered name; `reported` how, when Replace
    /// if changed fell back to it ([`Outcome::KeptBoth`],
    /// [`Outcome::KeptOriginal`]).
    Number {
        reported: Option<Outcome>,
    },
    Skip(Outcome),
    /// Replace the previous export recorded so.
    Replace(Recorded),
}

/// What is known of the file at an output's name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Previous {
    /// Nothing recorded for it.
    Unknown,
    /// A photo of a library: never replaced.
    Original,
    /// What an export recorded for it.
    Export(Recorded),
}

impl From<Option<Recorded>> for Previous {
    fn from(recorded: Option<Recorded>) -> Self {
        recorded.map_or(Previous::Unknown, Previous::Export)
    }
}

/// The decision for an output whose own name is `path`, before anything
/// is rendered: `exists` whether a file of the name (or of one of its mask
/// files) is on disk, `used` whether another output of this run took the
/// name, `mask_files` whether the export writes separate mask files (which
/// are never replaced), `previous` what was recorded for the file at
/// `path`, `source` the photo and `inputs` this export's [`inputs_hash`].
#[allow(clippy::too_many_arguments)]
pub fn decide<P>(
    policy: ExistingFiles,
    path: &Path,
    exists: bool,
    used: bool,
    mask_files: bool,
    previous: impl FnOnce() -> P,
    source: &str,
    inputs: &str,
) -> Decision
where
    P: Into<Previous>,
{
    // Two photos of one run named alike are both exported, whatever the
    // choice: skipping the second would lose it silently.
    if used {
        return Decision::Number { reported: None };
    }
    if !exists {
        return Decision::Write;
    }
    match policy {
        ExistingFiles::KeepBoth => Decision::Number { reported: None },
        ExistingFiles::Skip => Decision::Skip(Outcome::SkippedExists),
        ExistingFiles::ReplaceIfChanged => match previous().into() {
            Previous::Original => Decision::Number {
                reported: Some(Outcome::KeptOriginal),
            },
            Previous::Export(previous) if previous.is_export_of(path, source) => {
                if previous.inputs == inputs {
                    Decision::Skip(Outcome::SkippedUnchanged)
                } else if mask_files {
                    Decision::Number {
                        reported: Some(Outcome::KeptBoth),
                    }
                } else {
                    Decision::Replace(previous)
                }
            }
            _ => Decision::Number {
                reported: Some(Outcome::KeptBoth),
            },
        },
    }
}

const RECORDS_VERSION: u32 = 1;
/// `PRAGMA application_id` of an export records file ("RTXR"): any other
/// database is refused before anything is written to it.
const RECORDS_APPLICATION_ID: i32 = 0x5254_5852;

/// The record of the files exports wrote (output path → photo, inputs,
/// size, SHA-256), for Replace if changed. One connection per thread.
pub struct ExportRecords {
    conn: Connection,
}

/// The key a file is recorded under: its folder resolved (symlinks, `..`)
/// and its name, so two spellings of one path find one record.
fn key(path: &Path) -> String {
    let resolved = match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => rawtonic_fs::canonicalize(if parent.as_os_str().is_empty() {
            Path::new(".")
        } else {
            parent
        })
        .map(|parent| parent.join(name))
        .unwrap_or_else(|_| rawtonic_fs::simplified(path)),
        _ => rawtonic_fs::simplified(path),
    };
    resolved.to_string_lossy().into_owned()
}

fn initialize_records(conn: &Connection) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch(&format!(
        "PRAGMA application_id={RECORDS_APPLICATION_ID};
         CREATE TABLE IF NOT EXISTS records(
           output TEXT PRIMARY KEY,
           source TEXT NOT NULL,
           inputs TEXT NOT NULL,
           size INTEGER NOT NULL,
           sha256 TEXT NOT NULL,
           recorded_ms INTEGER NOT NULL);
         PRAGMA user_version={RECORDS_VERSION};"
    ))?;
    tx.commit()?;
    Ok(())
}

fn is_empty_records_file(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|meta| {
        meta.is_file() && meta.len() == 0 && !rawtonic_fs::is_cloud_placeholder_metadata(&meta)
    })
}

impl ExportRecords {
    pub fn open(path: &Path) -> Result<Self> {
        Self::open_with_initializer(path, initialize_records)
    }

    fn open_with_initializer(
        path: &Path,
        initialize: impl FnOnce(&Connection) -> Result<()>,
    ) -> Result<Self> {
        let parent = path
            .parent()
            .context("Export records need a parent directory")?;
        let parent = if parent.as_os_str().is_empty() {
            Path::new(".")
        } else {
            parent
        };
        fs::create_dir_all(parent)?;
        let missing = match fs::symlink_metadata(path) {
            Ok(_) => false,
            Err(e) if e.kind() == ErrorKind::NotFound => true,
            Err(e) => return Err(e.into()),
        };
        if missing || is_empty_records_file(path) {
            // A legacy crash could leave an empty main file. SQLite sidecars
            // at that name could also belong to another database.
            for suffix in ["-journal", "-wal", "-shm"] {
                let mut sidecar = path.as_os_str().to_os_string();
                sidecar.push(suffix);
                match fs::symlink_metadata(&sidecar) {
                    Err(e) if e.kind() == ErrorKind::NotFound => {}
                    Err(e) => return Err(e.into()),
                    Ok(_) => {
                        // A concurrent initializer may already have published
                        // its database and enabled WAL.
                        return Self::open_existing(path);
                    }
                }
            }

            // Close SQLite before publishing, while the entire database is
            // in the main file. WAL is enabled only under the final name.
            let temp = rawtonic_fs::temp::file_for(path)?;
            let conn = Connection::open(temp.path())?;
            initialize(&conn)?;
            conn.close().map_err(|(_, e)| e)?;
            temp.as_file().sync_all()?;
            if missing {
                match rawtonic_fs::no_clobber::persist_atomic(temp, path) {
                    Ok(()) => rawtonic_fs::sync_dir(parent)?,
                    Err(e) if e.kind() == ErrorKind::AlreadyExists => {}
                    Err(e) => return Err(e.into()),
                }
            } else if let Err(e) = rawtonic_fs::replace_existing(temp, path, is_empty_records_file)
            {
                // Another opener may have recovered this file while we
                // staged it. Use its database only if it is really ours.
                return Self::open_existing(path).map_err(|_| e);
            }
        }
        Self::open_existing(path)
    }

    fn open_existing(path: &Path) -> Result<Self> {
        let not_ours = || format!("{} is not a Rawtonic export records file", path.display());
        // Reject foreign files before SQLite can open or recover a journal.
        let mut header = [0u8; 100];
        ensure!(
            fs::metadata(path).with_context(not_ours)?.is_file(),
            not_ours()
        );
        File::open(path)
            .and_then(|mut file| file.read_exact(&mut header))
            .with_context(not_ours)?;
        ensure!(
            &header[..16] == b"SQLite format 3\0"
                && i32::from_be_bytes([header[68], header[69], header[70], header[71]])
                    == RECORDS_APPLICATION_ID,
            not_ours()
        );
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .with_context(|| format!("Cannot open the export records {}", path.display()))?;
        conn.busy_timeout(Duration::from_secs(5))?;
        let id: i32 = conn
            .query_row("PRAGMA application_id", [], |r| r.get(0))
            .with_context(not_ours)?;
        ensure!(id == RECORDS_APPLICATION_ID, not_ours());
        let version: u32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        ensure!(
            version <= RECORDS_VERSION,
            "The export records are from a newer Rawtonic (version {version})"
        );
        if version == 0 {
            // Older versions could leave our header but stop before writing
            // the table and schema version.
            initialize_records(&conn)?;
        }
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")?;
        Ok(Self { conn })
    }

    /// What was recorded for the file at `output`.
    pub fn get(&self, output: &Path) -> Result<Option<Recorded>> {
        Ok(self
            .conn
            .query_row(
                "SELECT source, inputs, size, sha256 FROM records WHERE output=?",
                [key(output)],
                |r| {
                    Ok(Recorded {
                        source: r.get(0)?,
                        inputs: r.get(1)?,
                        size: r.get::<_, i64>(2)? as u64,
                        sha256: r.get(3)?,
                    })
                },
            )
            .optional()?)
    }

    /// Records the file at `output` as it is now as the export of `source`
    /// with `inputs` (tests; exports record what they wrote, [`Self::record_written`]).
    pub fn record(&self, output: &Path, source: &str, inputs: &str) -> Result<()> {
        let mut file = std::fs::File::open(output)?;
        let size = file.metadata()?.len();
        let sha256 = rawtonic_fs::sha256_hex_read(&mut file)?;
        self.record_written(
            output,
            source,
            inputs,
            &crate::output::Written { size, sha256 },
        )
    }

    /// Records `written`, the file an export of `source` with `inputs`
    /// published at `output` (its size and SHA-256 as written, not as the
    /// path holds now).
    pub fn record_written(
        &self,
        output: &Path,
        source: &str,
        inputs: &str,
        written: &crate::output::Written,
    ) -> Result<()> {
        let (size, sha256) = (written.size, written.sha256.as_str());
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as i64);
        self.conn.execute(
            "INSERT INTO records(output, source, inputs, size, sha256, recorded_ms)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(output) DO UPDATE SET source=?2, inputs=?3, size=?4, sha256=?5,
               recorded_ms=?6",
            params![key(output), source, inputs, size as i64, sha256, now],
        )?;
        Ok(())
    }
}

/// The export records file of an app data folder.
pub fn records_path(app_data: &Path) -> PathBuf {
    app_data.join("export-records.sqlite")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn decide_for(
        policy: ExistingFiles,
        records: &ExportRecords,
        path: &Path,
        source: &str,
        inputs: &str,
    ) -> Decision {
        decide(
            policy,
            path,
            path.exists(),
            false,
            false,
            || records.get(path).unwrap(),
            source,
            inputs,
        )
    }

    #[test]
    fn a_free_name_is_written_and_a_name_this_run_took_is_numbered_by_every_choice() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.jpg");
        for policy in [
            ExistingFiles::KeepBoth,
            ExistingFiles::Skip,
            ExistingFiles::ReplaceIfChanged,
        ] {
            let previous = || -> Option<Recorded> { panic!("no record is read for a free name") };
            assert_eq!(
                decide(policy, &path, false, false, false, previous, "s", "i"),
                Decision::Write
            );
            assert_eq!(
                decide(policy, &path, true, true, false, || None, "s", "i"),
                Decision::Number { reported: None }
            );
        }
    }

    #[test]
    fn skip_and_keep_both_never_look_at_the_taken_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.jpg");
        fs::write(&path, b"someone's").unwrap();
        let never = || -> Option<Recorded> { panic!("not read") };
        assert_eq!(
            decide(
                ExistingFiles::Skip,
                &path,
                true,
                false,
                false,
                never,
                "s",
                "i"
            ),
            Decision::Skip(Outcome::SkippedExists)
        );
        assert_eq!(
            decide(
                ExistingFiles::KeepBoth,
                &path,
                true,
                false,
                false,
                never,
                "s",
                "i"
            ),
            Decision::Number { reported: None }
        );
    }

    #[test]
    fn only_an_unchanged_previous_export_of_the_same_photo_is_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let records = ExportRecords::open(&dir.path().join("records.sqlite")).unwrap();
        let path = dir.path().join("a.jpg");
        let photo = "/photos/a.raf";

        // A file Rawtonic did not write (an original, a camera JPEG): kept.
        fs::write(&path, b"camera jpeg").unwrap();
        assert_eq!(
            decide_for(
                ExistingFiles::ReplaceIfChanged,
                &records,
                &path,
                photo,
                "v1"
            ),
            Decision::Number {
                reported: Some(Outcome::KeptBoth)
            }
        );

        // Our export: unchanged inputs skip, changed ones replace.
        fs::write(&path, b"export v1").unwrap();
        records.record(&path, photo, "v1").unwrap();
        assert_eq!(
            decide_for(
                ExistingFiles::ReplaceIfChanged,
                &records,
                &path,
                photo,
                "v1"
            ),
            Decision::Skip(Outcome::SkippedUnchanged)
        );
        assert!(matches!(
            decide_for(ExistingFiles::ReplaceIfChanged, &records, &path, photo, "v2"),
            Decision::Replace(previous) if previous.inputs == "v1"
        ));
        // Recorded under another spelling of the same path.
        let other_spelling = dir.path().join(".").join("a.jpg");
        assert!(records.get(&other_spelling).unwrap().is_some());

        // Another photo's export of that name: kept.
        assert_eq!(
            decide_for(
                ExistingFiles::ReplaceIfChanged,
                &records,
                &path,
                "/photos/b.raf",
                "v2"
            ),
            Decision::Number {
                reported: Some(Outcome::KeptBoth)
            }
        );
        // With separate mask files beside it: kept (they are never replaced).
        assert_eq!(
            decide(
                ExistingFiles::ReplaceIfChanged,
                &path,
                true,
                false,
                true,
                || records.get(&path).unwrap(),
                photo,
                "v2"
            ),
            Decision::Number {
                reported: Some(Outcome::KeptBoth)
            }
        );

        // Edited or replaced by someone else since: kept, same size or not.
        fs::write(&path, b"export v9").unwrap();
        assert_eq!(
            decide_for(
                ExistingFiles::ReplaceIfChanged,
                &records,
                &path,
                photo,
                "v2"
            ),
            Decision::Number {
                reported: Some(Outcome::KeptBoth)
            }
        );
        fs::write(&path, b"retouched in another editor").unwrap();
        assert_eq!(
            decide_for(
                ExistingFiles::ReplaceIfChanged,
                &records,
                &path,
                photo,
                "v1"
            ),
            Decision::Number {
                reported: Some(Outcome::KeptBoth)
            }
        );

        // A symlink to our export is not our export.
        #[cfg(unix)]
        {
            fs::write(&path, b"export v1").unwrap();
            records.record(&path, photo, "v1").unwrap();
            let link = dir.path().join("link.jpg");
            std::os::unix::fs::symlink(&path, &link).unwrap();
            records.record(&link, photo, "v1").unwrap();
            assert_eq!(
                decide_for(
                    ExistingFiles::ReplaceIfChanged,
                    &records,
                    &link,
                    photo,
                    "v2"
                ),
                Decision::Number {
                    reported: Some(Outcome::KeptBoth)
                }
            );
        }
    }

    #[test]
    fn the_choice_changes_no_file_so_it_is_not_an_input() {
        let mut settings: crate::export_processing::ExportSettings =
            serde_json::from_value(serde_json::json!({
                "jpegQuality": 90, "keepMetadata": true, "stripGps": false,
                "resize": null, "filenameTemplate": null, "watermark": null,
            }))
            .unwrap();
        assert_eq!(settings.existing_files, ExistingFiles::KeepBoth);
        let keep = inputs_hash("1.0", &settings_for_hash(&settings));
        settings.existing_files = ExistingFiles::ReplaceIfChanged;
        assert_eq!(inputs_hash("1.0", &settings_for_hash(&settings)), keep);
        assert_ne!(
            inputs_hash("1.1", &settings_for_hash(&settings)),
            keep,
            "an update"
        );
        settings.jpeg_quality = 80;
        assert_ne!(inputs_hash("1.0", &settings_for_hash(&settings)), keep);
    }

    #[test]
    fn a_photo_of_a_library_is_kept_even_if_it_is_our_export() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.jpg");
        fs::write(&path, b"export").unwrap();
        assert_eq!(
            decide(
                ExistingFiles::ReplaceIfChanged,
                &path,
                true,
                false,
                false,
                || Previous::Original,
                "s",
                "v2"
            ),
            Decision::Number {
                reported: Some(Outcome::KeptOriginal)
            }
        );
    }

    #[test]
    fn another_photo_at_the_same_path_is_not_the_recorded_one() {
        let first = photo_key("/card/DSC_0001.NEF", Some("aaa")).unwrap();
        let reused = photo_key("/card/DSC_0001.NEF", Some("bbb")).unwrap();
        assert_ne!(first, reused);
        assert_eq!(photo_key("/card/DSC_0001.NEF", None), None);
        let dir = tempfile::tempdir().unwrap();
        let records = ExportRecords::open(&dir.path().join("records.sqlite")).unwrap();
        let path = dir.path().join("DSC_0001.jpg");
        fs::write(&path, b"export of the first").unwrap();
        records.record(&path, &first, "v1").unwrap();
        assert_eq!(
            decide_for(
                ExistingFiles::ReplaceIfChanged,
                &records,
                &path,
                &reused,
                "v2"
            ),
            Decision::Number {
                reported: Some(Outcome::KeptBoth)
            }
        );
    }

    #[test]
    fn empty_records_left_by_an_interrupted_create_are_recovered() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("records.sqlite");
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();

        let records = ExportRecords::open(&path).unwrap();
        let output = dir.path().join("photo.jpg");
        fs::write(&output, b"export").unwrap();
        assert!(records.get(&output).unwrap().is_none());
        records.record(&output, "photo", "v1").unwrap();
        drop(records);

        let records = ExportRecords::open(&path).unwrap();
        assert_eq!(records.get(&output).unwrap().unwrap().inputs, "v1");
    }

    #[test]
    fn interrupted_initialization_leaves_no_published_database() {
        let dir = tempfile::tempdir().unwrap();
        for was_empty in [false, true] {
            let path = dir.path().join(format!("records-{was_empty}.sqlite"));
            if was_empty {
                fs::write(&path, []).unwrap();
            }
            let result = ExportRecords::open_with_initializer(&path, |conn| {
                initialize_records(conn)?;
                if was_empty {
                    assert_eq!(fs::read(&path).unwrap(), b"");
                } else {
                    assert!(!path.exists());
                }
                anyhow::bail!("simulated interruption before publication")
            });
            assert_eq!(
                result.err().unwrap().to_string(),
                "simulated interruption before publication"
            );
            assert!(ExportRecords::open(&path).is_ok());
        }
    }

    #[test]
    fn foreign_file_arriving_during_initialization_is_preserved() {
        let dir = tempfile::tempdir().unwrap();
        for was_empty in [false, true] {
            let path = dir.path().join(format!("records-{was_empty}.sqlite"));
            if was_empty {
                fs::write(&path, []).unwrap();
            }
            let result = ExportRecords::open_with_initializer(&path, |conn| {
                initialize_records(conn)?;
                fs::write(&path, b"foreign")?;
                Ok(())
            });
            assert!(result.is_err());
            assert_eq!(fs::read(&path).unwrap(), b"foreign");
        }
    }

    #[test]
    fn another_database_is_refused_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let other = dir.path().join("other.sqlite");
        let conn = Connection::open(&other).unwrap();
        conn.execute_batch("CREATE TABLE notes(text TEXT); INSERT INTO notes VALUES('mine');")
            .unwrap();
        drop(conn);
        let before = fs::read(&other).unwrap();
        assert!(ExportRecords::open(&other).is_err());
        assert_eq!(fs::read(&other).unwrap(), before);
        // An initialized database whose tables were dropped: not ours either.
        let emptied = dir.path().join("emptied.sqlite");
        let conn = Connection::open(&emptied).unwrap();
        conn.execute_batch("CREATE TABLE t(x); DROP TABLE t;")
            .unwrap();
        drop(conn);
        let before = fs::read(&emptied).unwrap();
        assert!(ExportRecords::open(&emptied).is_err());
        assert_eq!(fs::read(&emptied).unwrap(), before);
        assert!(!dir.path().join("other.sqlite-wal").exists());
        for contents in [&b"not a database at all, just some text"[..], b"x", b"\0"] {
            let text = dir.path().join("notes.txt");
            fs::write(&text, contents).unwrap();
            assert!(ExportRecords::open(&text).is_err());
            assert_eq!(fs::read(&text).unwrap(), contents);
        }
        let sidecar = dir.path().join("recovery.sqlite-journal");
        let recovery = dir.path().join("recovery.sqlite");
        fs::write(&recovery, []).unwrap();
        fs::write(&sidecar, b"another database's journal").unwrap();
        assert!(ExportRecords::open(&recovery).is_err());
        assert_eq!(fs::read(&recovery).unwrap(), b"");
        assert_eq!(fs::read(&sidecar).unwrap(), b"another database's journal");
        // Its own file opens again.
        let own = dir.path().join("records.sqlite");
        drop(ExportRecords::open(&own).unwrap());
        assert!(ExportRecords::open(&own).is_ok());
    }

    #[test]
    fn the_cli_spelling_parses() {
        assert_eq!(ExistingFiles::parse("skip"), Ok(ExistingFiles::Skip));
        assert_eq!(
            ExistingFiles::parse("replace-if-changed"),
            Ok(ExistingFiles::ReplaceIfChanged)
        );
        assert!(ExistingFiles::parse("overwrite").is_err());
        for outcome in [
            "written",
            "skippedExists",
            "skippedUnchanged",
            "replaced",
            "keptBoth",
        ] {
            assert_eq!(Outcome::parse(outcome).unwrap().as_str(), outcome);
        }
    }
}
