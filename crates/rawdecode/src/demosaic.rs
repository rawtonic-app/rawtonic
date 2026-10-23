// SPDX-License-Identifier: AGPL-3.0-or-later
//! Full-resolution demosaicing for Bayer and X-Trans mosaics.
//!
//! Both algorithms are original implementations written for Rawtonic from
//! the published ideas, not ports of GPL code (ADR 0002):
//!
//! * Bayer: green from one-sided colour-difference estimates in four
//!   directions, blended by gradients and by a vertical/horizontal weight
//!   from colour-difference invariant high-pass energies (the direction
//!   logic of Ratio Corrected Demosaicing), then red/blue from colour
//!   differences along the diagonals and axes.
//! * X-Trans (and any other RGB pattern): green estimated along four
//!   directions with a curvature correction from the pixel's own colour,
//!   red/blue from colour differences per direction, and per pixel the
//!   directions whose result is smoothest in a perceptual space (the
//!   homogeneity idea of AHD and Markesteijn's X-Trans demosaic), after
//!   two passes that re-estimate green from colour differences.
//!
//! The input is a linear, white-balanced mosaic with white near 1.0
//! (values above are kept). Interpolation runs on square-root encoded
//! values: colour differences are far smoother there than in linear light
//! (+2…+4 dB on test images, no added shadow bias with sensor noise;
//! notes: reference/demosaicing.md). Work is split into overlapping tiles
//! that run in parallel, so memory beyond the output stays small.

use std::sync::atomic::{AtomicBool, Ordering};

use rayon::prelude::*;

/// A repeating colour filter pattern (0 = red, 1 = green, 2 = blue).
#[derive(Clone, Debug)]
pub struct CfaPattern {
    width: usize,
    height: usize,
    colors: Vec<u8>,
}

impl CfaPattern {
    pub fn new(width: usize, height: usize, colors: Vec<u8>) -> Option<Self> {
        (width > 0
            && height > 0
            && colors.len() == width * height
            && colors.iter().all(|&c| c < 3)
            && (0..3).all(|c| colors.contains(&c)))
        .then_some(Self {
            width,
            height,
            colors,
        })
    }

    /// Colour at an absolute (possibly negative) position.
    #[inline]
    pub fn color(&self, row: isize, col: isize) -> usize {
        let r = row.rem_euclid(self.height as isize) as usize;
        let c = col.rem_euclid(self.width as isize) as usize;
        self.colors[r * self.width + c] as usize
    }

    /// A 2×2 pattern with two greens on a diagonal.
    pub fn is_bayer(&self) -> bool {
        self.width == 2
            && self.height == 2
            && self.colors[0] == self.colors[3]
            && self.colors[0] == 1
            && self.colors[1] != 1
            && self.colors[2] != 1
            && self.colors[1] != self.colors[2]
            || self.width == 2
                && self.height == 2
                && self.colors[1] == 1
                && self.colors[2] == 1
                && self.colors[0] != 1
                && self.colors[3] != 1
                && self.colors[0] != self.colors[3]
    }
}

const TILE: usize = 512;

/// Asked between tiles and bands whether the demosaic is still wanted
/// (a prefetch gives way to the user's edits within a tile's time).
pub type Cancel<'a> = &'a (dyn Fn() -> bool + Sync);

/// A demosaic given up because its `Cancel` said so.
#[derive(Debug, PartialEq, Eq)]
pub struct Cancelled;

#[inline]
fn encode(v: f32) -> f32 {
    v.max(0.0).sqrt()
}

#[inline]
fn decode(v: f32) -> f32 {
    let v = v.max(0.0);
    v * v
}
/// Context around each tile: the X-Trans kernel reads up to 20 pixels away
/// (green ±6, three colour passes ±2, two green refinements ±3, smoothness
/// ±2), so tiles join without seams and the output does not depend on the
/// tile size (with 12, values within a few pixels of a tile edge differed by
/// up to 5 %).
pub const BORDER: usize = 20;

/// Whether the pattern is X-Trans sized (6 x 6), so 3 x 3 blocks hold all
/// three colours.
pub fn is_xtrans_like(cfa: &CfaPattern) -> bool {
    cfa.width == 6 && cfa.height == 6 && !cfa.is_bayer()
}

/// A draft without demosaicing: each `n` x `n` block of the mosaic becomes
/// one pixel with the mean of each colour's samples in it (X-Trans: 3 x 3
/// blocks hold 5 green, 2 red and 2 blue samples). `width / n` x
/// `height / n` pixels; partial blocks at the right and bottom are dropped.
pub fn bin(
    mosaic: &[f32],
    width: usize,
    height: usize,
    cfa: &CfaPattern,
    n: usize,
) -> Vec<[f32; 3]> {
    assert_eq!(mosaic.len(), width * height);
    let (out_w, out_h) = (width / n, height / n);
    let mut out = vec![[0.0f32; 3]; out_w * out_h];
    out.par_chunks_mut(out_w.max(1))
        .take(out_h)
        .enumerate()
        .for_each(|(by, line)| {
            for (bx, pixel) in line.iter_mut().enumerate() {
                let mut sum = [0.0f32; 3];
                let mut count = [0u32; 3];
                for dy in 0..n {
                    let row = by * n + dy;
                    for dx in 0..n {
                        let col = bx * n + dx;
                        let color = cfa.color(row as isize, col as isize);
                        sum[color] += mosaic[row * width + col];
                        count[color] += 1;
                    }
                }
                *pixel = std::array::from_fn(|k| {
                    if count[k] > 0 {
                        sum[k] / count[k] as f32
                    } else {
                        0.0
                    }
                });
            }
        });
    out
}

/// Demosaics `mosaic` (row-major, `width` × `height`, colours by `cfa` at
/// absolute positions) into RGB.
pub fn demosaic(mosaic: &[f32], width: usize, height: usize, cfa: &CfaPattern) -> Vec<[f32; 3]> {
    demosaic_unless(mosaic, width, height, cfa, &|| false).expect("never cancelled")
}

