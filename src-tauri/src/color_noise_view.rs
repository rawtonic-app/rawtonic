//! The export's render input in the Develop fit view. A fit view renders
//! from a reduced copy of the photograph, and the colour noise filter run on
//! that copy leaves 1.5–3 × the chroma noise the export shows at the same
//! size (its finest level is one of the export's coarse levels: notes:
//! benchmark-results/tpx-thumb-parity-2026-11-12/REPORT.md, TPX3); its warp
//! and edge-fringe pass at the view's scale also leave out what the
//! export's full-resolution pass does to a noisy photo's colour noise (up to
//! 1.9 × the export's at Color 0; TPX7). So the export's render input — the
//! full decode warped, through the edge-fringe pass and the colour noise
//! filter at full resolution, as an export uploads it — is made once per
//! photograph, geometry and colour noise setting, in the background after
//! the frame is shown, on the GPU
//! (`rawtonic_gpu::gpu_processing::color_noise_view::reduced_render_input`),
//! and kept at the fit view's pre-crop resolution: the fit view's final
//! frames render from it at Color 0 instead of their own reduced, warped and
//! fringe-treated copy. A drag and the frames before it is in show the
//! reduced copy as before; the input's frame follows on its own
//! (`Wanted::refresh`; webview previews, as on Linux, ask for it on
//! [`READY_EVENT`]). Kept for the open photograph only and dropped with its
//! source (`DevelopSession::invalidate`, `release_rebuildable`).
//! Measured: notes: benchmark-results/tpx4-cached-chroma-2026-11-15/REPORT.md
//! (the design, Apple silicon), benchmark-results/tpx5-gpu-chroma-2026-11-15/REPORT.md
//! (GPU), benchmark-results/tpx6-2026-11-17/REPORT.md (Linux),
//! benchmark-results/tpx7-2026-11-17/REPORT.md (the render input instead
//! of a correction of the bare decode).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};

use image::{DynamicImage, GenericImageView as _, Rgb32FImage};
use rawtonic_engine::image_processing::{AllAdjustments, GlobalAdjustments};
use tauri::Manager as _;

use crate::app_state::MaskOverlayRequest;
use crate::state::lock::lock_recover;

/// Quiet time after a final frame before the render input is built: a typed
/// value or a click on a slider is often followed by another.
pub(crate) const BUILD_IDLE: std::time::Duration = std::time::Duration::from_millis(300);

/// The largest render input kept (pixels): a fit view of a tightly cropped
/// photograph reduces less; beyond this the view keeps its own base.
const MAX_PIXELS: u64 = 6_000_000;

/// [`SLOW_IN_A_ROW`] builds in a row slower than this per 24 MP stop them
/// for the session: the fit view keeps its own base and quick filter. On a
/// Radeon 680M laptop (Ryzen CPU) the CPU build of a 24 MP colour noise
/// correction took 2.1–2.9 s with +1.3–1.85 GB of private memory and a drag
/// right after stuttered (gaps up to 402 ms; G17/WTPX4); an M5 Pro takes
/// 0.4 s, the GPU build there (TPX5) less, an RTX 3080 0.48–0.61 s with the
/// warp and edge-fringe pass (TPX7).
const MAX_MS_PER_24MP: f64 = 1000.0;

/// One slow build is not enough: a build beside a photograph's opening
/// (decode, thumbnails) took 1026 ms on the RTX 3080 where the next ones
/// took 0.5 s (TPX7); a machine too slow for it is slow every time.
const SLOW_IN_A_ROW: u32 = 2;

static TOO_SLOW: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Slow builds in a row so far.
static SLOW_BUILDS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Counts a build of `ms_per_24mp`; true when it stops the builds for the
/// session ([`SLOW_IN_A_ROW`] slow ones in a row).
fn note_build_time(ms_per_24mp: f64) -> bool {
    if ms_per_24mp <= MAX_MS_PER_24MP {
        SLOW_BUILDS.store(0, Ordering::Relaxed);
        return false;
    }
    SLOW_BUILDS.fetch_add(1, Ordering::Relaxed) + 1 >= SLOW_IN_A_ROW
}

/// Whether render inputs are built here: while no build was too slow and
/// the system is not short of memory.
pub(crate) fn enabled() -> bool {
    // Profiling builds can switch it off for A/B timing runs.
    #[cfg(feature = "performance-profiling")]
    if std::env::var_os("RAWTONIC_NO_COLOR_NOISE_VIEW").is_some() {
        return false;
    }
    !TOO_SLOW.load(Ordering::Relaxed) && crate::memory_pressure::level_now().is_none()
}

