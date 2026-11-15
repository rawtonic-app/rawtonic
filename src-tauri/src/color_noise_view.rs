//! The export's colour noise reduction in the Develop fit view. A fit view
//! renders from a reduced copy of the photograph, and the colour noise
//! filter run on that copy leaves 1.5–3 × the chroma noise the export
//! shows at the same size (its finest level is one of the export's coarse
//! levels; nothing tuned on the reduced frame reached the export: notes:
//! benchmark-results/tpx-thumb-parity-2026-11-12/REPORT.md, TPX3). So the
//! filter runs once per photograph and colour noise setting on the full
//! decode, in the background after the frame is shown (on the CPU on Apple
//! silicon, elsewhere the export's own GPU filter:
//! `rawtonic_gpu::gpu_processing::color_noise_view`), and
//! what it changes is kept at the fit view's (pre-warp) resolution
//! (`color_noise::reduced_correction`): the fit view's final frames add it
//! to their reduced copy and render without their own colour noise
//! reduction. A drag and the frames before the correction is in show the
//! reduced filter as before; the correction's frame follows on its own
//! (`Wanted::refresh`). Kept for the open photograph only and dropped with
//! its source (`DevelopSession::invalidate`, `release_rebuildable`).
//! Measured: notes: benchmark-results/tpx4-cached-chroma-2026-11-15/REPORT.md
//! (CPU, Apple silicon), benchmark-results/tpx5-gpu-chroma-2026-11-15/REPORT.md
//! (GPU, Windows).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};

use image::{DynamicImage, GenericImageView as _, Rgb32FImage};
use rawtonic_engine::color_noise;
use rawtonic_engine::image_processing::{AllAdjustments, GlobalAdjustments};
use tauri::Manager as _;

use crate::app_state::MaskOverlayRequest;
use crate::state::lock::lock_recover;

/// Quiet time after a final frame before the correction is built: a typed
/// value or a click on a slider is often followed by another.
pub(crate) const BUILD_IDLE: std::time::Duration = std::time::Duration::from_millis(300);

/// The largest correction kept (pixels): a fit view of a tightly cropped
/// photograph reduces less; beyond this the view keeps its own filter.
const MAX_PIXELS: u64 = 6_000_000;

/// Builds slower than this per 24 MP stop them for the session: the
/// fit view keeps its quick filter. On a Radeon 680M laptop (Ryzen CPU) the
/// CPU build of a 24 MP correction took 2.1–2.9 s with +1.3–1.85 GB of
/// private memory and a drag right after stuttered (gaps up to 402 ms;
/// G17/WTPX4); an M5 Pro takes 0.4 s, the GPU build there (TPX5) less.
const MAX_MS_PER_24MP: f64 = 1000.0;

static TOO_SLOW: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Whether corrections are built here: not on Linux, whose previews have
/// no native frame for the correction's frame to show in (a build's frame
/// is rendered without an invoke to answer), while no build was too slow
/// and the system is not short of memory.
pub(crate) fn enabled() -> bool {
    cfg!(not(target_os = "linux"))
        && !TOO_SLOW.load(Ordering::Relaxed)
        && crate::memory_pressure::level_now().is_none()
}

/// The colour noise settings a correction was built for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Key {
    amount: u32,
    detail: u32,
    smoothness: u32,
    version: u64,
}

impl Key {
    fn of(global: &GlobalAdjustments) -> (Self, color_noise::Settings, color_noise::Tuning) {
        let settings = color_noise::Settings {
            amount: global.color_noise_reduction,
            detail: global.noise_detail[2],
            smoothness: global.noise_detail[3],
        };
        let (version, tuning) = crate::gpu_processing::color_noise_tuning(global);
        (
            Self {
                amount: settings.amount.to_bits(),
                detail: settings.detail.to_bits(),
                smoothness: settings.smoothness.to_bits(),
                version,
            },
            settings,
            tuning,
        )
    }
}

struct Correction {
    source: Weak<DynamicImage>,
    /// The photograph's (virtual) path, for its thumbnail.
    path: String,
    key: Key,
    delta: Arc<Rgb32FImage>,
}