/// `demosaic`, given up as soon as `cancelled` says so (asked before each
/// tile and band; the result does not depend on it otherwise).
pub fn demosaic_unless(
    mosaic: &[f32],
    width: usize,
    height: usize,
    cfa: &CfaPattern,
    cancelled: Cancel,
) -> Result<Vec<[f32; 3]>, Cancelled> {
    assert_eq!(mosaic.len(), width * height);
    if cfa.is_bayer() {
        run_tiled(mosaic, width, height, cfa, cancelled, bayer_tile)
    } else {
        let tables = DirectionalTables::new(cfa);
        let mut out =
            match crate::gpu_demosaic::xtrans(mosaic, width, height, cfa, &tables, cancelled) {
                Some(out) => out?,
                None => run_tiled(mosaic, width, height, cfa, cancelled, |tile| {
                    directional_tile(tile, &tables)
                })?,
            };
        smooth_fine_grain(&mut out, width, height, cancelled)?;
        Ok(out)
    }
}

/// The X-Trans kernel on the CPU, before the fine-grain smoothing.
pub fn xtrans_cpu(
    mosaic: &[f32],
    width: usize,
    height: usize,
    cfa: &CfaPattern,
    tables: &DirectionalTables,
) -> Vec<[f32; 3]> {
    run_tiled(mosaic, width, height, cfa, &|| false, |tile| {
        directional_tile(tile, tables)
    })
    .expect("never cancelled")
}

/// Edge-aware smoothing of the finest luminance grain after X-Trans
/// demosaicing. Its noise sits at the pixel scale (finest band 2-2.7x
/// Camera Raw's at ISO 640-6400 while coarser bands were below it); a 3 x 3
/// bilateral filter on square-root luminance with a range of twice the
/// image's own pixel-scale noise brings that band to Camera Raw's level at
/// ISO 640-4000 for 1-2 % of texture detail (notes: benchmark-results/
/// xtrans-fine-grain.json). Colour ratios are kept.
fn smooth_fine_grain(
    rgb: &mut [[f32; 3]],
    width: usize,
    height: usize,
    cancelled: Cancel,
) -> Result<(), Cancelled> {
    const SPATIAL_SIGMA: f32 = 0.8;
    const RANGE_NOISE: f32 = 2.0;
    if width < 8 || height < 8 {
        return Ok(());
    }
    let luma = |p: &[f32; 3]| encode((p[0] + 2.0 * p[1] + p[2]) * 0.25);
    let y: Vec<f32> = rgb.par_iter().map(luma).collect();
    // Robust pixel-scale noise: median absolute deviation from the 3 x 3
    // mean on a sparse grid (texture is a minority of samples).
    let mut deviations: Vec<f32> = (1..height - 1)
        .step_by(7)
        .flat_map(|r| (1..width - 1).step_by(7).map(move |c| (r, c)))
        .map(|(r, c)| {
            let mut sum = 0.0;
            for dr in 0..3 {
                for dc in 0..3 {
                    sum += y[(r + dr - 1) * width + c + dc - 1];
                }
            }
            (y[r * width + c] - sum / 9.0).abs()
        })
        .collect();
    if deviations.is_empty() {
        return Ok(());
    }
    let mid = deviations.len() / 2;
    let (_, median, _) = deviations.select_nth_unstable_by(mid, f32::total_cmp);
    let sigma = 1.4826 * *median;
    if !sigma.is_finite() || sigma <= 1e-5 {
        return Ok(());
    }
    let range = RANGE_NOISE * sigma;
    let inv_range = -0.5 / (range * range);
    let w_edge = (-1.0 / (2.0 * SPATIAL_SIGMA * SPATIAL_SIGMA)).exp();
    let w_corner = (-2.0 / (2.0 * SPATIAL_SIGMA * SPATIAL_SIGMA)).exp();
    let spatial = [
        w_corner, w_edge, w_corner, w_edge, 1.0, w_edge, w_corner, w_edge, w_corner,
    ];
    // The range weight between two neighbours depends on their squared
    // difference only, the same from either side, so each pair's
    // exponential is evaluated once per band (instead of once per pixel and
    // direction); every pixel still sums the same values in the same order.
    let ex = |a: f32, b: f32| {
        let d = a - b;
        (d * d * inv_range).exp()
    };
    const BAND: usize = 64;
    let given_up = AtomicBool::new(false);
    rgb.par_chunks_mut(width * BAND)
        .enumerate()
        .for_each(|(band, rows)| {
            if given_up.load(Ordering::Relaxed) || cancelled() {
                given_up.store(true, Ordering::Relaxed);
                return;
            }
            let r0 = band * BAND;
            let r1 = r0 + rows.len() / width;
            // Interior rows of this band.
            let (first, last) = (r0.max(1), r1.min(height - 1));
            if first >= last {
                return;
            }
            // Pairs between row k and k + 1 for k in first - 1 .. last.
            let k0 = first - 1;
            let pair_rows = last - k0;
            let mut vertical = vec![0.0f32; pair_rows * width];
            let mut diag_right = vec![0.0f32; pair_rows * width];
            let mut diag_left = vec![0.0f32; pair_rows * width];
            for k in k0..last {
                let up = &y[k * width..(k + 1) * width];
                let down = &y[(k + 1) * width..(k + 2) * width];
                let o = (k - k0) * width;
                for c in 0..width {
                    vertical[o + c] = ex(down[c], up[c]);
                }
                for c in 0..width - 1 {
                    diag_right[o + c] = ex(down[c + 1], up[c]);
                }
                for c in 1..width {
                    diag_left[o + c] = ex(down[c - 1], up[c]);
                }
            }
            // Pairs between column c and c + 1 of the interior rows.
            let mut horizontal = vec![0.0f32; (last - first) * width];
            for r in first..last {
                let row = &y[r * width..(r + 1) * width];
                let o = (r - first) * width;
                for c in 0..width - 1 {
                    horizontal[o + c] = ex(row[c + 1], row[c]);
                }
            }
            for r in first..last {
                let up = (r - 1 - k0) * width;
                let here = (r - k0) * width;
                let h = (r - first) * width;
                let row = &mut rows[(r - r0) * width..(r - r0 + 1) * width];
                for (c, pixel) in row.iter_mut().enumerate().take(width - 1).skip(1) {
                    let center = y[r * width + c];
                    let range = [
                        diag_right[up + c - 1],
                        vertical[up + c],
                        diag_left[up + c + 1],
                        horizontal[h + c - 1],
                        1.0,
                        horizontal[h + c],
                        diag_left[here + c],
                        vertical[here + c],
                        diag_right[here + c],
                    ];
                    let (mut sum, mut weights) = (0.0, 0.0);
                    for (i, w_s) in spatial.iter().enumerate() {
                        let v = y[(r + i / 3 - 1) * width + c + i % 3 - 1];
                        let w = w_s * range[i];
                        sum += v * w;
                        weights += w;
                    }
                    let smoothed = sum / weights;
                    if center > 1e-4 {
                        let ratio = decode(smoothed) / decode(center);
                        for channel in pixel.iter_mut() {
                            *channel *= ratio;
                        }
                    }
                }
            }
        });
    if given_up.into_inner() {
        Err(Cancelled)
    } else {
        Ok(())
    }
}

