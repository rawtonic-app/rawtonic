// SPDX-License-Identifier: AGPL-3.0-or-later
//! Unbiased blacks in noisy shadows.
//!
//! Two biases lift and tint the shadows of high-ISO photos (notes:
//! benchmark-results/hc-highlight-clip-2026-10-30/):
//! - the mosaic is clipped at black before it is demosaiced (rawler's
//!   `apply_scaling`): read noise around black loses its negative half, so a
//!   dark area's mean rises by up to 0.4 of the noise, by a different amount
//!   per colour after white balance;
//! - the demosaic interpolates in square-root encoding (demosaic.rs), and the
//!   square of an average of noisy roots is below the average of the
//!   samples, so interpolated colours come out low where the noise is large
//!   against the signal.
//!
//! `ShadowMeans` records, before the clip, the signed mean and spread of each
//! CFA colour's samples per block of the mosaic; `restore` then scales each
//! demosaiced channel so its mean over a window of blocks matches the
//! samples' signed mean (never below zero), fully where the window's signal
//! is within `FULL_SNR` of its spread and not at all from `NO_SNR` on. Where
//! the signal stands clear of the noise neither bias is measurable and the
//! pixels keep their values exactly.
//!
//! `masked_black` reads the black level from the sensor's masked (optical
//! black) border where the camera has one: the levels some decoders take
//! from the maker notes are off by up to 15 levels per colour at high ISO
//! (EOS 7D at ISO 12800: 2032-2061 for a border, and Adobe's level, of 2047).

use rayon::prelude::*;

/// How the decode treats the black level (the recipe's `renderVersion`
/// decides it, image_loader.rs `decode_variant`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum BlackLevels {
    /// The decoder's black, the mosaic clipped at it (before `renderVersion` 8).
    #[default]
    Clipped,
    /// The masked border's black where it differs, and the shadows' means
    /// restored after the demosaic (`masked_black`, `ShadowMeans`).
    Unbiased,
}

/// Block side in photosites: a whole number of 2 × 2 and 6 × 6 periods.
pub const BLOCK: usize = 12;
/// Signal-to-noise ratios (per sample, from the photo's noise model) between
/// which the correction fades out.
const FULL_SNR: f64 = 4.0;
const NO_SNR: f64 = 8.0;
/// Largest gain applied to a channel.
const MAX_GAIN: f64 = 4.0;

/// Per block of the mosaic (`BLOCK` × `BLOCK`, from the origin of the area
/// measured) and CFA colour: count and sum of the signed samples, and count
/// and sum of the squared differences of same-colour samples at most
/// `NEIGHBOUR` columns apart (their noise, plus texture and edges where the
/// block has them), in the units `measure` was given.
#[derive(Clone, Debug)]
pub struct ShadowMeans {
    blocks_x: usize,
    blocks_y: usize,
    stats: Vec<[[f64; 4]; 3]>,
}

/// Same-colour samples up to this many columns apart count as neighbours
/// (2 on Bayer, 1 to 3 on X-Trans).
const NEIGHBOUR: usize = 3;
/// The noise of a brightness is the `NOISE_QUANTILE` of the blocks' noise
/// estimates at it: texture and edges only raise a block's estimate.
const NOISE_QUANTILE: f64 = 0.25;

impl ShadowMeans {
    /// `sample(row, col)` (rows and columns of the area, signed, black at 0)
    /// and `color(row, col)` (0, 1, 2) over an area of `width` × `height`.
    pub fn measure(
        width: usize,
        height: usize,
        sample: impl Fn(usize, usize) -> f32 + Sync,
        color: impl Fn(usize, usize) -> usize + Sync,
    ) -> Self {
        let blocks_x = width.div_ceil(BLOCK);
        let blocks_y = height.div_ceil(BLOCK);
        let stats = (0..blocks_y)
            .into_par_iter()
            .flat_map_iter(|by| {
                let mut row = vec![[[0.0f64; 4]; 3]; blocks_x];
                for r in by * BLOCK..((by + 1) * BLOCK).min(height) {
                    let mut last: [Option<(usize, f64)>; 3] = [None; 3];
                    for c in 0..width {
                        let v = sample(r, c) as f64;
                        let k = color(r, c);
                        let s = &mut row[c / BLOCK][k];
                        s[0] += 1.0;
                        s[1] += v;
                        if let Some((at, prev)) = last[k]
                            && c - at <= NEIGHBOUR
                        {
                            s[2] += 1.0;
                            s[3] += (v - prev) * (v - prev);
                        }
                        last[k] = Some((c, v));
                    }
                }
                row
            })
            .collect();
        Self {
            blocks_x,
            blocks_y,
            stats,
        }
    }

