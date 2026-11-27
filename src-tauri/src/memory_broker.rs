// SPDX-License-Identifier: AGPL-3.0-or-later
//! The app's memory budget (G74/BRK1): the broker of
//! `rawtonic_runtime::memory_budget` fed with this process's readings, the
//! open photo's state and the reclaim ledger's holders.
//!
//! Step 1 (this module's users): AI mask runs asked for (a click, the
//! masking tools, pending Subject/Sky layers of the open photo) and made
//! ahead, their model loads, the low-peak Subject model's background fetch,
//! the step prefetch's decodes and the multi view's background decodes.
//! Step 2 (a later change) adds exports (`Class::Export`, the rule is in the
//! broker), AI Denoise, Super Resolution, inpainting and depth: they call
//! [`reserve`] the same way.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use rawtonic_runtime::memory_budget::{Broker, Class, Granted, MB, Plan, Request, Wait};
use tauri::Manager;

/// The app's broker.
pub(crate) static BROKER: Broker = Broker::new();

/// How often a waiting request asks again.
const POLL: Duration = Duration::from_millis(250);

/// How often the app's memory is read for the peaks of the reservations held.
const SAMPLE: Duration = Duration::from_millis(250);

/// What a P3 request leaves for a click on Subject on a machine where the
/// click's run is the low-peak CPU plan's (≤ 16 GiB, AI on the CPU or on a
/// GPU that shares the system memory): the model (1.0 GB), its first run
/// (+1.36 GB, x86) and the photo's warped image (≈ 0.6 GB for 51 MP) →
/// 3.0 GB; 2.0 GB with the model loaded (notes: benchmark-results/
/// memb1-2026-11-26 §4, mem21-2026-11-26). Elsewhere none: macOS with Core
/// ML and larger machines were not short in any measurement, and a click
/// there reclaims held decodes first.
pub(crate) fn click_reserve(app: &tauri::AppHandle) -> u64 {
    let total = crate::memory_pressure::total_memory();
    let shares_memory = cfg!(not(target_os = "macos")) || rawtonic_onnx::models_run_on_cpu_only();
    if total == 0 || total > rawtonic_segment::LOW_PEAK_SUBJECT_MAX_MEMORY || !shares_memory {
        return 0;
    }
    let ai_state = app.state::<crate::state::ai::AiState>();
    let loaded = crate::state::lock::lock_recover(&ai_state.models)
        .as_ref()
        .is_some_and(|models| models.subject_model.is_some());
    if loaded { 2000 * MB } else { 3000 * MB }
}

/// What a settled frame of the open photo's full decode may add, per pixel
/// of the photo: the Lenovo's new-layer frame of a 51 MP RAF (8256x6192, a
/// Subject click's own frame) grew the app by 1877–2130 MB in 8 of 8 runs,
/// the first Develop frame by 1.3–2.5 GB (WBRK1, notes: benchmark-results/
/// brk2-2026-11-27): 2130 MB / 51.1 MP → 42 bytes. Most frames reuse their
/// caches and add nothing; the hold lasts only while the frame runs.
pub(crate) const FRAME_BYTES_PER_PIXEL: u64 = 42;

/// Settled frames of the open photo's full decode started so far, and the
/// newest start number among those that ended ([`VisibleFrame`]): an AI
/// mask's answer is held until a frame started after it has ended.
static FRAMES_STARTED: AtomicU64 = AtomicU64::new(0);
static FRAMES_ENDED_UP_TO: AtomicU64 = AtomicU64::new(0);

/// The open photo's frame caches (its full-resolution transform, the
/// reduced preview bases, the warp) were let go since its last settled
/// frame (`DevelopSession::release_rebuildable`, a new source): the next
/// settled frame builds them again — 2.4–3.7 GB at 50 MP right after a
/// Subject mask on a 14 GB laptop, where the frame before had built them
/// once (WBRK2 pending-layer runs, notes: benchmark-results/
/// brk3-2026-11-27).
static FRAME_CACHES_DROPPED: AtomicBool = AtomicBool::new(true);

/// Notes that the open photo's frame caches were let go.
pub(crate) fn note_frame_caches_dropped() {
    FRAME_CACHES_DROPPED.store(true, Ordering::SeqCst);
}

/// Whether the open photo's next settled frame builds its caches again.
pub(crate) fn frame_caches_dropped() -> bool {
    FRAME_CACHES_DROPPED.load(Ordering::SeqCst)
}

