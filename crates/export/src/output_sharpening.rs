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
    /// `from_f32(v.to_f32() + 0.0, max) == v` for every value (integer
    /// levels; not floats, where it would turn −0, negatives and NaN to 0).
    const ZERO_CHANGE_KEEPS_VALUE: bool;
    fn to_f32(self) -> f32;
    fn from_f32(value: f32, max: f32) -> Self;
}

impl Sample for u8 {
    const ZERO_CHANGE_KEEPS_VALUE: bool = true;
    fn to_f32(self) -> f32 {
        f32::from(self)
    }
    fn from_f32(value: f32, max: f32) -> Self {
        value.round().clamp(0.0, max) as u8
    }
}

impl Sample for u16 {
    const ZERO_CHANGE_KEEPS_VALUE: bool = true;
    fn to_f32(self) -> f32 {
        f32::from(self)
    }
    fn from_f32(value: f32, max: f32) -> Self {
        value.round().clamp(0.0, max) as u16
    }
}

impl Sample for f32 {
    const ZERO_CHANGE_KEEPS_VALUE: bool = false;
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

/// Positions one block of the blur passes works on at a time.
const LANES: usize = 16;

/// `out[x] = Σ_k weights[k] · src[x + k]`, summed in tap order from −0.0 as
/// the iterator sum of the reference does, so every position is bit-identical
/// to it. Taps outermost and positions innermost in fixed blocks, so the
/// arithmetic runs across positions in vector registers.
#[inline(always)]
fn blur_run<'a>(out: &mut [f32], weights: &[f32], tap: impl Fn(usize) -> &'a [f32]) {
    let (blocks, tail) = out.as_chunks_mut::<LANES>();
    for (b, block) in blocks.iter_mut().enumerate() {
        let x0 = b * LANES;
        let mut acc = [-0.0f32; LANES];
        for (k, &w) in weights.iter().enumerate() {
            let src: &[f32; LANES] = tap(k)[x0..x0 + LANES].try_into().unwrap();
            for i in 0..LANES {
                acc[i] += w * src[i];
            }
        }
        *block = acc;
    }
    let x0 = blocks.len() * LANES;
    for (i, value) in tail.iter_mut().enumerate() {
        let mut acc = -0.0f32;
        for (k, &w) in weights.iter().enumerate() {
            acc += w * tap(k)[x0 + i];
        }
        *value = acc;
    }
}

fn sharpen<T: Sample>(
    data: &mut [T],
    channels: usize,
    width: usize,
    height: usize,
    max: f32,
    settings: &OutputSharpening,
) {
    match channels {
        3 => sharpen_px::<T, 3>(data, width, height, max, settings),
        4 => sharpen_px::<T, 4>(data, width, height, max, settings),
        _ => unreachable!("output sharpening takes RGB or RGBA"),
    }
}

