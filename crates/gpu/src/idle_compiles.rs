//! Display variants compiled in idle time under a compile limit (G76/WCC3).
//!
//! On AMD's Windows Vulkan driver (the only one with a compile limit,
//! `device_start::compile_limit`) the first use of a tone slider waited for
//! its display variant: 56–74 s at 5–6 GB of private memory on a Radeon
//! 680M, once per machine and driver (the pipeline cache keeps it). From
//! the app's start (`GpuProcessor::compile_ahead_for_new_raw`: the plan of a new
//! RAW edit, so the time in the Library counts) and for each photo shown,
//! this compiles the variants a photographer most likely needs next, one at
//! a time and only when no other heavy compile waits or runs, so the first
//! slider use finds one ready: in a helper process whenever no heavy compile
//! ran for `IDLE_QUIET` (`idle_compile_child`), without one only while the
//! app is idle.
//!
//! Each is a superset of the photo's own features: a frame renders with the
//! first compiled superset of its features (an idle feature changes no
//! pixel, `GpuProcessor::preview_pipeline`), so one compile serves Contrast,
//! Highlights, a curve and a mask at once. A frame a superset serves keeps
//! rendering with it; its own variant is not compiled (in idle time those
//! compiles held the slot for minutes after the edits, G76/WGT14), except
//! the one its exports render with (`planned_jobs`, G76/WCC7). Layer-sum
//! variants a frame needs follow in idle time (`IdleCompiles::later`).
//!
//! Idle compiles show nothing (they are not counted in `compiles_pending`):
//! no photo waits for them. A frame that does wait for the running one
//! counts as a pending compile for that time (`IdleCompiles::covering`).
//!
//! They run strictly one at a time and only while the app's memory guard
//! allows (`set_idle_compile_memory_guard`): one display variant's compile
//! committed up to 5–6 GB of private memory on that driver, and on a 15 GB
//! laptop the app peaked at 13.8 GB with repeated memory pressure (G76/WGT14).

use super::*;
use std::collections::VecDeque;
use std::sync::Condvar;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// How long no frame may have rendered and no compile may have waited or
/// run before an idle compile starts.
const IDLE_QUIET: Duration = Duration::from_secs(3);
/// How often the worker looks whether the app is idle.
const IDLE_POLL: Duration = Duration::from_millis(250);
/// How long the worker waits before it asks a memory guard that held it
/// again.
const MEMORY_HOLD_POLL: Duration = Duration::from_secs(2);
/// How long after a frame last asked for the running job it counts as
/// waiting for it (`IdleCompiles::awaited`).
const AWAITED: Duration = Duration::from_millis(500);

/// Why the app does not allow an idle compile now (`Err`: short of memory,
/// an export runs), asked before each.
pub type IdleCompileMemoryGuard = Box<dyn Fn() -> Result<(), String> + Send + Sync>;

static MEMORY_GUARD: std::sync::OnceLock<IdleCompileMemoryGuard> = std::sync::OnceLock::new();

/// What the app gives back before a compile an export waits for
/// (`set_before_export_compile`).
static BEFORE_EXPORT_COMPILE: std::sync::OnceLock<Box<dyn Fn() + Send + Sync>> =
    std::sync::OnceLock::new();

/// Sets what the app does right before the idle worker compiles the variant
/// an export waits for (`export_of_shown_photo_starts`): give back the
/// memory it can rebuild, as before an idle compile (the memory guard is
/// not asked then: it holds idle compiles while an export runs). On a
/// Radeon 680M the app held 2.45 GB while a 16-bit export waited for its
/// helper (4.44 GB): 6.89 GB together (G76/WCOLD2). Once per process.
pub fn set_before_export_compile(release: Box<dyn Fn() + Send + Sync>) {
    if BEFORE_EXPORT_COMPILE.set(release).is_err() {
        log::warn!("GPU idle compiles: the export release was already set");
    }
}

/// Sets what the app checks before each idle compile (its private memory,
/// the system's memory pressure); once per process. Without one, idle
/// compiles do not wait for memory.
pub fn set_idle_compile_memory_guard(guard: IdleCompileMemoryGuard) {
    if MEMORY_GUARD.set(guard).is_err() {
        log::warn!("GPU idle compiles: the memory guard was already set");
    }
}

fn memory_allows() -> Result<(), String> {
    MEMORY_GUARD.get().map_or(Ok(()), |guard| guard())
}

/// One pipeline compiled in idle time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum IdleJob {
    /// The display variant of these features.
    Display(usize),
    /// The display variant of these features reading the layer sums.
    LayerSums(usize),
    /// The display variant of these features with float output in deferred
    /// mode: what 16-bit exports render with (`deferred_display`).
    Deferred(usize),
}

/// The display variant worth compiling ahead for a photo shown with
/// display variant `features` (a frame renders with the first compiled
/// superset of its features): `basic`, every Basic-panel edit the recipe's
/// versions make: the tone sliders in both directions (raised Shadows,
/// lowered Whites and Blacks of the newer tone versions), Highlights, a
/// curve, a mask layer, Clarity or Structure, Dehaze, Vibrance and the
/// colour mixer.
///
/// Alone, without the smaller tone-sliders variant before it (24 s on a
/// Radeon 680M, then 42 s for `basic`; G76/WHCF): on a computer's first
/// session `basic` is then ready 42 s after its helper starts instead of
/// 66 s, and it is the variant a photo that arrives edited shows with and
/// every edited photo exports with (`export_display_features`). The sliders
/// variant served only the tone edits made in those first 24 s (G76/COLD).
///
/// No entry with the effects (glow, halation, grain): the photo's features
/// with all three compiled for 56 s into 7.0 GB of private memory on a
/// Radeon 680M, the app's peak in idle time (G76/WGT20; the Basic-panel
/// entry 5.4 GB), and after Basic-panel edits it covered no frame (an edit
/// with Glow needs the edited features with glow). A recipe with an effect
/// has its export's variant planned (`planned_jobs`), with that effect only.
///
/// Edits add up: a photographer who moves Contrast, Highlights, Clarity,
/// Whites and Blacks needs the variant of all five at once, which no single
/// slider's variant covers (G76/WGT14: Clarity after Highlights 86 s,
/// Whites 97 s, Blacks 220 s on a Radeon 680M with the earlier plan). The
/// display variants' one tone stage (`shaders/display_tone_stage.wgsl`)
/// keeps `basic` within a few thousand inlined expressions of `sliders`
/// (G76/WCC4); without it each newer tone direction added a copy of the
/// stage.
pub(super) fn idle_plan(features: usize, adjustments: &AllAdjustments) -> Vec<usize> {
    let basic = basic_features(features, adjustments);
    if basic == features {
        Vec::new()
    } else {
        vec![basic]
    }
}

/// What `IdleCompiles::plan` queues for a photo shown with display variant
/// `features`, in this order:
///
/// 1. `basic` with Glow (`serving_features`), the variant that serves the
///    frames: every Basic-panel edit and Glow render with it (an idle
///    feature changes no pixel). First, because Glow turned on after
///    Basic-panel edits has no smaller variant: it waited 66 s for this one
///    on a Radeon 680M (G76/WSMOKE14), and 49 s in a computer's first
///    session when it was planned after `basic` (G76/WCOLD1: the two took
///    42 s and 46 s one after the other; this one alone is ready as soon as
///    `basic` was, a few seconds later). Halation and Grain are not planned.
/// 2. `idle_plan`'s `basic`, the variant an edited photo without Glow
///    exports with, and
/// 3. the variant the shown photo's exports render with now (`exact`,
///    `export_display_features`).
///
/// A frame keeps rendering with any compiled superset of its features, so
/// only the first entry is left out when a compiled superset covers it
/// (`covered_is_enough`); the others are export variants, which an export
/// would otherwise compile cold beside its textures (G76/WCC7 review: tens
/// of seconds and 4–6 GB on a Radeon 680M).
///
/// With the effect passes (`effect_passes`, G76/EFX) the first entry also
/// holds Halation, and an edited photo exports with it (`export_display_
/// features`): glow and halation add 0.1k inlined expressions to a main
/// pass and grain none, so one variant serves the Basic panel, the three
/// effects and the exports, and `basic` alone is not planned (a Radeon 680M
/// compiled the two for 42 s and 46 s one after the other).
pub(super) fn planned_jobs(features: usize, adjustments: &AllAdjustments) -> (Vec<usize>, usize) {
    let mut plan = vec![serving_features(features, adjustments)];
    let exact = export_display_features(adjustments);
    let basic = if effect_passes::here() {
        Vec::new()
    } else {
        idle_plan(features, adjustments)
    };
    for entry in basic.into_iter().chain([exact]) {
        if !plan.contains(&entry) {
            plan.push(entry);
        }
    }
    (plan, exact)
}

/// `planned_jobs`' first entry: every Basic-panel edit with Glow, the
/// variant an edited photo with Glow renders and exports with
/// (`export_display_features`); with the effect passes also with Halation,
/// the variant every edited photo renders and exports with.
pub(super) fn serving_features(features: usize, adjustments: &AllAdjustments) -> usize {
    basic_features(features, adjustments)
        | GLOW_FEATURE
        | if effect_passes::here() {
            HALATION_FEATURE
        } else {
            0
        }
}

/// Whether a compiled superset makes the planned display variant
/// `features` needless (`covered`: one is compiled): only the one planned
/// to serve the frames (`serving`, `planned_jobs`' first), never one
/// exports render with.
fn covered_is_enough(
    features: usize,
    serving: Option<usize>,
    exact: Option<usize>,
    covered: impl FnOnce() -> bool,
) -> bool {
    serving == Some(features) && exact != Some(features) && covered()
}

/// `features` with the tone sliders and Highlights (part of
/// `basic_features`).
fn sliders_features(features: usize, adjustments: &AllAdjustments) -> usize {
    let g = &adjustments.global;
    let tone = ToneVersion::from_uniform(g.tone_version);
    features
        | TONE_PREVIEW_FEATURE
        | 2
        | if g.tonemapper_mode == 3 && tone.highlights_edge_base() {
            HIGHLIGHTS_V14_FEATURE
        } else {
            0
        }
}

/// `idle_plan`'s entry: `features` with every Basic-panel edit the
/// recipe's versions make. Also the display variant an edited photo exports
/// with under a compile limit (`gpu_processing::export_display_features`),
/// so a change here changes those exports' bytes.
pub(super) fn basic_features(features: usize, adjustments: &AllAdjustments) -> usize {
    let g = &adjustments.global;
    let tone = ToneVersion::from_uniform(g.tone_version);
    let bit = |on: bool, bit: usize| if on { bit } else { 0 };
    let dehaze = DehazeVersion::from_uniform(g.dehaze_version);
    sliders_features(features, adjustments)
        | 4
        | 8
        | bit(tone.shadows_midtone_cut(), SHADOWS_V13_FEATURE)
        | bit(tone.whites_bright_reach(), WHITES_V12_FEATURE)
        | bit(tone.blacks_reach(), BLACKS_V16_FEATURE)
        | bit(
            ClarityVersion::from_uniform(g.clarity_version).get() >= 3
                || StructureVersion::from_uniform(g.structure_version).edge_aware(),
            PRESENCE_V3_FEATURE,
        )
        | bit(dehaze.airlight(), DEHAZE_V3_FEATURE)
        | bit(dehaze.veil() && !dehaze.airlight(), DEHAZE_V2_FEATURE)
        | bit(
            VibranceVersion::from_uniform(g.vibrance_version).oklab(),
            VIBRANCE_V2_FEATURE,
        )
        | bit(g.hsl[0].response > 0.5, HSL_V2_FEATURE)
}

/// The bits `basic_features` can add: a display variant with none of them
/// shows no Basic-panel edit.
pub(super) const BASIC_FEATURES: usize = TONE_PREVIEW_FEATURE
    | 2
    | 4
    | 8
    | HIGHLIGHTS_V14_FEATURE
    | SHADOWS_V13_FEATURE
    | WHITES_V12_FEATURE
    | BLACKS_V16_FEATURE
    | PRESENCE_V3_FEATURE
    | DEHAZE_V3_FEATURE
    | DEHAZE_V2_FEATURE
    | VIBRANCE_V2_FEATURE
    | HSL_V2_FEATURE;