/// One tile with its border, in buffer coordinates. Buffer pixel (0, 0) is
/// at absolute position (`top`, `left`), which may be negative.
struct Tile {
    data: Vec<f32>,
    colors: Vec<u8>,
    w: usize,
    h: usize,
    top: isize,
    left: isize,
}

struct OutPtr(*mut [f32; 3]);
unsafe impl Send for OutPtr {}
unsafe impl Sync for OutPtr {}

fn run_tiled<F>(
    mosaic: &[f32],
    width: usize,
    height: usize,
    cfa: &CfaPattern,
    cancelled: Cancel,
    kernel: F,
) -> Result<Vec<[f32; 3]>, Cancelled>
where
    F: Fn(&Tile) -> Vec<[f32; 3]> + Sync,
{
    let mut out = vec![[0.0f32; 3]; width * height];
    if width == 0 || height == 0 {
        return Ok(out);
    }
    let tiles_x = width.div_ceil(TILE);
    let tiles_y = height.div_ceil(TILE);
    let ptr = OutPtr(out.as_mut_ptr());
    let given_up = AtomicBool::new(false);
    (0..tiles_x * tiles_y).into_par_iter().for_each(|index| {
        if given_up.load(Ordering::Relaxed) || cancelled() {
            given_up.store(true, Ordering::Relaxed);
            return;
        }
        let ty = index / tiles_x;
        let tx = index % tiles_x;
        let y0 = ty * TILE;
        let x0 = tx * TILE;
        let th = TILE.min(height - y0);
        let tw = TILE.min(width - x0);
        let tile = extract_tile(mosaic, width, height, cfa, y0, x0, th, tw);
        let rgb = kernel(&tile);
        let ptr = &ptr;
        for r in 0..th {
            let src = &rgb[(r + BORDER) * tile.w + BORDER..][..tw];
            // Tiles cover disjoint output rectangles.
            let dst =
                unsafe { std::slice::from_raw_parts_mut(ptr.0.add((y0 + r) * width + x0), tw) };
            for (d, s) in dst.iter_mut().zip(src) {
                *d = s.map(decode);
            }
        }
    });
    if given_up.into_inner() {
        Err(Cancelled)
    } else {
        Ok(out)
    }
}

#[allow(clippy::too_many_arguments)]
fn extract_tile(
    mosaic: &[f32],
    width: usize,
    height: usize,
    cfa: &CfaPattern,
    y0: usize,
    x0: usize,
    th: usize,
    tw: usize,
) -> Tile {
    let w = tw + 2 * BORDER;
    let h = th + 2 * BORDER;
    let top = y0 as isize - BORDER as isize;
    let left = x0 as isize - BORDER as isize;
    let mut data = vec![0.0f32; w * h];
    let mut colors = vec![0u8; w * h];
    for r in 0..h {
        let ay = top + r as isize;
        for c in 0..w {
            let ax = left + c as isize;
            let color = cfa.color(ay, ax);
            colors[r * w + c] = color as u8;
            data[r * w + c] = encode(
                if ay >= 0 && ax >= 0 && (ay as usize) < height && (ax as usize) < width {
                    mosaic[ay as usize * width + ax as usize]
                } else {
                    nearest_same_color(mosaic, width, height, cfa, ay, ax, color)
                },
            );
        }
    }
    Tile {
        data,
        colors,
        w,
        h,
        top,
        left,
    }
}

/// Value of the nearest in-image sample of `color` to an outside position.
fn nearest_same_color(
    mosaic: &[f32],
    width: usize,
    height: usize,
    cfa: &CfaPattern,
    ay: isize,
    ax: isize,
    color: usize,
) -> f32 {
    let cy = ay.clamp(0, height as isize - 1);
    let cx = ax.clamp(0, width as isize - 1);
    let span_y = cfa.height as isize;
    let span_x = cfa.width as isize;
    let mut best = (isize::MAX, 0.0f32);
    for dy in -span_y..=span_y {
        for dx in -span_x..=span_x {
            let y = cy + dy;
            let x = cx + dx;
            if y < 0 || x < 0 || y >= height as isize || x >= width as isize {
                continue;
            }
            if cfa.color(y, x) != color {
                continue;
            }
            let d = (y - ay).pow(2) + (x - ax).pow(2);
            if d < best.0 {
                best = (d, mosaic[y as usize * width + x as usize]);
            }
        }
    }
    best.1
}

// ---------------------------------------------------------------------------
// Bayer
// ---------------------------------------------------------------------------

const EPS: f32 = 1e-5;
const EPS_SQ: f32 = 1e-10;

