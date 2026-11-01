// SPDX-License-Identifier: AGPL-3.0-or-later
//! Eye focus per face for culling: sharp, soft or uncertain, from how wide
//! the strongest edges of the sharper eye are once that eye is brought to a
//! fixed size. A suggestion only; the photographer's own call per face wins
//! (catalog `face_eyes.focus_override`).
//!
//! The measure is FOC's frozen `sig_n128` (notes:
//! benchmark-results/foc-eye-focus-2026-11-01/REPORT.md,
//! scripts/reference/foc/features_foc.py): each eye's region (its landmark
//! contour plus 0.18 × the eye distance) is resampled so the eyes are 128
//! px apart (area average down, Lanczos up), and the Gaussian sigma of its
//! strongest 8 % edges is read by re-blurring (Zhuo & Sim 2011: the
//! gradient ratio R = |∇I| / |∇(I * G(1))| of a blurred step gives
//! sigma = 1 / √(R² − 1)), contrast-invariant. The sharper eye counts (the
//! near eye in focus is fine). The threshold and its uncertain band are
//! the median of FOC's four leave-one-shoot-out folds (fitted on the other
//! shoots only), frozen before the held-out check (benchmark-results/
//! foc-eye-focus-2026-11-01/, section "FOCB held-out + app").
//!
//! Below an eye distance of 48 px every eye reads soft, which in a group
//! photograph is noise: no call there, as Lightroom does. The pixels must be
//! full resolution around the eyes (a small preview upsamples every eye
//! and reads soft).

use image::RgbImage;
use serde::{Deserialize, Serialize};

use crate::landmarks::{LEFT_EYE, RIGHT_EYE};

/// Bump when the measure or a threshold changes: stored readings of an
/// older version are read again (with `eyes::VERSION`, one pass).
pub const VERSION: u32 = 1;

/// Eye distance the eye regions are brought to, px.
pub const EYE_DISTANCE: f32 = 128.0;
/// Below this eye distance (px of the measured image) no call is made.
pub const MIN_EYE_DISTANCE: f32 = 48.0;
/// Edge sigma at `EYE_DISTANCE` (px): at or below `SHARP` sharp, at or
/// above `SOFT` soft, between uncertain. Threshold 1.585 px with a band of
/// ±0.039 in log10 (folds 1.370, 1.585, 1.585, 1.637; bands 0.026, 0.040,
/// 0.038, 0.066).
pub const SHARP: f32 = 1.449;
pub const SOFT: f32 = 1.734;

/// Padding of an eye's region around its contour, in eye distances.
const PAD: f32 = 0.18;
/// Share of the region's pixels taken as its strongest edges.
const TOP: f32 = 0.08;
/// Fewest edge pixels for a reading.
const MIN_POINTS: usize = 12;
/// Sigma of the re-blur, px.
const S0: f32 = 1.0;
/// Light pre-smoothing so single-pixel noise or JPEG ringing does not read
/// as a perfect edge, px.
const PRE: f32 = 0.5;
/// What an eye with no edge the re-blur changes reads (no detail at all).
const NO_DETAIL: f32 = 50.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FocusState {
    Sharp,
    /// Soft or missed: no measure (nor the labellers) tells them apart.
    Soft,
    /// No call: no landmarks, eyes hidden, a face too small, or a score
    /// too near the threshold.
    Uncertain,
}

impl FocusState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sharp => "sharp",
            Self::Soft => "soft",
            Self::Uncertain => "uncertain",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "sharp" => Self::Sharp,
            "soft" => Self::Soft,
            "uncertain" => Self::Uncertain,
            _ => return None,
        })
    }
}

/// One face's eye focus and the measurement behind the call.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FocusReading {
    pub state: FocusState,
    /// Edge sigma of the sharper eye at `EYE_DISTANCE`, px; `None` when not
    /// measured.
    pub sigma: Option<f32>,
    /// Eye distance in the measured image, px; `None` without landmarks.
    pub eye_distance: Option<f32>,
}

impl FocusReading {
    pub const NONE: Self = Self {
        state: FocusState::Uncertain,
        sigma: None,
        eye_distance: None,
    };
}

