//! The scene's neutral for Auto white balance: the colour a grey surface
//! has in a linear image.
//!
//! Three estimates, combined by their geometric mean (each normalised to a
//! chromaticity first):
//! - shades of grey: the p = 6 power mean, which leans towards bright
//!   surfaces without trusting a single highlight;
//! - bright surfaces: the mean of the brightest 2 % of pixels;
//! - grey surfaces: the mean of the 1 % of pixels whose local contrast is
//!   most alike in all three channels (in log space a grey surface changes
//!   brightness, never hue, so its edges have the same strength per
//!   channel).
//!
//! Chosen on scenes with a measured grey (notes: reference/white-balance.md):
//! on 270 held-out scenes 1.88° mean angular error against 2.72° for grey
//! world alone; tuned on another 272. Pixels at or above the decode's white
//! and near-black pixels are left out.

use image::{DynamicImage, Rgb32FImage};
use rayon::prelude::*;

/// Long edge of the image the estimate works on.
pub const SIZE: u32 = 512;
const CLIPPED: f32 = 1.0;
const DARK: f32 = 1e-4;
const POWER: f64 = 6.0;
const BRIGHT_SHARE: f64 = 0.02;
const GREY_SHARE: f64 = 0.01;
const GREY_SIGMA: f32 = 2.0;
const GREY_WINDOW: usize = 7;

/// The neutral (linear RGB, as in `image`) of a linear photograph, or
/// `None` when it has no usable pixels.
pub fn neutral(image: &DynamicImage) -> Option<[f32; 3]> {
    neutral_where(image, |_, _| true)
}

/// [`neutral`] from the pixels where `keep(x, y)` holds (coordinates 0..1
/// across the image), e.g. to leave out a reference target.
pub fn neutral_where(image: &DynamicImage, keep: impl Fn(f32, f32) -> bool) -> Option<[f32; 3]> {
    let small = crate::image_processing::thumbnail(image, SIZE, SIZE).to_rgb32f();
    let (w, h) = (small.width() as f32, small.height() as f32);
    let kept: Vec<bool> = (0..small.width() * small.height())
        .map(|i| {
            let (x, y) = ((i % small.width()) as f32, (i / small.width()) as f32);
            keep((x + 0.5) / w, (y + 0.5) / h)
        })
        .collect();
    neutral_of_kept(&small, &kept)
}

#[cfg(test)]
fn neutral_of(img: &Rgb32FImage) -> Option<[f32; 3]> {
    neutral_of_kept(img, &vec![true; (img.width() * img.height()) as usize])
}

fn neutral_of_kept(img: &Rgb32FImage, kept: &[bool]) -> Option<[f32; 3]> {
    let usable: Vec<bool> = img
        .pixels()
        .zip(kept)
        .map(|(p, keep)| *keep && p.0.iter().all(|v| *v < CLIPPED && *v > DARK))
        .collect();
    let pixels: Vec<[f32; 3]> = img
        .pixels()
        .zip(&usable)
        .filter(|(_, ok)| **ok)
        .map(|(p, _)| p.0)
        .collect();
    if pixels.is_empty() {
        return None;
    }
    let estimates = [
        shades_of_grey(&pixels),
        bright_surfaces(&pixels),
        grey_surfaces(img, &usable).unwrap_or_else(|| mean(pixels.iter())),
    ];
    let mut log = [0f64; 3];
    for e in estimates {
        let sum: f64 = e.iter().sum();
        // Also rejects NaN.
        let positive = |v: f64| v.is_finite() && v > 0.0;
        if !positive(sum) || !e.iter().all(|v| positive(*v)) {
            return None;
        }
        for (l, v) in log.iter_mut().zip(e) {
            *l += (v / sum).ln() / 3.0;
        }
    }
    Some(log.map(|l| l.exp() as f32))
}

fn mean<'a>(pixels: impl Iterator<Item = &'a [f32; 3]>) -> [f64; 3] {
    let (mut sum, mut n) = ([0f64; 3], 0f64);
    for p in pixels {
        for c in 0..3 {
            sum[c] += p[c] as f64;
        }
        n += 1.0;
    }
    sum.map(|s| s / n.max(1.0))
}

fn shades_of_grey(pixels: &[[f32; 3]]) -> [f64; 3] {
    let mut sum = [0f64; 3];
    for p in pixels {
        for c in 0..3 {
            sum[c] += (p[c] as f64).powf(POWER);
        }
    }
    sum.map(|s| (s / pixels.len() as f64).powf(1.0 / POWER))
}

