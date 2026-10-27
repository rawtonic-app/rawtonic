//! Face retouch (Capture One's Retouch Faces): blemishes, even skin and
//! texture, dark circles and shine, per face or for all faces.
//!
//! The recipe (`faceRetouch`) holds everything the render needs, made once
//! by the app from its face models (rawtonic-faces): per face a crop of
//! the photo (an affine map from crop pixels to the mask frame, the frame
//! spots and masks use), its skin and under-eye weights in that crop, the
//! eye distance, and the blemishes found with their scores. Rendering and
//! export therefore need no model. Faces are retouched in float on the base
//! image after spot removal and before every adjustment, in a square-root
//! encoding of the linear values, so later edits see the retouched skin.
//!
//! - **Blemishes** are healed without a source: within each spot the coarse
//!   layer (the blemish's scale) is replaced by the harmonic fill of its
//!   surroundings while the fine layer (pores) stays, so skin texture is
//!   kept and no neighbouring blemish is copied in. The slider takes the
//!   blemishes from the most prominent down.
//! - **Even skin** damps the middle frequencies of the skin (blotches,
//!   uneven tone and redness) and keeps large shading; **Texture** scales
//!   the fine layer (pores) up or down.
//! - **Dark circles** lift the under-eye area's coarse layer towards its
//!   surroundings and pull its colour towards theirs.
//! - **Shine** lowers specular highlights on the skin.
//!
//! - **Protect** (`protect`): areas the user paints keep their pixels from
//!   before the retouch (freckles, moles, beauty marks).
//!
//! Scales follow the face: the eye distance in base pixels.

use std::io::Cursor;

use base64::{Engine as _, engine::general_purpose};
use image::{GrayImage, Rgb32FImage};
use rayon::prelude::*;
use serde::Deserialize;
use serde_json::Value;

use crate::spot_removal::{mask_frame, to_base};

mod protect;
pub use protect::ProtectStroke;

/// Slider values, 0–100 (Texture −100–100); unset values of a face take
/// the values for all faces.
#[derive(Deserialize, Default, Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub impact: Option<f32>,
    pub blemishes: Option<f32>,
    pub even_skin: Option<f32>,
    pub texture: Option<f32>,
    pub dark_circles: Option<f32>,
    pub shine: Option<f32>,
    pub contouring: Option<f32>,
    /// Blemishes, even skin and texture on the neck too (below the chin).
    pub include_neck: Option<bool>,
}

/// Effective strengths: 0–1 (Texture −1–1), Impact already applied.
#[derive(Default, Clone, Copy, Debug, PartialEq)]
pub struct Amounts {
    pub blemishes: f32,
    pub even_skin: f32,
    pub texture: f32,
    pub dark_circles: f32,
    pub shine: f32,
    pub contouring: f32,
    pub include_neck: bool,
}

impl Settings {
    /// `self` (a face's own values) over `all` (the values for all faces).
    pub fn over(self, all: Settings) -> Settings {
        Settings {
            impact: self.impact.or(all.impact),
            blemishes: self.blemishes.or(all.blemishes),
            even_skin: self.even_skin.or(all.even_skin),
            texture: self.texture.or(all.texture),
            dark_circles: self.dark_circles.or(all.dark_circles),
            shine: self.shine.or(all.shine),
            contouring: self.contouring.or(all.contouring),
            include_neck: self.include_neck.or(all.include_neck),
        }
    }

    pub fn amounts(self) -> Amounts {
        let impact = self.impact.unwrap_or(100.0).clamp(0.0, 100.0) / 100.0;
        let unit = |v: Option<f32>| v.unwrap_or(0.0).clamp(0.0, 100.0) / 100.0 * impact;
        Amounts {
            blemishes: unit(self.blemishes),
            even_skin: unit(self.even_skin),
            texture: self.texture.unwrap_or(0.0).clamp(-100.0, 100.0) / 100.0 * impact,
            dark_circles: unit(self.dark_circles),
            shine: unit(self.shine),
            contouring: unit(self.contouring),
            include_neck: self.include_neck.unwrap_or(false),
        }
    }
}

impl Amounts {
    fn is_zero(&self) -> bool {
        self.blemishes <= 0.0
            && self.even_skin <= 0.0
            && self.texture == 0.0
            && self.dark_circles <= 0.0
            && self.shine <= 0.0
            && self.contouring <= 0.0
    }
}

/// A blemish found by the app, in mask-frame pixels.
#[derive(Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct Blemish {
    #[serde(default)]
    pub id: u32,
    pub x: f64,
    pub y: f64,
    pub r: f64,
    /// Prominence: how much it stands out (see `blemish_threshold`).
    pub score: f32,
    /// On the neck: healed only with `include_neck`.
    #[serde(default)]
    pub neck: bool,
}

#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct RetouchFace {
    /// Crop pixels to mask frame: x = a·u + b·v + c, y = d·u + e·v + f.
    pub crop: [f64; 6],
    /// Side of the crop in pixels (of the stored weights).
    pub size: u32,
    /// Distance between the eyes' outer corners, mask-frame pixels.
    pub eye_distance: f64,
    /// Skin weight in the crop (PNG data URL, `size`²): face skin without
    /// eyes, brows, lips, nostrils and hair.
    pub skin: String,
    /// Under-eye weight in the crop (PNG data URL); faces without
    /// landmarks have none.
    #[serde(default)]
    pub under_eyes: Option<String>,
    /// Neck skin below the chin in the crop (PNG data URL), if visible.
    #[serde(default)]
    pub neck: Option<String>,
    #[serde(default)]
    pub blemishes: Vec<Blemish>,
    /// Blemishes (ids) the user keeps.
    #[serde(default)]
    pub kept: Vec<u32>,
    /// Blemishes (ids) the user heals whatever the slider says.
    #[serde(default)]
    pub added: Vec<u32>,
    #[serde(default)]
    pub settings: Settings,
    #[serde(default = "enabled")]
    pub enabled: bool,
}

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct FaceRetouch {
    pub settings: Settings,
    pub faces: Vec<RetouchFace>,
    #[serde(default = "enabled")]
    pub enabled: bool,
    /// `frame_key` of the geometry the faces were found in. Faces of
    /// another geometry are not applied: their crops would be misplaced.
    pub frame: Option<String>,
    /// Set by the app for the quick preview while a control is dragged:
    /// crowded blemishes are healed once, not three times. Never stored.
    pub draft: bool,
    /// The protection brush's strokes (mask-frame pixels), for every face.
    #[serde(deserialize_with = "protect::deserialize_strokes")]
    pub protect: Vec<ProtectStroke>,
}

/// Geometry keys (cache_utils) that correct colour, not where a pixel is:
/// vignetting, lateral CA and edge fringes.
const COLOUR_ONLY_KEYS: [&str; 7] = [
    "lensVignetteAmount",
    "lensVignetteEnabled",
    "lensTcaAmount",
    "lensTcaEnabled",
    "autoChromaticAberration",
    "autoCaVersion",
    "fringeVersion",
];

/// The `frame` written since faces became stale only when they move.
const FRAME_VERSION_2: &str = "2:";

/// Identifies the mask frame the faces are found in: orientation, flips,
/// straightening and the geometry corrections that move pixels (cache_utils'
/// geometry keys without `COLOUR_ONLY_KEYS`), as canonical JSON after
/// `FRAME_VERSION_2`.
pub fn frame_key(adjustments: &Value) -> String {
    let placement = crate::cache_utils::GEOMETRY_KEYS
        .iter()
        .filter(|key| !COLOUR_ONLY_KEYS.contains(key));
    format!("{FRAME_VERSION_2}{}", keys_json(adjustments, placement))
}

/// The frame faces found before `FRAME_VERSION_2` carry: every geometry key,
/// so a vignetting or CA change still makes them stale (their recipes render
/// as before).
fn first_frame_key(adjustments: &Value) -> String {
    keys_json(adjustments, crate::cache_utils::GEOMETRY_KEYS.iter())
}

fn keys_json<'a>(adjustments: &Value, geometry: impl Iterator<Item = &'a &'a str>) -> String {
    let mut keys: std::collections::BTreeMap<&str, &Value> = std::collections::BTreeMap::new();
    for key in [
        "orientationSteps",
        "flipHorizontal",
        "flipVertical",
        "rotation",
    ]
    .iter()
    .chain(geometry)
    {
        if let Some(v) = adjustments.get(*key).filter(|v| !v.is_null()) {
            keys.insert(key, v);
        }
    }
    serde_json::to_string(&keys).unwrap_or_default()
}

/// Whether faces found in `frame` belong to the geometry of `adjustments`.
pub fn frame_matches(frame: &str, adjustments: &Value) -> bool {
    if frame.starts_with(FRAME_VERSION_2) {
        frame == frame_key(adjustments)
    } else {
        frame == first_frame_key(adjustments)
    }
}

fn enabled() -> bool {
    true
}

/// The recipe's face retouch, if it changes anything.
pub fn from_adjustments(adjustments: &Value) -> Option<FaceRetouch> {
    let value = adjustments.get("faceRetouch")?;
    let retouch: FaceRetouch = serde_json::from_value(value.clone()).ok()?;
    if let Some(frame) = &retouch.frame
        && !frame_matches(frame, adjustments)
    {
        log::warn!(
            "Face retouch skipped: its faces belong to another geometry ({frame} vs {})",
            frame_key(adjustments)
        );
        return None;
    }
    let active = retouch.enabled
        && retouch.faces.iter().any(|f| {
            f.enabled
                && (!f.settings.over(retouch.settings).amounts().is_zero() || !f.added.is_empty())
        });
    active.then_some(retouch)
}

