//! Hot and dead pixel suppression on the CFA mosaic, before highlight
//! reconstruction and demosaicing, always on (Camera Raw does this too;
//! Capture One offers it as Single Pixel noise reduction).
//!
//! A sample is defective when it stands out from every sample of its own
//! colour within two pixels and, for hot pixels, also from its adjacent
//! samples of the other colours (white-balanced): a real point of light is
//! spread by the lens over neighbouring photosites of every colour, a hot
//! photosite is not. Defective samples take the median of their same-colour
//! neighbours. Measured on an ILCE-7M4 copy with 550 injected defects
//! (notes: reference/hot-pixels.md).
//!
//! The first dead test compared a sample with its own colour only, so the
//! red and blue samples on a thin dark line (slats, mesh, wires against the
//! sky) were "repaired" to the brighter samples beside the line: magenta and
//! green stripes on X-Trans files, whose same-colour neighbours lie off a
//! 1–3 px line. `DeadTest::Adjacent` (`renderVersion` 3) also asks the
//! adjacent samples of the other colours, as the hot test does: a dark line
//! darkens them too, a dead photosite does not (notes:
//! reference/xtrans-fine-patterns.md).
//!
//! Both ratio tests still took the cores of sharp stars and the samples of
//! thin dark lines on Bayer files for defects. `DeadTest::Isolated`
//! (`renderVersion` 15) is the isolation test of RapidRAW v1.7.1
//! (`src-tauri/src/decode/hot_pixels.rs`, commit `63b415758`, Copyright
//! Timon Käch, AGPL-3.0-or-later), on this module's row prefilter: a sample
//! is defective only when it stands out from every same-colour neighbour by
//! more than their own spread, and its deviation from its neighbourhood's
//! median is far larger than the summed deviations of all 24 samples around
//! it, which a star's or a line's samples share with it (notes:
//! benchmark-results/hot1-2026-11-22/).
//!
//! That test needs a dead sample to lie below its darkest same-colour
//! neighbour by more than the neighbours' spread, so it left dead
//! photosites in busy texture about twice as often as the ratio tests.
//! `DeadTest::IsolatedTexture` (`renderVersion` 16) also repairs a sample
//! the isolation test leaves when it is darker than 0.4 of every
//! same-colour neighbour, still deviates from its neighbourhood's median by
//! more than 0.4 of the summed deviations around it, and no other sample
//! within two pixels is dark against its own colour, as the samples of a
//! dark line or a gap between bright points are (notes:
//! benchmark-results/hot2-2026-11-23/).
//!
//! Dead photosites in dark, busy texture still escaped that rule.
//! `DeadTest::IsolatedBlack` (`renderVersion` 17) also repairs a sample at
//! black, darker than 0.1 of every same-colour neighbour, when no sample
//! within two pixels is dark against its own colour, but only where the
//! darkest same-colour neighbour stands four times the photo's own noise
//! above the sample, so the black-clipped noise of very high ISOs stays as
//! it is; elsewhere the texture rule decides as before. A dead sample takes the green around it times
//! the median ratio of its same-colour neighbours to the green around
//! them, within their range: in texture the colour ratio varies far less
//! than the level (notes: benchmark-results/hot3-2026-11-23/).

use rawler::CFA;
use rayon::prelude::*;

/// Hot: brighter than this many times the brightest same-colour neighbour
/// and than this many times the brightest adjacent other colour.
const HOT_RATIO: f32 = 2.0;
const HOT_NEIGHBOUR_RATIO: f32 = 1.6;
/// Dead: darker than this share of the darkest same-colour neighbour
/// and (`DeadTest::Adjacent`) than this share of the darkest adjacent
/// other colour.
const DEAD_RATIO: f32 = 0.4;
const DEAD_NEIGHBOUR_RATIO: f32 = 0.6;
/// Margins in units of the sensor range (black 0, white 1).
const HOT_MARGIN: f32 = 0.01;
const DEAD_MARGIN: f32 = 0.005;
const DEAD_FLOOR: f32 = 0.02;
/// `DeadTest::Isolated` (RapidRAW v1.7.1's constants): a defect deviates
/// from the median of its neighbourhood by more than `ISOLATION` times the
/// summed deviations of the 24 samples around it; it stands out from its
/// same-colour neighbours by more than their spread, `CONTRAST` of the
/// nearest one and `ISOLATED_FLOOR` of the range; its adjacent samples
/// together deviate the same way by at most `SPILL` of its deviation; a
/// bright one has no adjacent sample at `CLIP` of the range or above.
const ISOLATION: f32 = 21.0 / 24.0;
const ISOLATED_FLOOR: f32 = 1.0 / 256.0;
const CONTRAST: f32 = 0.25;
const SPILL: f32 = 0.25;
const CLIP: f32 = 0.95;
/// `DeadTest::IsolatedTexture`, for a sample darker than `DEAD_RATIO` of
/// every same-colour neighbour, the darkest at least `ISOLATED_FLOOR`: it
/// deviates from the median of its neighbourhood by more than
/// `TEXTURE_ISOLATION` times the summed deviations of the 24 samples
/// around it, its adjacent samples together deviate the same way by at
/// most `TEXTURE_SPILL` times its deviation, and no sample around it lies
/// below `LINE_GUARD` of the median of its own same-colour neighbours.
const TEXTURE_ISOLATION: f32 = 0.4;
const TEXTURE_SPILL: f32 = 1.0;
const LINE_GUARD: f32 = 0.6;
/// `DeadTest::IsolatedBlack`: a sample darker than `BLACK_RATIO` of every
/// same-colour neighbour needs only the line guard where the darkest
/// same-colour neighbour lies more than `NOISE_MARGIN` times the noise at
/// its level above it; elsewhere the texture rule decides.
const BLACK_RATIO: f32 = 0.1;
const NOISE_MARGIN: f32 = 4.0;
/// The noise is measured on every `NOISE_ROW_STEP`th row (a prime, so
/// every row of a 2-, 6- or 12-row pattern takes its turn), per colour and
/// half octave of level (band 0 holds every level at or below
/// 2^-`NOISE_DARKEST`), from bands with at least `NOISE_MIN_PAIRS` pairs.
const NOISE_ROW_STEP: usize = 29;
const NOISE_DARKEST: i32 = 12;
const NOISE_BANDS: usize = 25;
const NOISE_MIN_PAIRS: u32 = 256;
/// Size bins of the pairs' differences: 2^`NOISE_BIN_BITS` per octave from
/// 2^-`NOISE_BIN_FLOOR`.
const NOISE_BIN_BITS: u32 = 3;
const NOISE_BIN_FLOOR: i32 = 20;
const NOISE_BINS: usize = 161;
/// The lower quartile of |a − b| for two samples with noise σ, over σ.
const QUARTILE_OF_DIFFERENCE: f32 = 0.4506;
/// The 24 offsets (dy, dx) around a sample within two pixels, row by row.
const WINDOW: [(isize, isize); 24] = {
    let mut window = [(0, 0); 24];
    let mut i = 0;
    let mut k = 0;
    while k < 25 {
        if k != 12 {
            window[i] = (k / 5 - 2, k % 5 - 2);
            i += 1;
        }
        k += 1;
    }
    window
};
/// Rows of the mosaic one task tests (the rows of levels it converts are
/// this plus four).
const HOT_BAND: usize = 32;

