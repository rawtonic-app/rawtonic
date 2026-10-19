//! Automatic upright (Lightroom's Upright Auto/Level/Vertical/Full, Capture
//! One's keystone): straight line segments are found in the lens-corrected
//! photo, vanishing points estimated from them, and the result is written as
//! guide lines for the existing guided perspective correction
//! (guided_perspective.rs), which then does the actual transform. The
//! Tauri command is in the app's `upright.rs`.
use std::f64::consts::PI;

use image::DynamicImage;
use serde::Serialize;

use crate::guided_perspective::{GuideLine, GuideOrientation, GuidePoint};

/// A straight segment in pixel coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Segment {
    pub x1: f64,
    pub y1: f64,
    pub x2: f64,
    pub y2: f64,
}

impl Segment {
    pub fn length(&self) -> f64 {
        (self.x2 - self.x1).hypot(self.y2 - self.y1)
    }
    /// Direction in (−π/2, π/2]: 0 is horizontal, ±π/2 vertical.
    pub fn angle(&self) -> f64 {
        let a = (self.y2 - self.y1).atan2(self.x2 - self.x1);
        wrap_half_turn(a)
    }
    pub(crate) fn midpoint(&self) -> (f64, f64) {
        ((self.x1 + self.x2) / 2.0, (self.y1 + self.y2) / 2.0)
    }
    /// Homogeneous line through both ends.
    pub(crate) fn line(&self) -> [f64; 3] {
        cross([self.x1, self.y1, 1.0], [self.x2, self.y2, 1.0])
    }
    /// Deviation from vertical (radians, signed).
    pub fn tilt_from_vertical(&self) -> f64 {
        wrap_half_turn(self.angle() - PI / 2.0)
    }
}

pub(crate) fn wrap_half_turn(a: f64) -> f64 {
    let mut a = a % PI;
    if a <= -PI / 2.0 {
        a += PI;
    } else if a > PI / 2.0 {
        a -= PI;
    }
    a
}

pub(crate) fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

/// Line segments of a grayscale image (0–255), in the manner of LSD: pixels
/// with strong gradients are grown into regions of similar edge direction,
/// and elongated, dense regions become segments.
pub fn detect_segments(gray: &[f32], width: usize, height: usize, min_length: f64) -> Vec<Segment> {
    if width < 8 || height < 8 || gray.len() != width * height {
        return Vec::new();
    }
    // Gaussian smoothing first (LSD): staircase edges would otherwise change
    // direction at every step and break into short pieces.
    let smooth = gaussian_blur(gray, width, height, 1.0);
    let at = |x: usize, y: usize| smooth[y * width + x];
    let mut magnitude = vec![0f32; width * height];
    let mut direction = vec![0f32; width * height];
    for y in 1..height - 1 {
        for x in 1..width - 1 {
            let gx = (at(x + 1, y - 1) + 2.0 * at(x + 1, y) + at(x + 1, y + 1))
                - (at(x - 1, y - 1) + 2.0 * at(x - 1, y) + at(x - 1, y + 1));
            let gy = (at(x - 1, y + 1) + 2.0 * at(x, y + 1) + at(x + 1, y + 1))
                - (at(x - 1, y - 1) + 2.0 * at(x, y - 1) + at(x + 1, y - 1));
            let i = y * width + x;
            magnitude[i] = (gx * gx + gy * gy).sqrt() / 8.0;
            // The edge runs perpendicular to the gradient.
            direction[i] = gy.atan2(gx) + std::f32::consts::FRAC_PI_2;
        }
    }
    // LSD's gradient threshold for a 22.5° tolerance and two grey levels of noise.
    const THRESHOLD: f32 = 5.2;
    const TOLERANCE: f64 = PI / 8.0;
    let mut order: Vec<usize> = (0..width * height)
        .filter(|&i| magnitude[i] > THRESHOLD)
        .collect();
    order.sort_unstable_by(|&a, &b| magnitude[b].total_cmp(&magnitude[a]));
    let mut used = vec![false; width * height];
    let mut segments = Vec::new();
    let mut region = Vec::new();
    let mut stack = Vec::new();
    for &seed in &order {
        if used[seed] {
            continue;
        }
        region.clear();
        stack.clear();
        used[seed] = true;
        stack.push(seed);
        // Region direction as a doubled-angle mean (edge direction is modulo π).
        let (mut sum_cos, mut sum_sin) = (0.0f64, 0.0f64);
        let doubled = |i: usize| 2.0 * direction[i] as f64;
        sum_cos += doubled(seed).cos();
        sum_sin += doubled(seed).sin();
        while let Some(i) = stack.pop() {
            region.push(i);
            let mean = sum_sin.atan2(sum_cos) / 2.0;
            let (x, y) = ((i % width) as isize, (i / width) as isize);
            for dy in -1..=1 {
                for dx in -1..=1 {
                    let (nx, ny) = (x + dx, y + dy);
                    if nx < 1 || ny < 1 || nx >= width as isize - 1 || ny >= height as isize - 1 {
                        continue;
                    }
                    let n = ny as usize * width + nx as usize;
                    if used[n] || magnitude[n] <= THRESHOLD {
                        continue;
                    }
                    if wrap_half_turn(direction[n] as f64 - mean).abs() < TOLERANCE {
                        used[n] = true;
                        sum_cos += doubled(n).cos();
                        sum_sin += doubled(n).sin();
                        stack.push(n);
                    }
                }
            }
        }
        if (region.len() as f64) < min_length * 0.5 {
            continue;
        }
        if let Some(segment) = fit_segment(&region, width, &magnitude, min_length) {
            segments.push(segment);
        }
    }
    segments
}