/// The recipe for a reduced copy of the base image at `scale` (its pixels
/// per full-resolution pixel), such as a fit-view preview: the faces' crop
/// maps, eye distances and blemishes, all in full-resolution mask-frame
/// pixels, scaled; the geometry stays, so the frame key still matches.
pub fn scaled_recipe(adjustments: &Value, scale: f64) -> Value {
    let mut recipe = adjustments.clone();
    let Some(faces) = recipe
        .get_mut("faceRetouch")
        .and_then(|r| r.get_mut("faces"))
        .and_then(Value::as_array_mut)
    else {
        return recipe;
    };
    let scale_value = |item: &mut Value, key: &str| {
        if let Some(v) = item.get(key).and_then(Value::as_f64) {
            item[key] = serde_json::json!(v * scale);
        }
    };
    for face in faces {
        if let Some(crop) = face.get_mut("crop").and_then(Value::as_array_mut) {
            for v in crop.iter_mut() {
                if let Some(x) = v.as_f64() {
                    *v = serde_json::json!(x * scale);
                }
            }
        }
        scale_value(face, "eyeDistance");
        if let Some(blemishes) = face.get_mut("blemishes").and_then(Value::as_array_mut) {
            for b in blemishes {
                for key in ["x", "y", "r"] {
                    scale_value(b, key);
                }
            }
        }
    }
    if let Some(strokes) = recipe
        .get_mut("faceRetouch")
        .and_then(|r| r.get_mut("protect"))
        .and_then(Value::as_array_mut)
    {
        for stroke in strokes {
            scale_value(stroke, "radius");
            for point in stroke
                .get_mut("points")
                .and_then(Value::as_array_mut)
                .into_iter()
                .flatten()
            {
                scale_value(point, "x");
                scale_value(point, "y");
            }
        }
    }
    recipe
}

pub fn is_active(adjustments: &Value) -> bool {
    adjustments.get("faceRetouch").is_some_and(|v| !v.is_null())
        && from_adjustments(adjustments).is_some()
}

/// Least prominence of the blemishes the slider removes at `amount`
/// (0–1): from only the most prominent at low amounts to faint ones at
/// 100, as Capture One's Blemish slider starts with the larger ones.
pub fn blemish_threshold(amount: f32) -> f32 {
    if amount <= 0.0 {
        return f32::INFINITY;
    }
    // Prominence of rawtonic-faces' blemish finder: about 1 for a clear
    // pimple, 0.1 for a faint red dot, 0.03 for the faintest marks. Against
    // Capture One 16.6 on the same RAWs: its Blemish 60 clears faint dots
    // on clean skin, 30 most of them, 100 nearly all: the curve falls
    // fast at first (amount^0.25: 0.045 at 30, 0.025 at 60, 0.015 at 100).
    const STRONG: f32 = 1.0;
    const FAINT: f32 = 0.015;
    STRONG * (FAINT / STRONG).powf(amount.min(1.0).powf(0.25))
}

/// The blemishes `face` removes at `amount`: those the slider reaches and
/// those the user added, without the ones the user keeps; the neck's only
/// with `neck`.
pub fn selected_blemishes(face: &RetouchFace, amount: f32, neck: bool) -> Vec<Blemish> {
    let threshold = blemish_threshold(amount);
    face.blemishes
        .iter()
        .filter(|b| {
            (b.score >= threshold || face.added.contains(&b.id))
                && !face.kept.contains(&b.id)
                && (neck || !b.neck)
        })
        .copied()
        .collect()
}

fn decode_mask(data_url: &str) -> Option<GrayImage> {
    let b64 = data_url.split_once(',').map_or(data_url, |(_, d)| d);
    let bytes = general_purpose::STANDARD.decode(b64).ok()?;
    Some(
        image::ImageReader::new(Cursor::new(bytes))
            .with_guessed_format()
            .ok()?
            .decode()
            .ok()?
            .into_luma8(),
    )
}

/// A point and where it maps to.
type PointPair = ((f64, f64), (f64, f64));

/// An affine map x = a·u + b·v + c, y = d·u + e·v + f.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Affine([f64; 6]);

impl Affine {
    fn apply(&self, u: f64, v: f64) -> (f64, f64) {
        let m = self.0;
        (m[0] * u + m[1] * v + m[2], m[3] * u + m[4] * v + m[5])
    }

    fn invert(&self) -> Option<Affine> {
        let [a, b, c, d, e, f] = self.0;
        let det = a * e - b * d;
        if det.abs() < 1e-12 {
            return None;
        }
        let (ia, ib, id, ie) = (e / det, -b / det, -d / det, a / det);
        Some(Affine([
            ia,
            ib,
            -(ia * c + ib * f),
            id,
            ie,
            -(id * c + ie * f),
        ]))
    }

    fn scale(&self) -> f64 {
        let [a, b, _, d, e, _] = self.0;
        (a * e - b * d).abs().sqrt()
    }

    /// Least-squares affine map through point pairs (u, v) → (x, y).
    fn fit(pairs: &[PointPair]) -> Option<Affine> {
        // Normal equations for [u v 1] · [a b c]ᵀ = x (and the same for y).
        let mut ata = [[0.0f64; 3]; 3];
        let mut atx = [0.0f64; 3];
        let mut aty = [0.0f64; 3];
        for &((u, v), (x, y)) in pairs {
            let row = [u, v, 1.0];
            for i in 0..3 {
                for j in 0..3 {
                    ata[i][j] += row[i] * row[j];
                }
                atx[i] += row[i] * x;
                aty[i] += row[i] * y;
            }
        }
        let sx = solve3(ata, atx)?;
        let sy = solve3(ata, aty)?;
        Some(Affine([sx[0], sx[1], sx[2], sy[0], sy[1], sy[2]]))
    }
}

fn solve3(m: [[f64; 3]; 3], r: [f64; 3]) -> Option<[f64; 3]> {
    let det = |m: [[f64; 3]; 3]| {
        m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
            - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
            + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
    };
    let d = det(m);
    if d.abs() < 1e-12 {
        return None;
    }
    let mut out = [0.0; 3];
    for (k, o) in out.iter_mut().enumerate() {
        let mut mk = m;
        for i in 0..3 {
            mk[i][k] = r[i];
        }
        *o = det(mk) / d;
    }
    Some(out)
}

/// Working space: square root of the linear values (as spot healing).
fn encode(v: f32) -> f32 {
    v.max(0.0).sqrt()
}
fn decode(v: f32) -> f32 {
    let v = v.max(0.0);
    v * v
}

/// A float plane of a region.
#[derive(Clone)]
struct Plane {
    w: usize,
    h: usize,
    data: Vec<f32>,
}

impl Plane {
    fn at(&self, x: usize, y: usize) -> f32 {
        self.data[y * self.w + x]
    }

    fn bilinear(&self, x: f32, y: f32) -> f32 {
        let x = x.clamp(0.0, (self.w - 1) as f32);
        let y = y.clamp(0.0, (self.h - 1) as f32);
        let (x0, y0) = (x.floor() as usize, y.floor() as usize);
        let (x1, y1) = ((x0 + 1).min(self.w - 1), (y0 + 1).min(self.h - 1));
        let (fx, fy) = (x - x0 as f32, y - y0 as f32);
        let top = self.at(x0, y0) * (1.0 - fx) + self.at(x1, y0) * fx;
        let bottom = self.at(x0, y1) * (1.0 - fx) + self.at(x1, y1) * fx;
        top * (1.0 - fy) + bottom * fy
    }
}

/// One pass of a box mean of radius `r` along rows.
fn box_rows(src: &[f32], w: usize, r: usize) -> Vec<f32> {
    // A running window sum per row (no buffer per row): heals blur many
    // small boxes, where allocations and threads cost more than the sums.
    let row_pass = |o: &mut [f32], row: &[f32]| {
        let mut sum = 0.0f64;
        for &v in &row[..(r + 1).min(w)] {
            sum += v as f64;
        }
        for (x, value) in o.iter_mut().enumerate() {
            let lo = x.saturating_sub(r);
            let hi = (x + r + 1).min(w);
            *value = (sum / (hi - lo) as f64) as f32;
            if x + r + 1 < w {
                sum += row[x + r + 1] as f64;
            }
            if x >= r {
                sum -= row[x - r] as f64;
            }
        }
    };
    let mut out = vec![0.0f32; src.len()];
    if src.len() < SMALL_PLANE {
        for (o, row) in out.chunks_mut(w).zip(src.chunks(w)) {
            row_pass(o, row);
        }
    } else {
        out.par_chunks_mut(w)
            .zip(src.par_chunks(w))
            .for_each(|(o, row)| row_pass(o, row));
    }
    out
}

/// Planes smaller than this are filtered on one thread.
const SMALL_PLANE: usize = 16 * 1024;

fn transpose(src: &[f32], w: usize, h: usize) -> Vec<f32> {
    let mut out = vec![0.0f32; w * h];
    let column = |(x, col): (usize, &mut [f32])| {
        for (y, v) in col.iter_mut().enumerate() {
            *v = src[y * w + x];
        }
    };
    if src.len() < SMALL_PLANE {
        out.chunks_mut(h).enumerate().for_each(column);
    } else {
        out.par_chunks_mut(h).enumerate().for_each(column);
    }
    out
}

