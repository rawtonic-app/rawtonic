//! Edge fringes: colour along strong luminance edges that neither side of
//! the edge explains — demosaic false colour on thin lines near the
//! sensor's resolution limit, the rest of lateral chromatic aberration and
//! colour bleeding at dark-on-bright edges (branches against a bright sky).
//!
//! Since the multi-scale colour noise filter (`colorNoiseVersion` 2) the base
//! image is developed without the decode-time chroma blur, and the filter
//! protects colour at luminance edges, so these fringes stayed visible
//! where Capture One and Camera Raw show clean edges (notes:
//! reference/edge-fringes.md, G05). This pass runs on the lens-corrected
//! image (after the warp, so after any lateral CA correction) and before
//! the renderer's noise reduction and sharpening.
//!
//! Method: in square-root encoded RGB the colour differences `u = √R − √G`
//! and `v = √B − √G` are modelled in every 5 × 5 window as linear in the
//! luminance `Y = √((R + 2G + B) / 4)` (a guided filter with luminance as
//! the guide). A two-colour edge — a red object against grey, a dark branch
//! against the sky — is linear in luminance and kept; colour that changes
//! sides across a dark line (red on one side, cyan on the other) or
//! alternates along it is not, and is replaced by the window's model. Only
//! pixels near strong luminance edges change, flat areas (colour noise,
//! colour texture) are left to the noise reduction; a pixel's chroma is
//! never raised, so no colour is added.
//!
//! Version 3 adds a second stage for edges between two saturated colours
//! (a yellow coat against grey, cyan against magenta), where the demosaic
//! interpolates red and blue across the edge a pixel or two away from
//! green: the transition pixels get a colour or a brightness neither side
//! has (a light seam between cyan and magenta of equal luminance, which
//! the luminance model cannot see). Blur mixes the two sides in linear
//! light, so in a window holding two colours every pixel lies on the line
//! between them; the stage moves a pixel onto its window's principal colour
//! line (in linear RGB) where the window holds two colours and a strong
//! edge, and leaves windows of three or more colours alone.

use std::borrow::Cow;

use image::DynamicImage;
use rayon::prelude::*;

/// The recipe key naming the edge-fringe treatment a recipe was made with.
pub const VERSION_KEY: &str = "fringeVersion";
/// The newest treatment: 1 (2026-10-16) removes edge fringes as described
/// above in 5 × 5 windows; 2 (2026-10-17) in 7 × 7 windows; 3 (2026-10-20)
/// adds the colour-line stage. Recipes without the key keep rendering
/// without it, recipes with 1 or 2 as before.
pub const VERSION: u64 = 3;

/// Window radius at full resolution: version 1's 5 × 5 spans a fringe on
/// both sides of a thin line. A lateral-CA rest of 1–2 px beside an edge
/// (bodies whose CA data under-state the CA, GF5) or a fringe on a 2–3 px
/// line keeps colour in it, which the colour noise reduction then breaks
/// into blotches along the edge; version 2's 7 × 7 removes those (bright-
/// edge excess lower on 50 of 55 files, GF5 3.33 → 2.40, GH3 1.63 → 0.90,
/// TZ96 1.89 → 0.97; neutral-edge chroma lower on 39, higher on 1; notes:
/// reference/edge-fringes.md "Version 2"). On a noisy photo (luminance
/// noise from half the edge test's start, about ISO 3200 and up) noise
/// alone passes the edge test, the 7 × 7 model evens out the fine chroma
/// noise unevenly and the colour noise reduction leaves blotches (EOS R10
/// ISO 32000: 14 → 1560 isolated specks per Mpx), so version 2 keeps 5 × 5
/// there and renders as version 1. `noise` is the full-resolution
/// luminance noise (`luminance_noise`).
fn radius_for(version: u64, noise: f32) -> f32 {
    if low_noise(version, noise) { 3.0 } else { 2.0 }
}

/// Whether version 2's wider windows and version 3's colour lines apply.
fn low_noise(version: u64, noise: f32) -> bool {
    version >= 2 && noise < 0.5 * EDGE_LOW
}

/// What the pass does to an image: the window radius (in the image's
/// pixels) and whether the colour-line stage runs. Version 3 runs it on
/// low-noise photos only: on noisy ones it found colour lines in the noise
/// and raised isolated colour specks after the colour noise reduction (EOS
/// R10 ISO 32000 651 → 1213 per Mpx, K-7 310 → 502; notes:
/// reference/edge-fringes.md "Version 3"), so they render as version 1.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Plan {
    pub radius: usize,
    pub colour_lines: bool,
}

/// The plan for `version` on an image at `scale` of the full resolution
/// whose full-resolution luminance noise is `noise`.
fn plan_for(version: u64, noise: f32, scale: f32) -> Plan {
    Plan {
        radius: ((radius_for(version, noise) * scale.min(1.0)).round() as usize).max(1),
        colour_lines: version >= 3 && low_noise(version, noise),
    }
}

/// Runs the pass as `plan` says on `w × h` pixels of `stride` floats (RGB
/// first): the luminance model, then the colour lines on its result.
pub fn apply(data: &mut [f32], w: usize, h: usize, stride: usize, plan: Plan) {
    suppress(data, w, h, stride, plan.radius);
    if plan.colour_lines {
        colour_lines(data, w, h, stride, plan.radius);
    }
}

