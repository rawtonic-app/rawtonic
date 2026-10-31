//! Red eye and pet eye correction (Lightroom's Red Eye tool with its Pet Eye
//! mode, Capture One's Remove Red Eye) as parametric operations on the
//! photo's base image.
//!
//! A correction is an ellipse around the pupil, found by a click (`detect`:
//! the connected area of red, or for pets of glow, around the clicked point,
//! deterministic image processing, no model), with Pupil Size and Darken
//! and, for pet eyes, an optional catchlight. It is kept in the photo as
//! decoded (the base image, before the user's turns, flips, straightening,
//! lens and transform corrections), in units of its long edge, so it stays
//! on the eye whatever the geometry does and fits reduced copies of the
//! base as they are. The editor shows it where the geometry puts it
//! (`shown`). It is applied in float on the base image after the AI patches
//! and before the spots, so a heal copying from an eye copies the corrected
//! pupil, and preview and export share it.
//!
//! The base image holds linear values for RAW files and sRGB-encoded values
//! for other formats. The correction is built from ratios of the image's own
//! values only (normalised redness, geometric means, largest channels,
//! levels measured around the eye, a geometric blend), so it commutes with any power-law encoding
//! and renders alike on either base without being told which: sRGB is not
//! quite a power law, and `renders_alike_in_either_encoding` measures the
//! rest (red eyes within 3 of 255 codes but for pixels at the redness
//! gate's edge, a pet eye's black within 6 in sRGB's toe).
//! Only the detector's acceptance thresholds are absolute, and it is told
//! the encoding.

use image::{DynamicImage, Rgb32FImage};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::spot_removal::{mask_frame, to_base};

/// The render of red eye corrections that recipes store `version` 1 for.
/// A correction with a version this build does not know is not applied.
pub const VERSION: u32 = 1;

/// Red eye (a red pupil from the flash) or pet eye (a glowing pupil of any
/// colour, the reflection of an animal's tapetum).
#[derive(Deserialize, Serialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub enum EyeKind {
    #[default]
    Red,
    Pet,
}

/// One correction as the frontend stores it (`adjustments.redEyes`).
#[derive(Deserialize, Serialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RedEye {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub kind: EyeKind,
    /// Centre of the pupil's ellipse in the photo as decoded, in units of
    /// its long edge from its top left (pixel centres at whole pixels).
    pub x: f64,
    pub y: f64,
    /// Semi-axes of the ellipse in the same units; `angle` (degrees) turns
    /// the first axis from the x axis towards the y axis.
    pub radius_x: f64,
    pub radius_y: f64,
    #[serde(default)]
    pub angle: f64,
    /// 0–100, 50 = the found pupil; the correction's ellipse scales from
    /// 0.6 to 1.4 of it.
    #[serde(default = "default_fifty")]
    pub pupil_size: f64,
    /// 0–100: 0 only takes the colour out, higher values darken the pupil
    /// further towards the dark of its surroundings.
    #[serde(default = "default_fifty")]
    pub darken: f64,
    /// Pet eye: a catchlight is painted into the darkened pupil, at
    /// `catchlightX`/`catchlightY` from its centre in units of the corrected
    /// pupil's mean radius along the decoded photo's x and y.
    #[serde(default)]
    pub catchlight: bool,
    #[serde(default)]
    pub catchlight_x: f64,
    #[serde(default)]
    pub catchlight_y: f64,
    #[serde(default = "default_visible")]
    pub visible: bool,
}

fn default_version() -> u32 {
    1
}
fn default_fifty() -> f64 {
    50.0
}
fn default_visible() -> bool {
    true
}

/// The catchlight's default place: up and left of the centre, where a
/// flash above the lens or a window light puts it.
pub const DEFAULT_CATCHLIGHT: (f64, f64) = (-0.3, -0.35);

/// The visible corrections of a recipe that this build renders (without
/// Lightroom's not yet placed, see `red_eyes_on`).
pub fn red_eyes(adjustments: &Value) -> Vec<RedEye> {
    red_eyes_with(adjustments, |item| item.clone())
}

/// `red_eyes` on a base image of `width` × `height` (or a copy of it),
/// Lightroom's imported ones not yet placed included where they stand
/// (`lightroom_retouch::placed_red_eye`).
fn red_eyes_on(adjustments: &Value, width: u32, height: u32) -> Vec<RedEye> {
    red_eyes_with(adjustments, |item| {
        if item.get(crate::lightroom_retouch::PENDING).is_some() {
            crate::lightroom_retouch::placed_red_eye(
                item,
                f64::from(width),
                f64::from(height),
                None,
            )
            .unwrap_or(Value::Null)
        } else {
            item.clone()
        }
    })
}

