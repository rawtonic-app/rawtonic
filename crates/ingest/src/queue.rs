//! Ingest jobs kept in the library (`Library::create_ingest_job`), so an
//! ingest a quit, crash or card taken out cut off is finished later: the
//! photographs not yet in the library are imported then, with the names
//! and numbers they would have had, none twice and none left out. Copies
//! are made as by `import_batch` (verified, never replacing a file, the
//! card's files only read).
use crate::{
    ImportOptions, ImportResult,
    batch::{BatchOptions, FileDone, pair_sources},
    destination_folder, import_pair,
};
use anyhow::Result;
use rawtonic_catalog::{
    Library,
    ingest_jobs::{IngestJob, ItemState, JobState, SourceFile, SourceState},
};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

/// An ingest's options as its job keeps them (`BatchOptions`; the pairs
/// are kept with its photographs).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobOptions {
    pub template: String,
    pub sequence_start: u64,
    pub backup: Option<PathBuf>,
    /// As `Library::stored` keeps it: relative to the library when inside
    /// it (a closed session moves as one folder).
    pub folder: Option<String>,
    pub roots: Vec<PathBuf>,
    /// The album the photographs go into.
    #[serde(default)]
    pub album: Option<String>,
    #[serde(default)]
    pub metadata: Option<crate::metadata::ImportMetadata>,
}

impl JobOptions {
    pub fn new(library: &Library, options: &BatchOptions<'_>) -> Result<Self> {
        Ok(Self {
            template: options.template.into(),
            sequence_start: options.sequence_start,
            backup: options.backup.map(Into::into),
            folder: options
                .folder
                .map(|folder| library.stored(&folder.canonicalize()?))
                .transpose()?,
            roots: options.roots.to_vec(),
            album: options.album.map(Into::into),
            metadata: options.metadata.cloned(),
        })
    }
}

/// Records an ingest job of `sources` with `options` (each photograph with
/// its companion, `pair_sources`) and returns its id; `run_job` imports it.
pub fn create_job(
    library: &Library,
    sources: &[PathBuf],
    options: &BatchOptions<'_>,
) -> Result<String> {
    if let Some(metadata) = options.metadata {
        metadata.validate()?;
    }
    let items: Vec<(SourceFile, Option<SourceFile>)> = pair_sources(sources, options.pair_raw)
        .into_iter()
        .map(|g| {
            (
                SourceFile::read(&g.photo),
                g.companion.as_deref().map(SourceFile::read),
            )
        })
        .collect();
    library.create_ingest_job(
        &serde_json::to_value(JobOptions::new(library, options)?)?,
        &items,
    )
}

/// What an unfinished job holds, for the app to say so.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobSummary {
    pub id: String,
    /// Photographs of the job.
    pub total: usize,
    /// Photographs still to import.
    pub open: usize,
    /// Of those, the ones whose files are there now as they were.
    pub available: usize,
}

impl JobSummary {
    pub fn of(job: &IngestJob) -> Self {
        let open: Vec<_> = job.open_items().collect();
        Self {
            id: job.id.clone(),
            total: job.items.len(),
            open: open.len(),
            available: open
                .iter()
                .filter(|item| item.source.state() == SourceState::There)
                .count(),
        }
    }
}

