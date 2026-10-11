// SPDX-License-Identifier: AGPL-3.0-or-later
//! What the face retouch needs of each face, made once from the analysis:
//! a square crop around the face (level eyes), the skin weight and the
//! under-eye weight in it, the eye distance, and the blemishes with their
//! prominence. The render (rawtonic-engine `face_retouch`) uses only these,
//! so it needs no model.
//!
//! Blemishes are dark or red spots that stand out from the skin around
//! them: at eight scales (0.5–3 % of the eye distance) the relative
//! darkening and the redness against a ring three times wider, kept where
//! the luminance is a blob (Hessian: both curvatures alike, not an edge)
//! and where the response is well above the response's own level around
//! it (pores, stubble and freckle fields raise that level). Prominence
//! grows with contrast and size, so the slider takes large, clear
//! blemishes first.

use image::{GrayImage, Luma, RgbImage};
use rayon::prelude::*;

use crate::geometry::{Affine, Polygon, guided_filter, sample_crop};
use crate::landmarks::{FACE_OVAL, LEFT_BROW, LEFT_EYE, LIPS_OUTER, RIGHT_BROW, RIGHT_EYE};
use crate::parse::Class;
use crate::{Face, FaceAnalysis};

/// Side of the stored weights.
pub const WEIGHT_SIZE: u32 = 768;
/// Crop side relative to the face box's longer side.
const CROP_SCALE: f32 = 2.2;
/// Eye distance (outer corners) at which blemishes are looked for; larger
/// faces are scaled down to it.
const DETECT_EYE_DISTANCE: f32 = 560.0;
/// Outer-corner eye distance over the YuNet eye-centre distance (for faces
/// without landmarks).
const OUTER_OVER_CENTRES: f32 = 1.43;
/// At most this many blemish candidates per face.
const MAX_BLEMISHES: usize = 1500;

/// Nostrils and the nose wings (MediaPipe indices), kept out of the skin
/// for blemishes.
const NOSE_WINGS: [usize; 16] = [
    98, 97, 2, 326, 327, 294, 278, 344, 440, 275, 4, 45, 220, 115, 48, 64,
];
/// Lower eyelids, from the outer to the inner corner.
const RIGHT_LOWER_LID: [usize; 9] = [33, 7, 163, 144, 145, 153, 154, 155, 133];
const LEFT_LOWER_LID: [usize; 9] = [263, 249, 390, 373, 374, 380, 381, 382, 362];

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Blemish {
    pub id: u32,
    /// Centre and radius in photo pixels.
    pub x: f32,
    pub y: f32,
    pub r: f32,
    pub score: f32,
    /// On the neck rather than the face (healed with "include neck").
    pub neck: bool,
}

#[derive(Clone, Debug)]
pub struct RetouchFace {
    /// Crop pixels (0..`WEIGHT_SIZE`) to photo pixels.
    pub crop: Affine,
    pub eye_distance: f32,
    pub skin: GrayImage,
    pub under_eyes: Option<GrayImage>,
    /// Neck skin below the chin in the crop, for "include neck".
    pub neck: Option<GrayImage>,
    pub blemishes: Vec<Blemish>,
}

fn outline(points: &[[f32; 2]], indices: &[usize]) -> Polygon {
    Polygon::new(indices.iter().map(|&i| points[i]).collect())
}

/// Eye distance (outer corners) and roll of a face.
fn eye_line(face: &Face) -> (f32, f32) {
    let (a, b) = match &face.landmarks {
        Some(p) => (p[33], p[263]),
        None => (face.detection.keypoints[0], face.detection.keypoints[1]),
    };
    let d = ((b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2)).sqrt();
    let d = if face.landmarks.is_some() {
        d
    } else {
        d * OUTER_OVER_CENTRES
    };
    (d, (b[1] - a[1]).atan2(b[0] - a[0]))
}

/// The crop of `face` with `out` pixels a side.
fn face_crop(face: &Face, out: u32) -> Affine {
    let (_, angle) = eye_line(face);
    let [_, _, w, h] = face.detection.bbox;
    Affine::crop(face.center(), w.max(h) * CROP_SCALE, angle, out)
}

/// Feature outlines of a face in photo pixels.
struct Features {
    eyes: [Polygon; 2],
    brows: [Polygon; 2],
    lips: Polygon,
    nose: Polygon,
    /// Mouth corners and the radius around them where the folds of the
    /// mouth meet (a third of the mouth's width).
    corners: [[f32; 2]; 2],
    corner_radius: f32,
}

impl Features {
    fn new(points: &[[f32; 2]]) -> Self {
        let corners = [points[61], points[291]];
        let width = ((corners[1][0] - corners[0][0]).powi(2)
            + (corners[1][1] - corners[0][1]).powi(2))
        .sqrt();
        Self {
            eyes: [outline(points, &RIGHT_EYE), outline(points, &LEFT_EYE)],
            brows: [outline(points, &RIGHT_BROW), outline(points, &LEFT_BROW)],
            lips: outline(points, &LIPS_OUTER),
            nose: outline(points, &NOSE_WINGS),
            corners,
            corner_radius: width / 3.0,
        }
    }

    /// How much (x, y) is a feature (or within `margin` of one; the nose
    /// wings only when `nose`).
    fn cover(&self, x: f32, y: f32, margin: f32, feather: f32, nose: bool) -> f32 {
        let mut v = 0.0f32;
        for p in self
            .eyes
            .iter()
            .chain(self.brows.iter())
            .chain([&self.lips])
        {
            v = v.max(p.coverage(x, y, margin, feather));
        }
        if nose {
            v = v.max(self.nose.coverage(x, y, margin * 0.5, feather));
            for c in &self.corners {
                let d = ((x - c[0]).powi(2) + (y - c[1]).powi(2)).sqrt();
                if d < self.corner_radius {
                    v = 1.0;
                }
            }
        }
        v
    }
}

/// Probability of `class` at (x, y) from this face's own parse.
fn parse_at(face: &Face, class: Class, x: f32, y: f32) -> f32 {
    let (p, w) = face.parse.sample(class, x, y);
    p * w
}

/// The skin weight of `face` on its crop: face skin from the parse, eyes,
/// brows and lips cut out with a margin for lashes, fitted to the photo.
/// An exact Gaussian blur for small `sigma` (the box passes of `gauss`
/// round a σ below ~1.5 to a whole box).
fn gauss_small(src: &[f32], w: usize, h: usize, sigma: f32) -> Vec<f32> {
    let radius = (3.0 * sigma).ceil().max(1.0) as usize;
    let kernel: Vec<f32> = (0..=2 * radius)
        .map(|i| (-((i as f32 - radius as f32).powi(2)) / (2.0 * sigma * sigma)).exp())
        .collect();
    let total: f32 = kernel.iter().sum();
    let kernel: Vec<f32> = kernel.iter().map(|k| k / total).collect();
    let rows: Vec<f32> = (0..w * h)
        .into_par_iter()
        .map(|k| {
            let (x, y) = (k % w, k / w);
            kernel
                .iter()
                .enumerate()
                .map(|(i, kv)| kv * src[y * w + (x + i).saturating_sub(radius).min(w - 1)])
                .sum()
        })
        .collect();
    (0..w * h)
        .into_par_iter()
        .map(|k| {
            let (x, y) = (k % w, k / w);
            kernel
                .iter()
                .enumerate()
                .map(|(i, kv)| kv * rows[(y + i).saturating_sub(radius).min(h - 1) * w + x])
                .sum()
        })
        .collect()
}

