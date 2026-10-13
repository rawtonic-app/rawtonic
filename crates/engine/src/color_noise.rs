// SPDX-License-Identifier: AGPL-3.0-or-later
//! Multi-scale colour noise reduction, set by the colour noise measured in
//! the photo itself.
//!
//! The chroma of the image (in square-root encoding, where shot noise has
//! about the same spread at every brightness: red minus green and blue minus
//! green) is split into a Laplacian pyramid. At each level the colour noise
//! is measured: the RMS of the level's detail in its smoothest blocks, where
//! the photo has no colour detail of its own (clipped and black blocks have
//! no noise to measure and are skipped). Each level's detail is then kept by
//! a soft threshold at a multiple of that level's noise, set by the recipe's
//! Color amount, Detail and Smoothness, and applied to the detail's local
//! energy (3 × 3), so single noisy pixels go and the tails of edges stay.
//! Two guards keep a level's detail where it belongs to an edge: a
//! luminance detail at that level well above the luminance noise, and a
//! chroma gradient of the coarser level well above the level's colour noise
//! (colour edges without a luminance step). Colour edges and colour texture
//! above the noise stay, blotches of every size within it go. Luminance is
//! kept exactly.
//!
//! The coarse levels are computed here on the CPU when a render source is
//! uploaded (`ChromaPyramid::build`, once per source) and rebuilt for new
//! slider values (`reconstruct`, a third of the source's pixels); the
//! renderer does the finest level per pixel (shader.wgsl,
//! `apply_color_noise_multiscale`) from level 1 of the pyramid and its
//! reconstruction. `apply_to_linear` is the same filter entirely on the CPU,
//! for tests and the measurement harness
//! (crates/gpu/examples/color_nr_harness.rs).
//!
//! The idea (a chroma pyramid thresholded by noise measured per level) was
//! described for RAWmakase's adaptive colour noise reduction; the
//! implementation, the measurement and every constant here are Rawtonic's
//! own, chosen on the synthetic harness (notes: reference/detail.md).

use image::DynamicImage;
use rayon::prelude::*;
use std::borrow::Cow;
use std::sync::{Arc, OnceLock};

/// Recipe key of the colour noise reduction's version: [`VERSION`] selects
/// this filter, 1 the renderer's cross-bilateral filter. Recipes without the
/// key keep the filter they rendered with before: the cross-bilateral one
/// when they set `colorNoiseReduction`, this one when they leave it to the
/// RAW default (photos never edited; the editor starts them the same way).
pub const VERSION_KEY: &str = "colorNoiseVersion";
pub const VERSION: u64 = 2;

/// Whether a recipe's colour noise reduction is this filter.
pub fn multiscale(adjustments: &serde_json::Value, is_raw: bool) -> bool {
    match adjustments
        .get(VERSION_KEY)
        .and_then(serde_json::Value::as_u64)
    {
        Some(version) => version >= VERSION,
        None => is_raw && adjustments.get("colorNoiseReduction").is_none(),
    }
}

/// Detail levels with their own threshold; the level below them (1/32 of
/// the resolution) is kept as it is.
pub const LEVELS: usize = 5;
/// Side of the blocks the noise is measured in, at every level.
const BLOCK: usize = 8;
/// Share of the measurable blocks, from the smoothest, whose detail is
/// taken as the noise: colour detail only adds to a block's variation.
const SMOOTHEST: f32 = 0.2;
/// The fewest measurable blocks a level's noise is measured from.
const MIN_BLOCKS: usize = 16;
/// A level without enough measurable blocks gets this share of the finer
/// level's noise (demosaiced colour noise on the synthetic harness: 0.45–0.7).
const COARSER_FALLBACK: f32 = 0.6;
/// Pixels darker than this linear luminance (crushed blacks) or with a
/// channel above `MEASURABLE_PEAK` (clipping) are not measured.
const MEASURABLE_BLACK: f32 = 0.0005;
const MEASURABLE_PEAK: f32 = 0.9;
/// Luminance weights of the renderer (shader.wgsl `LUMA_COEFF`).
const LUMA: [f32; 3] = [0.2126, 0.7152, 0.0722];

/// The recipe's colour noise settings, each 0–1: Color amount, Detail and
/// Smoothness (Lightroom's defaults 25, 50, 50).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Settings {
    pub amount: f32,
    pub detail: f32,
    pub smoothness: f32,
}

/// The constants of the filter, chosen on the measurement harness.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tuning {
    /// Threshold of each level (finest first) in multiples of its measured
    /// noise at Amount 25, Detail and Smoothness 50.
    pub multiples: [f32; LEVELS],
    /// How sharply the gain moves from removed to kept around the
    /// threshold: the gain is x^s / (1 + x^s) with x = |detail|² / threshold².
    pub steepness: f32,
    /// Threshold growth from Amount 25 to 100 (× (1 + growth)).
    pub amount_growth: f32,
    /// Detail scales level `l`'s threshold by 2^((0.5 − detail) · exp).
    pub detail_exponents: [f32; LEVELS],
    /// Smoothness scales level `l`'s threshold by 2^((smoothness − 0.5) · exp).
    pub smoothness_exponents: [f32; LEVELS],
    /// A level's chroma detail is kept where its luminance detail exceeds
    /// this multiple of the level's luminance noise (0: off).
    pub luma_guard: f32,
    /// A level's detail is kept where the coarser level's chroma gradient
    /// exceeds this multiple of the level's noise (0: off).
    pub edge_guard: f32,
}

