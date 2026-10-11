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
//! Where all channels are clipped the estimate lands near the sensor's
//! clipping level and near neutral, which is what Capture One shows there.
//! Values above the clipping level are kept; the tone pipeline decides how
//! to bring them into the display range (Highlights, roll-off).
//!
//! Per-colour clipping levels are measured from the data, not assumed: the
//! blue channel of Fujifilm sensors saturates visibly below the ADC limit.

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
/// Cells with fewer candidates are filled from their neighbours.
const MIN_CELL_SAMPLES: usize = 8;
/// Block size of the clipped-share map that steers the neutral fill.
const SHARE_BLOCK: usize = 8;
/// Box blur radius of that map in blocks (5 × 5 blocks, 40 px): the move
/// from rebuilt colour to neutral fill spreads over that distance instead
/// of drawing contours along the clipping boundary.
const SHARE_BLUR: usize = 2;
/// Unclipped samples within this fraction below their clipping level count
/// partly as clipped in that map.
const NEAR_CLIP: f32 = 0.08;

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
        };
        for r in r0..=r1 {
            let base = r * self.width;
            for c in c0..=c1 {
                let color = self.pattern.color(r, c);
                let clipped = self.mask[base + c] != 0;
                if color >= 3 || (unclipped_only && clipped) {
                    continue;
                }
                w.sum[color] += self.data[base + c] * self.wb[color];
                w.count[color] += 1;
                w.clipped[color] += clipped as u32;
            }
        }
        w
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

/// Per block of SHARE_BLOCK pixels: the share of clipped samples of every
/// colour, box-blurred over SHARE_BLUR blocks.
struct ShareMap {
    cols: usize,
    rows: usize,
    share: Vec<[f32; 3]>,
}

impl ShareMap {
    /// `data` and `thresholds` (data units) give every unclipped sample a
    /// partial share once it is within NEAR_CLIP of its clipping level, so
    /// a colour just below saturation counts almost as clipped.
    fn build(
        data: &[f32],
        mask: &[u8],
        width: usize,
        active: Area,
        pattern: &Pattern,
        thresholds: [f32; 3],
    ) -> Self {
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
        let share = (0..rows * cols)
            .into_par_iter()
            .map(|i| {
                let (by, bx) = (i / cols, i % cols);
                let mut total = [0f32; 3];
                let mut clipped = [0f32; 3];
                for y in by.saturating_sub(SHARE_BLUR)..(by + SHARE_BLUR + 1).min(rows) {
                    for x in bx.saturating_sub(SHARE_BLUR)..(bx + SHARE_BLUR + 1).min(cols) {
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
    let t = share.iter().copied().fold(1.0f32, f32::min).clamp(0.0, 1.0);
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
/// per colour index. Returns `None` when nothing clips.
pub fn reconstruct_clipped_mosaic(
    data: &mut [f32],
    width: usize,
    height: usize,
    active: Area,
    cfa: &CFA,
    wb: [f32; 4],
    levels: &ClipLevels,
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
    //    other colours.
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
    let accumulators: Vec<GridRow> = (0..grid_rows)
        .into_par_iter()
        .map(|gy| {
            let mut sums = vec![[0.0f32; 3]; grid_cols];
            let mut counts = vec![[0u32; 3]; grid_cols];
            let row_start = (gy * CELL).max(top);
            let row_end = ((gy + 1) * CELL).min(top + ah);
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
                    if !mosaic.any_clipped_near(row, col) {
                        continue;
                    }
                    let Some(mean_others) = mosaic
                        .window(row, col, CANDIDATE_RADIUS, true)
                        .mean_others(color)
                    else {
                        continue;
                    };
                    let gx = col / CELL;
                    sums[gx][color] += value - mean_others;
                    counts[gx][color] += 1;
                }
            }
            (gy, sums, counts)
        })
        .collect();
    let mut grid_sum = vec![[0.0f32; 3]; grid_cols * grid_rows];
    let mut grid_count = vec![[0u32; 3]; grid_cols * grid_rows];
    for (gy, sums, counts) in accumulators {
        for gx in 0..grid_cols {
            grid_sum[gy * grid_cols + gx] = sums[gx];
            grid_count[gy * grid_cols + gx] = counts[gx];
        }
    }
    let mut global_chrominance = [0.0f32; 3];
    let mut chrominance = vec![[0.0f32; 3]; grid_cols * grid_rows];
    for color in 0..3 {
        let total: f32 = grid_sum.iter().map(|s| s[color]).sum();
        let n: u32 = grid_count.iter().map(|c| c[color]).sum();
        global_chrominance[color] = if n > 0 { total / n as f32 } else { 0.0 };
        let mut sum: Vec<f32> = grid_sum.iter().map(|s| s[color]).collect();
        let mut count: Vec<u32> = grid_count.iter().map(|c| c[color]).collect();
        fill_grid(
            &mut sum,
            &mut count,
            grid_cols,
            grid_rows,
            global_chrominance[color],
        );
        for (cell, value) in chrominance.iter_mut().zip(sum.iter()) {
            cell[color] = *value;
        }
    }

    // 3. Raise clipped samples to the mean of the other colours around them
    //    plus the local chrominance (one pass over the original values).
    let neutral_level = clip_wb
        .iter()
        .copied()
        .filter(|level| level.is_finite())
        .fold(0.0f32, f32::max);
    let shares = ShareMap::build(data, &mask, width, active, &pattern, thresholds);
    let updates: Vec<Vec<(usize, f32)>> = (top..top + ah)
        .into_par_iter()
        .map(|row| {
            let mut changes = Vec::new();
            let line = &mask[row * width + left..row * width + left + aw];
            for (offset, clipped) in line.iter().enumerate() {
                if *clipped == 0 {
                    continue;
                }
                let col = left + offset;
                let color = pattern.color(row, col);
                if color >= 3 {
                    continue;
                }
                let w = mosaic.window(row, col, RADIUS, false);
                let Some(mean_others) = w.mean_others(color) else {
                    continue;
                };
                let opposed =
                    mean_others + chroma_at(&chrominance, grid_cols, grid_rows, row, col, color);
                // Where every colour is saturated nothing is known about the
                // colour; such areas become neutral at the highest clipping
                // level (no warm or magenta cast from the colour that clips
                // last), blended smoothly by how many colours clip around.
                let weight = neutral_weight(shares.at(row - top, col - left));
                let estimate = opposed + (neutral_level - opposed).max(0.0) * weight;
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
}