/// Regularisation of the colour-on-luminance slope (square-root units²):
/// windows with little luminance variation keep their mean colour.
const EPS: f32 = 4e-4;
/// Luminance standard deviation (square-root units) of a window from which
/// a pixel starts to count as near an edge, and from which fully.
const EDGE_LOW: f32 = 0.03;
const EDGE_HIGH: f32 = 0.08;
/// Below this resolution (reduced previews, thumbnails) a full-resolution
/// fringe of one or two pixels is averaged away; the pass is skipped.
const MIN_SCALE: f32 = 0.35;
/// Rows per parallel band.
const BAND: usize = 24;
/// Rows per parallel band of the colour-line stage (it reads 2r rows
/// around each band for 18 moments: taller bands spend less on them).
const LINE_BAND: usize = 40;
/// Regularisation of the colour line (linear units²): a window of little
/// colour variation keeps its mean less a small part of the variation.
const LINE_EPS: f32 = 1e-6;
/// Share of a window's colour variance off its principal line (in square-
/// root RGB) up to which the window counts as holding two colours fully,
/// and from which not at all: a misregistered transition adds a few
/// percent, a third colour of a few pixels much more.
const TWO_COLOUR_FULL: f32 = 0.05;
const TWO_COLOUR_NONE: f32 = 0.15;

/// The recipe's edge-fringe version: `fringeVersion`, 0 (none) when
/// missing, never newer than this renderer knows.
pub fn version_of(adjustments: &serde_json::Value) -> u64 {
    adjustments
        .get(VERSION_KEY)
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0)
        .min(VERSION)
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

/// The pass for a recipe on an image at `scale` of the full resolution
/// (1 for exports and full-resolution previews).
pub fn apply_for_recipe<'a>(
    image: Cow<'a, DynamicImage>,
    adjustments: &serde_json::Value,
    scale: f32,
) -> Cow<'a, DynamicImage> {
    let version = version_of(adjustments);
    if version == 0 || scale.is_nan() || scale < MIN_SCALE {
        return image;
    }
    if !matches!(
        image.as_ref(),
        DynamicImage::ImageRgb32F(_) | DynamicImage::ImageRgba32F(_)
    ) {
        return image;
    }
    let mut image = image;
    let (w, h) = (image.width() as usize, image.height() as usize);
    // A reduced preview averages the noise down by about its scale.
    let plan = |data: &[f32], stride| {
        let noise = if version >= 2 {
            luminance_noise(data, w, h, stride) / scale.min(1.0)
        } else {
            0.0
        };
        plan_for(version, noise, scale)
    };
    match image.to_mut() {
        DynamicImage::ImageRgb32F(img) => {
            let plan = plan(img.as_raw(), 3);
            apply(img.as_mut(), w, h, 3, plan)
        }
        DynamicImage::ImageRgba32F(img) => {
            let plan = plan(img.as_raw(), 4);
            apply(img.as_mut(), w, h, 4, plan)
        }
        _ => {}
    }
    image
}

#[inline]
fn enc(v: f32) -> f32 {
    v.max(0.0).sqrt()
}

/// `out[x]` = sum of `row` over `x - r ..= x + r` (edges repeated), using
/// `pad` (at least `row.len() + 2r` long) as scratch.
fn box_row(row: &[f32], r: usize, pad: &mut [f32], out: &mut [f32]) {
    let w = row.len();
    let pad = &mut pad[..w + 2 * r];
    pad[..r].fill(row[0]);
    pad[r..r + w].copy_from_slice(row);
    pad[r + w..].fill(row[w - 1]);
    out.copy_from_slice(&pad[..w]);
    for dx in 1..=2 * r {
        for (o, v) in out.iter_mut().zip(&pad[dx..dx + w]) {
            *o += v;
        }
    }
}

/// `out` = sum of the `2r + 1` rows of `rows` (each `w` long) starting at
/// row `first`.
fn box_column(rows: &[f32], w: usize, first: usize, r: usize, out: &mut [f32]) {
    out.copy_from_slice(&rows[first * w..(first + 1) * w]);
    for k in first + 1..=first + 2 * r {
        for (o, v) in out.iter_mut().zip(&rows[k * w..(k + 1) * w]) {
            *o += v;
        }
    }
}

/// The photo's luminance noise (standard deviation, square-root units):
/// from the median absolute difference of horizontal neighbours on every
/// fourth row and second column, which edges and texture barely move.
fn luminance_noise(data: &[f32], w: usize, h: usize, stride: usize) -> f32 {
    if w < 2 {
        return 0.0;
    }
    let diffs = (0..h)
        .step_by(4)
        .flat_map(|y| row_differences(&data[y * w * stride..(y + 1) * w * stride], w, stride))
        .collect();
    median_noise(diffs)
}

/// The sampled neighbour differences of one row of `w` pixels.
fn row_differences(row: &[f32], w: usize, stride: usize) -> impl Iterator<Item = f32> + '_ {
    let lum = move |x: usize| {
        let px = &row[x * stride..x * stride + 3];
        enc(0.25 * (px[0] + 2.0 * px[1] + px[2]))
    };
    (0..w.saturating_sub(1))
        .step_by(2)
        .map(move |x| (lum(x + 1) - lum(x)).abs())
}

fn median_noise(mut diffs: Vec<f32>) -> f32 {
    if diffs.is_empty() {
        return 0.0;
    }
    let mid = diffs.len() / 2;
    let (_, median, _) = diffs.select_nth_unstable_by(mid, f32::total_cmp);
    // Gaussian noise: median |difference| = 0.6745 · √2 · σ.
    *median / (0.6745 * std::f32::consts::SQRT_2)
}

/// The plan [`apply_for_recipe`] uses at full resolution for
/// `adjustments` on a `w × h` RGB image whose row `y` `row` gives (only
/// every fourth row is asked for): what a region cut from that image needs
/// to suppress fringes as the whole image does. None without the pass.
pub fn full_resolution_plan(
    adjustments: &serde_json::Value,
    w: usize,
    h: usize,
    row: impl Fn(usize) -> Vec<f32> + Sync,
) -> Option<Plan> {
    let version = version_of(adjustments);
    if version == 0 {
        return None;
    }
    let noise = if version >= 2 && w >= 2 {
        let rows: Vec<usize> = (0..h).step_by(4).collect();
        let diffs = rows
            .into_par_iter()
            .flat_map_iter(|y| row_differences(&row(y), w, 3).collect::<Vec<_>>())
            .collect();
        median_noise(diffs)
    } else {
        0.0
    };
    Some(plan_for(version, noise, 1.0))
}

