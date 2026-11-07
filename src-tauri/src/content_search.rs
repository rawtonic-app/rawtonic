// SPDX-License-Identifier: AGPL-3.0-or-later
//! Content search (G37, notes: parity-gaps/G37-search.md "SRS decision
//! note", benchmark-results/srs-search-ceiling-2026-11-03): the Library's
//! search box in Content mode sorts the current view by how well each
//! photograph matches what the photographer typed ("a heron", "Hochzeit in
//! der Kirche"). A ranking, not a filter: no score separates hits from
//! misses.
//!
//! - **Index:** the background pass reads every photograph of the open
//!   library from its cached thumbnail with SigLIP 2 B/16's image encoder
//!   (the auto tags' model) and keeps the embedding in the catalog
//!   (`rawtonic_catalog::content`, keyed by the vision file's SHA-256, so a
//!   new model reads everything again). It runs on one thread at utility
//!   QoS with two ONNX Runtime threads that sleep between operators, and
//!   waits while an export or a batch write runs, while the auto tags read a
//!   folder (they hand their embeddings over: one vision pass for both,
//!   `share_embedding`) and while the photographer's photo decodes.
//! - **Query:** the text encoder (as ONNX external data, so ONNX Runtime
//!   maps its 1.1 GB instead of copying it) and the tokenizer load on the
//!   first query and are dropped after `TEXT_IDLE` without one, or at once
//!   on memory pressure.
//! - **Privacy:** nothing is downloaded until the photographer clicks the
//!   download (`content_search_download`, PRIVACY.md); the pass runs only
//!   with the setting on and the vision model installed.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use rawtonic_catalog::Library;
use rawtonic_catalog::content::ContentStatus;
use serde::Serialize;
use tauri::{Emitter, Manager};
use tokenizers::Tokenizer;

use crate::ai_processing::{
    SIGLIP_TOKENIZER_FILENAME, SIGLIP_TOKENIZER_SHA256, SIGLIP_VISION_FILENAME,
    SIGLIP_VISION_SHA256, TextEncoder,
};
use crate::ipc::{CommandError, ErrorCode};
use crate::session_bridge::with_library;
use crate::state::lock::lock_recover;

/// The search model's id in the catalog: the vision model file's SHA-256.
pub const MODEL_ID: &str = SIGLIP_VISION_SHA256;

/// SigLIP 2's text encoder saved as ONNX external data
/// (scripts/models/external_data.py; same weights as `siglip2_text.onnx`).
const TEXT_FILENAME: &str = "siglip2_text_ext.onnx";
const TEXT_SHA256: &str = "8850287083d7a450ec8112162ccc82c0c830432aee4f7814bcc090de65218dba";
const TEXT_DATA_FILENAME: &str = "siglip2_text_ext.onnx.data";
const TEXT_DATA_SHA256: &str = "d7721a55cb0242769a6d4b1031ef14c48df21609757a63b856cb00c21e502348";

/// The files search needs with their SHA-256 and size (for the size the
/// download button shows), vision first.
const FILES: [(&str, &str, u64); 4] = [
    (SIGLIP_VISION_FILENAME, SIGLIP_VISION_SHA256, 371_783_722),
    (TEXT_FILENAME, TEXT_SHA256, 226_315),
    (TEXT_DATA_FILENAME, TEXT_DATA_SHA256, 1_129_214_976),
    (
        SIGLIP_TOKENIZER_FILENAME,
        SIGLIP_TOKENIZER_SHA256,
        34_363_136,
    ),
];
/// What the downloads list calls it.
const DOWNLOAD_NAME: &str = "Search model";

/// `{directory, indexed, failed, total}` while the pass runs.
pub const PROGRESS: &str = "content-index-progress";
/// The pass stopped: `{directory, indexed, failed, total, cancelled}`.
pub const DONE: &str = "content-index-done";

/// ONNX Runtime threads of the pass (SRS2: 5.5 min per 10,000 photographs
/// with two on the M4 Pro, 23 % slower than all cores).
const PASS_THREADS: usize = 2;
/// How long the pass waits before reading (the library's first thumbnails
/// come first), between photographs, and between looks while it waits.
const PASS_DELAY: Duration = Duration::from_secs(3);
const PASS_PAUSE: Duration = Duration::from_millis(2);
const WAIT_STEP: Duration = Duration::from_millis(500);
/// Photographs taken from the catalog at a time.
const PASS_BATCH: usize = 32;
/// The text encoder and tokenizer are dropped after this long without a
/// query (SRS2: ~0.75 GB while loaded).
const TEXT_IDLE: Duration = Duration::from_secs(300);

static APP: OnceLock<tauri::AppHandle> = OnceLock::new();
/// The running pass: its library and its cancel flag.
static PASS: Mutex<Option<(PathBuf, Arc<AtomicBool>)>> = Mutex::new(None);
/// The library whose pass was stopped (closed, moved): the periodic look
/// leaves it alone until it is opened again.
static STOPPED: Mutex<Option<PathBuf>> = Mutex::new(None);

