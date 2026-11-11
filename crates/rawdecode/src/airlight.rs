//! Airlight level of a decoded photograph, for lowered Dehaze (`dehazeVersion` 3).
//!
//! Camera Raw's negative Dehaze mixes the photo towards a level estimated
//! from the photo itself: frames without near-white areas (sunsets, pastel
//! walls under a blue sky) are veiled towards a grey well below white
//! (notes: benchmark-results/dhz2-dehaze-refit-2026-11-11). Of the airlight
//! estimators tried on 64 frames against Camera Raw (DHZ3: He et al.'s
//! dark-channel airlight, quad-tree search, per-channel colours, percentiles
//! of the minimum channel and of luminance, and pairs of them), the
//! brightest 0.1 % of the dark channel predicted its veil best on held-out
//! halves: the 99.9th percentile of the minimum channel's local minimum over
//! a window of 1.55 % of the long edge (31 px at 2000 px).
//!
//! It is measured once on the full-resolution decode (scene-linear, before
//! exposure and the baseline), like the image key, on a grid of block means
//! about 1024 samples across the long edge, so the editor's preview, every
//! export, crop and virtual copy use the same value.

use image::DynamicImage;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// Samples across the long edge.
const SAMPLES_LONG_EDGE: usize = 1024;
/// Window of the dark channel's local minimum, as a share of the long edge.
const WINDOW_SHARE: f32 = 31.0 / 2000.0;
/// The percentile of the dark channel kept.
const PERCENTILE: f32 = 0.999;

fn cache() -> &'static Mutex<HashMap<String, f32>> {
    static CACHE: OnceLock<Mutex<HashMap<String, f32>>> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// The airlight measured for `path`'s last full decode, if any.
pub fn for_path(path: &str) -> Option<f32> {
    cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(path)
        .copied()
}

/// Records `level`, measured on a full decode of `path`, as its last.
pub fn record(path: &str, level: f32) {
    let mut cache = cache().lock().unwrap_or_else(|e| e.into_inner());
    if cache.len() > 8192 {
        cache.clear();
    }
    cache.insert(path.to_string(), level);
}

/// The 99.9th percentile of the dark channel (linear); None for tiny or
/// black frames.
pub fn measure(image: &DynamicImage) -> Option<f32> {
    let rgb = match image {
        DynamicImage::ImageRgb32F(buffer) => std::borrow::Cow::Borrowed(buffer),
        other => std::borrow::Cow::Owned(other.to_rgb32f()),
    };
    let (width, height) = (rgb.width() as usize, rgb.height() as usize);
    if width < 16 || height < 16 {
        return None;
    }
    let step = width.max(height).div_ceil(SAMPLES_LONG_EDGE).max(1);
    let (w, h) = (width / step, height / step);
    if w < 8 || h < 8 {
        return None;
    }
    // Minimum channel of block means.
    let data = rgb.as_raw();
    let mut dark = vec![0.0f32; w * h];
    let norm = 1.0 / (step * step) as f32;
    for by in 0..h {
        for bx in 0..w {
            let mut sum = [0.0f32; 3];
            for y in by * step..(by + 1) * step {
                let row = &data[(y * width + bx * step) * 3..(y * width + (bx + 1) * step) * 3];
                for px in row.chunks_exact(3) {
                    for c in 0..3 {
                        if px[c].is_finite() {
                            sum[c] += px[c];
                        }
                    }
                }
            }
            dark[by * w + bx] = (sum[0].min(sum[1]).min(sum[2]) * norm).max(0.0);
        }
    }
    // Local minimum over a square window (separable).
    let radius = ((WINDOW_SHARE * w.max(h) as f32 - 1.0) / 2.0).round().max(0.0) as usize;
    let mut rows = vec![0.0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            let (x0, x1) = (x.saturating_sub(radius), (x + radius).min(w - 1));
            rows[y * w + x] = dark[y * w + x0..=y * w + x1]
                .iter()
                .copied()
                .fold(f32::INFINITY, f32::min);
        }
    }
    for x in 0..w {
        for y in 0..h {
            let (y0, y1) = (y.saturating_sub(radius), (y + radius).min(h - 1));
            dark[y * w + x] = (y0..=y1)
                .map(|yy| rows[yy * w + x])
                .fold(f32::INFINITY, f32::min);
        }
    }
    let index = ((dark.len() - 1) as f32 * PERCENTILE) as usize;
    let level = *dark.select_nth_unstable_by(index, f32::total_cmp).1;
    (level > 0.0).then_some(level)
}

#[cfg(test)]
mod tests {
    use super::measure;
    use image::{DynamicImage, Rgb32FImage};

    #[test]
    fn small_bright_spots_do_not_set_the_airlight() {
        // A grey frame at 0.2 with a few pixel-sized white spots: the local
        // minimum removes them.
        let image = Rgb32FImage::from_fn(3000, 2000, |x, y| {
            let v = if x % 400 == 7 && y % 300 == 5 { 1.0 } else { 0.2 };
            image::Rgb([v, v, v])
        });
        let level = measure(&DynamicImage::ImageRgb32F(image)).unwrap();
        assert!((level - 0.2).abs() < 0.005, "level {level}");
    }

    #[test]
    fn the_least_saturated_channel_of_a_large_bright_area() {
        // A sky-blue top third over a dark ground: the airlight is the sky's
        // weakest channel.
        let image = Rgb32FImage::from_fn(3000, 2000, |_, y| {
            if y < 700 {
                image::Rgb([0.35, 0.5, 0.8])
            } else {
                image::Rgb([0.05, 0.06, 0.04])
            }
        });
        let level = measure(&DynamicImage::ImageRgb32F(image)).unwrap();
        assert!((level - 0.35).abs() < 0.005, "level {level}");
    }

    #[test]
    fn independent_of_resolution() {
        let make = |w: u32, h: u32| {
            Rgb32FImage::from_fn(w, h, |x, y| {
                let v = 0.01 + 0.5 * (x as f32 / w as f32) * (y as f32 / h as f32);
                image::Rgb([v, v * 0.8, v * 1.1])
            })
        };
        let full = measure(&DynamicImage::ImageRgb32F(make(6000, 4000))).unwrap();
        let small = measure(&DynamicImage::ImageRgb32F(make(1500, 1000))).unwrap();
        assert!((full / small).log2().abs() < 0.03, "{full} vs {small}");
    }
}
