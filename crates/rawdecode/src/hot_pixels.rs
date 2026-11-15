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
}

/// Same-colour offsets within two pixels and adjacent other-colour offsets,
/// per phase of the CFA pattern.
struct Neighbours {
    period: usize,
    same: Vec<Vec<(isize, isize)>>,
    other: Vec<Vec<(isize, isize, usize)>>,
}

impl Neighbours {
    fn new(cfa: &CFA) -> Option<Self> {
        let period = if cfa.width == 6 || cfa.height == 6 {
            6
        } else {
            2
        };
        let mut same = Vec::new();
        let mut other = Vec::new();
        for row in 0..period {
            for col in 0..period {
                let colour = cfa.color_at(row + 6, col + 6);
                let mut s = Vec::new();
                let mut o = Vec::new();
                for dy in -2isize..=2 {
                    for dx in -2isize..=2 {
                        if dy == 0 && dx == 0 {
                            continue;
                        }
                        let c = cfa.color_at(
                            (row as isize + 6 + dy) as usize,
                            (col as isize + 6 + dx) as usize,
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
            period,
            same,
            other,
        })
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
    let Some(neighbours) = Neighbours::new(cfa) else {
        return 0;
    };
    let (left, top, aw, ah) = active;
    if aw < 8 || ah < 8 || data.len() < (top + ah) * width {
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
        let phase = (row % neighbours.period) * neighbours.period + col % neighbours.period;
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
    // Only samples brighter than twice their brightest same-colour
    // neighbour or darker than 0.4 of their darkest can be defective, and
    // the brightest and darkest do not depend on the order they are taken
    // in: they are found for whole rows at once, on rows of levels each
    // converted once per band, and only those samples are tested.
    let rows: Vec<usize> = (top + 2..top + ah - 2).collect();
    let fixes: Vec<(usize, T)> = rows
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
            let period = neighbours.period;
            for &row in band {
                fill(&mut ring[(row + 2) % 5], row + 2);
                hi.fill(f32::NEG_INFINITY);
                lo.fill(f32::INFINITY);
                for pc in 0..period {
                    let phase = (row % period) * period + pc;
                    // Active columns 2..aw - 2 of this phase.
                    let first = 2 + (pc + period - (left + 2) % period) % period;
                    for &(dy, dx) in &neighbours.same[phase] {
                        let from = &ring[(row as isize + dy) as usize % 5];
                        for col in (first..aw - 2).step_by(period) {
                            let n = from[(col as isize + dx) as usize];
                            hi[col] = hi[col].max(n);
                            lo[col] = lo[col].min(n);
                        }
                    }
                }
                let own = &ring[row % 5];
                for col in 2..aw - 2 {
                    let v = own[col];
                    let candidate = v > HOT_RATIO * hi[col].max(0.0) + HOT_MARGIN
                        || lo[col] > DEAD_FLOOR && v < DEAD_RATIO * lo[col] - DEAD_MARGIN;
                    if candidate && let Some(value) = check(row, left + col, &mut values) {
                        out.push((row * width + left + col, value));
                    }
                }
            }
            out
        })
        .collect();
    for (index, value) in &fixes {
        data[*index] = *value;
    }
    fixes.len()
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

    /// The row prefilter repairs exactly what the per-sample test did:
    /// u16 and f32 mosaics, Bayer and X-Trans, both dead tests, an active
    /// area off the origin, defects of every kind among noise and edges.
    #[test]
    fn the_row_prefilter_repairs_what_the_reference_repairs() {
        let mut state = 0x2545_f491u32;
        let mut noise = move || {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (state >> 8) as f32 / (1u32 << 24) as f32
        };
        let xtrans = CFA::new("GGRGGBGGBGGRBRGRBGGGBGGRGGRGGBRBGBRG");
        for cfa in [bayer(), CFA::new("GBRG"), xtrans] {
            for dead_test in [DeadTest::SameColour, DeadTest::Adjacent] {
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
                let b = super::hot_pixels_reference::suppress(
                    &mut old,
                    w,
                    active,
                    &cfa,
                    wb,
                    dead_test,
                    |v| v,
                    |v| v,
                );
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
                let b = super::hot_pixels_reference::suppress(
                    &mut old, w, active, &cfa, wb, dead_test, level, store,
                );
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
}