/// Gaussian blur of `sigma` pixels by three box passes per axis; large
/// sigmas run on a downscaled copy (they only carry low frequencies).
fn blur(p: &Plane, sigma: f32) -> Plane {
    if sigma < 0.5 {
        return p.clone();
    }
    let factor = ((sigma / 6.0).floor() as usize).max(1);
    if factor > 1 && p.w / factor >= 4 && p.h / factor >= 4 {
        let (sw, sh) = (p.w.div_ceil(factor), p.h.div_ceil(factor));
        let mut small = vec![0.0f32; sw * sh];
        small.par_chunks_mut(sw).enumerate().for_each(|(sy, row)| {
            for (sx, v) in row.iter_mut().enumerate() {
                let (mut acc, mut n) = (0.0f32, 0.0f32);
                for y in sy * factor..((sy + 1) * factor).min(p.h) {
                    for x in sx * factor..((sx + 1) * factor).min(p.w) {
                        acc += p.at(x, y);
                        n += 1.0;
                    }
                }
                *v = acc / n.max(1.0);
            }
        });
        let blurred = blur(
            &Plane {
                w: sw,
                h: sh,
                data: small,
            },
            sigma / factor as f32,
        );
        let mut data = vec![0.0f32; p.w * p.h];
        data.par_chunks_mut(p.w).enumerate().for_each(|(y, row)| {
            for (x, v) in row.iter_mut().enumerate() {
                *v = blurred.bilinear(
                    (x as f32 + 0.5) / factor as f32 - 0.5,
                    (y as f32 + 0.5) / factor as f32 - 0.5,
                );
            }
        });
        return Plane {
            w: p.w,
            h: p.h,
            data,
        };
    }
    if sigma < 2.5 {
        // Small blurs: an exact kernel (three boxes of a whole radius would
        // turn σ 0.8 into 1.4, and small faces' bands would shift).
        let radius = (3.0 * sigma).ceil() as usize;
        let kernel: Vec<f32> = (0..=2 * radius)
            .map(|i| (-((i as f32 - radius as f32).powi(2)) / (2.0 * sigma * sigma)).exp())
            .collect();
        let total: f32 = kernel.iter().sum();
        let kernel: Vec<f32> = kernel.iter().map(|k| k / total).collect();
        let rows = |src: &[f32], w: usize| -> Vec<f32> {
            let mut out = vec![0.0f32; src.len()];
            out.par_chunks_mut(w)
                .zip(src.par_chunks(w))
                .for_each(|(o, row)| {
                    for (x, v) in o.iter_mut().enumerate() {
                        *v = kernel
                            .iter()
                            .enumerate()
                            .map(|(i, k)| {
                                let at = (x + i).saturating_sub(radius).min(w - 1);
                                k * row[at]
                            })
                            .sum();
                    }
                });
            out
        };
        let data = rows(&p.data, p.w);
        let t = rows(&transpose(&data, p.w, p.h), p.h);
        return Plane {
            w: p.w,
            h: p.h,
            data: transpose(&t, p.h, p.w),
        };
    }
    // Three box passes of radius r give a Gaussian of σ² ≈ 3·r(r+1)/3.
    let r = (((sigma * sigma) + 0.25).sqrt() - 0.5).round().max(1.0) as usize;
    let mut data = p.data.clone();
    for _ in 0..3 {
        data = box_rows(&data, p.w, r);
    }
    let mut t = transpose(&data, p.w, p.h);
    for _ in 0..3 {
        t = box_rows(&t, p.h, r);
    }
    Plane {
        w: p.w,
        h: p.h,
        data: transpose(&t, p.h, p.w),
    }
}

