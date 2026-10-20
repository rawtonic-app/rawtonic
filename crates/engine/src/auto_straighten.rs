//! Auto straighten: the camera's roll from structural lines (horizons, water
//! lines, architecture verticals), as the crop rotation that levels the photo.
//! Angles here are the content's tilt in image coordinates (y down, positive
//! = clockwise on screen); the recipe's `rotation` that levels it is the
//! negative. Measured against Lightroom's crop Auto and Camera Raw's Upright
//! Level on held-out photographs, thresholds set against a separate tuning
//! half: docs/testing/auto-straighten.md (results and the remaining gaps).
use std::f64::consts::PI;

use crate::upright::{Segment, vanishing_point, wrap_half_turn};

/// The largest tilt (degrees) Auto straighten corrects.
pub const MAX_TILT: f64 = 10.0;

/// Segments within this angle of an axis count as horizontal or vertical.
const NEAR_AXIS: f64 = 20.0 * PI / 180.0;

/// A tilt estimate: the content's tilt (radians) and the line length that
/// supports it, relative to the frame's diagonal.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tilt {
    pub tilt: f64,
    pub support: f64,
}

fn near_axis(segments: &[Segment], diagonal: f64, min_length: f64) -> (Vec<Segment>, Vec<Segment>) {
    let mut horizontals = Vec::new();
    let mut verticals = Vec::new();
    for s in segments {
        if s.length() < min_length * diagonal {
            continue;
        }
        if s.angle().abs() < NEAR_AXIS {
            horizontals.push(*s);
        } else if s.tilt_from_vertical().abs() < NEAR_AXIS {
            verticals.push(*s);
        }
    }
    (horizontals, verticals)
}

/// Baseline: Upright Level's length-weighted median tilt of all near-axis
/// segments (25°, longer than 3 % of the diagonal).
pub fn baseline_tilt(segments: &[Segment], width: f64, height: f64) -> Option<Tilt> {
    let diagonal = width.hypot(height);
    let mut tilts: Vec<(f64, f64)> = segments
        .iter()
        .filter(|s| s.length() > 0.03 * diagonal)
        .filter_map(|s| {
            if s.tilt_from_vertical().abs() < 25f64.to_radians() {
                Some((s.tilt_from_vertical(), s.length()))
            } else if s.angle().abs() < 25f64.to_radians() {
                Some((s.angle(), s.length()))
            } else {
                None
            }
        })
        .collect();
    if tilts.is_empty() {
        return None;
    }
    tilts.sort_by(|a, b| a.0.total_cmp(&b.0));
    let total: f64 = tilts.iter().map(|t| t.1).sum();
    let mut acc = 0.0;
    let tilt = tilts.iter().find(|t| {
        acc += t.1;
        acc >= total / 2.0
    })?;
    Some(Tilt {
        tilt: tilt.0,
        support: total / diagonal,
    })
}

/// The longest straight near-horizontal line: segments that agree in angle
/// (0.3°) and lie on one line (within 0.5 % of the diagonal) — a horizon or a
/// water line, not the parallel but separate edges of waves, floors or
/// receding façades.
fn horizontal_cluster(horizontals: &[Segment], diagonal: f64) -> Option<Tilt> {
    const WINDOW: f64 = 0.3 * PI / 180.0;
    let mut best: Option<(f64, f64)> = None;
    for s in horizontals {
        let centre = s.angle();
        let (mx, my) = s.midpoint();
        let (nx, ny) = (-centre.sin(), centre.cos());
        let members: Vec<&Segment> = horizontals
            .iter()
            .filter(|t| {
                let (tx, ty) = t.midpoint();
                (t.angle() - centre).abs() <= WINDOW
                    && ((tx - mx) * nx + (ty - my) * ny).abs() <= 0.005 * diagonal
            })
            .collect();
        let support: f64 = members.iter().map(|t| t.length()).sum();
        if best.is_none_or(|(_, b)| support > b) {
            let mean = members.iter().map(|t| t.angle() * t.length()).sum::<f64>() / support;
            best = Some((mean, support));
        }
    }
    best.map(|(tilt, support)| Tilt {
        tilt,
        support: support / diagonal,
    })
}

/// The verticals' vanishing point seen from the frame centre: the camera's
/// roll also when the verticals converge (camera tilted up or down).
fn vertical_roll(verticals: &[Segment], width: f64, height: f64) -> Option<(Tilt, f64)> {
    let diagonal = width.hypot(height);
    let (vp, support) = vanishing_point(verticals)?;
    let (cx, cy) = (width / 2.0, height / 2.0);
    let (dx, dy, distance) = if vp[2].abs() > 1e-12 {
        let (x, y) = (vp[0] / vp[2], vp[1] / vp[2]);
        (x - cx, y - cy, (x - cx).hypot(y - cy))
    } else {
        (vp[0], vp[1], f64::INFINITY)
    };
    // The direction towards the point, as a tilt from vertical.
    let tilt = wrap_half_turn(dy.atan2(dx) - PI / 2.0);
    Some((
        Tilt {
            tilt,
            support: support / diagonal,
        },
        distance / diagonal,
    ))
}

