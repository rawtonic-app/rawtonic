//! Prepares preview proxies (preview_proxy.rs) in the background. Two kinds
//! of work:
//!
//! - Prefetch: the photographs the user is likely to open next, such as the
//!   filmstrip neighbours of the open one, on every core at normal priority
//!   as the open photograph's own decode (below the editor's priority on
//!   Linux): stepping on, its load waits for the decode under way. With a
//!   quiet time after every decode of the open photograph, four
//!   utility-class threads (the efficiency cores on Apple silicon) and a
//!   preview write before the next decode, 1 of 40 steps at 2.25 s per
//!   photograph found its decode ready on a six-core PC (notes:
//!   benchmark-results/fst-fast-steps-2026-11-01/REPORT.md). The newest
//!   request replaces the queue, and it goes before builds. It yields to
//!   the open photograph as the multi view's prefetch does: an interactive
//!   preview frame (a slider or brush drag) cancels the decode under way,
//!   which is done again later, as does the open photograph's own decode
//!   one of another photograph; none starts within
//!   `multi_view_prefetch::QUIET` after such an edit, while the open
//!   photograph is being decoded or developed (`multi_view_prefetch::busy`),
//!   nor within `multi_view_prefetch::SETTLE` after its decode or frame.
//!   On a six-core laptop with an integrated GPU the decodes slowed a drag's
//!   GPU work by ~70 % (the shared memory and power budget, not CPU time: a
//!   lower thread priority did not help), and on a first start they slowed
//!   the shader compile the first Develop frame waits for (AMD Radeon 680M:
//!   first frame 4.3–4.8 s after the click instead of 3.4–3.5 s).
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
use std::time::Duration;

use serde::Serialize;
use tauri::{Emitter, Manager};

