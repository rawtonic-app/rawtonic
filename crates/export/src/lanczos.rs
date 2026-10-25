//! Lanczos3 resizing of export renders with the arithmetic of the image
//! crate's `imageops::resize` (the same bytes), its rows in parallel.
//!
//! The image crate resizes on one thread and reads every tap through
//! `get_pixel`: resizing a 33–61 MP render to 2048 px took 0.2–0.5 s of each
//! photo's export (notes: parity-gaps/G43, task BX3). Here each output row
//! is its own job and the vertical pass adds whole source rows, but every
//! output value is still the sum of the same products in the same order.

use image::{DynamicImage, ImageBuffer, Pixel};
use rayon::prelude::*;

/// The source positions an output position reads and their weights.
struct Taps {
    first: usize,
    weights: Vec<f32>,
}

fn sinc(t: f32) -> f32 {
    let a = t * std::f32::consts::PI;
    if t == 0.0 { 1.0 } else { a.sin() / a }
}

fn lanczos3(x: f32) -> f32 {
    if x.abs() < 3.0 {
        sinc(x) * sinc(x / 3.0)
    } else {
        0.0
    }
}

/// The taps of each of `to` positions resampled from `from` (the image
/// crate's `vertical_sample`/`horizontal_sample` weights).
fn taps(from: u32, to: u32) -> Vec<Taps> {
    let ratio = from as f32 / to as f32;
    let sratio = if ratio < 1.0 { 1.0 } else { ratio };
    let support = 3.0 * sratio;
    (0..to)
        .map(|out| {
            let input = (out as f32 + 0.5) * ratio;
            let left = ((input - support).floor() as i64).clamp(0, i64::from(from) - 1);
            let right = ((input + support).ceil() as i64).clamp(left + 1, i64::from(from));
            let input = input - 0.5;
            let mut weights = Vec::with_capacity((right - left) as usize);
            let mut sum = 0.0;
            for i in left as u32..right as u32 {
                let w = lanczos3((i as f32 - input) / sratio);
                weights.push(w);
                sum += w;
            }
            for w in &mut weights {
                *w /= sum;
            }
            Taps {
                first: left as usize,
                weights,
            }
        })
        .collect()
}

/// A channel value: read as `f32`, written back clamped to its range and
/// rounded (floats unrounded), as the image crate does.
trait Sample: Copy + Send + Sync + 'static {
    const MAX: f32;
    fn to_f32(self) -> f32;
    fn from_clamped(value: f32) -> Self;
}

impl Sample for u8 {
    const MAX: f32 = u8::MAX as f32;
    fn to_f32(self) -> f32 {
        f32::from(self)
    }
    fn from_clamped(value: f32) -> Self {
        value.round() as u8
    }
}

impl Sample for u16 {
    const MAX: f32 = u16::MAX as f32;
    fn to_f32(self) -> f32 {
        f32::from(self)
    }
    fn from_clamped(value: f32) -> Self {
        value.round() as u16
    }
}

impl Sample for f32 {
    const MAX: f32 = 1.0;
    fn to_f32(self) -> f32 {
        self
    }
    fn from_clamped(value: f32) -> Self {
        value
    }
}