/// Which test finds dead photosites.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum DeadTest {
    /// Darker than every same-colour sample within two pixels (saved edits
    /// before `renderVersion` 3).
    #[default]
    SameColour,
    /// Also darker than every adjacent sample of the other colours
    /// (white-balanced), so thin dark lines keep their colour.
    Adjacent,
    /// Hot and dead alike: isolated from the 5 × 5 neighbourhood around it
    /// (RapidRAW v1.7.1's test, `renderVersion` 15), so the cores of sharp
    /// stars and thin lines keep their samples.
    Isolated,
    /// `Isolated`, and dead photosites in busy texture whose neighbours
    /// spread too widely for it (`renderVersion` 16).
    IsolatedTexture,
    /// `IsolatedTexture`, and dead photosites at black in dark texture
    /// where their neighbours stand clear of the photo's noise; dead
    /// samples take their neighbours' colour ratio (`renderVersion` 17).
    IsolatedBlack,
}

impl DeadTest {
    /// Samples from the active area's edge the test cannot reach: the
    /// isolation test takes medians around every sample within two pixels.
    fn margin(self) -> usize {
        if self.isolated() { 4 } else { 2 }
    }

    /// Whether this is the isolation test (with or without the texture
    /// rule for dead photosites).
    fn isolated(self) -> bool {
        matches!(
            self,
            DeadTest::Isolated | DeadTest::IsolatedTexture | DeadTest::IsolatedBlack
        )
    }

    /// Whether the isolation test also repairs dead samples in texture.
    fn texture(self) -> bool {
        matches!(self, DeadTest::IsolatedTexture | DeadTest::IsolatedBlack)
    }
}

/// The level a dead sample at (`row`, `col`) takes under
/// `DeadTest::IsolatedBlack`: the mean of the green samples in its 3 × 3
/// neighbourhood for a green one; for another colour that mean times the
/// median ratio of its same-colour neighbours (`same`) to the green mean
/// around each of them; within the neighbours' range. None (the median
/// then) without green around a sample, or with green below
/// `ISOLATED_FLOOR`.
fn colour_ratio_repair(
    cfa: &CFA,
    same: &[(isize, isize)],
    row: usize,
    col: usize,
    at: impl Fn(usize, usize) -> f32,
) -> Option<f32> {
    let green = |r: usize, c: usize| -> Option<f32> {
        let (mut sum, mut n) = (0.0f32, 0u32);
        for dy in -1isize..=1 {
            for dx in -1isize..=1 {
                let (y, x) = ((r as isize + dy) as usize, (c as isize + dx) as usize);
                if (dy, dx) != (0, 0) && cfa.color_at(y, x) == 1 {
                    sum += at(y, x);
                    n += 1;
                }
            }
        }
        (n > 0).then(|| sum / n as f32)
    };
    let (mut lo, mut hi) = (f32::INFINITY, f32::NEG_INFINITY);
    let mut ratios = [0f32; 24];
    let mut n = 0;
    let colour = cfa.color_at(row, col);
    for &(dy, dx) in same {
        let (r, c) = ((row as isize + dy) as usize, (col as isize + dx) as usize);
        let s = at(r, c);
        lo = lo.min(s);
        hi = hi.max(s);
        if colour != 1 {
            let g = green(r, c)?;
            if g < ISOLATED_FLOOR {
                return None;
            }
            ratios[n] = s / g;
            n += 1;
        }
    }
    let g = green(row, col)?;
    let value = if colour == 1 {
        g
    } else {
        if g < ISOLATED_FLOOR || n == 0 {
            return None;
        }
        g * middle(&mut ratios[..n])
    };
    // Not-a-number neighbours (a float mosaic) leave no range to keep.
    (lo <= hi && value.is_finite()).then(|| value.clamp(lo, hi))
}

/// The noise of each colour per half octave of level, measured on the photo
/// itself: the lower quartile of the differences between each sample and
/// the nearest same-colour sample to its right (at most two columns away)
/// on every `NOISE_ROW_STEP`th row. Flat areas set the lower quartile;
/// texture only raises it, which leaves more to the texture rule.
struct NoiseBands {
    sigma: [[Option<f32>; NOISE_BANDS]; 3],
}

/// Octave and its first `bits` mantissa bits of a positive level: the
/// level lies in [(1 + k / 2^bits) · 2^octave, (1 + (k + 1) / 2^bits) ·
/// 2^octave).
fn octave_steps(level: f32, bits: u32) -> (i32, u32) {
    let b = level.to_bits();
    (
        ((b >> 23) & 0xff) as i32 - 127,
        (b >> (23 - bits)) & ((1 << bits) - 1),
    )
}

impl NoiseBands {
    /// Band 0: at or below 2^-`NOISE_DARKEST`; then two bands per octave.
    fn band(level: f32) -> usize {
        if level <= (-NOISE_DARKEST as f32).exp2() {
            return 0;
        }
        let (octave, half) = octave_steps(level, 1);
        ((octave + NOISE_DARKEST) * 2 + half as i32 + 1).clamp(1, NOISE_BANDS as i32 - 1) as usize
    }

    /// Bin `NOISE_BIN_STEPS` · (octave + `NOISE_BIN_FLOOR`) + the first
    /// mantissa bits of a difference; 0 for none or a tiny one.
    fn bin(d: f32) -> usize {
        if d <= 0.0 {
            return 0;
        }
        let (octave, steps) = octave_steps(d, NOISE_BIN_BITS);
        ((octave + NOISE_BIN_FLOOR) * (1 << NOISE_BIN_BITS) + steps as i32)
            .clamp(0, NOISE_BINS as i32 - 1) as usize
    }

