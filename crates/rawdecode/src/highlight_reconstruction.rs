//! Reconstruction of clipped sensor channels on the CFA mosaic, before
//! demosaicing.
//!
//! When a photograph is over-exposed, the channels saturate at different
//! scene brightnesses once the white balance is applied: green (multiplier
//! 1.0) first, red and blue (multipliers 1.5–2.5) up to a stop later. Scaling
//! and desaturating the clipped pixels, as the inherited pipeline did, turns a
//! blue sky with clouds into a flat white (or lilac) area although red and
//! blue were still recorded.
//!
//! This step follows the idea of darktable's "inpaint opposed" method: for
//! every colour it measures, on unclipped pixels next to clipped areas, how
//! the channel sits relative to the mean of the other two (the chrominance),
//! and sets a clipped channel to the mean of the other channels in its
//! neighbourhood plus that chrominance. Chrominance is kept on a coarse grid
//! so a blue sky, a white cloud and a warm lamp each get their own colour.
//! Where two colours clip, both are solved from the recorded third and
//! their chrominances. Where all channels are clipped the area becomes
//! neutral, at least at the sensor's highest clipping level and at the
//! brightness of the rebuilt colours around it, which is what Capture One
//! shows there.
//! Values above the clipping level are kept; the tone pipeline decides how
//! to bring them into the display range (Highlights, roll-off).
//!
//! Per-colour clipping levels are measured from the data, not assumed: the
//! blue channel of Fujifilm sensors saturates visibly below the ADC limit.
//!
//! Inside large partly clipped areas (skies, lit walls) the chrominance
//! beside the clip describes the area poorly: the samples just below the
//! clip are the ones that happened to sit lower than their clipped
//! neighbours, so the rebuilt colour came out too dark and tinted (a purple
//! sunset sky, a redder table), and spreading the grid cell by cell drew
//! blotches with edges. `HighlightRebuild::Ratios` (`renderVersion` 9) also
//! measures, a few pixels away from the clip in locally uniform areas, the
//! ratio of every colour to every other, relaxes both grids smoothly into
//! the cells without samples, and rebuilds a clipped colour from each
//! recorded colour and its ratio, taking over from the beside-the-clip
//! estimate within about 20 pixels of the clip's edge (notes:
//! benchmark-results/hl-partial-clip-2026-10-31/).

use rawler::CFA;
use rayon::prelude::*;

/// Window half-size for the neighbourhood means of clipped samples (5 × 5
/// covers every colour of a Bayer or X-Trans pattern).
const RADIUS: usize = 2;
/// Window half-size for the chrominance candidates: small, so a candidate
/// is compared with colours of the same spot and not with the clipped area
/// behind the edge.
const CANDIDATE_RADIUS: usize = 1;
/// Chrominance grid cell size in mosaic pixels.
const CELL: usize = 64;
/// Samples within this fraction of the per-colour maximum always count as
/// clipped (hard ADC clipping piles up exactly at the maximum).
const CLIP_FRACTION: f32 = 0.996;
/// The saturation onset is searched in this top fraction of the range.
const ONSET_RANGE: f32 = 0.10;
/// Histogram bins over that range (0.2 % of the maximum each).
const ONSET_BINS: usize = 50;
/// A bin belongs to the saturation plateau when it holds this many times
/// the typical bin of the lower half of the range.
const ONSET_RATIO: f32 = 4.0;
/// A colour clips only if its maximum reaches this fraction of the nominal white.
const NOMINAL_FRACTION: f32 = 0.98;
/// Range below the maximum and resolution in which the robust maximum is
/// searched, and the samples a level needs to count as reached.
const ROBUST_RANGE: f32 = 0.2;
const ROBUST_BINS: usize = 4000;
const ROBUST_MIN_SAMPLES: u32 = 32;
/// Minimum number of saturated samples for a colour to count as clipping.
const MIN_CLIPPED_SAMPLES: usize = 64;
/// Chrominance candidates must be at least this bright relative to the clip.
const CANDIDATE_FRACTION: f32 = 0.5;
/// Candidates darker than the other colours around them by more than this
/// ratio are left out: they are mostly samples on the dark side of an edge
/// next to a bright area (branches against a bright sky), whose negative
/// chrominance turned the rebuilt sky magenta (notes: reference/
/// highlight-recovery-gfx.md, G05).
const CANDIDATE_MIN_RATIO: f32 = 0.75;
/// Cells with fewer candidates are filled from their neighbours.
const MIN_CELL_SAMPLES: usize = 8;
/// Block size of the clipped-share map that steers the neutral fill.
const SHARE_BLOCK: usize = 8;
/// Box blur radius of that map in blocks (5 × 5 blocks, 40 px): the move
/// from rebuilt colour to neutral fill spreads over that distance instead
/// of drawing contours along the clipping boundary.
const SHARE_BLUR: usize = 2;
/// Recorded samples this far below their clip level (as a fraction of it)
/// count fully as the dark side of an edge for the neutral fill.
const DARK_EDGE_RANGE: f32 = 0.4;
/// The fill level is measured on every this many pairs of rows.
const LEVEL_ROW_STEP: usize = 4;
/// Unclipped samples within this fraction below their clipping level count
/// partly as clipped in that map.
const NEAR_CLIP: f32 = 0.08;

/// Ratio candidates (`HighlightRebuild::Ratios`) lie within this many
/// pixels of a clipped sample but not next to one.
const RATIO_REACH: usize = 8;
/// A ratio candidate's colours may differ from their means one ring further
/// out by at most this fraction (no edge or texture inside its window).
const RATIO_UNIFORMITY: f32 = 0.08;
/// The ratio estimate takes over where the clipped colour's blurred share
/// (`ShareMap`) rises from this level over the next `RATIO_RAMP` (the clip's
/// edge sits near 0.5), so it meets the recorded colours without a step.
const RATIO_RAMP_START: f32 = 0.5;
const RATIO_RAMP: f32 = 0.4;

/// How clipped colours are rebuilt (the recipe's `renderVersion` decides it,
/// image_loader.rs `decode_variant`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum HighlightRebuild {
    /// From chrominance offsets measured beside the clip (before
    /// `renderVersion` 9).
    #[default]
    Offsets,
    /// Inside larger clipped areas also from colour ratios measured a few
    /// pixels away from the clip (see the module notes).
    Ratios,
}

/// Active-area rectangle: left, top, width, height in mosaic pixels.
pub type Area = (usize, usize, usize, usize);

#[derive(Debug, Clone, PartialEq)]
pub struct ClipLevels {
    /// Clipping threshold per colour index (R, G, B) in data units; `None`
    /// when the colour does not clip in this photograph.
    pub thresholds: [Option<f32>; 3],
}

#[derive(Debug, Clone, PartialEq)]
pub struct Reconstruction {
    pub levels: ClipLevels,
    /// Fraction of active-area samples at or above their clipping level.
    pub clipped_fraction: f32,
    /// Samples whose value was raised.
    pub raised: usize,
    /// Per-colour chrominance used where the grid had no candidates.
    pub global_chrominance: [f32; 3],
}

struct Pattern {
    table: [[u8; 48]; 48],
}

impl Pattern {
    fn new(cfa: &CFA) -> Self {
        let mut table = [[0u8; 48]; 48];
        for (row, line) in table.iter_mut().enumerate() {
            for (col, value) in line.iter_mut().enumerate() {
                *value = cfa.color_at(row, col) as u8;
            }
        }
        Self { table }
    }

    #[inline]
    fn color(&self, row: usize, col: usize) -> usize {
        self.table[row % 48][col % 48] as usize
    }
}