/// A fit view's base with the correction added, for the base it replaces.
struct CorrectedBase {
    base: Weak<DynamicImage>,
    delta: Weak<Rgb32FImage>,
    image: Arc<DynamicImage>,
}

/// The final frame to render again once its correction is in.
pub(crate) struct Refresh {
    pub adjustments: serde_json::Value,
    pub target_resolution: Option<u32>,
    pub request_analytics: bool,
    pub compute_waveform: bool,
    pub active_waveform_channel: Option<String>,
    pub mask_overlay: Option<MaskOverlayRequest>,
    pub generation: usize,
}

struct Wanted {
    source: Weak<DynamicImage>,
    path: String,
    key: Key,
    settings: color_noise::Settings,
    tuning: color_noise::Tuning,
    /// The recipe's adjustments (the GPU build takes its uniforms).
    global: GlobalAdjustments,
    dims: (u32, u32),
    /// The preview frame it was wanted for (`ColorNoiseView::note_frame`)
    /// and that frame to render again; None for the split view's before.
    frame: Option<(u64, Refresh)>,
}

#[derive(Default)]
struct Builds {
    /// At most [`KEPT`], oldest first.
    wanted: Vec<Wanted>,
    running: bool,
}

/// Corrections (and corrected bases) kept: the edit's and the split view's
/// before when its colour noise setting differs.
const KEPT: usize = 2;

/// The split view's last before request (`editor_preview::
/// render_split_before`), rendered again when a correction comes in.
pub(crate) struct SplitBeforeRequest {
    pub path: String,
    pub adjustments: serde_json::Value,
    pub target_resolution: Option<u32>,
}

/// The open photograph's corrections, the fit view bases they made and what
/// to build next. Part of `DevelopSession`.
#[derive(Default)]
pub struct ColorNoiseView {
    corrections: Mutex<Vec<Correction>>,
    corrected: Mutex<Vec<CorrectedBase>>,
    builds: Mutex<Builds>,
    /// Preview frames rendered so far (`note_frame`): a correction's frame
    /// follows only when none came after the one it was wanted for.
    frames: AtomicU64,
    /// Changes with every correction stored or dropped (`revision`).
    revision: AtomicU64,
    split_before: Mutex<Option<SplitBeforeRequest>>,
}

/// Keeps `item` last in `list` of at most [`KEPT`], dropping the oldest.
fn keep_last<T>(list: &mut Vec<T>, item: T) {
    list.push(item);
    if list.len() > KEPT {
        list.remove(0);
    }
}

impl ColorNoiseView {
    /// Drops the corrections and their bases (memory; rebuilt on demand).
    pub fn clear(&self) {
        lock_recover(&self.corrections).clear();
        lock_recover(&self.corrected).clear();
        self.revision.fetch_add(1, Ordering::SeqCst);
    }

    fn has_wanted(&self) -> bool {
        !lock_recover(&self.builds).wanted.is_empty()
    }

    /// Wants a correction; one wanted for the same photograph and setting
    /// merges with it: the larger size, and the edit's frame to render
    /// again whichever asked last (a before asks for none).
    fn want(&self, mut wanted: Wanted) {
        let mut builds = lock_recover(&self.builds);
        if let Some(i) = builds
            .wanted
            .iter()
            .position(|w| w.key == wanted.key && Weak::ptr_eq(&w.source, &wanted.source))
        {
            let earlier = builds.wanted.remove(i);
            wanted.dims = (
                wanted.dims.0.max(earlier.dims.0),
                wanted.dims.1.max(earlier.dims.1),
            );
            if wanted.frame.is_none() {
                wanted.frame = earlier.frame;
            }
        }
        keep_last(&mut builds.wanted, wanted);
    }

    fn store(&self, wanted: &Wanted, delta: Rgb32FImage) {
        let mut corrections = lock_recover(&self.corrections);
        corrections.retain(|c| Weak::ptr_eq(&c.source, &wanted.source) && c.key != wanted.key);
        keep_last(
            &mut corrections,
            Correction {
                source: wanted.source.clone(),
                path: wanted.path.clone(),
                key: wanted.key,
                delta: Arc::new(delta),
            },
        );
        self.revision.fetch_add(1, Ordering::SeqCst);
    }

