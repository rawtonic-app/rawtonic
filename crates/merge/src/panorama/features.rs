//! Scale-invariant keypoints for registering overlapping photos: extrema of
//! a difference-of-Gaussians scale space with sub-pixel refinement, a
//! dominant gradient orientation each, and a 4×4×8 gradient histogram
//! descriptor (the classic SIFT construction, our implementation), compared
//! as RootSIFT (square roots of L1-normalised histograms) so that brightness
//! and moderate exposure differences between frames do not matter.
use crate::planes::{Plane, downsample, gaussian_blur};
use rayon::prelude::*;

#[derive(Clone, Debug)]
pub struct Keypoint {
    /// Position in the input image's pixels.
    pub x: f32,
    pub y: f32,
    pub scale: f32,
    pub angle: f32,
    pub descriptor: [f32; 128],
}

const SCALES_PER_OCTAVE: usize = 3;
const BASE_SIGMA: f32 = 1.6;
const CONTRAST_THRESHOLD: f32 = 0.012;
const EDGE_RATIO: f32 = 10.0;

/// Keypoints of a single-channel image (any linear range; it is compressed
/// with a logarithm first so dark and bright frames give the same points).
pub fn detect(luma: &Plane, max_keypoints: usize) -> Vec<Keypoint> {
    // Perceptual domain: log keeps the same structure under exposure changes.
    let mean = luma.mean().max(1e-6) as f32;
    let base = luma.map(|v| ((v.max(0.0) / mean) + 0.02).ln() * 0.25);
    let octaves = {
        let mut n = 0;
        let mut s = base.w.min(base.h);
        while s >= 48 && n < 6 {
            s /= 2;
            n += 1;
        }
        n.max(1)
    };
    let k = 2f32.powf(1.0 / SCALES_PER_OCTAVE as f32);
    let mut found: Vec<Keypoint> = Vec::new();
    let mut octave_base = gaussian_blur(&base, (BASE_SIGMA * BASE_SIGMA - 0.25).sqrt());
    for octave in 0..octaves {
        // Gaussians at sigma0 · k^i, i = 0..S+2.
        let mut gauss = vec![octave_base.clone()];
        let mut sigma_prev = BASE_SIGMA;
        for i in 1..SCALES_PER_OCTAVE + 3 {
            let sigma_total = BASE_SIGMA * k.powi(i as i32);
            let inc = (sigma_total * sigma_total - sigma_prev * sigma_prev).sqrt();
            let next = gaussian_blur(gauss.last().unwrap(), inc);
            gauss.push(next);
            sigma_prev = sigma_total;
        }
        let dog: Vec<Plane> = gauss
            .windows(2)
            .map(|p| {
                let mut d = Plane::new(p[0].w, p[0].h);
                d.data
                    .iter_mut()
                    .zip(p[1].data.iter().zip(&p[0].data))
                    .for_each(|(o, (b, a))| *o = b - a);
                d
            })
            .collect();
        let grads: Vec<Grad> = gauss.iter().map(Grad::new).collect();
        let scale_to_input = 2f32.powi(octave);
        for s in 1..=SCALES_PER_OCTAVE {
            let (w, h) = (dog[s].w, dog[s].h);
            let candidates: Vec<(usize, usize)> = (5..h.saturating_sub(5))
                .into_par_iter()
                .flat_map_iter(|y| {
                    let mut row = Vec::new();
                    for x in 5..w.saturating_sub(5) {
                        let v = dog[s].at(x, y);
                        if v.abs() < 0.5 * CONTRAST_THRESHOLD / SCALES_PER_OCTAVE as f32 {
                            continue;
                        }
                        if is_extremum(&dog, s, x, y, v) {
                            row.push((x, y));
                        }
                    }
                    row.into_iter()
                })
                .collect();
            let points: Vec<Keypoint> = candidates
                .into_par_iter()
                .filter_map(|(x, y)| {
                    let (dx, dy, ds, value) = refine(&dog, s, x, y)?;
                    if value.abs() < CONTRAST_THRESHOLD / SCALES_PER_OCTAVE as f32
                        || !passes_edge_test(&dog[s], x, y)
                    {
                        return None;
                    }
                    let px = x as f32 + dx;
                    let py = y as f32 + dy;
                    let sigma = BASE_SIGMA * k.powf(s as f32 + ds);
                    let grad = &grads[s];
                    let angle = orientation(grad, px, py, sigma)?;
                    let descriptor = describe(grad, px, py, sigma, angle)?;
                    Some(Keypoint {
                        x: (px + 0.5) * scale_to_input - 0.5,
                        y: (py + 0.5) * scale_to_input - 0.5,
                        scale: sigma * scale_to_input,
                        angle,
                        descriptor,
                    })
                })
                .collect();
            found.extend(points);
        }
        if octave + 1 < octaves {
            octave_base = downsample(&gauss[SCALES_PER_OCTAVE]);
        }
    }
    // Keep the strongest spread over the image: a coarse grid with a quota.
    if found.len() > max_keypoints {
        found.sort_by(|a, b| b.scale.total_cmp(&a.scale));
        let cells = 16usize;
        let quota = max_keypoints / (cells * cells) + 1;
        let mut counts = vec![0usize; cells * cells];
        let (w, h) = (luma.w as f32, luma.h as f32);
        let mut kept = Vec::with_capacity(max_keypoints);
        let mut rest = Vec::new();
        for kp in found {
            let c = ((kp.y / h * cells as f32) as usize).min(cells - 1) * cells
                + ((kp.x / w * cells as f32) as usize).min(cells - 1);
            if counts[c] < quota {
                counts[c] += 1;
                kept.push(kp);
            } else {
                rest.push(kp);
            }
        }
        kept.extend(
            rest.into_iter()
                .take(max_keypoints.saturating_sub(kept.len())),
        );
        kept.truncate(max_keypoints);
        found = kept;
    }
    found
}