/// The horizon cluster and the vertical roll (with the vanishing point's
/// distance from the centre in diagonals), before any threshold.
pub fn roll_parts(
    segments: &[Segment],
    width: f64,
    height: f64,
) -> (Option<Tilt>, Option<(Tilt, f64)>) {
    let diagonal = width.hypot(height);
    let (horizontals, verticals) = near_axis(segments, diagonal, 0.02);
    let horizontal = horizontal_cluster(&horizontals, diagonal);
    let vertical = (verticals.len() >= 2)
        .then(|| vertical_roll(&verticals, width, height))
        .flatten();
    (horizontal, vertical)
}

/// New candidate: horizon cluster and vertical vanishing point, combined by
/// support; no answer when neither has enough or they disagree evenly.
pub fn roll_tilt(segments: &[Segment], width: f64, height: f64) -> Option<Tilt> {
    let diagonal = width.hypot(height);
    let (horizontal, vertical) = roll_parts(segments, width, height);
    let horizontal = horizontal.filter(|h| h.support > 0.25);
    let vertical = vertical
        .filter(|(v, distance)| v.support > 0.25 && *distance > 1.0)
        .map(|(v, _)| v);
    let chosen = match (horizontal, vertical) {
        (Some(h), Some(v)) if (h.tilt - v.tilt).abs() <= 0.5f64.to_radians() => Some(Tilt {
            tilt: (h.tilt * h.support + v.tilt * v.support) / (h.support + v.support),
            support: h.support + v.support,
        }),
        // Verticals are the stronger evidence: horizontals recede with
        // perspective. One straight line across half the width or more (a
        // horizon) wins over weak verticals.
        (Some(h), Some(v)) if h.support * diagonal >= 0.5 * width && v.support < 1.0 => Some(h),
        (Some(h), Some(v)) if 2.0 * v.support > 1.5 * h.support => Some(v),
        (Some(h), Some(v)) if h.support > 1.5 * 2.0 * v.support => Some(h),
        (Some(_), Some(_)) => None,
        (Some(h), None) => Some(h),
        (None, Some(v)) => Some(v),
        (None, None) => None,
    }?;
    // Larger tilts are rare in hand-held photos and their evidence is more
    // often a leaning object than the scene: no answer (manual straighten).
    (chosen.tilt.abs() <= MAX_TILT.to_radians()).then_some(chosen)
}

/// RapidRAW v1.6.6's estimator (auto_straighten.rs, AGPL-3.0, CyberTimon and
/// contributors) on Rawtonic's segments, for comparison: per class a
/// vanishing point that must lie at least 5 diagonals away, then the
/// length-weighted mean of the doubled angles of the agreeing segments.
pub fn rapidraw_tilt(segments: &[Segment], width: f64, height: f64) -> Option<Tilt> {
    let diagonal = width.hypot(height);
    let max_side = width.max(height);
    let (horizontals, verticals) = {
        let mut h = Vec::new();
        let mut v = Vec::new();
        for s in segments.iter().filter(|s| s.length() >= 0.02 * max_side) {
            if s.angle().abs() <= 30f64.to_radians() {
                h.push(*s);
            } else if s.tilt_from_vertical().abs() <= 30f64.to_radians() {
                v.push(*s);
            }
        }
        (h, v)
    };
    let trusted = |class: &[Segment]| -> Vec<Segment> {
        if class.len() < 3 {
            return Vec::new();
        }
        let Some((vp, _)) = vanishing_point(class) else {
            return Vec::new();
        };
        let far = vp[2].abs() < 1e-12
            || (vp[0] / vp[2] - width / 2.0).hypot(vp[1] / vp[2] - height / 2.0) >= 5.0 * diagonal;
        let agreeing: Vec<Segment> = class
            .iter()
            .filter(|s| {
                let (mx, my) = s.midpoint();
                let (dx, dy) = if vp[2].abs() > 1e-9 {
                    (vp[0] / vp[2] - mx, vp[1] / vp[2] - my)
                } else {
                    (vp[0], vp[1])
                };
                wrap_half_turn(dy.atan2(dx) - s.angle()).abs() < PI / 180.0
            })
            .copied()
            .collect();
        let length: f64 = agreeing.iter().map(Segment::length).sum();
        if far && agreeing.len() >= 3 && length >= 0.25 * max_side {
            agreeing
        } else {
            Vec::new()
        }
    };
    let h = trusted(&horizontals);
    let v = trusted(&verticals);
    let total = (h.len() + v.len()) as f64;
    if total == 0.0 {
        return None;
    }
    let mut sums = (0.0, 0.0);
    for (class, deviation) in [
        (&h, Segment::angle as fn(&Segment) -> f64),
        (&v, Segment::tilt_from_vertical as fn(&Segment) -> f64),
    ] {
        let length: f64 = class.iter().map(Segment::length).sum();
        for s in class.iter() {
            let weight = s.length() / length * class.len() as f64 / total;
            let doubled = 2.0 * deviation(s);
            sums.0 += weight * doubled.sin();
            sums.1 += weight * doubled.cos();
        }
    }
    let tilt = 0.5 * sums.0.atan2(sums.1);
    let support = h.iter().chain(v.iter()).map(Segment::length).sum::<f64>() / diagonal;
    (tilt.abs() <= 10f64.to_radians()).then_some(Tilt { tilt, support })
}