/// Measures which colours clip and at which level. `nominal_white` is the
/// decoder's white level in the same units as `samples`.
pub fn detect_clip_levels<T: Copy + Into<f32> + Send + Sync>(
    samples: &[T],
    width: usize,
    active: Area,
    cfa: &CFA,
    nominal_white: f32,
) -> Option<ClipLevels> {
    let (left, top, aw, ah) = active;
    if width == 0 || aw == 0 || ah == 0 || samples.len() < width * (top + ah) {
        return None;
    }
    let pattern = Pattern::new(cfa);
    // Per colour: maximum, then the count of samples within CLIP_FRACTION of it.
    let maxima = (top..top + ah)
        .into_par_iter()
        .map(|row| {
            let mut local = [f32::MIN; 3];
            let line = &samples[row * width + left..row * width + left + aw];
            for (i, value) in line.iter().enumerate() {
                let color = pattern.color(row, left + i);
                if color < 3 {
                    let v: f32 = (*value).into();
                    if v > local[color] {
                        local[color] = v;
                    }
                }
            }
            local
        })
        .reduce(
            || [f32::MIN; 3],
            |a, b| [a[0].max(b[0]), a[1].max(b[1]), a[2].max(b[2])],
        );
    let mut thresholds = onset_thresholds(samples, width, active, &pattern, maxima, nominal_white);
    // A colour missed because a few stray samples lie above its clip: try
    // again from the level that a group of samples reaches.
    if thresholds.iter().any(Option::is_none) {
        let robust = robust_maxima(samples, width, active, &pattern, maxima);
        if (0..3).any(|c| thresholds[c].is_none() && robust[c] < maxima[c]) {
            let retry = onset_thresholds(samples, width, active, &pattern, robust, nominal_white);
            for c in 0..3 {
                if thresholds[c].is_none() && robust[c] < maxima[c] {
                    thresholds[c] = retry[c];
                }
            }
        }
    }
    if thresholds.iter().all(Option::is_none) {
        return None;
    }
    Some(ClipLevels { thresholds })
}

/// Per colour, the level where the clipped plateau starts, from the
/// histogram of the top ONSET_RANGE below `maxima`.
fn onset_thresholds<T: Copy + Into<f32> + Send + Sync>(
    samples: &[T],
    width: usize,
    active: Area,
    pattern: &Pattern,
    maxima: [f32; 3],
    nominal_white: f32,
) -> [Option<f32>; 3] {
    let (left, top, aw, ah) = active;
    // Histogram of the top ONSET_RANGE of every colour. Sensors that
    // saturate softly (Fujifilm's blue spreads over ~2 % below its maximum)
    // show a plateau far above the density below it; hard clipping shows a
    // spike in the top bin only.
    let lows = [
        maxima[0] * (1.0 - ONSET_RANGE),
        maxima[1] * (1.0 - ONSET_RANGE),
        maxima[2] * (1.0 - ONSET_RANGE),
    ];
    let bin_width = [
        (maxima[0] * ONSET_RANGE / ONSET_BINS as f32).max(f32::MIN_POSITIVE),
        (maxima[1] * ONSET_RANGE / ONSET_BINS as f32).max(f32::MIN_POSITIVE),
        (maxima[2] * ONSET_RANGE / ONSET_BINS as f32).max(f32::MIN_POSITIVE),
    ];
    let histograms = (top..top + ah)
        .into_par_iter()
        .map(|row| {
            let mut local = [[0usize; ONSET_BINS]; 3];
            let line = &samples[row * width + left..row * width + left + aw];
            for (i, value) in line.iter().enumerate() {
                let color = pattern.color(row, left + i);
                if color < 3 {
                    let v: f32 = (*value).into();
                    if v >= lows[color] {
                        let bin =
                            (((v - lows[color]) / bin_width[color]) as usize).min(ONSET_BINS - 1);
                        local[color][bin] += 1;
                    }
                }
            }
            local
        })
        .reduce(
            || [[0usize; ONSET_BINS]; 3],
            |mut a, b| {
                for color in 0..3 {
                    for bin in 0..ONSET_BINS {
                        a[color][bin] += b[color][bin];
                    }
                }
                a
            },
        );
    let mut thresholds = [None; 3];
    for color in 0..3 {
        if maxima[color] < nominal_white * NOMINAL_FRACTION {
            continue;
        }
        let histogram = &histograms[color];
        let mut lower: Vec<usize> = histogram[..ONSET_BINS / 2].to_vec();
        lower.sort_unstable();
        let baseline = lower[lower.len() / 2].max(1) as f32;
        // Walk down from the top bin while the plateau continues.
        let mut onset = ONSET_BINS - 1;
        while onset > 0 && histogram[onset - 1] as f32 >= ONSET_RATIO * baseline {
            onset -= 1;
        }
        let plateau_start = lows[color] + onset as f32 * bin_width[color];
        let threshold = plateau_start.min(maxima[color] * CLIP_FRACTION);
        let count: usize = histogram[onset..].iter().sum();
        if count >= MIN_CLIPPED_SAMPLES
            && histogram[ONSET_BINS - 1] as f32 >= ONSET_RATIO * baseline
        {
            thresholds[color] = Some(threshold);
        }
    }
    thresholds
}

/// The highest level per colour that a group of samples reaches. A few
/// stray samples above the clip level (Panasonic LX15: up to 4351 with the
/// clip at 4095) would otherwise stretch the onset histogram so that the
/// clipping spike falls below its top bin, and the clipped colour was
/// missed (a pink sky).
fn robust_maxima<T: Copy + Into<f32> + Send + Sync>(
    samples: &[T],
    width: usize,
    active: Area,
    pattern: &Pattern,
    maxima: [f32; 3],
) -> [f32; 3] {
    let (left, top, aw, ah) = active;
    let lows = maxima.map(|m| m * (1.0 - ROBUST_RANGE));
    let bin_width = maxima.map(|m| (m * ROBUST_RANGE / ROBUST_BINS as f32).max(f32::MIN_POSITIVE));
    let histograms = (top..top + ah)
        .into_par_iter()
        .map(|row| {
            let mut local = vec![[0u32; 3]; ROBUST_BINS];
            let line = &samples[row * width + left..row * width + left + aw];
            for (i, value) in line.iter().enumerate() {
                let color = pattern.color(row, left + i);
                if color < 3 {
                    let v: f32 = (*value).into();
                    if v >= lows[color] {
                        let bin =
                            (((v - lows[color]) / bin_width[color]) as usize).min(ROBUST_BINS - 1);
                        local[bin][color] += 1;
                    }
                }
            }
            local
        })
        .reduce(
            || vec![[0u32; 3]; ROBUST_BINS],
            |mut a, b| {
                for (x, y) in a.iter_mut().zip(b) {
                    for c in 0..3 {
                        x[c] += y[c];
                    }
                }
                a
            },
        );
    let mut robust = maxima;
    for color in 0..3 {
        if let Some(bin) = (0..ROBUST_BINS)
            .rev()
            .find(|&b| histograms[b][color] >= ROBUST_MIN_SAMPLES)
        {
            robust[color] = lows[color] + (bin + 1) as f32 * bin_width[color];
        }
    }
    robust
}

struct Window {
    sum: [f32; 3],
    count: [u32; 3],
    clipped: [u32; 3],
    /// Sum of the unclipped samples only.
    sum_unclipped: [f32; 3],
}

impl Window {
    /// Mean of the two colours other than `color`, if both are present.
    fn mean_others(&self, color: usize) -> Option<f32> {
        let mut total = 0.0;
        for other in (0..3).filter(|c| *c != color) {
            if self.count[other] == 0 {
                return None;
            }
            total += self.sum[other] / self.count[other] as f32;
        }
        Some(total / 2.0)
    }