/// The event telling the editor that the open photograph's render input is in
/// and its fit frame should be requested again: previews in the webview
/// (Linux) are answers to the editor's requests, so the render input's frame
/// cannot be pushed to it (`src/utils/colorNoiseView.ts`).
pub(crate) const READY_EVENT: &str = "color-noise-view-ready";

/// The colour noise settings and the geometry a render input was built for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Key {
    amount: u32,
    detail: u32,
    smoothness: u32,
    version: u64,
    /// [`geometry_key`] of the recipe.
    geometry: u64,
}

/// What of `recipe` the render input depends on besides the colour noise
/// setting: the warp (lens inputs, automatic CA, perspective,
/// distortion) and the edge-fringe treatment. Rotation, orientation, crop
/// and lens blur follow the input in the fit view's base.
fn geometry_key(recipe: &serde_json::Value, is_raw: bool) -> u64 {
    use std::hash::{Hash as _, Hasher as _};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    serde_json::to_string(&rawtonic_engine::image_processing::geometry_params_for(
        recipe, is_raw,
    ))
    .unwrap_or_default()
    .hash(&mut hasher);
    rawtonic_engine::recipe_versions::FringeVersion::of(recipe)
        .enabled()
        .hash(&mut hasher);
    recipe
        .get(rawtonic_engine::edge_fringe::VERSION_KEY)
        .map(serde_json::Value::to_string)
        .hash(&mut hasher);
    hasher.finish()
}

impl Key {
    fn of(global: &GlobalAdjustments, recipe: &serde_json::Value, is_raw: bool) -> Self {
        Self {
            amount: global.color_noise_reduction.to_bits(),
            detail: global.noise_detail[2].to_bits(),
            smoothness: global.noise_detail[3].to_bits(),
            version: crate::gpu_processing::color_noise_tuning(global).0,
            geometry: geometry_key(recipe, is_raw),
        }
    }
}

struct RenderInput {
    source: Weak<DynamicImage>,
    /// The photograph's (virtual) path, for its thumbnail.
    path: String,
    key: Key,
    /// The export's render input at the fit view's pre-crop size.
    input: Arc<Rgb32FImage>,
}

/// A fit view's base made from a render input, for the base it replaces.
struct CorrectedBase {
    base: Weak<DynamicImage>,
    input: Weak<Rgb32FImage>,
    image: Arc<DynamicImage>,
}

/// The final frame to render again once its render input is in.
pub(crate) struct Refresh {
    pub adjustments: serde_json::Value,
    pub target_resolution: Option<u32>,
    pub request_analytics: bool,
    pub compute_waveform: bool,
    pub active_waveform_channel: Option<String>,
    pub mask_overlay: Option<MaskOverlayRequest>,
    pub generation: usize,
    /// Shown on the native display; else the frame is a JPEG answer to the
    /// editor's request, which asks again on [`READY_EVENT`].
    pub native: bool,
}

struct Wanted {
    source: Weak<DynamicImage>,
    path: String,
    key: Key,
    /// The recipe's adjustments (the GPU build takes its uniforms).
    global: GlobalAdjustments,
    /// The recipe (its warp and edge-fringe pass).
    recipe: serde_json::Value,
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

/// Render inputs (and the bases made from them) kept: the edit's and the
/// split view's before when its setting differs.
const KEPT: usize = 2;

/// The split view's last before request (`editor_preview::
/// render_split_before`), rendered again when a render input comes in.
pub(crate) struct SplitBeforeRequest {
    pub path: String,
    pub adjustments: serde_json::Value,
    pub target_resolution: Option<u32>,
}

/// The open photograph's render inputs, the fit view bases they made and
/// what to build next. Part of `DevelopSession`.
#[derive(Default)]
pub struct ColorNoiseView {
    inputs: Mutex<Vec<RenderInput>>,
    corrected: Mutex<Vec<CorrectedBase>>,
    builds: Mutex<Builds>,
    /// Preview frames rendered so far (`note_frame`): a render input's frame
    /// follows only when none came after the one it was wanted for.
    frames: AtomicU64,
    /// Changes with every render input stored or dropped (`revision`).
    revision: AtomicU64,
    split_before: Mutex<Option<SplitBeforeRequest>>,
    /// The corrected base the last preview frame rendered from (`note_shown`),
    /// None when it rendered from its own base.
    shown: Mutex<Option<Weak<DynamicImage>>>,
    /// On integrated GPUs, the last build's uploaded input for the next
    /// build of the same photograph and geometry (`on_gpu`).
    kept_input: rawtonic_gpu::gpu_processing::color_noise_view::KeptInput,
}

/// Keeps `item` last in `list` of at most [`KEPT`], dropping the oldest.
fn keep_last<T>(list: &mut Vec<T>, item: T) {
    list.push(item);
    if list.len() > KEPT {
        list.remove(0);
    }
}

impl ColorNoiseView {
    /// Drops the render inputs and their bases (memory; rebuilt on demand).
    pub fn clear(&self) {
        lock_recover(&self.inputs).clear();
        lock_recover(&self.corrected).clear();
        self.kept_input.clear();
        self.revision.fetch_add(1, Ordering::SeqCst);
    }

