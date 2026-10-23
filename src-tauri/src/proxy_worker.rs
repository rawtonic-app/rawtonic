//! Prepares preview proxies (preview_proxy.rs) in the background. Two kinds
//! of work:
//!
//! - Prefetch: the photographs the user is likely to open next, such as the
//!   filmstrip neighbours of the open one, at utility priority on a small
//!   thread pool, so the interactive work keeps the other cores. The newest
//!   request replaces the queue, and it goes before builds.
//! - Builds: Standard or 1:1 previews the user asked for (Lightroom's Build
//!   Previews, Capture One's preview files for a whole session), at full
//!   speed, with progress, a Stop and the reasons for what could not be
//!   built.
//!
//! Proxies already current at the wanted size are skipped.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};

use serde::Serialize;
use tauri::{Emitter, Manager};

use crate::app_settings::load_settings;
use crate::app_state::AppState;
use crate::file_management::parse_virtual_path;
use crate::formats::is_raw_file;
use crate::preview_proxy::{self, ProxyKind};

/// Threads decoding neighbours in the background (of the rayon work inside
/// a decode).
const WORKER_THREADS: usize = 4;
/// Event with the build's `BuildProgress`.
pub const PROGRESS_EVENT: &str = "preview-build-progress";
/// Failures kept for the progress report (the count is always complete).
const FAILURES_KEPT: usize = 50;

/// A photograph whose previews were asked for.
#[derive(Clone, Debug, PartialEq)]
struct BuildItem {
    path: String,
    /// Also its 1:1 preview.
    full: bool,
    /// The build request it belongs to; a Stop starts a new one.
    generation: usize,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BuildFailure {
    pub path: String,
    pub reason: String,
}

/// Where a build stands, for the top bar.
#[derive(Serialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BuildProgress {
    /// Photographs finished (built, already current, skipped or failed).
    pub done: usize,
    pub total: usize,
    pub running: bool,
    /// The build was stopped before its end.
    pub stopped: bool,
    /// Photographs that are not RAW files: they need no preview proxy.
    pub skipped: usize,
    pub failed_count: usize,
    pub failed: Vec<BuildFailure>,
}

#[derive(Debug, PartialEq)]
enum Outcome {
    Built,
    Current,
    Skipped,
    Failed(String),
    Cancelled,
}

#[derive(Debug, PartialEq)]
enum Job {
    Prefetch(String),
    Build(BuildItem),
}

fn source_of(path: &str) -> String {
    path.split("?vc=").next().unwrap_or(path).to_string()
}

#[derive(Default)]
struct Queue {
    prefetch: VecDeque<String>,
    builds: VecDeque<BuildItem>,
    /// The build being worked on.
    building: Option<BuildItem>,
    generation: usize,
    progress: BuildProgress,
}

impl Queue {
    /// Asks for the previews of `paths` (1:1 too with `full`). A photograph
    /// already waiting (or a virtual copy of it) is not added again; asked
    /// for 1:1 now, its entry includes it. A new build starts counting afresh.
    fn add_builds(&mut self, paths: Vec<String>, full: bool) {
        if !self.progress.running {
            self.progress = BuildProgress::default();
        }
        for path in paths {
            let source = source_of(&path);
            if let Some(waiting) = self
                .builds
                .iter_mut()
                .find(|item| source_of(&item.path) == source)
            {
                waiting.full |= full;
                continue;
            }
            if self
                .building
                .as_ref()
                .is_some_and(|b| source_of(&b.path) == source && (b.full || !full))
            {
                continue;
            }
            self.builds.push_back(BuildItem {
                path,
                full,
                generation: self.generation,
            });
            self.progress.total += 1;
        }
        self.progress.running = self.building.is_some() || !self.builds.is_empty();
    }

    /// The next job: prefetching first (the user waits for it), then builds.
    fn next(&mut self) -> Option<Job> {
        if let Some(path) = self.prefetch.pop_front() {
            return Some(Job::Prefetch(path));
        }
        let item = self.builds.pop_front()?;
        self.building = Some(item.clone());
        Some(Job::Build(item))
    }

    fn finish(&mut self, item: &BuildItem, outcome: Outcome) {
        if self.building.as_ref() == Some(item) {
            self.building = None;
        }
        // A stopped build's last photograph no longer counts.
        if item.generation != self.generation {
            return;
        }
        self.progress.done += 1;
        match outcome {
            Outcome::Skipped => self.progress.skipped += 1,
            Outcome::Failed(reason) => {
                self.progress.failed_count += 1;
                if self.progress.failed.len() < FAILURES_KEPT {
                    self.progress.failed.push(BuildFailure {
                        path: item.path.clone(),
                        reason,
                    });
                }
            }
            Outcome::Built | Outcome::Current | Outcome::Cancelled => {}
        }
        self.progress.running = !self.builds.is_empty();
    }

