//! Batch edits a kill or crash cut short (G66). Every batch write of more
//! than one photograph (paste, sync, preset or style; rating; colour label;
//! keyword added or removed) records itself as a job before its first
//! photograph is written: what it writes, how to run it again, and each
//! photograph's value before it. The job is removed when the batch returns
//! (written, failed or refused: the user hears about it then). One a quit,
//! kill or crash left is offered at the next open of its library as
//! "Interrupted: … applied to N of M photos" with Apply to the rest, Undo
//! the applied part and Dismiss: a batch is never silently half applied.
//!
//! Only our own job files are written and removed (in the app's data
//! folder); photographs and their sidecars are written by the batch
//! commands, never here.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Manager};

use crate::image_processing::ImageMetadata;

/// The folder of the job files, in the app's data folder.
const FOLDER: &str = "interrupted-batches";
/// The colour label's tag prefix (`COLOR_TAG_PREFIX` of the writers).
const COLOR_TAG_PREFIX: &str = "color:";

/// What a batch writes.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum BatchKind {
    /// Copied or synced settings (`apply_adjustments_to_paths` with `paste`).
    Paste,
    /// A style from the styles list.
    Style,
    /// A preset (settings replacing the photographs' own).
    Preset,
    Rating {
        rating: u8,
    },
    Color {
        color: Option<String>,
    },
    KeywordAdd {
        tag: String,
    },
    KeywordRemove {
        tag: String,
    },
}

impl BatchKind {
    /// The value of a photograph's metadata this batch writes (its whole
    /// recipe, its rating, its colour label, whether it has the keyword).
    pub fn field(&self, metadata: &ImageMetadata) -> Value {
        let tags = metadata.tags.as_deref().unwrap_or_default();
        match self {
            Self::Paste | Self::Style | Self::Preset => metadata.adjustments.clone(),
            Self::Rating { .. } => json!(metadata.rating),
            Self::Color { .. } => json!(
                tags.iter()
                    .find_map(|tag| tag.strip_prefix(COLOR_TAG_PREFIX))
                    .filter(|color| !color.is_empty())
            ),
            Self::KeywordAdd { tag } | Self::KeywordRemove { tag } => json!(tags.contains(tag)),
        }
    }

    /// Whether a photograph whose field was `before` shows this batch's
    /// effect with `now`: a recipe batch changed it; the others set a known
    /// value (a photograph that had it already counts as applied).
    fn applied(&self, before: &Value, now: &Value) -> bool {
        match self {
            Self::Paste | Self::Style | Self::Preset => before != now,
            Self::Rating { rating } => *now == json!(rating),
            Self::Color { color } => {
                *now == json!(color.as_deref().filter(|color| !color.is_empty()))
            }
            Self::KeywordAdd { .. } => *now == json!(true),
            Self::KeywordRemove { .. } => *now == json!(false),
        }
    }
}

/// One photograph of a job, with its field before the batch.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Photo {
    pub path: String,
    pub before: Value,
}

/// A job file.
#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct Job {
    id: String,
    /// The app run that wrote it: a job of this run is running, not
    /// interrupted.
    run: String,
    batch: BatchKind,
    /// The library open when it started (None: none was).
    library: Option<String>,
    /// The command and its arguments other than `paths`, to apply it to the
    /// rest.
    command: String,
    args: Value,
    started_ms: u64,
    photos: Vec<Photo>,
}

/// An interrupted batch as offered at the next open.
#[derive(Serialize, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct InterruptedBatch {
    pub id: String,
    pub batch: BatchKind,
    pub command: String,
    pub args: Value,
    pub started_ms: u64,
    pub total: usize,
    /// How many photographs show the batch's effect.
    pub applied: usize,
    /// Those of them whose value it changed, with the value before: what
    /// Undo the applied part writes back.
    pub undo: Vec<Photo>,
    /// The photographs it did not reach: what Apply to the rest writes.
    pub rest: Vec<String>,
}

/// This app run's identity (process id and start time).
fn this_run() -> &'static str {
    static RUN: OnceLock<String> = OnceLock::new();
    RUN.get_or_init(|| format!("{}-{}", std::process::id(), now_ms()))
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// A recorded job: its file goes when this is dropped (the batch returned,
/// whatever it did).
#[must_use]
pub struct Recorded(PathBuf);