/// How far [`apply`] reads around a pixel (rows and columns): each stage
/// reads twice its radius.
pub fn reach(plan: Plan) -> usize {
    if plan.colour_lines {
        4 * plan.radius
    } else {
        2 * plan.radius
    }
}

/// Removes edge fringes in place. `data` holds `w × h` pixels of `stride`
/// floats (RGB first). Each band of rows reads the unmodified image around
/// it and returns its changes, which are written once all bands are done.
pub fn suppress(data: &mut [f32], w: usize, h: usize, stride: usize, r: usize) {
    if w == 0 || h == 0 {
        return;
    }
    let source: &[f32] = data;
    let changes: Vec<Vec<(usize, f32, f32)>> = (0..h.div_ceil(BAND))
        .into_par_iter()
        .map(|band| {
            suppress_band(
                source,
                w,
                h,
                stride,
                r,
                band * BAND,
                ((band + 1) * BAND).min(h),
            )
        })
        .collect();
    for band in changes {
        for (i, red, blue) in band {
            data[i * stride] = red;
            data[i * stride + 2] = blue;
        }
    }
}

fn suppress_band(
    data: &[f32],
    w: usize,
    h: usize,
    stride: usize,
    r: usize,
    y0: usize,
    y1: usize,
) -> Vec<(usize, f32, f32)> {
    let row_of = |y: isize| y.clamp(0, h as isize - 1) as usize;
    let inv_area = 1.0 / ((2 * r + 1) * (2 * r + 1)) as f32;
    let mut pad = vec![0.0f32; w + 2 * r];
    // Rows y0 - 2r .. y1 + 2r: luminance and colour differences, and the
    // horizontal sums of their moments Y, u, v, YY, Yu, Yv.
    let ext0 = y0 as isize - 2 * r as isize;
    let ext_rows = (y1 - y0) + 4 * r;
    let (mut lum, mut du, mut dv) = (
        vec![0.0f32; ext_rows * w],
        vec![0.0f32; ext_rows * w],
        vec![0.0f32; ext_rows * w],
    );
    for e in 0..ext_rows {
        let y = row_of(ext0 + e as isize);
        let src = &data[y * w * stride..(y + 1) * w * stride];
        let span = e * w..(e + 1) * w;
        for (((px, l), u), v) in src
            .chunks_exact(stride)
            .zip(&mut lum[span.clone()])
            .zip(&mut du[span.clone()])
            .zip(&mut dv[span])
        {
            let g = enc(px[1]);
            *l = enc(0.25 * (px[0] + 2.0 * px[1] + px[2]));
            *u = enc(px[0]) - g;
            *v = enc(px[2]) - g;
        }
    }
    let mut hsum: [Vec<f32>; 6] = std::array::from_fn(|_| vec![0.0f32; ext_rows * w]);
    let mut moment = vec![0.0f32; w];
    for e in 0..ext_rows {
        let span = e * w..(e + 1) * w;
        let (l, u, v) = (&lum[span.clone()], &du[span.clone()], &dv[span.clone()]);
        for (k, sum) in hsum.iter_mut().enumerate() {
            match k {
                0 => moment.copy_from_slice(l),
                1 => moment.copy_from_slice(u),
                2 => moment.copy_from_slice(v),
                _ => {
                    let other = [l, u, v][k - 3];
                    for ((m, a), b) in moment.iter_mut().zip(l).zip(other) {
                        *m = a * b;
                    }
                }
            }
            box_row(&moment, r, &mut pad, &mut sum[span.clone()]);
        }
    }
    // Rows y0 - r .. y1 + r: the window's model (slopes and offsets of u and
    // v on Y) and its edge weight, then summed along rows. Mid row m is ext
    // row m + r; its window spans ext rows m ..= m + 2r.
    let mid_rows = (y1 - y0) + 2 * r;
    let mut csum: [Vec<f32>; 5] = std::array::from_fn(|_| vec![0.0f32; mid_rows * w]);
    let mut vs: [Vec<f32>; 6] = std::array::from_fn(|_| vec![0.0f32; w]);
    let mut coef: [Vec<f32>; 5] = std::array::from_fn(|_| vec![0.0f32; w]);
    for m in 0..mid_rows {
        for (k, out) in vs.iter_mut().enumerate() {
            box_column(&hsum[k], w, m, r, out);
        }
        for x in 0..w {
            let my = vs[0][x] * inv_area;
            let mu = vs[1][x] * inv_area;
            let mv = vs[2][x] * inv_area;
            let var = (vs[3][x] * inv_area - my * my).max(0.0);
            let au = (vs[4][x] * inv_area - my * mu) / (var + EPS);
            let av = (vs[5][x] * inv_area - my * mv) / (var + EPS);
            coef[0][x] = au;
            coef[1][x] = mu - au * my;
            coef[2][x] = av;
            coef[3][x] = mv - av * my;
            coef[4][x] = ((var.sqrt() - EDGE_LOW) / (EDGE_HIGH - EDGE_LOW)).clamp(0.0, 1.0);
        }
        let span = m * w..(m + 1) * w;
        for (k, sum) in csum.iter_mut().enumerate() {
            box_row(&coef[k], r, &mut pad, &mut sum[span.clone()]);
        }
    }
    let mut changes = Vec::new();
    let mut model: [Vec<f32>; 5] = std::array::from_fn(|_| vec![0.0f32; w]);
    for y in y0..y1 {
        // Output row y is mid row y - y0 + r (its window: mid rows y - y0 ..=
        // y - y0 + 2r) and ext row y - y0 + 2r.
        let m0 = y - y0;
        for (k, out) in model.iter_mut().enumerate() {
            box_column(&csum[k], w, m0, r, out);
        }
        let e = y - y0 + 2 * r;
        for x in 0..w {
            // Spread over the window, so a fringe a pixel or two beside the
            // edge's centre is covered fully.
            let weight = (2.0 * model[4][x] * inv_area).min(1.0);
            if weight <= 0.0 {
                continue;
            }
            let i = y * w + x;
            let px = &data[i * stride..][..3];
            if px.iter().any(|&c| c < 0.0 || !c.is_finite()) {
                continue;
            }
            let (l, u0, v0) = (lum[e * w + x], du[e * w + x], dv[e * w + x]);
            let mut u1 = (model[0][x] * l + model[1][x]) * inv_area;
            let mut v1 = (model[2][x] * l + model[3][x]) * inv_area;
            let (c0, c1) = (u0.hypot(v0), u1.hypot(v1));
            if c1 > c0 {
                // Never more colour than the pixel had.
                let k = if c1 > 0.0 { c0 / c1 } else { 0.0 };
                u1 *= k;
                v1 *= k;
            }
            let g = enc(px[1]);
            let red = (g + u0 + weight * (u1 - u0)).max(0.0);
            let blue = (g + v0 + weight * (v1 - v0)).max(0.0);
            changes.push((i, red * red, blue * blue));
        }
    }
    changes
}