/// What the worker compiles with: the processor's display shader, layout
/// and tables.
pub(super) struct IdleTarget {
    pub(super) device: Arc<wgpu::Device>,
    pub(super) shader: wgpu::ShaderModule,
    pub(super) display: Arc<DisplayShader>,
    pub(super) layout: wgpu::PipelineLayout,
    pub(super) pipelines: Arc<VariantTable<std::sync::OnceLock<wgpu::ComputePipeline>>>,
    pub(super) compilation: Arc<VariantTable<std::sync::OnceLock<()>>>,
    pub(super) failed: Arc<VariantTable<std::sync::atomic::AtomicBool>>,
    pub(super) layer_sums: Arc<VariantTable<std::sync::OnceLock<wgpu::ComputePipeline>>>,
    pub(super) layer_sum_compilation: Arc<VariantTable<std::sync::OnceLock<()>>>,
    pub(super) deferred: Arc<super::deferred_display::DeferredDisplay>,
}

#[derive(Default)]
struct Queue {
    /// The plan made last (`idle_plan`), the key that decides whether a
    /// shown photo replaces it: its features alone are not, since the plan
    /// also follows the recipe's look and response versions (a photo with
    /// the same features but Dehaze 2 instead of 3 needs another `basic`).
    /// The device, shader source and compile limit the plan compiles for
    /// are the processor's, which owns this queue (one per processor, only
    /// under a compile limit), so they never change under one key.
    planned: Option<Vec<usize>>,
    /// The planned display variant exports of the shown photo render with
    /// (`planned_jobs`): compiled even when a compiled superset covers it.
    exact: Option<usize>,
    /// Whether the plan holds the deferred variant of `exact`
    /// (`deferred_display::set_idle_deferred_exports`).
    deferred: bool,
    /// The planned supersets (replaced when another photo is shown).
    plan: VecDeque<IdleJob>,
    /// Layer-sum variants that frames need (the per-layer loop serves them
    /// until then).
    later: VecDeque<IdleJob>,
    /// The job compiling now.
    running: Option<IdleJob>,
    /// The variant an export that starts now renders with
    /// (`IdleCompiles::export_starts`): the worker compiles it next, without
    /// waiting for idle time or asking the memory guard, and the export
    /// waits for it. Cleared when the worker is done with it.
    export_job: Option<IdleJob>,
    /// The variants frames wait for that no planned variant covers
    /// (`IdleCompiles::for_frame`), each with when a frame last asked for
    /// it (`now_ms`): the worker compiles them next after an export's, as
    /// it does that one, in this order. One no frame asks for any more
    /// moves to `later`.
    frame_jobs: Vec<(IdleJob, u64)>,
    /// A frame waits for the planned variant at the front
    /// (`IdleCompiles::covering`): the worker compiles it now, without
    /// waiting for quiet or asking the memory guard (the frame would
    /// compile a variant of that size in the app otherwise).
    rush: bool,
    worker: bool,
    cancelled: bool,
}

impl Queue {
    /// When a frame last asked for frame job `job`, if it is one.
    fn frame_asked(&mut self, job: IdleJob) -> Option<&mut u64> {
        self.frame_jobs
            .iter_mut()
            .find_map(|(frame, asked)| (*frame == job).then_some(asked))
    }

    /// Moves the frame jobs no frame asked for within `AWAITED` to `later`
    /// (the frame's render ended without it): they compile in idle time.
    /// `now`: `now_ms`.
    fn frames_moved_on(&mut self, now: u64) {
        let waits = |asked: u64| now.saturating_sub(asked) < AWAITED.as_millis() as u64;
        let (kept, moved): (Vec<_>, Vec<_>) = std::mem::take(&mut self.frame_jobs)
            .into_iter()
            .partition(|(_, asked)| waits(*asked));
        self.frame_jobs = kept;
        for (job, _) in moved {
            if !self.later.contains(&job) {
                self.later.push_front(job);
            }
        }
    }

    /// Whether the helper running `job` stops for a frame's variant: a
    /// frame waits for another one, and none for `job` itself.
    fn frame_goes_before(&mut self, job: IdleJob, now: u64) -> bool {
        self.frames_moved_on(now);
        !self.frame_jobs.is_empty() && self.frame_asked(job).is_none()
    }

    /// Replaces the planned supersets with `plan` unless it is the plan
    /// made last; whether it did.
    /// `deferred`: the plan ends with the deferred variant of `exact`
    /// (`deferred_display::idle_deferred_exports`).
    fn replan(&mut self, (plan, exact): (Vec<usize>, usize), deferred: bool) -> bool {
        if self.planned.as_ref() == Some(&plan)
            && self.exact == Some(exact)
            && self.deferred == deferred
        {
            return false;
        }
        self.plan = plan.iter().copied().map(IdleJob::Display).collect();
        // Last: 16-bit exports of the photo render with it (G76/WCC10).
        if deferred {
            self.plan.push_back(IdleJob::Deferred(exact));
        }
        self.planned = Some(plan);
        self.exact = Some(exact);
        self.deferred = deferred;
        true
    }
}

/// Where the worker took a job from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Source {
    /// `Queue::export_job`.
    Export,
    /// `Queue::frame_job`.
    Frame,
    /// `Queue::plan`.
    Plan,
    /// `Queue::later`.
    Later,
}

pub(super) struct IdleCompiles {
    queue: Mutex<Queue>,
    changed: Condvar,
    /// Milliseconds since `since_start`'s start of the last frame rendered
    /// for the window with a display variant (0: none yet).
    last_frame_ms: AtomicU64,
    /// Milliseconds since `since_start`'s start when a frame last waited
    /// for the job that runs or runs next (`covering`; 0: never).
    awaited_ms: AtomicU64,
}

impl IdleCompiles {
    pub(super) fn new() -> Arc<Self> {
        let this = Arc::new(Self {
            queue: Mutex::new(Queue::default()),
            changed: Condvar::new(),
            last_frame_ms: AtomicU64::new(0),
            awaited_ms: AtomicU64::new(0),
        });
        let mut live = LIVE.lock().unwrap_or_else(|e| e.into_inner());
        live.retain(|idle| idle.strong_count() > 0);
        live.push(Arc::downgrade(&this));
        this
    }