fn bayer_tile(tile: &Tile) -> Vec<[f32; 3]> {
    let w = tile.w;
    let h = tile.h;
    let cfa = &tile.data;
    let col = &tile.colors;
    let n = w * h;
    let (w1, w2, w3, w4) = (w, 2 * w, 3 * w, 4 * w);

    let mut rgb = vec![[0.0f32; 3]; n];
    for i in 0..n {
        rgb[i][col[i] as usize] = cfa[i];
    }

    // Direction: squared high-pass along each axis. The kernel
    // [1, -3, -1, 6, -1, -3, 1] sums to zero on both colours of a Bayer
    // line, so flat colour areas give zero whatever their hue.
    let mut hp_v = vec![0.0f32; n];
    let mut hp_h = vec![0.0f32; n];
    for r in 3..h - 3 {
        for c in 3..w - 3 {
            let i = r * w + c;
            let v = cfa[i - w3] - cfa[i - w1] - cfa[i + w1] + cfa[i + w3]
                - 3.0 * (cfa[i - w2] + cfa[i + w2])
                + 6.0 * cfa[i];
            let hz = cfa[i - 3] - cfa[i - 1] - cfa[i + 1] + cfa[i + 3]
                - 3.0 * (cfa[i - 2] + cfa[i + 2])
                + 6.0 * cfa[i];
            hp_v[i] = v * v;
            hp_h[i] = hz * hz;
        }
    }
    let mut vh = vec![0.5f32; n];
    for r in 4..h - 4 {
        for c in 4..w - 4 {
            let i = r * w + c;
            let v = EPS_SQ + hp_v[i - w1] + hp_v[i] + hp_v[i + w1];
            let hz = EPS_SQ + hp_h[i - 1] + hp_h[i] + hp_h[i + 1];
            vh[i] = v / (v + hz);
        }
    }

    // Green at red and blue sites.
    for r in 5..h - 5 {
        for c in 5..w - 5 {
            let i = r * w + c;
            if col[i] == 1 {
                continue;
            }
            let ci = cfa[i];
            let n_grad = EPS
                + (cfa[i - w1] - cfa[i + w1]).abs()
                + (ci - cfa[i - w2]).abs()
                + (cfa[i - w1] - cfa[i - w3]).abs()
                + (cfa[i - w2] - cfa[i - w4]).abs();
            let s_grad = EPS
                + (cfa[i + w1] - cfa[i - w1]).abs()
                + (ci - cfa[i + w2]).abs()
                + (cfa[i + w1] - cfa[i + w3]).abs()
                + (cfa[i + w2] - cfa[i + w4]).abs();
            let w_grad = EPS
                + (cfa[i - 1] - cfa[i + 1]).abs()
                + (ci - cfa[i - 2]).abs()
                + (cfa[i - 1] - cfa[i - 3]).abs()
                + (cfa[i - 2] - cfa[i - 4]).abs();
            let e_grad = EPS
                + (cfa[i + 1] - cfa[i - 1]).abs()
                + (ci - cfa[i + 2]).abs()
                + (cfa[i + 1] - cfa[i + 3]).abs()
                + (cfa[i + 2] - cfa[i + 4]).abs();

            // One-sided estimates: the neighbouring green corrected by half
            // the own colour's change towards it (colour differences are
            // smooth on the encoded data).
            let n_est = cfa[i - w1] + 0.5 * (ci - cfa[i - w2]);
            let s_est = cfa[i + w1] + 0.5 * (ci - cfa[i + w2]);
            let w_est = cfa[i - 1] + 0.5 * (ci - cfa[i - 2]);
            let e_est = cfa[i + 1] + 0.5 * (ci - cfa[i + 2]);

            let v_est = (s_grad * n_est + n_grad * s_est) / (n_grad + s_grad);
            let h_est = (w_grad * e_est + e_grad * w_est) / (e_grad + w_grad);

            let central = vh[i];
            let around = 0.25 * (vh[i - w1 - 1] + vh[i - w1 + 1] + vh[i + w1 - 1] + vh[i + w1 + 1]);
            let disc = if (0.5 - central).abs() < (0.5 - around).abs() {
                around
            } else {
                central
            };
            rgb[i][1] = (h_est * disc + v_est * (1.0 - disc)).max(0.0);
        }
    }

    // Diagonal direction at red/blue sites.
    let mut hp_p = vec![0.0f32; n];
    let mut hp_q = vec![0.0f32; n];
    for r in 3..h - 3 {
        for c in 3..w - 3 {
            let i = r * w + c;
            let p = cfa[i - w3 - 3] - cfa[i - w1 - 1] - cfa[i + w1 + 1] + cfa[i + w3 + 3]
                - 3.0 * (cfa[i - w2 - 2] + cfa[i + w2 + 2])
                + 6.0 * cfa[i];
            let q = cfa[i - w3 + 3] - cfa[i - w1 + 1] - cfa[i + w1 - 1] + cfa[i + w3 - 3]
                - 3.0 * (cfa[i - w2 + 2] + cfa[i + w2 - 2])
                + 6.0 * cfa[i];
            hp_p[i] = p * p;
            hp_q[i] = q * q;
        }
    }
    let mut pq = vec![0.5f32; n];
    for r in 4..h - 4 {
        for c in 4..w - 4 {
            let i = r * w + c;
            let p = EPS_SQ + hp_p[i - w1 - 1] + hp_p[i] + hp_p[i + w1 + 1];
            let q = EPS_SQ + hp_q[i - w1 + 1] + hp_q[i] + hp_q[i + w1 - 1];
            pq[i] = p / (p + q);
        }
    }

    // Red at blue sites and blue at red sites, along the diagonals.
    for r in 7..h - 7 {
        for c in 7..w - 7 {
            let i = r * w + c;
            let own = col[i] as usize;
            if own == 1 {
                continue;
            }
            let x = 2 - own;
            let g = |k: usize| rgb[k][1];
            let v = |k: usize| rgb[k][x];
            let (nw, ne, sw, se) = (i - w1 - 1, i - w1 + 1, i + w1 - 1, i + w1 + 1);
            let nw_grad = EPS
                + (v(nw) - v(se)).abs()
                + (v(nw) - v(i - w3 - 3)).abs()
                + (g(i) - g(i - w2 - 2)).abs();
            let ne_grad = EPS
                + (v(ne) - v(sw)).abs()
                + (v(ne) - v(i - w3 + 3)).abs()
                + (g(i) - g(i - w2 + 2)).abs();
            let sw_grad = EPS
                + (v(sw) - v(ne)).abs()
                + (v(sw) - v(i + w3 - 3)).abs()
                + (g(i) - g(i + w2 - 2)).abs();
            let se_grad = EPS
                + (v(se) - v(nw)).abs()
                + (v(se) - v(i + w3 + 3)).abs()
                + (g(i) - g(i + w2 + 2)).abs();
            let p_est =
                (nw_grad * (v(se) - g(se)) + se_grad * (v(nw) - g(nw))) / (nw_grad + se_grad);
            let q_est =
                (ne_grad * (v(sw) - g(sw)) + sw_grad * (v(ne) - g(ne))) / (ne_grad + sw_grad);
            let central = pq[i];
            let around = 0.25 * (pq[nw] + pq[ne] + pq[sw] + pq[se]);
            let disc = if (0.5 - central).abs() < (0.5 - around).abs() {
                around
            } else {
                central
            };
            rgb[i][x] = (g(i) + (1.0 - disc) * p_est + disc * q_est).max(0.0);
        }
    }

    // Red and blue at green sites, along the axes.
    for r in 10..h - 10 {
        for c in 10..w - 10 {
            let i = r * w + c;
            if col[i] != 1 {
                continue;
            }
            let central = vh[i];
            let around = 0.25 * (vh[i - w1 - 1] + vh[i - w1 + 1] + vh[i + w1 - 1] + vh[i + w1 + 1]);
            let disc = if (0.5 - central).abs() < (0.5 - around).abs() {
                around
            } else {
                central
            };
            let gi = rgb[i][1];
            for x in [0usize, 2] {
                let v = |k: usize| rgb[k][x];
                let g = |k: usize| rgb[k][1];
                let n_grad = EPS
                    + (gi - g(i - w2)).abs()
                    + (v(i - w1) - v(i + w1)).abs()
                    + (v(i - w1) - v(i - w3)).abs();
                let s_grad = EPS
                    + (gi - g(i + w2)).abs()
                    + (v(i + w1) - v(i - w1)).abs()
                    + (v(i + w1) - v(i + w3)).abs();
                let w_grad = EPS
                    + (gi - g(i - 2)).abs()
                    + (v(i - 1) - v(i + 1)).abs()
                    + (v(i - 1) - v(i - 3)).abs();
                let e_grad = EPS
                    + (gi - g(i + 2)).abs()
                    + (v(i + 1) - v(i - 1)).abs()
                    + (v(i + 1) - v(i + 3)).abs();
                let n_est = v(i - w1) - g(i - w1);
                let s_est = v(i + w1) - g(i + w1);
                let w_est = v(i - 1) - g(i - 1);
                let e_est = v(i + 1) - g(i + 1);
                let v_est = (n_grad * s_est + s_grad * n_est) / (n_grad + s_grad);
                let h_est = (e_grad * w_est + w_grad * e_est) / (e_grad + w_grad);
                rgb[i][x] = (gi + (1.0 - disc) * v_est + disc * h_est).max(0.0);
            }
        }
    }
    rgb
}