/// A settled frame of the open photo's full decode while it runs: its hold
/// in the broker, and when it ends, that the frame caches are built.
pub(crate) struct VisibleFrame {
    _hold: rawtonic_runtime::memory_budget::Reservation,
    started: u64,
}

impl Drop for VisibleFrame {
    fn drop(&mut self) {
        FRAME_CACHES_DROPPED.store(false, Ordering::SeqCst);
        FRAMES_ENDED_UP_TO.fetch_max(self.started, Ordering::SeqCst);
    }
}

/// Holds a settled frame's possible growth (`FRAME_BYTES_PER_PIXEL` ×
/// `pixels`) in the broker while it runs, so a Subject click that comes as
/// its new layer's frame starts does not take the same memory (G74/BRK2).
pub(crate) fn hold_visible_frame(pixels: u64) -> VisibleFrame {
    VisibleFrame {
        _hold: BROKER.hold(
            Class::Visible,
            "visible frame",
            pixels.saturating_mul(FRAME_BYTES_PER_PIXEL),
            crate::memory_pressure::reading().app,
        ),
        started: FRAMES_STARTED.fetch_add(1, Ordering::SeqCst) + 1,
    }
}

/// The longest an AI mask's reservation is held after its answer
/// ([`hold_after_answer`]): the frame with the mask took 2.6–3.1 s at 50 MP
/// on a Radeon 680M laptop when it built its caches again (WBRK2).
pub(crate) const AFTER_ANSWER_HOLD: Duration = Duration::from_secs(15);

/// Whether a frame started after `started_before` (`frames_started()` then)
/// has ended and the open photo is quiet again.
fn frame_after_settled(started_before: u64) -> bool {
    FRAMES_ENDED_UP_TO.load(Ordering::SeqCst) > started_before && !visible_busy()
}

/// What happens after an AI mask run the photographer asked for is
/// answered, under the run's own grant (G74/BRK3): the webview puts the mask
/// in its layer, the frame with the mask renders and the recipe and its
/// thumbnail are saved. When the run's reservation and its AI turn ended
/// at the answer, idle shader compiles started at once and let the open
/// photo's frame caches go (`memory_pressure::release_rebuildable_memory`),
/// so the frame built them again beside the model still loaded and the
/// saved thumbnail's own full-resolution warp: +2.4–3.7 GB, the Lenovo's
/// watchdog in 2 of 2 pending-layer runs at 7 GB available (WBRK2). They
/// are held here, on a task, until a frame started after the answer has
/// ended and the photo is quiet (`ai_ahead::QUIET`), at most
/// [`AFTER_ANSWER_HOLD`], or until `gone` says the photo was left. While
/// held, speculative work does not start and idle compiles stay stopped
/// (`memory_pressure::guard_idle_compiles` reads `asked_active`).
pub(crate) fn hold_after_answer<T: Send + 'static>(
    held: T,
    label: &'static str,
    gone: impl Fn() -> bool + Send + 'static,
) {
    let started_before = FRAMES_STARTED.load(Ordering::SeqCst);
    let start = Instant::now();
    tauri::async_runtime::spawn(async move {
        let why = loop {
            if frame_after_settled(started_before) {
                break "the frame with the mask settled";
            }
            if gone() {
                break "the photo was left";
            }
            if start.elapsed() >= AFTER_ANSWER_HOLD {
                break "time limit";
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        };
        log::info!(
            "memory broker: {label} held {} ms after its answer ({why}); {}",
            start.elapsed().as_millis(),
            crate::memory_pressure::memory_line()
        );
        drop(held);
    });
}

/// How long the open photo's saved thumbnail waits at most for its frames
/// and a request of the photographer ([`wait_for_open_photo`]).
pub(crate) const THUMBNAIL_WAIT: Duration = Duration::from_secs(20);

/// Waits (blocking, at most `limit`) while the open photo's decode or a
/// frame of it is under way or a request of the photographer runs, waits or
/// is held after its answer: the saved thumbnail of the open photo warps
/// the full decode when the frame caches are gone (+1.2 GB at 50 MP,
/// 1.2 s on a Radeon 680M), and beside the frame with a new mask the two
/// warps came to +2.4–3.7 GB (WBRK2). One after the other, the peak is the
/// larger one. Gives how long it waited.
pub(crate) fn wait_for_open_photo(limit: Duration) -> Duration {
    let start = Instant::now();
    while (visible_busy() || BROKER.asked_active()) && start.elapsed() < limit {
        std::thread::sleep(Duration::from_millis(100));
    }
    start.elapsed()
}

/// Whether the open photo's full-resolution decode or a frame of it is under
/// way (or ended less than `ai_ahead::QUIET` ago).
pub(crate) fn visible_busy() -> bool {
    !crate::multi_view_prefetch::primary_settled(crate::ai_ahead::QUIET)
}

/// Why a request did not start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NotStarted {
    /// `give_up` said so (the photo changed, the task was cancelled).
    GaveUp(&'static str),
    /// Still waiting after the request's timeout, for this.
    TimedOut(Wait),
}

pub(crate) fn describe(wait: Wait) -> String {
    match wait {
        Wait::Exclusive => "another run of its kind holds its turn".into(),
        Wait::Visible => "the photo's decode or a frame of it is under way".into(),
        Wait::Asked => "a request of the photographer waits or runs".into(),
        Wait::Stopping => "speculative work cancelled for it is stopping".into(),
        Wait::Memory { room, need } => {
            format!("{} MB free for it of {} MB needed", room / MB, need / MB)
        }
    }
}

fn plan_name(plan: Plan) -> &'static str {
    match plan {
        Plan::Full => "full",
        Plan::Reduced => "reduced",
    }
}

