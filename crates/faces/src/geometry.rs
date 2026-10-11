// SPDX-License-Identifier: AGPL-3.0-or-later
//! Rotated square crops, soft polygons and a guided filter.
//!
//! Coordinates are continuous: pixel (i, j) covers [i, i + 1] × [j, j + 1],
//! so its centre is (i + 0.5, j + 0.5). The networks' landmarks use the same
//! convention in crop pixels.

use image::RgbImage;
use rayon::prelude::*;

/// Maps crop coordinates (u, v) to photo coordinates:
/// x = a·u + b·v + c, y = d·u + e·v + f.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Affine {
    pub a: f32,
    pub b: f32,
    pub c: f32,
    pub d: f32,
    pub e: f32,
    pub f: f32,
}

impl Affine {
    /// A square of `size` photo pixels centred on `center` whose x axis
    /// points along `angle` (radians, photo coordinates, y down), seen as an
    /// `out` × `out` crop.
    pub fn crop(center: [f32; 2], size: f32, angle: f32, out: u32) -> Self {
        let k = size / out as f32;
        let (s, c) = angle.sin_cos();
        let (a, b, d, e) = (k * c, -k * s, k * s, k * c);
        let half = out as f32 / 2.0;
        Self {
            a,
            b,
            c: center[0] - (a + b) * half,
            d,
            e,
            f: center[1] - (d + e) * half,
        }
    }

    pub fn apply(&self, u: f32, v: f32) -> [f32; 2] {
        [
            self.a * u + self.b * v + self.c,
            self.d * u + self.e * v + self.f,
        ]
    }

    pub fn invert(&self) -> Self {
        let det = self.a * self.e - self.b * self.d;
        let (a, b, d, e) = (self.e / det, -self.b / det, -self.d / det, self.a / det);
        Self {
            a,
            b,
            c: -(a * self.c + b * self.f),
            d,
            e,
            f: -(d * self.c + e * self.f),
        }
    }

    /// Photo pixels per crop pixel.
    pub fn scale(&self) -> f32 {
        (self.a * self.e - self.b * self.d).abs().sqrt()
    }
}

/// The crop `to_photo` describes, `out` × `out` RGB values 0..255 sampled
/// bilinearly; black outside the photo.
pub fn sample_crop(image: &RgbImage, to_photo: &Affine, out: u32) -> Vec<[f32; 3]> {
    let (w, h) = image.dimensions();
    let raw = image.as_raw();
    let fetch = |x: i64, y: i64| -> [f32; 3] {
        if x < 0 || y < 0 || x >= w as i64 || y >= h as i64 {
            return [0.0; 3];
        }
        let i = (y as usize * w as usize + x as usize) * 3;
        [raw[i] as f32, raw[i + 1] as f32, raw[i + 2] as f32]
    };
    // Downscaling crops average a few samples per pixel so small crops of
    // large photos do not alias.
    let n = (to_photo.scale() / 1.5).ceil().clamp(1.0, 4.0) as usize;
    (0..out * out)
        .into_par_iter()
        .map(|i| {
            let (u, v) = ((i % out) as f32, (i / out) as f32);
            let mut acc = [0.0f32; 3];
            for sy in 0..n {
                for sx in 0..n {
                    let su = u + (sx as f32 + 0.5) / n as f32;
                    let sv = v + (sy as f32 + 0.5) / n as f32;
                    let [x, y] = to_photo.apply(su, sv);
                    let (x, y) = (x - 0.5, y - 0.5);
                    let (x0, y0) = (x.floor(), y.floor());
                    let (fx, fy) = (x - x0, y - y0);
                    let (x0, y0) = (x0 as i64, y0 as i64);
                    let p00 = fetch(x0, y0);
                    let p10 = fetch(x0 + 1, y0);
                    let p01 = fetch(x0, y0 + 1);
                    let p11 = fetch(x0 + 1, y0 + 1);
                    for c in 0..3 {
                        let top = p00[c] + (p10[c] - p00[c]) * fx;
                        let bottom = p01[c] + (p11[c] - p01[c]) * fx;
                        acc[c] += top + (bottom - top) * fy;
                    }
                }
            }
            let k = 1.0 / (n * n) as f32;
            [acc[0] * k, acc[1] * k, acc[2] * k]
        })
        .collect()
}

