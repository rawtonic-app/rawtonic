//! Spot removal: Lightroom's Healing / Capture One's Heal and Clone as
//! parametric, full-precision operations on the photo's base image.
//!
//! Each spot is a circle (centre, radius) with a source circle of the same
//! size. Clone copies the source with a feathered edge. Heal copies the
//! source's texture and matches it to the surroundings: the difference
//! between target and source along the circle is interpolated into the disc
//! as a harmonic function (the Poisson integral of the disc, which is what
//! Poisson image cloning solves for a circular region), so tone and colour
//! join seamlessly while the source's grain and detail come along.
//!
//! Spots are kept in the coordinates the masks use (the photo after its
//! orientation steps, flips, straightening and geometry correction, before
//! the crop) and applied in float on the base image before every
//! adjustment, so later edits see healed pixels at full precision. The
//! inherited heal/clone patches were baked as 8-bit JPEG.

use image::{DynamicImage, Rgb32FImage};
use rayon::prelude::*;
use serde::Deserialize;
use serde_json::Value;

use crate::lightroom_retouch::BaseSpot;

/// One spot as the frontend stores it (`adjustments.spots`).
#[derive(Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Spot {
    #[serde(default = "default_mode")]
    pub mode: String,
    pub x: f64,
    pub y: f64,
    pub radius: f64,
    pub source_x: f64,
    pub source_y: f64,
    /// Soft edge, 0–100 % of the radius.
    #[serde(default = "default_feather")]
    pub feather: f64,
    /// 0–100.
    #[serde(default = "default_opacity")]
    pub opacity: f64,
    #[serde(default = "default_visible")]
    pub visible: bool,
    /// A brush stroke (Lightroom's heal strokes, Capture One's heal mask):
    /// the path the brush of `radius` followed. `x`/`y` is its first point
    /// and `sourceX`/`sourceY` where that point copies from, so the whole
    /// stroke copies from one offset. Fewer than two points: a circle.
    #[serde(default)]
    pub points: Vec<StrokePoint>,
}

#[derive(Deserialize, serde::Serialize, Debug, Clone, Copy, PartialEq)]
pub struct StrokePoint {
    pub x: f64,
    pub y: f64,
}

fn default_mode() -> String {
    "heal".into()
}
fn default_feather() -> f64 {
    0.0
}
fn default_opacity() -> f64 {
    100.0
}
fn default_visible() -> bool {
    true
}

/// The visible spots of a recipe.
pub fn spots(adjustments: &Value) -> Vec<Spot> {
    adjustments
        .get("spots")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| serde_json::from_value::<Spot>(item.clone()).ok())
                .filter(|s| s.visible && s.radius > 0.5 && s.opacity > 0.0)
                .collect()
        })
        .unwrap_or_default()
}

/// `adjustments` with its spots placed for a copy of the base image at
/// `scale` of its full resolution (spot positions and sizes are in
/// full-resolution pixels of the mask frame). Only `spots` is kept.
pub fn scaled_spots_recipe(adjustments: &Value, scale: f64) -> Value {
    let mut recipe = serde_json::json!({});
    for key in [
        "orientationSteps",
        "rotation",
        "flipHorizontal",
        "flipVertical",
    ] {
        if let Some(value) = adjustments.get(key) {
            recipe[key] = value.clone();
        }
    }
    let Some(items) = adjustments.get("spots").and_then(Value::as_array) else {
        return recipe;
    };
    let scale_value = |item: &mut Value, key: &str| {
        if let Some(v) = item.get(key).and_then(Value::as_f64) {
            item[key] = serde_json::json!(v * scale);
        }
    };
    let spots: Vec<Value> = items
        .iter()
        .cloned()
        .map(|mut item| {
            for key in ["x", "y", "radius", "sourceX", "sourceY"] {
                scale_value(&mut item, key);
            }
            if let Some(points) = item.get_mut("points").and_then(Value::as_array_mut) {
                for point in points {
                    scale_value(point, "x");
                    scale_value(point, "y");
                }
            }
            item
        })
        .collect();
    recipe["spots"] = Value::Array(spots);
    recipe
}

/// Whether the recipe has spots to apply, Lightroom's imported ones not yet
/// placed included (`lightroom_retouch`).
pub fn has_spots(adjustments: &Value) -> bool {
    !spots(adjustments).is_empty() || crate::lightroom_retouch::has_renderable(adjustments, "spots")
}

/// Mask-frame dimensions of a base image (orientation steps swap them).
pub(crate) fn mask_frame(adjustments: &Value, width: u32, height: u32) -> (f64, f64) {
    let steps = adjustments["orientationSteps"].as_u64().unwrap_or(0);
    if steps % 2 == 1 {
        (height as f64, width as f64)
    } else {
        (width as f64, height as f64)
    }
}

/// A mask-frame point in base-image pixels.
pub(crate) fn to_base(adjustments: &Value, frame: (f64, f64), x: f64, y: f64) -> (f64, f64) {
    crate::image_processing::inverse_transform_point(x, y, frame.0, frame.1, adjustments)
}

/// Working space for healing: square root of the (linear) values, so the
/// interpolated difference behaves perceptually and never goes negative.
fn encode(v: f32) -> f32 {
    v.max(0.0).sqrt()
}
fn decode(v: f32) -> f32 {
    v.max(0.0) * v.max(0.0)
}

fn sample(image: &Rgb32FImage, x: f64, y: f64) -> [f32; 3] {
    let (w, h) = image.dimensions();
    let x = x.clamp(0.0, w as f64 - 1.0);
    let y = y.clamp(0.0, h as f64 - 1.0);
    let (x0, y0) = (x.floor() as u32, y.floor() as u32);
    let (x1, y1) = ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1));
    let (fx, fy) = ((x - x0 as f64) as f32, (y - y0 as f64) as f32);
    let p = |xx, yy| image.get_pixel(xx, yy).0;
    let (a, b, c, d) = (p(x0, y0), p(x1, y0), p(x0, y1), p(x1, y1));
    std::array::from_fn(|i| {
        (a[i] * (1.0 - fx) + b[i] * fx) * (1.0 - fy) + (c[i] * (1.0 - fx) + d[i] * fx) * fy
    })
}

/// Coverage of a pixel at `distance` from the centre: full inside the
/// feathered edge, a smoothstep to zero at the radius.
pub(crate) fn coverage(distance: f64, radius: f64, feather: f64) -> f32 {
    let inner = radius * (1.0 - feather.clamp(0.0, 1.0));
    if distance <= inner {
        return 1.0;
    }
    if distance >= radius {
        return 0.0;
    }
    let t = ((radius - distance) / (radius - inner).max(1e-6)) as f32;
    t * t * (3.0 - 2.0 * t)
}