/// The call from a measurement.
pub fn classify(sigma: Option<f32>, eye_distance: f32) -> FocusState {
    match sigma {
        _ if eye_distance < MIN_EYE_DISTANCE => FocusState::Uncertain,
        Some(s) if s <= SHARP => FocusState::Sharp,
        Some(s) if s >= SOFT => FocusState::Soft,
        _ => FocusState::Uncertain,
    }
}

fn mean(points: impl Iterator<Item = [f32; 2]>) -> [f32; 2] {
    let (mut x, mut y, mut n) = (0.0, 0.0, 0.0);
    for [px, py] in points {
        x += px;
        y += py;
        n += 1.0;
    }
    [x / n, y / n]
}

/// Distance between the eyes' centres (mean of each eye's two corners).
pub fn eye_distance(landmarks: &[[f32; 2]]) -> f32 {
    let a = mean([33, 133].iter().map(|&i| landmarks[i]));
    let b = mean([263, 362].iter().map(|&i| landmarks[i]));
    (a[0] - b[0]).hypot(a[1] - b[1])
}

/// Reads one face's eye focus in `image` (the photograph as displayed,
/// sRGB 8-bit, at full resolution around the eyes) from its 478
/// `landmarks` given in pixels of an image `scale` times smaller (the
/// image the faces were found in: `scale` = this image's size over that
/// one's).
pub fn read(image: &RgbImage, landmarks: &[[f32; 2]], scale: f32) -> FocusReading {
    let points: Vec<[f32; 2]> = landmarks
        .iter()
        .map(|&[x, y]| [x * scale, y * scale])
        .collect();
    let distance = eye_distance(&points).max(1.0);
    if distance < MIN_EYE_DISTANCE {
        return FocusReading {
            state: FocusState::Uncertain,
            sigma: None,
            eye_distance: Some(distance),
        };
    }
    let sigma = [RIGHT_EYE, LEFT_EYE]
        .iter()
        .filter_map(|contour| eye_sigma(image, &points, contour, distance))
        .reduce(f32::min);
    FocusReading {
        state: classify(sigma, distance),
        sigma,
        eye_distance: Some(distance),
    }
}

/// One eye's edge sigma at `EYE_DISTANCE`, or `None` when its region is
/// too small or has too few edges.
fn eye_sigma(
    image: &RgbImage,
    points: &[[f32; 2]],
    contour: &[usize],
    distance: f32,
) -> Option<f32> {
    let eye: Vec<[f32; 2]> = contour.iter().map(|&i| points[i]).collect();
    let c = mean(eye.iter().copied());
    let span = |k: usize| {
        let (lo, hi) = eye.iter().fold((f32::MAX, f32::MIN), |(lo, hi), p| {
            (lo.min(p[k]), hi.max(p[k]))
        });
        hi - lo
    };
    let hx = (span(0) / 2.0).max(0.15 * distance) + PAD * distance;
    let hy = (span(1) / 2.0).max(0.08 * distance) + PAD * distance;
    let (w, h) = (image.width() as f32, image.height() as f32);
    // As the reference crops: truncated bounds, the far side inclusive.
    let x0 = (c[0] - hx).max(0.0) as usize;
    let x1 = (c[0] + hx + 1.0).min(w).max(0.0) as usize;
    let y0 = (c[1] - hy).max(0.0) as usize;
    let y1 = (c[1] + hy + 1.0).min(h).max(0.0) as usize;
    if x1 < x0 + 6 || y1 < y0 + 6 {
        return None;
    }
    let crop = Plane::luma(image, x0, y0, x1 - x0, y1 - y0);
    let s = EYE_DISTANCE / distance;
    let width = ((crop.w as f32 * s).round() as usize).max(8);
    let height = ((crop.h as f32 * s).round() as usize).max(8);
    let resized = if s < 1.0 {
        crop.resize_area(width, height)
    } else if width == crop.w && height == crop.h {
        crop
    } else {
        crop.resize_lanczos4(width, height)
    };
    reblur_sigma(&resized)
}