    /// Adds the deferred variant of the shown photo's export variant to a
    /// plan that lacks it (`plan_deferred_exports`).
    fn want_deferred(&self) {
        let mut queue = self.lock();
        let Some(exact) = queue.exact else {
            return;
        };
        if queue.cancelled || queue.deferred {
            return;
        }
        queue.deferred = true;
        queue.plan.push_back(IdleJob::Deferred(exact));
        self.changed.notify_all();
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Queue> {
        self.queue.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A frame for the window is rendering: idle compiles wait `IDLE_QUIET`
    /// after it. Thumbnails and other renders off the screen do not count:
    /// an import's thumbnails kept idle compiles from starting for as long
    /// as it ran, the time a computer's first session needs them (G76/COLD).
    pub(super) fn frame(&self) {
        self.last_frame_ms
            .store(now_ms().max(1), std::sync::atomic::Ordering::Relaxed);
    }

    /// Plans the supersets of `features` (a photo now shown with them),
    /// replacing the plan of a photo shown before.
    pub(super) fn plan(
        self: &Arc<Self>,
        features: usize,
        adjustments: &AllAdjustments,
        target: impl FnOnce() -> IdleTarget,
    ) {
        let mut queue = self.lock();
        let deferred = super::deferred_display::idle_deferred_exports();
        if queue.cancelled || !queue.replan(planned_jobs(features, adjustments), deferred) {
            return;
        }
        self.start(&mut queue, target);
    }

    /// Compiles `job` in idle time (after the planned ones).
    pub(super) fn later(self: &Arc<Self>, job: IdleJob, target: impl FnOnce() -> IdleTarget) {
        let mut queue = self.lock();
        if queue.cancelled || queue.running == Some(job) || queue.later.contains(&job) {
            return;
        }
        queue.later.push_back(job);
        self.start(&mut queue, target);
    }

    fn start(self: &Arc<Self>, queue: &mut Queue, target: impl FnOnce() -> IdleTarget) {
        if !queue.worker {
            queue.worker = true;
            let this = Arc::clone(self);
            let target = target();
            rawtonic_runtime::spawn_render_thread("idle-compile", move || this.work(target));
        }
        self.changed.notify_all();
    }

    /// The display variant compiling now if it covers `features`, or the
    /// planned one the worker compiles next if that does: a frame of those
    /// features renders with it once it is done, and waits for it instead
    /// of compiling its own. The planned one compiles at once then
    /// (`Queue::rush`): after Glow's own compile had stopped the helper, the
    /// Contrast edit right behind it found no helper running yet and
    /// compiled the whole variant in the app, 42 s at 5.9 GB on a Radeon
    /// 680M (G76/WCOLD2).
    pub(super) fn covering(&self, features: usize) -> Option<usize> {
        let mut queue = self.lock();
        let covers = |job: Option<IdleJob>| match job {
            Some(IdleJob::Display(job)) if job & features == features => Some(job),
            _ => None,
        };
        let now = now_ms().max(1);
        if let Some(running) = covers(queue.running) {
            self.awaited_ms.store(now, Ordering::Relaxed);
            if let Some(asked) = queue.frame_asked(IdleJob::Display(running)) {
                *asked = now;
            }
            return Some(running);
        }
        if queue.cancelled || !queue.worker {
            return None;
        }
        // A frame's variant is next whatever runs now: the worker stops
        // that for it (unless an export or another frame waits for it).
        if let Some((frame, asked)) = queue
            .frame_jobs
            .iter_mut()
            .find_map(|(job, asked)| covers(Some(*job)).map(|job| (job, asked)))
        {
            *asked = now;
            self.awaited_ms.store(now, Ordering::Relaxed);
            return Some(frame);
        }
        if queue.running.is_some() {
            return None;
        }
        // What the worker compiles next: an export's variant, then the
        // plan's first.
        let next = match covers(queue.export_job) {
            Some(next) => next,
            None => {
                let next = covers(queue.plan.front().copied())?;
                if !queue.rush {
                    queue.rush = true;
                    self.changed.notify_all();
                }
                next
            }
        };
        self.awaited_ms.store(now_ms().max(1), Ordering::Relaxed);
        Some(next)
    }

    /// A frame needs display variant `features`, which nothing compiled or
    /// planned covers (an effect no plan holds: Halation, Grain): where idle
    /// compiles run in helper processes it compiles in one now, and the
    /// frame waits for it (`covering` answers it from here on) instead of
    /// compiling it in the app. Whether it does. Halation moved by mistake
    /// on an edited photo compiled its variant in the app for 50.7 s at
    /// 6.7 GB of the app's memory on a Radeon 680M, with the window showing
    /// the old frame (G76/WSMOKE20); a helper commits about 4.4 GB beside
    /// an app that gave back what it can rebuild. The frame's render waits
    /// for it as it waited for its own compile (edits made meanwhile show
    /// afterwards); a job whose frame no longer asks (its render ended
    /// with another pipeline) moves to idle time. Several frames' variants
    /// (the photo shown, a thumbnail) compile one after the other.
    pub(super) fn for_frame(
        self: &Arc<Self>,
        features: usize,
        target: impl FnOnce() -> IdleTarget,
    ) -> bool {
        if !super::idle_compile_child::idle_compiles_in_helpers() {
            return false;
        }
        let mut queue = self.lock();
        if queue.cancelled {
            return false;
        }
        let (job, now) = (IdleJob::Display(features), now_ms().max(1));
        match queue.frame_asked(job) {
            Some(asked) => *asked = now,
            None => queue.frame_jobs.push((job, now)),
        }
        self.awaited_ms.store(now, Ordering::Relaxed);
        self.start(&mut queue, target);
        true
    }

    /// Whether a frame or an export waits for `job` now: a frame asks
    /// `covering` every few milliseconds while it waits.
    fn awaited(&self, job: IdleJob) -> bool {
        let last = self.awaited_ms.load(Ordering::Relaxed);
        (last != 0 && now_ms().saturating_sub(last) < AWAITED.as_millis() as u64)
            || self.lock().export_job == Some(job)
    }

    /// Ends the worker after the compile it runs (if any); nothing new
    /// starts. A driver call cannot be stopped midway.
    pub(super) fn cancel(&self) {
        let mut queue = self.lock();
        queue.cancelled = true;
        queue.export_job = None;
        queue.frame_jobs.clear();
        drop(queue);
        self.changed.notify_all();
    }

    /// An export of the shown photo starts: the variant it renders with
    /// (`deferred`: the deferred one of 16-bit exports and several outputs,
    /// else the display variant) compiles next (`Queue::export_job`).
    /// Whether the worker has it to look at.
    fn export_starts(&self, deferred: bool) -> bool {
        let mut queue = self.lock();
        let Some(exact) = queue.exact else {
            return false;
        };
        if queue.cancelled || !queue.worker {
            return false;
        }
        let job = if deferred {
            IdleJob::Deferred(exact)
        } else {
            IdleJob::Display(exact)
        };
        queue.plan.retain(|planned| *planned != job);
        queue.export_job = Some(job);
        drop(queue);
        self.changed.notify_all();
        true
    }

    /// The next job of the plan (a planned superset: a compiled superset
    /// can make it needless), else of the ones for later.
    fn next(queue: &mut Queue) -> Option<(IdleJob, Source)> {
        queue
            .plan
            .pop_front()
            .map(|job| (job, Source::Plan))
            .or_else(|| queue.later.pop_front().map(|job| (job, Source::Later)))
    }

    /// Whether an idle compile may start: `IDLE_QUIET` after the last heavy
    /// compile, and, where idle compiles run in this process (no helper),
    /// after the last frame for the window: there a compile is one call
    /// that nothing stops. A helper process is stopped the moment an edit
    /// needs the slot and costs the frames nothing (Exposure 30–76 FPS
    /// beside a compile on a Radeon 680M, G76/WGT12), so it does not wait
    /// for the photographer to pause: while they edited, the Glow variant
    /// never started and Glow then compiled for 49 s in the app
    /// (G76/WCOLD1). At once when there was neither yet (the app's start,
    /// `GpuProcessor::compile_ahead_for_new_raw`).
    fn idle(&self) -> bool {
        let quiet = IDLE_QUIET.as_millis() as u64;
        let quiet_since = |last: u64| last == 0 || now_ms().saturating_sub(last) >= quiet;
        (super::idle_compile_child::idle_compiles_in_helpers()
            || quiet_since(self.last_frame_ms.load(Ordering::Relaxed)))
            && quiet_since(LAST_COMPILE_MS.load(Ordering::Relaxed))
    }

    fn work(self: Arc<Self>, target: IdleTarget) {
        // Why the memory guard holds the worker, while it does.
        let mut held: Option<String> = None;
        loop {
            // Wait for a job, then for idle time and a free slot.
            let (slot, turn) = {
                let mut queue = self.lock();
                loop {
                    if queue.cancelled {
                        queue.worker = false;
                        return;
                    }
                    if !queue.plan.is_empty()
                        || !queue.later.is_empty()
                        || queue.export_job.is_some()
                        || !queue.frame_jobs.is_empty()
                    {
                        break;
                    }
                    queue = self.changed.wait(queue).unwrap_or_else(|e| e.into_inner());
                }
                queue.frames_moved_on(now_ms());
                let urgent =
                    queue.export_job.is_some() || !queue.frame_jobs.is_empty() || queue.rush;
                drop(queue);
                if !urgent && !self.idle() {
                    let queue = self.lock();
                    let _ = self
                        .changed
                        .wait_timeout(queue, IDLE_POLL)
                        .unwrap_or_else(|e| e.into_inner());
                    continue;
                }
                let slot = background_compile_slot("idle compile");
                let Some(slot) = slot else {
                    let queue = self.lock();
                    let _ = self
                        .changed
                        .wait_timeout(queue, IDLE_POLL)
                        .unwrap_or_else(|e| e.into_inner());
                    continue;
                };
                // Asked with the slot held, so only right before a compile
                // (the app's guard drops memory it can rebuild when it
                // allows one).
                // An export waits for this compile before it makes its
                // textures: the guard would hold it for that export.
                let allowed = if urgent { Ok(()) } else { memory_allows() };
                match allowed {
                    Ok(()) => {
                        if let Some(reason) = held.take() {
                            log::info!("GPU idle compiles resumed (held: {reason})");
                        }
                    }
                    Err(reason) => {
                        drop(slot);
                        if held.is_none() {
                            log::warn!("GPU idle compiles held: {reason}");
                        }
                        held = Some(reason);
                        let queue = self.lock();
                        let _ = self
                            .changed
                            .wait_timeout(queue, MEMORY_HOLD_POLL)
                            .unwrap_or_else(|e| e.into_inner());
                        continue;
                    }
                }
                // Not beside an AI network run on the CPU where the two
                // take turns (`memory_turns`): asked again after the poll,
                // without the slot, which a frame's compile may need.
                let Some(turn) = crate::memory_turns::compile() else {
                    drop(slot);
                    let queue = self.lock();
                    let _ = self
                        .changed
                        .wait_timeout(queue, IDLE_POLL)
                        .unwrap_or_else(|e| e.into_inner());
                    continue;
                };
                (slot, turn)
            };
            let (job, source) = {
                let mut queue = self.lock();
                if queue.cancelled {
                    queue.worker = false;
                    return;
                }
                // The slot is held from here on.
                queue.rush = false;
                // The variant an export waits for goes first, then a
                // frame's.
                queue.frames_moved_on(now_ms());
                let frame = queue.frame_jobs.first().map(|(job, _)| *job);
                let next = match (queue.export_job, frame) {
                    (Some(job), _) => Some((job, Source::Export)),
                    (None, Some(job)) => Some((job, Source::Frame)),
                    (None, None) => Self::next(&mut queue),
                };
                let Some((job, source)) = next else {
                    continue;
                };
                // Already compiled or compiling; the planned superset that
                // serves the frames also when a compiled one covers it.
                let needless = match job {
                    IdleJob::Display(features) => {
                        source == Source::Plan
                            && covered_is_enough(
                                features,
                                queue
                                    .planned
                                    .as_ref()
                                    .and_then(|plan| plan.first().copied()),
                                queue.exact,
                                || {
                                    target
                                        .pipelines
                                        .supersets(features)
                                        .any(|p| p.get().is_some())
                                },
                            )
                    }
                    _ => false,
                };
                if needless || !Self::unclaimed(&target, job) {
                    // The export or the frame has what it waited for.
                    Self::served(&mut queue, job, source);
                    continue;
                }
                queue.running = Some(job);
                // Neither waited for idle time nor asked the memory guard:
                // the app gives back what it can rebuild all the same.
                if matches!(source, Source::Export | Source::Frame)
                    && let Some(release) = BEFORE_EXPORT_COMPILE.get()
                {
                    drop(queue);
                    release();
                }
                (job, source)
            };
            let work = CompileWork::new();
            // In a helper process first, which a compile or an export that
            // waits for the slot stops (`idle_compile_child`); the job is
            // claimed only when it compiles here.
            // Stopped too when an export waits for another variant
            // (`export_starts`), and for an AI run the photographer asked
            // for unless a frame or an export waits for this job
            // (`memory_turns`). A frame's variant stops any other but an
            // export's and another frame's still waited for (`for_frame`).
            let stop = || {
                let mut queue = self.lock();
                if queue.cancelled
                    || queue.export_job.is_some_and(|export| export != job)
                    || (queue.export_job != Some(job) && queue.frame_goes_before(job, now_ms()))
                {
                    return true;
                }
                drop(queue);
                crate::memory_turns::asked_run_waits() && !self.awaited(job)
            };
            #[cfg(test)]
            let slowed_and_stopped = source == Source::Frame && tests::slow_frame_compile(&stop);
            #[cfg(not(test))]
            let slowed_and_stopped = false;
            let helped = if slowed_and_stopped {
                super::idle_compile_child::Helped::Stopped
            } else {
                super::idle_compile_child::compile_in_helper(&target, job, stop)
            };
            match helped {
                super::idle_compile_child::Helped::Stopped => {
                    let mut queue = self.lock();
                    queue.running = None;
                    // An edit's compile stopped the variant an export
                    // waited for: the export goes on and compiles it.
                    if queue.export_job == Some(job) {
                        queue.export_job = None;
                    }
                    match source {
                        Source::Export | Source::Plan => queue.plan.push_front(job),
                        Source::Later => queue.later.push_front(job),
                        // Still a frame's (`frame_jobs`): next again,
                        // unless its frame moved on (`frames_moved_on`,
                        // which requeued it for idle time).
                        Source::Frame => {}
                    }
                    drop(queue);
                    drop(work);
                    drop(turn);
                    drop(slot);
                    continue;
                }
                super::idle_compile_child::Helped::Compiled {
                    cache,
                    seeded,
                    mark,
                } => {
                    if Self::claim(&target, job) {
                        super::idle_compile_child::HELPER_CACHE
                            .with(|helper| *helper.borrow_mut() = Some(cache.clone()));
                        self.compile(&target, job);
                        super::idle_compile_child::HELPER_CACHE
                            .with(|helper| *helper.borrow_mut() = None);
                        let kept = self.keep(&target, job, cache, seeded, mark);
                        log::info!("GPU idle compile of {}: {kept}", job.arg());
                    }
                }
                super::idle_compile_child::Helped::Unavailable => {
                    if Self::claim(&target, job) {
                        self.compile(&target, job);
                    }
                }
            }
            drop(work);
            drop(turn);
            drop(slot);
            let mut queue = self.lock();
            queue.running = None;
            if queue.export_job == Some(job) {
                queue.export_job = None;
            }
            queue.frame_jobs.retain(|(frame, _)| *frame != job);
        }
    }

    /// `job` of `source` needs no compile: the export or the frame that
    /// waited for it goes on.
    fn served(queue: &mut Queue, job: IdleJob, source: Source) {
        match source {
            Source::Export => queue.export_job = None,
            Source::Frame => queue.frame_jobs.retain(|(frame, _)| *frame != job),
            _ => {}
        }
    }

    /// Makes the cache a helper left for `job` the app's cache file (the
    /// app's pipelines plus this job, `pipeline_cache::adopt`) and says
    /// what became of it, for the log. Only a cache that holds every
    /// pipeline of the app's: one seeded from it (`seeded`) with nothing
    /// compiled into the app's since (`mark`). Otherwise (a photo was
    /// opened while the helper ran: its detail pass, the RAW default
    /// variant at the app's start) the helper runs once more from the
    /// app's cache of now; the driver's own cache answers it without
    /// compiling (0.5 s on a Radeon 680M, G76/WHCF run 2), and without one
    /// it compiles again, in idle time as before. A helper stopped for an
    /// edit, or passed again, leaves the job in the driver's cache alone
    /// until a later session's helper keeps it.
    fn keep(
        &self,
        target: &IdleTarget,
        job: IdleJob,
        cache: wgpu::PipelineCache,
        seeded: bool,
        mark: u64,
    ) -> &'static str {
        use super::idle_compile_child::{Helped, cache_additions, compile_in_helper};
        if !seeded {
            return "not kept in the app's pipeline cache (the helper started empty)";
        }
        let cache = if cache_additions() == mark {
            cache
        } else {
            match compile_in_helper(target, job, || self.lock().cancelled) {
                Helped::Compiled {
                    cache,
                    seeded: true,
                    mark,
                } if cache_additions() == mark => cache,
                _ => {
                    return "not kept in the app's pipeline cache (the app compiled beside the helper)";
                }
            }
        };
        crate::pipeline_cache::adopt(&target.device, cache);
        #[cfg(test)]
        super::idle_compile_child::ADOPTED.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        "kept in the app's pipeline cache"
    }

    /// Whether nobody compiled or claimed `job` yet.
    fn unclaimed(target: &IdleTarget, job: IdleJob) -> bool {
        match job {
            IdleJob::Display(features) => target.compilation[features].get().is_none(),
            IdleJob::LayerSums(features) => target.layer_sum_compilation[features].get().is_none(),
            IdleJob::Deferred(features) => {
                target.deferred.get(features).is_none() && !target.deferred.failed(features)
            }
        }
    }

    /// Claims `job` for this worker: `false` when another compile has it.
    fn claim(target: &IdleTarget, job: IdleJob) -> bool {
        match job {
            IdleJob::Display(features) => target.compilation[features].set(()).is_ok(),
            IdleJob::LayerSums(features) => target.layer_sum_compilation[features].set(()).is_ok(),
            IdleJob::Deferred(_) => Self::unclaimed(target, job),
        }
    }

    fn compile(&self, target: &IdleTarget, job: IdleJob) {
        let (features, layer_sums) = match job {
            IdleJob::Display(features) => (features, false),
            IdleJob::LayerSums(features) => (features, true),
            IdleJob::Deferred(features) => {
                let started = std::time::Instant::now();
                log::info!(
                    "GPU idle compile of deferred display variant {features}: started (+{:.2} s since start)",
                    since_start()
                );
                let created = BACKGROUND_COMPILE.with(|background| {
                    background.set(true);
                    let created = target
                        .deferred
                        .compile(&target.device, features)
                        .map(|_| ());
                    background.set(false);
                    created
                });
                if let Err(error) = created {
                    log::error!(
                        "Deferred display variant {features} unavailable; exports compile their own: {error}"
                    );
                }
                log::info!(
                    "GPU idle compile of deferred display variant {features}: ended after {:.2} s (+{:.2} s since start)",
                    started.elapsed().as_secs_f64(),
                    since_start()
                );
                return;
            }
        };
        let started = std::time::Instant::now();
        log::info!(
            "GPU idle compile of display variant {features}{}: started (+{:.2} s since start)",
            if layer_sums { " (layer sums)" } else { "" },
            since_start()
        );
        let created = BACKGROUND_COMPILE.with(|background| {
            background.set(true);
            let created = create_preview_pipeline(
                &target.device,
                &target.shader,
                Some(&target.display),
                &target.layout,
                features,
                true,
                true,
                layer_sums,
                false,
            );
            background.set(false);
            created
        });
        match (created, layer_sums) {
            (Ok(pipeline), false) => {
                let _ = target.pipelines[features].set(pipeline);
            }
            (Ok(pipeline), true) => {
                let _ = target.layer_sums[features].set(pipeline);
            }
            (Err(error), false) => {
                log::error!(
                    "Specialized preview variant {features} unavailable; using the general shader: {error}"
                );
                target.failed[features].store(true, Ordering::Release);
            }
            (Err(error), true) => log::error!(
                "Layer sums variant {features} unavailable; using the per-layer loop: {error}"
            ),
        }
        log::info!(
            "GPU idle compile of display variant {features}: ended after {:.2} s (+{:.2} s since start)",
            started.elapsed().as_secs_f64(),
            since_start()
        );
    }
}

/// An export of the shown photo starts under a compile limit, before it
/// makes its textures (`export_of_shown_photo_starts`): the variant it
/// renders with compiles now if it is not compiled, in its helper, and the
/// export waits for it (`wait_for_running_compiles`). Picking 16-bit TIFF
/// and exporting at once is the normal way to export: the deferred variant
/// was then still waiting for idle time and the export compiled it itself
/// beside its textures, 35.9–37.9 s at 7.2 GB of the app's memory on a
/// Radeon 680M (G76/WSMOKE19, WCOLD1), against the helper's ~4.4 GB beside
/// an app without them. Likewise the display variant of an 8-bit export
/// made before idle time compiled it.
pub(super) fn export_starts(deferred: bool) {
    let live = LIVE.lock().unwrap_or_else(|e| e.into_inner()).clone();
    for idle in live.iter().filter_map(std::sync::Weak::upgrade) {
        idle.export_starts(deferred);
    }
}

/// Whether an idle worker still has the variant an export waits for to
/// compile or look at (`export_starts`).
pub(super) fn export_awaits_variant() -> bool {
    let live = LIVE.lock().unwrap_or_else(|e| e.into_inner()).clone();
    live.iter()
        .filter_map(std::sync::Weak::upgrade)
        .any(|idle| idle.lock().export_job.is_some())
}

/// The idle queues of the processors alive (`plan_deferred_exports`).
static LIVE: Mutex<Vec<std::sync::Weak<IdleCompiles>>> = Mutex::new(Vec::new());

/// The photographer picked an export that renders with a deferred display
/// variant (16-bit TIFF in the export dialog): from now on the idle plans
/// hold it, and the shown photo's plan gets it now, so the export does not
/// compile it (G76/WSMOKE14: the dialog's estimate compiled it for 39 s).
pub(super) fn plan_deferred_exports() {
    super::deferred_display::note_deferred_export();
    let live = LIVE.lock().unwrap_or_else(|e| e.into_inner()).clone();
    for idle in live.iter().filter_map(std::sync::Weak::upgrade) {
        idle.want_deferred();
    }
}

/// Ends the idle worker when its processor is dropped.
pub(super) struct IdleCompilesOwner(pub(super) Arc<IdleCompiles>);

impl Drop for IdleCompilesOwner {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

fn now_ms() -> u64 {
    (since_start() * 1000.0) as u64
}

/// When the last heavy compile other than an idle one ended (or started);
/// 0: none yet.
static LAST_COMPILE_MS: AtomicU64 = AtomicU64::new(0);

/// Notes a heavy compile other than an idle one (`validated`).
pub(super) fn compile_activity() {
    LAST_COMPILE_MS.store(now_ms().max(1), Ordering::Relaxed);
}

thread_local! {
    /// Set while this thread runs an idle compile under its slot: its
    /// creations neither wait for another slot nor count as pending.
    pub(super) static BACKGROUND_COMPILE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
mod tests {
    use super::super::new_raw_edit as new_raw_edit_at_start;
    use super::*;

    fn adjustments(tone_version: f32, tonemapper_mode: u32) -> AllAdjustments {
        let mut adjustments: AllAdjustments = bytemuck::Zeroable::zeroed();
        adjustments.global.tone_version = tone_version;
        adjustments.global.tonemapper_mode = tonemapper_mode;
        adjustments
    }

    /// A new edit of a RAW (every response at its newest) with `edits`.
    fn new_raw_edit(edits: serde_json::Value) -> AllAdjustments {
        let mut recipe =
            serde_json::Value::Object(crate::image_processing::new_edit_responses(true));
        let object = recipe.as_object_mut().unwrap();
        object.extend([
            ("sharpness".to_string(), serde_json::json!(40)),
            ("colorNoiseReduction".to_string(), serde_json::json!(25)),
        ]);
        object.extend(edits.as_object().unwrap().clone());
        // The RAW look (Camera Raw, `tonemapper_mode` 3).
        *crate::image_processing::get_all_adjustments_from_json(&recipe, true, Some(3)).uniforms
    }

    /// Under a compile limit an export renders with a display variant chosen
    /// from its recipe alone (`export_display_features`), and in a normal
    /// session it is one the editor compiled: a photo's own variant before
    /// any Basic-panel edit, the planned Basic-panel superset after edits,
    /// and for an effect on an unedited photo its features with that effect
    /// (`planned_jobs`; G76/WCC7, WCC8).
    #[test]
    fn exports_take_the_variant_the_editor_compiles() {
        let shown = new_raw_edit(serde_json::json!({}));
        let opened = preview_features(&shown);
        let plan = idle_plan(opened, &shown);
        let export = |edits: serde_json::Value| export_display_features(&new_raw_edit(edits));
        assert_eq!(export(serde_json::json!({})), opened);
        // Glow on an unedited photo: its own features with glow only, the
        // entry `planned_jobs` adds.
        let glow = new_raw_edit(serde_json::json!({"glowAmount": 30}));
        assert_eq!(
            export(serde_json::json!({"glowAmount": 30})),
            opened | GLOW_FEATURE
        );
        let (jobs, exact) = planned_jobs(preview_features(&glow), &glow);
        assert_eq!(exact, opened | GLOW_FEATURE);
        assert!(jobs.contains(&exact));
        // The Basic panel's variant with Glow serves the frames, first.
        assert_eq!(jobs[0], plan[0] | GLOW_FEATURE);
        let edit = serde_json::json!({"exposure": 0.4, "contrast": 25, "highlights": -40,
            "shadows": 30, "whites": -20, "blacks": -10, "clarity": 20, "structure": 15,
            "dehaze": 15, "vibrance": 20, "hsl": {"reds": {"hue": 12}}});
        for edits in [
            serde_json::json!({"contrast": 20}),
            serde_json::json!({"shadows": 40}),
            serde_json::json!({"whites": -30, "vibrance": 25}),
            edit.clone(),
        ] {
            let features = preview_features(&new_raw_edit(edits.clone()));
            let chosen = export(edits.clone());
            assert_eq!(chosen, plan[0], "{edits}");
            assert_eq!(chosen & features, features, "{edits}");
        }
        // Effects on an edited photo: the frame compiles the same superset
        // (no planned variant covers it), with the effects.
        let mut glow = edit;
        glow["glowAmount"] = serde_json::json!(30);
        assert_eq!(export(glow), plan[0] | GLOW_FEATURE);
    }

    /// The planned variant that serves the frames is left out when a
    /// compiled superset covers it; the variants exports render with
    /// compile all the same (Codex review of G76/WCC7): `basic` although
    /// `basic` with Glow covers it, and the shown photo's own.
    #[test]
    fn export_variants_compile_although_a_wider_one_serves_the_frames() {
        let check = |adjustments: &AllAdjustments, compiled: usize| {
            let features = preview_features(adjustments);
            let (plan, exact) = planned_jobs(features, adjustments);
            assert_eq!(exact, export_display_features(adjustments));
            assert!(plan.contains(&exact));
            plan.iter()
                .copied()
                .filter(|&p| {
                    !covered_is_enough(p, plan.first().copied(), Some(exact), || compiled & p == p)
                })
                .collect::<Vec<_>>()
        };
        let edit = new_raw_edit(serde_json::json!({"exposure": 0.4, "contrast": 25,
            "highlights": -40, "clarity": 20, "vibrance": 20}));
        let unedited = new_raw_edit(serde_json::json!({}));
        let basic = export_display_features(&edit);
        let glow = basic | GLOW_FEATURE;
        // Nothing compiled: everything, the frames' variant first.
        assert_eq!(check(&edit, 0), vec![glow, basic]);
        assert_eq!(
            check(&unedited, 0),
            vec![glow, basic, preview_features(&unedited)]
        );
        // A wider variant (Glow with halation) serves the frames: the
        // export variants still compile.
        let wider = glow | HALATION_FEATURE;
        assert_eq!(check(&edit, wider), vec![basic]);
        assert_eq!(
            check(&unedited, wider),
            vec![basic, preview_features(&unedited)]
        );
    }

    /// Glow turned on after Basic-panel edits renders and exports with
    /// `basic` and Glow, planned last (G76/WSMOKE14: 66 s for it on a
    /// Radeon 680M when Glow was first used); an edit with Glow already has
    /// no extra entry.
    #[test]
    fn glow_after_basic_edits_is_planned() {
        let shown = new_raw_edit(serde_json::json!({}));
        let opened = preview_features(&shown);
        let (jobs, _) = planned_jobs(opened, &shown);
        let edit = serde_json::json!({"contrast": 25, "clarity": 20, "glowAmount": 30});
        let glow = export_display_features(&new_raw_edit(edit.clone()));
        assert_eq!(jobs.first(), Some(&glow));
        assert_eq!(glow, idle_plan(opened, &shown)[0] | GLOW_FEATURE);
        let features = preview_features(&new_raw_edit(edit));
        assert_eq!(glow & features, features, "it serves the Glow frame");
        let with_glow = new_raw_edit(serde_json::json!({"glowAmount": 30}));
        let (jobs, _) = planned_jobs(preview_features(&with_glow), &with_glow);
        assert_eq!(jobs.iter().filter(|&&job| job == glow).count(), 1);
    }

    /// A 16-bit export picked in the export dialog adds the deferred variant
    /// of the shown photo's export variant to its plan, once (G76/WSMOKE14).
    #[test]
    fn a_sixteen_bit_pick_plans_the_deferred_variant() {
        let idle = IdleCompiles::new();
        idle.want_deferred();
        assert!(
            idle.lock().plan.is_empty(),
            "no photo shown, nothing planned"
        );
        let shown = new_raw_edit(serde_json::json!({}));
        let jobs = planned_jobs(preview_features(&shown), &shown);
        let exact = jobs.1;
        assert!(idle.lock().replan(jobs, false));
        idle.want_deferred();
        idle.want_deferred();
        let queue = idle.lock();
        assert_eq!(queue.plan.back(), Some(&IdleJob::Deferred(exact)));
        let deferred = queue
            .plan
            .iter()
            .filter(|job| matches!(job, IdleJob::Deferred(_)))
            .count();
        assert_eq!(deferred, 1);
        assert!(queue.deferred);
    }

    /// What the app plans at its start (`compile_ahead_for_new_raw`) is the
    /// plan of the never-edited RAW opened later, so the first photo keeps
    /// it (`Queue::replan`): the Basic-panel superset with Glow of the RAW
    /// default variant the processor compiles at start, then the one
    /// without Glow and that variant itself (export variants) — the
    /// variants a first session compiled for an unedited RAW on the Radeon
    /// 680M (G76/WHCF, WSMOKE18: 7335999, 5238847).
    #[test]
    fn the_start_plans_what_a_new_raw_edit_plans() {
        // Without a look set for every RAW in Preferences: the RAW look.
        let at_start = new_raw_edit_at_start(None);
        assert_eq!(at_start.global.tonemapper_mode, 3);
        let features = preview_features(&at_start);
        assert_eq!(features, RAW_DEFAULT_PREVIEW_FEATURES);
        let (plan, exact) = planned_jobs(features, &at_start);
        assert_eq!(exact, features);
        assert_eq!(plan, vec![5238847 | GLOW_FEATURE, 5238847, features]);
        // Response 8's detail-cache pass.
        assert_eq!(sharpen_level(&at_start), 2);
    }

    /// With the effect passes (under a compile limit, G76/EFX) one variant
    /// serves a photo's Basic-panel edits, Glow, Halation and Grain and its
    /// exports: the plan of a never-edited RAW holds it and the RAW default
    /// variant, and an edit with any effect asks for no other.
    #[test]
    fn with_effect_passes_one_variant_serves_edits_effects_and_exports() {
        use super::super::preview_tests::get_all_adjustments_from_json;
        use serde_json::json;
        effect_passes::FORCED.with(|forced| forced.set(Some(true)));
        let at_start = new_raw_edit_at_start(None);
        let features = preview_features(&at_start);
        let serving = 5238847 | GLOW_FEATURE | HALATION_FEATURE;
        assert_eq!(
            planned_jobs(features, &at_start),
            (vec![serving, features], features)
        );
        for edit in [
            json!({"contrast":20}),
            json!({"contrast":20,"glowAmount":30}),
            json!({"shadows":20,"halationAmount":30}),
            json!({"whites":-10,"grainAmount":30}),
            json!({"contrast":20,"glowAmount":30,"halationAmount":30,"grainAmount":30}),
        ] {
            let mut edited = new_raw_edit_at_start(None);
            let values = get_all_adjustments_from_json(&edit, true, Some(3));
            let (g, v) = (&mut edited.global, &values.global);
            (g.contrast, g.shadows, g.whites) = (v.contrast, v.shadows, v.whites);
            (g.glow_amount, g.halation_amount, g.grain_amount) =
                (v.glow_amount, v.halation_amount, v.grain_amount);
            let shown = preview_features(&edited);
            assert_eq!(shown & GRAIN_FEATURE, 0, "{edit}");
            assert_eq!(shown & serving, shown, "{edit}: not served");
            assert_eq!(export_display_features(&edited), serving, "{edit}");
            assert_eq!(planned_jobs(shown, &edited), (vec![serving], serving));
        }
        effect_passes::FORCED.with(|forced| forced.set(None));
        // Without them grain is the main shader's.
        let grain = get_all_adjustments_from_json(&json!({"grainAmount":30}), true, Some(3));
        assert_ne!(preview_features(&grain) & GRAIN_FEATURE, 0);
    }

    /// One entry, every Basic-panel edit, first: no smaller tone-sliders
    /// variant compiles before it (G76/COLD).
    #[test]
    fn plan_is_every_basic_edit_at_once() {
        let shown = new_raw_edit(serde_json::json!({}));
        let features = preview_features(&shown);
        let plan = idle_plan(features, &shown);
        let sliders = features | TONE_PREVIEW_FEATURE | 2 | HIGHLIGHTS_V14_FEATURE;
        let basic = sliders
            | 4
            | 8
            | SHADOWS_V13_FEATURE
            | WHITES_V12_FEATURE
            | BLACKS_V16_FEATURE
            | PRESENCE_V3_FEATURE
            | DEHAZE_V3_FEATURE
            | VIBRANCE_V2_FEATURE
            | HSL_V2_FEATURE;
        assert_eq!(plan, vec![basic]);
        assert_eq!(basic & features, features);
    }

    /// Every Basic-panel edit of a new RAW edit, alone and added up as a
    /// photographer makes them (G76/WGT14: Contrast, Highlights, Clarity,
    /// Whites, Blacks), renders with a planned variant.
    #[test]
    fn plan_covers_the_basic_edits_and_their_sums() {
        let shown = new_raw_edit(serde_json::json!({}));
        let plan = idle_plan(preview_features(&shown), &shown);
        let covered = |edits: &serde_json::Value| {
            let features = preview_features(&new_raw_edit(edits.clone()));
            plan.iter().any(|&p| p & features == features)
        };
        let mut sum = serde_json::json!({});
        for (key, value) in [
            ("exposure", 0.5),
            ("contrast", 20.0),
            ("highlights", -30.0),
            ("clarity", 40.0),
            ("whites", -30.0),
            ("blacks", -30.0),
            ("shadows", 40.0),
            ("structure", 30.0),
            ("dehaze", 20.0),
            ("vibrance", 25.0),
            ("saturation", 10.0),
        ] {
            for sign in [1.0, -1.0] {
                let edit = serde_json::json!({ key: value * sign });
                assert!(covered(&edit), "{edit}");
            }
            sum.as_object_mut()
                .unwrap()
                .insert(key.into(), serde_json::json!(value));
            assert!(covered(&sum), "{sum}");
        }
        // With a mixer slider and a curve on top.
        let mut all = sum.clone();
        let object = all.as_object_mut().unwrap();
        object.insert(
            "hsl".into(),
            serde_json::json!({"reds": {"hue": 10, "saturation": 0, "luminance": 0}}),
        );
        object.insert(
            "curves".into(),
            serde_json::json!({"luma": [{"x": 0, "y": 0}, {"x": 128, "y": 140}, {"x": 255, "y": 255}]}),
        );
        let features = preview_features(&new_raw_edit(all.clone()));
        assert_ne!(features & HSL_V2_FEATURE, 0, "the mixer moved: {all}");
        assert_ne!(features & 4, 0, "a curve: {all}");
        assert!(covered(&all), "{all}");
    }

    /// A photo shown with the features of the one before but other response
    /// versions (here Dehaze 2 against 3, Dehaze at 0 on both, so neither
    /// sets a Dehaze bit) replaces the plan; the same plan again does not.
    #[test]
    fn a_photo_with_the_same_features_but_other_versions_replans() {
        let mut dehaze_3 = adjustments(crate::image_processing::TONE_VERSION as f32, 3);
        dehaze_3.global.dehaze_version = 3;
        let mut dehaze_2 = dehaze_3;
        dehaze_2.global.dehaze_version = 2;
        let features = preview_features(&dehaze_3);
        assert_eq!(features, preview_features(&dehaze_2));
        let (plan_3, plan_2) = (
            idle_plan(features, &dehaze_3),
            idle_plan(features, &dehaze_2),
        );
        assert_ne!(plan_3, plan_2);
        let planned = |queue: &Queue| {
            queue
                .plan
                .iter()
                .map(|job| match job {
                    IdleJob::Display(features) => *features,
                    IdleJob::LayerSums(_) | IdleJob::Deferred(_) => unreachable!(),
                })
                .collect::<Vec<_>>()
        };
        let mut queue = Queue::default();
        assert!(queue.replan((plan_3.clone(), 0), false));
        assert_eq!(planned(&queue), plan_3);
        // The worker took the first job; the same photo again keeps the rest.
        queue.plan.pop_front();
        assert!(!queue.replan((plan_3.clone(), 0), false));
        assert_eq!(planned(&queue), plan_3[1..]);
        assert!(planned(&queue).is_empty());
        assert!(queue.replan((plan_2.clone(), 0), false));
        assert_eq!(planned(&queue), plan_2);
        // A look change (Camera Raw to AgX) with the same features too.
        let agx = adjustments(crate::image_processing::TONE_VERSION as f32, 1);
        assert_eq!(preview_features(&agx), features);
        assert!(queue.replan(planned_jobs(features, &agx), false));
        // 16-bit exports (G76/WCC10): the same plan with the deferred
        // variant of its export entry last, and again without it.
        let (plan, exact) = planned_jobs(features, &agx);
        assert!(queue.replan((plan.clone(), exact), true));
        assert_eq!(queue.plan.back(), Some(&IdleJob::Deferred(exact)));
        assert_eq!(queue.plan.len(), plan.len() + 1);
        assert!(!queue.replan((plan.clone(), exact), true));
        assert!(queue.replan((plan, exact), false));
        assert!(!queue.plan.contains(&IdleJob::Deferred(exact)));
    }

    #[test]
    fn plan_keeps_older_responses_and_skips_the_photos_own_variant() {
        // Another look: Highlights keeps the version-13 code; versions
        // before raised Shadows' newer response plan no bit for it.
        let other_look = adjustments(crate::image_processing::TONE_VERSION as f32, 0);
        let (sh, wh, bl) = (SHADOWS_V13_FEATURE, WHITES_V12_FEATURE, BLACKS_V16_FEATURE);
        let basic = 46 | sh | wh | bl;
        assert_eq!(idle_plan(0, &other_look), vec![basic]);
        assert_eq!(idle_plan(34, &other_look), vec![basic]);
        // A photo already shown with it plans nothing.
        assert_eq!(idle_plan(basic, &other_look), Vec::<usize>::new());
        // Tone version 6: none of the newer tone directions' code.
        let old = adjustments(6.0, 0);
        assert_eq!(idle_plan(34, &old), vec![46]);
    }

    /// Under a compile limit the shown photo plans its supersets, which
    /// compile in idle time; the first Contrast frame then renders at once
    /// without a compile of its own and keeps rendering with the superset.
    #[test]
    #[ignore = "GPU; sets the process-wide compile limit, so run it alone: cargo test -p rawtonic-gpu --lib -- --ignored --exact gpu_processing::idle_compiles::tests::idle_compiles_serve_the_first_tone_slider"]
    fn idle_compiles_serve_the_first_tone_slider() {
        use super::super::preview_tests::{context, get_all_adjustments_from_json};
        use std::time::Instant;
        set_compile_limit(Some(1));
        let processor = GpuProcessor::new(context(), 256, 256).unwrap();
        let recipe = |extra: serde_json::Value| {
            let mut recipe = serde_json::json!({
                "toneVersion": crate::image_processing::TONE_VERSION,
                "sharpness": 40,
                "colorNoiseReduction": 25,
            });
            recipe
                .as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            get_all_adjustments_from_json(&recipe, true, Some(3))
        };
        let wait = |what: &str, done: &dyn Fn() -> bool| {
            let deadline = Instant::now() + Duration::from_secs(300);
            while !done() {
                assert!(Instant::now() < deadline, "{what} did not compile");
                std::thread::sleep(Duration::from_millis(50));
            }
        };
        let photo = recipe(serde_json::json!({}));
        let shown = preview_features(&photo);
        processor.preview_pipeline(&photo, true).unwrap();
        let plan = idle_plan(shown, &photo);
        // The Basic panel; no all-effects entry (G76/WCC8).
        assert_eq!(plan.len(), 1);
        let started = Instant::now();
        wait("the planned supersets", &|| {
            for &f in &plan {
                assert!(
                    !processor.preview_failed[f].load(Ordering::Acquire),
                    "variant {f} failed"
                );
            }
            plan.iter()
                .all(|&f| processor.preview_pipelines[f].get().is_some())
        });
        assert!(
            started.elapsed() >= IDLE_QUIET,
            "idle compiles start after {IDLE_QUIET:?} without frames"
        );
        let contrast = recipe(serde_json::json!({"contrast": 20}));
        let features = preview_features(&contrast);
        assert_eq!(
            features & plan[0],
            features,
            "the first superset covers Contrast"
        );
        assert!(processor.preview_pipelines[features].get().is_none());
        let started = Instant::now();
        processor.preview_pipeline(&contrast, true).unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "the Contrast frame waited {:?}",
            started.elapsed()
        );
        assert!(!compiles_pending(), "no compile of its own started");
        // The superset keeps serving it: its own variant never compiles.
        std::thread::sleep(IDLE_QUIET + Duration::from_secs(2));
        assert!(processor.preview_pipelines[features].get().is_none());
        drop(processor);
        set_compile_limit(None);
    }

    /// Under a compile limit the app's start plans a new RAW edit's idle
    /// compiles (G76/COLD): the Basic-panel variant compiles with no photo
    /// shown, and the first Contrast frame of the photo opened later renders
    /// at once. The RAW default variant and the detail-cache pass are small
    /// compiles: they run while a heavy one holds the compile slot (an idle
    /// helper's, here the test's).
    #[test]
    #[ignore = "GPU; sets the process-wide compile limit, so run it alone: cargo test -p rawtonic-gpu --lib -- --ignored --exact gpu_processing::idle_compiles::tests::the_start_compiles_the_basic_variant_before_a_photo_is_shown"]
    fn the_start_compiles_the_basic_variant_before_a_photo_is_shown() {
        use super::super::preview_tests::context;
        use std::time::Instant;
        set_compile_limit(Some(1));
        let wait = |what: &str, done: &dyn Fn() -> bool| {
            let deadline = Instant::now() + Duration::from_secs(300);
            while !done() {
                assert!(Instant::now() < deadline, "{what} did not compile");
                std::thread::sleep(Duration::from_millis(50));
            }
        };
        let held = compile_slot("the test's heavy compile");
        let processor = GpuProcessor::new(context(), 256, 256).unwrap();
        let photo = super::super::new_raw_edit(None);
        let shown = preview_features(&photo);
        assert_eq!(shown, RAW_DEFAULT_PREVIEW_FEATURES);
        wait("the RAW default variant beside a heavy compile", &|| {
            processor.preview_pipelines[shown].get().is_some()
        });
        processor.detail_pipeline_for(sharpen_level(&photo));
        wait("the detail-cache pass beside a heavy compile", &|| {
            processor.detail_pipeline[sharpen_level(&photo)]
                .get()
                .is_some()
        });
        // Nothing heavy starts while the slot is held.
        processor.compile_ahead_for_new_raw(None);
        let (plan, _) = planned_jobs(shown, &photo);
        std::thread::sleep(Duration::from_secs(1));
        assert!(processor.preview_pipelines[plan[0]].get().is_none());
        assert!(
            processor
                .idle_compiles
                .as_ref()
                .unwrap()
                .0
                .lock()
                .running
                .is_none()
        );
        drop(held);
        // No frame was rendered: the plan compiles without one.
        wait("the planned variants", &|| {
            plan.iter()
                .all(|&f| processor.preview_pipelines[f].get().is_some())
        });
        let mut contrast = photo;
        contrast.global.contrast = 0.2;
        let features = preview_features(&contrast);
        assert_eq!(features & plan[0], features);
        let started = Instant::now();
        processor.preview_pipeline(&contrast, true).unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "the Contrast frame waited {:?}",
            started.elapsed()
        );
        assert!(processor.preview_pipelines[features].get().is_none());
        drop(processor);
        std::thread::sleep(IDLE_QUIET + Duration::from_secs(2));
        set_compile_limit(None);
    }

