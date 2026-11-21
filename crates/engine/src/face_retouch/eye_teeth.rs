//! Eyes and Teeth of the face retouch: Iris and Sclera (both eyes, or the
//! eye on the left or right of the photo) and Teeth Brightness and Whiten.
//!
//! The app stores per face the lid opening and the fitted iris circle of
//! each visible eye and the inner lip outline of an open mouth
//! (rawtonic-faces `eye_teeth`), in mask-frame pixels. The masks are drawn
//! here at the render's own resolution from those outlines and from the
//! pixels being retouched, so they follow the photo's edges at any size:
//!
//! - **Iris**: the circle within the lid opening, its rim soft over a tenth
//!   of the radius (the limbal ring is a soft edge itself). More texture
//!   (contrast against the iris's own mean), lighter and more colourful;
//!   never lighter than the white of the eye, the pupil stays dark.
//! - **Sclera**: the lid opening outside the iris, where the colour models
//!   take a pixel for eye white rather than lid skin (lashes, the lid's
//!   shadow and the pink inner corner mostly stay; lower-lid skin as light
//!   as a shadowed eye white can be taken along: notes
//!   benchmark-results/eye1-2026-11-21). Lighter with a roll-off
//!   towards white, and less colourful (red veins, yellowish whites).
//! - **Teeth**: inside the inner lip outline (grown by 2.5 % of the eye
//!   distance: Face Mesh's outline often cuts the teeth's edges), the
//!   pixels light for the mouth (in the upper part of its own dark-to-light
//!   range), of low colour and not red, weighed against the mouth's dark
//!   and the lips around it, so lips, gums and tongue mostly stay (light
//!   grey lips in a black-and-white photo can be taken along). Brightness
//!   lightens with a roll-off; Whiten takes out yellow only, towards
//!   neutral and never past it, so whitened teeth never turn blue.
//!
//! All of it in the face retouch's square-root working space; the lid
//! opening is pulled in by a little so no lid or lash edge is lightened.
//! Very small eyes (an iris under 4 pixels at full size) fade out: their
//! outlines are too coarse to keep the rim clean. An iris under 6 pixels in
//! the image being rendered (a reduced preview) is masked by the outlines
//! alone: too few pixels for colour models.

use image::Rgb32FImage;
use rayon::prelude::*;
use serde::Deserialize;
use serde_json::Value;

use super::{Amounts, decode, encode, smoothstep};
use crate::spot_removal::to_base;

/// Iris radius (render pixels) below which the eye masks come from the
/// outlines alone.
const TINY_IRIS: f32 = 6.0;

/// `faceRetouch.eyesTeethVersion`: the response of Eyes and Teeth a recipe
/// was made with. 1 (the only one) when missing.
pub const EYES_TEETH_VERSION: u64 = 1;

/// An eye as the app found it, mask-frame pixels.
#[derive(Deserialize, Clone, Debug, PartialEq)]
pub struct Eye {
    /// Iris centre and radius.
    pub iris: [f64; 3],
    /// Lid opening.
    pub lid: Vec<[f64; 2]>,
}

/// The tuning of one `eyesTeethVersion`.
#[derive(Clone, Copy, Debug)]
struct Tuning {
    /// Iris at 100: contrast gain, lightness gain (square-root values),
    /// colourfulness gain.
    iris_contrast: f32,
    iris_light: f32,
    iris_colour: f32,
    /// Sclera at 100: share of the way to white, share of colour removed.
    sclera_light: f32,
    sclera_colour: f32,
    /// Teeth at 100: share of the way to white (Brightness); share of the
    /// yellow removed and the lift that comes with it (Whiten).
    teeth_light: f32,
    teeth_yellow: f32,
    teeth_whiten_light: f32,
}

fn tuning(_version: u64) -> Tuning {
    Tuning {
        iris_contrast: 0.35,
        iris_light: 0.1,
        iris_colour: 0.1,
        sclera_light: 0.6,
        sclera_colour: 0.7,
        teeth_light: 0.6,
        teeth_yellow: 0.85,
        teeth_whiten_light: 0.15,
    }
}

/// The recipe's `eyesTeethVersion` (1 when missing, never newer than this
/// renderer knows).
pub fn version(retouch: &Value) -> u64 {
    retouch
        .get("eyesTeethVersion")
        .and_then(Value::as_u64)
        .unwrap_or(1)
        .clamp(1, EYES_TEETH_VERSION)
}

/// A closed outline in base pixels.
struct Outline {
    points: Vec<[f32; 2]>,
    min: [f32; 2],
    max: [f32; 2],
}

impl Outline {
    fn new(points: Vec<[f32; 2]>) -> Option<Self> {
        if points.len() < 3 {
            return None;
        }
        let mut min = [f32::INFINITY; 2];
        let mut max = [f32::NEG_INFINITY; 2];
        for p in &points {
            for k in 0..2 {
                min[k] = min[k].min(p[k]);
                max[k] = max[k].max(p[k]);
            }
        }
        Some(Self { points, min, max })
    }

    /// Unit vector between the outline's two farthest points (an eye's
    /// corners); (1, 0) for a degenerate outline.
    fn axis(&self) -> [f32; 2] {
        let mut best = (0.0f32, [1.0f32, 0.0]);
        for (i, a) in self.points.iter().enumerate() {
            for b in &self.points[i + 1..] {
                let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
                let d = dx.hypot(dy);
                if d > best.0 {
                    best = (d, [dx / d, dy / d]);
                }
            }
        }
        best.1
    }