impl Drop for Recorded {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_file(&self.0)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            log::warn!(
                "Could not remove the batch job {}: {error}",
                self.0.display()
            );
        }
    }
}

fn folder(app_handle: &AppHandle) -> Option<PathBuf> {
    app_handle
        .path()
        .app_data_dir()
        .ok()
        .map(|dir| dir.join(FOLDER))
}

/// Records the batch `batch` of `paths` (run by `command` with `args`) in
/// the app's job folder before it writes anything; `read` gives each
/// photograph's metadata as it is now. None for a single photograph (its
/// write is one step) and when the job cannot be recorded: the batch runs
/// as before, only not offered after a crash (logged).
pub fn begin(
    app_handle: &AppHandle,
    batch: BatchKind,
    command: &str,
    args: Value,
    library: Option<&Path>,
    paths: &[String],
    read: impl Fn(&str) -> ImageMetadata,
) -> Option<Recorded> {
    if paths.len() < 2 {
        return None;
    }
    let dir = folder(app_handle)?;
    let library = library
        .map(Path::to_path_buf)
        .or_else(crate::session_bridge::active_library_directory);
    begin_in(&dir, batch, command, args, library.as_deref(), paths, read)
        .map_err(|error| log::warn!("Could not record the batch {command}: {error}"))
        .ok()
}

pub(crate) fn begin_in(
    dir: &Path,
    batch: BatchKind,
    command: &str,
    args: Value,
    library: Option<&Path>,
    paths: &[String],
    read: impl Fn(&str) -> ImageMetadata,
) -> anyhow::Result<Recorded> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    std::fs::create_dir_all(dir)?;
    let id = format!("{}-{}", this_run(), NEXT.fetch_add(1, Ordering::Relaxed));
    let photos = paths
        .iter()
        .map(|path| Photo {
            path: path.clone(),
            before: batch.field(&read(path)),
        })
        .collect();
    let job = Job {
        id: id.clone(),
        run: this_run().to_string(),
        batch,
        library: library.map(|dir| dir.to_string_lossy().into_owned()),
        command: command.to_string(),
        args,
        started_ms: now_ms(),
        photos,
    };
    let file = dir.join(format!("{id}.json"));
    rawtonic_metadata::atomic_write(&file, serde_json::to_vec(&job)?)?;
    Ok(Recorded(file))
}

/// The jobs earlier runs left for `library` (None: those started with no
/// library open), oldest first, each with what it applied (`read` gives a
/// photograph's metadata as it is now).
pub(crate) fn interrupted_in(
    dir: &Path,
    library: Option<&Path>,
    read: impl Fn(&str) -> ImageMetadata,
) -> Vec<InterruptedBatch> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut jobs: Vec<Job> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
        .filter_map(|entry| {
            let job = std::fs::read(entry.path())
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Job>(&bytes).ok());
            if job.is_none() {
                log::warn!(
                    "Skipping an unreadable batch job {}",
                    entry.path().display()
                );
            }
            job
        })
        .filter(|job| job.run != this_run())
        .filter(|job| job.library.as_deref().map(Path::new) == library)
        .collect();
    jobs.sort_by_key(|job| job.started_ms);
    jobs.into_iter()
        .map(|job| {
            let mut applied = 0;
            let mut undo = Vec::new();
            let mut rest = Vec::new();
            for photo in job.photos.iter() {
                let now = job.batch.field(&read(&photo.path));
                if job.batch.applied(&photo.before, &now) {
                    applied += 1;
                    if now != photo.before {
                        undo.push(photo.clone());
                    }
                } else {
                    rest.push(photo.path.clone());
                }
            }
            InterruptedBatch {
                total: job.photos.len(),
                id: job.id,
                batch: job.batch,
                command: job.command,
                args: job.args,
                started_ms: job.started_ms,
                applied,
                undo,
                rest,
            }
        })
        .collect()
}