/// Separable Gaussian blur with clamped borders.
fn gaussian_blur(src: &[f32], width: usize, height: usize, sigma: f32) -> Vec<f32> {
    let radius = (sigma * 3.0).ceil() as isize;
    let kernel: Vec<f32> = (-radius..=radius)
        .map(|i| (-(i * i) as f32 / (2.0 * sigma * sigma)).exp())
        .collect();
    let total: f32 = kernel.iter().sum();
    let kernel: Vec<f32> = kernel.iter().map(|k| k / total).collect();
    let mut tmp = vec![0f32; src.len()];
    let mut out = vec![0f32; src.len()];
    for y in 0..height {
        for x in 0..width {
            tmp[y * width + x] = kernel
                .iter()
                .enumerate()
                .map(|(k, w)| {
                    let xx =
                        (x as isize + k as isize - radius).clamp(0, width as isize - 1) as usize;
                    w * src[y * width + xx]
                })
                .sum();
        }
    }
    for y in 0..height {
        for x in 0..width {
            out[y * width + x] = kernel
                .iter()
                .enumerate()
                .map(|(k, w)| {
                    let yy =
                        (y as isize + k as isize - radius).clamp(0, height as isize - 1) as usize;
                    w * tmp[yy * width + x]
                })
                .sum();
        }
    }
    out
}

/// Principal axis of a region, weighted by gradient magnitude.
fn fit_segment(
    region: &[usize],
    width: usize,
    magnitude: &[f32],
    min_length: f64,
) -> Option<Segment> {
    let (mut sw, mut sx, mut sy) = (0.0, 0.0, 0.0);
    for &i in region {
        let w = magnitude[i] as f64;
        sw += w;
        sx += w * (i % width) as f64;
        sy += w * (i / width) as f64;
    }
    let (cx, cy) = (sx / sw, sy / sw);
    let (mut xx, mut yy, mut xy) = (0.0, 0.0, 0.0);
    for &i in region {
        let w = magnitude[i] as f64;
        let (dx, dy) = ((i % width) as f64 - cx, (i / width) as f64 - cy);
        xx += w * dx * dx;
        yy += w * dy * dy;
        xy += w * dx * dy;
    }
    let theta = 0.5 * (2.0 * xy).atan2(xx - yy);
    let (ux, uy) = (theta.cos(), theta.sin());
    let (mut lo, mut hi, mut wlo, mut whi) = (f64::MAX, f64::MIN, f64::MAX, f64::MIN);
    for &i in region {
        let (dx, dy) = ((i % width) as f64 - cx, (i / width) as f64 - cy);
        let along = dx * ux + dy * uy;
        let across = -dx * uy + dy * ux;
        lo = lo.min(along);
        hi = hi.max(along);
        wlo = wlo.min(across);
        whi = whi.max(across);
    }
    let length = hi - lo;
    let thickness = (whi - wlo).max(1.0);
    let density = region.len() as f64 / (length.max(1.0) * thickness);
    (length >= min_length && thickness <= (length * 0.1).max(2.5) && density > 0.4).then_some(
        Segment {
            x1: cx + ux * lo,
            y1: cy + uy * lo,
            x2: cx + ux * hi,
            y2: cy + uy * hi,
        },
    )
}

