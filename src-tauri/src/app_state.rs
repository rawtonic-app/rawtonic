use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use image::DynamicImage;
use serde::{Deserialize, Serialize};
use sysinfo::Disks;
use tokio::sync::Mutex as TokioMutex;
use tokio::sync::Notify;
use tokio::task::JoinHandle;

use crate::ai_processing::AiState;
use crate::cache_utils::DecodedImageCache;
pub use crate::gpu_processing::{AnalyticsConfig, AnalyticsJob, GpuImageCache, GpuProcessorState};
use crate::gpu_processing::{GpuContext, LatestJobSender};
use crate::launch_request::ExternalEditSession;
use crate::lens_correction::LensDatabase;
use crate::lut_processing::LutCache;
use crate::mask_generation::{FullWarpedMaskCache, MaskBitmapCache};

pub struct AiTaskToken {
    cancelled: AtomicBool,
    notify: Notify,
}

impl AiTaskToken {
    pub fn new() -> Self {
        Self {
            cancelled: AtomicBool::new(false),
            notify: Notify::new(),
        }
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    pub async fn wait_for_cancel(&self) {
        let notified = self.notify.notified();
        if self.is_cancelled() {
            return;
        }
        notified.await;
    }
}

impl Default for AiTaskToken {
    fn default() -> Self {
        Self::new()
    }
}

pub struct AiTaskGuard<'a> {
    active_ai_tasks: &'a Mutex<HashMap<String, Arc<AiTaskToken>>>,
    task_id: String,
    pub token: Arc<AiTaskToken>,
}

impl<'a> AiTaskGuard<'a> {
    pub fn new(
        active_ai_tasks: &'a Mutex<HashMap<String, Arc<AiTaskToken>>>,
        task_id: String,
    ) -> Self {
        let token = Arc::new(AiTaskToken::new());
        active_ai_tasks
            .lock()
            .unwrap()
            .insert(task_id.clone(), Arc::clone(&token));
        Self {
            active_ai_tasks,
            task_id,
            token,
        }
    }
}

impl<'a> Drop for AiTaskGuard<'a> {
    fn drop(&mut self) {
        let mut tasks = self.active_ai_tasks.lock().unwrap();
        if let Some(existing) = tasks.get(&self.task_id)
            && Arc::ptr_eq(existing, &self.token)
        {
            tasks.remove(&self.task_id);
        }
    }
}

#[derive(Serialize, Deserialize)]
pub struct WindowState {
    pub width: u32,
    pub height: u32,
    pub x: i32,
    pub y: i32,
    pub maximized: bool,
    /// Scale when these physical pixel dimensions were saved. Older files
    /// omitted it; Linux uses its configured size once to migrate those.
    #[serde(default)]
    pub scale_factor: Option<f64>,
    /// Always written false and not restored (full screen is the photo-only
    /// view); kept so older files still read.
    #[serde(default)]
    pub fullscreen: bool,
}

#[derive(Clone)]
pub struct LoadedImage {
    pub path: String,
    pub image: Arc<DynamicImage>,
    pub is_raw: bool,
    /// The decode variant it was developed for (colour noise filter and
    /// dead-photosite test, `image_loader::load_base_image_from_bytes_for`);
    /// None for preview data.
    pub decode: Option<crate::image_loader::DecodeVariant>,
    /// The measures of the decode (`image_loader::DecodeMeasures`); None for
    /// preview data, drafts and other formats.
    pub measures: Option<crate::image_loader::DecodeMeasures>,
}

impl LoadedImage {
    /// Which development of the photo `image` is: the same while it is
    /// these pixels (also back from the decoded-image cache), another after
    /// the photo is developed again (AI Denoise, decode options, a 1:1
    /// preview). Caches of results computed from the pixels (AI masks,
    /// depth, faces) include it in their keys.
    pub fn source_generation(&self) -> u64 {
        source_generation(&self.image)
    }
}

/// Generations of the developments alive. An entry's `Weak` keeps the
/// allocation, so its address is not reused while it is listed; entries
/// whose image is gone are dropped, and generations never repeat.
static SOURCE_GENERATIONS: Mutex<(u64, Vec<LiveSource>)> = Mutex::new((0, Vec::new()));