/// The pass reports to the app from now on.
pub fn announce_to(app: tauri::AppHandle) {
    let _ = APP.set(app);
}

fn emit<S: Serialize + Clone>(event: &str, payload: S) {
    if let Some(app) = APP.get() {
        let _ = app.emit(event, payload);
    }
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct Progress<'a> {
    directory: &'a Path,
    #[serde(flatten)]
    status: ContentStatus,
    /// Only in `DONE`.
    #[serde(skip_serializing_if = "Option::is_none")]
    cancelled: Option<bool>,
}

fn enabled(app: &tauri::AppHandle) -> bool {
    crate::state::settings::try_current(app)
        .map(|s| s.content_search.unwrap_or(false))
        .unwrap_or(false)
}

/// The missing files of the search model and their bytes.
fn missing_files(models: &Path) -> Vec<(&'static str, &'static str, u64)> {
    FILES
        .into_iter()
        .filter(|(file, sha, _)| {
            !crate::ai_processing::verify_sha256(&models.join(file), sha).unwrap_or(false)
        })
        .collect()
}

/// Lowers the calling thread below the photographer's work (macOS QoS;
/// ONNX Runtime's threads created from it inherit the class).
fn lower_priority() {
    #[cfg(target_os = "macos")]
    // SAFETY: sets the calling thread's own QoS class; no pointers involved.
    unsafe {
        libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_UTILITY, 0);
    }
}

/// Whether the pass should wait: an export or a batch write runs, the auto
/// tags read a folder (they fill the index themselves), or a photograph the
/// photographer opened is decoding or rendering.
fn should_wait(app: &tauri::AppHandle) -> bool {
    let exports = app.state::<crate::state::export::ExportState>();
    let exporting = !lock_recover(&exports.running_jobs).is_empty();
    let tagging = lock_recover(&app.state::<crate::state::ai::AiState>().indexing_task).is_some();
    exporting || tagging || crate::batch_journal::running() || crate::multi_view_prefetch::busy()
}

/// Reads every photograph of the library at `directory` that has no
/// current embedding, newest first, until done or `cancel` is set.
fn run_pass(
    app: &tauri::AppHandle,
    directory: &Path,
    cancel: &AtomicBool,
) -> anyhow::Result<ContentStatus> {
    let models = crate::ai_processing::models_dir(app)?;
    let vision = rawtonic_onnx::Model::load_cpu_threads(
        &models.join(SIGLIP_VISION_FILENAME),
        rawtonic_onnx::ModelKind::TagVision,
        PASS_THREADS,
    )?;
    let gpu_context = crate::gpu_processing::get_or_init_gpu_context(
        &app.state::<crate::state::gpu::GpuState>(),
        app,
    )
    .ok();
    let mut status = Library::open(directory)?.content_status(MODEL_ID)?;
    emit(
        PROGRESS,
        Progress {
            directory,
            status,
            cancelled: None,
        },
    );
    // Photographs whose file is not there now: left unread in this pass.
    let mut skipped = std::collections::HashSet::new();
    let started = Instant::now();
    let mut read = 0usize;
    'pass: while !cancel.load(Ordering::Acquire) {
        let lib = Library::open(directory)?;
        let work: Vec<_> = lib
            .content_backlog(MODEL_ID, PASS_BATCH + skipped.len())?
            .into_iter()
            .filter(|w| !skipped.contains(&w.id))
            .take(PASS_BATCH)
            .collect();
        if work.is_empty() {
            break;
        }
        for item in work {
            while should_wait(app) {
                if cancel.load(Ordering::Acquire) {
                    break 'pass;
                }
                std::thread::sleep(WAIT_STEP);
            }
            if cancel.load(Ordering::Acquire) {
                break 'pass;
            }
            // The auto tags may have read it meanwhile.
            if lib.has_content(&item.id, MODEL_ID)? {
                continue;
            }
            if !item.path.is_file() {
                skipped.insert(item.id);
                continue;
            }
            let path = item.path.to_string_lossy();
            let embedding = crate::file_management::get_cached_or_generate_thumbnail_image(
                &path,
                app,
                gpu_context.as_ref(),
            )
            .and_then(|image| crate::tagging::image_embedding(&image, &vision))
            .map_err(|e| format!("{e:#}"));
            match &embedding {
                Ok(values) => {
                    lib.record_content(&item.id, MODEL_ID, &item.fingerprint, Ok(values))?
                }
                Err(error) => {
                    log::warn!("Content search could not read {path}: {error}");
                    lib.record_content(&item.id, MODEL_ID, &item.fingerprint, Err(error))?;
                }
            }
            read += 1;
            status = lib.content_status(MODEL_ID)?;
            emit(
                PROGRESS,
                Progress {
                    directory,
                    status,
                    cancelled: None,
                },
            );
            std::thread::sleep(PASS_PAUSE);
        }
    }
    if read > 0 {
        log::info!(
            "Content search read {read} photographs in {:.1} s",
            started.elapsed().as_secs_f64()
        );
    }
    Ok(status)
}

