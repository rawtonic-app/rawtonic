// SPDX-License-Identifier: AGPL-3.0-or-later
//! Readies the multi view's other photographs for 100 %: once the cells are
//! shown and nothing else is going on, their RAWs are decoded one at a time
//! in the background and kept as 1:1 previews (preview_proxy, the session's
//! `.cache/previews`, as Lightroom builds 1:1 previews and Capture One its
//! preview files). A locked zoom then develops each cell's region from the
//! few bands of its 1:1 preview it shows (`multi_view::prepare_region`)
//! instead of decoding every RAW at once.
//!
//! Bounds: one decode at a time (a 61 MP decode alone takes well over a
//! gigabyte while it runs; it is released, and the allocator's freed pages
//! returned, before the next), on a small pool of utility-priority threads;
//! nothing starts while the primary is being edited, a zoom is being
//! developed, Edit all selected follows a drag or the system's memory
//! pressure is critical, and an edit of the primary cancels the decode under
//! way (it is done again later). Photographs with a current 1:1 preview and
//! other files than RAWs are skipped.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use tauri::Manager;

use crate::app_settings::load_settings;
use crate::app_state::AppState;
use crate::file_management::{parse_virtual_path, read_file_mapped};
use crate::formats::is_raw_file;
use crate::preview_proxy;

/// Threads of the decode under way (of the rayon work inside it).
const THREADS: usize = 4;

/// How long the app must be quiet (no interactive preview frame, no zoom
/// being developed) before a decode starts.
pub(crate) const QUIET: Duration = Duration::from_millis(600);

/// The other photographs of the multi view, nearest the primary first (the
/// next ones before the previous ones at the same distance), as their cells
/// are ordered; the primary has its own full decode.
pub(crate) fn order(cells: &[String], primary: Option<&str>) -> Vec<String> {
    let at = primary.and_then(|p| cells.iter().position(|c| c == p));
    let mut others: Vec<(usize, usize, &String)> = cells
        .iter()
        .enumerate()
        .filter(|(_, path)| Some(path.as_str()) != primary)
        .map(|(i, path)| match at {
            Some(at) if i > at => (i - at, 0, path),
            Some(at) => (at - i, 1, path),
            None => (i, 0, path),
        })
        .collect();
    others.sort();
    let mut seen = std::collections::HashSet::new();
    others
        .into_iter()
        .filter(|(_, _, path)| seen.insert(source_of(path)))
        .map(|(_, _, path)| path.clone())
        .collect()
}

/// Virtual copies share their RAW and its 1:1 preview.
fn source_of(path: &str) -> String {
    path.split("?vc=").next().unwrap_or(path).to_string()
}

/// Whether a decode may start now: `quiet` since the last interactive frame
/// of the primary and since the zoom's regions were last developed, and
/// nothing else is going on.
pub(crate) fn may_start(since_interaction: Duration, busy: bool, short_of_memory: bool) -> bool {
    !busy && !short_of_memory && since_interaction >= QUIET
}

#[derive(Default)]
struct Queue {
    generation: usize,
    todo: VecDeque<String>,
}

struct Worker {
    queue: Mutex<Queue>,
    wake: Condvar,
    /// Raised to cancel the decode under way (a new plan, an edit).
    cancel: Arc<AtomicUsize>,
    /// The decode under way, if any.
    decoding: AtomicBool,
}

static WORKER: OnceLock<Arc<Worker>> = OnceLock::new();

/// Milliseconds since `EPOCH` of the last interactive preview frame or zoom
/// work; 0: none yet.
static LAST_ACTIVITY: AtomicU64 = AtomicU64::new(0);
/// Zoomed regions being developed.
static REGIONS_BUSY: AtomicUsize = AtomicUsize::new(0);

fn epoch() -> Instant {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    *EPOCH.get_or_init(Instant::now)
}

fn now_ms() -> u64 {
    epoch().elapsed().as_millis() as u64 + 1
}

