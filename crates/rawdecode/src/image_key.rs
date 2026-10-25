//! Image key of a decoded photograph, for the Shadows slider.
//!
//! Camera Raw's Shadows adapts to the photograph: on 60 raw.pixls.us
//! cameras its lift at +100 grew by about 0.3 EV of scene gain per EV of
//! the 90th percentile of scene luminance (brighter frames lift their
//! shadows more, lowering darkens more), while Blacks and Contrast did not
//! adapt (notes: reference/tone-controls.md, 2026-10-10). The 90th
//! percentile predicted it best of the percentiles tried (p50…p99.5) and
//! better than a large-radius local surround.
//!
//! Raising also depends on the frame's range: log2(p90 / p10) predicted
//! the lift around the black point better than p90 alone (leave-one-out
//! rmse 0.33 against 0.40 EV), so both percentiles are kept.
//!
//! Lowering Highlights (tone version 3) adapts to how dark the frame is;
//! the median tells a night scene from a frame of mid tones with a dark
//! part at the same p10 and p90 (notes: G05), so it is kept too.
//!
//! It is measured once on the full-resolution decode (scene-linear, before
//! exposure and the baseline), on a grid of about 1024 samples across the
//! long edge, so the editor's preview and every export use the same value.

use image::DynamicImage;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// Samples across the long edge.
const SAMPLES_LONG_EDGE: usize = 1024;
/// The percentiles of luminance kept: [10th, 90th, 50th].
const PERCENTILES: [f32; 3] = [0.1, 0.9, 0.5];

/// [10th, 90th, 50th] percentile of linear luminance.
pub type ImageKey = [f32; 3];

fn cache() -> &'static Mutex<HashMap<String, ImageKey>> {
    static CACHE: OnceLock<Mutex<HashMap<String, ImageKey>>> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// The key measured for `path`'s last full decode, if any.
pub fn for_path(path: &str) -> Option<ImageKey> {
    cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(path)
        .copied()
}

/// Records `key`, measured on a full decode of `path`, as its last.
pub fn record(path: &str, key: ImageKey) {
    let mut cache = cache().lock().unwrap_or_else(|e| e.into_inner());
    if cache.len() > 8192 {
        cache.clear();
    }
    cache.insert(path.to_string(), key);
}

/// The 10th, 90th and 50th percentile of linear luminance, sampled on a
/// grid; None for tiny or black frames.
pub fn measure(image: &DynamicImage) -> Option<ImageKey> {
    let rgb = match image {
        DynamicImage::ImageRgb32F(buffer) => std::borrow::Cow::Borrowed(buffer),
        other => std::borrow::Cow::Owned(other.to_rgb32f()),
    };
    let (width, height) = (rgb.width() as usize, rgb.height() as usize);
    if width < 16 || height < 16 {
        return None;
    }
    let step = (width.max(height) / SAMPLES_LONG_EDGE).max(1);
    let data = rgb.as_raw();
    let mut lumas: Vec<f32> = Vec::with_capacity((width / step + 1) * (height / step + 1));
    for y in (step / 2..height).step_by(step) {
        for x in (step / 2..width).step_by(step) {
            let i = (y * width + x) * 3;
            let luma = 0.2126 * data[i] + 0.7152 * data[i + 1] + 0.0722 * data[i + 2];
            if luma.is_finite() {
                lumas.push(luma.max(0.0));
            }
        }
    }
    if lumas.len() < 256 {
        return None;
    }
    let mut key = [0.0; 3];
    for (value, p) in key.iter_mut().zip(PERCENTILES) {
        let index = ((lumas.len() - 1) as f32 * p) as usize;
        *value = *lumas.select_nth_unstable_by(index, f32::total_cmp).1;
    }
    (key[1] > 0.0).then_some(key)
}

#[cfg(test)]
mod tests {
    use super::measure;
    use image::{DynamicImage, Rgb32FImage};

    #[test]
    fn tenth_ninetieth_and_fiftieth_percentile_of_luminance() {
        // Luminance rises left to right from 0 to 1: p10 0.1, p90 0.9, p50 0.5.
        let image = Rgb32FImage::from_fn(2000, 100, |x, _| {
            let v = x as f32 / 1999.0;
            image::Rgb([v, v, v])
        });
        let key = measure(&DynamicImage::ImageRgb32F(image)).unwrap();
        assert!(
            (key[0] - 0.1).abs() < 0.01
                && (key[1] - 0.9).abs() < 0.01
                && (key[2] - 0.5).abs() < 0.01,
            "key {key:?}"
        );
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
        for i in 0..3 {
            assert!(
                (full[i] / small[i]).log2().abs() < 0.02,
                "{full:?} vs {small:?}"
            );
        }
    }
}
