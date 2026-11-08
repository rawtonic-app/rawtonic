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
//! then carry their marks too. A drag's first job renders both copies, each
//! later one only one of them, in turn, with the other's marks of the
//! previous job (about 100 ms old): each copy's render took some 4 ms on an
//! integrated GPU (Radeon 680M), and two per job beside the frames made a
//! zoomed drag with masks up to 3 ms slower at p95
//! (notes: benchmark-results/g23m-masks-drag-2026-11-08/REPORT.md). The
//! release frame's own analytics replace them. The histogram and the scopes count the
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
use std::time::{Duration, Instant};

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
use rawtonic_engine::scopes::ClippedSides;

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
    let mut kept = ExtremesKept::default();
    let sender = gpu_processing::start_latest_job_worker(move |job: ScopeAnalyticsJob| {
        if let Err(error) = render(&worker_app, job, &mut kept) {
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

/// How long the marks of one block-extreme copy count for the next jobs:
/// within a drag the jobs come about every 100 ms, so marks older than this
/// belong to an earlier drag or another recipe and both copies are rendered.
const EXTREMES_MARKS_FRESH: Duration = Duration::from_millis(250);

/// What the scope worker keeps from one job of a drag to the next.
#[derive(Default)]
struct ExtremesKept {
    /// The copy the marks below were found in ([`ScopeProxy::id`]).
    proxy_id: u64,
    /// Per block-extreme copy (brightest, darkest) its last marks and when.
    marks: [Option<(ClippedSides, Instant)>; 2],
    /// The copy the next job renders when the other's marks are fresh.
    next: usize,
    /// The previous job's layers, serialized.
    layers: Vec<String>,
}

impl ExtremesKept {
    /// The copies this job renders: both at a drag's start (or after a
    /// pause), else one, in turn.
    fn copies_to_render(&mut self, proxy_id: u64, now: Instant) -> Vec<usize> {
        if self.proxy_id != proxy_id {
            *self = Self {
                proxy_id,
                ..Self::default()
            };
        }
        let fresh = |marks: &Option<(ClippedSides, Instant)>| {
            marks.is_some_and(|(_, at)| now.duration_since(at) <= EXTREMES_MARKS_FRESH)
        };
        let other = 1 - self.next;
        if fresh(&self.marks[other]) {
            let copy = self.next;
            self.next = other;
            vec![copy]
        } else {
            vec![0, 1]
        }
    }

    /// The marks of both copies: the ones just found, and the other copy's
    /// from the previous job.
    fn found(&mut self, copy: usize, marks: ClippedSides, now: Instant) -> ClippedSides {
        self.marks[copy] = Some((marks, now));
        self.marks
            .iter()
            .flatten()
            .fold(ClippedSides::default(), |all, (marks, _)| all.union(*marks))
    }

    /// Per layer of `masks`, whether it is the previous job's layer
    /// unchanged (its mask at the preview base's size then is the mask
    /// cache's entry, drawn once), and keeps `masks` for the next job.
    fn unchanged_layers(&mut self, masks: &[MaskDefinition]) -> Vec<bool> {
        let layers: Vec<String> = masks
            .iter()
            .map(|mask| serde_json::to_string(mask).unwrap_or_default())
            .collect();
        let unchanged = layers
            .iter()
            .enumerate()
            .map(|(i, layer)| !layer.is_empty() && self.layers.get(i) == Some(layer))
            .collect();
        self.layers = layers;
        unchanged
    }
}

/// The worker's sender, None before it started.
pub(crate) fn sender(preview: &PreviewState) -> Option<LatestJobSender<ScopeAnalyticsJob>> {
    preview
        .scope_analytics_tx
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

fn render(
    app_handle: &tauri::AppHandle,
    job: ScopeAnalyticsJob,
    kept: &mut ExtremesKept,
) -> Result<(), String> {
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
    let proxy_masks = crate::editor_preview::preview_layer_mask_bitmaps(
        &state,
        &develop,
        &job.recipe,
        &job.masks,
        (loaded_image, *source_scale, *full_dims),
        (width, height),
        scale,
        job.unscaled_crop_offset,
        |_| true,
    )?;
    let mut adjustments = job.adjustments;
    let mut analytics = job.analytics;
    if let Some(extremes) = &proxy.extremes {
        let (width, height) = extremes.brightest.dimensions();
        // The layers' masks as the release frame draws them (at the preview
        // base's size: the mask cache's entries of that frame), read where
        // each copy pixel came from, so that a mask's edge falls between
        // pixels as in the release frame, not between blocks. A layer whose
        // shape changed since the previous job (a mask's feather or handle
        // dragged) would be drawn at that size again for every job; it is
        // read from its mask at the averaged copy's size instead, the pixel
        // nearest to where each copy pixel came from, until the release
        // frame's exact marks.
        let unchanged = kept.unchanged_layers(&job.masks);
        let frame_masks = crate::editor_preview::preview_layer_mask_bitmaps(
            &state,
            &develop,
            &job.recipe,
            &job.masks,
            (loaded_image, *source_scale, *full_dims),
            job.source.dimensions(),
            job.scale,
            job.unscaled_crop_offset,
            |i| unchanged[i],
        )?;
        adjustments.global.detail_scale =
            job.scale * width as f32 / job.source.width().max(1) as f32;
        let copies = [
            (
                &extremes.brightest,
                &extremes.brightest_at,
                &proxy.brightest_in_proxy,
            ),
            (
                &extremes.darkest,
                &extremes.darkest_at,
                &proxy.darkest_in_proxy,
            ),
        ];
        for copy in kept.copies_to_render(proxy.id, Instant::now()) {
            #[cfg(feature = "performance-profiling")]
            let _stage = crate::performance::PerformanceStage::new("scope_clip_marks_render");
            let (image, at, in_proxy) = copies[copy];
            let mask_bitmaps: Vec<_> = frame_masks
                .iter()
                .zip(&proxy_masks)
                .filter_map(|(frame, small)| match (frame, small) {
                    (Some(frame), _) => Some(Arc::new(frame.gather(width, height, at))),
                    (None, Some(small)) => Some(Arc::new(small.gather(width, height, in_proxy))),
                    (None, None) => None,
                })
                .collect();
            let marks = gpu_processing::render_scope_clip_marks(
                &context,
                &gpu.processor,
                &gpu.scope_extremes_cache[copy],
                (image, proxy.id),
                RenderRequest {
                    adjustments: adjustments.clone(),
                    mask_bitmaps: &mask_bitmaps,
                    lut: job.lut.clone(),
                    roi: None,
                },
                job.display_space,
            )?;
            analytics.also_clipped = kept.found(copy, marks, Instant::now());
            if superseded() {
                return Ok(());
            }
        }
    }
    adjustments.global.detail_scale = scale;
    let mask_bitmaps: Vec<_> = proxy_masks.into_iter().flatten().collect();
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

/// Per pixel index of a `from` sized frame, the index of the nearest pixel
/// of its `to` sized copy.
fn nearest_in(at: &[u32], (from_w, from_h): (u32, u32), (to_w, to_h): (u32, u32)) -> Vec<u32> {
    let map = |v: u64, from: u32, to: u32| {
        ((v * 2 + 1) * to as u64 / (from as u64 * 2)).min(to as u64 - 1)
    };
    at.iter()
        .map(|&i| {
            let (x, y) = (i as u64 % from_w as u64, i as u64 / from_w as u64);
            (map(y, from_h, to_h) * to_w as u64 + map(x, from_w, to_w)) as u32
        })
        .collect()
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
    /// Per pixel of each block-extreme copy, the averaged copy's pixel
    /// nearest to where it came from (a changed layer's mask is read there).
    brightest_in_proxy: Arc<Vec<u32>>,
    darkest_in_proxy: Arc<Vec<u32>>,
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
    let in_proxy = |at: fn(&BlockExtremes) -> &Vec<u32>| {
        Arc::new(extremes.as_deref().map_or_else(Vec::new, |extremes| {
            nearest_in(at(extremes), (width, height), (w, h))
        }))
    };
    let proxy = ScopeProxy {
        of: Arc::downgrade(source),
        image,
        brightest_in_proxy: in_proxy(|e| &e.brightest_at),
        darkest_in_proxy: in_proxy(|e| &e.darkest_at),
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
            brightest_in_proxy: Arc::clone(&self.brightest_in_proxy),
            darkest_in_proxy: Arc::clone(&self.darkest_in_proxy),
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

    #[test]
    fn a_changed_layer_reads_the_nearest_pixel_of_the_smaller_mask() {
        // 8 x 4 frame, 4 x 2 copy: pixel (5, 3) lies in copy pixel (2, 1),
        // (0, 0) in (0, 0), (7, 0) in (3, 0).
        assert_eq!(nearest_in(&[3 * 8 + 5, 0, 7], (8, 4), (4, 2)), [6, 0, 3]);
        // Equal sizes keep the index.
        assert_eq!(nearest_in(&[0, 5, 11], (4, 3), (4, 3)), [0, 5, 11]);
    }

    #[test]
    fn the_drag_renders_one_extreme_copy_per_job_after_its_first() {
        let start = Instant::now();
        let at = |ms| start + Duration::from_millis(ms);
        let highlights = ClippedSides {
            highlights: true,
            ..Default::default()
        };
        let shadows = ClippedSides {
            shadows: true,
            ..Default::default()
        };
        let mut kept = ExtremesKept::default();
        // The first job renders both copies.
        assert_eq!(kept.copies_to_render(1, at(0)), [0, 1]);
        kept.found(0, highlights, at(0));
        assert_eq!(kept.found(1, ClippedSides::default(), at(0)), highlights);
        // Then one per job, in turn, each job's marks the union of both
        // copies' last ones.
        assert_eq!(kept.copies_to_render(1, at(100)), [0]);
        assert_eq!(
            kept.found(0, ClippedSides::default(), at(100)),
            ClippedSides::default()
        );
        assert_eq!(kept.copies_to_render(1, at(200)), [1]);
        assert_eq!(kept.found(1, shadows, at(200)), shadows);
        assert_eq!(kept.copies_to_render(1, at(300)), [0]);
        assert_eq!(
            kept.found(0, highlights, at(300)),
            highlights.union(shadows)
        );
        // After a pause (the next drag) the other copy's marks are old:
        // both again.
        assert_eq!(kept.copies_to_render(1, at(1300)), [0, 1]);
        // Another preview base forgets the marks.
        kept.found(0, highlights, at(1300));
        kept.found(1, shadows, at(1300));
        assert_eq!(kept.copies_to_render(2, at(1350)), [0, 1]);
        assert_eq!(
            kept.found(0, ClippedSides::default(), at(1350)),
            ClippedSides::default()
        );
    }
}
