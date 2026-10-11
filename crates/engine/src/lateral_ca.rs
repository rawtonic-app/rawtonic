//! Automatic lateral chromatic aberration (Lightroom's Remove Chromatic
//! Aberration, Capture One's Chromatic Aberration analysis): red and blue
//! images that are slightly larger or smaller than the green one, measured
//! on the photo itself and corrected by scaling those channels about the
//! centre (the same correction a lens profile's TCA applies).
//!
//! The estimate looks at edges that run across the radius (where a radial
//! scale shows as a shift), measures the sub-pixel shift of red and blue
//! against green along the radius in each, and fits the shift as
//! proportional to the radius: a channel scaled by `1 + k` is shifted by
//! `k · r`.

use image::Rgb32FImage;
use rayon::prelude::*;

/// Long edge of the copy the estimate works on. A radial scale is the same
/// at every resolution; this keeps the cost low while the shifts at the
/// edges stay well above the estimate's sub-pixel noise.
const WORK_EDGE: u32 = 2400;
/// One measured patch: radius, red shift, blue shift (pixels along the
/// outward radius) and the patch's weight.
type Sample = (f32, f32, f32, f32);
/// Half the side of a measured patch, and the largest shift searched.
const HALF: i64 = 10;
const MAX_SHIFT: f32 = 3.0;

/// Lateral CA of a photo: red and blue are sampled at `r · scale(r)` about
/// the centre to line up with green, `scale = 1 + a + b·(r / half-diagonal)²`
/// (`a` the plain size difference, `b` how it grows towards the corners, as
/// on zoom compacts).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CaModel {
    pub red: (f32, f32),
    pub blue: (f32, f32),
}

impl CaModel {
    pub const NONE: CaModel = CaModel {
        red: (0.0, 0.0),
        blue: (0.0, 0.0),
    };
}

