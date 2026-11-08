//! The second window (G54; Lightroom's Secondary Display, Capture One's
//! Viewer): one more window, typically on a second display, that shows the
//! current photograph live with its own Fit / 100 % zoom and pan, or, locked,
//! keeps showing one photograph while the main window moves on.
//!
//! It presents with the same native wgpu path as the main window, on a
//! surface of its own on the same device: a single multi view cell
//! (crates/gpu multi_view.rs) fills the window. Each whole frame the main
//! display binds (`on_live_frame`) is copied into that cell on the GPU, so
//! every edit shows in both windows at once without developing twice. When
//! the window needs more pixels than the main frame holds (100 %, or a fit
//! on a larger display) or the main frame is a tool's view (clipping
//! warnings), the photograph is developed again for this window once the
//! edit settles, from the recipe the main frame was rendered with
//! (multi_view.rs `develop_whole`, `develop_region`). Develop pixels never
//! cross IPC; the window's webview only sends its zoom and draws its
//! toolbar over the photograph.
//!
//! The window's place, size, display, full screen and zoom are kept for the
//! next start (`second_window.json` in the config folder), and it opens
//! again when it was open at quit.

// Linux has no native display: the window is never opened there.
#![cfg_attr(target_os = "linux", allow(dead_code))]

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::{Emitter, Manager};

use crate::gpu_processing::{
    self, CellExtent, CellLayer, CellZoom, GpuContext, LatestJobSender, LiveFrame, MultiViewCell,
    WgpuDisplay,
};
use crate::multi_view::{self, SecondBases, ZoomPayload};
use crate::state::lock::lock_recover;

/// The window's label (its webview renders `SecondView` for it).
pub const LABEL: &str = "second-view";
/// Sent to every webview whenever what the window shows changes.
const STATE_EVENT: &str = "second-window-state";
const STATE_FILE: &str = "second_window.json";
/// The smallest window kept or restored (logical px).
const MIN_SIZE: (f64, f64) = (320.0, 240.0);
/// How long the edit must rest before the window develops its own frame.
const SETTLE: Duration = Duration::from_millis(120);

/// The window as kept for the next start. Bounds are logical px (points),
/// which mean the same on displays of different scale: the outer top left
/// (which also names the display) and the inner size.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SavedSecondWindow {
    pub open: bool,
    pub bounds: Option<SavedBounds>,
    pub fullscreen: bool,
    /// Shown at 100 % (else to fit).
    pub actual_size: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedBounds {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// What the window shows, for its toolbar and the main window's menu.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SecondViewState {
    /// Whether this platform and renderer can show the window at all.
    pub available: bool,
    pub open: bool,
    pub locked: bool,
    pub path: Option<String>,
    /// The photograph's shown size (cropped, turned), full-resolution px.
    pub shown: Option<[f32; 2]>,
    /// None: to fit; else device px per full-resolution px and the
    /// photograph point (0–1) at the window's centre.
    pub zoom: Option<ZoomState>,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ZoomState {
    pub scale: f32,
    pub center: [f32; 2],
}

/// The photograph in the cell and the recipe its frames are of.
#[derive(Clone)]
struct Shown {
    path: String,
    recipe_key: u64,
    recipe: Arc<serde_json::Value>,
    /// Unknown until the photograph's size is (a followed Library photo).
    shown: Option<[f32; 2]>,
}

/// The cell's whole-photograph texture.
#[derive(Clone, Copy)]
struct Whole {
    recipe_key: u64,
    pixels: (u32, u32),
    /// Developed for this window at this window area (device px; else
    /// copied from the main frame).
    own: Option<[f32; 4]>,
}

#[derive(Default)]
struct Inner {
    display: Option<WgpuDisplay>,
    /// The device `display` presents with; a frame of another (a context
    /// replaced at start) is never copied into it.
    device: Option<Arc<wgpu::Device>>,
    area: [f32; 4],
    shown: Option<Shown>,
    whole: Option<Whole>,
    zoom: Option<CellZoom>,
    locked: bool,
    /// The background around the photograph (the main viewer's).
    background: [f32; 4],
}

/// The recipe of the newest preview the main window rendered.
struct Live {
    path: String,
    key: u64,
    recipe: Arc<serde_json::Value>,
    interactive: bool,
    full: (u32, u32),
}

static OPEN: AtomicBool = AtomicBool::new(false);
static INNER: Mutex<Option<Inner>> = Mutex::new(None);
static LIVE: Mutex<Option<Live>> = Mutex::new(None);
/// The Library's active photograph, asked for after the main window's
/// last preview (None once a preview follows): what an unlock or an
/// opening shows.
static FOLLOW: Mutex<Option<String>> = Mutex::new(None);
static GENERATION: AtomicUsize = AtomicUsize::new(0);
static SAVED: Mutex<Option<(PathBuf, SavedSecondWindow)>> = Mutex::new(None);
static SAVE_PENDING: AtomicBool = AtomicBool::new(false);
/// The window's newest size in device px (width << 32 | height).
static WINDOW_SIZE: AtomicU64 = AtomicU64::new(0);

fn bases() -> &'static SecondBases {
    static BASES: OnceLock<SecondBases> = OnceLock::new();
    BASES.get_or_init(SecondBases::default)
}

fn recipe_key(recipe: &serde_json::Value) -> u64 {
    let mut hash = DefaultHasher::new();
    recipe.to_string().hash(&mut hash);
    hash.finish() | 1
}

/// Whether the window is open (one atomic load: the main window's preview
/// path pays nothing more while it is closed).
pub fn is_open() -> bool {
    OPEN.load(Ordering::Relaxed)
}

