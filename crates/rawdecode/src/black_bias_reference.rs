//! `ShadowMeans::restore` as it was before its per-band gain rows
//! (2026-11-15), kept verbatim as the reference the faster one must match bit
//! for bit (tests in `black_bias`). Only the receiver and imports differ.

use super::*;

#[allow(clippy::needless_range_loop)]
pub(super) fn restore(
    means: &ShadowMeans,
    rgb: &mut [[f32; 3]],
    width: usize,
    scale: [f32; 3],
) -> usize {
    let height = rgb.len() / width;
    if means.blocks_x != width.div_ceil(BLOCK) || means.blocks_y != height.div_ceil(BLOCK) {
        return 0;
    }
    let (bx_n, by_n) = (means.blocks_x, means.blocks_y);
    // Sums of the demosaiced channels per block.
    let out: Vec<[f64; 3]> = (0..by_n)
        .into_par_iter()
        .flat_map_iter(|by| {
            let mut row = vec![[0.0f64; 3]; bx_n];
            for r in by * BLOCK..((by + 1) * BLOCK).min(height) {
                for (c, p) in rgb[r * width..(r + 1) * width].iter().enumerate() {
                    let s = &mut row[c / BLOCK];
                    for k in 0..3 {
                        s[k] += p[k] as f64;
                    }
                }
            }
            row
        })
        .collect();
    let pixels = |bx: usize, by: usize| {
        let w = (BLOCK).min(width - bx * BLOCK);
        let h = (BLOCK).min(height - by * BLOCK);
        (w * h) as f64
    };
    let noise = [0, 1, 2].map(|k| means.noise_model(k));
    // Gain per block and channel over its 3 × 3 window of blocks.
    let gains: Vec<[f32; 3]> = (0..by_n * bx_n)
        .into_par_iter()
        .map(|i| {
            let (bx, by) = (i % bx_n, i / bx_n);
            let mut g = [1.0f32; 3];
            for (k, gain) in g.iter_mut().enumerate() {
                let Some((slope, floor)) = noise[k] else {
                    continue;
                };
                let (mut n, mut sum, mut o, mut np) = (0.0, 0.0, 0.0, 0.0);
                for y in by.saturating_sub(1)..(by + 2).min(by_n) {
                    for x in bx.saturating_sub(1)..(bx + 2).min(bx_n) {
                        let s = means.stats[y * bx_n + x][k];
                        n += s[0];
                        sum += s[1];
                        o += out[y * bx_n + x][k];
                        np += pixels(x, y);
                    }
                }
                if n < 2.0 || np == 0.0 {
                    continue;
                }
                let mean = sum / n;
                let sigma = (slope * mean.max(0.0) + floor).sqrt();
                let snr = if sigma > 0.0 {
                    mean / sigma
                } else {
                    f64::INFINITY
                };
                let weight = 1.0 - smoothstep(FULL_SNR, NO_SNR, snr);
                if weight <= 0.0 {
                    continue;
                }
                let target = mean.max(0.0) * scale[k] as f64;
                let current = o / np;
                let full = if current > 0.0 {
                    (target / current).min(MAX_GAIN)
                } else {
                    1.0
                };
                *gain = (1.0 + weight * (full - 1.0)) as f32;
            }
            g
        })
        .collect();
    // Bilinear between block centres.
    let gain_at = |bx: usize, by: usize| gains[by.min(by_n - 1) * bx_n + bx.min(bx_n - 1)];
    let axis = |p: usize| {
        let f = (p as f32 + 0.5) / BLOCK as f32 - 0.5;
        if f <= 0.0 {
            (0usize, 0.0f32)
        } else {
            (f.floor() as usize, f.fract())
        }
    };
    rgb.par_chunks_mut(width)
        .enumerate()
        .map(|(r, line)| {
            let (by, ty) = axis(r);
            let mut changed = 0;
            for (c, p) in line.iter_mut().enumerate() {
                let (bx, tx) = axis(c);
                let g00 = gain_at(bx, by);
                let g10 = gain_at(bx + 1, by);
                let g01 = gain_at(bx, by + 1);
                let g11 = gain_at(bx + 1, by + 1);
                if [g00, g10, g01, g11].iter().all(|g| *g == [1.0; 3]) {
                    continue;
                }
                for k in 0..3 {
                    let top = g00[k] + (g10[k] - g00[k]) * tx;
                    let bottom = g01[k] + (g11[k] - g01[k]) * tx;
                    p[k] *= top + (bottom - top) * ty;
                }
                changed += 1;
            }
            changed
        })
        .sum()
}