    fn measure<T: Copy + Send + Sync>(
        source: &[T],
        width: usize,
        active: (usize, usize, usize, usize),
        cfa: &CFA,
        level: impl Fn(T) -> f32 + Sync,
    ) -> Self {
        let (left, top, aw, ah) = active;
        // Per phase of the pattern: the colour and the column step to the
        // pair's other sample.
        let (rows_p, cols_p) = (cfa.height.max(1), cfa.width.max(1));
        let pairs: Vec<Option<(usize, usize)>> = (0..rows_p * cols_p)
            .map(|k| {
                let (row, col) = (k / cols_p + rows_p, k % cols_p + cols_p);
                let colour = cfa.color_at(row, col);
                (colour < 3)
                    .then(|| (1..=2).find(|&dx| cfa.color_at(row, col + dx) == colour))
                    .flatten()
                    .map(|dx| (colour, dx))
            })
            .collect();
        let rows: Vec<usize> = (top..top + ah).step_by(NOISE_ROW_STEP).collect();
        let empty = || vec![[[0u32; NOISE_BINS]; NOISE_BANDS]; 3];
        // One histogram per thread: each is 48 kB to clear and to add.
        let chunk = rows.len().div_ceil(rayon::current_num_threads()).max(1);
        let counts = rows
            .par_chunks(chunk)
            .map(|rows| {
                let mut counts = empty();
                for &row in rows {
                    let phases = &pairs[(row % rows_p) * cols_p..][..cols_p];
                    let line = &source[row * width..][..left + aw];
                    for col in left..(left + aw).saturating_sub(2) {
                        let Some((colour, dx)) = phases[col % cols_p] else {
                            continue;
                        };
                        let (a, b) = (level(line[col]), level(line[col + dx]));
                        counts[colour][Self::band(0.5 * (a + b))][Self::bin((a - b).abs())] += 1;
                    }
                }
                counts
            })
            .reduce(empty, |mut a, b| {
                for (x, y) in a.iter_mut().zip(&b) {
                    for (x, y) in x.iter_mut().zip(y) {
                        for (x, y) in x.iter_mut().zip(y) {
                            *x += y;
                        }
                    }
                }
                a
            });
        let sigma = std::array::from_fn(|k| {
            std::array::from_fn(|band| {
                let bins = &counts[k][band];
                let total: u32 = bins.iter().sum();
                if total < NOISE_MIN_PAIRS {
                    return None;
                }
                let mut seen = 0;
                let bin = bins.iter().position(|&n| {
                    seen += n;
                    4 * seen >= total
                })? as i32;
                // The bin's middle.
                let steps = 1 << NOISE_BIN_BITS;
                let octave = bin / steps - NOISE_BIN_FLOOR;
                let middle = 1.0 + ((bin % steps) as f32 + 0.5) / steps as f32;
                Some(middle * (octave as f32).exp2() / QUARTILE_OF_DIFFERENCE)
            })
        });
        Self { sigma }
    }

    /// The noise of `colour` at `level`: its band's, or the nearest
    /// brighter band's when its own has too few pairs; None when no band
    /// at or above has enough.
    fn at(&self, colour: usize, level: f32) -> Option<f32> {
        self.sigma
            .get(colour)?
            .iter()
            .skip(Self::band(level))
            .find_map(|s| *s)
    }
}

/// The median of `values` (sorted in place), the mean of the middle two for
/// an even count.
fn middle(values: &mut [f32]) -> f32 {
    values.sort_unstable_by(f32::total_cmp);
    let half = values.len() / 2;
    if values.len().is_multiple_of(2) {
        0.5 * (values[half - 1] + values[half])
    } else {
        values[half]
    }
}

/// Whether a sample of level `v` whose same-colour neighbours span
/// `lo..=hi` stands out from them by more than their spread: `Some(true)`
/// hot, `Some(false)` dead. Needs only the extremes, so the row prefilter
/// finds exactly these samples.
fn isolated_candidate(v: f32, lo: f32, hi: f32) -> Option<bool> {
    let spread = ISOLATED_FLOOR.max(hi - lo);
    if v - hi > spread.max(hi * CONTRAST) {
        Some(true)
    } else if lo - v > spread.max(lo * CONTRAST) {
        Some(false)
    } else {
        None
    }
}

/// Same-colour offsets within two pixels and adjacent other-colour offsets,
/// per phase of the CFA pattern (`rows` × `cols` phases).
struct Neighbours {
    rows: usize,
    cols: usize,
    same: Vec<Vec<(isize, isize)>>,
    other: Vec<Vec<(isize, isize, usize)>>,
}

impl Neighbours {
    /// `exact`: the pattern's own period on each axis (RapidRAW's, doubled
    /// when odd), so 2 × 8 and 12 × 12 DNG patterns keep their colours
    /// (`DeadTest::Isolated`). Otherwise, as the ratio tests always took
    /// it, 6 × 6 for a pattern with a side of 6 and 2 × 2 for all others.
    fn new(cfa: &CFA, exact: bool) -> Option<Self> {
        let (rows, cols, base) = if exact {
            let period = |p: usize| if p.is_multiple_of(2) { p.max(2) } else { 2 * p };
            let (rows, cols) = (period(cfa.height), period(cfa.width));
            (rows, cols, (rows, cols))
        } else {
            let period = if cfa.width == 6 || cfa.height == 6 {
                6
            } else {
                2
            };
            (period, period, (6, 6))
        };
        let mut same = Vec::new();
        let mut other = Vec::new();
        for row in 0..rows {
            for col in 0..cols {
                let colour = cfa.color_at(row + base.0, col + base.1);
                let mut s = Vec::new();
                let mut o = Vec::new();
                for dy in -2isize..=2 {
                    for dx in -2isize..=2 {
                        if dy == 0 && dx == 0 {
                            continue;
                        }
                        let c = cfa.color_at(
                            (row as isize + base.0 as isize + dy) as usize,
                            (col as isize + base.1 as isize + dx) as usize,
                        );
                        if c == colour {
                            s.push((dy, dx));
                        } else if dy.abs() <= 1 && dx.abs() <= 1 && c < 3 {
                            o.push((dy, dx, c));
                        }
                    }
                }
                if s.len() < 4 {
                    return None;
                }
                same.push(s);
                other.push(o);
            }
        }
        Some(Self {
            rows,
            cols,
            same,
            other,
        })
    }

    /// The phase of the sample at (`row`, `col`).
    fn phase(&self, row: usize, col: usize) -> usize {
        (row % self.rows) * self.cols + col % self.cols
    }
}