/// `image::imageops::resize` with `FilterType::Lanczos3` of an image of
/// `C` interleaved channels: the vertical pass first, into `f32`, then the
/// horizontal one.
fn resize_channels<S: Sample, const C: usize>(
    pixels: &[S],
    (width, height): (u32, u32),
    (new_width, new_height): (u32, u32),
) -> Vec<S> {
    let row = width as usize * C;
    let rows = taps(height, new_height);
    let mut vertical = vec![0.0f32; row * new_height as usize];
    vertical
        .par_chunks_mut(row)
        .zip(rows.par_iter())
        .for_each(|(out, taps)| {
            for (i, &weight) in taps.weights.iter().enumerate() {
                let start = (taps.first + i) * row;
                for (sum, &value) in out.iter_mut().zip(&pixels[start..start + row]) {
                    *sum += value.to_f32() * weight;
                }
            }
        });

    let columns = taps(width, new_width);
    let new_row = new_width as usize * C;
    let mut resized = vec![S::from_clamped(0.0); new_row * new_height as usize];
    resized
        .par_chunks_mut(new_row)
        .zip(vertical.par_chunks(row))
        .for_each(|(out, line)| {
            for (pixel, taps) in out.as_chunks_mut::<C>().0.iter_mut().zip(&columns) {
                let mut sum = [0.0f32; C];
                for (i, &weight) in taps.weights.iter().enumerate() {
                    let start = (taps.first + i) * C;
                    for (sum, &value) in sum.iter_mut().zip(&line[start..start + C]) {
                        *sum += value * weight;
                    }
                }
                for (value, sum) in pixel.iter_mut().zip(sum) {
                    // The image crate's `clamp`: NaN passes.
                    let clamped = if sum < 0.0 {
                        0.0
                    } else if sum > S::MAX {
                        S::MAX
                    } else {
                        sum
                    };
                    *value = S::from_clamped(clamped);
                }
            }
        });
    resized
}

fn resize_buffer<P, const C: usize>(
    image: &ImageBuffer<P, Vec<P::Subpixel>>,
    new_width: u32,
    new_height: u32,
) -> ImageBuffer<P, Vec<P::Subpixel>>
where
    P: Pixel,
    P::Subpixel: Sample,
{
    let pixels = resize_channels::<P::Subpixel, C>(
        image.as_raw(),
        image.dimensions(),
        (new_width, new_height),
    );
    ImageBuffer::from_raw(new_width, new_height, pixels).expect("resized buffer size")
}

/// `image.resize_exact(new_width, new_height, FilterType::Lanczos3)`, the
/// same pixels, in the current rayon pool.
pub fn resize_exact(image: &DynamicImage, new_width: u32, new_height: u32) -> DynamicImage {
    let (width, height) = (image.width(), image.height());
    if width == 0 || height == 0 || (new_width, new_height) == (width, height) {
        return image.resize_exact(new_width, new_height, image::imageops::FilterType::Lanczos3);
    }
    match image {
        DynamicImage::ImageRgb8(buffer) => {
            DynamicImage::ImageRgb8(resize_buffer::<_, 3>(buffer, new_width, new_height))
        }
        DynamicImage::ImageRgba8(buffer) => {
            DynamicImage::ImageRgba8(resize_buffer::<_, 4>(buffer, new_width, new_height))
        }
        DynamicImage::ImageRgb16(buffer) => {
            DynamicImage::ImageRgb16(resize_buffer::<_, 3>(buffer, new_width, new_height))
        }
        DynamicImage::ImageRgba16(buffer) => {
            DynamicImage::ImageRgba16(resize_buffer::<_, 4>(buffer, new_width, new_height))
        }
        DynamicImage::ImageRgb32F(buffer) => {
            DynamicImage::ImageRgb32F(resize_buffer::<_, 3>(buffer, new_width, new_height))
        }
        DynamicImage::ImageRgba32F(buffer) => {
            DynamicImage::ImageRgba32F(resize_buffer::<_, 4>(buffer, new_width, new_height))
        }
        other => other.resize_exact(new_width, new_height, image::imageops::FilterType::Lanczos3),
    }
}

/// `image.resize(new_width, new_height, FilterType::Lanczos3)`: fitted
/// inside the box with the aspect ratio kept, the same pixels.
pub fn resize(image: &DynamicImage, new_width: u32, new_height: u32) -> DynamicImage {
    if (new_width, new_height) == (image.width(), image.height()) {
        return image.clone();
    }
    let (width, height) = fit_dimensions(image.width(), image.height(), new_width, new_height);
    resize_exact(image, width, height)
}

