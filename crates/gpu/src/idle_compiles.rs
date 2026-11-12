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
//! Highlights, a curve and a mask at once. While a superset serves a frame,
//! the frame's own variant is compiled here too, in idle time, instead of
//! holding the compile slot for a minute right after the slider moved.
//!
//! Idle compiles show nothing (they are not counted in `compiles_pending`):
//! no photo waits for them. A frame that does wait for the running one
//! counts as a pending compile for that time (`IdleCompiles::covering`).

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

/// One pipeline compiled in idle time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum IdleJob {
    /// The display variant of these features.
    Display(usize),
    /// The display variant of these features reading the layer sums.
    LayerSums(usize),
}

/// The display variants worth compiling ahead for a photo shown with
/// display variant `features`, most likely first: the tone sliders with
/// Highlights (Contrast, Shadows lowered, Whites raised, Blacks raised,
/// Highlights lowered), then the same with a curve and a mask layer (each
/// covers the ones before it, and the photo's own features), then the
/// photo's features with the effects (glow, halation, grain), then the
/// tone sliders with raised Shadows of the newer tone versions
/// (`SHADOWS_V13_FEATURE`).
///
/// Measured on a Radeon 680M (compile_probe round 12, AMD Windows Vulkan
/// 25.10; display variants with the one-call-site tone gains): the RAW
/// default with the sliders, curve and mask 37.9 s at 3.0 GB, with the
/// effects 37 s at 2.8 GB, the sliders with raised Shadows 58 s at 5.4 GB.
/// Lowered Whites and Blacks of the newer tone versions
/// (`WHITES_V12_FEATURE`, `BLACKS_V16_FEATURE`) are left out: with all
/// three the variant took 131 s at 11.1 GB.
pub(super) fn idle_plan(features: usize, adjustments: &AllAdjustments) -> Vec<usize> {
    let g = &adjustments.global;
    let lowered_highlights = if g.tonemapper_mode == 3
        && ToneVersion::from_uniform(g.tone_version).highlights_edge_base()
    {
        HIGHLIGHTS_V14_FEATURE
    } else {
        0
    };
    let sliders = features | TONE_PREVIEW_FEATURE | 2 | lowered_highlights;
    let local = sliders | 4 | 8;
    let effects = features | EFFECTS_FEATURE;
    let raised_shadows = if ToneVersion::from_uniform(g.tone_version).shadows_midtone_cut() {
        sliders | SHADOWS_V13_FEATURE
    } else {
        sliders
    };
    let mut plan = Vec::with_capacity(4);
    for candidate in [sliders, local, effects, raised_shadows] {
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
    /// The features the plan was made for.
    planned: Option<usize>,
    /// The planned supersets (replaced when another photo is shown).
    plan: VecDeque<IdleJob>,
    /// Frames' own variants that a superset serves until then.
    later: VecDeque<IdleJob>,
    /// The job compiling now.
    running: Option<IdleJob>,
    worker: bool,
    cancelled: bool,
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
        if queue.cancelled || queue.planned == Some(features) {
            return;
        }
        queue.planned = Some(features);
        queue.plan = idle_plan(features, adjustments)
            .into_iter()
            .map(IdleJob::Display)
            .collect();
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
                let slot = self
                    .idle()
                    .then(|| background_compile_slot("idle compile"))
                    .flatten();
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

    #[test]
    fn plan_adds_tone_highlights_then_curve_and_mask() {
        let raw = RAW_DEFAULT_PREVIEW_FEATURES;
        let newest = adjustments(crate::image_processing::TONE_VERSION as f32, 3);
        let sliders = raw | TONE_PREVIEW_FEATURE | 2 | HIGHLIGHTS_V14_FEATURE;
        assert_eq!(
            idle_plan(raw, &newest),
            vec![
                sliders,
                sliders | 4 | 8,
                raw | EFFECTS_FEATURE,
                sliders | SHADOWS_V13_FEATURE
            ]
        );
        // The measured variants: 4113 → 36915, 36927, 2101265, 53299.
        assert_eq!(idle_plan(4113, &newest), vec![36915, 36927, 2101265, 53299]);
        for planned in idle_plan(raw, &newest) {
            assert_eq!(planned & raw, raw, "a superset of the photo's features");
            assert_eq!(planned & (WHITES_V12_FEATURE | BLACKS_V16_FEATURE), 0);
        }
    }

    #[test]
    fn plan_keeps_older_highlights_and_skips_the_photos_own_variant() {
        // Another look: Highlights keeps the version-13 code.
        let other_look = adjustments(crate::image_processing::TONE_VERSION as f32, 0);
        let (effects, shadows) = (EFFECTS_FEATURE, SHADOWS_V13_FEATURE);
        assert_eq!(
            idle_plan(0, &other_look),
            vec![34, 46, effects, 34 | shadows]
        );
        // A photo already shown with the first superset skips it.
        assert_eq!(
            idle_plan(34, &other_look),
            vec![46, 34 | effects, 34 | shadows]
        );
        let all = 46 | effects | shadows;
        assert_eq!(idle_plan(all, &other_look), Vec::<usize>::new());
        // Tone versions before raised Shadows' newer response plan no
        // separate variant for it.
        let old = adjustments(6.0, 0);
        assert_eq!(idle_plan(34, &old), vec![46, 34 | effects]);
    }

    /// Under a compile limit the shown photo plans its supersets, which
    /// compile in idle time; the first Contrast frame then renders at once
    /// without a compile of its own, and its own variant follows in idle
    /// time.
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
        assert_eq!(plan.len(), 4);
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
        wait("Contrast's own variant", &|| {
            processor.preview_pipelines[features].get().is_some()
        });
        drop(processor);
        set_compile_limit(None);
    }
}