/// A closed polygon with its bounding box, for soft coverage tests.
#[derive(Clone, Debug)]
pub struct Polygon {
    pub points: Vec<[f32; 2]>,
    pub min: [f32; 2],
    pub max: [f32; 2],
}

impl Polygon {
    pub fn new(points: Vec<[f32; 2]>) -> Self {
        let mut min = [f32::INFINITY; 2];
        let mut max = [f32::NEG_INFINITY; 2];
        for p in &points {
            for k in 0..2 {
                min[k] = min[k].min(p[k]);
                max[k] = max[k].max(p[k]);
            }
        }
        Self { points, min, max }
    }

    /// Signed distance to the outline: negative inside, positive outside.
    pub fn signed_distance(&self, x: f32, y: f32) -> f32 {
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
        let d = best.sqrt();
        if inside { -d } else { d }
    }

    /// Coverage 0..1: 1 inside, 0 outside, a linear ramp `feather` pixels
    /// wide centred on the outline moved outwards by `grow`.
    pub fn coverage(&self, x: f32, y: f32, grow: f32, feather: f32) -> f32 {
        let reach = grow + feather;
        if x < self.min[0] - reach
            || y < self.min[1] - reach
            || x > self.max[0] + reach
            || y > self.max[1] + reach
        {
            return 0.0;
        }
        let d = self.signed_distance(x, y) - grow;
        if feather <= 0.0 {
            return if d <= 0.0 { 1.0 } else { 0.0 };
        }
        (0.5 - d / feather).clamp(0.0, 1.0)
    }

    pub fn centroid(&self) -> [f32; 2] {
        let n = self.points.len().max(1) as f32;
        let (sx, sy) = self
            .points
            .iter()
            .fold((0.0, 0.0), |(sx, sy), p| (sx + p[0], sy + p[1]));
        [sx / n, sy / n]
    }
}

/// Box mean of radius `r` (a (2r + 1)² window, clipped at the edges).
fn box_mean(src: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    let mut rows = vec![0.0f32; w * h];
    rows.par_chunks_mut(w)
        .zip(src.par_chunks(w))
        .for_each(|(out, row)| {
            let mut prefix = vec![0.0f64; w + 1];
            for x in 0..w {
                prefix[x + 1] = prefix[x] + row[x] as f64;
            }
            for (x, value) in out.iter_mut().enumerate() {
                let lo = x.saturating_sub(r);
                let hi = (x + r + 1).min(w);
                *value = ((prefix[hi] - prefix[lo]) / (hi - lo) as f64) as f32;
            }
        });
    let mut out = vec![0.0f32; w * h];
    let columns: Vec<Vec<f32>> = (0..w)
        .into_par_iter()
        .map(|x| {
            let mut prefix = vec![0.0f64; h + 1];
            for y in 0..h {
                prefix[y + 1] = prefix[y] + rows[y * w + x] as f64;
            }
            (0..h)
                .map(|y| {
                    let lo = y.saturating_sub(r);
                    let hi = (y + r + 1).min(h);
                    ((prefix[hi] - prefix[lo]) / (hi - lo) as f64) as f32
                })
                .collect()
        })
        .collect();
    for (x, column) in columns.iter().enumerate() {
        for (y, v) in column.iter().enumerate() {
            out[y * w + x] = *v;
        }
    }
    out
}

