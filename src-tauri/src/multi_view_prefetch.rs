// SPDX-License-Identifier: AGPL-3.0-or-later
//! Readies the multi view's other photographs for 100 %: once the cells are
//! shown and nothing else is going on, their RAWs are decoded one at a time
//! in the background and kept as 1:1 previews (preview_proxy, the session's
//! `.cache/previews`, as Lightroom builds 1:1 previews and Capture One its
//! preview files). A locked zoom then develops each cell's region from the
//! few bands of its 1:1 preview it shows (`multi_view::prepare_region`)
//! instead of decoding every RAW at once.
//!
//! The shown photographs are decoded first and their decodes held in
//! memory, within a budget, while their previews are written after them (a
//! 1:1 preview takes about as long to write as its RAW to decode): a zoom
//! cuts its regions from the decodes held, a full decode a zoom makes itself
//! is kept for its preview, and a cell made the primary opens on its held
//! decode.
//!
//! Bounds: one decode at a time (a 61 MP decode alone takes well over a
//! gigabyte while it runs), on utility-priority threads (a few for the
//! writes); decodes held up to an eighth of the physical memory, the oldest
//! whose previews are written let go first for the next decode, all of them
//! while the system is short of memory or once their photographs are no
//! longer shown; nothing starts while the primary is being decoded or
//! developed, a zoom is being developed, Edit all selected follows a drag or
//! the system's memory pressure is critical, a preview being written waits
//! meanwhile, and an edit of the primary or its decode cancels the decode
//! under way (it is done again later). Photographs with a current 1:1
//! preview and other files than RAWs are skipped.

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
use crate::image_loader::{DecodeMeasures, DecodeVariant};
use crate::preview_proxy;
use image::DynamicImage;

/// Threads of the decode under way (of the rayon work inside it): every
/// core, at utility priority, so that Develop's own work (the primary's
/// decode and frames, at higher priority) runs first. 2026-10-23, Mac mini
/// M4 Pro, 12-up of 20–61 MP RAWs, locked 100 % 8 s after entering: 2.9–3.1 s
/// with 4 threads, 2.5 s with 12.
fn decode_threads() -> usize {
    std::thread::available_parallelism().map_or(WRITE_THREADS, |n| n.get())
}

/// Threads writing a 1:1 preview: few, as its bands under way cannot wait
/// for a step to another photograph (whose decode they would slow).
const WRITE_THREADS: usize = 4;

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
    /// The photographs planned last, nearest the primary first (with those
    /// held): how far each is from the primary.
    order: Vec<String>,
}