// ---------------------------------------------------------------------------
// X-Trans and other RGB patterns
// ---------------------------------------------------------------------------

/// Per direction, for red and blue: (row offset, column offset, geometric
/// weight).
pub type PhaseTaps = [[Vec<(isize, isize, f32)>; 2]; 4];

// One refinement of green and colour (two measured 80 ms more on 40 MP for
// 3-9 % less false colour, which stays about half of Capture One's and
// Camera Raw's; notes: PROGRESS.md 2026-10-07).
const REFINE_PASSES: usize = 1;
const CHROMA_A: f32 = 2.0;
const EDGE_K: f32 = 0.2;
const CHROMA_B: f32 = 1.0;
const DIRECTIONS: [(isize, isize); 4] = [(0, 1), (1, 0), (1, 1), (1, -1)];

/// Per pattern phase and direction: where the nearest greens and the
/// nearest samples of the pixel's own colour lie along the line, and which
/// samples of each colour surround the pixel.
pub struct DirectionalTables {
    pub period_w: usize,
    pub period_h: usize,
    /// [phase][direction]
    pub green: Vec<[Option<GreenLine>; 4]>,
    /// [phase][colour]: offsets (dy, dx) of that colour within ±2.
    neighbours: Vec<[Vec<(isize, isize)>; 3]>,
}

#[derive(Clone, Copy)]
pub struct GreenLine {
    /// Distances (in steps) to the nearest green before and after.
    pub a: isize,
    pub b: isize,
    /// Distances to the nearest own-colour sample before and after.
    pub own: Option<(isize, isize)>,
}

impl DirectionalTables {
    pub fn new(cfa: &CfaPattern) -> Self {
        // A period that is a multiple of both pattern sizes keeps lookups
        // simple for odd layouts too.
        let period_w = cfa.width;
        let period_h = cfa.height;
        let mut green = Vec::with_capacity(period_w * period_h);
        let mut neighbours = Vec::with_capacity(period_w * period_h);
        for r in 0..period_h as isize {
            for c in 0..period_w as isize {
                let own = cfa.color(r, c);
                let mut lines = [None; 4];
                if own != 1 {
                    for (d, &(dy, dx)) in DIRECTIONS.iter().enumerate() {
                        let find = |sign: isize, color: usize, max: isize| {
                            (1..=max)
                                .find(|&k| cfa.color(r + sign * k * dy, c + sign * k * dx) == color)
                        };
                        if let (Some(a), Some(b)) = (find(-1, 1, 3), find(1, 1, 3)) {
                            let own_line = match (find(-1, own, 6), find(1, own, 6)) {
                                (Some(s1), Some(s2)) => Some((s1, s2)),
                                _ => None,
                            };
                            lines[d] = Some(GreenLine {
                                a,
                                b,
                                own: own_line,
                            });
                        }
                    }
                }
                green.push(lines);
                let mut by_color: [Vec<(isize, isize)>; 3] = Default::default();
                for dy in -2..=2isize {
                    for dx in -2..=2isize {
                        if dy == 0 && dx == 0 {
                            continue;
                        }
                        by_color[cfa.color(r + dy, c + dx)].push((dy, dx));
                    }
                }
                neighbours.push(by_color);
            }
        }
        Self {
            period_w,
            period_h,
            green,
            neighbours,
        }
    }

    #[inline]
    fn phase(&self, row: isize, col: isize) -> usize {
        row.rem_euclid(self.period_h as isize) as usize * self.period_w
            + col.rem_euclid(self.period_w as isize) as usize
    }
}

/// Per phase, direction and colour slot (red, blue): the buffer offsets of
/// that colour within ±2 and their weights, higher along the direction than
/// across it.
pub fn phase_taps(tables: &DirectionalTables) -> Vec<PhaseTaps> {
    tables
        .neighbours
        .iter()
        .map(|by_color| {
            std::array::from_fn(|d| {
                let (dy, dx) = DIRECTIONS[d];
                let norm = ((dy * dy + dx * dx) as f32).sqrt();
                let (uy, ux) = (dy as f32 / norm, dx as f32 / norm);
                std::array::from_fn(|slot| {
                    by_color[if slot == 0 { 0 } else { 2 }]
                        .iter()
                        .map(|&(oy, ox)| {
                            let along = oy as f32 * uy + ox as f32 * ux;
                            let across = oy as f32 * ux - ox as f32 * uy;
                            (oy, ox, 1.0 / (0.5 + along * along + 4.0 * across * across))
                        })
                        .collect()
                })
            })
        })
        .collect()
}