/// The colour-line stage of version 3 in place (see the module notes):
/// every pixel near a strong edge in a window of two colours moves onto
/// the window's principal colour line in linear RGB, averaged over the
/// windows that hold it (as a guided filter averages its models). Bands of
/// rows read the unmodified image around them, as [`suppress`].
fn colour_lines(data: &mut [f32], w: usize, h: usize, stride: usize, r: usize) {
    if w == 0 || h == 0 {
        return;
    }
    let source: &[f32] = data;
    let changes: Vec<Vec<(usize, [f32; 3])>> = (0..h.div_ceil(LINE_BAND))
        .into_par_iter()
        .map(|band| {
            colour_line_band(
                source,
                w,
                h,
                stride,
                r,
                band * LINE_BAND,
                ((band + 1) * LINE_BAND).min(h),
            )
        })
        .collect();
    for band in changes {
        for (i, px) in band {
            data[i * stride..i * stride + 3].copy_from_slice(&px);
        }
    }
}

/// Index pairs of the six second moments of three channels.
const PAIRS: [(usize, usize); 6] = [(0, 0), (0, 1), (0, 2), (1, 1), (1, 2), (2, 2)];

/// A row of windows' means and covariances (the six [`PAIRS`] entries),
/// principal axes and principal variances, one column per window. Row-wise
/// loops over columns, so they vectorise.
struct RowStats {
    mean: [Vec<f32>; 3],
    cov: [Vec<f32>; 6],
    axis: [Vec<f32>; 3],
    var: Vec<f32>,
}

impl RowStats {
    fn new(w: usize) -> Self {
        Self {
            mean: std::array::from_fn(|_| vec![0.0; w]),
            cov: std::array::from_fn(|_| vec![0.0; w]),
            axis: std::array::from_fn(|_| vec![0.0; w]),
            var: vec![0.0; w],
        }
    }

    /// Means and covariances from the box sums of a row of windows (three
    /// channels, then the six [`PAIRS`] products).
    fn of(&mut self, sums: &[Vec<f32>], inv_area: f32) {
        for (mean, sum) in self.mean.iter_mut().zip(sums) {
            for (m, s) in mean.iter_mut().zip(sum) {
                *m = s * inv_area;
            }
        }
        for (k, &(a, b)) in PAIRS.iter().enumerate() {
            let (ma, mb) = (&self.mean[a], &self.mean[b]);
            for (x, c) in self.cov[k].iter_mut().enumerate() {
                let v = sums[3 + k][x] * inv_area - ma[x] * mb[x];
                *c = if a == b { v.max(0.0) } else { v };
            }
        }
    }

