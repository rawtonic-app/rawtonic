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
    let fixes: Vec<(usize, T)> = (top + 2..top + ah - 2)
        .into_par_iter()
        .flat_map_iter(|row| {
            let mut out = Vec::new();
            let mut values = [0f32; 24];
            for col in left + 2..left + aw - 2 {
                let phase = (row % neighbours.period) * neighbours.period + col % neighbours.period;
                let colour = cfa.color_at(row, col);
                if colour >= 3 {
                    continue;
                }
                let v = level(source[row * width + col]);
                let same = &neighbours.same[phase];
                let mut lo = f32::INFINITY;
                let mut hi = f32::NEG_INFINITY;
                for (i, (dy, dx)) in same.iter().enumerate() {
                    let n = level(
                        source[(row as isize + dy) as usize * width + (col as isize + dx) as usize],
                    );
                    values[i] = n;
                    lo = lo.min(n);
                    hi = hi.max(n);
                }
                // Adjacent samples of the other colours, white-balanced.
                let adjacent = || {
                    neighbours.other[phase].iter().map(|(dy, dx, c)| {
                        level(
                            source[(row as isize + dy) as usize * width
                                + (col as isize + dx) as usize],
                        ) * wb[*c]
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
                    out.push((row * width + col, store(median)));
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
mod tests {
    use super::*;

    fn bayer() -> CFA {
        CFA::new("RGGB")
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
