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
use std::sync::atomic::{AtomicBool, Ordering};
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
    if granted.cancelled > 0 {
        done.push_str(&format!(", cancelled {} speculative", granted.cancelled));
    }
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
        crate::memory_pressure::reading(),
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
struct Waiter {
    request: Request,
    start: Instant,
    held: Option<Wait>,
    asked_waiting: Option<rawtonic_runtime::memory_budget::AskedWaiting>,
}

impl Waiter {
    fn new(request: Request) -> Self {
        Self {
            request,
            start: Instant::now(),
            held: None,
            asked_waiting: None,
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
    mut give_up: impl FnMut() -> Option<&'static str>,
    cancel: Option<Arc<dyn Fn() + Send + Sync>>,
    timeout: Option<Duration>,
) -> Result<Granted, NotStarted> {
    let mut waiter = Waiter::new(request);
    loop {
        if let Some(reason) = give_up() {
            return Err(NotStarted::GaveUp(reason));
        }
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
/// preview bases in Develop's caches. Not the open photo's warped or
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
            crate::state::lock::lock_recover(&develop.decoded_image_cache).retained_bytes() as u64
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