/// Which pixels of the developed frame hold photo, for a recipe's geometry
/// and a fine `rotation`: the geometry warp's own validity
/// (`geometry_valid_pixels`), moved by the orientation steps and flips, then
/// every Catmull-Rom tap of the rotation (`rotation_inverse`) inside that
/// frame and on photo. Queried per rectangle through an integral image of
/// the empty pixels.
pub struct FrameValidity {
    pub width: u32,
    pub height: u32,
    /// (width + 1) × (height + 1) running sums of empty pixels.
    empty: Vec<u32>,
}

impl FrameValidity {
    pub fn new(
        adjustments: &serde_json::Value,
        source_width: u32,
        source_height: u32,
        rotation: f64,
    ) -> Self {
        let params = crate::image_processing::get_geometry_params_from_json(adjustments);
        let warped =
            crate::image_processing::geometry_valid_pixels(&params, source_width, source_height);
        let steps = adjustments["orientationSteps"].as_u64().unwrap_or(0) % 4;
        let flip_h = adjustments["flipHorizontal"].as_bool().unwrap_or(false);
        let flip_v = adjustments["flipVertical"].as_bool().unwrap_or(false);
        let (sw, sh) = (source_width as i64, source_height as i64);
        let (w, h) = if steps % 2 == 1 { (sh, sw) } else { (sw, sh) };
        // Photo at (x, y) of the oriented, flipped frame (image crate's
        // rotate90/180/270 and fliph/flipv).
        let oriented = |x: i64, y: i64| -> bool {
            let (x, y) = (
                if flip_h { w - 1 - x } else { x },
                if flip_v { h - 1 - y } else { y },
            );
            let (sx, sy) = match steps {
                1 => (y, sh - 1 - x),
                2 => (sw - 1 - x, sh - 1 - y),
                3 => (sw - 1 - y, x),
                _ => (x, y),
            };
            warped
                .as_ref()
                .is_none_or(|mask| mask[(sy * sw + sx) as usize] != 0)
        };
        let empty_oriented = integral(w as usize, h as usize, |x, y| !oriented(x as i64, y as i64));
        let box_empty = |x0: i64, y0: i64, x1: i64, y1: i64| -> bool {
            // Any empty pixel or any pixel outside in [x0, x1) × [y0, y1).
            if x0 < 0 || y0 < 0 || x1 > w || y1 > h {
                return true;
            }
            sum(
                &empty_oriented,
                w as usize,
                x0 as usize,
                y0 as usize,
                x1 as usize,
                y1 as usize,
            ) > 0
        };
        let empty = if rotation % 360.0 == 0.0 {
            empty_oriented.clone()
        } else {
            let inverse =
                crate::image_processing::rotation_inverse(rotation as f32, w as u32, h as u32);
            integral(w as usize, h as usize, |x, y| {
                let (px, py) = inverse * (x as f32, y as f32);
                if !(px.is_finite() && py.is_finite()) {
                    return true;
                }
                let (left, top) = (px.floor() as i64 - 1, py.floor() as i64 - 1);
                box_empty(left, top, left + 4, top + 4)
            })
        };
        Self {
            width: w as u32,
            height: h as u32,
            empty,
        }
    }

    /// Whether every pixel of the `width` × `height` rectangle at (`x`, `y`)
    /// holds photo.
    pub fn holds_photo(&self, x: i64, y: i64, width: i64, height: i64) -> bool {
        if width <= 0 || height <= 0 || x < 0 || y < 0 {
            return false;
        }
        let (x1, y1) = (x + width, y + height);
        if x1 > self.width as i64 || y1 > self.height as i64 {
            return false;
        }
        sum(
            &self.empty,
            self.width as usize,
            x as usize,
            y as usize,
            x1 as usize,
            y1 as usize,
        ) == 0
    }
}