/// Heals or clones one spot in base-image pixels (`centre`, `radius`,
/// `offset` from target to source).
fn apply_spot(
    image: &mut Rgb32FImage,
    centre: (f64, f64),
    radius: f64,
    offset: (f64, f64),
    heal: bool,
    feather: f64,
    opacity: f32,
) {
    let (w, h) = image.dimensions();
    let (cx, cy) = centre;
    let x0 = (cx - radius).floor().max(0.0) as u32;
    let y0 = (cy - radius).floor().max(0.0) as u32;
    let x1 = ((cx + radius).ceil() as i64).clamp(0, w as i64 - 1) as u32;
    let y1 = ((cy + radius).ceil() as i64).clamp(0, h as i64 - 1) as u32;
    if x0 > x1 || y0 > y1 {
        return;
    }

    // Target − source along the circle (working space), averaged over a
    // thin band and smoothed along the circle so that grain and single
    // texture pixels do not print into the healed area.
    let boundary: Vec<[f32; 3]> = if heal {
        let count = ((std::f64::consts::TAU * radius / 1.5).round() as usize).clamp(24, 720);
        let raw: Vec<[f32; 3]> = (0..count)
            .map(|k| {
                let phi = std::f64::consts::TAU * k as f64 / count as f64;
                let (c, s) = (phi.cos(), phi.sin());
                let mut acc = [0.0f32; 3];
                for band in [-1.0, 0.0, 1.0] {
                    let r = (radius + band).max(0.5);
                    let t = sample(image, cx + r * c, cy + r * s);
                    let src = sample(image, cx + offset.0 + r * c, cy + offset.1 + r * s);
                    for i in 0..3 {
                        acc[i] += (encode(t[i]) - encode(src[i])) / 3.0;
                    }
                }
                acc
            })
            .collect();
        let reach = (count / 48).max(1);
        (0..count)
            .map(|k| {
                let mut acc = [0.0f32; 3];
                for j in 0..=2 * reach {
                    let v = raw[(k + count + j - reach) % count];
                    for i in 0..3 {
                        acc[i] += v[i];
                    }
                }
                acc.map(|v| v / (2 * reach + 1) as f32)
            })
            .collect()
    } else {
        Vec::new()
    };
    let angles: Vec<(f64, f64)> = (0..boundary.len())
        .map(|k| {
            let phi = std::f64::consts::TAU * k as f64 / boundary.len() as f64;
            (phi.cos(), phi.sin())
        })
        .collect();

    let width = (x1 - x0 + 1) as usize;
    let snapshot = &*image;
    let rows: Vec<(u32, Vec<Option<[f32; 3]>>)> = (y0..=y1)
        .into_par_iter()
        .map(|y| {
            let row = (0..width)
                .map(|i| {
                    let x = x0 + i as u32;
                    let (dx, dy) = (x as f64 - cx, y as f64 - cy);
                    let distance = dx.hypot(dy);
                    let alpha = coverage(distance, radius, feather) * opacity;
                    if alpha <= 0.0 {
                        return None;
                    }
                    let target = snapshot.get_pixel(x, y).0;
                    let source = sample(snapshot, x as f64 + offset.0, y as f64 + offset.1);
                    let replaced = if heal {
                        // Discrete Poisson integral of the disc.
                        let r2 = distance * distance;
                        let rr = radius * radius;
                        let mut weight_sum = 0.0f64;
                        let mut acc = [0.0f64; 3];
                        for (b, (c, s)) in boundary.iter().zip(&angles) {
                            let along = dx * c + dy * s;
                            let wgt =
                                ((rr - r2) / (rr + r2 - 2.0 * radius * along).max(1e-9)).max(0.0);
                            weight_sum += wgt;
                            for i in 0..3 {
                                acc[i] += wgt * b[i] as f64;
                            }
                        }
                        let m = acc.map(|v| (v / weight_sum.max(1e-12)) as f32);
                        std::array::from_fn(|i| decode(encode(source[i]) + m[i]))
                    } else {
                        source
                    };
                    Some(std::array::from_fn(|i| {
                        target[i] + (replaced[i] - target[i]) * alpha
                    }))
                })
                .collect();
            (y, row)
        })
        .collect();
    for (y, row) in rows {
        for (i, value) in row.into_iter().enumerate() {
            if let Some(v) = value {
                image.put_pixel(x0 + i as u32, y, image::Rgb(v));
            }
        }
    }
}

/// Distance from `p` to the polyline `path`.
fn distance_to_path(p: (f64, f64), path: &[(f64, f64)]) -> f64 {
    let mut best = f64::INFINITY;
    for pair in path.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        let (vx, vy) = (b.0 - a.0, b.1 - a.1);
        let length2 = vx * vx + vy * vy;
        let t = if length2 > 0.0 {
            (((p.0 - a.0) * vx + (p.1 - a.1) * vy) / length2).clamp(0.0, 1.0)
        } else {
            0.0
        };
        best = best.min((p.0 - a.0 - t * vx).hypot(p.1 - a.1 - t * vy));
    }
    best
}

/// Solves Laplace's equation for `u` where `free` is set, with the other
/// cells as fixed (Dirichlet) values: the membrane that Poisson cloning adds
/// to the source. Cascadic multigrid: the problem is solved on a grid of half
/// the size, carried up as the starting value and smoothed with red-black
/// Gauss-Seidel sweeps; cells beyond the grid's edge are left out (Neumann).
fn solve_laplace(u: &mut [[f32; 3]], free: &[bool], width: usize, height: usize) {
    // Over-relaxation of the sweeps.
    const OMEGA: f32 = 1.6;
    if !free.iter().any(|&f| f) {
        return;
    }
    let sweeps = if width.max(height) <= 16 {
        // The coarsest grid: converge outright.
        400
    } else {
        let (cw, ch) = (width.div_ceil(2), height.div_ceil(2));
        let mut coarse = vec![[0.0f32; 3]; cw * ch];
        let mut coarse_free = vec![true; cw * ch];
        for cy in 0..ch {
            for cx in 0..cw {
                // A coarse cell is fixed when any of its cells is, at their mean.
                let mut sum = [0.0f32; 3];
                let mut fixed = 0;
                for (x, y) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    let (fx, fy) = (2 * cx + x, 2 * cy + y);
                    if fx < width && fy < height && !free[fy * width + fx] {
                        let v = u[fy * width + fx];
                        for i in 0..3 {
                            sum[i] += v[i];
                        }
                        fixed += 1;
                    }
                }
                if fixed > 0 {
                    coarse[cy * cw + cx] = sum.map(|v| v / fixed as f32);
                    coarse_free[cy * cw + cx] = false;
                }
            }
        }
        solve_laplace(&mut coarse, &coarse_free, cw, ch);
        for y in 0..height {
            for x in 0..width {
                if free[y * width + x] {
                    u[y * width + x] = coarse[(y / 2) * cw + x / 2];
                }
            }
        }
        60
    };
    // Free cells of each colour; same-coloured cells never neighbour, so a
    // colour can be updated in place or in parallel.
    let colours: [Vec<usize>; 2] = std::array::from_fn(|colour| {
        (0..width * height)
            .filter(|&i| free[i] && (i % width + i / width) % 2 == colour)
            .collect()
    });
    let relaxed = |u: &[[f32; 3]], at: usize| -> [f32; 3] {
        let (x, y) = (at % width, at / width);
        let mut sum = [0.0f32; 3];
        let mut n = 0.0f32;
        let mut add = |i: usize| {
            for c in 0..3 {
                sum[c] += u[i][c];
            }
            n += 1.0;
        };
        if x > 0 {
            add(at - 1);
        }
        if x + 1 < width {
            add(at + 1);
        }
        if y > 0 {
            add(at - width);
        }
        if y + 1 < height {
            add(at + width);
        }
        let old = u[at];
        if n == 0.0 {
            return old;
        }
        std::array::from_fn(|c| old[c] + OMEGA * (sum[c] / n - old[c]))
    };
    let mut buffer = Vec::new();
    for _ in 0..sweeps {
        for cells in &colours {
            if cells.len() < 32_768 {
                for &at in cells {
                    u[at] = relaxed(u, at);
                }
            } else {
                let snapshot = &*u;
                cells
                    .par_iter()
                    .map(|&at| relaxed(snapshot, at))
                    .collect_into_vec(&mut buffer);
                for (&at, v) in cells.iter().zip(&buffer) {
                    u[at] = *v;
                }
            }
        }
    }
}