/// Gradient magnitude and angle of one scale.
struct Grad {
    w: usize,
    h: usize,
    data: Vec<(f32, f32)>,
}

impl Grad {
    fn new(g: &Plane) -> Grad {
        let (w, h) = (g.w, g.h);
        let mut data = vec![(0.0f32, 0.0f32); w * h];
        data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
            for (x, o) in row.iter_mut().enumerate() {
                let dx = g.clamped(x as i64 + 1, y as i64) - g.clamped(x as i64 - 1, y as i64);
                let dy = g.clamped(x as i64, y as i64 + 1) - g.clamped(x as i64, y as i64 - 1);
                *o = ((dx * dx + dy * dy).sqrt(), dy.atan2(dx));
            }
        });
        Grad { w, h, data }
    }

    fn at(&self, x: i64, y: i64) -> Option<(f32, f32)> {
        (x >= 0 && y >= 0 && (x as usize) < self.w && (y as usize) < self.h)
            .then(|| self.data[y as usize * self.w + x as usize])
    }
}

fn is_extremum(dog: &[Plane], s: usize, x: usize, y: usize, v: f32) -> bool {
    let greater = v > 0.0;
    for ds in 0..3 {
        let p = &dog[s + ds - 1];
        for dy in 0..3 {
            for dx in 0..3 {
                if ds == 1 && dx == 1 && dy == 1 {
                    continue;
                }
                let n = p.at(x + dx - 1, y + dy - 1);
                if (greater && n >= v) || (!greater && n <= v) {
                    return false;
                }
            }
        }
    }
    true
}