    /// Stops the build: what waits is dropped, the photograph being built is
    /// abandoned (its decode is cancelled; nothing is written for it).
    fn stop(&mut self) {
        if !self.progress.running {
            return;
        }
        self.builds.clear();
        self.generation += 1;
        self.progress.running = false;
        self.progress.stopped = true;
        self.progress.total = self.progress.done;
    }
}

struct Worker {
    queue: Mutex<Queue>,
    wake: Condvar,
    /// Raised by a Stop; a decode started for an older value gives up.
    cancel: Arc<AtomicUsize>,
    /// Long edge of the Develop view the frontend expects (Standard size).
    viewer_dim: AtomicU32,
}

static WORKER: OnceLock<Arc<Worker>> = OnceLock::new();

#[cfg(target_os = "macos")]
fn lower_priority() {
    // SAFETY: sets the calling thread's own QoS class; no pointers involved.
    unsafe {
        libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_UTILITY, 0);
    }
}

#[cfg(not(target_os = "macos"))]
fn lower_priority() {}

fn worker(app_handle: &tauri::AppHandle) -> Arc<Worker> {
    WORKER
        .get_or_init(|| {
            let worker = Arc::new(Worker {
                queue: Mutex::new(Queue::default()),
                wake: Condvar::new(),
                cancel: Arc::new(AtomicUsize::new(0)),
                viewer_dim: AtomicU32::new(0),
            });
            let shared = Arc::clone(&worker);
            let app_handle = app_handle.clone();
            std::thread::Builder::new()
                .name("preview-proxies".into())
                .spawn(move || run(&shared, &app_handle))
                .expect("spawn the preview proxy worker");
            worker
        })
        .clone()
}

fn emit_progress(app_handle: &tauri::AppHandle, progress: &BuildProgress) {
    let _ = app_handle.emit(PROGRESS_EVENT, progress);
}

fn run(worker: &Worker, app_handle: &tauri::AppHandle) {
    lower_priority();
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(WORKER_THREADS)
        .thread_name(|i| format!("preview-proxy-{i}"))
        .start_handler(|_| lower_priority())
        .build()
        .expect("preview proxy thread pool");
    // Builds the user asked for run like an export: every core at normal
    // priority (Lightroom builds previews at full speed too). On four
    // utility-class threads a 40 MP X-Trans decode took 2-3 s of a build.
    let build_pool = rayon::ThreadPoolBuilder::new()
        .num_threads(std::thread::available_parallelism().map_or(WORKER_THREADS, |n| n.get()))
        .thread_name(|i| format!("preview-build-{i}"))
        .build()
        .expect("preview build thread pool");
    loop {
        let job = {
            let mut queue = worker.queue.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                if let Some(job) = queue.next() {
                    break job;
                }
                queue = worker.wake.wait(queue).unwrap_or_else(|e| e.into_inner());
            }
        };
        match job {
            Job::Prefetch(path) => pool.install(|| prepare(app_handle, &path)),
            Job::Build(item) => {
                let outcome = build_pool.install(|| build(app_handle, worker, &item));
                let (progress, finished) = {
                    let mut queue = worker.queue.lock().unwrap_or_else(|e| e.into_inner());
                    queue.finish(&item, outcome);
                    (
                        queue.progress.clone(),
                        !queue.progress.running && item.generation == queue.generation,
                    )
                };
                emit_progress(app_handle, &progress);
                if finished {
                    apply_limits_near(app_handle, &item.path);
                }
            }
        }
    }
}

/// The Standard proxy size wanted now.
fn wanted_size(app_handle: &tauri::AppHandle, viewer_dim: u32) -> u32 {
    let state = app_handle.state::<AppState>();
    let settings = load_settings(app_handle.clone()).unwrap_or_default();
    let dim = state.last_preview_dim.load(Ordering::Relaxed);
    preview_proxy::standard_size(if dim > 0 { dim } else { viewer_dim }, &settings)
}