/// The photo's lateral CA, `None` when it has too few usable edges.
pub fn estimate(image: &Rgb32FImage) -> Option<CaModel> {
    let small = downscale(image, WORK_EDGE);
    let (w, h) = small.dimensions();
    let (cx, cy) = ((w as f32 - 1.0) / 2.0, (h as f32 - 1.0) / 2.0);
    let half_diagonal = (cx * cx + cy * cy).sqrt();
    let channel = |c: usize| -> Vec<f32> { small.pixels().map(|p| p[c].max(0.0).sqrt()).collect() };
    let (r, g, b) = (channel(0), channel(1), channel(2));
    let at = |plane: &[f32], x: f32, y: f32| -> f32 {
        let x = x.clamp(0.0, w as f32 - 1.001);
        let y = y.clamp(0.0, h as f32 - 1.001);
        let (x0, y0) = (x.floor() as usize, y.floor() as usize);
        let (fx, fy) = (x - x0 as f32, y - y0 as f32);
        let i = y0 * w as usize + x0;
        let wi = w as usize;
        (plane[i] * (1.0 - fx) + plane[i + 1] * fx) * (1.0 - fy)
            + (plane[i + wi] * (1.0 - fx) + plane[i + wi + 1] * fx) * fy
    };

    // One measurement of the red and blue planes against green.
    let measure = |r: &[f32], b: &[f32], report: bool| -> Option<CaModel> {
        // Patch centres on a grid, outside the inner third of the radius (where
        // the shifts are too small to tell).
        let step = (HALF * 2) as u32;
        let mut centres = Vec::new();
        let mut y = HALF as u32 + MAX_SHIFT as u32 + 2;
        while y + (HALF as u32) + (MAX_SHIFT as u32) + 2 < h {
            let mut x = HALF as u32 + MAX_SHIFT as u32 + 2;
            while x + (HALF as u32) + (MAX_SHIFT as u32) + 2 < w {
                let (dx, dy) = (x as f32 - cx, y as f32 - cy);
                if (dx * dx + dy * dy).sqrt() > 0.35 * half_diagonal {
                    centres.push((x, y));
                }
                x += step;
            }
            y += step;
        }

        // Per patch: (radius, red shift, blue shift, weight), shifts in pixels
        // along the outward radius.
        let samples: Vec<Sample> = centres
            .par_iter()
            .filter_map(|&(px, py)| {
                let (dx, dy) = (px as f32 - cx, py as f32 - cy);
                let radius = (dx * dx + dy * dy).sqrt();
                let (ux, uy) = (dx / radius, dy / radius);
                // Gradient of green along the radius versus across it: the
                // patch must hold edges that cross the radius.
                let (mut along, mut across) = (0.0f32, 0.0f32);
                for j in -HALF..=HALF {
                    for i in -HALF..=HALF {
                        let (x, y) = (px as f32 + i as f32, py as f32 + j as f32);
                        let gx = at(&g, x + 1.0, y) - at(&g, x - 1.0, y);
                        let gy = at(&g, x, y + 1.0) - at(&g, x, y - 1.0);
                        let radial = gx * ux + gy * uy;
                        let tangential = -gx * uy + gy * ux;
                        along += radial * radial;
                        across += tangential * tangential;
                    }
                }
                let cells = ((2 * HALF + 1) * (2 * HALF + 1)) as f32;
                if along / cells < 2e-4 || along < 2.0 * across {
                    return None;
                }
                let shift = |plane: &[f32]| shift_along(plane, &g, &at, px, py, ux, uy);
                let (sr, sb) = (shift(r)?, shift(b)?);
                Some((radius, sr, sb, along))
            })
            .collect();
        if report {
            // Tool output: median shift / radius per radius band, and their
            // mean absolute value per channel (‰), the residual measure.
            let (mut sum_r, mut sum_b, mut bands) = (0.0f32, 0.0f32, 0);
            for band in 0..7 {
                let (lo, hi) = (0.35 + band as f32 * 0.1, 0.45 + band as f32 * 0.1);
                let mut rs: Vec<f32> = Vec::new();
                let mut bs: Vec<f32> = Vec::new();
                for s in &samples {
                    let rn = s.0 / half_diagonal;
                    if rn >= lo && rn < hi {
                        rs.push(s.1 / s.0);
                        bs.push(s.2 / s.0);
                    }
                }
                if rs.len() > 5 {
                    rs.sort_by(f32::total_cmp);
                    bs.sort_by(f32::total_cmp);
                    println!(
                        "  r {lo:.2}-{hi:.2}: red {:+.5} blue {:+.5} (n {})",
                        rs[rs.len() / 2],
                        bs[bs.len() / 2],
                        rs.len()
                    );
                    sum_r += rs[rs.len() / 2].abs();
                    sum_b += bs[bs.len() / 2].abs();
                    bands += 1;
                }
            }
            if bands > 0 {
                println!(
                    "  residual (mean |band median|, per mille): red {:.3} blue {:.3}",
                    1000.0 * sum_r / bands as f32,
                    1000.0 * sum_b / bands as f32
                );
            }
        }
        if samples.len() < 12 {
            return None;
        }
        // Per radius band the median of shift / radius (robust against
        // coloured edges and texture), then `a + b·x` through the bands
        // (x = squared relative radius), weighted by their patch counts.
        let fit = |pick: &dyn Fn(&Sample) -> f32| -> (f32, f32) {
            let mut bands: Vec<(f64, f64, f64)> = Vec::new();
            for band in 0..7 {
                let (lo, hi) = (0.35 + band as f32 * 0.1, 0.45 + band as f32 * 0.1);
                let mut q: Vec<f32> = Vec::new();
                let mut x: Vec<f32> = Vec::new();
                for s in &samples {
                    let rn = s.0 / half_diagonal;
                    if rn >= lo && rn < hi {
                        q.push(pick(s) / s.0);
                        x.push(rn * rn);
                    }
                }
                if q.len() >= 5 {
                    q.sort_by(f32::total_cmp);
                    x.sort_by(f32::total_cmp);
                    bands.push((x[x.len() / 2] as f64, q[q.len() / 2] as f64, q.len() as f64));
                }
            }
            let total: f64 = bands.iter().map(|b| b.2).sum();
            if total == 0.0 {
                return (0.0, 0.0);
            }
            let mean_x = bands.iter().map(|b| b.0 * b.2).sum::<f64>() / total;
            let mean_q = bands.iter().map(|b| b.1 * b.2).sum::<f64>() / total;
            let sxx: f64 = bands
                .iter()
                .map(|b| b.2 * (b.0 - mean_x).powi(2))
                .sum::<f64>()
                / total;
            let sxq: f64 = bands
                .iter()
                .map(|b| b.2 * (b.0 - mean_x) * (b.1 - mean_q))
                .sum::<f64>()
                / total;
            // A growth term only with bands spread over enough of the radius.
            if bands.len() < 3 || sxx < 0.01 {
                return (mean_q as f32, 0.0);
            }
            let b = sxq / sxx;
            ((mean_q - b * mean_x) as f32, b as f32)
        };

        Some(CaModel {
            red: fit(&|s| s.1),
            blue: fit(&|s| s.2),
        })
    };
    // Two passes: the second measures what is left after the first
    // correction (resampled planes) and adds it, which settles the growth
    // term where one pass undershoots it.
    // (With RAWTONIC_CA_PROFILE the first pass reports the photo's own
    // residual, the measure used against Camera Raw.)
    let report = std::env::var_os("RAWTONIC_CA_PROFILE").is_some();
    let first = measure(&r, &b, report)?;
    let resample = |plane: &[f32], m: (f32, f32)| -> Vec<f32> {
        (0..(w * h) as usize)
            .into_par_iter()
            .map(|i| {
                let (x, y) = ((i % w as usize) as f32, (i / w as usize) as f32);
                let (dx, dy) = (x - cx, y - cy);
                let r2 = (dx * dx + dy * dy) / (half_diagonal * half_diagonal);
                let k = 1.0 + m.0 + m.1 * r2;
                at(plane, cx + dx * k, cy + dy * k)
            })
            .collect()
    };
    let second = measure(&resample(&r, first.red), &resample(&b, first.blue), false);
    Some(match second {
        Some(rest) => CaModel {
            red: (first.red.0 + rest.red.0, first.red.1 + rest.red.1),
            blue: (first.blue.0 + rest.blue.0, first.blue.1 + rest.blue.1),
        },
        None => first,
    })
}

