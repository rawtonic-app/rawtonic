//! What an export does when its file name is taken ("If a file exists").
//!
//! * **Keep both** (the default): the new file gets a free numbered name
//!   (`photo_1.jpg`); nothing is ever written over.
//! * **Skip**: the photo is not exported for that recipe; decided before
//!   its source is read, and reported.
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
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
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
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::Written => "written",
            Outcome::SkippedExists => "skippedExists",
            Outcome::SkippedUnchanged => "skippedUnchanged",
            Outcome::Replaced => "replaced",
            Outcome::KeptBoth => "keptBoth",
        }
    }
    pub fn parse(text: &str) -> Option<Self> {
        [
            Outcome::Written,
            Outcome::SkippedExists,
            Outcome::SkippedUnchanged,
            Outcome::Replaced,
            Outcome::KeptBoth,
        ]
        .into_iter()
        .find(|outcome| outcome.as_str() == text)
    }
    pub fn is_skip(self) -> bool {
        matches!(self, Outcome::SkippedExists | Outcome::SkippedUnchanged)
    }
}

/// What was recorded for a file an export wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recorded {
    /// The photo it was exported from (a virtual copy's own path).
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
/// (the recipe, the library metadata and the export settings the caller
/// passes, without the "If a file exists" choice) and the app version, so
/// a new version re-exports once.
pub fn inputs_hash(parts: &serde_json::Value) -> String {
    let mut bytes = format!("rawtonic-export {}\n", env!("CARGO_PKG_VERSION")).into_bytes();
    bytes.extend(serde_json::to_vec(parts).unwrap_or_default());
    rawtonic_fs::sha256_hex(bytes)
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
    /// Give it the next free numbered name; `reported` when Replace if
    /// changed fell back to it ([`Outcome::KeptBoth`]).
    Number {
        reported: bool,
    },
    Skip(Outcome),
    /// Replace the previous export recorded so.
    Replace(Recorded),
}

/// The decision for an output whose own name is `path`, before anything
/// is rendered: `exists` whether a file of the name (or of one of its mask
/// files) is on disk, `used` whether another output of this run took the
/// name, `mask_files` whether the export writes separate mask files (which
/// are never replaced), `previous` what was recorded for the file at
/// `path`, `source` the photo and `inputs` this export's [`inputs_hash`].
#[allow(clippy::too_many_arguments)]
pub fn decide(
    policy: ExistingFiles,
    path: &Path,
    exists: bool,
    used: bool,
    mask_files: bool,
    previous: impl FnOnce() -> Option<Recorded>,
    source: &str,
    inputs: &str,
) -> Decision {
    // Two photos of one run named alike are both exported, whatever the
    // choice: skipping the second would lose it silently.
    if used {
        return Decision::Number { reported: false };
    }
    if !exists {
        return Decision::Write;
    }
    match policy {
        ExistingFiles::KeepBoth => Decision::Number { reported: false },
        ExistingFiles::Skip => Decision::Skip(Outcome::SkippedExists),
        ExistingFiles::ReplaceIfChanged => {
            match previous().filter(|p| p.is_export_of(path, source)) {
                Some(previous) if previous.inputs == inputs => {
                    Decision::Skip(Outcome::SkippedUnchanged)
                }
                Some(previous) if !mask_files => Decision::Replace(previous),
                _ => Decision::Number { reported: true },
            }
        }
    }
}

const RECORDS_VERSION: u32 = 1;

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

impl ExportRecords {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)
            .with_context(|| format!("Cannot open the export records {}", path.display()))?;
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")?;
        let version: u32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        ensure!(
            version <= RECORDS_VERSION,
            "The export records are from a newer Rawtonic (version {version})"
        );
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS records(
               output TEXT PRIMARY KEY,
               source TEXT NOT NULL,
               inputs TEXT NOT NULL,
               size INTEGER NOT NULL,
               sha256 TEXT NOT NULL,
               recorded_ms INTEGER NOT NULL);
             PRAGMA user_version=1;",
        )?;
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

    /// Records the file just written at `output` as the export of `source`
    /// with `inputs`: its size and SHA-256 as it is now.
    pub fn record(&self, output: &Path, source: &str, inputs: &str) -> Result<()> {
        let mut file = std::fs::File::open(output)?;
        let size = file.metadata()?.len();
        let sha256 = rawtonic_fs::sha256_hex_read(&mut file)?;
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
                Decision::Number { reported: false }
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
            Decision::Number { reported: false }
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
            Decision::Number { reported: true }
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
            Decision::Number { reported: true }
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
            Decision::Number { reported: true }
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
            Decision::Number { reported: true }
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
            Decision::Number { reported: true }
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
                Decision::Number { reported: true }
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
        let keep = inputs_hash(&settings_for_hash(&settings));
        settings.existing_files = ExistingFiles::ReplaceIfChanged;
        assert_eq!(inputs_hash(&settings_for_hash(&settings)), keep);
        settings.jpeg_quality = 80;
        assert_ne!(inputs_hash(&settings_for_hash(&settings)), keep);
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