/// Notes the recipe of the main window's preview about to be rendered
/// (`editor_preview::process_preview_job`); its frame follows in
/// [`on_frame`].
pub(crate) fn note_live(
    path: &str,
    recipe: &serde_json::Value,
    interactive: bool,
    full: (u32, u32),
) {
    // While closed only the settled recipe is kept (no key: the hash is
    // made when the window opens), so opening shows the photo at once.
    if !is_open() {
        if !interactive {
            *lock_recover(&FOLLOW) = None;
            *lock_recover(&LIVE) = Some(Live {
                path: path.to_string(),
                key: 0,
                recipe: Arc::new(recipe.clone()),
                interactive,
                full,
            });
        }
        return;
    }
    *lock_recover(&FOLLOW) = None;
    let key = recipe_key(recipe);
    let mut live = lock_recover(&LIVE);
    let recipe = match live.as_ref() {
        Some(l) if l.key == key && l.path == path => Arc::clone(&l.recipe),
        _ => Arc::new(recipe.clone()),
    };
    *live = Some(Live {
        path: path.to_string(),
        key,
        recipe,
        interactive,
        full,
    });
}

/// Starts listening to the main display's frames (once, at start-up).
pub fn install(app_handle: &tauri::AppHandle) {
    let app = app_handle.clone();
    gpu_processing::on_live_frame(move |context, frame| on_frame(&app, context, frame));
}

/// The main display bound `frame`: shown here too unless the window is
/// locked, the frame is the crop tool's uncropped one, or a tool's view.
fn on_frame(app: &tauri::AppHandle, context: &GpuContext, frame: &LiveFrame) {
    if !is_open() {
        return;
    }
    let Some(path) = frame.path else { return };
    // Only a frame whose recipe is known (the preview job's own): an
    // activation still of the next photograph waits for it.
    let live = lock_recover(&LIVE)
        .as_ref()
        .filter(|live| live.path == path)
        .map(|live| {
            (
                live.key,
                Arc::clone(&live.recipe),
                live.interactive,
                live.full,
            )
        });
    let Some((key, recipe, interactive, full)) = live else {
        return;
    };
    #[cfg(feature = "performance-profiling")]
    let started = std::time::Instant::now();
    let background = main_background(context);
    let mut changed = false;
    {
        let mut guard = lock_recover(&INNER);
        let Some(inner) = guard.as_mut() else { return };
        if inner.locked
            || !inner
                .device
                .as_ref()
                .is_some_and(|device| Arc::ptr_eq(device, &context.device))
        {
            return;
        }
        if let Some(background) = background {
            inner.background = background;
        }
        let (w, h) = multi_view::shown_size_of(&recipe, full.0, full.1);
        changed |= set_photo(inner, context, path, key, recipe, Some([w, h]));
        if frame.uncropped {
            return;
        }
        if !frame.tool_view {
            let keep_own = inner.whole.is_some_and(|whole| {
                whole.own.is_some()
                    && whole.recipe_key == key
                    && u64::from(whole.pixels.0) * u64::from(whole.pixels.1)
                        >= u64::from(frame.width) * u64::from(frame.height)
            });
            if let Some(display) = inner.display.as_mut() {
                if !keep_own
                    && display.copy_cell_frame(
                        &context.device,
                        &context.queue,
                        path,
                        frame.texture,
                        (frame.width, frame.height),
                        key,
                        Some(CellExtent {
                            shown: [w, h],
                            region: None,
                        }),
                    )
                {
                    inner.whole = Some(Whole {
                        recipe_key: key,
                        pixels: (frame.width, frame.height),
                        own: None,
                    });
                }
                // A region of an earlier recipe would cover the edit.
                if !region_is_of(display, path, key) {
                    display.remove_cell_region(path);
                }
                paint(display, context, inner.background);
            }
        }
    }
    #[cfg(feature = "performance-profiling")]
    log::info!(
        "[PERF] {}",
        serde_json::json!({
            "stage": "second_window_frame", "ms": started.elapsed().as_secs_f64() * 1e3,
            "size": [frame.width, frame.height], "interactive": interactive,
            "tool_view": frame.tool_view, "uncropped": frame.uncropped,
        })
    );
    if changed {
        emit_state(app);
    }
    if !interactive {
        schedule(app);
    }
}

/// Whether the cell's zoomed region was developed from the recipe `key`.
fn region_is_of(display: &WgpuDisplay, path: &str, key: u64) -> bool {
    display
        .multi_view()
        .region_of(path)
        .is_some_and(|(region_key, _)| region_key == region_recipe_tag(key))
}

/// The tag a region developed from recipe `key` is kept under: what
/// [`region_is_of`] compares (the gpu crate keeps one key per texture).
fn region_recipe_tag(key: u64) -> u64 {
    key.rotate_left(17) | 1
}

/// The main viewer's background, so the photograph sits on the same grey.
fn main_background(context: &GpuContext) -> Option<[f32; 4]> {
    context
        .display
        .try_lock()
        .ok()
        .and_then(|display| display.as_ref().map(|d| d.latest_transform.bg_primary))
}