impl Queue {
    /// How far `path` is from the primary: its place in the plan (0: the
    /// primary itself, or nearest), past the end when not planned.
    fn rank(&self, path: &str) -> usize {
        let source = source_of(path);
        self.order
            .iter()
            .position(|p| source_of(p) == source)
            .map_or(usize::MAX, |at| at + 1)
    }
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

/// A decode kept in memory for its 1:1 preview (the Standard preview too,
/// when missing): the cell's path, the decode's cache key, the decode, its
/// measures and decode variant.
struct HeldDecode {
    path: String,
    key: String,
    image: Arc<DynamicImage>,
    measures: Option<DecodeMeasures>,
    variant: DecodeVariant,
    /// Its previews are written: kept while it fits, let go first.
    written: bool,
    /// How far its photograph is from the primary (`Queue::rank`): the
    /// farthest written decode is let go first.
    rank: usize,
    /// The [`decodes_generation`] it was made in.
    generation: usize,
    /// The settings it was decoded with: its previews are labelled with
    /// them, and written only while the decoder's settings are still those
    /// (`proxy_key`, what its previews would carry).
    settings: crate::app_settings::AppSettings,
    proxy_key: Option<preview_proxy::ProxyKey>,
}

/// What a held decode's previews are written from (a copy of its entry).
struct Written {
    path: String,
    image: Arc<DynamicImage>,
    measures: Option<DecodeMeasures>,
    variant: DecodeVariant,
    generation: usize,
    settings: crate::app_settings::AppSettings,
    proxy_key: Option<preview_proxy::ProxyKey>,
}

/// The decodes held for their 1:1 previews, oldest first. The shown
/// photographs are decoded first, while they fit [`hold_budget`], and their
/// previews written after; written ones stay while they fit, until room is
/// needed for the next: a zoom cuts its regions from the decodes in memory
/// (`held_decode`) and a cell made the primary opens on one instead of
/// decoding its RAW again (a 1:1 preview takes about as long to write as
/// its RAW to decode, 2026-10-23: 0.3–0.9 s against 0.3–0.5 s for 24–33 MP).
static HELD: Mutex<VecDeque<HeldDecode>> = Mutex::new(VecDeque::new());
/// Notified when a decode is held or the decode under way ends.
static HELD_CHANGED: Condvar = Condvar::new();
/// Raised (under the [`HELD`] lock) when the decoder's settings change: a
/// decode made before is neither held nor written.
static DECODES_GENERATION: AtomicUsize = AtomicUsize::new(0);

/// The generation of decodes made now (`forget_decodes`).
pub(crate) fn decodes_generation() -> usize {
    DECODES_GENERATION.load(Ordering::SeqCst)
}

/// The decoder's settings changed (`cache_utils::clear_image_caches`):
/// the decodes held, the one under way and those still to be written are
/// of the old settings; let go, cancelled and decoded again.
pub(crate) fn forget_decodes() {
    let Some(worker) = WORKER.get() else {
        DECODES_GENERATION.fetch_add(1, Ordering::SeqCst);
        return;
    };
    let mut queue = worker.queue.lock().unwrap_or_else(|e| e.into_inner());
    let mut held = HELD.lock().unwrap_or_else(|e| e.into_inner());
    DECODES_GENERATION.fetch_add(1, Ordering::SeqCst);
    // Their previews are of the old settings too: all made again.
    for path in held
        .drain(..)
        .map(|h| h.path)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
    {
        if !queue.todo.contains(&path) {
            queue.todo.push_front(path);
        }
    }
    drop(held);
    queue.generation += 1;
    worker.cancel.fetch_add(1, Ordering::SeqCst);
    worker.wake.notify_one();
    drop(queue);
    rawtonic_runtime::allocator::release_freed_memory();
}

/// The decoded cache key of the decode under way, if any.
static DECODING: Mutex<Option<String>> = Mutex::new(None);

/// Marks the decode of `key` under way until dropped.
struct Decoding;

impl Decoding {
    fn start(key: &str) -> Self {
        *DECODING.lock().unwrap_or_else(|e| e.into_inner()) = Some(key.to_string());
        Decoding
    }
}

impl Drop for Decoding {
    fn drop(&mut self) {
        *DECODING.lock().unwrap_or_else(|e| e.into_inner()) = None;
        // Under the lock the waiters check with, so none misses it.
        let _held = HELD.lock().unwrap_or_else(|e| e.into_inner());
        HELD_CHANGED.notify_all();
    }
}

fn decoding(key: &str) -> bool {
    DECODING
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_deref()
        == Some(key)
}

fn held_bytes(held: &VecDeque<HeldDecode>) -> usize {
    held.iter().map(|h| h.image.as_bytes().len()).sum()
}

/// Bytes of decodes held at most: an eighth of the physical memory (3 GB
/// of 24 GB: five to ten 24–61 MP decodes), none while the system is short
/// of memory (each preview is then written right after its decode).
fn hold_budget() -> usize {
    #[cfg(feature = "performance-profiling")]
    if let Some(mb) = std::env::var("RAWTONIC_MV_HOLD_MB")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
    {
        return mb << 20;
    }
    if crate::memory_pressure::level_now().is_some() {
        return 0;
    }
    static TOTAL: OnceLock<u64> = OnceLock::new();
    let total = *TOTAL.get_or_init(|| {
        let mut system = sysinfo::System::new();
        system.refresh_memory();
        system.total_memory()
    });
    (total / 8) as usize
}

/// The decode with this decoded cache key held for its 1:1 preview, if
/// any, with its measures.
pub(crate) fn held_decode(key: &str) -> Option<(Arc<DynamicImage>, Option<DecodeMeasures>)> {
    find_held(&HELD.lock().unwrap_or_else(|e| e.into_inner()), key)
}

fn find_held(
    held: &VecDeque<HeldDecode>,
    key: &str,
) -> Option<(Arc<DynamicImage>, Option<DecodeMeasures>)> {
    held.iter()
        .find(|held| held.key == key)
        .map(|held| (Arc::clone(&held.image), held.measures))
}

/// [`held_decode`], waiting up to `wait` for the decode of `key` under way
/// to end (a cell made the primary while it is decoded: its decode is
/// finished rather than started again).
pub(crate) fn held_or_pending_decode(
    key: &str,
    wait: Duration,
) -> Option<(Arc<DynamicImage>, Option<DecodeMeasures>)> {
    let until = Instant::now() + wait;
    let mut held = HELD.lock().unwrap_or_else(|e| e.into_inner());
    loop {
        if let Some(found) = find_held(&held, key) {
            return Some(found);
        }
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() || !decoding(key) {
            return None;
        }
        held = HELD_CHANGED
            .wait_timeout(held, left)
            .unwrap_or_else(|e| e.into_inner())
            .0;
    }
}

/// A full decode a zoom made of `path` (decoded cache key `key`): kept for
/// the 1:1 preview still to be made of it, so the RAW is not decoded again,
/// when it is of the decode variant the previews stand for and fits the
/// budget beside the decodes held. Whether it was kept.
pub(crate) fn adopt(
    path: &str,
    key: &str,
    image: &Arc<DynamicImage>,
    measures: Option<DecodeMeasures>,
    generation: usize,
    settings: &crate::app_settings::AppSettings,
) -> bool {
    let Some(worker) = WORKER.get() else {
        return false;
    };
    let source = source_of(path);
    let (source_path, _) = parse_virtual_path(path);
    let source_str = source_path.to_string_lossy();
    let variant = crate::image_loader::stored_decode_variant(&source_str);
    if crate::image_loader::decoded_cache_key(&source_str, None, variant) != key {
        return false;
    }
    let mut queue = worker.queue.lock().unwrap_or_else(|e| e.into_inner());
    let Some(at) = queue.todo.iter().position(|p| source_of(p) == source) else {
        return false;
    };
    // The decode under way is of the first photograph planned.
    if at == 0 && worker.decoding.load(Ordering::SeqCst) {
        return false;
    }
    let mut held = HELD.lock().unwrap_or_else(|e| e.into_inner());
    if held.iter().any(|h| h.key == key) || decodes_generation() != generation {
        return false;
    }
    let budget = hold_budget();
    let rank = queue.rank(path);
    make_room(&mut held, image.as_bytes().len(), budget, Some(rank));
    if held_bytes(&held) + image.as_bytes().len() > budget {
        return false;
    }
    let path = queue.todo.remove(at).expect("found");
    held.push_back(HeldDecode {
        path,
        key: key.to_string(),
        image: Arc::clone(image),
        measures,
        variant,
        written: false,
        rank,
        generation,
        settings: settings.clone(),
        proxy_key: preview_proxy::current_key(&source_path, settings, variant),
    });
    worker.wake.notify_one();
    true
}

/// Before each band of a 1:1 preview being written: waits while zoomed
/// regions are developed, the primary is decoded or developed, or Edit all
/// selected follows an edit, and for [`QUIET`] after an edit (they need the
/// cores; the write goes on after).
fn yield_to_editing() {
    while busy() || since_activity() < QUIET {
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Milliseconds since `EPOCH` of the last interactive preview frame or zoom
/// work; 0: none yet.
static LAST_ACTIVITY: AtomicU64 = AtomicU64::new(0);
/// Zoomed regions being developed.
static REGIONS_BUSY: AtomicUsize = AtomicUsize::new(0);
/// Full decodes of the primary under way.
static PRIMARY_DECODES: AtomicUsize = AtomicUsize::new(0);
/// Frames of the primary being developed.
static PREVIEW_FRAMES: AtomicUsize = AtomicUsize::new(0);

fn epoch() -> Instant {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    *EPOCH.get_or_init(Instant::now)
}

fn now_ms() -> u64 {
    epoch().elapsed().as_millis() as u64 + 1
}

pub(crate) fn since_activity() -> Duration {
    let last = LAST_ACTIVITY.load(Ordering::Relaxed);
    if last == 0 {
        return Duration::MAX;
    }
    Duration::from_millis(now_ms().saturating_sub(last))
}

/// The preview worker develops a frame of the primary while the guard
/// lives: nothing starts and a 1:1 preview being written waits. An
/// interactive frame (a slider or brush drag) also cancels the decode under
/// way and holds the next step back for [`QUIET`] after it.
pub(crate) struct PreviewFrame {
    interactive: bool,
}

impl PreviewFrame {
    pub(crate) fn start(interactive: bool) -> Self {
        PREVIEW_FRAMES.fetch_add(1, Ordering::SeqCst);
        if interactive {
            LAST_ACTIVITY.store(now_ms(), Ordering::Relaxed);
            if let Some(worker) = WORKER.get()
                && worker.decoding.load(Ordering::Relaxed)
            {
                worker.cancel.fetch_add(1, Ordering::SeqCst);
            }
        }
        PreviewFrame { interactive }
    }
}

impl Drop for PreviewFrame {
    fn drop(&mut self) {
        if self.interactive {
            LAST_ACTIVITY.store(now_ms(), Ordering::Relaxed);
        }
        PREVIEW_FRAMES.fetch_sub(1, Ordering::SeqCst);
        if let Some(worker) = WORKER.get() {
            worker.wake.notify_one();
        }
    }
}

/// Zoomed regions (or the cells) are being developed while the guard
/// lives: the decode under way is cancelled and none starts (they need the
/// cores and memory); after regions nothing starts for [`QUIET`].
pub(crate) struct RegionsBusy {
    quiet_after: bool,
}

impl RegionsBusy {
    pub(crate) fn start() -> Self {
        Self::new(true)
    }

    /// For the cells' own development (entering the multi view, a cell
    /// added): the background work goes on right after it.
    pub(crate) fn cells() -> Self {
        Self::new(false)
    }

    fn new(quiet_after: bool) -> Self {
        REGIONS_BUSY.fetch_add(1, Ordering::SeqCst);
        if let Some(worker) = WORKER.get()
            && worker.decoding.load(Ordering::Relaxed)
        {
            worker.cancel.fetch_add(1, Ordering::SeqCst);
        }
        RegionsBusy { quiet_after }
    }
}

impl Drop for RegionsBusy {
    fn drop(&mut self) {
        if self.quiet_after {
            LAST_ACTIVITY.store(now_ms(), Ordering::Relaxed);
        }
        REGIONS_BUSY.fetch_sub(1, Ordering::SeqCst);
        if let Some(worker) = WORKER.get() {
            worker.wake.notify_one();
        }
    }
}

/// The primary's full decode runs while the guard lives (a photograph
/// opened in Develop, the next one stepped to): the decode under way is
/// cancelled and none starts, as for a zoom.
pub(crate) struct PrimaryDecode;

impl PrimaryDecode {
    pub(crate) fn start() -> Self {
        PRIMARY_DECODES.fetch_add(1, Ordering::SeqCst);
        if let Some(worker) = WORKER.get()
            && worker.decoding.load(Ordering::Relaxed)
        {
            worker.cancel.fetch_add(1, Ordering::SeqCst);
        }
        PrimaryDecode
    }
}

impl Drop for PrimaryDecode {
    fn drop(&mut self) {
        LAST_ACTIVITY.store(now_ms(), Ordering::Relaxed);
        PRIMARY_DECODES.fetch_sub(1, Ordering::SeqCst);
        if let Some(worker) = WORKER.get() {
            worker.wake.notify_one();
        }
    }
}

/// Zoomed regions, the primary's decode or frame want the cores.
fn busy() -> bool {
    REGIONS_BUSY.load(Ordering::SeqCst) > 0
        || PRIMARY_DECODES.load(Ordering::SeqCst) > 0
        || PREVIEW_FRAMES.load(Ordering::SeqCst) > 0
        || crate::multi_view::live_pending()
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
pub(crate) fn plan(app_handle: &tauri::AppHandle, paths: Vec<String>, primary: Option<&str>) {
    // Profiling builds can switch it off for paired runs.
    #[cfg(feature = "performance-profiling")]
    if std::env::var_os("RAWTONIC_NO_ZOOM_PREFETCH").is_some() {
        return;
    }
    let worker = worker(app_handle);
    let mut queue = worker.queue.lock().unwrap_or_else(|e| e.into_inner());
    // Decodes held for photographs no longer shown are let go (the
    // primary's too, once its cell is gone; all when the multi view ends).
    let shown: std::collections::HashSet<String> = paths
        .iter()
        .map(|p| source_of(p))
        .chain(primary.map(source_of))
        .collect();
    let mut held = HELD.lock().unwrap_or_else(|e| e.into_inner());
    let before = held.len();
    held.retain(|h| shown.contains(&source_of(&h.path)));
    let held_paths: Vec<String> = held.iter().map(|h| h.path.clone()).collect();
    let released = held.len() < before;
    drop(held);
    let decoding = worker
        .decoding
        .load(Ordering::SeqCst)
        .then(|| queue.todo.front().cloned())
        .flatten();
    let order: Vec<String> = primary
        .into_iter()
        .map(str::to_string)
        .chain(paths.iter().cloned())
        .collect();
    let (todo, cancel) = replanned(paths, &held_paths, decoding.as_deref(), primary);
    // The same plan again (the multi view plans after every change of its
    // cells): nothing changes.
    if todo != queue.todo || order != queue.order {
        queue.generation += 1;
        queue.order = order;
        queue.todo = todo;
        for held in HELD.lock().unwrap_or_else(|e| e.into_inner()).iter_mut() {
            held.rank = queue.rank(&held.path);
        }
        if cancel {
            worker.cancel.fetch_add(1, Ordering::SeqCst);
        }
        worker.wake.notify_one();
    }
    drop(queue);
    if released {
        rawtonic_runtime::allocator::release_freed_memory();
    }
}

/// The photographs still to decode for the plan `paths`: those whose
/// decodes are not held (`held`), with the one being decoded
/// (`decoding`) first while it is still planned (the multi view plans
/// again after every change of its cells); and whether the decode under
/// way is to be cancelled: its photograph is no longer shown (a cell
/// made the `primary` opens on it, `held_or_pending_decode`).
fn replanned(
    paths: Vec<String>,
    held: &[String],
    decoding: Option<&str>,
    primary: Option<&str>,
) -> (VecDeque<String>, bool) {
    let mut todo: VecDeque<String> = paths
        .into_iter()
        .filter(|p| !held.iter().any(|h| source_of(h) == source_of(p)))
        .collect();
    let Some(decoding) = decoding else {
        return (todo, false);
    };
    match todo.iter().position(|p| p == decoding) {
        Some(at) => {
            let path = todo.remove(at).expect("found");
            todo.push_front(path);
            (todo, false)
        }
        None => {
            let primary = primary.is_some_and(|p| source_of(p) == source_of(decoding));
            (todo, !primary)
        }
    }
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

/// What the worker does next: decode a photograph planned (with the
/// queue's generation then), or write the 1:1 preview of the oldest decode
/// held.
enum Step {
    Decode(String, usize),
    Write,
}

/// Whether the oldest held decode still to be written gets its previews
/// next rather than the next photograph decoded: with nothing left to
/// decode, or when the next decode (`next` bytes, 0 when not known: one
/// more) would not fit `budget` beside the `held` bytes. Without decodes to
/// write the next is decoded (a first one always).
fn write_next(to_write: bool, held: usize, todo: bool, next: usize, budget: usize) -> bool {
    to_write && (!todo || held + next.max(1) > budget)
}

/// Lets written decodes go, the farthest from the primary first (only
/// those farther than `rank`, the next decode's, when given), until `next`
/// more bytes (0: one more) fit `budget` beside the rest.
fn make_room(
    held: &mut VecDeque<HeldDecode>,
    next: usize,
    budget: usize,
    rank: Option<usize>,
) -> bool {
    let before = held.len();
    while held_bytes(held) + next.max(1) > budget
        && let Some(at) = held
            .iter()
            .enumerate()
            .filter(|(_, h)| h.written && rank.is_none_or(|rank| h.rank > rank))
            .max_by_key(|(_, h)| h.rank)
            .map(|(at, _)| at)
    {
        held.remove(at);
    }
    held.len() < before
}

/// Holds `decoded`, a decode just made, unless its photograph is no longer
/// planned (the multi view ended or moved on meanwhile) or the decoder's
/// settings changed since it began; with its distance from the primary.
/// Under the queue's lock, so that neither can change in between.
fn accept(queue: &Queue, held: &mut VecDeque<HeldDecode>, mut decoded: HeldDecode) -> bool {
    let rank = queue.rank(&decoded.path);
    if rank == usize::MAX
        || decodes_generation() != decoded.generation
        || held.iter().any(|h| h.key == decoded.key)
    {
        return false;
    }
    decoded.rank = rank;
    held.push_back(decoded);
    true
}

/// Lets every held decode go, those still to be written back to the front
/// of `todo` (decoded again later).
fn let_all_go(held: &mut VecDeque<HeldDecode>, todo: &mut VecDeque<String>) -> bool {
    let unwritten: Vec<String> = held
        .iter()
        .filter(|h| !h.written && !todo.contains(&h.path))
        .map(|h| h.path.clone())
        .collect();
    for path in unwritten.into_iter().rev() {
        todo.push_front(path);
    }
    let any = !held.is_empty();
    held.clear();
    any
}

/// The next step: decodes while they fit the budget (letting written
/// decodes go for them), else (or with nothing left to decode) writes the
/// oldest held decode's previews; None with nothing to do. Short of memory
/// (no budget) the written decodes are let go; critically short, all (and
/// nothing starts, `may_start`).
fn next_step(queue: &mut Queue) -> Option<Step> {
    let mut held = HELD.lock().unwrap_or_else(|e| e.into_inner());
    if crate::memory_pressure::level_now() == Some(crate::memory_pressure::Level::Critical) {
        let freed = let_all_go(&mut held, &mut queue.todo);
        drop(held);
        if freed {
            rawtonic_runtime::allocator::release_freed_memory();
        }
        return queue
            .todo
            .front()
            .map(|path| Step::Decode(path.clone(), queue.generation));
    }
    let next = queue
        .todo
        .front()
        .and_then(|path| crate::multi_view::full_size_of(path))
        .map_or(0, |(w, h)| w as usize * h as usize * 3 * size_of::<f32>());
    let budget = hold_budget();
    let freed = match queue.todo.front() {
        // Kept while they fit (looked at again now and then: none under
        // memory pressure).
        None => make_room(&mut held, 0, budget, None),
        // Nearer the primary than the next: kept (it is written after its
        // decode then, and let go if it does not fit).
        Some(front) => make_room(&mut held, next, budget, Some(queue.rank(front))),
    };
    let to_write = held.iter().any(|h| !h.written);
    let step = if write_next(
        to_write,
        held_bytes(&held),
        !queue.todo.is_empty(),
        next,
        budget,
    ) {
        Some(Step::Write)
    } else {
        queue
            .todo
            .front()
            .map(|path| Step::Decode(path.clone(), queue.generation))
    };
    drop(held);
    if freed {
        rawtonic_runtime::allocator::release_freed_memory();
    }
    step
}

fn run(worker: &Worker, app_handle: &tauri::AppHandle) {
    lower_priority();
    let pool = |threads: usize, name: &'static str| {
        rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .thread_name(move |i| format!("multi-view-{name}-{i}"))
            .start_handler(|_| lower_priority())
            .build()
            .expect("multi view prefetch thread pool")
    };
    let decode_pool = pool(decode_threads(), "decode");
    let write_pool = pool(WRITE_THREADS, "write");
    let mut waiting_for = "";
    loop {
        // The next step, once the app is quiet.
        let step = {
            let mut queue = worker.queue.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                if let Some(step) = next_step(&mut queue) {
                    let quiet = since_activity();
                    let busy = busy();
                    // Critical only: a Mac with Capture One and Lightroom
                    // open beside it sat at the warning level for minutes.
                    let short_of_memory = crate::memory_pressure::level_now()
                        == Some(crate::memory_pressure::Level::Critical);
                    if may_start(quiet, busy, short_of_memory) {
                        waiting_for = "";
                        break step;
                    }
                    let reason = if short_of_memory {
                        "memory pressure"
                    } else if busy {
                        "zoom, Edit all selected or the primary's decode"
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
                // Decodes held: looked at again now and then, so they are
                // let go under memory pressure.
                let holding = !HELD.lock().unwrap_or_else(|e| e.into_inner()).is_empty();
                queue = if holding {
                    worker
                        .wake
                        .wait_timeout(queue, Duration::from_secs(5))
                        .unwrap_or_else(|e| e.into_inner())
                        .0
                } else {
                    worker.wake.wait(queue).unwrap_or_else(|e| e.into_inner())
                };
            }
        };
        match step {
            Step::Decode(path, generation) => {
                let token = worker.cancel.load(Ordering::SeqCst);
                worker.decoding.store(true, Ordering::SeqCst);
                let outcome = decode_pool
                    .install(|| decode(app_handle, &path, (Arc::clone(&worker.cancel), token)));
                worker.decoding.store(false, Ordering::SeqCst);
                let mut queue = worker.queue.lock().unwrap_or_else(|e| e.into_inner());
                let (decoded, under_way, decoded_ms) = match outcome {
                    Outcome::Cancelled => {
                        // An edit, a zoom: the same photograph again later.
                        drop(queue);
                        rawtonic_runtime::allocator::release_freed_memory();
                        continue;
                    }
                    Outcome::Skipped => (None, None, 0.0),
                    Outcome::Decoded(decoded, under_way, ms) => {
                        (Some(decoded), Some(under_way), ms)
                    }
                };
                // Planned again meanwhile: a decode held stays while its
                // photograph is shown (plan lets the others go).
                if queue.generation == generation && queue.todo.front() == Some(&path) {
                    queue.todo.pop_front();
                }
                let Some(decoded) = decoded else {
                    continue;
                };
                let image = Arc::clone(&decoded.image);
                let (accepted, count, bytes) = {
                    let mut held = HELD.lock().unwrap_or_else(|e| e.into_inner());
                    let accepted = accept(&queue, &mut held, *decoded);
                    (accepted, held.len(), held_bytes(&held))
                };
                drop(queue);
                // Waiters (a cell made the primary) find it held now.
                drop(under_way);
                if !accepted {
                    drop(image);
                    rawtonic_runtime::allocator::release_freed_memory();
                    continue;
                }
                // What a zoom's region from this decode needs of the whole
                // photograph.
                let measuring = Instant::now();
                decode_pool.install(|| {
                    crate::multi_view::measure_decode(
                        &path,
                        &crate::multi_view::stored_recipe(&path),
                        &image,
                    )
                });
                log::info!(
                    "[multi_view] {path}: decoded for its 1:1 preview in {decoded_ms:.0} ms, measured {:.0} ms ({count} held, {:.0} MB)",
                    measuring.elapsed().as_secs_f64() * 1e3,
                    bytes as f64 / 1e6
                );
            }
            Step::Write => {
                let oldest = HELD
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .iter()
                    .find(|h| !h.written)
                    .map(|h| {
                        (
                            h.key.clone(),
                            Written {
                                path: h.path.clone(),
                                image: Arc::clone(&h.image),
                                measures: h.measures,
                                variant: h.variant,
                                generation: h.generation,
                                settings: h.settings.clone(),
                                proxy_key: h.proxy_key.clone(),
                            },
                        )
                    });
                let Some((key, oldest)) = oldest else {
                    continue;
                };
                write_pool.install(|| write(app_handle, &oldest));
                let Written { path, image, .. } = oldest;
                let trimmed = {
                    let mut held = HELD.lock().unwrap_or_else(|e| e.into_inner());
                    if let Some(written) = held.iter_mut().find(|h| h.key == key) {
                        written.written = true;
                    }
                    // Back within the budget: a decode that did not fit
                    // beside nearer ones goes now.
                    make_room(&mut held, 0, hold_budget(), None)
                };
                drop(image);
                if trimmed {
                    rawtonic_runtime::allocator::release_freed_memory();
                }
                // A zoom warps regions from the 1:1 preview's rows with what
                // only the whole preview tells (lateral CA estimate, fringe
                // radius).
                write_pool.install(|| {
                    let settings = load_settings(app_handle.clone()).unwrap_or_default();
                    crate::multi_view::measure_preview(
                        &path,
                        &crate::multi_view::stored_recipe(&path),
                        &settings,
                    )
                });
            }
        }
    }
}

enum Outcome {
    /// Decoded (in this many ms), to be held for its previews; the decode
    /// counts as under way until it is (`held_or_pending_decode`).
    Decoded(Box<HeldDecode>, Decoding, f64),
    Skipped,
    Cancelled,
}

/// Whether the Standard preview of `source` is missing (or smaller than
/// the fit view now asks), and its size.
fn standard_needed(
    app_handle: &tauri::AppHandle,
    source: &std::path::Path,
    settings: &crate::app_settings::AppSettings,
) -> (bool, u32) {
    let state = app_handle.state::<AppState>();
    let standard =
        preview_proxy::standard_size(state.last_preview_dim.load(Ordering::Relaxed), settings);
    let needed =
        preview_proxy::stored_size(source, settings).is_none_or(|stored| stored < standard);
    (needed, standard)
}

/// Decodes `path` and holds the decode for its previews, unless it has a
/// current 1:1 preview (and Standard preview).
fn decode(app_handle: &tauri::AppHandle, path: &str, cancel: (Arc<AtomicUsize>, usize)) -> Outcome {
    let (source, _): (PathBuf, _) = parse_virtual_path(path);
    if !is_raw_file(&source)
        || preview_proxy::proxy_path_of(&source, preview_proxy::ProxyKind::Full).is_none()
        || !preview_proxy::source_available(&source)
    {
        return Outcome::Skipped;
    }
    // Before the settings are read: a change after makes this decode stale.
    let generation = decodes_generation();
    let settings = load_settings(app_handle.clone()).unwrap_or_default();
    // The Standard preview too, when missing (cheap from the decode): the
    // cell is then developed from it when the multi view is shown again,
    // without a decode.
    let (needs_standard, _) = standard_needed(app_handle, &source, &settings);
    if preview_proxy::has_full(&source, &settings) && !needs_standard {
        // A zoom warps regions from the 1:1 preview's rows with what only
        // the whole preview tells (lateral CA estimate, fringe radius).
        crate::multi_view::measure_preview(
            path,
            &crate::multi_view::stored_recipe(path),
            &settings,
        );
        return Outcome::Skipped;
    }
    let source_str = source.to_string_lossy().to_string();
    // For the decode variant of the stored recipe, as the previews stand
    // for it (preview_proxy::header_is_current).
    let variant = crate::image_loader::stored_decode_variant(&source_str);
    let key = crate::image_loader::decoded_cache_key(&source_str, None, variant);
    if held_decode(&key).is_some() {
        return Outcome::Skipped;
    }
    let start = Instant::now();
    let under_way = Decoding::start(&key);
    let Ok(bytes) = read_file_mapped(&source) else {
        return Outcome::Skipped;
    };
    let (tracker, token) = (&cancel.0, cancel.1);
    // Decoded here, not taken from Develop's cache: a decode held carries
    // the settings it was made with.
    let (image, measures) = match crate::image_loader::load_base_image_measured_for(
        &bytes,
        &source_str,
        false,
        &settings,
        Some(cancel.clone()),
        None,
        None,
        Some(variant),
    ) {
        Ok((image, measures)) => (Arc::new(image), measures),
        Err(_) if tracker.load(Ordering::SeqCst) != token => return Outcome::Cancelled,
        Err(e) => {
            log::warn!("[multi_view] 1:1 preview of {path}: decode failed: {e}");
            return Outcome::Skipped;
        }
    };
    if cancel.0.load(Ordering::SeqCst) != cancel.1 {
        return Outcome::Cancelled;
    }
    let decoded_ms = start.elapsed().as_secs_f64() * 1e3;
    let proxy_key = preview_proxy::current_key(&source, &settings, variant);
    Outcome::Decoded(
        Box::new(HeldDecode {
            path: path.to_string(),
            key,
            image,
            measures,
            variant,
            written: false,
            rank: usize::MAX,
            generation,
            settings,
            proxy_key,
        }),
        under_way,
        decoded_ms,
    )
}

/// Whether previews of `held` labelled with its own settings are current
/// now (`now`: the key a preview made now would carry): not once the
/// decoder's settings changed since its decode, saved (`now` differs) or
/// applied (`forget_decodes`).
fn labels_hold(held: &Written, now: Option<&preview_proxy::ProxyKey>) -> bool {
    decodes_generation() == held.generation && now == held.proxy_key.as_ref()
}

/// Writes the previews of a held decode: the Standard preview when
/// missing and the 1:1 preview, labelled with the settings it was decoded
/// with, waiting while a zoom develops its regions. None while the
/// decoder's settings differ from those (changed since its decode).
fn write(app_handle: &tauri::AppHandle, held: &Written) {
    let path = held.path.as_str();
    let (source, _): (PathBuf, _) = parse_virtual_path(path);
    let settings = &held.settings;
    let now = load_settings(app_handle.clone()).unwrap_or_default();
    if !labels_hold(
        held,
        preview_proxy::current_key(&source, &now, held.variant).as_ref(),
    ) {
        return;
    }
    let start = Instant::now();
    let (needs_standard, standard) = standard_needed(app_handle, &source, settings);
    if needs_standard {
        match preview_proxy::write(
            &source,
            &held.image,
            standard,
            settings,
            held.variant,
            held.measures,
        ) {
            // The thumbnail is then developed from it (as proxy_worker does).
            Ok(_) => crate::file_management::refresh_thumbnail(app_handle, path),
            Err(e) => log::warn!("[multi_view] Standard preview of {path} not written: {e}"),
        }
    }
    if preview_proxy::has_full(&source, settings) {
        return;
    }
    match preview_proxy::write_full_with(
        &source,
        &held.image,
        settings,
        held.variant,
        held.measures,
        &yield_to_editing,
    ) {
        Ok(()) => log::info!(
            "[multi_view] 1:1 preview of {path} written in the background in {:.0} ms",
            start.elapsed().as_secs_f64() * 1e3
        ),
        Err(e) => log::warn!("[multi_view] 1:1 preview of {path} not written: {e}"),
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
    fn planning_again_keeps_the_decode_under_way_and_skips_those_held() {
        let planned = paths(&["b", "c", "d", "e"]);
        // Nothing under way: the plan without the photographs held.
        let (todo, cancel) = replanned(planned.clone(), &paths(&["c"]), None, None);
        assert_eq!(Vec::from(todo), paths(&["b", "d", "e"]));
        assert!(!cancel);
        // The one being decoded goes on, first.
        let (todo, cancel) = replanned(planned.clone(), &paths(&["c"]), Some("d"), None);
        assert_eq!(Vec::from(todo), paths(&["d", "b", "e"]));
        assert!(!cancel);
        // No longer shown: cancelled.
        let (todo, cancel) = replanned(planned.clone(), &[], Some("x"), Some("b"));
        assert_eq!(Vec::from(todo), paths(&["b", "c", "d", "e"]));
        assert!(cancel);
        // Made the primary: it goes on (the primary opens on it).
        let (todo, cancel) = replanned(planned, &[], Some("x"), Some("x?vc=2"));
        assert_eq!(Vec::from(todo), paths(&["b", "c", "d", "e"]));
        assert!(!cancel);
        // A held virtual copy stands for its RAW.
        let (todo, _) = replanned(
            paths(&["a.raf", "b.raf"]),
            &paths(&["a.raf?vc=1"]),
            None,
            None,
        );
        assert_eq!(Vec::from(todo), paths(&["b.raf"]));
    }

    /// The tests that use the module's statics (the worker, the decodes
    /// held) run one at a time.
    static STATICS: Mutex<()> = Mutex::new(());

    /// The static worker, without its thread (shared by these tests only).
    fn test_worker() -> Arc<Worker> {
        WORKER
            .get_or_init(|| {
                Arc::new(Worker {
                    queue: Mutex::new(Queue::default()),
                    wake: Condvar::new(),
                    cancel: Arc::new(AtomicUsize::new(0)),
                    decoding: AtomicBool::new(true),
                })
            })
            .clone()
    }

    #[test]
    fn a_change_of_the_decoders_settings_forgets_the_decodes() {
        let _statics = STATICS.lock().unwrap_or_else(|e| e.into_inner());
        let worker = test_worker();
        let (generation, cancel) = (decodes_generation(), worker.cancel.load(Ordering::SeqCst));
        HELD.lock()
            .unwrap()
            .extend([held("x.raf", 1, true, 1), held("y.raf", 1, false, 2)]);
        forget_decodes();
        // Neither is held; both are made again (their previews are of the
        // old settings); the decode under way is cancelled; a decode begun
        // before is no longer held (`decode`, `adopt`) or written (`write`).
        assert!(HELD.lock().unwrap().is_empty());
        let mut queue = worker.queue.lock().unwrap();
        assert_eq!(
            &queue.todo.iter().take(2).cloned().collect::<Vec<_>>(),
            &paths(&["x.raf", "y.raf"])
        );
        queue.todo.retain(|p| p != "x.raf" && p != "y.raf");
        assert_eq!(worker.cancel.load(Ordering::SeqCst), cancel + 1);
        assert_eq!(decodes_generation(), generation + 1);
    }

    #[test]
    fn a_cell_made_the_primary_opens_on_its_decode_under_way() {
        let _statics = STATICS.lock().unwrap_or_else(|e| e.into_inner());
        let key = "a cell made the primary";
        let image = Arc::new(DynamicImage::new_rgb32f(2, 2));
        // Nothing under way and nothing held: none, without waiting.
        let start = Instant::now();
        assert!(held_or_pending_decode(key, Duration::from_secs(5)).is_none());
        assert!(start.elapsed() < Duration::from_secs(1));
        // Under way: the decode once it is held, not a decode of its own.
        let under_way = Decoding::start(key);
        let decoded = Arc::clone(&image);
        let worker = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            HELD.lock().unwrap().push_back(HeldDecode {
                path: "a.raf".into(),
                key: key.into(),
                image: decoded,
                measures: None,
                variant: DecodeVariant::default(),
                written: false,
                rank: 1,
                generation: decodes_generation(),
                settings: Default::default(),
                proxy_key: None,
            });
            drop(under_way);
        });
        let (found, _) = held_or_pending_decode(key, Duration::from_secs(5)).expect("held");
        assert!(Arc::ptr_eq(&found, &image));
        worker.join().unwrap();
        HELD.lock().unwrap().retain(|h| h.key != key);
        // A decode that ends without being held (cancelled): none.
        let under_way = Decoding::start(key);
        let worker = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            drop(under_way);
        });
        assert!(held_or_pending_decode(key, Duration::from_secs(5)).is_none());
        worker.join().unwrap();
    }

    #[test]
    fn decodes_are_held_within_the_budget_and_written_after() {
        const MB: usize = 1 << 20;
        // Nothing held: decode (even one larger than the budget).
        assert!(!write_next(false, 0, true, 800 * MB, 500 * MB));
        assert!(!write_next(false, 0, false, 0, 500 * MB));
        // The next fits beside those held: decode it first.
        assert!(!write_next(true, 300 * MB, true, 200 * MB, 500 * MB));
        // It does not: write the oldest first.
        assert!(write_next(true, 300 * MB, true, 201 * MB, 500 * MB));
        // Size unknown: one more while under the budget.
        assert!(!write_next(true, 499 * MB, true, 0, 500 * MB));
        assert!(write_next(true, 500 * MB, true, 0, 500 * MB));
        // Nothing left to decode: write.
        assert!(write_next(true, MB, false, 0, 500 * MB));
        // No budget (short of memory): each written right after its decode.
        assert!(write_next(true, MB, true, 0, 0));
        assert!(!write_next(false, 0, true, MB, 0));
        // All held are written (and stay while they fit): decode.
        assert!(!write_next(false, 400 * MB, true, 200 * MB, 500 * MB));
    }

    /// A held decode of `units` × 12 MiB (1024 × 1024 float RGB each),
    /// `rank` from the primary.
    fn held(key: &str, units: u32, written: bool, rank: usize) -> HeldDecode {
        HeldDecode {
            path: key.into(),
            key: key.into(),
            image: Arc::new(DynamicImage::new_rgb32f(1024, 1024 * units)),
            measures: None,
            variant: DecodeVariant::default(),
            written,
            rank,
            generation: 0,
            settings: Default::default(),
            proxy_key: None,
        }
    }

    #[test]
    fn a_decode_is_held_only_while_its_photograph_is_planned_and_current() {
        let _statics = STATICS.lock().unwrap_or_else(|e| e.into_inner());
        let queue = Queue {
            order: paths(&["p.raf", "a.raf", "b.raf"]),
            ..Queue::default()
        };
        let mut all = VecDeque::new();
        let current = |key: &str| HeldDecode {
            generation: decodes_generation(),
            ..held(key, 1, false, usize::MAX)
        };
        // Planned: held, with its distance from the primary.
        assert!(accept(&queue, &mut all, current("b.raf")));
        assert_eq!(all.back().unwrap().rank, 3);
        // Held already: not twice.
        assert!(!accept(&queue, &mut all, current("b.raf")));
        // The multi view moved on or ended meanwhile: let go.
        assert!(!accept(&queue, &mut all, current("x.raf")));
        assert!(!accept(&Queue::default(), &mut all, current("a.raf")));
        // Begun before the decoder's settings changed: let go.
        let stale = current("a.raf");
        forget_decodes();
        assert!(!accept(&queue, &mut all, stale));
        assert_eq!(all.len(), 1);
        test_worker().queue.lock().unwrap().todo.clear();
    }

    #[test]
    fn previews_of_a_decode_are_written_only_with_the_settings_it_was_made_with() {
        let _statics = STATICS.lock().unwrap_or_else(|e| e.into_inner());
        let key = |decoder: &str| preview_proxy::ProxyKey {
            file_size: 1,
            modified_ns: 2,
            decoder: decoder.into(),
            engine: 3,
        };
        let held = Written {
            path: "a.raf".into(),
            image: Arc::new(DynamicImage::new_rgb32f(1, 1)),
            measures: None,
            variant: DecodeVariant::default(),
            generation: decodes_generation(),
            settings: Default::default(),
            proxy_key: Some(key("old")),
        };
        assert!(labels_hold(&held, Some(&key("old"))));
        // New settings saved, the caches not cleared yet: not written.
        assert!(!labels_hold(&held, Some(&key("new"))));
        assert!(!labels_hold(&held, None));
        // Cleared (back to the old settings even): not written either.
        forget_decodes();
        assert!(!labels_hold(&held, Some(&key("old"))));
        test_worker().queue.lock().unwrap().todo.clear();
    }

    #[test]
    fn critically_short_of_memory_every_decode_goes_and_is_made_again() {
        let mut all: VecDeque<HeldDecode> = [
            held("a", 1, true, 1),
            held("b", 1, false, 2),
            held("c", 1, false, 3),
        ]
        .into();
        let mut todo: VecDeque<String> = paths(&["d", "c"]).into();
        assert!(let_all_go(&mut all, &mut todo));
        assert!(all.is_empty());
        // The written one has its previews; the others are made again, first.
        assert_eq!(Vec::from(todo.clone()), paths(&["b", "d", "c"]));
        assert!(!let_all_go(&mut all, &mut todo));
    }

    #[test]
    fn written_decodes_far_from_the_primary_make_room_first() {
        const UNIT: usize = 12 << 20;
        let keys = |h: &VecDeque<HeldDecode>| h.iter().map(|h| h.key.clone()).collect::<Vec<_>>();
        let mut all: VecDeque<HeldDecode> = [
            held("a", 1, true, 1),
            held("b", 1, false, 4),
            held("c", 1, true, 3),
            held("d", 1, true, 2),
        ]
        .into();
        assert_eq!(held_bytes(&all), 4 * UNIT);
        // Room for one more within 5: nothing goes.
        assert!(!make_room(&mut all, UNIT, 5 * UNIT, Some(5)));
        // Within 4 for one farther than all: nothing nearer goes for it.
        assert!(!make_room(&mut all, UNIT, 4 * UNIT, Some(5)));
        assert_eq!(all.len(), 4);
        // For one next to the primary: the farthest written goes, the one
        // to write stays.
        assert!(make_room(&mut all, UNIT, 4 * UNIT, Some(1)));
        assert_eq!(keys(&all), ["a", "b", "d"]);
        // For one at rank 2 only those farther may go: none is written.
        assert!(!make_room(&mut all, UNIT, 3 * UNIT, Some(2)));
        // Trimmed to 2: the farthest written goes.
        assert!(make_room(&mut all, 0, 3 * UNIT, None));
        assert_eq!(keys(&all), ["a", "b"]);
        // No budget: every written one goes.
        assert!(make_room(&mut all, 0, 0, None));
        assert_eq!(keys(&all), ["b"]);
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
        let _statics = STATICS.lock().unwrap_or_else(|e| e.into_inner());
        let worker = test_worker();
        let before = worker.cancel.load(Ordering::SeqCst);
        {
            let _frame = PreviewFrame::start(false);
            assert!(busy());
        }
        assert_eq!(worker.cancel.load(Ordering::SeqCst), before);
        drop(PreviewFrame::start(true));
        assert_eq!(worker.cancel.load(Ordering::SeqCst), before + 1);
        assert!(since_activity() < QUIET);
        {
            let _busy = RegionsBusy::start();
            assert_eq!(worker.cancel.load(Ordering::SeqCst), before + 2);
            assert_eq!(REGIONS_BUSY.load(Ordering::SeqCst), 1);
        }
        assert_eq!(REGIONS_BUSY.load(Ordering::SeqCst), 0);
        {
            let _decoding = PrimaryDecode::start();
            assert_eq!(worker.cancel.load(Ordering::SeqCst), before + 3);
            assert!(busy());
        }
        assert!(!busy());
    }
}