    fn has_wanted(&self) -> bool {
        !lock_recover(&self.builds).wanted.is_empty()
    }

    /// Wants a render input; one wanted for the same photograph and setting
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

    fn store(&self, wanted: &Wanted, input: Rgb32FImage) {
        // Ringing is cut at 0 and colours beyond sRGB keep their negative
        // channels already, as in the view's own reduction
        // (`resample_f32_image`, `lanczos::resize_wide_gamut`).
        let mut inputs = lock_recover(&self.inputs);
        inputs.retain(|c| Weak::ptr_eq(&c.source, &wanted.source) && c.key != wanted.key);
        keep_last(
            &mut inputs,
            RenderInput {
                source: wanted.source.clone(),
                path: wanted.path.clone(),
                key: wanted.key,
                input: Arc::new(input),
            },
        );
        self.revision.fetch_add(1, Ordering::SeqCst);
    }

    /// A number that changes whenever the render inputs do: a split view's
    /// before rendered with another is stale.
    pub(crate) fn revision(&self) -> u64 {
        self.revision.load(Ordering::SeqCst)
    }

    /// Remembers the split view's before request, to render it again when
    /// a render input comes in.
    pub(crate) fn note_split_before(&self, request: SplitBeforeRequest) {
        *lock_recover(&self.split_before) = Some(request);
    }

