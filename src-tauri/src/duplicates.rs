//! Find Duplicates, stage 1 (`rawtonic_catalog::duplicates`): the background
//! pass that reads the image data hash of every photograph of a library
//! without a current one, the groups of photographs with identical image
//! data for the review, and the check before the photographs the user
//! marked go to the Trash (the webview trashes them through the journaled
//! file operations, `delete_files_from_disk`, one undoable step).
//!
//! One pass at a time: it reports `DUPLICATES_PROGRESS` and ends with
//! `DUPLICATES_FINISHED` (and `FACTS_RECORDED`, so smart albums with the
//! "Is duplicate" rule count again); `duplicates_cancel` stops it, keeping
//! what it recorded.
use crate::file_management::parse_virtual_path;
use crate::library_search::{FACTS_RECORDED, announcer};
use crate::session_bridge::with_library;
use rawtonic_catalog::Library;
use rawtonic_catalog::duplicates::DuplicateMember;
use serde::Serialize;
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tauri::Emitter;

pub const DUPLICATES_PROGRESS: &str = "duplicates-progress";
pub const DUPLICATES_FINISHED: &str = "duplicates-finished";

/// Files read at the same time: measured on the Lenovo X13 (1,000 RAWs,
/// cold cache) 15.2 s with one, 8.2 s with two, 6.5 s with four (notes:
/// benchmark-results/dup-stage1-2026-10-31).
const WORKERS: usize = 4;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Progress {
    run: u64,
    directory: String,
    done: usize,
    total: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Finished {
    run: u64,
    directory: String,
    photos: usize,
    hashed: usize,
    unreadable: Vec<String>,
    cancelled: bool,
    error: Option<String>,
    seconds: f64,
}

/// The running pass: its number, library and stop switch.
struct Pass {
    run: u64,
    directory: PathBuf,
    cancel: Arc<AtomicBool>,
}

static PASS: Mutex<Option<Pass>> = Mutex::new(None);
static RUNS: AtomicU64 = AtomicU64::new(0);

/// Starts the pass for the library at `directory` and returns its number;
/// one already running for it goes on (its number is returned), one for
/// another library is stopped.
fn start(app: tauri::AppHandle, directory: PathBuf) -> Result<u64, String> {
    let cancel = Arc::new(AtomicBool::new(false));
    let run = {
        let mut running = PASS.lock().map_err(|e| e.to_string())?;
        if let Some(pass) = running.as_ref() {
            if pass.directory == directory && !pass.cancel.load(Ordering::Acquire) {
                return Ok(pass.run);
            }
            pass.cancel.store(true, Ordering::Release);
        }
        let run = RUNS.fetch_add(1, Ordering::Relaxed) + 1;
        *running = Some(Pass {
            run,
            directory: directory.clone(),
            cancel: cancel.clone(),
        });
        run
    };
    std::thread::Builder::new()
        .name("image-hashes".into())
        .spawn(move || {
            let started = Instant::now();
            let name = directory.to_string_lossy().into_owned();
            let mut last = None::<Instant>;
            let result = Library::open(&directory).and_then(|lib| {
                lib.fill_image_hashes(WORKERS, &cancel, &mut |done, total| {
                    if done < total
                        && last.is_some_and(|t| t.elapsed() < Duration::from_millis(100))
                    {
                        return;
                    }
                    last = Some(Instant::now());
                    let _ = app.emit(
                        DUPLICATES_PROGRESS,
                        Progress {
                            run,
                            directory: name.clone(),
                            done,
                            total,
                        },
                    );
                })
            });
            if let Ok(mut running) = PASS.lock()
                && running.as_ref().is_some_and(|pass| pass.run == run)
            {
                *running = None;
            }
            let finished = match result {
                Ok(pass) => {
                    if pass.hashed > 0 {
                        log::info!(
                            "Read the image data of {} photographs in {:.1} s",
                            pass.hashed,
                            started.elapsed().as_secs_f64()
                        );
                    }
                    Finished {
                        run,
                        directory: name.clone(),
                        photos: pass.photos,
                        hashed: pass.hashed,
                        unreadable: pass
                            .unreadable
                            .iter()
                            .map(|p| p.to_string_lossy().into_owned())
                            .collect(),
                        cancelled: pass.cancelled,
                        error: None,
                        seconds: started.elapsed().as_secs_f64(),
                    }
                }
                Err(e) => {
                    log::warn!("Image data not read: {e:#}");
                    Finished {
                        run,
                        directory: name.clone(),
                        photos: 0,
                        hashed: 0,
                        unreadable: Vec::new(),
                        cancelled: false,
                        error: Some(format!("{e:#}")),
                        seconds: started.elapsed().as_secs_f64(),
                    }
                }
            };
            if finished.hashed > 0 {
                let _ = app.emit(FACTS_RECORDED, &directory);
            }
            let _ = app.emit(DUPLICATES_FINISHED, finished);
        })
        .map_err(|e| e.to_string())?;
    Ok(run)
}

/// Starts the pass when a search or smart album that compares image data
/// (the "Is duplicate" rule) looks at a library with photographs not read
/// yet; its answer grows as the pass records them.
pub(crate) fn start_pass_if_needed(lib: &Library, rule: &rawtonic_catalog::Rule) {
    if !lib.rule_compares_images(rule).unwrap_or(false) {
        return;
    }
    if PASS
        .lock()
        .is_ok_and(|p| p.as_ref().is_some_and(|p| p.directory == lib.directory))
    {
        return;
    }
    if lib.image_hash_backlog().is_ok_and(|n| n > 0)
        && let Some(app) = announcer()
    {
        let _ = start(app.clone(), lib.directory.clone());
    }
}

/// Starts reading the image data of the library's photographs that have no
/// current hash; returns the pass's number (its events carry it).
#[tauri::command]
pub fn duplicates_start(app: tauri::AppHandle, directory: String) -> Result<u64, String> {
    let directory = Path::new(&directory)
        .canonicalize()
        .map_err(|e| e.to_string())?;
    start(app, directory)
}

/// Stops pass `run`; what it recorded is kept.
#[tauri::command]
pub fn duplicates_cancel(run: u64) {
    if let Ok(running) = PASS.lock()
        && let Some(pass) = running.as_ref().filter(|pass| pass.run == run)
    {
        pass.cancel.store(true, Ordering::Release);
    }
}

/// A photograph of a group as the review shows it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewMember {
    #[serde(flatten)]
    pub member: DuplicateMember,
    /// Its develop settings differ from a fresh photograph's.
    pub edited: bool,
}