    /// The principal axes (unit vectors) and variances of the windows at
    /// `columns` by power iteration from each covariance's largest column
    /// (three steps: the error falls with (λ2/λ1)³, under 0.4 % where a
    /// window passes the two-colour test); axis and variance 0 for a window
    /// of one colour. Other columns are left as they are.
    fn principal(&mut self, columns: &[usize]) {
        let [c00, c01, c02, c11, c12, c22] = &self.cov;
        let [e0, e1, e2] = &mut self.axis;
        for &x in columns {
            let (a, b, c, d, e, f) = (c00[x], c01[x], c02[x], c11[x], c12[x], c22[x]);
            let n0 = a * a + b * b + c * c;
            let n1 = b * b + d * d + e * e;
            let n2 = c * c + e * e + f * f;
            let mut v = if n0 >= n1 && n0 >= n2 {
                [a, b, c]
            } else if n1 >= n2 {
                [b, d, e]
            } else {
                [c, e, f]
            };
            let mul = |v: [f32; 3]| {
                [
                    a * v[0] + b * v[1] + c * v[2],
                    b * v[0] + d * v[1] + e * v[2],
                    c * v[0] + e * v[1] + f * v[2],
                ]
            };
            for _ in 0..3 {
                let inv = 1.0 / (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
                v = mul(v.map(|t| t * inv));
            }
            let n = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
            let flat = !(n > 1e-12 && n.is_finite());
            let v = if flat { [0.0; 3] } else { v.map(|t| t / n) };
            let cv = mul(v);
            e0[x] = v[0];
            e1[x] = v[1];
            e2[x] = v[2];
            self.var[x] = if flat {
                0.0
            } else {
                (v[0] * cv[0] + v[1] * cv[1] + v[2] * cv[2]).max(0.0)
            };
        }
    }

    /// The colour-line stage's weight for each window from these statistics
    /// of its square-root RGB: near a strong edge (as the luminance pass's
    /// test, a neutral edge's principal variance being three times its
    /// luminance variance) and holding two colours (little variance off its
    /// principal line). `columns` is scratch.
    fn weights(&mut self, out: &mut [f32], columns: &mut Vec<usize>) {
        // The principal variance is at most the total and at most the
        // covariance's Frobenius norm: windows failing either test with
        // those bounds need no principal axis.
        let [c00, c01, c02, c11, c12, c22] = &self.cov;
        columns.clear();
        columns.extend((0..out.len()).filter(|&x| {
            let total = c00[x] + c11[x] + c22[x];
            let off = c01[x] * c01[x] + c02[x] * c02[x] + c12[x] * c12[x];
            let frobenius =
                (c00[x] * c00[x] + c11[x] * c11[x] + c22[x] * c22[x] + 2.0 * off).sqrt();
            let bound = total.min(frobenius);
            bound > 3.0 * EDGE_LOW * EDGE_LOW && total - bound < TWO_COLOUR_NONE * bound
        }));
        out.fill(0.0);
        self.principal(columns);
        let [c00, _, _, c11, _, c22] = &self.cov;
        for &x in columns.iter() {
            let out = &mut out[x];
            let var = self.var[x];
            let total = c00[x] + c11[x] + c22[x];
            let off_line = ((total - var) / var.max(1e-20)).max(0.0);
            let edge = (((var / 3.0).sqrt() - EDGE_LOW) / (EDGE_HIGH - EDGE_LOW)).clamp(0.0, 1.0);
            let two = ((TWO_COLOUR_NONE - off_line) / (TWO_COLOUR_NONE - TWO_COLOUR_FULL))
                .clamp(0.0, 1.0);
            *out = if var > 0.0 { edge * two } else { 0.0 };
        }
    }
}

/// Whether each of `w × rows` cells lies within `d` cells (rows and columns)
/// of a cell of `weights` above 0.
fn near_some(weights: &[f32], w: usize, rows: usize, d: usize) -> Vec<bool> {
    let mut along = vec![false; w * rows];
    for (row, out) in weights.chunks_exact(w).zip(along.chunks_exact_mut(w)) {
        let mut last = None;
        for x in 0..w {
            if row[x] > 0.0 {
                last = Some(x);
            }
            out[x] = last.is_some_and(|l| x - l <= d);
        }
        let mut next = None;
        for x in (0..w).rev() {
            if row[x] > 0.0 {
                next = Some(x);
            }
            out[x] |= next.is_some_and(|n| n - x <= d);
        }
    }
    let mut near = vec![false; w * rows];
    for y in 0..rows {
        let (first, last) = (y.saturating_sub(d), (y + d).min(rows - 1));
        let out = &mut near[y * w..(y + 1) * w];
        for k in first..=last {
            for (o, a) in out.iter_mut().zip(&along[k * w..(k + 1) * w]) {
                *o |= *a;
            }
        }
    }
    near
}

fn colour_line_band(
    data: &[f32],
    w: usize,
    h: usize,
    stride: usize,
    r: usize,
    y0: usize,
    y1: usize,
) -> Vec<(usize, [f32; 3])> {
    let row_of = |y: isize| y.clamp(0, h as isize - 1) as usize;
    let inv_area = 1.0 / ((2 * r + 1) * (2 * r + 1)) as f32;
    let mut pad = vec![0.0f32; w + 2 * r];
    // Rows y0 - 2r .. y1 + 2r: linear RGB (the colour lines) and square-
    // root RGB (the edge and two-colour tests, on the scale of the
    // luminance pass), and the horizontal sums of their means and second
    // moments.
    let ext0 = y0 as isize - 2 * r as isize;
    let ext_rows = (y1 - y0) + 4 * r;
    let mut linear: [Vec<f32>; 3] = std::array::from_fn(|_| vec![0.0f32; ext_rows * w]);
    let mut root: [Vec<f32>; 3] = std::array::from_fn(|_| vec![0.0f32; ext_rows * w]);
    for e in 0..ext_rows {
        let y = row_of(ext0 + e as isize);
        let src = &data[y * w * stride..(y + 1) * w * stride];
        for (x, px) in src.chunks_exact(stride).enumerate() {
            for c in 0..3 {
                linear[c][e * w + x] = px[c].max(0.0);
                root[c][e * w + x] = enc(px[c]);
            }
        }
    }
    let mut hsum: Vec<Vec<f32>> = (0..18).map(|_| vec![0.0f32; ext_rows * w]).collect();
    let mut moment = vec![0.0f32; w];
    for e in 0..ext_rows {
        let span = e * w..(e + 1) * w;
        for (set, channels) in [&linear, &root].into_iter().enumerate() {
            for k in 0..9 {
                if k < 3 {
                    moment.copy_from_slice(&channels[k][span.clone()]);
                } else {
                    let (a, b) = PAIRS[k - 3];
                    for ((m, p), q) in moment
                        .iter_mut()
                        .zip(&channels[a][span.clone()])
                        .zip(&channels[b][span.clone()])
                    {
                        *m = p * q;
                    }
                }
                box_row(&moment, r, &mut pad, &mut hsum[set * 9 + k][span.clone()]);
            }
        }
    }
    // Rows y0 - r .. y1 + r: each window's weight; then, for windows within
    // 2r of a window of some weight (the only ones a changed pixel reads;
    // box sums are exact window sums, so the others may stay 0), its
    // projection onto its line as an affine map per output channel (offset
    // and three slopes); both summed along rows. Mid row m is ext row
    // m + r; its window spans ext rows m ..= m + 2r.
    let mid_rows = (y1 - y0) + 2 * r;
    let mut stats = RowStats::new(w);
    let mut vs: Vec<Vec<f32>> = (0..9).map(|_| vec![0.0f32; w]).collect();
    let mut weights = vec![0.0f32; mid_rows * w];
    let mut columns = Vec::with_capacity(w);
    for m in 0..mid_rows {
        for (k, out) in vs.iter_mut().enumerate() {
            box_column(&hsum[9 + k], w, m, r, out);
        }
        stats.of(&vs, inv_area);
        stats.weights(&mut weights[m * w..(m + 1) * w], &mut columns);
    }
    let weighted: Vec<bool> = weights
        .chunks_exact(w)
        .map(|row| row.iter().any(|&v| v > 0.0))
        .collect();
    let needed = near_some(&weights, w, mid_rows, 2 * r);
    // Per window: the offsets of the three channels, the six entries of the
    // projection's symmetric slope matrix (shrink · axis · axisᵀ, the
    // [`PAIRS`] entries) and the weight.
    let mut csum: Vec<Vec<f32>> = (0..10).map(|_| vec![0.0f32; mid_rows * w]).collect();
    let mut coef: Vec<Vec<f32>> = (0..10).map(|_| vec![0.0f32; w]).collect();
    for m in 0..mid_rows {
        if !weighted[m.saturating_sub(2 * r)..(m + 2 * r + 1).min(mid_rows)].contains(&true) {
            continue;
        }
        for (k, out) in vs.iter_mut().enumerate() {
            box_column(&hsum[k], w, m, r, out);
        }
        stats.of(&vs, inv_area);
        let weights = &weights[m * w..(m + 1) * w];
        columns.clear();
        columns.extend((0..w).filter(|&x| needed[m * w + x]));
        stats.principal(&columns);
        for c in coef.iter_mut() {
            c.fill(0.0);
        }
        for &x in &columns {
            let e = [stats.axis[0][x], stats.axis[1][x], stats.axis[2][x]];
            let mean = [stats.mean[0][x], stats.mean[1][x], stats.mean[2][x]];
            let var = stats.var[x];
            // A window of one colour (a clipped patch) maps to its mean.
            let shrink = var / (var + LINE_EPS);
            let t = shrink * (e[0] * mean[0] + e[1] * mean[1] + e[2] * mean[2]);
            for c in 0..3 {
                coef[c][x] = mean[c] - t * e[c];
            }
            for (k, &(a, b)) in PAIRS.iter().enumerate() {
                coef[3 + k][x] = shrink * e[a] * e[b];
            }
            coef[9][x] = if var > 0.0 { weights[x] } else { 0.0 };
        }
        let span = m * w..(m + 1) * w;
        for (k, sum) in csum.iter_mut().enumerate() {
            box_row(&coef[k], r, &mut pad, &mut sum[span.clone()]);
        }
    }
    let mut changes = Vec::new();
    let mut model: Vec<Vec<f32>> = (0..10).map(|_| vec![0.0f32; w]).collect();
    for y in y0..y1 {
        // Output row y is mid row y - y0 + r (its windows: mid rows y - y0
        // ..= y - y0 + 2r) and ext row y - y0 + 2r.
        let m0 = y - y0;
        if !weighted[m0..=m0 + 2 * r].contains(&true) {
            continue;
        }
        for (k, out) in model.iter_mut().enumerate() {
            box_column(&csum[k], w, m0, r, out);
        }
        let e = y - y0 + 2 * r;
        for x in 0..w {
            // Spread over the window, as in the luminance pass.
            let weight = (2.0 * model[9][x] * inv_area).min(1.0);
            if weight <= 0.0 {
                continue;
            }
            let i = y * w + x;
            let px = &data[i * stride..][..3];
            if px.iter().any(|&c| c < 0.0 || !c.is_finite()) {
                continue;
            }
            let p = [
                linear[0][e * w + x],
                linear[1][e * w + x],
                linear[2][e * w + x],
            ];
            // The [`PAIRS`] index of each slope matrix entry.
            const ENTRY: [[usize; 3]; 3] = [[0, 1, 2], [1, 3, 4], [2, 4, 5]];
            let s = |a: usize, b: usize| model[3 + ENTRY[a][b]][x];
            let out = std::array::from_fn(|c| {
                let projected =
                    (model[c][x] + s(c, 0) * p[0] + s(c, 1) * p[1] + s(c, 2) * p[2]) * inv_area;
                (p[c] + weight * (projected - p[c])).max(0.0)
            });
            changes.push((i, out));
        }
    }
    changes
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgb32FImage, RgbImage};

