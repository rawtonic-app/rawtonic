//! Output sharpening: an unsharp mask on the finished output's lightness,
//! after the resize and before borders and watermarks (Capture One's
//! recipe sharpening with amount, radius and threshold; for print the
//! radius follows the viewing distance and resolution).

use image::DynamicImage;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct OutputSharpening {
    /// Strength in percent of the detail added back (0–500).
    pub amount: f32,
    /// Gaussian radius (sigma) in output pixels (0.2–10).
    pub radius: f32,
    /// Detail smaller than this many levels of 255 is left alone (0–50).
    #[serde(default)]
    pub threshold: f32,
}

impl OutputSharpening {
    fn is_active(&self) -> bool {
        self.amount > 0.0 && self.radius >= 0.1
    }
}

/// The radius that sharpens the finest detail an eye resolves at
/// `distance_cm` on a print of `ppi`: one arc minute (0.29 mm at 1 m).
pub fn print_radius(distance_cm: f32, ppi: f32) -> f32 {
    let detail_mm = distance_cm * 10.0 * (1.0f32 / 60.0).to_radians().tan();
    (detail_mm * ppi / 25.4).clamp(0.2, 10.0)
}

/// Sharpens `image` in place; formats without an RGB layout pass unchanged.
pub fn apply(image: &mut DynamicImage, settings: &OutputSharpening) {
    if !settings.is_active() {
        return;
    }
    let (width, height) = (image.width() as usize, image.height() as usize);
    match image {
        DynamicImage::ImageRgb8(buffer) => {
            sharpen(buffer.as_mut(), 3, width, height, 255.0, settings)
        }
        DynamicImage::ImageRgba8(buffer) => {
            sharpen(buffer.as_mut(), 4, width, height, 255.0, settings)
        }
        DynamicImage::ImageRgb16(buffer) => {
            sharpen(buffer.as_mut(), 3, width, height, 65535.0, settings)
        }
        DynamicImage::ImageRgba16(buffer) => {
            sharpen(buffer.as_mut(), 4, width, height, 65535.0, settings)
        }
        DynamicImage::ImageRgb32F(buffer) => {
            sharpen(buffer.as_mut(), 3, width, height, 1.0, settings)
        }
        DynamicImage::ImageRgba32F(buffer) => {
            sharpen(buffer.as_mut(), 4, width, height, 1.0, settings)
        }
        _ => {}
    }
}

trait Sample: Copy + Send + Sync {
    fn to_f32(self) -> f32;
    fn from_f32(value: f32, max: f32) -> Self;
}

impl Sample for u8 {
    fn to_f32(self) -> f32 {
        f32::from(self)
    }
    fn from_f32(value: f32, max: f32) -> Self {
        value.round().clamp(0.0, max) as u8
    }
}

impl Sample for u16 {
    fn to_f32(self) -> f32 {
        f32::from(self)
    }
    fn from_f32(value: f32, max: f32) -> Self {
        value.round().clamp(0.0, max) as u16
    }
}

impl Sample for f32 {
    fn to_f32(self) -> f32 {
        self
    }
    fn from_f32(value: f32, _max: f32) -> Self {
        value.max(0.0)
    }
}

fn kernel(sigma: f32) -> Vec<f32> {
    let reach = (sigma * 3.0).ceil().max(1.0) as i32;
    let weights: Vec<f32> = (-reach..=reach)
        .map(|i| (-(i * i) as f32 / (2.0 * sigma * sigma)).exp())
        .collect();
    let sum: f32 = weights.iter().sum();
    weights.into_iter().map(|w| w / sum).collect()
}

/// Rec. 709 lightness of the encoded values, in 0–1.
#[inline]
fn lightness<T: Sample>(px: &[T], scale: f32) -> f32 {
    (0.2126 * px[0].to_f32() + 0.7152 * px[1].to_f32() + 0.0722 * px[2].to_f32()) * scale
}