impl Default for Tuning {
    fn default() -> Self {
        Self {
            multiples: [5.0, 5.0, 5.0, 5.0, 5.0],
            steepness: 3.0,
            amount_growth: 1.0,
            detail_exponents: [3.0, 1.5, 0.5, 0.0, 0.0],
            smoothness_exponents: [0.0, 0.0, 0.5, 1.5, 2.0],
            luma_guard: 3.0,
            edge_guard: 7.0,
        }
    }
}

impl Tuning {
    /// Each level's threshold (in square-root chroma units) for `settings`
    /// and the measured `noise`; all zero at Amount 0.
    pub fn thresholds(&self, settings: Settings, noise: &[f32; LEVELS]) -> [f32; LEVELS] {
        let amount = settings.amount.clamp(0.0, 1.0);
        if amount <= 0.0 {
            return [0.0; LEVELS];
        }
        // Like Lightroom's Color slider, most of the effect is reached by
        // 25; above, the thresholds grow slowly.
        let response = (amount.min(0.25) / 0.25).sqrt()
            * (1.0 + self.amount_growth * (amount - 0.25).max(0.0) / 0.75);
        let detail = settings.detail.clamp(0.0, 1.0);
        let smoothness = settings.smoothness.clamp(0.0, 1.0);
        std::array::from_fn(|l| {
            noise[l]
                * self.multiples[l]
                * response
                * (0.5 - detail).mul_add(self.detail_exponents[l], 0.0).exp2()
                * (smoothness - 0.5)
                    .mul_add(self.smoothness_exponents[l], 0.0)
                    .exp2()
        })
    }
}

/// Share of a level's detail that is kept: x^s / (1 + x^s) with x =
/// `size2` / `threshold2`; everything is kept without a threshold.
#[inline]
pub fn gain(size2: f32, threshold2: f32, steepness: f32) -> f32 {
    if threshold2 <= 0.0 {
        return 1.0;
    }
    if size2 <= 0.0 {
        return 0.0;
    }
    1.0 / (1.0 + (threshold2 / size2).powf(steepness))
}

/// Channels of the pyramid: red − green, blue − green and luminance, all in
/// square-root encoding.
pub const CH: usize = 3;
pub type Px = [f32; CH];

/// A chroma plane (red − green, blue − green and the luminance that guards
/// edges, in square-root encoding).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Plane {
    pub width: usize,
    pub height: usize,
    pub data: Vec<Px>,
}

impl Plane {
    #[inline]
    fn at(&self, x: usize, y: usize) -> Px {
        self.data[y * self.width + x]
    }
}

/// Signed square root, as the renderer's `nr_encode`.
#[inline]
pub fn encode(v: f32) -> f32 {
    if v < 0.0 { -(-v).sqrt() } else { v.sqrt() }
}

#[inline]
fn decode(p: f32) -> f32 {
    p * p.abs()
}

/// Chroma of a linear RGB pixel.
#[inline]
pub fn chroma(rgb: [f32; 3]) -> [f32; 2] {
    let p = rgb.map(encode);
    [p[0] - p[1], p[2] - p[1]]
}

/// Chroma and luminance of a linear RGB pixel, as the pyramid holds them.
#[inline]
pub fn chroma_luma(rgb: [f32; 3]) -> Px {
    let p = rgb.map(encode);
    [
        p[0] - p[1],
        p[2] - p[1],
        LUMA[0] * p[0] + LUMA[1] * p[1] + LUMA[2] * p[2],
    ]
}

#[inline]
fn measurable(rgb: [f32; 3]) -> bool {
    let luma = LUMA[0] * rgb[0] + LUMA[1] * rgb[1] + LUMA[2] * rgb[2];
    luma >= MEASURABLE_BLACK && rgb.iter().all(|v| *v < MEASURABLE_PEAK)
}

#[inline]
fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// Linear RGB pixels of a render source (RAW sources are linear, others
/// sRGB-encoded, as the renderer reads them).
struct Source<'a> {
    data: Cow<'a, [f32]>,
    channels: usize,
    width: usize,
    height: usize,
    srgb: bool,
}

impl<'a> Source<'a> {
    fn new(image: &'a DynamicImage, linear: bool) -> Self {
        let (width, height) = (image.width() as usize, image.height() as usize);
        let (data, channels) = match image {
            DynamicImage::ImageRgb32F(buffer) => (Cow::Borrowed(buffer.as_raw().as_slice()), 3),
            DynamicImage::ImageRgba32F(buffer) => (Cow::Borrowed(buffer.as_raw().as_slice()), 4),
            other => (Cow::Owned(other.to_rgb32f().into_raw()), 3),
        };
        Self {
            data,
            channels,
            width,
            height,
            srgb: !linear,
        }
    }

    #[inline]
    fn pixel(&self, x: usize, y: usize) -> [f32; 3] {
        let i = (y * self.width + x) * self.channels;
        let rgb = [self.data[i], self.data[i + 1], self.data[i + 2]];
        if self.srgb {
            rgb.map(srgb_to_linear)
        } else {
            rgb
        }
    }
}

