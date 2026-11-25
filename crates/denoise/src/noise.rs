// SPDX-License-Identifier: AGPL-3.0-or-later
//! Blind estimate of a photo's noise: variance = shot · level + read per
//! packed plane, in black-subtracted, white-normalised units.
//!
//! Each plane is cut into 16 × 16 blocks; a block's noise variance is taken
//! from horizontal neighbour differences (robust: 1.4826 × MAD), its level
//! from its mean. Blocks are binned by level (20 quantile bins) and the
//! flattest fifth of each bin stands for the noise there (texture only adds
//! variance); a line through those points gives shot and read.

use anyhow::Result;

use crate::tiles::Planes;

const BLOCK: usize = 16;

#[derive(Clone, Debug)]
pub struct NoiseProfile {
    /// (shot, read) per plane in network order (R, G, B, G).
    pub channels: [(f32, f32); 4],
}

pub fn estimate(planes: &Planes) -> Result<NoiseProfile> {
    let mut channels = [(0.0, 0.0); 4];
    for (c, slot) in channels.iter_mut().enumerate() {
        *slot = fit(planes.plane(c), planes.width, planes.height);
    }
    Ok(NoiseProfile { channels })
}

fn median(values: &mut [f32]) -> f32 {
    let mid = values.len() / 2;
    *values.select_nth_unstable_by(mid, f32::total_cmp).1
}

fn percentile(values: &mut [f32], p: f32) -> f32 {
    let i = ((values.len() - 1) as f32 * p).round() as usize;
    *values.select_nth_unstable_by(i, f32::total_cmp).1
}

fn fit(plane: &[f32], width: usize, height: usize) -> (f32, f32) {
    let (bw, bh) = ((width - 1) / BLOCK, height / BLOCK);
    let mut blocks = Vec::with_capacity(bw * bh);
    let mut diffs = Vec::with_capacity(BLOCK * BLOCK);
    for by in 0..bh {
        for bx in 0..bw {
            diffs.clear();
            let mut sum = 0.0;
            for y in by * BLOCK..(by + 1) * BLOCK {
                for x in bx * BLOCK..(bx + 1) * BLOCK {
                    let v = plane[y * width + x];
                    sum += v;
                    diffs.push((plane[y * width + x + 1] - v) / std::f32::consts::SQRT_2);
                }
            }
            let mean = sum / (BLOCK * BLOCK) as f32;
            if !(0.002..0.8).contains(&mean) {
                continue;
            }
            let centre = median(&mut diffs);
            for d in diffs.iter_mut() {
                *d = (*d - centre).abs();
            }
            let sigma = 1.4826 * median(&mut diffs);
            blocks.push((mean, sigma * sigma));
        }
    }
    if blocks.len() < 40 {
        // Too little usable area: a flat guess from whatever there is.
        let mut vars: Vec<f32> = blocks.iter().map(|b| b.1).collect();
        let read = if vars.is_empty() {
            1e-6
        } else {
            median(&mut vars)
        };
        return (1e-8, read.max(1e-10));
    }
    blocks.sort_by(|a, b| a.0.total_cmp(&b.0));
    let bins = 20;
    let (mut xs, mut ys) = (Vec::new(), Vec::new());
    for i in 0..bins {
        let bin = &blocks[i * blocks.len() / bins..(i + 1) * blocks.len() / bins];
        if bin.len() <= 20 {
            continue;
        }
        let mut levels: Vec<f32> = bin.iter().map(|b| b.0).collect();
        let mut vars: Vec<f32> = bin.iter().map(|b| b.1).collect();
        xs.push(median(&mut levels));
        ys.push(percentile(&mut vars, 0.2));
    }
    if xs.len() < 2 {
        let read = ys.first().copied().unwrap_or(1e-6);
        return (1e-8, read.max(1e-10));
    }
    let n = xs.len() as f32;
    let (mx, my) = (xs.iter().sum::<f32>() / n, ys.iter().sum::<f32>() / n);
    let sxx: f32 = xs.iter().map(|x| (x - mx) * (x - mx)).sum();
    let sxy: f32 = xs.iter().zip(&ys).map(|(x, y)| (x - mx) * (y - my)).sum();
    let shot = if sxx > 0.0 { sxy / sxx } else { 0.0 };
    let read = my - shot * mx;
    (shot.max(1e-8), read.max(1e-10))
}