/// Shows `path` with recipe `key` in the cell; true when the photograph or
/// its shown size changed (the webview is told).
fn set_photo(
    inner: &mut Inner,
    context: &GpuContext,
    path: &str,
    key: u64,
    recipe: Arc<serde_json::Value>,
    shown: Option<[f32; 2]>,
) -> bool {
    let same_photo = inner.shown.as_ref().is_some_and(|s| s.path == path);
    let size_changed = inner.shown.as_ref().map(|s| s.shown) != Some(shown);
    if !same_photo {
        inner.whole = None;
        // A new photograph opens at the same zoom, centred.
        if let Some(zoom) = inner.zoom.as_mut() {
            zoom.center = [0.5, 0.5];
        }
        if let Some(display) = inner.display.as_mut() {
            display.set_multi_view(
                &context.device,
                &context.queue,
                None,
                vec![MultiViewCell {
                    path: path.to_string(),
                    area: inner.area,
                }],
            );
            display.set_multi_view_zoom(inner.zoom);
        }
        bases().keep_only(path);
    }
    inner.shown = Some(Shown {
        path: path.to_string(),
        recipe_key: key,
        recipe,
        shown: shown.or_else(|| inner.shown.as_ref().filter(|_| same_photo)?.shown),
    });
    !same_photo || size_changed
}

fn paint(display: &mut WgpuDisplay, context: &GpuContext, background: [f32; 4]) {
    display.latest_transform.bg_primary = background;
    display.latest_transform.bg_secondary = background;
    display.render(&context.device, &context.queue);
}

/// What the window wants developed for itself, worked out on the worker.
enum Want {
    Whole {
        recipe: Arc<serde_json::Value>,
    },
    Region {
        recipe: Arc<serde_json::Value>,
        zoom: CellZoom,
    },
}

struct Job {
    generation: usize,
}

fn worker(app_handle: &tauri::AppHandle) -> &'static LatestJobSender<Job> {
    static WORKER: OnceLock<LatestJobSender<Job>> = OnceLock::new();
    WORKER.get_or_init(|| {
        let app = app_handle.clone();
        gpu_processing::start_latest_job_worker(move |job: Job| run(&app, job))
    })
}

/// Asks the worker to bring the window's own frames up to date once the
/// edit rests.
fn schedule(app_handle: &tauri::AppHandle) {
    let generation = GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    worker(app_handle).send(Job { generation });
}

/// Whether a whole frame of `pixels` holds what a fit in `area` shows of a
/// photograph `shown` large (within a few percent: a fit of 3000 px from a
/// 2944 px frame is not developed again).
fn whole_suffices(pixels: (u32, u32), area: [f32; 4], shown: [f32; 2]) -> bool {
    let fit = gpu_processing::multi_view::fit_rect(area, shown[0], shown[1]);
    let needed = fit[2].max(fit[3]);
    let have = pixels.0.max(pixels.1) as f32;
    // A photograph smaller than the window is never enlarged beyond 1:1.
    let full = shown[0].max(shown[1]);
    have >= needed.min(full) * 0.97
}

/// Whether the cell's whole frame serves a fit in `area`: developed here
/// for an area at least as large (never asked for again at the same size,
/// even when the source gave fewer pixels), or holding enough pixels.
fn whole_is_enough(whole: Whole, area: [f32; 4], shown: [f32; 2]) -> bool {
    whole
        .own
        .is_some_and(|a| a[2] >= area[2] && a[3] >= area[3])
        || whole_suffices(whole.pixels, area, shown)
}

fn want(inner: &Inner) -> Option<Want> {
    let shown = inner.shown.as_ref()?;
    let display = inner.display.as_ref()?;
    let whole_current = inner.whole.filter(|w| w.recipe_key == shown.recipe_key);
    let whole_ok = match (whole_current, shown.shown) {
        (Some(whole), Some(size)) => {
            inner.zoom.is_some() || whole_is_enough(whole, inner.area, size)
        }
        (Some(_), None) => true,
        (None, _) => false,
    };
    if !whole_ok {
        return Some(Want::Whole {
            recipe: Arc::clone(&shown.recipe),
        });
    }
    let zoom = inner.zoom?;
    let size = shown.shown?;
    let current = display
        .multi_view()
        .region_of(&shown.path)
        .filter(|(key, _)| *key == region_recipe_tag(shown.recipe_key))
        .and_then(|(_, extent)| extent.region)
        .is_some_and(|region| {
            gpu_processing::multi_view::region_covers(region, inner.area, size, zoom)
        });
    // A photograph shown at most 1:1 by its whole frame needs no region.
    let whole_is_sharp = inner.whole.is_some_and(|w| {
        w.pixels.0.max(w.pixels.1) as f32 >= size[0].max(size[1]) * zoom.scale * 0.97
    });
    (!current && !whole_is_sharp).then(|| Want::Region {
        recipe: Arc::clone(&shown.recipe),
        zoom,
    })
}

