//! `maskVersion` 2 layers: coverage computed in floating point and kept at 16
//! bits, AI masks grown and feathered in proportion to the photo (not to the
//! mask's crop and grid), and the layer's Refine edge.
//! notes: reference/local-masks-baseline.md "WP5".

use super::*;

/// AI mask types whose grow and feather version 2 applies itself; a depth
/// range keeps its own blur and the 8-bit steps.
const AI_GROW_FEATHER: [&str; 4] = ["ai-subject", "ai-foreground", "ai-sky", "ai-people"];

/// Whether a version 2 layer's coverage at full opacity is finer than 8
/// bits: a gradient, a sub-mask opacity, an AI mask's grow or feather, or
/// Refine edge. Otherwise its mask is the 8-bit one (every step on bytes is
/// then exact), which also keeps brush layers on the byte path while painting.
fn full_needs_wide(def: &MaskDefinition) -> bool {
    def.refine_radius().is_some()
        || def.sub_masks.iter().filter(|sub| sub.visible).any(|sub| {
            sub.opacity < 100.0
                || matches!(sub.mask_type.as_str(), "radial" | "linear")
                || (AI_GROW_FEATHER.contains(&sub.mask_type.as_str())
                    && grow_feather(&sub.parameters).is_some())
        })
}

/// Whether a version 2 layer's mask is finer than 8 bits.
pub(super) fn needs_wide(def: &MaskDefinition) -> bool {
    def.opacity < 100.0 || full_needs_wide(def)
}

/// A version 2 layer's mask. The full-opacity mask is made first and the
/// opacity applied to it, as `cached_or_generated_mask` derives a layer below
/// full opacity from the cached full one, so both give the same pixels.
pub(super) fn layer_mask(
    def: &MaskDefinition,
    width: u32,
    height: u32,
    scale: f32,
    crop_offset: (f32, f32),
    geometry: &PhotoGeometry,
    warped_image: Option<&ParametricMaskSource>,
) -> MaskBitmap {
    let full = if full_needs_wide(def) {
        let mut coverage = combined_coverage(
            def,
            width,
            height,
            scale,
            crop_offset,
            geometry,
            warped_image,
        );
        if def.invert {
            coverage.par_iter_mut().for_each(|v| *v = 1.0 - *v);
        }
        if let (Some(radius), Some(source)) = (def.refine_radius(), warped_image) {
            let sampler = SourceSampler::new(source, scale, crop_offset);
            refine_edge(
                &mut coverage,
                width as usize,
                height as usize,
                radius * scale,
                &sampler,
                &RefineTuning::for_version(def.mask_version),
            );
        }
        MaskBitmap::from_coverage(width, height, &coverage)
    } else {
        MaskBitmap::from(eight_bit_mask(
            def,
            100.0,
            width,
            height,
            scale,
            crop_offset,
            geometry,
            warped_image,
        ))
    };
    if def.opacity < 100.0 {
        full.with_opacity(def.opacity, true)
    } else {
        full
    }
}

/// The layer's sub-masks combined as `eight_bit_mask` does (add = max,
/// subtract, intersect = min, each with its invert and opacity), in 0–1.
fn combined_coverage(
    def: &MaskDefinition,
    width: u32,
    height: u32,
    scale: f32,
    crop_offset: (f32, f32),
    geometry: &PhotoGeometry,
    warped_image: Option<&ParametricMaskSource>,
) -> Vec<f32> {
    let len = width as usize * height as usize;
    let mut combined: Option<Vec<f32>> = None;
    for sub in &def.sub_masks {
        let Some(mut coverage) = sub_mask_coverage(
            sub,
            width,
            height,
            scale,
            crop_offset,
            geometry,
            warped_image,
        ) else {
            continue;
        };
        let multiplier = (sub.opacity / 100.0).clamp(0.0, 1.0);
        let invert = sub.invert;
        coverage.par_iter_mut().for_each(|v| {
            let value = if invert { 1.0 - *v } else { *v };
            *v = value * multiplier;
        });
        let Some(target) = combined.as_mut() else {
            combined = Some(match sub.mode {
                SubMaskMode::Additive => coverage,
                SubMaskMode::Subtractive | SubMaskMode::Intersect => vec![0.0; len],
            });
            continue;
        };
        let pixels = target.par_iter_mut().zip(coverage.par_iter());
        match sub.mode {
            SubMaskMode::Additive => pixels.for_each(|(pixel, sub)| *pixel = pixel.max(*sub)),
            SubMaskMode::Subtractive => {
                pixels.for_each(|(pixel, sub)| *pixel = (*pixel - *sub).max(0.0))
            }
            SubMaskMode::Intersect => pixels.for_each(|(pixel, sub)| *pixel = pixel.min(*sub)),
        }
    }
    combined.unwrap_or_else(|| vec![0.0; len])
}

fn bytes_to_coverage(mask: &GrayImage) -> Vec<f32> {
    mask.as_raw()
        .par_iter()
        .map(|v| *v as f32 / 255.0)
        .collect()
}

fn sub_mask_coverage(
    sub: &SubMask,
    width: u32,
    height: u32,
    scale: f32,
    crop_offset: (f32, f32),
    geometry: &PhotoGeometry,
    warped_image: Option<&ParametricMaskSource>,
) -> Option<Vec<f32>> {
    if !sub.visible {
        return None;
    }
    let drawn = |kind: &str| shape_as_shown(kind, &sub.parameters, geometry.shown(), scale);
    let bitmap = |sub: &SubMask| {
        generate_sub_mask_bitmap(
            sub,
            width,
            height,
            scale,
            crop_offset,
            geometry,
            warped_image,
        )
    };
    match sub.mask_type.as_str() {
        "radial" => Some(radial_coverage(
            &drawn("radial"),
            width,
            height,
            scale,
            crop_offset,
        )),
        "linear" => Some(linear_coverage(
            &drawn("linear"),
            width,
            height,
            scale,
            crop_offset,
        )),
        kind if AI_GROW_FEATHER.contains(&kind) => match grow_feather(&sub.parameters) {
            None => bitmap(sub).map(|mask| bytes_to_coverage(&mask)),
            Some(amounts) => {
                let mut parameters = sub.parameters.clone();
                parameters["grow"] = Value::from(0.0);
                parameters["feather"] = Value::from(0.0);
                let plain = SubMask {
                    parameters,
                    ..sub.clone()
                };
                let mut coverage = bytes_to_coverage(&bitmap(&plain)?);
                // The photo's short side on this grid, so the amounts mean the
                // same in the preview, an export and any crop.
                let base = geometry
                    .size
                    .map_or(width.min(height) as f32, |(w, h)| w.min(h) as f32 * scale);
                grow_and_feather(
                    &mut coverage,
                    width as usize,
                    height as usize,
                    amounts,
                    base,
                );
                Some(coverage)
            }
        },
        _ => bitmap(sub).map(|mask| bytes_to_coverage(&mask)),
    }
}