/// The groups of photographs with identical image data, as recorded.
#[tauri::command]
pub async fn duplicates_groups(
    app: tauri::AppHandle,
    directory: String,
) -> Result<Vec<Vec<ReviewMember>>, String> {
    let settings = crate::app_settings::load_settings(app).unwrap_or_default();
    with_library(directory, false, move |lib| {
        let groups = lib.duplicate_groups().map_err(|e| e.to_string())?;
        Ok(groups
            .into_iter()
            .map(|group| {
                group
                    .into_iter()
                    .map(|member| {
                        let edited = edited(lib, &member, &settings);
                        ReviewMember { member, edited }
                    })
                    .collect()
            })
            .collect())
    })
    .await
    .map_err(|e| e.to_string())?
}

fn edited(
    lib: &Library,
    member: &DuplicateMember,
    settings: &crate::app_settings::AppSettings,
) -> bool {
    let Ok(Some(state)) = lib.editor_state(&member.id) else {
        return false;
    };
    let is_raw = crate::formats::is_raw_file(&member.path);
    let tonemapper = crate::image_processing::resolve_tonemapper_override(settings, is_raw);
    crate::image_processing::is_image_edited(&state["adjustments"], is_raw, tonemapper)
}

/// Checks the files again before the photographs in `trash` go to the
/// Trash: returns those that no longer hold the same image data as a
/// photograph of `keep` (empty: go ahead).
#[tauri::command]
pub async fn duplicates_confirm(
    directory: String,
    keep: Vec<String>,
    trash: Vec<String>,
) -> Result<Vec<String>, String> {
    with_library(directory, false, move |lib| {
        let paths = |paths: &[String]| -> Vec<PathBuf> {
            paths.iter().map(|p| parse_virtual_path(p).0).collect()
        };
        let changed = lib
            .confirm_duplicates(&paths(&keep), &paths(&trash))
            .map_err(|e| e.to_string())?;
        Ok(trash
            .into_iter()
            .filter(|p| changed.contains(&parse_virtual_path(p).0))
            .collect())
    })
    .await
    .map_err(|e| e.to_string())?
}