fn since_activity() -> Duration {
    let last = LAST_ACTIVITY.load(Ordering::Relaxed);
    if last == 0 {
        return Duration::MAX;
    }
    Duration::from_millis(now_ms().saturating_sub(last))
}

/// The preview worker develops a frame of the primary: an interactive one
/// (a slider or brush drag) cancels the decode under way and holds the
/// next one back for [`QUIET`].
pub(crate) fn note_preview(interactive: bool) {
    if !interactive {
        return;
    }
    LAST_ACTIVITY.store(now_ms(), Ordering::Relaxed);
    if let Some(worker) = WORKER.get()
        && worker.decoding.load(Ordering::Relaxed)
    {
        worker.cancel.fetch_add(1, Ordering::SeqCst);
    }
}

/// Zoomed regions are being developed while the guard lives: the decode
/// under way is cancelled and none starts (they need the cores and memory).
pub(crate) struct RegionsBusy;

impl RegionsBusy {
    pub(crate) fn start() -> Self {
        REGIONS_BUSY.fetch_add(1, Ordering::SeqCst);
        if let Some(worker) = WORKER.get()
            && worker.decoding.load(Ordering::Relaxed)
        {
            worker.cancel.fetch_add(1, Ordering::SeqCst);
        }
        RegionsBusy
    }
}

impl Drop for RegionsBusy {
    fn drop(&mut self) {
        LAST_ACTIVITY.store(now_ms(), Ordering::Relaxed);
        REGIONS_BUSY.fetch_sub(1, Ordering::SeqCst);
        if let Some(worker) = WORKER.get() {
            worker.wake.notify_one();
        }
    }
}

fn worker(app_handle: &tauri::AppHandle) -> Arc<Worker> {
    WORKER
        .get_or_init(|| {
            let worker = Arc::new(Worker {
                queue: Mutex::new(Queue::default()),
                wake: Condvar::new(),
                cancel: Arc::new(AtomicUsize::new(0)),
                decoding: AtomicBool::new(false),
            });
            let shared = Arc::clone(&worker);
            let app_handle = app_handle.clone();
            std::thread::Builder::new()
                .name("multi-view-prefetch".into())
                .spawn(move || run(&shared, &app_handle))
                .expect("spawn the multi view prefetch worker");
            worker
        })
        .clone()
}

/// Readies `paths` (in order) for 100 %, replacing what was planned; empty
/// stops (the multi view ended). Called once the cells are shown.
pub(crate) fn plan(app_handle: &tauri::AppHandle, paths: Vec<String>) {
    // Profiling builds can switch it off for paired runs.
    #[cfg(feature = "performance-profiling")]
    if std::env::var_os("RAWTONIC_NO_ZOOM_PREFETCH").is_some() {
        return;
    }
    let worker = worker(app_handle);
    let mut queue = worker.queue.lock().unwrap_or_else(|e| e.into_inner());
    if queue.todo.iter().eq(paths.iter()) {
        return;
    }
    queue.generation += 1;
    queue.todo = paths.into();
    worker.cancel.fetch_add(1, Ordering::SeqCst);
    worker.wake.notify_one();
}

#[cfg(target_os = "macos")]
fn lower_priority() {
    // SAFETY: sets the calling thread's own QoS class; no pointers involved.
    unsafe {
        libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_UTILITY, 0);
    }
}

#[cfg(not(target_os = "macos"))]
fn lower_priority() {}