/// A development alive and its generation.
type LiveSource = (std::sync::Weak<DynamicImage>, u64);

fn source_generation(image: &Arc<DynamicImage>) -> u64 {
    let mut generations = SOURCE_GENERATIONS.lock().unwrap_or_else(|e| e.into_inner());
    let (last, live) = &mut *generations;
    live.retain(|(source, _)| source.strong_count() > 0);
    if let Some((_, generation)) = live
        .iter()
        .find(|(source, _)| std::ptr::eq(source.as_ptr(), Arc::as_ptr(image)))
    {
        return *generation;
    }
    *last += 1;
    live.push((Arc::downgrade(image), *last));
    *last
}

/// The open photograph's preview proxy (preview_proxy.rs) while its full
/// decode runs: `image` at `scale` proxy pixels per full-resolution pixel.
#[derive(Clone)]
pub struct LoadedProxy {
    pub path: String,
    pub image: Arc<DynamicImage>,
    pub full_width: u32,
    pub full_height: u32,
    pub is_raw: bool,
    /// The decode variant a stored preview proxy was made for (from its
    /// key); None for a draft of the decode under way.
    pub made_for: Option<crate::image_loader::DecodeVariant>,
    /// The measures of the decode a stored proxy was made from (its facts);
    /// None for drafts and proxies made before they were kept.
    pub measures: Option<crate::image_loader::DecodeMeasures>,
}

/// The source the preview worker last rendered a frame from (a decode, its
/// draft or a stored proxy), so that work can wait until the photograph is
/// on screen: the work a decode leaves for later (its preview proxy and
/// thumbnail) and the full development after its draft. Started beside that
/// frame, it took the cores the frame needed.
#[derive(Default)]
pub struct RenderedSource {
    source: Mutex<std::sync::Weak<DynamicImage>>,
    rendered: Condvar,
}

impl RenderedSource {
    /// Call when a frame from `source` is rendered.
    pub fn mark(&self, source: &Arc<DynamicImage>) {
        *self.source.lock().unwrap_or_else(|e| e.into_inner()) = Arc::downgrade(source);
        self.rendered.notify_all();
    }

    /// Waits until a frame from `source` is rendered or `timeout` passed;
    /// whether it was rendered. Holds no strong reference to the source.
    pub fn wait(
        &self,
        source: &std::sync::Weak<DynamicImage>,
        timeout: std::time::Duration,
    ) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        let mut last = self.source.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if last.ptr_eq(source) {
                return true;
            }
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                return false;
            }
            last = self
                .rendered
                .wait_timeout(last, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }
}

/// Wakes the commands waiting for the open photograph's full decode
/// (`image_loader::wait_for_original_image`) when its source slots change:
/// a blocking worker waits on the condition variable, an async command on
/// the notification, so neither polls.
#[derive(Default)]
pub struct SourceSignal {
    changes: Mutex<u64>,
    changed: Condvar,
    notify: Notify,
}

impl SourceSignal {
    /// Call after a write that can settle a waiting command (a decode in
    /// `original_image`, `offline_source` set, `proxy_image` cleared), with
    /// their locks released.
    pub fn changed(&self) {
        *self.changes.lock().unwrap_or_else(|e| e.into_inner()) += 1;
        self.changed.notify_all();
        self.notify.notify_waiters();
    }

    /// `check`'s first `Some`, checked now and after each change until
    /// `timeout`; then `None`. Blocks the calling thread.
    pub fn wait_for<T>(
        &self,
        timeout: std::time::Duration,
        mut check: impl FnMut() -> Option<T>,
    ) -> Option<T> {
        let deadline = std::time::Instant::now() + timeout;
        // Held while checking: a change cannot slip between the check and
        // the wait.
        let mut changes = self.changes.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if let Some(value) = check() {
                return Some(value);
            }
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                return None;
            }
            changes = self
                .changed
                .wait_timeout(changes, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }

    /// `wait_for` without blocking the async runtime's thread.
    pub async fn wait_for_async<T>(
        &self,
        timeout: std::time::Duration,
        mut check: impl FnMut() -> Option<T>,
    ) -> Option<T> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            // Registered before the check, so a change after it wakes us.
            notified.as_mut().enable();
            if let Some(value) = check() {
                return Some(value);
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                return check();
            }
        }
    }
}

#[derive(Clone)]
pub struct CachedPreview {
    pub source: std::sync::Weak<DynamicImage>,
    pub image: Arc<DynamicImage>,
    pub small_image: Option<Arc<DynamicImage>>,
    pub transform_hash: u64,
    pub scale: f32,
    pub unscaled_crop_offset: (f32, f32),
    pub preview_dim: u32,
    pub interactive_divisor: f32,
    /// Built from a reduced source shown only until the decode is in
    /// (`editor_preview::interim_source`).
    pub interim: bool,
    /// Built while a control was dragged, without the edge-fringe pass's
    /// chromatic-aberration stage (`edge_fringe::DRAFT_KEY`).
    pub fringe_draft: bool,
}

impl CachedPreview {
    /// Whether this base serves a frame of `source` with `transform_hash`.
    /// An interim base never serves a lasting source: the same Standard
    /// proxy may turn out to be an offline photograph's only source. A
    /// fringe draft serves only frames while a control is dragged.
    pub fn serves(
        &self,
        source: &Arc<DynamicImage>,
        transform_hash: u64,
        interim: bool,
        interactive: bool,
    ) -> bool {
        self.source.ptr_eq(&Arc::downgrade(source))
            && self.transform_hash == transform_hash
            && self.interim == interim
            && (interactive || !self.fringe_draft)
    }
}

/// The mask layer the native display draws over the preview while it is
/// edited (the webview's Show mask state), sent with each preview request.
#[derive(Clone, Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MaskOverlayRequest {
    pub mask_id: String,
    pub color: [u8; 3],
    /// Percent.
    pub opacity: u8,
    pub grayscale: bool,
}

pub struct PreviewJob {
    pub generation: usize,
    /// Submission order from the webview, which keeps up to four previews in
    /// flight. Two invokes can reach the worker out of order; a sequenced job
    /// older than one already taken is dropped. `None` keeps arrival order.
    pub sequence: Option<u64>,
    pub adjustments: serde_json::Value,
    pub is_interactive: bool,
    pub target_resolution: Option<u32>,
    pub roi: Option<(f32, f32, f32, f32)>,
    pub request_analytics: bool,
    pub compute_waveform: bool,
    pub active_waveform_channel: Option<String>,
    pub mask_overlay: Option<MaskOverlayRequest>,
    /// The frame is the uncropped, unrotated frame shown while cropping; the
    /// display then places it by the crop geometry.
    pub crop_edit: bool,
    /// A dropped responder means the job was superseded; `Err` is a real failure.
    pub responder: tokio::sync::oneshot::Sender<Result<Vec<u8>, String>>,
}

impl crate::preview_slot::Supersedes for PreviewJob {
    /// Whether `self`, received after `current`, should replace it.
    fn supersedes(&self, current: &PreviewJob) -> bool {
        supersedes(self.sequence, current.sequence)
    }
}

pub(crate) fn supersedes(candidate: Option<u64>, current: Option<u64>) -> bool {
    match (candidate, current) {
        (Some(candidate), Some(current)) => candidate > current,
        _ => true,
    }
}

/// A sequenced job older than the newest one already processed.
pub fn preview_job_is_stale(sequence: Option<u64>, newest_processed: Option<u64>) -> bool {
    matches!((sequence, newest_processed), (Some(sequence), Some(newest)) if sequence < newest)
}

pub struct ThumbnailProgressTracker {
    pub total: usize,
    pub completed: usize,
}

pub struct ThumbnailManager {
    pub queue: Mutex<VecDeque<String>>,
    pub cvar: Condvar,
    pub processing_now: Mutex<HashSet<String>>,
    pub rotational_disk: AtomicBool,
    pub io_gate: Mutex<()>,
}