/// `estimate` for the same photo once: the warp runs for every preview and
/// export of it. Keyed by the image's size and a sparse sample of its pixels.
pub fn estimate_cached(image: &Rgb32FImage) -> Option<CaModel> {
    use std::hash::{Hash, Hasher};
    use std::sync::Mutex;
    static CACHE: Mutex<Vec<(u64, Option<CaModel>)>> = Mutex::new(Vec::new());
    let key = {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        image.dimensions().hash(&mut hasher);
        let raw = image.as_raw();
        let stride = (raw.len() / 4096).max(1);
        for v in raw.iter().step_by(stride) {
            v.to_bits().hash(&mut hasher);
        }
        hasher.finish()
    };
    if let Some((_, value)) = CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .find(|(k, _)| *k == key)
    {
        return *value;
    }
    let value = estimate(image);
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if cache.len() >= 8 {
        cache.remove(0);
    }
    cache.push((key, value));
    value
}

/// The shift (pixels, outward positive) at which `plane` along the radius
/// best matches `reference` in the patch: the position of the plane's
/// feature relative to green's. Gain and offset are normalised, so colour
/// differences between the channels do not matter.
fn shift_along(
    plane: &[f32],
    reference: &[f32],
    at: &dyn Fn(&[f32], f32, f32) -> f32,
    px: u32,
    py: u32,
    ux: f32,
    uy: f32,
) -> Option<f32> {
    let n = (2 * HALF + 1) as usize;
    let mut refs = Vec::with_capacity(n * n);
    for j in -HALF..=HALF {
        for i in -HALF..=HALF {
            refs.push(at(reference, px as f32 + i as f32, py as f32 + j as f32));
        }
    }
    let normalised = |v: &mut Vec<f32>| -> bool {
        let mean = v.iter().sum::<f32>() / v.len() as f32;
        let var = v.iter().map(|x| (x - mean) * (x - mean)).sum::<f32>() / v.len() as f32;
        if var < 1e-8 {
            return false;
        }
        let sd = var.sqrt();
        v.iter_mut().for_each(|x| *x = (*x - mean) / sd);
        true
    };
    if !normalised(&mut refs) {
        return None;
    }
    let score = |s: f32| -> f32 {
        let mut v = Vec::with_capacity(n * n);
        for j in -HALF..=HALF {
            for i in -HALF..=HALF {
                v.push(at(
                    plane,
                    px as f32 + i as f32 + s * ux,
                    py as f32 + j as f32 + s * uy,
                ));
            }
        }
        if !normalised(&mut v) {
            return f32::MAX;
        }
        v.iter().zip(&refs).map(|(a, b)| (a - b) * (a - b)).sum()
    };
    // Coarse search, then a parabola through the best three.
    let mut best = (0.0f32, f32::MAX);
    let mut s = -MAX_SHIFT;
    while s <= MAX_SHIFT + 1e-6 {
        let v = score(s);
        if v < best.1 {
            best = (s, v);
        }
        s += 0.25;
    }
    if best.0.abs() >= MAX_SHIFT {
        return None;
    }
    let h = 0.25;
    let (l, c, r) = (score(best.0 - h), best.1, score(best.0 + h));
    let d = l - 2.0 * c + r;
    let offset = if d > 1e-9 { 0.5 * (l - r) / d * h } else { 0.0 };
    Some(best.0 + offset)
}