fn directional_tile(tile: &Tile, tables: &DirectionalTables) -> Vec<[f32; 3]> {
    let w = tile.w;
    let h = tile.h;
    let n = w * h;
    let data = &tile.data;
    let col = &tile.colors;
    let phase = |r: usize, c: usize| tables.phase(tile.top + r as isize, tile.left + c as isize);

    // 1. Green along each direction.
    let mut greens: [Vec<f32>; 4] = std::array::from_fn(|_| data.clone());
    for r in 3..h - 3 {
        for c in 3..w - 3 {
            let i = r * w + c;
            if col[i] == 1 {
                continue;
            }
            // Clamp to the greens around the pixel.
            let mut lo = f32::MAX;
            let mut hi = f32::MIN;
            for dy in -1..=1isize {
                for dx in -1..=1isize {
                    let k = (i as isize + dy * w as isize + dx) as usize;
                    if col[k] == 1 {
                        lo = lo.min(data[k]);
                        hi = hi.max(data[k]);
                    }
                }
            }
            let lines = &tables.green[phase(r, c)];
            let mut fallback = None;
            for (d, &(dy, dx)) in DIRECTIONS.iter().enumerate() {
                let step = dy * w as isize + dx;
                let at = |k: isize| data[(i as isize + k * step) as usize];
                let value = match lines[d] {
                    Some(line)
                        if r as isize >= 6
                            && (r as isize) < h as isize - 6
                            && c as isize >= 6
                            && (c as isize) < w as isize - 6 =>
                    {
                        let (a, b) = (line.a as f32, line.b as f32);
                        let ga = at(-line.a);
                        let gb = at(line.b);
                        let mut g = (b * ga + a * gb) / (a + b);
                        if let Some((s1, s2)) = line.own {
                            let (f1, f2) = (s1 as f32, s2 as f32);
                            let own_interp = (f2 * at(-s1) + f1 * at(s2)) / (f1 + f2);
                            g += a * b / (f1 * f2) * (data[i] - own_interp);
                        }
                        Some(g)
                    }
                    _ => None,
                };
                match value {
                    Some(g) => {
                        greens[d][i] = if lo <= hi {
                            g.clamp(lo, hi)
                        } else {
                            g.max(0.0)
                        }
                    }
                    None => {
                        let g =
                            *fallback.get_or_insert(if lo <= hi { 0.5 * (lo + hi) } else { 0.0 });
                        greens[d][i] = g;
                    }
                }
            }
        }
    }

    // 2. Red and blue per direction from colour differences. Per pattern
    // phase, direction and colour: the buffer offsets of that colour within
    // ±2 and their weights, higher along the direction than across it.
    // Per phase, direction and slot: (row offset, column offset, weight).
    let taps = phase_taps(tables);
    let phases: Vec<u8> = (0..n).map(|i| phase(i / w, i % w) as u8).collect();
    // Pixels one pattern period apart share their taps. Rows are split by
    // column phase (`deinterleave`) so that those pixels, and each of their
    // taps, lie next to each other and the loops below run as vector
    // instructions; every pixel still performs exactly the scalar
    // operations in the same order (bit-identical output).
    let period = tables.period_w;
    let w_planar = w.div_ceil(period);
    let deinterleave = |src: &[f32], out: &mut Vec<f32>| {
        out.clear();
        out.resize(h * period * w_planar, 0.0);
        for r in 0..h {
            let row = &src[r * w..(r + 1) * w];
            for q in 0..period.min(w) {
                let dst = &mut out[(r * period + q) * w_planar..][..w_planar];
                for (d, &v) in dst.iter_mut().zip(row[q..].iter().step_by(period)) {
                    *d = v;
                }
            }
        }
    };
    let mut data_planar = Vec::new();
    deinterleave(data, &mut data_planar);
    let mut green_planar = Vec::new();
    let mut sum = vec![0.0f32; w_planar];
    let mut weight = vec![0.0f32; w_planar];
    // One output row, split by column phase like the inputs.
    let mut row_out = vec![[0.0f32; 3]; period * w_planar];
    let mut rb_pass = |greens: &[Vec<f32>; 4], candidates: &mut [Vec<[f32; 3]>; 4]| {
        for (d, out) in candidates.iter_mut().enumerate() {
            deinterleave(&greens[d], &mut green_planar);
            let gp = &green_planar;
            let dp = &data_planar;
            for r in 2..h - 2 {
                for q in 0..period.min(w) {
                    let row_out = &mut row_out[q * w_planar..(q + 1) * w_planar];
                    // Columns c = cc · period + q with 2 <= c < w - 2.
                    let start = if q >= 2 { 0 } else { (2 - q).div_ceil(period) };
                    let end = if w - 2 > q {
                        (w - 2 - q).div_ceil(period)
                    } else {
                        0
                    };
                    if start >= end {
                        continue;
                    }
                    let m = end - start;
                    let own = col[r * w + q] as usize;
                    let phase_taps = &taps[phases[r * w + q] as usize][d];
                    let row = (r * period + q) * w_planar + start;
                    let gi = &gp[row..row + m];
                    for (slot, x) in [0usize, 2].into_iter().enumerate() {
                        if x == own {
                            let own_values = &dp[row..row + m];
                            for (o, &v) in row_out[start..end].iter_mut().zip(own_values) {
                                o[x] = v;
                            }
                            continue;
                        }
                        let (sum, weight) = (&mut sum[..m], &mut weight[..m]);
                        sum.fill(0.0);
                        weight.fill(0.0);
                        for &(oy, ox, geometric) in &phase_taps[slot] {
                            let shifted = q as isize + ox;
                            let tap_q = shifted.rem_euclid(period as isize) as usize;
                            let tap_cc = start as isize + shifted.div_euclid(period as isize);
                            let at = ((r as isize + oy) as usize * period + tap_q) * w_planar
                                + tap_cc as usize;
                            let gk = &gp[at..at + m];
                            let dk = &dp[at..at + m];
                            for ((((s, wt), &gk), &dk), &gi) in sum
                                .iter_mut()
                                .zip(weight.iter_mut())
                                .zip(gk)
                                .zip(dk)
                                .zip(gi)
                            {
                                // Samples whose green differs from the pixel's
                                // count less, so colour does not bleed across
                                // edges: weight / (1 + (Δg / (0.2 (g + gᵢ)))²).
                                let scale = EDGE_K * (gk + gi) + 1e-4;
                                let dg = gk - gi;
                                let wgt = geometric * scale * scale / (scale * scale + dg * dg);
                                *s += wgt * (dk - gk);
                                *wt += wgt;
                            }
                        }
                        for (((o, &s), &wt), &g) in row_out[start..end]
                            .iter_mut()
                            .zip(sum.iter())
                            .zip(weight.iter())
                            .zip(gi)
                        {
                            o[x] = if wt > 0.0 { (g + s / wt).max(0.0) } else { g };
                        }
                    }
                    for (o, &g) in row_out[start..end].iter_mut().zip(gi) {
                        o[1] = g;
                    }
                }
                // Back to pixel order: columns 2 .. w - 2 of this row.
                let dst = &mut out[r * w..(r + 1) * w];
                for q in 0..period.min(w) {
                    let src = &row_out[q * w_planar..(q + 1) * w_planar];
                    for (c, v) in (q..w).step_by(period).zip(src) {
                        if c >= 2 && c < w - 2 {
                            dst[c] = *v;
                        }
                    }
                }
            }
        }
    };
    let mut candidates: [Vec<[f32; 3]>; 4] = std::array::from_fn(|_| vec![[0.0f32; 3]; n]);
    rb_pass(&greens, &mut candidates);
    // Green again from colour differences at the nearest greens along each
    // direction, now that those carry red and blue; then red and blue again.
    for _ in 0..REFINE_PASSES {
        for (d, &(dy, dx)) in DIRECTIONS.iter().enumerate() {
            let step = dy * w as isize + dx;
            for r in 8..h - 8 {
                for c in 8..w - 8 {
                    let i = r * w + c;
                    let own = col[i] as usize;
                    if own == 1 {
                        continue;
                    }
                    let Some(line) = tables.green[phases[i] as usize][d] else {
                        continue;
                    };
                    let qa = (i as isize - line.a * step) as usize;
                    let qb = (i as isize + line.b * step) as usize;
                    let (a, b) = (line.a as f32, line.b as f32);
                    let da = data[qa] - candidates[d][qa][own];
                    let db = data[qb] - candidates[d][qb][own];
                    greens[d][i] = (data[i] + (b * da + a * db) / (a + b)).max(0.0);
                }
            }
        }
        rb_pass(&greens, &mut candidates);
    }

    // 3. Smoothness of each candidate along its direction, in a
    // perceptual space; per pixel keep the smoothest directions.
    let mut lab = vec![[0.0f32; 3]; n];
    let mut roughness: [Vec<f32>; 4] = std::array::from_fn(|_| vec![0.0f32; n]);
    for (d, &(dy, dx)) in DIRECTIONS.iter().enumerate() {
        let cand = &candidates[d];
        for r in 2..h - 2 {
            for c in 2..w - 2 {
                let i = r * w + c;
                let [pr, pg, pb] = cand[i];
                // The values are square-root encoded, close enough to
                // perceptual lightness; chroma weighted as in CIELab.
                lab[i] = [
                    0.25 * pr + 0.5 * pg + 0.25 * pb,
                    CHROMA_A * (pr - pg),
                    CHROMA_B * (pg - pb),
                ];
            }
        }
        let step = dy * w as isize + dx;
        let rough = &mut roughness[d];
        for r in 3..h - 3 {
            for c in 3..w - 3 {
                let i = r * w + c;
                let a = lab[(i as isize - step) as usize];
                let b = lab[(i as isize + step) as usize];
                let m = lab[i];
                let dl = 2.0 * m[0] - a[0] - b[0];
                let da = 2.0 * m[1] - a[1] - b[1];
                let db = 2.0 * m[2] - a[2] - b[2];
                rough[i] = dl * dl + da * da + db * db;
            }
        }
    }
    // Horizontal triples, summed left to right as the 3×3 window below
    // used to (same rounding, a third of the loads).
    let triples: [Vec<f32>; 4] = std::array::from_fn(|d| {
        let rough = &roughness[d];
        let mut t = vec![0.0f32; n];
        for r in 0..h {
            for c in 1..w - 1 {
                let i = r * w + c;
                t[i] = rough[i - 1] + rough[i] + rough[i + 1];
            }
        }
        t
    });

    let mut rgb = vec![[0.0f32; 3]; n];
    for r in 0..h {
        for c in 0..w {
            let i = r * w + c;
            if r < 6 || c < 6 || r >= h - 6 || c >= w - 6 {
                rgb[i] = candidates[0][i];
                continue;
            }
            let mut sums = [0.0f32; 4];
            for (d, triple) in triples.iter().enumerate() {
                let mut s = 0.0;
                for dy in -1..=1isize {
                    s += triple[(i as isize + dy * w as isize) as usize];
                }
                sums[d] = s;
            }
            let best = sums.iter().cloned().fold(f32::MAX, f32::min);
            let limit = best * 1.5 + 1e-6;
            let mut acc = [0.0f32; 3];
            let mut count = 0.0f32;
            for d in 0..4 {
                if sums[d] <= limit {
                    for k in 0..3 {
                        acc[k] += candidates[d][i][k];
                    }
                    count += 1.0;
                }
            }
            rgb[i] = [acc[0] / count, acc[1] / count, acc[2] / count];
            // Keep measured samples exact.
            let own = col[i] as usize;
            rgb[i][own] = data[i];
        }
    }
    rgb
}