    /// Scales the channels of `rgb` (the demosaiced area, `width` × `height`,
    /// the measured units times `scale` per channel) so that their means
    /// match the samples' (see the module notes). Returns the number of
    /// pixels changed.
    pub fn restore(&self, rgb: &mut [[f32; 3]], width: usize, scale: [f32; 3]) -> usize {
        let height = rgb.len() / width;
        if self.blocks_x != width.div_ceil(BLOCK) || self.blocks_y != height.div_ceil(BLOCK) {
            return 0;
        }
        let (bx_n, by_n) = (self.blocks_x, self.blocks_y);
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
        let noise = [0, 1, 2].map(|k| self.noise_model(k));
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
                            let s = self.stats[y * bx_n + x][k];
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
}

impl ShadowMeans {
    /// The noise variance of colour `k` as `slope · level + floor` (shot and
    /// read noise), fitted to the lower quantile of the blocks' noise
    /// estimates per brightness band (half-octave bands, the darkest band
    /// holding every block at or below 2^-16 of white). None without two
    /// bands of 16 blocks, or one band at black.
    fn noise_model(&self, k: usize) -> Option<(f64, f64)> {
        const BANDS: usize = 33;
        let mut bands: Vec<Vec<(f64, f64)>> = vec![Vec::new(); BANDS];
        for s in &self.stats {
            let s = s[k];
            if s[0] < 4.0 || s[2] < 4.0 {
                continue;
            }
            let mean = s[1] / s[0];
            let var = s[3] / (2.0 * s[2]);
            let band = if mean <= 2f64.powi(-16) {
                0
            } else {
                ((mean.log2() + 16.0) * 2.0)
                    .ceil()
                    .clamp(1.0, (BANDS - 1) as f64) as usize
            };
            bands[band].push((mean, var));
        }
        let points: Vec<(f64, f64)> = bands
            .iter_mut()
            .filter(|b| b.len() >= 16)
            .map(|b| {
                b.sort_by(|x, y| x.1.total_cmp(&y.1));
                let var = b[((b.len() - 1) as f64 * NOISE_QUANTILE) as usize].1;
                let mut means: Vec<f64> = b.iter().map(|p| p.0.max(0.0)).collect();
                means.sort_by(f64::total_cmp);
                (means[means.len() / 2], var)
            })
            .collect();
        match points.len() {
            0 => None,
            1 if points[0].0 <= 2f64.powi(-16) => Some((0.0, points[0].1)),
            1 => None,
            _ => {
                // Least squares in relative error (weights 1 / var²).
                let (mut sw, mut sx, mut sy, mut sxx, mut sxy) = (0.0, 0.0, 0.0, 0.0, 0.0);
                for &(x, y) in &points {
                    let w = 1.0 / (y * y).max(1e-30);
                    sw += w;
                    sx += w * x;
                    sy += w * y;
                    sxx += w * x * x;
                    sxy += w * x * y;
                }
                let det = sw * sxx - sx * sx;
                let (slope, floor) = if det.abs() > 0.0 {
                    ((sw * sxy - sx * sy) / det, (sxx * sy - sx * sxy) / det)
                } else {
                    (0.0, sy / sw)
                };
                let floor = floor.max(points.iter().map(|p| p.1).fold(f64::MAX, f64::min) * 0.5);
                Some((slope.max(0.0), floor.max(0.0)))
            }
        }
    }
}

fn smoothstep(edge0: f64, edge1: f64, x: f64) -> f64 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The black level per position of a `period_w` × `period_h` repeat, read
/// from the masked `areas` of a mosaic (`width` wide, `sample(index)`), the
/// part of each area outside `active` only. None without enough masked
/// photosites (64 per position).
pub fn masked_black(
    areas: &[(usize, usize, usize, usize)],
    active: (usize, usize, usize, usize),
    width: usize,
    height: usize,
    period_w: usize,
    period_h: usize,
    sample: impl Fn(usize) -> f32,
) -> Option<Vec<f32>> {
    let (ax, ay, aw, ah) = active;
    let mut values = vec![Vec::new(); period_w * period_h];
    for &(x, y, w, h) in areas {
        for r in y..(y + h).min(height) {
            for c in x..(x + w).min(width) {
                if (ax..ax + aw).contains(&c) && (ay..ay + ah).contains(&r) {
                    continue;
                }
                values[(r % period_h) * period_w + c % period_w].push(sample(r * width + c));
            }
        }
    }
    values
        .into_iter()
        .map(|mut v| {
            if v.len() < 64 {
                return None;
            }
            // Mean within 4 spreads of the median (hot or lit photosites in
            // the border stay out).
            v.sort_by(f32::total_cmp);
            let median = v[v.len() / 2] as f64;
            let n = v.len() as f64;
            let mean = v.iter().map(|&x| x as f64).sum::<f64>() / n;
            let spread = (v.iter().map(|&x| (x as f64 - mean).powi(2)).sum::<f64>() / n).sqrt();
            let kept: Vec<f64> = v
                .iter()
                .map(|&x| x as f64)
                .filter(|x| (x - median).abs() <= 4.0 * spread.max(0.5))
                .collect();
            (!kept.is_empty()).then(|| (kept.iter().sum::<f64>() / kept.len() as f64) as f32)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noise(seed: &mut u64) -> f32 {
        // Sum of four uniforms: roughly normal, unit spread.
        (0..4)
            .map(|_| {
                *seed = seed
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                ((*seed >> 33) as f32 / (1u64 << 31) as f32) - 0.5
            })
            .sum::<f32>()
            * 1.732
    }

    /// A mosaic whose rows run from `low` to `high` (white 1), with shot
    /// and read noise.
    fn ramp(low: f32, high: f32, seed: &mut u64) -> (usize, Vec<f32>) {
        let (w, h) = (240, 480);
        let v = (0..w * h)
            .map(|i| {
                let x = low + (high - low) * (i / w) as f32 / (h - 1) as f32;
                x + (1e-6 + 1e-4 * x.max(0.0)).sqrt() * noise(seed)
            })
            .collect();
        (w, v)
    }

    #[test]
    fn clear_signal_keeps_its_pixels_and_noisy_black_regains_its_mean() {
        let color = |r: usize, c: usize| (r % 2) + (c % 2);
        let mut seed = 7u64;
        let (w, bright) = ramp(0.2, 0.6, &mut seed);
        let means = ShadowMeans::measure(w, bright.len() / w, |r, c| bright[r * w + c], color);
        let mut rgb: Vec<[f32; 3]> = bright.iter().map(|v| [v.max(0.0); 3]).collect();
        let before = rgb.clone();
        assert_eq!(means.restore(&mut rgb, w, [1.0; 3]), 0);
        assert_eq!(rgb, before);

        // From black to 0.004: a "demosaic" that clips at zero lifts the
        // darkest rows; the restored rows keep the samples' means.
        let (w, dark) = ramp(0.0, 0.004, &mut seed);
        let means = ShadowMeans::measure(w, dark.len() / w, |r, c| dark[r * w + c], color);
        let mut rgb: Vec<[f32; 3]> = dark.iter().map(|v| [v.max(0.0); 3]).collect();
        let before = rgb.clone();
        assert!(means.restore(&mut rgb, w, [1.0; 3]) > 0);
        let rows = 0..48 * w;
        let mean = |img: &[[f32; 3]]| {
            img[rows.clone()].iter().map(|p| p[1] as f64).sum::<f64>() / rows.len() as f64
        };
        let truth = dark[rows.clone()].iter().map(|v| *v as f64).sum::<f64>() / rows.len() as f64;
        assert!(mean(&before) > truth + 2e-4, "{} {truth}", mean(&before));
        assert!((mean(&rgb) - truth).abs() < 5e-5, "{} {truth}", mean(&rgb));
    }

    #[test]
    fn masked_black_reads_the_border_outside_the_active_area() {
        let (w, h) = (40, 40);
        // Columns 0..10 masked at 2048 (+1 on odd rows), image from column 8.
        let data: Vec<f32> = (0..w * h)
            .map(|i| {
                if i % w < 8 {
                    2048.0 + ((i / w) % 2) as f32
                } else {
                    3000.0
                }
            })
            .collect();
        let black =
            masked_black(&[(0, 0, 10, h)], (8, 0, w - 8, h), w, h, 2, 2, |i| data[i]).unwrap();
        assert_eq!(black, vec![2048.0, 2048.0, 2049.0, 2049.0]);
        assert!(masked_black(&[(0, 0, 1, 4)], (8, 0, w - 8, h), w, h, 2, 2, |i| data[i]).is_none());
    }
}