    /// The smaller of the means of the two colours other than `color`, if
    /// both are present.
    fn min_other(&self, color: usize) -> Option<f32> {
        let mut low = f32::INFINITY;
        for other in (0..3).filter(|c| *c != color) {
            if self.count[other] == 0 {
                return None;
            }
            low = low.min(self.sum[other] / self.count[other] as f32);
        }
        Some(low)
    }
}

/// The mosaic with its clipped-sample mask, in white-balanced units.
struct Mosaic<'a> {
    data: &'a [f32],
    mask: &'a [u8],
    width: usize,
    active: Area,
    pattern: &'a Pattern,
    wb: [f32; 3],
}

impl Mosaic<'_> {
    /// Means of the colours in the window around (row, col), either over
    /// all samples or over unclipped samples only.
    #[inline]
    fn window(&self, row: usize, col: usize, radius: usize, unclipped_only: bool) -> Window {
        let (left, top, aw, ah) = self.active;
        let r0 = row.saturating_sub(radius).max(top);
        let r1 = (row + radius).min(top + ah - 1);
        let c0 = col.saturating_sub(radius).max(left);
        let c1 = (col + radius).min(left + aw - 1);
        let mut w = Window {
            sum: [0.0; 3],
            count: [0; 3],
            clipped: [0; 3],
            sum_unclipped: [0.0; 3],
        };
        for r in r0..=r1 {
            let base = r * self.width;
            for c in c0..=c1 {
                let color = self.pattern.color(r, c);
                let clipped = self.mask[base + c] != 0;
                if color >= 3 || (unclipped_only && clipped) {
                    continue;
                }
                let value = self.data[base + c] * self.wb[color];
                w.sum[color] += value;
                w.count[color] += 1;
                w.clipped[color] += clipped as u32;
                if !clipped {
                    w.sum_unclipped[color] += value;
                }
            }
        }
        w
    }

    /// Whether nothing clips within two pixels and every colour's mean over
    /// `near` (the candidate's own value for its colour) stays within
    /// RATIO_UNIFORMITY of its mean one ring further out.
    fn uniform_around(
        &self,
        row: usize,
        col: usize,
        color: usize,
        value: f32,
        near: &Window,
    ) -> bool {
        let wide = self.window(row, col, RADIUS, false);
        if wide.clipped.iter().any(|c| *c != 0) {
            return false;
        }
        (0..3).filter(|k| wide.count[*k] > 0).all(|k| {
            let outer = wide.sum[k] / wide.count[k] as f32;
            let inner = if k == color {
                value
            } else if near.count[k] > 0 {
                near.sum[k] / near.count[k] as f32
            } else {
                outer
            };
            (inner / outer.max(1e-12) - 1.0).abs() <= RATIO_UNIFORMITY
        })
    }

    #[inline]
    fn any_clipped_near(&self, row: usize, col: usize) -> bool {
        let (left, top, aw, ah) = self.active;
        let r0 = row.saturating_sub(CANDIDATE_RADIUS).max(top);
        let r1 = (row + CANDIDATE_RADIUS).min(top + ah - 1);
        let c0 = col.saturating_sub(CANDIDATE_RADIUS).max(left);
        let c1 = (col + CANDIDATE_RADIUS).min(left + aw - 1);
        (r0..=r1).any(|r| {
            self.mask[r * self.width + c0..=r * self.width + c1]
                .iter()
                .any(|m| *m != 0)
        })
    }
}

/// Per grid row: its index and the per-cell chrominance sums and counts.
type GridRow = (usize, Vec<[f32; 3]>, Vec<[u32; 3]>);

/// Fills grid cells without enough candidates from their neighbours.
fn fill_grid(sum: &mut [f32], count: &mut [u32], cols: usize, rows: usize, fallback: f32) {
    let mut known: Vec<bool> = count
        .iter()
        .map(|c| *c as usize >= MIN_CELL_SAMPLES)
        .collect();
    let mut value: Vec<f32> = sum
        .iter()
        .zip(count.iter())
        .map(|(s, c)| {
            if *c as usize >= MIN_CELL_SAMPLES {
                s / *c as f32
            } else {
                0.0
            }
        })
        .collect();
    if !known.iter().any(|k| *k) {
        value.iter_mut().for_each(|v| *v = fallback);
        sum.copy_from_slice(&value);
        count.iter_mut().for_each(|c| *c = 1);
        return;
    }
    let max_passes = cols + rows;
    for _ in 0..max_passes {
        let mut next_known = known.clone();
        let mut next_value = value.clone();
        let mut changed = false;
        for y in 0..rows {
            for x in 0..cols {
                let i = y * cols + x;
                if known[i] {
                    continue;
                }
                let mut acc = 0.0;
                let mut n = 0;
                for dy in -1i32..=1 {
                    for dx in -1i32..=1 {
                        let (nx, ny) = (x as i32 + dx, y as i32 + dy);
                        if nx < 0 || ny < 0 || nx >= cols as i32 || ny >= rows as i32 {
                            continue;
                        }
                        let j = ny as usize * cols + nx as usize;
                        if known[j] {
                            acc += value[j];
                            n += 1;
                        }
                    }
                }
                if n > 0 {
                    next_value[i] = acc / n as f32;
                    next_known[i] = true;
                    changed = true;
                }
            }
        }
        known = next_known;
        value = next_value;
        if !changed {
            break;
        }
    }
    sum.copy_from_slice(&value);
    count.iter_mut().for_each(|c| *c = 1);
}

/// Lets the cells without enough samples (`known` false) relax to the mean
/// of their four neighbours, the known ones fixed: smooth membranes instead
/// of `fill_grid`'s plateaus, whose steps showed as blotches with edges in
/// large clipped skies. Starts from `fill_grid`'s values.
fn relax_grid(value: &mut [f32], known: &[bool], cols: usize, rows: usize) {
    if known.iter().all(|k| *k) || !known.iter().any(|k| *k) {
        return;
    }
    let mut next = value.to_vec();
    for _ in 0..4 * (cols + rows) {
        for y in 0..rows {
            for x in 0..cols {
                let i = y * cols + x;
                if known[i] {
                    continue;
                }
                let mut acc = 0.0;
                let mut n = 0.0;
                if x > 0 {
                    acc += value[i - 1];
                    n += 1.0;
                }
                if x + 1 < cols {
                    acc += value[i + 1];
                    n += 1.0;
                }
                if y > 0 {
                    acc += value[i - cols];
                    n += 1.0;
                }
                if y + 1 < rows {
                    acc += value[i + cols];
                    n += 1.0;
                }
                next[i] = acc / n;
            }
        }
        value.copy_from_slice(&next);
    }
}

/// Index of the colour pair (R-G, R-B, G-B) in the ratio grid, and the sign
/// that turns its log ratio (lower index over higher) into `color` over
/// `other`.
fn pair_index(color: usize, other: usize) -> (usize, f32) {
    let index = match (color.min(other), color.max(other)) {
        (0, 1) => 0,
        (0, 2) => 1,
        _ => 2,
    };
    (index, if color < other { 1.0 } else { -1.0 })
}