/// Repairs defective samples in place. `level` maps a stored sample to the
/// 0–1 sensor range, `store` maps back. Returns how many were replaced.
#[allow(clippy::too_many_arguments)]
pub fn suppress<T: Copy + Send + Sync>(
    data: &mut [T],
    width: usize,
    active: (usize, usize, usize, usize),
    cfa: &CFA,
    wb: [f32; 4],
    dead_test: DeadTest,
    level: impl Fn(T) -> f32 + Sync,
    store: impl Fn(f32) -> T + Sync,
) -> usize {
    let Some(neighbours) = Neighbours::new(cfa, dead_test.isolated()) else {
        return 0;
    };
    let (left, top, aw, ah) = active;
    let margin = dead_test.margin();
    if aw < 8 || ah < 8 || aw <= 2 * margin || ah <= 2 * margin || data.len() < (top + ah) * width {
        return 0;
    }
    let wb: [f32; 3] = std::array::from_fn(|i| {
        if wb[i] > 0.0 && wb[i].is_finite() {
            wb[i]
        } else {
            1.0
        }
    });
    let source: &[T] = data;
    // The test of one sample, as it always was.
    let check = |row: usize, col: usize, values: &mut [f32; 24]| -> Option<T> {
        let phase = neighbours.phase(row, col);
        let colour = cfa.color_at(row, col);
        if colour >= 3 {
            return None;
        }
        let v = level(source[row * width + col]);
        let same = &neighbours.same[phase];
        let mut lo = f32::INFINITY;
        let mut hi = f32::NEG_INFINITY;
        for (i, (dy, dx)) in same.iter().enumerate() {
            let n =
                level(source[(row as isize + dy) as usize * width + (col as isize + dx) as usize]);
            values[i] = n;
            lo = lo.min(n);
            hi = hi.max(n);
        }
        // Adjacent samples of the other colours, white-balanced.
        let adjacent = || {
            neighbours.other[phase].iter().map(|(dy, dx, c)| {
                level(source[(row as isize + dy) as usize * width + (col as isize + dx) as usize])
                    * wb[*c]
                    / wb[colour]
            })
        };
        let hot = v > HOT_RATIO * hi.max(0.0) + HOT_MARGIN
            && v > HOT_NEIGHBOUR_RATIO * adjacent().fold(0.0f32, f32::max) + HOT_MARGIN;
        let dead = !hot
            && lo > DEAD_FLOOR
            && v < DEAD_RATIO * lo - DEAD_MARGIN
            && (dead_test == DeadTest::SameColour
                || v < DEAD_NEIGHBOUR_RATIO * adjacent().fold(f32::INFINITY, f32::min)
                    - DEAD_MARGIN);
        if hot || dead {
            let n = same.len();
            let slice = &mut values[..n];
            slice.sort_by(f32::total_cmp);
            let median = 0.5 * (slice[(n - 1) / 2] + slice[n / 2]);
            Some(store(median))
        } else {
            None
        }
    };
    let at = |row: usize, col: usize| level(source[row * width + col]);
    // `DeadTest::IsolatedBlack`'s level for a dead sample whose
    // same-colour median is `median`.
    let black = dead_test == DeadTest::IsolatedBlack;
    // None (no repair) when that is not a number, as around not-a-number
    // neighbours of a float mosaic.
    let dead_level = |row: usize, col: usize, median: f32| -> Option<f32> {
        if black {
            let same = &neighbours.same[neighbours.phase(row, col)];
            Some(colour_ratio_repair(cfa, same, row, col, at).unwrap_or(median))
                .filter(|v| v.is_finite())
        } else {
            Some(median)
        }
    };
    // The median of the same-colour samples within two pixels of
    // (`row`, `col`), with `centre` among them when given.
    let median_around = |row: usize, col: usize, centre: Option<f32>| -> f32 {
        let phase = neighbours.phase(row, col);
        let mut values = [0f32; 25];
        let mut n = 0;
        for &(dy, dx) in &neighbours.same[phase] {
            values[n] = at((row as isize + dy) as usize, (col as isize + dx) as usize);
            n += 1;
        }
        if let Some(centre) = centre {
            values[n] = centre;
            n += 1;
        }
        middle(&mut values[..n])
    };
    // The isolation test of a sample the prefilter found hot (`hot`) or
    // dead (RapidRAW v1.7.1, `repair`).
    let isolated = |row: usize, col: usize, v: f32, hot: bool| -> Option<T> {
        let phase = neighbours.phase(row, col);
        let sign = if hot { 1.0 } else { -1.0 };
        if !neighbours.same[phase].iter().all(|&(dy, dx)| {
            let s = at((row as isize + dy) as usize, (col as isize + dx) as usize);
            sign * (v - s) > ISOLATED_FLOOR.max(s * CONTRAST)
        }) {
            return None;
        }
        let deviation = v - median_around(row, col, Some(v));
        // Each sample's deviation from its own neighbourhood, adjacent ones
        // first: a clipped or co-deviating neighbour rejects at once, and
        // so do deviations around it already too large to be isolated from
        // (the sums are then taken in RapidRAW's order, so the decision is
        // its own to the last bit).
        let mut d = [0f32; 24];
        let deviation_at = |dy: isize, dx: isize| {
            let (r, c) = ((row as isize + dy) as usize, (col as isize + dx) as usize);
            let value = at(r, c);
            (value, value - median_around(r, c, Some(value)))
        };
        let mut spill = 0.0f32;
        let mut partial = 0.0f32;
        for (i, &(dy, dx)) in WINDOW.iter().enumerate() {
            if dy.abs() > 1 || dx.abs() > 1 {
                continue;
            }
            let (value, deviation_here) = deviation_at(dy, dx);
            if deviation > 0.0 && value >= CLIP {
                return None;
            }
            d[i] = deviation_here;
            spill += deviation_here;
            partial += deviation_here.abs();
            // Rounding cannot make the full sum (24 terms) smaller than
            // this part of it by 1e-4.
            if ISOLATION * partial * (1.0 - 1e-4) >= deviation.abs() {
                return None;
            }
        }
        if spill * deviation.signum() > SPILL * deviation.abs() {
            return None;
        }
        for (i, &(dy, dx)) in WINDOW.iter().enumerate() {
            if dy.abs() > 1 || dx.abs() > 1 {
                d[i] = deviation_at(dy, dx).1;
            }
        }
        let surround: f32 = d.iter().fold(0.0, |sum, d| sum + d.abs());
        if deviation.abs() <= ISOLATION * surround {
            return None;
        }
        let median = median_around(row, col, None);
        if hot {
            Some(store(median))
        } else {
            dead_level(row, col, median).map(&store)
        }
    };
    // `DeadTest::IsolatedTexture`'s rule for a dark sample of level `v`
    // the isolation test left.
    // The medians of the same-colour samples within two pixels of
    // (`row`, `col`) without and with `centre` among them, from one sort.
    let medians_around = |row: usize, col: usize, centre: f32| -> (f32, f32) {
        let phase = neighbours.phase(row, col);
        let mut values = [0f32; 24];
        let mut n = 0;
        for &(dy, dx) in &neighbours.same[phase] {
            values[n] = at((row as isize + dy) as usize, (col as isize + dx) as usize);
            n += 1;
        }
        let sorted = &mut values[..n];
        let without = middle(sorted);
        // The sorted values with `centre` inserted at `place`.
        let place = sorted.partition_point(|s| s.total_cmp(&centre).is_lt());
        let merged = |i: usize| match i.cmp(&place) {
            std::cmp::Ordering::Less => sorted[i],
            std::cmp::Ordering::Equal => centre,
            std::cmp::Ordering::Greater => sorted[i - 1],
        };
        let count = n + 1;
        let half = count / 2;
        let with = if count.is_multiple_of(2) {
            0.5 * (merged(half - 1) + merged(half))
        } else {
            merged(half)
        };
        (without, with)
    };
    let textured_dead = |row: usize, col: usize, v: f32| -> Option<T> {
        let (replacement, median) = medians_around(row, col, v);
        let deviation = v - median;
        let mut spill = 0.0f32;
        let mut surround = 0.0f32;
        for &(dy, dx) in &WINDOW {
            let (r, c) = ((row as isize + dy) as usize, (col as isize + dx) as usize);
            let value = at(r, c);
            let (without, with) = medians_around(r, c, value);
            // Another dark sample: a line, a gap, a shadow's edge.
            if value < LINE_GUARD * without {
                return None;
            }
            let deviation_here = value - with;
            surround += deviation_here.abs();
            // The sum only grows (same order as the full sum below).
            if deviation.abs() <= TEXTURE_ISOLATION * surround {
                return None;
            }
            if dy.abs() <= 1 && dx.abs() <= 1 {
                spill += deviation_here;
            }
        }
        if spill * deviation.signum() > TEXTURE_SPILL * deviation.abs() {
            return None;
        }
        dead_level(row, col, replacement).map(&store)
    };
    // `DeadTest::IsolatedBlack`'s rule for a sample at black: no other
    // sample within two pixels is dark against its own colour.
    let black_dead = |row: usize, col: usize| -> Option<T> {
        for &(dy, dx) in &WINDOW {
            let (r, c) = ((row as isize + dy) as usize, (col as isize + dx) as usize);
            let value = at(r, c);
            if value < LINE_GUARD * median_around(r, c, None) {
                return None;
            }
        }
        dead_level(row, col, median_around(row, col, None)).map(&store)
    };
    let texture = dead_test.texture();
    // Only samples brighter than twice their brightest same-colour
    // neighbour or darker than 0.4 of their darkest can be defective, and
    // the brightest and darkest do not depend on the order they are taken
    // in: they are found for whole rows at once, on rows of levels each
    // converted once per band, and only those samples are tested.
    let rows: Vec<usize> = (top + margin..top + ah - margin).collect();
    let found: Vec<Found<T>> = rows
        .par_chunks(HOT_BAND)
        .flat_map_iter(|band| {
            let mut out = Vec::new();
            let mut values = [0f32; 24];
            let mut ring: [Vec<f32>; 5] = std::array::from_fn(|_| vec![0.0f32; aw]);
            let (mut hi, mut lo) = (vec![0.0f32; aw], vec![0.0f32; aw]);
            let fill = |buffer: &mut [f32], row: usize| {
                for (l, &s) in buffer.iter_mut().zip(&source[row * width + left..][..aw]) {
                    *l = level(s);
                }
            };
            for row in band[0] - 2..band[0] + 2 {
                fill(&mut ring[row % 5], row);
            }
            let (rows, cols) = (neighbours.rows, neighbours.cols);
            for &row in band {
                fill(&mut ring[(row + 2) % 5], row + 2);
                hi.fill(f32::NEG_INFINITY);
                lo.fill(f32::INFINITY);
                for pc in 0..cols {
                    let phase = (row % rows) * cols + pc;
                    // Active columns margin..aw - margin of this phase.
                    let first = margin + (pc + cols - (left + margin) % cols) % cols;
                    for &(dy, dx) in &neighbours.same[phase] {
                        let from = &ring[(row as isize + dy) as usize % 5];
                        for col in (first..aw - margin).step_by(cols) {
                            let n = from[(col as isize + dx) as usize];
                            hi[col] = hi[col].max(n);
                            lo[col] = lo[col].min(n);
                        }
                    }
                }
                let own = &ring[row % 5];
                for col in margin..aw - margin {
                    let v = own[col];
                    let fix = if dead_test.isolated() {
                        let candidate = isolated_candidate(v, lo[col], hi[col]);
                        let textured =
                            texture && lo[col] >= ISOLATED_FLOOR && v < DEAD_RATIO * lo[col];
                        if (candidate.is_some() || textured) && cfa.color_at(row, left + col) < 3 {
                            let index = row * width + left + col;
                            candidate
                                .and_then(|hot| isolated(row, left + col, v, hot))
                                .or_else(|| {
                                    textured
                                        .then(|| textured_dead(row, left + col, v))
                                        .flatten()
                                })
                                .map(|value| Found::Fix(index, value))
                                .or_else(|| {
                                    // A sample at black the texture rule leaves waits
                                    // for the photo's noise.
                                    (textured && black && v < BLACK_RATIO * lo[col])
                                        .then_some(Found::Texture(index, v, lo[col]))
                                })
                        } else {
                            None
                        }
                    } else if v > HOT_RATIO * hi[col].max(0.0) + HOT_MARGIN
                        || lo[col] > DEAD_FLOOR && v < DEAD_RATIO * lo[col] - DEAD_MARGIN
                    {
                        check(row, left + col, &mut values)
                            .map(|value| Found::Fix(row * width + left + col, value))
                    } else {
                        None
                    };
                    out.extend(fix);
                }
            }
            out
        })
        .collect();
    let mut fixes = Vec::with_capacity(found.len());
    let mut pending = Vec::new();
    for f in found {
        match f {
            Found::Fix(index, value) => fixes.push((index, value)),
            Found::Texture(index, v, lo) => pending.push((index, v, lo)),
        }
    }
    if !pending.is_empty() {
        let noise = NoiseBands::measure(source, width, active, cfa, &level);
        fixes.par_extend(pending.par_iter().filter_map(|&(index, v, lo)| {
            let (row, col) = (index / width, index % width);
            // In the noise (or where the noise is not known) the texture
            // rule has decided alone, as with `DeadTest::IsolatedTexture`.
            let clear = noise
                .at(cfa.color_at(row, col), lo)
                .is_some_and(|sigma| lo - v > NOISE_MARGIN * sigma);
            clear
                .then(|| black_dead(row, col))
                .flatten()
                .map(|value| (index, value))
        }));
    }
    for (index, value) in &fixes {
        data[*index] = *value;
    }
    fixes.len()
}