fn red_eyes_with(adjustments: &Value, resolve: impl Fn(&Value) -> Value) -> Vec<RedEye> {
    adjustments
        .get("redEyes")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| serde_json::from_value::<RedEye>(resolve(item)).ok())
                .filter(|e| {
                    e.visible
                        && e.version == VERSION
                        && e.radius_x > 0.0
                        && e.radius_y > 0.0
                        && e.x.is_finite()
                        && e.y.is_finite()
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn is_active(adjustments: &Value) -> bool {
    !red_eyes(adjustments).is_empty()
        || crate::lightroom_retouch::has_renderable(adjustments, "redEyes")
}

/// Smallest value the ratios work with (pupils are dark, channels may be 0
/// or slightly negative in wide-gamut RAW data).
const FLOOR: f32 = 1e-6;

fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Log redness: red against the larger of green and blue.
fn redness(p: [f32; 3]) -> f32 {
    p[0].max(FLOOR).ln() - p[1].max(p[2]).max(FLOOR).ln()
}

/// Log brightness as the largest channel: a saturated glow is bright
/// whatever its colour (a mean would read a pure green as dark), and it
/// commutes with any encoding.
fn log_level(p: [f32; 3]) -> f32 {
    p[0].max(p[1]).max(p[2]).max(FLOOR).ln()
}

/// The `q`-quantile (0–1) of `values` (sorted in place); None when empty.
fn quantile(values: &mut [f32], q: f32) -> Option<f32> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(f32::total_cmp);
    let i = ((values.len() - 1) as f32 * q).round() as usize;
    Some(values[i])
}

/// An eye's ellipse in base-image pixels: centre and the inverse of the
/// map from ellipse units (semi-axes 1) to pixel offsets.
struct Placement {
    centre: (f64, f64),
    /// Inverse of [first axis, second axis] (columns), base pixels.
    inverse: [[f64; 2]; 2],
    /// Box of the pixels within `reach` ellipse units.
    reach_px: f64,
}

impl Placement {
    /// `eye` on a base image of `width` × `height` pixels.
    fn of(eye: &RedEye, width: u32, height: u32, reach: f64) -> Option<Self> {
        let unit = f64::from(width.max(height));
        let (s, c) = eye.angle.to_radians().sin_cos();
        let (a, b) = (eye.radius_x * unit, eye.radius_y * unit);
        if !(a.is_finite() && b.is_finite() && a > 1e-3 && b > 1e-3) {
            return None;
        }
        // [a·(c, s), b·(−s, c)]⁻¹: a rotation back, then the semi-axes.
        let inverse = [[c / a, s / a], [-s / b, c / b]];
        Some(Self {
            centre: (eye.x * unit, eye.y * unit),
            inverse,
            reach_px: reach * a.max(b),
        })
    }

    /// The pixel's offset in ellipse units.
    fn units(&self, x: f64, y: f64) -> (f64, f64) {
        let (dx, dy) = (x - self.centre.0, y - self.centre.1);
        (
            self.inverse[0][0] * dx + self.inverse[0][1] * dy,
            self.inverse[1][0] * dx + self.inverse[1][1] * dy,
        )
    }

    /// Pixel box (inclusive) within the reach, clipped to the image.
    fn bounds(&self, width: u32, height: u32) -> Option<(u32, u32, u32, u32)> {
        let (cx, cy) = self.centre;
        let r = self.reach_px + 1.0;
        let x0 = (cx - r).floor().max(0.0);
        let y0 = (cy - r).floor().max(0.0);
        let x1 = (cx + r).ceil().min(width as f64 - 1.0);
        let y1 = (cy + r).ceil().min(height as f64 - 1.0);
        (x0 <= x1 && y0 <= y1).then_some((x0 as u32, y0 as u32, x1 as u32, y1 as u32))
    }
}

/// The ellipse units out to which the surroundings are measured.
const RING: (f64, f64) = (1.6, 2.2);

/// Where an eye is measured, in ellipse units of its corrected pupil: the
/// pupil within the first, the ring around it between the other two. A
/// pupil only a pixel or two across in this copy of the base (a reduced
/// preview) is measured on whole pixels: its nearest pixel, and a ring a
/// pixel or more beyond it.
fn regions(eye: &RedEye, unit: f64, size: f64) -> (f64, (f64, f64)) {
    let pixel = 1.0 / (eye.radius_x.min(eye.radius_y) * unit * size).max(1e-6);
    let inner = 0.8f64.max(0.75 * pixel);
    (
        inner,
        (RING.0.max(inner + pixel), RING.1.max(inner + 2.5 * pixel)),
    )
}

/// Applies one correction to `image` (base pixels).
fn apply_eye(
    image: &mut Rgb32FImage,
    eye: &RedEye,
    place: &Placement,
    (inner, ring): (f64, (f64, f64)),
) {
    let (w, h) = image.dimensions();
    let Some((x0, y0, x1, y1)) = place.bounds(w, h) else {
        return;
    };
    // Pupil Size scales the ellipse the correction covers.
    let size = 0.6 + 0.8 * (eye.pupil_size.clamp(0.0, 100.0) / 100.0);
    let darken = (eye.darken.clamp(0.0, 100.0) / 100.0) as f32;
    let pet = eye.kind == EyeKind::Pet;

    // Measures of the eye before its correction: inside the pupil and on a
    // ring around it (iris, lids), in ellipse units of the scaled pupil.
    let mut inner_red = Vec::new();
    let mut inner_level = Vec::new();
    let mut ring_red = Vec::new();
    let mut ring_level = Vec::new();
    for y in y0..=y1 {
        for x in x0..=x1 {
            let (u, v) = place.units(x as f64, y as f64);
            let rho = u.hypot(v) / size;
            let p = image.get_pixel(x, y).0;
            if rho <= inner {
                inner_red.push(redness(p));
                inner_level.push(log_level(p));
            } else if (ring.0..=ring.1).contains(&rho) {
                ring_red.push(redness(p));
                ring_level.push(log_level(p));
            }
        }
    }
    let (Some(ring_red_mid), Some(ring_mid), Some(ring_dark)) = (
        quantile(&mut ring_red, 0.5),
        quantile(&mut ring_level.clone(), 0.5),
        quantile(&mut ring_level, 0.1),
    ) else {
        return;
    };
    let (Some(peak_red), Some(peak_level), Some(top_level)) = (
        quantile(&mut inner_red, 0.8),
        quantile(&mut inner_level.clone(), 0.8),
        quantile(&mut inner_level, 1.0),
    ) else {
        return;
    };
    // Redness between the surroundings' and the pupil's: 0 to 1.
    let red_span = (peak_red - ring_red_mid).max(1e-3);
    // Pet eye: the pupil's level, the darkest tenth of the ring darkened
    // further by how far the glow stands above it (log units).
    let pet_level = ring_dark + (ring_dark - peak_level).min(0.0) * (0.5 + darken);
    // Catchlight: centre and radius in ellipse units (its offset is along
    // the photo's x and y in units of the corrected pupil's mean radius).
    let catchlight = (pet && eye.catchlight).then(|| {
        let mean = (eye.radius_x * eye.radius_y).sqrt() * size;
        let offset = (eye.catchlight_x * mean, eye.catchlight_y * mean);
        let (s, c) = eye.angle.to_radians().sin_cos();
        let along = (offset.0 * c + offset.1 * s) / eye.radius_x;
        let across = (-offset.0 * s + offset.1 * c) / eye.radius_y;
        ((along, across), 0.2 * size)
    });

    for y in y0..=y1 {
        for x in x0..=x1 {
            let (u, v) = place.units(x as f64, y as f64);
            let rho = (u.hypot(v) / size) as f32;
            // Each pixel is read before it is written, and only itself.
            let p = image.get_pixel(x, y).0;
            let (target, weight) = if pet {
                let weight = 1.0 - smoothstep(0.85, 1.15, rho);
                (pet_level, weight)
            } else {
                let t = (redness(p) - ring_red_mid) / red_span;
                // The soft edge reaches past the found ellipse (a blurred
                // pupil's red edge lies outside it); the redness gate keeps
                // the iris and skin.
                let weight = (1.0 - smoothstep(0.9, 1.5, rho)) * smoothstep(0.1, 0.4, t);
                // Without red: the geometric mean of green and blue, then
                // darkened by its distance below the surroundings.
                let neutral = (p[1].max(FLOOR).ln() + p[2].max(FLOOR).ln()) / 2.0;
                (neutral + (neutral - ring_mid).min(0.0) * darken, weight)
            };
            let mut out = p;
            if weight > 0.0 {
                // A geometric blend towards the neutral target.
                out = p.map(|c| (c.max(FLOOR).ln() * (1.0 - weight) + target * weight).exp());
            }
            if let Some(((cu, cv), radius)) = catchlight {
                let d = ((u - cu).hypot(v - cv)) as f32;
                let wc = 1.0 - smoothstep(0.5 * radius as f32, radius as f32, d);
                if wc > 0.0 {
                    out = out.map(|c| (c.max(FLOOR).ln() * (1.0 - wc) + top_level * wc).exp());
                }
            }
            if weight > 0.0 || out != p {
                image.put_pixel(x, y, image::Rgb(out));
            }
        }
    }
}

/// The base image with the recipe's red eye corrections applied; None when
/// it has none.
pub fn apply_red_eyes(base: &DynamicImage, adjustments: &Value) -> Option<DynamicImage> {
    let eyes = red_eyes_on(adjustments, base.width(), base.height());
    if eyes.is_empty() {
        return None;
    }
    let mut image = base.to_rgb32f();
    let (width, height) = image.dimensions();
    let unit = f64::from(width.max(height));
    for eye in &eyes {
        let size = 0.6 + 0.8 * (eye.pupil_size.clamp(0.0, 100.0) / 100.0);
        let measured = regions(eye, unit, size);
        if let Some(place) = Placement::of(eye, width, height, measured.1.1 * size) {
            apply_eye(&mut image, eye, &place, measured);
        }
    }
    Some(DynamicImage::ImageRgb32F(image))
}

/// Convex hull of grid points, each edge turning the same way (positive
/// cross products, as `within` reads them)
/// (Andrew's monotone chain); collinear points left out.
fn convex_hull(mut points: Vec<(i64, i64)>) -> Vec<(i64, i64)> {
    points.sort_unstable();
    points.dedup();
    if points.len() < 3 {
        return points;
    }
    let cross = |o: (i64, i64), a: (i64, i64), b: (i64, i64)| {
        (a.0 - o.0) * (b.1 - o.1) - (a.1 - o.1) * (b.0 - o.0)
    };
    let mut hull: Vec<(i64, i64)> = Vec::with_capacity(points.len() * 2);
    for pass in 0..2 {
        let start = hull.len();
        let ordered: Box<dyn Iterator<Item = &(i64, i64)>> = if pass == 0 {
            Box::new(points.iter())
        } else {
            Box::new(points.iter().rev())
        };
        for &p in ordered {
            while hull.len() >= start + 2
                && cross(hull[hull.len() - 2], hull[hull.len() - 1], p) <= 0
            {
                hull.pop();
            }
            hull.push(p);
        }
        hull.pop();
    }
    hull
}

/// Whether `p` lies in the convex polygon `hull` (`convex_hull`'s order),
/// edges included.
fn within(hull: &[(i64, i64)], p: (i64, i64)) -> bool {
    if hull.len() < 3 {
        return hull.contains(&p);
    }
    (0..hull.len()).all(|k| {
        let (a, b) = (hull[k], hull[(k + 1) % hull.len()]);
        (b.0 - a.0) * (p.1 - a.1) - (b.1 - a.1) * (p.0 - a.0) >= 0
    })
}

/// An ellipse: centre, semi-axes and angle (degrees). As the detector
/// returns it and corrections store it, in the photo as decoded in units
/// of its long edge; as `shown` returns it, in mask-frame pixels.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Pupil {
    pub x: f64,
    pub y: f64,
    pub radius_x: f64,
    pub radius_y: f64,
    #[serde(default)]
    pub angle: f64,
}