    /// A number that changes whenever the corrections do: a split view's
    /// before rendered with another is stale.
    pub(crate) fn revision(&self) -> u64 {
        self.revision.load(Ordering::SeqCst)
    }

    /// Remembers the split view's before request, to render it again when
    /// a correction comes in.
    pub(crate) fn note_split_before(&self, request: SplitBeforeRequest) {
        *lock_recover(&self.split_before) = Some(request);
    }

    /// Counts a preview frame of the open photograph (every frame, a drag's
    /// too) and returns its number.
    pub(crate) fn note_frame(&self) -> u64 {
        self.frames.fetch_add(1, Ordering::SeqCst) + 1
    }
}

/// A final fit frame of a full decode that the correction applies to.
pub(crate) struct FitFrame<'a> {
    /// Its number (`ColorNoiseView::note_frame`); None for the split view's
    /// before.
    pub number: Option<u64>,
    pub source: &'a Arc<DynamicImage>,
    pub path: &'a str,
    /// The frame's reduced base (warped, cropped) and the size of its
    /// pre-warp reduction (`editor_preview::build_reduced_preview_base`).
    pub base: &'a Arc<DynamicImage>,
    pub reduced_dims: (u32, u32),
}

/// Whether the recipe's colour noise reduction is the multi-scale filter
/// with nothing a local layer adds to it.
pub(crate) fn applies(adjustments: &AllAdjustments, is_raw: bool) -> bool {
    let global = &adjustments.global;
    is_raw
        && global.color_noise[0] > 0.0
        && global.color_noise_reduction > 0.0
        && adjustments.mask_adjustments
            [..(adjustments.mask_count as usize).min(adjustments.mask_adjustments.len())]
            .iter()
            .all(|mask| mask.color_noise_reduction == 0.0)
}

/// The size a correction for a reduction to `dims` is built at: `dims`,
/// within [`MAX_PIXELS`].
fn build_dims(dims: (u32, u32)) -> Option<(u32, u32)> {
    let pixels = u64::from(dims.0) * u64::from(dims.1);
    (dims.0 > 0 && dims.1 > 0 && pixels <= MAX_PIXELS).then_some(dims)
}

/// The base a final fit `frame` renders from instead of its own, with the
/// colour noise reduction already in (the frame then renders at Color 0),
/// built with `build` from the correction; None when there is no
/// correction for this photograph, setting and size yet: then one is wanted
/// (built after [`BUILD_IDLE`], `after_final_frame`), its frame rendered
/// from `refresh` (the split view's before: rendered again,
/// `SplitBeforeRequest`).
pub(crate) fn corrected_base(
    view: &ColorNoiseView,
    global: &GlobalAdjustments,
    frame: FitFrame<'_>,
    build: impl FnOnce(&Rgb32FImage) -> Result<Arc<DynamicImage>, String>,
    refresh: impl FnOnce() -> Refresh,
) -> Option<Arc<DynamicImage>> {
    let dims = build_dims(frame.reduced_dims)?;
    let (key, settings, tuning) = Key::of(global);
    let source = Arc::downgrade(frame.source);
    let delta = {
        let mut corrections = lock_recover(&view.corrections);
        // Another photograph's corrections go as soon as this one shows.
        let before = corrections.len();
        corrections.retain(|c| Weak::ptr_eq(&c.source, &source));
        if corrections.len() != before {
            view.revision.fetch_add(1, Ordering::SeqCst);
        }
        corrections
            .iter()
            .find(|c| c.key == key && c.delta.width() >= dims.0 && c.delta.height() >= dims.1)
            .map(|c| Arc::clone(&c.delta))
    };
    let Some(delta) = delta else {
        view.want(Wanted {
            source,
            path: frame.path.to_string(),
            key,
            settings,
            tuning,
            global: *global,
            dims,
            frame: frame.number.map(|number| (number, refresh())),
        });
        return None;
    };
    {
        let corrected = lock_recover(&view.corrected);
        if let Some(c) = corrected.iter().find(|c| {
            Weak::ptr_eq(&c.base, &Arc::downgrade(frame.base))
                && Weak::ptr_eq(&c.delta, &Arc::downgrade(&delta))
        }) {
            return Some(Arc::clone(&c.image));
        }
    }
    match build(&delta) {
        Ok(image) if image.dimensions() == frame.base.dimensions() => {
            let mut corrected = lock_recover(&view.corrected);
            corrected.retain(|c| c.delta.strong_count() > 0);
            keep_last(
                &mut corrected,
                CorrectedBase {
                    base: Arc::downgrade(frame.base),
                    delta: Arc::downgrade(&delta),
                    image: Arc::clone(&image),
                },
            );
            Some(image)
        }
        Ok(image) => {
            log::warn!(
                "Colour noise view: corrected base {:?} is not the base's {:?}",
                image.dimensions(),
                frame.base.dimensions()
            );
            None
        }
        Err(e) => {
            log::warn!("Colour noise view: corrected base failed: {e}");
            None
        }
    }
}