/// A sample the row pass repairs, or (`DeadTest::IsolatedBlack`) a sample
/// at black the texture rule left, of level `v` and darkest same-colour
/// neighbour `lo`, that waits for the photo's noise.
enum Found<T> {
    Fix(usize, T),
    Texture(usize, f32, f32),
}

#[cfg(test)]
#[path = "hot_pixels_reference.rs"]
mod hot_pixels_reference;

#[cfg(test)]
mod tests {
    use super::*;

    fn bayer() -> CFA {
        CFA::new("RGGB")
    }

    /// A 12 × 12 pattern: Bayer with red and blue swapped in every third
    /// 2 × 2 block.
    fn twelve() -> CFA {
        let pattern: String = (0..144)
            .map(|i| {
                let (r, c) = (i / 12, i % 12);
                let swap = (r / 2 + c / 2) % 3 == 0;
                match (r % 2, c % 2) {
                    (0, 0) if swap => 'B',
                    (0, 0) => 'R',
                    (1, 1) if swap => 'R',
                    (1, 1) => 'B',
                    _ => 'G',
                }
            })
            .collect();
        CFA::new(&pattern)
    }

    /// The per-sample reference of `dead_test`.
    #[allow(clippy::too_many_arguments)]
    fn reference<T: Copy + Send + Sync>(
        data: &mut [T],
        width: usize,
        active: (usize, usize, usize, usize),
        cfa: &CFA,
        wb: [f32; 4],
        dead_test: DeadTest,
        level: impl Fn(T) -> f32 + Sync,
        store: impl Fn(f32) -> T + Sync,
    ) -> usize {
        if dead_test.isolated() {
            super::hot_pixels_reference::isolated(data, width, active, cfa, dead_test, level, store)
        } else {
            super::hot_pixels_reference::suppress(
                data, width, active, cfa, wb, dead_test, level, store,
            )
        }
    }

