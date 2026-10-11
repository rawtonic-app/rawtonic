//! Luminance noise level of a decoded photograph, for the noise reduction.
//!
//! Camera Raw scales its noise reduction to a noise model of the camera and
//! ISO; a fixed tolerance (as inherited) smooths a clean photo and barely
//! touches a noisy one. Here the noise is measured on the full-resolution
//! decode: luminance in square-root encoding (where shot noise has about
//! the same spread at every brightness) minus the mean of its four
//! neighbours, over the flattest quarter of the samples (texture inflates
//! the rest). The level is the standard deviation of that residual for
//! white noise, kept per file for the renderer.

use image::DynamicImage;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// Rows and columns between analysed samples.
const STEP: usize = 2;
/// For Gaussian noise the 25th percentile of |residual| is 0.3186 σ.
const P25_TO_SIGMA: f32 = 1.0 / 0.3186;
/// Residual against the four-neighbour mean has 1.25 times the variance.
const RESIDUAL_TO_PIXEL: f32 = 0.894_427_2; // 1 / sqrt(1.25)

fn cache() -> &'static Mutex<HashMap<String, f32>> {
    static CACHE: OnceLock<Mutex<HashMap<String, f32>>> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// The level measured for `path`'s last full decode, if any.
pub fn for_path(path: &str) -> Option<f32> {
    cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(path)
        .copied()
}

/// Measures and remembers the level of a full-resolution decode.
pub fn remember(path: &str, image: &DynamicImage) {
    if let Some(level) = measure(image) {
        let mut cache = cache().lock().unwrap_or_else(|e| e.into_inner());
        if cache.len() > 8192 {
            cache.clear();
        }
        cache.insert(path.to_string(), level);
        log::info!("Noise level for '{path}': {level:.5}");
    }
}

/// Noise level (σ of single-pixel luminance in square-root encoding).
pub fn measure(image: &DynamicImage) -> Option<f32> {
    let rgb = match image {
        DynamicImage::ImageRgb32F(buffer) => std::borrow::Cow::Borrowed(buffer),
        other => std::borrow::Cow::Owned(other.to_rgb32f()),
    };
    let (width, height) = (rgb.width() as usize, rgb.height() as usize);
    if width < 16 || height < 16 {
        return None;
    }
    let data = rgb.as_raw();
    let encoded = |x: usize, y: usize| {
        let i = (y * width + x) * 3;
        let luma = 0.2126 * data[i] + 0.7152 * data[i + 1] + 0.0722 * data[i + 2];
        (luma.max(0.0), luma.max(0.0).sqrt())
    };
    let mut residuals: Vec<f32> = Vec::with_capacity((width / STEP) * (height / STEP));
    for y in (1..height - 1).step_by(STEP) {
        for x in (1..width - 1).step_by(STEP) {
            let (luma, centre) = encoded(x, y);
            // Skip crushed blacks and clipped highlights: no noise there.
            if !(0.0005..=0.8).contains(&luma) {
                continue;
            }
            let neighbours = encoded(x - 1, y).1
                + encoded(x + 1, y).1
                + encoded(x, y - 1).1
                + encoded(x, y + 1).1;
            residuals.push((centre - 0.25 * neighbours).abs());
        }
    }
    if residuals.len() < 1024 {
        return None;
    }
    let index = residuals.len() / 4;
    let (_, p25, _) = residuals.select_nth_unstable_by(index, f32::total_cmp);
    Some(*p25 * P25_TO_SIGMA * RESIDUAL_TO_PIXEL)
}

#[cfg(test)]
mod tests {
    use super::measure;
    use image::{DynamicImage, Rgb32FImage};

    #[test]
    fn measures_white_noise_in_square_root_encoding() {
        // Flat grey 0.25 (0.5 encoded) with Gaussian-like noise of σ 0.01
        // in square-root encoding.
        let mut state = 7u32;
        let mut uniform = || {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (state >> 8) as f32 / (1u32 << 24) as f32
        };
        let image = Rgb32FImage::from_fn(256, 256, |_, _| {
            let gaussian = (0..12).map(|_| uniform()).sum::<f32>() - 6.0;
            let v = (0.5 + 0.01 * gaussian).powi(2);
            image::Rgb([v, v, v])
        });
        let level = measure(&DynamicImage::ImageRgb32F(image)).unwrap();
        assert!((level - 0.01).abs() < 0.0015, "level {level}");
    }
}