/// A single-channel float image, row-major.
#[derive(Clone, Debug)]
pub struct Plane {
    pub w: usize,
    pub h: usize,
    pub px: Vec<f32>,
}

impl Plane {
    pub fn new(w: usize, h: usize, px: Vec<f32>) -> Self {
        assert_eq!(px.len(), w * h);
        Self { w, h, px }
    }

    /// The region's luma 0..1 from sRGB-encoded values (ITU-R 601 weights
    /// rounded to 8 bits, as the reference's greyscale conversion).
    fn luma(image: &RgbImage, x0: usize, y0: usize, w: usize, h: usize) -> Self {
        let mut px = Vec::with_capacity(w * h);
        for y in y0..y0 + h {
            for x in x0..x0 + w {
                let [r, g, b] = image.get_pixel(x as u32, y as u32).0;
                let l =
                    (u32::from(r) * 19595 + u32::from(g) * 38470 + u32::from(b) * 7471 + 0x8000)
                        >> 16;
                px.push(l as f32 / 255.0);
            }
        }
        Self::new(w, h, px)
    }

    fn at(&self, x: usize, y: usize) -> f32 {
        self.px[y * self.w + x]
    }

    /// Area-average downsampling (each output pixel the mean of the input
    /// it covers, partial pixels weighted by their share).
    fn resize_area(&self, w: usize, h: usize) -> Self {
        let xs = area_weights(self.w, w);
        let ys = area_weights(self.h, h);
        let mut rows = vec![0.0f32; self.h * w];
        for y in 0..self.h {
            for (dx, taps) in xs.iter().enumerate() {
                rows[y * w + dx] = taps.iter().map(|&(sx, k)| k * self.at(sx, y)).sum();
            }
        }
        let mut px = vec![0.0f32; w * h];
        for (dy, taps) in ys.iter().enumerate() {
            for dx in 0..w {
                px[dy * w + dx] = taps.iter().map(|&(sy, k)| k * rows[sy * w + dx]).sum();
            }
        }
        Self::new(w, h, px)
    }

    /// Lanczos (a = 4) upsampling, pixel centres aligned, edges repeated.
    fn resize_lanczos4(&self, w: usize, h: usize) -> Self {
        let xs = lanczos_weights(self.w, w);
        let ys = lanczos_weights(self.h, h);
        let mut rows = vec![0.0f32; self.h * w];
        for y in 0..self.h {
            for (dx, taps) in xs.iter().enumerate() {
                rows[y * w + dx] = taps.iter().map(|&(sx, k)| k * self.at(sx, y)).sum();
            }
        }
        let mut px = vec![0.0f32; w * h];
        for (dy, taps) in ys.iter().enumerate() {
            for dx in 0..w {
                px[dy * w + dx] = taps.iter().map(|&(sy, k)| k * rows[sy * w + dx]).sum();
            }
        }
        Self::new(w, h, px)
    }

    /// Separable Gaussian blur, kernel radius 4 sigma, edges mirrored
    /// without repeating the edge pixel.
    pub fn gaussian(&self, sigma: f32) -> Self {
        let n = ((sigma * 8.0 + 1.0).round() as usize) | 1;
        let r = (n / 2) as isize;
        let mut k: Vec<f32> = (-r..=r)
            .map(|i| (-((i * i) as f32) / (2.0 * sigma * sigma)).exp())
            .collect();
        let sum: f32 = k.iter().sum();
        k.iter_mut().for_each(|v| *v /= sum);
        let mut rows = vec![0.0f32; self.px.len()];
        for y in 0..self.h {
            for x in 0..self.w {
                rows[y * self.w + x] = k
                    .iter()
                    .enumerate()
                    .map(|(j, kv)| kv * self.at(mirror(x as isize + j as isize - r, self.w), y))
                    .sum();
            }
        }
        let mut px = vec![0.0f32; self.px.len()];
        for y in 0..self.h {
            for x in 0..self.w {
                px[y * self.w + x] = k
                    .iter()
                    .enumerate()
                    .map(|(j, kv)| {
                        kv * rows[mirror(y as isize + j as isize - r, self.h) * self.w + x]
                    })
                    .sum();
            }
        }
        Self::new(self.w, self.h, px)
    }

