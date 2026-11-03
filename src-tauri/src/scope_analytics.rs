//! The scopes while a zoomed-in view is dragged. Those frames render only
//! the visible region and carry no analytics, so the histogram, waveform,
//! parade and vectorscope stood still until the release. As in Lightroom and
//! Capture One they keep describing the whole photo: beside each such frame
//! that asks for analytics (at most about ten a second, `useImageProcessing`)
//! a worker renders a small full-frame copy of the frame's input with the
//! frame's recipe and sends its analytics
//! (`gpu_processing::render_scope_analytics`).
//!
//! The drag's clipping marks come from the copy, so a highlight smaller than
//! one copy pixel (about 12 full-resolution pixels at 26 MP) can average
//! below the mark until the release frame sends the exact ones; like the
//! histograms of other editors, which read a reduced preview, the drag's
//! scopes are a sample.
//!
//! Why a separate small render instead of a low-resolution pass inside the
//! frame: the frame's reply does not wait for it (the scope render runs on
//! another thread, taking its turn at the processor between two frames),
//! and the copy is downscaled once per preview base, not per frame.
use std::sync::atomic::Ordering;
use std::sync::{Arc, Weak};

use image::{DynamicImage, GenericImageView};
use tauri::Manager;

use crate::app_state::{AnalyticsConfig, AppState, LoadedImage};
use crate::gpu_processing::{self, LatestJobSender, RenderRequest};
use crate::image_processing::get_or_init_gpu_context;
use crate::lut_processing::Lut;
use crate::mask_generation::MaskDefinition;
use rawtonic_engine::image_processing::downscale_f32_image_signed;

/// Long edge of the scope render's copy of the frame's input: 512 pixels
/// hold about 175 000 of a 3:2 photo, some 680 per waveform column of 256
/// and far more than the 256 levels of a histogram need. The release frame
/// sends the full-detail scopes. Measured on a Radeon 680M (26 MP RAF), a
/// scope render of a 1024-pixel copy held the processor for about 12 ms
/// besides its colour noise preparation, and the drag's frames waited for it.
pub(crate) const SCOPE_PROXY_EDGE: u32 = 512;

/// A zoomed frame's request for the whole frame's analytics.
pub(crate) struct ScopeAnalyticsJob {
    /// The frame's full-frame input (the preview base it rendered a region
    /// of) and its pixels per full-resolution pixel.
    pub source: Arc<DynamicImage>,
    pub scale: f32,
    pub unscaled_crop_offset: (f32, f32),
    /// The frame's hydrated recipe and its layers' masks, drawn again at
    /// the copy's size.
    pub recipe: serde_json::Value,
    pub masks: Vec<MaskDefinition>,
    pub adjustments: rawtonic_engine::image_processing::PreparedAdjustments,
    pub lut: Option<Arc<Lut>>,
    /// The photograph the masks are drawn from: image, proxy pixels per
    /// full-resolution pixel, full size.
    pub photo: (LoadedImage, f32, (u32, u32)),
    /// Read in the native display's space (the display frames' space).
    pub display_space: bool,
    pub analytics: AnalyticsConfig,
}

/// The size of the scope render's copy of a `width` x `height` input: its
/// long edge at most [`SCOPE_PROXY_EDGE`], the aspect kept, never larger
/// than the input.
pub(crate) fn proxy_size(width: u32, height: u32) -> (u32, u32) {
    let long = width.max(height);
    if long <= SCOPE_PROXY_EDGE {
        return (width, height);
    }
    let fit = |side: u32| {
        ((side as u64 * SCOPE_PROXY_EDGE as u64 + long as u64 / 2) / long as u64).max(1) as u32
    };
    (fit(width), fit(height))
}

/// The analytics jobs the scopes show, in the order the frames asked for
/// them: a job of an older request arriving after a newer one's (a zoomed
/// drag's scope render finishing after the release frame) is dropped.
#[derive(Default)]
pub(crate) struct AnalyticsOrder {
    newest: u64,
}

impl AnalyticsOrder {
    /// Whether the job of `generation` is shown.
    pub(crate) fn admits(&mut self, generation: u64) -> bool {
        if generation < self.newest {
            return false;
        }
        self.newest = generation;
        true
    }
}

/// The next generation of analytics requests (`AnalyticsConfig::generation`).
pub(crate) fn next_generation(state: &AppState) -> u64 {
    state.analytics_generation.fetch_add(1, Ordering::SeqCst) + 1
}