/// Running sums over a `width` × `height` grid of `flag(x, y)`.
fn integral(width: usize, height: usize, flag: impl Fn(usize, usize) -> bool + Sync) -> Vec<u32> {
    use rayon::prelude::*;
    let rows: Vec<Vec<u32>> = (0..height)
        .into_par_iter()
        .map(|y| {
            let mut acc = 0u32;
            let mut row = vec![0u32; width + 1];
            for x in 0..width {
                acc += flag(x, y) as u32;
                row[x + 1] = acc;
            }
            row
        })
        .collect();
    let stride = width + 1;
    let mut table = vec![0u32; stride * (height + 1)];
    for y in 0..height {
        let (above, here) = table.split_at_mut((y + 1) * stride);
        let above = &above[y * stride..];
        for x in 0..=width {
            here[x] = above[x] + rows[y][x];
        }
    }
    table
}

fn sum(table: &[u32], width: usize, x0: usize, y0: usize, x1: usize, y1: usize) -> u32 {
    let stride = width + 1;
    table[y1 * stride + x1] + table[y0 * stride + x0]
        - table[y0 * stride + x1]
        - table[y1 * stride + x0]
}

/// A crop rectangle in pixels of the developed (oriented, rotated) frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CropRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl CropRect {
    fn rounded(self) -> (i64, i64, i64, i64) {
        (
            self.x.ceil() as i64,
            self.y.ceil() as i64,
            self.width.floor() as i64,
            self.height.floor() as i64,
        )
    }
}

/// The crop after straightening: the starting crop's ratio, its centre
/// turned with the rotation change, shrunk about that centre until it holds
/// only photo; if no small crop there does, the centre moves towards the
/// frame's until one does. None when not even 1 % of the starting crop fits.
pub fn reframe_crop(
    frame: &FrameValidity,
    start: CropRect,
    rotation_change: f64,
) -> Option<CropRect> {
    let (w, h) = (frame.width as f64, frame.height as f64);
    let (sin, cos) = rotation_change.to_radians().sin_cos();
    let (px, py) = (
        start.x + start.width / 2.0 - w / 2.0,
        start.y + start.height / 2.0 - h / 2.0,
    );
    let centre = (w / 2.0 + px * cos - py * sin, h / 2.0 + px * sin + py * cos);
    let at = |(cx, cy): (f64, f64), scale: f64| CropRect {
        x: cx - start.width * scale / 2.0,
        y: cy - start.height * scale / 2.0,
        width: start.width * scale,
        height: start.height * scale,
    };
    let fits = |c: CropRect| {
        let (x, y, cw, ch) = c.rounded();
        frame.holds_photo(x, y, cw, ch)
    };
    let largest = |centre: (f64, f64)| -> Option<CropRect> {
        if !fits(at(centre, 0.1)) {
            return None;
        }
        if fits(at(centre, 1.0)) {
            return Some(at(centre, 1.0));
        }
        let (mut low, mut high) = (0.1, 1.0);
        for _ in 0..20 {
            let mid = (low + high) / 2.0;
            if fits(at(centre, mid)) {
                low = mid;
            } else {
                high = mid;
            }
        }
        Some(at(centre, low))
    };
    let finish = |c: CropRect| {
        let (x, y, cw, ch) = c.rounded();
        CropRect {
            x: x as f64,
            y: y as f64,
            width: cw as f64,
            height: ch as f64,
        }
    };
    if let Some(c) = largest(centre) {
        return Some(finish(c));
    }
    // Towards the frame centre, in 1 % steps.
    for k in 1..=100 {
        let t = k as f64 / 100.0;
        let moved = (
            centre.0 + (w / 2.0 - centre.0) * t,
            centre.1 + (h / 2.0 - centre.1) * t,
        );
        if let Some(c) = largest(moved) {
            return Some(finish(c));
        }
    }
    None
}

/// Analysis size (long side), as Upright's.
pub const ANALYSIS_SIDE: u32 = 1400;

/// What Auto straighten sets: the recipe's `rotation` (degrees) and the
/// crop (pixels of the developed frame), and the line support it rests on.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Straightened {
    pub rotation: f64,
    pub crop: CropRect,
    pub support: f64,
}