const KERNEL: [f32; 5] = [1.0 / 16.0, 4.0 / 16.0, 6.0 / 16.0, 4.0 / 16.0, 1.0 / 16.0];

#[inline]
fn mirror(i: isize, n: usize) -> usize {
    let n = n as isize;
    if n == 1 {
        return 0;
    }
    let i = if i < 0 { -i } else { i };
    let i = if i >= n { 2 * n - 2 - i } else { i };
    i.clamp(0, n - 1) as usize
}

/// Taps and weights of the expansion of a coarser level at finer position
/// `x` (`n` coarser samples): 1/8, 3/4, 1/8 around an even position, 1/2,
/// 1/2 between two samples at an odd one. Shared with the renderer.
#[inline]
fn expand_taps(x: usize, n: usize) -> ([usize; 3], [f32; 3]) {
    let last = n as isize - 1;
    let clamp = |i: isize| i.clamp(0, last) as usize;
    let k = (x / 2) as isize;
    if x.is_multiple_of(2) {
        ([clamp(k - 1), clamp(k), clamp(k + 1)], [0.125, 0.75, 0.125])
    } else {
        ([clamp(k), clamp(k + 1), clamp(k + 1)], [0.5, 0.5, 0.0])
    }
}

/// The expansion of `plane` at finer position (x, y).
#[inline]
pub fn expand_at(plane: &Plane, x: usize, y: usize) -> Px {
    let (xs, wx) = expand_taps(x, plane.width);
    let (ys, wy) = expand_taps(y, plane.height);
    let mut out = [0.0f32; CH];
    for (j, &yy) in ys.iter().enumerate() {
        if wy[j] == 0.0 {
            continue;
        }
        let mut row = [0.0f32; CH];
        for (i, &xx) in xs.iter().enumerate() {
            if wx[i] == 0.0 {
                continue;
            }
            let v = plane.at(xx, yy);
            for c in 0..CH {
                row[c] += wx[i] * v[c];
            }
        }
        for c in 0..CH {
            out[c] += wy[j] * row[c];
        }
    }
    out
}

/// `plane` filtered by the 5-tap binomial kernel and halved (rounded up).
fn reduce(plane: &Plane) -> Plane {
    let (w, h) = (plane.width, plane.height);
    let (w2, h2) = (w.div_ceil(2), h.div_ceil(2));
    let mut horizontal = vec![[0.0f32; CH]; w2 * h];
    horizontal
        .par_chunks_mut(w2)
        .enumerate()
        .for_each(|(y, row)| {
            for (x, v) in row.iter_mut().enumerate() {
                let mut sum = [0.0f32; CH];
                for (k, weight) in KERNEL.iter().enumerate() {
                    let s = plane.at(mirror(2 * x as isize + k as isize - 2, w), y);
                    for c in 0..CH {
                        sum[c] += weight * s[c];
                    }
                }
                *v = sum;
            }
        });
    let mut data = vec![[0.0f32; CH]; w2 * h2];
    data.par_chunks_mut(w2).enumerate().for_each(|(y, row)| {
        for (x, v) in row.iter_mut().enumerate() {
            let mut sum = [0.0f32; CH];
            for (k, weight) in KERNEL.iter().enumerate() {
                let s = horizontal[mirror(2 * y as isize + k as isize - 2, h) * w2 + x];
                for c in 0..CH {
                    sum[c] += weight * s[c];
                }
            }
            *v = sum;
        }
    });
    Plane {
        width: w2,
        height: h2,
        data,
    }
}

/// Measurability halved: a coarser pixel is measurable when the finer
/// pixels it covers are.
fn reduce_mask(mask: &[bool], w: usize, h: usize) -> Vec<bool> {
    let (w2, h2) = (w.div_ceil(2), h.div_ceil(2));
    (0..w2 * h2)
        .into_par_iter()
        .map(|i| {
            let (x, y) = (2 * (i % w2), 2 * (i / w2));
            mask[y * w + x]
                && mask[y * w + (x + 1).min(w - 1)]
                && mask[(y + 1).min(h - 1) * w + x]
                && mask[(y + 1).min(h - 1) * w + (x + 1).min(w - 1)]
        })
        .collect()
}

/// Rows of the first pyramid level computed together from the source.
const BAND_ROWS: usize = 32;

