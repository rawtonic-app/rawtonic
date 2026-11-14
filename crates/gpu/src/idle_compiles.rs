//! Display variants compiled in idle time under a compile limit (G76/WCC3).
//!
//! On AMD's Windows Vulkan driver (the only one with a compile limit,
//! `device_start::compile_limit`) the first use of a tone slider waited for
//! its display variant: 56–74 s at 5–6 GB of private memory on a Radeon
//! 680M, once per machine and driver (the pipeline cache keeps it). After
//! the first photo is shown, this compiles the variants a photographer most
//! likely needs next while the app is idle, one at a time and only when no
//! other compile waits or runs, so the first slider use finds one ready.
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

/// Why the app does not allow an idle compile now (`Err`: short of memory,
/// an export runs), asked before each.
pub type IdleCompileMemoryGuard = Box<dyn Fn() -> Result<(), String> + Send + Sync>;

static MEMORY_GUARD: std::sync::OnceLock<IdleCompileMemoryGuard> = std::sync::OnceLock::new();

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

/// The display variants worth compiling ahead for a photo shown with
/// display variant `features`, a chain of supersets, each covering the ones
/// before it (a frame renders with the first compiled superset of its
/// features):
///
/// 1. `sliders`: the tone sliders with Highlights (Contrast, Shadows
///    lowered, Whites and Blacks raised, Highlights in both directions);
/// 2. `basic`: every Basic-panel edit the recipe's versions make: the tone
///    sliders in both directions (raised Shadows, lowered Whites and Blacks
///    of the newer tone versions), a curve, a mask layer, Clarity or
///    Structure, Dehaze, Vibrance and the colour mixer.
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
    let sliders = sliders_features(features, adjustments);
    let basic = basic_features(features, adjustments);
    let mut plan = Vec::with_capacity(2);
    for candidate in [sliders, basic] {
        if candidate != features && !plan.contains(&candidate) {
            plan.push(candidate);
        }
    }
    plan
}

/// What `IdleCompiles::plan` queues for a photo shown with display variant
/// `features`: `idle_plan`, then the display variant its exports render
/// with under a compile limit (`export_display_features`) if the plan lacks
/// it, and that variant. A frame keeps rendering with any compiled superset
/// of its features, so after Glow is turned off, or on a photo shown after
/// another one's wider variant, no planned entry may compile; an export
/// would then compile its own variant cold (G76/WCC7 review: tens of
/// seconds and 4–6 GB on a Radeon 680M). That entry is compiled even when a
/// superset covers it (`covered_is_enough`).
///
/// Last, `basic` with Glow (`glow_features`): Glow turned on after Basic-panel
/// edits waited 66 s for that variant on a Radeon 680M (G76/WSMOKE14; 60 s /
/// 4.7 GB alone, probe round 16); compiled ahead, the first Glow frame and
/// a Glow export find it. Halation and Grain are not planned.
pub(super) fn planned_jobs(features: usize, adjustments: &AllAdjustments) -> (Vec<usize>, usize) {
    let mut plan = idle_plan(features, adjustments);
    let exact = export_display_features(adjustments);
    for entry in [exact, glow_features(features, adjustments)] {
        if !plan.contains(&entry) {
            plan.push(entry);
        }
    }
    (plan, exact)
}

/// `planned_jobs`' last entry: every Basic-panel edit with Glow, the
/// variant an edited photo with Glow renders and exports with
/// (`export_display_features`).
fn glow_features(features: usize, adjustments: &AllAdjustments) -> usize {
    basic_features(features, adjustments) | GLOW_FEATURE
}

/// Whether a compiled superset makes the planned display variant
/// `features` needless (`covered`: one is compiled); never for the one
/// exports render with (`exact`).
fn covered_is_enough(
    features: usize,
    exact: Option<usize>,
    covered: impl FnOnce() -> bool,
) -> bool {
    exact != Some(features) && covered()
}

/// `idle_plan`'s first entry: `features` with the tone sliders and
/// Highlights.
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

/// `idle_plan`'s second entry: `features` with every Basic-panel edit the
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
    worker: bool,
    cancelled: bool,
}