/// Running maximum (`max` true) or minimum over a square window of radius
/// `r` on an `n`² grid, separable.
fn square_filter(src: &[f32], n: usize, r: usize, max: bool) -> Vec<f32> {
    let pass = |data: &[f32], horizontal: bool| -> Vec<f32> {
        (0..n * n)
            .into_par_iter()
            .map(|k| {
                let (x, y) = (k % n, k / n);
                let (lo, hi) = if horizontal {
                    (x.saturating_sub(r), (x + r).min(n - 1))
                } else {
                    (y.saturating_sub(r), (y + r).min(n - 1))
                };
                let values = (lo..=hi).map(|i| {
                    if horizontal {
                        data[y * n + i]
                    } else {
                        data[i * n + x]
                    }
                });
                if max {
                    values.fold(f32::MIN, f32::max)
                } else {
                    values.fold(f32::MAX, f32::min)
                }
            })
            .collect()
    };
    pass(&pass(src, true), false)
}

fn median(mut values: Vec<f32>) -> f32 {
    if values.is_empty() {
        return 0.0;
    }
    let mid = values.len() / 2;
    *values.select_nth_unstable_by(mid, f32::total_cmp).1
}

/// Pixels not in `mask` that the mask encloses (not reachable from the
/// edge of the `n`² grid without crossing it), in holes of at most
/// `max_area` pixels.
fn enclosed(mask: &[bool], n: usize, max_area: usize) -> Vec<bool> {
    let neighbours = |k: usize| {
        let (x, y) = (k % n, k / n);
        [
            (x > 0).then(|| k - 1),
            (x + 1 < n).then(|| k + 1),
            (y > 0).then(|| k - n),
            (y + 1 < n).then(|| k + n),
        ]
        .into_iter()
        .flatten()
    };
    // 0 unvisited, 1 outside, 2 a hole being measured, 3 a kept hole.
    let mut state = vec![0u8; n * n];
    let mut stack: Vec<usize> = (0..n)
        .flat_map(|i| [i, (n - 1) * n + i, i * n, i * n + n - 1])
        .filter(|&k| !mask[k])
        .collect();
    for &k in &stack {
        state[k] = 1;
    }
    while let Some(k) = stack.pop() {
        for j in neighbours(k) {
            if !mask[j] && state[j] == 0 {
                state[j] = 1;
                stack.push(j);
            }
        }
    }
    for start in 0..n * n {
        if mask[start] || state[start] != 0 {
            continue;
        }
        let mut hole = vec![start];
        state[start] = 2;
        let mut i = 0;
        while i < hole.len() {
            for j in neighbours(hole[i]) {
                if !mask[j] && state[j] == 0 {
                    state[j] = 2;
                    hole.push(j);
                }
            }
            i += 1;
        }
        let keep = if hole.len() <= max_area { 3 } else { 1 };
        for k in hole {
            state[k] = keep;
        }
    }
    state.iter().map(|&v| v == 3).collect()
}

/// How much each pixel of the `n`² eyes-level crop is beard (0–1): in the
/// zone below the nose, areas that differ from the face's own skin between
/// the eyes and the nose's base as hair does — more textured for their
/// brightness and darker (dark beards), less colourful (grey and white
/// beards), lighter (white beards) or, in black-and-white photos, just more
/// textured. Shadows on skin are dark but smooth; stubble stays mostly
/// skin. Tuned on twelve bearded test portraits (notes:
/// reference/face-retouch.md).
fn beard_weight(image: &RgbImage, face: &Face, crop: &Affine, n: usize) -> Vec<f32> {
    let (d, _) = eye_line(face);
    let d = d / crop.scale();
    let to_crop = crop.invert();
    let level = |p: [f32; 2]| to_crop.apply(p[0], p[1])[1];
    let (eye_v, nose_v) = match &face.landmarks {
        Some(p) => ((level(p[33]) + level(p[263])) / 2.0, level(p[2])),
        None => {
            let k = &face.detection.keypoints;
            ((level(k[0]) + level(k[1])) / 2.0, level(k[2]) + 0.1 * d)
        }
    };
    let zone = |k: usize| smoothstep(nose_v - 0.15 * d, nose_v + 0.05 * d, (k / n) as f32 + 0.5);
    let pixels = sample_crop(image, crop, n as u32);
    let lin: Vec<[f32; 3]> = pixels.iter().map(|p| p.map(linear)).collect();
    let lum: Vec<f32> = lin
        .iter()
        .map(|p| (0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2]).sqrt())
        .collect();
    let fine_blur = gauss_small(&lum, n, n, (0.008 * d).max(0.7));
    let fine_sq: Vec<f32> = lum
        .iter()
        .zip(&fine_blur)
        .map(|(l, b)| (l - b) * (l - b))
        .collect();
    let local = 0.03 * d;
    let texture = gauss(&fine_sq, n, n, local);
    let mean = gauss(&lum, n, n, local);
    let channel = |c: usize| gauss(&lin.iter().map(|p| p[c]).collect::<Vec<_>>(), n, n, local);
    let (r, g, b) = (channel(0), channel(1), channel(2));
    let chroma: Vec<f32> = (0..n * n)
        .map(|k| {
            let (hi, lo) = (r[k].max(g[k]).max(b[k]), r[k].min(g[k]).min(b[k]));
            (hi - lo) / hi.max(1e-4)
        })
        .collect();
    // Texture for the brightness: dark hair still has its highlights.
    let rel: Vec<f32> = (0..n * n)
        .map(|k| texture[k].max(0.0).sqrt() / mean[k].max(0.02))
        .collect();
    let skin_at = |k: usize| {
        let [x, y] = crop.apply((k % n) as f32 + 0.5, (k / n) as f32 + 0.5);
        parse_at(face, Class::FaceSkin, x, y)
    };
    let mut reference: Vec<usize> = (0..n * n)
        .filter(|&k| {
            let v = (k / n) as f32 + 0.5;
            v > eye_v + 0.15 * d && v < nose_v - 0.05 * d && skin_at(k) > 0.8
        })
        .collect();
    if reference.len() < 50 {
        reference = (0..n * n).filter(|&k| skin_at(k) > 0.8).collect();
    }
    if reference.is_empty() {
        return vec![0.0; n * n];
    }
    let ref_rel = median(reference.iter().map(|&k| rel[k]).collect()).max(1e-4);
    let ref_level = median(reference.iter().map(|&k| mean[k]).collect()).max(1e-4);
    let ref_chroma = median(reference.iter().map(|&k| chroma[k]).collect());
    let mono = ref_chroma < 0.06;
    let raw: Vec<f32> = (0..n * n)
        .into_par_iter()
        .map(|k| {
            let z = zone(k);
            if z <= 0.0 {
                return 0.0;
            }
            let t = rel[k] / ref_rel;
            let l = mean[k] / ref_level;
            let dark = (1.0 - smoothstep(0.55, 0.8, l)) * smoothstep(1.1, 1.6, t);
            let bright = smoothstep(1.1, 1.3, l) * smoothstep(1.3, 1.9, t);
            let grey = if mono {
                smoothstep(1.6, 2.4, t)
            } else {
                (1.0 - smoothstep(0.45, 0.7, chroma[k] / ref_chroma.max(1e-4)))
                    * smoothstep(1.2, 1.8, t)
            };
            z * dark.max(bright).max(grey)
        })
        .collect();
    // Dense beards: close small gaps, then fill what the beard encloses.
    let radius = ((0.03 * d).round() as usize).max(1);
    let closed = square_filter(&square_filter(&raw, n, radius, true), n, radius, false);
    // Holes up to a disc of 0.15 eye distances (a lit patch of a dense
    // beard), not a smooth shadow a crisp edge happens to ring.
    let max_hole = (std::f32::consts::PI * (0.15 * d).powi(2)) as usize;
    let inside = enclosed(
        &closed.iter().map(|&v| v > 0.5).collect::<Vec<_>>(),
        n,
        max_hole,
    );
    let filled: Vec<f32> = (0..n * n)
        .map(|k| {
            if inside[k] {
                closed[k].max(zone(k))
            } else {
                closed[k]
            }
        })
        .collect();
    gauss(&filled, n, n, 0.02 * d)
        .into_iter()
        .map(|v| smoothstep(0.25, 0.6, v))
        .collect()
}