pub(crate) fn start_scope_analytics_worker(app_handle: tauri::AppHandle) {
    let worker_app = app_handle.clone();
    let sender = gpu_processing::start_latest_job_worker(move |job: ScopeAnalyticsJob| {
        if let Err(error) = render(&worker_app, job) {
            log::warn!("Scope analytics of the whole frame failed: {error}");
        }
    });
    *app_handle
        .state::<AppState>()
        .scope_analytics_tx
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = Some(sender);
}

/// The worker's sender, None before it started.
pub(crate) fn sender(state: &AppState) -> Option<LatestJobSender<ScopeAnalyticsJob>> {
    state
        .scope_analytics_tx
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

fn render(app_handle: &tauri::AppHandle, job: ScopeAnalyticsJob) -> Result<(), String> {
    let state = app_handle.state::<AppState>();
    // A frame that carried its own analytics came since (the release).
    if state.full_frame_analytics_generation.load(Ordering::SeqCst) > job.analytics.generation {
        return Ok(());
    }
    #[cfg(feature = "performance-profiling")]
    let _stage = crate::performance::PerformanceStage::new("scope_analytics_render");
    let context = get_or_init_gpu_context(&state, app_handle)?;
    let proxy = proxy_of(&state, &job.source);
    let (width, height) = proxy.image.dimensions();
    let scale = job.scale * width as f32 / job.source.width().max(1) as f32;
    let (loaded_image, source_scale, full_dims) = &job.photo;
    let mask_bitmaps = crate::editor_preview::preview_mask_bitmaps(
        &state,
        &job.recipe,
        &job.masks,
        (loaded_image, *source_scale, *full_dims),
        (width, height),
        scale,
        job.unscaled_crop_offset,
    )?;
    let mut adjustments = job.adjustments;
    adjustments.global.detail_scale = scale;
    gpu_processing::render_scope_analytics(
        &context,
        &state.gpu_processor,
        &state.scope_gpu_cache,
        (&proxy.image, proxy.id),
        RenderRequest {
            adjustments,
            mask_bitmaps: &mask_bitmaps,
            lut: job.lut,
            roi: None,
        },
        job.analytics,
        job.display_space,
    )
}

/// The copy of a preview base the scope render reads, and the id its GPU
/// upload is kept under (`AppState::scope_gpu_cache`).
pub(crate) struct ScopeProxy {
    of: Weak<DynamicImage>,
    image: Arc<DynamicImage>,
    id: u64,
}

/// The copy of `source` the scope render reads, made once per preview base.
fn proxy_of(state: &AppState, source: &Arc<DynamicImage>) -> ScopeProxy {
    let mut kept = state.scope_proxy.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(proxy) = kept.as_ref()
        && proxy.of.ptr_eq(&Arc::downgrade(source))
    {
        return proxy.clone();
    }
    let (width, height) = source.dimensions();
    let (w, h) = proxy_size(width, height);
    let image = if (w, h) == (width, height) {
        Arc::clone(source)
    } else {
        Arc::new(downscale_f32_image_signed(source, w, h))
    };
    let proxy = ScopeProxy {
        of: Arc::downgrade(source),
        image,
        id: kept
            .as_ref()
            .map_or(1, |previous| previous.id.wrapping_add(1)),
    };
    *kept = Some(proxy.clone());
    proxy
}

impl Clone for ScopeProxy {
    fn clone(&self) -> Self {
        Self {
            of: Weak::clone(&self.of),
            image: Arc::clone(&self.image),
            id: self.id,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_copy_keeps_the_aspect_within_the_long_edge() {
        assert_eq!(proxy_size(6240, 4160), (512, 341));
        assert_eq!(proxy_size(4160, 6240), (341, 512));
        assert_eq!(proxy_size(8000, 10), (512, 1));
        // A small input is read as it is.
        assert_eq!(proxy_size(512, 256), (512, 256));
        assert_eq!(proxy_size(300, 200), (300, 200));
    }

    #[test]
    fn an_older_request_arriving_after_a_newer_one_is_dropped() {
        let mut order = AnalyticsOrder::default();
        assert!(order.admits(1));
        assert!(order.admits(3));
        // The zoomed drag's scope render of request 2 after the release
        // frame's analytics (3).
        assert!(!order.admits(2));
        assert!(order.admits(3));
        assert!(order.admits(4));
    }
}