impl ThumbnailManager {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            queue: Mutex::new(VecDeque::new()),
            cvar: Condvar::new(),
            processing_now: Mutex::new(HashSet::new()),
            rotational_disk: AtomicBool::new(false),
            io_gate: Mutex::new(()),
        })
    }
}

pub struct PendingMetadata {
    pub virtual_path: String,
    pub image_path: PathBuf,
    pub sidecar_path: PathBuf,
}

pub struct MetadataManager {
    pub queue: Mutex<VecDeque<PendingMetadata>>,
    pub cvar: Condvar,
    pub pending: Mutex<HashSet<PathBuf>>,
}

impl MetadataManager {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            queue: Mutex::new(VecDeque::new()),
            cvar: Condvar::new(),
            pending: Mutex::new(HashSet::new()),
        })
    }
}

pub type ThumbnailGeometryEntry = (
    u64,
    Arc<DynamicImage>,
    f32,
    Option<crate::mask_generation::ParametricMaskSource>,
);
pub type TransformedImageCache = (u64, Arc<DynamicImage>, (f32, f32));

pub struct AppState {
    pub window_setup_complete: AtomicBool,
    pub gpu_crash_flag_path: Mutex<Option<PathBuf>>,
    pub original_image: Mutex<Option<LoadedImage>>,
    pub proxy_image: Mutex<Option<LoadedProxy>>,
    pub proxy_memory: Mutex<crate::preview_proxy::ProxyMemory>,
    /// The open photograph when its RAW is offline and it opened on a preview
    /// proxy: its 1:1 proxy is `original_image` (preview data, never reused
    /// by exports), or its Standard proxy stays `proxy_image` and no full
    /// decode will come.
    pub offline_source: Mutex<Option<String>>,
    /// Signalled when `original_image`, `proxy_image` or `offline_source`
    /// changes.
    pub source_signal: SourceSignal,
    pub rendered_source: RenderedSource,
    /// The photograph the editor asked for last (load_image / load_image_proxy).
    pub current_source: Mutex<String>,
    /// Long edge of the viewer's last full preview, for the proxy size.
    pub last_preview_dim: std::sync::atomic::AtomicU32,
    pub cached_preview: Mutex<Option<CachedPreview>>,
    pub(crate) reusable_previews: Mutex<crate::preview_cache::PreviewBaseCache>,
    pub gpu_context: Mutex<Option<GpuContext>>,
    pub gpu_image_cache: Mutex<Option<GpuImageCache>>,
    pub gpu_processor: Mutex<Option<GpuProcessorState>>,
    pub ai_state: Mutex<Option<AiState>>,
    pub ai_init_lock: TokioMutex<()>,
    /// AI Denoise (ai_denoise.rs) by `aiDenoiseVersion` (index 0: version
    /// 1), each loaded on first use.
    pub ai_denoiser:
        Mutex<[Option<Arc<dyn rawtonic_rawdecode::mosaic_denoise::MosaicDenoiser>>; 2]>,
    /// Face models and the open photo's faces (ai_faces.rs), loaded on first use.
    pub faces: crate::ai_faces::SharedFaceState,
    pub active_ai_tasks: Mutex<HashMap<String, Arc<AiTaskToken>>>,
    pub export_task_token: Arc<Mutex<Option<Arc<AtomicBool>>>>,
    /// A finished merge waiting for `save_merge` (merges.rs).
    pub merge_result: Arc<Mutex<Option<crate::merges::MergeOutcome>>>,
    pub indexing_task_handle: Mutex<Option<JoinHandle<()>>>,
    pub lut_cache: Mutex<LutCache>,
    pub initial_file_path: Mutex<Option<String>>,
    pub pending_edit_session: Mutex<Option<ExternalEditSession>>,
    pub thumbnail_cancellation_token: Arc<AtomicBool>,
    pub thumbnail_progress: Mutex<ThumbnailProgressTracker>,
    /// The preview worker's next job (preview_slot.rs): a newer request
    /// replaces the waiting one on arrival.
    pub preview_jobs: crate::preview_slot::LatestSlot<PreviewJob>,
    /// The recipe of the brush stroke being drawn (live_stroke.rs).
    pub live_stroke: Mutex<crate::live_stroke::LiveStrokeRecipe>,
    pub analytics_worker_tx: Mutex<Option<LatestJobSender<AnalyticsJob>>>,
    pub mask_cache: Mutex<MaskBitmapCache>,
    /// Masks drawn for the layer list's thumbnails, apart from the
    /// preview's so they never evict its full-size masks.
    pub mask_thumbnail_cache: Mutex<MaskBitmapCache>,
    pub patch_cache: Mutex<HashMap<String, serde_json::Value>>,
    pub geometry_cache: Mutex<HashMap<u64, DynamicImage>>,
    pub thumbnail_geometry_cache: Mutex<HashMap<String, ThumbnailGeometryEntry>>,
    pub lens_db: Mutex<Option<Arc<LensDatabase>>>,
    pub load_image_generation: Arc<AtomicUsize>,
    pub preview_generation: AtomicUsize,
    pub full_warped_cache: Mutex<Option<FullWarpedMaskCache>>,
    /// The editor photo's `full_warped_cache` builds in progress, so a stroke
    /// arriving during a prewarm of the same warp waits for it instead of
    /// building twice.
    pub full_warped_build: crate::editor_preview::BuildSlot<crate::editor_preview::WarpBuildKey>,
    /// The photo whose full-resolution mask source the editor wants built
    /// ahead of the first Auto Mask stroke (`prewarm_mask_source`).
    pub mask_source_wanted: Mutex<Option<String>>,
    /// The last final preview's photo and hydrated recipe, which the mask
    /// source prewarm builds from.
    pub mask_source_recipe: Mutex<Option<(String, Arc<serde_json::Value>)>>,
    pub patched_warped_cache: Mutex<Option<(u64, Arc<DynamicImage>)>>,
    pub full_transformed_cache: Mutex<Option<TransformedImageCache>>,
    pub decoded_image_cache: Mutex<DecodedImageCache>,
    pub thumbnail_manager: Arc<ThumbnailManager>,
    pub metadata_manager: Arc<MetadataManager>,
    pub disks_cache: Mutex<Option<Disks>>,
    pub disks_cache_refreshing: AtomicBool,
}