/// Heals or clones a brush stroke in base-image pixels: the area within
/// `radius` of `path`, copied from `offset` (whole pixels). Heal solves for
/// the smooth membrane that joins the copy to the target along the stroke's
/// edge (Poisson cloning for any shape; on a disc it is the Poisson integral
/// `apply_spot` uses), in the same square-root working space.
fn apply_stroke(
    image: &mut Rgb32FImage,
    path: &[(f64, f64)],
    radius: f64,
    offset: (f64, f64),
    heal: bool,
    feather: f64,
    opacity: f32,
) {
    let (w, h) = image.dimensions();
    // Smoothing of the edge difference, as along the disc's rim (about an
    // eighth of the radius each side), so grain does not print in.
    let reach = ((radius * 0.13).round() as usize).max(1);
    let margin = radius + reach as f64 + 2.0;
    let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for p in path {
        x0 = x0.min(p.0 - margin);
        y0 = y0.min(p.1 - margin);
        x1 = x1.max(p.0 + margin);
        y1 = y1.max(p.1 + margin);
    }
    let x0 = x0.floor().max(0.0) as u32;
    let y0 = y0.floor().max(0.0) as u32;
    let x1 = (x1.ceil() as i64).clamp(0, w as i64 - 1) as u32;
    let y1 = (y1.ceil() as i64).clamp(0, h as i64 - 1) as u32;
    if x0 > x1 || y0 > y1 {
        return;
    }
    let (gw, gh) = ((x1 - x0 + 1) as usize, (y1 - y0 + 1) as usize);
    let source_at = |x: u32, y: u32| sample(image, x as f64 + offset.0, y as f64 + offset.1);

    let alpha: Vec<f32> = (0..gw * gh)
        .into_par_iter()
        .map(|i| {
            let p = ((x0 as usize + i % gw) as f64, (y0 as usize + i / gw) as f64);
            coverage(distance_to_path(p, path), radius, feather) * opacity
        })
        .collect();

    let membrane: Vec<[f32; 3]> = if heal {
        // Target − source over the grid; inside the stroke it is solved for.
        let difference: Vec<[f32; 3]> = (0..gw * gh)
            .into_par_iter()
            .map(|i| {
                let (x, y) = (x0 + (i % gw) as u32, y0 + (i / gw) as u32);
                let t = image.get_pixel(x, y).0;
                let s = source_at(x, y);
                std::array::from_fn(|c| encode(t[c]) - encode(s[c]))
            })
            .collect();
        let free: Vec<bool> = alpha.iter().map(|&a| a > 0.0).collect();
        // Edge values: the mean difference of the outside cells within
        // `reach` (summed-area tables of the masked difference).
        let stride = gw + 1;
        let mut sums = vec![[0.0f64; 4]; stride * (gh + 1)];
        for y in 0..gh {
            for x in 0..gw {
                let i = y * gw + x;
                let mut v = [0.0f64; 4];
                if !free[i] {
                    for c in 0..3 {
                        v[c] = difference[i][c] as f64;
                    }
                    v[3] = 1.0;
                }
                let (up, left, diag) = (
                    sums[y * stride + x + 1],
                    sums[(y + 1) * stride + x],
                    sums[y * stride + x],
                );
                sums[(y + 1) * stride + x + 1] =
                    std::array::from_fn(|c| v[c] + up[c] + left[c] - diag[c]);
            }
        }
        let mut u: Vec<[f32; 3]> = (0..gw * gh)
            .into_par_iter()
            .map(|i| {
                if free[i] {
                    return [0.0; 3];
                }
                let (x, y) = (i % gw, i / gw);
                let (ax, ay) = (x.saturating_sub(reach), y.saturating_sub(reach));
                let (bx, by) = ((x + reach + 1).min(gw), (y + reach + 1).min(gh));
                let s: [f64; 4] = std::array::from_fn(|c| {
                    sums[by * stride + bx][c]
                        - sums[ay * stride + bx][c]
                        - sums[by * stride + ax][c]
                        + sums[ay * stride + ax][c]
                });
                std::array::from_fn(|c| (s[c] / s[3].max(1.0)) as f32)
            })
            .collect();
        solve_laplace(&mut u, &free, gw, gh);
        u
    } else {
        Vec::new()
    };

    let snapshot = &*image;
    let updates: Vec<(u32, u32, [f32; 3])> = (0..gw * gh)
        .into_par_iter()
        .filter_map(|i| {
            let a = alpha[i];
            if a <= 0.0 {
                return None;
            }
            let (x, y) = (x0 + (i % gw) as u32, y0 + (i / gw) as u32);
            let target = snapshot.get_pixel(x, y).0;
            let source = source_at(x, y);
            let replaced: [f32; 3] = if heal {
                std::array::from_fn(|c| decode(encode(source[c]) + membrane[i][c]))
            } else {
                source
            };
            Some((
                x,
                y,
                std::array::from_fn(|c| target[c] + (replaced[c] - target[c]) * a),
            ))
        })
        .collect();
    for (x, y, v) in updates {
        image.put_pixel(x, y, image::Rgb(v));
    }
}

