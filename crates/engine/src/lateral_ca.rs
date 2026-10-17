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
//!
//! Version 2 (`autoCaVersion`, `estimate_v2`) fixes why version 1 could not
//! be a default (notes: reference/lateral-ca.md): the measured shift depends
//! on the resolution it is measured at — soft red and blue halos weigh more
//! on a reduced copy, so an 80 MP frame analysed at a quarter of its size
//! was corrected about twice too strongly — and a fit is not always what
//! leaves the least fringe. It refines the estimate at full resolution and
//! applies the strength that measurably lowers the colour at the edges, or
//! nothing.

use image::Rgb32FImage;
use rayon::prelude::*;

/// Long edge of the copy the estimate works on. A radial scale is the same
/// at every resolution; this keeps the cost low while the shifts at the
/// edges stay well above the estimate's sub-pixel noise. (Real lateral CA
/// is not a pure scale; version 2 refines at full resolution.)
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

/// The recipe key naming the estimate a recipe's automatic lateral CA
/// correction uses.
pub const VERSION_KEY: &str = "autoCaVersion";
/// The newest estimate: 2 (2026-10-17), refined at full resolution and
/// applied only when it is trustworthy; it is also the default for new RAW
/// edits whose lens correction has no lateral CA data of its own. Recipes
/// without the key use version 1 (`estimate`), and only when
/// `autoChromaticAberration` is on.
pub const VERSION: u64 = 2;
/// The key of the Remove chromatic aberration switch.
pub const SWITCH_KEY: &str = "autoChromaticAberration";

/// The recipe's estimate version: `autoCaVersion`, 1 when missing, never
/// newer than this renderer knows.
pub fn version_of(adjustments: &serde_json::Value) -> u64 {
    adjustments
        .get(VERSION_KEY)
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(1)
        .clamp(1, VERSION)
}

/// Whether the automatic correction runs for a recipe: when its switch is
/// on; from version 2 also when the switch is not set and the lens
/// correction has no lateral CA correction of its own (`has_own_tca`:
/// camera data or a profile keep priority).
pub fn applies(adjustments: &serde_json::Value, has_own_tca: bool) -> bool {
    match adjustments
        .get(SWITCH_KEY)
        .and_then(serde_json::Value::as_bool)
    {
        Some(on) => on,
        None => version_of(adjustments) >= 2 && !has_own_tca,
    }
}

/// Sets the current version on the recipe of a never-edited RAW (as the
/// editor does for new edits); recipes that were saved are left as they
/// are.
pub fn apply_default(adjustments: &mut serde_json::Value, never_edited: bool, is_raw: bool) {
    if !never_edited || !is_raw || adjustments.get(VERSION_KEY).is_some() {
        return;
    }
    if adjustments.is_null() {
        *adjustments = serde_json::json!({});
    }
    if let Some(map) = adjustments.as_object_mut() {
        map.insert(VERSION_KEY.to_string(), serde_json::json!(VERSION));
    }
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

/// The cache key of an image: its size and a sparse sample of its pixels.
fn image_key(image: &Rgb32FImage, version: u64) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    image.dimensions().hash(&mut hasher);
    let raw = image.as_raw();
    let stride = (raw.len() / 4096).max(1);
    for v in raw.iter().step_by(stride) {
        v.to_bits().hash(&mut hasher);
    }
    version.hash(&mut hasher);
    hasher.finish()
}

type Cache = std::sync::Mutex<Vec<(u64, Option<CaModel>)>>;
static CACHE: Cache = Cache::new(Vec::new());

fn cached(key: u64) -> Option<Option<CaModel>> {
    CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, value)| *value)
}

fn remember(key: u64, value: Option<CaModel>) {
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    cache.retain(|(k, _)| *k != key);
    if cache.len() >= 16 {
        cache.remove(0);
    }
    cache.push((key, value));
}