    /// An export that starts while an idle helper runs stops it (it asks
    /// for the slot), unless the idle worker has the export's own variant to
    /// compile: then the export waits for that (G76/WSMOKE19).
    #[test]
    #[ignore = "sets the process-wide compile limit, so run it alone: cargo test -p rawtonic-gpu --lib -- --ignored --exact gpu_processing::idle_compiles::tests::an_export_waits_for_its_own_variant_instead_of_stopping_it"]
    fn an_export_waits_for_its_own_variant_instead_of_stopping_it() {
        set_compile_limit(Some(1));
        let idle = IdleCompiles::new();
        {
            let mut queue = idle.lock();
            queue.worker = true;
            queue.exact = Some(7335999);
        }
        for own in [false, true] {
            // The idle worker's slot while its helper runs.
            let helper = compile_slot("an idle helper");
            if own {
                export_starts(true);
                assert_eq!(idle.lock().export_job, Some(IdleJob::Deferred(7335999)));
            }
            let export = std::thread::spawn(|| wait_for_running_compiles("export"));
            std::thread::sleep(Duration::from_millis(500));
            assert!(!export.is_finished(), "the export waits for the helper");
            assert_eq!(slot_wanted(), !own, "own: {own}");
            // The worker is done with it.
            idle.lock().export_job = None;
            drop(helper);
            export.join().unwrap();
            assert!(!slot_wanted());
        }
        set_compile_limit(None);
    }