/// A vanishing point (homogeneous) and the length of the segments that agree.
pub(crate) fn vanishing_point(candidates: &[Segment]) -> Option<([f64; 3], f64)> {
    const AGREEMENT: f64 = 1.0 * PI / 180.0;
    let mut longest: Vec<&Segment> = candidates.iter().collect();
    longest.sort_by(|a, b| b.length().total_cmp(&a.length()));
    longest.truncate(40);
    let agrees = |s: &Segment, vp: [f64; 3]| {
        let (mx, my) = s.midpoint();
        // Direction from the segment's middle towards the point (or along it at infinity).
        let (dx, dy) = if vp[2].abs() > 1e-9 {
            (vp[0] / vp[2] - mx, vp[1] / vp[2] - my)
        } else {
            (vp[0], vp[1])
        };
        wrap_half_turn(dy.atan2(dx) - s.angle()).abs() < AGREEMENT
    };
    let mut best: Option<([f64; 3], f64)> = None;
    for i in 0..longest.len() {
        for j in i + 1..longest.len() {
            let vp = cross(longest[i].line(), longest[j].line());
            let norm = (vp[0] * vp[0] + vp[1] * vp[1] + vp[2] * vp[2]).sqrt();
            if norm < 1e-12 {
                continue;
            }
            let vp = [vp[0] / norm, vp[1] / norm, vp[2] / norm];
            let score: f64 = candidates
                .iter()
                .filter(|s| agrees(s, vp))
                .map(Segment::length)
                .sum();
            if best.is_none_or(|(_, b)| score > b) {
                best = Some((vp, score));
            }
        }
    }
    // The best pair only fixes the point roughly: refine it by least squares
    // over every agreeing segment (twice, as the agreeing set can change).
    let (mut vp, _) = best?;
    for _ in 0..2 {
        let agreeing: Vec<&Segment> = candidates.iter().filter(|s| agrees(s, vp)).collect();
        if agreeing.len() < 2 {
            break;
        }
        if let Some(refined) = least_squares_point(&agreeing) {
            vp = refined;
        }
    }
    let score = candidates
        .iter()
        .filter(|s| agrees(s, vp))
        .map(Segment::length)
        .sum();
    Some((vp, score))
}

/// The homogeneous point closest to all segment lines (length-weighted), in
/// coordinates centred and scaled for conditioning.
fn least_squares_point(segments: &[&Segment]) -> Option<[f64; 3]> {
    let (mut cx, mut cy, mut extent) = (0.0, 0.0, 1.0f64);
    for s in segments {
        cx += s.x1 + s.x2;
        cy += s.y1 + s.y2;
        extent = extent.max((s.x1 - s.x2).abs()).max((s.y1 - s.y2).abs());
    }
    cx /= 2.0 * segments.len() as f64;
    cy /= 2.0 * segments.len() as f64;
    let scale = segments
        .iter()
        .flat_map(|s| {
            [
                (s.x1 - cx).abs(),
                (s.x2 - cx).abs(),
                (s.y1 - cy).abs(),
                (s.y2 - cy).abs(),
            ]
        })
        .fold(extent, f64::max);
    let mut m = nalgebra::Matrix3::<f64>::zeros();
    for s in segments {
        let p = nalgebra::Vector3::new((s.x1 - cx) / scale, (s.y1 - cy) / scale, 1.0);
        let q = nalgebra::Vector3::new((s.x2 - cx) / scale, (s.y2 - cy) / scale, 1.0);
        let mut line = p.cross(&q);
        let n = line.x.hypot(line.y);
        if n < 1e-12 {
            continue;
        }
        line /= n;
        m += s.length() * line * line.transpose();
    }
    let eigen = m.symmetric_eigen();
    let (index, _) = eigen
        .eigenvalues
        .iter()
        .enumerate()
        .min_by(|a, b| a.1.total_cmp(b.1))?;
    let v = eigen.eigenvectors.column(index);
    // Back to pixels: x = cx + scale·x', y = cy + scale·y'.
    let point = [v[0] * scale + cx * v[2], v[1] * scale + cy * v[2], v[2]];
    let norm = (point[0] * point[0] + point[1] * point[1] + point[2] * point[2]).sqrt();
    (norm > 1e-12).then(|| [point[0] / norm, point[1] / norm, point[2] / norm])
}

