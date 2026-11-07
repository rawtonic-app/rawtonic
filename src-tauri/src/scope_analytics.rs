//! The scopes while a zoomed-in view is dragged. Those frames render only
//! the visible region and carry no analytics, so the histogram, waveform,
//! parade and vectorscope stood still until the release. As in Lightroom and
//! Capture One they keep describing the whole photo: beside each such frame
//! that asks for analytics (at most about ten a second, `useImageProcessing`)
//! a worker renders a small full-frame copy of the frame's input with the
//! frame's recipe and sends its analytics
//! (`gpu_processing::render_scope_analytics`).
//!
//! The copy is an average (a pixel of it stands for a block of about 12 x 12
//! pixels of a 26 MP frame), so a clipped highlight or shadow smaller than a
//! block would average below the clipping marks until the release. The
//! drag's marks therefore also come from two copies of the release frame's
//! input (`block_extreme_pixels`, long edge [`SCOPE_EXTREMES_EDGE`]): per
//! block the pixel whose brightest channel is highest and the one whose
//! darkest channel is lowest, real pixels, rendered with the frame's recipe
//! and the frame's layer masks read where each pixel came from
//! (`render_scope_clip_marks`), before the averaged copy, whose analytics
//! then carry their marks too. The histogram and the scopes count the
//! averaged copy alone; like the histograms of other editors, which read a
//! reduced preview, they are a sample.
//!
//! The marks are the release frame's for a clipped pixel that is its
//! block's brightest (or darkest) one. Not covered, until the release: a
//! second pixel of the same block clipping in another channel (the
//! triangle's colour), a colour step that makes another pixel of a block
//! clip first (a white balance or a colour-range adjustment), and the
//! effects that read a pixel's neighbours or position other than masks
//! (sharpening, clarity, vignette, grain: they see the block's place and
//! its neighbouring blocks' pixels). A should_fix limit, kept for the drag's
//! cost: exact channels need up to six such renders per update, and two at
//! 512 pixels already made the drag slower
//! (notes: parity-gaps/G23-analysis.md, G23b).
//!
//! Why a separate small render instead of a low-resolution pass inside the
//! frame: the frame's reply does not wait for it (the scope render runs on
//! another thread, taking its turn at the processor between two frames),
//! and the copy is downscaled once per preview base, not per frame.
use std::sync::atomic::Ordering;
use std::sync::{Arc, Weak};

use image::{DynamicImage, GenericImageView};
use tauri::Manager;

use crate::app_state::{AnalyticsConfig, AppState};
use crate::gpu_processing::{self, LatestJobSender, RenderRequest};
use crate::image_processing::get_or_init_gpu_context;
use crate::lut_processing::Lut;
use crate::mask_generation::MaskDefinition;
use crate::state::develop::DevelopSession;
use crate::state::preview::PreviewState;
use crate::state::source::LoadedImage;
use rawtonic_engine::image_processing::{
    BlockExtremes, block_extreme_pixels, downscale_f32_image_signed,
};

/// Long edge of the scope render's copy of the frame's input: 512 pixels
/// hold about 175 000 of a 3:2 photo, some 680 per waveform column of 256
/// and far more than the 256 levels of a histogram need. The release frame
/// sends the full-detail scopes. Measured on a Radeon 680M (26 MP RAF), a
/// scope render of a 1024-pixel copy held the processor for about 12 ms
/// besides its colour noise preparation, and the drag's frames waited for it.
pub(crate) const SCOPE_PROXY_EDGE: u32 = 512;

/// Long edge of the block-extreme copies, which only find the clipping: a
/// one-pixel highlight stays in its block's copy at any block size, and a
/// copy of 256 pixels renders in about 1.5 ms against 4 ms at 512 (RTX 3080,
/// with sharpening and colour noise reduction): at 512 the drag's
/// input-to-preview p95 rose 1.3 ms
/// (notes: benchmark-results/g23b-zoom-clip-extrema-2026-11-07/REPORT.md).
pub(crate) const SCOPE_EXTREMES_EDGE: u32 = 256;

/// A zoomed frame's request for the whole frame's analytics.
pub(crate) struct ScopeAnalyticsJob {
    /// The release frame's full-frame input (the preview base the drag's
    /// frame rendered a region of, or of a reduced copy of) and its pixels
    /// per full-resolution pixel.
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
    size_within(width, height, SCOPE_PROXY_EDGE)
}