fn prepare(app_handle: &tauri::AppHandle, path: &str) {
    let state = app_handle.state::<AppState>();
    // The open photograph's own decode writes its proxy.
    if *state
        .current_source
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        == path
    {
        return;
    }
    let (source, _): (PathBuf, _) = parse_virtual_path(path);
    if !is_raw_file(&source) || preview_proxy::proxy_path(&source).is_none() {
        return;
    }
    let settings = load_settings(app_handle.clone()).unwrap_or_default();
    let size = wanted_size(app_handle, 0);
    if preview_proxy::stored_size(&source, &settings).is_some_and(|s| s >= size)
        || !preview_proxy::source_available(&source)
    {
        // Stored already (or all there is): keep it in memory for an
        // instant activation.
        // Only for a photograph of the previews' decode variant (a virtual
        // copy's may differ from its master's).
        let wanted = crate::image_loader::stored_decode_variant(path);
        if wanted != crate::image_loader::stored_decode_variant(&source.to_string_lossy()) {
            return;
        }
        let in_memory = state
            .proxy_memory
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(path, wanted)
            .is_some();
        if !in_memory && let Some(proxy) = preview_proxy::read(&source, &settings) {
            state
                .proxy_memory
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remember(path, proxy);
        }
        return;
    }
    #[cfg(feature = "performance-profiling")]
    let _stage = crate::performance::PerformanceStage::new("preview_proxy_prefetch");
    let Some((image, variant, measures)) = decode(app_handle, &source, &settings, None) else {
        return;
    };
    match preview_proxy::write(&source, &image, size, &settings, variant, measures) {
        // The thumbnail is now developed from the proxy.
        Ok(proxy) => {
            drop(image);
            state
                .proxy_memory
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remember(path, proxy);
            crate::file_management::refresh_thumbnail(app_handle, path);
            apply_limits_near(app_handle, path);
        }
        Err(e) => log::warn!("Preview proxy for {} not written: {}", source.display(), e),
    }
}

/// The full decode of `source` for the decode variant of its stored recipe
/// (returned with it and the decode's measures): the open photograph's when
/// it is in memory, else decoded now (cancelled when `cancel` is raised past
/// its value).
fn decode(
    app_handle: &tauri::AppHandle,
    source: &Path,
    settings: &crate::app_settings::AppSettings,
    cancel: Option<(Arc<AtomicUsize>, usize)>,
) -> Option<(
    Arc<image::DynamicImage>,
    crate::image_loader::DecodeVariant,
    Option<crate::image_loader::DecodeMeasures>,
)> {
    let state = app_handle.state::<AppState>();
    let source_str = source.to_string_lossy();
    let variant = crate::image_loader::stored_decode_variant(&source_str);
    let cached = state
        .decoded_image_cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_measured(&crate::image_loader::decoded_cache_key(
            &source_str,
            None,
            variant,
        ));
    if let Some((image, _, measures)) = cached {
        return Some((image, variant, measures));
    }
    let bytes = crate::file_management::read_file_mapped(source).ok()?;
    match crate::image_loader::load_base_image_measured_for(
        &bytes,
        &source_str,
        false,
        settings,
        cancel,
        None,
        None,
        Some(variant),
    ) {
        Ok((image, measures)) => Some((Arc::new(image), variant, measures)),
        Err(e) => {
            log::warn!(
                "Preview proxy for {}: decode failed: {}",
                source.display(),
                e
            );
            None
        }
    }
}

/// Builds the asked-for previews of one photograph.
fn build(app_handle: &tauri::AppHandle, worker: &Worker, item: &BuildItem) -> Outcome {
    if worker.cancel.load(Ordering::SeqCst) != item.generation {
        return Outcome::Cancelled;
    }
    #[cfg(feature = "performance-profiling")]
    let _stage = crate::performance::PerformanceStage::new("preview_build");
    let (source, _): (PathBuf, _) = parse_virtual_path(&item.path);
    if !is_raw_file(&source) {
        return Outcome::Skipped;
    }
    if preview_proxy::proxy_path(&source).is_none() {
        return Outcome::Failed("No folder for its previews".into());
    }
    if !preview_proxy::source_available(&source) {
        return Outcome::Failed(if source.exists() {
            "The RAW file cannot be read (offline)".into()
        } else {
            "The RAW file is missing (offline)".into()
        });
    }
    let settings = load_settings(app_handle.clone()).unwrap_or_default();
    let size = wanted_size(app_handle, worker.viewer_dim.load(Ordering::Relaxed));
    let needs_standard =
        preview_proxy::stored_size(&source, &settings).is_none_or(|stored| stored < size);
    let needs_full = item.full && !preview_proxy::has_full(&source, &settings);
    if !needs_standard && !needs_full {
        return Outcome::Current;
    }
    let cancel = Some((Arc::clone(&worker.cancel), item.generation));
    let Some((image, variant, measures)) = decode(app_handle, &source, &settings, cancel) else {
        return if worker.cancel.load(Ordering::SeqCst) != item.generation {
            Outcome::Cancelled
        } else {
            Outcome::Failed("The RAW file could not be decoded".into())
        };
    };
    if worker.cancel.load(Ordering::SeqCst) != item.generation {
        return Outcome::Cancelled;
    }
    if needs_standard {
        if let Err(e) = preview_proxy::write(&source, &image, size, &settings, variant, measures) {
            return Outcome::Failed(format!("Standard preview not written: {e}"));
        }
        // The thumbnail is now developed from the proxy.
        crate::file_management::refresh_thumbnail(app_handle, &item.path);
    }
    if needs_full
        && let Err(e) = preview_proxy::write_full(&source, &image, &settings, variant, measures)
    {
        return Outcome::Failed(format!("1:1 preview not written: {e}"));
    }
    Outcome::Built
}