/// The unsharp mask of `reference::sharpen`, rewritten for speed with the
/// same arithmetic per pixel: the lightness is blurred horizontally into a
/// plane (edges repeat the outermost pixel), then vertically per output row,
/// and the detail is added back to every colour channel.
fn sharpen_px<T: Sample, const C: usize>(
    data: &mut [T],
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
    let stride = width * C;
    // Horizontal pass of the lightness into a row padded by `reach` repeated
    // edge values on both sides, so every tap reads in bounds unclamped.
    let mut horizontal = vec![0.0f32; width * height];
    horizontal
        .par_chunks_mut(width)
        .zip(data.par_chunks(stride))
        .for_each_init(Vec::new, |padded: &mut Vec<f32>, (out, row)| {
            padded.clear();
            padded.resize(width + 2 * reach, 0.0);
            let (pixels, _) = row.as_chunks::<C>();
            for (value, px) in padded[reach..reach + width].iter_mut().zip(pixels) {
                *value = lightness(px, scale);
            }
            let (first, last) = (padded[reach], padded[reach + width - 1]);
            padded[..reach].fill(first);
            padded[reach + width..].fill(last);
            blur_run(out, &weights, |k| &padded[k..]);
        });
    let amount = settings.amount / 100.0;
    let threshold = settings.threshold / 255.0;
    let horizontal = &horizontal;
    data.par_chunks_mut(stride).enumerate().for_each_init(
        || (Vec::new(), Vec::new()),
        |(blurred, rows): &mut (Vec<f32>, Vec<usize>), (y, row)| {
            blurred.clear();
            blurred.resize(width, 0.0);
            rows.clear();
            rows.extend((0..weights.len()).map(|k| (y + k).saturating_sub(reach).min(height - 1)));
            blur_run(blurred, &weights, |k| {
                &horizontal[rows[k] * width..(rows[k] + 1) * width]
            });
            let (pixels, _) = row.as_chunks_mut::<C>();
            // The blurred lightness becomes the change of this row's pixels.
            for (px, delta) in pixels.iter().zip(blurred.iter_mut()) {
                let detail = lightness(px, scale) - *delta;
                // Below the threshold nothing changes; above it the full
                // detail returns over a short ramp (no hard step).
                let gate = if threshold > 0.0 {
                    ((detail.abs() - threshold) / threshold).clamp(0.0, 1.0)
                } else {
                    1.0
                };
                *delta = amount * detail * gate * max;
            }
            if T::ZERO_CHANGE_KEEPS_VALUE {
                // Branch-free, so it vectorises; a zero change rounds back
                // to the same level.
                for (px, &delta) in pixels.iter_mut().zip(blurred.iter()) {
                    for value in px.iter_mut().take(3) {
                        *value = T::from_f32(value.to_f32() + delta, max);
                    }
                }
            } else {
                for (px, &delta) in pixels.iter_mut().zip(blurred.iter()) {
                    if delta != 0.0 {
                        for value in px.iter_mut().take(3) {
                            *value = T::from_f32(value.to_f32() + delta, max);
                        }
                    }
                }
            }
        },
    );
}