/// Imports the photographs of job `id` not yet in the library, one after
/// the other in the job's order, each reported through `on_file` as
/// `import_batch` does (`total`: the files still to import when it
/// starts), and puts the job's photographs into its album. A photograph
/// whose files are not there as they were (a card taken out, another card
/// with files of the same names) is reported with an error and left for a
/// later run; the job is then `Interrupted`, else `Done`; a cancel makes
/// it `Cancelled`. Fails at once only when the job's destination is not one
/// an import may copy into.
pub fn run_job(
    library: &Library,
    id: &str,
    cancel: &AtomicBool,
    mut on_file: impl FnMut(FileDone<'_>),
) -> Result<Vec<ImportResult>> {
    let job = library.ingest_job(id)?;
    let options: JobOptions = serde_json::from_value(job.options.clone())?;
    let folder = options
        .folder
        .as_deref()
        .map(|folder| library.resolve_stored(folder));
    let destination = match destination_folder(library, folder.as_deref(), &options.roots) {
        Ok(destination) => destination,
        Err(error) => {
            library.set_ingest_job_state(id, JobState::Interrupted)?;
            return Err(error);
        }
    };
    library.set_ingest_job_state(id, JobState::Running)?;
    let _batch = library.import_batch(rawtonic_catalog::ImportKind::Ingest);
    let items: Vec<_> = job.open_items().cloned().collect();
    let total = items
        .iter()
        .map(|item| 1 + usize::from(item.companion.is_some()))
        .sum();
    let mut results = Vec::with_capacity(total);
    // The files are read ahead while the one before is copied.
    let read_ahead = crate::copy::ReadAhead::start(
        items
            .iter()
            .flat_map(|item| std::iter::once(&item.source).chain(&item.companion))
            .map(|source| source.path.clone())
            .collect(),
    );
    let mut bytes = 0u64;
    let mut left = false;
    for item in &items {
        if cancel.load(Ordering::Acquire) {
            break;
        }
        let failed = |source: &Path, error: String| ImportResult {
            source: source.to_path_buf(),
            error: Some(error),
            ..Default::default()
        };
        let state = item.source.state();
        if state != SourceState::There {
            left = true;
            let error = match state {
                SourceState::Other => format!(
                    "Another file is at {} now (another card?); the photograph waits for its own",
                    item.source.path.display()
                ),
                _ => format!(
                    "{} is not there (a card or drive not connected?); it is imported when it is",
                    item.source.path.display()
                ),
            };
            let report: Vec<ImportResult> = std::iter::once(&item.source)
                .chain(&item.companion)
                .map(|source| failed(&source.path, error.clone()))
                .collect();
            for result in report {
                on_file(FileDone {
                    index: results.len(),
                    total,
                    result: &result,
                    bytes,
                });
                results.push(result);
            }
            continue;
        }
        let record = |planned: &Path, companion: Option<&Path>| {
            library.set_ingest_item(
                id,
                item.position,
                ItemState::Copying,
                Some(planned),
                companion,
                None,
            )
        };
        let import = ImportOptions {
            destination: &destination,
            template: &options.template,
            sequence: options.sequence_start + item.number,
            sequence_start: options.sequence_start,
            backup: options.backup.as_deref(),
            earlier: item.destination.as_deref(),
            before_copy: Some(&record),
            read_ahead: Some(&read_ahead),
        };
        // A JPEG gone or changed since (the RAW still there) does not hold
        // its RAW back: the RAW comes in alone.
        let (companion_source, lost_companion) = match &item.companion {
            Some(companion) if companion.state() == SourceState::There => {
                (Some(companion.path.as_path()), None)
            }
            Some(companion) => (
                None,
                Some(failed(
                    &companion.path,
                    format!(
                        "{} is no longer the JPEG of {} (gone or changed); the RAW came in alone",
                        companion.path.display(),
                        item.source.path.display()
                    ),
                )),
            ),
            None => (None, None),
        };
        let (mut photo, mut companion) = match import_pair(
            library,
            &item.source.path,
            companion_source,
            &import,
            None,
            cancel,
        ) {
            Ok(pair) => (pair.photo, pair.companion.or(lost_companion)),
            Err(error) => {
                let message = error.to_string();
                (
                    failed(&item.source.path, message.clone()),
                    item.companion.as_ref().map(|c| failed(&c.path, message)),
                )
            }
        };
        if cancel.load(Ordering::Acquire) && photo.error.is_some() {
            break;
        }
        // Recorded by a run cut off before the job knew: imported by that
        // run, not a duplicate.
        if photo.skipped && photo.destination.is_some() && photo.destination == item.destination {
            photo.skipped = false;
            if let Some(companion) = companion.as_mut().filter(|c| c.companion_of.is_some()) {
                companion.skipped = false;
            }
        }
        let mut done: Vec<ImportResult> = std::iter::once(photo).chain(companion).collect();
        // Before ItemState::Done, so a crash or catalog-write failure repeats it.
        // XMP conflicts remain pending and are reported with the photograph.
        // Duplicates belonging to earlier jobs are left untouched.
        // A resumed unfinished item intentionally reapplies its immutable
        // snapshot, including explicit overwrites. Completed items never replay.
        // Catalog-write errors stop here for retry rather than silently losing
        // the requested preset; optional camera fields were validated earlier.
        if let Some(metadata) = &options.metadata {
            for result in &mut done {
                crate::metadata::apply_to_copy(library, result, metadata)?;
            }
        }
        // Into the album before the photograph counts as done: a run cut
        // off in between does it again (it is idempotent).
        if done[0].error.is_none()
            && let Some(album) = &options.album
        {
            add_to_album(library, album, &done)?;
        }
        let photo = &done[0];
        // A JPEG copied but not recorded (the library busy): the item stays
        // open, and a later run records that copy.
        let unrecorded = done[1..]
            .iter()
            .any(|r| r.error.is_some() && r.destination.is_some());
        match &photo.error {
            None if unrecorded => left = true,
            None => library.set_ingest_item(
                id,
                item.position,
                ItemState::Done,
                photo.destination.as_deref(),
                None,
                None,
            )?,
            // The card was taken out meanwhile, or the copy is made but
            // could not be recorded (the library busy): left for a later
            // run, which records that copy.
            Some(_) if item.source.state() != SourceState::There || photo.destination.is_some() => {
                left = true
            }
            Some(error) => library.set_ingest_item(
                id,
                item.position,
                ItemState::Failed,
                None,
                None,
                Some(error),
            )?,
        }
        for result in done {
            if cancel.load(Ordering::Acquire) && result.error.is_some() {
                break;
            }
            if !result.skipped && result.error.is_none() {
                bytes += fs::metadata(&result.source).map(|m| m.len()).unwrap_or(0);
            }
            on_file(FileDone {
                index: results.len(),
                total,
                result: &result,
                bytes,
            });
            results.push(result);
        }
    }
    let state = if cancel.load(Ordering::Acquire) {
        JobState::Cancelled
    } else if left {
        JobState::Interrupted
    } else {
        JobState::Done
    };
    library.set_ingest_job_state(id, state)?;
    Ok(results)
}

/// Puts the photographs of `results` (one photograph's: the RAW, and its
/// JPEG when it came in as a photograph of its own; duplicates the library
/// held already included) into `album`. An album deleted meanwhile takes
/// none; another failure stops the run, its photograph not done (a later
/// run puts it in).
fn add_to_album(library: &Library, album: &str, results: &[ImportResult]) -> Result<()> {
    let assets: Vec<String> = results
        .iter()
        .filter(|r| r.error.is_none() && r.companion_of.is_none())
        .filter_map(|r| r.asset_id.clone())
        .collect();
    if assets.is_empty() {
        return Ok(());
    }
    match library.add_to_collection(album, &assets) {
        Err(error) if error.to_string() == "Unknown album" => Ok(()),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::batch::{BatchOptions, import_batch};
    use rawtonic_catalog::Mode;
    use std::panic::{AssertUnwindSafe, catch_unwind};

    fn raw(path: &Path) -> bool {
        path.extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("arw"))
    }
    fn options(template: &str) -> BatchOptions<'_> {
        BatchOptions {
            template,
            sequence_start: 1,
            backup: None,
            folder: None,
            roots: &[],
            pair_raw: Some(raw),
            album: None,
            metadata: None,
        }
    }
    fn write(path: &Path, bytes: &[u8]) -> PathBuf {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
        path.to_path_buf()
    }
    fn names(folder: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(folder)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }
    /// A card of pairs and single RAWs.
    fn card(dir: &Path) -> Vec<PathBuf> {
        let card = dir.join("card/DCIM/100CAM");
        let mut sources = Vec::new();
        for i in 0..4 {
            sources.push(write(
                &card.join(format!("C{i}.ARW")),
                format!("raw {i}").as_bytes(),
            ));
            if i % 2 == 0 {
                sources.push(write(
                    &card.join(format!("C{i}.JPG")),
                    format!("jpeg {i}").as_bytes(),
                ));
            }
        }
        sources
    }
    /// Opening the library again: the session's folder index registers
    /// whatever photograph files it finds (`session_bridge::index_folder`).
    fn reopen_and_index(lib: &Library) -> Library {
        let lib = Library::open(&lib.directory).unwrap();
        let capture = lib.directory.join("Capture");
        let files: Vec<PathBuf> = names(&capture).iter().map(|n| capture.join(n)).collect();
        lib.add_files(&files).unwrap();
        lib
    }

    /// Killed right after the copies of a pair were published, before the
    /// library recorded them: on reopening, the folder index leaves those
    /// copies to the job, and resuming records them as one photograph,
    /// copies the rest, and makes no copy twice.
    #[test]
    fn a_job_killed_between_copy_and_commit_resumes_without_duplicates() {
        let d = tempfile::tempdir().unwrap();
        let lib = Library::create(&d.path().join("Job"), "Job", Mode::Session).unwrap();
        let sources = card(d.path());
        let id = create_job(&lib, &sources, &options("{orig}")).unwrap();
        // What the killed run left: C0's pair planned and published.
        let capture = lib.directory.join("Capture");
        lib.set_ingest_item(
            &id,
            0,
            ItemState::Copying,
            Some(&capture.join("C0.ARW")),
            Some(&capture.join("C0.JPG")),
            None,
        )
        .unwrap();
        fs::copy(&sources[0], capture.join("C0.ARW")).unwrap();
        fs::copy(&sources[1], capture.join("C0.JPG")).unwrap();
        let lib = reopen_and_index(&lib);
        assert_eq!(lib.summary().unwrap().assets, 0, "left to the job");
        let jobs = lib.unfinished_ingest_jobs().unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(JobSummary::of(&jobs[0]).open, 4);
        let results = run_job(&lib, &id, &AtomicBool::new(false), |_| {}).unwrap();
        assert!(
            results.iter().all(|r| r.error.is_none() && !r.skipped),
            "{results:?}"
        );
        assert_eq!(
            names(&capture),
            ["C0.ARW", "C0.JPG", "C1.ARW", "C2.ARW", "C2.JPG", "C3.ARW"]
        );
        assert_eq!(lib.summary().unwrap().assets, 4);
        let c0 = lib.asset_at(&capture.join("C0.ARW")).unwrap().unwrap();
        assert_eq!(lib.companion(&c0).unwrap(), Some(capture.join("C0.JPG")));
        assert!(lib.unfinished_ingest_jobs().unwrap().is_empty());
        assert_eq!(lib.ingest_job(&id).unwrap().state, JobState::Done);
    }

    /// A photograph is recorded only once its copies reached permanent
    /// storage (`copy::Barrier`): when the drive cache cannot be flushed,
    /// none is recorded and the copies stay; after a reopen (the session's
    /// folder index), the job's next run records those copies, with their
    /// backups, without copying anything again.
    #[test]
    fn copies_not_made_durable_are_recorded_by_the_next_run() {
        for use_preset in [false, true] {
            let d = tempfile::tempdir().unwrap();
            let lib = Library::create(&d.path().join("Job"), "Job", Mode::Session).unwrap();
            let sources = card(d.path());
            let backup = d.path().join("backup");
            let preset = crate::metadata::ImportMetadata {
                creator: "Photographer".into(),
                ..Default::default()
            };
            let options = BatchOptions {
                backup: Some(&backup),
                metadata: use_preset.then_some(&preset),
                ..options("{orig}")
            };
            let id = create_job(&lib, &sources, &options).unwrap();
            crate::copy::FAIL_FLUSH.with(|fail| fail.set(true));
            let results = run_job(&lib, &id, &AtomicBool::new(false), |_| {});
            crate::copy::FAIL_FLUSH.with(|fail| fail.set(false));
            let results = results.unwrap();
            assert!(
                results
                    .iter()
                    .all(|r| r.error.as_deref().is_some_and(|e| e.contains("durable"))),
                "{results:?}"
            );
            assert_eq!(lib.summary().unwrap().assets, 0);
            assert!(
                !lib.directory.join("Capture/C0.xmp").exists(),
                "no preset XMP before the copy barrier"
            );
            let capture = lib.directory.join("Capture");
            let all = ["C0.ARW", "C0.JPG", "C1.ARW", "C2.ARW", "C2.JPG", "C3.ARW"];
            assert_eq!(names(&capture), all);
            assert_eq!(names(&backup), all);
            let lib = reopen_and_index(&lib);
            assert_eq!(lib.summary().unwrap().assets, 0, "left to the job");
            let results = run_job(&lib, &id, &AtomicBool::new(false), |_| {}).unwrap();
            assert!(results.iter().all(|r| r.error.is_none()), "{results:?}");
            let mut with_xmp = all.map(String::from).to_vec();
            if use_preset {
                with_xmp.extend((0..4).map(|i| format!("C{i}.xmp")));
            }
            with_xmp.sort();
            assert_eq!(names(&capture), with_xmp);
            assert_eq!(names(&backup), all);
            assert_eq!(lib.summary().unwrap().assets, 4);
            let c2 = lib.asset_at(&capture.join("C2.ARW")).unwrap().unwrap();
            assert_eq!(
                lib.metadata(&c2.id).unwrap().creator,
                if use_preset { "Photographer" } else { "" }
            );
            if use_preset {
                assert_eq!(lib.summary().unwrap().xmp_pending, 0);
            }
            assert_eq!(lib.companion(&c2).unwrap(), Some(capture.join("C2.JPG")));
            assert_eq!(lib.ingest_job(&id).unwrap().state, JobState::Done);
            for name in all {
                let card = sources.iter().find(|s| s.ends_with(name)).unwrap();
                assert_eq!(
                    fs::read(capture.join(name)).unwrap(),
                    fs::read(card).unwrap()
                );
            }
        }
    }

    /// Without a backup too, the next run makes the copies an earlier run
    /// left unflushed durable before it records them: they are synced and
    /// the drive cache is flushed again (macOS: counted).
    #[test]
    fn copies_reused_by_the_next_run_are_flushed_again() {
        let d = tempfile::tempdir().unwrap();
        let lib = Library::create(&d.path().join("Job"), "Job", Mode::Session).unwrap();
        let sources = card(d.path());
        let id = create_job(&lib, &sources, &options("{orig}")).unwrap();
        crate::copy::FAIL_FLUSH.with(|fail| fail.set(true));
        let results = run_job(&lib, &id, &AtomicBool::new(false), |_| {});
        crate::copy::FAIL_FLUSH.with(|fail| fail.set(false));
        assert!(results.unwrap().iter().all(|r| r.error.is_some()));
        crate::copy::FLUSHES.with(|count| count.set(0));
        let results = run_job(&lib, &id, &AtomicBool::new(false), |_| {}).unwrap();
        assert!(results.iter().all(|r| r.error.is_none()), "{results:?}");
        assert_eq!(lib.summary().unwrap().assets, 4);
        #[cfg(target_vendor = "apple")]
        assert_eq!(
            crate::copy::FLUSHES.with(std::cell::Cell::get),
            4,
            "one per photograph"
        );
    }

    /// A companion JPEG whose backup copy fails (a full or failing backup
    /// drive) still reaches the library beside its RAW; the backup failure
    /// only warns, as it did before both came from one read.
    #[test]
    fn a_failed_backup_of_a_jpeg_leaves_its_library_copy() {
        let d = tempfile::tempdir().unwrap();
        let lib = Library::create(&d.path().join("Job"), "Job", Mode::Session).unwrap();
        let sources = card(d.path());
        let backup = d.path().join("backup");
        let options = BatchOptions {
            backup: Some(&backup),
            ..options("{orig}")
        };
        let failing = backup.clone();
        crate::copy::set_after_write(Some(Box::new(move |temp: &Path| {
            if temp.starts_with(&failing) && fs::read(temp).unwrap().starts_with(b"jpeg") {
                fs::write(temp, b"jpeg damaged").unwrap();
            }
        })));
        let results = import_batch(&lib, &sources, &options, &AtomicBool::new(false), |_| {});
        crate::copy::set_after_write(None);
        let results = results.unwrap();
        assert!(results.iter().all(|r| r.error.is_none()), "{results:?}");
        let capture = lib.directory.join("Capture");
        assert_eq!(
            names(&capture),
            ["C0.ARW", "C0.JPG", "C1.ARW", "C2.ARW", "C2.JPG", "C3.ARW"]
        );
        assert_eq!(names(&backup), ["C0.ARW", "C1.ARW", "C2.ARW", "C3.ARW"]);
        let c0 = lib.asset_at(&capture.join("C0.ARW")).unwrap().unwrap();
        assert_eq!(lib.companion(&c0).unwrap(), Some(capture.join("C0.JPG")));
        let c0_result = results
            .iter()
            .find(|r| r.source.ends_with("C0.ARW"))
            .unwrap();
        assert!(
            c0_result
                .warning
                .as_deref()
                .is_some_and(|w| w.contains("Backup")),
            "{c0_result:?}"
        );
    }

    /// The library could not record a copy (busy, a full disk): the job
    /// stays open and a later run records that copy.
    #[test]
    fn a_copy_the_library_could_not_record_is_recorded_later() {
        let d = tempfile::tempdir().unwrap();
        let lib = Library::create(&d.path().join("Job"), "Job", Mode::Session).unwrap();
        let sources = card(d.path());
        lib.inject_write_failure("assets").unwrap();
        let results = import_batch(
            &lib,
            &sources,
            &options("{orig}"),
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        assert!(results[0].error.is_some());
        lib.clear_write_failures().unwrap();
        let jobs = lib.unfinished_ingest_jobs().unwrap();
        assert_eq!(jobs.len(), 1);
        run_job(&lib, &jobs[0].id, &AtomicBool::new(false), |_| {}).unwrap();
        let capture = lib.directory.join("Capture");
        assert_eq!(names(&capture).len(), 6, "{:?}", names(&capture));
        assert_eq!(lib.summary().unwrap().assets, 4);
    }

    /// A card folder taken out of the card reader (renamed away from under
    /// the import, what unmounting looks like to the files). The reader is
    /// emptied after the first photograph has been imported (its copy is
    /// published and registered when its event arrives, which is when the
    /// reader's light goes out); the import has that file's handles open
    /// then, and Windows refuses to rename a folder with open files below
    /// it, so there the card is renamed at the next event, once the last
    /// handle of the first photograph is closed. Either way files after it
    /// are gone. The rest waits (interrupted), is imported when the card is
    /// back, and a cancel is never resumed.
    #[test]
    fn a_card_taken_out_is_finished_when_it_is_back_and_a_cancel_is_final() {
        let d = tempfile::tempdir().unwrap();
        let lib = Library::create(&d.path().join("Job"), "Job", Mode::Session).unwrap();
        let sources = card(d.path());
        let card_dir = d.path().join("card");
        let away = d.path().join("away");
        let mut taken = false;
        let results = import_batch(
            &lib,
            &sources,
            &options("{orig}"),
            &AtomicBool::new(false),
            |done| {
                // The first photograph is in the library (or the card is
                // already gone) and its handles are closed, as with the
                // press that empties a reader: every event from there on
                // is after the pressed card.
                if done.index >= 1 && !taken {
                    fs::rename(&card_dir, &away).unwrap();
                    taken = true;
                }
            },
        )
        .unwrap();
        assert!(
            results[2..].iter().all(|r| r.error.is_some()),
            "{results:?}"
        );
        let jobs = lib.unfinished_ingest_jobs().unwrap();
        assert_eq!(jobs[0].state, JobState::Interrupted);
        assert_eq!(JobSummary::of(&jobs[0]).available, 0);
        fs::rename(&away, &card_dir).unwrap();
        assert_eq!(JobSummary::of(&jobs[0]).available, 3);
        run_job(&lib, &jobs[0].id, &AtomicBool::new(false), |_| {}).unwrap();
        assert_eq!(lib.summary().unwrap().assets, 4);
        assert!(lib.unfinished_ingest_jobs().unwrap().is_empty());

        let other = Library::create(&d.path().join("Other"), "Other", Mode::Session).unwrap();
        let cancel = AtomicBool::new(false);
        import_batch(&other, &sources, &options("{orig}"), &cancel, |_| {
            cancel.store(true, Ordering::Release)
        })
        .unwrap();
        assert!(other.unfinished_ingest_jobs().unwrap().is_empty());
    }

    /// Property-style: a run killed at a random point (the process gone
    /// mid-job: nothing after it runs), the library reopened and indexed,
    /// then resumed, repeatedly: every file of the card ends in the
    /// library once, pairs as one photograph, no copy twice.
    #[test]
    fn jobs_killed_anywhere_resume_to_the_same_library() {
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = move |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        for round in 0..40 {
            let d = tempfile::tempdir().unwrap();
            let mut lib = Library::create(&d.path().join("Job"), "Job", Mode::Session).unwrap();
            let sources = card(d.path());
            let template = ["{orig}", "S{seq:2}"][next(2) as usize];
            let id = create_job(&lib, &sources, &options(template)).unwrap();
            for _ in 0..3 {
                let kill_after = next(sources.len() as u64 + 1) as usize;
                let mut seen = 0;
                let killed = catch_unwind(AssertUnwindSafe(|| {
                    run_job(&lib, &id, &AtomicBool::new(false), |_| {
                        seen += 1;
                        if seen > kill_after {
                            panic!("killed");
                        }
                    })
                }));
                if killed.is_ok() {
                    break;
                }
                lib = reopen_and_index(&lib);
            }
            run_job(&lib, &id, &AtomicBool::new(false), |_| {}).unwrap();
            let capture = lib.directory.join("Capture");
            assert_eq!(
                names(&capture).len(),
                sources.len(),
                "round {round}: {:?}",
                names(&capture)
            );
            assert_eq!(lib.summary().unwrap().assets, 4, "round {round}");
            assert_eq!(lib.companions().unwrap().len(), 2, "round {round}");
            assert_eq!(lib.ingest_job(&id).unwrap().state, JobState::Done);
        }
    }

    /// Another card with files of the same names (the camera's numbering
    /// started again) is never taken for the interrupted one: its files
    /// wait, and come in when the right card is back.
    #[test]
    fn another_card_with_the_same_names_is_not_resumed() {
        let d = tempfile::tempdir().unwrap();
        let lib = Library::create(&d.path().join("Job"), "Job", Mode::Session).unwrap();
        let sources = card(d.path());
        let id = create_job(&lib, &sources, &options("{orig}")).unwrap();
        let card_dir = d.path().join("card");
        fs::rename(&card_dir, d.path().join("first")).unwrap();
        for source in &sources {
            write(source, b"another card's photograph");
        }
        let job = lib.ingest_job(&id).unwrap();
        assert_eq!(JobSummary::of(&job).available, 0);
        let results = run_job(&lib, &id, &AtomicBool::new(false), |_| {}).unwrap();
        assert!(results.iter().all(|r| {
            r.error
                .as_deref()
                .is_some_and(|e| e.contains("Another file"))
        }));
        assert_eq!(lib.summary().unwrap().assets, 0);
        assert_eq!(lib.ingest_job(&id).unwrap().state, JobState::Interrupted);
        fs::remove_dir_all(&card_dir).unwrap();
        fs::rename(d.path().join("first"), &card_dir).unwrap();
        run_job(&lib, &id, &AtomicBool::new(false), |_| {}).unwrap();
        assert_eq!(lib.summary().unwrap().assets, 4);
        assert_eq!(lib.ingest_job(&id).unwrap().state, JobState::Done);
    }

    /// A session with an interrupted job moved while closed: its planned
    /// copies are found at the new place (stored relative to it), left to
    /// the job by the index, and the job finishes there, into the folder
    /// it was given.
    #[test]
    fn an_interrupted_session_moved_while_closed_resumes_in_its_new_place() {
        let d = tempfile::tempdir().unwrap();
        let lib = Library::create(&d.path().join("Job"), "Job", Mode::Session).unwrap();
        let selects = lib.directory.join("Selects");
        let sources = card(d.path());
        let mut opts = options("{orig}");
        opts.folder = Some(&selects);
        let id = create_job(&lib, &sources, &opts).unwrap();
        lib.set_ingest_item(
            &id,
            0,
            ItemState::Copying,
            Some(&selects.join("C0.ARW")),
            Some(&selects.join("C0.JPG")),
            None,
        )
        .unwrap();
        fs::copy(&sources[0], selects.join("C0.ARW")).unwrap();
        fs::copy(&sources[1], selects.join("C0.JPG")).unwrap();
        drop(lib);
        fs::rename(d.path().join("Job"), d.path().join("Moved")).unwrap();
        let lib = Library::open(&d.path().join("Moved")).unwrap();
        let selects = lib.directory.join("Selects");
        let files: Vec<PathBuf> = names(&selects).iter().map(|n| selects.join(n)).collect();
        assert_eq!(lib.add_files(&files).unwrap().importing.len(), 2);
        run_job(&lib, &id, &AtomicBool::new(false), |_| {}).unwrap();
        assert_eq!(names(&selects).len(), 6);
        let c0 = lib.asset_at(&selects.join("C0.ARW")).unwrap().unwrap();
        assert_eq!(lib.companion(&c0).unwrap(), Some(selects.join("C0.JPG")));
        assert_eq!(lib.summary().unwrap().assets, 4);
    }

    /// The album a job imports into is kept with it: photographs a run cut
    /// off brought in, and those the resumed run brings, all go into it.
    #[test]
    fn a_resumed_job_puts_all_its_photographs_into_its_album() {
        let d = tempfile::tempdir().unwrap();
        let lib = Library::create(&d.path().join("Job"), "Job", Mode::Session).unwrap();
        let album = lib.create_collection("Client picks", None).unwrap();
        let sources = card(d.path());
        let mut opts = options("{orig}");
        opts.album = Some(&album.id);
        let id = create_job(&lib, &sources, &opts).unwrap();
        let killed = catch_unwind(AssertUnwindSafe(|| {
            run_job(&lib, &id, &AtomicBool::new(false), |done| {
                if done.index == 1 {
                    panic!("killed");
                }
            })
        }));
        assert!(killed.is_err());
        // Each photograph went into the album as it came in.
        assert_eq!(lib.collection_assets(&album.id).unwrap().len(), 1);
        run_job(&lib, &id, &AtomicBool::new(false), |_| {}).unwrap();
        assert_eq!(lib.collection_assets(&album.id).unwrap().len(), 4);
    }

    /// Killed at the very end (the last photograph done, nothing after):
    /// every photograph is in the album already. A JPEG that came in as a
    /// photograph of its own (its RAW was in the library) goes in too.
    #[test]
    fn album_membership_comes_with_each_photograph() {
        let d = tempfile::tempdir().unwrap();
        let lib = Library::create(&d.path().join("Job"), "Job", Mode::Session).unwrap();
        let album = lib.create_collection("Client picks", None).unwrap();
        let sources = card(d.path());
        let mut opts = options("{orig}");
        opts.album = Some(&album.id);
        // C0's RAW came in alone before (no album).
        import_batch(
            &lib,
            &sources[..1],
            &options("{orig}"),
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        let id = create_job(&lib, &sources, &opts).unwrap();
        let total = sources.len();
        let killed = catch_unwind(AssertUnwindSafe(|| {
            run_job(&lib, &id, &AtomicBool::new(false), |done| {
                if done.index + 1 == total {
                    panic!("killed");
                }
            })
        }));
        assert!(killed.is_err());
        // C0 (held already), C0.JPG (its own photograph now), C1, C2, C3.
        assert_eq!(lib.collection_assets(&album.id).unwrap().len(), 5);
        assert_eq!(lib.summary().unwrap().assets, 5);
        // Left running with nothing to do: settled as done.
        assert_eq!(lib.ingest_job(&id).unwrap().state, JobState::Running);
        assert_eq!(lib.settle_ingest_jobs().unwrap(), 1);
        assert_eq!(lib.ingest_job(&id).unwrap().state, JobState::Done);
    }

    /// A source reached through a symlink is read through it, as the copy
    /// does: its job is not left waiting for it.
    #[cfg(unix)]
    #[test]
    fn a_symlinked_source_is_imported() {
        let d = tempfile::tempdir().unwrap();
        let lib = Library::create(&d.path().join("Job"), "Job", Mode::Session).unwrap();
        let real = write(&d.path().join("drive/A.ARW"), b"raw");
        let link = d.path().join("links/A.ARW");
        fs::create_dir_all(link.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let results = import_batch(
            &lib,
            &[link],
            &options("{orig}"),
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        assert!(results[0].error.is_none(), "{results:?}");
        assert_eq!(lib.summary().unwrap().assets, 1);
        assert!(lib.unfinished_ingest_jobs().unwrap().is_empty());
    }

    /// Cut off after its backup copy was published, before the library's:
    /// the backup is kept, not made a second time.
    #[test]
    fn a_backup_a_cut_off_run_made_is_kept() {
        let d = tempfile::tempdir().unwrap();
        let lib = Library::create(&d.path().join("Job"), "Job", Mode::Session).unwrap();
        let backup = d.path().join("backup");
        let sources = card(d.path());
        fs::create_dir_all(&backup).unwrap();
        fs::copy(&sources[2], backup.join("C1.ARW")).unwrap();
        let mut opts = options("{orig}");
        opts.backup = Some(&backup);
        import_batch(&lib, &sources, &opts, &AtomicBool::new(false), |_| {}).unwrap();
        assert_eq!(
            names(&backup),
            ["C0.ARW", "C0.JPG", "C1.ARW", "C2.ARW", "C2.JPG", "C3.ARW"]
        );
    }

    /// Different bytes of the same size with the same modification time
    /// (fixed-size RAWs, a card's coarse timestamps) are not the source.
    #[test]
    fn same_size_and_time_with_other_content_is_not_the_source() {
        let d = tempfile::tempdir().unwrap();
        let lib = Library::create(&d.path().join("Job"), "Job", Mode::Session).unwrap();
        let source = write(&d.path().join("card/A.ARW"), &[1u8; 200_000]);
        let id = create_job(&lib, std::slice::from_ref(&source), &options("{orig}")).unwrap();
        let time = fs::metadata(&source).unwrap().modified().unwrap();
        fs::write(&source, [2u8; 200_000]).unwrap();
        fs::File::options()
            .write(true)
            .open(&source)
            .unwrap()
            .set_modified(time)
            .unwrap();
        assert_eq!(fs::metadata(&source).unwrap().modified().unwrap(), time);
        let results = run_job(&lib, &id, &AtomicBool::new(false), |_| {}).unwrap();
        assert!(
            results[0]
                .error
                .as_deref()
                .is_some_and(|e| e.contains("Another file"))
        );
        assert_eq!(lib.summary().unwrap().assets, 0);
    }

    /// The RAW in the library already, its JPEG copied as a photograph of
    /// its own but not recorded (the library busy): the job stays open and
    /// a later run records that copy, no second one.
    #[test]
    fn an_unrecorded_jpeg_keeps_its_item_open() {
        let d = tempfile::tempdir().unwrap();
        let lib = Library::create(&d.path().join("Job"), "Job", Mode::Session).unwrap();
        let sources = card(d.path());
        import_batch(
            &lib,
            &sources[..1],
            &options("{orig}"),
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        let id = create_job(&lib, &sources[..2], &options("{orig}")).unwrap();
        lib.inject_write_failure("assets").unwrap();
        let results = run_job(&lib, &id, &AtomicBool::new(false), |_| {}).unwrap();
        assert!(
            results[1].error.is_some() && results[1].destination.is_some(),
            "{results:?}"
        );
        lib.clear_write_failures().unwrap();
        assert_eq!(lib.unfinished_ingest_jobs().unwrap().len(), 1);
        run_job(&lib, &id, &AtomicBool::new(false), |_| {}).unwrap();
        let capture = lib.directory.join("Capture");
        assert_eq!(names(&capture), ["C0.ARW", "C0.JPG"]);
        assert_eq!(lib.summary().unwrap().assets, 2);
        assert_eq!(lib.ingest_job(&id).unwrap().state, JobState::Done);
    }

    /// The album could not be written (the library busy): the photograph
    /// is not done, and the next run puts it in.
    #[test]
    fn an_album_write_failure_is_done_again_later() {
        let d = tempfile::tempdir().unwrap();
        let lib = Library::create(&d.path().join("Job"), "Job", Mode::Session).unwrap();
        let album = lib.create_collection("Client picks", None).unwrap();
        let sources = card(d.path());
        let mut opts = options("{orig}");
        opts.album = Some(&album.id);
        let id = create_job(&lib, &sources[2..3], &opts).unwrap();
        lib.inject_write_failure("collection_assets").unwrap();
        assert!(run_job(&lib, &id, &AtomicBool::new(false), |_| {}).is_err());
        lib.clear_write_failures().unwrap();
        assert_eq!(lib.unfinished_ingest_jobs().unwrap().len(), 1);
        run_job(&lib, &id, &AtomicBool::new(false), |_| {}).unwrap();
        assert_eq!(lib.collection_assets(&album.id).unwrap().len(), 1);
        assert_eq!(lib.ingest_job(&id).unwrap().state, JobState::Done);
    }
}