impl Queue {
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

pub(super) struct IdleCompiles {
    queue: Mutex<Queue>,
    changed: Condvar,
    /// Milliseconds since `since_start`'s start of the last frame rendered
    /// with a display variant.
    last_frame_ms: AtomicU64,
}

impl IdleCompiles {
    pub(super) fn new() -> Arc<Self> {
        let this = Arc::new(Self {
            queue: Mutex::new(Queue::default()),
            changed: Condvar::new(),
            last_frame_ms: AtomicU64::new(0),
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

    /// A frame is rendering: idle compiles wait `IDLE_QUIET` after it.
    pub(super) fn frame(&self) {
        self.last_frame_ms
            .store(now_ms(), std::sync::atomic::Ordering::Relaxed);
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

    /// The display variant compiling now if it covers `features`: a frame
    /// of those features renders with it once it is done.
    pub(super) fn covering(&self, features: usize) -> Option<usize> {
        match self.lock().running {
            Some(IdleJob::Display(running)) if running & features == features => Some(running),
            _ => None,
        }
    }

    /// Ends the worker after the compile it runs (if any); nothing new
    /// starts. A driver call cannot be stopped midway.
    pub(super) fn cancel(&self) {
        self.lock().cancelled = true;
        self.changed.notify_all();
    }

    /// The next job, and whether it is a planned superset (one a compiled
    /// superset makes needless).
    fn next(queue: &mut Queue) -> Option<(IdleJob, bool)> {
        queue
            .plan
            .pop_front()
            .map(|job| (job, true))
            .or_else(|| queue.later.pop_front().map(|job| (job, false)))
    }

    /// Whether the app has been idle for `IDLE_QUIET`: no frame and no
    /// compile waiting or running.
    fn idle(&self) -> bool {
        let last_frame = self.last_frame_ms.load(Ordering::Relaxed);
        let quiet = IDLE_QUIET.as_millis() as u64;
        now_ms().saturating_sub(last_frame) >= quiet
            && now_ms().saturating_sub(LAST_COMPILE_MS.load(Ordering::Relaxed)) >= quiet
    }

    fn work(self: Arc<Self>, target: IdleTarget) {
        // Why the memory guard holds the worker, while it does.
        let mut held: Option<String> = None;
        loop {
            // Wait for a job, then for idle time and a free slot.
            let slot = {
                let mut queue = self.lock();
                loop {
                    if queue.cancelled {
                        queue.worker = false;
                        return;
                    }
                    if !queue.plan.is_empty() || !queue.later.is_empty() {
                        break;
                    }
                    queue = self.changed.wait(queue).unwrap_or_else(|e| e.into_inner());
                }
                drop(queue);
                if !self.idle() {
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
                match memory_allows() {
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
                slot
            };
            let (job, planned) = {
                let mut queue = self.lock();
                if queue.cancelled {
                    queue.worker = false;
                    return;
                }
                let Some((job, planned)) = Self::next(&mut queue) else {
                    continue;
                };
                // Already compiled or compiling; a planned superset also
                // when a compiled one covers it, unless exports render with
                // it.
                let needless = match job {
                    IdleJob::Display(features) => {
                        planned
                            && covered_is_enough(features, queue.exact, || {
                                target
                                    .pipelines
                                    .supersets(features)
                                    .any(|p| p.get().is_some())
                            })
                    }
                    _ => false,
                };
                if needless || !Self::unclaimed(&target, job) {
                    continue;
                }
                queue.running = Some(job);
                (job, planned)
            };
            let work = CompileWork::new();
            // In a helper process first, which a compile or an export that
            // waits for the slot stops (`idle_compile_child`); the job is
            // claimed only when it compiles here.
            let helped = super::idle_compile_child::compile_in_helper(&target, job, || {
                self.lock().cancelled
            });
            match helped {
                super::idle_compile_child::Helped::Stopped => {
                    let mut queue = self.lock();
                    queue.running = None;
                    if planned {
                        queue.plan.push_front(job);
                    } else {
                        queue.later.push_front(job);
                    }
                    drop(queue);
                    drop(work);
                    drop(slot);
                    continue;
                }
                super::idle_compile_child::Helped::Compiled(cache) => {
                    if Self::claim(&target, job) {
                        super::idle_compile_child::HELPER_CACHE
                            .with(|helper| *helper.borrow_mut() = Some(cache));
                        self.compile(&target, job);
                        super::idle_compile_child::HELPER_CACHE
                            .with(|helper| *helper.borrow_mut() = None);
                    }
                }
                super::idle_compile_child::Helped::Unavailable => {
                    if Self::claim(&target, job) {
                        self.compile(&target, job);
                    }
                }
            }
            drop(work);
            drop(slot);
            self.lock().running = None;
        }
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

/// When the last compile other than an idle one ended (or started).
static LAST_COMPILE_MS: AtomicU64 = AtomicU64::new(0);

/// Notes a compile other than an idle one (`validated`).
pub(super) fn compile_activity() {
    LAST_COMPILE_MS.store(now_ms(), Ordering::Relaxed);
}

thread_local! {
    /// Set while this thread runs an idle compile under its slot: its
    /// creations neither wait for another slot nor count as pending.
    pub(super) static BACKGROUND_COMPILE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
mod tests {
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
            assert_eq!(chosen, plan[1], "{edits}");
            assert_eq!(chosen & features, features, "{edits}");
        }
        // Effects on an edited photo: the frame compiles the same superset
        // (no planned variant covers it), with the effects.
        let mut glow = edit;
        glow["glowAmount"] = serde_json::json!(30);
        assert_eq!(export(glow), plan[1] | GLOW_FEATURE);
    }

    /// A wider compiled variant keeps serving the frames after Glow is
    /// turned off, or on the next photo, so no other planned entry
    /// compiles; the variant exports render with still does (Codex review
    /// of G76/WCC7).
    #[test]
    fn the_export_variant_compiles_although_a_wider_one_serves_the_frames() {
        let edit = serde_json::json!({"exposure": 0.4, "contrast": 25, "highlights": -40,
            "clarity": 20, "vibrance": 20});
        let mut glow = edit.clone();
        glow["glowAmount"] = serde_json::json!(30);
        let compiled = export_display_features(&new_raw_edit(glow));
        let check = |adjustments: &AllAdjustments| {
            let features = preview_features(adjustments);
            let (plan, exact) = planned_jobs(features, adjustments);
            assert_eq!(exact, export_display_features(adjustments));
            assert!(plan.contains(&exact));
            // The Glow variant covers every entry, the export's too ...
            assert!(plan.iter().all(|&p| compiled & p == p), "{plan:?}");
            // ... yet only the export's is compiled.
            let compiles = plan
                .iter()
                .filter(|&&p| !covered_is_enough(p, Some(exact), || compiled & p == p))
                .collect::<Vec<_>>();
            assert_eq!(compiles, vec![&exact]);
        };
        // Glow turned off: the Basic entry.
        check(&new_raw_edit(edit));
        // Another, unedited photo: its own variant (not an `idle_plan`
        // entry).
        let unedited = new_raw_edit(serde_json::json!({}));
        assert!(
            !idle_plan(preview_features(&unedited), &unedited)
                .contains(&export_display_features(&unedited))
        );
        check(&unedited);
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
        assert_eq!(jobs.last(), Some(&glow));
        assert_eq!(glow, idle_plan(opened, &shown)[1] | GLOW_FEATURE);
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

    #[test]
    fn plan_climbs_from_the_tone_sliders_to_every_basic_edit() {
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
        assert_eq!(plan, vec![sliders, basic]);
        // Each covers the ones before it and the photo's own features.
        assert_eq!(basic & sliders, sliders);
        assert_eq!(sliders & features, features);
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
        assert_eq!(idle_plan(0, &other_look), vec![34, basic]);
        // A photo already shown with the first superset skips it.
        assert_eq!(idle_plan(34, &other_look), vec![basic]);
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
        // Sliders and Basic panel; no all-effects entry (G76/WCC8).
        assert_eq!(plan.len(), 2);
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
        let plan = idle_plan(shown, &photo);
        wait("an idle helper starts", 60, &|| {
            idle.lock().running.is_some()
        });
        let running = idle.lock().running;
        assert_eq!(running, Some(IdleJob::Display(plan[0])));
        // Glow is in no running job: its frame compiles its own variant.
        let glow = recipe(serde_json::json!({"glowAmount": 30}));
        assert_ne!(preview_features(&glow) & plan[0], preview_features(&glow));
        let started = Instant::now();
        // Not on screen: the shown photo's plan stays.
        processor.preview_pipeline(&glow, false).unwrap();
        let waited = started.elapsed();
        assert!(STOPS.load(Ordering::SeqCst) >= 1, "the helper was stopped");
        assert!(
            waited < Duration::from_secs(60),
            "the Glow frame waited {waited:?} (the slow helper runs 120 s)"
        );
        // The stopped job compiles again in idle time, now in a real helper,
        // and serves the Contrast frame.
        SLOW.store(false, Ordering::SeqCst);
        wait("the first planned superset", 300, &|| {
            processor.preview_pipelines[plan[0]].get().is_some()
        });
        assert!(DONE.load(Ordering::SeqCst) >= 1, "a helper compiled it");
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
}