    /// The row prefilter repairs exactly what the per-sample test did:
    /// u16 and f32 mosaics, Bayer and X-Trans, every dead test, an active
    /// area off the origin, defects of every kind among noise and edges.
    #[test]
    fn the_row_prefilter_repairs_what_the_reference_repairs() {
        let mut state = 0x2545_f491u32;
        let mut noise = move || {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (state >> 8) as f32 / (1u32 << 24) as f32
        };
        let xtrans = CFA::new("GGRGGBGGBGGRBRGRBGGGBGGRGGRGGBRBGBRG");
        for cfa in [bayer(), CFA::new("GBRG"), xtrans, twelve()] {
            for dead_test in [
                DeadTest::SameColour,
                DeadTest::Adjacent,
                DeadTest::Isolated,
                DeadTest::IsolatedTexture,
                DeadTest::IsolatedBlack,
            ] {
                let (w, h) = (203usize, 150usize);
                let active = (5, 3, 190, 141);
                let levels: Vec<f32> = (0..w * h)
                    .map(|i| {
                        let (y, x) = (i / w, i % w);
                        let base = if x > 120 {
                            0.4
                        } else if y > 90 {
                            0.003
                        } else {
                            0.15
                        };
                        match (x * 31 + y * 17) % 211 {
                            0 => 0.97,
                            1 => 0.0,
                            2 => base * 0.1,
                            3 => base * 3.0,
                            _ => base * (1.0 + 0.3 * (noise() - 0.5)),
                        }
                    })
                    .collect();
                let wb = [2.1, 1.0, 1.6, 1.0];
                let mut new = levels.clone();
                let mut old = levels.clone();
                let a = suppress(&mut new, w, active, &cfa, wb, dead_test, |v| v, |v| v);
                let b = reference(&mut old, w, active, &cfa, wb, dead_test, |v| v, |v| v);
                assert!(a > 10, "{a}");
                assert_eq!(a, b);
                assert!(
                    new.iter()
                        .zip(&old)
                        .all(|(x, y)| x.to_bits() == y.to_bits())
                );
                let raw: Vec<u16> = levels.iter().map(|v| (v * 4000.0 + 600.0) as u16).collect();
                let (mut new, mut old) = (raw.clone(), raw);
                let level = |v: u16| (v as f32 - 600.0) / 4000.0;
                let store = |l: f32| (l * 4000.0 + 600.0).round().clamp(0.0, 65535.0) as u16;
                let a = suppress(&mut new, w, active, &cfa, wb, dead_test, level, store);
                let b = reference(&mut old, w, active, &cfa, wb, dead_test, level, store);
                assert_eq!(a, b);
                assert_eq!(new, old);
            }
        }
    }

    #[test]
    fn isolated_hot_and_dead_samples_are_replaced_but_spread_points_stay() {
        let (w, h) = (40, 40);
        let mut data = vec![0.3f32; w * h];
        // Hot photosite and dead photosite.
        data[10 * w + 10] = 0.95;
        data[20 * w + 21] = 0.0;
        // A real point of light: 3 × 3 photosites of every colour.
        for dy in 0..3 {
            for dx in 0..3 {
                data[(30 + dy) * w + 30 + dx] = 0.95;
            }
        }
        let fixed = suppress(
            &mut data,
            w,
            (0, 0, w, h),
            &bayer(),
            [1.0; 4],
            DeadTest::Adjacent,
            |v| v,
            |v| v,
        );
        assert_eq!(fixed, 2);
        assert!((data[10 * w + 10] - 0.3).abs() < 1e-6);
        assert!((data[20 * w + 21] - 0.3).abs() < 1e-6);
        assert_eq!(data[31 * w + 31], 0.95);
    }

    #[test]
    fn noise_in_the_dark_is_left_alone() {
        let (w, h) = (32, 32);
        let mut data: Vec<f32> = (0..w * h)
            .map(|i| 0.002 + 0.002 * ((i * 7919) % 13) as f32 / 13.0)
            .collect();
        let before = data.clone();
        assert_eq!(
            suppress(
                &mut data,
                w,
                (0, 0, w, h),
                &bayer(),
                [2.0, 1.0, 1.5, 1.0],
                DeadTest::Adjacent,
                |v| v,
                |v| v
            ),
            0
        );
        assert_eq!(data, before);
    }

    /// A thin dark line keeps its samples: on X-Trans the red and blue
    /// samples on a one-pixel line have no same-colour neighbour on it
    /// within two pixels, so the same-colour test alone took them for dead
    /// photosites and lifted them to the bright samples beside the line.
    #[test]
    fn thin_dark_lines_are_not_dead_photosites() {
        let xtrans = CFA::new("GGRGGBGGBGGRBRGRBGGGBGGRGGRGGBRBGBRG");
        let (w, h) = (48, 48);
        // A dark vertical line one photosite wide in column 20, white
        // balance as a daylight X-Trans file.
        let wb = [1.9, 1.0, 1.9, 1.0];
        let mosaic: Vec<f32> = (0..w * h)
            .map(|i| {
                let level = if i % w == 20 { 0.02 } else { 0.25 };
                level / wb[xtrans.color_at(i / w, i % w)]
            })
            .collect();
        let mut data = mosaic.clone();
        let fixed = suppress(
            &mut data,
            w,
            (0, 0, w, h),
            &xtrans,
            wb,
            DeadTest::Adjacent,
            |v| v,
            |v| v,
        );
        assert_eq!(fixed, 0);
        assert_eq!(data, mosaic);
        // The same-colour test (saved edits) still changes the line.
        assert!(
            suppress(
                &mut data,
                w,
                (0, 0, w, h),
                &xtrans,
                wb,
                DeadTest::SameColour,
                |v| v,
                |v| v
            ) > 0
        );
        // A dead photosite beside the line is still found.
        let mut data = mosaic.clone();
        data[24 * w + 30] = 0.0;
        assert_eq!(
            suppress(
                &mut data,
                w,
                (0, 0, w, h),
                &xtrans,
                wb,
                DeadTest::Adjacent,
                |v| v,
                |v| v
            ),
            1
        );
        assert!(data[24 * w + 30] > 0.1);
    }