#[cfg(test)]
#[path = "output_sharpening_reference.rs"]
mod reference;

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

    /// A small deterministic generator (xorshift), so the tests need no
    /// extra crate.
    struct Noise(u64);

    impl Noise {
        fn next(&mut self) -> f32 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 >> 40) as f32 / (1u64 << 24) as f32
        }
    }

    /// A photo-like scene: smooth gradients, hard edges, fine texture and
    /// sensor-like noise, with `max` levels.
    fn scene(width: usize, height: usize, channels: usize, max: f32, seed: u64) -> Vec<f32> {
        let mut noise = Noise(seed | 1);
        let mut out = Vec::with_capacity(width * height * channels);
        for y in 0..height {
            for x in 0..width {
                let (fx, fy) = (x as f32 / width as f32, y as f32 / height as f32);
                let edge = if (x / 7 + y / 5) % 3 == 0 { 0.35 } else { 0.0 };
                let texture = 0.08 * ((x as f32 * 1.7).sin() * (y as f32 * 2.3).cos());
                for c in 0..channels {
                    let base = 0.2 + 0.4 * fx * (1.0 - fy) + 0.1 * c as f32 + edge + texture;
                    let v = (base + 0.06 * (noise.next() - 0.5)).clamp(0.0, 1.0);
                    out.push(v * max);
                }
            }
        }
        out
    }

    fn settings_cases() -> Vec<OutputSharpening> {
        let mut cases = vec![
            // The UT web and print recipes.
            OutputSharpening {
                amount: 100.0,
                radius: 0.8,
                threshold: 1.0,
            },
            OutputSharpening {
                amount: 100.0,
                radius: 1.7181,
                threshold: 1.0,
            },
            OutputSharpening {
                amount: 100.0,
                radius: 1.0,
                threshold: 0.0,
            },
        ];
        let mut noise = Noise(0x5eed);
        for _ in 0..10 {
            cases.push(OutputSharpening {
                amount: 1.0 + noise.next() * 499.0,
                radius: 0.1 + noise.next() * 9.9,
                threshold: if noise.next() < 0.3 {
                    0.0
                } else {
                    noise.next() * 50.0
                },
            });
        }
        cases.push(OutputSharpening {
            amount: 500.0,
            radius: 10.0,
            threshold: 50.0,
        });
        cases.push(OutputSharpening {
            amount: 300.0,
            radius: 0.1,
            threshold: 2.0,
        });
        cases
    }

    /// Sizes around the vector block (16), narrower than the kernel, and
    /// single rows and columns.
    const SIZES: [(usize, usize); 9] = [
        (1, 1),
        (1, 9),
        (9, 1),
        (5, 3),
        (15, 17),
        (16, 16),
        (17, 4),
        (61, 33),
        (130, 47),
    ];

    fn same_as_reference<T: Sample + PartialEq + std::fmt::Debug>(
        image: &[T],
        channels: usize,
        width: usize,
        height: usize,
        max: f32,
        settings: &OutputSharpening,
    ) {
        let mut fast = image.to_vec();
        let mut slow = image.to_vec();
        sharpen(&mut fast, channels, width, height, max, settings);
        reference::sharpen(&mut slow, channels, width, height, max, settings);
        let bits = |v: &[T]| v.iter().map(|x| x.to_f32().to_bits()).collect::<Vec<_>>();
        assert!(
            bits(&fast) == bits(&slow),
            "{width}x{height}x{channels} max {max} {settings:?}"
        );
    }

    #[test]
    fn matches_the_reference_bit_for_bit_on_random_scenes() {
        let mut seed = 1;
        for settings in settings_cases() {
            for (width, height) in SIZES {
                for channels in [3, 4] {
                    seed += 1;
                    let u8s: Vec<u8> = scene(width, height, channels, 255.0, seed)
                        .iter()
                        .map(|v| v.round() as u8)
                        .collect();
                    same_as_reference(&u8s, channels, width, height, 255.0, &settings);
                    let u16s: Vec<u16> = scene(width, height, channels, 65535.0, seed)
                        .iter()
                        .map(|v| v.round() as u16)
                        .collect();
                    same_as_reference(&u16s, channels, width, height, 65535.0, &settings);
                    let f32s = scene(width, height, channels, 1.0, seed);
                    same_as_reference(&f32s, channels, width, height, 1.0, &settings);
                }
            }
        }
    }

    /// Float exports can carry values the encoders never see: negatives,
    /// zeros of both signs, huge values, infinities and NaN.
    #[test]
    fn matches_the_reference_bit_for_bit_on_extreme_floats() {
        let specials = [
            0.0,
            -0.0,
            -1.0,
            1e30,
            -1e30,
            f32::INFINITY,
            f32::NEG_INFINITY,
            f32::NAN,
            f32::MIN_POSITIVE,
            1e-42,
        ];
        let (width, height) = (37, 23);
        for channels in [3, 4] {
            let mut image = scene(width, height, channels, 1.0, 99);
            for (i, v) in image.iter_mut().enumerate() {
                if i % 11 == 0 {
                    *v = specials[(i / 11) % specials.len()];
                }
            }
            for settings in settings_cases() {
                same_as_reference(&image, channels, width, height, 1.0, &settings);
            }
        }
    }

    /// A photo-sized frame with the UT print recipe: many blocks per row
    /// and rows split over every worker.
    #[test]
    fn matches_the_reference_bit_for_bit_at_photo_width() {
        let (width, height) = (1203, 211);
        let image: Vec<u16> = scene(width, height, 3, 65535.0, 7)
            .iter()
            .map(|v| v.round() as u16)
            .collect();
        for settings in &settings_cases()[..2] {
            same_as_reference(&image, 3, width, height, 65535.0, settings);
        }
    }

    #[test]
    fn print_radius_follows_distance_and_resolution() {
        assert!((print_radius(50.0, 300.0) - 1.72).abs() < 0.01);
        assert!((print_radius(100.0, 300.0) - 3.44).abs() < 0.02);
    }
}