/// Level 1 of the chroma pyramid straight from the source (the
/// full-resolution chroma plane is never stored) and its measurability.
fn first_level(source: &Source) -> (Plane, Vec<bool>) {
    let (w, h) = (source.width, source.height);
    let (w2, h2) = (w.div_ceil(2), h.div_ceil(2));
    let mut data = vec![[0.0f32; CH]; w2 * h2];
    let mut mask = vec![false; w2 * h2];
    data.par_chunks_mut(w2 * BAND_ROWS)
        .zip(mask.par_chunks_mut(w2 * BAND_ROWS))
        .enumerate()
        .for_each(|(band, (out, out_mask))| {
            let y0 = band * BAND_ROWS;
            let rows = out.len() / w2;
            // Source rows 2·y0 − 2 … 2·(y0 + rows) + 1, horizontally reduced.
            let first = 2 * y0 as isize - 2;
            let count = 2 * rows + 3;
            let mut horizontal = vec![[0.0f32; CH]; count * w2];
            let mut line = vec![[0.0f32; CH]; w];
            for r in 0..count {
                let y = mirror(first + r as isize, h);
                for (x, c) in line.iter_mut().enumerate() {
                    *c = chroma_luma(source.pixel(x, y));
                }
                for x in 0..w2 {
                    let mut sum = [0.0f32; CH];
                    for (k, weight) in KERNEL.iter().enumerate() {
                        let s = line[mirror(2 * x as isize + k as isize - 2, w)];
                        for c in 0..CH {
                            sum[c] += weight * s[c];
                        }
                    }
                    horizontal[r * w2 + x] = sum;
                }
            }
            for row in 0..rows {
                for x in 0..w2 {
                    let mut sum = [0.0f32; CH];
                    for (k, weight) in KERNEL.iter().enumerate() {
                        let s = horizontal[(2 * row + k) * w2 + x];
                        for c in 0..CH {
                            sum[c] += weight * s[c];
                        }
                    }
                    out[row * w2 + x] = sum;
                    let (sx, sy) = (2 * x, 2 * (y0 + row));
                    out_mask[row * w2 + x] =
                        [(0, 0), (1, 0), (0, 1), (1, 1)].iter().all(|(dx, dy)| {
                            measurable(source.pixel((sx + dx).min(w - 1), (sy + dy).min(h - 1)))
                        });
                }
            }
        });
    (
        Plane {
            width: w2,
            height: h2,
            data,
        },
        mask,
    )
}

/// The smoothest-blocks RMS of a level's detail: `detail(x, y)` at the
/// level's pixels, `measurable(x, y)` at the coarser level's (each block of
/// `BLOCK` pixels covers `BLOCK / 2` of them). Blocks are taken every
/// `stride` blocks. `None` with fewer than `MIN_BLOCKS` measurable blocks.
fn smoothest_rms(
    width: usize,
    height: usize,
    stride: usize,
    detail: impl Fn(usize, usize) -> [f32; 2] + Sync,
    block_measurable: impl Fn(usize, usize) -> bool + Sync,
) -> Option<f32> {
    let (bw, bh) = (width / BLOCK, height / BLOCK);
    let (sw, sh) = (bw.div_ceil(stride), bh.div_ceil(stride));
    let mut energies: Vec<f32> = (0..sw * sh)
        .into_par_iter()
        .filter_map(|i| {
            let (bx, by) = ((i % sw) * stride, (i / sw) * stride);
            if !block_measurable(bx, by) {
                return None;
            }
            let mut sum = 0.0f32;
            for y in by * BLOCK..(by + 1) * BLOCK {
                for x in bx * BLOCK..(bx + 1) * BLOCK {
                    let d = detail(x, y);
                    sum += d[0] * d[0] + d[1] * d[1];
                }
            }
            Some(sum / (BLOCK * BLOCK) as f32)
        })
        .collect();
    if energies.len() < MIN_BLOCKS {
        return None;
    }
    let k = ((energies.len() as f32 * SMOOTHEST) as usize).min(energies.len() - 1);
    let (_, e, _) = energies.select_nth_unstable_by(k, f32::total_cmp);
    Some(e.sqrt())
}

/// Block stride keeping the measured blocks of a level near this count.
const MEASURED_BLOCKS: usize = 60_000;

fn block_stride(width: usize, height: usize) -> usize {
    let blocks = (width / BLOCK) * (height / BLOCK);
    let mut stride = 1;
    while blocks / (stride * stride) > MEASURED_BLOCKS {
        stride += 1;
    }
    stride
}

/// The chroma pyramid of a render source: levels 1–5 (half resolution
/// down to 1/32) and the colour noise measured at levels 0–4.
#[derive(Clone, Debug)]
pub struct ChromaPyramid {
    pub width: usize,
    pub height: usize,
    /// Gaussian levels 1…LEVELS (index 0 = half resolution).
    pub levels: Vec<Plane>,
    /// Colour noise (RMS of the level's detail in square-root chroma units)
    /// at levels 0…LEVELS − 1; 0 where nothing could be measured.
    pub noise: [f32; LEVELS],
    /// Luminance noise measured the same way.
    pub luma_noise: [f32; LEVELS],
    prepared: OnceLock<Arc<Prepared>>,
}