fn run(app_handle: &tauri::AppHandle, job: Job) {
    std::thread::sleep(SETTLE);
    if GENERATION.load(Ordering::SeqCst) != job.generation || !is_open() {
        return;
    }
    let Some(context) = app_handle.state::<crate::state::gpu::GpuState>().context() else {
        return;
    };
    let (path, recipe_key, area, wanted) = {
        let guard = lock_recover(&INNER);
        let Some(inner) = guard.as_ref() else { return };
        let Some(shown) = inner.shown.as_ref() else {
            return;
        };
        let Some(wanted) = want(inner) else { return };
        (shown.path.clone(), shown.recipe_key, inner.area, wanted)
    };
    let started = std::time::Instant::now();
    match wanted {
        Want::Whole { recipe } => {
            match multi_view::develop_whole(
                app_handle,
                &context,
                bases(),
                &path,
                (*recipe).clone(),
                area,
            ) {
                Ok(developed) => {
                    let mut changed = false;
                    {
                        let mut guard = lock_recover(&INNER);
                        let Some(inner) = guard.as_mut() else { return };
                        if !inner
                            .shown
                            .as_ref()
                            .is_some_and(|s| s.path == path && s.recipe_key == recipe_key)
                        {
                            return;
                        }
                        if let Some(shown) = inner.shown.as_mut()
                            && shown.shown != Some(developed.extent.shown)
                        {
                            shown.shown = Some(developed.extent.shown);
                            changed = true;
                        }
                        let background = inner.background;
                        if let Some(display) = inner.display.as_mut() {
                            display.upload_cell(
                                &context.device,
                                &context.queue,
                                &path,
                                developed.key,
                                developed.extent,
                                CellLayer::Whole,
                                developed.rgba.width(),
                                developed.rgba.height(),
                                developed.rgba.as_raw(),
                            );
                            paint(display, &context, background);
                        }
                        inner.whole = Some(Whole {
                            recipe_key,
                            pixels: developed.rgba.dimensions(),
                            own: Some(area),
                        });
                    }
                    log::info!(
                        "[second_window] {path}: whole {}x{} from {} in {:.1} ms",
                        developed.rgba.width(),
                        developed.rgba.height(),
                        developed.source_kind,
                        developed.ms
                    );
                    if changed {
                        emit_state(app_handle);
                    }
                    // Zoomed in, the region follows.
                    schedule(app_handle);
                }
                Err(error) => log::warn!("[second_window] {path}: not developed: {error}"),
            }
        }
        Want::Region { recipe, zoom } => {
            let tag = region_recipe_tag(recipe_key);
            match multi_view::develop_region(
                app_handle,
                &context,
                &bases().regions,
                &path,
                area,
                zoom,
                (*recipe).clone(),
                tag,
            ) {
                Ok(region) => {
                    let mut guard = lock_recover(&INNER);
                    let Some(inner) = guard.as_mut() else { return };
                    if !inner
                        .shown
                        .as_ref()
                        .is_some_and(|s| s.path == path && s.recipe_key == recipe_key)
                        || inner.zoom.map(|z| z.scale) != Some(zoom.scale)
                    {
                        return;
                    }
                    let background = inner.background;
                    if let Some(display) = inner.display.as_mut() {
                        display.upload_cell(
                            &context.device,
                            &context.queue,
                            &path,
                            region.key,
                            region.extent,
                            CellLayer::Region,
                            region.rgba.width(),
                            region.rgba.height(),
                            region.rgba.as_raw(),
                        );
                        paint(display, &context, background);
                    }
                    log::info!(
                        "[second_window] {path}: region {}x{} in {:.1} ms",
                        region.rgba.width(),
                        region.rgba.height(),
                        region.ms
                    );
                }
                Err(error) => log::warn!("[second_window] {path}: region not developed: {error}"),
            }
        }
    }
    #[cfg(feature = "performance-profiling")]
    log::info!(
        "[PERF] {}",
        serde_json::json!({
            "stage": "second_window_develop",
            "ms": started.elapsed().as_secs_f64() * 1e3,
        })
    );
    #[cfg(not(feature = "performance-profiling"))]
    let _ = started;
}

/// Whether this build and platform can show the window: it presents with
/// the native display, which Linux and the browser renderer do not have.
fn available(app_handle: &tauri::AppHandle) -> bool {
    cfg!(not(target_os = "linux"))
        && crate::state::settings::current(app_handle)
            .use_wgpu_renderer
            .unwrap_or(true)
}

fn state_of(app_handle: &tauri::AppHandle) -> SecondViewState {
    let guard = lock_recover(&INNER);
    let inner = guard.as_ref();
    SecondViewState {
        available: available(app_handle),
        open: is_open(),
        locked: inner.is_some_and(|i| i.locked),
        path: inner.and_then(|i| i.shown.as_ref().map(|s| s.path.clone())),
        shown: inner.and_then(|i| i.shown.as_ref().and_then(|s| s.shown)),
        zoom: inner.and_then(|i| {
            i.zoom.map(|z| ZoomState {
                scale: z.scale,
                center: z.center,
            })
        }),
    }
}

fn emit_state(app_handle: &tauri::AppHandle) {
    let _ = app_handle.emit(STATE_EVENT, state_of(app_handle));
}

// ---------------------------------------------------------------- saving

fn saved_path(app_handle: &tauri::AppHandle) -> Option<PathBuf> {
    app_handle
        .path()
        .app_config_dir()
        .ok()
        .map(|dir| dir.join(STATE_FILE))
}

/// The kept state (read once from disk).
fn saved(app_handle: &tauri::AppHandle) -> SavedSecondWindow {
    let mut slot = lock_recover(&SAVED);
    if let Some((_, state)) = slot.as_ref() {
        return state.clone();
    }
    let Some(path) = saved_path(app_handle) else {
        return SavedSecondWindow::default();
    };
    let state = std::fs::read_to_string(&path)
        .ok()
        .and_then(|json| serde_json::from_str(&json).ok())
        .unwrap_or_default();
    *slot = Some((path, state));
    slot.as_ref().map(|(_, s)| s.clone()).unwrap_or_default()
}

/// Changes the kept state; written by the next [`flush`].
fn update_saved(app_handle: &tauri::AppHandle, change: impl FnOnce(&mut SavedSecondWindow)) {
    let _ = saved(app_handle);
    let mut slot = lock_recover(&SAVED);
    if let Some((_, state)) = slot.as_mut() {
        let before = state.clone();
        change(state);
        if *state != before {
            SAVE_PENDING.store(true, Ordering::SeqCst);
        }
    }
}