#[cfg(test)]
mod preview_job_order_tests {
    use super::{preview_job_is_stale, supersedes};

    #[test]
    fn late_older_requests_never_replace_or_follow_newer_ones() {
        assert!(supersedes(Some(8), Some(7)));
        assert!(!supersedes(Some(7), Some(8)));
        assert!(supersedes(None, Some(8)));
        assert!(supersedes(Some(1), None));
        assert!(preview_job_is_stale(Some(7), Some(8)));
        assert!(!preview_job_is_stale(Some(8), Some(8)));
        assert!(!preview_job_is_stale(None, Some(8)));
        assert!(!preview_job_is_stale(Some(1), None));
    }
}

#[cfg(test)]
mod cached_preview_tests {
    use super::CachedPreview;
    use image::DynamicImage;
    use std::sync::Arc;

    #[test]
    fn an_interim_base_does_not_serve_the_same_source_once_it_lasts() {
        // A Standard proxy's base built while it stood in for a decode, then
        // the photograph turned out offline: the same proxy is its source.
        let proxy = Arc::new(DynamicImage::new_rgb32f(4, 4));
        let cached = CachedPreview {
            source: Arc::downgrade(&proxy),
            image: Arc::new(DynamicImage::new_rgb32f(2, 2)),
            small_image: None,
            transform_hash: 7,
            scale: 0.5,
            unscaled_crop_offset: (0.0, 0.0),
            preview_dim: 2,
            interactive_divisor: 1.0,
            interim: true,
            fringe_draft: false,
        };
        assert!(cached.serves(&proxy, 7, true, false));
        assert!(!cached.serves(&proxy, 7, false, false));
        assert!(!cached.serves(&proxy, 8, true, false));
        assert!(!cached.serves(&Arc::new(DynamicImage::new_rgb32f(4, 4)), 7, true, false));
    }