/// `width` x `height` with its long edge at most `edge`, the aspect kept.
fn size_within(width: u32, height: u32, edge: u32) -> (u32, u32) {
    let long = width.max(height);
    if long <= edge {
        return (width, height);
    }
    let fit =
        |side: u32| ((side as u64 * edge as u64 + long as u64 / 2) / long as u64).max(1) as u32;
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
pub(crate) fn next_generation(preview: &PreviewState) -> u64 {
    preview.analytics_generation.fetch_add(1, Ordering::SeqCst) + 1
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
        .preview
        .scope_analytics_tx
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = Some(sender);
}

/// The worker's sender, None before it started.
pub(crate) fn sender(preview: &PreviewState) -> Option<LatestJobSender<ScopeAnalyticsJob>> {
    preview
        .scope_analytics_tx
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

fn render(app_handle: &tauri::AppHandle, job: ScopeAnalyticsJob) -> Result<(), String> {
    let state = app_handle.state::<AppState>();
    let develop = app_handle.state::<DevelopSession>();
    // A frame that carried its own analytics came since (the release).
    let generation = job.analytics.generation;
    let superseded = || {
        state
            .preview
            .full_frame_analytics_generation
            .load(Ordering::SeqCst)
            > generation
    };
    if superseded() {
        return Ok(());
    }
    #[cfg(feature = "performance-profiling")]
    let _stage = crate::performance::PerformanceStage::new("scope_analytics_render");
    let gpu = app_handle.state::<crate::state::gpu::GpuState>();
    let context = get_or_init_gpu_context(&gpu, app_handle)?;
    let proxy = proxy_of(&state.preview, &job.source);
    let (width, height) = proxy.image.dimensions();
    let scale = job.scale * width as f32 / job.source.width().max(1) as f32;
    let (loaded_image, source_scale, full_dims) = &job.photo;
    let mask_bitmaps = crate::editor_preview::preview_mask_bitmaps(
        &state,
        &develop,
        &job.recipe,
        &job.masks,
        (loaded_image, *source_scale, *full_dims),
        (width, height),
        scale,
        job.unscaled_crop_offset,
    )?;
    let mut adjustments = job.adjustments;
    let mut analytics = job.analytics;
    if let Some(extremes) = &proxy.extremes {
        let (width, height) = extremes.brightest.dimensions();
        // The layers' masks as the release frame draws them (at the preview
        // base's size: the mask cache's entries of that frame), read where
        // each copy pixel came from, so that a mask's edge falls between
        // pixels as in the release frame, not between blocks.
        let frame_masks = crate::editor_preview::preview_mask_bitmaps(
            &state,
            &develop,
            &job.recipe,
            &job.masks,
            (loaded_image, *source_scale, *full_dims),
            job.source.dimensions(),
            job.scale,
            job.unscaled_crop_offset,
        )?;
        adjustments.global.detail_scale =
            job.scale * width as f32 / job.source.width().max(1) as f32;
        let copies = [
            (&extremes.brightest, &extremes.brightest_at),
            (&extremes.darkest, &extremes.darkest_at),
        ];
        for ((copy, at), cache) in copies.into_iter().zip(&gpu.scope_extremes_cache) {
            #[cfg(feature = "performance-profiling")]
            let _stage = crate::performance::PerformanceStage::new("scope_clip_marks_render");
            let mask_bitmaps: Vec<_> = frame_masks
                .iter()
                .map(|mask| Arc::new(mask.gather(width, height, at)))
                .collect();
            let marks = gpu_processing::render_scope_clip_marks(
                &context,
                &gpu.processor,
                cache,
                (copy, proxy.id),
                RenderRequest {
                    adjustments: adjustments.clone(),
                    mask_bitmaps: &mask_bitmaps,
                    lut: job.lut.clone(),
                    roi: None,
                },
                job.display_space,
            )?;
            analytics.also_clipped = analytics.also_clipped.union(marks);
            if superseded() {
                return Ok(());
            }
        }
    }
    adjustments.global.detail_scale = scale;
    gpu_processing::render_scope_analytics(
        &context,
        &gpu.processor,
        &gpu.scope_cache,
        (&proxy.image, proxy.id),
        RenderRequest {
            adjustments,
            mask_bitmaps: &mask_bitmaps,
            lut: job.lut,
            roi: None,
        },
        analytics,
        job.display_space,
    )
}

/// The copy of a preview base the scope render reads, its block-extreme
/// copies (brightest, darkest pixels, long edge [`SCOPE_EXTREMES_EDGE`];
/// none when the copy is the base itself), and the id their GPU uploads are
/// kept under
/// (`GpuState::scope_cache`, `scope_extremes_cache`).
pub(crate) struct ScopeProxy {
    of: Weak<DynamicImage>,
    image: Arc<DynamicImage>,
    extremes: Option<Arc<BlockExtremes>>,
    id: u64,
}

/// The copy of `source` the scope render reads, made once per preview base.
fn proxy_of(preview: &PreviewState, source: &Arc<DynamicImage>) -> ScopeProxy {
    let mut kept = preview
        .scope_proxy
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if let Some(proxy) = kept.as_ref()
        && proxy.of.ptr_eq(&Arc::downgrade(source))
    {
        return proxy.clone();
    }
    #[cfg(feature = "performance-profiling")]
    let _stage = crate::performance::PerformanceStage::new("scope_proxy_build");
    let (width, height) = source.dimensions();
    let (w, h) = proxy_size(width, height);
    let (image, extremes) = if (w, h) == (width, height) {
        (Arc::clone(source), None)
    } else {
        let image = downscale_f32_image_signed(source, w, h);
        let (ew, eh) = size_within(width, height, SCOPE_EXTREMES_EDGE);
        let extremes = block_extreme_pixels(source, ew, eh);
        (Arc::new(image), Some(Arc::new(extremes)))
    };
    let proxy = ScopeProxy {
        of: Arc::downgrade(source),
        image,
        extremes,
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
            extremes: self.extremes.clone(),
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
        // The block-extreme copies, smaller, at the same aspect.
        assert_eq!(size_within(6240, 4160, SCOPE_EXTREMES_EDGE), (256, 171));
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
