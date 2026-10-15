// SPDX-License-Identifier: AGPL-3.0-or-later
//! How long the masking models stay in memory (user decision 2026-10-10).
//!
//! Subject (BiRefNet; on Core ML about 0.5 GB, 1.6 GB more once it has run)
//! and Sky, SAM 2.1, depth, MI-GAN and the face models are loaded when a photo opens in
//! Develop for someone who used the masking tools before (the webview asks,
//! `useAiMasking`; not while memory is short), or on first use. They are
//! dropped again, with what was computed from them (SAM's embedding, the
//! depth map, the faces found):
//! - after `IDLE_UNLOAD` without masking use, when no computation runs;
//! - when the Library is shown (the webview asks: `unload_ai_models`);
//! - at once on memory pressure (`memory_pressure`), then also the tagging
//!   models and the finished Subject/Sky masks, and background runs are
//!   cancelled.
//!
//! Unloading only drops the app's references: a computation that holds a
//! model finishes with it, and the model is freed when it ends. A load that
//! finishes after an unload hands its model to its caller but does not keep
//! it (`Lifecycle::keep`), so the next use loads again. AI Denoise is not
//! managed here (its model is loaded with the photos that use it).

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tauri::{Emitter, Manager};

use crate::ai_processing::AiState;
use crate::app_state::AppState;

/// Models unused this long are dropped.
pub const IDLE_UNLOAD: Duration = Duration::from_secs(5 * 60);
/// How often the idle time is checked.
const IDLE_CHECK: Duration = Duration::from_secs(15);
/// Event sent to the webview after an unload, with the `UnloadReason`.
pub const UNLOADED_EVENT: &str = "ai-models-unloaded";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UnloadReason {
    /// `IDLE_UNLOAD` without masking use.
    Idle,
    /// The Library was shown.
    Library,
    /// The system is short of memory.
    MemoryPressure,
}

#[derive(Default)]
struct Usage {
    /// Last masking use, or when the models were loaded; `None` after an
    /// unload until the next use or load.
    last_use: Option<Instant>,
    /// Computations running with a model.
    busy: usize,
    /// Counts unloads; a load that started before one is not kept.
    generation: u64,
}

/// Use, running computations and unloads of the masking models.
pub struct Lifecycle {
    usage: Mutex<Usage>,
}

/// The app's lifecycle.
pub static LIFECYCLE: Lifecycle = Lifecycle::new();

/// A computation with a model is running while this lives (no idle unload).
pub struct Running<'a>(&'a Lifecycle);

impl Drop for Running<'_> {
    fn drop(&mut self) {
        let mut usage = self.0.usage();
        usage.busy = usage.busy.saturating_sub(1);
    }
}

impl Lifecycle {
    pub const fn new() -> Self {
        Self {
            usage: Mutex::new(Usage {
                last_use: None,
                busy: 0,
                generation: 0,
            }),
        }
    }

    fn usage(&self) -> std::sync::MutexGuard<'_, Usage> {
        self.usage.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The user used the masking tools: the idle time starts again.
    pub fn touch(&self) {
        self.touch_at(Instant::now());
    }

    fn touch_at(&self, now: Instant) {
        self.usage().last_use = Some(now);
    }

    /// Read before loading a model; pass to `keep` afterwards.
    pub fn generation(&self) -> u64 {
        self.usage().generation
    }

    /// Whether a model whose load started at `generation` may be kept (no
    /// unload since). Call under the lock of the state that will hold it,
    /// as `unload` takes the models under that lock after counting.
    /// Starts the idle time if it is not running.
    pub fn keep(&self, generation: u64) -> bool {
        self.keep_at(generation, Instant::now())
    }

    fn keep_at(&self, generation: u64, now: Instant) -> bool {
        let mut usage = self.usage();
        if usage.generation != generation {
            return false;
        }
        usage.last_use.get_or_insert(now);
        true
    }

    /// Marks a computation with a model as running until the guard drops.
    pub fn run(&self) -> Running<'_> {
        self.usage().busy += 1;
        Running(self)
    }

    /// Whether the models have been unused for `idle` and nothing runs.
    fn idle_at(&self, now: Instant, idle: Duration) -> bool {
        let usage = self.usage();
        usage.busy == 0
            && usage
                .last_use
                .is_some_and(|t| now.saturating_duration_since(t) >= idle)
    }

    /// Stops the idle time until the next use or load.
    fn forget_use(&self) {
        self.usage().last_use = None;
    }

    /// Counts an unload: loads that started before it are not kept, and the
    /// idle time stops until the next use or load.
    fn unloaded(&self) {
        let mut usage = self.usage();
        usage.generation += 1;
        usage.last_use = None;
    }
}