    #[test]
    fn a_fringe_draft_serves_only_frames_of_a_drag() {
        let source = Arc::new(DynamicImage::new_rgb32f(4, 4));
        let cached = CachedPreview {
            source: Arc::downgrade(&source),
            image: Arc::new(DynamicImage::new_rgb32f(2, 2)),
            small_image: None,
            transform_hash: 7,
            scale: 0.5,
            unscaled_crop_offset: (0.0, 0.0),
            preview_dim: 2,
            interactive_divisor: 1.0,
            interim: false,
            fringe_draft: true,
        };
        assert!(cached.serves(&source, 7, false, true));
        assert!(!cached.serves(&source, 7, false, false));
        let finished = CachedPreview {
            fringe_draft: false,
            ..cached
        };
        assert!(finished.serves(&source, 7, false, true));
        assert!(finished.serves(&source, 7, false, false));
    }
}

#[cfg(test)]
mod rendered_source_tests {
    use super::RenderedSource;
    use image::DynamicImage;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    #[test]
    fn waits_for_a_frame_of_that_decode() {
        let rendered = Arc::new(RenderedSource::default());
        let decode = Arc::new(DynamicImage::new_rgb32f(4, 4));
        let other = Arc::new(DynamicImage::new_rgb32f(4, 4));
        // No frame of it: the wait ends at its timeout.
        rendered.mark(&other);
        let start = Instant::now();
        assert!(!rendered.wait(&Arc::downgrade(&decode), Duration::from_millis(50)));
        assert!(start.elapsed() >= Duration::from_millis(50));
        // A frame rendered meanwhile ends the wait at once.
        let marker = {
            let (rendered, decode) = (Arc::clone(&rendered), Arc::clone(&decode));
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(20));
                rendered.mark(&decode);
            })
        };
        let start = Instant::now();
        assert!(rendered.wait(&Arc::downgrade(&decode), Duration::from_secs(10)));
        assert!(start.elapsed() < Duration::from_secs(5));
        marker.join().unwrap();
        // And one rendered before the wait.
        assert!(rendered.wait(&Arc::downgrade(&decode), Duration::ZERO));
    }
}

#[cfg(test)]
mod source_signal_tests {
    use super::SourceSignal;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    #[test]
    fn a_blocking_waiter_wakes_on_the_change_and_gives_up_at_the_deadline() {
        let signal = Arc::new(SourceSignal::default());
        let ready = Arc::new(AtomicBool::new(false));
        let writer = {
            let (signal, ready) = (signal.clone(), ready.clone());
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(30));
                ready.store(true, Ordering::SeqCst);
                signal.changed();
            })
        };
        let started = Instant::now();
        let woke = signal.wait_for(Duration::from_secs(20), || {
            ready.load(Ordering::SeqCst).then_some(())
        });
        writer.join().unwrap();
        assert!(woke.is_some());
        assert!(started.elapsed() < Duration::from_secs(10));

        let started = Instant::now();
        assert!(
            signal
                .wait_for(Duration::from_millis(40), || None::<()>)
                .is_none()
        );
        assert!(started.elapsed() >= Duration::from_millis(40));
    }

    /// The async wait yields its thread: on a single-threaded runtime the
    /// task that publishes the source runs while the command waits (a sleep
    /// loop there would hold the thread until its deadline).
    #[test]
    fn an_async_waiter_lets_the_runtime_publish_the_source() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        let signal = Arc::new(SourceSignal::default());
        let ready = Arc::new(AtomicBool::new(false));
        let started = Instant::now();
        let woke = runtime.block_on(async {
            let publisher = {
                let (signal, ready) = (signal.clone(), ready.clone());
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(30)).await;
                    ready.store(true, Ordering::SeqCst);
                    signal.changed();
                })
            };
            let woke = signal
                .wait_for_async(Duration::from_secs(20), || {
                    ready.load(Ordering::SeqCst).then_some(())
                })
                .await;
            publisher.await.unwrap();
            woke
        });
        assert!(woke.is_some());
        assert!(started.elapsed() < Duration::from_secs(10));
        assert!(
            runtime
                .block_on(signal.wait_for_async(Duration::from_millis(40), || None::<()>))
                .is_none()
        );
    }
}
