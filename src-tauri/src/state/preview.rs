//! The editor's preview (plan step 3.3): the preview worker's next job and
//! its generations, the size of the last full preview, the brush stroke being
//! drawn, and the scopes' workers and their generations. Written by the
//! preview commands (`editor_preview`) and `scope_analytics`. Locks:
//! `README.md` here.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize};

use crate::app_state::MaskOverlayRequest;
use crate::gpu_processing::{AnalyticsJob, LatestJobSender};

/// The preview's slots and counters. Part of `AppState` (`AppState.preview`).
#[derive(Default)]
pub struct PreviewState {
    /// Long edge of the viewer's last full preview, for the proxy size.
    pub last_preview_dim: AtomicU32,
    /// The preview worker's next job (preview_slot.rs): a newer request
    /// replaces the waiting one on arrival.
    pub preview_jobs: crate::preview_slot::LatestSlot<PreviewJob>,
    pub preview_generation: AtomicUsize,
    /// The recipe of the brush stroke being drawn (live_stroke.rs).
    pub live_stroke: Mutex<crate::live_stroke::LiveStrokeRecipe>,
    pub analytics_worker_tx: Mutex<Option<LatestJobSender<AnalyticsJob>>>,
    /// The last generation of analytics requests
    /// (`scope_analytics::next_generation`), and the last one a frame
    /// carried itself (not a zoomed drag's scope render).
    pub analytics_generation: AtomicU64,
    pub full_frame_analytics_generation: AtomicU64,
    /// The zoomed drag's scope renders of the whole frame (scope_analytics.rs),
    /// and the small copy of the preview base they read (its GPU upload is
    /// `GpuState::scope_cache`).
    pub scope_analytics_tx:
        Mutex<Option<LatestJobSender<crate::scope_analytics::ScopeAnalyticsJob>>>,
    pub scope_proxy: Mutex<Option<crate::scope_analytics::ScopeProxy>>,
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
    pub responder: tokio::sync::oneshot::Sender<Result<Vec<u8>, crate::ipc::CommandError>>,
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