use crate::app_settings::load_settings;
use crate::app_state::AppState;
use crate::file_management::parse_virtual_path;
use crate::formats::is_raw_file;
use crate::preview_proxy::{self, ProxyKind};

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
    /// The first photograph of the last prefetch request (the likeliest
    /// next one): its decode is kept for its opening.
    prefetch_head: Option<String>,
    /// Raised by each prefetch request (it replaces the queue).
    prefetch_generation: usize,
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

    /// Replaces what waits to be prefetched.
    fn set_prefetch(&mut self, paths: Vec<String>) {
        self.prefetch_head = paths.first().cloned();
        self.prefetch = paths.into();
        self.prefetch_generation += 1;
    }

    /// Whether the prefetch of `path` keeps its decode (the request's head).
    fn keeps_decode(&self, path: &str) -> bool {
        self.prefetch_head.as_deref() == Some(path)
    }

    /// Whether the request asks first for a photograph of RAW `source` (a
    /// virtual copy's too).
    fn asks_first_for(&self, source: &str) -> bool {
        self.prefetch_head
            .as_deref()
            .is_some_and(|head| source_of(head) == source)
    }

    /// A prefetch cancelled by an edit is done again first, unless a newer
    /// request replaced the queue since `generation`.
    fn retry_prefetch(&mut self, path: String, generation: usize) {
        if generation == self.prefetch_generation && !self.prefetch.contains(&path) {
            self.prefetch.push_front(path);
        }
    }

    /// The next job: prefetching first (the user waits for it) once the app
    /// is `quiet`, then builds (they run while the user edits).
    fn next(&mut self, quiet: bool) -> Option<Job> {
        if quiet && let Some(path) = self.prefetch.pop_front() {
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
    /// Cancels the prefetch decode under way on an edit.
    prefetch_gate: PrefetchGate,
}

/// Interactive frames counted for the prefetch: a decode started before
/// one gives up at its next cancel check.
#[derive(Default)]
struct PrefetchGate(Arc<AtomicUsize>);

impl PrefetchGate {
    /// Called after the frame was recorded as activity (`step_prefetch_wait`).
    fn interaction(&self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }

    /// The cancel token of a decode that may start now, None while it must
    /// still `wait` (after an edit or the open photograph's last decode or
    /// frame) or the open photograph is `busy` (its decode, or a frame under
    /// way, which on a first start waits for its shader compile). The token
    /// is taken before the wait is read: an interactive frame after it
    /// cancels the decode, one before it is seen as activity.
    fn start(
        &self,
        wait: impl FnOnce() -> Duration,
        busy: impl FnOnce() -> bool,
    ) -> Option<(Arc<AtomicUsize>, usize)> {
        let token = self.0.load(Ordering::SeqCst);
        (wait().is_zero() && !busy()).then(|| (Arc::clone(&self.0), token))
    }

    fn cancelled(&self, token: usize) -> bool {
        self.0.load(Ordering::SeqCst) != token
    }
}

static WORKER: OnceLock<Arc<Worker>> = OnceLock::new();

/// A decode with what it was made from (`preview_proxy::current_key`: file,
/// decoder settings, engine): its RAW, decoded-cache key, the decode, its
/// measures and that identity.
struct MadeDecode<I> {
    source: String,
    key: String,
    image: Arc<image::DynamicImage>,
    measures: Option<crate::image_loader::DecodeMeasures>,
    made: Option<I>,
}

impl<I: PartialEq> MadeDecode<I> {
    /// The decode, when it is of `key` and what a decode made `now` would be
    /// (a preference changed since, or the file, it is not used).
    fn take_if(
        slot: &mut Option<Self>,
        key: &str,
        now: &Option<I>,
    ) -> Option<(
        Arc<image::DynamicImage>,
        Option<crate::image_loader::DecodeMeasures>,
    )> {
        if !slot.as_ref().is_some_and(|held| held.key == key) {
            return None;
        }
        let held = slot.take()?;
        (held.made.is_some() && held.made == *now).then_some((held.image, held.measures))
    }
}

/// The decode kept for the photograph likeliest opened next (the prefetch
/// request's head), until its load takes it, another one replaces it or a
/// request no longer asks for it: one decode, within the multi view's hold
/// budget (none while short of memory).
static AHEAD: Mutex<Option<MadeDecode<preview_proxy::ProxyKey>>> = Mutex::new(None);

/// The prefetch decode under way, which the open photograph's load may
/// take over (`decode_for_open`).
struct UnderWay<I> {
    /// Its photograph and decoded-cache key.
    path: Option<String>,
    key: Option<String>,
    /// What it is made from (`preview_proxy::current_key`).
    made: Option<I>,
    /// The open photograph's load waits for it: it is handed over.
    wanted: bool,
    /// A decode handed over and not taken yet: kept until its load takes
    /// it, also while the next prefetch decode runs.
    handed: Option<MadeDecode<I>>,
}

/// What the open photograph's load does about the prefetch decode under way.
#[derive(Debug, PartialEq)]
enum OnOpen {
    /// None under way.
    Decode,
    /// It is the open photograph's: wait and take it.
    Wait,
    /// Another photograph's: cancel it (it is done again later), so the
    /// open photograph's decode has the cores.
    Cancel,
}

impl<I: PartialEq> UnderWay<I> {
    const fn idle() -> Self {
        Self {
            path: None,
            key: None,
            made: None,
            wanted: false,
            handed: None,
        }
    }

    fn start(&mut self, path: &str, key: &str, made: Option<I>) {
        self.path = Some(path.to_string());
        self.key = Some(key.to_string());
        self.made = made;
        self.wanted = false;
    }

    /// The decode ended (`decoded`: None when cancelled or failed); whether
    /// it was handed over to the load waiting for it.
    fn end(
        &mut self,
        decoded: Option<(
            Arc<image::DynamicImage>,
            Option<crate::image_loader::DecodeMeasures>,
        )>,
    ) -> bool {
        let key = self.key.take();
        let made = self.made.take();
        let source = self.path.take().unwrap_or_default();
        match (std::mem::take(&mut self.wanted), key, decoded) {
            (true, Some(key), Some((image, measures))) => {
                self.handed = Some(MadeDecode {
                    source,
                    key,
                    image,
                    measures,
                    made,
                });
                true
            }
            _ => false,
        }
    }

    /// What the load of `key` (a decode of it made `now` would be made from
    /// `now`) does about the decode under way: one of other decoder settings
    /// is cancelled at once rather than waited for and refused.
    fn on_open(&mut self, key: &str, now: &Option<I>) -> OnOpen {
        match self.key.as_deref() {
            None => OnOpen::Decode,
            Some(under_way) if under_way == key && self.made.is_some() && self.made == *now => {
                self.wanted = true;
                OnOpen::Wait
            }
            Some(_) => OnOpen::Cancel,
        }
    }

    /// Whether a new prefetch request of `paths` leaves the decode under way
    /// unasked for (a jump elsewhere): it is cancelled then, unless the open
    /// photograph's load waits for it.
    fn stale_for(&self, paths: &[String]) -> bool {
        !self.wanted
            && self.path.as_deref().is_some_and(|under_way| {
                !paths.iter().any(|p| source_of(p) == source_of(under_way))
            })
    }
}

static UNDER_WAY: (Mutex<UnderWay<preview_proxy::ProxyKey>>, Condvar) =
    (Mutex::new(UnderWay::idle()), Condvar::new());

/// Before the open photograph's own decode (`key`, its decoded-cache key;
/// `now`: what a decode of it made now would be, `preview_proxy::current_key`):
/// the decode kept ahead for it, or a prefetch decode of it under way
/// (finished first, waiting up to `wait`), instead of decoding the RAW a
/// second time; one of another photograph under way is cancelled, so the
/// open one has the cores. A decode of other settings or of a file changed
/// since is never used.
pub(crate) fn decode_for_open(
    key: &str,
    now: Option<preview_proxy::ProxyKey>,
    wait: Duration,
) -> Option<(
    Arc<image::DynamicImage>,
    Option<crate::image_loader::DecodeMeasures>,
)> {
    if let Some(kept) = MadeDecode::take_if(
        &mut AHEAD.lock().unwrap_or_else(|e| e.into_inner()),
        key,
        &now,
    ) {
        return Some(kept);
    }
    take_under_way(&UNDER_WAY, key, &now, wait, || {
        if let Some(worker) = WORKER.get() {
            worker.prefetch_gate.interaction();
        }
    })
}

/// Whether a decode is kept for the photograph of decoded-cache key `key`
/// that its load will take (made as a decode `now` would be).
pub(crate) fn keeps_decode_of(key: &str, now: &Option<preview_proxy::ProxyKey>) -> bool {
    AHEAD
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .is_some_and(|kept| kept.key == key && kept.made.is_some() && kept.made == *now)
}

/// The decode kept for the photograph of decoded-cache key `key` (made as a
/// decode `now` would be), shared rather than taken: the step onto it shows
/// its developed frame from it at once (`image_loader::open_kept_decode`),
/// and the slot keeps answering for it (`keeps_decode_of`) until its load
/// finds it in the decoded cache. No second copy is made.
pub(crate) fn kept_decode(
    key: &str,
    now: &Option<preview_proxy::ProxyKey>,
) -> Option<(
    Arc<image::DynamicImage>,
    Option<crate::image_loader::DecodeMeasures>,
)> {
    AHEAD
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .filter(|kept| kept.key == key && kept.made.is_some() && kept.made == *now)
        .map(|kept| (Arc::clone(&kept.image), kept.measures))
}

/// Whether the prefetch decode under way is the one of decoded-cache key
/// `key`.
pub(crate) fn decoding_ahead(key: &str) -> bool {
    UNDER_WAY.0.lock().unwrap_or_else(|e| e.into_inner()).key.as_deref() == Some(key)
}

/// Waits up to `wait` while the prefetch decode of decoded-cache key `key`
/// is under way (and its preview base built ahead, `decode`). Neither takes
/// nor asks for it: kept ahead, it is then shared (`kept_decode`).
pub(crate) fn wait_under_way(key: &str, wait: Duration) {
    let (under_way, ended) = &UNDER_WAY;
    let deadline = std::time::Instant::now() + wait;
    let mut guard = under_way.lock().unwrap_or_else(|e| e.into_inner());
    while guard.key.as_deref() == Some(key) {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            break;
        }
        guard = ended
            .wait_timeout(guard, left)
            .unwrap_or_else(|e| e.into_inner())
            .0;
    }
}