/// Prepares preview proxies for `paths` (in order) in the background,
/// replacing any earlier request that has not started yet.
#[tauri::command]
pub fn prepare_preview_proxies(paths: Vec<String>, app_handle: tauri::AppHandle) {
    // Profiling builds can switch it off to measure first openings.
    #[cfg(feature = "performance-profiling")]
    if std::env::var_os("RAWTONIC_NO_PROXY_PREFETCH").is_some() {
        return;
    }
    let worker = worker(&app_handle);
    let mut queue = worker.queue.lock().unwrap_or_else(|e| e.into_inner());
    queue.prefetch = paths.into_iter().collect();
    worker.wake.notify_one();
}

/// Builds the Standard previews of `paths` (with `full`, their 1:1 previews
/// too) in the background. `viewer_dim`: the Develop view's long edge in
/// pixels as the frontend expects it, for the Standard size before a
/// photograph was shown this run.
#[tauri::command]
pub fn build_previews(
    paths: Vec<String>,
    full: bool,
    viewer_dim: Option<u32>,
    app_handle: tauri::AppHandle,
) -> BuildProgress {
    let worker = worker(&app_handle);
    if let Some(dim) = viewer_dim {
        worker.viewer_dim.store(dim, Ordering::Relaxed);
    }
    let progress = {
        let mut queue = worker.queue.lock().unwrap_or_else(|e| e.into_inner());
        if !queue.progress.running {
            // A new build: decodes it starts are cancelled by a later Stop.
            worker.cancel.store(queue.generation, Ordering::SeqCst);
        }
        queue.add_builds(paths, full);
        queue.progress.clone()
    };
    worker.wake.notify_one();
    emit_progress(&app_handle, &progress);
    progress
}

/// Stops building previews.
#[tauri::command]
pub fn stop_preview_build(app_handle: tauri::AppHandle) -> BuildProgress {
    let worker = worker(&app_handle);
    let progress = {
        let mut queue = worker.queue.lock().unwrap_or_else(|e| e.into_inner());
        queue.stop();
        worker.cancel.store(queue.generation, Ordering::SeqCst);
        queue.progress.clone()
    };
    emit_progress(&app_handle, &progress);
    progress
}

/// Where the current build stands.
#[tauri::command]
pub fn preview_build_progress(app_handle: tauri::AppHandle) -> BuildProgress {
    worker(&app_handle)
        .queue
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .progress
        .clone()
}

fn kinds(full_only: bool) -> &'static [ProxyKind] {
    if full_only {
        &[ProxyKind::Full]
    } else {
        &[ProxyKind::Standard, ProxyKind::Full]
    }
}

/// Discards the 1:1 previews of `paths` (with `full_only` false, their
/// Standard previews too). Returns how many files went.
#[tauri::command]
pub async fn discard_previews(
    paths: Vec<String>,
    full_only: bool,
    app_handle: tauri::AppHandle,
) -> Result<usize, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app_handle.state::<AppState>();
        let mut removed = 0;
        let mut seen = std::collections::HashSet::new();
        for path in paths {
            let source = source_of(&path);
            if !seen.insert(source.clone()) {
                continue;
            }
            removed += preview_proxy::discard_source(Path::new(&source), kinds(full_only));
            if !full_only {
                state
                    .proxy_memory
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .forget_source(&source);
            }
        }
        removed
    })
    .await
    .map_err(|e| e.to_string())
}