    fn chroma(p: [f32; 3]) -> f32 {
        let (r, g, b) = (enc(p[0]), enc(p[1]), enc(p[2]));
        (r - g).hypot(b - g)
    }

    fn run(img: &Rgb32FImage, version: u64) -> Rgb32FImage {
        let recipe = serde_json::json!({ VERSION_KEY: version });
        apply_for_recipe(
            Cow::Owned(DynamicImage::ImageRgb32F(img.clone())),
            &recipe,
            1.0,
        )
        .into_owned()
        .into_rgb32f()
    }

    /// A dark neutral line on a bright sky, with lateral-CA-like fringes: red
    /// on one side, cyan on the other.
    fn branch(width: u32) -> Rgb32FImage {
        Rgb32FImage::from_fn(40, 40, |x, _| {
            let x = x as i32;
            let (l, r) = (18, 18 + width as i32);
            if x >= l && x < r {
                image::Rgb([0.05, 0.05, 0.05])
            } else if x == l - 1 {
                image::Rgb([0.55, 0.3, 0.3])
            } else if x == r {
                image::Rgb([0.3, 0.55, 0.55])
            } else {
                image::Rgb([0.8, 0.8, 0.8])
            }
        })
    }

    #[test]
    fn fringes_beside_a_dark_line_lose_most_of_their_colour() {
        for (version, width) in (1..=VERSION).flat_map(|v| [1, 2, 3].map(|w| (v, w))) {
            let before = branch(width);
            let after = run(&before, version);
            for x in [17u32, 18 + width] {
                let (c0, c1) = (
                    chroma(before.get_pixel(x, 20).0),
                    chroma(after.get_pixel(x, 20).0),
                );
                assert!(
                    c1 < 0.4 * c0,
                    "v{version} width {width} x {x}: chroma {c0} -> {c1}"
                );
            }
        }
    }