/// sRGB-encoded value to linear light.
fn srgb_to_linear(v: f32) -> f32 {
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

/// Red must reach this many times the larger of green and blue in linear
/// light at the reddest point (skin and a brown iris stay near 2; a red
/// pupil is far above).
const MIN_RED_RATIO: f32 = 3.0;
/// The reddest point against the circle's rim (linear ratio).
const MIN_RED_CONTRAST: f32 = 1.8;
/// A pet's glow against the darkest quarter of the circle (linear ratio
/// of levels).
const MIN_GLOW_CONTRAST: f32 = 1.6;

/// The derivative of `to_base` at mask-frame (`x`, `y`): base pixels per
/// mask-frame pixel, columns along the mask frame's x and y.
fn derivative(adjustments: &Value, frame: (f64, f64), x: f64, y: f64) -> [[f64; 2]; 2] {
    let h = 0.5;
    let px = to_base(adjustments, frame, x + h, y);
    let nx = to_base(adjustments, frame, x - h, y);
    let py = to_base(adjustments, frame, x, y + h);
    let ny = to_base(adjustments, frame, x, y - h);
    [
        [(px.0 - nx.0) / (2.0 * h), (py.0 - ny.0) / (2.0 * h)],
        [(px.1 - nx.1) / (2.0 * h), (py.1 - ny.1) / (2.0 * h)],
    ]
}

fn invert(m: [[f64; 2]; 2]) -> Option<[[f64; 2]; 2]> {
    let det = m[0][0] * m[1][1] - m[0][1] * m[1][0];
    (det.abs() > 1e-12 && det.is_finite()).then(|| {
        [
            [m[1][1] / det, -m[0][1] / det],
            [-m[1][0] / det, m[0][0] / det],
        ]
    })
}

/// The mask-frame point that `to_base` takes to base pixel `target`
/// (Newton's method from the frame's centre: exact in one step for the
/// turns, flips, straightening and transform, a few more for lens
/// distortion). `to_base` computes in f32, so a few thousand pixels out it
/// resolves only fractions of a thousandth of a pixel: the closest point
/// found counts when it lies within a hundredth of a pixel. None when
/// nothing comes that close.
pub(crate) fn to_mask(
    adjustments: &Value,
    frame: (f64, f64),
    target: (f64, f64),
) -> Option<(f64, f64)> {
    let mut m = (frame.0 / 2.0, frame.1 / 2.0);
    let mut best: Option<((f64, f64), f64)> = None;
    for _ in 0..30 {
        let b = to_base(adjustments, frame, m.0, m.1);
        let (ex, ey) = (target.0 - b.0, target.1 - b.1);
        let error = ex.hypot(ey);
        if best.is_none_or(|(_, e)| error < e) {
            best = Some((m, error));
        }
        if error < 1e-6 {
            break;
        }
        let Some(inv) = invert(derivative(adjustments, frame, m.0, m.1)) else {
            break;
        };
        m = (
            m.0 + inv[0][0] * ex + inv[0][1] * ey,
            m.1 + inv[1][0] * ex + inv[1][1] * ey,
        );
        if !(m.0.is_finite() && m.1.is_finite()) {
            break;
        }
    }
    best.filter(|&(_, error)| error < 0.01).map(|(m, _)| m)
}

/// The ellipse with covariance-like matrix `m` (semi-axes² on its
/// eigenvectors): semi-axes and angle (degrees) of its first axis.
pub(crate) fn ellipse_of(m: [[f64; 2]; 2]) -> (f64, f64, f64) {
    let mean = (m[0][0] + m[1][1]) / 2.0;
    let spread = (((m[0][0] - m[1][1]) / 2.0).powi(2) + m[0][1] * m[0][1]).sqrt();
    let angle = 0.5 * (2.0 * m[0][1]).atan2(m[0][0] - m[1][1]);
    (
        (mean + spread).sqrt(),
        (mean - spread).max(0.0).sqrt(),
        angle.to_degrees(),
    )
}

/// A correction's ellipse (`pupil`, in the photo as decoded) where the
/// photo `adjustments` describe shows it: mask-frame pixels, the ellipse
/// carried through the geometry's local derivative (so a transform that
/// stretches one way stretches it too). `width` × `height`: the base
/// image's full-resolution size.
pub fn shown(adjustments: &Value, width: u32, height: u32, pupil: &Pupil) -> Option<Pupil> {
    let unit = f64::from(width.max(height));
    let frame = mask_frame(adjustments, width, height);
    let centre = to_mask(adjustments, frame, (pupil.x * unit, pupil.y * unit))?;
    let to_mask_axes = invert(derivative(adjustments, frame, centre.0, centre.1))?;
    let (s, c) = pupil.angle.to_radians().sin_cos();
    let (a, b) = (pupil.radius_x * unit, pupil.radius_y * unit);
    // Base ellipse matrix R·diag(a², b²)·Rᵀ, then J·Σ·Jᵀ.
    let base = [
        [a * a * c * c + b * b * s * s, (a * a - b * b) * s * c],
        [(a * a - b * b) * s * c, a * a * s * s + b * b * c * c],
    ];
    let j = to_mask_axes;
    let js: [[f64; 2]; 2] = std::array::from_fn(|r| {
        std::array::from_fn(|k| j[r][0] * base[0][k] + j[r][1] * base[1][k])
    });
    let m: [[f64; 2]; 2] =
        std::array::from_fn(|r| std::array::from_fn(|k| js[r][0] * j[k][0] + js[r][1] * j[k][1]));
    let (radius_x, radius_y, angle) = ellipse_of(m);
    Some(Pupil {
        x: centre.0,
        y: centre.1,
        radius_x,
        radius_y,
        angle,
    })
}

/// Mask-frame (`x`, `y`) in the photo as decoded, in units of its long
/// edge (where a correction dragged there is kept).
pub fn image_point(adjustments: &Value, width: u32, height: u32, x: f64, y: f64) -> (f64, f64) {
    let unit = f64::from(width.max(height));
    let frame = mask_frame(adjustments, width, height);
    let base = to_base(adjustments, frame, x, y);
    (base.0 / unit, base.1 / unit)
}

/// Points per edge of `box_outline_shown`'s outline: enough for lens
/// distortion to bend a face-sized box visibly.
const OUTLINE_STEPS: usize = 4;

/// Where the photo `adjustments` describe (base image `width` × `height`)
/// shows a box given as fractions `[x, y, width, height]` of `region` of
/// its base image (the whole of it when None) — a face found on the camera's
/// own image of the photograph, upright and uncropped (`face_eyes`; a RAW's
/// embedded JPEG shows the camera's aspect crop of a whole-sensor frame,
/// `camera_crop::camera_image_region`): its outline in mask-frame pixels, `OUTLINE_STEPS` points
/// per edge from the top left along the top edge, so turns, flips,
/// straightening, transforms and lens distortion carry it as they carry the
/// photo (`to_mask`, as the red eye corrections are shown). None when the
/// geometry cannot show a point of it.
pub fn box_outline_shown(
    adjustments: &Value,
    width: u32,
    height: u32,
    bbox: [f64; 4],
    region: Option<crate::camera_crop::CameraCrop>,
) -> Option<Vec<(f64, f64)>> {
    let (ox, oy, w, h) = region.map_or((0.0, 0.0, f64::from(width), f64::from(height)), |r| {
        (
            f64::from(r.x),
            f64::from(r.y),
            f64::from(r.width),
            f64::from(r.height),
        )
    });
    let [x0, y0] = [ox + bbox[0] * w, oy + bbox[1] * h];
    let [x1, y1] = [ox + (bbox[0] + bbox[2]) * w, oy + (bbox[1] + bbox[3]) * h];
    let corners = [(x0, y0), (x1, y0), (x1, y1), (x0, y1)];
    let frame = mask_frame(adjustments, width, height);
    (0..4)
        .flat_map(|edge| {
            let (a, b) = (corners[edge], corners[(edge + 1) % 4]);
            (0..OUTLINE_STEPS).map(move |k| {
                let t = k as f64 / OUTLINE_STEPS as f64;
                (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t)
            })
        })
        .map(|p| to_mask(adjustments, frame, p))
        .collect()
}

/// Finds the pupil inside the circle of `radius` mask-frame pixels around
/// mask-frame (`x`, `y`): for red eyes the connected area around the
/// reddest point whose redness is more than half-way from the rim's to that
/// point's, for pet eyes the same with the brightness of the glow; holes
/// (catchlights) are filled. `linear`: the base image holds linear values
/// (RAW files), else sRGB-encoded ones. The ellipse is returned in the
/// photo as decoded (see `RedEye`). None when nothing red (or glowing)
/// enough is there, when the area reaches most of the rim (a red surface,
/// not a pupil) or when it is a sliver.
pub fn detect(
    base: &DynamicImage,
    adjustments: &Value,
    x: f64,
    y: f64,
    radius: f64,
    kind: EyeKind,
    linear: bool,
) -> Option<Pupil> {
    let image = base.to_rgb32f();
    let (w, h) = image.dimensions();
    let frame = mask_frame(adjustments, w, h);
    let centre = to_base(adjustments, frame, x, y);
    // The circle in base pixels: the radius by the geometry's mean scale
    // there.
    let d = derivative(adjustments, frame, x, y);
    let r = radius * (d[0][0] * d[1][1] - d[0][1] * d[1][0]).abs().sqrt();
    if r.is_nan() || r < 2.0 {
        return None;
    }
    let x0 = (centre.0 - r).floor().max(0.0) as i64;
    let y0 = (centre.1 - r).floor().max(0.0) as i64;
    let x1 = ((centre.0 + r).ceil() as i64).min(w as i64 - 1);
    let y1 = ((centre.1 + r).ceil() as i64).min(h as i64 - 1);
    if x0 > x1 || y0 > y1 {
        return None;
    }
    let bw = (x1 - x0 + 1) as usize;
    let bh = (y1 - y0 + 1) as usize;
    let at = |i: usize| (x0 as f64 + (i % bw) as f64, y0 as f64 + (i / bw) as f64);
    let distance = |i: usize| {
        let (px, py) = at(i);
        (px - centre.0).hypot(py - centre.1)
    };
    let inside: Vec<bool> = (0..bw * bh).map(|i| distance(i) <= r).collect();
    // Log brightness (the largest channel) and the score: redness for red
    // eyes, brightness for pet eyes.
    let (bright, score): (Vec<f32>, Vec<f32>) = (0..bw * bh)
        .map(|i| {
            let (px, py) = at(i);
            let mut p = image.get_pixel(px as u32, py as u32).0;
            if !linear {
                p = p.map(srgb_to_linear);
            }
            let bright = p[0].max(p[1]).max(p[2]).max(FLOOR).ln();
            match kind {
                EyeKind::Red => (bright, redness(p)),
                EyeKind::Pet => (bright, bright),
            }
        })
        .unzip();
    // 3 × 3 mean of the score inside the circle, so one noisy pixel is not
    // taken for the pupil.
    let smooth: Vec<f32> = (0..bw * bh)
        .map(|i| {
            let (cx, cy) = ((i % bw) as i64, (i / bw) as i64);
            let (mut sum, mut n) = (0.0, 0);
            for dy in -1..=1 {
                for dx in -1..=1 {
                    let (nx, ny) = (cx + dx, cy + dy);
                    if nx >= 0 && ny >= 0 && (nx as usize) < bw && (ny as usize) < bh {
                        let j = ny as usize * bw + nx as usize;
                        if inside[j] {
                            sum += score[j];
                            n += 1;
                        }
                    }
                }
            }
            if n > 0 { sum / n as f32 } else { f32::MIN }
        })
        .collect();
    let mut rim: Vec<f32> = (0..bw * bh)
        .filter(|&i| inside[i] && distance(i) >= 0.85 * r)
        .map(|i| score[i])
        .collect();
    let rim_level = quantile(&mut rim, 0.5)?;
    // The seed: for red eyes the reddest point within 0.75 of the radius;
    // for pet eyes the brightest point within half the radius of the
    // score's 3 × 3 minimum, so a small catchlight (brighter than the glow)
    // is not taken for the pupil.
    let inside = &inside;
    let middle = |reach: f64| (0..bw * bh).filter(move |&i| inside[i] && distance(i) <= reach * r);
    let (seed, level) = match kind {
        EyeKind::Red => {
            let seed =
                middle(0.75).max_by(|&a, &b| smooth[a].total_cmp(&smooth[b]).then(b.cmp(&a)))?;
            if smooth[seed] < MIN_RED_RATIO.ln() || smooth[seed] - rim_level < MIN_RED_CONTRAST.ln()
            {
                return None;
            }
            (seed, smooth[seed])
        }
        EyeKind::Pet => {
            let eroded = |i: usize| {
                let (cx, cy) = ((i % bw) as i64, (i / bw) as i64);
                let mut low = f32::MAX;
                for dy in -1..=1 {
                    for dx in -1..=1 {
                        let (nx, ny) = (cx + dx, cy + dy);
                        if nx >= 0 && ny >= 0 && (nx as usize) < bw && (ny as usize) < bh {
                            low = low.min(score[ny as usize * bw + nx as usize]);
                        }
                    }
                }
                low
            };
            let (seed, level) = middle(0.5)
                .map(|i| (i, eroded(i)))
                .max_by(|a, b| a.1.total_cmp(&b.1).then(b.0.cmp(&a.0)))?;
            (seed, level)
        }
    };
    // The level the area is told from: the rim's for red eyes; for pet eyes
    // the dark around the glow (the iris), since the rim may be bright fur
    // or skin, even brighter than the glow.
    let background = match kind {
        EyeKind::Red => rim_level,
        EyeKind::Pet => {
            let mut all: Vec<f32> = middle(1.0).map(|i| score[i]).collect();
            let dark = quantile(&mut all, 0.25)?;
            if level - dark < MIN_GLOW_CONTRAST.ln() {
                return None;
            }
            dark
        }
    };
    let threshold = background + 0.5 * (level - background);
    let mut region = vec![false; bw * bh];
    let mut stack = vec![seed];
    region[seed] = true;
    let neighbours = |i: usize| {
        let (cx, cy) = (i % bw, i / bw);
        [
            (cx > 0).then(|| i - 1),
            (cx + 1 < bw).then(|| i + 1),
            (cy > 0).then(|| i - bw),
            (cy + 1 < bh).then(|| i + bw),
        ]
    };
    while let Some(i) = stack.pop() {
        for j in neighbours(i).into_iter().flatten() {
            if !region[j] && inside[j] && score[j] >= threshold {
                region[j] = true;
                stack.push(j);
            }
        }
    }
    // A catchlight in a red pupil is not red but brighter than the pupil:
    // brighter pixels joined to the area belong to it, unless they reach
    // the rim (bright skin).
    if kind == EyeKind::Red {
        let mut levels: Vec<f32> = (0..bw * bh)
            .filter(|&i| region[i])
            .map(|i| bright[i])
            .collect();
        let light = quantile(&mut levels, 0.9)?;
        let mut grown = vec![false; bw * bh];
        let mut stack: Vec<usize> = (0..bw * bh).filter(|&i| region[i]).collect();
        while let Some(i) = stack.pop() {
            for j in neighbours(i).into_iter().flatten() {
                if !region[j] && !grown[j] && inside[j] && bright[j] > light {
                    grown[j] = true;
                    stack.push(j);
                }
            }
        }
        if !(0..bw * bh).any(|i| grown[i] && distance(i) >= 0.9 * r) {
            for i in 0..bw * bh {
                region[i] |= grown[i];
            }
        }
    }
    // A pupil is convex: what lies within the hull of the area belongs to
    // it (a catchlight inside it or biting into its edge, a dark speck).
    let hull = convex_hull(
        (0..bw * bh)
            .filter(|&i| region[i])
            .map(|i| ((i % bw) as i64, (i / bw) as i64))
            .collect(),
    );
    let pupil: Vec<usize> = (0..bw * bh)
        .filter(|&i| region[i] || within(&hull, ((i % bw) as i64, (i / bw) as i64)))
        .collect();
    let rim_count = (0..bw * bh)
        .filter(|&i| inside[i] && distance(i) >= 0.9 * r)
        .count();
    let on_rim = pupil.iter().filter(|&&i| distance(i) >= 0.9 * r).count();
    if pupil.len() < 3 || on_rim * 5 > rim_count * 2 {
        return None;
    }
    // The ellipse of the area's second moments (a filled ellipse's variance
    // along an axis is a quarter of its squared semi-axis).
    let n = pupil.len() as f64;
    let (mx, my) = pupil.iter().fold((0.0, 0.0), |(sx, sy), &i| {
        let (px, py) = at(i);
        (sx + px / n, sy + py / n)
    });
    let (mut sxx, mut syy, mut sxy) = (0.0, 0.0, 0.0);
    for &i in &pupil {
        let (px, py) = at(i);
        let (dx, dy) = (px - mx, py - my);
        sxx += dx * dx / n;
        syy += dy * dy / n;
        sxy += dx * dy / n;
    }
    // Each pixel is a unit square, not a point: its own variance (1/12 per
    // axis) belongs to the area.
    sxx += 1.0 / 12.0;
    syy += 1.0 / 12.0;
    let mean = (sxx + syy) / 2.0;
    let spread = (((sxx - syy) / 2.0).powi(2) + sxy * sxy).sqrt();
    let (l1, l2) = (mean + spread, (mean - spread).max(1e-6));
    let phi = 0.5 * (2.0 * sxy).atan2(sxx - syy);
    let (a, b) = (2.0 * l1.sqrt(), 2.0 * l2.sqrt());
    // A sliver (red between dark letters, a reflection on a rim) is no
    // pupil: a pupil is at least a pixel across, and a red one is round
    // (a slit-pupilled animal's glow may be long).
    if b < 1.0 || (kind == EyeKind::Red && a > 2.5 * b) {
        return None;
    }

    let unit = f64::from(w.max(h));
    Some(Pupil {
        x: mx / unit,
        y: my / unit,
        radius_x: a / unit,
        radius_y: b / unit,
        angle: phi.to_degrees(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const SKIN: [f32; 3] = [0.45, 0.25, 0.17];
    const IRIS: [f32; 3] = [0.08, 0.045, 0.025];
    const RED_PUPIL: [f32; 3] = [0.5, 0.02, 0.025];
    /// The synthetic photos' long edge.
    const UNIT: f64 = 80.0;

    /// Coverage of a pixel by an ellipse (4 × 4 supersampling).
    fn covered(x: u32, y: u32, (cx, cy): (f32, f32), (a, b): (f32, f32), angle: f32) -> f32 {
        let (s, c) = angle.to_radians().sin_cos();
        let mut hits = 0;
        for sy in 0..4 {
            for sx in 0..4 {
                let px = x as f32 + (sx as f32 + 0.5) / 4.0 - 0.5 - cx;
                let py = y as f32 + (sy as f32 + 0.5) / 4.0 - 0.5 - cy;
                let (u, v) = (px * c + py * s, -px * s + py * c);
                if (u / a).powi(2) + (v / b).powi(2) <= 1.0 {
                    hits += 1;
                }
            }
        }
        hits as f32 / 16.0
    }

    fn mix(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
        std::array::from_fn(|i| a[i] + (b[i] - a[i]) * t)
    }

    /// A synthetic eye on `around` (skin, fur) with fine grain: iris of
    /// radius 11, a pupil ellipse of `pupil` colour and semi-axes `axes`
    /// turned by `angle`, a white catchlight of radius 1.2 up left of its
    /// centre.
    fn eye_on(
        around: [f32; 3],
        pupil: [f32; 3],
        centre: (f32, f32),
        axes: (f32, f32),
        angle: f32,
    ) -> Rgb32FImage {
        Rgb32FImage::from_fn(80, 64, |x, y| {
            let grain = 1.0 + (((x * 7919 + y * 104_729) % 97) as f32 / 97.0 - 0.5) * 0.04;
            let iris = covered(x, y, centre, (11.0, 11.0), 0.0);
            let mut p = mix(around, IRIS, iris);
            p = mix(p, pupil, covered(x, y, centre, axes, angle));
            let light = (centre.0 - 0.3 * axes.0, centre.1 - 0.35 * axes.1);
            p = mix(p, [0.9, 0.9, 0.9], covered(x, y, light, (1.2, 1.2), 0.0));
            image::Rgb(p.map(|v| v * grain))
        })
    }

    fn eye(pupil: [f32; 3], centre: (f32, f32), axes: (f32, f32), angle: f32) -> Rgb32FImage {
        eye_on(SKIN, pupil, centre, axes, angle)
    }

    fn linear_eye(pupil: [f32; 3], axes: (f32, f32), angle: f32) -> DynamicImage {
        DynamicImage::ImageRgb32F(eye(pupil, (40.3, 31.6), axes, angle))
    }

    /// A found pupil in pixels of the synthetic photo.
    fn pixels(p: Pupil) -> Pupil {
        Pupil {
            x: p.x * UNIT,
            y: p.y * UNIT,
            radius_x: p.radius_x * UNIT,
            radius_y: p.radius_y * UNIT,
            angle: p.angle,
        }
    }

    /// Angle difference of two ellipse axes (degrees, modulo 180).
    fn turn(a: f64, b: f64) -> f64 {
        let d = (a - b).rem_euclid(180.0);
        d.min(180.0 - d)
    }

    #[test]
    fn finds_a_round_red_pupil_with_its_catchlight() {
        let image = linear_eye(RED_PUPIL, (5.0, 5.0), 0.0);
        // Clicked off centre, with a circle reaching into the skin.
        let found =
            pixels(detect(&image, &json!({}), 42.0, 30.0, 15.0, EyeKind::Red, true).unwrap());
        assert!(
            (found.x - 40.3).abs() < 0.3 && (found.y - 31.6).abs() < 0.3,
            "{found:?}"
        );
        assert!((found.radius_x - 5.0).abs() < 0.4, "{found:?}");
        assert!((found.radius_y - 5.0).abs() < 0.4, "{found:?}");
    }

    #[test]
    fn finds_a_tilted_elliptic_pupil() {
        let image = linear_eye(RED_PUPIL, (7.0, 4.0), 30.0);
        let found =
            pixels(detect(&image, &json!({}), 40.0, 32.0, 14.0, EyeKind::Red, true).unwrap());
        assert!(
            (found.x - 40.3).abs() < 0.3 && (found.y - 31.6).abs() < 0.3,
            "{found:?}"
        );
        assert!((found.radius_x - 7.0).abs() < 0.5, "{found:?}");
        assert!((found.radius_y - 4.0).abs() < 0.5, "{found:?}");
        assert!(turn(found.angle, 30.0) < 4.0, "{found:?}");
    }

    #[test]
    fn finds_a_tiny_pupil() {
        let image = linear_eye(RED_PUPIL, (2.0, 2.0), 0.0);
        let found =
            pixels(detect(&image, &json!({}), 40.0, 31.0, 8.0, EyeKind::Red, true).unwrap());
        assert!((found.radius_x - 2.0).abs() < 0.5, "{found:?}");
    }

    #[test]
    fn finds_nothing_in_a_dark_eye_or_on_skin() {
        let dark = linear_eye([0.01, 0.008, 0.008], (5.0, 5.0), 0.0);
        assert_eq!(
            detect(&dark, &json!({}), 40.0, 31.0, 15.0, EyeKind::Red, true),
            None
        );
        // A red-brown iris (red 2.4 × green) without a red pupil.
        let brown = DynamicImage::ImageRgb32F(Rgb32FImage::from_fn(60, 60, |x, y| {
            let iris = covered(x, y, (30.0, 30.0), (11.0, 11.0), 0.0);
            image::Rgb(mix(SKIN, [0.12, 0.05, 0.03], iris))
        }));
        assert_eq!(
            detect(&brown, &json!({}), 30.0, 30.0, 15.0, EyeKind::Red, true),
            None
        );
        // A red surface filling the circle is no pupil.
        let red = DynamicImage::ImageRgb32F(Rgb32FImage::from_pixel(60, 60, image::Rgb(RED_PUPIL)));
        assert_eq!(
            detect(&red, &json!({}), 30.0, 30.0, 10.0, EyeKind::Red, true),
            None
        );
        // Nor is a thin red gap between dark shapes.
        let gap = DynamicImage::ImageRgb32F(Rgb32FImage::from_fn(60, 60, |x, y| {
            let dark = (x as i32 - 30).abs() >= 2 && (y as i32 - 30).abs() < 8;
            image::Rgb(if dark { [0.01, 0.01, 0.02] } else { RED_PUPIL })
        }));
        assert_eq!(
            detect(&gap, &json!({}), 30.0, 30.0, 12.0, EyeKind::Red, true),
            None
        );
    }

    fn srgb_encode(v: f32) -> f32 {
        if v <= 0.003_130_8 {
            v * 12.92
        } else {
            1.055 * v.powf(1.0 / 2.4) - 0.055
        }
    }

    #[test]
    fn finds_a_red_pupil_in_srgb_encoded_values() {
        let linear = eye(RED_PUPIL, (40.3, 31.6), (5.0, 5.0), 0.0);
        let encoded = Rgb32FImage::from_fn(80, 64, |x, y| {
            image::Rgb(linear.get_pixel(x, y).0.map(srgb_encode))
        });
        let found = pixels(
            detect(
                &DynamicImage::ImageRgb32F(encoded),
                &json!({}),
                41.0,
                31.0,
                15.0,
                EyeKind::Red,
                false,
            )
            .unwrap(),
        );
        assert!((found.radius_x - 5.0).abs() < 0.4, "{found:?}");
    }

    #[test]
    fn finds_a_pets_glowing_pupil_of_any_colour() {
        for glow in [[0.3, 0.7, 0.35], [0.75, 0.7, 0.3], [0.8, 0.8, 0.8]] {
            let image = linear_eye(glow, (6.0, 6.0), 0.0);
            let found =
                pixels(detect(&image, &json!({}), 41.0, 31.0, 16.0, EyeKind::Pet, true).unwrap());
            assert!(
                (found.x - 40.3).abs() < 0.4 && (found.y - 31.6).abs() < 0.4,
                "{glow:?} {found:?}"
            );
            assert!((found.radius_x - 6.0).abs() < 0.6, "{glow:?} {found:?}");
        }
        let dark = linear_eye([0.01, 0.01, 0.01], (6.0, 6.0), 0.0);
        assert_eq!(
            detect(&dark, &json!({}), 41.0, 31.0, 16.0, EyeKind::Pet, true),
            None
        );
    }

    #[test]
    fn finds_a_pets_glow_in_fur_brighter_than_the_glow() {
        // White fur (0.95) around a dark iris around a 0.7 glow.
        let image = DynamicImage::ImageRgb32F(eye_on(
            [0.95, 0.95, 0.95],
            [0.7, 0.68, 0.6],
            (40.3, 31.6),
            (6.0, 6.0),
            0.0,
        ));
        let found =
            pixels(detect(&image, &json!({}), 41.0, 31.0, 16.0, EyeKind::Pet, true).unwrap());
        assert!((found.radius_x - 6.0).abs() < 0.6, "{found:?}");
    }

    /// The mask-frame point that shows base pixel `target` (by search).
    fn shown_point(adjustments: &Value, target: (f64, f64)) -> (f64, f64) {
        let frame = mask_frame(adjustments, 80, 64);
        let mut best = (0.0, 0.0, f64::MAX);
        for i in 0..(frame.0 as usize * 10) {
            for j in 0..(frame.1 as usize * 10) {
                let (mx, my) = (i as f64 / 10.0, j as f64 / 10.0);
                let b = to_base(adjustments, frame, mx, my);
                let d = (b.0 - target.0).hypot(b.1 - target.1);
                if d < best.2 {
                    best = (mx, my, d);
                }
            }
        }
        (best.0, best.1)
    }

    #[test]
    fn a_turned_photo_finds_and_shows_the_pupil_where_it_is() {
        // The base image is turned a quarter (orientation step 1): the mask
        // frame shows it upright, 64 × 80.
        let upright = DynamicImage::ImageRgb32F(eye(RED_PUPIL, (40.3, 31.6), (7.0, 4.0), 0.0));
        let adjustments = json!({"orientationSteps": 1});
        let click = shown_point(&adjustments, (40.3, 31.6));
        let found = detect(
            &upright,
            &adjustments,
            click.0,
            click.1,
            14.0,
            EyeKind::Red,
            true,
        )
        .unwrap();
        // Kept in the photo as decoded, whatever the turn.
        let kept = pixels(found);
        assert!(
            (kept.x - 40.3).abs() < 0.3 && (kept.y - 31.6).abs() < 0.3,
            "{kept:?}"
        );
        assert!(turn(kept.angle, 0.0) < 4.0, "{kept:?}");
        // Shown upright: the long axis along the mask frame's y.
        let on_screen = shown(&adjustments, 80, 64, &found).unwrap();
        assert!((on_screen.x - click.0).abs() < 0.2 && (on_screen.y - click.1).abs() < 0.2);
        assert!(turn(on_screen.angle, 90.0) < 4.0, "{on_screen:?}");
        assert!((on_screen.radius_x - 7.0).abs() < 0.5, "{on_screen:?}");
        // A drag's end point back in the photo.
        let back = image_point(&adjustments, 80, 64, on_screen.x, on_screen.y);
        assert!((back.0 - found.x).abs() < 1e-3 && (back.1 - found.y).abs() < 1e-3);
    }

    #[test]
    fn corrections_are_shown_anywhere_on_a_full_size_photo() {
        // to_base computes in f32: far from the origin it resolves only
        // fractions of a thousandth of a pixel, and the shown centre must
        // still be found.
        for adjustments in [
            json!({}),
            json!({"rotation": 3.0, "orientationSteps": 1, "transformVertical": 15.0}),
        ] {
            for (x, y) in [
                (5000.2, 2000.0),
                (5999.7, 3999.3),
                (0.4, 0.6),
                (3001.3, 1999.9),
            ] {
                let stored = Pupil {
                    x: x / 6000.0,
                    y: y / 6000.0,
                    radius_x: 10.0 / 6000.0,
                    radius_y: 8.0 / 6000.0,
                    angle: 0.0,
                };
                let on_screen = shown(&adjustments, 6000, 4000, &stored)
                    .unwrap_or_else(|| panic!("{adjustments} ({x}, {y})"));
                let back = image_point(&adjustments, 6000, 4000, on_screen.x, on_screen.y);
                assert!(
                    (back.0 * 6000.0 - x).abs() < 0.02 && (back.1 * 6000.0 - y).abs() < 0.02,
                    "{adjustments} ({x}, {y}): {back:?}"
                );
            }
        }
    }

    #[test]
    fn a_face_box_is_shown_where_the_geometry_puts_it() {
        // A face box (fractions of the upright base image, 80 × 64) carried
        // through a turn, a flip, straightening, a transform and lens
        // distortion: every outline point lands back on the box's edge.
        let bbox = [0.25, 0.3, 0.2, 0.25];
        let edge = |(x, y): (f64, f64)| {
            let (x0, y0, x1, y1) = (20.0, 19.2, 36.0, 35.2);
            let on_x =
                ((x - x0).abs() < 0.02 || (x - x1).abs() < 0.02) && y > y0 - 0.02 && y < y1 + 0.02;
            let on_y =
                ((y - y0).abs() < 0.02 || (y - y1).abs() < 0.02) && x > x0 - 0.02 && x < x1 + 0.02;
            on_x || on_y
        };
        for adjustments in [
            json!({}),
            json!({"orientationSteps": 1}),
            json!({"orientationSteps": 3, "flipHorizontal": true, "rotation": 8.0}),
            json!({"transformVertical": 20.0, "transformAspect": 30.0, "rotation": -5.0}),
            json!({"lensDistortionParams": {"k1": -0.1, "k2": 0.02, "k3": 0.0, "model": 0}}),
        ] {
            let outline = box_outline_shown(&adjustments, 80, 64, bbox, None).unwrap();
            assert_eq!(outline.len(), 4 * OUTLINE_STEPS);
            let frame = mask_frame(&adjustments, 80, 64);
            for &p in &outline {
                let b = to_base(&adjustments, frame, p.0, p.1);
                assert!(edge(b), "{adjustments}: {p:?} -> {b:?}");
            }
            // The first point is the box's top left as the geometry shows it.
            let corner = shown_point(&adjustments, (20.0, 19.2));
            assert!(
                (outline[0].0 - corner.0).abs() < 0.15 && (outline[0].1 - corner.1).abs() < 0.15
            );
        }
        // Without geometry, the box itself; lens distortion moves it.
        let plain = box_outline_shown(&json!({}), 80, 64, bbox, None).unwrap();
        let lens = json!({"lensDistortionParams": {"k1": -0.1, "k2": 0.02, "k3": 0.0, "model": 0}});
        let bent = box_outline_shown(&lens, 80, 64, bbox, None).unwrap();
        assert!(
            plain
                .iter()
                .zip(&bent)
                .any(|(a, b)| (a.0 - b.0).hypot(a.1 - b.1) > 0.05)
        );
        // Read on a camera image that shows a centred 64 × 64 crop of the
        // frame: the fractions are of that crop.
        let square = crate::camera_crop::CameraCrop {
            x: 8,
            y: 0,
            width: 64,
            height: 64,
        };
        let in_crop = box_outline_shown(&json!({}), 80, 64, bbox, Some(square)).unwrap();
        assert!((in_crop[0].0 - 24.0).abs() < 0.01 && (in_crop[0].1 - 19.2).abs() < 0.01);
        assert!(
            (in_crop[OUTLINE_STEPS * 2].0 - 36.8).abs() < 0.01
                && (in_crop[OUTLINE_STEPS * 2].1 - 35.2).abs() < 0.01
        );
        assert!(
            (plain[OUTLINE_STEPS * 2].0 - 36.0).abs() < 0.01
                && (plain[OUTLINE_STEPS * 2].1 - 35.2).abs() < 0.01
        );
    }

    #[test]
    fn shown_ellipse_follows_a_stretching_transform() {
        // Aspect, perspective and a turn stretch and shear the photo: the
        // shown ellipse's rim lands on the stored ellipse's rim.
        let adjustments = json!({"transformAspect": 60.0, "transformVertical": 20.0,
            "transformXOffset": 5.0, "rotation": 7.0, "flipHorizontal": true});
        let stored = Pupil {
            x: 30.0 / UNIT,
            y: 25.0 / UNIT,
            radius_x: 6.0 / UNIT,
            radius_y: 3.0 / UNIT,
            angle: 25.0,
        };
        let on_screen = shown(&adjustments, 80, 64, &stored).unwrap();
        let frame = mask_frame(&adjustments, 80, 64);
        let (s, c) = on_screen.angle.to_radians().sin_cos();
        let (bs, bc) = stored.angle.to_radians().sin_cos();
        for k in 0..16 {
            let t = std::f64::consts::TAU * k as f64 / 16.0;
            let (u, v) = (on_screen.radius_x * t.cos(), on_screen.radius_y * t.sin());
            let rim = (on_screen.x + u * c - v * s, on_screen.y + u * s + v * c);
            let b = to_base(&adjustments, frame, rim.0, rim.1);
            let (dx, dy) = (b.0 - stored.x * UNIT, b.1 - stored.y * UNIT);
            let (bu, bv) = (dx * bc + dy * bs, -dx * bs + dy * bc);
            let rho = (bu / (stored.radius_x * UNIT)).hypot(bv / (stored.radius_y * UNIT));
            assert!((rho - 1.0).abs() < 0.03, "{k}: {rho}");
        }
    }

    fn correction(kind: &str) -> Value {
        json!({"redEyes": [{"version": 1, "kind": kind, "x": 40.3 / UNIT, "y": 31.6 / UNIT,
            "radiusX": 5.0 / UNIT, "radiusY": 5.0 / UNIT, "angle": 0.0, "pupilSize": 50,
            "darken": 50, "catchlight": kind == "pet", "catchlightX": -0.3, "catchlightY": -0.35,
            "visible": true}]})
    }

    #[test]
    fn corrected_pupil_is_dark_neutral_and_the_rest_untouched() {
        let image = linear_eye(RED_PUPIL, (5.0, 5.0), 0.0);
        let before = image.to_rgb32f();
        let after = apply_red_eyes(&image, &correction("red"))
            .unwrap()
            .to_rgb32f();
        let centre = after.get_pixel(41, 33).0;
        assert!(centre[0] < 1.3 * centre[1].max(centre[2]), "{centre:?}");
        assert!(centre[0] < 0.03, "{centre:?}");
        // The catchlight stays white.
        let light = after.get_pixel(39, 30).0;
        assert!(light[0] > 0.6 && light[1] > 0.6, "{light:?}");
        // Skin and iris outside the pupil keep their exact values.
        for (x, y) in [(5, 5), (40, 22), (31, 31), (50, 31)] {
            assert_eq!(after.get_pixel(x, y), before.get_pixel(x, y), "({x}, {y})");
        }
        // The geometry does not move it: it stays on the eye of the photo.
        let mut turned = correction("red");
        turned["orientationSteps"] = json!(1);
        turned["rotation"] = json!(12.0);
        let same = apply_red_eyes(&image, &turned).unwrap().to_rgb32f();
        assert_eq!(same, after);
        // Darken 0 takes only the colour out; more darkens.
        let mut light_recipe = correction("red");
        light_recipe["redEyes"][0]["darken"] = json!(0);
        let neutral = apply_red_eyes(&image, &light_recipe).unwrap().to_rgb32f();
        let mut dark_recipe = correction("red");
        dark_recipe["redEyes"][0]["darken"] = json!(100);
        let darker = apply_red_eyes(&image, &dark_recipe).unwrap().to_rgb32f();
        let level = |im: &Rgb32FImage| im.get_pixel(41, 33).0.iter().sum::<f32>();
        assert!(level(&neutral) > level(&after) && level(&after) > level(&darker));
    }

    #[test]
    fn pet_eye_turns_the_glow_dark_and_adds_a_catchlight() {
        let image = linear_eye([0.3, 0.7, 0.35], (5.0, 5.0), 0.0);
        let after = apply_red_eyes(&image, &correction("pet"))
            .unwrap()
            .to_rgb32f();
        let centre = after.get_pixel(41, 33).0;
        assert!(centre.iter().all(|&c| c < 0.03), "{centre:?}");
        assert!((centre[0] - centre[1]).abs() < 1e-4, "{centre:?}");
        // The catchlight at (-0.3, -0.35) of the radius: (38.8, 29.85).
        let light = after.get_pixel(39, 30).0;
        assert!(light.iter().all(|&c| c > 0.3), "{light:?}");
        let mut without = correction("pet");
        without["redEyes"][0]["catchlight"] = json!(false);
        let plain = apply_red_eyes(&image, &without).unwrap().to_rgb32f();
        assert!(plain.get_pixel(39, 30).0.iter().all(|&c| c < 0.05));
    }

    #[test]
    fn a_saturated_glow_turns_dark_and_its_catchlight_bright() {
        // A pure green glow on a neutral grey, no catchlight of its own.
        let image = DynamicImage::ImageRgb32F(Rgb32FImage::from_fn(80, 64, |x, y| {
            let glow = covered(x, y, (40.3, 31.6), (5.0, 5.0), 0.0);
            image::Rgb(mix([0.04, 0.04, 0.04], [0.001, 0.7, 0.001], glow))
        }));
        let after = apply_red_eyes(&image, &correction("pet"))
            .unwrap()
            .to_rgb32f();
        let pupil = after.get_pixel(41, 33).0;
        assert!(pupil.iter().all(|&c| c < 0.01), "{pupil:?}");
        let light = after.get_pixel(39, 30).0;
        assert!(light.iter().all(|&c| c > 0.3), "{light:?}");
        // Darken still darkens.
        let mut darker = correction("pet");
        darker["redEyes"][0]["darken"] = json!(100);
        let darkest = apply_red_eyes(&image, &darker).unwrap().to_rgb32f();
        assert!(darkest.get_pixel(41, 33)[0] < pupil[0]);
    }

    #[test]
    fn renders_alike_in_either_encoding() {
        let linear = eye(RED_PUPIL, (40.3, 31.6), (5.0, 5.0), 0.0);
        let pet_eye = eye([0.3, 0.7, 0.35], (40.3, 31.6), (5.0, 5.0), 0.0);
        // Corrected in an encoding, decoded, against corrected in linear
        // light: the largest difference in linear values and as 8-bit sRGB
        // codes.
        let compare = |image: &Rgb32FImage,
                       kind: &str,
                       encode: &dyn Fn(f32) -> f32,
                       decode: &dyn Fn(f32) -> f32| {
            let encoded = Rgb32FImage::from_fn(80, 64, |x, y| {
                image::Rgb(image.get_pixel(x, y).0.map(encode))
            });
            let a = apply_red_eyes(&DynamicImage::ImageRgb32F(image.clone()), &correction(kind))
                .unwrap()
                .to_rgb32f();
            let b = apply_red_eyes(&DynamicImage::ImageRgb32F(encoded), &correction(kind))
                .unwrap()
                .to_rgb32f();
            let (mut linear_gap, mut codes, mut beyond) = (0.0f32, 0.0f32, 0);
            for (pa, pb) in a.pixels().zip(b.pixels()) {
                for c in 0..3 {
                    let decoded = decode(pb[c]);
                    linear_gap = linear_gap.max((decoded - pa[c]).abs());
                    let cd = (srgb_encode(decoded) - srgb_encode(pa[c])).abs() * 255.0;
                    codes = codes.max(cd);
                    if cd > 3.0 {
                        beyond += 1;
                    }
                }
            }
            (linear_gap, codes, beyond)
        };
        // A power law commutes with the correction: equal within float
        // rounding.
        let gamma = 2.2f32;
        for (image, kind) in [(&linear, "red"), (&pet_eye, "pet")] {
            let (gap, _, _) = compare(image, kind, &|v| v.powf(1.0 / gamma), &|v| v.powf(gamma));
            assert!(gap < 2e-4, "{kind}: {gap}");
        }
        // sRGB is not a power law (its offset and linear toe): measured,
        // red eyes within 3 of 255 codes but for pixels whose redness lies
        // at the gate's edge (2 of the eye's 5120, the catchlight's pink
        // rim, up to 14 codes); a pet eye's black pupil lies in the toe
        // (up to 5.5 codes).
        let (_, codes, beyond) = compare(&linear, "red", &srgb_encode, &srgb_to_linear);
        assert!(beyond <= 2 && codes < 15.0, "red: {codes} {beyond}");
        let (_, codes, beyond) = compare(&pet_eye, "pet", &srgb_encode, &srgb_to_linear);
        assert!(codes < 6.0, "pet: {codes} {beyond}");
    }

    #[test]
    fn pupil_size_widens_the_correction() {
        let image = linear_eye(RED_PUPIL, (5.0, 5.0), 0.0);
        let mut small = correction("red");
        small["redEyes"][0]["pupilSize"] = json!(0);
        // The pupil's edge at 0.6 × the ellipse keeps its red; at 50 it does not.
        let edge = (44, 32);
        let red = |im: &Rgb32FImage| {
            let p = im.get_pixel(edge.0, edge.1).0;
            p[0] / p[1].max(p[2])
        };
        let kept = apply_red_eyes(&image, &small).unwrap().to_rgb32f();
        let taken = apply_red_eyes(&image, &correction("red"))
            .unwrap()
            .to_rgb32f();
        assert!(
            red(&kept) > 5.0 && red(&taken) < 2.0,
            "{} {}",
            red(&kept),
            red(&taken)
        );
    }

    #[test]
    fn a_pupil_of_a_pixel_in_a_reduced_copy_is_still_corrected() {
        // A 3 px pupil at a fifth of the size: 0.6 px, its centre on the
        // corner of four pixels (8.5, 6.5), so no pixel centre lies within
        // 0.8 of its radius.
        let full = DynamicImage::ImageRgb32F(eye(RED_PUPIL, (42.5, 32.5), (3.0, 3.0), 0.0));
        let small = full.resize_exact(16, 13, image::imageops::FilterType::Triangle);
        let recipe = json!({"redEyes": [{"x": 42.5 / UNIT, "y": 32.5 / UNIT,
            "radiusX": 3.0 / UNIT, "radiusY": 3.0 / UNIT}]});
        let before = small.to_rgb32f();
        let after = apply_red_eyes(&small, &recipe).unwrap().to_rgb32f();
        let red = |p: [f32; 3]| p[0] / p[1].max(p[2]);
        for (x, y) in [(8, 6), (9, 6), (8, 7), (9, 7)] {
            let (b, a) = (before.get_pixel(x, y).0, after.get_pixel(x, y).0);
            assert!(red(a) < 0.8 * red(b), "({x}, {y}): {b:?} → {a:?}");
        }
    }

    #[test]
    fn a_reduced_copy_is_corrected_at_its_place() {
        // The same recipe on a copy at half size (units of the long edge).
        let full = linear_eye(RED_PUPIL, (6.0, 6.0), 0.0);
        let half = full.resize_exact(40, 32, image::imageops::FilterType::Triangle);
        let mut recipe = correction("red");
        recipe["redEyes"][0]["radiusX"] = json!(6.0 / UNIT);
        recipe["redEyes"][0]["radiusY"] = json!(6.0 / UNIT);
        let after = apply_red_eyes(&half, &recipe).unwrap().to_rgb32f();
        let p = after.get_pixel(20, 16).0;
        assert!(p[0] < 1.3 * p[1].max(p[2]), "{p:?}");
    }

    /// Detector and correction on real photographs (never committed):
    /// `RED_EYE_PHOTOS` is a JSON manifest, a list of `{"file", "x", "y",
    /// "radius", "kind", "eye"}` (`file` relative to the manifest, a click
    /// in pixels of the upright photo, `eye`: whether a pupil to correct is
    /// there). Prints what was found; with `RED_EYE_OUT` (an empty folder)
    /// it writes a crop of each eye before and after the correction there,
    /// never over an existing file.
    /// `RED_EYE_PHOTOS=… cargo test -p rawtonic-engine --lib red_eye::tests::photos -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn photos() {
        let manifest = std::path::PathBuf::from(std::env::var("RED_EYE_PHOTOS").unwrap());
        let out = std::env::var("RED_EYE_OUT")
            .ok()
            .map(std::path::PathBuf::from);
        let items: Vec<Value> =
            serde_json::from_str(&std::fs::read_to_string(&manifest).unwrap()).unwrap();
        let mut wrong = 0;
        for (n, item) in items.iter().enumerate() {
            let file = manifest
                .parent()
                .unwrap()
                .join(item["file"].as_str().unwrap());
            // Non-RAW bases hold sRGB-encoded values.
            let image = DynamicImage::ImageRgb32F(image::open(&file).unwrap().to_rgb32f());
            let kind: EyeKind = serde_json::from_value(item["kind"].clone()).unwrap();
            let (x, y, radius) = (
                item["x"].as_f64().unwrap(),
                item["y"].as_f64().unwrap(),
                item["radius"].as_f64().unwrap(),
            );
            let found = detect(&image, &json!({}), x, y, radius, kind, false);
            let expected = item["eye"].as_bool().unwrap();
            if found.is_some() != expected {
                wrong += 1;
            }
            println!("{n} {}: {found:?} (expected {expected})", file.display());
            let (Some(pupil), Some(out)) = (found, &out) else {
                continue;
            };
            let recipe = json!({"redEyes": [{"kind": kind, "x": pupil.x, "y": pupil.y,
                "radiusX": pupil.radius_x, "radiusY": pupil.radius_y, "angle": pupil.angle,
                "catchlight": kind == EyeKind::Pet,
                "catchlightX": DEFAULT_CATCHLIGHT.0, "catchlightY": DEFAULT_CATCHLIGHT.1}]});
            let after = apply_red_eyes(&image, &recipe).unwrap();
            let r = (radius * 1.5) as u32;
            let (cx, cy) = (x as u32, y as u32);
            let crop = |im: &DynamicImage| {
                im.crop_imm(cx.saturating_sub(r), cy.saturating_sub(r), 2 * r, 2 * r)
                    .to_rgb8()
            };
            let (before, after) = (crop(&image), crop(&after));
            let mut both = image::RgbImage::new(before.width() * 2, before.height());
            image::imageops::replace(&mut both, &before, 0, 0);
            image::imageops::replace(&mut both, &after, i64::from(before.width()), 0);
            // Never over an existing file: a synced temporary file beside
            // it, published without clobbering.
            let mut png = std::io::Cursor::new(Vec::new());
            both.write_to(&mut png, image::ImageFormat::Png).unwrap();
            let path = out.join(format!("eye_{n}.png"));
            let mut temp = tempfile::Builder::new().tempfile_in(out).unwrap();
            std::io::Write::write_all(&mut temp, png.get_ref()).unwrap();
            temp.as_file().sync_all().unwrap();
            rawtonic_metadata::no_clobber::persist(temp, &path)
                .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        }
        assert_eq!(wrong, 0, "{wrong} photos found wrongly");
    }

    #[test]
    fn recipes_without_known_corrections_leave_the_base_alone() {
        let image = linear_eye(RED_PUPIL, (5.0, 5.0), 0.0);
        assert!(apply_red_eyes(&image, &json!({})).is_none());
        assert!(apply_red_eyes(&image, &json!({"redEyes": []})).is_none());
        let mut hidden = correction("red");
        hidden["redEyes"][0]["visible"] = json!(false);
        assert!(!is_active(&hidden));
        // A later version's correction is not rendered by this build.
        let mut later = correction("red");
        later["redEyes"][0]["version"] = json!(2);
        assert!(!is_active(&later));
        // Older fields fall back to their defaults.
        let minimal = json!({"redEyes": [{"x": 40.3, "y": 31.6, "radiusX": 5.0, "radiusY": 5.0}]});
        let eyes = red_eyes(&minimal);
        assert_eq!(eyes[0].version, 1);
        assert_eq!(eyes[0].kind, EyeKind::Red);
        assert_eq!(eyes[0].pupil_size, 50.0);
    }
}