fn bright_surfaces(pixels: &[[f32; 3]]) -> [f64; 3] {
    let brightness: Vec<f32> = pixels.iter().map(|p| p[0] + p[1] + p[2]).collect();
    let limit = quantile(&brightness, 1.0 - BRIGHT_SHARE);
    mean(
        pixels
            .iter()
            .zip(&brightness)
            .filter(|(_, b)| **b >= limit)
            .map(|(p, _)| p),
    )
}

/// The value below which `share` of `values` lie (linear interpolation
/// between order statistics).
fn quantile(values: &[f32], share: f64) -> f32 {
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let pos = share.clamp(0.0, 1.0) * (sorted.len() - 1) as f64;
    let (i, f) = (pos.floor() as usize, pos.fract() as f32);
    let next = sorted[(i + 1).min(sorted.len() - 1)];
    sorted[i] + (next - sorted[i]) * f
}

/// Mean of the pixels whose log-contrast (|Laplacian of Gaussian| of the
/// log image) is most alike across channels; `None` when too few pixels
/// carry contrast.
fn grey_surfaces(img: &Rgb32FImage, usable: &[bool]) -> Option<[f64; 3]> {
    let (w, h) = (img.width() as usize, img.height() as usize);
    let contrast: Vec<Vec<f32>> = (0..3)
        .into_par_iter()
        .map(|c| {
            let log: Vec<f32> = img.pixels().map(|p| p.0[c].max(DARK).ln()).collect();
            laplacian_of_gaussian(&log, w, h, GREY_SIGMA)
                .into_iter()
                .map(f32::abs)
                .collect()
        })
        .collect();
    let mut greyness = vec![0f32; w * h];
    let mut carries = vec![false; w * h];
    for i in 0..w * h {
        let m = (contrast[0][i] + contrast[1][i] + contrast[2][i]) / 3.0;
        let spread = (0..3)
            .map(|c| (contrast[c][i] - m).powi(2))
            .sum::<f32>()
            .sqrt();
        greyness[i] = spread / (m + 1e-6);
        carries[i] = usable[i] && m > 1e-3;
    }
    let carries = erode(&carries, w, h);
    let masked: Vec<f32> = greyness
        .iter()
        .zip(&carries)
        .map(|(g, ok)| if *ok { *g } else { 1e3 })
        .collect();
    let smooth = box_filter(&masked, w, h, GREY_WINDOW);
    let candidates: Vec<f32> = smooth
        .iter()
        .zip(&carries)
        .filter(|(_, ok)| **ok)
        .map(|(g, _)| *g)
        .collect();
    if candidates.len() < 50 {
        return None;
    }
    let limit = quantile(&candidates, GREY_SHARE);
    let pixels: Vec<[f32; 3]> = img
        .pixels()
        .enumerate()
        .filter(|(i, _)| carries[*i] && smooth[*i] <= limit)
        .map(|(_, p)| p.0)
        .collect();
    Some(mean(pixels.iter()))
}

/// Mirror index for "reflect" borders (edge sample repeated).
fn reflect(i: isize, n: usize) -> usize {
    let n = n as isize;
    let period = 2 * n;
    let mut i = i.rem_euclid(period);
    if i >= n {
        i = period - 1 - i;
    }
    i as usize
}

fn convolve_rows(src: &[f32], w: usize, kernel: &[f32]) -> Vec<f32> {
    let r = (kernel.len() / 2) as isize;
    let mut out = vec![0f32; src.len()];
    out.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let line = &src[y * w..(y + 1) * w];
        for (x, o) in row.iter_mut().enumerate() {
            *o = kernel
                .iter()
                .enumerate()
                .map(|(k, v)| v * line[reflect(x as isize + k as isize - r, w)])
                .sum();
        }
    });
    out
}

fn convolve_cols(src: &[f32], w: usize, h: usize, kernel: &[f32]) -> Vec<f32> {
    let r = (kernel.len() / 2) as isize;
    let mut out = vec![0f32; w * h];
    out.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (k, v) in kernel.iter().enumerate() {
            let line = &src[reflect(y as isize + k as isize - r, h) * w..][..w];
            for (o, s) in row.iter_mut().zip(line) {
                *o += v * s;
            }
        }
    });
    out
}

