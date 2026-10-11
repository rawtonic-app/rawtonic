//! The photo's tone distribution for the luma range eyedropper.
//!
//! Lightroom's Luminance Range picker works in the photo's luminance
//! percentiles: a click sets a range 20 percentiles wide around the clicked
//! tone, with 25 percentiles of feather on each side (notes:
//! reference/luminance-sampling.md). The eyedropper turns that into the luma
//! range's tones (percent of white) with these levels.

use image::{DynamicImage, GenericImageView};

/// At most this many pixels are counted.
const MAX_SAMPLES: u64 = 1_000_000;

/// The tone (percent of white, Rec. 601 luma of the 8-bit mask source, as the
/// luma range compares tones) at each whole percentile 0, 1, …, 100 of the
/// image's pixels, interpolated within quarter-level histogram bins. Empty for
/// an empty image.
pub fn luma_levels(image: &DynamicImage) -> Vec<f32> {
    let (width, height) = image.dimensions();
    let count = width as u64 * height as u64;
    if count == 0 {
        return Vec::new();
    }
    let step = count.div_ceil(MAX_SAMPLES).max(1);
    let mut histogram = vec![0u64; 1024];
    let mut index = 0u64;
    while index < count {
        let (x, y) = ((index % width as u64) as u32, (index / width as u64) as u32);
        let pixel = image.get_pixel(x, y);
        let luma = 0.299 * pixel[0] as f32 + 0.587 * pixel[1] as f32 + 0.114 * pixel[2] as f32;
        histogram[((luma * 4.0) as usize).min(1023)] += 1;
        index += step;
    }
    levels_from_histogram(&histogram)
}

/// Percentiles 0..=100 of a quarter-level luma histogram, in percent of white.
fn levels_from_histogram(histogram: &[u64]) -> Vec<f32> {
    let total: u64 = histogram.iter().sum();
    if total == 0 {
        return Vec::new();
    }
    let first = histogram.iter().position(|&c| c > 0).unwrap_or(0);
    let last = histogram.iter().rposition(|&c| c > 0).unwrap_or(0);
    let to_percent = |level: f32| (level / 4.0 / 2.55).clamp(0.0, 100.0);
    (0..=100)
        .map(|p| {
            if p == 0 {
                return to_percent(first as f32);
            }
            if p == 100 {
                return to_percent(last as f32 + 1.0);
            }
            let target = total as f64 * p as f64 / 100.0;
            let mut below = 0u64;
            for (bin, &n) in histogram.iter().enumerate() {
                if n > 0 && (below + n) as f64 >= target {
                    let within = ((target - below as f64) / n as f64) as f32;
                    return to_percent(bin as f32 + within);
                }
                below += n;
            }
            to_percent(last as f32 + 1.0)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgb, RgbImage};

    #[test]
    fn a_horizontal_ramp_has_evenly_spaced_levels() {
        let ramp = RgbImage::from_fn(256, 8, |x, _| Rgb([x as u8; 3]));
        let levels = luma_levels(&DynamicImage::ImageRgb8(ramp));
        assert_eq!(levels.len(), 101);
        for (p, level) in levels.iter().enumerate() {
            assert!((level - p as f32).abs() < 0.8, "percentile {p}: {level}");
        }
        assert!(levels.windows(2).all(|w| w[0] <= w[1]));
    }

    #[test]
    fn levels_follow_the_distribution_not_the_scale() {
        // Three quarters of the photo dark (tone 20 %), a quarter bright (80 %).
        let image = RgbImage::from_fn(100, 100, |x, _| {
            let v = if x < 75 { 51 } else { 204 };
            Rgb([v, v, v])
        });
        let levels = luma_levels(&DynamicImage::ImageRgb8(image));
        assert!((levels[50] - 20.0).abs() < 0.2, "{}", levels[50]);
        assert!((levels[74] - 20.0).abs() < 0.2, "{}", levels[74]);
        assert!((levels[80] - 80.0).abs() < 0.2, "{}", levels[80]);
    }

    #[test]
    fn an_empty_image_has_no_levels() {
        assert!(luma_levels(&DynamicImage::new_rgb8(0, 0)).is_empty());
        assert!(levels_from_histogram(&[0; 1024]).is_empty());
    }
}