/// The base image with the recipe's spots applied (float RGB), or `None`
/// without visible spots.
///
/// Lightroom's imported spots not yet placed (`lightroom_retouch`) apply
/// where they stand in the list, placed on this base image exactly as the
/// editor will place them (`placed_spot`), so opening the photo changes no
/// pixel; one the geometry cannot show is applied at its base pixels.
pub fn apply_spots(base: &DynamicImage, adjustments: &Value) -> Option<DynamicImage> {
    let (width, height) = (base.width(), base.height());
    let frame = mask_frame(adjustments, width, height);
    let spots: Vec<(Spot, BaseSpot)> = adjustments
        .get("spots")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let placed = if item.get(crate::lightroom_retouch::PENDING).is_some() {
                match crate::lightroom_retouch::placed_spot(item, adjustments, width, height, None)
                {
                    Some(placed) => placed,
                    None => return unplaceable(item, width, height),
                }
            } else {
                item.clone()
            };
            let spot = serde_json::from_value::<Spot>(placed).ok()?;
            let centre = to_base(adjustments, frame, spot.x, spot.y);
            let edge = to_base(adjustments, frame, spot.x + spot.radius, spot.y);
            let base = BaseSpot {
                centre,
                radius: (edge.0 - centre.0).hypot(edge.1 - centre.1),
                source: to_base(adjustments, frame, spot.source_x, spot.source_y),
                path: if spot.points.len() >= 2 {
                    spot.points
                        .iter()
                        .map(|p| to_base(adjustments, frame, p.x, p.y))
                        .collect()
                } else {
                    Vec::new()
                },
            };
            Some((spot, base))
        })
        .filter(|(s, _)| s.visible && s.radius > 0.5 && s.opacity > 0.0)
        .collect();
    if spots.is_empty() {
        return None;
    }
    let mut image = base.to_rgb32f();
    for (spot, base) in &spots {
        let BaseSpot { centre, source, .. } = *base;
        let radius = base.radius.max(0.5);
        if base.path.len() >= 2 {
            let path = &base.path;
            // The offset is taken at the first point, which `x`/`y` holds.
            let first = path[0];
            apply_stroke(
                &mut image,
                path,
                radius,
                ((source.0 - first.0).round(), (source.1 - first.1).round()),
                spot.mode != "clone",
                spot.feather / 100.0,
                (spot.opacity / 100.0).clamp(0.0, 1.0) as f32,
            );
            continue;
        }
        apply_spot(
            &mut image,
            centre,
            radius,
            // Whole pixels: a fractional offset would resample (blur) the
            // copied texture; Camera Raw's heals keep full grain.
            ((source.0 - centre.0).round(), (source.1 - centre.1).round()),
            spot.mode != "clone",
            spot.feather / 100.0,
            (spot.opacity / 100.0).clamp(0.0, 1.0) as f32,
        );
    }
    Some(DynamicImage::ImageRgb32F(image))
}

/// A pending Lightroom spot the geometry cannot show in the mask frame, at
/// its base pixels (`apply_spots`).
fn unplaceable(item: &Value, width: u32, height: u32) -> Option<(Spot, BaseSpot)> {
    let base =
        crate::lightroom_retouch::base_spot(item, f64::from(width), f64::from(height), None)?;
    // Its settings with the base geometry, for the visibility filter.
    let mut settings = item.clone();
    settings["x"] = base.centre.0.into();
    settings["y"] = base.centre.1.into();
    settings["radius"] = base.radius.into();
    settings["sourceX"] = base.source.0.into();
    settings["sourceY"] = base.source.1.into();
    settings["points"] = Value::Array(Vec::new());
    Some((serde_json::from_value::<Spot>(settings).ok()?, base))
}

/// Mean squared difference (working space) between the surroundings of a
/// target circle and of a candidate source circle: rings at 1.1–1.7 radii.
fn surround_difference(
    image: &Rgb32FImage,
    target: (f64, f64),
    source: (f64, f64),
    radius: f64,
) -> f64 {
    let mut sum = 0.0;
    let mut count = 0usize;
    for ring in [1.1, 1.35, 1.7] {
        let samples = 48;
        for k in 0..samples {
            let phi = std::f64::consts::TAU * k as f64 / samples as f64;
            let (c, s) = (phi.cos() * ring * radius, phi.sin() * ring * radius);
            let a = sample(image, target.0 + c, target.1 + s);
            let b = sample(image, source.0 + c, source.1 + s);
            for i in 0..3 {
                let d = (encode(a[i]) - encode(b[i])) as f64;
                sum += d * d;
            }
            count += 3;
        }
    }
    sum / count.max(1) as f64
}

/// Mean and standard deviation per channel (working space) of samples on
/// rings at the given multiples of `radius` around `centre`.
fn ring_statistics(
    image: &Rgb32FImage,
    centre: (f64, f64),
    radius: f64,
    rings: &[f64],
) -> ([f64; 3], [f64; 3]) {
    let mut sum = [0.0f64; 3];
    let mut sq = [0.0f64; 3];
    let mut n = 0.0;
    for ring in rings {
        let samples = if *ring == 0.0 { 1 } else { 40 };
        for k in 0..samples {
            let phi = std::f64::consts::TAU * k as f64 / samples as f64;
            let v = sample(
                image,
                centre.0 + ring * radius * phi.cos(),
                centre.1 + ring * radius * phi.sin(),
            );
            for i in 0..3 {
                let e = encode(v[i]) as f64;
                sum[i] += e;
                sq[i] += e * e;
            }
            n += 1.0;
        }
    }
    let mean = sum.map(|v| v / n);
    let std = std::array::from_fn(|i| (sq[i] / n - mean[i] * mean[i]).max(0.0).sqrt());
    (mean, std)
}

/// The best source centre (base pixels) for a target circle: the candidate
/// whose surroundings match the target's best, at 2.5–5 radii, whole
/// circle and surroundings inside the photo; slightly prefers near ones.
fn best_source(image: &Rgb32FImage, target: (f64, f64), radius: f64) -> Option<(f64, f64)> {
    let (w, h) = (image.width() as f64, image.height() as f64);
    let margin = radius * 1.8;
    let inside = |p: (f64, f64)| {
        p.0 - margin >= 0.0 && p.1 - margin >= 0.0 && p.0 + margin < w && p.1 + margin < h
    };
    // The target's surroundings, as statistics a good source interior
    // should share (a tussock or a branch inside the source would be copied).
    let surround = ring_statistics(image, target, radius, &[1.15, 1.4, 1.7]);
    let score = |p: (f64, f64), distance: f64| {
        let inside = ring_statistics(image, p, radius, &[0.0, 0.3, 0.55, 0.8]);
        // An object inside the source (a tussock, a twig) differs from the
        // source's own rim even when it shares the surroundings' colour.
        let core = ring_statistics(image, p, radius, &[0.0, 0.25, 0.5]);
        let rim = ring_statistics(image, p, radius, &[0.95, 1.1]);
        let content: f64 = (0..3)
            .map(|i| {
                let mean = inside.0[i] - surround.0[i];
                let spread = inside.1[i] - surround.1[i];
                let object = core.0[i] - rim.0[i];
                mean * mean + spread * spread + 4.0 * object * object
            })
            .sum::<f64>()
            / 3.0;
        (surround_difference(image, target, p, radius) + content) * (1.0 + 0.04 * distance / radius)
    };
    let mut best: Option<((f64, f64), f64)> = None;
    for distance in [2.5, 3.2, 4.0, 5.0].map(|d| d * radius) {
        for k in 0..32 {
            let phi = std::f64::consts::TAU * k as f64 / 32.0;
            let p = (
                target.0 + distance * phi.cos(),
                target.1 + distance * phi.sin(),
            );
            if !inside(p) {
                continue;
            }
            let s = score(p, distance);
            if best.is_none_or(|(_, b)| s < b) {
                best = Some((p, s));
            }
        }
    }
    // Refine around the best candidate.
    let (mut point, mut value) = best?;
    let mut step = radius * 0.5;
    while step >= 1.0 {
        let mut improved = false;
        for (dx, dy) in [(step, 0.0), (-step, 0.0), (0.0, step), (0.0, -step)] {
            let p = (point.0 + dx, point.1 + dy);
            let distance = (p.0 - target.0).hypot(p.1 - target.1);
            if !inside(p) || distance < 2.2 * radius {
                continue;
            }
            let s = score(p, distance);
            if s < value {
                (point, value) = (p, s);
                improved = true;
            }
        }
        if !improved {
            step /= 2.0;
        }
    }
    Some(point)
}