/// The preview worker after a final frame: when a correction is wanted and
/// no request comes within [`BUILD_IDLE`], it is built in the background;
/// a request that comes is returned (the next final frame wants again).
pub(crate) fn after_final_frame<T>(
    app_handle: &tauri::AppHandle,
    jobs: &crate::preview_slot::LatestSlot<T>,
) -> Option<T> {
    let develop = app_handle.state::<crate::state::develop::DevelopSession>();
    if !develop.color_noise_view.has_wanted() {
        return None;
    }
    if let Some(next) = jobs.take_timeout(BUILD_IDLE) {
        return Some(next);
    }
    start_build(app_handle);
    None
}

/// After the split view's before rendered: a correction it wanted is built
/// once no preview frame came within [`BUILD_IDLE`] (a before alone, as
/// when a snapshot is chosen, renders no frame that would start it).
pub(crate) fn after_split_before(app_handle: &tauri::AppHandle) {
    let develop = app_handle.state::<crate::state::develop::DevelopSession>();
    let view = &develop.color_noise_view;
    if !view.has_wanted() {
        return;
    }
    let frames = view.frames.load(Ordering::SeqCst);
    let app = app_handle.clone();
    let spawned = std::thread::Builder::new()
        .name("color-noise-view-wait".into())
        .spawn(move || {
            std::thread::sleep(BUILD_IDLE);
            let develop = app.state::<crate::state::develop::DevelopSession>();
            if develop.color_noise_view.frames.load(Ordering::SeqCst) == frames {
                start_build(&app);
            }
        });
    if let Err(e) = spawned {
        log::warn!("Colour noise view: no wait thread: {e}");
    }
}

fn start_build(app_handle: &tauri::AppHandle) {
    let develop = app_handle.state::<crate::state::develop::DevelopSession>();
    let wanted = {
        let mut builds = lock_recover(&develop.color_noise_view.builds);
        if builds.running {
            // The running build takes it next.
            return;
        }
        if builds.wanted.is_empty() {
            return;
        }
        let wanted = builds.wanted.remove(0);
        builds.running = true;
        wanted
    };
    let app = app_handle.clone();
    let spawned = std::thread::Builder::new()
        .name("color-noise-view".into())
        .spawn(move || {
            let mut next = Some(wanted);
            while let Some(wanted) = next {
                build(&app, wanted);
                let develop = app.state::<crate::state::develop::DevelopSession>();
                let mut builds = lock_recover(&develop.color_noise_view.builds);
                next = (!builds.wanted.is_empty()).then(|| builds.wanted.remove(0));
                builds.running = next.is_some();
            }
        });
    if let Err(e) = spawned {
        log::warn!("Colour noise view: no build thread: {e}");
        lock_recover(&develop.color_noise_view.builds).running = false;
    }
}

/// Every core below the editor's priority, as preview proxy builds
/// (`proxy_worker`): the editor's frames and the next photograph's decode
/// go first.
fn pool() -> &'static rayon::ThreadPool {
    static POOL: std::sync::OnceLock<rayon::ThreadPool> = std::sync::OnceLock::new();
    POOL.get_or_init(|| {
        rayon::ThreadPoolBuilder::new()
            .thread_name(|i| format!("color-noise-view-{i}"))
            .stack_size(rawtonic_runtime::RENDER_STACK_SIZE)
            .start_handler(|_| crate::export_processing::lower_thread_priority())
            .build()
            .expect("colour noise view thread pool")
    })
}