/// Quadratic fit of the DoG around (x, y, s): offset and interpolated value.
fn refine(dog: &[Plane], s: usize, x: usize, y: usize) -> Option<(f32, f32, f32, f32)> {
    let d = |ds: i64, dx: i64, dy: i64| {
        dog[(s as i64 + ds) as usize].at((x as i64 + dx) as usize, (y as i64 + dy) as usize)
    };
    let g = nalgebra::Vector3::new(
        (d(0, 1, 0) - d(0, -1, 0)) * 0.5,
        (d(0, 0, 1) - d(0, 0, -1)) * 0.5,
        (d(1, 0, 0) - d(-1, 0, 0)) * 0.5,
    );
    let c = d(0, 0, 0);
    let dxx = d(0, 1, 0) + d(0, -1, 0) - 2.0 * c;
    let dyy = d(0, 0, 1) + d(0, 0, -1) - 2.0 * c;
    let dss = d(1, 0, 0) + d(-1, 0, 0) - 2.0 * c;
    let dxy = (d(0, 1, 1) - d(0, -1, 1) - d(0, 1, -1) + d(0, -1, -1)) * 0.25;
    let dxs = (d(1, 1, 0) - d(1, -1, 0) - d(-1, 1, 0) + d(-1, -1, 0)) * 0.25;
    let dys = (d(1, 0, 1) - d(1, 0, -1) - d(-1, 0, 1) + d(-1, 0, -1)) * 0.25;
    let hess = nalgebra::Matrix3::new(dxx, dxy, dxs, dxy, dyy, dys, dxs, dys, dss);
    let off = -(hess.try_inverse()? * g);
    if off.iter().any(|v| v.abs() > 0.6) {
        return None;
    }
    Some((off[0], off[1], off[2], c + 0.5 * g.dot(&off)))
}

fn passes_edge_test(p: &Plane, x: usize, y: usize) -> bool {
    let c = p.at(x, y);
    let dxx = p.at(x + 1, y) + p.at(x - 1, y) - 2.0 * c;
    let dyy = p.at(x, y + 1) + p.at(x, y - 1) - 2.0 * c;
    let dxy =
        (p.at(x + 1, y + 1) - p.at(x - 1, y + 1) - p.at(x + 1, y - 1) + p.at(x - 1, y - 1)) * 0.25;
    let tr = dxx + dyy;
    let det = dxx * dyy - dxy * dxy;
    det > 0.0 && tr * tr / det < (EDGE_RATIO + 1.0).powi(2) / EDGE_RATIO
}

/// The dominant gradient orientation around a keypoint (36-bin histogram
/// weighted by a Gaussian of 1.5 σ, parabolic peak interpolation).
fn orientation(grad: &Grad, x: f32, y: f32, sigma: f32) -> Option<f32> {
    let radius = (3.0 * 1.5 * sigma).round() as i64;
    let weight_sigma = 1.5 * sigma;
    let mut hist = [0.0f32; 36];
    let (cx, cy) = (x.round() as i64, y.round() as i64);
    for dy in -radius..=radius {
        for dx in -radius..=radius {
            let Some((m, a)) = grad.at(cx + dx, cy + dy) else {
                continue;
            };
            let wgt = (-((dx * dx + dy * dy) as f32) / (2.0 * weight_sigma * weight_sigma)).exp();
            let bin =
                (((a + std::f32::consts::PI) / std::f32::consts::TAU * 36.0) as usize).min(35);
            hist[bin] += wgt * m;
        }
    }
    // Smooth twice.
    for _ in 0..2 {
        let prev = hist;
        for i in 0..36 {
            hist[i] = 0.25 * prev[(i + 35) % 36] + 0.5 * prev[i] + 0.25 * prev[(i + 1) % 36];
        }
    }
    let (peak, &max) = hist.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1))?;
    if max <= 0.0 {
        return None;
    }
    let (l, r) = (hist[(peak + 35) % 36], hist[(peak + 1) % 36]);
    let offset = 0.5 * (l - r) / (l - 2.0 * max + r).min(-1e-12);
    let bin = peak as f32 + offset + 0.5;
    Some(bin / 36.0 * std::f32::consts::TAU - std::f32::consts::PI)
}