/// Two guide lines through a vanishing point, a quarter of the frame in from
/// either side (or parallel to its direction when it lies at infinity).
fn lines_through(
    vp: [f64; 3],
    width: f64,
    height: f64,
    orientation: GuideOrientation,
    tag: &str,
) -> Vec<GuideLine> {
    let anchors = match orientation {
        GuideOrientation::Vertical => [(0.3 * width, 0.5 * height), (0.7 * width, 0.5 * height)],
        GuideOrientation::Horizontal => [(0.5 * width, 0.3 * height), (0.5 * width, 0.7 * height)],
    };
    let half = 0.3 * width.min(height);
    anchors
        .iter()
        .enumerate()
        .filter_map(|(k, &(ax, ay))| {
            let (dx, dy) = if vp[2].abs() > 1e-9 {
                (vp[0] / vp[2] - ax, vp[1] / vp[2] - ay)
            } else {
                (vp[0], vp[1])
            };
            let n = dx.hypot(dy);
            (n > 1e-9).then(|| {
                // Both ends inside the frame (guide lines are stored as 0–1).
                let (dirx, diry) = (dx / n, dy / n);
                let room = |pos: f64, size: f64, d: f64| {
                    if d.abs() < 1e-9 {
                        f64::MAX
                    } else {
                        (pos.min(size - pos) - 0.02 * size).max(0.0) / d.abs()
                    }
                };
                let length = half.min(room(ax, width, dirx)).min(room(ay, height, diry));
                let (ux, uy) = (dirx * length, diry * length);
                GuideLine {
                    id: format!("{tag}-{k}"),
                    orientation,
                    p1: GuidePoint {
                        x: ax - ux,
                        y: ay - uy,
                    },
                    p2: GuidePoint {
                        x: ax + ux,
                        y: ay + uy,
                    },
                }
            })
        })
        .collect()
}

/// Lightroom's Upright modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UprightMode {
    /// Vertical where the photo has enough converging verticals, else Level.
    Auto,
    /// Rotation only.
    Level,
    /// Converging verticals made parallel (and level).
    Vertical,
    /// Vertical and horizontal perspective.
    Full,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UprightResult {
    pub lines: Vec<GuideLine>,
    /// What was applied: "level", "vertical" or "full".
    pub applied: &'static str,
    /// Segments found, and those that agree with the result.
    pub segments: usize,
    pub agreeing: usize,
}