/// Forgets an interrupted job (after Apply to the rest, Undo the applied
/// part or Dismiss). Only a plain job name of the folder is taken.
pub(crate) fn dismiss_in(dir: &Path, id: &str) -> Result<(), String> {
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return Err("Not a batch job".into());
    }
    match std::fs::remove_file(dir.join(format!("{id}.json"))) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

/// A photograph's metadata as the batch commands read it.
pub(crate) fn current(path: &str) -> ImageMetadata {
    let (_, sidecar) = crate::file_management::parse_virtual_path(path);
    crate::exif_processing::load_sidecar(&sidecar)
}

/// The batches earlier runs left half applied in `directory` (the open
/// library; None: no library open).
#[tauri::command]
pub async fn interrupted_batches(
    directory: Option<String>,
    app_handle: AppHandle,
) -> Result<Vec<InterruptedBatch>, String> {
    let Some(dir) = folder(&app_handle) else {
        return Ok(Vec::new());
    };
    crate::file_management::blocking(move || {
        interrupted_in(&dir, directory.as_deref().map(Path::new), current)
    })
    .await
}

#[tauri::command]
pub fn dismiss_interrupted_batch(id: String, app_handle: AppHandle) -> Result<(), String> {
    let dir = folder(&app_handle).ok_or("No app data folder")?;
    dismiss_in(&dir, &id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    fn metadata(rating: u8, tags: &[&str], exposure: f64) -> ImageMetadata {
        ImageMetadata {
            rating,
            tags: (!tags.is_empty()).then(|| tags.iter().map(|t| t.to_string()).collect()),
            adjustments: json!({ "exposure": exposure }),
            ..ImageMetadata::default()
        }
    }

    /// A fake library: path → metadata.
    struct Photos(Mutex<HashMap<String, ImageMetadata>>);
    impl Photos {
        fn new(n: usize) -> (Self, Vec<String>) {
            let paths: Vec<String> = (0..n).map(|i| format!("/lib/Capture/{i}.ARW")).collect();
            let map = paths
                .iter()
                .map(|p| (p.clone(), metadata(1, &["a"], 0.0)))
                .collect();
            (Self(Mutex::new(map)), paths)
        }
        fn read(&self, path: &str) -> ImageMetadata {
            self.0.lock().unwrap()[path].clone()
        }
        fn edit(&self, path: &str, f: impl FnOnce(&mut ImageMetadata)) {
            f(self.0.lock().unwrap().get_mut(path).unwrap());
        }
    }

    /// A job left by an earlier run (as a kill leaves it).
    fn leave_from_earlier_run(recorded: Recorded) {
        let file = recorded.0.clone();
        std::mem::forget(recorded);
        let mut job: Job = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        job.run = "earlier".into();
        std::fs::write(&file, serde_json::to_vec(&job).unwrap()).unwrap();
    }

    #[test]
    fn a_batch_that_returns_leaves_no_job() {
        let dir = tempfile::tempdir().unwrap();
        let (photos, paths) = Photos::new(3);
        let job = begin_in(
            dir.path(),
            BatchKind::Paste,
            "apply_adjustments_to_paths",
            json!({}),
            None,
            &paths,
            |p| photos.read(p),
        )
        .unwrap();
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
        drop(job);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn a_running_batch_of_this_run_is_not_interrupted() {
        let dir = tempfile::tempdir().unwrap();
        let (photos, paths) = Photos::new(3);
        let _job = begin_in(
            dir.path(),
            BatchKind::Paste,
            "apply_adjustments_to_paths",
            json!({}),
            None,
            &paths,
            |p| photos.read(p),
        )
        .unwrap();
        assert!(interrupted_in(dir.path(), None, |p| photos.read(p)).is_empty());
    }

    #[test]
    fn a_killed_paste_says_how_far_it_got_and_what_undo_writes_back() {
        let dir = tempfile::tempdir().unwrap();
        let library = Path::new("/lib");
        let (photos, paths) = Photos::new(5);
        let args = json!({ "adjustments": { "exposure": 1.0 }, "paste": { "source": "/lib/Capture/x.ARW" } });
        let job = begin_in(
            dir.path(),
            BatchKind::Paste,
            "apply_adjustments_to_paths",
            args.clone(),
            Some(library),
            &paths,
            |p| photos.read(p),
        )
        .unwrap();
        // The kill fell after two photographs were written.
        for path in &paths[..2] {
            photos.edit(path, |m| m.adjustments = json!({ "exposure": 1.0 }));
        }
        leave_from_earlier_run(job);
        // Not offered in another library.
        assert!(
            interrupted_in(dir.path(), Some(Path::new("/other")), |p| photos.read(p)).is_empty()
        );
        assert!(interrupted_in(dir.path(), None, |p| photos.read(p)).is_empty());
        let found = interrupted_in(dir.path(), Some(Path::new("/lib/")), |p| photos.read(p));
        assert_eq!(found.len(), 1);
        let batch = &found[0];
        assert_eq!((batch.applied, batch.total), (2, 5));
        assert_eq!(batch.rest, paths[2..].to_vec());
        assert_eq!(batch.args, args);
        assert_eq!(
            batch.undo,
            paths[..2]
                .iter()
                .map(|path| Photo {
                    path: path.clone(),
                    before: json!({ "exposure": 0.0 })
                })
                .collect::<Vec<_>>()
        );
        dismiss_in(dir.path(), &batch.id).unwrap();
        assert!(interrupted_in(dir.path(), Some(library), |p| photos.read(p)).is_empty());
    }

    #[test]
    fn a_killed_rating_counts_photographs_that_had_the_rating_already_but_undoes_only_changed_ones()
    {
        let dir = tempfile::tempdir().unwrap();
        let (photos, paths) = Photos::new(4);
        photos.edit(&paths[3], |m| m.rating = 4);
        let job = begin_in(
            dir.path(),
            BatchKind::Rating { rating: 4 },
            "set_rating_for_paths",
            json!({ "rating": 4 }),
            None,
            &paths,
            |p| photos.read(p),
        )
        .unwrap();
        photos.edit(&paths[0], |m| m.rating = 4);
        leave_from_earlier_run(job);
        let found = interrupted_in(dir.path(), None, |p| photos.read(p));
        assert_eq!((found[0].applied, found[0].total), (2, 4));
        assert_eq!(
            found[0].undo,
            vec![Photo {
                path: paths[0].clone(),
                before: json!(1)
            }]
        );
        assert_eq!(found[0].rest, paths[1..3].to_vec());
    }

    #[test]
    fn keyword_and_label_batches_compare_only_their_own_field() {
        let dir = tempfile::tempdir().unwrap();
        let (photos, paths) = Photos::new(3);
        let added = begin_in(
            dir.path(),
            BatchKind::KeywordAdd { tag: "b".into() },
            "add_tag_for_paths",
            json!({ "tag": "b" }),
            None,
            &paths,
            |p| photos.read(p),
        )
        .unwrap();
        let labelled = begin_in(
            dir.path(),
            BatchKind::Color {
                color: Some("red".into()),
            },
            "set_color_label_for_paths",
            json!({ "color": "red" }),
            None,
            &paths,
            |p| photos.read(p),
        )
        .unwrap();
        photos.edit(&paths[1], |m| {
            m.tags = Some(vec!["a".into(), "b".into(), "color:red".into()]);
            m.rating = 5;
        });
        leave_from_earlier_run(added);
        leave_from_earlier_run(labelled);
        let found = interrupted_in(dir.path(), None, |p| photos.read(p));
        assert_eq!(found.len(), 2);
        for batch in &found {
            assert_eq!((batch.applied, batch.total), (1, 3));
            assert_eq!(batch.undo.len(), 1);
            assert_eq!(batch.undo[0].path, paths[1]);
        }
    }

    #[test]
    fn odd_names_and_unreadable_files_are_never_jobs() {
        let dir = tempfile::tempdir().unwrap();
        assert!(dismiss_in(dir.path(), "../x").is_err());
        assert!(dismiss_in(dir.path(), "").is_err());
        std::fs::write(dir.path().join("garbage.json"), b"{").unwrap();
        assert!(interrupted_in(dir.path(), None, |_| ImageMetadata::default()).is_empty());
        assert!(dir.path().join("garbage.json").exists());
    }
}