fn sharpen<T: Sample>(
    data: &mut [T],
    channels: usize,
    width: usize,
    height: usize,
    max: f32,
    settings: &OutputSharpening,
) {
    if width == 0 || height == 0 {
        return;
    }
    let weights = kernel(settings.radius);
    let reach = weights.len() / 2;
    let scale = 1.0 / max;
    let stride = width * channels;
    // Horizontal pass of the lightness; the vertical pass runs per output
    // row, so one plane is all the extra memory.
    let mut horizontal = vec![0.0f32; width * height];
    horizontal
        .par_chunks_mut(width)
        .zip(data.par_chunks(stride))
        .for_each(|(out, row)| {
            let light: Vec<f32> = row
                .chunks_exact(channels)
                .map(|px| lightness(px, scale))
                .collect();
            for (x, value) in out.iter_mut().enumerate() {
                *value = weights
                    .iter()
                    .enumerate()
                    .map(|(k, w)| {
                        let xi = (x + k).saturating_sub(reach).min(width - 1);
                        w * light[xi]
                    })
                    .sum();
            }
        });
    let amount = settings.amount / 100.0;
    let threshold = settings.threshold / 255.0;
    data.par_chunks_mut(stride)
        .enumerate()
        .for_each(|(y, row)| {
            for x in 0..width {
                let blurred: f32 = weights
                    .iter()
                    .enumerate()
                    .map(|(k, w)| {
                        let yi = (y + k).saturating_sub(reach).min(height - 1);
                        w * horizontal[yi * width + x]
                    })
                    .sum();
                let px = &mut row[x * channels..x * channels + channels];
                let detail = lightness(px, scale) - blurred;
                // Below the threshold nothing changes; above it the full
                // detail returns over a short ramp (no hard step).
                let gate = if threshold > 0.0 {
                    ((detail.abs() - threshold) / threshold).clamp(0.0, 1.0)
                } else {
                    1.0
                };
                let delta = amount * detail * gate * max;
                if delta != 0.0 {
                    for value in px.iter_mut().take(3) {
                        *value = T::from_f32(value.to_f32() + delta, max);
                    }
                }
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageBuffer, Rgb};

    fn edge() -> DynamicImage {
        DynamicImage::ImageRgb8(ImageBuffer::from_fn(32, 8, |x, _| {
            if x < 16 {
                Rgb([60, 60, 60])
            } else {
                Rgb([180, 180, 180])
            }
        }))
    }

    #[test]
    fn steepens_an_edge_and_leaves_flat_areas() {
        let mut image = edge();
        apply(
            &mut image,
            &OutputSharpening {
                amount: 100.0,
                radius: 1.0,
                threshold: 0.0,
            },
        );
        let px = |x: u32| image.as_rgb8().unwrap().get_pixel(x, 4)[0];
        assert!(px(15) < 60 && px(16) > 180, "{} {}", px(15), px(16));
        assert_eq!((px(2), px(29)), (60, 180));
    }

    #[test]
    fn threshold_keeps_fine_texture_and_zero_amount_changes_nothing() {
        let mut flat = DynamicImage::ImageRgb8(ImageBuffer::from_fn(16, 16, |x, y| {
            let v = 100 + ((x + y) % 2) as u8 * 2;
            Rgb([v, v, v])
        }));
        let before = flat.clone();
        apply(
            &mut flat,
            &OutputSharpening {
                amount: 200.0,
                radius: 1.0,
                threshold: 4.0,
            },
        );
        assert_eq!(flat, before);
        let mut image = edge();
        apply(
            &mut image,
            &OutputSharpening {
                amount: 0.0,
                radius: 1.0,
                threshold: 0.0,
            },
        );
        assert_eq!(image, edge());
    }

    #[test]
    fn print_radius_follows_distance_and_resolution() {
        assert!((print_radius(50.0, 300.0) - 1.72).abs() < 0.01);
        assert!((print_radius(100.0, 300.0) - 3.44).abs() < 0.02);
    }
}