/// The automatic source for a new spot at mask-frame (`x`, `y`) with
/// `radius`, in mask-frame coordinates: searched on the base image (with
/// the recipe's patches and spots) and carried back through the local
/// derivative of the geometry.
pub fn find_source(
    base: &DynamicImage,
    adjustments: &Value,
    x: f64,
    y: f64,
    radius: f64,
) -> Option<(f64, f64)> {
    let image = base.to_rgb32f();
    let frame = mask_frame(adjustments, image.width(), image.height());
    let centre = to_base(adjustments, frame, x, y);
    let ex = to_base(adjustments, frame, x + 1.0, y);
    let base_radius = radius * (ex.0 - centre.0).hypot(ex.1 - centre.1);
    let source = best_source(&image, centre, base_radius.max(1.0))?;
    to_mask_point(
        adjustments,
        frame,
        (x, y),
        (source.0 - centre.0, source.1 - centre.1),
    )
}

/// The mask-frame point that lies `offset` base pixels from mask-frame
/// `point`, through the inverse of the geometry's local derivative.
pub(crate) fn to_mask_point(
    adjustments: &Value,
    frame: (f64, f64),
    point: (f64, f64),
    offset: (f64, f64),
) -> Option<(f64, f64)> {
    let (x, y) = point;
    let centre = to_base(adjustments, frame, x, y);
    let ex = to_base(adjustments, frame, x + 1.0, y);
    let ey = to_base(adjustments, frame, x, y + 1.0);
    let (a, b) = (ex.0 - centre.0, ey.0 - centre.0);
    let (c, d) = (ex.1 - centre.1, ey.1 - centre.1);
    let det = a * d - b * c;
    if det.abs() < 1e-9 {
        return None;
    }
    let (ox, oy) = offset;
    Some((x + (d * ox - b * oy) / det, y + (-c * ox + a * oy) / det))
}

/// Sample points of a stroke's neighbourhood in base pixels, on a grid of
/// about `radius / 3` (coarser for long strokes), sorted by their distance
/// from the path in units of `radius`: (point, distance / radius).
fn stroke_samples(path: &[(f64, f64)], radius: f64, reach: f64) -> Vec<((f64, f64), f64)> {
    let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for p in path {
        x0 = x0.min(p.0);
        y0 = y0.min(p.1);
        x1 = x1.max(p.0);
        y1 = y1.max(p.1);
    }
    let span = reach * radius;
    let area = (x1 - x0 + 2.0 * span) * (y1 - y0 + 2.0 * span);
    // At most about 4000 grid points.
    let step = (radius / 3.0).max((area / 4000.0).sqrt()).max(1.0);
    let mut samples = Vec::new();
    let mut y = y0 - span;
    while y <= y1 + span {
        let mut x = x0 - span;
        while x <= x1 + span {
            let d = distance_to_path((x, y), path) / radius;
            if d <= reach {
                samples.push(((x, y), d));
            }
            x += step;
        }
        y += step;
    }
    samples
}

/// Mean and standard deviation per channel (working space) at `points`
/// shifted by `offset`.
fn point_statistics(
    image: &Rgb32FImage,
    points: &[(f64, f64)],
    offset: (f64, f64),
) -> ([f64; 3], [f64; 3]) {
    let mut sum = [0.0f64; 3];
    let mut sq = [0.0f64; 3];
    for p in points {
        let v = sample(image, p.0 + offset.0, p.1 + offset.1);
        for i in 0..3 {
            let e = encode(v[i]) as f64;
            sum[i] += e;
            sq[i] += e * e;
        }
    }
    let n = points.len().max(1) as f64;
    let mean = sum.map(|v| v / n);
    let std = std::array::from_fn(|i| (sq[i] / n - mean[i] * mean[i]).max(0.0).sqrt());
    (mean, std)
}

/// The best source offset (base pixels) for a brush stroke: scored like a
/// circle's source (`best_source`) on the stroke's own surroundings and
/// interior, among offsets that keep the copy clear of the stroke and inside
/// the photo.
fn best_stroke_offset(image: &Rgb32FImage, path: &[(f64, f64)], radius: f64) -> Option<(f64, f64)> {
    let (w, h) = (image.width() as f64, image.height() as f64);
    let samples = stroke_samples(path, radius, 1.7);
    let pick = |lo: f64, hi: f64| -> Vec<(f64, f64)> {
        samples
            .iter()
            .filter(|(_, d)| *d >= lo && *d <= hi)
            .map(|(p, _)| *p)
            .collect()
    };
    let (surround, interior) = (pick(1.1, 1.7), pick(0.0, 0.8));
    let (core, rim) = (pick(0.0, 0.5), pick(0.95, 1.1));
    if surround.is_empty() || interior.is_empty() {
        return None;
    }
    let reference = point_statistics(image, &surround, (0.0, 0.0));
    let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for p in path {
        x0 = x0.min(p.0);
        y0 = y0.min(p.1);
        x1 = x1.max(p.0);
        y1 = y1.max(p.1);
    }
    let margin = radius * 1.8;
    let allowed = |o: (f64, f64)| {
        x0 + o.0 - margin >= 0.0
            && y0 + o.1 - margin >= 0.0
            && x1 + o.0 + margin < w
            && y1 + o.1 + margin < h
            // The copied area must not overlap the stroke itself.
            && interior
                .iter()
                .chain(&rim)
                .all(|p| distance_to_path((p.0 + o.0, p.1 + o.1), path) > 1.15 * radius)
    };
    let score = |o: (f64, f64)| {
        let mut difference = 0.0;
        for p in &surround {
            let a = sample(image, p.0, p.1);
            let b = sample(image, p.0 + o.0, p.1 + o.1);
            for i in 0..3 {
                let d = (encode(a[i]) - encode(b[i])) as f64;
                difference += d * d;
            }
        }
        difference /= (3 * surround.len()) as f64;
        let inside = point_statistics(image, &interior, o);
        let core = point_statistics(image, &core, o);
        let rim = point_statistics(image, &rim, o);
        let content: f64 = (0..3)
            .map(|i| {
                let mean = inside.0[i] - reference.0[i];
                let spread = inside.1[i] - reference.1[i];
                let object = core.0[i] - rim.0[i];
                mean * mean + spread * spread + 4.0 * object * object
            })
            .sum::<f64>()
            / 3.0;
        (difference + content) * (1.0 + 0.04 * o.0.hypot(o.1) / radius)
    };
    // Offsets of a few brush widths, and of the stroke's extent for strokes
    // that leave no room beside them.
    let extent = (x1 - x0).hypot(y1 - y0);
    let mut distances: Vec<f64> = [2.5, 3.2, 4.0, 5.0].map(|d| d * radius).to_vec();
    distances.extend([1.0, 1.3].map(|d| d * extent + 2.5 * radius));
    let mut best: Option<((f64, f64), f64)> = None;
    for distance in distances {
        for k in 0..32 {
            let phi = std::f64::consts::TAU * k as f64 / 32.0;
            let o = (distance * phi.cos(), distance * phi.sin());
            if !allowed(o) {
                continue;
            }
            let s = score(o);
            if best.is_none_or(|(_, b)| s < b) {
                best = Some((o, s));
            }
        }
    }
    let (mut offset, mut value) = best?;
    let mut step = radius * 0.5;
    while step >= 1.0 {
        let mut improved = false;
        for (dx, dy) in [(step, 0.0), (-step, 0.0), (0.0, step), (0.0, -step)] {
            let o = (offset.0 + dx, offset.1 + dy);
            if !allowed(o) {
                continue;
            }
            let s = score(o);
            if s < value {
                (offset, value) = (o, s);
                improved = true;
            }
        }
        if !improved {
            step /= 2.0;
        }
    }
    Some(offset)
}