#[cfg(test)]
mod tests {
    #[test]
    fn fine_grain_smoothing_keeps_edges_and_lowers_noise() {
        let (w, h) = (64, 64);
        let mut state = 12345u32;
        let mut noise = || {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (state >> 8) as f32 / (1u32 << 24) as f32 - 0.5
        };
        // Left half 0.1, right half 0.4, with 2 % pixel noise.
        let mut rgb: Vec<[f32; 3]> = (0..w * h)
            .map(|i| {
                let base = if i % w < w / 2 { 0.1 } else { 0.4 };
                let v = base * (1.0 + 0.04 * noise());
                [v, v, v]
            })
            .collect();
        let spread = |rgb: &[[f32; 3]]| {
            let vals: Vec<f32> = (8..56)
                .flat_map(|r| (4..24).map(move |c| r * w + c))
                .map(|i| rgb[i][1])
                .collect();
            let mean = vals.iter().sum::<f32>() / vals.len() as f32;
            (vals.iter().map(|v| (v - mean).powi(2)).sum::<f32>() / vals.len() as f32).sqrt()
        };
        let before = spread(&rgb);
        super::smooth_fine_grain(&mut rgb, w, h, &|| false).unwrap();
        assert!(spread(&rgb) < 0.8 * before, "{} vs {before}", spread(&rgb));
        // The edge between the halves stays sharp.
        let left = rgb[32 * w + w / 2 - 1][1];
        let right = rgb[32 * w + w / 2][1];
        assert!(left < 0.12 && right > 0.38, "{left} {right}");
    }