/// Takes from `ai` what `reason` unloads; the rest stays. Finished Subject
/// and Sky masks (a few MB, they answer a click without a model) and the
/// tagging models (used in the Library) go only under memory pressure.
fn take_unloaded(ai: &mut AiState, reason: UnloadReason) -> AiState {
    let mut taken = std::mem::take(ai);
    if reason != UnloadReason::MemoryPressure {
        ai.subject_masks = std::mem::take(&mut taken.subject_masks);
        ai.sky_masks = std::mem::take(&mut taken.sky_masks);
        ai.tag_models = taken.tag_models.take();
    }
    taken
}

/// Names of the models and caches in `taken`, for the log.
fn names(taken: &AiState) -> Vec<&'static str> {
    [
        ("subject", taken.subject_model.is_some()),
        ("sky", taken.sky_model.is_some()),
        ("SAM 2.1", taken.sam_models.is_some()),
        ("SAM embedding", taken.embeddings.is_some()),
        ("depth", taken.depth_model.is_some()),
        ("relight", taken.relight_model.is_some()),
        ("people normals", taken.people_normals_model.is_some()),
        ("depth map", taken.depth_map.is_some()),
        ("MI-GAN", taken.inpaint_model.is_some()),
        ("tagging", taken.tag_models.is_some()),
        ("subject masks", !taken.subject_masks.is_empty()),
        ("sky masks", !taken.sky_masks.is_empty()),
    ]
    .into_iter()
    .filter_map(|(name, present)| present.then_some(name))
    .collect()
}

/// Whether any model `unload` drops is loaded.
fn models_loaded(state: &AppState) -> bool {
    let ai = state.ai_state.lock().unwrap_or_else(|e| e.into_inner());
    let ai_models = ai.as_ref().is_some_and(|ai| {
        ai.subject_model.is_some()
            || ai.sky_model.is_some()
            || ai.sam_models.is_some()
            || ai.depth_model.is_some()
            || ai.relight_model.is_some()
            || ai.people_normals_model.is_some()
            || ai.inpaint_model.is_some()
    });
    drop(ai);
    ai_models
        || state
            .faces
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .has_models()
}

/// Drops the masking models and their caches (see the module), returns
/// what was dropped.
pub fn unload(state: &AppState, reason: UnloadReason) -> Vec<&'static str> {
    LIFECYCLE.unloaded();
    let taken = {
        let mut ai = state.ai_state.lock().unwrap_or_else(|e| e.into_inner());
        ai.as_mut().map(|ai| take_unloaded(ai, reason))
    };
    let mut dropped = taken.as_ref().map(names).unwrap_or_default();
    let faces = state
        .faces
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .unload();
    if faces {
        dropped.push("faces");
    }
    // Freed here, outside the locks (Core ML and ONNX Runtime release GPU
    // and CPU memory); models a computation still holds go when it ends.
    drop(taken);
    if !dropped.is_empty() {
        rawtonic_runtime::allocator::release_freed_memory();
    }
    dropped
}

/// Background runs the webview started ahead of a click (`useAiMasking`'s
/// task ids), stopped on memory pressure and when the Library is shown.
fn cancel_background_runs(state: &AppState) {
    let tasks = state
        .active_ai_tasks
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    for (id, token) in tasks.iter() {
        if is_background_task(id) {
            token.cancel();
        }
    }
}

fn is_background_task(id: &str) -> bool {
    id.starts_with("subject-precompute-") || id.starts_with("sky-precompute-")
}

/// Unloads for `reason`, logs and tells the webview.
pub fn unload_and_notify(app: &tauri::AppHandle, reason: UnloadReason) -> Vec<&'static str> {
    let state = app.state::<AppState>();
    if reason != UnloadReason::Idle {
        cancel_background_runs(&state);
    }
    let footprint_before = footprint_mb();
    let dropped = unload(&state, reason);
    if dropped.is_empty() && reason == UnloadReason::Idle {
        return dropped;
    }
    log::info!(
        "AI models unloaded ({reason:?}): {}{}",
        if dropped.is_empty() {
            "nothing was loaded".to_string()
        } else {
            dropped.join(", ")
        },
        footprint_before
            .zip(footprint_mb())
            .map(|(a, b)| format!("; footprint {a:.0} → {b:.0} MB"))
            .unwrap_or_default()
    );
    let _ = app.emit(UNLOADED_EVENT, reason);
    dropped
}