/// Guide lines for `mode` from the segments of a `width` × `height` photo.
pub fn upright_lines(
    segments: &[Segment],
    width: f64,
    height: f64,
    mode: UprightMode,
    portrait_on_sensor: bool,
) -> Option<UprightResult> {
    let diagonal = width.hypot(height);
    let near = |s: &&Segment, vertical: bool| {
        let deviation = if vertical {
            s.tilt_from_vertical()
        } else {
            s.angle()
        };
        deviation.abs() < 25f64.to_radians() && s.length() > 0.03 * diagonal
    };
    let verticals: Vec<Segment> = segments.iter().filter(|s| near(s, true)).copied().collect();
    let horizontals: Vec<Segment> = segments
        .iter()
        .filter(|s| near(s, false))
        .copied()
        .collect();
    let enough = |score: f64| score > 0.25 * diagonal;
    let v_vp = (verticals.len() >= 2)
        .then(|| vanishing_point(&verticals))
        .flatten();
    let h_vp = (horizontals.len() >= 2)
        .then(|| vanishing_point(&horizontals))
        .flatten();
    let count = |set: &[Segment], vp: [f64; 3]| {
        set.iter()
            .filter(|s| {
                let (mx, my) = s.midpoint();
                let (dx, dy) = if vp[2].abs() > 1e-9 {
                    (vp[0] / vp[2] - mx, vp[1] / vp[2] - my)
                } else {
                    (vp[0], vp[1])
                };
                wrap_half_turn(dy.atan2(dx) - s.angle()).abs() < PI / 180.0
            })
            .count()
    };
    let level = || -> Option<UprightResult> {
        // Length-weighted median tilt of all near-axis segments.
        let mut tilts: Vec<(f64, f64)> = verticals
            .iter()
            .map(|s| (s.tilt_from_vertical(), s.length()))
            .chain(horizontals.iter().map(|s| (s.angle(), s.length())))
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
        // Two parallel lines along the tilted verticals' direction (a vanishing
        // point at infinity): the guided correction rotates them upright.
        let along = [-tilt.0.sin(), tilt.0.cos(), 0.0];
        Some(UprightResult {
            lines: lines_through(along, width, height, GuideOrientation::Vertical, "level"),
            applied: "level",
            segments: segments.len(),
            agreeing: tilts.len(),
        })
    };
    // The photo's verticals: the sensor's horizontal axis for a photo turned
    // by 90° (guide lines are stored, and corrected, in sensor coordinates).
    let (upright_set, upright_vp, upright_axis, across_set, across_vp, across_axis) =
        if portrait_on_sensor {
            (
                &horizontals,
                h_vp,
                GuideOrientation::Horizontal,
                &verticals,
                v_vp,
                GuideOrientation::Vertical,
            )
        } else {
            (
                &verticals,
                v_vp,
                GuideOrientation::Vertical,
                &horizontals,
                h_vp,
                GuideOrientation::Horizontal,
            )
        };
    let vertical = |with_horizontal: bool| -> Option<UprightResult> {
        let (vp, score) = upright_vp?;
        if !enough(score) {
            return None;
        }
        let mut lines = lines_through(vp, width, height, upright_axis, "vertical");
        let mut agreeing = count(upright_set, vp);
        if with_horizontal {
            let (hvp, hscore) = across_vp?;
            if !enough(hscore) {
                return None;
            }
            lines.extend(lines_through(hvp, width, height, across_axis, "horizontal"));
            agreeing += count(across_set, hvp);
        }
        Some(UprightResult {
            lines,
            applied: if with_horizontal { "full" } else { "vertical" },
            segments: segments.len(),
            agreeing,
        })
    };
    match mode {
        UprightMode::Level => level(),
        UprightMode::Vertical => vertical(false),
        UprightMode::Full => vertical(true),
        UprightMode::Auto => vertical(false).or_else(level),
    }
}

