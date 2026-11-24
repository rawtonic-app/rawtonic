//! Hot and dead pixel suppression as it was before its row prefilter
//! (2026-11-15), kept verbatim as the reference the faster `suppress` must
//! match exactly (tests in `hot_pixels`). Only the `super::` imports differ.
//! `isolated` is the reference of `DeadTest::Isolated`: RapidRAW v1.7.1's
//! `repair_hot_pixels` (`src-tauri/src/decode/hot_pixels.rs`, commit
//! `63b415758`, Copyright Timon Käch, AGPL-3.0-or-later), every sample
//! tested on its own, adapted to `suppress`'s levels, active area and CFA;
//! with its texture rules also of `DeadTest::IsolatedTexture` and
//! `DeadTest::IsolatedBlack`.

use super::*;

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
    let Some(neighbours) = Neighbours::new(cfa, false) else {
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
                let phase = neighbours.phase(row, col);
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

/// RapidRAW v1.7.1's per-sample test (`Sensor::repair`) on the levels
/// `suppress` takes: black 0, white 1.
#[allow(clippy::too_many_arguments)]
pub fn isolated<T: Copy + Send + Sync>(
    data: &mut [T],
    width: usize,
    active: (usize, usize, usize, usize),
    cfa: &CFA,
    wb: [f32; 4],
    dead_test: DeadTest,
    level: impl Fn(T) -> f32 + Sync,
    store: impl Fn(f32) -> T + Sync,
) -> usize {
    const REACH: isize = 2;
    let texture = dead_test.texture();
    let black = dead_test.black();
    let wb: [f32; 3] = std::array::from_fn(|i| {
        if wb[i] > 0.0 && wb[i].is_finite() {
            wb[i]
        } else {
            1.0
        }
    });
    let cross = (dead_test == DeadTest::IsolatedCross).then_some(wb);
    let black_guard = dead_test.black_line_guard();
    let period = [cfa.width, cfa.height].map(|p| if p % 2 == 0 { p.max(2) } else { 2 * p });
    let mut offsets: Vec<(isize, isize)> = (-REACH..=REACH)
        .flat_map(|dy| (-REACH..=REACH).map(move |dx| (dx, dy)))
        .filter(|&(dx, dy)| dx != 0 || dy != 0)
        .collect();
    offsets.sort_by_key(|&(dx, dy)| dx * dx + dy * dy);
    // Same-colour offsets per phase, nearest first.
    let same: Vec<Vec<(isize, isize)>> = (0..period[0] * period[1])
        .map(|k| {
            let (x, y) = (k % period[0] + period[0], k / period[0] + period[1]);
            let colour = cfa.color_at(y, x);
            offsets
                .iter()
                .filter(|&&(dx, dy)| {
                    cfa.color_at((y as isize + dy) as usize, (x as isize + dx) as usize) == colour
                })
                .map(|&(dx, dy)| (dy, dx))
                .collect()
        })
        .collect();
    let source: &[T] = data;
    let noise = black.then(|| NoiseBands::measure(source, width, active, cfa, &level));
    let value = |x: usize, y: usize| level(source[y * width + x]);
    let site = |x: usize, y: usize| &same[(y % period[1]) * period[0] + x % period[0]];
    let neighbours = |x: usize, y: usize| {
        site(x, y)
            .iter()
            .map(move |&(dy, dx)| value((x as isize + dx) as usize, (y as isize + dy) as usize))
    };
    let median = |x: usize, y: usize, centre: Option<f32>| {
        let mut samples: Vec<f32> = neighbours(x, y).chain(centre).collect();
        samples.sort_unstable_by(f32::total_cmp);
        let half = samples.len() / 2;
        if samples.len().is_multiple_of(2) {
            (samples[half - 1] + samples[half]) * 0.5
        } else {
            samples[half]
        }
    };
    // A dead sample's level: the colour ratio of its neighbours under
    // `black`, else (or without green around) their median; under `black`
    // no repair when that is not a number.
    let dead_level = |x: usize, y: usize| -> Option<f32> {
        let level = black
            .then(|| colour_ratio_repair(cfa, site(x, y), y, x, cross, |r, c| value(c, r)))
            .flatten()
            .unwrap_or_else(|| median(x, y, None));
        (!black || level.is_finite()).then_some(level)
    };
    let repair = |x: usize, y: usize| -> Option<T> {
        if cfa.color_at(y, x) >= 3 {
            return None;
        }
        let v = value(x, y);
        let first = neighbours(x, y).next()?;
        let sign = if v > first { 1.0 } else { -1.0 };
        if !neighbours(x, y).all(|s| sign * (v - s) > ISOLATED_FLOOR.max(s * CONTRAST)) {
            return None;
        }
        let (lo, hi) =
            neighbours(x, y).fold((f32::MAX, f32::MIN), |(lo, hi), s| (lo.min(s), hi.max(s)));
        let spread = ISOLATED_FLOOR.max(hi - lo);
        let hot = v - hi > spread.max(hi * CONTRAST);
        let dead = lo - v > spread.max(lo * CONTRAST);
        if !hot && !dead {
            return None;
        }
        let deviation = v - median(x, y, Some(v));
        let mut surround = 0.0;
        let mut spill = 0.0;
        for dy in -REACH..=REACH {
            for dx in -REACH..=REACH {
                if dx != 0 || dy != 0 {
                    let (nx, ny) = ((x as isize + dx) as usize, (y as isize + dy) as usize);
                    let value = value(nx, ny);
                    let d = value - median(nx, ny, Some(value));
                    surround += d.abs();
                    if dx.abs() <= 1 && dy.abs() <= 1 {
                        if deviation > 0.0 && value >= CLIP {
                            return None;
                        }
                        spill += d;
                    }
                }
            }
        }
        if deviation.abs() <= ISOLATION * surround
            || spill * deviation.signum() > SPILL * deviation.abs()
        {
            return None;
        }
        if hot {
            Some(store(median(x, y, None)))
        } else {
            dead_level(x, y).map(&store)
        }
    };
    // A dark sample in busy texture: darker than 0.4 of every same-colour
    // neighbour, isolated by the looser bound, no other dark sample around.
    let textured = |x: usize, y: usize| -> Option<T> {
        if !texture || cfa.color_at(y, x) >= 3 {
            return None;
        }
        let v = value(x, y);
        let lo = neighbours(x, y).fold(f32::MAX, f32::min);
        if lo < ISOLATED_FLOOR || v >= DEAD_RATIO * lo {
            return None;
        }
        if let Some(noise) = &noise {
            // At black, where the neighbours stand clear of the photo's
            // noise: only the line guard.
            let clear = noise
                .at(cfa.color_at(y, x), lo)
                .is_some_and(|sigma| lo - v > NOISE_MARGIN * sigma);
            if clear && v < BLACK_RATIO * lo {
                for dy in -REACH..=REACH {
                    for dx in -REACH..=REACH {
                        let (nx, ny) = ((x as isize + dx) as usize, (y as isize + dy) as usize);
                        if (dx != 0 || dy != 0)
                            && value(nx, ny) < black_guard * median(nx, ny, None)
                        {
                            return None;
                        }
                    }
                }
                return dead_level(x, y).map(&store);
            }
        }
        let deviation = v - median(x, y, Some(v));
        let mut surround = 0.0;
        let mut spill = 0.0;
        for dy in -REACH..=REACH {
            for dx in -REACH..=REACH {
                if dx != 0 || dy != 0 {
                    let (nx, ny) = ((x as isize + dx) as usize, (y as isize + dy) as usize);
                    let value = value(nx, ny);
                    if value < LINE_GUARD * median(nx, ny, None) {
                        return None;
                    }
                    let d = value - median(nx, ny, Some(value));
                    surround += d.abs();
                    if dx.abs() <= 1 && dy.abs() <= 1 {
                        spill += d;
                    }
                }
            }
        }
        if deviation.abs() <= TEXTURE_ISOLATION * surround
            || spill * deviation.signum() > TEXTURE_SPILL * deviation.abs()
        {
            return None;
        }
        dead_level(x, y).map(&store)
    };
    let repair = |x: usize, y: usize| repair(x, y).or_else(|| textured(x, y));
    let (left, top, aw, ah) = active;
    let margin = 2 * REACH as usize;
    let repair = &repair;
    let fixes: Vec<(usize, T)> = (top + margin..top + ah - margin)
        .into_par_iter()
        .flat_map_iter(|y| {
            (left + margin..left + aw - margin)
                .filter_map(move |x| repair(x, y).map(|v| (y * width + x, v)))
                .collect::<Vec<_>>()
        })
        .collect();
    for (index, value) in &fixes {
        data[*index] = *value;
    }
    fixes.len()
}