/// The rotation that levels `image` (the photo as decoded, any size) under
/// `adjustments`, analysed on the photo as developed before its fine
/// rotation and crop: lens and perspective geometry, orientation steps and
/// flips. Segments next to empty borders of the warp are left out (the
/// border is a straight edge of no structure). None without enough lines.
pub fn level_rotation(
    image: &image::DynamicImage,
    adjustments: &serde_json::Value,
) -> Option<Tilt> {
    use crate::image_processing::{
        apply_coarse_rotation, apply_flip, apply_geometry_warp, geometry_valid_pixels,
        get_geometry_params_from_json,
    };
    let (w, h) = (image.width(), image.height());
    let scale = (ANALYSIS_SIDE as f64 / w.max(h) as f64).min(1.0);
    let (pw, ph) = (
        ((w as f64 * scale).round() as u32).max(1),
        ((h as f64 * scale).round() as u32).max(1),
    );
    let proxy = crate::image_processing::thumbnail(image, pw, ph);
    let steps = adjustments["orientationSteps"].as_u64().unwrap_or(0) as u8;
    let (flip_h, flip_v) = (
        adjustments["flipHorizontal"].as_bool().unwrap_or(false),
        adjustments["flipVertical"].as_bool().unwrap_or(false),
    );
    let developed = apply_flip(
        apply_coarse_rotation(apply_geometry_warp(&proxy, adjustments), steps),
        flip_h,
        flip_v,
    )
    .into_owned();
    let (gray, gw, gh) = crate::upright::analysis_image(&developed, ANALYSIS_SIDE);
    let segments =
        crate::upright::detect_segments(&gray, gw, gh, (gw as f64).hypot(gh as f64) * 0.02);
    // The warp's empty borders, oriented like the analysis image.
    let params = get_geometry_params_from_json(adjustments);
    let segments: Vec<Segment> = match geometry_valid_pixels(&params, pw, ph) {
        None => segments,
        Some(mask) => {
            let mask = image::GrayImage::from_raw(pw, ph, mask.iter().map(|&v| v * 255).collect())?;
            let mask = apply_flip(
                apply_coarse_rotation(image::DynamicImage::ImageLuma8(mask), steps),
                flip_h,
                flip_v,
            )
            .into_owned()
            .into_luma8();
            let (mw, mh) = mask.dimensions();
            let (sx, sy) = (mw as f64 / gw as f64, mh as f64 / gh as f64);
            let near_border = |x: f64, y: f64| {
                let (cx, cy) = ((x * sx) as i64, (y * sy) as i64);
                (-3..=3).any(|dy| {
                    (-3..=3).any(|dx| {
                        let (px, py) = (cx + dx, cy + dy);
                        px < 0
                            || py < 0
                            || px >= mw as i64
                            || py >= mh as i64
                            || mask.get_pixel(px as u32, py as u32)[0] == 0
                    })
                })
            };
            // An empty border's straight edge runs along it: most of the
            // segment lies next to it. A line of the photo that only reaches
            // the frame's edge (a horizon across a zoomed frame) does not.
            segments
                .into_iter()
                .filter(|s| {
                    let near = (1..10)
                        .filter(|i| {
                            let t = *i as f64 / 10.0;
                            near_border(s.x1 + t * (s.x2 - s.x1), s.y1 + t * (s.y2 - s.y1))
                        })
                        .count();
                    near < 5
                })
                .collect()
        }
    };
    roll_tilt(&segments, gw as f64, gh as f64)
}

