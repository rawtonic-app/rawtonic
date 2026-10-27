//! The protection brush (Capture One's blemish protection brush): areas the
//! user paints — freckles, moles, beauty marks — that the face retouch
//! leaves as they were. The retouch of every face is computed as before,
//! then the protected pixels are put back from the image before it, by the
//! brush's coverage, so a fully covered pixel is exactly its unretouched
//! value and the feathered edge blends into the retouched skin. Erase
//! strokes take coverage away again; strokes apply in the order painted.
//!
//! Strokes are kept in mask-frame pixels at full resolution, like spots
//! (`spot_removal`), and are photo-wide: one painted area protects every
//! face it touches.

use image::Rgb32FImage;
use rayon::prelude::*;
use serde::Deserialize;
use serde_json::Value;

use crate::spot_removal::{StrokePoint, coverage, to_base};

/// One stroke of the protection brush (`faceRetouch.protect[]`).
#[derive(Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ProtectStroke {
    /// The path the brush's centre followed (one point: a dab), mask-frame
    /// pixels.
    pub points: Vec<StrokePoint>,
    /// Brush radius, mask-frame pixels.
    pub radius: f64,
    /// Soft edge, 0–100 % of the radius.
    #[serde(default = "default_feather")]
    pub feather: f64,
    /// Takes protection away instead of adding it.
    #[serde(default)]
    pub erase: bool,
}

fn default_feather() -> f64 {
    50.0
}

/// A stroke placed on the base image.
struct Placed {
    path: Vec<(f64, f64)>,
    radius: f64,
    feather: f64,
    erase: bool,
}

/// The protected area on the base image: coverage 0–1 over the box
/// (`x0`, `y0`, `width` × `height`) that holds every protecting stroke,
/// with the unretouched pixels there.
pub(super) struct Protection {
    x0: u32,
    y0: u32,
    width: u32,
    height: u32,
    weight: Vec<f32>,
    kept: Vec<[f32; 3]>,
}