/// Writes the kept state if it changed (every `window_state::INTERVAL`
/// and when the app quits).
pub fn flush() {
    if !SAVE_PENDING.swap(false, Ordering::SeqCst) {
        return;
    }
    let slot = lock_recover(&SAVED);
    let Some((path, state)) = slot.as_ref() else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    match serde_json::to_string(state) {
        Ok(json) => {
            if let Err(e) = rawtonic_fs::atomic_write(path, json) {
                log::warn!("Could not save the second window's state: {e}");
            }
        }
        Err(e) => log::warn!("Could not save the second window's state: {e}"),
    }
}

/// Where the window opens: where it was, if that is still on a display;
/// else on a display other than the main window's (centred, 80 % of it),
/// else beside the main window on its display.
pub(crate) fn placement(
    saved: Option<SavedBounds>,
    monitors: &[crate::startup::MonitorBounds],
    main_monitor: Option<crate::startup::MonitorBounds>,
) -> Option<SavedBounds> {
    if let Some(bounds) = saved
        && crate::startup::overlaps_a_monitor(
            bounds.x,
            bounds.y,
            bounds.width,
            bounds.height,
            monitors,
        )
    {
        return Some(bounds);
    }
    let other = monitors.iter().find(|m| Some(**m) != main_monitor).copied();
    let (monitor, share) = match (other, main_monitor) {
        (Some(m), _) => (m, 0.8),
        (None, Some(m)) => (m, 0.6),
        (None, None) => return None,
    };
    let width = (monitor.width as f64 * share) as u32;
    let height = (monitor.height as f64 * share) as u32;
    Some(SavedBounds {
        x: monitor.x + ((monitor.width - width) / 2) as i32,
        y: monitor.y + ((monitor.height - height) / 2) as i32,
        width,
        height,
    })
}

// ---------------------------------------------------------------- window

/// A display's bounds in logical px (its own scale).
#[cfg(not(target_os = "linux"))]
fn monitor_bounds(monitor: &tauri::Monitor) -> crate::startup::MonitorBounds {
    let scale = monitor.scale_factor();
    let position = monitor.position().to_logical::<f64>(scale);
    let size = monitor.size().to_logical::<f64>(scale);
    crate::startup::MonitorBounds {
        x: position.x.round() as i32,
        y: position.y.round() as i32,
        width: size.width.round() as u32,
        height: size.height.round() as u32,
    }
}

/// Opens the window (or brings it to the front).
async fn open_window(app_handle: &tauri::AppHandle) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        let _ = app_handle;
        Err("The second window needs the native display, which Linux does not have".into())
    }
    #[cfg(not(target_os = "linux"))]
    open_native(app_handle).await
}