/// A radial gradient as `generate_radial_bitmap` draws it, without rounding
/// its centre to whole pixels or its coverage to bytes.
fn radial_coverage(
    params_value: &Value,
    width: u32,
    height: u32,
    scale: f32,
    crop_offset: (f32, f32),
) -> Vec<f32> {
    let params: RadialMaskParameters =
        serde_json::from_value(params_value.clone()).unwrap_or_default();
    let center_x = params.center_x as f32 * scale - crop_offset.0;
    let center_y = params.center_y as f32 * scale - crop_offset.1;
    let radius_x = (params.radius_x as f32 * scale).max(0.01);
    let radius_y = (params.radius_y as f32 * scale).max(0.01);
    let (sin_rot, cos_rot) = (params.rotation * PI / 180.0).sin_cos();
    let inner_bound = 1.0 - params.feather.clamp(0.0, 1.0);
    let feather_width = (1.0 - inner_bound).max(0.01);
    let mut coverage = vec![0.0; width as usize * height as usize];
    if width == 0 {
        return coverage;
    }
    coverage
        .par_chunks_mut(width as usize)
        .enumerate()
        .for_each(|(y, row)| {
            let dy = y as f32 - center_y;
            for (x, pixel) in row.iter_mut().enumerate() {
                let dx = x as f32 - center_x;
                let norm_x = (dx * cos_rot + dy * sin_rot) / radius_x;
                let norm_y = (-dx * sin_rot + dy * cos_rot) / radius_y;
                let dist = (norm_x * norm_x + norm_y * norm_y).sqrt();
                *pixel = (1.0 - (dist - inner_bound) / feather_width).clamp(0.0, 1.0);
            }
        });
    coverage
}

/// A linear gradient as `generate_linear_bitmap` draws it, without rounding
/// its coverage to bytes.
fn linear_coverage(
    params_value: &Value,
    width: u32,
    height: u32,
    scale: f32,
    crop_offset: (f32, f32),
) -> Vec<f32> {
    let params: LinearMaskParameters =
        serde_json::from_value(params_value.clone()).unwrap_or_default();
    let mut coverage = vec![0.0; width as usize * height as usize];
    let start_x = params.start_x as f32 * scale - crop_offset.0;
    let start_y = params.start_y as f32 * scale - crop_offset.1;
    let line_x = params.end_x as f32 * scale - crop_offset.0 - start_x;
    let line_y = params.end_y as f32 * scale - crop_offset.1 - start_y;
    let len_sq = line_x * line_x + line_y * line_y;
    if len_sq < 0.01 || width == 0 {
        return coverage;
    }
    let (perp_x, perp_y) = (-line_y / len_sq.sqrt(), line_x / len_sq.sqrt());
    let half_width = (params.range * scale).max(0.01);
    let end_half_width = params
        .range_end
        .map_or(half_width, |range_end| (range_end * scale).max(0.01));
    coverage
        .par_chunks_mut(width as usize)
        .enumerate()
        .for_each(|(y, row)| {
            let pixel_y = y as f32 - start_y;
            for (x, pixel) in row.iter_mut().enumerate() {
                let dist = (x as f32 - start_x) * perp_x + pixel_y * perp_y;
                // Negative distances lie on the start (100%) side.
                let t = if dist > 0.0 {
                    dist / end_half_width
                } else {
                    dist / half_width
                };
                *pixel = (0.5 - t * 0.5).clamp(0.0, 1.0);
            }
        });
    coverage
}

/// An AI mask's grow and feather amounts, when either is set.
fn grow_feather(parameters: &Value) -> Option<(f32, f32)> {
    let amounts: GrowFeatherParameters =
        serde_json::from_value(parameters.clone()).unwrap_or_default();
    (amounts.grow.abs() > 0.01 || amounts.feather > 0.0).then_some((amounts.grow, amounts.feather))
}

/// `apply_grow_and_feather`'s amounts on coverage: grow by up to 1% and
/// feather with a Gaussian of up to 0.5% of `base` (the photo's short side on
/// this grid), a fractional grow by blending the two nearest whole ones.
fn grow_and_feather(
    coverage: &mut Vec<f32>,
    width: usize,
    height: usize,
    (grow, feather): (f32, f32),
    base: f32,
) {
    let grow_px = grow / 100.0 * base * 0.01;
    if grow_px.abs() > 0.01 {
        let reach = grow_px.abs();
        let whole = reach.floor() as usize;
        let part = reach - whole as f32;
        let dilate = grow_px > 0.0;
        let lower = rank_filter(coverage, width, height, whole, dilate);
        *coverage = if part > 0.0 {
            let upper = rank_filter(&lower, width, height, 1, dilate);
            lower
                .par_iter()
                .zip(upper.par_iter())
                .map(|(a, b)| a + (b - a) * part)
                .collect()
        } else {
            lower
        };
    }
    let sigma = feather / 100.0 * base * 0.005;
    if sigma > 0.01 {
        gaussian_blur(coverage, width, height, sigma);
    }
}

/// Maximum (`dilate`) or minimum over a (2r + 1)² square, as
/// `grayscale_dilate` / `grayscale_erode`.
fn rank_filter(src: &[f32], width: usize, height: usize, r: usize, dilate: bool) -> Vec<f32> {
    if r == 0 || width == 0 || height == 0 {
        return src.to_vec();
    }
    let pick = move |a: f32, b: f32| if dilate { a.max(b) } else { a.min(b) };
    let mut rows = vec![0.0; src.len()];
    rows.par_chunks_mut(width)
        .zip(src.par_chunks(width))
        .for_each(|(out, row)| {
            for x in 0..width {
                let (a, b) = (x.saturating_sub(r), (x + r).min(width - 1));
                *out.get_mut(x).unwrap() = row[a..=b].iter().copied().reduce(pick).unwrap();
            }
        });
    let mut out = vec![0.0; src.len()];
    out.par_chunks_mut(width).enumerate().for_each(|(y, row)| {
        let (a, b) = (y.saturating_sub(r), (y + r).min(height - 1));
        for (x, value) in row.iter_mut().enumerate() {
            *value = (a..=b).map(|yy| rows[yy * width + x]).reduce(pick).unwrap();
        }
    });
    out
}