impl Protection {
    /// The protection `strokes` give the base `image` (before the retouch),
    /// `frame` its mask-frame size; none when nothing is protected.
    pub(super) fn new(
        image: &Rgb32FImage,
        strokes: &[ProtectStroke],
        adjustments: &Value,
        frame: (f64, f64),
    ) -> Option<Self> {
        let placed: Vec<Placed> = strokes
            .iter()
            .filter(|s| !s.points.is_empty() && s.radius > 0.0)
            .map(|s| {
                let path: Vec<(f64, f64)> = s
                    .points
                    .iter()
                    .map(|p| to_base(adjustments, frame, p.x, p.y))
                    .collect();
                // The radius as the geometry scales it at the first point.
                let first = s.points[0];
                let edge = to_base(adjustments, frame, first.x + s.radius, first.y);
                Placed {
                    radius: (edge.0 - path[0].0).hypot(edge.1 - path[0].1),
                    path,
                    feather: (s.feather / 100.0).clamp(0.0, 1.0),
                    erase: s.erase,
                }
            })
            .filter(|s| s.radius > 0.0)
            .collect();
        // Only painted strokes protect; erasing reaches no farther.
        let (w, h) = image.dimensions();
        let bounds = |s: &Placed| {
            s.path.iter().fold(
                (f64::MAX, f64::MAX, f64::MIN, f64::MIN),
                |(x0, y0, x1, y1), p| {
                    (
                        x0.min(p.0 - s.radius),
                        y0.min(p.1 - s.radius),
                        x1.max(p.0 + s.radius),
                        y1.max(p.1 + s.radius),
                    )
                },
            )
        };
        let (bx0, by0, bx1, by1) = placed
            .iter()
            .filter(|s| !s.erase)
            .map(bounds)
            .fold((f64::MAX, f64::MAX, f64::MIN, f64::MIN), |a, b| {
                (a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3))
            });
        if bx0 > bx1 || w == 0 || h == 0 {
            return None;
        }
        let x0 = bx0.floor().clamp(0.0, f64::from(w - 1)) as u32;
        let y0 = by0.floor().clamp(0.0, f64::from(h - 1)) as u32;
        let x1 = bx1.ceil().clamp(0.0, f64::from(w - 1)) as u32;
        let y1 = by1.ceil().clamp(0.0, f64::from(h - 1)) as u32;
        if bx1 < 0.0 || by1 < 0.0 || bx0 > f64::from(w - 1) || by0 > f64::from(h - 1) {
            return None;
        }
        let (width, height) = (x1 - x0 + 1, y1 - y0 + 1);
        let mut weight = vec![0.0f32; width as usize * height as usize];
        for stroke in &placed {
            let (sx0, sy0, sx1, sy1) = bounds(stroke);
            let cx0 = (sx0.floor() as i64).max(i64::from(x0));
            let cy0 = (sy0.floor() as i64).max(i64::from(y0));
            let cx1 = (sx1.ceil() as i64).min(i64::from(x1));
            let cy1 = (sy1.ceil() as i64).min(i64::from(y1));
            if cx0 > cx1 || cy0 > cy1 {
                continue;
            }
            let row = width as usize;
            // Segments (a dab: one of length zero) with their rows' reach.
            let segments: Vec<((f64, f64), (f64, f64))> = if stroke.path.len() == 1 {
                vec![(stroke.path[0], stroke.path[0])]
            } else {
                stroke.path.windows(2).map(|p| (p[0], p[1])).collect()
            };
            weight
                .par_chunks_mut(row)
                .enumerate()
                .skip((cy0 - i64::from(y0)) as usize)
                .take((cy1 - cy0 + 1) as usize)
                .for_each(|(j, line)| {
                    let y = f64::from(y0) + j as f64;
                    let near: Vec<_> = segments
                        .iter()
                        .filter(|(a, b)| {
                            a.1.min(b.1) - stroke.radius <= y && y <= a.1.max(b.1) + stroke.radius
                        })
                        .collect();
                    if near.is_empty() {
                        return;
                    }
                    for x in cx0..=cx1 {
                        let p = (x as f64, y);
                        let distance = near
                            .iter()
                            .map(|(a, b)| distance_to_segment(p, *a, *b))
                            .fold(f64::INFINITY, f64::min);
                        let c = coverage(distance, stroke.radius, stroke.feather);
                        if c <= 0.0 {
                            continue;
                        }
                        let w = &mut line[(x - i64::from(x0)) as usize];
                        *w = if stroke.erase {
                            *w * (1.0 - c)
                        } else {
                            w.max(c)
                        };
                    }
                });
        }
        if weight.iter().all(|&w| w <= 0.0) {
            return None;
        }
        let kept = (0..height)
            .flat_map(|j| (0..width).map(move |i| (i, j)))
            .map(|(i, j)| image.get_pixel(x0 + i, y0 + j).0)
            .collect();
        Some(Self {
            x0,
            y0,
            width,
            height,
            weight,
            kept,
        })
    }

    /// Puts the protected pixels of the retouched `image` back by their
    /// coverage: exactly as they were where it is full.
    pub(super) fn restore(&self, image: &mut Rgb32FImage) {
        for j in 0..self.height {
            for i in 0..self.width {
                let k = (j * self.width + i) as usize;
                let w = self.weight[k];
                if w <= 0.0 {
                    continue;
                }
                let kept = self.kept[k];
                let p = image.get_pixel_mut(self.x0 + i, self.y0 + j);
                for c in 0..3 {
                    p[c] = kept[c] * w + p[c] * (1.0 - w);
                }
            }
        }
    }
}

/// Distance from `p` to the segment `a`–`b`.
fn distance_to_segment(p: (f64, f64), a: (f64, f64), b: (f64, f64)) -> f64 {
    let (vx, vy) = (b.0 - a.0, b.1 - a.1);
    let length2 = vx * vx + vy * vy;
    let t = if length2 > 0.0 {
        (((p.0 - a.0) * vx + (p.1 - a.1) * vy) / length2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    (p.0 - a.0 - t * vx).hypot(p.1 - a.1 - t * vy)
}
