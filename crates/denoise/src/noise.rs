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

#[cfg(test)]
mod tests {
    use super::*;

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