fn run(worker: &Worker, app_handle: &tauri::AppHandle) {
    lower_priority();
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(THREADS)
        .thread_name(|i| format!("multi-view-prefetch-{i}"))
        .start_handler(|_| lower_priority())
        .build()
        .expect("multi view prefetch thread pool");
    let mut waiting_for = "";
    loop {
        // The next photograph, once the app is quiet.
        let (path, generation) = {
            let mut queue = worker.queue.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                if !queue.todo.is_empty() {
                    let quiet = since_activity();
                    let busy = REGIONS_BUSY.load(Ordering::SeqCst) > 0
                        || crate::multi_view::live_pending();
                    // Critical only: a Mac with Capture One and Lightroom
                    // open beside it sat at the warning level for minutes.
                    let short_of_memory = crate::memory_pressure::level_now()
                        == Some(crate::memory_pressure::Level::Critical);
                    if may_start(quiet, busy, short_of_memory) {
                        waiting_for = "";
                        break;
                    }
                    let reason = if short_of_memory {
                        "memory pressure"
                    } else if busy {
                        "zoom or Edit all selected"
                    } else {
                        "edits of the primary"
                    };
                    if reason != waiting_for {
                        log::info!("[multi_view] 1:1 previews wait ({reason})");
                        waiting_for = reason;
                    }
                    let wait = QUIET.saturating_sub(quiet).max(Duration::from_millis(100));
                    queue = worker
                        .wake
                        .wait_timeout(queue, wait)
                        .unwrap_or_else(|e| e.into_inner())
                        .0;
                    continue;
                }
                queue = worker.wake.wait(queue).unwrap_or_else(|e| e.into_inner());
            }
            let path = queue.todo.front().cloned().expect("not empty");
            (path, queue.generation)
        };
        let token = worker.cancel.load(Ordering::SeqCst);
        worker.decoding.store(true, Ordering::SeqCst);
        let outcome =
            pool.install(|| ready(app_handle, &path, (Arc::clone(&worker.cancel), token)));
        worker.decoding.store(false, Ordering::SeqCst);
        rawtonic_runtime::allocator::release_freed_memory();
        let mut queue = worker.queue.lock().unwrap_or_else(|e| e.into_inner());
        // Cancelled (an edit, a zoom): the same photograph again later.
        if outcome != Outcome::Cancelled
            && queue.generation == generation
            && queue.todo.front() == Some(&path)
        {
            queue.todo.pop_front();
        }
    }
}

#[derive(Debug, PartialEq)]
enum Outcome {
    Ready,
    Skipped,
    Cancelled,
}

