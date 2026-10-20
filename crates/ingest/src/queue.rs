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
    ingest_jobs::{IngestJob, ItemState, JobState},
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
    pub folder: Option<PathBuf>,
    pub roots: Vec<PathBuf>,
}

impl JobOptions {
    pub fn new(options: &BatchOptions<'_>) -> Self {
        Self {
            template: options.template.into(),
            sequence_start: options.sequence_start,
            backup: options.backup.map(Into::into),
            folder: options.folder.map(Into::into),
            roots: options.roots.to_vec(),
        }
    }
}

/// Records an ingest job of `sources` with `options` (each photograph with
/// its companion, `pair_sources`) and returns its id; `run_job` imports it.
pub fn create_job(
    library: &Library,
    sources: &[PathBuf],
    options: &BatchOptions<'_>,
) -> Result<String> {
    let items: Vec<(PathBuf, Option<PathBuf>)> = pair_sources(sources, options.pair_raw)
        .into_iter()
        .map(|g| (g.photo, g.companion))
        .collect();
    library.create_ingest_job(&serde_json::to_value(JobOptions::new(options))?, &items)
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
    /// Of those, the ones whose source is there now.
    pub available: usize,
}

impl JobSummary {
    pub fn of(job: &IngestJob) -> Self {
        let open: Vec<_> = job.open_items().collect();
        Self {
            id: job.id.clone(),
            total: job.items.len(),
            open: open.len(),
            available: open.iter().filter(|item| item.source.is_file()).count(),
        }
    }
}

/// Imports the photographs of job `id` not yet in the library, one after
/// the other in the job's order, each reported through `on_file` as
/// `import_batch` does (`total`: the files still to import when it
/// starts). A photograph whose source is not there (a card taken out) is
/// reported with an error and left for a later run; the job is then
/// `Interrupted`, else `Done`; a cancel makes it `Cancelled`. Fails at once
/// only when the job's destination is not one an import may copy into.
pub fn run_job(
    library: &Library,
    id: &str,
    cancel: &AtomicBool,
    mut on_file: impl FnMut(FileDone<'_>),
) -> Result<Vec<ImportResult>> {
    let job = library.ingest_job(id)?;
    let options: JobOptions = serde_json::from_value(job.options.clone())?;
    let destination = match destination_folder(library, options.folder.as_deref(), &options.roots) {
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
        if !item.source.is_file() {
            left = true;
            let error = format!(
                "{} is not there (a card or drive not connected?); it is imported when it is",
                item.source.display()
            );
            let report: Vec<ImportResult> = std::iter::once(&item.source)
                .chain(&item.companion)
                .map(|source| failed(source, error.clone()))
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
        };
        let (mut photo, mut companion) = match import_pair(
            library,
            &item.source,
            item.companion.as_deref(),
            &import,
            None,
            cancel,
        ) {
            Ok(pair) => (pair.photo, pair.companion),
            Err(error) => {
                let message = error.to_string();
                (
                    failed(&item.source, message.clone()),
                    item.companion.as_deref().map(|c| failed(c, message)),
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
        match &photo.error {
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
            Some(_) if !item.source.is_file() || photo.destination.is_some() => left = true,
            Some(error) => library.set_ingest_item(
                id,
                item.position,
                ItemState::Failed,
                None,
                None,
                Some(error),
            )?,
        }
        for result in std::iter::once(photo).chain(companion) {
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

    /// The card taken out mid-import: the rest waits (interrupted), is
    /// imported when the card is back, and a cancel is never resumed.
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
                if done.index == 1 && !taken {
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
}