/// Where corrections are built: on the CPU on Apple silicon (0.4 s at 24 MP
/// on an M5 Pro, TPX4), elsewhere on the GPU, where a Radeon 680M laptop's
/// CPU took 2.1–2.9 s (WTPX4) and its GPU runs the export's filter in less
/// (TPX5).
const ON_CPU: bool = cfg!(all(target_os = "macos", target_arch = "aarch64"));

/// The GPU build of `wanted`'s correction and when it started (after the
/// pass's pipelines are ready: their compile is no build's time); None when
/// a newer frame came meanwhile (`stop`; its final frame wants again),
/// for recipes the GPU pass does not take (`colorNoiseVersion` below 11:
/// the fit view keeps its quick filter) and on errors (logged).
fn on_gpu(
    app: &tauri::AppHandle,
    source: &DynamicImage,
    wanted: &Wanted,
    stop: &(dyn Fn() -> bool + Sync),
) -> Option<(Option<Rgb32FImage>, std::time::Instant)> {
    use rawtonic_gpu::gpu_processing::color_noise_view;
    let gpu = app.state::<crate::state::gpu::GpuState>();
    let context = crate::gpu_processing::get_or_init_gpu_context(&gpu, app)
        .inspect_err(|e| log::warn!("Colour noise view: no GPU: {e}"))
        .ok()?;
    color_noise_view::ready(&context)
        .inspect_err(|e| log::warn!("Colour noise view: GPU pass failed: {e}"))
        .ok()?;
    // A shader compile running now (a frame's variant, an idle compile)
    // goes first, and its time is no build's.
    while rawtonic_gpu::gpu_processing::compiling_now() {
        if stop() {
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let started = std::time::Instant::now();
    let delta = pool()
        .install(|| {
            color_noise_view::reduced_correction(
                &context,
                source,
                &wanted.global,
                wanted.dims,
                stop,
            )
        })
        .inspect_err(|e| log::warn!("Colour noise view: GPU build failed: {e}"))
        .ok()??;
    Some((Some(delta), started))
}

fn build(app: &tauri::AppHandle, wanted: Wanted) {
    let develop = app.state::<crate::state::develop::DevelopSession>();
    let view = &develop.color_noise_view;
    // A newer frame came: its final frame finds the correction or wants its
    // own.
    let current = |frame: &Option<(u64, Refresh)>| {
        frame
            .as_ref()
            .is_none_or(|(number, _)| *number == view.frames.load(Ordering::SeqCst))
    };
    if !current(&wanted.frame) {
        return;
    }
    let Some(source) = wanted.source.upgrade() else {
        return;
    };
    let (w, h) = wanted.dims;
    let delta = if ON_CPU {
        let started = std::time::Instant::now();
        let delta = pool().install(|| {
            color_noise::reduced_correction(&source, wanted.settings, &wanted.tuning, w, h)
        });
        Some((delta, started))
    } else {
        on_gpu(app, &source, &wanted, &|| !current(&wanted.frame))
    };
    let Some((delta, started)) = delta else {
        return;
    };
    log::info!(
        "Colour noise view: correction of {}x{} at {}x{} in {:.0} ms",
        source.width(),
        source.height(),
        w,
        h,
        started.elapsed().as_secs_f64() * 1000.0
    );
    let megapixels = f64::from(source.width()) * f64::from(source.height()) / 1e6;
    if started.elapsed().as_secs_f64() * 1000.0 * 24.0 / megapixels.max(1.0) > MAX_MS_PER_24MP {
        TOO_SLOW.store(true, Ordering::Relaxed);
        log::warn!("Colour noise view: too slow here, off for this session");
    }
    drop(source);
    let Some(delta) = delta else {
        return;
    };
    view.store(&wanted, delta);
    // The full-resolution filter's buffers (about 1 GB at 24 MP) go back.
    pool().broadcast(|_| rawtonic_runtime::allocator::release_freed_memory());
    rawtonic_runtime::allocator::release_freed_memory();
    // Shown only while the frame it was wanted for is the latest: after a
    // newer one (a drag's) its recipe would be an older state.
    if !current(&wanted.frame) {
        return;
    }
    // The split view's before, when one is shown (it may use it too).
    let split_before = lock_recover(&view.split_before).take();
    if let Some(request) = split_before {
        crate::editor_preview::render_split_before_again(app, request);
    }
    let Some((_, refresh)) = wanted.frame else {
        return;
    };
    let (responder, _) = tokio::sync::oneshot::channel();
    let job = crate::state::preview::PreviewJob {
        generation: refresh.generation,
        sequence: None,
        adjustments: refresh.adjustments,
        is_interactive: false,
        target_resolution: refresh.target_resolution,
        roi: None,
        request_analytics: refresh.request_analytics,
        compute_waveform: refresh.compute_waveform,
        active_waveform_channel: refresh.active_waveform_channel,
        mask_overlay: refresh.mask_overlay,
        crop_edit: false,
        responder,
    };
    app.state::<crate::app_state::AppState>()
        .preview
        .preview_jobs
        .submit_if_empty(job);
    // Its thumbnail too (`for_thumbnail`).
    crate::file_management::redevelop_thumbnail(app, &wanted.path);
}

/// The open photograph's correction for its thumbnail, rendered from the
/// preview proxy with it added and at Color 0: when `path` is the
/// photograph a correction was built for and `adjustments` (the
/// thumbnail's recipe) has that colour noise setting.
pub(crate) fn for_thumbnail(
    view: &ColorNoiseView,
    path: &str,
    adjustments: &AllAdjustments,
    is_raw: bool,
) -> Option<Arc<Rgb32FImage>> {
    if !applies(adjustments, is_raw) {
        return None;
    }
    let key = Key::of(&adjustments.global).0;
    lock_recover(&view.corrections)
        .iter()
        .find(|c| c.path == path && c.key == key)
        .map(|c| Arc::clone(&c.delta))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn global(amount: f32) -> GlobalAdjustments {
        let mut global = GlobalAdjustments::default();
        global.color_noise_reduction = amount;
        global.noise_detail = [0.0, 0.0, 0.5, 0.5];
        global.color_noise = [10.0, 0.0, 0.0, 0.0];
        global
    }

    fn refresh() -> Refresh {
        Refresh {
            adjustments: serde_json::Value::Null,
            target_resolution: None,
            request_analytics: false,
            compute_waveform: false,
            active_waveform_channel: None,
            mask_overlay: None,
            generation: 0,
        }
    }

    fn image(w: u32, h: u32) -> Arc<DynamicImage> {
        Arc::new(DynamicImage::ImageRgb32F(Rgb32FImage::new(w, h)))
    }

    #[test]
    fn corrections_beyond_the_cap_are_not_built() {
        assert_eq!(build_dims((2588, 1725)), Some((2588, 1725)));
        assert_eq!(build_dims((4000, 3000)), None);
        assert_eq!(build_dims((0, 10)), None);
    }

    /// A frame without its correction wants one and renders as before; once
    /// the correction is in, the frame's base is built once with it and
    /// reused; another setting, a larger reduction or another photograph
    /// does not take it.
    #[test]
    fn fit_frames_use_only_their_own_correction() {
        let view = ColorNoiseView::default();
        let (source, base) = (image(600, 400), image(200, 133));
        let builds = Cell::new(0);
        let corrected = image(200, 133);
        let frame_of = |source: &Arc<DynamicImage>, dims, amount| {
            let number = Some(view.note_frame());
            corrected_base(
                &view,
                &global(amount),
                FitFrame {
                    number,
                    source,
                    path: "a.raw",
                    base: &base,
                    reduced_dims: dims,
                },
                |_| {
                    builds.set(builds.get() + 1);
                    Ok(Arc::clone(&corrected))
                },
                refresh,
            )
        };
        assert!(frame_of(&source, (300, 200), 0.25).is_none());
        let wanted = lock_recover(&view.builds).wanted.pop().unwrap();
        assert_eq!(wanted.dims, (300, 200));
        view.store(&wanted, Rgb32FImage::new(300, 200));

        let shown = frame_of(&source, (300, 200), 0.25).unwrap();
        assert!(Arc::ptr_eq(&shown, &corrected));
        assert!(frame_of(&source, (280, 187), 0.25).is_some());
        assert_eq!(builds.get(), 1, "the corrected base is kept");
        assert!(lock_recover(&view.builds).wanted.is_empty());

        assert!(frame_of(&source, (300, 200), 0.5).is_none());
        assert!(frame_of(&source, (310, 207), 0.25).is_none());
        // Another photograph drops the correction.
        let other = image(600, 400);
        assert!(frame_of(&other, (300, 200), 0.25).is_none());
        assert!(lock_recover(&view.corrections).is_empty());
    }

    /// A before asking for the edit's correction keeps the edit's frame to
    /// render again, and the larger size.
    #[test]
    fn a_before_wanting_the_same_correction_keeps_the_edits_frame() {
        let view = ColorNoiseView::default();
        let source = image(600, 400);
        let wanted = |frame, dims| Wanted {
            source: Arc::downgrade(&source),
            path: "a.raw".into(),
            key: Key::of(&global(0.25)).0,
            settings: Key::of(&global(0.25)).1,
            tuning: Key::of(&global(0.25)).2,
            global: global(0.25),
            dims,
            frame,
        };
        view.want(wanted(Some((3, refresh())), (300, 200)));
        view.want(wanted(None, (310, 207)));
        let builds = lock_recover(&view.builds);
        assert_eq!(builds.wanted.len(), 1);
        assert_eq!(builds.wanted[0].dims, (310, 207));
        assert_eq!(builds.wanted[0].frame.as_ref().map(|f| f.0), Some(3));
    }

    #[test]
    fn thumbnails_take_the_correction_of_their_photograph_and_setting() {
        let view = ColorNoiseView::default();
        let source = image(600, 400);
        let wanted = Wanted {
            source: Arc::downgrade(&source),
            path: "a.raw".into(),
            key: Key::of(&global(0.25)).0,
            settings: Key::of(&global(0.25)).1,
            tuning: Key::of(&global(0.25)).2,
            global: global(0.25),
            dims: (300, 200),
            frame: Some((1, refresh())),
        };
        view.store(&wanted, Rgb32FImage::new(300, 200));
        let all = |amount| AllAdjustments {
            global: global(amount),
            ..Default::default()
        };
        assert!(for_thumbnail(&view, "a.raw", &all(0.25), true).is_some());
        assert!(for_thumbnail(&view, "a.raw", &all(0.5), true).is_none());
        // The split view's before with another setting keeps its own beside
        // the edit's; a third setting replaces the oldest.
        let other = |amount| Wanted {
            key: Key::of(&global(amount)).0,
            frame: None,
            path: "a.raw".into(),
            source: Arc::downgrade(&source),
            settings: Key::of(&global(amount)).1,
            tuning: Key::of(&global(amount)).2,
            global: global(amount),
            dims: (300, 200),
        };
        let revision = view.revision();
        view.store(&other(0.5), Rgb32FImage::new(300, 200));
        assert_ne!(view.revision(), revision);
        assert!(for_thumbnail(&view, "a.raw", &all(0.25), true).is_some());
        assert!(for_thumbnail(&view, "a.raw", &all(0.5), true).is_some());
        view.store(&other(0.75), Rgb32FImage::new(300, 200));
        assert!(for_thumbnail(&view, "a.raw", &all(0.25), true).is_none());
        assert!(for_thumbnail(&view, "a.raw", &all(0.5), true).is_some());
        view.store(&wanted, Rgb32FImage::new(300, 200));
        assert!(for_thumbnail(&view, "b.raw", &all(0.25), true).is_none());
        assert!(for_thumbnail(&view, "a.raw", &all(0.25), false).is_none());
        view.clear();
        assert!(for_thumbnail(&view, "a.raw", &all(0.25), true).is_none());
    }
}