impl ChromaPyramid {
    /// Builds the pyramid of `image` (linear RGB when `linear`, otherwise
    /// sRGB-encoded) and measures its colour noise. `None` for images too
    /// small to filter.
    pub fn build(image: &DynamicImage, linear: bool) -> Option<Self> {
        let source = Source::new(image, linear);
        let (w, h) = (source.width, source.height);
        if w < 2 * BLOCK || h < 2 * BLOCK {
            return None;
        }
        let (g1, mask1) = first_level(&source);
        let mut levels = vec![g1];
        let mut masks = vec![mask1];
        for _ in 1..LEVELS {
            let previous = levels.last().unwrap();
            let next = reduce(previous);
            let mask = reduce_mask(masks.last().unwrap(), previous.width, previous.height);
            levels.push(next);
            masks.push(mask);
        }

        // Chroma (channels 0, 1) and luminance (channel 2) noise per level.
        let measure = |channels: std::ops::Range<usize>| -> [f32; LEVELS] {
            let pick = |d: Px| -> [f32; 2] {
                let mut out = [0.0f32; 2];
                for (o, c) in out.iter_mut().zip(channels.clone()) {
                    *o = d[c];
                }
                out
            };
            let mut measured = [None; LEVELS];
            // Level 0: the source minus level 1 expanded; a block is
            // measurable when the level-1 pixels it covers are.
            let g1 = &levels[0];
            measured[0] = smoothest_rms(
                w,
                h,
                block_stride(w, h),
                |x, y| {
                    let c = chroma_luma(source.pixel(x, y));
                    let e = expand_at(g1, x, y);
                    pick(std::array::from_fn(|k| c[k] - e[k]))
                },
                |bx, by| block_covered(&masks[0], g1.width, g1.height, bx, by),
            );
            for l in 1..LEVELS {
                let (fine, coarse) = (&levels[l - 1], &levels[l]);
                measured[l] = smoothest_rms(
                    fine.width,
                    fine.height,
                    block_stride(fine.width, fine.height),
                    |x, y| {
                        let c = fine.at(x, y);
                        let e = expand_at(coarse, x, y);
                        pick(std::array::from_fn(|k| c[k] - e[k]))
                    },
                    |bx, by| block_covered(&masks[l], coarse.width, coarse.height, bx, by),
                );
            }
            let mut noise = [0.0f32; LEVELS];
            for l in 0..LEVELS {
                noise[l] = match (measured[l], l) {
                    (Some(n), 0) => n,
                    // Demosaiced colour noise is never larger at a coarser
                    // level: more than the finer level's is colour detail.
                    (Some(n), _) => n.min(noise[l - 1]),
                    (None, 0) => 0.0,
                    (None, _) => noise[l - 1] * COARSER_FALLBACK,
                };
            }
            noise
        };
        let noise = measure(0..2);
        let luma_noise = measure(2..3);
        Some(Self {
            width: w,
            height: h,
            levels,
            noise,
            luma_noise,
            prepared: OnceLock::new(),
        })
    }

    /// Level 1 rebuilt with each level's detail kept by its threshold
    /// (`thresholds` from [`Tuning::thresholds`]); level 1 itself when all
    /// thresholds of the coarse levels are zero.
    pub fn reconstruct(&self, thresholds: &[f32; LEVELS], tuning: &Tuning) -> Plane {
        if thresholds[1..].iter().all(|t| *t <= 0.0) {
            return self.levels[0].clone();
        }
        // What does not depend on the sliders is prepared once per source.
        let computed;
        let prepared = match self.prepared.get() {
            Some(prepared) if prepared.tuning == *tuning => prepared,
            _ => {
                computed = Arc::new(self.prepare(tuning));
                if self.prepared.get().is_none() {
                    let _ = self.prepared.set(Arc::clone(&computed));
                }
                &computed
            }
        };
        let steepness = tuning.steepness;
        // The coarsest level stays; levels LEVELS − 1 … 1 are rebuilt.
        let mut rebuilt = self.levels[LEVELS - 1].clone();
        for l in (1..LEVELS).rev() {
            let fine = &self.levels[l - 1];
            let level = &prepared.levels[l - 1];
            let t2 = thresholds[l] * thresholds[l];
            let mut data = vec![[0.0f32; CH]; fine.width * fine.height];
            data.par_chunks_mut(fine.width)
                .enumerate()
                .for_each(|(y, row)| {
                    for (x, v) in row.iter_mut().enumerate() {
                        let i = y * fine.width + x;
                        let d = level.detail[i];
                        let g = gain(level.size2[i], t2, steepness).max(level.guard[i]);
                        let r = expand_at(&rebuilt, x, y);
                        *v = [r[0] + g * d[0], r[1] + g * d[1], 0.0];
                    }
                });
            rebuilt = Plane {
                width: fine.width,
                height: fine.height,
                data,
            };
        }
        rebuilt
    }

    /// Each coarse level's detail, its local energy and the share its
    /// guards keep.
    fn prepare(&self, tuning: &Tuning) -> Prepared {
        let steepness = tuning.steepness;
        let levels = (1..LEVELS)
            .map(|l| {
                let fine = &self.levels[l - 1];
                let coarse = &self.levels[l];
                // Colour edges mostly come with a luminance edge: where the
                // level's luminance detail stands out of its noise, its
                // chroma detail stays.
                let luma2 = (tuning.luma_guard * self.luma_noise[l]).powi(2);
                // Colour edges of the coarser level (its chroma gradient
                // well above this level's noise) keep this level's detail
                // around them, also where the luminance does not change.
                let edge2 = (tuning.edge_guard * self.noise[l]).powi(2);
                let gradient = (edge2 > 0.0).then(|| chroma_gradient(coarse));
                let (detail, guard): (Vec<[f32; 2]>, Vec<f32>) = (0..fine.width * fine.height)
                    .into_par_iter()
                    .map(|i| {
                        let (x, y) = (i % fine.width, i / fine.width);
                        let f = fine.at(x, y);
                        let e = expand_at(coarse, x, y);
                        let mut guard = 0.0f32;
                        if luma2 > 0.0 {
                            let dl = f[2] - e[2];
                            guard = guard.max(gain(dl * dl, luma2, steepness));
                        }
                        if let Some(gradient) = &gradient {
                            guard = guard.max(gain(expand_at(gradient, x, y)[0], edge2, steepness));
                        }
                        ([f[0] - e[0], f[1] - e[1]], guard)
                    })
                    .unzip();
                // The size of the detail: its local energy (3 × 3, weights
                // 1-2-1), so isolated noise peaks go and an edge's tails stay.
                let size2 = local_energy(&detail, fine.width, fine.height);
                PreparedLevel {
                    detail,
                    size2,
                    guard,
                }
            })
            .collect();
        Prepared {
            tuning: *tuning,
            levels,
        }
    }
}