/// One previews folder and what it holds.
#[derive(Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PreviewFolder {
    pub dir: String,
    pub usage: preview_proxy::PreviewUsage,
}

/// The previews of the open session (`folder` inside it) and those of
/// photographs outside sessions (the app's cache folder).
#[derive(Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PreviewCacheInfo {
    pub session: Option<PreviewFolder>,
    pub outside: Option<PreviewFolder>,
}

fn session_previews_dir(folder: Option<&str>) -> Option<PathBuf> {
    let folder = Path::new(folder?);
    preview_proxy::session_root(&folder.join("_"))?;
    preview_proxy::previews_dir_for_folder(folder)
}

fn outside_previews_dir() -> Option<PathBuf> {
    preview_proxy::outside_sessions_dir()
}

/// Disk use of the previews of the session holding `folder` and of those
/// outside sessions.
#[tauri::command]
pub async fn preview_cache_usage(folder: Option<String>) -> Result<PreviewCacheInfo, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let describe = |dir: PathBuf| PreviewFolder {
            usage: preview_proxy::usage(&dir),
            dir: dir.to_string_lossy().into_owned(),
        };
        PreviewCacheInfo {
            session: session_previews_dir(folder.as_deref()).map(describe),
            outside: outside_previews_dir().map(describe),
        }
    })
    .await
    .map_err(|e| e.to_string())
}

/// Discards the 1:1 previews (with `full_only` false, all previews) of the
/// session holding `folder` (`session` true) or of photographs outside
/// sessions. Returns how many files went.
#[tauri::command]
pub async fn clear_preview_cache(
    folder: Option<String>,
    session: bool,
    full_only: bool,
    app_handle: tauri::AppHandle,
) -> Result<usize, String> {
    let dir = if session {
        session_previews_dir(folder.as_deref())
    } else {
        outside_previews_dir()
    }
    .ok_or("No previews folder")?;
    let removed = tauri::async_runtime::spawn_blocking(move || {
        preview_proxy::discard(&dir, kinds(full_only), None)
    })
    .await
    .map_err(|e| e.to_string())?;
    if !full_only {
        *app_handle
            .state::<AppState>()
            .proxy_memory
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Default::default();
    }
    Ok(removed)
}

/// Applies the preferences' limits to a previews folder: discards 1:1
/// previews unused for longer than allowed and keeps the Standard previews
/// within their size limit. Returns how many files went.
fn apply_limits(dir: &Path, settings: &crate::app_settings::AppSettings) -> usize {
    let expired = preview_proxy::full_expiry(settings).map_or(0, |age| {
        preview_proxy::discard(dir, &[ProxyKind::Full], Some(age))
    });
    let over = preview_proxy::standard_limit(settings)
        .map_or(0, |limit| preview_proxy::enforce_standard_limit(dir, limit));
    expired + over
}

/// Applies the preferences' limits for previews (1:1 expiry, Standard size
/// limit) in the session holding `folder` and outside sessions: when a
/// library opens, after builds and when the preferences change. Returns
/// how many files went.
#[tauri::command]
pub async fn apply_preview_limits(
    folder: Option<String>,
    app_handle: tauri::AppHandle,
) -> Result<usize, String> {
    let settings = load_settings(app_handle).unwrap_or_default();
    tauri::async_runtime::spawn_blocking(move || {
        [
            session_previews_dir(folder.as_deref()),
            outside_previews_dir(),
        ]
        .into_iter()
        .flatten()
        .map(|dir| apply_limits(&dir, &settings))
        .sum()
    })
    .await
    .map_err(|e| e.to_string())
}

/// How often the limits are applied to one folder while previews are being
/// written (a folder scan; the newest previews are never the ones to go).
const LIMITS_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);