/// The image crate's `resize_dimensions` without fill.
fn fit_dimensions(width: u32, height: u32, new_width: u32, new_height: u32) -> (u32, u32) {
    let ratio = f64::min(
        f64::from(new_width) / f64::from(width),
        f64::from(new_height) / f64::from(height),
    );
    let fitted_width = ((f64::from(width) * ratio).round() as u64).max(1);
    let fitted_height = ((f64::from(height) * ratio).round() as u64).max(1);
    if fitted_width > u64::from(u32::MAX) {
        let ratio = f64::from(u32::MAX) / f64::from(width);
        (
            u32::MAX,
            ((f64::from(height) * ratio).round() as u32).max(1),
        )
    } else if fitted_height > u64::from(u32::MAX) {
        let ratio = f64::from(u32::MAX) / f64::from(height);
        (((f64::from(width) * ratio).round() as u32).max(1), u32::MAX)
    } else {
        (fitted_width as u32, fitted_height as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::imageops::FilterType;

    /// Deterministic noise with full-range values and smooth areas.
    fn pattern(width: u32, height: u32, seed: u64) -> Vec<f32> {
        let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        (0..width as usize * height as usize * 4)
            .map(|i| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                let noise = (state >> 40) as f32 / (1u64 << 24) as f32;
                let ramp = (i % 977) as f32 / 977.0;
                if i % 5 == 0 { ramp } else { noise }
            })
            .collect()
    }

    fn images(width: u32, height: u32, seed: u64) -> Vec<DynamicImage> {
        let values = pattern(width, height, seed);
        let rgba32 = image::Rgba32FImage::from_raw(width, height, values.clone()).unwrap();
        let rgba = DynamicImage::ImageRgba32F(rgba32);
        // Out of range floats, which the resize clamps.
        let wide = image::Rgb32FImage::from_raw(
            width,
            height,
            values
                .iter()
                .take(width as usize * height as usize * 3)
                .map(|v| v * 1.4 - 0.2)
                .collect(),
        )
        .unwrap();
        vec![
            DynamicImage::ImageRgba8(rgba.to_rgba8()),
            DynamicImage::ImageRgb8(rgba.to_rgb8()),
            DynamicImage::ImageRgba16(rgba.to_rgba16()),
            DynamicImage::ImageRgb16(rgba.to_rgb16()),
            DynamicImage::ImageRgb32F(wide),
            rgba,
        ]
    }

    #[test]
    fn resizes_to_the_image_crates_pixels() {
        let sizes = [
            ((997, 661), (256, 170)),
            ((640, 480), (2048, 1536)),
            ((501, 333), (499, 400)),
            ((300, 200), (1, 1)),
            ((7, 5), (3, 11)),
            ((1, 9), (4, 2)),
        ];
        for (seed, ((width, height), (new_width, new_height))) in sizes.into_iter().enumerate() {
            for image in images(width, height, seed as u64) {
                let expected = image.resize_exact(new_width, new_height, FilterType::Lanczos3);
                let actual = resize_exact(&image, new_width, new_height);
                assert_eq!(actual.color(), expected.color());
                assert_eq!(
                    (actual.width(), actual.height()),
                    (expected.width(), expected.height())
                );
                assert!(
                    actual.as_bytes() == expected.as_bytes(),
                    "{:?} {width}x{height} -> {new_width}x{new_height}",
                    image.color()
                );
            }
        }
    }

    #[test]
    fn resize_keeps_the_aspect_ratio_like_the_image_crate() {
        for (width, height, box_width, box_height) in [
            (997, 661, 300, 300),
            (333, 777, 100, 50),
            (640, 480, 640, 480),
            (90, 60, 2048, 9999),
        ] {
            for image in images(width, height, 9) {
                let expected = image.resize(box_width, box_height, FilterType::Lanczos3);
                let actual = resize(&image, box_width, box_height);
                assert_eq!(
                    (actual.width(), actual.height()),
                    (expected.width(), expected.height())
                );
                assert!(actual.as_bytes() == expected.as_bytes());
            }
        }
    }
}