#[cfg(not(target_os = "linux"))]
async fn open_native(app_handle: &tauri::AppHandle) -> Result<(), String> {
    if !available(app_handle) {
        return Err("The second window needs the native display renderer".into());
    }
    if let Some(window) = app_handle.get_webview_window(LABEL) {
        let _ = window.show();
        let _ = window.set_focus();
        return Ok(());
    }
    let gpu = app_handle.state::<crate::state::gpu::GpuState>();
    let context = {
        let app = app_handle.clone();
        tauri::async_runtime::spawn_blocking(move || {
            let gpu = app.state::<crate::state::gpu::GpuState>();
            gpu_processing::get_or_init_gpu_context(&gpu, &app)
        })
        .await
        .map_err(|e| e.to_string())??
    };
    let Some((instance, adapter)) = lock_recover(&gpu.adapter).clone() else {
        return Err("No GPU adapter".into());
    };

    let saved_state = saved(app_handle);
    let main = app_handle.get_webview_window("main");
    let monitors: Vec<_> = main
        .as_ref()
        .and_then(|w| w.available_monitors().ok())
        .unwrap_or_default()
        .iter()
        .map(monitor_bounds)
        .collect();
    let main_monitor = main
        .as_ref()
        .and_then(|w| w.current_monitor().ok().flatten())
        .map(|m| monitor_bounds(&m));
    let bounds = placement(saved_state.bounds, &monitors, main_monitor);

    let mut builder = tauri::WebviewWindowBuilder::new(
        app_handle,
        LABEL,
        tauri::WebviewUrl::App("index.html".into()),
    )
    .title("Rawtonic — Second Window")
    .transparent(true)
    .min_inner_size(MIN_SIZE.0, MIN_SIZE.1)
    .visible(false);
    #[cfg(target_os = "macos")]
    {
        builder = builder.title_bar_style(tauri::TitleBarStyle::Visible);
    }
    if bounds.is_none() {
        builder = builder.inner_size(1280.0, 800.0).center();
    }
    let window = builder.build().map_err(|e| e.to_string())?;
    if let Some(bounds) = bounds {
        let _ = window.set_size(tauri::Size::Logical(tauri::LogicalSize::new(
            f64::from(bounds.width),
            f64::from(bounds.height),
        )));
        let _ = window.set_position(tauri::Position::Logical(tauri::LogicalPosition::new(
            f64::from(bounds.x),
            f64::from(bounds.y),
        )));
    }

    // The surface is made on the main thread (AppKit's view and layer).
    let (send, receive) = std::sync::mpsc::channel();
    {
        let window = window.clone();
        let device = Arc::clone(&context.device);
        app_handle
            .run_on_main_thread(move || {
                let made = (|| {
                    let surface = instance
                        .create_surface(window.clone())
                        .map_err(|e| e.to_string())?;
                    if !adapter.is_surface_supported(&surface) {
                        return Err("The GPU cannot present to this window".to_string());
                    }
                    gpu_processing::configure_display(&device, &adapter, surface, &window)
                })();
                let _ = send.send(made);
            })
            .map_err(|e| e.to_string())?;
    }
    let display = tauri::async_runtime::spawn_blocking(move || {
        receive
            .recv_timeout(Duration::from_secs(10))
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())??;
    let mut display = match display {
        Ok(display) => display,
        Err(error) => {
            let _ = window.destroy();
            return Err(error);
        }
    };
    // Placed and sized after the surface was configured, or meanwhile.
    let size = window
        .inner_size()
        .unwrap_or(tauri::PhysicalSize::new(1280, 800));
    if (display.config.width, display.config.height) != (size.width.max(1), size.height.max(1)) {
        display.resize(&context.device, &context.queue, size.width, size.height);
    }
    {
        let mut guard = lock_recover(&INNER);
        *guard = Some(Inner {
            display: Some(display),
            device: Some(Arc::clone(&context.device)),
            area: [0.0, 0.0, size.width as f32, size.height as f32],
            zoom: saved_state.actual_size.then_some(CellZoom {
                scale: 1.0,
                center: [0.5, 0.5],
            }),
            background: main_background(&context).unwrap_or([0.094, 0.094, 0.094, 1.0]),
            ..Inner::default()
        });
    }
    OPEN.store(true, Ordering::SeqCst);
    let _ = window.show();
    if saved_state.fullscreen {
        let _ = crate::window_customizer::set_window_full_screen(window.clone(), true);
    }
    update_saved(app_handle, |s| s.open = true);
    note_bounds(app_handle, &window);
    show_current(app_handle, &context);
    emit_state(app_handle);
    Ok(())
}

/// Fills a newly opened or unlocked window with the main window's current
/// photograph (developed for it: the main frame is not at hand here).
fn show_current(app_handle: &tauri::AppHandle, context: &GpuContext) {
    // The Library's active photograph when it was asked for after the
    // main window's last preview, else that preview's photograph.
    if let Some(path) = lock_recover(&FOLLOW).clone() {
        show_followed(app_handle, context, path);
        return;
    }
    let live = lock_recover(&LIVE).as_mut().map(|l| {
        if l.key == 0 {
            l.key = recipe_key(&l.recipe);
        }
        (l.path.clone(), l.key, Arc::clone(&l.recipe), l.full)
    });
    if let Some((path, key, recipe, full)) = live {
        {
            let mut guard = lock_recover(&INNER);
            let Some(inner) = guard.as_mut() else { return };
            let (w, h) = multi_view::shown_size_of(&recipe, full.0, full.1);
            set_photo(inner, context, &path, key, recipe, Some([w, h]));
        }
        schedule(app_handle);
    }
}

/// Shows `path` from its stored recipe (the Library's active photograph).
fn show_followed(app_handle: &tauri::AppHandle, context: &GpuContext, path: String) {
    {
        let mut guard = lock_recover(&INNER);
        let Some(inner) = guard.as_mut() else { return };
        if inner.locked || inner.shown.as_ref().is_some_and(|s| s.path == path) {
            return;
        }
        let recipe = multi_view::recipe_of(&path);
        let key = recipe_key(&recipe);
        let shown = multi_view::full_size_of(&path)
            .map(|(w, h)| multi_view::shown_size_of(&recipe, w, h))
            .map(|(w, h)| [w, h]);
        set_photo(inner, context, &path, key, Arc::new(recipe), shown);
    }
    emit_state(app_handle);
    schedule(app_handle);
}

/// The window is closing: its surface goes first, while the window lives.
pub(crate) fn closing(app_handle: &tauri::AppHandle) {
    OPEN.store(false, Ordering::SeqCst);
    GENERATION.fetch_add(1, Ordering::SeqCst);
    *lock_recover(&INNER) = None;
    bases().clear();
    update_saved(app_handle, |s| s.open = false);
    emit_state(app_handle);
}

async fn close(app_handle: &tauri::AppHandle) {
    closing(app_handle);
    if let Some(window) = app_handle.get_webview_window(LABEL) {
        let _ = window.close();
    }
}

/// The window was resized (device px).
pub(crate) fn resized(
    app_handle: &tauri::AppHandle,
    window: &tauri::Window,
    width: u32,
    height: u32,
) {
    if !is_open() {
        return;
    }
    // The newest size; applied off the main thread (a frame may hold the
    // display), always the last one of a series of resizes.
    WINDOW_SIZE.store(
        (u64::from(width) << 32) | u64::from(height),
        Ordering::SeqCst,
    );
    if let Some(window) = app_handle.get_webview_window(window.label()) {
        note_bounds(app_handle, &window);
    }
    let app = app_handle.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let Some(context) = app.state::<crate::state::gpu::GpuState>().context() else {
            return;
        };
        let size = WINDOW_SIZE.load(Ordering::SeqCst);
        let (width, height) = (((size >> 32) as u32).max(1), (size as u32).max(1));
        {
            let mut guard = lock_recover(&INNER);
            let Some(inner) = guard.as_mut() else { return };
            let area = [0.0, 0.0, width as f32, height as f32];
            if inner.area == area {
                return;
            }
            inner.area = area;
            let path = inner.shown.as_ref().map(|s| s.path.clone());
            let zoom = inner.zoom;
            let background = inner.background;
            if let Some(display) = inner.display.as_mut() {
                if let Some(path) = path {
                    display.set_multi_view(
                        &context.device,
                        &context.queue,
                        None,
                        vec![MultiViewCell { path, area }],
                    );
                    display.set_multi_view_zoom(zoom);
                }
                display.latest_transform.bg_primary = background;
                display.latest_transform.bg_secondary = background;
                display.resize(&context.device, &context.queue, width, height);
            }
        }
        schedule(&app);
    });
}