/// Separable Gaussian blur of a single-channel image, edges clamped.
fn gaussian_blur(values: &mut [f32], width: usize, height: usize, sigma: f32) {
    if width == 0 || height == 0 || sigma <= 0.0 {
        return;
    }
    let radius = (sigma * 3.0).ceil() as isize;
    let kernel: Vec<f32> = {
        let raw: Vec<f32> = (-radius..=radius)
            .map(|i| (-(i * i) as f32 / (2.0 * sigma * sigma)).exp())
            .collect();
        let sum: f32 = raw.iter().sum();
        raw.into_iter().map(|k| k / sum).collect()
    };
    let mut rows = vec![0.0; values.len()];
    rows.par_chunks_mut(width)
        .zip(values.par_chunks(width))
        .for_each(|(out, row)| {
            for (x, value) in out.iter_mut().enumerate() {
                *value = kernel
                    .iter()
                    .enumerate()
                    .map(|(k, weight)| {
                        let xx = (x as isize + k as isize - radius).clamp(0, width as isize - 1);
                        row[xx as usize] * weight
                    })
                    .sum();
            }
        });
    values
        .par_chunks_mut(width)
        .enumerate()
        .for_each(|(y, out)| {
            for (x, value) in out.iter_mut().enumerate() {
                *value = kernel
                    .iter()
                    .enumerate()
                    .map(|(k, weight)| {
                        let yy = (y as isize + k as isize - radius).clamp(0, height as isize - 1);
                        rows[yy as usize * width + x] * weight
                    })
                    .sum();
            }
        });
}

/// Squared Euclidean distance from each pixel to the nearest `feature` pixel
/// and that pixel's index (Felzenszwalb and Huttenlocher's exact transform);
/// infinite distance and `u32::MAX` where there is none.
fn distance_transform(feature: &[bool], width: usize, height: usize) -> (Vec<f32>, Vec<u32>) {
    const FAR: f64 = 1e20;
    // Columns: the nearest feature row within each column.
    let columns: Vec<(Vec<f64>, Vec<u32>)> = (0..width)
        .into_par_iter()
        .map(|x| {
            let f: Vec<f64> = (0..height)
                .map(|y| if feature[y * width + x] { 0.0 } else { FAR })
                .collect();
            edt_1d(&f)
        })
        .collect();
    let mut distance = vec![f32::INFINITY; width * height];
    let mut nearest = vec![u32::MAX; width * height];
    distance
        .par_chunks_mut(width)
        .zip(nearest.par_chunks_mut(width))
        .enumerate()
        .for_each(|(y, (distance, nearest))| {
            let f: Vec<f64> = columns.iter().map(|(d, _)| d[y]).collect();
            let (d, column) = edt_1d(&f);
            for x in 0..width {
                if d[x] < FAR / 2.0 {
                    let cx = column[x] as usize;
                    distance[x] = d[x] as f32;
                    nearest[x] = (columns[cx].1[y] as usize * width + cx) as u32;
                }
            }
        });
    (distance, nearest)
}

/// One dimension of `distance_transform`: the lower envelope of parabolas
/// rooted at `f`, with the root each sample's distance comes from.
fn edt_1d(f: &[f64]) -> (Vec<f64>, Vec<u32>) {
    let n = f.len();
    let mut d = vec![0.0; n];
    let mut arg = vec![0u32; n];
    if n == 0 {
        return (d, arg);
    }
    let mut v = vec![0usize; n];
    let mut z = vec![0.0f64; n + 1];
    let mut k = 0usize;
    z[0] = f64::NEG_INFINITY;
    z[1] = f64::INFINITY;
    let intersection = |q: usize, p: usize| {
        ((f[q] + (q * q) as f64) - (f[p] + (p * p) as f64)) / (2.0 * (q as f64 - p as f64))
    };
    for q in 1..n {
        // z[0] is −∞, so k never drops below zero.
        let mut s = intersection(q, v[k]);
        while s <= z[k] {
            k -= 1;
            s = intersection(q, v[k]);
        }
        k += 1;
        v[k] = q;
        z[k] = s;
        z[k + 1] = f64::INFINITY;
    }
    k = 0;
    for q in 0..n {
        while z[k + 1] < q as f64 {
            k += 1;
        }
        let p = v[k];
        let dq = q as f64 - p as f64;
        d[q] = dq * dq + f[p];
        arg[q] = p as u32;
    }
    (d, arg)
}

/// Constants of Refine edge, in units of the radius where they are lengths.
#[derive(Clone, Copy, Debug)]
pub(super) struct RefineTuning {
    /// Width of the colour averages of the sure regions.
    smoothing: f32,
    /// Foreground and background separation (|F − B| / (|F| + |B|)) at which
    /// the photo starts and ends deciding.
    separation: (f32, f32),
    /// Distance of a colour from the F–B line (/ |F − B|) at which it stops
    /// and starts being trusted less.
    residual: (f32, f32),
    /// Width of the fade back to the mask at the band's edge.
    taper: f32,
    /// `maskVersion` 3: where the foreground colour is a mix of the
    /// backgrounds on both sides of the mask, the side it is mostly made of
    /// is the subject (a thin stroke inside a soft edge).
    both_sides: bool,
}

impl RefineTuning {
    const DEFAULT: Self = Self {
        smoothing: 1.0 / 12.0,
        separation: (0.02, 0.06),
        residual: (0.5, 1.0),
        taper: 0.25,
        both_sides: false,
    };

    pub(super) fn for_version(version: Option<u32>) -> Self {
        Self {
            both_sides: version.is_some_and(|v| v >= 3),
            ..Self::DEFAULT
        }
    }
}

/// `maskVersion` 3: how far past a pixel (in radii) the sure background on
/// the far side of the mask is looked for.
const REFINE_FAR_REACH: f32 = 4.0;
/// `maskVersion` 3: the foreground colour must be at least this much more
/// one side's than the other's to choose that side as the subject.
const REFINE_MIX_MARGIN: f32 = 0.1;