/// Starts the pass for the library at `directory` in the background. A
/// pass for another library is stopped; one running for this library goes
/// on.
fn start_pass(app: tauri::AppHandle, directory: PathBuf) {
    let cancel = Arc::new(AtomicBool::new(false));
    {
        let mut running = lock_recover(&PASS);
        if let Some((library, previous)) = running.as_ref() {
            if *library == directory && !previous.load(Ordering::Acquire) {
                return;
            }
            previous.store(true, Ordering::Release);
        }
        *running = Some((directory.clone(), cancel.clone()));
    }
    let spawned = std::thread::Builder::new()
        .name("content-index".into())
        .spawn(move || {
            lower_priority();
            std::thread::sleep(PASS_DELAY);
            let result = run_pass(&app, &directory, &cancel);
            let cancelled = cancel.swap(true, Ordering::AcqRel);
            let status = match result {
                Ok(status) => status,
                Err(e) => {
                    log::warn!("Content search index paused: {e:#}");
                    Library::open(&directory)
                        .and_then(|lib| Ok(lib.content_status(MODEL_ID)?))
                        .unwrap_or_default()
                }
            };
            // Freed now: the vision session goes with `run_pass`.
            rawtonic_runtime::allocator::release_freed_memory();
            emit(
                DONE,
                Progress {
                    directory: &directory,
                    status,
                    cancelled: Some(cancelled),
                },
            );
        });
    if let Err(e) = spawned {
        log::warn!("Content search index not started: {e}");
    }
}

/// Whether a pass may start for `directory`; `resume` lifts a stop.
fn may_start(directory: &Path, resume: bool) -> bool {
    let mut stopped = lock_recover(&STOPPED);
    if resume {
        *stopped = None;
        return true;
    }
    stopped.as_deref() != Some(directory)
}

/// Stops a running pass of another library than `directory`.
fn stop_other_pass(directory: &Path) {
    if let Some((library, cancel)) = lock_recover(&PASS).as_ref()
        && library != directory
    {
        cancel.store(true, Ordering::Release);
    }
}

/// Starts indexing the library at `directory` when the setting is on, the
/// vision model is installed and there is anything to read; otherwise does
/// nothing (never downloads). `resume`: the library was opened or the
/// setting turned on; without it (the periodic look for new photographs) a
/// library whose pass was stopped stays stopped.
#[tauri::command]
pub async fn start_content_index(
    directory: String,
    resume: bool,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    if !enabled(&app_handle) {
        stop_content_index(None);
        return Ok(());
    }
    stop_other_pass(Path::new(&directory));
    if !may_start(Path::new(&directory), resume) {
        return Ok(());
    }
    let models = crate::ai_processing::models_dir(&app_handle).map_err(|e| e.to_string())?;
    let vision = models.join(SIGLIP_VISION_FILENAME);
    let installed = tauri::async_runtime::spawn_blocking(move || {
        crate::ai_processing::verify_sha256(&vision, SIGLIP_VISION_SHA256).unwrap_or(false)
    })
    .await
    .map_err(|e| e.to_string())?;
    if !installed || crate::ai_processing::ensure_runtime().is_err() {
        return Ok(());
    }
    let backlog = with_library(directory.clone(), false, |lib| {
        lib.content_backlog_count(MODEL_ID)
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())??;
    if backlog > 0 {
        start_pass(app_handle, PathBuf::from(directory));
    }
    Ok(())
}

/// Stops the running pass: the setting turned off (any library), a library
/// closing or moving (`directory`: only that library's).
#[tauri::command]
pub fn stop_content_index(directory: Option<String>) {
    if let Some((library, cancel)) = lock_recover(&PASS).as_ref()
        && directory.is_none_or(|d| Path::new(&d) == library)
    {
        cancel.store(true, Ordering::Release);
        *lock_recover(&STOPPED) = Some(library.clone());
    }
}

/// What the Content switch shows: whether the search model is installed
/// (else the bytes its download takes) and how far the library's index is.
#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ContentSearchStatus {
    pub installed: bool,
    pub download_bytes: u64,
    pub running: bool,
    #[serde(flatten)]
    pub index: ContentStatus,
}