fn skin_weight(image: &RgbImage, face: &Face, crop: &Affine, beard: &[f32]) -> GrayImage {
    let n = WEIGHT_SIZE as usize;
    let (d, _) = eye_line(face);
    let features = face.landmarks.as_deref().map(Features::new);
    let feather = (d * 0.02).max(1.0);
    let raw: Vec<f32> = (0..n * n)
        .into_par_iter()
        .map(|k| {
            let [x, y] = crop.apply((k % n) as f32 + 0.5, (k / n) as f32 + 0.5);
            let skin = parse_at(face, Class::FaceSkin, x, y);
            let cut = features
                .as_ref()
                .map_or(0.0, |f| f.cover(x, y, feather * 1.5, feather, false));
            skin * (1.0 - cut)
        })
        .collect();
    let crop_pixels = sample_crop(image, crop, WEIGHT_SIZE);
    let guide: Vec<f32> = crop_pixels
        .iter()
        .map(|p| (0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2]) / 255.0)
        .collect();
    let refined = guided_filter(&guide, &raw, n, n, 4, 2e-3);
    GrayImage::from_fn(WEIGHT_SIZE, WEIGHT_SIZE, |u, v| {
        let k = v as usize * n + u as usize;
        // Levels as for the People masks; never more than the parse
        // allowed plus a little, so the filter does not grow skin into hair.
        // The beard is hair, not skin: even skin and texture leave it be.
        let value = ((refined[k] - 0.1) / 0.8)
            .clamp(0.0, 1.0)
            .min(raw[k] + 0.15)
            * (1.0 - beard[k]);
        Luma([(value * 255.0).round() as u8])
    })
}

fn midpoint(a: [f32; 2], b: [f32; 2]) -> [f32; 2] {
    [(a[0] + b[0]) / 2.0, (a[1] + b[1]) / 2.0]
}

fn distance(a: [f32; 2], b: [f32; 2]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt()
}

/// Whether YuNet's keypoints describe an upright face plausibly: the nose
/// between the eyes and the mouth, the eyes in the upper middle of the box,
/// eyes to mouth a third to half of it. On a face lying on its side or
/// seen from the top of the head they do not (keypoints on the scalp, a
/// nose on the mouth's line), and nothing is built from them.
fn keypoints_plausible(face: &Face) -> bool {
    let k = &face.detection.keypoints;
    let [_, y, _, h] = face.detection.bbox;
    let eyes = midpoint(k[0], k[1]);
    let mouth = midpoint(k[3], k[4]);
    let down = [mouth[0] - eyes[0], mouth[1] - eyes[1]];
    let length2 = down[0] * down[0] + down[1] * down[1];
    if length2 < 4.0 || h <= 0.0 {
        return false;
    }
    let nose = ((k[2][0] - eyes[0]) * down[0] + (k[2][1] - eyes[1]) * down[1]) / length2;
    let eye_level = |p: [f32; 2]| (p[1] - y) / h;
    let to_mouth = length2.sqrt() / h;
    (0.3..0.7).contains(&nose)
        && (0.25..0.55).contains(&eye_level(k[0]))
        && (0.25..0.55).contains(&eye_level(k[1]))
        && (0.25..0.6).contains(&to_mouth)
}

/// The chin in photo pixels: Face Mesh's, or for faces without landmarks
/// one estimated from YuNet's keypoints (below the mouth by half the eyes'
/// distance to it) — only where the parse puts the hair on the other side
/// of the eyes. YuNet assumes upright faces; on a face lying on its side
/// its keypoints are wrong but consistent, and "below the chin" would
/// fall on a hand or a pillow.
fn chin_point(face: &Face) -> Option<[f32; 2]> {
    if let Some(points) = &face.landmarks {
        return Some(points[152]);
    }
    if !keypoints_plausible(face) {
        return None;
    }
    let k = &face.detection.keypoints;
    let eyes = midpoint(k[0], k[1]);
    let mouth = midpoint(k[3], k[4]);
    let down = [mouth[0] - eyes[0], mouth[1] - eyes[1]];
    let length = distance(eyes, mouth);
    // Hair around the head, weighted by its probability.
    let [x, y, w, h] = face.detection.bbox;
    let (centre, reach) = ([x + w / 2.0, y + h / 2.0], w.max(h));
    let (mut sum, mut sx, mut sy) = (0.0f32, 0.0f32, 0.0f32);
    for j in 0..32 {
        for i in 0..32 {
            let px = centre[0] + reach * (i as f32 / 31.0 - 0.5) * 2.0;
            let py = centre[1] + reach * (j as f32 / 31.0 - 0.5) * 2.0;
            let p = parse_at(face, Class::Hair, px, py);
            sum += p;
            sx += p * (px - centre[0]);
            sy += p * (py - centre[1]);
        }
    }
    if sum < 20.0 {
        return None;
    }
    let (hx, hy) = (sx / sum, sy / sum);
    let up = (-(hx * down[0] + hy * down[1])) / ((hx * hx + hy * hy).sqrt() * length).max(1e-6);
    (up > 0.6).then(|| [mouth[0] + 0.5 * down[0], mouth[1] + 0.5 * down[1]])
}