/// For the rows `rows`: 1 where a clipped sample lies within RATIO_REACH
/// pixels (in both directions), else 0; `rows.len()` lines of `width`.
fn near_clip(mask: &[u8], width: usize, height: usize, rows: std::ops::Range<usize>) -> Vec<u8> {
    let first = rows.start.saturating_sub(RATIO_REACH);
    let last = (rows.end + RATIO_REACH).min(height);
    // Clipped samples within reach along each row of the band.
    let horizontal: Vec<u8> = (first..last)
        .flat_map(|row| {
            let line = &mask[row * width..(row + 1) * width];
            let mut count = line[..RATIO_REACH.min(width)]
                .iter()
                .filter(|m| **m != 0)
                .count();
            (0..width).map(move |col| {
                if col + RATIO_REACH < width && line[col + RATIO_REACH] != 0 {
                    count += 1;
                }
                if col > RATIO_REACH && line[col - RATIO_REACH - 1] != 0 {
                    count -= 1;
                }
                (count > 0) as u8
            })
        })
        .collect();
    let mut near = vec![0u8; rows.len() * width];
    for (i, row) in rows.enumerate() {
        let r0 = row.saturating_sub(RATIO_REACH).max(first) - first;
        let r1 = (row + RATIO_REACH + 1).min(last) - first;
        let out = &mut near[i * width..(i + 1) * width];
        for r in r0..r1 {
            for (o, h) in out.iter_mut().zip(&horizontal[r * width..(r + 1) * width]) {
                *o |= *h;
            }
        }
    }
    near
}

/// Per block of SHARE_BLOCK pixels: samples and clipped samples per colour.
struct BlockCounts {
    cols: usize,
    rows: usize,
    counts: Vec<[[f32; 3]; 2]>,
}

/// Per block of SHARE_BLOCK pixels: the share of clipped samples of every
/// colour, box-blurred over a few blocks.
struct ShareMap {
    cols: usize,
    rows: usize,
    share: Vec<[f32; 3]>,
}

impl ShareMap {
    /// `data` and `thresholds` (data units) give every unclipped sample a
    /// partial share once it is within NEAR_CLIP of its clipping level, so
    /// a colour just below saturation counts almost as clipped.
    fn block_counts(
        data: &[f32],
        mask: &[u8],
        width: usize,
        active: Area,
        pattern: &Pattern,
        thresholds: [f32; 3],
    ) -> BlockCounts {
        let (left, top, aw, ah) = active;
        let cols = aw.div_ceil(SHARE_BLOCK);
        let rows = ah.div_ceil(SHARE_BLOCK);
        let counts: Vec<[[f32; 3]; 2]> = (0..rows * cols)
            .into_par_iter()
            .map(|i| {
                let (by, bx) = (i / cols, i % cols);
                let mut c = [[0f32; 3]; 2];
                for row in top + by * SHARE_BLOCK..(top + (by + 1) * SHARE_BLOCK).min(top + ah) {
                    for col in
                        left + bx * SHARE_BLOCK..(left + (bx + 1) * SHARE_BLOCK).min(left + aw)
                    {
                        let color = pattern.color(row, col);
                        if color < 3 {
                            let index = row * width + col;
                            c[0][color] += 1.0;
                            c[1][color] += if mask[index] != 0 {
                                1.0
                            } else {
                                let x = ((data[index] / thresholds[color] - (1.0 - NEAR_CLIP))
                                    / NEAR_CLIP)
                                    .clamp(0.0, 1.0);
                                x * x * (3.0 - 2.0 * x)
                            };
                        }
                    }
                }
                c
            })
            .collect();
        BlockCounts { cols, rows, counts }
    }

    /// The block shares box-blurred over `radius` blocks.
    fn blurred(blocks: &BlockCounts, radius: usize) -> Self {
        let BlockCounts { cols, rows, counts } = blocks;
        let (cols, rows) = (*cols, *rows);
        let share = (0..rows * cols)
            .into_par_iter()
            .map(|i| {
                let (by, bx) = (i / cols, i % cols);
                let mut total = [0f32; 3];
                let mut clipped = [0f32; 3];
                for y in by.saturating_sub(radius)..(by + radius + 1).min(rows) {
                    for x in bx.saturating_sub(radius)..(bx + radius + 1).min(cols) {
                        let c = &counts[y * cols + x];
                        for k in 0..3 {
                            total[k] += c[0][k];
                            clipped[k] += c[1][k];
                        }
                    }
                }
                [0, 1, 2].map(|k| {
                    if total[k] == 0.0 {
                        0.0
                    } else {
                        clipped[k] / total[k]
                    }
                })
            })
            .collect();
        Self { cols, rows, share }
    }

    /// Bilinear lookup at a position relative to the active area.
    fn at(&self, y: usize, x: usize) -> [f32; 3] {
        let fy = (y as f32 + 0.5) / SHARE_BLOCK as f32 - 0.5;
        let fx = (x as f32 + 0.5) / SHARE_BLOCK as f32 - 0.5;
        let y0 = fy.floor().clamp(0.0, (self.rows - 1) as f32) as usize;
        let x0 = fx.floor().clamp(0.0, (self.cols - 1) as f32) as usize;
        let y1 = (y0 + 1).min(self.rows - 1);
        let x1 = (x0 + 1).min(self.cols - 1);
        let ty = (fy - y0 as f32).clamp(0.0, 1.0);
        let tx = (fx - x0 as f32).clamp(0.0, 1.0);
        let get = |yy: usize, xx: usize| self.share[yy * self.cols + xx];
        let (a, b, c, d) = (get(y0, x0), get(y0, x1), get(y1, x0), get(y1, x1));
        [0, 1, 2].map(|k| {
            let top = a[k] + (b[k] - a[k]) * tx;
            let bottom = c[k] + (d[k] - c[k]) * tx;
            top + (bottom - top) * ty
        })
    }
}

/// Weight of the neutral fill: the share of the least clipped colour, so
/// only areas where all three colours saturate become neutral. Where one
/// colour is still recorded its colour is kept (raising only the clipped
/// colours towards neutral would tint the area with the complement of the
/// recorded one, e.g. cyan where red survives).
fn neutral_weight(share: [f32; 3]) -> f32 {
    smooth(share.iter().copied().fold(1.0f32, f32::min))
}

