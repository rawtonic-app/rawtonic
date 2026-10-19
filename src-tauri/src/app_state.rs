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
use crate::lut_processing::Lut;
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
    /// The decode was developed for the multi-scale colour noise filter
    /// (without the decode-time chroma clean-up,
    /// `image_loader::load_base_image_from_bytes_for`); None for preview data.
    pub multiscale: Option<bool>,
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
    pub lut_cache: Mutex<HashMap<String, Arc<Lut>>>,
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