    /// A sharp star (σ 0.6 photosites) and a one-photosite dark line of
    /// slope ½ on a Bayer file: the ratio tests take the star's core and the
    /// line's samples for defects, the isolation test keeps both and still
    /// repairs a hot and a dead photosite.
    #[test]
    fn the_isolation_test_keeps_sharp_stars_and_thin_lines() {
        let cfa = bayer();
        let (w, h) = (64, 64);
        let mut mosaic = vec![0.0f32; w * h];
        // The star: a Gaussian integrated over each photosite, red and blue
        // tinted 0.7.
        let (cx, cy, sigma, peak) = (20.5f32, 20.5f32, 0.6f32, 0.8f32);
        for y in 0..h {
            for x in 0..w {
                let mut g = 0.0;
                for sy in 0..4 {
                    for sx in 0..4 {
                        let dx = x as f32 + (sx as f32 + 0.5) / 4.0 - cx;
                        let dy = y as f32 + (sy as f32 + 0.5) / 4.0 - cy;
                        g += (-(dx * dx + dy * dy) / (2.0 * sigma * sigma)).exp() / 16.0;
                    }
                }
                let tint = if cfa.color_at(y, x) == 1 { 1.0 } else { 0.7 };
                let line = x >= 32 && (x as i64 - 2 * y as i64).rem_euclid(48) == 0;
                mosaic[y * w + x] = if line { 0.05 } else { 0.02 + peak * tint * g };
                if x >= 32 && !line {
                    mosaic[y * w + x] = 0.6;
                }
            }
        }
        let fixed = |test: DeadTest, data: &mut Vec<f32>| {
            suppress(data, w, (0, 0, w, h), &cfa, [1.0; 4], test, |v| v, |v| v)
        };
        let star = |data: &[f32]| {
            (18..23)
                .flat_map(|y| (18..23).map(move |x| y * w + x))
                .filter(|&i| data[i] != mosaic[i])
                .count()
        };
        let line = |data: &[f32]| {
            (0..w * h)
                .filter(|&i| i % w >= 32 && data[i] != mosaic[i])
                .count()
        };
        let mut adjacent = mosaic.clone();
        fixed(DeadTest::Adjacent, &mut adjacent);
        assert!(
            star(&adjacent) > 0 && line(&adjacent) > 0,
            "{} {}",
            star(&adjacent),
            line(&adjacent)
        );
        for test in [
            DeadTest::Isolated,
            DeadTest::IsolatedTexture,
            DeadTest::IsolatedBlack,
        ] {
            let mut isolated = mosaic.clone();
            assert_eq!(fixed(test, &mut isolated), 0);
            assert_eq!(isolated, mosaic);
            // Defects on the flat sky and the bright field.
            let mut data = mosaic.clone();
            data[40 * w + 10] = 0.9;
            data[10 * w + 50] = 0.0;
            assert_eq!(fixed(test, &mut data), 2);
            assert!((data[40 * w + 10] - 0.02).abs() < 1e-6);
            assert!((data[10 * w + 50] - 0.6).abs() < 1e-6);
        }
    }

    /// A dead photosite on a steep ramp (same-colour neighbours 0.10–0.26):
    /// it lies below its darkest neighbour by less than their spread, so
    /// the isolation test leaves it; the texture rule repairs it and
    /// leaves the ramps, and a dark one-photosite line across them, alone.
    #[test]
    fn dead_photosites_in_busy_texture_are_repaired() {
        let cfa = bayer();
        let (w, h) = (48, 48);
        // Steep ramps, 0.02–0.62 every 16 photosites.
        let texture: Vec<f32> = (0..w * h)
            .map(|i| 0.02 + 0.04 * (i % w % 16) as f32)
            .collect();
        let run = |test: DeadTest, data: &mut Vec<f32>| {
            suppress(data, w, (0, 0, w, h), &cfa, [1.0; 4], test, |v| v, |v| v)
        };
        for test in [
            DeadTest::Isolated,
            DeadTest::IsolatedTexture,
            DeadTest::IsolatedBlack,
        ] {
            let mut data = texture.clone();
            assert_eq!(run(test, &mut data), 0, "{test:?}");
        }
        let at = 20 * w + 20;
        let mut dead = texture.clone();
        dead[at] = 0.0;
        let mut data = dead.clone();
        assert_eq!(run(DeadTest::Isolated, &mut data), 0);
        assert_eq!(run(DeadTest::IsolatedTexture, &mut data), 1);
        assert!(data[at] >= 0.1, "{}", data[at]);
        // A dark line through the texture keeps its samples.
        let mut line = texture.clone();
        for x in 0..w {
            line[20 * w + x] = 0.01;
        }
        for test in [DeadTest::IsolatedTexture, DeadTest::IsolatedBlack] {
            let mut data = line.clone();
            assert_eq!(run(test, &mut data), 0, "{test:?}");
            assert_eq!(data, line);
        }
    }

    /// A float mosaic whose red samples around a dead one are not numbers:
    /// the repair leaves it rather than writing a not-a-number level.
    #[test]
    fn a_dead_sample_among_not_a_number_neighbours_stays_a_number() {
        let cfa = bayer();
        let (w, h) = (64, 64);
        let mut data = vec![0.3f32; w * h];
        for y in 26..39 {
            for x in 26..39 {
                if cfa.color_at(y, x) == 0 {
                    data[y * w + x] = f32::NAN;
                }
            }
        }
        let at = 32 * w + 32;
        data[at] = 0.0;
        suppress(
            &mut data,
            w,
            (0, 0, w, h),
            &cfa,
            [1.0; 4],
            DeadTest::IsolatedBlack,
            |v| v,
            |v| v,
        );
        assert!(data[at].is_finite(), "{}", data[at]);
    }