/// He et al.'s guided filter: `p` smoothed so its edges follow `guide`
/// (both `w` × `h`, values about 0..1).
pub fn guided_filter(guide: &[f32], p: &[f32], w: usize, h: usize, r: usize, eps: f32) -> Vec<f32> {
    let ii: Vec<f32> = guide.par_iter().map(|v| v * v).collect();
    let ip: Vec<f32> = guide.par_iter().zip(p).map(|(i, p)| i * p).collect();
    let mean_i = box_mean(guide, w, h, r);
    let mean_p = box_mean(p, w, h, r);
    let corr_i = box_mean(&ii, w, h, r);
    let corr_ip = box_mean(&ip, w, h, r);
    let (a, b): (Vec<f32>, Vec<f32>) = (0..w * h)
        .into_par_iter()
        .map(|k| {
            let var = corr_i[k] - mean_i[k] * mean_i[k];
            let cov = corr_ip[k] - mean_i[k] * mean_p[k];
            let a = cov / (var + eps);
            (a, mean_p[k] - a * mean_i[k])
        })
        .unzip();
    let mean_a = box_mean(&a, w, h, r);
    let mean_b = box_mean(&b, w, h, r);
    (0..w * h)
        .into_par_iter()
        .map(|k| mean_a[k] * guide[k] + mean_b[k])
        .collect()
}

pub fn smoothstep(lo: f32, hi: f32, x: f32) -> f32 {
    let t = ((x - lo) / (hi - lo)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Display luma (Rec. 709 weights on the encoded values) in 0..1.
pub fn luma(p: [u8; 3]) -> f32 {
    (0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32) / 255.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crop_maps_its_corners_and_inverts() {
        let t = Affine::crop([100.0, 50.0], 40.0, std::f32::consts::FRAC_PI_2, 256);
        let centre = t.apply(128.0, 128.0);
        assert!((centre[0] - 100.0).abs() < 1e-3 && (centre[1] - 50.0).abs() < 1e-3);
        // The crop's x axis points down the photo at 90°.
        let right = t.apply(256.0, 128.0);
        assert!((right[0] - 100.0).abs() < 1e-3 && (right[1] - 70.0).abs() < 1e-3);
        let back = t.invert().apply(right[0], right[1]);
        assert!((back[0] - 256.0).abs() < 1e-2 && (back[1] - 128.0).abs() < 1e-2);
        assert!((t.scale() - 40.0 / 256.0).abs() < 1e-5);
    }

    #[test]
    fn crop_samples_the_photo() {
        let image = RgbImage::from_fn(64, 64, |x, _| image::Rgb([(x * 4) as u8, 0, 0]));
        let t = Affine::crop([32.0, 32.0], 64.0, 0.0, 64);
        let crop = sample_crop(&image, &t, 64);
        assert!((crop[10][0] - 40.0).abs() < 1e-3);
        // Outside the photo is black.
        let t = Affine::crop([0.0, 0.0], 64.0, 0.0, 64);
        assert_eq!(sample_crop(&image, &t, 64)[0], [0.0; 3]);
    }

    #[test]
    fn polygon_coverage_is_soft_at_the_outline() {
        let square = Polygon::new(vec![[10.0, 10.0], [30.0, 10.0], [30.0, 30.0], [10.0, 30.0]]);
        assert_eq!(square.coverage(20.0, 20.0, 0.0, 2.0), 1.0);
        assert_eq!(square.coverage(40.0, 20.0, 0.0, 2.0), 0.0);
        assert!((square.coverage(30.0, 20.0, 0.0, 2.0) - 0.5).abs() < 1e-6);
        assert!((square.coverage(32.0, 20.0, 2.0, 2.0) - 0.5).abs() < 1e-6);
        assert!(square.signed_distance(20.0, 20.0) < -9.9);
    }

    #[test]
    fn guided_filter_keeps_an_edge_of_the_guide() {
        let (w, h) = (40, 20);
        let guide: Vec<f32> = (0..w * h)
            .map(|k| if k % w < 20 { 0.1 } else { 0.9 })
            .collect();
        // A blurry mask whose true edge is the guide's.
        let p: Vec<f32> = (0..w * h)
            .map(|k| ((k % w) as f32 - 14.0).clamp(0.0, 12.0) / 12.0)
            .collect();
        let out = guided_filter(&guide, &p, w, h, 4, 1e-4);
        let row = &out[10 * w..11 * w];
        assert!(row[17] < 0.35 && row[22] > 0.65, "{row:?}");
    }
}