    #[test]
    fn version_2_also_clears_two_pixel_wide_fringes() {
        // A lateral-CA rest of two pixels on each side of a dark line.
        let before = Rgb32FImage::from_fn(40, 40, |x, _| match x {
            18..=19 => image::Rgb([0.05, 0.05, 0.05]),
            16..=17 => image::Rgb([0.55, 0.3, 0.3]),
            20..=21 => image::Rgb([0.3, 0.55, 0.55]),
            _ => image::Rgb([0.8, 0.8, 0.8]),
        });
        let left = |version| {
            let after = run(&before, version);
            [16u32, 17, 20, 21]
                .iter()
                .map(|&x| chroma(after.get_pixel(x, 20).0))
                .sum::<f32>()
        };
        let (v1, v2) = (left(1), left(2));
        assert!(v2 < 0.7 * v1, "fringe chroma v1 {v1} v2 {v2}");
    }

    #[test]
    fn real_two_colour_edges_and_thin_coloured_lines_keep_their_colour() {
        // A saturated red line (1-3 px) on grey and a red object's edge.
        for width in [1i32, 2, 3, 12] {
            let before = Rgb32FImage::from_fn(40, 40, |x, _| {
                if (18..18 + width).contains(&(x as i32)) {
                    image::Rgb([0.5, 0.04, 0.03])
                } else {
                    image::Rgb([0.7, 0.7, 0.7])
                }
            });
            for version in 1..=VERSION {
                let after = run(&before, version);
                for x in 18..(18 + width as u32).min(40) {
                    let (c0, c1) = (
                        chroma(before.get_pixel(x, 20).0),
                        chroma(after.get_pixel(x, 20).0),
                    );
                    assert!(
                        c1 > 0.95 * c0,
                        "v{version} width {width} x {x}: chroma {c0} -> {c1}"
                    );
                }
            }
        }
    }

    #[test]
    fn flat_colour_texture_and_recipes_without_the_key_are_untouched() {
        // Colour noise on a flat area: no edge, nothing changes.
        let noisy = Rgb32FImage::from_fn(32, 32, |x, y| {
            let n = ((x * 7 + y * 13) % 5) as f32 * 0.01;
            image::Rgb([0.4 + n, 0.4, 0.4 - n])
        });
        assert_eq!(run(&noisy, VERSION), noisy);
        let fringed = branch(2);
        assert_eq!(run(&fringed, 0), fringed);
    }

    /// Flat grey with Gaussian-like noise of standard deviation `sigma` in
    /// the square-root encoding (sum of uniforms from a fixed generator).
    fn noisy_grey(sigma: f32) -> Rgb32FImage {
        let mut state = 0x2545_f491_u32;
        let mut uniform = move || {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state as f32 / u32::MAX as f32 - 0.5
        };
        Rgb32FImage::from_fn(96, 96, |_, _| {
            let mut channel = || {
                let n = (0..12).map(|_| uniform()).sum::<f32>();
                (0.6 + sigma * n).max(0.0).powi(2)
            };
            image::Rgb([channel(), channel(), channel()])
        })
    }

    #[test]
    fn the_noise_estimate_follows_the_noise() {
        for sigma in [0.005f32, 0.02, 0.05] {
            let img = noisy_grey(sigma);
            let noise = luminance_noise(img.as_raw(), 96, 96, 3);
            // Luminance averages four channel samples: about half the noise.
            let expected = sigma * (6.0f32).sqrt() / 4.0;
            assert!(
                (noise / expected - 1.0).abs() < 0.25,
                "sigma {sigma}: noise {noise} vs {expected}"
            );
        }
    }

    #[test]
    fn versions_2_and_3_render_noisy_photos_as_version_1() {
        let noisy = noisy_grey(0.05);
        assert_eq!(run(&noisy, 2), run(&noisy, 1));
        assert_eq!(run(&noisy, 3), run(&noisy, 1));
        let clean = noisy_grey(0.004);
        assert!(luminance_noise(clean.as_raw(), 96, 96, 3) < 0.5 * EDGE_LOW);
        assert_eq!(radius_for(2, 0.004), 3.0);
        assert_eq!(radius_for(1, 0.004), 2.0);
    }

    fn linear_mix(a: [f32; 3], b: [f32; 3], t: f32) -> image::Rgb<f32> {
        image::Rgb(std::array::from_fn(|c| t * a[c] + (1.0 - t) * b[c]))
    }

    /// Linear-light cyan and magenta of about equal luminance.
    const CYAN: [f32; 3] = [0.0, 0.37, 0.55];
    const MAGENTA: [f32; 3] = [0.64, 0.10, 0.35];

    /// The share of `a`'s colour (0 at `b`) along the line from `b` to `a`
    /// and the distance from that line, in linear RGB.
    fn off_line(p: [f32; 3], a: [f32; 3], b: [f32; 3]) -> f32 {
        let d: [f32; 3] = std::array::from_fn(|c| a[c] - b[c]);
        let q: [f32; 3] = std::array::from_fn(|c| p[c] - b[c]);
        let dd = d.iter().map(|v| v * v).sum::<f32>();
        let t = q.iter().zip(&d).map(|(x, y)| x * y).sum::<f32>() / dd;
        q.iter()
            .zip(&d)
            .map(|(x, y)| (x - t * y).powi(2))
            .sum::<f32>()
            .sqrt()
    }