/// Grayscale (perceptual 0–255) of an image downscaled to at most `max_side`.
pub fn analysis_image(image: &DynamicImage, max_side: u32) -> (Vec<f32>, usize, usize) {
    let (w, h) = (image.width(), image.height());
    let scale = (max_side as f64 / w.max(h) as f64).min(1.0);
    let small = if scale < 1.0 {
        crate::image_processing::thumbnail(
            image,
            (w as f64 * scale).round() as u32,
            (h as f64 * scale).round() as u32,
        )
    } else {
        image.clone()
    };
    let rgb = small.to_rgb32f();
    let (sw, sh) = rgb.dimensions();
    let lumas: Vec<f32> = rgb
        .pixels()
        .map(|p| 0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2])
        .collect();
    // Each pixel by its brightness rank (histogram equalisation), not scaled
    // by the brightest pixel: a dark, flat D600 frame of a picture on a wall
    // (linear mean 0.03, one bright highlight) left its frame's edges below
    // the gradient threshold (6 segments; Camera Raw found the verticals).
    // On 1024 perceptual levels below the 99.5th percentile first, so
    // differences far below a grey level (resampling noise in a flat area)
    // are not stretched into texture.
    let mut sorted = lumas.clone();
    sorted.sort_by(f32::total_cmp);
    let white = sorted
        .get(((sorted.len().saturating_sub(1)) as f32 * 0.995) as usize)
        .copied()
        .unwrap_or(1.0)
        .max(1e-6);
    let levels: Vec<u16> = lumas
        .iter()
        .map(|&luma| ((luma / white).clamp(0.0, 1.0).powf(1.0 / 2.2) * 1023.0).round() as u16)
        .collect();
    let mut histogram = [0u32; 1024];
    for &level in &levels {
        histogram[level as usize] += 1;
    }
    // Rank of each level: the share of pixels darker than it.
    let mut below = [0f32; 1024];
    let mut acc = 0u32;
    for (level, count) in histogram.iter().enumerate() {
        below[level] = acc as f32 / levels.len().max(1) as f32;
        acc += count;
    }
    let gray = levels
        .iter()
        .map(|&level| below[level as usize] * 255.0)
        .collect();
    (gray, sw as usize, sh as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A synthetic photo: dark background with bright bars along the given
    /// segments (anti-aliased by supersampling).
    fn draw(width: usize, height: usize, bars: &[Segment]) -> Vec<f32> {
        let mut img = vec![30f32; width * height];
        for y in 0..height {
            for x in 0..width {
                let (px, py) = (x as f64 + 0.5, y as f64 + 0.5);
                for s in bars {
                    let (dx, dy) = (s.x2 - s.x1, s.y2 - s.y1);
                    let len2 = dx * dx + dy * dy;
                    let t = (((px - s.x1) * dx + (py - s.y1) * dy) / len2).clamp(0.0, 1.0);
                    let d = (px - s.x1 - t * dx).hypot(py - s.y1 - t * dy);
                    if d < 2.0 {
                        img[y * width + x] = 220.0;
                    }
                }
            }
        }
        img
    }

    fn converging(width: f64, height: f64, vp_y: f64) -> Vec<Segment> {
        // Verticals of a building photographed from below meet above the frame.
        [0.15, 0.35, 0.65, 0.85]
            .iter()
            .map(|&f| {
                let xb = f * width;
                let (x2, y2) = (width / 2.0, vp_y);
                let t = (0.15 * height - 0.9 * height) / (y2 - 0.9 * height);
                Segment {
                    x1: xb,
                    y1: 0.9 * height,
                    x2: xb + (x2 - xb) * t,
                    y2: 0.15 * height,
                }
            })
            .collect()
    }

    #[test]
    fn segments_are_found_where_bars_are_drawn() {
        let bars = [
            Segment {
                x1: 20.0,
                y1: 30.0,
                x2: 220.0,
                y2: 40.0,
            },
            Segment {
                x1: 100.0,
                y1: 60.0,
                x2: 110.0,
                y2: 180.0,
            },
        ];
        let img = draw(256, 200, &bars);
        let found = detect_segments(&img, 256, 200, 30.0);
        for bar in &bars {
            assert!(
                found
                    .iter()
                    .any(
                        |s| wrap_half_turn(s.angle() - bar.angle()).abs() < 2f64.to_radians()
                            && (s.length() - bar.length()).abs() < 0.2 * bar.length()
                    ),
                "{bar:?} not in {found:?}"
            );
        }
    }

    #[test]
    fn vertical_mode_finds_the_vanishing_point_of_converging_verticals() {
        let (w, h) = (300usize, 400usize);
        let bars = converging(w as f64, h as f64, -900.0);
        let img = draw(w, h, &bars);
        let segments = detect_segments(&img, w, h, 20.0);
        let result =
            upright_lines(&segments, w as f64, h as f64, UprightMode::Vertical, false).unwrap();
        assert_eq!(result.applied, "vertical");
        assert_eq!(result.lines.len(), 2);
        // Both guide lines point at the vanishing point (150, −900).
        for line in &result.lines {
            let t = (-900.0 - line.p1.y) / (line.p2.y - line.p1.y);
            let x = line.p1.x + t * (line.p2.x - line.p1.x);
            assert!(
                (x - 150.0).abs() < 25.0,
                "{line:?} meets y = −900 at x = {x}"
            );
        }
        // Auto chooses the vertical correction for such a photo.
        let auto = upright_lines(&segments, w as f64, h as f64, UprightMode::Auto, false).unwrap();
        assert_eq!(auto.applied, "vertical");
    }

    #[test]
    fn level_mode_measures_the_tilt_of_a_rotated_scene() {
        let (w, h) = (400usize, 300usize);
        let tilt = 4f64.to_radians();
        let (c, s) = (tilt.cos(), tilt.sin());
        let rotate = |x: f64, y: f64| {
            let (dx, dy) = (x - 200.0, y - 150.0);
            (200.0 + dx * c - dy * s, 150.0 + dx * s + dy * c)
        };
        let bar = |a: (f64, f64), b: (f64, f64)| {
            let (p, q) = (rotate(a.0, a.1), rotate(b.0, b.1));
            Segment {
                x1: p.0,
                y1: p.1,
                x2: q.0,
                y2: q.1,
            }
        };
        let bars = [
            bar((60.0, 80.0), (340.0, 80.0)),
            bar((60.0, 220.0), (340.0, 220.0)),
            bar((120.0, 60.0), (120.0, 240.0)),
        ];
        let segments = detect_segments(&draw(w, h, &bars), w, h, 20.0);
        let result =
            upright_lines(&segments, w as f64, h as f64, UprightMode::Level, false).unwrap();
        assert_eq!(result.applied, "level");
        // Both lines are parallel and tilted like the scene's verticals.
        for line in &result.lines {
            let deviation = (line.p2.x - line.p1.x).atan2(line.p2.y - line.p1.y);
            assert!(
                (deviation.abs() - tilt).abs() < 0.6f64.to_radians(),
                "{deviation}"
            );
        }
    }

    #[test]
    fn the_engine_makes_the_detected_verticals_parallel_and_upright() {
        let (w, h) = (300usize, 400usize);
        let bars = converging(w as f64, h as f64, -900.0);
        let img = draw(w, h, &bars);
        let segments = detect_segments(&img, w, h, 20.0);
        let mut result =
            upright_lines(&segments, w as f64, h as f64, UprightMode::Vertical, false).unwrap();
        for line in &mut result.lines {
            for p in [&mut line.p1, &mut line.p2] {
                p.x /= w as f64;
                p.y /= h as f64;
            }
        }
        let gray =
            image::GrayImage::from_raw(w as u32, h as u32, img.iter().map(|&v| v as u8).collect())
                .unwrap();
        let source = DynamicImage::ImageLuma8(gray).to_rgb8();
        let adjustments = serde_json::json!({
            "guidedPerspective": {"enabled": true, "lines": result.lines, "autoCrop": true, "upright": "vertical"}
        });
        let source = DynamicImage::ImageRgb8(source);
        let warped = crate::image_processing::apply_geometry_warp(&source, &adjustments);
        let (gray, ww, wh) = analysis_image(&warped, 400);
        let after = detect_segments(&gray, ww, wh, 20.0);
        let tilts: Vec<f64> = after
            .iter()
            .filter(|s| s.length() > 60.0 && s.tilt_from_vertical().abs() < 20f64.to_radians())
            .map(|s| s.tilt_from_vertical().to_degrees())
            .collect();
        assert!(tilts.len() >= 3, "{after:?}");
        for tilt in &tilts {
            assert!(
                tilt.abs() < 0.5,
                "verticals after the correction: {tilts:?}"
            );
        }
    }

    #[test]
    fn nothing_to_straighten_gives_no_vertical_result() {
        let img = vec![40f32; 200 * 150];
        let segments = detect_segments(&img, 200, 150, 10.0);
        assert!(segments.is_empty());
        assert!(upright_lines(&segments, 200.0, 150.0, UprightMode::Vertical, false).is_none());
        assert!(upright_lines(&segments, 200.0, 150.0, UprightMode::Auto, false).is_none());
    }
}

