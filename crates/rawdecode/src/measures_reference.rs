//! The decode measures as they were before they ran in parallel (2026-11-16),
//! kept verbatim as the reference the parallel `measure`s must match bit for
//! bit (tests below). Only the function names differ.

use image::DynamicImage;

const NOISE_STEP: usize = 2;
const P25_TO_SIGMA: f32 = 1.0 / 0.3186;
const RESIDUAL_TO_PIXEL: f32 = 0.894_427_2;
const SAMPLES_LONG_EDGE: usize = 1024;
const KEY_PERCENTILES: [f32; 3] = [0.1, 0.9, 0.5];
const WINDOW_SHARE: f32 = 31.0 / 2000.0;
const AIRLIGHT_PERCENTILE: f32 = 0.999;

pub fn noise_level(image: &DynamicImage) -> Option<f32> {
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
    let mut residuals: Vec<f32> = Vec::with_capacity((width / NOISE_STEP) * (height / NOISE_STEP));
    for y in (1..height - 1).step_by(NOISE_STEP) {
        for x in (1..width - 1).step_by(NOISE_STEP) {
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

pub fn image_key(image: &DynamicImage) -> Option<[f32; 3]> {
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
    for (value, p) in key.iter_mut().zip(KEY_PERCENTILES) {
        let index = ((lumas.len() - 1) as f32 * p) as usize;
        *value = *lumas.select_nth_unstable_by(index, f32::total_cmp).1;
    }
    (key[1] > 0.0).then_some(key)
}

pub fn airlight(image: &DynamicImage) -> Option<f32> {
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
                for px in row.as_chunks::<3>().0 {
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
    let radius = ((WINDOW_SHARE * w.max(h) as f32 - 1.0) / 2.0)
        .round()
        .max(0.0) as usize;
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
    let index = ((dark.len() - 1) as f32 * AIRLIGHT_PERCENTILE) as usize;
    let level = *dark.select_nth_unstable_by(index, f32::total_cmp).1;
    (level > 0.0).then_some(level)
}

mod tests {
    use image::{DynamicImage, Rgb32FImage};

    /// Frames with noise, flat areas, clipped and negative values, NaN and
    /// infinity, at sizes on and off the sampling steps.
    fn frames() -> Vec<DynamicImage> {
        let mut state = 5u32;
        let mut uniform = move || {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (state >> 8) as f32 / (1u32 << 24) as f32
        };
        // The last too narrow for the airlight grid, not for the noise level.
        [(3001, 2003), (2048, 1365), (517, 389), (40, 30), (20, 5000)]
            .into_iter()
            .map(|(w, h)| {
                let image = Rgb32FImage::from_fn(w, h, |x, y| {
                    let base = 0.002 + 0.9 * (x as f32 / w as f32) * (y as f32 / h as f32);
                    let noise = 0.02 * (uniform() - 0.5);
                    let v = match (x * 7 + y * 13) % 997 {
                        0 => f32::NAN,
                        1 => f32::INFINITY,
                        2 => -0.3,
                        3 => 4.0,
                        _ if y < h / 5 => 0.6,
                        _ => base + noise,
                    };
                    image::Rgb([v, v * (0.8 + 0.2 * uniform()), v * 1.1])
                });
                DynamicImage::ImageRgb32F(image)
            })
            .collect()
    }

    #[test]
    fn the_parallel_measures_match_the_serial_ones_bit_for_bit() {
        let bits = |v: Option<f32>| v.map(f32::to_bits);
        for frame in frames() {
            let size = (frame.width(), frame.height());
            assert_eq!(
                bits(crate::noise_level::measure(&frame)),
                bits(super::noise_level(&frame)),
                "noise level {size:?}"
            );
            assert_eq!(
                crate::image_key::measure(&frame).map(|k| k.map(f32::to_bits)),
                super::image_key(&frame).map(|k| k.map(f32::to_bits)),
                "image key {size:?}"
            );
            assert_eq!(
                bits(crate::airlight::measure(&frame)),
                bits(super::airlight(&frame)),
                "airlight {size:?}"
            );
            let (noise, airlight) = crate::airlight::with_noise_level(&frame);
            assert_eq!(
                (bits(noise), bits(airlight)),
                (
                    bits(super::noise_level(&frame)),
                    bits(super::airlight(&frame))
                ),
                "in one pass {size:?}"
            );
        }
    }
}