    #[test]
    fn version_3_removes_a_misregistered_seam_between_two_colours() {
        // Cyan meets magenta over a 2 px blur, but red rises a pixel before
        // green falls (the demosaic's red interpolated across the edge): a
        // light seam neither colour has, at about equal luminance, which the
        // luminance model of versions 1 and 2 cannot see.
        let edge = |x: f32| ((x - 19.5) / 2.0 + 0.5).clamp(0.0, 1.0);
        let before = Rgb32FImage::from_fn(40, 40, |x, _| {
            let (tg, tr) = (edge(x as f32), edge(x as f32 + 1.0));
            image::Rgb([
                tr * MAGENTA[0] + (1.0 - tr) * CYAN[0],
                tg * MAGENTA[1] + (1.0 - tg) * CYAN[1],
                tg * MAGENTA[2] + (1.0 - tg) * CYAN[2],
            ])
        });
        let seam = |img: &Rgb32FImage| {
            (12..28)
                .map(|x| off_line(img.get_pixel(x, 20).0, MAGENTA, CYAN))
                .fold(0.0f32, f32::max)
        };
        let s0 = seam(&before);
        assert!(s0 > 0.1, "seam {s0}");
        let (v2, v3) = (seam(&run(&before, 2)), seam(&run(&before, 3)));
        assert!(v2 > 0.95 * s0, "version 2 leaves the seam: {s0} -> {v2}");
        assert!(v3 < 0.65 * s0, "version 3: {s0} -> {v3}");
    }

    #[test]
    fn version_3_keeps_blurred_two_colour_edges_and_a_third_colour() {
        // A clean edge between two colours (blurred in linear light) is on
        // its colour line already: version 3 moves it no more than the
        // luminance model alone (which bends a saturated one a little).
        for (a, b) in [
            (CYAN, MAGENTA),
            ([0.95, 0.42, 0.0], [0.28, 0.22, 0.10]),
            ([0.8, 0.8, 0.85], [0.02, 0.02, 0.02]),
        ] {
            let before = Rgb32FImage::from_fn(40, 40, |x, _| {
                linear_mix(a, b, ((x as f32 - 19.5) / 3.0 + 0.5).clamp(0.0, 1.0))
            });
            let moved = |after: Rgb32FImage| {
                after
                    .pixels()
                    .zip(before.pixels())
                    .flat_map(|(p, q)| (0..3).map(move |c| (p[c] - q[c]).abs()))
                    .fold(0.0f32, f32::max)
            };
            let (v2, v3) = (moved(run(&before, 2)), moved(run(&before, 3)));
            assert!(
                v3 <= v2 + 1e-4,
                "{a:?}/{b:?}: moved {v2} by version 2, {v3} by 3"
            );
        }
        // A green stripe of three pixels between magenta and grey: three
        // colours in a window, the stripe keeps its colour.
        let before = Rgb32FImage::from_fn(40, 40, |x, _| match x {
            0..=17 => image::Rgb(MAGENTA),
            18..=20 => image::Rgb([0.05, 0.45, 0.08]),
            _ => image::Rgb([0.3, 0.3, 0.3]),
        });
        let after = run(&before, 3);
        for x in 18..=20 {
            let (c0, c1) = (
                chroma(before.get_pixel(x, 20).0),
                chroma(after.get_pixel(x, 20).0),
            );
            assert!(c1 > 0.9 * c0, "stripe x {x}: chroma {c0} -> {c1}");
        }
    }

    #[test]
    fn the_result_does_not_depend_on_the_band_layout() {
        // Taller than several bands, with edges across band borders.
        let img = Rgb32FImage::from_fn(37, 3 * BAND as u32 + 7, |x, y| {
            let v = ((x * 31 + y * 17) % 11) as f32 / 11.0;
            if (x + y) % 9 < 2 {
                image::Rgb([0.05 + 0.3 * v, 0.05, 0.1])
            } else {
                image::Rgb([0.8, 0.75 + 0.1 * v, 0.7])
            }
        });
        let banded = run(&img, VERSION);
        // Reference: the same per pixel computation with the whole image as
        // one band.
        let mut whole = img.clone();
        let (w, h) = (img.width() as usize, img.height() as usize);
        let plan = plan_for(VERSION, luminance_noise(img.as_raw(), w, h, 3), 1.0);
        assert!(plan.colour_lines);
        let changes = suppress_band(img.as_raw(), w, h, 3, plan.radius, 0, h);
        for (i, red, blue) in changes {
            whole.as_mut()[i * 3] = red;
            whole.as_mut()[i * 3 + 2] = blue;
        }
        let first = whole.clone();
        for (i, px) in colour_line_band(first.as_raw(), w, h, 3, plan.radius, 0, h) {
            whole.as_mut()[i * 3..i * 3 + 3].copy_from_slice(&px);
        }
        assert_eq!(banded, whole);
    }

    #[test]
    fn reduced_previews_skip_and_other_formats_pass_through() {
        let recipe = serde_json::json!({ VERSION_KEY: VERSION });
        let fringed = DynamicImage::ImageRgb32F(branch(2));
        let small = apply_for_recipe(Cow::Borrowed(&fringed), &recipe, 0.25);
        assert!(matches!(small, Cow::Borrowed(_)));
        let eight = DynamicImage::ImageRgb8(RgbImage::new(8, 8));
        assert!(matches!(
            apply_for_recipe(Cow::Borrowed(&eight), &recipe, 1.0),
            Cow::Borrowed(_)
        ));
    }

    #[test]
    fn the_default_is_only_for_never_edited_raws() {
        let mut never = serde_json::Value::Null;
        apply_default(&mut never, true, true);
        assert_eq!(version_of(&never), VERSION);
        let mut saved = serde_json::json!({"exposure": 0.3});
        apply_default(&mut saved, false, true);
        assert!(saved.get(VERSION_KEY).is_none());
        let mut jpeg = serde_json::json!({});
        apply_default(&mut jpeg, true, false);
        assert!(jpeg.get(VERSION_KEY).is_none());
        assert_eq!(version_of(&serde_json::json!({VERSION_KEY: 7})), VERSION);
    }
}