/// Whether the decode kept ahead is of RAW `source`.
fn kept_ahead(source: &str) -> bool {
    AHEAD
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .is_some_and(|kept| kept.source == source)
}

/// Whether the last prefetch request asks for `source` first.
fn is_prefetch_head(source: &str) -> bool {
    WORKER.get().is_some_and(|worker| {
        worker
            .queue
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .asks_first_for(source)
    })
}

/// The decoder's settings changed or the caches were cleared: the decode
/// kept ahead goes.
pub(crate) fn forget_ahead() {
    *AHEAD.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

fn take_under_way<I: PartialEq>(
    (under_way, ended): &(Mutex<UnderWay<I>>, Condvar),
    key: &str,
    now: &Option<I>,
    wait: Duration,
    cancel: impl FnOnce(),
) -> Option<(
    Arc<image::DynamicImage>,
    Option<crate::image_loader::DecodeMeasures>,
)> {
    let until = std::time::Instant::now() + wait;
    let mut state = under_way.lock().unwrap_or_else(|e| e.into_inner());
    // Handed over already (the next prefetch decode may have started).
    if state.handed.as_ref().is_some_and(|held| held.key == key) {
        return MadeDecode::take_if(&mut state.handed, key, now);
    }
    match state.on_open(key, now) {
        OnOpen::Decode => return None,
        OnOpen::Cancel => {
            drop(state);
            cancel();
            return None;
        }
        OnOpen::Wait => {}
    }
    loop {
        if state.handed.as_ref().is_some_and(|held| held.key == key) {
            return MadeDecode::take_if(&mut state.handed, key, now);
        }
        let left = until.saturating_duration_since(std::time::Instant::now());
        if state.key.as_deref() != Some(key) || left.is_zero() {
            // Cancelled, failed, or too slow: decoded by the load itself.
            state.wanted = false;
            return None;
        }
        state = ended
            .wait_timeout(state, left)
            .unwrap_or_else(|e| e.into_inner())
            .0;
    }
}

fn worker(app_handle: &tauri::AppHandle) -> Arc<Worker> {
    WORKER
        .get_or_init(|| {
            let worker = Arc::new(Worker {
                queue: Mutex::new(Queue::default()),
                wake: Condvar::new(),
                cancel: Arc::new(AtomicUsize::new(0)),
                viewer_dim: AtomicU32::new(0),
                prefetch_gate: PrefetchGate::default(),
            });
            // Short of memory: the decode kept for the next photograph goes
            // (made again when it is wanted and memory allows).
            crate::memory_pressure::watch(Arc::new(|_| forget_ahead()));
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

/// The preview worker develops a frame of the open photograph: an
/// interactive one cancels the prefetch decode under way (the quiet time
/// itself is `multi_view_prefetch`'s, which this frame restarts).
pub(crate) fn note_preview(interactive: bool) {
    if interactive && let Some(worker) = WORKER.get() {
        worker.prefetch_gate.interaction();
    }
}

fn emit_progress(app_handle: &tauri::AppHandle, progress: &BuildProgress) {
    let _ = app_handle.emit(PROGRESS_EVENT, progress);
}

fn run(worker: &Worker, app_handle: &tauri::AppHandle) {
    // Prefetches and the builds the user asked for run like an export: every
    // core (Lightroom builds previews at full speed too), below the editor's
    // priority. On four utility-class threads (the efficiency cores on Apple
    // silicon) a 40 MP X-Trans decode took 2-3 s of a build. Linux: on the
    // time the editor leaves, so the next photograph's decode slows the
    // frames of the one stepped to less (six-core PC, developed frame from a
    // kept decode 0.66 s p50 at normal priority, 0.55-0.61 s this way).
    let pool = rayon::ThreadPoolBuilder::new()
        .thread_name(|i| format!("preview-build-{i}"))
        .start_handler(|_| crate::export_processing::lower_thread_priority())
        .build()
        .expect("preview build thread pool");
    loop {
        let job = {
            let mut queue = worker.queue.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                let start = worker.prefetch_gate.start(
                    crate::multi_view_prefetch::step_prefetch_wait,
                    crate::multi_view_prefetch::busy,
                );
                if let Some(job) = queue.next(start.is_some()) {
                    let keep = matches!(&job, Job::Prefetch(path) if queue.keeps_decode(path));
                    break (job, queue.prefetch_generation, start, keep);
                }
                queue = if queue.prefetch.is_empty() {
                    worker.wake.wait(queue).unwrap_or_else(|e| e.into_inner())
                } else {
                    let wait = crate::multi_view_prefetch::step_prefetch_wait()
                        .max(Duration::from_millis(25));
                    worker
                        .wake
                        .wait_timeout(queue, wait)
                        .unwrap_or_else(|e| e.into_inner())
                        .0
                };
            }
        };
        let (job, prefetch_generation, start, keep) = job;
        match job {
            Job::Prefetch(path) => {
                let cancel = start.expect("prefetching only starts when quiet");
                let token = cancel.1;
                pool.install(|| prepare(app_handle, &path, cancel, keep));
                if worker.prefetch_gate.cancelled(token) {
                    // When the cancelled decode gave the cores back (compare
                    // with the pointer session's start).
                    #[cfg(feature = "performance-profiling")]
                    log::info!(
                        "[PERF] {}",
                        serde_json::json!({"stage": "preview_proxy_prefetch_cancelled", "epoch_ms": crate::performance::epoch_ms()})
                    );
                    worker
                        .queue
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .retry_prefetch(path, prefetch_generation);
                }
            }
            Job::Build(item) => {
                let outcome = pool.install(|| build(app_handle, worker, &item));
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

/// Prepares the preview proxy of `path`; with `keep` (the likeliest next
/// photograph) its decode stays in the decoded-image cache too, so opening
/// it develops the full frame without decoding the RAW again.
fn prepare(
    app_handle: &tauri::AppHandle,
    path: &str,
    cancel: (Arc<AtomicUsize>, usize),
    keep: bool,
) {
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
        // Decoded only to be kept: not while nothing may be held (short of
        // memory).
        if keep
            && preview_proxy::source_available(&source)
            && crate::multi_view_prefetch::hold_budget() > 0
        {
            #[cfg(feature = "performance-profiling")]
            let _stage = crate::performance::PerformanceStage::new("preview_prefetch_decode");
            let _ = decode(app_handle, &source, &settings, Some(cancel), Some(true));
        }
        return;
    }
    #[cfg(feature = "performance-profiling")]
    let _stage = crate::performance::PerformanceStage::new("preview_proxy_prefetch");
    let (raised, token) = cancel;
    let Some((image, variant, measures)) = decode(
        app_handle,
        &source,
        &settings,
        Some((Arc::clone(&raised), token)),
        Some(keep),
    ) else {
        return;
    };
    // An edit during the decode's last steps: no downscale and write
    // beside it either (done again later).
    if raised.load(Ordering::SeqCst) != token {
        return;
    }
    // Handed over to the open photograph's load, or kept for it: that load
    // writes the proxy beside its frames, so the next photograph's decode
    // starts now instead of after this write (~0.4 s for a 36 MP RAW on a
    // six-core PC, which made every step wait for a decode started late).
    if *state
        .current_source
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        == path
        || kept_ahead(&source.to_string_lossy())
    {
        return;
    }
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
/// its value). `prefetch`: a prefetch decode, which the open photograph's
/// load may take over (`decode_for_open`), with whether it is kept for that
/// load (`AHEAD`, within the multi view's hold budget), both with what it is
/// made from (`preview_proxy::current_key`).
fn decode(
    app_handle: &tauri::AppHandle,
    source: &Path,
    settings: &crate::app_settings::AppSettings,
    cancel: Option<(Arc<AtomicUsize>, usize)>,
    prefetch: Option<bool>,
) -> Option<(
    Arc<image::DynamicImage>,
    crate::image_loader::DecodeVariant,
    Option<crate::image_loader::DecodeMeasures>,
)> {
    let state = app_handle.state::<AppState>();
    let source_str = source.to_string_lossy();
    let variant = crate::image_loader::stored_decode_variant(&source_str);
    let key = crate::image_loader::decoded_cache_key(&source_str, None, variant);
    let cached = state
        .decoded_image_cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_measured(&key);
    if let Some((image, _, measures)) = cached {
        return Some((image, variant, measures));
    }
    let cancelled = cancel
        .clone()
        .map(|(raised, value)| move || raised.load(Ordering::SeqCst) != value);
    let made = prefetch.and_then(|_| preview_proxy::current_key(source, settings, variant));
    if prefetch == Some(true)
        && let Some(kept) = AHEAD
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .filter(|kept| kept.key == key && kept.made.is_some() && kept.made == made)
    {
        return Some((Arc::clone(&kept.image), variant, kept.measures));
    }
    let bytes = crate::file_management::read_file_mapped(source).ok()?;
    if prefetch.is_some() {
        UNDER_WAY.0.lock().unwrap_or_else(|e| e.into_inner()).start(
            &source_str,
            &key,
            made.clone(),
        );
    }
    let decoded = crate::image_loader::load_base_image_measured_for(
        &bytes,
        &source_str,
        false,
        settings,
        cancel,
        None,
        None,
        Some(variant),
    )
    .map(|(image, measures)| (Arc::new(image), measures));
    if let Some(keep) = prefetch {
        // Kept (before the load waiting for it is told), else handed over
        // to the open photograph's load, which waits for it. Also when a
        // request made since asks for it first (stepped on meanwhile: the
        // decode started as the second photograph ahead), rather than
        // decoding it again as the new head.
        let mut kept = None;
        if (keep || is_prefetch_head(&source_str))
            && made.is_some()
            && let Ok((image, measures)) = &decoded
            && image.as_bytes().len() <= crate::multi_view_prefetch::hold_budget()
        {
            *AHEAD.lock().unwrap_or_else(|e| e.into_inner()) = Some(MadeDecode {
                source: source_str.to_string(),
                key: key.clone(),
                image: Arc::clone(image),
                measures: *measures,
                made: made.clone(),
            });
            kept = Some(Arc::clone(image));
        }
        let (under_way, ended) = &UNDER_WAY;
        // Its first developed frame then finds its preview base built,
        // before the decode is told ended (a step onto it waits for both,
        // `wait_under_way`). Not for a decode the open photograph's load
        // waits for: its frame is being made from the load, and the next
        // decode starts sooner (before that frame holds it back).
        let wanted = under_way.lock().unwrap_or_else(|e| e.into_inner()).wanted;
        if !wanted && let Some(image) = kept {
            crate::editor_preview::prepare_base_ahead(&state, &source_str, &image);
        }
        under_way.lock().unwrap_or_else(|e| e.into_inner()).end(
            decoded
                .as_ref()
                .ok()
                .map(|(image, measures)| (Arc::clone(image), *measures)),
        );
        ended.notify_all();
    }
    match decoded {
        Ok((image, measures)) => Some((image, variant, measures)),
        Err(_) if cancelled.is_some_and(|cancelled| cancelled()) => None,
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
    let Some((image, variant, measures)) = decode(app_handle, &source, &settings, cancel, None)
    else {
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
    // A jump elsewhere: the decode under way is not wanted any more.
    if UNDER_WAY
        .0
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .stale_for(&paths)
    {
        worker.prefetch_gate.interaction();
    }
    // Nor the decode kept ahead: let go.
    {
        let mut ahead = AHEAD.lock().unwrap_or_else(|e| e.into_inner());
        if ahead
            .as_ref()
            .is_some_and(|kept| !paths.iter().any(|p| source_of(p) == kept.source))
        {
            *ahead = None;
        }
    }
    let mut queue = worker.queue.lock().unwrap_or_else(|e| e.into_inner());
    queue.set_prefetch(paths);
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

fn session_previews_dir(folder: Option<&str>) -> Option<PathBuf> {
    let folder = Path::new(folder?);
    preview_proxy::session_root(&folder.join("_"))?;
    preview_proxy::previews_dir_for_folder(folder)
}

fn outside_previews_dir() -> Option<PathBuf> {
    preview_proxy::outside_sessions_dir()
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
        queue.set_prefetch(vec!["/s/Capture/N.RAF".to_string()]);
        assert_eq!(
            queue.next(true),
            Some(Job::Prefetch("/s/Capture/N.RAF".into()))
        );
        let Some(Job::Build(first)) = queue.next(true) else {
            panic!("a build")
        };
        assert_eq!(first.path, "/s/Capture/A.RAF");
        queue.finish(&first, Outcome::Built);
        let Some(Job::Build(second)) = queue.next(true) else {
            panic!("a build")
        };
        queue.finish(&second, Outcome::Current);
        assert_eq!(queue.next(true), None);
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
    fn prefetching_waits_while_the_user_edits_and_builds_go_on() {
        let mut queue = Queue::default();
        queue.set_prefetch(paths(&["N.RAF"]));
        assert_eq!(queue.next(false), None);
        queue.add_builds(paths(&["A.RAF"]), false);
        let Some(Job::Build(build)) = queue.next(false) else {
            panic!("a build")
        };
        queue.finish(&build, Outcome::Built);
        assert_eq!(
            queue.next(true),
            Some(Job::Prefetch("/s/Capture/N.RAF".into()))
        );
    }

    #[test]
    fn an_interactive_frame_while_a_prefetch_starts_is_never_lost() {
        let gate = PrefetchGate::default();
        let idle = || false;
        // Recorded as activity before the wait is read: no start.
        assert!(
            gate.start(|| crate::multi_view_prefetch::QUIET, idle)
                .is_none()
        );
        // Lands after the token was taken, before its activity is seen:
        // the decode starts but is already cancelled.
        let (_, token) = gate
            .start(
                || {
                    gate.interaction();
                    Duration::ZERO
                },
                idle,
            )
            .expect("quiet");
        assert!(gate.cancelled(token));
        // Quiet and no frame since: the decode runs.
        let (_, token) = gate.start(|| Duration::ZERO, idle).expect("quiet");
        assert!(!gate.cancelled(token));
        gate.interaction();
        assert!(gate.cancelled(token));
    }

    #[test]
    fn no_prefetch_starts_while_the_open_photograph_is_decoded_or_developed() {
        // Quiet (no slider frame yet), but the open photograph's decode or
        // its first frame (on a first start waiting for its shader compile)
        // is under way: the neighbours wait.
        let gate = PrefetchGate::default();
        assert!(gate.start(|| Duration::ZERO, || true).is_none());
        assert!(gate.start(|| Duration::ZERO, || false).is_some());
    }

    #[test]
    fn a_cancelled_prefetch_is_done_again_unless_a_newer_request_replaced_it() {
        let mut queue = Queue::default();
        queue.set_prefetch(paths(&["N.RAF", "P.RAF"]));
        let generation = queue.prefetch_generation;
        let Some(Job::Prefetch(first)) = queue.next(true) else {
            panic!("a prefetch")
        };
        queue.retry_prefetch(first.clone(), generation);
        assert_eq!(queue.next(true), Some(Job::Prefetch(first.clone())));
        queue.set_prefetch(paths(&["Q.RAF"]));
        queue.retry_prefetch(first, generation);
        assert_eq!(
            queue.next(true),
            Some(Job::Prefetch("/s/Capture/Q.RAF".into()))
        );
        assert_eq!(queue.next(true), None);
    }

    #[test]
    fn a_photograph_asked_for_twice_is_built_once_with_both_kinds() {
        let mut queue = Queue::default();
        queue.add_builds(paths(&["A.RAF", "B.RAF"]), false);
        queue.add_builds(vec!["/s/Capture/A.RAF?vc=1a2b3c".into()], true);
        assert_eq!(queue.progress.total, 2);
        let Some(Job::Build(first)) = queue.next(true) else {
            panic!("a build")
        };
        assert!(first.full);
        // Asked for again while it is being built: not queued again.
        queue.add_builds(paths(&["A.RAF"]), false);
        assert_eq!(queue.progress.total, 2);
        // Asked for its 1:1 preview while only its Standard one is built.
        let Some(Job::Build(second)) = queue.next(true) else {
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
            let Some(Job::Build(item)) = queue.next(true) else {
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
        let Some(Job::Build(first)) = queue.next(true) else {
            panic!("a build")
        };
        queue.finish(&first, Outcome::Built);
        let Some(Job::Build(second)) = queue.next(true) else {
            panic!("a build")
        };
        queue.stop();
        assert!(!queue.progress.running && queue.progress.stopped);
        assert_eq!((queue.progress.done, queue.progress.total), (1, 1));
        // The abandoned photograph reports back after the Stop: not counted.
        queue.finish(&second, Outcome::Cancelled);
        assert_eq!((queue.progress.done, queue.progress.total), (1, 1));
        assert_eq!(queue.next(true), None);
        // The next build starts afresh and is not stopped.
        queue.add_builds(paths(&["B.RAF"]), false);
        let Some(Job::Build(again)) = queue.next(true) else {
            panic!("a build")
        };
        assert_eq!(again.generation, queue.generation);
        assert!(!queue.progress.stopped);
    }

    #[test]
    fn a_decode_started_second_is_kept_once_a_step_asks_for_it_first() {
        let mut queue = Queue::default();
        queue.set_prefetch(paths(&["N1.NEF", "N2.NEF?vc=1a2b3c", "P.NEF"]));
        assert!(queue.asks_first_for("/s/Capture/N1.NEF"));
        assert!(!queue.asks_first_for("/s/Capture/N2.NEF"));
        // Stepped on while N2 decodes: the new request asks for it first,
        // so its decode, ending now, is kept for its opening.
        queue.set_prefetch(paths(&["N2.NEF?vc=1a2b3c", "N3.NEF", "N1.NEF"]));
        assert!(queue.asks_first_for("/s/Capture/N2.NEF"));
        assert!(!queue.asks_first_for("/s/Capture/N1.NEF"));
    }

    #[test]
    fn only_the_first_photograph_asked_for_keeps_its_decode() {
        let mut queue = Queue::default();
        queue.set_prefetch(paths(&["N.RAF", "NN.RAF", "P.RAF"]));
        assert!(queue.keeps_decode("/s/Capture/N.RAF"));
        assert!(!queue.keeps_decode("/s/Capture/NN.RAF"));
        // A step back: the previous photograph comes first now.
        queue.set_prefetch(paths(&["P.RAF", "PP.RAF", "N.RAF"]));
        assert!(queue.keeps_decode("/s/Capture/P.RAF"));
        assert!(!queue.keeps_decode("/s/Capture/N.RAF"));
    }

    fn decoded() -> (
        Arc<image::DynamicImage>,
        Option<crate::image_loader::DecodeMeasures>,
    ) {
        (Arc::new(image::DynamicImage::new_rgb8(2, 2)), None)
    }

    /// What a decode was made from, in tests: a number per settings state.
    type Shared = (Mutex<UnderWay<u32>>, Condvar);

    fn shared() -> Shared {
        (Mutex::new(UnderWay::idle()), Condvar::new())
    }

    #[test]
    fn a_jump_cancels_the_prefetch_decode_it_no_longer_asks_for() {
        let mut under_way = UnderWay::<u32>::idle();
        assert!(!under_way.stale_for(&paths(&["N.RAF"])));
        under_way.start("/s/Capture/N.RAF", "N-key", Some(0));
        // Still asked for (a virtual copy of it counts): kept going.
        assert!(!under_way.stale_for(&paths(&["N.RAF?vc=2", "P.RAF"])));
        assert!(under_way.stale_for(&paths(&["X.RAF", "Y.RAF"])));
        // The open photograph's load waits for it: never cancelled.
        assert_eq!(under_way.on_open("N-key", &Some(0)), OnOpen::Wait);
        assert!(!under_way.stale_for(&paths(&["X.RAF"])));
    }

    #[test]
    fn opening_another_photograph_cancels_the_prefetch_decode_under_way() {
        let shared = shared();
        let mut cancelled = false;
        assert!(
            take_under_way(&shared, "N-key", &Some(0), Duration::ZERO, || cancelled =
                true)
            .is_none()
        );
        assert!(!cancelled, "nothing under way: the load decodes");
        shared
            .0
            .lock()
            .unwrap()
            .start("/s/Capture/N.RAF", "N-key", Some(0));
        let taken = take_under_way(&shared, "X-key", &Some(0), Duration::from_secs(1), || {
            cancelled = true
        });
        assert!(taken.is_none() && cancelled);
        // The cancelled decode ends with nothing to hand over.
        assert!(!shared.0.lock().unwrap().end(None));
        assert!(shared.0.lock().unwrap().handed.is_none());
    }

    #[test]
    fn the_open_photographs_load_takes_over_its_prefetch_decode() {
        let shared = Arc::new(shared());
        shared
            .0
            .lock()
            .unwrap()
            .start("/s/Capture/N.RAF", "N-key", Some(0));
        let decoder = {
            let shared = Arc::clone(&shared);
            std::thread::spawn(move || {
                // Ends once the load waits for it.
                while !shared.0.lock().unwrap().wanted {
                    std::thread::sleep(Duration::from_millis(1));
                }
                let handed = shared.0.lock().unwrap().end(Some(decoded()));
                shared.1.notify_all();
                handed
            })
        };
        let taken = take_under_way(&shared, "N-key", &Some(0), Duration::from_secs(10), || {
            panic!("its own decode is not cancelled")
        });
        assert!(decoder.join().unwrap(), "handed over");
        assert!(taken.is_some());
        assert!(shared.0.lock().unwrap().handed.is_none(), "taken, not kept");
    }

    #[test]
    fn a_decode_of_settings_changed_since_is_never_taken() {
        // A preference that changes the decode (Linear RAW, highlight
        // recovery) changed while it was made, or after it was kept.
        let shared = shared();
        {
            let mut state = shared.0.lock().unwrap();
            state.start("/s/Capture/N.RAF", "N-key", Some(3));
            assert_eq!(state.on_open("N-key", &Some(3)), OnOpen::Wait);
            assert!(state.end(Some(decoded())));
        }
        assert!(take_under_way(&shared, "N-key", &Some(4), Duration::ZERO, || {}).is_none());
        assert!(
            shared.0.lock().unwrap().handed.is_none(),
            "dropped, not kept"
        );
        let mut kept = Some(MadeDecode {
            source: "/s/Capture/N.RAF".into(),
            key: "N-key".into(),
            image: decoded().0,
            measures: None,
            made: Some(3u32),
        });
        assert!(MadeDecode::take_if(&mut kept, "N-key", &Some(4)).is_none());
        assert!(kept.is_none(), "a stale one goes");
        // Unchanged settings: taken; another photograph's: left.
        let mut kept = Some(MadeDecode {
            source: "/s/Capture/N.RAF".into(),
            key: "N-key".into(),
            image: decoded().0,
            measures: None,
            made: Some(4u32),
        });
        assert!(MadeDecode::take_if(&mut kept, "X-key", &Some(4)).is_none());
        assert!(kept.is_some());
        assert!(MadeDecode::take_if(&mut kept, "N-key", &Some(4)).is_some());
        // Nothing known about how it was made: never taken.
        let mut unknown = Some(MadeDecode::<u32> {
            source: "/s/Capture/N.RAF".into(),
            key: "N-key".into(),
            image: decoded().0,
            measures: None,
            made: None,
        });
        assert!(MadeDecode::take_if(&mut unknown, "N-key", &None).is_none());
    }

    #[test]
    fn a_decode_of_other_settings_under_way_is_cancelled_not_waited_for() {
        let shared = shared();
        shared
            .0
            .lock()
            .unwrap()
            .start("/s/Capture/N.RAF", "N-key", Some(3));
        let mut cancelled = false;
        let started = std::time::Instant::now();
        let taken = take_under_way(&shared, "N-key", &Some(4), Duration::from_secs(5), || {
            cancelled = true
        });
        assert!(taken.is_none() && cancelled);
        assert!(started.elapsed() < Duration::from_secs(1), "no wait");
        assert!(!shared.0.lock().unwrap().wanted);
    }

    #[test]
    fn a_handed_decode_waits_for_its_load_while_the_next_prefetch_starts() {
        let shared = shared();
        {
            let mut state = shared.0.lock().unwrap();
            state.start("/s/Capture/N.RAF", "N-key", Some(0));
            assert_eq!(state.on_open("N-key", &Some(0)), OnOpen::Wait);
            assert!(state.end(Some(decoded())));
            // The worker starts the next decode before the load wakes up.
            state.start("/s/Capture/NN.RAF", "NN-key", Some(0));
        }
        let mut cancelled = false;
        let taken = take_under_way(&shared, "N-key", &Some(0), Duration::ZERO, || {
            cancelled = true
        });
        assert!(taken.is_some() && !cancelled);
        assert!(shared.0.lock().unwrap().handed.is_none());
    }

    #[test]
    fn a_prefetch_decode_nobody_waits_for_is_not_handed_over() {
        let mut under_way = UnderWay::<u32>::idle();
        under_way.start("/s/Capture/N.RAF", "N-key", Some(0));
        assert!(!under_way.end(Some(decoded())));
        assert!(under_way.handed.is_none() && under_way.key.is_none());
        // A load that gave up waiting leaves nothing behind either.
        let shared = shared();
        shared
            .0
            .lock()
            .unwrap()
            .start("/s/Capture/N.RAF", "N-key", Some(0));
        assert!(
            take_under_way(&shared, "N-key", &Some(0), Duration::from_millis(5), || {}).is_none()
        );
        assert!(!shared.0.lock().unwrap().end(Some(decoded())));
    }
}