/// How much a crop point (eyes level) is on the neck: below the chin
/// (`chin` in the same crop's pixels) and within a column under the face
/// (a hand or shoulder beside it is body skin too).
fn neck_zone(face: &Face, crop: &Affine, n: f32, u: f32, v: f32, chin: [f32; 2]) -> f32 {
    let [cx, cy] = chin;
    let [_, _, bw, bh] = face.detection.bbox;
    let (w, h) = (bw / crop.scale(), bh / crop.scale());
    let below = (v - cy) / h.max(1.0);
    // The column's middle between the chin and the face's centre: turned
    // faces show the neck behind the chin.
    let aside = (u - (cx + n / 2.0) / 2.0).abs() / w.max(1.0);
    // Down to about the collar: below that an arm or a shoulder joins the
    // neck's body skin (a child leaning over a table).
    smoothstep(-0.05, 0.1, below)
        * (1.0 - smoothstep(0.65, 0.85, below))
        * (1.0 - smoothstep(0.7, 0.95, aside))
}

/// The neck's skin on the crop: body skin from the parse below the chin,
/// fitted to the photo; `None` when the crop shows none or the chin is not
/// known.
fn neck_weight(image: &RgbImage, face: &Face, crop: &Affine) -> Option<GrayImage> {
    let n = WEIGHT_SIZE as usize;
    let chin = chin_point(face)?;
    let chin = crop.invert().apply(chin[0], chin[1]);
    let attached = neck_region(face, crop, n, chin);
    let raw: Vec<f32> = (0..n * n)
        .into_par_iter()
        .map(|k| {
            if !attached[k] {
                return 0.0;
            }
            let (u, v) = ((k % n) as f32 + 0.5, (k / n) as f32 + 0.5);
            let [x, y] = crop.apply(u, v);
            parse_at(face, Class::BodySkin, x, y) * neck_zone(face, crop, n as f32, u, v, chin)
        })
        .collect();
    if raw.iter().all(|&v| v < 0.3) {
        return None;
    }
    let crop_pixels = sample_crop(image, crop, WEIGHT_SIZE);
    let guide: Vec<f32> = crop_pixels
        .iter()
        .map(|p| (0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2]) / 255.0)
        .collect();
    let refined = guided_filter(&guide, &raw, n, n, 4, 2e-3);
    Some(GrayImage::from_fn(WEIGHT_SIZE, WEIGHT_SIZE, |u, v| {
        let k = v as usize * n + u as usize;
        let value = ((refined[k] - 0.1) / 0.8)
            .clamp(0.0, 1.0)
            .min(raw[k] + 0.15);
        Luma([(value * 255.0).round() as u8])
    }))
}

/// The body skin of the neck zone joined to the face's skin (on an `n`²
/// crop grid): a hand or shoulder in the zone that does not touch the face
/// stays out.
fn neck_region(face: &Face, crop: &Affine, n: usize, chin: [f32; 2]) -> Vec<bool> {
    let (body, face_skin): (Vec<bool>, Vec<bool>) = (0..n * n)
        .into_par_iter()
        .map(|k| {
            let (u, v) = ((k % n) as f32 + 0.5, (k / n) as f32 + 0.5);
            let [x, y] = crop.apply(u, v);
            let zone = neck_zone(face, crop, n as f32, u, v, chin);
            (
                zone > 0.0 && parse_at(face, Class::BodySkin, x, y) > 0.5,
                parse_at(face, Class::FaceSkin, x, y) > 0.5,
            )
        })
        .unzip();
    // Seeds: neck pixels within 1 % of the crop of the face's skin.
    let reach = (n / 100).max(1) as isize;
    let mut region = vec![false; n * n];
    let mut queue: Vec<usize> = Vec::new();
    for k in 0..n * n {
        if !body[k] {
            continue;
        }
        let (x, y) = ((k % n) as isize, (k / n) as isize);
        let near_face = (-reach..=reach).any(|dy| {
            (-reach..=reach).any(|dx| {
                let (a, b) = (x + dx, y + dy);
                a >= 0
                    && b >= 0
                    && (a as usize) < n
                    && (b as usize) < n
                    && face_skin[b as usize * n + a as usize]
            })
        });
        if near_face {
            region[k] = true;
            queue.push(k);
        }
    }
    while let Some(k) = queue.pop() {
        let (x, y) = (k % n, k / n);
        let mut visit = |j: usize| {
            if body[j] && !region[j] {
                region[j] = true;
                queue.push(j);
            }
        };
        if x > 0 {
            visit(k - 1);
        }
        if x + 1 < n {
            visit(k + 1);
        }
        if y > 0 {
            visit(k - n);
        }
        if y + 1 < n {
            visit(k + n);
        }
    }
    region
}