/// The process's physical footprint in MB (macOS), for the log.
fn footprint_mb() -> Option<f64> {
    #[cfg(target_os = "macos")]
    {
        // SAFETY: `rusage_info_v4` is plain data, filled by the kernel for
        // this process when the flavour matches the struct.
        let mut info: libc::rusage_info_v4 = unsafe { std::mem::zeroed() };
        let result = unsafe {
            libc::proc_pid_rusage(
                std::process::id() as libc::c_int,
                libc::RUSAGE_INFO_V4,
                (&mut info as *mut libc::rusage_info_v4).cast(),
            )
        };
        (result == 0).then(|| info.ri_phys_footprint as f64 / 1e6)
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

/// Starts the idle check and the memory pressure watch (not for headless
/// exports).
pub fn start(app: &tauri::AppHandle) {
    let idle_app = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(IDLE_CHECK).await;
            if LIFECYCLE.idle_at(Instant::now(), IDLE_UNLOAD) {
                let state = idle_app.state::<AppState>();
                if models_loaded(&state) {
                    unload_and_notify(&idle_app, UnloadReason::Idle);
                } else {
                    // Used, but nothing loaded since: stop the idle time.
                    LIFECYCLE.forget_use();
                }
            }
        }
    });
    let pressure_app = app.clone();
    crate::memory_pressure::watch(Arc::new(move |level| {
        log::warn!("Memory pressure ({level:?}): unloading the AI models");
        unload_and_notify(&pressure_app, UnloadReason::MemoryPressure);
    }));
}

/// The webview's unload request: `library` when the Library is shown
/// (QA builds may also ask for `memoryPressure`).
#[tauri::command]
pub fn unload_ai_models(reason: UnloadReason, app_handle: tauri::AppHandle) -> Vec<String> {
    unload_and_notify(&app_handle, reason)
        .into_iter()
        .map(String::from)
        .collect()
}