/// A pyramid's coarse levels ready for new thresholds (levels 1…LEVELS − 1).
#[derive(Debug)]
struct Prepared {
    tuning: Tuning,
    levels: Vec<PreparedLevel>,
}

#[derive(Debug)]
struct PreparedLevel {
    detail: Vec<[f32; 2]>,
    size2: Vec<f32>,
    guard: Vec<f32>,
}

/// Squared chroma gradient (central differences) of a level, in channel 0.
fn chroma_gradient(plane: &Plane) -> Plane {
    let (w, h) = (plane.width, plane.height);
    let data = (0..w * h)
        .into_par_iter()
        .map(|i| {
            let (x, y) = (i % w, i / w);
            let l = plane.at(x.saturating_sub(1), y);
            let r = plane.at((x + 1).min(w - 1), y);
            let u = plane.at(x, y.saturating_sub(1));
            let d = plane.at(x, (y + 1).min(h - 1));
            let gx = [0.5 * (r[0] - l[0]), 0.5 * (r[1] - l[1])];
            let gy = [0.5 * (d[0] - u[0]), 0.5 * (d[1] - u[1])];
            [
                gx[0] * gx[0] + gx[1] * gx[1] + gy[0] * gy[0] + gy[1] * gy[1],
                0.0,
                0.0,
            ]
        })
        .collect();
    Plane {
        width: w,
        height: h,
        data,
    }
}

/// Squared size of each detail value: the 1-2-1 weighted mean of the
/// squared detail over its 3 × 3 neighbourhood (borders clamped), as the
/// renderer computes it at the finest level.
fn local_energy(detail: &[[f32; 2]], width: usize, height: usize) -> Vec<f32> {
    let own: Vec<f32> = detail.iter().map(|d| d[0] * d[0] + d[1] * d[1]).collect();
    const W: [f32; 3] = [0.25, 0.5, 0.25];
    (0..width * height)
        .into_par_iter()
        .map(|i| {
            let (x, y) = (i % width, i / width);
            let mut sum = 0.0;
            for (j, wy) in W.iter().enumerate() {
                let yy = (y + j).saturating_sub(1).min(height - 1);
                for (k, wx) in W.iter().enumerate() {
                    let xx = (x + k).saturating_sub(1).min(width - 1);
                    sum += wy * wx * own[yy * width + xx];
                }
            }
            sum
        })
        .collect()
}

/// Whether every coarser pixel under block (bx, by) of the finer level is
/// measurable.
fn block_covered(mask: &[bool], width: usize, height: usize, bx: usize, by: usize) -> bool {
    let half = BLOCK / 2;
    (by * half..((by + 1) * half).min(height))
        .all(|y| (bx * half..((bx + 1) * half).min(width)).all(|x| mask[y * width + x]))
}

/// The filtered colour of one linear pixel: its chroma `c0` is replaced by
/// `expand(rebuilt) + gain · (c0 − expand(level 1))` and its luminance kept,
/// as the renderer does it.
#[inline]
pub fn filtered_pixel(rgb: [f32; 3], level1: Px, rebuilt: Px, g: f32) -> [f32; 3] {
    let p = rgb.map(encode);
    let lp = LUMA[0] * p[0] + LUMA[1] * p[1] + LUMA[2] * p[2];
    let c0 = [p[0] - p[1], p[2] - p[1]];
    let d = [c0[0] - level1[0], c0[1] - level1[1]];
    let a = rebuilt[0] + g * d[0];
    let b = rebuilt[1] + g * d[1];
    // Chroma with zero luminance from the two differences.
    let pg = -(LUMA[0] * a + LUMA[2] * b);
    let q = [lp + pg + a, lp + pg, lp + pg + b];
    let mut out = q.map(decode);
    let luma = LUMA[0] * rgb[0] + LUMA[1] * rgb[1] + LUMA[2] * rgb[2];
    let new_luma = LUMA[0] * out[0] + LUMA[1] * out[1] + LUMA[2] * out[2];
    if new_luma > 1e-6 {
        let k = luma / new_luma;
        out = out.map(|v| v * k);
    }
    out
}

/// The whole filter on the CPU: `image` (linear RGB, 3 channels) with its
/// colour noise reduced by `settings`; the image unchanged at Amount 0.
pub fn apply_to_linear(
    image: &image::Rgb32FImage,
    settings: Settings,
    tuning: &Tuning,
) -> image::Rgb32FImage {
    let dynamic = DynamicImage::ImageRgb32F(image.clone());
    let Some(pyramid) = ChromaPyramid::build(&dynamic, true) else {
        return image.clone();
    };
    let thresholds = tuning.thresholds(settings, &pyramid.noise);
    if thresholds.iter().all(|t| *t <= 0.0) {
        return image.clone();
    }
    apply_with(image, &pyramid, &thresholds, tuning)
}