    /// Gradient magnitude (3 × 3 Sobel over 8), edges mirrored.
    fn gradient(&self) -> Vec<f32> {
        let mut g = vec![0.0f32; self.px.len()];
        let p = |x: isize, y: isize| self.at(mirror(x, self.w), mirror(y, self.h));
        for y in 0..self.h as isize {
            for x in 0..self.w as isize {
                let gx = (p(x + 1, y - 1) + 2.0 * p(x + 1, y) + p(x + 1, y + 1)
                    - p(x - 1, y - 1)
                    - 2.0 * p(x - 1, y)
                    - p(x - 1, y + 1))
                    / 8.0;
                let gy = (p(x - 1, y + 1) + 2.0 * p(x, y + 1) + p(x + 1, y + 1)
                    - p(x - 1, y - 1)
                    - 2.0 * p(x, y - 1)
                    - p(x + 1, y - 1))
                    / 8.0;
                g[y as usize * self.w + x as usize] = gx.hypot(gy);
            }
        }
        g
    }
}

/// Index `i` mirrored into 0..n without repeating the edge (… 2 1 | 0 1 2 …).
fn mirror(i: isize, n: usize) -> usize {
    let n = n as isize;
    if n == 1 {
        return 0;
    }
    let period = 2 * (n - 1);
    let mut i = i.rem_euclid(period);
    if i >= n {
        i = period - i;
    }
    i as usize
}

/// Per output pixel the input pixels it covers and their weights.
fn area_weights(from: usize, to: usize) -> Vec<Vec<(usize, f32)>> {
    let scale = from as f64 / to as f64;
    (0..to)
        .map(|d| {
            let a = d as f64 * scale;
            let b = (a + scale).min(from as f64);
            let cell = b - a;
            let mut taps = Vec::new();
            let mut s = a.floor() as usize;
            while (s as f64) < b && s < from {
                let cover = (b.min(s as f64 + 1.0) - a.max(s as f64)).max(0.0);
                if cover > 1e-3 {
                    taps.push((s, (cover / cell) as f32));
                }
                s += 1;
            }
            taps
        })
        .collect()
}

/// Per output pixel the eight Lanczos taps (normalised) and their inputs.
fn lanczos_weights(from: usize, to: usize) -> Vec<Vec<(usize, f32)>> {
    let scale = from as f64 / to as f64;
    let sinc = |x: f64| {
        if x.abs() < 1e-9 {
            1.0
        } else {
            let p = std::f64::consts::PI * x;
            p.sin() / p
        }
    };
    (0..to)
        .map(|d| {
            let f = (d as f64 + 0.5) * scale - 0.5;
            let s = f.floor();
            let frac = f - s;
            let mut taps: Vec<(usize, f64)> = (-3..=4)
                .map(|i| {
                    let t = i as f64 - frac;
                    let src = (s as isize + i).clamp(0, from as isize - 1) as usize;
                    (src, sinc(t) * sinc(t / 4.0))
                })
                .collect();
            let sum: f64 = taps.iter().map(|t| t.1).sum();
            taps.iter_mut().for_each(|t| t.1 /= sum);
            taps.into_iter().map(|(s, k)| (s, k as f32)).collect()
        })
        .collect()
}

/// Linear-interpolated quantile `q` of `v` (sorted in place).
fn quantile(v: &mut [f32], q: f32) -> f32 {
    v.sort_by(f32::total_cmp);
    let at = (v.len() - 1) as f32 * q;
    let i = at.floor() as usize;
    let j = (i + 1).min(v.len() - 1);
    v[i] + (v[j] - v[i]) * (at - i as f32)
}