#[tauri::command]
pub async fn content_search_status(
    directory: String,
    app_handle: tauri::AppHandle,
) -> Result<ContentSearchStatus, String> {
    let models = crate::ai_processing::models_dir(&app_handle).map_err(|e| e.to_string())?;
    let missing = tauri::async_runtime::spawn_blocking(move || missing_files(&models))
        .await
        .map_err(|e| e.to_string())?;
    let index = with_library(directory.clone(), false, |lib| {
        lib.content_status(MODEL_ID).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())??;
    let running = lock_recover(&PASS)
        .as_ref()
        .is_some_and(|(library, cancel)| {
            library == Path::new(&directory) && !cancel.load(Ordering::Acquire)
        });
    Ok(ContentSearchStatus {
        installed: missing.is_empty(),
        download_bytes: missing.iter().map(|(_, _, size)| size).sum(),
        running,
        index,
    })
}

/// The photographer clicked "Download search model": fetches the missing
/// files from the model pack (each checked against its SHA-256), then
/// starts indexing the library at `directory`. The frontend turns the
/// setting on first.
#[tauri::command]
pub async fn content_search_download(
    directory: String,
    app_handle: tauri::AppHandle,
) -> Result<(), CommandError> {
    crate::ai_processing::ensure_runtime()?;
    let models = crate::ai_processing::models_dir(&app_handle).map_err(|e| e.to_string())?;
    let dir = models.clone();
    let missing = tauri::async_runtime::spawn_blocking(move || missing_files(&dir))
        .await
        .map_err(|e| e.to_string())?;
    let mut batch = crate::ai_processing::DownloadBatch {
        done: 0,
        total: missing.iter().map(|(_, _, size)| size).sum(),
    };
    for (file, sha, _) in missing {
        crate::ai_processing::download_verified_in(
            &app_handle,
            &models,
            file,
            sha,
            DOWNLOAD_NAME,
            Some(&mut batch),
        )
        .await
        .map_err(|e| format!("{e:#}"))?;
    }
    start_content_index(directory, true, app_handle).await?;
    Ok(())
}

/// The loaded text encoder and when it was last used.
struct LoadedText {
    encoder: Arc<TextEncoder>,
    used: Instant,
}

static TEXT: Mutex<Option<LoadedText>> = Mutex::new(None);
static TEXT_INIT: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Drops the text encoder and tokenizer (idle, memory pressure); a query
/// running with them finishes first. True when they were loaded.
pub fn unload_text() -> bool {
    let taken = lock_recover(&TEXT).take();
    let dropped = taken.is_some();
    let before = crate::ai_lifecycle::footprint_mb();
    drop(taken);
    if dropped {
        rawtonic_runtime::allocator::release_freed_memory();
        log::info!(
            "Content search: text model unloaded (footprint {} -> {} MB)",
            mb(before),
            mb(crate::ai_lifecycle::footprint_mb())
        );
    }
    dropped
}

/// A footprint for the log ("?" where it is not known).
fn mb(value: Option<f64>) -> String {
    value.map_or_else(|| "?".into(), |v| format!("{v:.0}"))
}

/// Drops the text encoder once it was not used for `TEXT_IDLE`.
fn watch_idle_text() {
    static WATCHING: AtomicBool = AtomicBool::new(false);
    if WATCHING.swap(true, Ordering::AcqRel) {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("content-text-idle".into())
        .spawn(|| {
            loop {
                std::thread::sleep(Duration::from_secs(15));
                let idle = lock_recover(&TEXT)
                    .as_ref()
                    .map(|loaded| loaded.used.elapsed() >= TEXT_IDLE);
                match idle {
                    None => break,
                    Some(true) => {
                        unload_text();
                        break;
                    }
                    Some(false) => {}
                }
            }
            WATCHING.store(false, Ordering::Release);
        });
    if spawned.is_err() {
        WATCHING.store(false, Ordering::Release);
    }
}

/// The text encoder, loaded on first use (never downloaded here: without
/// its files `ModelsMissing`).
async fn text_encoder(app: &tauri::AppHandle) -> Result<Arc<TextEncoder>, CommandError> {
    let cached = || {
        lock_recover(&TEXT).as_mut().map(|loaded| {
            loaded.used = Instant::now();
            loaded.encoder.clone()
        })
    };
    if let Some(encoder) = cached() {
        return Ok(encoder);
    }
    let _guard = TEXT_INIT.lock().await;
    if let Some(encoder) = cached() {
        return Ok(encoder);
    }
    crate::ai_processing::ensure_runtime()?;
    let models = crate::ai_processing::models_dir(app).map_err(|e| e.to_string())?;
    let encoder = tauri::async_runtime::spawn_blocking(move || {
        if !missing_files(&models).is_empty() {
            return Err(CommandError::new(
                ErrorCode::ModelsMissing,
                "The search model is not installed",
            ));
        }
        let started = Instant::now();
        let before = crate::ai_lifecycle::footprint_mb();
        let session = rawtonic_onnx::Model::load(
            &models.join(TEXT_FILENAME),
            rawtonic_onnx::ModelKind::TagText,
        )
        .map_err(|e| format!("{e:#}"))?;
        let tokenizer = Tokenizer::from_file(models.join(SIGLIP_TOKENIZER_FILENAME))
            .map_err(|e| e.to_string())?;
        log::info!(
            "Content search: text model loaded in {} ms (footprint {} -> {} MB)",
            started.elapsed().as_millis(),
            mb(before),
            mb(crate::ai_lifecycle::footprint_mb())
        );
        Ok::<_, CommandError>(TextEncoder { session, tokenizer })
    })
    .await
    .map_err(|e| e.to_string())??;
    let encoder = Arc::new(encoder);
    *lock_recover(&TEXT) = Some(LoadedText {
        encoder: encoder.clone(),
        used: Instant::now(),
    });
    crate::register_exit_handler();
    watch_idle_text();
    Ok(encoder)
}

/// The query as the model reads it: trimmed and lower case (SigLIP's
/// training text was lower case, SRS).
fn query_text(query: &str) -> String {
    query.trim().to_lowercase()
}

/// The photographs of the library ranked for a query: every indexed one,
/// best match first (in the form of `listed`, the folder the grid lists),
/// and how far the index is.
#[derive(Serialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ContentRanking {
    pub paths: Vec<PathBuf>,
    #[serde(flatten)]
    pub index: ContentStatus,
}

/// Ranks the photographs of the library at `directory` for `query`.
#[tauri::command]
pub async fn content_search_rank(
    directory: String,
    query: String,
    listed: Option<String>,
    app_handle: tauri::AppHandle,
) -> Result<ContentRanking, CommandError> {
    let text = query_text(&query);
    if text.is_empty() {
        return Err(CommandError::from("Nothing to search for".to_string()));
    }
    let started = Instant::now();
    let encoder = text_encoder(&app_handle).await?;
    let loaded = started.elapsed();
    let embedding = tauri::async_runtime::spawn_blocking(move || {
        crate::tagging::embed_texts(&encoder, vec![text]).map_err(|e| format!("{e:#}"))
    })
    .await
    .map_err(|e| e.to_string())??;
    let encoded = started.elapsed();
    let ranking = with_library(directory, false, move |lib| {
        let ranked = lib
            .content_ranking(MODEL_ID, &embedding)
            .map_err(|e| e.to_string())?;
        let index = lib.content_status(MODEL_ID).map_err(|e| e.to_string())?;
        let paths = ranked.into_iter().map(|(path, _)| path).collect();
        Ok(ContentRanking {
            paths: match listed {
                Some(listed) => crate::library_search::as_listed(paths, Path::new(&listed)),
                None => paths,
            },
            index,
        })
    })
    .await
    .map_err(|e| e.to_string())??;
    log::info!(
        "[PERF] content_search_rank photos={} load_ms={} text_ms={} total_ms={}",
        ranking.paths.len(),
        loaded.as_millis(),
        (encoded - loaded).as_millis(),
        started.elapsed().as_millis()
    );
    Ok(ranking)
}

/// The auto tags read the photograph at `path` (`embedding` from the same
/// thumbnail and model): content search keeps it, when it is on and the
/// photograph is in the open library without a current embedding. Errors
/// only cost the pass a read later.
pub fn share_embedding(app: &tauri::AppHandle, path: &str, embedding: &[f32]) {
    if !enabled(app) {
        return;
    }
    let Some(directory) = crate::session_bridge::active_library_directory() else {
        return;
    };
    let file = rawtonic_metadata::photo_ref::source_of(path);
    let shared = Library::open(&directory).and_then(|lib| {
        let Some(asset) = lib.asset_at(Path::new(file))? else {
            return Ok(());
        };
        if !lib.has_content(&asset.id, MODEL_ID)? {
            lib.record_content(&asset.id, MODEL_ID, &asset.fingerprint, Ok(embedding))?;
        }
        Ok(())
    });
    if let Err(e) = shared {
        log::debug!("Content search: embedding of the auto tags not kept: {e:#}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queries_are_read_in_lower_case() {
        assert_eq!(query_text("  A Heron "), "a heron");
        assert_eq!(
            query_text("Hochzeit in der KIRCHE"),
            "hochzeit in der kirche"
        );
        assert_eq!(query_text("   "), "");
    }

    #[test]
    fn the_download_lists_every_file_of_the_model() {
        let dir = tempfile::tempdir().unwrap();
        let missing = missing_files(dir.path());
        assert_eq!(missing.len(), 4);
        let bytes: u64 = missing.iter().map(|(_, _, size)| size).sum();
        // 1.5 GB with the vision model, 1.2 GB when auto tags installed it.
        assert_eq!(bytes, 1_535_588_149);
        assert_eq!(bytes - FILES[0].2, 1_163_804_427);
    }

    #[test]
    fn a_stop_names_the_library_it_stops() {
        let cancel = Arc::new(AtomicBool::new(false));
        *lock_recover(&PASS) = Some((PathBuf::from("/A"), cancel.clone()));
        stop_content_index(Some("/B".into()));
        stop_other_pass(Path::new("/A"));
        assert!(!cancel.load(Ordering::Acquire));
        stop_content_index(Some("/A".into()));
        assert!(cancel.swap(false, Ordering::AcqRel));
        stop_other_pass(Path::new("/B"));
        assert!(cancel.load(Ordering::Acquire));
        *lock_recover(&PASS) = None;
        assert!(!may_start(Path::new("/A"), false));
        assert!(may_start(Path::new("/A"), true));
        assert!(may_start(Path::new("/A"), false));
    }
}

/// Measurement tool (notes: benchmark-results/srb-content-search-2026-11-07):
/// the pass's and the query's own functions on real files, outside the app.
///
/// - `CONTENT_MODELS`: a models folder with the search model's four files;
///   `ORT_DYLIB_PATH`: ONNX Runtime.
/// - `CONTENT_THUMBS`: a folder of cached thumbnails (the app's
///   `*_medium.jpg`): each is decoded and read as the pass does (two ONNX
///   Runtime threads on a utility-QoS thread) into a scratch library; times
///   per step.
/// - Then a scratch library of 10,000 photographs with random embeddings:
///   text model load, footprint loaded and after dropping it, and the
///   query (text encoder + ranking) for 20 queries.
/// - `CONTENT_LIBRARY` + `CONTENT_QUERIES` (optional): a library the app
///   indexed and SRS's queries.tsv: each query ranked there, one line per
///   query (id, then file names best first) in `ranked.tsv`.
/// - `CONTENT_OUT`: a new folder for `timing.txt` (and `ranked.tsv`).
///
/// cargo test -p rawtonic-app --release --lib measures_content_search -- --ignored --nocapture
#[cfg(test)]
mod measurement {
    use super::*;
    use rawtonic_catalog::Mode;
    use rawtonic_catalog::content::DIM;
    use std::fmt::Write as _;

    fn footprint_mb() -> f64 {
        crate::ai_lifecycle::footprint_mb().unwrap_or(f64::NAN)
    }

    fn median(mut v: Vec<f64>) -> f64 {
        v.sort_by(f64::total_cmp);
        v[v.len() / 2]
    }

    #[test]
    #[ignore = "tool: needs CONTENT_MODELS, CONTENT_THUMBS, CONTENT_OUT and ORT_DYLIB_PATH"]
    fn measures_content_search() {
        let env = |name| std::env::var_os(name).map(PathBuf::from);
        let (Some(models), Some(thumbs), Some(out)) = (
            env("CONTENT_MODELS"),
            env("CONTENT_THUMBS"),
            env("CONTENT_OUT"),
        ) else {
            eprintln!("SKIP: CONTENT_MODELS, CONTENT_THUMBS and CONTENT_OUT are needed");
            return;
        };
        assert!(missing_files(&models).is_empty(), "search model incomplete");
        std::fs::create_dir(&out).expect("CONTENT_OUT must be a new folder");
        let mut report = String::new();
        let scratch = tempfile::tempdir().unwrap();

        // 1. The pass on cached thumbnails.
        let mut files: Vec<PathBuf> = std::fs::read_dir(&thumbs)
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("jpg")))
            .collect();
        files.sort();
        let lib = Library::create(&scratch.path().join("Pass"), "Pass", Mode::Session).unwrap();
        let capture = lib.directory.join("Capture");
        let ids: Vec<(String, String)> = files
            .iter()
            .enumerate()
            .map(|(i, _)| {
                let file = capture.join(format!("{i:06}.jpg"));
                std::fs::write(&file, i.to_string()).unwrap();
                let asset = lib.add_asset(&file).unwrap();
                (asset.id, asset.fingerprint)
            })
            .collect();
        drop(lib);
        let directory = scratch.path().join("Pass");
        let vision_path = models.join(SIGLIP_VISION_FILENAME);
        let (decode, model, record, wall) = std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    lower_priority();
                    let vision = rawtonic_onnx::Model::load_cpu_threads(
                        &vision_path,
                        rawtonic_onnx::ModelKind::TagVision,
                        PASS_THREADS,
                    )
                    .unwrap();
                    let lib = Library::open(&directory).unwrap();
                    let (mut decode, mut model, mut record) = (0.0, 0.0, 0.0);
                    let started = Instant::now();
                    for (file, (id, fingerprint)) in files.iter().zip(&ids) {
                        let t = Instant::now();
                        let image = image::open(file).unwrap();
                        decode += t.elapsed().as_secs_f64();
                        let t = Instant::now();
                        let embedding = crate::tagging::image_embedding(&image, &vision).unwrap();
                        model += t.elapsed().as_secs_f64();
                        let t = Instant::now();
                        lib.record_content(id, MODEL_ID, fingerprint, Ok(&embedding))
                            .unwrap();
                        record += t.elapsed().as_secs_f64();
                    }
                    (decode, model, record, started.elapsed().as_secs_f64())
                })
                .join()
                .unwrap()
        });
        let n = files.len() as f64;
        writeln!(
            report,
            "pass: {} thumbnails, {PASS_THREADS} ORT threads, utility QoS\n  per photo: decode {:.1} ms, model {:.1} ms, record {:.2} ms, wall {:.1} ms\n  per 10,000: {:.1} min",
            files.len(),
            decode / n * 1e3,
            model / n * 1e3,
            record / n * 1e3,
            wall / n * 1e3,
            wall / n * 10_000.0 / 60.0
        )
        .unwrap();
        println!("{report}");

        // 2. The query at 10,000 photographs.
        let big = Library::create(&scratch.path().join("Big"), "Big", Mode::Session).unwrap();
        let capture = big.directory.join("Capture");
        let paths: Vec<PathBuf> = (0..10_000)
            .map(|i| {
                let file = capture.join(format!("{i:06}.jpg"));
                std::fs::write(&file, i.to_string()).unwrap();
                file
            })
            .collect();
        big.add_files(&paths).unwrap();
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut random = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed >> 11) as f32 / (1u64 << 53) as f32 - 0.5
        };
        for work in big.content_backlog(MODEL_ID, 20_000).unwrap() {
            let v: Vec<f32> = (0..DIM).map(|_| random()).collect();
            let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
            let v: Vec<f32> = v.iter().map(|x| x / norm).collect();
            big.record_content(&work.id, MODEL_ID, &work.fingerprint, Ok(&v))
                .unwrap();
        }
        assert_eq!(big.content_status(MODEL_ID).unwrap().indexed, 10_000);
        drop(big);
        rawtonic_runtime::allocator::release_freed_memory();
        let before = footprint_mb();
        let t = Instant::now();
        let encoder = TextEncoder {
            session: rawtonic_onnx::Model::load(
                &models.join(TEXT_FILENAME),
                rawtonic_onnx::ModelKind::TagText,
            )
            .unwrap(),
            tokenizer: Tokenizer::from_file(models.join(SIGLIP_TOKENIZER_FILENAME)).unwrap(),
        };
        let load_ms = t.elapsed().as_secs_f64() * 1e3;
        let loaded = footprint_mb();
        let queries = [
            "a dog",
            "a white car",
            "a bride",
            "a church",
            "a speaker on stage",
            "a kitchen",
            "a drone view",
            "a pottery wheel",
            "a heron",
            "Hochzeit",
            "a group photo",
            "a man in a suit",
            "a staircase",
            "a cake",
            "flowers",
            "a swimming pool",
            "red car at night",
            "a child",
            "Luftaufnahme",
            "a sunset",
        ];
        let (mut text_ms, mut rank_ms, mut total_ms) = (Vec::new(), Vec::new(), Vec::new());
        let big = Library::open(&scratch.path().join("Big")).unwrap();
        let mut first_ms = 0.0;
        for (i, query) in queries.iter().enumerate() {
            let t = Instant::now();
            let embedding = crate::tagging::embed_texts(&encoder, vec![query_text(query)]).unwrap();
            let text = t.elapsed().as_secs_f64() * 1e3;
            let t2 = Instant::now();
            let ranked = big.content_ranking(MODEL_ID, &embedding).unwrap();
            assert_eq!(ranked.len(), 10_000);
            let rank = t2.elapsed().as_secs_f64() * 1e3;
            if i == 0 {
                first_ms = text + rank;
            } else {
                text_ms.push(text);
                rank_ms.push(rank);
                total_ms.push(text + rank);
            }
        }
        let peak = footprint_mb();
        drop(encoder);
        rawtonic_runtime::allocator::release_freed_memory();
        let after = footprint_mb();
        let max = total_ms.iter().copied().fold(0.0, f64::max);
        writeln!(
            report,
            "query at 10,000 photographs ({} queries after the first):\n  text model + tokenizer load {load_ms:.0} ms; first query {first_ms:.1} ms\n  median: text {:.1} ms + ranking {:.1} ms = {:.1} ms; slowest {max:.1} ms\n  footprint: {before:.0} MB before, {loaded:.0} MB loaded (+{:.0}), {peak:.0} MB after the queries, {after:.0} MB after dropping it (process of the test, not the app)",
            total_ms.len(),
            median(text_ms),
            median(rank_ms),
            median(total_ms),
            loaded - before
        )
        .unwrap();

        // 3. Rankings in a library the app indexed.
        if let (Some(library), Some(queries)) = (env("CONTENT_LIBRARY"), env("CONTENT_QUERIES")) {
            let encoder = TextEncoder {
                session: rawtonic_onnx::Model::load(
                    &models.join(TEXT_FILENAME),
                    rawtonic_onnx::ModelKind::TagText,
                )
                .unwrap(),
                tokenizer: Tokenizer::from_file(models.join(SIGLIP_TOKENIZER_FILENAME)).unwrap(),
            };
            let lib = Library::open(&library).unwrap();
            let status = lib.content_status(MODEL_ID).unwrap();
            let mut ranked_out = String::new();
            for line in std::fs::read_to_string(queries).unwrap().lines() {
                let fields: Vec<&str> = line.split('\t').collect();
                if !line.starts_with('q') || fields.len() < 4 {
                    continue;
                }
                let embedding =
                    crate::tagging::embed_texts(&encoder, vec![query_text(fields[3])]).unwrap();
                let names: Vec<String> = lib
                    .content_ranking(MODEL_ID, &embedding)
                    .unwrap()
                    .iter()
                    .map(|(p, _)| p.file_name().unwrap().to_string_lossy().into_owned())
                    .collect();
                writeln!(ranked_out, "{}\t{}", fields[0], names.join("\t")).unwrap();
            }
            std::fs::write(out.join("ranked.tsv"), ranked_out).unwrap();
            writeln!(
                report,
                "rankings: library indexed {} of {} ({} failed)",
                status.indexed, status.total, status.failed
            )
            .unwrap();
        }
        println!("{report}");
        std::fs::write(out.join("timing.txt"), report).unwrap();
    }
}