/// Smoothstep from 0 to 1.
fn smooth(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Bilinear lookup in the chrominance grid (cell centres at CELL / 2), so
/// neighbouring cells with different colours do not draw block edges.
fn chroma_at(
    grid: &[[f32; 3]],
    cols: usize,
    rows: usize,
    row: usize,
    col: usize,
    color: usize,
) -> f32 {
    let fy = (row as f32 + 0.5) / CELL as f32 - 0.5;
    let fx = (col as f32 + 0.5) / CELL as f32 - 0.5;
    let y0 = fy.floor().clamp(0.0, (rows - 1) as f32) as usize;
    let x0 = fx.floor().clamp(0.0, (cols - 1) as f32) as usize;
    let y1 = (y0 + 1).min(rows - 1);
    let x1 = (x0 + 1).min(cols - 1);
    let ty = (fy - y0 as f32).clamp(0.0, 1.0);
    let tx = (fx - x0 as f32).clamp(0.0, 1.0);
    let a = grid[y0 * cols + x0][color];
    let b = grid[y0 * cols + x1][color];
    let c = grid[y1 * cols + x0][color];
    let d = grid[y1 * cols + x1][color];
    let top = a + (b - a) * tx;
    let bottom = c + (d - c) * tx;
    top + (bottom - top) * ty
}

/// Reconstructs clipped samples in place. `data` is the mosaic in the units
/// the thresholds were measured in; `wb` are the white-balance multipliers
/// per colour index; `rebuild` picks the estimate inside larger clipped
/// areas. Returns `None` when nothing clips.
#[allow(clippy::too_many_arguments)]
pub fn reconstruct_clipped_mosaic(
    data: &mut [f32],
    width: usize,
    height: usize,
    active: Area,
    cfa: &CFA,
    wb: [f32; 4],
    levels: &ClipLevels,
    rebuild: HighlightRebuild,
) -> Option<Reconstruction> {
    let (left, top, aw, ah) = active;
    if width == 0 || aw == 0 || ah == 0 || left + aw > width || top + ah > height {
        return None;
    }
    if data.len() < width * height {
        return None;
    }
    let wb = [
        if wb[0] > 0.0 { wb[0] } else { 1.0 },
        if wb[1] > 0.0 { wb[1] } else { 1.0 },
        if wb[2] > 0.0 { wb[2] } else { 1.0 },
    ];
    let pattern = Pattern::new(cfa);
    let thresholds = [
        levels.thresholds[0].unwrap_or(f32::INFINITY),
        levels.thresholds[1].unwrap_or(f32::INFINITY),
        levels.thresholds[2].unwrap_or(f32::INFINITY),
    ];
    // Clipping levels in white-balanced units.
    let clip_wb = [
        thresholds[0] * wb[0],
        thresholds[1] * wb[1],
        thresholds[2] * wb[2],
    ];

    // 1. Clipped-sample mask.
    let mut mask = vec![0u8; width * height];
    let clipped_total: usize = mask
        .par_chunks_mut(width)
        .enumerate()
        .map(|(row, line)| {
            if row < top || row >= top + ah {
                return 0;
            }
            let mut n = 0;
            for col in left..left + aw {
                let color = pattern.color(row, col);
                if color < 3 && data[row * width + col] >= thresholds[color] {
                    line[col] = 1;
                    n += 1;
                }
            }
            n
        })
        .sum();
    if clipped_total == 0 {
        return None;
    }

    // 2. Chrominance candidates on a coarse grid: unclipped, bright samples
    //    next to clipped ones, compared with the unclipped neighbours of the
    //    other colours. With `Ratios` also the log ratios of each colour
    //    pair, from bright samples within RATIO_REACH of the clip but not
    //    next to it, in uniform surroundings.
    let ratios = rebuild == HighlightRebuild::Ratios;
    let grid_cols = width.div_ceil(CELL);
    let grid_rows = height.div_ceil(CELL);
    let mosaic = Mosaic {
        data,
        mask: &mask,
        width,
        active,
        pattern: &pattern,
        wb,
    };
    let accumulators: Vec<(GridRow, GridRow)> = (0..grid_rows)
        .into_par_iter()
        .map(|gy| {
            let mut sums = vec![[0.0f32; 3]; grid_cols];
            let mut counts = vec![[0u32; 3]; grid_cols];
            let mut ratio_sums = vec![[0.0f32; 3]; grid_cols];
            let mut ratio_counts = vec![[0u32; 3]; grid_cols];
            let row_start = (gy * CELL).max(top);
            let row_end = ((gy + 1) * CELL).min(top + ah).max(row_start);
            let near = if ratios {
                near_clip(&mask, width, height, row_start..row_end)
            } else {
                Vec::new()
            };
            for row in row_start..row_end {
                for col in left..left + aw {
                    let index = row * width + col;
                    if mask[index] != 0 {
                        continue;
                    }
                    let color = pattern.color(row, col);
                    if color >= 3 {
                        continue;
                    }
                    let value = data[index] * wb[color];
                    if value < clip_wb[color] * CANDIDATE_FRACTION {
                        continue;
                    }
                    let adjacent = mosaic.any_clipped_near(row, col);
                    let ratio_candidate =
                        ratios && !adjacent && near[(row - row_start) * width + col] != 0;
                    if !adjacent && !ratio_candidate {
                        continue;
                    }
                    let window = mosaic.window(row, col, CANDIDATE_RADIUS, true);
                    let Some(mean_others) = window.mean_others(color) else {
                        continue;
                    };
                    if value < mean_others * CANDIDATE_MIN_RATIO {
                        continue;
                    }
                    let gx = col / CELL;
                    if adjacent {
                        sums[gx][color] += value - mean_others;
                        counts[gx][color] += 1;
                    } else if mosaic.uniform_around(row, col, color, value, &window) {
                        for other in (0..3).filter(|o| *o != color) {
                            let mean = window.sum[other] / window.count[other] as f32;
                            if mean > 0.0 {
                                let (pair, sign) = pair_index(color, other);
                                ratio_sums[gx][pair] += sign * (value / mean).ln();
                                ratio_counts[gx][pair] += 1;
                            }
                        }
                    }
                }
            }
            ((gy, sums, counts), (gy, ratio_sums, ratio_counts))
        })
        .collect();
    let mut grid_sum = vec![[0.0f32; 3]; grid_cols * grid_rows];
    let mut grid_count = vec![[0u32; 3]; grid_cols * grid_rows];
    let mut ratio_sum = vec![[0.0f32; 3]; grid_cols * grid_rows];
    let mut ratio_count = vec![[0u32; 3]; grid_cols * grid_rows];
    for ((gy, sums, counts), (_, r_sums, r_counts)) in accumulators {
        for gx in 0..grid_cols {
            grid_sum[gy * grid_cols + gx] = sums[gx];
            grid_count[gy * grid_cols + gx] = counts[gx];
            ratio_sum[gy * grid_cols + gx] = r_sums[gx];
            ratio_count[gy * grid_cols + gx] = r_counts[gx];
        }
    }
    // Each colour's chrominance grid and, with `Ratios`, each colour pair's
    // log ratio grid (`pair_index`): the mean per cell with enough
    // candidates, carried into the others; one grid per thread.
    let filled = |sums: &[[f32; 3]], counts: &[[u32; 3]], k: usize, relax: bool| {
        let total: f32 = sums.iter().map(|s| s[k]).sum();
        let n: u32 = counts.iter().map(|c| c[k]).sum();
        let mean = if n > 0 { total / n as f32 } else { 0.0 };
        let mut sum: Vec<f32> = sums.iter().map(|s| s[k]).collect();
        let mut count: Vec<u32> = counts.iter().map(|c| c[k]).collect();
        let known: Vec<bool> = count
            .iter()
            .map(|c| *c as usize >= MIN_CELL_SAMPLES)
            .collect();
        fill_grid(&mut sum, &mut count, grid_cols, grid_rows, mean);
        if relax {
            relax_grid(&mut sum, &known, grid_cols, grid_rows);
        }
        (mean, sum)
    };
    let grids: Vec<(f32, Vec<f32>)> = (0..if ratios { 6 } else { 3 })
        .into_par_iter()
        .map(|k| {
            if k < 3 {
                filled(&grid_sum, &grid_count, k, ratios)
            } else {
                filled(&ratio_sum, &ratio_count, k - 3, true)
            }
        })
        .collect();
    let interleave = |grids: &[(f32, Vec<f32>)]| -> Vec<[f32; 3]> {
        (0..grid_cols * grid_rows)
            .map(|i| [grids[0].1[i], grids[1].1[i], grids[2].1[i]])
            .collect()
    };
    let global_chrominance = [grids[0].0, grids[1].0, grids[2].0];
    let chrominance = interleave(&grids[..3]);
    let ratio_grid: Option<Vec<[f32; 3]>> = ratios.then(|| interleave(&grids[3..]));

    // 3. Per clipped sample: the mean of the other colours around it plus
    //    the local chrominance (the opposed estimate), and how far the
    //    neutral fill of fully clipped areas takes over there.
    let neutral_level = clip_wb
        .iter()
        .copied()
        .filter(|level| level.is_finite())
        .fold(0.0f32, f32::max);
    let blocks = ShareMap::block_counts(data, &mask, width, active, &pattern, thresholds);
    let shares = ShareMap::blurred(&blocks, SHARE_BLUR);
    // The opposed estimate of a clipped sample and the weight of the neutral
    // fill there.
    let estimate_parts = |row: usize, col: usize, color: usize| -> Option<(f32, f32, f32)> {
        let w = mosaic.window(row, col, RADIUS, false);
        let mean_others = w.mean_others(color)?;
        // A clipped colour is not the smallest of the three: that
        // would need a magenta (for green) object brighter than the
        // clip in every colour. The other colours' means (clipped
        // ones at their clip level, a lower bound) floor it; a sky
        // whose green and blue clip over recorded red turned pink
        // when the opposed estimate fell below red (D800, G05).
        let floor = w.min_other(color).unwrap_or(0.0);
        let chroma = |c: usize| chroma_at(&chrominance, grid_cols, grid_rows, row, col, c);
        let mut opposed = (mean_others + chroma(color)).max(floor);
        // A second colour clipped here as well: its clip level is
        // only a lower bound, so the mean of the others under-
        // estimates both. Solve both clipped colours from the
        // recorded one and the two chrominances (c - mean(others) =
        // chroma for each): c = recorded + (4 chroma_c + 2 chroma_o)
        // / 3. Near a low sun green and blue clip over a recorded red
        // that already exceeds blue's clip level; the old estimate
        // left blue and green below red there (a pink sky, D800).
        // The weight follows the blurred clipped-share map, so the
        // estimate changes smoothly where the clipping does.
        let share = shares.at(row - top, col - left);
        let (a, b) = ((color + 1) % 3, (color + 2) % 3);
        let (other_clipped, recorded) = if share[a] >= share[b] { (a, b) } else { (b, a) };
        let joint_weight =
            smooth(share[other_clipped]) * smooth(2.0 * (share[other_clipped] - share[recorded]));
        if joint_weight > 0.0 && w.count[recorded] > 0 {
            let level = w.sum[recorded] / w.count[recorded] as f32;
            let joint = level + (4.0 * chroma(color) + 2.0 * chroma(other_clipped)) / 3.0;
            opposed += (joint - opposed).max(0.0) * joint_weight;
        }
        // `Ratios`: the clipped colour from each recorded colour around it
        // and their ratio (colours partly clipped here count less, their
        // unclipped samples being the lower ones), taking over from the
        // estimate above with the clipped share, so the two meet at the
        // clip's edge.
        if let Some(ratio_grid) = &ratio_grid {
            let mut total = 0.0;
            let mut weights = 0.0;
            for other in [a, b] {
                let unclipped = w.count[other] - w.clipped[other];
                if unclipped == 0 {
                    continue;
                }
                let recorded = unclipped as f32 / w.count[other] as f32;
                let weight = recorded * recorded * recorded * recorded;
                let mean = w.sum_unclipped[other] / unclipped as f32;
                let (pair, sign) = pair_index(color, other);
                let log_ratio = chroma_at(ratio_grid, grid_cols, grid_rows, row, col, pair);
                total += weight * mean * (sign * log_ratio).exp();
                weights += weight;
            }
            if weights > 0.0 {
                let take =
                    weights.min(1.0) * smooth((share[color] - RATIO_RAMP_START) / RATIO_RAMP);
                opposed = (opposed + (total / weights - opposed) * take).max(floor);
            }
        }
        // Where every colour is saturated nothing is known about the
        // colour; such areas become neutral (no warm or magenta cast
        // from the colour that clips last), blended smoothly by how
        // many colours clip around. Held back where the other
        // colours are recorded well below their clip right here: at
        // the edge of a dark object inside a white area its red and
        // blue samples are recorded, and raising the clipped green
        // alone drew green rims (LX100). Recorded samples just below
        // their clip (a smooth clipping contour) do not hold it back.
        let hold = [a, b]
            .map(|c| {
                let unclipped = w.count[c] - w.clipped[c];
                if unclipped == 0 {
                    return 0.0;
                }
                let level = w.sum_unclipped[c] / unclipped as f32 / clip_wb[c];
                let darkness = smooth((1.0 - level) / DARK_EDGE_RANGE);
                darkness * smooth(2.0 * unclipped as f32 / w.count[c] as f32)
            })
            .into_iter()
            .fold(0.0f32, f32::max);
        let weight = neutral_weight(share) * (1.0 - hold);
        // The brightest colour here as it stands: samples above a
        // clip threshold keep their value (Sony's linear white sits
        // below the saturation), so the fill must reach it to be
        // neutral (a lilac sky on a ZV-1 frame otherwise).
        let brightest = (0..3)
            .filter(|c| w.count[*c] > 0)
            .map(|c| w.sum[c] / w.count[c] as f32)
            .fold(0.0f32, f32::max);
        Some((opposed, weight, brightest))
    };
    let pattern = &pattern;
    let clipped_samples = |row: usize| {
        let line = &mask[row * width + left..row * width + left + aw];
        line.iter()
            .enumerate()
            .filter_map(move |(offset, clipped)| {
                let col = left + offset;
                let color = pattern.color(row, col);
                (*clipped != 0 && color < 3).then_some((col, color))
            })
    };
    // 4. The level of the neutral fill: at least the highest clipping level,
    //    and at least the brightness of the rebuilt colours around a fully
    //    clipped area (camera (R + 2G + B) / 4 of the partly clipped samples,
    //    per grid cell, carried into the cells without such samples). A
    //    constant level left a rebuilt blue sky brighter than the white area
    //    it surrounds, a dark step along the contour (GFX 50R frame, G05).
    let level_rows: Vec<GridRow> = (0..grid_rows)
        .into_par_iter()
        .map(|gy| {
            let mut sums = vec![[0.0f32; 3]; grid_cols];
            let mut counts = vec![[0u32; 3]; grid_cols];
            // Every fourth pair of rows (both rows of the CFA pattern) is
            // enough for a cell mean and keeps this pass cheap.
            let rows = ((gy * CELL).max(top)..((gy + 1) * CELL).min(top + ah))
                .filter(|row| (row / 2) % LEVEL_ROW_STEP == 0);
            for row in rows {
                for col in left..left + aw {
                    let color = pattern.color(row, col);
                    if color >= 3 {
                        continue;
                    }
                    let index = row * width + col;
                    let value = if mask[index] != 0 {
                        // The fill weight is at most the share map's.
                        if neutral_weight(shares.at(row - top, col - left)) >= 0.5 {
                            continue;
                        }
                        match estimate_parts(row, col, color) {
                            Some((opposed, weight, _)) if weight < 0.5 => opposed,
                            _ => continue,
                        }
                    } else {
                        // Recorded samples count where they border clipped
                        // ones (a colour still recorded beside two clipped).
                        let value = data[index] * wb[color];
                        if value < clip_wb[color] * CANDIDATE_FRACTION
                            || !mosaic.any_clipped_near(row, col)
                        {
                            continue;
                        }
                        value
                    };
                    sums[col / CELL][color] += value;
                    counts[col / CELL][color] += 1;
                }
            }
            (gy, sums, counts)
        })
        .collect();
    let mut level_sum = vec![0.0f32; grid_cols * grid_rows];
    let mut level_count = vec![0u32; grid_cols * grid_rows];
    for (gy, sums, counts) in level_rows {
        for gx in 0..grid_cols {
            let (s, c) = (sums[gx], counts[gx]);
            if c.iter().all(|n| *n as usize >= MIN_CELL_SAMPLES) {
                let mean = |k: usize| s[k] / c[k] as f32;
                level_sum[gy * grid_cols + gx] = (mean(0) + 2.0 * mean(1) + mean(2)) / 4.0;
                level_count[gy * grid_cols + gx] = MIN_CELL_SAMPLES as u32;
            }
        }
    }
    level_sum
        .iter_mut()
        .zip(level_count.iter_mut())
        .for_each(|(s, c)| *s *= *c as f32);
    fill_grid(
        &mut level_sum,
        &mut level_count,
        grid_cols,
        grid_rows,
        neutral_level,
    );
    let fill_level: Vec<[f32; 3]> = level_sum
        .iter()
        .map(|v| [v.max(neutral_level); 3])
        .collect();

    // 5. Raise clipped samples to the opposed estimate blended into the
    //    neutral fill (one pass over the original values; never lowered).
    let updates: Vec<Vec<(usize, f32)>> = (top..top + ah)
        .into_par_iter()
        .map(|row| {
            let mut changes = Vec::new();
            for (col, color) in clipped_samples(row) {
                let Some((opposed, weight, brightest)) = estimate_parts(row, col, color) else {
                    continue;
                };
                let level =
                    chroma_at(&fill_level, grid_cols, grid_rows, row, col, 0).max(brightest);
                // Both ways: a rebuilt colour above the fill level fades
                // into it too, so its chroma does not end at the contour.
                let estimate = opposed + (level - opposed) * weight;
                let index = row * width + col;
                let current = data[index] * wb[color];
                if estimate > current {
                    changes.push((index, estimate / wb[color]));
                }
            }
            changes
        })
        .collect();
    let mut raised = 0;
    for changes in updates {
        for (index, value) in changes {
            data[index] = value;
            raised += 1;
        }
    }
    Some(Reconstruction {
        levels: levels.clone(),
        clipped_fraction: clipped_total as f32 / (aw * ah) as f32,
        raised,
        global_chrominance,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rggb() -> CFA {
        CFA::new("RGGB")
    }

    /// A mosaic of one uniform camera colour with the given white-balanced
    /// RGB, clipped at `clip` (data units) per colour.
    fn mosaic(width: usize, height: usize, rgb_wb: [f32; 3], wb: [f32; 3], clip: f32) -> Vec<f32> {
        let cfa = rggb();
        let mut data = vec![0.0; width * height];
        for row in 0..height {
            for col in 0..width {
                let c = cfa.color_at(row, col);
                let raw = rgb_wb[c] / wb[c];
                data[row * width + col] = raw.min(clip);
            }
        }
        data
    }

    #[test]
    fn unclipped_mosaic_is_left_alone() {
        let (w, h) = (64, 64);
        let wb = [1.7, 1.0, 1.6];
        let data = mosaic(w, h, [0.5, 0.6, 0.7], wb, 1.0);
        let levels = detect_clip_levels(&data, w, (0, 0, w, h), &rggb(), 1.0);
        assert!(levels.is_none());
    }

    #[test]
    fn clipped_green_is_rebuilt_from_red_and_blue() {
        // A blue sky brightening from left to right (chromaticity
        // 0.8276 : 0.9655 : 1.0 after white balance, scale 0.6 → 1.5): green
        // clips at 1.0 on the right, red and blue are still recorded.
        let (w, h) = (256, 64);
        let wb = [1.7, 1.0, 1.6];
        let chroma = [0.8276, 0.9655, 1.0];
        let cfa = rggb();
        let scale = |col: usize| 0.6 + 0.9 * col as f32 / (w - 1) as f32;
        let mut data = vec![0.0; w * h];
        for row in 0..h {
            for col in 0..w {
                let c = cfa.color_at(row, col);
                data[row * w + col] = (chroma[c] * scale(col) / wb[c]).min(1.0);
            }
        }
        let levels = detect_clip_levels(&data, w, (0, 0, w, h), &cfa, 1.0).expect("green clips");
        assert!(levels.thresholds[1].is_some());
        assert!(levels.thresholds[0].is_none() && levels.thresholds[2].is_none());
        let before = data.clone();
        let result = reconstruct_clipped_mosaic(
            &mut data,
            w,
            h,
            (0, 0, w, h),
            &cfa,
            [wb[0], wb[1], wb[2], 0.0],
            &levels,
            HighlightRebuild::Offsets,
        )
        .expect("reconstructed");
        assert!(result.raised > 0);
        // Unclipped samples are untouched, clipped green lands within 5 % of
        // the scene value.
        for row in 4..h - 4 {
            for col in 4..w - 4 {
                let i = row * w + col;
                let c = cfa.color_at(row, col);
                let truth = chroma[c] * scale(col) / wb[c];
                if before[i] < levels.thresholds[1].unwrap() || c != 1 {
                    assert_eq!(data[i], before[i]);
                } else {
                    assert!(
                        (data[i] - truth).abs() < 0.05 * truth,
                        "col {col} green {} truth {truth}",
                        data[i]
                    );
                }
            }
        }
    }

    #[test]
    fn fully_clipped_area_becomes_neutral_at_the_highest_clip() {
        // Everything clipped: nothing is known about the colour, so every
        // channel goes to the highest clipping level (red's 1.7 after white
        // balance) instead of leaving red above green and blue (a warm cast).
        let (w, h) = (64, 64);
        let wb = [1.7, 1.0, 1.6];
        let mut data = mosaic(w, h, [3.0, 3.0, 3.0], wb, 1.0);
        let cfa = rggb();
        let levels = detect_clip_levels(&data, w, (0, 0, w, h), &cfa, 1.0).expect("all clip");
        let result = reconstruct_clipped_mosaic(
            &mut data,
            w,
            h,
            (0, 0, w, h),
            &cfa,
            [wb[0], wb[1], wb[2], 0.0],
            &levels,
            HighlightRebuild::Offsets,
        )
        .expect("reconstructed");
        assert_eq!(result.global_chrominance, [0.0; 3]);
        let red = data[0] * wb[0];
        let green = data[1] * wb[1];
        let blue = data[w + 1] * wb[2];
        assert!((red - 1.7).abs() < 1e-4, "red {red}");
        assert!(
            (green - red).abs() < 0.01 && (blue - red).abs() < 0.01,
            "{red} {green} {blue}"
        );
    }

    #[test]
    fn ratios_rebuild_a_large_clipped_sky_at_its_own_colour() {
        // A sky brightening from left to right at one colour (0.9 : 1.0 :
        // 0.85 after white balance, scale 0.5 → 1.8): green clips over the
        // right two thirds, red and blue stay recorded. Its brightest part
        // is far from the clip's edge, where the offset beside the clip no
        // longer fits (green − mean(red, blue) grows with the brightness).
        let (w, h) = (512, 192);
        let wb = [1.7, 1.0, 1.6];
        let chroma = [0.9, 1.0, 0.85];
        let cfa = rggb();
        let scale = |col: usize| 0.5 + 1.3 * col as f32 / (w - 1) as f32;
        let mut sky = vec![0.0; w * h];
        for row in 0..h {
            for col in 0..w {
                let c = cfa.color_at(row, col);
                sky[row * w + col] = (chroma[c] * scale(col) / wb[c]).min(1.0);
            }
        }
        let levels = detect_clip_levels(&sky, w, (0, 0, w, h), &cfa, 1.0).expect("green clips");
        assert!(levels.thresholds[0].is_none() && levels.thresholds[2].is_none());
        // Mean error of the rebuilt green over the far half of the clipped
        // area, as a fraction of the true green.
        let green_error = |rebuild| {
            let mut data = sky.clone();
            reconstruct_clipped_mosaic(
                &mut data,
                w,
                h,
                (0, 0, w, h),
                &cfa,
                [wb[0], wb[1], wb[2], 0.0],
                &levels,
                rebuild,
            )
            .expect("reconstructed");
            let (mut sum, mut n) = (0.0, 0.0);
            for row in h / 4..3 * h / 4 {
                for col in 2 * w / 3..w - 16 {
                    if cfa.color_at(row, col) == 1 {
                        sum += data[row * w + col] * wb[1] / (chroma[1] * scale(col)) - 1.0;
                        n += 1.0;
                    }
                }
            }
            sum / n
        };
        let offsets = green_error(HighlightRebuild::Offsets);
        let ratios = green_error(HighlightRebuild::Ratios);
        assert!(ratios.abs() < 0.01, "ratios {ratios}");
        assert!(offsets < -0.03, "offsets {offsets}");
    }

    #[test]
    fn ratios_leave_unclipped_samples_alone() {
        let (w, h) = (128, 128);
        let wb = [1.7, 1.0, 1.6];
        let cfa = rggb();
        // A bright disc whose green clips, on a darker recorded ground.
        let mut data = vec![0.0; w * h];
        for row in 0..h {
            for col in 0..w {
                let (dy, dx) = (row as f32 - 64.0, col as f32 - 64.0);
                let level: f32 = if dy * dy + dx * dx < 900.0 { 1.6 } else { 0.4 };
                let c = cfa.color_at(row, col);
                data[row * w + col] = ([0.8, 1.0, 0.7][c] * level / wb[c]).min(1.0);
            }
        }
        let before = data.clone();
        let levels = detect_clip_levels(&data, w, (0, 0, w, h), &cfa, 1.0).expect("green clips");
        reconstruct_clipped_mosaic(
            &mut data,
            w,
            h,
            (0, 0, w, h),
            &cfa,
            [wb[0], wb[1], wb[2], 0.0],
            &levels,
            HighlightRebuild::Ratios,
        )
        .expect("reconstructed");
        for (i, (a, b)) in before.iter().zip(&data).enumerate() {
            if *a < 1.0 {
                assert_eq!(a, b, "sample {i}");
            } else {
                assert!(b >= a, "sample {i}");
            }
        }
    }

    #[test]
    fn soft_saturation_plateau_sets_the_threshold() {
        // Blue saturates softly between 98 % and 100 % of its maximum (as on
        // Fujifilm sensors); red and green are a smooth ramp below white.
        let (w, h) = (256, 256);
        let cfa = rggb();
        let mut data = vec![0.0f32; w * h];
        let mut state = 12345u32;
        for row in 0..h {
            for col in 0..w {
                state = state.wrapping_mul(1664525).wrapping_add(1013904223);
                let noise = (state >> 8) as f32 / (1u32 << 24) as f32;
                let i = row * w + col;
                data[i] = match cfa.color_at(row, col) {
                    2 if col >= w / 2 => 0.98 + 0.02 * noise,
                    _ => 0.5 * noise + 0.2,
                };
            }
        }
        let levels = detect_clip_levels(&data, w, (0, 0, w, h), &cfa, 1.0).expect("blue clips");
        assert!(levels.thresholds[0].is_none() && levels.thresholds[1].is_none());
        let blue = levels.thresholds[2].unwrap();
        assert!((0.975..=0.982).contains(&blue), "blue threshold {blue}");
    }

    /// Reconstructs `data` (clip levels detected from it, nominal white 1.0).
    fn rebuild(data: &mut [f32], w: usize, h: usize, wb: [f32; 3]) -> Reconstruction {
        let cfa = rggb();
        let levels = detect_clip_levels(data, w, (0, 0, w, h), &cfa, 1.0).expect("clips");
        reconstruct_clipped_mosaic(
            data,
            w,
            h,
            (0, 0, w, h),
            &cfa,
            [wb[0], wb[1], wb[2], 0.0],
            &levels,
            HighlightRebuild::Offsets,
        )
        .expect("reconstructed")
    }

    /// Mean white-balanced R, G, B of the 2 × 2 cells in a rectangle.
    fn cell_means(
        data: &[f32],
        w: usize,
        wb: [f32; 3],
        rows: std::ops::Range<usize>,
        cols: std::ops::Range<usize>,
    ) -> [f32; 3] {
        let cfa = rggb();
        let mut sum = [0.0f32; 3];
        let mut n = [0u32; 3];
        for row in rows {
            for col in cols.clone() {
                let c = cfa.color_at(row, col);
                sum[c] += data[row * w + col] * wb[c];
                n[c] += 1;
            }
        }
        [0, 1, 2].map(|c| sum[c] / n[c] as f32)
    }

    #[test]
    fn two_clipped_colours_follow_the_recorded_one() {
        // A neutral area brightening from left to right: green clips first
        // (multiplier 1.0), then blue (1.4); red (2.0) is recorded all the
        // way. Where green and blue clip over a red that is already above
        // blue's clip level, both are rebuilt from red: the area stays
        // neutral instead of keeping blue at its clip level below red (a
        // pink sky near a low sun).
        let (w, h) = (256, 64);
        let wb = [2.0, 1.0, 1.4];
        let cfa = rggb();
        let scale = |col: usize| 0.6 + 1.2 * col as f32 / (w - 1) as f32;
        let mut data = vec![0.0; w * h];
        for row in 0..h {
            for col in 0..w {
                let c = cfa.color_at(row, col);
                data[row * w + col] = (scale(col) / wb[c]).min(1.0);
            }
        }
        rebuild(&mut data, w, h, wb);
        // Columns where the scene is at 1.6–1.75: green and blue clipped.
        let [r, g, b] = cell_means(&data, w, wb, 8..h - 8, 214..238);
        assert!(r > 1.5 && r < 1.8, "red {r}");
        assert!(
            (g - r).abs() < 0.05 * r && (b - r).abs() < 0.05 * r,
            "{r} {g} {b}"
        );
    }

    #[test]
    fn dark_object_in_a_white_area_gets_no_green_rim() {
        // A fully clipped white area beside a less bright one where only
        // green clips (red and blue recorded at 60 and 80 % of their clips),
        // as around a darker cloud in a bright sky. The neutral fill fades
        // in over the share map's width; it must not lift green alone on
        // the recorded side (green rims, LX100).
        let (w, h) = (128, 128);
        let wb = [2.0, 1.0, 1.5];
        let cfa = rggb();
        let mut data = vec![0.0; w * h];
        for row in 0..h {
            for col in 0..w {
                let c = cfa.color_at(row, col);
                let scene: f32 = if col < 64 { 3.0 } else { 1.2 };
                data[row * w + col] = (scene / wb[c]).min(1.0);
            }
        }
        rebuild(&mut data, w, h, wb);
        for col in (66..96).step_by(2) {
            let [r, g, b] = cell_means(&data, w, wb, 16..h - 16, col..col + 2);
            assert!(
                g <= 1.1 * r.max(b),
                "green rim at column {col}: {r} {g} {b}"
            );
        }
        // Away from the bar the area is neutral at the highest clip.
        let [r, g, b] = cell_means(&data, w, wb, 16..h - 16, 16..40);
        assert!(
            (g - r).abs() < 0.02 * r && (b - r).abs() < 0.02 * r,
            "{r} {g} {b}"
        );
    }

    #[test]
    fn neutral_fill_reaches_samples_above_their_threshold() {
        // A threshold below the samples' saturation (Sony's linear white):
        // red keeps its value above it, so the fill lifts green and blue to
        // red's value, not just to red's threshold.
        let (w, h) = (64, 64);
        let wb = [2.0, 1.0, 1.5];
        let cfa = rggb();
        let mut data = vec![0.0; w * h];
        for row in 0..h {
            for col in 0..w {
                data[row * w + col] = match cfa.color_at(row, col) {
                    0 => 1.0,
                    _ => 0.94,
                };
            }
        }
        let levels = ClipLevels {
            thresholds: [Some(0.94), Some(0.94), Some(0.94)],
        };
        reconstruct_clipped_mosaic(
            &mut data,
            w,
            h,
            (0, 0, w, h),
            &cfa,
            [wb[0], wb[1], wb[2], 0.0],
            &levels,
            HighlightRebuild::Offsets,
        )
        .expect("reconstructed");
        let [r, g, b] = cell_means(&data, w, wb, 8..h - 8, 8..w - 8);
        assert!((r - 2.0).abs() < 1e-4, "red {r}");
        assert!(
            (g - r).abs() < 0.01 * r && (b - r).abs() < 0.01 * r,
            "{r} {g} {b}"
        );
    }
}