/// [`apply_to_linear`] with a built pyramid and thresholds.
pub fn apply_with(
    image: &image::Rgb32FImage,
    pyramid: &ChromaPyramid,
    thresholds: &[f32; LEVELS],
    tuning: &Tuning,
) -> image::Rgb32FImage {
    let steepness = tuning.steepness;
    let rebuilt = pyramid.reconstruct(thresholds, tuning);
    let level1 = &pyramid.levels[0];
    let (w, h) = (image.width() as usize, image.height() as usize);
    let src = image.as_raw();
    let pixel = |i: usize| [src[3 * i], src[3 * i + 1], src[3 * i + 2]];
    let detail: Vec<[f32; 2]> = (0..w * h)
        .into_par_iter()
        .map(|i| {
            let c = chroma(pixel(i));
            let e = expand_at(level1, i % w, i / w);
            [c[0] - e[0], c[1] - e[1]]
        })
        .collect();
    let size2 = local_energy(&detail, w, h);
    let t2 = thresholds[0] * thresholds[0];
    let mut out = vec![0.0f32; w * h * 3];
    out.par_chunks_mut(w * 3).enumerate().for_each(|(y, row)| {
        for x in 0..w {
            let i = y * w + x;
            let v = filtered_pixel(
                pixel(i),
                expand_at(level1, x, y),
                expand_at(&rebuilt, x, y),
                gain(size2[i], t2, steepness),
            );
            row[x * 3..x * 3 + 3].copy_from_slice(&v);
        }
    });
    image::Rgb32FImage::from_raw(w as u32, h as u32, out).unwrap()
}