fn downscale(image: &Rgb32FImage, long_edge: u32) -> Rgb32FImage {
    let (w, h) = image.dimensions();
    let factor = (w.max(h) as f32 / long_edge as f32).floor().max(1.0) as u32;
    if factor == 1 {
        return image.clone();
    }
    let (sw, sh) = (w / factor, h / factor);
    let mut out = Rgb32FImage::new(sw, sh);
    out.enumerate_rows_mut().par_bridge().for_each(|(_, row)| {
        for (x, y, p) in row {
            let mut acc = [0.0f32; 3];
            for j in 0..factor {
                for i in 0..factor {
                    let q = image.get_pixel(x * factor + i, y * factor + j);
                    for c in 0..3 {
                        acc[c] += q[c];
                    }
                }
            }
            *p = image::Rgb(acc.map(|v| v / (factor * factor) as f32));
        }
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scene of concentric and radial bars (edges in every direction),
    /// with red rendered `kr` larger and blue `kb` larger than green.
    fn scene(kr: f32, kb: f32) -> Rgb32FImage {
        scene_growing(kr, 0.0, kb, 0.0)
    }

    /// As `scene`, red and blue enlarged by `a + b·(r/half-diagonal)²`.
    fn scene_growing(ar: f32, br: f32, ab: f32, bb: f32) -> Rgb32FImage {
        let (w, h) = (1800u32, 1200u32);
        let (cx, cy) = ((w as f32 - 1.0) / 2.0, (h as f32 - 1.0) / 2.0);
        let value = |x: f32, y: f32| {
            let (dx, dy) = (x - cx, y - cy);
            let r = (dx * dx + dy * dy).sqrt();
            let a = dy.atan2(dx);
            let rings = ((r / 23.0).sin() > 0.0) as u8 as f32;
            let spokes = ((a * 40.0).sin() > 0.0) as u8 as f32;
            0.05 + 0.3 * rings + 0.15 * spokes
        };
        let hd2 = cx * cx + cy * cy;
        Rgb32FImage::from_fn(w, h, |x, y| {
            let (x, y) = (x as f32, y as f32);
            // The channel's point at output radius r shows the scene at
            // r / (1 + k(r)), with k evaluated at that scene radius
            // (one fixed-point step is enough for these sizes).
            let scaled = |a: f32, b: f32| {
                let r2 = ((x - cx).powi(2) + (y - cy).powi(2)) / hd2;
                let k = a + b * r2;
                value(cx + (x - cx) / (1.0 + k), cy + (y - cy) / (1.0 + k))
            };
            image::Rgb([scaled(ar, br), value(x, y), scaled(ab, bb) * 0.8])
        })
    }

    /// The model's scale at relative radius `rn` minus 1.
    fn at(m: (f32, f32), rn: f32) -> f32 {
        m.0 + m.1 * rn * rn
    }

    #[test]
    fn estimates_the_red_and_blue_scale_of_a_photo() {
        let m = estimate(&scene(0.0015, -0.001)).unwrap();
        for rn in [0.5, 0.9] {
            assert!((at(m.red, rn) - 0.0015).abs() < 0.0002, "{m:?}");
            assert!((at(m.blue, rn) + 0.001).abs() < 0.0002, "{m:?}");
        }
        let m = estimate(&scene(0.0, 0.0)).unwrap();
        for rn in [0.5, 0.9] {
            assert!(
                at(m.red, rn).abs() < 0.0001 && at(m.blue, rn).abs() < 0.0001,
                "{m:?}"
            );
        }
    }

    #[test]
    fn follows_a_lateral_ca_that_grows_towards_the_corners() {
        // Red 0.3 ‰ larger at the centre and 1.3 ‰ at the corners.
        let m = estimate(&scene_growing(0.0003, 0.001, 0.0, 0.0)).unwrap();
        for rn in [0.45, 0.7, 0.9] {
            let expected = 0.0003 + 0.001 * rn * rn;
            assert!((at(m.red, rn) - expected).abs() < 0.00015, "{rn}: {m:?}");
        }
    }

    #[test]
    fn a_flat_photo_gives_no_estimate() {
        let flat = Rgb32FImage::from_pixel(1200, 800, image::Rgb([0.2, 0.3, 0.4]));
        assert_eq!(estimate(&flat), None);
    }
}