/// Median edge sigma of a region's strongest edges by re-blurring, `None`
/// with too few edge pixels.
pub fn reblur_sigma(y: &Plane) -> Option<f32> {
    if y.w < 8 || y.h < 8 {
        return None;
    }
    let y0 = y.gaussian(PRE);
    let g = y0.gradient();
    let gb = y0.gaussian(S0).gradient();
    // Two pixels at each edge are left out.
    let inside = |i: usize| {
        let (x, r) = (i % y.w, i / y.w);
        x >= 2 && x + 2 < y.w && r >= 2 && r + 2 < y.h
    };
    let mut values: Vec<f32> = (0..gb.len())
        .filter(|&i| inside(i))
        .map(|i| gb[i])
        .collect();
    if values.len() < MIN_POINTS * 3 {
        return None;
    }
    let t = quantile(&mut values, 1.0 - TOP);
    let ratios: Vec<f32> = (0..gb.len())
        .filter(|&i| inside(i) && gb[i] >= t && gb[i] > 1e-4)
        .map(|i| g[i] / gb[i])
        .collect();
    if ratios.len() < MIN_POINTS {
        return None;
    }
    let mut sigmas: Vec<f32> = ratios
        .into_iter()
        .filter(|&r| r > 1.0001)
        .map(|r| {
            // The edge's sigma including the pre-smoothing, then without it.
            let s = S0 / (r * r - 1.0).sqrt();
            (s * s - PRE * PRE).max(0.01).sqrt()
        })
        .collect();
    if sigmas.len() < MIN_POINTS {
        return Some(NO_DETAIL);
    }
    sigmas.sort_by(f32::total_cmp);
    let n = sigmas.len();
    Some(if n % 2 == 1 {
        sigmas[n / 2]
    } else {
        (sigmas[n / 2 - 1] + sigmas[n / 2]) / 2.0
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A synthetic eye: dark iris disc with a pupil, lid lines and lashes on
    /// skin, eyes `distance` px apart in an image of that scale, blurred by
    /// `blur` px (Gaussian), with landmarks on each eye's contour.
    fn face(distance: f32, blur: f32) -> (RgbImage, Vec<[f32; 2]>) {
        let size = (distance * 3.0) as u32;
        let centres = [
            [size as f32 / 2.0 - distance / 2.0, size as f32 / 2.0],
            [size as f32 / 2.0 + distance / 2.0, size as f32 / 2.0],
        ];
        let eye_w = distance * 0.45;
        let eye_h = distance * 0.18;
        let mut px = vec![0.0f32; (size * size) as usize];
        for y in 0..size {
            for x in 0..size {
                let mut v = 0.75f32;
                for c in centres {
                    let (dx, dy) = (x as f32 + 0.5 - c[0], y as f32 + 0.5 - c[1]);
                    let r = dx.hypot(dy);
                    let lid = (dx / (eye_w / 2.0)).powi(2) + (dy / (eye_h / 2.0)).powi(2);
                    if lid <= 1.0 {
                        v = 0.92;
                        if r < eye_h * 0.48 {
                            v = 0.30;
                        }
                        if r < eye_h * 0.2 {
                            v = 0.05;
                        }
                    }
                    if (lid - 1.0).abs() < 0.12 && dy < 0.0 {
                        v = 0.1;
                    }
                }
                px[(y * size + x) as usize] = v;
            }
        }
        let mut plane = Plane::new(size as usize, size as usize, px);
        if blur > 0.0 {
            plane = plane.gaussian(blur);
        }
        let image = RgbImage::from_fn(size, size, |x, y| {
            let v = (plane.at(x as usize, y as usize).clamp(0.0, 1.0) * 255.0).round() as u8;
            image::Rgb([v, v, v])
        });
        let mut landmarks = vec![[size as f32 / 2.0; 2]; 478];
        for (contour, c, corners) in [
            (&RIGHT_EYE, centres[0], [33, 133]),
            (&LEFT_EYE, centres[1], [263, 362]),
        ] {
            for (k, &i) in contour.iter().enumerate() {
                let a = k as f32 / contour.len() as f32 * std::f32::consts::TAU;
                landmarks[i] = [c[0] + a.cos() * eye_w / 2.0, c[1] + a.sin() * eye_h / 2.0];
            }
            landmarks[corners[0]] = [c[0] - eye_w / 2.0, c[1]];
            landmarks[corners[1]] = [c[0] + eye_w / 2.0, c[1]];
        }
        (image, landmarks)
    }

    #[test]
    fn sharp_eyes_read_sharp_and_blurred_eyes_soft() {
        for distance in [64.0, 128.0, 300.0] {
            let (sharp, lm) = face(distance, 0.0);
            let read = read(&sharp, &lm, 1.0);
            assert_eq!(read.state, FocusState::Sharp, "{distance}: {read:?}");
            assert!((read.eye_distance.unwrap() - distance).abs() < 1.0);
            // Blur that scales with the face: the same defocus for its size.
            let (soft, lm) = face(distance, distance / 128.0 * 3.0);
            let read = super::read(&soft, &lm, 1.0);
            assert_eq!(read.state, FocusState::Soft, "{distance}: {read:?}");
        }
    }

    #[test]
    fn sigma_grows_with_blur_and_not_with_contrast() {
        let sigma = |blur: f32| {
            let (image, lm) = face(128.0, blur);
            read(&image, &lm, 1.0).sigma.unwrap()
        };
        let values: Vec<f32> = [0.0, 1.0, 2.0, 3.0, 4.0]
            .iter()
            .map(|&b| sigma(b))
            .collect();
        assert!(values.windows(2).all(|w| w[0] < w[1]), "{values:?}");
        // At the fixed size, a Gaussian blur of 3 px reads about 3 px.
        assert!((values[3] - 3.0).abs() < 0.6, "{values:?}");
        // Halving the contrast leaves the sigma nearly as it was.
        let (image, lm) = face(128.0, 2.0);
        let dim = RgbImage::from_fn(image.width(), image.height(), |x, y| {
            let v = image.get_pixel(x, y).0[0];
            image::Rgb([v / 2 + 60; 3])
        });
        let a = read(&image, &lm, 1.0).sigma.unwrap();
        let b = read(&dim, &lm, 1.0).sigma.unwrap();
        assert!((a - b).abs() / a < 0.1, "{a} vs {b}");
    }

    #[test]
    fn landmarks_of_a_smaller_image_scale_to_this_one() {
        let (image, lm) = face(256.0, 0.0);
        let half: Vec<[f32; 2]> = lm.iter().map(|&[x, y]| [x / 2.0, y / 2.0]).collect();
        assert_eq!(read(&image, &half, 2.0), read(&image, &lm, 1.0));
    }

    #[test]
    fn small_faces_get_no_call() {
        let (image, lm) = face(40.0, 0.0);
        let read = read(&image, &lm, 1.0);
        assert_eq!(read.state, FocusState::Uncertain);
        assert_eq!(read.sigma, None);
        assert_eq!(classify(Some(1.0), 47.9), FocusState::Uncertain);
    }

    #[test]
    fn calls_follow_the_frozen_threshold() {
        assert_eq!(classify(Some(1.2), 100.0), FocusState::Sharp);
        assert_eq!(classify(Some(SHARP), 100.0), FocusState::Sharp);
        assert_eq!(classify(Some(1.585), 100.0), FocusState::Uncertain);
        assert_eq!(classify(Some(SOFT), 100.0), FocusState::Soft);
        assert_eq!(classify(Some(NO_DETAIL), 100.0), FocusState::Soft);
        assert_eq!(classify(None, 100.0), FocusState::Uncertain);
        for state in [FocusState::Sharp, FocusState::Soft, FocusState::Uncertain] {
            assert_eq!(FocusState::parse(state.as_str()), Some(state));
        }
    }

    #[test]
    fn resampling_keeps_flat_areas_and_mirrors_edges() {
        let flat = Plane::new(10, 7, vec![0.5; 70]);
        for p in [
            flat.resize_area(4, 3),
            flat.resize_lanczos4(23, 17),
            flat.gaussian(1.0),
        ] {
            assert!(p.px.iter().all(|v| (v - 0.5).abs() < 1e-5));
        }
        assert_eq!(
            (-2..7).map(|i| mirror(i, 5)).collect::<Vec<_>>(),
            [2, 1, 0, 1, 2, 3, 4, 3, 2]
        );
    }
}
