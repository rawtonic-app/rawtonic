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
//! compiles held the slot for minutes after the edits, G76/WGT14). Layer-sum
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
///    Structure, Dehaze, Vibrance and the colour mixer;
/// 3. the photo's features with the effects (glow, halation, grain).
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
    let g = &adjustments.global;
    let tone = ToneVersion::from_uniform(g.tone_version);
    let bit = |on: bool, bit: usize| if on { bit } else { 0 };
    let sliders = features
        | TONE_PREVIEW_FEATURE
        | 2
        | bit(
            g.tonemapper_mode == 3 && tone.highlights_edge_base(),
            HIGHLIGHTS_V14_FEATURE,
        );
    let dehaze = DehazeVersion::from_uniform(g.dehaze_version);
    let basic = sliders
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
        | bit(g.hsl[0].response > 0.5, HSL_V2_FEATURE);
    let effects = features | EFFECTS_FEATURE;
    let mut plan = Vec::with_capacity(3);
    for candidate in [sliders, basic, effects] {
        if candidate != features && !plan.contains(&candidate) {
            plan.push(candidate);
        }
    }
    plan
}

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
    fn replan(&mut self, plan: Vec<usize>) -> bool {
        if self.planned.as_ref() == Some(&plan) {
            return false;
        }
        self.plan = plan.iter().copied().map(IdleJob::Display).collect();
        self.planned = Some(plan);
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
        Arc::new(Self {
            queue: Mutex::new(Queue::default()),
            changed: Condvar::new(),
            last_frame_ms: AtomicU64::new(0),
        })
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
        if queue.cancelled || !queue.replan(idle_plan(features, adjustments)) {
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
                match memory_allows() {
                    Ok(()) => {
                        if let Some(reason) = held.take() {
                            log::info!("GPU idle compiles resumed (held: {reason})");
                        }
                    }
                    Err(reason) => {
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
                let slot = background_compile_slot("idle compile");
                let Some(slot) = slot else {
                    let queue = self.lock();
                    let _ = self
                        .changed
                        .wait_timeout(queue, IDLE_POLL)
                        .unwrap_or_else(|e| e.into_inner());
                    continue;
                };
                slot
            };
            let job = {
                let mut queue = self.lock();
                if queue.cancelled {
                    queue.worker = false;
                    return;
                }
                let Some((job, planned)) = Self::next(&mut queue) else {
                    continue;
                };
                // Already compiled or compiling; a planned superset also
                // when a compiled one covers it.
                let claimed = match job {
                    IdleJob::Display(features) => {
                        (planned
                            && target
                                .pipelines
                                .supersets(features)
                                .any(|p| p.get().is_some()))
                            || target.compilation[features].set(()).is_err()
                    }
                    IdleJob::LayerSums(features) => {
                        target.layer_sum_compilation[features].set(()).is_err()
                    }
                };
                if claimed {
                    continue;
                }
                queue.running = Some(job);
                job
            };
            self.compile(&target, job);
            drop(slot);
            self.lock().running = None;
        }
    }

    fn compile(&self, target: &IdleTarget, job: IdleJob) {
        let (features, layer_sums) = match job {
            IdleJob::Display(features) => (features, false),
            IdleJob::LayerSums(features) => (features, true),
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
        assert_eq!(plan, vec![sliders, basic, features | EFFECTS_FEATURE]);
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
                    IdleJob::LayerSums(_) => unreachable!(),
                })
                .collect::<Vec<_>>()
        };
        let mut queue = Queue::default();
        assert!(queue.replan(plan_3.clone()));
        assert_eq!(planned(&queue), plan_3);
        // The worker took the first job; the same photo again keeps the rest.
        queue.plan.pop_front();
        assert!(!queue.replan(plan_3.clone()));
        assert_eq!(planned(&queue), plan_3[1..]);
        assert!(queue.replan(plan_2.clone()));
        assert_eq!(planned(&queue), plan_2);
        // A look change (Camera Raw to AgX) with the same features too.
        let agx = adjustments(crate::image_processing::TONE_VERSION as f32, 1);
        assert_eq!(preview_features(&agx), features);
        assert!(queue.replan(idle_plan(features, &agx)));
    }

    #[test]
    fn plan_keeps_older_responses_and_skips_the_photos_own_variant() {
        // Another look: Highlights keeps the version-13 code; versions
        // before raised Shadows' newer response plan no bit for it.
        let other_look = adjustments(crate::image_processing::TONE_VERSION as f32, 0);
        let (sh, wh, bl) = (SHADOWS_V13_FEATURE, WHITES_V12_FEATURE, BLACKS_V16_FEATURE);
        let basic = 46 | sh | wh | bl;
        assert_eq!(idle_plan(0, &other_look), vec![34, basic, EFFECTS_FEATURE]);
        // A photo already shown with the first superset skips it.
        assert_eq!(
            idle_plan(34, &other_look),
            vec![basic, 34 | EFFECTS_FEATURE]
        );
        let all = basic | EFFECTS_FEATURE;
        assert_eq!(idle_plan(all, &other_look), Vec::<usize>::new());
        // Tone version 6: none of the newer tone directions' code.
        let old = adjustments(6.0, 0);
        assert_eq!(idle_plan(34, &old), vec![46, 34 | EFFECTS_FEATURE]);
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
        assert_eq!(plan.len(), 3);
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
}