/// Logs a grant with what was done for it.
fn log_grant(granted: &Granted, waited: Duration) {
    let r = &granted.reservation;
    let mut done = String::new();
    for (name, bytes) in &granted.reclaimed {
        done.push_str(&format!(", released {name} ({} MB)", bytes / MB));
    }
    log::info!(
        "memory broker: {} ({}) granted after {} ms, {} plan (estimate {} MB, room {} MB){done}; {}",
        r.label(),
        r.class().name(),
        waited.as_millis(),
        plan_name(r.plan()),
        r.estimate() / MB,
        granted.room / MB,
        crate::memory_pressure::memory_line()
    );
}

fn log_release(released: &rawtonic_runtime::memory_budget::Released) {
    // A frame of the open photo is held for every settled render: only those
    // that grew the app are worth a line (WBRK1 rechecks read them).
    if released.class == Class::Visible && released.peak_growth < 100 * MB {
        return;
    }
    log::info!(
        "memory broker: {} ({}) released after {} ms, {} plan, peak +{} MB of {} MB estimated{}",
        released.label,
        released.class.name(),
        released.held_for.as_millis(),
        plan_name(released.plan),
        released.peak_growth / MB,
        released.estimate / MB,
        if released.alone {
            ""
        } else {
            " (beside other work)"
        }
    );
}

static SAMPLING: AtomicBool = AtomicBool::new(false);

/// Reads the app's memory every `SAMPLE` while reservations are held, for
/// their measured peaks (`Broker::observe`).
fn ensure_sampler() {
    if SAMPLING.swap(true, Ordering::SeqCst) {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("memory-broker-sampler".into())
        .spawn(|| {
            loop {
                if let Some(app) = crate::memory_pressure::reading().app {
                    BROKER.observe(app);
                }
                std::thread::sleep(SAMPLE);
                if !BROKER.holding() {
                    SAMPLING.store(false, Ordering::SeqCst);
                    // A grant between the check and the store starts no
                    // sampler of its own: go on for it.
                    if BROKER.holding() && !SAMPLING.swap(true, Ordering::SeqCst) {
                        continue;
                    }
                    return;
                }
            }
        });
    if spawned.is_err() {
        SAMPLING.store(false, Ordering::SeqCst);
    }
}

/// One attempt, without waiting: granted, or why not. For worker loops that
/// wait in their own way (the multi view's background decodes).
pub(crate) fn try_now(
    request: Request,
    waited: Duration,
    cancel: Option<Arc<dyn Fn() + Send + Sync>>,
) -> Result<Granted, Wait> {
    let granted = BROKER.try_reserve(
        request,
        &crate::memory_pressure::reading,
        visible_busy(),
        waited,
        cancel,
    )?;
    log_grant(&granted, waited);
    ensure_sampler();
    Ok(granted)
}

/// The loop of [`reserve`] and [`reserve_blocking`]: one attempt, with the
/// class `class_now` says (a request made ahead the photographer asks for
/// meanwhile becomes `Asked`), the log line on a change of reason and the
/// asked-waiting count.
struct Waiter<'a> {
    request: Request,
    start: Instant,
    held: Option<Wait>,
    asked_waiting: Option<rawtonic_runtime::memory_budget::AskedWaiting>,
    /// Told each new reason it waits for.
    on_wait: Option<&'a mut (dyn FnMut(Wait) + Send)>,
}

impl<'a> Waiter<'a> {
    fn new(request: Request) -> Self {
        Self {
            request,
            start: Instant::now(),
            held: None,
            asked_waiting: None,
            on_wait: None,
        }
    }