/// The webview reports masking use (the masking tools opened, a selection
/// pointed at or clicked): the models stay loaded `IDLE_UNLOAD` from now.
#[tauri::command]
pub fn note_ai_mask_use() {
    LIFECYCLE.touch();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai_processing::{CachedDepthMap, CachedMask, ImageEmbeddings};

    fn mask(key: &str) -> CachedMask {
        CachedMask {
            key: key.to_string(),
            mask_data_base64: Arc::new(String::from("data:image/png;base64,AAAA")),
        }
    }

    fn filled_state() -> AiState {
        let zeros = |shape: &[usize]| ndarray::Array::zeros(ndarray::IxDyn(shape));
        AiState {
            embeddings: Some(ImageEmbeddings {
                path_hash: "a".into(),
                embeddings: zeros(&[1, 4]),
                high_res_feats: [zeros(&[1, 2]), zeros(&[1, 2])],
                original_size: (10, 10),
            }),
            depth_map: Some(CachedDepthMap {
                path_hash: "a".into(),
                depth_image: image::GrayImage::new(4, 4),
                original_size: (4, 4),
            }),
            subject_masks: vec![mask("a")],
            sky_masks: vec![mask("a"), mask("b")],
            ..Default::default()
        }
    }

    #[test]
    fn models_unused_for_the_idle_time_are_due() {
        let lifecycle = Lifecycle::new();
        let t0 = Instant::now();
        let idle = Duration::from_secs(300);
        // Nothing loaded or used: nothing to unload.
        assert!(!lifecycle.idle_at(t0 + idle * 2, idle));
        assert!(lifecycle.keep_at(lifecycle.generation(), t0));
        assert!(!lifecycle.idle_at(t0 + idle - Duration::from_secs(1), idle));
        assert!(lifecycle.idle_at(t0 + idle, idle));
        // Use restarts the idle time.
        lifecycle.touch_at(t0 + Duration::from_secs(200));
        assert!(!lifecycle.idle_at(t0 + idle, idle));
        assert!(lifecycle.idle_at(t0 + Duration::from_secs(500), idle));
        // A second load does not restart it (loading ahead is not use).
        assert!(lifecycle.keep_at(lifecycle.generation(), t0 + Duration::from_secs(499)));
        assert!(lifecycle.idle_at(t0 + Duration::from_secs(500), idle));
    }

    #[test]
    fn nothing_is_unloaded_for_idleness_while_a_computation_runs() {
        let lifecycle = Lifecycle::new();
        let t0 = Instant::now();
        let idle = Duration::from_secs(300);
        lifecycle.touch_at(t0);
        let run = lifecycle.run();
        let second = lifecycle.run();
        assert!(!lifecycle.idle_at(t0 + idle * 3, idle));
        drop(run);
        assert!(!lifecycle.idle_at(t0 + idle * 3, idle));
        drop(second);
        assert!(lifecycle.idle_at(t0 + idle * 3, idle));
    }

    #[test]
    fn a_load_that_finishes_after_an_unload_is_not_kept() {
        let lifecycle = Lifecycle::new();
        let before = lifecycle.generation();
        lifecycle.unloaded();
        assert!(!lifecycle.keep(before));
        // The unload stopped the idle time; a later load starts it again.
        assert!(!lifecycle.idle_at(Instant::now() + IDLE_UNLOAD * 2, IDLE_UNLOAD));
        let now = lifecycle.generation();
        assert!(lifecycle.keep(now));
        assert!(lifecycle.idle_at(Instant::now() + IDLE_UNLOAD, IDLE_UNLOAD));
    }

    #[test]
    fn idle_and_library_unloads_keep_the_finished_masks() {
        for reason in [UnloadReason::Idle, UnloadReason::Library] {
            let mut ai = filled_state();
            let taken = take_unloaded(&mut ai, reason);
            assert!(ai.embeddings.is_none() && ai.depth_map.is_none());
            assert_eq!(ai.subject_masks.len(), 1);
            assert_eq!(ai.sky_masks.len(), 2);
            assert_eq!(names(&taken), ["SAM embedding", "depth map"]);
        }
    }

    #[test]
    fn memory_pressure_drops_everything() {
        let mut ai = filled_state();
        let taken = take_unloaded(&mut ai, UnloadReason::MemoryPressure);
        assert!(ai.embeddings.is_none() && ai.depth_map.is_none());
        assert!(ai.subject_masks.is_empty() && ai.sky_masks.is_empty());
        assert_eq!(
            names(&taken),
            ["SAM embedding", "depth map", "subject masks", "sky masks"]
        );
    }

    #[test]
    fn a_computation_keeps_its_model_through_an_unload() {
        // What a run holds (`Arc` clones of the models) stays valid; the
        // state no longer has it, and the next use loads again.
        let lifecycle = Lifecycle::new();
        let slot: Mutex<Option<Arc<String>>> = Mutex::new(None);
        let generation = lifecycle.generation();
        let model = Arc::new(String::from("model"));
        {
            let mut slot = slot.lock().unwrap();
            assert!(lifecycle.keep(generation));
            *slot = Some(model.clone());
        }
        let in_run = slot.lock().unwrap().clone().unwrap();
        let running = lifecycle.run();
        drop(model);
        // An unload while it runs.
        lifecycle.unloaded();
        let taken = slot.lock().unwrap().take();
        drop(taken);
        assert_eq!(in_run.as_str(), "model");
        assert_eq!(Arc::strong_count(&in_run), 1);
        drop(running);
        drop(in_run);
        assert!(slot.lock().unwrap().is_none());
    }

    #[test]
    fn only_runs_started_ahead_are_cancelled() {
        assert!(is_background_task("subject-precompute-1234"));
        assert!(is_background_task("sky-precompute-1234"));
        assert!(!is_background_task("4f1c2a7e-sub-mask"));
    }

    #[test]
    fn reasons_are_named_as_the_webview_sends_them() {
        let parse = |s: &str| serde_json::from_value::<UnloadReason>(serde_json::json!(s)).unwrap();
        assert_eq!(parse("library"), UnloadReason::Library);
        assert_eq!(parse("idle"), UnloadReason::Idle);
        assert_eq!(parse("memoryPressure"), UnloadReason::MemoryPressure);
    }
}