/// The automatic source for a new brush stroke along mask-frame `points`
/// with `radius`: where its first point copies from, in mask-frame
/// coordinates (see `find_source`).
pub fn find_stroke_source(
    base: &DynamicImage,
    adjustments: &Value,
    points: &[StrokePoint],
    radius: f64,
) -> Option<(f64, f64)> {
    let first = points.first()?;
    let image = base.to_rgb32f();
    let frame = mask_frame(adjustments, image.width(), image.height());
    let path: Vec<(f64, f64)> = points
        .iter()
        .map(|p| to_base(adjustments, frame, p.x, p.y))
        .collect();
    let ex = to_base(adjustments, frame, first.x + 1.0, first.y);
    let base_radius = radius * (ex.0 - path[0].0).hypot(ex.1 - path[0].1);
    let offset = best_stroke_offset(&image, &path, base_radius.max(1.0))?;
    to_mask_point(adjustments, frame, (first.x, first.y), offset)
}

/// Running box blur of a single-channel image (rows, then columns),
/// `radius` pixels each side; three passes approximate a Gaussian.
fn box_blur(values: &[f32], width: usize, height: usize, radius: usize) -> Vec<f32> {
    let blur_rows = |src: &[f32], w: usize, h: usize| -> Vec<f32> {
        let mut out = vec![0.0f32; w * h];
        out.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
            let line = &src[y * w..(y + 1) * w];
            let mut sum = 0.0f32;
            let at = |i: isize| line[i.clamp(0, w as isize - 1) as usize];
            for i in -(radius as isize)..=(radius as isize) {
                sum += at(i);
            }
            let n = (2 * radius + 1) as f32;
            for (x, value) in row.iter_mut().enumerate() {
                *value = sum / n;
                sum += at(x as isize + radius as isize + 1) - at(x as isize - radius as isize);
            }
        });
        out
    };
    let transpose = |src: &[f32], w: usize, h: usize| -> Vec<f32> {
        let mut out = vec![0.0f32; w * h];
        out.par_chunks_mut(h).enumerate().for_each(|(x, col)| {
            for (y, value) in col.iter_mut().enumerate() {
                *value = src[y * w + x];
            }
        });
        out
    };
    let mut v = values.to_vec();
    for _ in 0..3 {
        v = blur_rows(&v, width, height);
    }
    let mut t = transpose(&v, width, height);
    for _ in 0..3 {
        t = blur_rows(&t, height, width);
    }
    transpose(&t, height, width)
}

/// Band-pass of the photo's luminance for Visualize Spots (Lightroom): the
/// difference between a fine and a coarse blur of the square-root
/// luminance, where dust and small blemishes stand out from smooth areas,
/// reduced by a maximum filter to at most `max_edge` pixels on the long
/// side (so a few-pixel spot survives), with its median for scaling.
pub fn spot_band_pass(
    image: &DynamicImage,
    max_edge: u32,
) -> (image::ImageBuffer<image::Luma<f32>, Vec<f32>>, f32) {
    let rgb = image.to_rgb32f();
    let (w, h) = (rgb.width() as usize, rgb.height() as usize);
    let luma: Vec<f32> = rgb
        .pixels()
        .map(|p| {
            (0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2])
                .max(0.0)
                .sqrt()
        })
        .collect();
    // Scales relative to the photo, so a dust spot of a given share of the
    // frame looks alike on every sensor.
    let long = w.max(h) as f32;
    let fine = ((long / 6000.0).round() as usize).max(1);
    let coarse = ((long / 600.0).round() as usize).max(fine + 2);
    let a = box_blur(&luma, w, h, fine);
    let b = box_blur(&luma, w, h, coarse);
    let block = (w.max(h) as u32).div_ceil(max_edge.max(64)) as usize;
    let (ow, oh) = (w.div_ceil(block), h.div_ceil(block));
    let pooled: Vec<f32> = (0..oh)
        .into_par_iter()
        .flat_map_iter(|oy| {
            let (a, b) = (&a, &b);
            (0..ow).map(move |ox| {
                let mut m = 0.0f32;
                for y in oy * block..((oy + 1) * block).min(h) {
                    for x in ox * block..((ox + 1) * block).min(w) {
                        m = m.max((a[y * w + x] - b[y * w + x]).abs());
                    }
                }
                m
            })
        })
        .collect();
    let mut sorted = pooled.clone();
    sorted.sort_by(f32::total_cmp);
    let median = sorted[sorted.len() / 2].max(1e-6);
    (
        image::ImageBuffer::from_raw(ow as u32, oh as u32, pooled).expect("pooled size"),
        median,
    )
}

/// The Visualize Spots view at `sensitivity` (0–100, more shows more):
/// white where the band-pass exceeds a multiple of its median.
pub fn visualize(
    band: &image::ImageBuffer<image::Luma<f32>, Vec<f32>>,
    median: f32,
    sensitivity: f64,
) -> image::GrayImage {
    let threshold = median * (6.0 - 4.5 * (sensitivity.clamp(0.0, 100.0) / 100.0) as f32);
    image::GrayImage::from_fn(band.width(), band.height(), |x, y| {
        let v = band.get_pixel(x, y)[0];
        let t = ((v - threshold) / threshold).clamp(0.0, 1.0);
        image::Luma([(t.sqrt() * 255.0) as u8])
    })
}

pub type BandCache = std::sync::Mutex<
    Option<(
        u64,
        std::sync::Arc<(image::ImageBuffer<image::Luma<f32>, Vec<f32>>, f32)>,
    )>,
>;