    fn attempt(
        &mut self,
        class: Class,
        cancel: &Option<Arc<dyn Fn() + Send + Sync>>,
    ) -> Result<Granted, Wait> {
        let request = Request {
            class,
            ..self.request
        };
        if class == Class::Asked && self.asked_waiting.is_none() {
            self.asked_waiting = Some(BROKER.waiting_asked());
        }
        let outcome = try_now(request, self.start.elapsed(), cancel.clone());
        match outcome {
            Ok(granted) => {
                self.asked_waiting = None;
                Ok(granted)
            }
            Err(wait) => {
                if self.held.as_ref().map(std::mem::discriminant)
                    != Some(std::mem::discriminant(&wait))
                {
                    log::info!(
                        "memory broker: {} ({}) waits, {} (estimate {} MB); {}",
                        request.label,
                        class.name(),
                        describe(wait),
                        request.need / MB,
                        crate::memory_pressure::memory_line()
                    );
                    if let Some(on_wait) = self.on_wait.as_mut() {
                        on_wait(wait);
                    }
                }
                self.held = Some(wait);
                Err(wait)
            }
        }
    }

    fn timed_out(&self, timeout: Option<Duration>) -> Option<NotStarted> {
        let wait = self.held?;
        if timeout.is_some_and(|t| self.start.elapsed() >= t) {
            log::info!(
                "memory broker: {} not started, still waiting ({}) after {} s; {}",
                self.request.label,
                describe(wait),
                self.start.elapsed().as_secs(),
                crate::memory_pressure::memory_line()
            );
            return Some(NotStarted::TimedOut(wait));
        }
        None
    }
}

/// Waits until `request` is granted (`Broker::try_reserve`): its class each
/// attempt from `class_now`, `give_up` checked before each, at most
/// `timeout` (None: no limit; a P1 is never refused, it waits only for
/// another exclusive P1 or briefly for the visible photo). `cancel` stops
/// speculative work when a P0/P1 needs its room.
pub(crate) async fn reserve(
    request: Request,
    class_now: impl Fn() -> Class,
    give_up: impl FnMut() -> Option<&'static str>,
    cancel: Option<Arc<dyn Fn() + Send + Sync>>,
    timeout: Option<Duration>,
) -> Result<Granted, NotStarted> {
    reserve_telling(|| request, class_now, give_up, cancel, timeout, &mut |_| {}).await
}

/// [`reserve`] with the request as `request_now` makes it each attempt (an
/// AI mask run's estimate drops once the photo's warped image is cached),
/// telling `on_wait` each new reason it waits for (an AI mask run that
/// waits for memory tells the photographer, G74/BRK2).
pub(crate) async fn reserve_telling(
    mut request_now: impl FnMut() -> Request,
    class_now: impl Fn() -> Class,
    mut give_up: impl FnMut() -> Option<&'static str>,
    cancel: Option<Arc<dyn Fn() + Send + Sync>>,
    timeout: Option<Duration>,
    on_wait: &mut (dyn FnMut(Wait) + Send),
) -> Result<Granted, NotStarted> {
    let mut waiter = Waiter::new(request_now());
    waiter.on_wait = Some(on_wait);
    loop {
        if let Some(reason) = give_up() {
            return Err(NotStarted::GaveUp(reason));
        }
        waiter.request = request_now();
        if let Ok(granted) = waiter.attempt(class_now(), &cancel) {
            return Ok(granted);
        }
        if let Some(not_started) = waiter.timed_out(timeout) {
            return Err(not_started);
        }
        tokio::time::sleep(POLL).await;
    }
}

/// [`reserve`] on a worker thread (the step prefetch): sleeps between
/// attempts.
pub(crate) fn reserve_blocking(
    request: Request,
    mut give_up: impl FnMut() -> Option<&'static str>,
    cancel: Option<Arc<dyn Fn() + Send + Sync>>,
    timeout: Option<Duration>,
) -> Result<Granted, NotStarted> {
    let mut waiter = Waiter::new(request);
    loop {
        if let Some(reason) = give_up() {
            return Err(NotStarted::GaveUp(reason));
        }
        if let Ok(granted) = waiter.attempt(request.class, &cancel) {
            return Ok(granted);
        }
        if let Some(not_started) = waiter.timed_out(timeout) {
            return Err(not_started);
        }
        std::thread::sleep(POLL);
    }
}

/// Logs the memory at the start and end of a stage (G74/BRK1 harness: the
/// preview jobs and the AI mask's stages, to attribute what a click adds).
pub(crate) struct Stage {
    what: String,
    start: Instant,
}