    /// Under a compile limit a 16-bit export picked and started at once
    /// finds its variant compiled: with the effect passes it renders with
    /// the display variant the plan compiled (its main pass writes float
    /// output), so no deferred variant compiles and the export does not
    /// wait (G76/EFX; before, the idle worker compiled the deferred variant
    /// first, G76/WSMOKE19).
    #[test]
    #[ignore = "GPU; sets the process-wide compile limit, so run it alone: cargo test -p rawtonic-gpu --lib -- --ignored --exact gpu_processing::idle_compiles::tests::an_export_right_after_a_sixteen_bit_pick_waits_for_its_variant"]
    fn an_export_right_after_a_sixteen_bit_pick_waits_for_its_variant() {
        use super::super::preview_tests::context;
        use std::time::Instant;
        set_compile_limit(Some(1));
        let processor = GpuProcessor::new(context(), 256, 256).unwrap();
        let photo = super::super::new_raw_edit(None);
        let shown = preview_features(&photo);
        processor.preview_pipeline(&photo, true).unwrap();
        let (plan, exact) = planned_jobs(shown, &photo);
        let deadline = Instant::now() + Duration::from_secs(300);
        while !plan
            .iter()
            .all(|&f| processor.preview_pipelines[f].get().is_some())
        {
            assert!(
                Instant::now() < deadline,
                "the display plan did not compile"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(processor.deferred_display.get(exact).is_none());
        // A frame just rendered: idle time is `IDLE_QUIET` away.
        processor.preview_pipeline(&photo, true).unwrap();
        assert!(processor.effect_passes.is_some());
        let started = Instant::now();
        export_of_shown_photo_starts(true);
        wait_for_running_compiles("export");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "the export waited {:?}",
            started.elapsed()
        );
        assert!(
            processor.deferred_display.get(exact).is_none(),
            "a deferred variant was compiled"
        );
        drop(processor);
        std::thread::sleep(IDLE_QUIET + Duration::from_secs(2));
        set_compile_limit(None);
    }

    /// An export of the shown photo names the variant it renders with for
    /// the worker to compile next (G76/WSMOKE19: 16-bit TIFF picked and
    /// exported right away), taken out of the plan; without a shown photo
    /// or a worker nothing waits.
    #[test]
    fn an_export_start_names_its_variant_for_the_worker() {
        let idle = IdleCompiles::new();
        assert!(!idle.export_starts(true), "no photo shown");
        {
            let mut queue = idle.lock();
            queue.exact = Some(5238847);
            queue.plan = [
                IdleJob::Display(7335999),
                IdleJob::Display(5238847),
                IdleJob::Deferred(5238847),
            ]
            .into();
        }
        assert!(!idle.export_starts(true), "no worker");
        idle.lock().worker = true;
        assert!(idle.export_starts(true));
        {
            let queue = idle.lock();
            assert_eq!(queue.export_job, Some(IdleJob::Deferred(5238847)));
            assert_eq!(
                queue.plan,
                [IdleJob::Display(7335999), IdleJob::Display(5238847)]
            );
        }
        // An 8-bit export: its display variant.
        assert!(idle.export_starts(false));
        {
            let queue = idle.lock();
            assert_eq!(queue.export_job, Some(IdleJob::Display(5238847)));
            assert_eq!(queue.plan, [IdleJob::Display(7335999)]);
        }
        // A processor that ends no longer holds an export.
        idle.cancel();
        assert_eq!(idle.lock().export_job, None);
        assert!(!idle.export_starts(true));
    }

    /// A frame waits for the planned variant that covers it even when the
    /// worker has not started it yet, and has it
    /// started at once (G76/WCOLD2: after Glow's own compile stopped the
    /// helper, Contrast compiled the whole variant in the app).
    #[test]
    fn a_frame_waits_for_the_planned_variant_that_covers_it() {
        let idle = IdleCompiles::new();
        let contrast = RAW_DEFAULT_PREVIEW_FEATURES | TONE_PREVIEW_FEATURE;
        assert_eq!(idle.covering(contrast), None, "nothing planned");
        {
            let mut queue = idle.lock();
            queue.worker = true;
            queue.plan = [IdleJob::Display(7335999), IdleJob::Display(5238847)].into();
        }
        assert_eq!(idle.covering(contrast), Some(7335999));
        assert!(idle.lock().rush, "the worker starts it now");
        // Halation is in no planned variant: its frame compiles its own.
        idle.lock().rush = false;
        assert_eq!(idle.covering(contrast | HALATION_FEATURE), None);
        assert!(!idle.lock().rush);
        // A running job that does not cover the frame: not the next one's
        // turn yet.
        idle.lock().running = Some(IdleJob::Deferred(4113));
        assert_eq!(idle.covering(contrast), None);
        idle.lock().running = Some(IdleJob::Display(7335999));
        assert_eq!(idle.covering(contrast), Some(7335999));
    }

    /// A frame no planned variant covers (Halation) waits for its own
    /// variant in a helper (`for_frame`), also while the helper still runs
    /// another job, which the worker stops for it; without helpers it
    /// compiles in the app as before (G76/WSMOKE20: 50.7 s in the app).
    #[test]
    fn a_frame_waits_for_its_own_variant_in_a_helper() {
        let idle = IdleCompiles::new();
        let halation = RAW_DEFAULT_PREVIEW_FEATURES | TONE_PREVIEW_FEATURE | HALATION_FEATURE;
        let own = 7335999 | HALATION_FEATURE;
        // No helper is set in unit tests.
        assert!(!idle.for_frame(own, || unreachable!()));
        assert!(idle.lock().frame_jobs.is_empty());
        let (planned, frame) = (IdleJob::Display(7335999), IdleJob::Display(own));
        let asked = 1;
        {
            let mut queue = idle.lock();
            queue.worker = true;
            queue.running = Some(planned);
            queue.frame_jobs = vec![(frame, asked)];
        }
        assert_eq!(idle.covering(halation), Some(own));
        // Grain on top is another variant: that frame asks for its own.
        assert_eq!(idle.covering(halation | GRAIN_FEATURE), None);
        let mut queue = idle.lock();
        let now = *queue.frame_asked(frame).unwrap();
        assert!(now >= asked);
        // The planned job's helper stops for the frame's; the frame's own
        // helper does not stop for itself or for a second frame's.
        assert!(queue.frame_goes_before(planned, now));
        assert!(!queue.frame_goes_before(frame, now));
        let grain = IdleJob::Display(own | GRAIN_FEATURE);
        queue.frame_jobs.push((grain, now));
        assert!(!queue.frame_goes_before(frame, now));
        // A frame that moved on: its job compiles in idle time, and the
        // helper running it stops for the frame that still waits.
        *queue.frame_asked(grain).unwrap() = now + 10_000;
        assert!(queue.frame_goes_before(frame, now + 10_000));
        assert_eq!(queue.frame_jobs, vec![(grain, now + 10_000)]);
        assert_eq!(queue.later, [frame]);
        queue.frames_moved_on(now + 20_000);
        assert!(queue.frame_jobs.is_empty());
        assert_eq!(queue.later, [grain, frame]);
        // A job served without a compile leaves the list.
        queue.frame_jobs.push((frame, now));
        IdleCompiles::served(&mut queue, frame, Source::Frame);
        assert!(queue.frame_jobs.is_empty());
    }

    /// Under a compile limit a Basic-panel edit made before the planned
    /// variant compiled waits for the idle worker to compile that one, at
    /// once, instead of compiling its own beside it.
    #[test]
    #[ignore = "GPU; sets the process-wide compile limit, so run it alone: cargo test -p rawtonic-gpu --lib -- --ignored --exact gpu_processing::idle_compiles::tests::an_edit_before_the_plan_compiled_waits_for_the_planned_variant"]
    fn an_edit_before_the_plan_compiled_waits_for_the_planned_variant() {
        use super::super::preview_tests::context;
        use std::time::Instant;
        set_compile_limit(Some(1));
        let processor = GpuProcessor::new(context(), 256, 256).unwrap();
        let photo = super::super::new_raw_edit(None);
        processor.preview_pipeline(&photo, true).unwrap();
        let (plan, _) = planned_jobs(preview_features(&photo), &photo);
        let mut contrast = photo;
        contrast.global.contrast = 0.2;
        let features = preview_features(&contrast);
        let own = export_display_features(&contrast);
        assert_ne!(own, plan[0]);
        // Right after the photo's frame: no idle time has passed.
        let started = Instant::now();
        let rendered_with: *const wgpu::ComputePipeline =
            processor.preview_pipeline(&contrast, true).unwrap().0;
        assert!(
            started.elapsed() < Duration::from_secs(120),
            "the Contrast frame waited {:?}",
            started.elapsed()
        );
        // The planned variant rendered it. (Its own variant is the plan's
        // next entry, which the worker may have begun by now: after a long
        // wait on a cold driver cache the quiet time has passed. That made
        // an earlier form of this test fail once on a Radeon 680M,
        // G76/WCOLD3.)
        let planned: *const wgpu::ComputePipeline = processor.preview_pipelines[plan[0]]
            .get()
            .expect("the planned variant compiled");
        assert!(
            std::ptr::eq(rendered_with, planned),
            "the frame did not render with the planned variant"
        );
        assert!(processor.preview_pipelines[features].get().is_none());
        drop(processor);
        std::thread::sleep(IDLE_QUIET + Duration::from_secs(2));
        set_compile_limit(None);
    }

    #[test]
    fn small_variants_are_those_within_the_raw_default_one() {
        for small in [0, 1, 16, 17, RAW_DEFAULT_PREVIEW_FEATURES] {
            assert!(small_display_variant(small), "{small}");
        }
        for heavy in [
            RAW_DEFAULT_PREVIEW_FEATURES | TONE_PREVIEW_FEATURE,
            RAW_DEFAULT_PREVIEW_FEATURES | GLOW_FEATURE,
            2,
            5238847,
        ] {
            assert!(!small_display_variant(heavy), "{heavy}");
        }
    }

    /// The program version both sides of `idle_compiles_in_a_helper_give_way`
    /// name their cache with, not this crate's (the app's differs from it:
    /// G76/WSMOKE15, a helper whose file the app never found).
    const TEST_BUILD: &str = "0.0.0-helper-test";

    /// The helper process of `idle_compiles_in_a_helper_give_way` (this test
    /// binary started again with `RAWTONIC_TEST_IDLE_HELPER`): a slow one
    /// only sleeps, a real one compiles its job. Without the variable it
    /// does nothing.
    #[test]
    #[ignore = "helper process of idle_compiles_in_a_helper_give_way"]
    fn idle_helper_process() {
        let Ok(spec) = std::env::var("RAWTONIC_TEST_IDLE_HELPER") else {
            return;
        };
        let mut parts = spec.splitn(3, '|');
        let (mode, job, folder) = (
            parts.next().unwrap(),
            parts.next().unwrap(),
            parts.next().unwrap(),
        );
        if mode == "slow" {
            std::thread::sleep(Duration::from_secs(120));
            std::process::exit(1);
        }
        let result =
            super::super::compile_in_helper_process(job, std::path::Path::new(folder), TEST_BUILD);
        std::process::exit(i32::from(result.is_err()));
    }

    /// Under a compile limit idle compiles run in a helper process: a frame
    /// that needs another variant than the running job stops the helper at
    /// once instead of waiting for it (G76/WCC11: 17–39 s on a Radeon 680M),
    /// and the job compiles in a later idle time through the helper's
    /// pipeline cache.
    #[test]
    #[ignore = "GPU; sets the process-wide compile limit and idle helper, so run it alone: cargo test -p rawtonic-gpu --lib -- --ignored --exact gpu_processing::idle_compiles::tests::idle_compiles_in_a_helper_give_way"]
    fn idle_compiles_in_a_helper_give_way() {
        use super::super::idle_compile_child::{DONE, STOPS};
        use super::super::preview_tests::get_all_adjustments_from_json;
        use std::sync::atomic::AtomicBool;
        use std::time::Instant;
        static SLOW: AtomicBool = AtomicBool::new(true);
        struct Eprint;
        impl log::Log for Eprint {
            fn enabled(&self, _: &log::Metadata) -> bool {
                true
            }
            fn log(&self, record: &log::Record) {
                if record.target().starts_with("rawtonic") {
                    eprintln!("[{}] {}", record.level(), record.args());
                }
            }
            fn flush(&self) {}
        }
        let _ = log::set_logger(&Eprint);
        log::set_max_level(log::LevelFilter::Info);
        let exe = std::env::current_exe().unwrap();
        super::super::set_idle_compile_helper(Box::new(move |job, folder| {
            let mode = if SLOW.load(Ordering::SeqCst) {
                "slow"
            } else {
                "real"
            };
            std::process::Command::new(&exe)
                .args([
                    "--exact",
                    "gpu_processing::idle_compiles::tests::idle_helper_process",
                    "--ignored",
                    "--nocapture",
                ])
                .env(
                    "RAWTONIC_TEST_IDLE_HELPER",
                    format!("{mode}|{job}|{}", folder.display()),
                )
                .stdout(std::process::Stdio::null())
                .spawn()
        }));
        let cache = tempfile::tempdir().unwrap();
        let context =
            GpuContext::headless_with_pipeline_cache_of(Some(cache.path()), TEST_BUILD).unwrap();
        if crate::pipeline_cache::file_name_of(&context.device).is_none() {
            // Helpers hand their pipeline over through a pipeline cache,
            // which only Vulkan has (helpers run only under a compile limit,
            // on AMD's Windows Vulkan driver).
            eprintln!("SKIP: no pipeline cache on this backend, so no idle helpers");
            return;
        }
        set_compile_limit(Some(1));
        let processor = GpuProcessor::new(context, 256, 256).unwrap();
        let idle = Arc::clone(&processor.idle_compiles.as_ref().unwrap().0);
        let recipe = |extra: serde_json::Value| {
            let mut recipe =
                serde_json::Value::Object(crate::image_processing::new_edit_responses(true));
            recipe
                .as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            get_all_adjustments_from_json(&recipe, true, Some(3))
        };
        let wait = |what: &str, seconds: u64, done: &dyn Fn() -> bool| {
            let deadline = Instant::now() + Duration::from_secs(seconds);
            while !done() {
                assert!(Instant::now() < deadline, "{what}");
                std::thread::sleep(Duration::from_millis(50));
            }
        };
        let photo = recipe(serde_json::json!({}));
        let shown = preview_features(&photo);
        processor.preview_pipeline(&photo, true).unwrap();
        let (plan, _) = planned_jobs(shown, &photo);
        wait("an idle helper starts", 60, &|| {
            idle.lock().running.is_some()
        });
        let running = idle.lock().running;
        assert_eq!(running, Some(IdleJob::Display(plan[0])));
        // Halation is in no planned variant: its frame's own variant
        // compiles in a helper of its own (a real one), which stops the
        // running helper, and the frame waits for it (G76/COLD2).
        let halation = recipe(serde_json::json!({"halationAmount": 30}));
        assert_ne!(
            preview_features(&halation) & plan[0],
            preview_features(&halation)
        );
        SLOW.store(false, Ordering::SeqCst);
        let own = export_display_features(&halation);
        let started = Instant::now();
        // Not on screen: the shown photo's plan stays.
        processor.preview_pipeline(&halation, false).unwrap();
        let waited = started.elapsed();
        assert!(STOPS.load(Ordering::SeqCst) >= 1, "the helper was stopped");
        assert!(
            DONE.load(Ordering::SeqCst) >= 1,
            "a helper compiled the halation frame's variant"
        );
        assert!(processor.preview_pipelines[own].get().is_some());
        eprintln!("the halation frame waited {waited:?} for its helper");
        // The stopped job compiles again in idle time, now in a real helper,
        // and serves the Contrast frame.
        wait("the first planned superset", 300, &|| {
            processor.preview_pipelines[plan[0]].get().is_some()
        });
        assert!(DONE.load(Ordering::SeqCst) >= 1, "a helper compiled it");
        // The helper started from the app's cache, and the app keeps the
        // helper's cache (its pipelines and the job's) as its own file
        // (G76/HCF), which a new cache reads back whole.
        let device = Arc::clone(&processor.context.device);
        if let Some(name) = crate::pipeline_cache::file_name_of(&device) {
            assert!(
                super::super::idle_compile_child::ADOPTED.load(Ordering::SeqCst) >= 1,
                "the app kept a helper's cache"
            );
            let (path, length) = crate::pipeline_cache::save_now(&device).unwrap();
            assert_eq!(path, cache.path().join(&name));
            assert!(length > 0);
            assert!(crate::pipeline_cache::foreign(&device, cache.path(), &name).is_some());
            // A NEW process that loads only that file (no helper folder, no
            // app pipelines in memory) gets the planned variant from it
            // without a compile: the file does not grow and the compile
            // takes a fraction of the helper's (WHCF run 2 by hand: 0.07 s
            // against 24–42 s).
            let file = cache.path().join(&name);
            let before = std::fs::metadata(&file).unwrap().len();
            let job = IdleJob::Display(plan[0]).arg();
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "gpu_processing::idle_compiles::tests::idle_helper_process",
                    "--ignored",
                    "--nocapture",
                ])
                .env(
                    "RAWTONIC_TEST_IDLE_HELPER",
                    format!("real|{job}|{}", cache.path().display()),
                )
                .stdout(std::process::Stdio::null())
                .status()
                .unwrap();
            assert!(status.success(), "the new process compiled: {status}");
            let seconds: f64 = std::fs::read_to_string(
                cache
                    .path()
                    .join(super::super::idle_compile_child::HELPER_COMPILE_FILE),
            )
            .unwrap()
            .trim()
            .parse()
            .unwrap();
            let after = std::fs::metadata(&file).unwrap().len();
            eprintln!(
                "new process: variant in {seconds:.2} s, cache file {before} -> {after} bytes"
            );
            assert_eq!(before, after, "the new process compiled the variant anew");
        }
        let contrast = recipe(serde_json::json!({"contrast": 20}));
        assert_eq!(
            preview_features(&contrast) & plan[0],
            preview_features(&contrast)
        );
        drop(processor);
        // The worker stops its helper and ends before the process does.
        wait("the idle worker ends", 30, &|| !idle.lock().worker);
        set_compile_limit(None);
    }