/// Blind estimate of an X-Trans mosaic's noise (`width` × `height`, the
/// canonical layout's origin at sample `origin`, `xtrans::phase`): one
/// (shot, read) for every colour, as raw samples' noise does not depend on
/// the filter in front of them. MatchFilter's scheme on each of the 36
/// single-colour planes reads 0.87–2.77× the true noise there, as its
/// neighbours 6 samples apart see texture; here each 6 × 6 cell gives its 8
/// pairs of horizontally adjacent green samples (1 sample apart), blocks
/// are 8 × 8 cells, and the rest is `fit`'s scheme (robust spread, the
/// flattest fifth per level bin, a line). On 26 held-out X-T1 frames it read
/// 0.84–1.07× the true noise but on one heavily textured low-noise frame
/// (1.38×) (notes: benchmark-results/xts1-2026-11-25). Medians and the 20th
/// percentile are numpy's, as the network was scored with them.
pub fn estimate_xtrans(
    mosaic: &[f32],
    width: usize,
    height: usize,
    origin: (usize, usize),
) -> NoiseProfile {
    const CELLS: usize = 8;
    let (oy, ox) = origin;
    // Whole cells from the canonical origin, both samples of every pair inside.
    let cells_y = height.saturating_sub(oy) / 6;
    let cells_x = width.saturating_sub(ox) / 6;
    let (bh, bw) = (cells_y / CELLS, cells_x / CELLS);
    let mut blocks = Vec::with_capacity(bh * bw);
    let mut diffs = Vec::with_capacity(8 * CELLS * CELLS);
    let mut levels_sum;
    for by in 0..bh {
        for bx in 0..bw {
            diffs.clear();
            levels_sum = 0.0f64;
            for cy in by * CELLS..(by + 1) * CELLS {
                for cx in bx * CELLS..(bx + 1) * CELLS {
                    // The green pairs of a cell: rows 1, 2, 4, 5 at columns 0–1 and 3–4.
                    for r in [1, 2, 4, 5] {
                        for c in [0, 3] {
                            let i = (oy + 6 * cy + r) * width + ox + 6 * cx + c;
                            let (a, b) = (mosaic[i], mosaic[i + 1]);
                            diffs.push((b - a) / std::f32::consts::SQRT_2);
                            levels_sum += ((a + b) / 2.0) as f64;
                        }
                    }
                }
            }
            let mean = (levels_sum / diffs.len() as f64) as f32;
            if !(mean > 0.002 && mean < 0.8) {
                continue;
            }
            let centre = numpy_median(&mut diffs);
            for d in diffs.iter_mut() {
                *d = (*d - centre).abs();
            }
            let sigma = 1.4826 * numpy_median(&mut diffs);
            blocks.push((mean, sigma * sigma));
        }
    }
    let flat = |read: f32| NoiseProfile {
        channels: [(1e-8, read.max(1e-10)); 4],
    };
    blocks.sort_by(|a, b| a.0.total_cmp(&b.0));
    let bins = 20;
    let (mut xs, mut ys) = (Vec::new(), Vec::new());
    for i in 0..bins {
        let bin = &blocks[i * blocks.len() / bins..(i + 1) * blocks.len() / bins];
        if bin.len() <= 20 {
            continue;
        }
        let mut levels: Vec<f32> = bin.iter().map(|b| b.0).collect();
        let mut vars: Vec<f32> = bin.iter().map(|b| b.1).collect();
        xs.push(numpy_median(&mut levels) as f64);
        ys.push(numpy_nearest_percentile(&mut vars, 0.2) as f64);
    }
    if xs.len() < 2 {
        // Too little usable area: a flat guess from whatever there is.
        let mut vars: Vec<f32> = blocks.iter().map(|b| b.1).collect();
        return flat(if vars.is_empty() {
            1e-6
        } else {
            numpy_median(&mut vars)
        });
    }
    // A least-squares line, in f64 as numpy's polyfit.
    let n = xs.len() as f64;
    let (mx, my) = (xs.iter().sum::<f64>() / n, ys.iter().sum::<f64>() / n);
    let sxx: f64 = xs.iter().map(|x| (x - mx) * (x - mx)).sum();
    let sxy: f64 = xs.iter().zip(&ys).map(|(x, y)| (x - mx) * (y - my)).sum();
    let shot = if sxx > 0.0 { sxy / sxx } else { 0.0 };
    let read = my - shot * mx;
    let channel = ((shot as f32).max(1e-8), (read as f32).max(1e-10));
    NoiseProfile {
        channels: [channel; 4],
    }
}