    fn centroid(&self) -> [f32; 2] {
        let n = self.points.len() as f32;
        let (sx, sy) = self
            .points
            .iter()
            .fold((0.0, 0.0), |(sx, sy), p| (sx + p[0], sy + p[1]));
        [sx / n, sy / n]
    }

    /// Signed distance to the outline: negative inside.
    fn signed_distance(&self, x: f32, y: f32) -> f32 {
        let n = self.points.len();
        let mut best = f32::INFINITY;
        let mut inside = false;
        for i in 0..n {
            let p = self.points[i];
            let q = self.points[(i + 1) % n];
            let (ex, ey) = (q[0] - p[0], q[1] - p[1]);
            let (wx, wy) = (x - p[0], y - p[1]);
            let len2 = ex * ex + ey * ey;
            let t = if len2 > 0.0 {
                ((wx * ex + wy * ey) / len2).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let (dx, dy) = (wx - ex * t, wy - ey * t);
            best = best.min(dx * dx + dy * dy);
            if (p[1] > y) != (q[1] > y) && x < p[0] + (y - p[1]) * ex / ey {
                inside = !inside;
            }
        }
        if inside { -best.sqrt() } else { best.sqrt() }
    }

    /// Pixel box (x0, y0, x1, y1) around the outline grown by `pad`, within
    /// the image.
    fn pixel_box(&self, pad: f32, (w, h): (u32, u32)) -> Option<(u32, u32, u32, u32)> {
        let x0 = (self.min[0] - pad).floor().max(0.0) as u32;
        let y0 = (self.min[1] - pad).floor().max(0.0) as u32;
        let x1 = ((self.max[0] + pad).ceil().max(0.0) as u32).min(w);
        let y1 = ((self.max[1] + pad).ceil().max(0.0) as u32).min(h);
        (x1 > x0 && y1 > y0).then_some((x0, y0, x1, y1))
    }
}

fn lum(p: [f32; 3]) -> f32 {
    0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2]
}

/// `q`-quantile of `values` (sorted in place); `None` when empty.
fn quantile(values: &mut [f32], q: f32) -> Option<f32> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(f32::total_cmp);
    Some(values[((values.len() - 1) as f32 * q).round() as usize])
}

/// `rgb` with its colour (each channel over the luminance, against
/// neutral) scaled by `colour` and its luminance set to `y`.
fn recolour(rgb: [f32; 3], y_in: f32, y: f32, colour: impl Fn(usize, f32) -> f32) -> [f32; 3] {
    if y_in <= 1e-9 {
        return rgb;
    }
    let mut out = [0.0; 3];
    for c in 0..3 {
        let ratio = rgb[c] / y_in;
        out[c] = (1.0 + colour(c, ratio - 1.0)).max(0.0) * y;
    }
    out
}

/// What Eyes and Teeth need of one face, in base pixels.
pub(super) struct Placement<'a> {
    pub adjustments: &'a Value,
    pub frame: (f64, f64),
    /// Base pixels per mask-frame pixel.
    pub scale: f64,
    /// Full-size pixels per mask-frame pixel (above 1 in a reduced
    /// preview's recipe).
    pub full_size: f64,
    /// Eye distance, base pixels.
    pub eye_distance: f32,
}

impl Placement<'_> {
    fn point(&self, p: [f64; 2]) -> [f32; 2] {
        let (x, y) = to_base(self.adjustments, self.frame, p[0], p[1]);
        [x as f32, y as f32]
    }
}

/// Whether `amounts` change eyes or teeth.
pub(super) fn active(amounts: &Amounts) -> bool {
    amounts.iris.iter().chain(&amounts.sclera).any(|&a| a > 0.0)
        || amounts.teeth_brightness > 0.0
        || amounts.teeth_whiten > 0.0
}

/// Eyes and Teeth of one face.
pub(super) fn retouch(
    image: &mut Rgb32FImage,
    eyes: &[Option<Eye>],
    mouth: Option<&[[f64; 2]]>,
    place: &Placement,
    amounts: &Amounts,
    version: u64,
) {
    let tune = tuning(version);
    // Left, then right in the photo; `None`: that eye is not seen.
    for (side, eye) in eyes.iter().enumerate().take(2) {
        let Some(eye) = eye else { continue };
        let (iris, sclera) = (amounts.iris[side], amounts.sclera[side]);
        if iris > 0.0 || sclera > 0.0 {
            retouch_eye(image, eye, place, iris, sclera, &tune);
        }
    }
    if let Some(mouth) = mouth
        && (amounts.teeth_brightness > 0.0 || amounts.teeth_whiten > 0.0)
    {
        retouch_teeth(image, mouth, place, amounts, &tune);
    }
}

/// Colour features of a pixel: square-root lightness and the red and blue
/// share against neutral.
fn features(rgb: [f32; 3]) -> [f32; 3] {
    let y = lum(rgb);
    if y <= 1e-9 {
        return [0.0; 3];
    }
    [encode(y), rgb[0] / y - 1.0, rgb[2] / y - 1.0]
}

/// How far red stands above both green and blue (`features`' shares; the
/// green share follows from the luminance weights): lips, gums, tongue.
fn redness(f: [f32; 3]) -> f32 {
    let green = -(0.2126 * f[1] + 0.0722 * f[2]) / 0.7152;
    f[1] - green.max(f[2])
}

/// A class of pixels (eye white, iris, lid skin, …) as a normal
/// distribution of their features, sampled from the photo itself.
#[derive(Clone, Copy, Debug)]
struct Class {
    mean: [f32; 3],
    var: [f32; 3],
}

impl Class {
    /// Least spread per feature: lightness, red and blue share.
    const FLOOR: [f32; 3] = [0.03 * 0.03, 0.04 * 0.04, 0.04 * 0.04];

