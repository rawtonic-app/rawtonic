//! Synchronize Folder and Previous Import for the open library (glue over
//! `rawtonic_catalog::folder_sync` and `add`), and the background pass that
//! records content hashes so moves can be verified later.
use crate::session_bridge::{after_relink, with_library};
use rawtonic_catalog::{
    ImportBatchInfo, Library,
    folder_sync::{Cancelled, SyncAction, SyncApplied, SyncOptions, SyncPlan, SyncProgress},
};
use std::{
    cell::Cell,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tauri::Emitter;

static CANCEL_SYNC: AtomicBool = AtomicBool::new(false);
static SYNC_RUNNING: AtomicBool = AtomicBool::new(false);

/// The event carrying a scan's progress (`SyncProgress`).
pub const SYNC_PROGRESS: &str = "folder-sync-progress";

/// Scans `folder` of the library at `directory` (the whole library without
/// one) for photographs moved, renamed, added or deleted outside Rawtonic,
/// reporting progress with `folder-sync-progress`. Reads only; cancelled with
/// `folder_sync_cancel`.
#[tauri::command]
pub async fn folder_sync_scan(
    directory: String,
    folder: Option<String>,
    app: tauri::AppHandle,
) -> Result<SyncPlan, String> {
    if SYNC_RUNNING.swap(true, Ordering::AcqRel) {
        return Err("A synchronization is already running".into());
    }
    CANCEL_SYNC.store(false, Ordering::Release);
    let result = with_library(directory, false, move |lib| {
        let last = Cell::new(Instant::now() - Duration::from_secs(1));
        let progress = |progress: SyncProgress| {
            // At most ten updates a second, and always the last one.
            if last.get().elapsed() >= Duration::from_millis(100)
                || (progress.total > 0 && progress.done == progress.total)
            {
                last.set(Instant::now());
                let _ = app.emit(SYNC_PROGRESS, progress);
            }
        };
        lib.scan_folder_sync(
            folder.as_deref().map(Path::new),
            &SyncOptions {
                accept: &|path| crate::formats::is_supported_image_file(path),
                cancel: &CANCEL_SYNC,
                progress: &progress,
            },
        )
        .map_err(|e| {
            if e.downcast_ref::<Cancelled>().is_some() {
                "cancelled".to_string()
            } else {
                format!("{e:#}")
            }
        })
    })
    .await
    .map_err(|e| e.to_string());
    SYNC_RUNNING.store(false, Ordering::Release);
    result?
}

#[tauri::command]
pub fn folder_sync_cancel() {
    CANCEL_SYNC.store(true, Ordering::Release);
}

/// Applies the changes the user accepted after a scan. Each is checked
/// again and skipped when disk or library changed since; moved photographs
/// take their previews along and get their derived sidecars written as
/// after a relink. Files on disk are never moved, modified or deleted.
/// When adding the new files fails after the moves and removals were
/// applied, those are still followed and reported, the files in `skipped`
/// (and `addError`).
#[tauri::command]
pub async fn folder_sync_apply(
    directory: String,
    actions: Vec<SyncAction>,
) -> Result<SyncApplied, String> {
    with_library(directory, true, move |lib| {
        let applied = lib
            .apply_folder_sync(&actions)
            .map_err(|e| format!("{e:#}"))?;
        if let Some(error) = &applied.add_error {
            log::warn!("Synchronize Folder: new files not added: {error}");
        }
        after_relink(lib, &applied.relinked());
        if !applied.added.is_empty() {
            start_checksum_pass(lib.directory.clone());
            crate::library_search::start_facts_pass(lib.directory.clone());
        }
        Ok(applied)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// The newest import batch and its photographs still in the library.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PreviousImport {
    pub batch: Option<ImportBatchInfo>,
    pub paths: Vec<PathBuf>,
}

/// Lightroom's Previous Import: the photographs the newest ingest, Add or
/// Synchronize Folder added.
#[tauri::command]
pub async fn session_previous_import(directory: String) -> Result<PreviousImport, String> {
    with_library(directory, false, |lib| {
        Ok(PreviousImport {
            batch: lib.previous_import().map_err(|e| e.to_string())?,
            paths: lib
                .previous_import_assets()
                .map_err(|e| e.to_string())?
                .into_iter()
                .map(|asset| asset.path)
                .collect(),
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// The running background pass, stopped when another one starts.
static CHECKSUM_PASS: Mutex<Option<Arc<AtomicBool>>> = Mutex::new(None);

/// How long a pass waits before reading files: opening the library and its
/// first previews come first.
const PASS_DELAY: Duration = Duration::from_secs(20);
/// The pause between two files of a pass.
const PASS_PAUSE: Duration = Duration::from_millis(25);

/// Stops the running pass, if any (its library is about to move).
pub fn stop_checksum_pass() {
    if let Ok(mut running) = CHECKSUM_PASS.lock()
        && let Some(cancel) = running.take()
    {
        cancel.store(true, Ordering::Release);
    }
}

/// Event: what an interrupted job left was removed or found
/// ([`LeftoversReport`]); the activity centre says so once.
pub const LEFTOVERS_REMOVED: &str = "leftovers-removed";

/// What [`report_leftovers`] tells the webview.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LeftoversReport {
    pub temp_files: usize,
    pub folders: usize,
    pub bytes: u64,
    /// Staging folders left in place (unmarked, in use or not plain).
    pub kept: Vec<PathBuf>,
}

/// Logs what a sweep removed and, when it removed or kept anything, tells
/// the webview.
pub fn report_leftovers(
    app: &tauri::AppHandle,
    temp: rawtonic_metadata::temp::Removed,
    staging: &rawtonic_metadata::staging::Removed,
) {
    let temp_files = temp.files;
    use tauri::Emitter;
    if temp_files == 0 && staging.folders == 0 && staging.kept.is_empty() {
        return;
    }
    log::info!(
        "Interrupted jobs' leftovers: removed {temp_files} temporary files ({} bytes) and {} staging folders ({} bytes); left {:?}",
        temp.bytes,
        staging.folders,
        staging.bytes,
        staging.kept
    );
    let _ = app.emit(
        LEFTOVERS_REMOVED,
        LeftoversReport {
            temp_files,
            folders: staging.folders,
            // What the activity centre says was freed: temporary files too.
            bytes: staging.bytes + temp.bytes,
            kept: staging.kept.clone(),
        },
    );
}

/// Removes, in the background, what a killed app left in the library at
/// `directory` (`Library::sweep_leftovers`, G66): stale temporary files of
/// ours and abandoned Import from Catalog staging folders.
pub fn start_leftover_sweep(app: tauri::AppHandle, directory: PathBuf) {
    let spawned = std::thread::Builder::new()
        .name("leftovers".into())
        .spawn(move || {
            let Ok(lib) = Library::open(&directory) else {
                return;
            };
            match lib.sweep_leftovers(std::time::SystemTime::now(), &AtomicBool::new(false)) {
                Ok(swept) => report_leftovers(
                    &app,
                    rawtonic_metadata::temp::Removed {
                        files: swept.temp_files,
                        bytes: swept.temp_bytes,
                    },
                    &swept.staging,
                ),
                Err(e) => log::warn!("Leftovers of interrupted jobs not swept: {e:#}"),
            }
        });
    if let Err(e) = spawned {
        log::warn!("Leftovers of interrupted jobs not swept: {e}");
    }
}

/// Starts recording the content hash (SHA-256) of every photograph of the
/// library at `directory` that has none yet, in the background: one file
/// at a time, with pauses, after a delay; it stops when it is done, when
/// another library opens or another pass starts. Only reads the
/// photographs; a hash records the file as registered.
pub fn start_checksum_pass(directory: PathBuf) {
    let cancel = Arc::new(AtomicBool::new(false));
    if let Ok(mut running) = CHECKSUM_PASS.lock()
        && let Some(previous) = running.replace(cancel.clone())
    {
        previous.store(true, Ordering::Release);
    }
    let spawned = std::thread::Builder::new()
        .name("checksums".into())
        .spawn(move || {
            let started = Instant::now();
            while started.elapsed() < PASS_DELAY {
                if cancel.load(Ordering::Acquire) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            crate::gpu_processing::after_first_display_variant();
            let mut cursor = 0;
            let mut recorded = 0;
            loop {
                let Ok(lib) = Library::open(&directory) else {
                    return;
                };
                match lib.fill_checksums(&mut cursor, 64, PASS_PAUSE, &cancel) {
                    Ok((0, _)) => break,
                    Ok((_, done)) => recorded += done,
                    Err(e) => {
                        log::warn!("Content hashes paused: {e:#}");
                        return;
                    }
                }
                if cancel.load(Ordering::Acquire) {
                    return;
                }
            }
            if recorded > 0 {
                log::info!(
                    "Recorded the content hashes of {recorded} photographs in {:.0} s",
                    started.elapsed().as_secs_f64()
                );
            }
        });
    if let Err(e) = spawned {
        log::warn!("Content hashes not recorded: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rawtonic_catalog::Mode;
    use std::fs;
    use tauri::async_runtime::block_on;

    #[test]
    fn previous_import_lists_the_newest_batch_and_apply_follows_moves() {
        let temp = tempfile::tempdir().unwrap();
        let lib = Library::create(temp.path(), "Sync", Mode::Session).unwrap();
        let capture = lib.directory.join("Capture");
        fs::write(capture.join("A.ARW"), vec![1u8; 200_000]).unwrap();
        {
            let _batch = lib.import_batch(rawtonic_catalog::ImportKind::Ingest);
            lib.add_files(&[capture.join("A.ARW")]).unwrap();
        }
        lib.fill_all_checksums(&AtomicBool::new(false)).unwrap();
        let directory = lib.directory.to_string_lossy().into_owned();
        let previous = block_on(session_previous_import(directory.clone())).unwrap();
        assert_eq!(previous.paths, [capture.join("A.ARW")]);
        fs::rename(capture.join("A.ARW"), capture.join("B.ARW")).unwrap();
        fs::write(capture.join("C.ARW"), vec![3u8; 210_000]).unwrap();
        let plan = lib
            .scan_folder_sync(
                None,
                &SyncOptions {
                    accept: &|p| crate::formats::is_supported_image_file(p),
                    cancel: &AtomicBool::new(false),
                    progress: &|_| {},
                },
            )
            .unwrap();
        let mut actions: Vec<SyncAction> =
            plan.verified_moves.iter().map(SyncAction::from).collect();
        actions.extend(plan.new_files.iter().map(|f| SyncAction::Add {
            path: f.path.clone(),
            identity: f.identity.clone(),
        }));
        let applied = block_on(folder_sync_apply(directory.clone(), actions)).unwrap();
        assert_eq!(applied.moved.len(), 1);
        assert_eq!(applied.added, [capture.join("C.ARW")]);
        let previous = block_on(session_previous_import(directory)).unwrap();
        assert_eq!(previous.paths, [capture.join("C.ARW")]);
        assert_eq!(
            previous.batch.unwrap().kind,
            rawtonic_catalog::ImportKind::Sync
        );
        assert_eq!(fs::read(capture.join("B.ARW")).unwrap(), vec![1u8; 200_000]);
    }
}