    /// Where idle compiles and AI network runs take turns (`memory_turns`,
    /// an integrated GPU under a compile limit): a run the photographer
    /// asked for stops an idle helper nobody waits for, no helper starts
    /// while a run is in progress, and a run made ahead waits for the
    /// helper (G76/WSMOKE20: 8.25–8.5 GB with both at once on a Radeon
    /// 680M).
    #[test]
    #[ignore = "GPU; sets the process-wide compile limit, idle helper and turns, so run it alone: cargo test -p rawtonic-gpu --lib -- --ignored --exact gpu_processing::idle_compiles::tests::an_idle_helper_and_an_ai_run_take_turns"]
    fn an_idle_helper_and_an_ai_run_take_turns() {
        use super::super::idle_compile_child::STOPS;
        use super::super::preview_tests::get_all_adjustments_from_json;
        use crate::memory_turns::{AiRun, ai_run};
        use std::time::Instant;
        let exe = std::env::current_exe().unwrap();
        // Helpers that only sleep: what matters is when they run.
        super::super::set_idle_compile_helper(Box::new(move |job, folder| {
            std::process::Command::new(&exe)
                .args([
                    "--exact",
                    "gpu_processing::idle_compiles::tests::idle_helper_process",
                    "--ignored",
                    "--nocapture",
                ])
                .env(
                    "RAWTONIC_TEST_IDLE_HELPER",
                    format!("slow|{job}|{}", folder.display()),
                )
                .stdout(std::process::Stdio::null())
                .spawn()
        }));
        let cache = tempfile::tempdir().unwrap();
        let context =
            GpuContext::headless_with_pipeline_cache_of(Some(cache.path()), TEST_BUILD).unwrap();
        if crate::pipeline_cache::file_name_of(&context.device).is_none() {
            eprintln!("SKIP: no pipeline cache on this backend, so no idle helpers");
            return;
        }
        set_compile_limit(Some(1));
        crate::memory_turns::enable();
        let processor = GpuProcessor::new(context, 256, 256).unwrap();
        let idle = Arc::clone(&processor.idle_compiles.as_ref().unwrap().0);
        let wait = |what: &str, seconds: u64, done: &dyn Fn() -> bool| {
            let deadline = Instant::now() + Duration::from_secs(seconds);
            while !done() {
                assert!(Instant::now() < deadline, "{what}");
                std::thread::sleep(Duration::from_millis(50));
            }
        };
        let recipe = serde_json::Value::Object(crate::image_processing::new_edit_responses(true));
        let photo = get_all_adjustments_from_json(&recipe, true, Some(3));
        processor.preview_pipeline(&photo, true).unwrap();
        let running = || idle.lock().running.is_some();
        wait("an idle helper starts", 60, &running);
        // A run the photographer asked for: the helper stops for it.
        let stops = STOPS.load(Ordering::SeqCst);
        let started = Instant::now();
        let asked = ai_run(AiRun::Asked);
        let waited = started.elapsed();
        assert!(
            waited < Duration::from_secs(10),
            "the asked run waited {waited:?} (the slow helper runs 120 s)"
        );
        assert!(
            STOPS.load(Ordering::SeqCst) > stops,
            "the helper was stopped"
        );
        // No helper starts while the run is in progress (the worker asks
        // every 250 ms once the quiet time has passed).
        let deadline = Instant::now() + IDLE_QUIET + Duration::from_secs(3);
        while Instant::now() < deadline {
            assert!(!running(), "a helper started beside the run");
            std::thread::sleep(Duration::from_millis(50));
        }
        drop(asked);
        wait("the helper starts again after the run", 30, &running);
        // A run made ahead waits for the helper...
        let (sender, ahead) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let turn = ai_run(AiRun::Ahead);
            let _ = sender.send(());
            drop(turn);
        });
        assert!(
            ahead.recv_timeout(Duration::from_secs(3)).is_err(),
            "a run made ahead started beside the helper"
        );
        assert!(running());
        // ...until the idle compiles end with their processor.
        drop(processor);
        wait("the idle worker ends", 30, &|| !idle.lock().worker);
        assert!(ahead.recv_timeout(Duration::from_secs(10)).is_ok());
        set_compile_limit(None);
    }

    /// Under a compile limit a frame no planned variant covers (effects on
    /// an edited photo) compiles the variant its export renders with, and
    /// the export then compiles nothing (G76/WCC7).
    #[test]
    #[ignore = "GPU; sets the process-wide compile limit, so run it alone: cargo test -p rawtonic-gpu --lib -- --ignored --exact gpu_processing::idle_compiles::tests::an_export_takes_the_variant_its_frame_compiled"]
    fn an_export_takes_the_variant_its_frame_compiled() {
        use super::super::preview_tests::{context, get_all_adjustments_from_json, input};
        use std::time::Instant;
        set_compile_limit(Some(1));
        let context = context();
        let processor = GpuProcessor::new(context.clone(), 256, 256).unwrap();
        let mut recipe =
            serde_json::Value::Object(crate::image_processing::new_edit_responses(true));
        recipe.as_object_mut().unwrap().extend(
            serde_json::json!({"contrast": 20, "clarity": 25, "glowAmount": 30})
                .as_object()
                .unwrap()
                .clone(),
        );
        let edit = get_all_adjustments_from_json(&recipe, true, Some(3));
        let features = preview_features(&edit);
        let exported = export_display_features(&edit);
        assert_ne!(features, exported, "the export's variant is a superset");
        processor.preview_pipeline(&edit, true).unwrap();
        assert!(processor.preview_pipelines[exported].get().is_some());
        assert!(processor.preview_pipelines[features].get().is_none());
        let view = input(&context, 256, 256, 7);
        let started = Instant::now();
        processor
            .run(
                &view,
                256,
                256,
                RenderRequest {
                    adjustments: edit,
                    mask_bitmaps: &[],
                    lut: None,
                    roi: None,
                },
                false,
                RenderOutputPrecision::EightBit,
            )
            .unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the export waited {:?}",
            started.elapsed()
        );
        assert!(!compiles_pending(), "the export started a compile");
        drop(processor);
        // The idle worker ends after its quiet time; exiting while it still
        // holds the device crashed the test process on NVIDIA's Linux driver.
        std::thread::sleep(IDLE_QUIET + Duration::from_secs(2));
        set_compile_limit(None);
    }

    /// While set, the worker holds a frame's job (`Source::Frame`) before
    /// its compile as a slow helper would: stopped as a helper is
    /// (`stop`), else compiled once it is cleared.
    static SLOW_FRAME_COMPILES: std::sync::atomic::AtomicBool =
        std::sync::atomic::AtomicBool::new(false);

    /// `SLOW_FRAME_COMPILES`'s hold; whether the job was stopped meanwhile.
    pub(super) fn slow_frame_compile(stop: &dyn Fn() -> bool) -> bool {
        while SLOW_FRAME_COMPILES.load(Ordering::SeqCst) {
            if stop() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    /// A frame that waits for its own variant in a (slow) helper gives way
    /// to a newer request (G76/SUPS: the first Halation frame on a cold
    /// Radeon 680M waited ~53 s, and Halation 0 showed only afterwards):
    /// the wait ends with `FRAME_SUPERSEDED`, the next frame (Halation
    /// back to 0) renders at once with the compiled planned variant, and
    /// the abandoned job keeps compiling as an idle job, which serves a
    /// later Halation frame. Runs on every backend: without a pipeline
    /// cache (Metal) the worker compiles its jobs in the app.
    #[test]
    #[ignore = "GPU; sets the process-wide compile limit and idle helper, so run it alone: cargo test -p rawtonic-gpu --lib -- --ignored --exact gpu_processing::idle_compiles::tests::a_newer_frame_replaces_one_that_waits_for_a_helper"]
    fn a_newer_frame_replaces_one_that_waits_for_a_helper() {
        use super::super::frame_supersession::{is_superseded, supersedable};
        use super::super::preview_tests::get_all_adjustments_from_json;
        use std::sync::atomic::AtomicBool;
        use std::time::Instant;
        let exe = std::env::current_exe().unwrap();
        super::super::set_idle_compile_helper(Box::new(move |job, folder| {
            std::process::Command::new(&exe)
                .args([
                    "--exact",
                    "gpu_processing::idle_compiles::tests::idle_helper_process",
                    "--ignored",
                    "--nocapture",
                ])
                .env(
                    "RAWTONIC_TEST_IDLE_HELPER",
                    format!("real|{job}|{}", folder.display()),
                )
                .stdout(std::process::Stdio::null())
                .spawn()
        }));
        let cache = tempfile::tempdir().unwrap();
        let context =
            GpuContext::headless_with_pipeline_cache_of(Some(cache.path()), TEST_BUILD).unwrap();
        set_compile_limit(Some(1));
        let processor = GpuProcessor::new(context, 256, 256).unwrap();
        let idle = Arc::clone(&processor.idle_compiles.as_ref().unwrap().0);
        let recipe = |extra: serde_json::Value| {
            let mut recipe =
                serde_json::Value::Object(crate::image_processing::new_edit_responses(true));
            recipe
                .as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            get_all_adjustments_from_json(&recipe, true, Some(3))
        };
        let wait = |what: &str, seconds: u64, done: &dyn Fn() -> bool| {
            let deadline = Instant::now() + Duration::from_secs(seconds);
            while !done() {
                assert!(Instant::now() < deadline, "{what}");
                std::thread::sleep(Duration::from_millis(50));
            }
        };
        let photo = recipe(serde_json::json!({}));
        processor.preview_pipeline(&photo, true).unwrap();
        let (plan, _) = planned_jobs(preview_features(&photo), &photo);
        wait("the first planned superset", 300, &|| {
            processor.preview_pipelines[plan[0]].get().is_some()
        });
        SLOW_FRAME_COMPILES.store(true, Ordering::SeqCst);
        let halation = recipe(serde_json::json!({"halationAmount": 37}));
        let own = export_display_features(&halation);
        assert_ne!(own & plan[0], own, "Halation is in no planned variant");
        // The newer request arrives once the helper holds the frame's job.
        let newer = Arc::new(AtomicBool::new(false));
        let arrived = Arc::new(Mutex::new(None::<Instant>));
        let asker = {
            let (idle, newer, arrived) =
                (Arc::clone(&idle), Arc::clone(&newer), Arc::clone(&arrived));
            std::thread::spawn(move || {
                let deadline = Instant::now() + Duration::from_secs(300);
                while idle.lock().running != Some(IdleJob::Display(own)) {
                    assert!(Instant::now() < deadline, "the frame's job never ran");
                    std::thread::sleep(Duration::from_millis(20));
                }
                std::thread::sleep(Duration::from_millis(300));
                *arrived.lock().unwrap() = Some(Instant::now());
                newer.store(true, Ordering::SeqCst);
            })
        };
        let check = Arc::clone(&newer);
        let waited = supersedable(
            move || check.load(Ordering::SeqCst),
            || processor.preview_pipeline(&halation, true).map(|_| ()),
        );
        let ended = Instant::now();
        asker.join().unwrap();
        let error = waited.expect_err("the waiting frame gave way");
        assert!(is_superseded(&error), "{error}");
        let after = ended.duration_since(arrived.lock().unwrap().unwrap());
        eprintln!("the waiting frame ended {after:?} after the newer request");
        assert!(
            after < Duration::from_millis(500),
            "it ended {after:?} late"
        );
        assert!(processor.preview_pipelines[own].get().is_none());
        // Halation back to 0: the planned variant serves it at once.
        let started = Instant::now();
        processor.preview_pipeline(&photo, true).unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "{:?}",
            started.elapsed()
        );
        // No frame asks for the abandoned job any more: it goes on as an
        // idle job (not stopped: nothing else needs the slot).
        wait("the abandoned job moves to idle time", 10, &|| {
            let mut queue = idle.lock();
            queue.frames_moved_on(now_ms());
            queue.frame_jobs.is_empty() && queue.later.contains(&IdleJob::Display(own))
        });
        assert_eq!(idle.lock().running, Some(IdleJob::Display(own)));
        SLOW_FRAME_COMPILES.store(false, Ordering::SeqCst);
        wait("the abandoned job compiles in idle time", 300, &|| {
            processor.preview_pipelines[own].get().is_some()
        });
        // A later Halation frame finds it.
        let started = Instant::now();
        processor.preview_pipeline(&halation, true).unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "{:?}",
            started.elapsed()
        );
        drop(processor);
        wait("the idle worker ends", 60, &|| !idle.lock().worker);
        std::thread::sleep(IDLE_QUIET + Duration::from_secs(2));
        set_compile_limit(None);
    }
}