    /// `None` with fewer than `MIN_SAMPLES`.
    fn of(samples: &[[f32; 3]]) -> Option<Class> {
        const MIN_SAMPLES: usize = 6;
        if samples.len() < MIN_SAMPLES {
            return None;
        }
        let n = samples.len() as f32;
        let mut mean = [0.0f32; 3];
        for f in samples {
            for c in 0..3 {
                mean[c] += f[c] / n;
            }
        }
        let mut var = [0.0f32; 3];
        for f in samples {
            for c in 0..3 {
                var[c] += (f[c] - mean[c]).powi(2) / n;
            }
        }
        for (v, floor) in var.iter_mut().zip(Self::FLOOR) {
            *v = v.max(floor);
        }
        Some(Class { mean, var })
    }

    fn log_likelihood(&self, f: [f32; 3]) -> f32 {
        (0..3)
            .map(|c| -0.5 * ((f[c] - self.mean[c]).powi(2) / self.var[c] + self.var[c].ln()))
            .sum()
    }
}

/// How much more likely `f` belongs to `a` than to any of `others`, 0–1.
fn posterior(a: &Class, others: &[Class], f: [f32; 3]) -> f32 {
    let la = a.log_likelihood(f);
    let lb = others
        .iter()
        .map(|o| o.log_likelihood(f))
        .fold(f32::NEG_INFINITY, f32::max);
    1.0 / (1.0 + (lb - la).clamp(-30.0, 30.0).exp())
}

/// Box mean of `values` (`w` wide) over a (2r + 1)² window, clipped at the
/// edges.
fn box_mean(values: &[f32], w: usize, r: usize) -> Vec<f32> {
    if r == 0 || values.is_empty() {
        return values.to_vec();
    }
    let h = values.len() / w;
    let mut rows = vec![0.0f32; values.len()];
    for y in 0..h {
        let row = &values[y * w..(y + 1) * w];
        let mut prefix = vec![0.0f32; w + 1];
        for x in 0..w {
            prefix[x + 1] = prefix[x] + row[x];
        }
        for x in 0..w {
            let (lo, hi) = (x.saturating_sub(r), (x + r + 1).min(w));
            rows[y * w + x] = (prefix[hi] - prefix[lo]) / (hi - lo) as f32;
        }
    }
    let mut out = vec![0.0f32; values.len()];
    for x in 0..w {
        let mut prefix = vec![0.0f32; h + 1];
        for y in 0..h {
            prefix[y + 1] = prefix[y] + rows[y * w + x];
        }
        for y in 0..h {
            let (lo, hi) = (y.saturating_sub(r), (y + r + 1).min(h));
            out[y * w + x] = (prefix[hi] - prefix[lo]) / (hi - lo) as f32;
        }
    }
    out
}

/// Lighter by `strength` (square-root values): proportional in the darks,
/// rolling off towards white, never past it.
fn lighten(e: f32, strength: f32) -> f32 {
    let e = e.clamp(0.0, 1.0);
    e * (1.0 + strength) / (1.0 + strength * e)
}

/// The pixels of a box of the image and its geometry.
struct Patch {
    x0: u32,
    y0: u32,
    w: usize,
    h: usize,
    rgb: Vec<[f32; 3]>,
}

impl Patch {
    fn new(image: &Rgb32FImage, (x0, y0, x1, y1): (u32, u32, u32, u32)) -> Self {
        let (w, h) = ((x1 - x0) as usize, (y1 - y0) as usize);
        let rgb = (0..w * h)
            .map(|k| image.get_pixel(x0 + (k % w) as u32, y0 + (k / w) as u32).0)
            .collect();
        Self { x0, y0, w, h, rgb }
    }

    /// Centre of pixel `k`, base pixels.
    fn centre(&self, k: usize) -> (f32, f32) {
        (
            (self.x0 as usize + k % self.w) as f32 + 0.5,
            (self.y0 as usize + k / self.w) as f32 + 0.5,
        )
    }

    /// Writes `rgb` blended over the original by `blend`.
    fn write(&self, image: &mut Rgb32FImage, out: &[Option<[f32; 3]>]) {
        for (k, v) in out.iter().enumerate() {
            if let Some(rgb) = v {
                image.put_pixel(
                    self.x0 + (k % self.w) as u32,
                    self.y0 + (k / self.w) as u32,
                    image::Rgb(*rgb),
                );
            }
        }
    }
}

fn blend(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
    ]
}