fn smoothstep(lo: f32, hi: f32, x: f32) -> f32 {
    let t = ((x - lo) / (hi - lo)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Heals one blemish at base (`cx`, `cy`) of radius `r` without a source:
/// in the working space, the coarse layer inside the spot becomes the
/// harmonic fill of the coarse layer on a ring around it, the fine layer
/// stays.
/// A healed blemish: its box and the new working-space values in it.
struct HealPatch {
    x0: usize,
    y0: usize,
    w: usize,
    h: usize,
    values: Vec<Plane>,
}

/// The box a blemish's heal reads (x0, y0, x1, y1).
fn heal_box(dims: (u32, u32), cx: f64, cy: f64, r: f64) -> (usize, usize, usize, usize) {
    let ring = 1.6 * r;
    let sigma = (0.3 * r).max(0.6);
    let pad = ring + 3.0 * sigma + 2.0;
    (
        (cx - pad).floor().max(0.0) as usize,
        (cy - pad).floor().max(0.0) as usize,
        ((cx + pad).ceil() as usize).min(dims.0 as usize),
        ((cy + pad).ceil() as usize).min(dims.1 as usize),
    )
}

/// Heals blemishes (base centre, radius). A heal fills from a ring around
/// the spot; in a dense field (acne, pigment) that ring lies on the
/// neighbours, and a fill from their darkness leaves a mottled patch. So
/// the spots whose ring touches another are healed again, from rings over
/// their healed neighbours.
fn heal_blemishes(image: &mut Rgb32FImage, spots: &[(f64, f64, f64)], draft: bool) {
    heal_in_waves(image, spots);
    if draft {
        return;
    }
    let crowded: Vec<(f64, f64, f64)> = spots
        .iter()
        .enumerate()
        .filter(|&(i, &(x, y, r))| {
            spots.iter().enumerate().any(|(j, &(ox, oy, or))| {
                j != i && ((x - ox).powi(2) + (y - oy).powi(2)).sqrt() < 1.6 * r + 1.1 * or
            })
        })
        .map(|(_, &s)| s)
        .collect();
    // Twice more: each pass reads rings over neighbours healed better by
    // the one before (a field of spots at 40 % cover settles within 5 % of
    // the skin after three passes, 7 % after two).
    for _ in 0..2 {
        heal_in_waves(image, &crowded);
    }
}

/// Heals the spots: those whose boxes do not overlap in parallel, in
/// waves, so each heal sees the ones before it.
fn heal_in_waves(image: &mut Rgb32FImage, spots: &[(f64, f64, f64)]) {
    let dims = image.dimensions();
    let mut waves: Vec<Vec<usize>> = Vec::new();
    let mut boxes_of_wave: Vec<Vec<(usize, usize, usize, usize)>> = Vec::new();
    for (i, &(cx, cy, r)) in spots.iter().enumerate() {
        let b = heal_box(dims, cx, cy, r);
        let apart =
            |o: &(usize, usize, usize, usize)| b.2 <= o.0 || o.2 <= b.0 || b.3 <= o.1 || o.3 <= b.1;
        match boxes_of_wave
            .iter()
            .position(|boxes| boxes.iter().all(apart))
        {
            Some(w) => {
                waves[w].push(i);
                boxes_of_wave[w].push(b);
            }
            None => {
                waves.push(vec![i]);
                boxes_of_wave.push(vec![b]);
            }
        }
    }
    for wave in waves {
        let patches: Vec<HealPatch> = wave
            .par_iter()
            .filter_map(|&i| {
                let (cx, cy, r) = spots[i];
                heal_blemish(image, cx, cy, r)
            })
            .collect();
        for p in patches {
            for y in 0..p.h {
                for x in 0..p.w {
                    let k = y * p.w + x;
                    image.put_pixel(
                        (p.x0 + x) as u32,
                        (p.y0 + y) as u32,
                        image::Rgb(std::array::from_fn(|c| decode(p.values[c].data[k]))),
                    );
                }
            }
        }
    }
}

fn heal_blemish(image: &Rgb32FImage, cx: f64, cy: f64, r: f64) -> Option<HealPatch> {
    let ring = 1.6 * r;
    let sigma = (0.3 * r).max(0.6);
    let (x0, y0, x1, y1) = heal_box(image.dimensions(), cx, cy, r);
    if x1 <= x0 + 2 || y1 <= y0 + 2 {
        return None;
    }
    let (bw, bh) = (x1 - x0, y1 - y0);
    let mut channels: Vec<Plane> = (0..3)
        .map(|c| Plane {
            w: bw,
            h: bh,
            data: (0..bw * bh)
                .map(|k| encode(image.get_pixel((x0 + k % bw) as u32, (y0 + k / bw) as u32)[c]))
                .collect(),
        })
        .collect();
    let coarse: Vec<Plane> = channels.iter().map(|p| blur(p, sigma as f32)).collect();
    let (lcx, lcy) = ((cx - x0 as f64) as f32, (cy - y0 as f64) as f32);
    let count = ((std::f64::consts::TAU * ring / 2.0).round() as usize).clamp(24, 256);
    let boundary: Vec<[f32; 3]> = (0..count)
        .map(|k| {
            let phi = std::f64::consts::TAU * k as f64 / count as f64;
            let (x, y) = (
                lcx + (ring * phi.cos()) as f32 - 0.5,
                lcy + (ring * phi.sin()) as f32 - 0.5,
            );
            std::array::from_fn(|c| coarse[c].bilinear(x, y))
        })
        .collect();
    let ring = ring as f32;
    // The harmonic fill of the disc from its boundary as a Fourier series
    // (the Poisson integral's own expansion): a few modes suffice, the
    // coarse layer is smooth.
    let modes = (count / 2).min(16);
    let coefficients: Vec<Vec<(f32, f32)>> = (0..3)
        .map(|c| {
            (0..=modes)
                .map(|n| {
                    let (mut a, mut b) = (0.0f32, 0.0f32);
                    for (k, value) in boundary.iter().enumerate() {
                        let phi = std::f32::consts::TAU * (n * k) as f32 / count as f32;
                        a += value[c] * phi.cos();
                        b += value[c] * phi.sin();
                    }
                    let scale = if n == 0 { 1.0 } else { 2.0 } / count as f32;
                    (a * scale, b * scale)
                })
                .collect()
        })
        .collect();
    // The fine layer's level on the ring: inside, the blemish's own fine
    // structure (its rim, a pustule's head) is held to it, so no ghost of
    // the blemish stays.
    let fine_level: Vec<f32> = (0..3)
        .map(|c| {
            let mut sum = 0.0f32;
            for k in 0..count {
                let phi = std::f64::consts::TAU * k as f64 / count as f64;
                let x = (lcx + ring * phi.cos() as f32).clamp(0.0, (bw - 1) as f32) as usize;
                let y = (lcy + ring * phi.sin() as f32).clamp(0.0, (bh - 1) as f32) as usize;
                let f = channels[c].at(x, y) - coarse[c].at(x, y);
                sum += f * f;
            }
            (sum / count as f32).sqrt()
        })
        .collect();
    for (c, plane) in channels.iter_mut().enumerate() {
        let coefficients = &coefficients[c];
        let limit = 2.0 * fine_level[c];
        let fill_row = |(y, row): (usize, &mut [f32])| {
            for (x, v) in row.iter_mut().enumerate() {
                let (dx, dy) = (x as f32 + 0.5 - lcx, y as f32 + 0.5 - lcy);
                let rho = (dx * dx + dy * dy).sqrt();
                if rho >= ring {
                    continue;
                }
                let t = rho / ring;
                let (cos1, sin1) = if rho > 1e-6 {
                    (dx / rho, dy / rho)
                } else {
                    (1.0, 0.0)
                };
                let (mut cos_n, mut sin_n, mut t_n) = (1.0f32, 0.0f32, 1.0f32);
                let mut fill = coefficients[0].0;
                for &(a, b) in &coefficients[1..] {
                    let next_cos = cos_n * cos1 - sin_n * sin1;
                    sin_n = sin_n * cos1 + cos_n * sin1;
                    cos_n = next_cos;
                    t_n *= t;
                    fill += t_n * (a * cos_n + b * sin_n);
                }
                let coarse_here = coarse[c].at(x, y);
                // Full inside the blemish, fading out to the ring.
                let alpha = 1.0 - smoothstep(r as f32 * 1.1, ring, rho);
                let fine = *v - coarse_here;
                let held = fine.clamp(-limit, limit);
                *v = coarse_here + fine + alpha * (fill - coarse_here + held - fine);
            }
        };
        if plane.data.len() < SMALL_PLANE {
            plane.data.chunks_mut(bw).enumerate().for_each(fill_row);
        } else {
            plane.data.par_chunks_mut(bw).enumerate().for_each(fill_row);
        }
    }
    Some(HealPatch {
        x0,
        y0,
        w: bw,
        h: bh,
        values: channels,
    })
}

/// The face's crop → base-image map: the stored crop → mask-frame map
/// followed by the geometry back to base pixels, fitted on a 5 × 5 grid.
fn crop_to_base(face: &RetouchFace, adjustments: &Value, frame: (f64, f64)) -> Option<Affine> {
    let crop = Affine(face.crop);
    let n = face.size as f64;
    let mut pairs = Vec::with_capacity(25);
    for j in 0..5 {
        for i in 0..5 {
            let (u, v) = (n * i as f64 / 4.0, n * j as f64 / 4.0);
            let (mx, my) = crop.apply(u, v);
            pairs.push(((u, v), to_base(adjustments, frame, mx, my)));
        }
    }
    Affine::fit(&pairs)
}

/// Share of the red blotches (middle band) and of the broad redness above
/// the surroundings Blemishes 100 takes, rising as amount^0.7; fitted to
/// Capture One 16.6's Blemish on the same RAWs (mean log(R/G) of the face's
/// skin at 30/60/100; notes: reference/face-retouch.md).
const RED_MID: f32 = 0.21;
const RED_BROAD: f32 = 0.1;

/// Even skin, texture, dark circles and shine on one face.
fn retouch_skin(
    image: &mut Rgb32FImage,
    face: &RetouchFace,
    to_base: Affine,
    eye_distance: f32,
    amounts: Amounts,
) {
    if amounts.blemishes <= 0.0
        && amounts.even_skin <= 0.0
        && amounts.texture == 0.0
        && amounts.dark_circles <= 0.0
        && amounts.shine <= 0.0
        && amounts.contouring <= 0.0
    {
        return;
    }
    let Some(skin) = decode_mask(&face.skin) else {
        return;
    };
    let under_eyes = face.under_eyes.as_deref().and_then(decode_mask);
    let neck = if amounts.include_neck {
        face.neck.as_deref().and_then(decode_mask)
    } else {
        None
    };
    let Some(to_crop) = to_base.invert() else {
        return;
    };
    let (w, h) = image.dimensions();
    let n = face.size as f64;
    let corners = [(0.0, 0.0), (n, 0.0), (0.0, n), (n, n)].map(|(u, v)| to_base.apply(u, v));
    let x0 = corners
        .iter()
        .map(|c| c.0)
        .fold(f64::MAX, f64::min)
        .floor()
        .max(0.0) as usize;
    let y0 = corners
        .iter()
        .map(|c| c.1)
        .fold(f64::MAX, f64::min)
        .floor()
        .max(0.0) as usize;
    let x1 = (corners.iter().map(|c| c.0).fold(f64::MIN, f64::max).ceil() as usize).min(w as usize);
    let y1 = (corners.iter().map(|c| c.1).fold(f64::MIN, f64::max).ceil() as usize).min(h as usize);
    if x1 <= x0 + 4 || y1 <= y0 + 4 {
        return;
    }
    let (rw, rh) = (x1 - x0, y1 - y0);
    let weight = |mask: &GrayImage, x: usize, y: usize| -> f32 {
        let (u, v) = to_crop.apply((x0 + x) as f64 + 0.5, (y0 + y) as f64 + 0.5);
        let (mw, mh) = mask.dimensions();
        let (u, v) = (
            (u * mw as f64 / n - 0.5) as f32,
            (v * mh as f64 / n - 0.5) as f32,
        );
        if u < -0.5 || v < -0.5 || u > mw as f32 - 0.5 || v > mh as f32 - 0.5 {
            return 0.0;
        }
        let (u, v) = (u.clamp(0.0, (mw - 1) as f32), v.clamp(0.0, (mh - 1) as f32));
        let (ux, vy) = (u.floor() as u32, v.floor() as u32);
        let (ux1, vy1) = ((ux + 1).min(mw - 1), (vy + 1).min(mh - 1));
        let (fu, fv) = (u - ux as f32, v - vy as f32);
        let p = |a: u32, b: u32| mask.get_pixel(a, b)[0] as f32 / 255.0;
        let top = p(ux, vy) * (1.0 - fu) + p(ux1, vy) * fu;
        let bottom = p(ux, vy1) * (1.0 - fu) + p(ux1, vy1) * fu;
        // Faded out over the crop's outer 4 %: skin reaching the crop's
        // edge (a face larger than its box) leaves no seam there.
        let edge = u.min(v).min(mw as f32 - 1.0 - u).min(mh as f32 - 1.0 - v);
        (top * (1.0 - fv) + bottom * fv) * smoothstep(0.0, 0.04 * mw as f32, edge)
    };
    let skin_w: Vec<f32> = (0..rw * rh)
        .into_par_iter()
        .map(|k| weight(&skin, k % rw, k / rw))
        .collect();
    // The neck counts as skin for blemishes, even skin and texture (not for
    // contouring or shine: those shape the face).
    let neck_w: Vec<f32> = match &neck {
        Some(m) => (0..rw * rh)
            .into_par_iter()
            .map(|k| weight(m, k % rw, k / rw))
            .collect(),
        None => Vec::new(),
    };
    let eye_w: Vec<f32> = match &under_eyes {
        Some(m) => (0..rw * rh)
            .into_par_iter()
            .map(|k| weight(m, k % rw, k / rw))
            .collect(),
        None => vec![0.0; rw * rh],
    };
    if skin_w.iter().all(|&v| v <= 0.0)
        && eye_w.iter().all(|&v| v <= 0.0)
        && neck_w.iter().all(|&v| v <= 0.0)
    {
        return;
    }

    // Luminance and chromaticity in the working space.
    let pixels: Vec<[f32; 3]> = (0..rw * rh)
        .into_par_iter()
        .map(|k| {
            image
                .get_pixel((x0 + k % rw) as u32, (y0 + k / rw) as u32)
                .0
        })
        .collect();
    let lum = |p: &[f32; 3]| 0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2];
    let y = Plane {
        w: rw,
        h: rh,
        data: pixels.par_iter().map(|p| encode(lum(p))).collect(),
    };
    // Chromaticity: each channel over the luminance.
    let chroma: Vec<Plane> = (0..3)
        .map(|c| Plane {
            w: rw,
            h: rh,
            data: pixels
                .par_iter()
                .map(|p| {
                    let l = lum(p);
                    if l > 1e-6 { p[c] / l } else { 1.0 }
                })
                .collect(),
        })
        .collect();

    let d = eye_distance.max(8.0);
    let fine_sigma = 0.010 * d;
    let mid_sigma = 0.045 * d;
    let specks = smoothstep(0.0, 0.3, amounts.blemishes);
    let plane = |data: Vec<f32>| Plane { w: rw, h: rh, data };
    // Hairs (stubble, a beard's edge, lashes) are lines, specks are dots:
    // where the local gradients share one direction (structure tensor) the
    // specks pass stays off. The tensor's products, blurred below.
    let tensor_terms: Vec<Plane> = if specks > 0.0 {
        let gx: Vec<f32> = (0..rw * rh)
            .into_par_iter()
            .map(|k| {
                let (x, yy) = (k % rw, k / rw);
                (y.at((x + 1).min(rw - 1), yy) - y.at(x.saturating_sub(1), yy)) / 2.0
            })
            .collect();
        let gy: Vec<f32> = (0..rw * rh)
            .into_par_iter()
            .map(|k| {
                let (x, yy) = (k % rw, k / rw);
                (y.at(x, (yy + 1).min(rh - 1)) - y.at(x, yy.saturating_sub(1))) / 2.0
            })
            .collect();
        vec![
            plane(gx.iter().map(|g| g * g).collect()),
            plane(gx.iter().zip(&gy).map(|(a, b)| a * b).collect()),
            plane(gy.iter().map(|g| g * g).collect()),
        ]
    } else {
        Vec::new()
    };
    // The blurs that need nothing but the image, at once (each blur alone
    // keeps few cores busy on a face-sized region). Chromaticity's green
    // ratio follows from the red and blue ones (the three weigh up to the
    // luminance), so only those two are blurred.
    let tensor_sigma = (0.006 * d).max(1.5);
    let mut jobs: Vec<(&Plane, f32)> = vec![(&y, fine_sigma), (&y, mid_sigma), (&y, 0.25 * d)];
    for c in [0, 2] {
        for sigma in [fine_sigma * 2.0, mid_sigma, 0.25 * d] {
            jobs.push((&chroma[c], sigma));
        }
    }
    jobs.extend(tensor_terms.iter().map(|t| (t, tensor_sigma)));
    let mut blurred: Vec<Plane> = jobs.par_iter().map(|(p, s)| blur(p, *s)).collect();
    let tensor_blurred = blurred.split_off(9);
    let mut blurred = blurred.into_iter();
    let (g1, g2, wide) = (
        blurred.next().unwrap(),
        blurred.next().unwrap(),
        blurred.next().unwrap(),
    );
    let red: Vec<Plane> = blurred.by_ref().take(3).collect();
    let blue: Vec<Plane> = blurred.take(3).collect();
    let green_of = |r: &Plane, b: &Plane| {
        plane(
            r.data
                .iter()
                .zip(&b.data)
                .map(|(r, b)| (1.0 - 0.2126 * r - 0.0722 * b) / 0.7152)
                .collect(),
        )
    };
    let mut chroma_g1 = Vec::with_capacity(3);
    let mut chroma_g2 = Vec::with_capacity(3);
    let mut chroma_wide = Vec::with_capacity(3);
    for (i, (r, b)) in red.into_iter().zip(blue).enumerate() {
        let g = green_of(&r, &b);
        match i {
            0 => chroma_g1.extend([r, g, b]),
            1 => chroma_g2.extend([r, g, b]),
            _ => chroma_wide.extend([r, g, b]),
        }
    }
    let lines: Vec<f32> = if specks > 0.0 {
        let (jxx, jxy, jyy) = (&tensor_blurred[0], &tensor_blurred[1], &tensor_blurred[2]);
        (0..rw * rh)
            .into_par_iter()
            .map(|k| {
                let (a, b, c) = (jxx.data[k], jxy.data[k], jyy.data[k]);
                let coherence = ((a - c).powi(2) + 4.0 * b * b).sqrt() / (a + c).max(1e-12);
                smoothstep(0.35, 0.65, coherence)
            })
            .collect()
    } else {
        Vec::new()
    };
    // Local amplitude of the middle band (RMS around each pixel): blotches
    // are faint, the shading that forms the face is strong.
    // Local level of the fine layer (RMS): the skin's own pores. Dark
    // specks far below it are what the Blemishes slider clears beyond the
    // blemishes it heals one by one (Capture One's Blemish takes such dots
    // from about 30; here the pores stay).
    // Red specks (small pimples, broken capillaries): the red ratio's
    // outliers above the skin's own level of red variation, as for dark
    // specks.
    // Pores and red: twice, the second time with every value capped at
    // twice the first level, so the specks themselves do not raise it.
    let mid_sq = plane(
        g1.data
            .iter()
            .zip(&g2.data)
            .map(|(a, b)| (a - b) * (a - b))
            .collect(),
    );
    let fine_sq = plane(
        y.data
            .iter()
            .zip(&g1.data)
            .map(|(v, a)| (v - a) * (v - a))
            .collect(),
    );
    let red_sq = plane(if specks > 0.0 {
        chroma[0]
            .data
            .iter()
            .zip(&chroma_g1[0].data)
            .map(|(v, a)| (v - a) * (v - a))
            .collect()
    } else {
        Vec::new()
    });
    let mut second: Vec<(&Plane, f32)> = vec![(&mid_sq, mid_sigma), (&fine_sq, 0.04 * d)];
    if specks > 0.0 {
        second.push((&red_sq, 0.04 * d));
    }
    let mut firsts = second
        .par_iter()
        .map(|(p, s)| blur(p, *s))
        .collect::<Vec<_>>()
        .into_iter();
    let mid_power = firsts.next().unwrap();
    let fine_first = firsts.next().unwrap();
    let red_first = firsts.next();
    let capped = |sq: &Plane, first: &Plane| {
        plane(
            sq.data
                .iter()
                .zip(&first.data)
                .map(|(f, p)| f.min(4.0 * p.max(0.0)))
                .collect(),
        )
    };
    let fine_capped = capped(&fine_sq, &fine_first);
    let red_capped = red_first.as_ref().map(|first| capped(&red_sq, first));
    let mut third: Vec<&Plane> = vec![&fine_capped];
    third.extend(red_capped.as_ref());
    let mut powers = third
        .par_iter()
        .map(|p| blur(p, 0.04 * d))
        .collect::<Vec<_>>()
        .into_iter();
    let fine_power = powers.next().unwrap();
    let red_power = powers.next().unwrap_or(Plane {
        w: 0,
        h: 0,
        data: Vec::new(),
    });
    // How far below the pores' level a speck has to be: 2.5 times it up to
    // the slider's middle (pores stay whole), down to once it at 100 (the
    // darkest parts of the pores go too, a clean skin as Capture One's 100).
    let speck_floor = 2.5 - 1.5 * smoothstep(0.5, 1.0, amounts.blemishes);

    let out: Vec<Option<[f32; 3]>> = (0..rw * rh)
        .into_par_iter()
        .map(|k| {
            // Face skin (shaping tools) and all skin (smoothing tools).
            let wf = skin_w[k];
            let ws = neck_w.get(k).map_or(wf, |&n| wf.max(n));
            let we = eye_w[k];
            if ws <= 0.0 && we <= 0.0 {
                return None;
            }
            let (yv, a, b) = (y.data[k], g1.data[k], g2.data[k]);
            let fine = yv - a;
            let mid = a - b;
            // Even skin: damp blotches (middle frequencies of small
            // amplitude, about ±3 % of the local level); the shading that
            // gives the face its form (nose wings, lids, cheekbones) has
            // more amplitude and stays.
            let tau = 0.03 * b.max(1e-4);
            let envelope = mid_power.data[k].max(0.0).sqrt();
            let small = 1.0 / (1.0 + (envelope / tau).powi(4));
            let even = amounts.even_skin * ws;
            let mut mid_out = mid * (1.0 - even * small);
            // Specks: dark outliers of the fine layer raised to
            // `speck_floor` times its local level.
            let floor = -speck_floor * fine_power.data[k].max(0.0).sqrt();
            let raised = if specks > 0.0 && fine < floor {
                specks * ws * (1.0 - lines[k]) * (floor - fine)
            } else {
                0.0
            };
            let mut speck_share = if fine < 0.0 { raised / -fine } else { 0.0 };
            if specks > 0.0 {
                let excess = chroma[0].data[k] - chroma_g1[0].data[k];
                let ceiling = speck_floor * red_power.data[k].max(0.0).sqrt();
                if excess > ceiling && excess > 0.0 {
                    let pulled = specks * ws * (1.0 - lines[k]) * (excess - ceiling) / excess;
                    speck_share = speck_share.max(pulled);
                }
            }
            // Texture: the fine layer up or down.
            let fine_out = (fine + raised) * (1.0 + amounts.texture * ws).max(0.0);
            // Shine: highlights standing out from the coarse level.
            let excess = (a - wide.data[k] - 0.04 * wide.data[k]).max(0.0);
            mid_out -= amounts.shine * wf * excess * 0.9;
            // Dark circles: the under-eye coarse layer towards the wide
            // surroundings.
            let lift = (wide.data[k] - b).max(0.0) * amounts.dark_circles * we;
            // Contouring (Capture One's): the shadows that give the face
            // its volume (cheek hollows, jaw, temples: the coarse layer
            // below its wide surroundings) a little deeper; highlights and
            // the under-eye area untouched.
            let deepen = (wide.data[k] - b).max(0.0) * 0.6 * amounts.contouring * wf * (1.0 - we);
            let y_out = b + lift - deepen + mid_out + fine_out;
            // Damping the middle band lowers the linear mean (the mean of
            // squares loses the band's power): put that power back, so even
            // skin does not darken the face.
            let lost = envelope * envelope * (1.0 - (1.0 - even * small).powi(2));
            let y_out = (y_out * y_out + lost).max(0.0).sqrt() * y_out.signum().max(0.0);

            // Redness (Capture One's Blemish takes it along): red blotches
            // of the middle band and redness above the wide surroundings'
            // are pulled towards the colour around them.
            let red_amount = amounts.blemishes.powf(0.7) * ws;
            let red_mid = if chroma_g1[0].data[k] > chroma_g2[0].data[k] {
                (RED_MID * red_amount).min(1.0 - 0.8 * even * small)
            } else {
                0.0
            };
            let red_broad = if chroma_g2[0].data[k] > chroma_wide[0].data[k] {
                RED_BROAD * red_amount
            } else {
                0.0
            };
            let mut rgb = [0.0f32; 3];
            for (c, out) in rgb.iter_mut().enumerate() {
                let cv = chroma[c].data[k];
                let c1 = chroma_g1[c].data[k];
                let c2 = chroma_g2[c].data[k];
                // Uneven colour (redness) damped like the tone, where the
                // tone says blotch rather than form.
                let mut cc = cv - (0.8 * even * small + red_mid) * (c1 - c2);
                // Broad redness above the face's own colour.
                cc -= red_broad * (c2 - chroma_wide[c].data[k]);
                // A raised speck takes the colour around it too.
                cc += speck_share * (c1 - cv);
                // Under the eyes, colour towards the surroundings'.
                cc += 0.7 * amounts.dark_circles * we * (chroma_wide[c].data[k] - c2);
                // Shine: highlights get back some of the skin's colour.
                cc += 0.5 * amounts.shine * wf * smoothstep(0.0, 0.1, excess) * (c2 - cv);
                *out = cc.max(0.0) * decode(y_out);
            }
            Some(rgb)
        })
        .collect();
    for (k, value) in out.into_iter().enumerate() {
        if let Some(rgb) = value {
            image.put_pixel((x0 + k % rw) as u32, (y0 + k / rw) as u32, image::Rgb(rgb));
        }
    }
}

/// Applies the recipe's face retouch to the base image (float RGB, after
/// spot removal). Returns whether anything changed.
pub fn apply_face_retouch(image: &mut Rgb32FImage, adjustments: &Value) -> bool {
    let Some(retouch) = from_adjustments(adjustments) else {
        return false;
    };
    let frame = mask_frame(adjustments, image.width(), image.height());
    let protection = protect::Protection::new(image, &retouch.protect, adjustments, frame);
    let mut changed = false;
    for face in retouch.faces.iter().filter(|f| f.enabled) {
        let amounts = face.settings.over(retouch.settings).amounts();
        if amounts.is_zero() && face.added.is_empty() {
            continue;
        }
        let Some(to_base_map) = crop_to_base(face, adjustments, frame) else {
            continue;
        };
        let crop_scale = Affine(face.crop).scale();
        let base_per_mask = if crop_scale > 0.0 {
            to_base_map.scale() / crop_scale
        } else {
            1.0
        };
        let spots: Vec<(f64, f64, f64)> =
            selected_blemishes(face, amounts.blemishes, amounts.include_neck)
                .iter()
                .map(|b| {
                    let (bx, by) = to_base(adjustments, frame, b.x, b.y);
                    (bx, by, b.r * base_per_mask)
                })
                .collect();
        heal_blemishes(image, &spots, retouch.draft);
        retouch_skin(
            image,
            face,
            to_base_map,
            (face.eye_distance * base_per_mask) as f32,
            amounts,
        );
        changed = true;
    }
    if changed && let Some(protection) = protection {
        protection.restore(image);
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{DynamicImage, Luma};
    use serde_json::json;

    fn png_data_url(mask: &GrayImage) -> String {
        let mut png = Cursor::new(Vec::new());
        DynamicImage::ImageLuma8(mask.clone())
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        format!(
            "data:image/png;base64,{}",
            general_purpose::STANDARD.encode(png.into_inner())
        )
    }

    /// Skin with a fine, deterministic texture (pores) at 0.2 linear.
    fn skin_image(w: u32, h: u32) -> Rgb32FImage {
        Rgb32FImage::from_fn(w, h, |x, y| {
            let t = (((x * 7 + y * 13) % 5) as f32 - 2.0) * 0.004;
            image::Rgb([0.24 + t, 0.18 + t, 0.15 + t])
        })
    }

    fn face(blemishes: Vec<Blemish>, settings: Settings) -> RetouchFace {
        RetouchFace {
            // A 100 px crop covering the photo's top-left 200 × 200 pixels.
            crop: [2.0, 0.0, 0.0, 0.0, 2.0, 0.0],
            size: 100,
            eye_distance: 80.0,
            skin: png_data_url(&GrayImage::from_pixel(100, 100, Luma([255]))),
            under_eyes: None,
            neck: None,
            blemishes,
            kept: vec![],
            added: vec![],
            settings,
            enabled: true,
        }
    }

    fn recipe(faces: Vec<RetouchFace>, settings: Value) -> Value {
        let faces: Vec<Value> = faces
            .iter()
            .map(|f| {
                json!({
                    "crop": f.crop, "size": f.size, "eyeDistance": f.eye_distance,
                    "skin": f.skin, "underEyes": f.under_eyes, "neck": f.neck,
                    "blemishes": f.blemishes.iter().map(|b| json!({"id": b.id, "x": b.x, "y": b.y, "r": b.r, "score": b.score, "neck": b.neck})).collect::<Vec<_>>(),
                    "kept": f.kept,
                })
            })
            .collect();
        json!({"faceRetouch": {"settings": settings, "faces": faces}})
    }

    #[test]
    fn settings_of_a_face_override_all_faces_and_impact_scales() {
        let all = Settings {
            impact: Some(50.0),
            blemishes: Some(40.0),
            even_skin: Some(60.0),
            ..Default::default()
        };
        let own = Settings {
            even_skin: Some(20.0),
            ..Default::default()
        };
        let a = own.over(all).amounts();
        assert!((a.blemishes - 0.2).abs() < 1e-6);
        assert!((a.even_skin - 0.1).abs() < 1e-6);
        assert_eq!(a.texture, 0.0);
    }

    #[test]
    fn blemish_slider_takes_the_most_prominent_first() {
        assert!(blemish_threshold(0.0).is_infinite());
        assert!(blemish_threshold(0.2) > blemish_threshold(0.6));
        assert!((blemish_threshold(1.0) - 0.015).abs() < 1e-6);
        let mut f = face(
            vec![
                Blemish {
                    id: 1,
                    x: 50.0,
                    y: 50.0,
                    r: 4.0,
                    score: 0.9,
                    neck: false,
                },
                Blemish {
                    id: 2,
                    x: 80.0,
                    y: 50.0,
                    r: 4.0,
                    score: 0.02,
                    neck: false,
                },
            ],
            Settings::default(),
        );
        assert_eq!(selected_blemishes(&f, 0.2, false).len(), 1);
        assert_eq!(selected_blemishes(&f, 1.0, false).len(), 2);
        // A kept blemish stays.
        f.kept = vec![1];
        assert_eq!(
            selected_blemishes(&f, 1.0, false)
                .iter()
                .map(|b| b.id)
                .collect::<Vec<_>>(),
            vec![2]
        );
        // An added one is healed below the slider's reach.
        f.kept = vec![];
        f.added = vec![2];
        assert_eq!(selected_blemishes(&f, 0.0, false).len(), 1);
    }

    #[test]
    fn faces_of_another_geometry_are_not_applied() {
        let mut r = recipe(
            vec![face(vec![], Settings::default())],
            json!({"evenSkin": 30}),
        );
        r["rotation"] = json!(2.0);
        r["faceRetouch"]["frame"] = json!(frame_key(&r));
        assert!(is_active(&r));
        r["rotation"] = json!(3.5);
        assert!(!is_active(&r));
        // Exposure is not geometry.
        r["rotation"] = json!(2.0);
        r["exposure"] = json!(1.0);
        assert!(is_active(&r));
        // Nor are vignetting, CA and fringe corrections: the faces stay.
        for (key, value) in [
            ("lensVignetteAmount", json!(40)),
            ("lensTcaAmount", json!(80)),
            ("autoCaVersion", json!(3)),
            ("fringeVersion", json!(1)),
        ] {
            let mut changed = r.clone();
            changed[key] = value;
            assert!(is_active(&changed), "{key}");
        }
        r["lensDistortionAmount"] = json!(60);
        assert!(!is_active(&r));
    }

    #[test]
    fn faces_found_before_frame_version_2_keep_their_rule() {
        // Their frame lists every geometry key, colour corrections included:
        // they render as before.
        let mut r = recipe(
            vec![face(vec![], Settings::default())],
            json!({"evenSkin": 30}),
        );
        r["fringeVersion"] = json!(1);
        r["faceRetouch"]["frame"] = json!(first_frame_key(&r));
        assert!(is_active(&r));
        r["fringeVersion"] = json!(2);
        assert!(!is_active(&r));
    }

    #[test]
    fn inactive_without_amounts() {
        let r = recipe(vec![face(vec![], Settings::default())], json!({}));
        assert!(!is_active(&r));
        assert!(!is_active(&json!({})));
        let r = recipe(
            vec![face(vec![], Settings::default())],
            json!({"evenSkin": 30}),
        );
        assert!(is_active(&r));
        let r = recipe(
            vec![face(vec![], Settings::default())],
            json!({"evenSkin": 30, "impact": 0}),
        );
        assert!(!is_active(&r));
    }

    #[test]
    fn a_blemish_heals_and_the_skin_texture_stays() {
        let mut image = skin_image(200, 200);
        let original = image.clone();
        // A dark, reddish spot of radius 5 at (100, 100), over the pores
        // (they go on through it).
        for (x, y, p) in image.enumerate_pixels_mut() {
            let d = ((x as f32 - 100.0).powi(2) + (y as f32 - 100.0).powi(2)).sqrt();
            if d < 5.0 {
                p[0] -= 0.05;
                p[1] -= 0.07;
                p[2] -= 0.06;
            }
        }
        let r = recipe(
            vec![face(
                vec![Blemish {
                    id: 1,
                    x: 100.0,
                    y: 100.0,
                    r: 5.0,
                    score: 0.4,
                    neck: false,
                }],
                Settings::default(),
            )],
            json!({"blemishes": 50}),
        );
        assert!(apply_face_retouch(&mut image, &r));
        // The spot's centre is back near the skin (within 3 % per channel).
        let healed = image.get_pixel(100, 100);
        let skin = original.get_pixel(100, 100);
        for c in 0..3 {
            assert!(
                (healed[c] - skin[c]).abs() < 0.03 * skin[c] + 0.006,
                "{healed:?} vs {skin:?}"
            );
        }
        // The fine texture inside the healed spot is kept: neighbouring
        // pixels still differ as the original's do.
        let diff =
            |img: &Rgb32FImage| (img.get_pixel(99, 100)[1] - img.get_pixel(100, 100)[1]).abs();
        assert!(
            (diff(&image) - diff(&original)).abs() < 0.003,
            "{} vs {}",
            diff(&image),
            diff(&original)
        );
        // Far away only the skin's redness is evened, barely (the test
        // skin has hardly any).
        for c in 0..3 {
            let (a, b) = (image.get_pixel(10, 10)[c], original.get_pixel(10, 10)[c]);
            assert!((a - b).abs() < 1e-3 * b, "{a} vs {b}");
        }
    }

    /// A photo with two dark spots (at (60, 100) and (140, 100), radius 5)
    /// and the recipe that heals both and evens the skin.
    fn two_spots_and_recipe() -> (Rgb32FImage, Value) {
        let mut image = skin_image(200, 200);
        for (x, y, p) in image.enumerate_pixels_mut() {
            for cx in [60.0, 140.0] {
                let d = ((x as f32 - cx).powi(2) + (y as f32 - 100.0).powi(2)).sqrt();
                if d < 5.0 {
                    p[0] -= 0.05;
                    p[1] -= 0.07;
                    p[2] -= 0.06;
                }
            }
        }
        let spot = |id, x| Blemish {
            id,
            x,
            y: 100.0,
            r: 5.0,
            score: 0.4,
            neck: false,
        };
        let r = recipe(
            vec![face(
                vec![spot(1, 60.0), spot(2, 140.0)],
                Settings::default(),
            )],
            json!({"blemishes": 50, "evenSkin": 60, "texture": -40}),
        );
        (image, r)
    }

    #[test]
    fn protected_pixels_keep_their_values_and_the_rest_is_retouched() {
        let (original, mut r) = two_spots_and_recipe();
        let mut unprotected = original.clone();
        assert!(apply_face_retouch(&mut unprotected, &r));
        // A hard dab of radius 12 over the left spot.
        r["faceRetouch"]["protect"] =
            json!([{"points": [{"x": 60, "y": 100}], "radius": 12, "feather": 0}]);
        let mut image = original.clone();
        assert!(apply_face_retouch(&mut image, &r));
        for (x, y, p) in image.enumerate_pixels() {
            let d = (x as f64 - 60.0).hypot(y as f64 - 100.0);
            if d <= 11.5 {
                assert_eq!(p, original.get_pixel(x, y), "({x}, {y}) protected");
            } else if d >= 12.5 {
                assert_eq!(p, unprotected.get_pixel(x, y), "({x}, {y}) outside");
            }
        }
        // The other spot is still healed.
        assert!(image.get_pixel(140, 100)[1] > original.get_pixel(140, 100)[1] + 0.03);
        // Protection alone retouches nothing.
        let mut only = r.clone();
        only["faceRetouch"]["settings"] = json!({});
        assert!(!is_active(&only));
        // An empty list renders as no list (old recipes).
        let mut empty = r.clone();
        empty["faceRetouch"]["protect"] = json!([]);
        let mut image = original.clone();
        apply_face_retouch(&mut image, &empty);
        assert_eq!(image, unprotected);
    }

    #[test]
    fn malformed_protection_does_not_disable_the_retouch_or_valid_strokes() {
        let (original, mut r) = two_spots_and_recipe();
        let valid = json!({"points": [{"x": 60, "y": 100}], "radius": 12, "feather": 0});
        r["faceRetouch"]["protect"] = json!([valid]);
        let mut expected = original.clone();
        assert!(apply_face_retouch(&mut expected, &r));
        r["faceRetouch"]["protect"] = json!([
            {"points": [{"x": null, "y": 100}], "radius": 12},
            valid,
            {"points": [{"x": 140, "y": 100}]}
        ]);
        let mut actual = original;
        assert!(apply_face_retouch(&mut actual, &r));
        assert_eq!(actual, expected);
    }

    #[test]
    fn a_feathered_stroke_blends_and_erasing_takes_protection_away() {
        let (original, mut r) = two_spots_and_recipe();
        let mut unprotected = original.clone();
        apply_face_retouch(&mut unprotected, &r);
        // A stroke along the row through both spots, soft over its outer
        // half; then its right half erased.
        r["faceRetouch"]["protect"] = json!([
            {"points": [{"x": 40, "y": 100}, {"x": 160, "y": 100}], "radius": 20, "feather": 50},
            {"points": [{"x": 100, "y": 100}, {"x": 190, "y": 100}], "radius": 25, "feather": 0, "erase": true},
        ]);
        let mut image = original.clone();
        apply_face_retouch(&mut image, &r);
        // The left spot: inside the solid core, as it was.
        assert_eq!(image.get_pixel(60, 100), original.get_pixel(60, 100));
        // In the feathered edge, between the two.
        let (o, u, m) = (
            original.get_pixel(60, 116)[1],
            unprotected.get_pixel(60, 116)[1],
            image.get_pixel(60, 116)[1],
        );
        assert!((o - u).abs() > 1e-4, "the retouch changes the edge");
        assert!(m > o.min(u) && m < o.max(u), "{m} between {o} and {u}");
        // The erased right half is retouched as without protection.
        assert_eq!(image.get_pixel(140, 100), unprotected.get_pixel(140, 100));
    }

    #[test]
    fn previews_scale_the_protection_with_the_faces() {
        let (_, mut r) = two_spots_and_recipe();
        r["faceRetouch"]["protect"] =
            json!([{"points": [{"x": 60, "y": 100}, {"x": 80, "y": 90}], "radius": 12}]);
        let scaled = scaled_recipe(&r, 0.5);
        assert_eq!(
            scaled["faceRetouch"]["protect"],
            json!([{"points": [{"x": 30.0, "y": 50.0}, {"x": 40.0, "y": 45.0}], "radius": 6.0}])
        );
    }

    #[test]
    fn a_blemish_in_a_dense_field_heals_to_the_clean_skin() {
        // Dark pigment spots every 11 px: the heal's ring crosses its
        // neighbours, which must not darken the fill.
        let clean = skin_image(200, 200);
        let mut image = clean.clone();
        for (x, y, p) in image.enumerate_pixels_mut() {
            let (dx, dy) = ((x as f32 - 100.0) / 11.0, (y as f32 - 100.0) / 11.0);
            let (fx, fy) = ((dx - dx.round()) * 11.0, (dy - dy.round()) * 11.0);
            if (fx * fx + fy * fy).sqrt() < 4.0 {
                for c in p.0.iter_mut() {
                    *c *= 0.6;
                }
            }
        }
        // All spots of the field, as Blemishes 100 takes them.
        let spots: Vec<(f64, f64, f64)> = (-8..=8)
            .flat_map(|i| {
                (-8..=8).map(move |j| (100.5 + 11.0 * i as f64, 100.5 + 11.0 * j as f64, 5.0))
            })
            .collect();
        heal_blemishes(&mut image, &spots, false);
        let level = |img: &Rgb32FImage| {
            let mut sum = 0.0;
            for y in 98..103 {
                for x in 98..103 {
                    sum += img.get_pixel(x, y)[1];
                }
            }
            sum / 25.0
        };
        let (healed, skin) = (level(&image), level(&clean));
        assert!((healed - skin).abs() < 0.05 * skin, "{healed} vs {skin}");
    }

    #[test]
    fn blemishes_take_broad_redness() {
        // A red blotch a third of the eye distance across.
        let mut image = skin_image(200, 200);
        for (x, y, p) in image.enumerate_pixels_mut() {
            let d2 = (x as f32 - 100.0).powi(2) + (y as f32 - 100.0).powi(2);
            p[0] *= 1.0 + 0.12 * (-d2 / (2.0 * 12.0 * 12.0)).exp();
        }
        let original = image.clone();
        let r = recipe(
            vec![face(vec![], Settings::default())],
            json!({"blemishes": 100}),
        );
        assert!(apply_face_retouch(&mut image, &r));
        let red = |img: &Rgb32FImage, x: u32, y: u32| {
            let p = img.get_pixel(x, y);
            (p[0] / p[1]).ln()
        };
        let before = red(&original, 100, 100) - red(&original, 30, 30);
        let after = red(&image, 100, 100) - red(&image, 30, 30);
        // Taken partly (Capture One takes about as much on real faces).
        assert!(
            after < 0.95 * before && after > 0.5 * before,
            "{after} vs {before}"
        );
        // Lightness kept.
        let lum = |p: &image::Rgb<f32>| 0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2];
        let (a, b) = (
            lum(image.get_pixel(100, 100)),
            lum(original.get_pixel(100, 100)),
        );
        assert!((a - b).abs() < 0.02 * b, "{a} vs {b}");
    }

    #[test]
    fn a_scaled_recipe_retouches_a_reduced_copy_alike() {
        // A preview at half resolution with the scaled recipe against the
        // full-resolution render reduced afterwards.
        // Blotches at the eye distance's scale as in a fit view (eye
        // distance 160 px at full, 80 px in the preview), without pores at
        // the full resolution's pixel scale (halving removes those; real
        // skin is checked in ai_faces' renders_face_retouch_of_real_photos).
        let mut full = Rgb32FImage::from_pixel(200, 200, image::Rgb([0.24, 0.18, 0.15]));
        for (x, y, p) in full.enumerate_pixels_mut() {
            let blotch = (x as f32 / 3.0).sin() * (y as f32 / 3.0).sin() * 0.01;
            for c in p.0.iter_mut() {
                *c += blotch;
            }
        }
        for (x, y, p) in full.enumerate_pixels_mut() {
            if ((x as f32 - 100.0).powi(2) + (y as f32 - 100.0).powi(2)).sqrt() < 6.0 {
                p[1] *= 0.7;
            }
        }
        let half = |img: &Rgb32FImage| {
            Rgb32FImage::from_fn(100, 100, |x, y| {
                let px = |dx, dy| img.get_pixel(2 * x + dx, 2 * y + dy).0;
                let q = [px(0, 0), px(1, 0), px(0, 1), px(1, 1)];
                image::Rgb(std::array::from_fn(|c| {
                    q.iter().map(|p| p[c]).sum::<f32>() / 4.0
                }))
            })
        };
        let mut reduced = half(&full);
        let original = reduced.clone();
        let mut f = face(
            vec![Blemish {
                id: 1,
                x: 100.0,
                y: 100.0,
                r: 6.0,
                score: 0.5,
                neck: false,
            }],
            Settings::default(),
        );
        f.eye_distance = 160.0;
        let r = recipe(vec![f], json!({"blemishes": 60, "evenSkin": 80}));
        assert!(apply_face_retouch(&mut full, &r));
        assert!(apply_face_retouch(&mut reduced, &scaled_recipe(&r, 0.5)));
        let expected = half(&full);
        let (mut diff, mut change) = (0.0, 0.0);
        for ((a, b), o) in reduced
            .pixels()
            .zip(expected.pixels())
            .zip(original.pixels())
        {
            diff += (a[1] - b[1]).abs();
            change += (b[1] - o[1]).abs();
        }
        // The two differ far less than the retouch changes.
        assert!(diff < 0.25 * change, "{diff} vs {change}");
        let (healed, reference) = (reduced.get_pixel(50, 50)[1], expected.get_pixel(50, 50)[1]);
        assert!(
            (healed - reference).abs() < 0.05 * reference,
            "{healed} vs {reference}"
        );
    }

    #[test]
    fn neck_is_retouched_only_when_included() {
        // Face skin in the top half of the crop, neck skin in the bottom
        // half; a red spot on the neck and blotches all over.
        let mut base = with_blotches(0.01);
        for (x, y, p) in base.enumerate_pixels_mut() {
            let d = ((x as f32 - 100.0).powi(2) + (y as f32 - 150.0).powi(2)).sqrt();
            if d < 5.0 {
                p[0] *= 0.8;
                p[1] *= 0.6;
                p[2] *= 0.6;
            }
        }
        let mut f = face(
            vec![Blemish {
                id: 1,
                x: 100.0,
                y: 150.0,
                r: 5.0,
                score: 0.4,
                neck: true,
            }],
            Settings::default(),
        );
        f.skin = png_data_url(&GrayImage::from_fn(100, 100, |_, v| {
            Luma([if v < 50 { 255 } else { 0 }])
        }));
        f.neck = Some(png_data_url(&GrayImage::from_fn(100, 100, |_, v| {
            Luma([if v >= 50 { 255 } else { 0 }])
        })));
        let render = |include: bool| {
            let mut image = base.clone();
            let r = recipe(
                vec![f.clone()],
                json!({"blemishes": 50, "evenSkin": 100, "includeNeck": include}),
            );
            assert!(apply_face_retouch(&mut image, &r));
            image
        };
        let without = render(false);
        let with = render(true);
        // The face is retouched either way, the same.
        assert_ne!(without.get_pixel(100, 40), base.get_pixel(100, 40));
        assert_eq!(without.get_pixel(100, 40), with.get_pixel(100, 40));
        // The neck (spot and blotches) only with the option.
        assert_eq!(without.get_pixel(100, 150), base.get_pixel(100, 150));
        assert_eq!(without.get_pixel(60, 170), base.get_pixel(60, 170));
        let skin = base.get_pixel(60, 150)[1];
        assert!((with.get_pixel(100, 150)[1] - skin).abs() < 0.1 * skin);
        assert_ne!(with.get_pixel(60, 170), base.get_pixel(60, 170));
    }

    /// Fine and middle band energy of the green channel (working space)
    /// at the scales the render uses for an eye distance of 80 px.
    fn band_energy(img: &Rgb32FImage) -> (f32, f32) {
        let (w, h) = img.dimensions();
        let y = Plane {
            w: w as usize,
            h: h as usize,
            data: img.pixels().map(|p| encode(p[1])).collect(),
        };
        let a = blur(&y, 0.8);
        let b = blur(&y, 3.6);
        // Inside the crop's faded edge.
        let inner = |k: usize| {
            let (x, yy) = (k % w as usize, k / w as usize);
            (10..w as usize - 10).contains(&x) && (10..h as usize - 10).contains(&yy)
        };
        let fine = (0..y.data.len())
            .filter(|&k| inner(k))
            .map(|k| (y.data[k] - a.data[k]).powi(2))
            .sum::<f32>();
        let mid = (0..y.data.len())
            .filter(|&k| inner(k))
            .map(|k| (a.data[k] - b.data[k]).powi(2))
            .sum::<f32>();
        (fine, mid)
    }

    fn with_blotches(amplitude: f32) -> Rgb32FImage {
        let mut image = skin_image(200, 200);
        for (x, y, p) in image.enumerate_pixels_mut() {
            // Blotches about a tenth of the eye distance across.
            let blotch = (x as f32 / 1.5).sin() * (y as f32 / 1.5).sin() * amplitude;
            for c in p.0.iter_mut() {
                *c += blotch;
            }
        }
        image
    }

    #[test]
    fn even_skin_damps_blotches_and_keeps_pores() {
        let mut image = with_blotches(0.006);
        let before = image.clone();
        let r = recipe(
            vec![face(vec![], Settings::default())],
            json!({"evenSkin": 100}),
        );
        apply_face_retouch(&mut image, &r);
        let (fine0, mid0) = band_energy(&before);
        let (fine1, mid1) = band_energy(&image);
        assert!(mid1 < mid0 * 0.4, "mid {mid0} → {mid1}");
        assert!(fine1 > fine0 * 0.6, "fine {fine0} → {fine1}");
    }

    #[test]
    fn even_skin_keeps_the_shading_of_the_face() {
        // The same scale at twenty times the amplitude (about ±30 % in
        // lightness, as the shadow of a nose wing): form, not blotches.
        let mut image = with_blotches(0.12);
        let before = image.clone();
        let r = recipe(
            vec![face(vec![], Settings::default())],
            json!({"evenSkin": 100}),
        );
        apply_face_retouch(&mut image, &r);
        let (_, mid0) = band_energy(&before);
        let (_, mid1) = band_energy(&image);
        assert!(mid1 > mid0 * 0.85, "mid {mid0} → {mid1}");
    }

    #[test]
    fn blemishes_clear_dark_specks_and_keep_the_pores() {
        let mut image = skin_image(200, 200);
        // Brown specks of 2 px every 17 px.
        for (x, y, p) in image.enumerate_pixels_mut() {
            if x % 17 < 2 && y % 17 < 2 {
                p[0] *= 0.6;
                p[1] *= 0.5;
                p[2] *= 0.5;
            }
        }
        let before = image.clone();
        // Eye distance 400 px: the fine layer reaches 4 px, as for a portrait.
        let mut f = face(vec![], Settings::default());
        f.eye_distance = 400.0;
        let r = recipe(vec![f], json!({"blemishes": 60}));
        apply_face_retouch(&mut image, &r);
        // The speck at (68, 68) is close to the skin now; a pore (a
        // ±0.008 step of the texture) is kept.
        let speck = |img: &Rgb32FImage| img.get_pixel(68, 68)[1];
        assert!(
            speck(&image) > 0.155,
            "{} → {}",
            speck(&before),
            speck(&image)
        );
        let pore =
            |img: &Rgb32FImage| (img.get_pixel(100, 100)[1] - img.get_pixel(101, 100)[1]).abs();
        assert!(pore(&image) > 0.7 * pore(&before));
    }

    #[test]
    fn blemishes_keep_hairs() {
        let mut image = skin_image(200, 200);
        // A dark hair, about 2 px wide and antialiased, across the skin.
        for (x, y, p) in image.enumerate_pixels_mut() {
            let d = (y as f32 - x as f32 / 3.0 - 60.0).abs() / (1.0f32 + 1.0 / 9.0).sqrt();
            let a = (1.0 - d).clamp(0.0, 1.0);
            for c in p.0.iter_mut() {
                *c *= 1.0 - 0.65 * a;
            }
        }
        let before = image.clone();
        let mut f = face(vec![], Settings::default());
        f.eye_distance = 400.0;
        let r = recipe(vec![f], json!({"blemishes": 100}));
        apply_face_retouch(&mut image, &r);
        let hair = |img: &Rgb32FImage| img.get_pixel(90, 90)[1];
        assert!(
            hair(&image) < hair(&before) * 1.3,
            "{} → {}",
            hair(&before),
            hair(&image)
        );
    }

    #[test]
    fn contouring_deepens_shadows_and_keeps_highlights() {
        let mut image = skin_image(200, 200);
        // A broad shadow band (a cheek hollow) and a broad highlight.
        for (x, _, p) in image.enumerate_pixels_mut() {
            let k = if (60..90).contains(&x) {
                0.7
            } else if (120..150).contains(&x) {
                1.3
            } else {
                1.0
            };
            for c in p.0.iter_mut() {
                *c *= k;
            }
        }
        let before = image.clone();
        let r = recipe(
            vec![face(vec![], Settings::default())],
            json!({"contouring": 100}),
        );
        apply_face_retouch(&mut image, &r);
        let at = |img: &Rgb32FImage, x: u32| img.get_pixel(x, 100)[1];
        assert!(at(&image, 75) < at(&before, 75) * 0.97, "shadow");
        assert!(
            (at(&image, 135) - at(&before, 135)).abs() < 0.002,
            "highlight"
        );
    }

    #[test]
    fn even_skin_keeps_the_brightness() {
        let mut image = with_blotches(0.006);
        let before = image.clone();
        let r = recipe(
            vec![face(vec![], Settings::default())],
            json!({"evenSkin": 100}),
        );
        apply_face_retouch(&mut image, &r);
        let mean = |img: &Rgb32FImage| img.pixels().map(|p| p[1]).sum::<f32>() / 40000.0;
        assert!((mean(&image) / mean(&before) - 1.0).abs() < 0.002);
    }

    #[test]
    fn texture_softens_pores_only_on_skin() {
        let mut image = skin_image(300, 200);
        let before = image.clone();
        let r = recipe(
            vec![face(vec![], Settings::default())],
            json!({"texture": -100}),
        );
        apply_face_retouch(&mut image, &r);
        let local = |img: &Rgb32FImage, x: u32| {
            (img.get_pixel(x, 50)[1] - img.get_pixel(x + 1, 50)[1]).abs()
        };
        // Inside the face's crop (x < 200) the pores flatten, outside not.
        assert!(local(&image, 100) < local(&before, 100) * 0.5);
        assert_eq!(image.get_pixel(260, 50), before.get_pixel(260, 50));
    }

    #[test]
    fn dark_circles_lift_the_under_eye_area() {
        let mut image = skin_image(200, 200);
        // A darker band under the "eye" at y 70–90, x 60–140.
        for (x, y, p) in image.enumerate_pixels_mut() {
            if (60..140).contains(&x) && (70..90).contains(&y) {
                for c in p.0.iter_mut() {
                    *c *= 0.7;
                }
            }
        }
        let before = image.clone();
        let mut f = face(vec![], Settings::default());
        // Under-eye weight in crop pixels (crop = photo / 2).
        f.under_eyes = Some(png_data_url(&GrayImage::from_fn(100, 100, |x, y| {
            Luma([if (30..70).contains(&x) && (35..45).contains(&y) {
                255
            } else {
                0
            }])
        })));
        let mut r = recipe(vec![f], json!({"darkCircles": 100}));
        r["faceRetouch"]["faces"][0]["underEyes"] =
            json!(png_data_url(&GrayImage::from_fn(100, 100, |x, y| {
                Luma([if (30..70).contains(&x) && (35..45).contains(&y) {
                    255
                } else {
                    0
                }])
            })));
        apply_face_retouch(&mut image, &r);
        let lifted = image.get_pixel(100, 80)[1];
        assert!(lifted > before.get_pixel(100, 80)[1] * 1.15, "{lifted}");
        assert!(lifted <= original_level() * 1.05);
    }

    fn original_level() -> f32 {
        0.18 + 0.008
    }
}