    use super::*;

    fn bayer() -> CfaPattern {
        CfaPattern::new(2, 2, vec![0, 1, 1, 2]).unwrap()
    }

    fn xtrans() -> CfaPattern {
        let rows = ["GGRGGB", "GGBGGR", "BRGRBG", "GGBGGR", "GGRGGB", "RBGBRG"];
        let colors = rows
            .iter()
            .flat_map(|row| {
                row.chars().map(|ch| match ch {
                    'R' => 0u8,
                    'G' => 1,
                    _ => 2,
                })
            })
            .collect();
        CfaPattern::new(6, 6, colors).unwrap()
    }

    fn mosaic_of(
        width: usize,
        height: usize,
        cfa: &CfaPattern,
        f: impl Fn(usize, usize) -> [f32; 3],
    ) -> Vec<f32> {
        let mut out = vec![0.0; width * height];
        for y in 0..height {
            for x in 0..width {
                out[y * width + x] = f(y, x)[cfa.color(y as isize, x as isize)];
            }
        }
        out
    }

    /// Timing on a 40 MP X-Trans and a 45 MP Bayer mosaic (run with
    /// `cargo test --release -p rawtonic-app --lib demosaic_speed -- --ignored --nocapture`),
    /// plus a checksum to confirm optimisations leave the output unchanged.
    #[test]
    #[ignore]
    fn demosaic_speed() {
        for (name, cfa, w, h) in [
            ("x-trans", xtrans(), 7752, 5178),
            ("bayer", bayer(), 8280, 5520),
        ] {
            let mosaic = mosaic_of(w, h, &cfa, |y, x| {
                let t = ((x * 7 + y * 13) % 97) as f32 / 97.0;
                [
                    0.1 + 0.6 * t,
                    0.2 + 0.3 * ((y % 31) as f32 / 31.0),
                    0.05 + 0.4 * ((x % 17) as f32 / 17.0),
                ]
            });
            let _ = demosaic(&mosaic[..w * 64], w, 64, &cfa);
            let started = std::time::Instant::now();
            let rgb = demosaic(&mosaic, w, h, &cfa);
            let ms = started.elapsed().as_secs_f64() * 1000.0;
            let sum = rgb.iter().flatten().fold(0xcbf29ce484222325u64, |h, v| {
                (h ^ v.to_bits() as u64).wrapping_mul(0x100000001b3)
            });
            println!("{name}: {ms:.0} ms, checksum {sum:016x}");
        }
    }

    #[test]
    fn flat_colour_is_reproduced_exactly_enough() {
        for cfa in [bayer(), xtrans()] {
            let (w, h) = (437, 301);
            let colour = [0.31, 0.52, 0.17];
            let mosaic = mosaic_of(w, h, &cfa, |_, _| colour);
            let rgb = demosaic(&mosaic, w, h, &cfa);
            for px in &rgb {
                for k in 0..3 {
                    assert!((px[k] - colour[k]).abs() < 1e-3, "{px:?}");
                }
            }
        }
    }

    /// A cancel raised during the demosaic stops it within the tiles and
    /// bands already started (a prefetch gives way to a slider drag).
    #[test]
    fn a_cancel_stops_the_demosaic_between_tiles() {
        use std::sync::atomic::AtomicUsize;
        for cfa in [bayer(), xtrans()] {
            // 32 tiles, more than the threads that may be in flight.
            let (w, h) = (8 * TILE, 4 * TILE);
            let mosaic = mosaic_of(w, h, &cfa, |y, x| {
                [
                    0.1 + (x % 13) as f32 * 0.03,
                    0.4,
                    0.2 + (y % 7) as f32 * 0.05,
                ]
            });
            let asked = AtomicUsize::new(0);
            // Wanted for the first two tiles only.
            let cancelled = || asked.fetch_add(1, Ordering::SeqCst) >= 2;
            assert_eq!(
                demosaic_unless(&mosaic, w, h, &cfa, &cancelled),
                Err(Cancelled)
            );
            // Tiles skip without asking once one was told to stop; those
            // asking meanwhile on other threads stop too.
            let asked = asked.load(Ordering::SeqCst);
            assert!(
                asked <= 2 + rayon::current_num_threads(),
                "asked {asked} times"
            );
        }
        let (w, h) = (64, 3 * 64);
        // Grain, so the smoothing has work to do.
        let mut rgb: Vec<[f32; 3]> = (0..w * h)
            .map(|i| [0.2, 0.3 + (i * 7919 % 101) as f32 * 1e-3, 0.4])
            .collect();
        assert_eq!(smooth_fine_grain(&mut rgb, w, h, &|| true), Err(Cancelled));
    }

    #[test]
    fn smooth_gradient_has_small_error() {
        for cfa in [bayer(), xtrans()] {
            let (w, h) = (400, 260);
            let f = |y: usize, x: usize| {
                let t = x as f32 / w as f32;
                let s = y as f32 / h as f32;
                [0.2 + 0.5 * t, 0.3 + 0.3 * s, 0.6 - 0.4 * t * s]
            };
            let mosaic = mosaic_of(w, h, &cfa, f);
            let rgb = demosaic(&mosaic, w, h, &cfa);
            let mut worst = 0.0f32;
            for y in 0..h {
                for x in 0..w {
                    let want = f(y, x);
                    for k in 0..3 {
                        worst = worst.max((rgb[y * w + x][k] - want[k]).abs());
                    }
                }
            }
            assert!(worst < 0.01, "worst {worst}");
        }
    }
}