/// numpy's median: the mean of the two middle values of an even count.
fn numpy_median(values: &mut [f32]) -> f32 {
    let n = values.len();
    let (below, upper, _) = values.select_nth_unstable_by(n / 2, f32::total_cmp);
    let upper = *upper;
    if n % 2 == 1 {
        return upper;
    }
    let lower = below.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    // numpy averages in the values' own type.
    (lower + upper) / 2.0
}

/// numpy's `percentile(values, 100 p, method="nearest")`.
fn numpy_nearest_percentile(values: &mut [f32], p: f32) -> f32 {
    let i = ((values.len() - 1) as f64 * p as f64).round_ties_even() as usize;
    *values.select_nth_unstable_by(i, f32::total_cmp).1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gaussian(seed: &mut u64) -> f32 {
        let mut u = || {
            *seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((*seed >> 11) as f64 / (1u64 << 53) as f64).max(1e-12)
        };
        let (a, b) = (u(), u());
        ((-2.0 * a.ln()).sqrt() * (2.0 * std::f64::consts::PI * b).cos()) as f32
    }

    #[test]
    fn xtrans_noise_is_recovered_from_green_pairs_at_any_phase() {
        let (w, h) = (1500, 1000);
        let (shot, read) = (0.002f32, 0.000_04f32);
        for origin in [(0, 0), (2, 5)] {
            let mut seed = 11u64;
            let mut mosaic = vec![0.0; w * h];
            for y in 0..h {
                for x in 0..w {
                    // Colours at different levels, a smooth ramp across.
                    let colour =
                        crate::xtrans::CANONICAL[(y + 6 - origin.0) % 6][(x + 6 - origin.1) % 6];
                    let level =
                        (0.02 + 0.6 * x as f32 / w as f32) * [0.5, 1.0, 0.7][colour as usize];
                    mosaic[y * w + x] = level + (shot * level + read).sqrt() * gaussian(&mut seed);
                }
            }
            let profile = estimate_xtrans(&mosaic, w, h, origin);
            let (s, r) = profile.channels[0];
            assert!((s / shot - 1.0).abs() < 0.2, "shot {s} at {origin:?}");
            assert!((r / read - 1.0).abs() < 0.5, "read {r} at {origin:?}");
            assert!(profile.channels.iter().all(|&c| c == (s, r)), "one profile");
        }
    }

    #[test]
    fn numpy_medians_and_nearest_percentiles() {
        assert_eq!(numpy_median(&mut [4.0, 1.0, 3.0, 2.0]), 2.5);
        assert_eq!(numpy_median(&mut [5.0, 1.0, 3.0]), 3.0);
        // np.percentile(range(n), 20, method='nearest'): index (n - 1) · 0.2
        // rounded half to even.
        let mut v: Vec<f32> = (0..11).map(|i| i as f32).collect();
        assert_eq!(numpy_nearest_percentile(&mut v, 0.2), 2.0);
        let mut v: Vec<f32> = (0..36).map(|i| i as f32).collect();
        assert_eq!(numpy_nearest_percentile(&mut v, 0.2), 7.0); // 7.0 exactly
        let mut v: Vec<f32> = (0..8).map(|i| i as f32).collect();
        assert_eq!(numpy_nearest_percentile(&mut v, 0.5), 4.0); // 3.5 → 4
        let mut v: Vec<f32> = (0..6).map(|i| i as f32).collect();
        assert_eq!(numpy_nearest_percentile(&mut v, 0.5), 2.0); // 2.5 → 2
    }

    #[test]
    fn shot_and_read_noise_are_recovered() {
        // A ramp with variance 0.002 · level + 0.00004 (Gaussian approximation).
        let (w, h) = (1200, 800);
        let (shot, read) = (0.002f32, 0.000_04f32);
        let mut seed = 7u64;
        let mut gauss = || {
            let mut u = || {
                seed = seed
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                ((seed >> 11) as f64 / (1u64 << 53) as f64).max(1e-12)
            };
            let (a, b) = (u(), u());
            ((-2.0 * a.ln()).sqrt() * (2.0 * std::f64::consts::PI * b).cos()) as f32
        };
        let mut plane = vec![0.0; w * h];
        for y in 0..h {
            for x in 0..w {
                let level = 0.02 + 0.6 * x as f32 / w as f32;
                plane[y * w + x] = level + (shot * level + read).sqrt() * gauss();
            }
        }
        let (s, r) = fit(&plane, w, h);
        assert!((s / shot - 1.0).abs() < 0.25, "shot {s}");
        assert!((r / read - 1.0).abs() < 0.5, "read {r}");
    }
}