/// After a preview of `path` was written: the limits for its previews
/// folder, at most every `LIMITS_INTERVAL` per folder.
pub fn apply_limits_near(app_handle: &tauri::AppHandle, path: &str) {
    static LAST: Mutex<Option<std::collections::HashMap<PathBuf, std::time::Instant>>> =
        Mutex::new(None);
    let (source, _) = parse_virtual_path(path);
    let Some(dir) =
        preview_proxy::proxy_path(&source).and_then(|p| p.parent().map(Path::to_path_buf))
    else {
        return;
    };
    {
        let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
        let last = last.get_or_insert_with(Default::default);
        let now = std::time::Instant::now();
        if last
            .get(&dir)
            .is_some_and(|at| now.duration_since(*at) < LIMITS_INTERVAL)
        {
            return;
        }
        last.insert(dir.clone(), now);
    }
    let settings = load_settings(app_handle.clone()).unwrap_or_default();
    apply_limits(&dir, &settings);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| format!("/s/Capture/{n}")).collect()
    }

    #[test]
    fn prefetching_goes_before_builds_and_builds_keep_their_order() {
        let mut queue = Queue::default();
        queue.add_builds(paths(&["A.RAF", "B.RAF"]), false);
        queue.prefetch = ["/s/Capture/N.RAF".to_string()].into();
        assert_eq!(queue.next(), Some(Job::Prefetch("/s/Capture/N.RAF".into())));
        let Some(Job::Build(first)) = queue.next() else {
            panic!("a build")
        };
        assert_eq!(first.path, "/s/Capture/A.RAF");
        queue.finish(&first, Outcome::Built);
        let Some(Job::Build(second)) = queue.next() else {
            panic!("a build")
        };
        queue.finish(&second, Outcome::Current);
        assert_eq!(queue.next(), None);
        assert_eq!(
            queue.progress,
            BuildProgress {
                done: 2,
                total: 2,
                ..Default::default()
            }
        );
    }

    #[test]
    fn a_photograph_asked_for_twice_is_built_once_with_both_kinds() {
        let mut queue = Queue::default();
        queue.add_builds(paths(&["A.RAF", "B.RAF"]), false);
        queue.add_builds(vec!["/s/Capture/A.RAF?vc=1a2b3c".into()], true);
        assert_eq!(queue.progress.total, 2);
        let Some(Job::Build(first)) = queue.next() else {
            panic!("a build")
        };
        assert!(first.full);
        // Asked for again while it is being built: not queued again.
        queue.add_builds(paths(&["A.RAF"]), false);
        assert_eq!(queue.progress.total, 2);
        // Asked for its 1:1 preview while only its Standard one is built.
        let Some(Job::Build(second)) = queue.next() else {
            panic!("a build")
        };
        assert!(!second.full);
        queue.add_builds(paths(&["B.RAF"]), true);
        assert_eq!(queue.progress.total, 3);
    }

    #[test]
    fn failures_and_skips_are_counted_and_a_new_build_starts_afresh() {
        let mut queue = Queue::default();
        queue.add_builds(paths(&["A.RAF", "B.JPG", "C.RAF"]), false);
        for outcome in [
            Outcome::Failed("The RAW file is missing (offline)".into()),
            Outcome::Skipped,
            Outcome::Built,
        ] {
            let Some(Job::Build(item)) = queue.next() else {
                panic!("a build")
            };
            assert!(queue.progress.running);
            queue.finish(&item, outcome);
        }
        let progress = queue.progress.clone();
        assert!(!progress.running);
        assert_eq!((progress.done, progress.total), (3, 3));
        assert_eq!((progress.skipped, progress.failed_count), (1, 1));
        assert_eq!(progress.failed[0].path, "/s/Capture/A.RAF");
        queue.add_builds(paths(&["D.RAF"]), false);
        assert_eq!(
            (
                queue.progress.done,
                queue.progress.total,
                queue.progress.failed_count
            ),
            (0, 1, 0)
        );
    }

    #[test]
    fn stopping_drops_what_waits_and_the_photograph_in_work() {
        let mut queue = Queue::default();
        queue.add_builds(paths(&["A.RAF", "B.RAF", "C.RAF"]), true);
        let Some(Job::Build(first)) = queue.next() else {
            panic!("a build")
        };
        queue.finish(&first, Outcome::Built);
        let Some(Job::Build(second)) = queue.next() else {
            panic!("a build")
        };
        queue.stop();
        assert!(!queue.progress.running && queue.progress.stopped);
        assert_eq!((queue.progress.done, queue.progress.total), (1, 1));
        // The abandoned photograph reports back after the Stop: not counted.
        queue.finish(&second, Outcome::Cancelled);
        assert_eq!((queue.progress.done, queue.progress.total), (1, 1));
        assert_eq!(queue.next(), None);
        // The next build starts afresh and is not stopped.
        queue.add_builds(paths(&["B.RAF"]), false);
        let Some(Job::Build(again)) = queue.next() else {
            panic!("a build")
        };
        assert_eq!(again.generation, queue.generation);
        assert!(!queue.progress.stopped);
    }
}