/// Draws the window again (it was covered: a covered window's frame is
/// not presented, see `WgpuDisplay::render`).
pub(crate) fn repaint(app_handle: &tauri::AppHandle) {
    if !is_open() {
        return;
    }
    let Some(context) = app_handle.state::<crate::state::gpu::GpuState>().context() else {
        return;
    };
    tauri::async_runtime::spawn_blocking(move || {
        let mut guard = lock_recover(&INNER);
        if let Some(inner) = guard.as_mut() {
            let background = inner.background;
            if let Some(display) = inner.display.as_mut() {
                paint(display, &context, background);
            }
        }
    });
}

/// Keeps the window's place and size (not while full screen or minimised:
/// it returns to the frame it had) and whether it is full screen.
pub(crate) fn note_bounds(app_handle: &tauri::AppHandle, window: &tauri::WebviewWindow) {
    if !is_open() || window.is_minimized().unwrap_or(false) {
        return;
    }
    let fullscreen = window.is_fullscreen().unwrap_or(false);
    let bounds = match (
        window.outer_position(),
        window.inner_size(),
        window.scale_factor(),
    ) {
        (Ok(p), Ok(s), Ok(scale)) if !fullscreen => {
            let p = p.to_logical::<f64>(scale);
            let s = s.to_logical::<f64>(scale);
            Some(SavedBounds {
                x: p.x.round() as i32,
                y: p.y.round() as i32,
                width: s.width.round() as u32,
                height: s.height.round() as u32,
            })
        }
        _ => None,
    };
    update_saved(app_handle, |saved| {
        saved.fullscreen = fullscreen;
        if bounds.is_some() {
            saved.bounds = bounds;
        }
    });
}

/// Opens it again at start-up when it was open at quit.
pub fn restore(app_handle: &tauri::AppHandle) {
    if !saved(app_handle).open || !available(app_handle) {
        return;
    }
    let app = app_handle.clone();
    tauri::async_runtime::spawn(async move {
        // After the main window has shown its first frame.
        tokio::time::sleep(Duration::from_millis(1500)).await;
        if let Err(error) = open_window(&app).await {
            log::warn!("The second window did not open again: {error}");
        }
    });
}

// ---------------------------------------------------------------- commands

#[tauri::command]
pub(crate) async fn second_window_state(app_handle: tauri::AppHandle) -> SecondViewState {
    state_of(&app_handle)
}

/// Opens or closes the window (Window ▸ Second Window, ⇧F).
#[tauri::command]
pub(crate) async fn second_window_set_open(
    open: bool,
    app_handle: tauri::AppHandle,
) -> Result<SecondViewState, String> {
    if open {
        open_window(&app_handle).await?;
    } else {
        close(&app_handle).await;
    }
    Ok(state_of(&app_handle))
}

/// Locks the window to the photograph it shows (Lightroom's locked
/// secondary loupe), or follows the main window again.
#[tauri::command]
pub(crate) async fn second_window_set_locked(
    locked: bool,
    app_handle: tauri::AppHandle,
) -> Result<SecondViewState, String> {
    let unlocked = {
        let mut guard = lock_recover(&INNER);
        let Some(inner) = guard.as_mut() else {
            return Ok(state_of(&app_handle));
        };
        let was = inner.locked;
        inner.locked = locked;
        was && !locked
    };
    if unlocked && let Some(context) = app_handle.state::<crate::state::gpu::GpuState>().context() {
        show_current(&app_handle, &context);
    }
    emit_state(&app_handle);
    Ok(state_of(&app_handle))
}

/// The window's zoom from its webview: None to fit, else the scale
/// (device px per full-resolution px; 1 = 100 %) and the photograph point
/// at the window's centre. Enlarged at once; the visible region is then
/// developed at that magnification.
#[tauri::command]
pub(crate) async fn second_window_set_zoom(
    zoom: Option<ZoomPayload>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    let zoom = zoom.and_then(multi_view::cell_zoom);
    let Some(context) = app_handle.state::<crate::state::gpu::GpuState>().context() else {
        return Ok(());
    };
    tauri::async_runtime::spawn_blocking(move || {
        {
            let mut guard = lock_recover(&INNER);
            let Some(inner) = guard.as_mut() else { return };
            if inner.zoom == zoom {
                return;
            }
            inner.zoom = zoom;
            let background = inner.background;
            if let Some(display) = inner.display.as_mut() {
                display.set_multi_view_zoom(zoom);
                paint(display, &context, background);
            }
        }
        update_saved(&app_handle, |s| s.actual_size = zoom.is_some());
        emit_state(&app_handle);
        schedule(&app_handle);
    })
    .await
    .map_err(|e| e.to_string())
}