    /// A frame of fine texture everywhere (a fabric close-up; the ramps
    /// above at a tenth of their level): the photo's noise, measured on the
    /// texture itself, comes out too high for the rule at black, and the
    /// texture rule still repairs the dead photosite it repaired before.
    #[test]
    fn a_frame_of_fine_texture_keeps_the_texture_rule() {
        let cfa = bayer();
        let (w, h) = (1024, 1024);
        let texture: Vec<f32> = (0..w * h)
            .map(|i| 0.1 * (0.02 + 0.04 * (i % w % 16) as f32))
            .collect();
        let at = 516 * w + 516;
        let mut dead = texture.clone();
        dead[at] = 0.0;
        for test in [DeadTest::IsolatedTexture, DeadTest::IsolatedBlack] {
            let mut data = dead.clone();
            let fixed = suppress(
                &mut data,
                w,
                (0, 0, w, h),
                &cfa,
                [1.0; 4],
                test,
                |v| v,
                |v| v,
            );
            assert_eq!(fixed, 1, "{test:?}");
            assert!(data[at] >= 0.01, "{test:?} {}", data[at]);
        }
    }

    /// A patch of busy dark texture (each sample 0.015–0.075, red half the
    /// green, blue a third, little noise): a dead red photosite lies within
    /// the spread of its neighbours' deviations, so only the rule for
    /// samples at black repairs it, and closer to the red the green around
    /// it implies than its neighbours' median is. Black-clipped noise of a
    /// very high ISO (0.02 ± 0.012) gets no more repairs than under the
    /// texture rule, and a dark line keeps its samples.
    #[test]
    fn dead_photosites_at_black_in_dark_texture_are_repaired() {
        let cfa = bayer();
        let (w, h) = (256, 256);
        let mut state = 0x9e37_79b9u32;
        let mut uniform = move || {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (state >> 8) as f32 / (1u32 << 24) as f32
        };
        let share = [0.5f32, 1.0, 1.0 / 3.0];
        // Texture in a 64 × 64 patch: green 0.045–0.075 in waves a few
        // photosites long (as a lens draws fine detail); elsewhere a smooth
        // ramp over the same levels (the noise is measured where the photo
        // is flat); then a little sensor noise.
        let green: Vec<f32> = (0..w * h)
            .map(|i| {
                let (y, x) = ((i / w) as f32, (i % w) as f32);
                if (96.0..160.0).contains(&y) && (96.0..160.0).contains(&x) {
                    0.06 + 0.015 * (1.1 * x + 0.4 * y).sin() * (0.8 * y - 0.5 * x).cos()
                } else {
                    0.03 + 0.06 * x / w as f32
                }
            })
            .collect();
        let texture: Vec<f32> = (0..w * h)
            .map(|i| {
                share[cfa.color_at(i / w, i % w)] * green[i] * (1.0 + 0.01 * (uniform() - 0.5))
            })
            .collect();
        let run = |test: DeadTest, data: &mut Vec<f32>| {
            suppress(data, w, (0, 0, w, h), &cfa, [1.0; 4], test, |v| v, |v| v)
        };
        // A dead red photosite at each of 64 places in the patch, one at a
        // time.
        let (mut by_texture, mut by_black) = (0, 0);
        let (mut error, mut median_error) = (0.0f32, 0.0f32);
        for k in 0..64 {
            let (y, x) = (100 + 7 * (k / 8), 100 + 7 * (k % 8));
            let (y, x) = (y - y % 2, x - x % 2);
            let at = y * w + x;
            let mut dead = texture.clone();
            dead[at] = 0.0;
            let mut data = dead.clone();
            run(DeadTest::IsolatedTexture, &mut data);
            by_texture += usize::from(data[at] != 0.0);
            let mut data = dead.clone();
            run(DeadTest::IsolatedBlack, &mut data);
            if data[at] != 0.0 {
                by_black += 1;
                let mut same: Vec<f32> = [
                    (-2isize, -2isize),
                    (-2, 0),
                    (-2, 2),
                    (0, -2),
                    (0, 2),
                    (2, -2),
                    (2, 0),
                    (2, 2),
                ]
                .iter()
                .map(|&(dy, dx)| {
                    texture[(y as isize + dy) as usize * w + (x as isize + dx) as usize]
                })
                .collect();
                error += (data[at] - texture[at]).abs();
                median_error += (middle(&mut same) - texture[at]).abs();
            }
        }
        assert!(
            by_black >= 48 && by_texture <= by_black / 2,
            "{by_texture} {by_black}"
        );
        assert!(error < 0.7 * median_error, "{error} {median_error}");
        // Very high ISO: black-clipped noise around a dark level.
        let noisy: Vec<f32> = (0..w * h)
            .map(|_| {
                let g = (-2.0 * uniform().max(1e-7).ln()).sqrt()
                    * (std::f32::consts::TAU * uniform()).cos();
                (0.02 + 0.012 * g).max(0.0)
            })
            .collect();
        let mut texture_run = noisy.clone();
        let mut black_run = noisy.clone();
        run(DeadTest::IsolatedTexture, &mut texture_run);
        run(DeadTest::IsolatedBlack, &mut black_run);
        let changed = |data: &[f32]| data.iter().zip(&noisy).filter(|(a, b)| a != b).count();
        assert!(
            changed(&black_run) <= changed(&texture_run),
            "{} {}",
            changed(&black_run),
            changed(&texture_run)
        );
        // A dark one-photosite line through the texture keeps its samples.
        let mut line = texture.clone();
        for x in 96..160 {
            line[110 * w + x] = 0.0;
        }
        let mut data = line.clone();
        assert_eq!(run(DeadTest::IsolatedBlack, &mut data), 0);
        assert_eq!(data, line);
    }

    /// A 12 × 12 pattern (DNG CFAs up to 12 × 12 and 2 × 8 exist): the
    /// isolation test reads each colour's own neighbours, not those of a
    /// 2 × 2 phase of another colour, so a flat field of each colour stays
    /// as it is and a hot photosite is still found.
    #[test]
    fn the_isolation_test_takes_a_large_patterns_own_period() {
        let cfa = twelve();
        let (w, h) = (60, 60);
        let flat = [0.65f32, 0.3, 0.08];
        let mosaic: Vec<f32> = (0..w * h)
            .map(|i| flat[cfa.color_at(i / w, i % w)])
            .collect();
        let mut data = mosaic.clone();
        let run = |data: &mut Vec<f32>| {
            suppress(
                data,
                w,
                (0, 0, w, h),
                &cfa,
                [1.0; 4],
                DeadTest::Isolated,
                |v| v,
                |v| v,
            )
        };
        assert_eq!(run(&mut data), 0);
        assert_eq!(data, mosaic);
        let at = (30..40)
            .flat_map(|y| (30..40).map(move |x| (y, x)))
            .find(|&(y, x)| cfa.color_at(y, x) == 2)
            .unwrap();
        data[at.0 * w + at.1] = 0.9;
        assert_eq!(run(&mut data), 1);
        assert_eq!(data, mosaic);
    }
}