    /// Counts a preview frame of the open photograph (every frame, a drag's
    /// too) and returns its number.
    pub(crate) fn note_frame(&self) -> u64 {
        self.frames.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// What the preview frame being rendered now renders from: `corrected`
    /// (from [`corrected_base`]) or its own base (None). Probes of the
    /// shown frame follow it ([`shown_corrected_base`]).
    pub(crate) fn note_shown(&self, corrected: Option<&Arc<DynamicImage>>) {
        *lock_recover(&self.shown) = corrected.map(Arc::downgrade);
    }

    /// What [`note_shown`](Self::note_shown) last recorded, to put back
    /// after a render nobody sees ([`restore_shown`](Self::restore_shown)).
    pub(crate) fn shown_mark(&self) -> Option<Weak<DynamicImage>> {
        lock_recover(&self.shown).clone()
    }

    /// Puts back the record of [`shown_mark`](Self::shown_mark): a render
    /// nobody sees (Linux's drag-draft warm-up) leaves the shown frame's
    /// base the one probes sample.
    pub(crate) fn restore_shown(&self, mark: Option<Weak<DynamicImage>>) {
        *lock_recover(&self.shown) = mark;
    }

    /// The number of the last frame noted.
    pub(crate) fn frames_noted(&self) -> u64 {
        self.frames.load(Ordering::SeqCst)
    }

    /// Takes back the one frame noted after `before` (`frames_noted`): a
    /// render nobody sees (Linux's drag-draft warm-up,
    /// `editor_preview::warm_draft_source`) leaves the frame before it the
    /// latest. Nothing when no frame or more than one came.
    pub(crate) fn forget_frame(&self, before: u64) {
        let _ =
            self.frames
                .compare_exchange(before + 1, before, Ordering::SeqCst, Ordering::SeqCst);
    }
}

/// A final fit frame of a full decode that a render input stands in for.
pub(crate) struct FitFrame<'a> {
    /// Its number (`ColorNoiseView::note_frame`); None for the split view's
    /// before.
    pub number: Option<u64>,
    pub source: &'a Arc<DynamicImage>,
    pub path: &'a str,
    /// The frame's recipe.
    pub recipe: &'a serde_json::Value,
    pub is_raw: bool,
    /// The frame's reduced base (warped, cropped) and the size of its
    /// pre-warp reduction (`editor_preview::build_reduced_preview_base`).
    pub base: &'a Arc<DynamicImage>,
    pub reduced_dims: (u32, u32),
}

/// Whether a render input stands in for a RAW's fit view base with
/// `adjustments` (of `recipe`): the export's colour noise reduction is off
/// or the multi-scale filter the GPU build runs (version 11), with nothing a
/// local layer adds to it, and there is something the view's own base
/// leaves out (the filter or an edge-fringe pass); not with retouching
/// composited into the base (AI patches, spots), which the view composites
/// at its own scale.
pub(crate) fn applies(
    adjustments: &AllAdjustments,
    recipe: &serde_json::Value,
    is_raw: bool,
) -> bool {
    let global = &adjustments.global;
    let filtered = global.color_noise_reduction > 0.0;
    is_raw
        && (!filtered
            || (global.color_noise[0] > 0.0
                && crate::gpu_processing::color_noise_tuning(global)
                    .1
                    .gpu_pyramid))
        && (filtered || rawtonic_engine::recipe_versions::FringeVersion::of(recipe).enabled())
        && !crate::image_loader::has_base_retouch(recipe)
        && adjustments.mask_adjustments
            [..(adjustments.mask_count as usize).min(adjustments.mask_adjustments.len())]
            .iter()
            .all(|mask| mask.color_noise_reduction == 0.0)
}

/// The size a render input for a reduction to `dims` is built at: `dims`,
/// within [`MAX_PIXELS`].
fn build_dims(dims: (u32, u32)) -> Option<(u32, u32)> {
    let pixels = u64::from(dims.0) * u64::from(dims.1);
    (dims.0 > 0 && dims.1 > 0 && pixels <= MAX_PIXELS).then_some(dims)
}

/// The base a final fit `frame` renders from instead of its own, with the
/// colour noise reduction already in (the frame then renders at Color 0),
/// built with `build` from the render input; None when there is no
/// render input for this photograph, setting, geometry and size yet: then one is wanted
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
    let key = Key::of(global, frame.recipe, frame.is_raw);
    let source = Arc::downgrade(frame.source);
    let input = {
        let mut inputs = lock_recover(&view.inputs);
        // Another photograph's render inputs go as soon as this one shows.
        let before = inputs.len();
        inputs.retain(|c| Weak::ptr_eq(&c.source, &source));
        if inputs.len() != before {
            view.revision.fetch_add(1, Ordering::SeqCst);
        }
        inputs
            .iter()
            .find(|c| c.key == key && c.input.width() >= dims.0 && c.input.height() >= dims.1)
            .map(|c| Arc::clone(&c.input))
    };
    let Some(input) = input else {
        view.want(Wanted {
            source,
            path: frame.path.to_string(),
            key,
            global: *global,
            recipe: frame.recipe.clone(),
            dims,
            frame: frame.number.map(|number| (number, refresh())),
        });
        return None;
    };
    {
        let corrected = lock_recover(&view.corrected);
        if let Some(c) = corrected.iter().find(|c| {
            Weak::ptr_eq(&c.base, &Arc::downgrade(frame.base))
                && Weak::ptr_eq(&c.input, &Arc::downgrade(&input))
        }) {
            return Some(Arc::clone(&c.image));
        }
    }
    match build(&input) {
        Ok(image) if image.dimensions() == frame.base.dimensions() => {
            let mut corrected = lock_recover(&view.corrected);
            corrected.retain(|c| c.input.strong_count() > 0);
            keep_last(
                &mut corrected,
                CorrectedBase {
                    base: Arc::downgrade(frame.base),
                    input: Arc::downgrade(&input),
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

/// The corrected base the last preview frame of `base` rendered from for
/// this colour noise setting (a settled fit frame; `note_shown`), if it
/// did: what probes of the shown frame (the Levels pickers) render from,
/// at Color 0. None after a frame from its own base (a zoomed region, a
/// drag). Looks up only; never wants or builds one.
pub(crate) fn shown_corrected_base(
    view: &ColorNoiseView,
    adjustments: &AllAdjustments,
    recipe: &serde_json::Value,
    source: &Arc<DynamicImage>,
    is_raw: bool,
    base: &Arc<DynamicImage>,
) -> Option<Arc<DynamicImage>> {
    if !enabled() || !applies(adjustments, recipe, is_raw) {
        return None;
    }
    let key = Key::of(&adjustments.global, recipe, is_raw);
    let source = Arc::downgrade(source);
    let inputs: Vec<Weak<Rgb32FImage>> = lock_recover(&view.inputs)
        .iter()
        .filter(|c| Weak::ptr_eq(&c.source, &source) && c.key == key)
        .map(|c| Arc::downgrade(&c.input))
        .collect();
    let shown = lock_recover(&view.shown).clone()?;
    let base = Arc::downgrade(base);
    lock_recover(&view.corrected)
        .iter()
        .find(|c| {
            Weak::ptr_eq(&c.base, &base)
                && inputs.iter().any(|i| Weak::ptr_eq(&c.input, i))
                && Weak::ptr_eq(&Arc::downgrade(&c.image), &shown)
        })
        .map(|c| Arc::clone(&c.image))
}

/// The preview worker after a final frame: when a render input is wanted and
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

/// After the split view's before rendered: a render input it wanted is built
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
/// go first. Only the build's CPU work runs here; its GPU calls stay on the
/// build thread at normal priority (`reduced_render_input`'s `cpu`).
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

/// Apple silicon's GPU is integrated but shares the machine's memory
/// bandwidth and is no Radeon 680M: the 1 s guard decides there.
const APPLE_SILICON: bool = cfg!(all(target_os = "macos", target_arch = "aarch64"));

/// A GPU build's render input, when it started (after the pass's
/// pipelines are ready: their compile is no build's time) and whether its
/// time counts for the guard.
struct GpuBuild {
    input: Rgb32FImage,
    started: std::time::Instant,
    timed: bool,
}

/// The key of `wanted`'s uploaded input: its photograph and geometry.
fn kept_key(wanted: &Wanted) -> u64 {
    use std::hash::{Hash as _, Hasher as _};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    wanted.path.hash(&mut hasher);
    (Weak::as_ptr(&wanted.source) as usize).hash(&mut hasher);
    wanted.key.geometry.hash(&mut hasher);
    hasher.finish()
}

/// The GPU build of `wanted`'s render input; None when a newer frame came
/// meanwhile (`stop`; its final frame wants again), when an integrated
/// GPU's memory has no room for it, for recipes the GPU pass does not take
/// and on errors (logged).
///
/// On an integrated GPU (outside Apple silicon) or under a compile limit
/// the graphics memory is the machine's own: there a build runs only when
/// its memory (`color_noise_view::transient_bytes`) fits what the app may
/// still take (`memory_pressure::work_headroom`), and its uploaded input is
/// kept for the next build of the photograph and geometry, so that typed
/// Color, Detail and Smoothness changes skip the upload, the build's
/// largest part (a Radeon 680M laptop: 1.9–2.9 s and +1.07–1.68 GB of
/// private memory per build with the upload, WTPX5B; notes:
/// benchmark-results/tpx8-2026-11-22/REPORT.md). The build with the upload
/// does not count for the 1 s guard there: the changes after it do.
fn on_gpu(
    app: &tauri::AppHandle,
    source: &Arc<DynamicImage>,
    wanted: &Wanted,
    stop: &(dyn Fn() -> bool + Sync),
) -> Option<GpuBuild> {
    use rawtonic_gpu::gpu_processing::color_noise_view;
    let gpu = app.state::<crate::state::gpu::GpuState>();
    let integrated = lock_recover(&gpu.adapter)
        .as_ref()
        .is_some_and(|(_, adapter)| {
            adapter.get_info().device_type == wgpu::DeviceType::IntegratedGpu
        });
    let shared_memory =
        (integrated && !APPLE_SILICON) || rawtonic_gpu::gpu_processing::compiles_limited();
    let develop = app.state::<crate::state::develop::DevelopSession>();
    let kept = &develop.color_noise_view.kept_input;
    let key = kept_key(wanted);
    let uploads = !shared_memory || !kept.holds(key);
    if shared_memory {
        let needed = color_noise_view::transient_bytes(source.width(), source.height(), uploads);
        let room = crate::memory_pressure::work_headroom();
        if room.is_none_or(|room| needed > room) {
            log::info!(
                "Colour noise view: not built, {} MB wanted, room {:?} MB",
                needed >> 20,
                room.map(|r| r >> 20)
            );
            return None;
        }
    }
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
    let input = color_noise_view::reduced_render_input(
        &context,
        source,
        &wanted.recipe,
        &wanted.global,
        wanted.dims,
        stop,
        &|work| pool().install(work),
        shared_memory.then_some((kept, key)),
    )
    .inspect_err(|e| log::warn!("Colour noise view: GPU build failed: {e}"))
    .ok()??;
    Some(GpuBuild {
        input,
        started,
        timed: !(shared_memory && uploads),
    })
}

fn build(app: &tauri::AppHandle, wanted: Wanted) {
    let develop = app.state::<crate::state::develop::DevelopSession>();
    let view = &develop.color_noise_view;
    // A newer frame came: its final frame finds the render input or wants
    // its own.
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
    let Some(GpuBuild {
        input,
        started,
        timed,
    }) = on_gpu(app, &source, &wanted, &|| !current(&wanted.frame))
    else {
        return;
    };
    log::info!(
        "Colour noise view: render input of {}x{} at {}x{} in {:.0} ms{}",
        source.width(),
        source.height(),
        w,
        h,
        started.elapsed().as_secs_f64() * 1000.0,
        if timed { "" } else { " (with its upload)" }
    );
    let megapixels = f64::from(source.width()) * f64::from(source.height()) / 1e6;
    if timed
        && note_build_time(started.elapsed().as_secs_f64() * 1000.0 * 24.0 / megapixels.max(1.0))
    {
        TOO_SLOW.store(true, Ordering::Relaxed);
        log::warn!("Colour noise view: too slow here, off for this session");
    }
    drop(source);
    view.store(&wanted, input);
    // The build's buffers go back.
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
    if !refresh.native {
        use tauri::Emitter as _;
        let _ = app.emit(READY_EVENT, serde_json::json!({ "path": wanted.path }));
        crate::file_management::redevelop_thumbnail(app, &wanted.path);
        return;
    }
    let (responder, _) = tokio::sync::oneshot::channel();
    let job = crate::state::preview::PreviewJob {
        generation: refresh.generation,
        sequence: None,
        adjustments: refresh.adjustments,
        is_interactive: false,
        grain_drag: false,
        browser_full_zoom: false,
        target_resolution: refresh.target_resolution,
        roi: None,
        request_analytics: refresh.request_analytics,
        compute_waveform: refresh.compute_waveform,
        active_waveform_channel: refresh.active_waveform_channel,
        mask_overlay: refresh.mask_overlay,
        crop_edit: false,
        region_first: false,
        responder,
    };
    app.state::<crate::app_state::AppState>()
        .preview
        .preview_jobs
        .submit_if_empty(job);
    // Its thumbnail too (`for_thumbnail`).
    crate::file_management::redevelop_thumbnail(app, &wanted.path);
}

/// The open photograph's render input for its thumbnail, which renders
/// from it at Color 0 in place of its warped proxy: when `path` is the
/// photograph it was built for and `adjustments` (the thumbnail's
/// recipe, `recipe`) has that colour noise setting and geometry.
pub(crate) fn for_thumbnail(
    view: &ColorNoiseView,
    path: &str,
    adjustments: &AllAdjustments,
    recipe: &serde_json::Value,
    is_raw: bool,
) -> Option<Arc<Rgb32FImage>> {
    if !applies(adjustments, recipe, is_raw) {
        return None;
    }
    let key = Key::of(&adjustments.global, recipe, is_raw);
    lock_recover(&view.inputs)
        .iter()
        .find(|c| c.path == path && c.key == key)
        .map(|c| Arc::clone(&c.input))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn recipe(amount: u32) -> serde_json::Value {
        serde_json::json!({
            "colorNoiseReduction": amount,
            "colorNoiseVersion": 11,
            "fringeVersion": 6,
        })
    }

    fn all(amount: u32) -> AllAdjustments {
        *rawtonic_engine::image_processing::get_all_adjustments_from_json(
            &recipe(amount),
            true,
            None,
        )
        .uniforms
    }

    fn global(amount: u32) -> GlobalAdjustments {
        all(amount).global
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
            native: true,
        }
    }

    fn image(w: u32, h: u32) -> Arc<DynamicImage> {
        Arc::new(DynamicImage::ImageRgb32F(Rgb32FImage::new(w, h)))
    }

    fn wanted_for(
        source: &Arc<DynamicImage>,
        recipe: serde_json::Value,
        frame: Option<(u64, Refresh)>,
        dims: (u32, u32),
    ) -> Wanted {
        let global =
            rawtonic_engine::image_processing::get_all_adjustments_from_json(&recipe, true, None)
                .global;
        Wanted {
            source: Arc::downgrade(source),
            path: "a.raw".into(),
            key: Key::of(&global, &recipe, true),
            global,
            recipe,
            dims,
            frame,
        }
    }

    #[test]
    fn two_slow_builds_in_a_row_stop_them() {
        SLOW_BUILDS.store(0, Ordering::Relaxed);
        assert!(!note_build_time(1026.0));
        assert!(!note_build_time(500.0), "a fast one in between");
        assert!(!note_build_time(1100.0));
        assert!(note_build_time(1200.0));
        SLOW_BUILDS.store(0, Ordering::Relaxed);
    }

    /// A background mask run's release keeps the fit view's render inputs
    /// (and the kept upload with them); other releases drop them (TPX8).
    #[test]
    fn a_mask_runs_release_keeps_the_render_inputs() {
        let develop = crate::state::develop::DevelopSession::default();
        let source = image(8, 8);
        let view = &develop.color_noise_view;
        let wanted = wanted_for(&source, recipe(25), None, (4, 4));
        view.store(&wanted, Rgb32FImage::new(4, 4));
        develop.release_rebuildable(true);
        assert_eq!(lock_recover(&view.inputs).len(), 1);
        develop.release_rebuildable(false);
        assert!(lock_recover(&view.inputs).is_empty());
    }

    #[test]
    fn render_inputs_beyond_the_cap_are_not_built() {
        assert_eq!(build_dims((2588, 1725)), Some((2588, 1725)));
        assert_eq!(build_dims((4000, 3000)), None);
        assert_eq!(build_dims((0, 10)), None);
    }

    /// RAWs whose colour noise filter is version 11 or off, with an
    /// edge-fringe pass or the filter, without retouching in the base.
    #[test]
    fn render_inputs_stand_in_where_the_view_leaves_out_the_exports_passes() {
        let of = |recipe: serde_json::Value, is_raw| {
            let all = rawtonic_engine::image_processing::get_all_adjustments_from_json(
                &recipe, is_raw, None,
            );
            applies(&all, &recipe, is_raw)
        };
        assert!(of(recipe(25), true));
        assert!(of(recipe(0), true), "Color 0 with the edge-fringe pass");
        assert!(!of(recipe(25), false));
        assert!(!of(
            serde_json::json!({"colorNoiseReduction": 0, "colorNoiseVersion": 11}),
            true
        ));
        assert!(of(
            serde_json::json!({"colorNoiseReduction": 25, "colorNoiseVersion": 11}),
            true
        ));
        assert!(!of(
            serde_json::json!({"colorNoiseReduction": 25, "colorNoiseVersion": 10, "fringeVersion": 6}),
            true
        ));
        let mut patched = recipe(25);
        patched["aiPatches"] = serde_json::json!([{"id": "p"}]);
        assert!(crate::image_loader::has_base_retouch(&patched));
        assert!(!of(patched, true));
    }

    /// The geometry is part of the key: another warp or edge-fringe
    /// treatment does not take another's render input; a crop or rotation
    /// (after the input) does.
    #[test]
    fn render_inputs_follow_the_warp_not_the_crop() {
        let key = |recipe: &serde_json::Value| Key::of(&global(25), recipe, true);
        let base = recipe(25);
        let mut warped = base.clone();
        warped["transformDistortion"] = serde_json::json!(20);
        let mut fringe = base.clone();
        fringe["fringeVersion"] = serde_json::json!(4);
        let mut rotated = base.clone();
        rotated["rotation"] = serde_json::json!(2.5);
        rotated["crop"] =
            serde_json::json!({"x": 10.0, "y": 10.0, "width": 300.0, "height": 200.0});
        assert_ne!(key(&base), key(&warped));
        assert_ne!(key(&base), key(&fringe));
        assert_eq!(key(&base), key(&rotated));
    }

    /// A frame without its render input wants one and renders as before;
    /// once it is in, the frame's base is built once from it and reused;
    /// another setting, a larger reduction or another photograph does not
    /// take it.
    #[test]
    fn fit_frames_use_only_their_own_render_input() {
        let view = ColorNoiseView::default();
        let (source, base) = (image(600, 400), image(200, 133));
        let builds = Cell::new(0);
        let corrected = image(200, 133);
        let frame_of = |source: &Arc<DynamicImage>, dims, amount| {
            let number = Some(view.note_frame());
            let recipe = recipe(amount);
            corrected_base(
                &view,
                &global(amount),
                FitFrame {
                    number,
                    source,
                    path: "a.raw",
                    recipe: &recipe,
                    is_raw: true,
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
        assert!(frame_of(&source, (300, 200), 25).is_none());
        let wanted = lock_recover(&view.builds).wanted.pop().unwrap();
        assert_eq!(wanted.dims, (300, 200));
        view.store(&wanted, Rgb32FImage::new(300, 200));

        let shown = frame_of(&source, (300, 200), 25).unwrap();
        assert!(Arc::ptr_eq(&shown, &corrected));
        assert!(frame_of(&source, (280, 187), 25).is_some());
        assert_eq!(builds.get(), 1, "the base made from it is kept");
        assert!(lock_recover(&view.builds).wanted.is_empty());

        assert!(frame_of(&source, (300, 200), 50).is_none());
        assert!(frame_of(&source, (310, 207), 25).is_none());
        // Another photograph drops the render input.
        let other = image(600, 400);
        assert!(frame_of(&other, (300, 200), 25).is_none());
        assert!(lock_recover(&view.inputs).is_empty());
    }

    /// The Levels pickers sample the base a settled fit frame showed, for
    /// its setting only, and never ask for one to be built.
    #[test]
    fn probes_find_the_corrected_base_the_fit_frame_shows() {
        let view = ColorNoiseView::default();
        let (source, base) = (image(600, 400), image(200, 133));
        let corrected = image(200, 133);
        let shown = |amount| {
            shown_corrected_base(&view, &all(amount), &recipe(amount), &source, true, &base)
        };
        assert!(shown(25).is_none(), "no frame showed one yet");
        assert!(
            lock_recover(&view.builds).wanted.is_empty(),
            "a probe wants none"
        );
        let frame = || {
            corrected_base(
                &view,
                &global(25),
                FitFrame {
                    number: Some(view.note_frame()),
                    source: &source,
                    path: "a.raw",
                    recipe: &recipe(25),
                    is_raw: true,
                    base: &base,
                    reduced_dims: (300, 200),
                },
                |_| Ok(Arc::clone(&corrected)),
                refresh,
            )
        };
        assert!(frame().is_none());
        view.note_shown(None);
        let wanted = lock_recover(&view.builds).wanted.pop().unwrap();
        view.store(&wanted, Rgb32FImage::new(300, 200));
        let used = frame().unwrap();
        assert!(
            shown(25).is_none(),
            "built, but no frame rendered from it yet"
        );
        view.note_shown(Some(&used));
        assert!(Arc::ptr_eq(&shown(25).unwrap(), &corrected));
        // A render nobody sees (the drag-draft warm-up) changes nothing.
        let mark = view.shown_mark();
        view.note_shown(None);
        view.restore_shown(mark);
        assert!(Arc::ptr_eq(&shown(25).unwrap(), &corrected));
        // A zoomed or dragged frame after it renders from its own base.
        view.note_shown(None);
        assert!(shown(25).is_none());
        view.note_shown(Some(&used));
        // Another setting or base: the frame would not show it.
        assert!(shown(50).is_none());
        let other = image(200, 133);
        assert!(
            shown_corrected_base(&view, &all(25), &recipe(25), &source, true, &other).is_none()
        );
    }

    /// A frame nobody sees (the drag-draft warm-up) keeps the frame before
    /// it the latest; a frame shown after it does not.
    #[test]
    fn an_unseen_frame_leaves_the_last_shown_frame_current() {
        let view = ColorNoiseView::default();
        let shown = view.note_frame();
        let before = view.frames_noted();
        view.note_frame();
        view.forget_frame(before);
        assert_eq!(view.frames_noted(), shown);
        view.note_frame();
        view.note_frame();
        view.forget_frame(before);
        assert_eq!(view.frames_noted(), shown + 2);
    }

    /// A before asking for the edit's render input keeps the edit's frame
    /// to render again, and the larger size.
    #[test]
    fn a_before_wanting_the_same_render_input_keeps_the_edits_frame() {
        let view = ColorNoiseView::default();
        let source = image(600, 400);
        view.want(wanted_for(
            &source,
            recipe(25),
            Some((3, refresh())),
            (300, 200),
        ));
        view.want(wanted_for(&source, recipe(25), None, (310, 207)));
        let builds = lock_recover(&view.builds);
        assert_eq!(builds.wanted.len(), 1);
        assert_eq!(builds.wanted[0].dims, (310, 207));
        assert_eq!(builds.wanted[0].frame.as_ref().map(|f| f.0), Some(3));
    }

    #[test]
    fn thumbnails_take_the_render_input_of_their_photograph_and_setting() {
        let view = ColorNoiseView::default();
        let source = image(600, 400);
        let wanted = wanted_for(&source, recipe(25), Some((1, refresh())), (300, 200));
        view.store(&wanted, Rgb32FImage::new(300, 200));
        let thumb = |path, amount, is_raw| {
            for_thumbnail(&view, path, &all(amount), &recipe(amount), is_raw)
        };
        assert!(thumb("a.raw", 25, true).is_some());
        assert!(thumb("a.raw", 50, true).is_none());
        // The split view's before with another setting keeps its own beside
        // the edit's; a third setting replaces the oldest.
        let revision = view.revision();
        view.store(
            &wanted_for(&source, recipe(50), None, (300, 200)),
            Rgb32FImage::new(300, 200),
        );
        assert_ne!(view.revision(), revision);
        assert!(thumb("a.raw", 25, true).is_some());
        assert!(thumb("a.raw", 50, true).is_some());
        view.store(
            &wanted_for(&source, recipe(75), None, (300, 200)),
            Rgb32FImage::new(300, 200),
        );
        assert!(thumb("a.raw", 25, true).is_none());
        assert!(thumb("a.raw", 50, true).is_some());
        view.store(&wanted, Rgb32FImage::new(300, 200));
        assert!(thumb("b.raw", 25, true).is_none());
        assert!(thumb("a.raw", 25, false).is_none());
        view.clear();
        assert!(thumb("a.raw", 25, true).is_none());
    }

    /// The stored render input keeps a colour beyond sRGB (negative red):
    /// its reduction (`SignedRowStream::wide_gamut`) cut the ringing already
    /// and kept such colours, as the view's own reduction.
    #[test]
    fn stored_render_inputs_keep_colours_beyond_srgb() {
        let view = ColorNoiseView::default();
        let source = image(600, 400);
        let mut input = Rgb32FImage::new(4, 4);
        input.put_pixel(1, 1, image::Rgb([-0.01, 0.5, 0.2]));
        view.store(&wanted_for(&source, recipe(25), None, (4, 4)), input);
        let kept = for_thumbnail(&view, "a.raw", &all(25), &recipe(25), true).unwrap();
        assert_eq!(kept.get_pixel(1, 1).0, [-0.01, 0.5, 0.2]);
    }
}