fn smoothstep(lo: f32, hi: f32, x: f32) -> f32 {
    let t = ((x - lo) / (hi - lo)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The under-eye areas on the crop: from each lower lid down by 30 % of the
/// eye distance, soft, without the eye.
fn under_eye_weight(face: &Face, crop: &Affine, skin: &GrayImage) -> Option<GrayImage> {
    let Some(points) = face.landmarks.as_deref() else {
        return under_eye_from_keypoints(face, crop, skin);
    };
    let to_crop = crop.invert();
    let (d, _) = eye_line(face);
    let scale = crop.scale();
    let d_crop = d / scale;
    let areas: Vec<Polygon> = [&RIGHT_LOWER_LID[..], &LEFT_LOWER_LID[..]]
        .iter()
        .map(|lid| {
            let top: Vec<[f32; 2]> = lid
                .iter()
                .map(|&i| to_crop.apply(points[i][0], points[i][1]))
                .collect();
            // In the crop the eyes are level: down is +v.
            let mut ring = top.clone();
            for p in top.iter().rev() {
                ring.push([p[0], p[1] + 0.30 * d_crop]);
            }
            Polygon::new(ring)
        })
        .collect();
    let eyes: Vec<Polygon> = [&RIGHT_EYE[..], &LEFT_EYE[..]]
        .iter()
        .map(|e| {
            Polygon::new(
                e.iter()
                    .map(|&i| to_crop.apply(points[i][0], points[i][1]))
                    .collect(),
            )
        })
        .collect();
    let feather = 0.10 * d_crop;
    let margin = 0.03 * d_crop;
    Some(GrayImage::from_fn(WEIGHT_SIZE, WEIGHT_SIZE, |u, v| {
        let (x, y) = (u as f32 + 0.5, v as f32 + 0.5);
        let area = areas
            .iter()
            .map(|a| a.coverage(x, y, 0.0, feather))
            .fold(0.0, f32::max);
        let eye = eyes
            .iter()
            .map(|e| e.coverage(x, y, margin, margin))
            .fold(0.0, f32::max);
        let s = skin.get_pixel(u, v)[0] as f32 / 255.0;
        Luma([((area * (1.0 - eye) * s.max(0.5 * area)).clamp(0.0, 1.0) * 255.0).round() as u8])
    }))
}

/// The under-eye areas of a face without landmarks, from YuNet's eye
/// keypoints (the crop is eyes level: down is +v): below each visible eye,
/// from the lower lid down by about 30 % of the eye distance, without the
/// eye, on the face's skin. A profile shows one eye: the keypoint farther
/// from the nose (YuNet puts the hidden eye beside the nose bridge), and
/// the eyes' distance to the mouth gives the scale.
fn under_eye_from_keypoints(face: &Face, crop: &Affine, skin: &GrayImage) -> Option<GrayImage> {
    if !keypoints_plausible(face) {
        return None;
    }
    let to_crop = crop.invert();
    let k = face.detection.keypoints.map(|p| to_crop.apply(p[0], p[1]));
    let (e0, e1, nose) = (k[0], k[1], k[2]);
    let mouth = midpoint(k[3], k[4]);
    let to_mouth = distance(midpoint(e0, e1), mouth);
    let gap = distance(e0, e1);
    if to_mouth < 4.0 {
        return None;
    }
    let eyes = if gap > 0.5 * to_mouth {
        vec![e0, e1]
    } else if distance(e0, nose) > distance(e1, nose) {
        vec![e0]
    } else {
        vec![e1]
    };
    // Outer-corner eye distance (keypoints are the pupils).
    let d = (gap * OUTER_OVER_CENTRES).max(0.95 * to_mouth);
    let ellipse = |x: f32, y: f32, c: [f32; 2], dy: f32, rx: f32, ry: f32| {
        let q = ((x - c[0]) / rx).powi(2) + ((y - c[1] - dy) / ry).powi(2);
        1.0 - smoothstep(0.6, 1.0, q)
    };
    let weight = GrayImage::from_fn(WEIGHT_SIZE, WEIGHT_SIZE, |u, v| {
        let (x, y) = (u as f32 + 0.5, v as f32 + 0.5);
        let area = eyes
            .iter()
            .map(|&e| ellipse(x, y, e, 0.22 * d, 0.21 * d, 0.15 * d))
            .fold(0.0, f32::max);
        let eye = eyes
            .iter()
            .map(|&e| ellipse(x, y, e, 0.0, 0.16 * d, 0.07 * d))
            .fold(0.0, f32::max);
        let s = skin.get_pixel(u, v)[0] as f32 / 255.0;
        Luma([((area * (1.0 - eye) * s).clamp(0.0, 1.0) * 255.0).round() as u8])
    });
    weight.pixels().any(|p| p[0] > 64).then_some(weight)
}

/// Gaussian blur by three box passes per axis.
fn gauss(src: &[f32], w: usize, h: usize, sigma: f32) -> Vec<f32> {
    let r = (((sigma * sigma) + 0.25).sqrt() - 0.5).round().max(1.0) as usize;
    let pass = |data: &[f32], w: usize| -> Vec<f32> {
        let mut out = vec![0.0f32; data.len()];
        out.par_chunks_mut(w)
            .zip(data.par_chunks(w))
            .for_each(|(o, row)| {
                let mut prefix = Vec::with_capacity(w + 1);
                prefix.push(0.0f64);
                for &v in row {
                    prefix.push(prefix.last().unwrap() + v as f64);
                }
                for (x, value) in o.iter_mut().enumerate() {
                    let lo = x.saturating_sub(r);
                    let hi = (x + r + 1).min(w);
                    *value = ((prefix[hi] - prefix[lo]) / (hi - lo) as f64) as f32;
                }
            });
        out
    };
    let transpose = |data: &[f32], w: usize, h: usize| -> Vec<f32> {
        let mut out = vec![0.0f32; w * h];
        out.par_chunks_mut(h).enumerate().for_each(|(x, col)| {
            for (y, v) in col.iter_mut().enumerate() {
                *v = data[y * w + x];
            }
        });
        out
    };
    let mut data = src.to_vec();
    for _ in 0..3 {
        data = pass(&data, w);
    }
    let mut t = transpose(&data, w, h);
    for _ in 0..3 {
        t = pass(&t, h);
    }
    transpose(&t, h, w)
}

fn linear(v: f32) -> f32 {
    let v = v / 255.0;
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

/// Blemish candidates of a face, in photo pixels, most prominent first.
fn find_blemishes(image: &RgbImage, face: &Face) -> Vec<Blemish> {
    let (d, angle) = eye_line(face);
    if d < 24.0 {
        return Vec::new();
    }
    let scale = (DETECT_EYE_DISTANCE / d).min(1.0);
    let [_, _, bw, bh] = face.detection.bbox;
    let side = bw.max(bh) * CROP_SCALE;
    let n = ((side * scale).round() as u32).clamp(64, 2048);
    let crop = Affine::crop(face.center(), side, angle, n);
    let dd = d * n as f32 / side; // eye distance in detection pixels
    let nn = n as usize;
    let pixels = sample_crop(image, &crop, n);
    let lin: Vec<[f32; 3]> = pixels
        .par_iter()
        .map(|p| [linear(p[0]), linear(p[1]), linear(p[2])])
        .collect();
    let lum: Vec<f32> = lin
        .iter()
        .map(|p| {
            (0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2])
                .max(0.0)
                .sqrt()
        })
        .collect();
    let red: Vec<f32> = lin
        .iter()
        .map(|p| ((p[0] + 1e-3) / (p[1] + 1e-3)).ln())
        .collect();

    // Where blemishes may be: clear face skin, away from the features and
    // the nose wings, not hair.
    let features = face.landmarks.as_deref().map(Features::new);
    // The face itself: ears and neck stay out.
    let oval = face.landmarks.as_deref().map(|p| outline(p, &FACE_OVAL));
    let margin = 0.06 * d;
    // Without landmarks (profiles): near the eyes, nose and mouth only
    // (1.5 times the eye–mouth distance from their centre), which leaves
    // out the ear of a close-up profile.
    let k = &face.detection.keypoints;
    let centre = [
        (k[0][0] + k[1][0] + k[2][0] + k[3][0] + k[4][0]) / 5.0,
        (k[0][1] + k[1][1] + k[2][1] + k[3][1] + k[4][1]) / 5.0,
    ];
    let eye_mouth = (((k[0][0] + k[1][0]) - (k[3][0] + k[4][0])).powi(2)
        + ((k[0][1] + k[1][1]) - (k[3][1] + k[4][1])).powi(2))
    .sqrt()
        / 2.0;
    let reach = 1.5 * eye_mouth.max(0.25 * d);
    let valid: Vec<bool> = (0..nn * nn)
        .into_par_iter()
        .map(|k| {
            let [x, y] = crop.apply((k % nn) as f32 + 0.5, (k / nn) as f32 + 0.5);
            let skin = parse_at(face, Class::FaceSkin, x, y);
            let hair = parse_at(face, Class::Hair, x, y);
            let cut = features
                .as_ref()
                .map_or(0.0, |f| f.cover(x, y, margin, 1.0, true));
            let inside = match &oval {
                Some(o) => o.coverage(x, y, 0.04 * d, 1.0) > 0.5,
                None => ((x - centre[0]).powi(2) + (y - centre[1]).powi(2)).sqrt() < reach,
            };
            skin > 0.7 && hair < 0.2 && cut < 0.5 && inside
        })
        .collect();
    // The neck: body skin below the chin (blemishes found there are marked
    // and healed only with "include neck").
    let chin = chin_point(face).map(|c| crop.invert().apply(c[0], c[1]));
    let attached = chin.map(|c| neck_region(face, &crop, nn, c));
    let on_neck: Vec<bool> = (0..nn * nn)
        .into_par_iter()
        .map(|k| {
            let (Some(chin), Some(attached)) = (chin, &attached) else {
                return false;
            };
            let (u, v) = ((k % nn) as f32 + 0.5, (k / nn) as f32 + 0.5);
            let [x, y] = crop.apply(u, v);
            attached[k]
                && neck_zone(face, &crop, nn as f32, u, v, chin) > 0.9
                && parse_at(face, Class::BodySkin, x, y) > 0.7
                && parse_at(face, Class::Hair, x, y) < 0.2
        })
        .collect();
    let valid: Vec<bool> = valid.iter().zip(&on_neck).map(|(a, b)| *a || *b).collect();
    // Shrink by 3 % of the eye distance (a blemish needs skin around it).
    let erode = (0.03 * dd).round().max(1.0) as usize;
    let valid_f: Vec<f32> = valid.iter().map(|&v| if v { 1.0 } else { 0.0 }).collect();
    let mean = gauss(&valid_f, nn, nn, erode as f32);
    let valid: Vec<bool> = mean.iter().map(|&m| m > 0.97).collect();

    // Around each blemish: the fine texture (pores, stubble) and the
    // lightness, against the face's medians. Beard stubble is far rougher
    // and a dense beard far darker than skin: no place for blemishes. The
    // surroundings are sampled on a ring, so the blemish itself does not
    // count.
    let fine_sigma = (0.004 * dd).max(0.7);
    let smooth = gauss(&lum, nn, nn, fine_sigma);
    let fine: Vec<f32> = lum
        .iter()
        .zip(&smooth)
        .map(|(l, s)| (l - s).abs() / s.max(1e-3))
        .collect();
    let texture = gauss(&fine, nn, nn, 0.02 * dd);
    let level = gauss(&lum, nn, nn, 0.02 * dd);
    let median_on_skin = |map: &[f32]| -> Option<f32> {
        let mut values: Vec<f32> = map
            .iter()
            .zip(&valid)
            .filter(|(_, v)| **v)
            .map(|(t, _)| *t)
            .collect();
        if values.is_empty() {
            return None;
        }
        let mid = values.len() / 2;
        Some(*values.select_nth_unstable_by(mid, f32::total_cmp).1)
    };
    let (Some(median_texture), Some(median_level), Some(median_red)) = (
        median_on_skin(&texture),
        median_on_skin(&level),
        median_on_skin(&red),
    ) else {
        return Vec::new();
    };
    let surroundings = |x: usize, y: usize, sigma: f32| -> (f32, f32) {
        let rho = (4.0 * sigma).max(0.03 * dd);
        let mut t = [0.0f32; 8];
        let mut l = [0.0f32; 8];
        for (i, (tt, ll)) in t.iter_mut().zip(l.iter_mut()).enumerate() {
            let a = i as f32 * std::f32::consts::TAU / 8.0;
            let sx = (x as f32 + rho * a.cos()).clamp(0.0, (nn - 1) as f32) as usize;
            let sy = (y as f32 + rho * a.sin()).clamp(0.0, (nn - 1) as f32) as usize;
            *tt = texture[sy * nn + sx];
            *ll = level[sy * nn + sx];
        }
        t.sort_by(f32::total_cmp);
        l.sort_by(f32::total_cmp);
        (t[4], l[4])
    };

    // Scales from 0.18 % of the eye distance (the fine red and brown dots,
    // two or three pixels across at 560 px, that Capture One's Blemish takes
    // early) up; prominence counts size against 0.5 %.
    let sigma0 = (0.0018 * dd).max(0.8);
    let sigma_ref = 0.005 * dd;
    let mut found: Vec<(f32, usize, usize, f32)> = Vec::new();
    for i in 0..12 {
        let sigma = sigma0 * 1.3f32.powi(i);
        if sigma < 0.6 {
            continue;
        }
        let lc = gauss(&lum, nn, nn, sigma);
        let ls = gauss(&lum, nn, nn, 3.0 * sigma);
        let rc = gauss(&red, nn, nn, sigma);
        let rs = gauss(&red, nn, nn, 3.0 * sigma);
        let score: Vec<f32> = (0..nn * nn)
            .into_par_iter()
            .map(|k| {
                let dark = (ls[k] - lc[k]) / ls[k].max(1e-3);
                let redness = rc[k] - rs[k];
                // Very dark and not red: a mole or a pore opening, which
                // the skin keeps; darker still (a fifth of the lightness
                // around): a nostril or an eye corner, never a blemish.
                if (dark > 0.4 && redness < 0.3 * dark) || dark > 0.55 {
                    return 0.0;
                }
                // A light head on an inflamed (red) base: a pustule. Light
                // spots without the red ring are shine and stay.
                let light = -dark;
                let ring_red = rs[k] - median_red;
                if light > 0.06 && ring_red > 0.04 {
                    return light + 0.6 * ring_red;
                }
                let s = dark.max(0.0) + 0.6 * redness.max(0.0);
                // Brown rather than red (freckles, pigment marks): part of
                // the person, taken only towards the slider's end.
                if redness < 0.5 * dark { 0.6 * s } else { s }
            })
            .collect();
        let sq: Vec<f32> = score.iter().map(|s| s * s).collect();
        let bg = gauss(&sq, nn, nn, 5.0 * sigma);
        let win = (sigma.round() as usize).max(1);
        let step = (sigma.round() as usize).max(1);
        let peaks: Vec<(f32, usize, usize, f32)> = (win..nn.saturating_sub(win))
            .into_par_iter()
            .flat_map_iter(|y| {
                let (lc, score, bg, valid, surroundings) =
                    (&lc, &score, &bg, &valid, &surroundings);
                (win..nn - win).filter_map(move |x| {
                    let k = y * nn + x;
                    let s = score[k];
                    if s < 0.03 || !valid[k] {
                        return None;
                    }
                    // Standing out from the response around: a dot in a
                    // field of dots (freckles, pores) counts less but is
                    // still taken towards the slider's end.
                    let z = s / (bg[k].sqrt() + 0.01);
                    if z < 1.2 {
                        return None;
                    }
                    for yy in y - win..=y + win {
                        for xx in x - win..=x + win {
                            if score[yy * nn + xx] > s {
                                return None;
                            }
                        }
                    }
                    // A blob, not an edge: Hessian of the blurred luminance.
                    let at = |xx: usize, yy: usize| lc[yy * nn + xx];
                    let (xm, xp) = (x - step.min(x), (x + step).min(nn - 1));
                    let (ym, yp) = (y - step.min(y), (y + step).min(nn - 1));
                    let c = at(x, y);
                    let dxx = at(xp, y) + at(xm, y) - 2.0 * c;
                    let dyy = at(x, yp) + at(x, ym) - 2.0 * c;
                    let dxy = (at(xp, yp) + at(xm, ym) - at(xp, ym) - at(xm, yp)) / 4.0;
                    let det = dxx * dyy - dxy * dxy;
                    let tr = dxx + dyy;
                    if det <= 0.0 || tr * tr >= det * 6.25 {
                        return None;
                    }
                    // Not part of a line (a smile fold, a wrinkle, a hair):
                    // around a spot the gradients point every way, along a
                    // line they share one direction (structure tensor on a
                    // ring of three times the scale, at least four pixels,
                    // so the neighbouring pores of a small dot do not
                    // decide).
                    let rho = (3.0 * sigma).max(4.0);
                    let (mut jxx, mut jxy, mut jyy) = (0.0f32, 0.0f32, 0.0f32);
                    for i in 0..16 {
                        let a = i as f32 * std::f32::consts::TAU / 16.0;
                        let px = (x as f32 + rho * a.cos())
                            .round()
                            .clamp(1.0, (nn - 2) as f32) as usize;
                        let py = (y as f32 + rho * a.sin())
                            .round()
                            .clamp(1.0, (nn - 2) as f32) as usize;
                        let gx = (at(px + 1, py) - at(px - 1, py)) / 2.0;
                        let gy = (at(px, py + 1) - at(px, py - 1)) / 2.0;
                        jxx += gx * gx;
                        jxy += gx * gy;
                        jyy += gy * gy;
                    }
                    let coherence =
                        ((jxx - jyy).powi(2) + 4.0 * jxy * jxy).sqrt() / (jxx + jyy).max(1e-12);
                    if coherence > 0.7 {
                        return None;
                    }
                    let (rough, light) = surroundings(x, y, sigma);
                    if rough > 2.5 * median_texture || light < 0.6 * median_level {
                        return None;
                    }
                    let prominence = s * (sigma / sigma_ref).powf(0.35) * (z / 4.0).min(1.0);
                    Some((prominence, x, y, sigma))
                })
            })
            .collect();
        found.extend(peaks);
    }
    found.sort_by(|a, b| b.0.total_cmp(&a.0));
    let mut kept: Vec<(f32, usize, usize, f32)> = Vec::new();
    for c in found {
        let clear = kept.iter().all(|k| {
            let dist =
                ((c.1 as f32 - k.1 as f32).powi(2) + (c.2 as f32 - k.2 as f32).powi(2)).sqrt();
            // A dot inside a larger, fainter blemish stays its own: healing
            // the large one keeps everything finer than a third of it.
            // (one spot seen at two scales has one centre: suppressed).
            let (small, large) = (c.3.min(k.3), c.3.max(k.3));
            if dist < 2.0 * small {
                return false;
            }
            large > 2.0 * small || dist > 1.5 * large
        });
        if clear {
            kept.push(c);
            if kept.len() >= MAX_BLEMISHES {
                break;
            }
        }
    }
    let to_photo = crop.scale();
    kept.iter()
        .enumerate()
        .map(|(i, &(score, x, y, sigma))| {
            let [px, py] = crop.apply(x as f32 + 0.5, y as f32 + 0.5);
            let neck = on_neck[y * nn + x];
            Blemish {
                id: i as u32 + 1,
                x: px,
                y: py,
                // Enough to take a red halo along.
                r: 2.4 * sigma * to_photo,
                score,
                neck,
            }
        })
        .collect()
}

/// Everything the retouch needs of face `index`.
pub fn retouch_face(
    image: &RgbImage,
    analysis: &FaceAnalysis,
    index: usize,
) -> Option<RetouchFace> {
    let face = analysis.faces.get(index)?;
    let crop = face_crop(face, WEIGHT_SIZE);
    let n = WEIGHT_SIZE as usize;
    let beard = beard_weight(image, face, &crop, n);
    let skin = skin_weight(image, face, &crop, &beard);
    let under_eyes = under_eye_weight(face, &crop, &skin);
    let neck = neck_weight(image, face, &crop);
    let (eye_distance, _) = eye_line(face);
    Some(RetouchFace {
        crop,
        eye_distance,
        skin,
        under_eyes,
        neck,
        // None inside a beard (hairs, not blemishes).
        blemishes: find_blemishes(image, face)
            .into_iter()
            .filter(|b| {
                let [u, v] = crop.invert().apply(b.x, b.y);
                let (u, v) = (u.floor() as i64, v.floor() as i64);
                !(0..n as i64).contains(&u)
                    || !(0..n as i64).contains(&v)
                    || beard[v as usize * n + u as usize] < 0.5
            })
            .collect(),
    })
}

/// How strongly the parse sees skin around a point, for tests.
#[cfg(test)]
fn skin_level(weight: &GrayImage, u: u32, v: u32) -> f32 {
    weight.get_pixel(u, v)[0] as f32 / 255.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::Detection;
    use crate::parse::{CLASSES, INPUT, ParseCrop, whole_photo};

    fn analysis_with_skin(image: &RgbImage) -> FaceAnalysis {
        let (w, h) = image.dimensions();
        let mut probs = vec![0u8; (INPUT * INPUT) as usize * CLASSES];
        for px in probs.as_chunks_mut::<CLASSES>().0 {
            px[Class::FaceSkin as usize] = 255;
        }
        let center = [w as f32 / 2.0, h as f32 / 2.0];
        let face = Face {
            detection: Detection {
                score: 0.9,
                bbox: [center[0] - 100.0, center[1] - 100.0, 200.0, 200.0],
                keypoints: [
                    [center[0] - 45.0, center[1] - 20.0],
                    [center[0] + 45.0, center[1] - 20.0],
                    center,
                    [center[0] - 30.0, center[1] + 40.0],
                    [center[0] + 30.0, center[1] + 40.0],
                ],
            },
            landmarks: None,
            parse: ParseCrop::new(
                crate::geometry::Affine::crop(center, 600.0, 0.0, INPUT),
                probs,
            ),
        };
        FaceAnalysis {
            width: w,
            height: h,
            faces: vec![face],
            global: ParseCrop::new(
                whole_photo(w, h),
                vec![0u8; (INPUT * INPUT) as usize * CLASSES],
            ),
        }
    }

    #[test]
    fn finds_dark_and_red_spots_and_ranks_the_clear_one_first() {
        // Skin with fine, low-contrast texture, a clear dark red spot and a
        // faint one.
        let mut image = RgbImage::from_fn(600, 600, |x, y| {
            let t = ((x * 7 + y * 13) % 5) as i32 - 2;
            image::Rgb([(200 + t) as u8, (160 + t) as u8, (140 + t) as u8])
        });
        let spot = |image: &mut RgbImage, cx: f32, cy: f32, r: f32, k: f32| {
            for (x, y, p) in image.enumerate_pixels_mut() {
                let d = ((x as f32 - cx).powi(2) + (y as f32 - cy).powi(2)).sqrt();
                let a = (1.0 - (d / r).powi(2)).max(0.0) * k;
                p[0] = (p[0] as f32 * (1.0 - 0.3 * a)) as u8;
                p[1] = (p[1] as f32 * (1.0 - 0.6 * a)) as u8;
                p[2] = (p[2] as f32 * (1.0 - 0.6 * a)) as u8;
            }
        };
        // A dark fold (a line of the face) must not be taken for blemishes.
        for (x, y, p) in image.enumerate_pixels_mut() {
            let d = (x as f32 - 300.0 - 0.3 * (y as f32 - 300.0)).abs();
            if (240..360).contains(&y) && d < 10.0 {
                // Fading out at both ends, as a fold does.
                let taper = (std::f32::consts::PI * (y as f32 - 240.0) / 120.0).sin();
                let a = (1.0 - d / 10.0) * taper;
                for c in 0..3 {
                    p[c] = (p[c] as f32 * (1.0 - 0.25 * a)) as u8;
                }
            }
        }
        spot(&mut image, 260.0, 330.0, 6.0, 1.0);
        spot(&mut image, 350.0, 280.0, 3.0, 0.35);
        let analysis = analysis_with_skin(&image);
        let face = retouch_face(&image, &analysis, 0).unwrap();
        assert!(!face.blemishes.is_empty());
        let first = face.blemishes[0];
        assert!(
            (first.x - 260.0).abs() < 3.0 && (first.y - 330.0).abs() < 3.0,
            "{first:?}"
        );
        assert!(first.r > 3.0 && first.r < 14.0, "{first:?}");
        let faint = face
            .blemishes
            .iter()
            .find(|b| (b.x - 350.0).abs() < 3.0 && (b.y - 280.0).abs() < 3.0);
        assert!(faint.is_some_and(|f| f.score < first.score));
        let on_fold = face.blemishes.iter().filter(|b| {
            (240.0..360.0).contains(&b.y) && (b.x - 300.0 - 0.3 * (b.y - 300.0)).abs() < 8.0
        });
        assert_eq!(on_fold.count(), 0, "{:?}", face.blemishes);
        // The flat skin itself yields nothing else of note.
        assert!(face.blemishes.iter().filter(|b| b.score > 0.05).count() <= 2);
        // The skin weight covers the face's middle.
        assert!(skin_level(&face.skin, WEIGHT_SIZE / 2, WEIGHT_SIZE / 2) > 0.9);
        // Without landmarks the under-eye area comes from the keypoints:
        // below the eyes (keypoints 20 px above the centre), not above.
        let under = face
            .under_eyes
            .as_ref()
            .expect("under-eye area from keypoints");
        let to_crop = face.crop.invert();
        let at = |x: f32, y: f32| {
            let [u, v] = to_crop.apply(x, y);
            skin_level(under, u as u32, v as u32)
        };
        assert!(at(255.0, 300.0) > 0.5, "{}", at(255.0, 300.0));
        assert!(at(255.0, 250.0) < 0.05);
        assert!(at(255.0, 280.0) < 0.5, "the eye itself stays out");
        assert!((face.eye_distance - 90.0 * OUTER_OVER_CENTRES).abs() < 1e-3);
    }

    #[test]
    fn a_beard_is_not_skin_but_a_shadow_is() {
        // Skin with fine texture; below the nose (keypoint at the centre) a
        // dark, hairy patch on one side and a soft shadow as dark on the
        // other.
        let image = RgbImage::from_fn(600, 600, |x, y| {
            let t = ((x * 7 + y * 13) % 5) as f32 - 2.0;
            let hairy = (330..380).contains(&y) && (220..290).contains(&x);
            let (xf, yf) = (x as f32, y as f32);
            // Shade dims the skin and its texture alike, with a soft edge.
            let edge = |v: f32, lo: f32, hi: f32| ((v - lo).min(hi - v) / 12.0).clamp(0.0, 1.0);
            let shade = 1.0 - 0.625 * edge(xf, 300.0, 400.0) * edge(yf, 320.0, 390.0);
            let v = if hairy {
                // Strands: dark, with light hairs every few pixels.
                70.0 + if (x + 2 * y).is_multiple_of(4) {
                    60.0
                } else {
                    -15.0
                }
            } else {
                (200.0 + t) * shade
            };
            image::Rgb([v as u8, (v * 0.8) as u8, (v * 0.7) as u8])
        });
        let analysis = analysis_with_skin(&image);
        let face = retouch_face(&image, &analysis, 0).unwrap();
        let to_crop = face.crop.invert();
        let at = |x: f32, y: f32| {
            let [u, v] = to_crop.apply(x, y);
            skin_level(&face.skin, u as u32, v as u32)
        };
        assert!(at(255.0, 355.0) < 0.2, "beard {}", at(255.0, 355.0));
        assert!(at(345.0, 355.0) > 0.8, "shadow {}", at(345.0, 355.0));
        // Above the nose nothing is beard.
        assert!(at(255.0, 250.0) > 0.8);
    }

    #[test]
    fn enclosed_holes_are_found_and_the_outside_is_not() {
        let n = 9;
        // A ring around the middle pixel.
        let mask: Vec<bool> = (0..n * n)
            .map(|k| {
                let (x, y) = ((k % n) as i32 - 4, (k / n) as i32 - 4);
                x.abs().max(y.abs()) == 2
            })
            .collect();
        let inside = enclosed(&mask, n, 100);
        assert!(inside[4 * n + 4]);
        assert!(inside[3 * n + 4]);
        assert!(!inside[0]);
        assert!(!inside[2 * n + 2], "the ring itself");
        assert_eq!(inside.iter().filter(|&&v| v).count(), 9);
        // Larger holes stay open.
        assert!(!enclosed(&mask, n, 8).iter().any(|&v| v));
    }

    #[test]
    fn under_eye_area_lies_below_the_lower_lids() {
        let image = RgbImage::from_pixel(600, 600, image::Rgb([200, 160, 140]));
        let mut analysis = analysis_with_skin(&image);
        let mut points = vec![[300.0f32, 300.0]; crate::landmarks::COUNT];
        let ring = |points: &mut Vec<[f32; 2]>, idx: &[usize], c: [f32; 2]| {
            for (k, &i) in idx.iter().enumerate() {
                let a = k as f32 / idx.len() as f32 * std::f32::consts::TAU;
                points[i] = [c[0] + 20.0 * a.cos(), c[1] + 7.0 * a.sin()];
            }
        };
        ring(&mut points, &RIGHT_EYE, [255.0, 280.0]);
        ring(&mut points, &LEFT_EYE, [345.0, 280.0]);
        points[33] = [235.0, 280.0];
        points[263] = [365.0, 280.0];
        for (k, &i) in RIGHT_LOWER_LID.iter().enumerate() {
            points[i] = [235.0 + k as f32 * 5.0, 285.0];
        }
        for (k, &i) in LEFT_LOWER_LID.iter().enumerate() {
            points[i] = [365.0 - k as f32 * 5.0, 285.0];
        }
        analysis.faces[0].landmarks = Some(points);
        let face = retouch_face(&image, &analysis, 0).unwrap();
        let under = face.under_eyes.unwrap();
        let at = |x: f32, y: f32| {
            let [u, v] = face.crop.invert().apply(x, y);
            under.get_pixel(u as u32, v as u32)[0]
        };
        assert!(at(255.0, 300.0) > 200, "below the right eye");
        assert!(at(255.0, 260.0) < 30, "above it");
        assert!(at(300.0, 380.0) < 30, "far below");
    }
}