/// Full chroma plane of a linear image (tests).
#[cfg(test)]
fn chroma_plane(image: &image::Rgb32FImage) -> Plane {
    Plane {
        width: image.width() as usize,
        height: image.height() as usize,
        data: image.pixels().map(|p| chroma_luma(p.0)).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgb32FImage;

    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> f32 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((self.0 >> 40) as f32) / (1u64 << 24) as f32
        }
        fn gaussian(&mut self) -> f32 {
            let u1 = self.next().max(1e-7);
            let u2 = self.next();
            (-2.0 * u1.ln()).sqrt() * (std::f32::consts::TAU * u2).cos()
        }
    }

    /// A grey image with chroma noise of `sigma` per square-root channel
    /// on its left half and strong colour texture on its right half.
    fn noisy(width: u32, height: u32, sigma: f32, textured: bool, seed: u64) -> Rgb32FImage {
        let mut rng = Rng(seed);
        Rgb32FImage::from_fn(width, height, |x, y| {
            let base = if textured && x >= width / 2 {
                let t = ((x as f32 * 0.9).sin() * (y as f32 * 0.7).cos()) * 0.12;
                [0.45 + t, 0.45, 0.45 - t]
            } else {
                [0.45, 0.45, 0.45]
            };
            image::Rgb(base.map(|v| {
                let p = v.sqrt() + sigma * rng.gaussian();
                p * p
            }))
        })
    }

    /// RMS of level-0 detail of a whole image (what the estimator should
    /// find on a flat noisy image).
    fn level0_rms(image: &Rgb32FImage) -> f32 {
        let plane = chroma_plane(image);
        let g1 = reduce(&plane);
        let mut sum = 0.0f64;
        for y in 0..plane.height {
            for x in 0..plane.width {
                let c = plane.at(x, y);
                let e = expand_at(&g1, x, y);
                sum += ((c[0] - e[0]).powi(2) + (c[1] - e[1]).powi(2)) as f64;
            }
        }
        (sum / (plane.width * plane.height) as f64).sqrt() as f32
    }

    #[test]
    fn first_level_matches_the_reduced_full_plane() {
        let image = noisy(77, 53, 0.02, true, 3);
        let pyramid =
            ChromaPyramid::build(&DynamicImage::ImageRgb32F(image.clone()), true).unwrap();
        let reduced = reduce(&chroma_plane(&image));
        assert_eq!(pyramid.levels[0].width, reduced.width);
        for (a, b) in pyramid.levels[0].data.iter().zip(&reduced.data) {
            assert!((a[0] - b[0]).abs() < 1e-6 && (a[1] - b[1]).abs() < 1e-6);
        }
    }

    #[test]
    fn measures_known_noise_within_tolerance() {
        for (sigma, seed) in [(0.004, 1), (0.012, 2), (0.03, 3)] {
            let flat = noisy(512, 384, sigma, false, seed);
            let truth = level0_rms(&flat);
            let pyramid = ChromaPyramid::build(&DynamicImage::ImageRgb32F(flat), true).unwrap();
            let ratio = pyramid.noise[0] / truth;
            assert!((0.85..=1.0).contains(&ratio), "flat σ {sigma}: {ratio}");
            // Texture on half the frame does not raise the estimate.
            let textured = noisy(512, 384, sigma, true, seed + 10);
            let measured = ChromaPyramid::build(&DynamicImage::ImageRgb32F(textured), true)
                .unwrap()
                .noise[0];
            assert!(
                (measured / pyramid.noise[0] - 1.0).abs() < 0.1,
                "textured σ {sigma}: {measured}"
            );
            // White noise of σ per channel: chroma differences have σ√2 per
            // component; the estimate scales with σ.
            assert!(pyramid.noise[0] > sigma && pyramid.noise[0] < 2.0 * sigma);
            // Coarser levels of white noise are much smaller.
            assert!(pyramid.noise[1] < 0.5 * pyramid.noise[0]);
        }
    }

    #[test]
    fn clipped_and_black_areas_are_not_measured() {
        let mut image = noisy(256, 256, 0.01, false, 5);
        // A clipped half with huge differences between channels.
        for y in 0..256 {
            for x in 0..128 {
                image.put_pixel(
                    x,
                    y,
                    image::Rgb([if (x + y) % 2 == 0 { 1.0 } else { 0.95 }, 0.2, 1.0]),
                );
            }
        }
        let pyramid = ChromaPyramid::build(&DynamicImage::ImageRgb32F(image), true).unwrap();
        let flat = ChromaPyramid::build(
            &DynamicImage::ImageRgb32F(noisy(128, 256, 0.01, false, 5)),
            true,
        )
        .unwrap();
        assert!((pyramid.noise[0] / flat.noise[0] - 1.0).abs() < 0.15);
    }

    #[test]
    fn amount_zero_passes_the_image_through_bit_exactly() {
        let image = noisy(96, 80, 0.02, true, 7);
        let settings = Settings {
            amount: 0.0,
            detail: 0.5,
            smoothness: 0.5,
        };
        let out = apply_to_linear(&image, settings, &Tuning::default());
        assert_eq!(out.as_raw(), image.as_raw());
        let pyramid = ChromaPyramid::build(&DynamicImage::ImageRgb32F(image), true).unwrap();
        assert_eq!(
            pyramid.reconstruct(&[0.0; LEVELS], &Tuning::default()),
            pyramid.levels[0]
        );
    }

    #[test]
    fn the_pyramid_reconstructs_its_input() {
        // Thresholds far below every detail keep it all: the pyramid
        // returns level 1 and each pixel its own colour.
        let image = noisy(130, 70, 0.02, true, 9);
        let pyramid =
            ChromaPyramid::build(&DynamicImage::ImageRgb32F(image.clone()), true).unwrap();
        let tiny = [1e-12; LEVELS];
        let rebuilt = pyramid.reconstruct(&tiny, &Tuning::default());
        for (a, b) in rebuilt.data.iter().zip(&pyramid.levels[0].data) {
            assert!((a[0] - b[0]).abs() < 1e-5 && (a[1] - b[1]).abs() < 1e-5);
        }
        let out = apply_with(&image, &pyramid, &tiny, &Tuning::default());
        for (a, b) in out.as_raw().iter().zip(image.as_raw()) {
            assert!((a - b).abs() < 1e-4 * b.abs().max(1e-3), "{a} {b}");
        }
    }

    #[test]
    fn removes_noise_and_keeps_luminance() {
        let image = noisy(256, 256, 0.02, false, 11);
        let settings = Settings {
            amount: 0.25,
            detail: 0.5,
            smoothness: 0.5,
        };
        let out = apply_to_linear(&image, settings, &Tuning::default());
        let before = level0_rms(&image);
        let after = level0_rms(&out);
        assert!(after < 0.3 * before, "{before} → {after}");
        for (a, b) in out.pixels().zip(image.pixels()) {
            let la = LUMA[0] * a[0] + LUMA[1] * a[1] + LUMA[2] * a[2];
            let lb = LUMA[0] * b[0] + LUMA[1] * b[1] + LUMA[2] * b[2];
            assert!((la - lb).abs() < 1e-5 * lb.max(1e-3));
        }
    }

    #[test]
    fn the_version_follows_the_recipe() {
        use serde_json::json;
        assert!(multiscale(&json!({"colorNoiseVersion": 2}), false));
        assert!(!multiscale(&json!({"colorNoiseVersion": 1}), true));
        // Saved colour noise reduction without a version: as before.
        assert!(!multiscale(&json!({"colorNoiseReduction": 25}), true));
        // The RAW default (never edited): this filter.
        assert!(multiscale(&json!({}), true));
        assert!(multiscale(&serde_json::Value::Null, true));
        assert!(!multiscale(&json!({}), false));
    }

    #[test]
    fn thresholds_follow_the_sliders() {
        let tuning = Tuning::default();
        let noise = [0.01, 0.006, 0.004, 0.003, 0.002];
        let at = |amount, detail, smoothness| {
            tuning.thresholds(
                Settings {
                    amount,
                    detail,
                    smoothness,
                },
                &noise,
            )
        };
        assert_eq!(at(0.0, 0.5, 0.5), [0.0; LEVELS]);
        let default = at(0.25, 0.5, 0.5);
        for l in 0..LEVELS {
            assert!((default[l] - noise[l] * tuning.multiples[l]).abs() < 1e-7);
            assert!(at(1.0, 0.5, 0.5)[l] > default[l]);
            assert!(at(0.1, 0.5, 0.5)[l] < default[l]);
        }
        // More Detail lowers the finest thresholds, more Smoothness raises
        // the coarsest.
        assert!(at(0.25, 1.0, 0.5)[0] < default[0]);
        assert!(at(0.25, 0.5, 1.0)[LEVELS - 1] > default[LEVELS - 1]);
    }
}