/// The masks of one eye: how much each pixel of `patch` is iris and how
/// much eye white. Face Mesh's lid outline often reaches over the lid's
/// rim and into the corners' skin, and the iris circle runs under the
/// lids; so within them each pixel is weighed as eye white, iris or lid
/// skin by colour models sampled from this eye: the white beside the iris
/// at its height, the iris ring, and the skin just outside the outline.
fn eye_masks(patch: &Patch, lid: &Outline, (cx, cy, r): (f32, f32, f32)) -> (Vec<f32>, Vec<f32>) {
    let [ux, uy] = lid.axis();
    let n = patch.w * patch.h;
    let feature: Vec<[f32; 3]> = patch.rgb.par_iter().map(|&p| features(p)).collect();
    let sd: Vec<f32> = (0..n)
        .into_par_iter()
        .map(|k| {
            let (x, y) = patch.centre(k);
            lid.signed_distance(x, y)
        })
        .collect();
    let dc: Vec<f32> = (0..n)
        .into_par_iter()
        .map(|k| {
            let (x, y) = patch.centre(k);
            (x - cx).hypot(y - cy)
        })
        .collect();
    let pick =
        |test: &dyn Fn(usize) -> bool| -> Vec<usize> { (0..n).filter(|&k| test(k)).collect() };
    let of = |seeds: &[usize]| Class::of(&seeds.iter().map(|&k| feature[k]).collect::<Vec<_>>());
    // Beside the iris along the eye (corner to corner, so a tilted head or
    // a turned photo is the same), well inside the outline: mostly eye
    // white; its lighter half (no lashes or corner shadow).
    let mut white_seeds = pick(&|k| {
        let (x, y) = patch.centre(k);
        let across = ((x - cx) * uy - (y - cy) * ux).abs();
        sd[k] < -0.08 * r && dc[k] > 1.15 * r && dc[k] < 1.9 * r && across < 0.35 * r
    });
    white_seeds.sort_by(|&a, &b| feature[a][0].total_cmp(&feature[b][0]));
    let white_seeds = white_seeds.split_off(white_seeds.len() / 2);
    let white = of(&white_seeds);
    let iris_seeds = pick(&|k| dc[k] > 0.4 * r && dc[k] < 0.85 * r && sd[k] < -0.12 * r);
    let iris = of(&iris_seeds);
    let skin = of(&pick(&|k| {
        sd[k] > 0.12 * r && sd[k] < 0.5 * r && dc[k] > 1.05 * r
    }));

    let feather = (0.08 * r).max(1.0);
    let iris_feather = (0.1 * r).max(1.0);
    let smooth = ((0.04 * r).round() as usize).max(1);
    let region: Vec<f32> = sd
        .iter()
        .map(|&d| (0.5 - d / feather).clamp(0.0, 1.0))
        .collect();
    let disk: Vec<f32> = dc
        .iter()
        .map(|&d| (0.5 - (d - r) / iris_feather).clamp(0.0, 1.0))
        .collect();
    // An iris a few pixels across (a reduced preview of a group photo) has
    // too few pixels for colour models: the outlines alone, the white by
    // its lightness against the opening's lighter pixels, so the preview
    // shows what the full-size render does.
    if r < TINY_IRIS {
        let mut light: Vec<f32> = (0..n)
            .filter(|&k| region[k] >= 0.99 && disk[k] <= 0.0)
            .map(|k| feature[k][0])
            .collect();
        let white = quantile(&mut light, 0.75).filter(|&w| w > 0.0);
        let iris_w = (0..n).map(|k| region[k] * disk[k]).collect();
        let white_w = (0..n)
            .map(|k| {
                white.map_or(0.0, |w| {
                    region[k] * (1.0 - disk[k]) * smoothstep(0.55, 0.85, feature[k][0] / w)
                })
            })
            .collect();
        return (iris_w, white_w);
    }
    let likely = |class: Option<Class>| -> Vec<f32> {
        match (class, skin) {
            (Some(a), Some(s)) => box_mean(
                &feature
                    .par_iter()
                    .map(|&f| posterior(&a, &[s], f))
                    .collect::<Vec<_>>(),
                patch.w,
                smooth,
            ),
            // Without a sample of the skin the outline alone decides;
            // without a sample of the part, nothing is that part.
            (Some(_), None) => vec![1.0; n],
            (None, _) => vec![0.0; n],
        }
    };
    // Well inside the outline the iris is iris: its colour model decides
    // only towards the outline, where lid skin may lie (a colour test
    // across the iris left patches of it untouched). The eye white keeps
    // the test everywhere: Face Mesh's outline often runs over the lower
    // lid, whose skin can be as light as a shadowed eye white.
    let trust: Vec<f32> = sd
        .iter()
        .map(|&d| smoothstep(-0.3 * r, -0.05 * r, d))
        .collect();
    let towards_outline = |p: Vec<f32>| -> Vec<f32> {
        p.iter()
            .zip(&trust)
            .map(|(&p, &t)| 1.0 - t * (1.0 - p))
            .collect()
    };
    let p_iris = if iris.is_some() {
        towards_outline(likely(iris))
    } else {
        vec![0.0; n]
    };
    let p_white = likely(white);
    // Only what joins its own samples counts: lid skin as light as a
    // shadowed eye white lies beyond the lid's darker rim.
    let joined = |seeds: &[usize], candidate: &dyn Fn(usize) -> bool| -> Vec<f32> {
        let reached = flood(patch.w, patch.h, seeds, candidate);
        box_mean(&reached, patch.w, smooth)
    };
    let iris_joined = joined(&iris_seeds, &|k| {
        region[k] > 0.3 && disk[k] > 0.3 && p_iris[k] > 0.5
    });
    let white_joined = joined(&white_seeds, &|k| {
        region[k] > 0.3 && disk[k] < 0.7 && p_white[k] > 0.5
    });
    let iris_w = (0..n)
        .map(|k| region[k] * disk[k] * p_iris[k] * iris_joined[k])
        .collect();
    let white_w = (0..n)
        .map(|k| region[k] * (1.0 - disk[k]) * p_white[k] * white_joined[k])
        .collect();
    (iris_w, white_w)
}

/// 1 where a 4-connected path of `candidate` pixels leads from a seed
/// (itself a candidate), else 0.
fn flood(w: usize, h: usize, seeds: &[usize], candidate: &dyn Fn(usize) -> bool) -> Vec<f32> {
    let mut reached = vec![0.0f32; w * h];
    let mut stack: Vec<usize> = seeds.iter().copied().filter(|&k| candidate(k)).collect();
    for &k in &stack {
        reached[k] = 1.0;
    }
    while let Some(k) = stack.pop() {
        let (x, y) = (k % w, k / w);
        let mut visit = |j: usize| {
            if reached[j] == 0.0 && candidate(j) {
                reached[j] = 1.0;
                stack.push(j);
            }
        };
        if x > 0 {
            visit(k - 1);
        }
        if x + 1 < w {
            visit(k + 1);
        }
        if y > 0 {
            visit(k - w);
        }
        if y + 1 < h {
            visit(k + w);
        }
    }
    reached
}