/// The main window's photograph while it shows no live preview (the
/// Library): developed here from its stored recipe.
#[tauri::command]
pub(crate) async fn second_window_follow(
    path: Option<String>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    let Some(path) = path else { return Ok(()) };
    // Kept while closed or locked: an opening or unlock shows it.
    *lock_recover(&FOLLOW) = Some(path.clone());
    if !is_open() {
        return Ok(());
    }
    let Some(context) = app_handle.state::<crate::state::gpu::GpuState>().context() else {
        return Ok(());
    };
    tauri::async_runtime::spawn_blocking(move || show_followed(&app_handle, &context, path))
        .await
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::startup::MonitorBounds;

    /// The exact payloads `src/components/secondView` and the main
    /// window's menu send (invoke arguments are camelCase).
    #[test]
    fn the_webview_payloads_deserialize() {
        #[derive(Deserialize)]
        struct Open {
            open: bool,
        }
        #[derive(Deserialize)]
        struct Locked {
            locked: bool,
        }
        #[derive(Deserialize)]
        struct Zoom {
            zoom: Option<ZoomPayload>,
        }
        #[derive(Deserialize)]
        struct Follow {
            path: Option<String>,
        }
        let open: Open = serde_json::from_str(r#"{"open":true}"#).unwrap();
        assert!(open.open);
        let locked: Locked = serde_json::from_str(r#"{"locked":false}"#).unwrap();
        assert!(!locked.locked);
        let zoom: Zoom =
            serde_json::from_str(r#"{"zoom":{"scale":1,"center":[0.25,0.75]}}"#).unwrap();
        let zoom = zoom.zoom.and_then(multi_view::cell_zoom).unwrap();
        assert_eq!((zoom.scale, zoom.center), (1.0, [0.25, 0.75]));
        let fit: Zoom = serde_json::from_str(r#"{"zoom":null}"#).unwrap();
        assert!(fit.zoom.is_none());
        let follow: Follow = serde_json::from_str(r#"{"path":"/s/Capture/a.CR3"}"#).unwrap();
        assert_eq!(follow.path.as_deref(), Some("/s/Capture/a.CR3"));
        let none: Follow = serde_json::from_str(r#"{"path":null}"#).unwrap();
        assert!(none.path.is_none());
    }

    #[test]
    fn the_state_the_webview_reads_is_camel_case() {
        let state = SecondViewState {
            available: true,
            open: true,
            locked: true,
            path: Some("a".into()),
            shown: Some([6000.0, 4000.0]),
            zoom: Some(ZoomState {
                scale: 1.0,
                center: [0.5, 0.5],
            }),
        };
        let json = serde_json::to_value(&state).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "available": true, "open": true, "locked": true, "path": "a",
                "shown": [6000.0, 4000.0], "zoom": {"scale": 1.0, "center": [0.5, 0.5]}
            })
        );
    }

    #[test]
    fn the_saved_state_round_trips_and_reads_old_or_partial_files() {
        let state = SavedSecondWindow {
            open: true,
            bounds: Some(SavedBounds {
                x: 3008,
                y: 40,
                width: 2400,
                height: 1500,
            }),
            fullscreen: true,
            actual_size: true,
        };
        let json = serde_json::to_string(&state).unwrap();
        assert_eq!(
            serde_json::from_str::<SavedSecondWindow>(&json).unwrap(),
            state
        );
        let partial: SavedSecondWindow = serde_json::from_str(r#"{"open":true}"#).unwrap();
        assert!(partial.open && partial.bounds.is_none() && !partial.fullscreen);
        assert_eq!(
            serde_json::from_str::<SavedSecondWindow>("{}").unwrap(),
            SavedSecondWindow::default()
        );
    }

    fn monitor(x: i32, width: u32, height: u32) -> MonitorBounds {
        MonitorBounds {
            x,
            y: 0,
            width,
            height,
        }
    }

    #[test]
    fn it_opens_where_it_was_else_on_the_other_display() {
        let main = monitor(0, 3024, 1964);
        let second = monitor(3024, 3840, 2160);
        let kept = SavedBounds {
            x: 3200,
            y: 100,
            width: 2000,
            height: 1200,
        };
        assert_eq!(
            placement(Some(kept), &[main, second], Some(main)),
            Some(kept)
        );
        // The display it was on is gone: centred on the other one, else
        // beside the main window on its own.
        let on_other = placement(None, &[main, second], Some(main)).unwrap();
        assert_eq!(
            on_other,
            SavedBounds {
                x: 3024 + 384,
                y: 216,
                width: 3072,
                height: 1728
            }
        );
        let gone = placement(Some(kept), &[main], Some(main)).unwrap();
        assert_eq!((gone.x, gone.width), (605, 1814));
        assert_eq!(placement(None, &[], None), None);
    }

    #[test]
    fn a_whole_frame_is_developed_again_only_when_too_small() {
        let area = [0.0, 0.0, 3840.0, 2160.0];
        let shown = [6000.0, 4000.0];
        // A fit shows 3240 x 2160: a 2560 px main frame is too small.
        assert!(!whole_suffices((2560, 1707), area, shown));
        assert!(whole_suffices((3200, 2133), area, shown));
        // Never more than the photograph itself.
        assert!(whole_suffices((1200, 800), area, [1200.0, 800.0]));
    }

    #[test]
    fn an_own_frame_is_developed_again_when_the_window_grows() {
        let small = [0.0, 0.0, 1600.0, 900.0];
        let full = [0.0, 0.0, 1920.0, 1200.0];
        let shown = [6000.0, 4000.0];
        let own = Whole {
            recipe_key: 1,
            pixels: (1350, 900),
            own: Some(small),
        };
        assert!(whole_is_enough(own, small, shown));
        assert!(
            !whole_is_enough(own, full, shown),
            "full screen needs 1800 px"
        );
        // A source smaller than the area (a reduced preview) is not asked
        // for again at the area it was developed for.
        let short = Whole {
            pixels: (800, 533),
            ..own
        };
        assert!(whole_is_enough(short, small, shown));
    }

    #[test]
    fn the_recipe_key_follows_the_recipe() {
        let a = serde_json::json!({"exposure": 0.5});
        let b = serde_json::json!({"exposure": 0.6});
        assert_eq!(recipe_key(&a), recipe_key(&a.clone()));
        assert_ne!(recipe_key(&a), recipe_key(&b));
        assert_ne!(region_recipe_tag(recipe_key(&a)), recipe_key(&a));
    }
}