#[cfg(test)]
mod diagnose {
    use super::*;
    /// UPRIGHT_IMAGE=/path cargo test -p rawtonic-app --lib upright::diagnose -- --ignored --nocapture
    #[test]
    #[ignore]
    fn print_vanishing_point() {
        let Ok(path) = std::env::var("UPRIGHT_IMAGE") else {
            return;
        };
        let image = image::open(path).unwrap();
        let (gray, w, h) = analysis_image(&image, 1400);
        let segments = detect_segments(&gray, w, h, (w as f64).hypot(h as f64) * 0.02);
        let verticals: Vec<Segment> = segments
            .iter()
            .filter(|s| {
                s.tilt_from_vertical().abs() < 25f64.to_radians()
                    && s.length() > 0.03 * (w as f64).hypot(h as f64)
            })
            .copied()
            .collect();
        let (vp, score) = vanishing_point(&verticals).unwrap();
        println!(
            "{w}x{h}: {} segments, {} verticals, vp {:?} (x {:.0}, y {:.0}), score {score:.0}",
            segments.len(),
            verticals.len(),
            vp,
            vp[0] / vp[2],
            vp[1] / vp[2]
        );
        let r = upright_lines(&segments, w as f64, h as f64, UprightMode::Vertical, false);
        println!("{:?}", r.map(|r| r.lines));
    }
}