/// The frontend's payloads (src/contracts/contentSearch.json, pinned on its
/// side by src/contracts/contentSearch.test.ts), deserialized with the
/// commands' argument types; the answers and events serialize to the shapes
/// listed there. The `command` checks bind each envelope to its command's
/// signature at compile time.
#[cfg(test)]
mod contract_tests {
    use super::*;
    use serde::Deserialize;
    use serde_json::{Value, json};
    use std::future::Future;

    fn payloads() -> Value {
        serde_json::from_str(include_str!("../../src/contracts/contentSearch.json")).unwrap()
    }

    fn args<T: for<'de> Deserialize<'de>>(name: &str) -> T {
        serde_json::from_value(payloads()[name].clone()).unwrap_or_else(|e| panic!("{name}: {e}"))
    }

    // Tauri reads each top-level camelCase argument; these envelopes also
    // refuse a misspelled or unexpected key.
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct StartArgs {
        directory: String,
        resume: bool,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct StopArgs {
        directory: Option<String>,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct DirectoryArgs {
        directory: String,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct RankArgs {
        directory: String,
        query: String,
        listed: Option<String>,
    }

    #[test]
    fn the_frontends_payloads_fit_the_commands() {
        fn start<F: Future<Output = Result<(), String>>>(
            _: impl FnOnce(String, bool, tauri::AppHandle) -> F,
        ) {
        }
        fn stop(_: impl FnOnce(Option<String>)) {}
        fn status<F: Future<Output = Result<ContentSearchStatus, String>>>(
            _: impl FnOnce(String, tauri::AppHandle) -> F,
        ) {
        }
        fn download<F: Future<Output = Result<(), CommandError>>>(
            _: impl FnOnce(String, tauri::AppHandle) -> F,
        ) {
        }
        fn rank<F: Future<Output = Result<ContentRanking, CommandError>>>(
            _: impl FnOnce(String, String, Option<String>, tauri::AppHandle) -> F,
        ) {
        }
        start(start_content_index);
        stop(stop_content_index);
        status(content_search_status);
        download(content_search_download);
        rank(content_search_rank);

        let a: StartArgs = args("start_content_index");
        assert_eq!((a.directory.as_str(), a.resume), ("/Shoots/Job", true));
        let a: StartArgs = args("start_content_index_periodic");
        assert!(!a.resume);
        let a: StopArgs = args("stop_content_index");
        assert_eq!(a.directory.as_deref(), Some("/Shoots/Job"));
        let a: StopArgs = args("stop_content_index_any");
        assert_eq!(a.directory, None);
        for name in ["content_search_status", "content_search_download"] {
            let a: DirectoryArgs = args(name);
            assert_eq!(a.directory, "/Shoots/Job");
        }
        let a: RankArgs = args("content_search_rank");
        assert_eq!(
            (a.directory.as_str(), a.query.as_str(), a.listed.as_deref()),
            ("/Shoots/Job", "a heron", Some("/Shoots/Job/Capture"))
        );
        let a: RankArgs = args("content_search_rank_album");
        assert_eq!(a.listed, None);
    }

    #[test]
    fn answers_and_events_have_the_frontends_shape() {
        let index = ContentStatus {
            indexed: 4213,
            failed: 2,
            total: 10000,
        };
        let status = ContentSearchStatus {
            installed: false,
            download_bytes: FILES.iter().map(|f| f.2).sum(),
            running: true,
            index,
        };
        assert_eq!(
            serde_json::to_value(status).unwrap(),
            payloads()["status_result"]
        );
        let ranking = ContentRanking {
            paths: vec![
                "/Shoots/Job/Capture/B.ARW".into(),
                "/Shoots/Job/Capture/A.ARW".into(),
            ],
            index: ContentStatus {
                indexed: 2,
                failed: 0,
                total: 3,
            },
        };
        assert_eq!(
            serde_json::to_value(ranking).unwrap(),
            payloads()["rank_result"]
        );
        let directory = Path::new("/Shoots/Job");
        let progress = Progress {
            directory,
            status: ContentStatus {
                indexed: 1,
                failed: 0,
                total: 3,
            },
            cancelled: None,
        };
        assert_eq!(
            serde_json::to_value(progress).unwrap(),
            payloads()["progress_event"]
        );
        let done = Progress {
            directory,
            status: ContentStatus {
                indexed: 3,
                failed: 0,
                total: 3,
            },
            cancelled: Some(false),
        };
        assert_eq!(
            serde_json::to_value(done).unwrap(),
            payloads()["done_event"]
        );
        assert_eq!(json!(PROGRESS), json!("content-index-progress"));
        assert_eq!(json!(DONE), json!("content-index-done"));
    }
}