pub fn band_cache() -> &'static BandCache {
    static CACHE: std::sync::OnceLock<BandCache> = std::sync::OnceLock::new();
    CACHE.get_or_init(Default::default)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A smooth gradient with a dark dust spot and fine grain.
    fn photo_with_dust() -> Rgb32FImage {
        Rgb32FImage::from_fn(200, 120, |x, y| {
            let grain = (((x * 7919 + y * 104_729) % 97) as f32 / 97.0 - 0.5) * 0.01;
            let base = 0.1 + 0.3 * x as f32 / 200.0 + 0.1 * y as f32 / 120.0 + grain;
            let d = ((x as f32 - 100.0).powi(2) + (y as f32 - 60.0).powi(2)).sqrt();
            let dust = if d < 6.0 { 0.4 } else { 1.0 };
            image::Rgb([base * dust, base * 0.9 * dust, base * 0.8 * dust])
        })
    }

    fn mean_abs_deviation_from_clean(image: &Rgb32FImage) -> f32 {
        let clean = |x: u32, y: u32| 0.1 + 0.3 * x as f32 / 200.0 + 0.1 * y as f32 / 120.0;
        let mut sum = 0.0;
        let mut n = 0;
        for y in 50..70 {
            for x in 90..110 {
                sum += (image.get_pixel(x, y)[0] - clean(x, y)).abs();
                n += 1;
            }
        }
        sum / n as f32
    }

    #[test]
    fn spots_heal_a_reduced_copy_at_their_place() {
        let full = DynamicImage::ImageRgb32F(photo_with_dust());
        let half = full.resize_exact(100, 60, image::imageops::FilterType::Triangle);
        let adjustments = serde_json::json!({"rotation": 0.0, "spots": [
            {"x": 100.0, "y": 60.0, "radius": 10.0, "sourceX": 60.0, "sourceY": 60.0,
             "points": []}
        ]});
        let dust = |image: &Rgb32FImage| {
            // The darkest pixel around the dust's place at half size.
            (45..55)
                .flat_map(|x| (25..35).map(move |y| (x, y)))
                .map(|(x, y)| image.get_pixel(x, y)[0])
                .fold(f32::MAX, f32::min)
        };
        assert!(dust(&half.to_rgb32f()) < 0.12);
        let scaled = scaled_spots_recipe(&adjustments, 0.5);
        assert_eq!(scaled["spots"][0]["radius"], 5.0);
        assert_eq!(scaled["rotation"], 0.0);
        let healed = apply_spots(&half, &scaled).unwrap().to_rgb32f();
        assert!(dust(&healed) > 0.15, "{}", dust(&healed));
    }

    #[test]
    fn visualize_spots_shows_faint_dust_and_ignores_smooth_gradients() {
        // A faint dust spot (4 % darker, radius 8) on a smooth gradient with
        // mild grain: the band-pass marks the spot, not the gradient.
        let image = Rgb32FImage::from_fn(1200, 800, |x, y| {
            let grain = (((x * 7919 + y * 104_729) % 97) as f32 / 97.0 - 0.5) * 0.004;
            let base = 0.2 + 0.2 * x as f32 / 1200.0 + grain;
            let d = ((x as f32 - 600.0).powi(2) + (y as f32 - 400.0).powi(2)).sqrt();
            let v = if d < 8.0 { base * 0.96 } else { base };
            image::Rgb([v, v, v])
        });
        let (band, median) = spot_band_pass(&DynamicImage::ImageRgb32F(image), 600);
        assert_eq!((band.width(), band.height()), (600, 400));
        let view = visualize(&band, median, 50.0);
        let at_spot = (295..305)
            .flat_map(|x| (195..205).map(move |y| (x, y)))
            .map(|(x, y)| view.get_pixel(x, y)[0])
            .max()
            .unwrap();
        let elsewhere = view
            .enumerate_pixels()
            .filter(|(x, y, _)| (*x as i32 - 300).abs() > 40 || (*y as i32 - 200).abs() > 40)
            .filter(|(_, _, p)| p[0] > 128)
            .count();
        assert!(at_spot > 200, "spot {at_spot}");
        assert!(
            elsewhere < 600 * 400 / 200,
            "{elsewhere} bright pixels elsewhere"
        );
    }

    #[test]
    fn heal_removes_a_dust_spot_and_matches_the_gradient() {
        let mut image = photo_with_dust();
        let before = mean_abs_deviation_from_clean(&image);
        // Source 40 px to the left: darker part of the gradient, which a
        // plain clone would carry along.
        apply_spot(
            &mut image,
            (100.0, 60.0),
            10.0,
            (-40.0, 0.0),
            true,
            0.3,
            1.0,
        );
        let healed = mean_abs_deviation_from_clean(&image);
        let mut cloned = photo_with_dust();
        apply_spot(
            &mut cloned,
            (100.0, 60.0),
            10.0,
            (-40.0, 0.0),
            false,
            0.3,
            1.0,
        );
        let clone_error = mean_abs_deviation_from_clean(&cloned);
        assert!(before > 0.05, "{before}");
        assert!(healed < 0.01, "healed {healed}");
        assert!(
            clone_error > 3.0 * healed,
            "clone {clone_error} heal {healed}"
        );
        // Outside the circle nothing changes.
        assert_eq!(image.get_pixel(10, 10), photo_with_dust().get_pixel(10, 10));
    }

    #[test]
    fn spots_are_read_from_the_recipe_and_applied_through_the_geometry() {
        let base = DynamicImage::ImageRgb32F(photo_with_dust());
        let adjustments = serde_json::json!({"spots": [
            {"x": 100.0, "y": 60.0, "radius": 10.0, "sourceX": 60.0, "sourceY": 60.0},
            {"x": 20.0, "y": 20.0, "radius": 5.0, "sourceX": 40.0, "sourceY": 20.0, "visible": false}
        ]});
        assert_eq!(spots(&adjustments).len(), 1);
        let healed = apply_spots(&base, &adjustments).unwrap().to_rgb32f();
        assert!(mean_abs_deviation_from_clean(&healed) < 0.01);
        assert!(apply_spots(&base, &serde_json::json!({})).is_none());
        // Turned a quarter: the mask frame is 120 × 200, the dust spot at
        // (60, 100) there.
        let turned = serde_json::json!({"orientationSteps": 1, "spots": [
            {"x": 60.0, "y": 100.0, "radius": 10.0, "sourceX": 60.0, "sourceY": 140.0}
        ]});
        let healed = apply_spots(&base, &turned).unwrap().to_rgb32f();
        assert!(mean_abs_deviation_from_clean(&healed) < 0.01);
    }

    /// A smooth gradient with grain and a dark scratch along y = 60 from
    /// x = 40 to 160, 3 px wide.
    fn photo_with_scratch() -> Rgb32FImage {
        Rgb32FImage::from_fn(200, 120, |x, y| {
            let grain = (((x * 7919 + y * 104_729) % 97) as f32 / 97.0 - 0.5) * 0.01;
            let base = 0.1 + 0.3 * x as f32 / 200.0 + 0.1 * y as f32 / 120.0 + grain;
            let scratch = (40..=160).contains(&x) && (59..=61).contains(&y);
            let k = if scratch { 0.4 } else { 1.0 };
            image::Rgb([base * k, base * 0.9 * k, base * 0.8 * k])
        })
    }

    #[test]
    fn the_membrane_solver_reproduces_harmonic_functions_on_a_disc() {
        // x and r² cos 2φ are harmonic: given on the rim, the solver must
        // return them inside.
        let (w, h, r) = (161usize, 161usize, 70.0f64);
        let (cx, cy) = (80.0, 80.0);
        let exact = |x: f64, y: f64| -> [f32; 3] {
            let (dx, dy) = (x - cx, y - cy);
            [
                (dx / r) as f32,
                ((dx * dx - dy * dy) / (r * r)) as f32,
                0.25,
            ]
        };
        let free: Vec<bool> = (0..w * h)
            .map(|i| ((i % w) as f64 - cx).hypot((i / w) as f64 - cy) < r)
            .collect();
        let mut u: Vec<[f32; 3]> = (0..w * h)
            .map(|i| {
                if free[i] {
                    [0.0; 3]
                } else {
                    exact((i % w) as f64, (i / w) as f64)
                }
            })
            .collect();
        solve_laplace(&mut u, &free, w, h);
        let worst = (0..w * h)
            .filter(|&i| free[i])
            .map(|i| {
                let e = exact((i % w) as f64, (i / w) as f64);
                (0..3)
                    .map(|c| (u[i][c] - e[c]).abs())
                    .fold(0.0f32, f32::max)
            })
            .fold(0.0f32, f32::max);
        assert!(worst < 0.01, "worst error {worst}");
    }

    #[test]
    fn a_heal_stroke_removes_a_scratch_and_matches_the_gradient() {
        let path = [(40.0, 60.0), (100.0, 60.0), (160.0, 60.0)];
        let error = |image: &Rgb32FImage| {
            let clean = |x: u32, y: u32| 0.1 + 0.3 * x as f32 / 200.0 + 0.1 * y as f32 / 120.0;
            let mut sum = 0.0;
            let mut n = 0;
            for y in 56..65 {
                for x in 40..161 {
                    sum += (image.get_pixel(x, y)[0] - clean(x, y)).abs();
                    n += 1;
                }
            }
            sum / n as f32
        };
        let before = error(&photo_with_scratch());
        // Source 30 px lower: a brighter part of the gradient.
        let mut healed = photo_with_scratch();
        apply_stroke(&mut healed, &path, 6.0, (0.0, 30.0), true, 0.0, 1.0);
        let mut cloned = photo_with_scratch();
        apply_stroke(&mut cloned, &path, 6.0, (0.0, 30.0), false, 0.0, 1.0);
        let (healed_error, clone_error) = (error(&healed), error(&cloned));
        assert!(before > 0.02, "{before}");
        assert!(healed_error < 0.006, "healed {healed_error}");
        assert!(
            clone_error > 2.0 * healed_error,
            "clone {clone_error} heal {healed_error}"
        );
        // Pixels beyond the brush are untouched.
        assert_eq!(
            healed.get_pixel(100, 70),
            photo_with_scratch().get_pixel(100, 70)
        );
        assert_eq!(
            healed.get_pixel(30, 60),
            photo_with_scratch().get_pixel(30, 60)
        );
    }

    #[test]
    fn a_dot_shaped_stroke_heals_like_the_circle() {
        // A stroke of two coincident points is a disc: the membrane must
        // agree with the disc's Poisson integral.
        let mut circle = photo_with_dust();
        apply_spot(
            &mut circle,
            (100.0, 60.0),
            12.0,
            (-40.0, 0.0),
            true,
            0.0,
            1.0,
        );
        let mut stroke = photo_with_dust();
        apply_stroke(
            &mut stroke,
            &[(100.0, 60.0), (100.0, 60.0)],
            12.0,
            (-40.0, 0.0),
            true,
            0.0,
            1.0,
        );
        let mut worst = 0.0f32;
        for y in 48..73 {
            for x in 88..113 {
                // On the rim itself (distance exactly 12) the disc's
                // Poisson weights vanish and the circle copies unhealed.
                if (x as f64 - 100.0).hypot(y as f64 - 60.0) > 11.5 {
                    continue;
                }
                for c in 0..3 {
                    worst =
                        worst.max((circle.get_pixel(x, y)[c] - stroke.get_pixel(x, y)[c]).abs());
                }
            }
        }
        // The rim difference is smoothed differently (along the circle vs a
        // square neighbourhood): within grain amplitude.
        assert!(worst < 0.01, "worst {worst}");
    }

    #[test]
    fn strokes_are_read_from_the_recipe_and_applied_through_the_geometry() {
        let base = DynamicImage::ImageRgb32F(photo_with_scratch());
        let adjustments = serde_json::json!({"spots": [{
            "x": 40.0, "y": 60.0, "radius": 6.0, "sourceX": 40.0, "sourceY": 90.0,
            "points": [{"x": 40.0, "y": 60.0}, {"x": 160.0, "y": 60.0}]
        }]});
        let healed = apply_spots(&base, &adjustments).unwrap().to_rgb32f();
        let mut direct = photo_with_scratch();
        apply_stroke(
            &mut direct,
            &[(40.0, 60.0), (160.0, 60.0)],
            6.0,
            (0.0, 30.0),
            true,
            0.0,
            1.0,
        );
        assert_eq!(healed, direct);
        // Turned a quarter: the mask frame is 120 × 200 and the scratch runs
        // along x = 60 there.
        let turned = serde_json::json!({"orientationSteps": 1, "spots": [{
            "x": 60.0, "y": 40.0, "radius": 6.0, "sourceX": 30.0, "sourceY": 40.0,
            "points": [{"x": 60.0, "y": 40.0}, {"x": 60.0, "y": 160.0}]
        }]});
        let turned = apply_spots(&base, &turned).unwrap().to_rgb32f();
        let dark = (40..=160)
            .filter(|&x| turned.get_pixel(x, 60)[0] < 0.08)
            .count();
        assert_eq!(dark, 0, "scratch pixels left");
    }

    #[test]
    fn the_automatic_stroke_source_stays_clear_of_the_stroke() {
        // Bright top, dark bottom; a stroke in the bright half along its
        // length should copy from the bright half and not from itself.
        let image = Rgb32FImage::from_fn(400, 240, |_, y| {
            let v = if y < 120 { 0.6 } else { 0.05 };
            image::Rgb([v, v, v])
        });
        let base = DynamicImage::ImageRgb32F(image);
        let points = [
            StrokePoint { x: 100.0, y: 80.0 },
            StrokePoint { x: 300.0, y: 80.0 },
        ];
        let (sx, sy) = find_stroke_source(&base, &serde_json::json!({}), &points, 8.0).unwrap();
        let (ox, oy) = (sx - 100.0, sy - 80.0);
        // The copied stroke lies wholly in the bright half, clear of itself.
        assert!(
            sy + 1.7 * 8.0 < 120.0,
            "source {sx} {sy} reaches the dark half"
        );
        let path = [(100.0, 80.0), (300.0, 80.0)];
        for p in path {
            assert!(
                distance_to_path((p.0 + ox, p.1 + oy), &path) > 1.15 * 8.0,
                "{ox} {oy}"
            );
        }
    }

    #[test]
    fn automatic_source_avoids_the_spot_and_matches_the_surroundings() {
        // Left half bright, right half dark; a spot near the boundary on the
        // bright side should take its source from the bright side.
        let image = Rgb32FImage::from_fn(300, 200, |x, _| {
            let v = if x < 150 { 0.6 } else { 0.05 };
            image::Rgb([v, v, v])
        });
        let base = DynamicImage::ImageRgb32F(image);
        let (sx, sy) = find_source(&base, &serde_json::json!({}), 100.0, 100.0, 12.0).unwrap();
        let distance = (sx - 100.0).hypot(sy - 100.0);
        assert!(distance >= 2.2 * 12.0, "{sx} {sy}");
        assert!(sx + 1.7 * 12.0 < 150.0, "source {sx} reaches the dark side");
    }
}