fn retouch_eye(
    image: &mut Rgb32FImage,
    eye: &Eye,
    place: &Placement,
    iris_amount: f32,
    sclera_amount: f32,
    tune: &Tuning,
) {
    let [ix, iy, ir] = eye.iris;
    let [cx, cy] = place.point([ix, iy]);
    let r = (ir * place.scale) as f32;
    // By the full-size iris, so a reduced preview fades exactly as the
    // export does.
    let fade = smoothstep(1.5, 4.0, (ir * place.full_size) as f32);
    if fade <= 0.0 {
        return;
    }
    let Some(lid) = Outline::new(eye.lid.iter().map(|&p| place.point(p)).collect()) else {
        return;
    };
    let Some(bounds) = lid.pixel_box(0.6 * r + 2.0, image.dimensions()) else {
        return;
    };
    let patch = Patch::new(image, bounds);
    let (iris_w, white_w) = eye_masks(&patch, &lid, (cx, cy, r));
    let n = patch.w * patch.h;
    // The iris's mean lightness and the eye white's, for the iris's
    // contrast and its ceiling.
    let mean_of = |w: &[f32]| {
        let (mut sum, mut total) = (0.0f32, 0.0f32);
        for (&weight, &rgb) in w.iter().zip(&patch.rgb) {
            if weight > 0.5 {
                sum += encode(lum(rgb));
                total += 1.0;
            }
        }
        (total > 0.0).then(|| sum / total)
    };
    let iris_mean = mean_of(&iris_w);
    let mut white_values: Vec<f32> = (0..n)
        .filter(|&k| white_w[k] > 0.5)
        .map(|k| encode(lum(patch.rgb[k])))
        .collect();
    let white = quantile(&mut white_values, 0.75);

    let out: Vec<Option<[f32; 3]>> = (0..n)
        .into_par_iter()
        .map(|k| {
            let (wi, ww) = (fade * iris_w[k], fade * white_w[k]);
            if wi <= 0.0 && ww <= 0.0 {
                return None;
            }
            let rgb = patch.rgb[k];
            let y_in = lum(rgb);
            let e = encode(y_in);
            let mut out = rgb;
            if iris_amount > 0.0
                && wi > 0.0
                && let Some(mean) = iris_mean
            {
                let a = iris_amount;
                let contrast = (mean + (e - mean) * (1.0 + tune.iris_contrast * a)).max(0.0);
                let lighter = contrast * (1.0 + tune.iris_light * a);
                // Never lighter than the eye white (catchlights keep theirs).
                let ceiling = white.map_or(1.0, |wv| (0.95 * wv).max(e));
                let gain = 1.0 + tune.iris_colour * a;
                let new = recolour(rgb, y_in, decode(lighter.min(ceiling)), |_, d| d * gain);
                out = blend(out, new, wi);
            }
            if sclera_amount > 0.0 && ww > 0.0 {
                let a = sclera_amount;
                let keep = 1.0 - tune.sclera_colour * a;
                let new = recolour(
                    rgb,
                    y_in,
                    decode(lighten(e, tune.sclera_light * a)),
                    |_, d| d * keep,
                );
                out = blend(out, new, ww);
            }
            Some(out)
        })
        .collect();
    patch.write(image, &out);
}

/// How much each pixel of `patch` is teeth: inside the inner lip outline
/// grown by `grow` (Face Mesh's outline often cuts the teeth's edges),
/// weighed by colour models sampled from this mouth: the teeth (the light,
/// low-colour part inside the outline), the mouth's dark and the lips and
/// skin just outside the grown outline.
fn teeth_mask(patch: &Patch, outline: &Outline, grow: f32, feather: f32, reach: f32) -> Vec<f32> {
    let n = patch.w * patch.h;
    let feature: Vec<[f32; 3]> = patch.rgb.par_iter().map(|&p| features(p)).collect();
    let sd: Vec<f32> = (0..n)
        .into_par_iter()
        .map(|k| {
            let (x, y) = patch.centre(k);
            outline.signed_distance(x, y)
        })
        .collect();
    // The mouth's own dark-to-light range (10th and 90th percentile).
    let mut inside: Vec<f32> = (0..n)
        .filter(|&k| sd[k] < 0.0)
        .map(|k| feature[k][0])
        .collect();
    let (Some(lo), Some(hi)) = (quantile(&mut inside, 0.1), {
        let len = inside.len();
        (len > 0).then(|| inside[((len - 1) as f32 * 0.9).round() as usize])
    }) else {
        return vec![0.0; n];
    };
    if hi - lo < 0.06 {
        return vec![0.0; n];
    }
    let saturation = |k: usize| {
        let enc = patch.rgb[k].map(encode);
        let max = enc.iter().copied().fold(0.0, f32::max);
        let min = enc.iter().copied().fold(f32::INFINITY, f32::min);
        if max > 0.0 { (max - min) / max } else { 0.0 }
    };
    let teeth = Class::of(
        &(0..n)
            .filter(|&k| {
                sd[k] < 0.0
                    && feature[k][0] >= lo + 0.75 * (hi - lo)
                    && saturation(k) < 0.35
                    && redness(feature[k]) < 0.2
            })
            .map(|k| feature[k])
            .collect::<Vec<_>>(),
    );
    let Some(teeth) = teeth else {
        return vec![0.0; n];
    };
    // The mouth's dark, and the lips and skin around the grown outline in
    // eight sectors, each its own model: a lip lit by a flash on one side
    // and in shadow on the other are two colours.
    let [mx, my] = outline.centroid();
    let sector = |k: usize| {
        let (x, y) = patch.centre(k);
        let a = (y - my).atan2(x - mx) + std::f32::consts::PI;
        ((a / std::f32::consts::TAU * 8.0) as usize).min(7)
    };
    let mut ring: [Vec<[f32; 3]>; 8] = Default::default();
    for k in 0..n {
        if sd[k] > grow + feather && sd[k] < grow + feather + reach {
            ring[sector(k)].push(feature[k]);
        }
    }
    let dark = Class::of(
        &(0..n)
            .filter(|&k| sd[k] < 0.0 && feature[k][0] <= lo + 0.3 * (hi - lo))
            .map(|k| feature[k])
            .collect::<Vec<_>>(),
    );
    let others: Vec<Class> = std::iter::once(dark)
        .chain(ring.iter().map(|samples| Class::of(samples)))
        .flatten()
        .collect();
    let likely: Vec<f32> = feature
        .par_iter()
        .map(|&f| {
            if others.is_empty() {
                1.0
            } else {
                posterior(&teeth, &others, f)
            }
        })
        .collect();
    let smooth = ((0.25 * feather).round() as usize).max(1);
    let likely = box_mean(&likely, patch.w, smooth);
    (0..n)
        .into_par_iter()
        .map(|k| {
            let cover = (0.5 - (sd[k] - grow) / feather).clamp(0.0, 1.0);
            // Red above both others is lips, gums or tongue, however light.
            cover * likely[k] * (1.0 - smoothstep(0.15, 0.3, redness(feature[k])))
        })
        .collect()
}