#[cfg(test)]
mod diagnose_warp {
    use super::*;
    /// UPRIGHT_IMAGE=in.jpg UPRIGHT_OUT=out.jpg UPRIGHT_LINES='[...]' cargo test ... -- --ignored
    #[test]
    fn constrain_crop_off_keeps_the_frame_unzoomed() {
        let params = |extra: serde_json::Value| {
            let mut guided =
                serde_json::json!({"enabled": true, "lines": [], "upright": "vertical"});
            guided
                .as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            crate::image_processing::get_geometry_params_from_json(
                &serde_json::json!({ "guidedPerspective": guided }),
            )
            .guided_fill
        };
        assert!(params(serde_json::json!({})));
        assert!(params(serde_json::json!({"autoCrop": true})));
        assert!(!params(serde_json::json!({"autoCrop": false})));
        assert!(!params(serde_json::json!({"upright": "off"})));
    }

    #[test]
    #[ignore]
    fn warp_with_lines() {
        let (Ok(input), Ok(out), Ok(lines)) = (
            std::env::var("UPRIGHT_IMAGE"),
            std::env::var("UPRIGHT_OUT"),
            std::env::var("UPRIGHT_LINES"),
        ) else {
            return;
        };
        let image = image::open(input).unwrap();
        let lines: serde_json::Value = if lines == "vertical" || lines == "level" || lines == "full"
        {
            let mode = match lines.as_str() {
                "level" => UprightMode::Level,
                "full" => UprightMode::Full,
                _ => UprightMode::Vertical,
            };
            let (gray, w, h) = analysis_image(&image, 1400);
            let segments = detect_segments(&gray, w, h, (w as f64).hypot(h as f64) * 0.02);
            let mut result = upright_lines(&segments, w as f64, h as f64, mode, false).unwrap();
            for line in &mut result.lines {
                for p in [&mut line.p1, &mut line.p2] {
                    p.x /= w as f64;
                    p.y /= h as f64;
                }
            }
            serde_json::to_value(result.lines).unwrap()
        } else {
            serde_json::from_str(&lines).unwrap()
        };
        let adjustments = serde_json::json!({"guidedPerspective": {"enabled": true, "lines": lines, "autoCrop": true, "upright": "vertical"}});
        let warped = crate::image_processing::apply_geometry_warp(&image, &adjustments);
        warped.to_rgb8().save(out).unwrap();
    }
}