/// 4×4 cells × 8 orientations around the keypoint, rotated to `angle`,
/// trilinearly interpolated, clipped at 0.2 and returned as RootSIFT.
fn describe(grad: &Grad, x: f32, y: f32, sigma: f32, angle: f32) -> Option<[f32; 128]> {
    const D: usize = 4;
    const BINS: usize = 8;
    let cell = 3.0 * sigma;
    let radius = (cell * std::f32::consts::SQRT_2 * (D as f32 + 1.0) * 0.5).round() as i64;
    let (cos_t, sin_t) = (angle.cos(), angle.sin());
    let mut hist = [0.0f32; D * D * BINS];
    let (cx, cy) = (x.round() as i64, y.round() as i64);
    let weight_sigma = 0.5 * D as f32;
    for dy in -radius..=radius {
        for dx in -radius..=radius {
            // Rotated coordinates in cell units.
            let rx = (cos_t * dx as f32 + sin_t * dy as f32) / cell;
            let ry = (-sin_t * dx as f32 + cos_t * dy as f32) / cell;
            let bx = rx + D as f32 / 2.0 - 0.5;
            let by = ry + D as f32 / 2.0 - 0.5;
            if bx <= -1.0 || bx >= D as f32 || by <= -1.0 || by >= D as f32 {
                continue;
            }
            let Some((m, a)) = grad.at(cx + dx, cy + dy) else {
                continue;
            };
            let wgt = (-(rx * rx + ry * ry) / (2.0 * weight_sigma * weight_sigma)).exp() * m;
            let mut o = (a - angle) / std::f32::consts::TAU * BINS as f32;
            o = o.rem_euclid(BINS as f32);
            let (x0, y0, o0) = (bx.floor(), by.floor(), o.floor());
            let (fx, fy, fo) = (bx - x0, by - y0, o - o0);
            for (iy, wy) in [(y0 as i64, 1.0 - fy), (y0 as i64 + 1, fy)] {
                if iy < 0 || iy >= D as i64 {
                    continue;
                }
                for (ix, wx) in [(x0 as i64, 1.0 - fx), (x0 as i64 + 1, fx)] {
                    if ix < 0 || ix >= D as i64 {
                        continue;
                    }
                    for (io, wo) in [
                        (o0 as usize % BINS, 1.0 - fo),
                        ((o0 as usize + 1) % BINS, fo),
                    ] {
                        hist[(iy as usize * D + ix as usize) * BINS + io] += wgt * wy * wx * wo;
                    }
                }
            }
        }
    }
    let norm = hist.iter().map(|v| v * v).sum::<f32>().sqrt();
    if norm <= 1e-12 {
        return None;
    }
    for v in hist.iter_mut() {
        *v = (*v / norm).min(0.2);
    }
    let l1: f32 = hist.iter().sum();
    if l1 <= 1e-12 {
        return None;
    }
    for v in hist.iter_mut() {
        *v = (*v / l1).sqrt();
    }
    Some(hist)
}

/// Mutual nearest neighbours that pass the ratio test (`ratio` on the
/// distance to the second best match).
pub fn match_keypoints(a: &[Keypoint], b: &[Keypoint], ratio: f32) -> Vec<(usize, usize)> {
    let best = |from: &[Keypoint], to: &[Keypoint]| -> Vec<Option<(usize, f32, f32)>> {
        from.par_iter()
            .map(|p| {
                let (mut b1, mut d1, mut d2) = (usize::MAX, f32::MAX, f32::MAX);
                for (j, q) in to.iter().enumerate() {
                    // RootSIFT vectors have unit length: d² = 2 − 2·dot.
                    let dot: f32 = p
                        .descriptor
                        .iter()
                        .zip(&q.descriptor)
                        .map(|(x, y)| x * y)
                        .sum();
                    let d = 2.0 - 2.0 * dot;
                    if d < d1 {
                        d2 = d1;
                        d1 = d;
                        b1 = j;
                    } else if d < d2 {
                        d2 = d;
                    }
                }
                (b1 != usize::MAX).then_some((b1, d1, d2))
            })
            .collect()
    };
    let ab = best(a, b);
    let ba = best(b, a);
    ab.iter()
        .enumerate()
        .filter_map(|(i, m)| {
            let (j, d1, d2) = (*m)?;
            let back = ba[j]?.0;
            (back == i && d1.max(0.0).sqrt() < ratio * d2.max(0.0).sqrt()).then_some((i, j))
        })
        .collect()
}