/// The estimate of `version` for the same photo once: the warp runs for
/// every preview and export of it.
pub fn estimate_cached(image: &Rgb32FImage, version: u64) -> Option<CaModel> {
    let key = image_key(image, version);
    if let Some(value) = cached(key) {
        return value;
    }
    let value = if version >= 2 {
        estimate_v2(image)
    } else {
        estimate(image)
    };
    remember(key, value);
    value
}

/// Makes the warp of `reduced` (a reduced preview of `full`) use the
/// estimate measured on `full`. From version 2 the estimate is refined at
/// the photo's own resolution, and a reduced copy measures a different,
/// larger lateral CA (notes: reference/lateral-ca.md), so the preview would
/// not show what the export renders. Version 1 measures each image itself,
/// as before.
pub fn share_estimate(full: &Rgb32FImage, reduced: &Rgb32FImage, version: u64) {
    if version < 2 {
        return;
    }
    let value = estimate_cached(full, version);
    remember(image_key(reduced, version), value);
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

// ---- Version 2 ----------------------------------------------------------

/// One patch measured by `measure_patches`: centre (pixels of the image it
/// was measured on), radius, and the red and blue shift along the outward
/// radius (pixels) left after the model.
#[derive(Debug, Clone, Copy)]
struct Patch {
    x: f32,
    y: f32,
    radius: f32,
    red: f32,
    blue: f32,
}

/// Where the model samples a channel for the point `(x, y)`: scaled by
/// `1 + a + b·(r / half-diagonal)²` about the centre.
#[inline]
fn modelled(m: (f32, f32), x: f32, y: f32, cx: f32, cy: f32, inv_hd2: f32) -> (f32, f32) {
    let (dx, dy) = (x - cx, y - cy);
    let k = 1.0 + m.0 + m.1 * (dx * dx + dy * dy) * inv_hd2;
    (cx + dx * k, cy + dy * k)
}

/// The red and blue shift (pixels along the outward radius) that is left
/// after `model` at each of `centres` of `image`, measured at the image's
/// own resolution on square-root values. With `check_edges` a patch is
/// kept only when green's edges run across the radius.
fn measure_patches(
    image: &Rgb32FImage,
    centres: &[(f32, f32)],
    model: CaModel,
    check_edges: bool,
) -> Vec<Patch> {
    let (w, h) = image.dimensions();
    let (cx, cy) = ((w as f32 - 1.0) / 2.0, (h as f32 - 1.0) / 2.0);
    let inv_hd2 = 1.0 / (cx * cx + cy * cy);
    let raw = image.as_raw();
    centres
        .par_iter()
        .filter_map(|&(px, py)| {
            let (dx, dy) = (px - cx, py - cy);
            let radius = (dx * dx + dy * dy).sqrt();
            if radius < 1.0 {
                return None;
            }
            let (ux, uy) = (dx / radius, dy / radius);
            // A local square-root copy of the three planes around the patch,
            // wide enough for the model's displacement and the search.
            let reach = |m: (f32, f32)| {
                let corner = (HALF as f32 + 1.0) * std::f32::consts::SQRT_2;
                let far = radius + corner;
                (m.0.abs() + m.1.abs() * far * far * inv_hd2) * far
            };
            let margin =
                (HALF as f32 + MAX_SHIFT + 3.0 + reach(model.red).max(reach(model.blue))).ceil();
            let x0 = (px - margin).floor().max(0.0) as i64;
            let y0 = (py - margin).floor().max(0.0) as i64;
            let x1 = ((px + margin).ceil() as i64).min(w as i64 - 1);
            let y1 = ((py + margin).ceil() as i64).min(h as i64 - 1);
            if x1 - x0 < 2 || y1 - y0 < 2 {
                return None;
            }
            let (lw, lh) = ((x1 - x0 + 1) as usize, (y1 - y0 + 1) as usize);
            let mut local = [
                vec![0.0f32; lw * lh],
                vec![0.0f32; lw * lh],
                vec![0.0f32; lw * lh],
            ];
            for j in 0..lh {
                let row = (y0 as usize + j) * w as usize;
                for i in 0..lw {
                    let p = (row + x0 as usize + i) * 3;
                    for (c, plane) in local.iter_mut().enumerate() {
                        plane[j * lw + i] = raw[p + c].max(0.0).sqrt();
                    }
                }
            }
            let at = |plane: &[f32], x: f32, y: f32| -> f32 {
                let x = (x - x0 as f32).clamp(0.0, lw as f32 - 1.001);
                let y = (y - y0 as f32).clamp(0.0, lh as f32 - 1.001);
                let (xi, yi) = (x.floor() as usize, y.floor() as usize);
                let (fx, fy) = (x - xi as f32, y - yi as f32);
                let i = yi * lw + xi;
                (plane[i] * (1.0 - fx) + plane[i + 1] * fx) * (1.0 - fy)
                    + (plane[i + lw] * (1.0 - fx) + plane[i + lw + 1] * fx) * fy
            };
            let g = &local[1];
            let mut along = 0.0f32;
            let mut across = 0.0f32;
            for j in -HALF..=HALF {
                for i in -HALF..=HALF {
                    let (x, y) = (px + i as f32, py + j as f32);
                    let gx = at(g, x + 1.0, y) - at(g, x - 1.0, y);
                    let gy = at(g, x, y + 1.0) - at(g, x, y - 1.0);
                    let radial = gx * ux + gy * uy;
                    let tangential = -gx * uy + gy * ux;
                    along += radial * radial;
                    across += tangential * tangential;
                }
            }
            let cells = ((2 * HALF + 1) * (2 * HALF + 1)) as f32;
            if check_edges && (along / cells < 2e-4 || along < 2.0 * across) {
                return None;
            }
            let n = (2 * HALF + 1) as usize;
            let mut refs = Vec::with_capacity(n * n);
            for j in -HALF..=HALF {
                for i in -HALF..=HALF {
                    refs.push(at(g, px + i as f32, py + j as f32));
                }
            }
            if !normalise(&mut refs) {
                return None;
            }
            let shift = |plane: &[f32], m: (f32, f32)| -> Option<f32> {
                let score = |s: f32| -> f32 {
                    let mut v = Vec::with_capacity(n * n);
                    for j in -HALF..=HALF {
                        for i in -HALF..=HALF {
                            let (x, y) = (px + i as f32 + s * ux, py + j as f32 + s * uy);
                            let (x, y) = modelled(m, x, y, cx, cy, inv_hd2);
                            v.push(at(plane, x, y));
                        }
                    }
                    if !normalise(&mut v) {
                        return f32::MAX;
                    }
                    v.iter().zip(&refs).map(|(a, b)| (a - b) * (a - b)).sum()
                };
                parabolic_minimum(&score)
            };
            let red = shift(&local[0], model.red)?;
            let blue = shift(&local[2], model.blue)?;
            Some(Patch {
                x: px,
                y: py,
                radius,
                red,
                blue,
            })
        })
        .collect()
}

/// Mean 0, standard deviation 1; false for a flat patch.
fn normalise(v: &mut [f32]) -> bool {
    let mean = v.iter().sum::<f32>() / v.len() as f32;
    let var = v.iter().map(|x| (x - mean) * (x - mean)).sum::<f32>() / v.len() as f32;
    if var < 1e-8 {
        return false;
    }
    let sd = var.sqrt();
    v.iter_mut().for_each(|x| *x = (*x - mean) / sd);
    true
}

/// The shift in ±`MAX_SHIFT` with the lowest score: a coarse search in
/// quarter pixels, then a parabola through the best three.
fn parabolic_minimum(score: &dyn Fn(f32) -> f32) -> Option<f32> {
    let mut best = (0.0f32, f32::MAX);
    let mut s = -MAX_SHIFT;
    while s <= MAX_SHIFT + 1e-6 {
        let v = score(s);
        if v < best.1 {
            best = (s, v);
        }
        s += 0.25;
    }
    if best.0.abs() >= MAX_SHIFT || best.1 == f32::MAX {
        return None;
    }
    let h = 0.25;
    let (l, c, r) = (score(best.0 - h), best.1, score(best.0 + h));
    let d = l - 2.0 * c + r;
    let offset = if d > 1e-9 { 0.5 * (l - r) / d * h } else { 0.0 };
    Some(best.0 + offset)
}

/// `a + b·x` (x the squared relative radius) through the median of
/// shift / radius per radius band, weighted by the bands' patch counts.
fn band_fit(patches: &[Patch], half_diagonal: f32, pick: impl Fn(&Patch) -> f32) -> (f32, f32) {
    let mut bands: Vec<(f64, f64, f64)> = Vec::new();
    for band in 0..7 {
        let (lo, hi) = (0.35 + band as f32 * 0.1, 0.45 + band as f32 * 0.1);
        let mut q: Vec<f32> = Vec::new();
        let mut x: Vec<f32> = Vec::new();
        for p in patches {
            let rn = p.radius / half_diagonal;
            if rn >= lo && rn < hi {
                q.push(pick(p) / p.radius);
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
    let sxx = bands
        .iter()
        .map(|b| b.2 * (b.0 - mean_x).powi(2))
        .sum::<f64>()
        / total;
    let sxq = bands
        .iter()
        .map(|b| b.2 * (b.0 - mean_x) * (b.1 - mean_q))
        .sum::<f64>()
        / total;
    if bands.len() < 3 || sxx < 0.01 {
        return (mean_q as f32, 0.0);
    }
    let b = sxq / sxx;
    ((mean_q - b * mean_x) as f32, b as f32)
}

fn add(m: CaModel, rest: CaModel) -> CaModel {
    CaModel {
        red: (m.red.0 + rest.red.0, m.red.1 + rest.red.1),
        blue: (m.blue.0 + rest.blue.0, m.blue.1 + rest.blue.1),
    }
}

/// Half the side of the windows `fringe_cost` checks a model in, and the
/// guided filter's window radius there (5 × 5, as the edge-fringe pass).
const CHECK_HALF: i64 = 16;
const CHECK_RADIUS: i64 = 2;

/// The colour that the local luminance does not explain at the edges in
/// windows around `centres` of `image` when red and blue are sampled with
/// `model`: in square-root RGB, `u = √R − √G` and `v = √B − √G` are fitted
/// as linear in `Y = √((R + 2G + B) / 4)` in every 5 × 5 window (as the
/// edge-fringe pass does) and the squared residual is summed over pixels
/// whose window holds an edge. Lateral CA leaves colour on one side of an
/// edge and its complement on the other, which no such fit explains;
/// a two-colour edge is explained. Sum and number of pixels.
fn fringe_cost(image: &Rgb32FImage, centres: &[(f32, f32)], model: CaModel) -> (f64, usize) {
    let (w, h) = image.dimensions();
    let (cx, cy) = ((w as f32 - 1.0) / 2.0, (h as f32 - 1.0) / 2.0);
    let inv_hd2 = 1.0 / (cx * cx + cy * cy);
    let raw = image.as_raw();
    let side = (2 * CHECK_HALF + 1) as usize;
    centres
        .par_iter()
        .map(|&(px, py)| {
            let (px, py) = (px.round() as i64, py.round() as i64);
            if px - CHECK_HALF < 1
                || py - CHECK_HALF < 1
                || px + CHECK_HALF + 1 >= w as i64
                || py + CHECK_HALF + 1 >= h as i64
            {
                return (0.0, 0);
            }
            let value = |c: usize, x: i64, y: i64| -> f32 {
                let x = x.clamp(0, w as i64 - 1) as usize;
                let y = y.clamp(0, h as i64 - 1) as usize;
                raw[(y * w as usize + x) * 3 + c].max(0.0).sqrt()
            };
            let sample = |c: usize, x: f32, y: f32| -> f32 {
                let x = x.clamp(0.0, w as f32 - 1.001);
                let y = y.clamp(0.0, h as f32 - 1.001);
                let (xi, yi) = (x.floor() as i64, y.floor() as i64);
                let (fx, fy) = (x - xi as f32, y - yi as f32);
                (value(c, xi, yi) * (1.0 - fx) + value(c, xi + 1, yi) * fx) * (1.0 - fy)
                    + (value(c, xi, yi + 1) * (1.0 - fx) + value(c, xi + 1, yi + 1) * fx) * fy
            };
            let n = side * side;
            let (mut y_, mut u, mut v) = (vec![0.0f32; n], vec![0.0f32; n], vec![0.0f32; n]);
            for j in 0..side {
                for i in 0..side {
                    let (x, y) = (px - CHECK_HALF + i as i64, py - CHECK_HALF + j as i64);
                    let g = value(1, x, y);
                    let (rx, ry) = modelled(model.red, x as f32, y as f32, cx, cy, inv_hd2);
                    let (bx, by) = modelled(model.blue, x as f32, y as f32, cx, cy, inv_hd2);
                    let (r, b) = (sample(0, rx, ry), sample(2, bx, by));
                    let k = j * side + i;
                    y_[k] = ((r * r + 2.0 * g * g + b * b) / 4.0).sqrt();
                    u[k] = r - g;
                    v[k] = b - g;
                }
            }
            let (mut sum, mut count) = (0.0f64, 0usize);
            let r = CHECK_RADIUS as usize;
            let cells = ((2 * r + 1) * (2 * r + 1)) as f32;
            for j in r..side - r {
                for i in r..side - r {
                    let (mut sy, mut syy, mut su, mut sv, mut syu, mut syv) =
                        (0.0f32, 0.0f32, 0.0f32, 0.0f32, 0.0f32, 0.0f32);
                    for jj in j - r..=j + r {
                        for ii in i - r..=i + r {
                            let k = jj * side + ii;
                            sy += y_[k];
                            syy += y_[k] * y_[k];
                            su += u[k];
                            sv += v[k];
                            syu += y_[k] * u[k];
                            syv += y_[k] * v[k];
                        }
                    }
                    let my = sy / cells;
                    let var = (syy / cells - my * my).max(0.0);
                    if var.sqrt() < EDGE_STD {
                        continue;
                    }
                    let k = j * side + i;
                    let (mu, mv) = (su / cells, sv / cells);
                    let bu = (syu / cells - my * mu) / (var + 4e-4);
                    let bv = (syv / cells - my * mv) / (var + 4e-4);
                    let du = u[k] - (mu + bu * (y_[k] - my));
                    let dv = v[k] - (mv + bv * (y_[k] - my));
                    sum += (du * du + dv * dv) as f64;
                    count += 1;
                }
            }
            (sum, count)
        })
        .reduce(|| (0.0, 0), |a, b| (a.0 + b.0, a.1 + b.1))
}

/// Luminance standard deviation (square-root units) of a 5 × 5 window from
/// which `fringe_cost` counts it as an edge (the edge-fringe pass's).
const EDGE_STD: f32 = 0.03;
/// Strengths of a measured model that are tried.
const GAINS: [f32; 4] = [0.25, 0.5, 0.75, 1.0];
/// The share by which a correction must lower the fringe cost to be
/// applied: below it the photo is left as it is.
const MIN_GAIN: f64 = 0.03;

fn scaled(m: CaModel, g: f32) -> CaModel {
    CaModel {
        red: (m.red.0 * g, m.red.1 * g),
        blue: (m.blue.0 * g, m.blue.1 * g),
    }
}

/// Version 2 of the estimate (notes: reference/lateral-ca.md):
/// - found on the reduced working copy, then refined twice at the photo's
///   full resolution, where the fringes are seen (a reduced copy measures a
///   larger CA: soft red and blue halos weigh more there);
/// - the refined and the reduced copy's model are tried at a quarter, half,
///   three quarters and full strength, and the one that leaves the least
///   colour at the measured edges at full resolution (`fringe_cost`) wins,
///   if it lowers that colour by `MIN_GAIN` at least; else `None` (the
///   photo is not corrected), so the correction never adds fringes.
pub fn estimate_v2(image: &Rgb32FImage) -> Option<CaModel> {
    let report = std::env::var_os("RAWTONIC_CA_PROFILE").is_some();
    let start = std::time::Instant::now();
    let (w, h) = image.dimensions();
    let small = downscale(image, WORK_EDGE);
    let (sw, sh) = small.dimensions();
    let factor = (w as f32 / sw as f32).round().max(1.0);
    let (scx, scy) = ((sw as f32 - 1.0) / 2.0, (sh as f32 - 1.0) / 2.0);
    let small_hd = (scx * scx + scy * scy).sqrt();
    let mut centres = Vec::new();
    let border = HALF as u32 + MAX_SHIFT as u32 + 2;
    let mut y = border;
    while y + border < sh {
        let mut x = border;
        while x + border < sw {
            let (dx, dy) = (x as f32 - scx, y as f32 - scy);
            if (dx * dx + dy * dy).sqrt() > 0.35 * small_hd {
                centres.push((x as f32, y as f32));
            }
            x += (HALF * 2) as u32;
        }
        y += (HALF * 2) as u32;
    }
    let first = measure_patches(&small, &centres, CaModel::NONE, true);
    if first.len() < 12 {
        return None;
    }
    let fit = |patches: &[Patch], hd: f32| CaModel {
        red: band_fit(patches, hd, |p| p.red),
        blue: band_fit(patches, hd, |p| p.blue),
    };
    let coarse = fit(&first, small_hd);
    if report {
        println!(
            "  v2 reduced 1/{factor}: {coarse:?} ({} patches)",
            first.len()
        );
    }
    // The same patches at full resolution.
    let (cx, cy) = ((w as f32 - 1.0) / 2.0, (h as f32 - 1.0) / 2.0);
    let hd = (cx * cx + cy * cy).sqrt();
    let full: Vec<(f32, f32)> = first
        .iter()
        .map(|p| ((p.x + 0.5) * factor - 0.5, (p.y + 0.5) * factor - 0.5))
        .collect();
    // Refined twice; a refinement whose second step does not settle (it
    // moves the corners by more than half as much as the first) is noise.
    let corner = |m: CaModel| (m.red.0 + m.red.1).abs().max((m.blue.0 + m.blue.1).abs());
    let mut refined = Some(coarse);
    let mut last_step = f32::MAX;
    for pass in 0..2 {
        let Some(model) = refined else { break };
        let patches = measure_patches(image, &full, model, false);
        if patches.len() < 12 {
            refined = None;
            break;
        }
        let rest = fit(&patches, hd);
        let step = corner(rest);
        if report {
            println!(
                "  v2 refine {pass}: rest {rest:?} ({} patches)",
                patches.len()
            );
        }
        if pass > 0 && step > 0.5 * last_step && step > 5e-5 {
            refined = None;
            break;
        }
        last_step = step;
        refined = Some(add(model, rest));
    }
    // The candidate that leaves the least colour at the edges.
    let (none, count) = fringe_cost(image, &full, CaModel::NONE);
    if count == 0 || none <= 0.0 {
        return None;
    }
    let mut best = (none, CaModel::NONE);
    for model in refined.into_iter().chain([coarse]) {
        for g in GAINS {
            let candidate = scaled(model, g);
            let cost = fringe_cost(image, &full, candidate).0;
            if report {
                println!("  v2 check {g} x {model:?}: {:.4}", cost / none);
            }
            if cost < best.0 {
                best = (cost, candidate);
            }
        }
    }
    if report {
        println!(
            "  v2 choice: {:?} ({:.4}, {:?})",
            best.1,
            best.0 / none,
            start.elapsed()
        );
    }
    (best.0 <= (1.0 - MIN_GAIN) * none).then_some(best.1)
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
        scene_sized(1800, 1200, ar, br, ab, bb)
    }

    fn scene_sized(w: u32, h: u32, ar: f32, br: f32, ab: f32, bb: f32) -> Rgb32FImage {
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
        assert_eq!(estimate_v2(&flat), None);
    }

    #[test]
    fn version_2_refines_at_full_resolution() {
        // 5000 px wide, so the first estimate runs on a copy reduced by two.
        let m = estimate_v2(&scene_sized(5000, 3200, 0.0008, 0.0, -0.0006, 0.0)).unwrap();
        for rn in [0.5, 0.9] {
            assert!((at(m.red, rn) - 0.0008).abs() < 0.00012, "{m:?}");
            assert!((at(m.blue, rn) + 0.0006).abs() < 0.00012, "{m:?}");
        }
        let m = estimate_v2(&scene_growing(0.0003, 0.001, 0.0, 0.0)).unwrap();
        for rn in [0.45, 0.7, 0.9] {
            let expected = 0.0003 + 0.001 * rn * rn;
            assert!((at(m.red, rn) - expected).abs() < 0.0002, "{rn}: {m:?}");
        }
    }

    #[test]
    fn version_2_leaves_a_photo_without_lateral_ca_alone() {
        // Nothing to correct: no model lowers the colour at the edges.
        assert_eq!(estimate_v2(&scene(0.0, 0.0)), None);
        // Colour shading (red rising towards the corners) is no lateral CA.
        let shaded = {
            let mut image = scene(0.0, 0.0);
            let (w, h) = image.dimensions();
            let (cx, cy) = ((w as f32 - 1.0) / 2.0, (h as f32 - 1.0) / 2.0);
            let hd2 = cx * cx + cy * cy;
            for (x, y, p) in image.enumerate_pixels_mut() {
                let r2 = ((x as f32 - cx).powi(2) + (y as f32 - cy).powi(2)) / hd2;
                p[0] *= 1.0 + 0.4 * r2;
                p[2] *= 1.0 - 0.3 * r2;
            }
            image
        };
        assert_eq!(estimate_v2(&shaded), None);
    }

    #[test]
    fn the_automatic_default_leaves_camera_data_and_saved_recipes_alone() {
        use serde_json::json;
        // Saved recipes: only the switch.
        assert!(!applies(&json!({}), false));
        assert!(applies(&json!({"autoChromaticAberration": true}), true));
        assert_eq!(version_of(&json!({"autoChromaticAberration": true})), 1);
        // New RAW edits: on where the lens data has no CA correction.
        let new = json!({"autoCaVersion": 2});
        assert!(applies(&new, false));
        assert!(!applies(&new, true));
        assert!(!applies(
            &json!({"autoCaVersion": 2, "autoChromaticAberration": false}),
            false
        ));
        assert!(applies(
            &json!({"autoCaVersion": 2, "autoChromaticAberration": true}),
            true
        ));
        assert_eq!(version_of(&json!({"autoCaVersion": 9})), VERSION);
        let mut never = serde_json::Value::Null;
        apply_default(&mut never, true, true);
        assert_eq!(never, json!({"autoCaVersion": VERSION}));
        let mut saved = json!({"exposure": 1});
        apply_default(&mut saved, false, true);
        assert_eq!(saved, json!({"exposure": 1}));
    }
}