/// Auto straighten for a photo decoded at `source_width` × `source_height`
/// (`analysed`: the same photo, any size): the levelling rotation and the
/// crop reframed from the recipe's current crop (or the whole frame at the
/// recipe's aspect ratio). The result replaces `rotation`, so a second run
/// gives the same answer.
pub fn straighten_recipe(
    analysed: &image::DynamicImage,
    source_width: u32,
    source_height: u32,
    adjustments: &serde_json::Value,
) -> Option<Straightened> {
    let tilt = level_rotation(analysed, adjustments)?;
    let rotation = -tilt.tilt.to_degrees();
    let rotation = (rotation * 100.0).round() / 100.0;
    let previous = adjustments["rotation"].as_f64().unwrap_or(0.0);
    let frame = FrameValidity::new(adjustments, source_width, source_height, rotation);
    let (w, h) = (frame.width as f64, frame.height as f64);
    let start =
        serde_json::from_value::<crate::image_processing::Crop>(adjustments["crop"].clone())
            .ok()
            .filter(|c| c.width > 0.0 && c.height > 0.0)
            .map(|c| CropRect {
                x: c.x,
                y: c.y,
                width: c.width,
                height: c.height,
            })
            .unwrap_or_else(|| {
                // The whole frame, or the largest centred crop of the chosen ratio.
                let ratio = adjustments["aspectRatio"]
                    .as_f64()
                    .filter(|r| *r > 0.0)
                    .unwrap_or(w / h);
                let (cw, ch) = if w / h > ratio {
                    (h * ratio, h)
                } else {
                    (w, w / ratio)
                };
                CropRect {
                    x: (w - cw) / 2.0,
                    y: (h - ch) / 2.0,
                    width: cw,
                    height: ch,
                }
            });
    let crop = reframe_crop(&frame, start, rotation - previous)?;
    Some(Straightened {
        rotation,
        crop,
        support: tilt.support,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::upright::{analysis_image, detect_segments};

    /// A synthetic photo: a smooth brightness ramp (photos have continuous
    /// histograms; the analysis equalises by rank) with dark bars along
    /// `lines` (pixel coordinates).
    fn drawn(width: u32, height: u32, lines: &[(f64, f64, f64, f64)]) -> image::DynamicImage {
        let mut img = image::RgbImage::new(width, height);
        for y in 0..height {
            for x in 0..width {
                let level = (90.0 + 140.0 * (x + y) as f64 / (width + height) as f64) as u8;
                img.put_pixel(x, y, image::Rgb([level; 3]));
                let (px, py) = (x as f64 + 0.5, y as f64 + 0.5);
                for &(x1, y1, x2, y2) in lines {
                    let (dx, dy) = (x2 - x1, y2 - y1);
                    let t =
                        (((px - x1) * dx + (py - y1) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
                    let d = (px - x1 - t * dx).hypot(py - y1 - t * dy);
                    if d < 2.5 {
                        img.put_pixel(x, y, image::Rgb([40, 40, 40]));
                    }
                }
            }
        }
        image::DynamicImage::ImageRgb8(img)
    }

    /// Building verticals of a camera rolled by `roll` (degrees, clockwise
    /// on screen) whose vanishing point lies `distance` diagonals above the
    /// centre (infinity: parallel), as the photo shows them.
    fn verticals(width: f64, height: f64, roll: f64, distance: f64) -> Vec<(f64, f64, f64, f64)> {
        let (cx, cy) = (width / 2.0, height / 2.0);
        let (s, c) = roll.to_radians().sin_cos();
        // Upwards on screen, turned by the roll.
        let up = (s, -c);
        [0.15, 0.3, 0.45, 0.6, 0.75, 0.88]
            .iter()
            .map(|&f| {
                let base = (
                    cx + (f - 0.5) * width * 0.9 * c,
                    cy + 0.35 * height + (f - 0.5) * width * 0.9 * s,
                );
                let towards = if distance.is_finite() {
                    let vp = (
                        cx + up.0 * distance * width.hypot(height),
                        cy + up.1 * distance * width.hypot(height),
                    );
                    let (dx, dy) = (vp.0 - base.0, vp.1 - base.1);
                    let n = dx.hypot(dy);
                    (dx / n, dy / n)
                } else {
                    up
                };
                let length = 0.7 * height;
                (
                    base.0,
                    base.1,
                    base.0 + towards.0 * length,
                    base.1 + towards.1 * length,
                )
            })
            .collect()
    }

    #[test]
    fn verticals_level_the_photo_also_when_they_converge() {
        let (w, h) = (900u32, 600u32);
        for (roll, distance) in [
            (3.0, f64::INFINITY),
            (-2.0, f64::INFINITY),
            (4.0, 3.0),
            (-6.0, 2.0),
        ] {
            let image = drawn(w, h, &verticals(w as f64, h as f64, roll, distance));
            let tilt = level_rotation(&image, &serde_json::json!({})).expect("lines found");
            let rotation = -tilt.tilt.to_degrees();
            assert!(
                (rotation + roll).abs() < 0.15,
                "roll {roll} at {distance}: {rotation}"
            );
        }
    }

    #[test]
    fn a_horizon_levels_the_photo_and_no_lines_give_no_answer() {
        let (w, h) = (900u32, 600u32);
        let a = 2.5f64.to_radians();
        let horizon = [(
            20.0,
            300.0 - 430.0 * a.tan(),
            880.0,
            300.0 + 430.0 * a.tan(),
        )];
        let tilt =
            level_rotation(&drawn(w, h, &horizon), &serde_json::json!({})).expect("horizon found");
        assert!((tilt.tilt.to_degrees() - 2.5).abs() < 0.1, "{tilt:?}");
        assert!(level_rotation(&drawn(w, h, &[]), &serde_json::json!({})).is_none());
    }

    /// Under a transform (zoom in), a horizon or verticals spanning the whole
    /// frame reach its edge at both ends and still level the photo.
    #[test]
    fn lines_spanning_the_frame_count_under_a_transform() {
        let (w, h) = (900u32, 600u32);
        let a = 2.5f64.to_radians();
        let horizon = [(0.0, 300.0 - 450.0 * a.tan(), 900.0, 300.0 + 450.0 * a.tan())];
        let recipe = serde_json::json!({"transformScale": 125.0});
        // The transform's scale bends the line a little (k(|p|) stretch), so
        // the rendered tilt is near, not at, the drawn one.
        let tilt = level_rotation(&drawn(w, h, &horizon), &recipe).expect("horizon found");
        assert!((tilt.tilt.to_degrees() - 2.5).abs() < 0.5, "{tilt:?}");
        let (s, c) = 2.0f64.to_radians().sin_cos();
        let full_height: Vec<(f64, f64, f64, f64)> = [0.2, 0.4, 0.6, 0.8]
            .iter()
            .map(|f| {
                let x = f * w as f64;
                (x - 300.0 * s / c, 0.0, x + 300.0 * s / c, h as f64)
            })
            .collect();
        let found =
            straighten_recipe(&drawn(w, h, &full_height), w, h, &recipe).expect("verticals found");
        assert!((found.rotation.abs() - 2.0).abs() < 0.5, "{found:?}");
    }

    /// A perspective correction leaves empty borders whose straight edges
    /// are no structure; the answer stays the photo's own and repeats.
    #[test]
    fn empty_warp_borders_are_ignored_and_auto_repeats() {
        let (w, h) = (900u32, 600u32);
        let image = drawn(w, h, &verticals(w as f64, h as f64, 2.0, f64::INFINITY));
        let recipe = serde_json::json!({"transformScale": 80.0});
        let first = straighten_recipe(&image, w, h, &recipe).expect("straightened");
        assert!((first.rotation + 2.0).abs() < 0.15, "{first:?}");
        let mut again = recipe.clone();
        again["rotation"] = serde_json::json!(first.rotation);
        again["crop"] = serde_json::json!({"x": first.crop.x, "y": first.crop.y,
            "width": first.crop.width, "height": first.crop.height, "unit": "px"});
        let second = straighten_recipe(&image, w, h, &again).expect("straightened again");
        assert_eq!(first.rotation, second.rotation);
        assert_eq!(first.crop, second.crop);
    }

    /// The reframed crop holds only photo in the real CPU render (geometry
    /// warp, orientation, flips, rotation) of a white source: every pixel
    /// fully white, for zoom in and out, offsets, perspective, turned and
    /// flipped frames, both rotation directions and an off-centre start.
    #[test]
    fn the_reframed_crop_holds_only_photo() {
        use crate::image_processing::{
            apply_coarse_rotation, apply_flip, apply_geometry_warp, apply_rotation,
        };
        let (sw, sh) = (360u32, 240u32);
        let white = image::DynamicImage::ImageRgb32F(image::ImageBuffer::from_pixel(
            sw,
            sh,
            image::Rgb([1.0f32; 3]),
        ));
        let cases = [
            serde_json::json!({}),
            serde_json::json!({"transformScale": 80.0}),
            serde_json::json!({"transformScale": 125.0, "transformXOffset": 10.0}),
            serde_json::json!({"transformVertical": 30.0}),
            serde_json::json!({"transformVertical": -20.0, "transformHorizontal": 15.0, "orientationSteps": 1}),
            serde_json::json!({"orientationSteps": 3, "flipHorizontal": true}),
            serde_json::json!({"lensDistortionParams": {"k1": 0.03, "k2": -0.08, "k3": 0.05, "model": 1,
                "tca_vr": 1.0003, "tca_vb": 0.9997, "vig_k1": 0.0, "vig_k2": 0.0, "vig_k3": 0.0}}),
            serde_json::json!({"lensDistortionParams": {"k1": -0.03, "k2": 0.08, "k3": -0.06, "model": 1,
                "tca_vr": 1.0, "tca_vb": 1.0, "vig_k1": 0.0, "vig_k2": 0.0, "vig_k3": 0.0}}),
        ];
        for recipe in cases {
            for rotation in [-8.0f64, 3.3, 8.0] {
                let mut recipe = recipe.clone();
                recipe["rotation"] = serde_json::json!(rotation);
                let warped = apply_geometry_warp(&white, &recipe).into_owned();
                let turned = apply_coarse_rotation(
                    warped,
                    recipe["orientationSteps"].as_u64().unwrap_or(0) as u8,
                )
                .into_owned();
                let flipped = apply_flip(
                    turned,
                    recipe["flipHorizontal"].as_bool().unwrap_or(false),
                    recipe["flipVertical"].as_bool().unwrap_or(false),
                )
                .into_owned();
                let rendered = apply_rotation(flipped, rotation as f32, false)
                    .into_owned()
                    .to_rgba32f();
                let frame = FrameValidity::new(&recipe, sw, sh, rotation);
                assert_eq!((frame.width, frame.height), rendered.dimensions());
                let (w, h) = (frame.width as f64, frame.height as f64);
                // The turned frame's corners are empty.
                assert!(!frame.holds_photo(0, 0, frame.width as i64, frame.height as i64));
                for start in [
                    CropRect {
                        x: 0.0,
                        y: 0.0,
                        width: w,
                        height: h,
                    },
                    CropRect {
                        x: w * 0.4,
                        y: h * 0.1,
                        width: w * 0.5,
                        height: h * 0.5,
                    },
                ] {
                    let Some(crop) = reframe_crop(&frame, start, rotation) else {
                        panic!("no crop for {recipe} from {start:?}");
                    };
                    for y in crop.y as u32..(crop.y + crop.height) as u32 {
                        for x in crop.x as u32..(crop.x + crop.width) as u32 {
                            let p = rendered.get_pixel(x, y);
                            assert!(
                                (p[0] - 1.0).abs() < 1e-3,
                                "{recipe}: ({x}, {y}) = {:?} in {crop:?}",
                                p
                            );
                        }
                    }
                    assert!(
                        crop.width * crop.height > 0.2 * start.width * start.height,
                        "{recipe}: {crop:?}"
                    );
                }
            }
        }
    }

    /// Time of the validity frame for a 45 MP photo with lens correction and
    /// a rotation (`--ignored --nocapture`, release).
    #[test]
    #[ignore = "timing probe"]
    fn frame_validity_time_45mp() {
        let recipe = serde_json::json!({"transformVertical": 20.0, "rotation": 4.0,
            "lensDistortionParams": {"k1": 0.02, "k2": -0.05, "k3": 0.03, "model": 1, "tca_vr": 1.0, "tca_vb": 1.0, "vig_k1": 0.0, "vig_k2": 0.0, "vig_k3": 0.0}});
        for _ in 0..3 {
            let started = std::time::Instant::now();
            let frame = FrameValidity::new(&recipe, 8256, 5504, 4.0);
            let crop = reframe_crop(
                &frame,
                CropRect {
                    x: 0.0,
                    y: 0.0,
                    width: 8256.0,
                    height: 5504.0,
                },
                4.0,
            );
            println!(
                "45 MP validity + reframe: {:.0} ms ({crop:?})",
                started.elapsed().as_secs_f64() * 1000.0
            );
        }
    }

    /// Times `straighten_recipe` on the images in `$STRAIGHTEN_FILES`,
    /// converted to float RGB like the app's decode (`--release`).
    #[test]
    #[ignore = "tool: needs STRAIGHTEN_FILES"]
    fn time_straighten_recipe() {
        let list = std::fs::read_to_string(std::env::var("STRAIGHTEN_FILES").unwrap()).unwrap();
        for path in list.lines().filter(|l| !l.trim().is_empty()) {
            let image = image::DynamicImage::ImageRgb32F(image::open(path).unwrap().to_rgb32f());
            let (w, h) = (image.width(), image.height());
            let mut times = Vec::new();
            for _ in 0..3 {
                let started = std::time::Instant::now();
                let result = straighten_recipe(&image, w, h, &serde_json::json!({}));
                times.push(started.elapsed().as_secs_f64() * 1000.0);
                std::hint::black_box(result);
            }
            println!(
                "TIME\t{path}\t{w}x{h}\t{:.0}\t{:.0}\t{:.0}",
                times[0], times[1], times[2]
            );
        }
    }

    /// Prints each candidate's levelling rotation (degrees, the recipe's
    /// `rotation`) for the images listed in `$STRAIGHTEN_FILES` (one path per
    /// line), analysed at 1400 px like Upright.
    #[test]
    #[ignore = "tool: needs STRAIGHTEN_FILES"]
    fn print_straighten_candidates() {
        let list = std::fs::read_to_string(std::env::var("STRAIGHTEN_FILES").unwrap()).unwrap();
        for path in list.lines().filter(|l| !l.trim().is_empty()) {
            let Ok(image) = image::open(path) else {
                println!("STRAIGHTEN\t{path}\tunreadable");
                continue;
            };
            let started = std::time::Instant::now();
            let (gray, w, h) = analysis_image(&image, 1400);
            let segments = detect_segments(&gray, w, h, (w as f64).hypot(h as f64) * 0.02);
            let detect_ms = started.elapsed().as_secs_f64() * 1000.0;
            let show = |t: Option<Tilt>| {
                t.map_or("none\t0".to_string(), |t| {
                    format!("{:.3}\t{:.3}", -t.tilt.to_degrees(), t.support)
                })
            };
            let (w, h) = (w as f64, h as f64);
            let (hp, vp) = roll_parts(&segments, w, h);
            println!(
                "STRAIGHTEN\t{path}\t{}\t{}\t{}\t{}\t{detect_ms:.0}\t{}\t{}\t{:.1}",
                segments.len(),
                show(baseline_tilt(&segments, w, h)),
                show(rapidraw_tilt(&segments, w, h)),
                show(roll_tilt(&segments, w, h)),
                show(hp),
                show(vp.map(|(v, _)| v)),
                vp.map_or(0.0, |(_, d)| d),
            );
        }
    }
}