fn retouch_teeth(
    image: &mut Rgb32FImage,
    mouth: &[[f64; 2]],
    place: &Placement,
    amounts: &Amounts,
    tune: &Tuning,
) {
    let Some(outline) = Outline::new(mouth.iter().map(|&p| place.point(p)).collect()) else {
        return;
    };
    let feather = (0.01 * place.eye_distance).max(1.0);
    let grow = 0.025 * place.eye_distance;
    let reach = 0.06 * place.eye_distance;
    let Some(bounds) = outline.pixel_box(grow + feather + reach, image.dimensions()) else {
        return;
    };
    let patch = Patch::new(image, bounds);
    let mask = teeth_mask(&patch, &outline, grow, feather, reach);
    let (b, wh) = (amounts.teeth_brightness, amounts.teeth_whiten);
    let out: Vec<Option<[f32; 3]>> = (0..patch.w * patch.h)
        .into_par_iter()
        .map(|k| {
            if mask[k] <= 0.0 {
                return None;
            }
            let rgb = patch.rgb[k];
            let y_in = lum(rgb);
            let e = encode(y_in);
            let light = tune.teeth_light * b + tune.teeth_whiten_light * wh;
            // Yellow: red and green above blue (colour against neutral).
            let ratio = |c: usize| {
                if y_in > 1e-9 {
                    rgb[c] / y_in - 1.0
                } else {
                    0.0
                }
            };
            let yellow = ratio(0).min(ratio(1)) - ratio(2);
            let take = tune.teeth_yellow * wh * smoothstep(0.0, 0.08, yellow);
            let new = recolour(rgb, y_in, decode(lighten(e, light)), |_, d| {
                d * (1.0 - take)
            });
            Some(blend(rgb, new, mask[k]))
        })
        .collect();
    patch.write(image, &out);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::face_retouch::apply_face_retouch;
    use serde_json::json;

    const SKIN: [f32; 3] = [0.30, 0.19, 0.13];
    const WHITE: [f32; 3] = [0.42, 0.40, 0.36];
    const IRIS: [f32; 3] = [0.05, 0.08, 0.14];
    const LIPS: [f32; 3] = [0.30, 0.09, 0.08];
    const TEETH: [f32; 3] = [0.50, 0.44, 0.26];
    const DARK: [f32; 3] = [0.01, 0.005, 0.005];

    fn ellipse(cx: f32, cy: f32, rx: f32, ry: f32, n: usize) -> Vec<[f64; 2]> {
        (0..n)
            .map(|k| {
                let a = k as f32 / n as f32 * std::f32::consts::TAU;
                [(cx + rx * a.cos()) as f64, (cy + ry * a.sin()) as f64]
            })
            .collect()
    }

    /// A face of 400 × 300: two eyes (opening 60 × 24, iris radius 14) at
    /// y 100, the right one's outline drawn 8 px too low and too wide, as
    /// Face Mesh's often is; an open mouth with yellowish teeth between
    /// red lips at y 220.
    fn face() -> (Rgb32FImage, Value) {
        let image = Rgb32FImage::from_fn(400, 300, |x, y| {
            let (x, y) = (x as f32 + 0.5, y as f32 + 0.5);
            let eye = |cx: f32| ((x - cx) / 30.0).powi(2) + ((y - 100.0) / 12.0).powi(2) <= 1.0;
            let iris = |cx: f32| (x - cx).hypot(y - 100.0) <= 14.0;
            let mouth = ((x - 200.0) / 50.0).powi(2) + ((y - 220.0) / 14.0).powi(2) <= 1.0;
            let lips = ((x - 200.0) / 60.0).powi(2) + ((y - 220.0) / 24.0).powi(2) <= 1.0;
            let rgb = if eye(120.0) || eye(280.0) {
                if iris(120.0) || iris(280.0) {
                    IRIS
                } else {
                    WHITE
                }
            } else if mouth {
                if y < 226.0 { TEETH } else { DARK }
            } else if lips {
                LIPS
            } else {
                SKIN
            };
            image::Rgb(rgb)
        });
        let eyes = json!([
            {"iris": [120.0, 100.0, 14.0], "lid": ellipse(120.0, 100.0, 30.0, 12.0, 16)},
            {"iris": [280.0, 100.0, 14.0], "lid": ellipse(280.0, 106.0, 38.0, 16.0, 16)},
        ]);
        let recipe = json!({"faceRetouch": {"settings": {}, "faces": [{
            "crop": [4.0, 0.0, 0.0, 0.0, 4.0, 0.0], "size": 100, "eyeDistance": 160.0,
            "skin": "", "blemishes": [], "kept": [],
            "eyes": eyes,
            "mouth": ellipse(200.0, 220.0, 46.0, 10.0, 20),
        }]}});
        (image, recipe)
    }

    fn with(mut recipe: Value, settings: Value) -> Value {
        recipe["faceRetouch"]["settings"] = settings;
        recipe
    }

    fn changed(a: &Rgb32FImage, b: &Rgb32FImage, x: u32, y: u32) -> bool {
        let (p, q) = (a.get_pixel(x, y).0, b.get_pixel(x, y).0);
        (0..3).any(|c| (p[c] - q[c]).abs() > 1e-4)
    }

    #[test]
    fn zero_values_change_nothing() {
        let (before, recipe) = face();
        let mut image = before.clone();
        let recipe = with(recipe, json!({"iris": 0, "sclera": 0, "teethWhiten": 0}));
        assert!(!apply_face_retouch(&mut image, &recipe));
        assert_eq!(image.as_raw(), before.as_raw());
    }

    #[test]
    fn iris_and_sclera_stay_in_the_eye() {
        let (before, recipe) = face();
        let mut image = before.clone();
        assert!(apply_face_retouch(
            &mut image,
            &with(recipe, json!({"iris": 100, "sclera": 100}))
        ));
        for cx in [120, 280] {
            // Iris lighter and more colourful, white lighter and greyer.
            let (i0, i1) = (
                before.get_pixel(cx + 7, 100).0,
                image.get_pixel(cx + 7, 100).0,
            );
            assert!(lum(i1) > lum(i0) * 1.1, "iris {i0:?} → {i1:?}");
            assert!(i1[2] / i1[0] > i0[2] / i0[0], "iris colour {i0:?} → {i1:?}");
            let (w0, w1) = (
                before.get_pixel(cx + 22, 100).0,
                image.get_pixel(cx + 22, 100).0,
            );
            assert!(lum(w1) > lum(w0) * 1.1, "white {w0:?} → {w1:?}");
            assert!((w1[0] - w1[2]) / lum(w1) < (w0[0] - w0[2]) / lum(w0));
        }
        // The skin below the right eye lies inside its too large outline,
        // yet stays: it is neither eye white nor iris.
        for (x, y) in [(280, 116), (300, 114), (250, 115), (316, 104)] {
            assert!(!changed(&before, &image, x, y), "skin at ({x}, {y})");
        }
    }

    /// `face()` turned a quarter: the eyes one above the other.
    fn turned_face() -> (Rgb32FImage, Value) {
        let (image, mut recipe) = face();
        let turned = Rgb32FImage::from_fn(300, 400, |x, y| *image.get_pixel(y, x));
        let swap = |v: &mut Value| {
            if let Some(p) = v.as_array_mut()
                && p.len() >= 2
            {
                p.swap(0, 1);
            }
        };
        let face = &mut recipe["faceRetouch"]["faces"][0];
        for eye in face["eyes"].as_array_mut().unwrap() {
            swap(&mut eye["iris"]);
            for p in eye["lid"].as_array_mut().unwrap() {
                swap(p);
            }
        }
        for p in face["mouth"].as_array_mut().unwrap() {
            swap(p);
        }
        (turned, recipe)
    }

    #[test]
    fn a_turned_face_gets_its_eye_whites() {
        let (before, recipe) = turned_face();
        let mut image = before.clone();
        apply_face_retouch(&mut image, &with(recipe, json!({"sclera": 100})));
        for cy in [120, 280] {
            let (w0, w1) = (
                before.get_pixel(100, cy + 22).0,
                image.get_pixel(100, cy + 22).0,
            );
            assert!(
                lum(w1) > lum(w0) * 1.1,
                "white beside the iris at y {cy}: {w0:?} → {w1:?}"
            );
        }
        assert!(
            !changed(&before, &image, 116, 280),
            "skin beside the lower eye"
        );
    }

    #[test]
    fn one_eye_alone() {
        let (before, recipe) = face();
        let mut image = before.clone();
        apply_face_retouch(
            &mut image,
            &with(recipe, json!({"iris": 0, "irisRight": 80})),
        );
        assert!(!changed(&before, &image, 127, 100), "left eye");
        assert!(changed(&before, &image, 287, 100), "right eye");
    }

    #[test]
    fn teeth_whiten_without_turning_blue_and_lips_stay() {
        let (before, recipe) = face();
        let mut image = before.clone();
        apply_face_retouch(
            &mut image,
            &with(recipe, json!({"teethBrightness": 100, "teethWhiten": 100})),
        );
        let (t0, t1) = (before.get_pixel(200, 216).0, image.get_pixel(200, 216).0);
        assert!(lum(t1) > lum(t0), "lighter: {t0:?} → {t1:?}");
        let yellow = |p: [f32; 3]| (p[0].min(p[1]) - p[2]) / lum(p);
        assert!(
            yellow(t1) < 0.5 * yellow(t0),
            "less yellow: {t0:?} → {t1:?}"
        );
        assert!(yellow(t1) >= 0.0, "never blue: {t1:?}");
        for (x, y) in [(200, 200), (200, 240), (145, 220), (200, 230)] {
            assert!(
                !changed(&before, &image, x, y),
                "lips or mouth at ({x}, {y})"
            );
        }
    }

    #[test]
    fn faces_without_eyes_or_mouth_are_left_alone() {
        let (before, mut recipe) = face();
        let face = &mut recipe["faceRetouch"]["faces"][0];
        face.as_object_mut().unwrap().remove("eyes");
        face.as_object_mut().unwrap().remove("mouth");
        let mut image = before.clone();
        apply_face_retouch(
            &mut image,
            &with(
                recipe,
                json!({"iris": 100, "sclera": 100, "teethWhiten": 100}),
            ),
        );
        assert_eq!(image.as_raw(), before.as_raw());
    }

    #[test]
    fn reduced_previews_show_the_eyes_too() {
        // The face at 0.15 of its size: an iris of 2.1 px, as in a fit view
        // of a large group photo.
        let (full, recipe) = face();
        let scale = 0.15;
        let small = Rgb32FImage::from_fn(60, 45, |x, y| {
            *full.get_pixel(
                ((x as f64 + 0.5) / scale) as u32,
                ((y as f64 + 0.5) / scale) as u32,
            )
        });
        let recipe = crate::face_retouch::scaled_recipe(
            &with(recipe, json!({"iris": 100, "sclera": 100})),
            scale,
        );
        let mut image = small.clone();
        assert!(apply_face_retouch(&mut image, &recipe));
        // The full-size render, for the strength the preview must show.
        let (_, full_recipe) = face();
        let mut full_after = full.clone();
        apply_face_retouch(
            &mut full_after,
            &with(full_recipe, json!({"iris": 100, "sclera": 100})),
        );
        let gain = |a: &Rgb32FImage, b: &Rgb32FImage, x: u32, y: u32| {
            lum(b.get_pixel(x, y).0) / lum(a.get_pixel(x, y).0) - 1.0
        };
        // The iris (120, 100) → (18, 15); the white beside it.
        let (preview, export) = (
            gain(&small, &image, 18, 15),
            gain(&full, &full_after, 120, 100),
        );
        assert!(
            preview > 0.5 * export,
            "iris: preview {preview}, export {export}"
        );
        let (preview, export) = (
            gain(&small, &image, 21, 15),
            gain(&full, &full_after, 142, 100),
        );
        assert!(
            preview > 0.5 * export,
            "white: preview {preview}, export {export}"
        );
        assert!(!changed(&small, &image, 18, 25), "cheek");
    }

    #[test]
    fn version_defaults_to_one_and_clamps() {
        assert_eq!(version(&json!({})), 1);
        assert_eq!(
            version(&json!({"eyesTeethVersion": 99})),
            EYES_TEETH_VERSION
        );
    }

    /// Time of Eyes and Teeth at 100 on real photos at full size: for each
    /// `NAME.jpg` in `EYE_TIMING_DIR` a `NAME.json` with the faces' `eyes`,
    /// `mouth` and `eyeDistance` in photo pixels (rawtonic-faces test
    /// `eye_teeth_photos` writes them).
    ///
    /// EYE_TIMING_DIR=… cargo test -p rawtonic-engine --release --lib eye_teeth::tests::times -- --ignored --nocapture
    #[test]
    #[ignore]
    fn times_eyes_and_teeth_on_photos() {
        let Some(dir) = std::env::var_os("EYE_TIMING_DIR").map(std::path::PathBuf::from) else {
            eprintln!("SKIP: EYE_TIMING_DIR is needed");
            return;
        };
        let mut names: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|e| e == "jpg"))
            .collect();
        names.sort();
        for path in names {
            let found: Value = serde_json::from_str(
                &std::fs::read_to_string(path.with_extension("json")).unwrap(),
            )
            .unwrap();
            let image = image::open(&path).unwrap().to_rgb32f();
            let faces: Vec<Value> = found["faces"]
                .as_array()
                .unwrap()
                .iter()
                .map(|f| {
                    json!({"crop": [1.0, 0.0, 0.0, 0.0, 1.0, 0.0], "size": 100,
                        "eyeDistance": f["eyeDistance"], "skin": "", "blemishes": [],
                        "eyes": f["eyes"], "mouth": f["mouth"]})
                })
                .collect();
            let recipe = json!({"faceRetouch": {"settings": {"iris": 100, "sclera": 100,
                "teethBrightness": 100, "teethWhiten": 100}, "faces": faces}});
            let mut best = f64::INFINITY;
            for _ in 0..5 {
                let mut work = image.clone();
                let start = std::time::Instant::now();
                apply_face_retouch(&mut work, &recipe);
                best = best.min(start.elapsed().as_secs_f64() * 1000.0);
            }
            println!(
                "{}: {} x {}, {} face(s): {best:.1} ms (best of 5)",
                path.file_stem().unwrap().to_string_lossy(),
                image.width(),
                image.height(),
                faces.len()
            );
        }
    }

    #[test]
    fn lighten_rolls_off_and_keeps_black() {
        assert_eq!(lighten(0.0, 0.6), 0.0);
        assert!((lighten(1.0, 0.6) - 1.0).abs() < 1e-6);
        assert!(lighten(0.5, 0.6) > 0.5 && lighten(0.5, 0.6) < 1.0);
    }
}