/// Share of the deepest distance from the 50% line at which the sure
/// regions start when the radius reaches farther.
const SURE_SHARE: f32 = 0.8;

/// Refine edge: within `radius` pixels of the mask's 50% line the coverage
/// becomes the photo's own mix of the colours on either side. Pixels farther
/// in (the mask's sure foreground) and farther out (sure background) give
/// the colours: for each pixel near the edge, F and B are the averaged
/// colours of the nearest sure pixels, and its coverage is its linear-light
/// colour's share between them (its projection onto the F–B line); where the
/// mask is thinner than the radius, its deepest part is the sure region. Where
/// F and B are alike, or the colour is far from their line, the mask keeps
/// its own coverage; the band's edge fades back to the mask.
///
/// `maskVersion` 3 (`RefineTuning::both_sides`), for a mask thinner than the
/// radius: a thin stroke inside a soft edge has the subject's interior
/// outside it too, so B on one side is the subject. Where F lies on the line between the nearest B and the sure
/// background on the far side of the mask, F is their mix and the side it
/// is mostly made of becomes the foreground, the other the background.
/// notes: reference/local-masks-baseline.md "Task AA".
pub(super) fn refine_edge(
    coverage: &mut [f32],
    width: usize,
    height: usize,
    radius: f32,
    sampler: &SourceSampler,
    tuning: &RefineTuning,
) {
    if radius < 0.5 || width == 0 || height == 0 {
        return;
    }
    // The work area: the 50% line's bounding box, widened by the band and
    // the reach of the sure colours.
    let inside = |i: usize| coverage[i] >= 0.5;
    let contour = (0..height)
        .into_par_iter()
        .filter_map(|y| {
            let mut span: Option<(usize, usize)> = None;
            for x in 0..width {
                let i = y * width + x;
                let here = inside(i);
                let edge = (x + 1 < width && inside(i + 1) != here)
                    || (y + 1 < height && inside(i + width) != here);
                if edge {
                    span = Some(span.map_or((x, x + 1), |(a, _)| (a, x + 1)));
                }
            }
            span.map(|(a, b)| (y, a, b))
        })
        .collect::<Vec<_>>();
    let Some(first) = contour.first() else {
        return;
    };
    let (mut top, mut bottom, mut left, mut right) = (first.0, first.0, first.1, first.2);
    for (y, a, b) in &contour {
        top = top.min(*y);
        bottom = bottom.max(*y);
        left = left.min(*a);
        right = right.max(*b);
    }
    let smoothing = (radius * tuning.smoothing).max(0.75);
    let margin = (2.0 * radius + 3.0 * smoothing).ceil() as usize + 2;
    let x0 = left.saturating_sub(margin);
    let y0 = top.saturating_sub(margin);
    let x1 = (right + margin).min(width - 1);
    let y1 = (bottom + 1 + margin).min(height - 1);
    let (w, h) = (x1 - x0 + 1, y1 - y0 + 1);
    let at = |x: usize, y: usize| coverage[(y0 + y) * width + x0 + x];

    let mask: Vec<f32> = (0..w * h).map(|i| at(i % w, i / w)).collect();
    let inside: Vec<bool> = mask.iter().map(|v| *v >= 0.5).collect();
    let outside: Vec<bool> = inside.iter().map(|v| !v).collect();
    let (to_inside, _) = distance_transform(&inside, w, h);
    let (to_outside, _) = distance_transform(&outside, w, h);
    // Signed distance to the 50% line: positive inside.
    let signed: Vec<f32> = (0..w * h)
        .map(|i| {
            if inside[i] {
                to_outside[i].sqrt()
            } else {
                -to_inside[i].sqrt()
            }
        })
        .collect();
    if !signed.iter().any(|d| d.abs() < radius) {
        return;
    }

    let colours = source_linear_region(sampler, (x0 as u32, y0 as u32, x1 as u32, y1 as u32));
    let finite = |i: usize| colours[i].iter().all(|c| c.is_finite());
    // The sure regions start at the radius, or nearer the line where the
    // mask (or the gap around it) is thinner than that: a stroke narrower
    // than the radius still has a foreground.
    let deepest = |sign: f32| signed.iter().map(|d| d * sign).fold(0.0f32, f32::max);
    let (depth_f, depth_b) = (
        radius.min(SURE_SHARE * deepest(1.0)),
        radius.min(SURE_SHARE * deepest(-1.0)),
    );
    // Version 3 reads the far side only where the whole mask lies within
    // the band (a thin stroke); a wider mask has a sure foreground of its
    // own.
    let both_sides = tuning.both_sides && deepest(1.0) < radius;
    let sure_f: Vec<bool> = (0..w * h)
        .map(|i| signed[i] >= depth_f.max(1.0) && finite(i))
        .collect();
    let sure_b: Vec<bool> = (0..w * h)
        .map(|i| signed[i] <= -depth_b.max(1.0) && finite(i))
        .collect();
    let (_, nearest_f) = distance_transform(&sure_f, w, h);
    let (to_sure_b, nearest_b) = distance_transform(&sure_b, w, h);
    // The sure background on the far side of a pixel from its nearest one:
    // marched away from the nearest along the line through both, in steps
    // of the distance to the nearest sure background (which cannot skip
    // one).
    let far_side = |i: usize| -> Option<usize> {
        let nb = nearest_b[i] as usize;
        let (x, y) = ((i % w) as f32, (i / w) as f32);
        let (bx, by) = ((nb % w) as f32, (nb / w) as f32);
        let (vx, vy) = (x - bx, y - by);
        let len = vx.hypot(vy);
        if len < 0.5 {
            return None;
        }
        let (vx, vy) = (vx / len, vy / len);
        let reach = REFINE_FAR_REACH * radius;
        let mut t = 1.0f32;
        while t <= reach {
            let (qx, qy) = (x + vx * t, y + vy * t);
            if qx < 0.0 || qy < 0.0 || qx > (w - 1) as f32 || qy > (h - 1) as f32 {
                return None;
            }
            let q = qy.round() as usize * w + qx.round() as usize;
            let d = to_sure_b[q].sqrt();
            if d <= 1.0 {
                let hit = nearest_b[q] as usize;
                return (hit != u32::MAX as usize).then_some(hit);
            }
            t += d.max(1.0);
        }
        None
    };

    // The band's pixels with their nearest sure foreground and background
    // (and the far side's); the averaged colours are needed only there.
    let band: Vec<(usize, usize, usize, Option<usize>)> = (0..w * h)
        .into_par_iter()
        .filter_map(|i| {
            if signed[i].abs() >= radius || !finite(i) {
                return None;
            }
            let (nf, nb) = (nearest_f[i], nearest_b[i]);
            if nf == u32::MAX || nb == u32::MAX {
                return None;
            }
            let far = if both_sides { far_side(i) } else { None };
            Some((i, nf as usize, nb as usize, far))
        })
        .collect();
    let mean_f = SureMeans::at(
        &colours,
        &sure_f,
        w,
        h,
        smoothing,
        band.iter().map(|p| p.1).collect(),
    );
    let mean_b = SureMeans::at(
        &colours,
        &sure_b,
        w,
        h,
        smoothing,
        band.iter()
            .flat_map(|p| std::iter::once(p.2).chain(p.3))
            .collect(),
    );

    let refined: Vec<(usize, f32)> = band
        .par_iter()
        .filter_map(|&(i, nf, nb, far)| {
            let distance = signed[i].abs();
            let (mut f, mut b, c) = (mean_f.get(nf), mean_b.get(nb), colours[i]);
            if let Some(far) = far {
                let b2 = mean_b.get(far);
                if let Some((t, true)) = line_share(f, b, b2, AUTO_MASK_BAND)
                    && (0.1..0.9).contains(&t)
                    && (t - 0.5).abs() >= REFINE_MIX_MARGIN
                {
                    // f is a mix of b and b2: the one it is mostly made of
                    // is the subject.
                    (f, b) = if t >= 0.5 { (b, b2) } else { (b2, b) };
                }
            }
            let d = [f[0] - b[0], f[1] - b[1], f[2] - b[2]];
            let dd = d[0] * d[0] + d[1] * d[1] + d[2] * d[2];
            if dd <= 1e-12 {
                return None;
            }
            let cb = [c[0] - b[0], c[1] - b[1], c[2] - b[2]];
            let alpha = ((cb[0] * d[0] + cb[1] * d[1] + cb[2] * d[2]) / dd).clamp(0.0, 1.0);
            let norm = |v: [f32; 3]| (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
            let separation = dd.sqrt() / (norm(f) + norm(b)).max(1e-4);
            let residual = norm([
                cb[0] - alpha * d[0],
                cb[1] - alpha * d[1],
                cb[2] - alpha * d[2],
            ]) / dd.sqrt();
            let ramp = |x: f32, (lo, hi): (f32, f32)| ((x - lo) / (hi - lo)).clamp(0.0, 1.0);
            let confidence =
                ramp(separation, tuning.separation) * (1.0 - ramp(residual, tuning.residual));
            let taper = ((radius - distance) / (radius * tuning.taper).max(1.0)).clamp(0.0, 1.0);
            let weight = confidence * taper;
            Some((i, weight * alpha + (1.0 - weight) * mask[i]))
        })
        .collect();
    for (i, value) in refined {
        coverage[(y0 + i / w) * width + x0 + i % w] = value;
    }
}

/// Share of `a` in `c` on the line from `b` to `a`, and whether `c` lies
/// within `band` × |a − b| of that line.
fn line_share(c: [f32; 3], a: [f32; 3], b: [f32; 3], band: f32) -> Option<(f32, bool)> {
    let axis = [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
    let len_sq = axis[0] * axis[0] + axis[1] * axis[1] + axis[2] * axis[2];
    if len_sq <= 1e-12 {
        return None;
    }
    let d = [c[0] - b[0], c[1] - b[1], c[2] - b[2]];
    let t = (d[0] * axis[0] + d[1] * axis[1] + d[2] * axis[2]) / len_sq;
    let r = [d[0] - t * axis[0], d[1] - t * axis[1], d[2] - t * axis[2]];
    let r_sq = r[0] * r[0] + r[1] * r[1] + r[2] * r[2];
    Some((t, r_sq < band * band * len_sq))
}

/// Sure pixels' colours averaged with the sure pixels around them
/// (normalised Gaussian convolution), computed only at the pixels asked
/// for. Each value is bit for bit what blurring the whole region would give
/// there (`gaussian_blur`'s kernel, border clamping and order of sums);
/// other pixels read zero.
struct SureMeans {
    indices: Vec<usize>,
    means: Vec<[f32; 3]>,
}

impl SureMeans {
    fn at(
        colours: &[[f32; 3]],
        sure: &[bool],
        width: usize,
        height: usize,
        sigma: f32,
        mut indices: Vec<usize>,
    ) -> Self {
        indices.par_sort_unstable();
        indices.dedup();
        let radius = (sigma * 3.0).ceil() as isize;
        let kernel: Vec<f32> = {
            let raw: Vec<f32> = (-radius..=radius)
                .map(|i| (-(i * i) as f32 / (2.0 * sigma * sigma)).exp())
                .collect();
            let sum: f32 = raw.iter().sum();
            raw.into_iter().map(|k| k / sum).collect()
        };
        let means = indices
            .par_iter()
            .map(|&i| {
                if !sure[i] {
                    return [0.0; 3];
                }
                let (x, y) = ((i % width) as isize, (i / width) as isize);
                // Per channel (weight, r, g, b): the row sums first, then
                // the column sum over them, as the separable blur does.
                let mut total = [0.0f32; 4];
                for (ky, wy) in kernel.iter().enumerate() {
                    let yy = (y + ky as isize - radius).clamp(0, height as isize - 1) as usize;
                    let mut row = [0.0f32; 4];
                    for (kx, wx) in kernel.iter().enumerate() {
                        let xx = (x + kx as isize - radius).clamp(0, width as isize - 1) as usize;
                        let j = yy * width + xx;
                        let (s, c) = if sure[j] {
                            (1.0, colours[j])
                        } else {
                            (0.0, [0.0; 3])
                        };
                        row[0] += s * wx;
                        row[1] += c[0] * wx;
                        row[2] += c[1] * wx;
                        row[3] += c[2] * wx;
                    }
                    for k in 0..4 {
                        total[k] += row[k] * wy;
                    }
                }
                if total[0] > 0.0 {
                    [
                        total[1] / total[0],
                        total[2] / total[0],
                        total[3] / total[0],
                    ]
                } else {
                    [0.0; 3]
                }
            })
            .collect();
        Self { indices, means }
    }

    fn get(&self, i: usize) -> [f32; 3] {
        self.indices
            .binary_search(&i)
            .map_or([0.0; 3], |k| self.means[k])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn brute_force(feature: &[bool], width: usize, height: usize) -> Vec<f32> {
        (0..width * height)
            .map(|i| {
                let (x, y) = ((i % width) as f32, (i / width) as f32);
                (0..width * height)
                    .filter(|j| feature[*j])
                    .map(|j| {
                        let (fx, fy) = ((j % width) as f32, (j / width) as f32);
                        (x - fx).powi(2) + (y - fy).powi(2)
                    })
                    .fold(f32::INFINITY, f32::min)
            })
            .collect()
    }

    #[test]
    fn distance_transform_is_exact() {
        let (width, height) = (23, 17);
        let feature: Vec<bool> = (0..width * height)
            .map(|i| (i * 7919) % 31 == 0 || i == 5)
            .collect();
        let (distance, nearest) = distance_transform(&feature, width, height);
        let expected = brute_force(&feature, width, height);
        for i in 0..width * height {
            assert_eq!(distance[i], expected[i], "pixel {i}");
            let j = nearest[i] as usize;
            assert!(feature[j]);
            let d = ((i % width) as f32 - (j % width) as f32).powi(2)
                + ((i / width) as f32 - (j / width) as f32).powi(2);
            assert_eq!(d, expected[i]);
        }
        let (none, nearest) = distance_transform(&[false; 6], 3, 2);
        assert!(none.iter().all(|d| d.is_infinite()));
        assert!(nearest.iter().all(|n| *n == u32::MAX));
    }

    /// A photo with a soft vertical edge from red (left) to blue (right),
    /// half way at `edge`, and a step mask whose 50% line is `offset` px off.
    fn soft_edge(
        width: u32,
        height: u32,
        edge: f32,
        offset: f32,
    ) -> (ParametricMaskSource, Vec<f32>) {
        let lut = srgb8_linear_lut();
        let encode = |linear: f32| {
            (0..256)
                .min_by(|a, b| {
                    (lut[*a] - linear)
                        .abs()
                        .partial_cmp(&(lut[*b] - linear).abs())
                        .unwrap()
                })
                .unwrap() as u8
        };
        let image = image::RgbaImage::from_fn(width, height, |x, _| {
            // Coverage of red in linear light: a 6 px wide ramp.
            let a = (0.5 - (x as f32 + 0.5 - edge) / 6.0).clamp(0.0, 1.0);
            Rgba([encode(0.8 * a), encode(0.05), encode(0.7 * (1.0 - a)), 255])
        });
        let source = ParametricMaskSource::new(
            Arc::new(DynamicImage::ImageRgba8(image)),
            (width as f64, height as f64),
        )
        .unwrap();
        let mask = (0..width * height)
            .map(|i| ((i % width) as f32 + 0.5 < edge + offset) as u8 as f32)
            .collect();
        (source, mask)
    }

    #[test]
    fn refine_edge_moves_the_edge_to_the_photos() {
        let (width, height) = (120, 40);
        for offset in [-9.0, 7.0] {
            let (source, mut mask) = soft_edge(width, height, 60.0, offset);
            let sampler = SourceSampler::new(&source, 1.0, (0.0, 0.0));
            refine_edge(
                &mut mask,
                width as usize,
                height as usize,
                16.0,
                &sampler,
                &RefineTuning::DEFAULT,
            );
            let row = &mask[20 * width as usize..21 * width as usize];
            // The photo's own mix: 50% at x + 0.5 = 60, a 6 px ramp.
            for (x, value) in row.iter().enumerate() {
                let expected = (0.5 - (x as f32 + 0.5 - 60.0) / 6.0).clamp(0.0, 1.0);
                assert!(
                    (value - expected).abs() < 0.03,
                    "offset {offset}, x {x}: {value} vs {expected}"
                );
            }
        }
    }

    /// `SureMeans` the old way: the whole region blurred.
    fn sure_mean(
        colours: &[[f32; 3]],
        sure: &[bool],
        width: usize,
        height: usize,
        sigma: f32,
    ) -> Vec<[f32; 3]> {
        let mut weight: Vec<f32> = sure.iter().map(|s| *s as u8 as f32).collect();
        gaussian_blur(&mut weight, width, height, sigma);
        let channels: Vec<Vec<f32>> = (0..3)
            .into_par_iter()
            .map(|c| {
                let mut channel: Vec<f32> = colours
                    .iter()
                    .zip(sure)
                    .map(|(colour, s)| if *s { colour[c] } else { 0.0 })
                    .collect();
                gaussian_blur(&mut channel, width, height, sigma);
                channel
            })
            .collect();
        (0..width * height)
            .map(|i| {
                if sure[i] && weight[i] > 0.0 {
                    [
                        channels[0][i] / weight[i],
                        channels[1][i] / weight[i],
                        channels[2][i] / weight[i],
                    ]
                } else {
                    [0.0; 3]
                }
            })
            .collect()
    }

    #[test]
    fn sure_means_match_blurring_the_whole_region() {
        let (w, h) = (90usize, 70usize);
        let colours: Vec<[f32; 3]> = (0..w * h)
            .map(|i| {
                let (x, y) = ((i % w) as f32, (i / w) as f32);
                [
                    (x * 0.37).sin().abs(),
                    (y * 0.11).cos().abs(),
                    ((x + y) * 0.05).fract(),
                ]
            })
            .collect();
        let sure: Vec<bool> = (0..w * h)
            .map(|i| (i * 7919) % 13 < 9 && i % w > 3)
            .collect();
        for sigma in [0.75f32, 3.3] {
            let whole = sure_mean(&colours, &sure, w, h, sigma);
            let lazy = SureMeans::at(&colours, &sure, w, h, sigma, (0..w * h).rev().collect());
            for (i, mean) in whole.iter().enumerate() {
                assert_eq!(lazy.get(i), *mean, "sigma {sigma}, pixel {i}");
            }
        }
    }

    #[test]
    fn refine_edge_v3_reads_a_thin_masks_far_side() {
        // A thin stroke 8 px inside a 40 px wide soft edge: its sure
        // foreground is itself a mix, and its inner outside is the subject.
        // Version 2 takes that outside for background and drops the subject;
        // version 3 keeps it and follows the photo's own mix.
        let (width, height) = (240usize, 20usize);
        let lut = srgb8_linear_lut();
        let encode = |linear: f32| {
            (0..256)
                .min_by(|a, b| {
                    (lut[*a] - linear)
                        .abs()
                        .total_cmp(&(lut[*b] - linear).abs())
                })
                .unwrap() as u8
        };
        let share = |x: f32| (0.5 - (x + 0.5 - 120.0) / 40.0).clamp(0.0, 1.0);
        let image = image::RgbaImage::from_fn(width as u32, height as u32, |x, _| {
            let a = share(x as f32);
            Rgba([encode(0.8 * a), encode(0.05), encode(0.7 * (1.0 - a)), 255])
        });
        let source = ParametricMaskSource::new(
            Arc::new(DynamicImage::ImageRgba8(image)),
            (width as f64, height as f64),
        )
        .unwrap();
        let sampler = SourceSampler::new(&source, 1.0, (0.0, 0.0));
        let stroke: Vec<f32> = (0..width * height)
            .map(|i| (((i % width) as f32 + 0.5 - 112.0).abs() < 20.0) as u8 as f32)
            .collect();
        let refined = |version: Option<u32>| {
            let mut mask = stroke.clone();
            refine_edge(
                &mut mask,
                width,
                height,
                30.0,
                &sampler,
                &RefineTuning::for_version(version),
            );
            mask[10 * width..11 * width].to_vec()
        };
        let (v2, v3) = (refined(Some(2)), refined(Some(3)));
        let mean = |row: &[f32], xs: std::ops::Range<usize>| {
            row[xs.clone()].iter().sum::<f32>() / xs.len() as f32
        };
        assert!(mean(&v2, 94..100) < 0.6, "version 2 {:?}", &v2[94..100]);
        assert!(mean(&v3, 94..100) > 0.9, "version 3 {:?}", &v3[94..100]);
        for (x, value) in v3.iter().enumerate().take(128).skip(104) {
            let expected = share(x as f32);
            assert!(
                (value - expected).abs() < 0.1,
                "x {x}: {value} vs {expected}"
            );
        }
    }

    #[test]
    fn refine_edge_leaves_alike_colours_and_far_pixels_alone() {
        let (width, height) = (80, 20);
        let image = image::RgbaImage::from_pixel(width, height, Rgba([90, 120, 60, 255]));
        let source = ParametricMaskSource::new(
            Arc::new(DynamicImage::ImageRgba8(image)),
            (width as f64, height as f64),
        )
        .unwrap();
        let sampler = SourceSampler::new(&source, 1.0, (0.0, 0.0));
        let mask: Vec<f32> = (0..width * height)
            .map(|i| (i % width) as f32 / (width - 1) as f32)
            .collect();
        let mut refined = mask.clone();
        refine_edge(&mut refined, 80, 20, 10.0, &sampler, &RefineTuning::DEFAULT);
        assert_eq!(refined, mask);
    }

    fn layer(
        mask_version: Option<u32>,
        refine_edge: Option<f32>,
        opacity: f32,
        sub: Value,
    ) -> MaskDefinition {
        serde_json::from_value(serde_json::json!({
            "id": "l", "name": "l", "visible": true, "invert": false, "opacity": opacity,
            "adjustments": {}, "subMasks": [sub],
            "maskVersion": mask_version, "refineEdge": refine_edge,
        }))
        .unwrap()
    }

    fn radial() -> Value {
        serde_json::json!({"id": "r", "type": "radial", "visible": true, "mode": "additive",
            "parameters": {"centerX": 40.3, "centerY": 30.7, "radiusX": 30.0, "radiusY": 20.0,
                           "rotation": 15.0, "feather": 0.8}})
    }

    fn brush() -> Value {
        serde_json::json!({"id": "b", "type": "brush", "visible": true, "mode": "additive",
            "parameters": {"lines": [{"tool": "brush", "brushSize": 20.0, "feather": 0.5,
                "points": [{"x": 10.0, "y": 10.0}, {"x": 60.0, "y": 40.0}]}]}})
    }

    #[test]
    fn version_two_keeps_bytes_where_they_are_exact() {
        let geometry = PhotoGeometry::default();
        let generate = |def: &MaskDefinition| {
            generate_layer_mask(def, 80, 60, 1.0, (0.0, 0.0), &geometry, None).unwrap()
        };
        // A brush layer at full opacity: the same bytes, no 16-bit plane.
        let old = generate(&layer(None, None, 100.0, brush()));
        let new = generate(&layer(Some(2), None, 100.0, brush()));
        assert!(old.wide().is_none() && new.wide().is_none());
        assert_eq!(old, new);
        // Gradients and opacity below 100% are 16-bit; their bytes are the
        // rounded coverage.
        for def in [
            layer(Some(2), None, 100.0, radial()),
            layer(Some(2), None, 37.0, brush()),
        ] {
            let mask = generate(&def);
            let wide = mask.wide().unwrap();
            for (byte, value) in mask.as_raw().iter().zip(wide) {
                assert_eq!(*byte, (*value as f32 / 65535.0 * 255.0).round() as u8);
            }
        }
        // The 16-bit radial gradient follows the shape without 8-bit steps.
        let mask = generate(&layer(Some(2), None, 100.0, radial()));
        let levels: std::collections::HashSet<u16> = mask.wide().unwrap().iter().copied().collect();
        let bytes: std::collections::HashSet<u8> = mask.as_raw().iter().copied().collect();
        assert!(
            levels.len() > 4 * bytes.len(),
            "{} vs {}",
            levels.len(),
            bytes.len()
        );
        // Without the photo, Refine edge leaves the layer as it is.
        let refined = generate(&layer(Some(2), Some(12.0), 100.0, radial()));
        assert_eq!(refined, mask);
    }

    #[test]
    fn version_two_opacity_from_the_cache_matches_a_fresh_mask() {
        let geometry = PhotoGeometry::default();
        for sub in [radial(), brush()] {
            for opacity in [100.0, 61.0, 3.0] {
                let def = layer(Some(2), None, opacity, sub.clone());
                let full = MaskDefinition {
                    opacity: 100.0,
                    ..def.clone()
                };
                let cache = std::sync::Mutex::new(MaskBitmapCache::new());
                cached_or_generated_mask(&cache, &full, 80, 60, 1.0, (0.0, 0.0), &geometry, None)
                    .unwrap();
                let derived = cached_or_generated_mask(
                    &cache,
                    &def,
                    80,
                    60,
                    1.0,
                    (0.0, 0.0),
                    &geometry,
                    None,
                )
                .unwrap()
                .unwrap();
                let fresh =
                    generate_layer_mask(&def, 80, 60, 1.0, (0.0, 0.0), &geometry, None).unwrap();
                assert_eq!(*derived, fresh, "opacity {opacity}");
            }
        }
    }

    /// Refine edge's cost on a preview and a full-resolution grid with a
    /// large subject (its whole outline in the band).
    /// `cargo test -p rawtonic-engine --release -- --ignored refine_edge_timing --nocapture`
    #[test]
    #[ignore = "timing"]
    fn refine_edge_timing() {
        for (width, height, radius) in [(1536u32, 1024u32, 10.0f32), (6000, 4000, 40.0)] {
            let image = image::RgbaImage::from_fn(width, height, |x, y| {
                let d = ((x as f32 - width as f32 / 2.0).powi(2)
                    + (y as f32 - height as f32 / 2.0).powi(2))
                .sqrt();
                let v = ((d / height as f32 * 7.0).sin() * 60.0 + 120.0) as u8;
                Rgba([v, 255 - v, (x % 251) as u8, 255])
            });
            let source = ParametricMaskSource::new(
                Arc::new(DynamicImage::ImageRgba8(image)),
                (width as f64, height as f64),
            )
            .unwrap();
            let sampler = SourceSampler::new(&source, 1.0, (0.0, 0.0));
            let disc: Vec<f32> = (0..width * height)
                .map(|i| {
                    let (x, y) = ((i % width) as f32, (i / width) as f32);
                    let d = ((x - width as f32 / 2.0).powi(2) + (y - height as f32 / 2.0).powi(2))
                        .sqrt();
                    (d < height as f32 * 0.4) as u8 as f32
                })
                .collect();
            // A thin stroke (narrower than the radius) across the photo.
            let stroke: Vec<f32> = (0..width * height)
                .map(|i| {
                    let (x, y) = ((i % width) as f32, (i / width) as f32);
                    let centre =
                        height as f32 / 2.0 + (x / width as f32 * 9.0).sin() * radius * 3.0;
                    ((y - centre).abs() < radius * 0.7) as u8 as f32
                })
                .collect();
            for (shape, mask) in [("disc", &disc), ("thin stroke", &stroke)] {
                for version in [2, 3] {
                    let mut times = Vec::new();
                    for _ in 0..5 {
                        let mut m = mask.clone();
                        let start = std::time::Instant::now();
                        refine_edge(
                            &mut m,
                            width as usize,
                            height as usize,
                            radius,
                            &sampler,
                            &RefineTuning::for_version(Some(version)),
                        );
                        times.push(start.elapsed().as_secs_f64() * 1000.0);
                    }
                    times.sort_by(|a, b| a.partial_cmp(b).unwrap());
                    println!(
                        "refine_edge {width}x{height} radius {radius}, {shape}, version {version}: median {:.1} ms (min {:.1}, max {:.1})",
                        times[2], times[0], times[4]
                    );
                }
            }
        }
    }

    /// The layer stage of a live brush stroke per move (the brush prefix
    /// cache keeps earlier moves): 8-bit and version 2 layers at full and
    /// 60 % opacity, a 300 px brush on a 1536 × 1024 preview.
    /// `cargo test -p rawtonic-engine --release -- --ignored brush_layer_stage_timing --nocapture`
    #[test]
    #[ignore = "timing: run with --release"]
    fn brush_layer_stage_timing() {
        let (w, h) = (1536u32, 1024u32);
        let scale = w as f32 / 7728.0;
        let geometry = PhotoGeometry::default();
        for (label, version, opacity) in [
            ("8-bit, 100%", None, 100.0),
            ("8-bit, 60%", None, 60.0),
            ("v2, 100%", Some(2), 100.0),
            ("v2, 60%", Some(2), 60.0),
        ] {
            let mut times = Vec::new();
            for stroke in 0..5 {
                clear_brush_prefixes();
                let y = 2000.0 + stroke as f64 * 300.0;
                for n in 2..=60usize {
                    let points: Vec<Value> = (0..n)
                        .map(|i| serde_json::json!({"x": 800.0 + i as f64 * 100.0, "y": y + 40.0 * (i as f64 / 6.0).sin()}))
                        .collect();
                    let sub = serde_json::json!({"id": "b", "type": "brush", "visible": true, "mode": "additive",
                        "parameters": {"lines": [{"tool": "brush", "brushSize": 300.0, "feather": 0.5,
                            "hardness": 50.0, "points": points}]}});
                    let def = layer(version, None, opacity, sub);
                    let started = std::time::Instant::now();
                    let mask = generate_layer_mask(&def, w, h, scale, (0.0, 0.0), &geometry, None);
                    times.push(started.elapsed().as_secs_f64() * 1000.0);
                    std::hint::black_box(mask);
                }
            }
            times.sort_by(f64::total_cmp);
            let at = |q: f64| times[((times.len() - 1) as f64 * q).round() as usize];
            println!(
                "{label}: layer stage per move p50 {:.2} ms, p95 {:.2} ms ({} moves)",
                at(0.5),
                at(0.95),
                times.len()
            );
        }
    }

    #[test]
    fn rank_filter_and_blur_match_their_definitions() {
        let (width, height) = (9, 7);
        let mut src = vec![0.0; width * height];
        src[3 * width + 4] = 1.0;
        let grown = rank_filter(&src, width, height, 2, true);
        for y in 0..height {
            for x in 0..width {
                let inside = x.abs_diff(4) <= 2 && y.abs_diff(3) <= 2;
                assert_eq!(grown[y * width + x], inside as u8 as f32);
            }
        }
        assert_eq!(rank_filter(&grown, width, height, 2, false), src);
        let mut flat = vec![0.25; width * height];
        gaussian_blur(&mut flat, width, height, 1.5);
        assert!(flat.iter().all(|v| (v - 0.25).abs() < 1e-6));
    }
}