/// Decodes `path` and keeps its 1:1 preview, unless it has a current one.
fn ready(app_handle: &tauri::AppHandle, path: &str, cancel: (Arc<AtomicUsize>, usize)) -> Outcome {
    let (source, _): (PathBuf, _) = parse_virtual_path(path);
    if !is_raw_file(&source)
        || preview_proxy::proxy_path_of(&source, preview_proxy::ProxyKind::Full).is_none()
        || !preview_proxy::source_available(&source)
    {
        return Outcome::Skipped;
    }
    let settings = load_settings(app_handle.clone()).unwrap_or_default();
    // The Standard preview too, when missing (cheap from the decode): the
    // cell is then developed from it when the multi view is shown again,
    // without a decode.
    let state = app_handle.state::<AppState>();
    let standard =
        preview_proxy::standard_size(state.last_preview_dim.load(Ordering::Relaxed), &settings);
    let needs_standard =
        preview_proxy::stored_size(&source, &settings).is_none_or(|stored| stored < standard);
    if preview_proxy::has_full(&source, &settings) && !needs_standard {
        return Outcome::Skipped;
    }
    let source_str = source.to_string_lossy().to_string();
    // The open photograph's decode in memory serves as it is.
    let cached = app_handle
        .state::<AppState>()
        .decoded_image_cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&source_str)
        .map(|(image, _)| image);
    let start = Instant::now();
    let image = match cached {
        Some(image) => image,
        None => {
            let Ok(bytes) = read_file_mapped(&source) else {
                return Outcome::Skipped;
            };
            let (tracker, token) = (&cancel.0, cancel.1);
            match crate::image_loader::load_base_image_from_bytes(
                &bytes,
                &source_str,
                false,
                &settings,
                Some(cancel.clone()),
            ) {
                Ok(image) => Arc::new(image),
                Err(_) if tracker.load(Ordering::SeqCst) != token => return Outcome::Cancelled,
                Err(e) => {
                    log::warn!("[multi_view] 1:1 preview of {path}: decode failed: {e}");
                    return Outcome::Skipped;
                }
            }
        }
    };
    if cancel.0.load(Ordering::SeqCst) != cancel.1 {
        return Outcome::Cancelled;
    }
    let decoded_ms = start.elapsed().as_secs_f64() * 1e3;
    if needs_standard {
        match preview_proxy::write(&source, &image, standard, &settings) {
            // The thumbnail is then developed from it (as proxy_worker does).
            Ok(_) => crate::file_management::refresh_thumbnail(app_handle, path),
            Err(e) => log::warn!("[multi_view] Standard preview of {path} not written: {e}"),
        }
    }
    if preview_proxy::has_full(&source, &settings) {
        return Outcome::Ready;
    }
    match preview_proxy::write_full(&source, &image, &settings) {
        Ok(()) => {
            log::info!(
                "[multi_view] 1:1 preview of {path} ready in the background: decoded {decoded_ms:.0} ms, written {:.0} ms",
                start.elapsed().as_secs_f64() * 1e3 - decoded_ms
            );
            Outcome::Ready
        }
        Err(e) => {
            log::warn!("[multi_view] 1:1 preview of {path} not written: {e}");
            Outcome::Skipped
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn the_primarys_neighbours_come_first() {
        let cells = paths(&["a", "b", "c", "d", "e", "f"]);
        assert_eq!(order(&cells, Some("c")), paths(&["d", "b", "e", "a", "f"]));
        assert_eq!(order(&cells, Some("a")), paths(&["b", "c", "d", "e", "f"]));
        assert_eq!(order(&cells, Some("f")), paths(&["e", "d", "c", "b", "a"]));
        // No primary in the cells: in their order.
        assert_eq!(order(&cells, None), cells);
    }

    #[test]
    fn virtual_copies_share_one_preview() {
        let cells = paths(&["a.raf", "a.raf?vc=1", "b.raf", "a.raf?vc=2"]);
        assert_eq!(order(&cells, Some("b.raf")), paths(&["a.raf?vc=2"]));
        // A copy of the primary's RAW: its 1:1 preview comes from the
        // primary's decode in memory.
        assert_eq!(
            order(&cells, Some("a.raf")),
            paths(&["a.raf?vc=1", "b.raf"])
        );
    }

    #[test]
    fn decodes_wait_for_a_quiet_app_with_memory_to_spare() {
        assert!(may_start(Duration::MAX, false, false));
        assert!(may_start(QUIET, false, false));
        assert!(!may_start(QUIET - Duration::from_millis(1), false, false));
        assert!(!may_start(Duration::MAX, true, false));
        assert!(!may_start(Duration::MAX, false, true));
    }

    #[test]
    fn an_edit_or_a_zoom_cancels_the_decode_under_way() {
        let worker = Arc::new(Worker {
            queue: Mutex::new(Queue::default()),
            wake: Condvar::new(),
            cancel: Arc::new(AtomicUsize::new(0)),
            decoding: AtomicBool::new(true),
        });
        // The static worker is shared by the tests of this module only.
        let worker = WORKER.get_or_init(|| worker).clone();
        let before = worker.cancel.load(Ordering::SeqCst);
        note_preview(false);
        assert_eq!(worker.cancel.load(Ordering::SeqCst), before);
        note_preview(true);
        assert_eq!(worker.cancel.load(Ordering::SeqCst), before + 1);
        assert!(since_activity() < QUIET);
        {
            let _busy = RegionsBusy::start();
            assert_eq!(worker.cancel.load(Ordering::SeqCst), before + 2);
            assert_eq!(REGIONS_BUSY.load(Ordering::SeqCst), 1);
        }
        assert_eq!(REGIONS_BUSY.load(Ordering::SeqCst), 0);
    }
}