/// Gaussian (order 0) and its second derivative (order 2), radius 4σ.
fn gaussian_kernels(sigma: f32) -> (Vec<f32>, Vec<f32>) {
    let radius = (4.0 * sigma + 0.5) as i32;
    let s2 = sigma * sigma;
    let g: Vec<f32> = (-radius..=radius)
        .map(|x| (-0.5 * (x * x) as f32 / s2).exp())
        .collect();
    let total: f32 = g.iter().sum();
    let g: Vec<f32> = g.iter().map(|v| v / total).collect();
    let d2 = (-radius..=radius)
        .zip(&g)
        .map(|(x, v)| v * ((x * x) as f32 / (s2 * s2) - 1.0 / s2))
        .collect();
    (g, d2)
}

fn laplacian_of_gaussian(src: &[f32], w: usize, h: usize, sigma: f32) -> Vec<f32> {
    let (g, d2) = gaussian_kernels(sigma);
    let xx = convolve_cols(&convolve_rows(src, w, &d2), w, h, &g);
    let yy = convolve_cols(&convolve_rows(src, w, &g), w, h, &d2);
    xx.iter().zip(&yy).map(|(a, b)| a + b).collect()
}

/// Binary erosion with the 4-neighbourhood; outside the image counts as
/// unset.
fn erode(mask: &[bool], w: usize, h: usize) -> Vec<bool> {
    let at = |x: isize, y: isize| {
        x >= 0
            && y >= 0
            && (x as usize) < w
            && (y as usize) < h
            && mask[y as usize * w + x as usize]
    };
    (0..w * h)
        .map(|i| {
            let (x, y) = ((i % w) as isize, (i / w) as isize);
            at(x, y) && at(x - 1, y) && at(x + 1, y) && at(x, y - 1) && at(x, y + 1)
        })
        .collect()
}

fn box_filter(src: &[f32], w: usize, h: usize, size: usize) -> Vec<f32> {
    let kernel = vec![1.0 / size as f32; size];
    convolve_cols(&convolve_rows(src, w, &kernel), w, h, &kernel)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scene(cast: [f32; 3]) -> Rgb32FImage {
        // Coloured patches plus one grey card, all lit by `cast`.
        Rgb32FImage::from_fn(96, 64, |x, y| {
            let surface = match (x / 32, y / 32) {
                (0, 0) => [0.6, 0.2, 0.1],
                (1, 0) => [0.1, 0.5, 0.2],
                (2, 0) => [0.5, 0.5, 0.5],
                (0, 1) => [0.2, 0.3, 0.6],
                (1, 1) => [0.4, 0.35, 0.1],
                _ => [0.3, 0.1, 0.4],
            };
            // A little texture so local contrast exists inside patches.
            let t = 1.0 + 0.2 * (((x * 7 + y * 13) % 5) as f32 / 4.0 - 0.5);
            image::Rgb(std::array::from_fn(|c| surface[c] * cast[c] * t))
        })
    }

    fn chromaticity(v: [f32; 3]) -> [f32; 3] {
        let s: f32 = v.iter().sum();
        v.map(|c| c / s)
    }

    /// A change of light changes the neutral by that light's colour.
    #[test]
    fn neutral_follows_the_light() {
        let white = chromaticity(neutral_of(&scene([1.0, 1.0, 1.0])).unwrap());
        let warm = [1.4, 1.0, 0.6];
        let lit = chromaticity(neutral_of(&scene(warm)).unwrap());
        let want = chromaticity(std::array::from_fn(|c| white[c] * warm[c]));
        for c in 0..3 {
            assert!((lit[c] - want[c]).abs() < 0.01, "{lit:?} vs {want:?}");
        }
    }

    #[test]
    fn clipped_and_black_pixels_are_left_out() {
        let mut img = scene([1.0, 1.0, 1.0]);
        let before = neutral_of(&img).unwrap();
        for x in 0..8 {
            img.put_pixel(x, 0, image::Rgb([5.0, 1.2, 0.1]));
            img.put_pixel(x, 1, image::Rgb([0.0, 0.0, 0.0]));
        }
        let after = neutral_of(&img).unwrap();
        for c in 0..3 {
            assert!((chromaticity(before)[c] - chromaticity(after)[c]).abs() < 0.01);
        }
    }

    #[test]
    fn no_usable_pixels_gives_none() {
        let img = Rgb32FImage::from_pixel(16, 16, image::Rgb([2.0, 2.0, 2.0]));
        assert!(neutral_of(&img).is_none());
    }

    #[test]
    fn quantile_interpolates() {
        assert_eq!(quantile(&[4.0, 1.0, 3.0, 2.0], 0.5), 2.5);
        assert_eq!(quantile(&[1.0, 2.0], 1.0), 2.0);
    }
}