impl Stage {
    pub(crate) fn new(what: impl Into<String>) -> Self {
        let what = what.into();
        log::info!(
            "Develop memory: {what} started; {}",
            crate::memory_pressure::memory_line()
        );
        Self {
            what,
            start: Instant::now(),
        }
    }
}

impl Drop for Stage {
    fn drop(&mut self) {
        log::info!(
            "Develop memory: {} ended after {} ms; {}",
            self.what,
            self.start.elapsed().as_millis(),
            crate::memory_pressure::memory_line()
        );
    }
}

/// Registers the reclaim ledger's holders and the release log (once, at
/// start). Order: decodes held for neighbours (the multi view's written
/// ones and the step prefetch's kept one) → other photos' decodes and
/// preview bases in Develop's caches; each counts only buffers nothing else
/// holds (the open photo's decode is shared with its cache entry), and the
/// broker reads the memory again after releasing. Not the open photo's warped or
/// transformed images: a click's new layer renders from them (REV1 review:
/// dropping them made the frame rebuild the full-resolution transform beside
/// the network). Idle models other than the one asked for and the GPU export
/// processor come with step 2.
pub(crate) fn start(app: &tauri::AppHandle) {
    Broker::set_release_log(log_release);
    BROKER.register_reclaim(
        "decodes held for neighbours",
        0,
        || {
            (crate::multi_view_prefetch::held_written_bytes()
                + crate::proxy_worker::kept_ahead_bytes()) as u64
        },
        || {
            crate::multi_view_prefetch::release_held_written();
            crate::proxy_worker::forget_ahead();
            rawtonic_runtime::allocator::release_freed_memory();
        },
    );
    let bytes_app = app.clone();
    let release_app = app.clone();
    BROKER.register_reclaim(
        "other photos' decodes in Develop's cache",
        1,
        move || {
            let develop = bytes_app.state::<crate::state::develop::DevelopSession>();
            crate::state::lock::lock_recover(&develop.decoded_image_cache).releasable_bytes() as u64
        },
        move || {
            let develop = release_app.state::<crate::state::develop::DevelopSession>();
            crate::state::lock::lock_recover(&develop.decoded_image_cache).clear();
            crate::state::lock::lock_recover(&develop.reusable_previews).clear();
            rayon::spawn_broadcast(|_| rawtonic_runtime::allocator::release_freed_memory());
            rawtonic_runtime::allocator::release_freed_memory();
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Something held after an answer: tells when it is dropped.
    struct Held(Arc<AtomicBool>);

    impl Drop for Held {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    async fn dropped_within(flag: &AtomicBool, limit: Duration) -> bool {
        let start = Instant::now();
        while start.elapsed() < limit {
            if flag.load(Ordering::SeqCst) {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        flag.load(Ordering::SeqCst)
    }

    /// One test for the process-wide frame counters (G74/BRK3): what an AI
    /// mask's answer holds lasts until a frame started after the answer has
    /// ended, not one that was under way already; leaving the photo ends
    /// it too. A settled frame builds the frame caches a release let go.
    #[tokio::test]
    async fn an_answer_is_held_until_the_frame_with_its_mask_settles() {
        // The caches let go (an idle compile), then a frame builds them.
        note_frame_caches_dropped();
        assert!(frame_caches_dropped());
        let before = hold_visible_frame(1);
        let dropped = Arc::new(AtomicBool::new(false));
        hold_after_answer(Held(dropped.clone()), "AI subject click", || false);
        // The frame under way at the answer ends: it was not the mask's.
        drop(before);
        assert!(!frame_caches_dropped());
        assert!(!dropped_within(&dropped, Duration::from_millis(400)).await);
        // The frame with the mask.
        let with_mask = hold_visible_frame(1);
        assert!(!dropped_within(&dropped, Duration::from_millis(300)).await);
        drop(with_mask);
        assert!(dropped_within(&dropped, Duration::from_secs(2)).await);

        // Leaving the photo ends the hold at once.
        let left = Arc::new(AtomicBool::new(false));
        let gone = Arc::new(AtomicBool::new(false));
        let gone_now = gone.clone();
        hold_after_answer(Held(left.clone()), "AI sky click", move || {
            gone_now.load(Ordering::SeqCst)
        });
        assert!(!dropped_within(&left, Duration::from_millis(300)).await);
        gone.store(true, Ordering::SeqCst);
        assert!(dropped_within(&left, Duration::from_secs(2)).await);
    }
}
