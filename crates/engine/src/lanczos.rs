//! Lanczos3 resizing (export renders, reduced display bases:
//! `image_processing::resample_f32_image`) with the arithmetic of the image
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
/// horizontal one. Results are clamped to `floor` ..= `ceiling` (the image
/// crate's: 0 and the sample type's maximum, 1.0 for floats).
fn resize_channels<S: Sample, const C: usize>(
    pixels: &[S],
    (width, height): (u32, u32),
    (new_width, new_height): (u32, u32),
    floor: f32,
    ceiling: f32,
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
                    let clamped = if sum < floor {
                        floor
                    } else if sum > ceiling {
                        ceiling
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
        0.0,
        <P::Subpixel as Sample>::MAX,
    );
    ImageBuffer::from_raw(new_width, new_height, pixels).expect("resized buffer size")
}

/// [`resize_exact`] of a scene-linear float image without the image crate's
/// clamp at 1.0: values above white (highlights a RAW decode keeps for
/// Exposure and Highlights to bring back) stay; negative ringing still
/// clamps to 0. The same taps and sums otherwise.
pub fn resize_scene_linear(
    image: &image::Rgb32FImage,
    new_width: u32,
    new_height: u32,
) -> image::Rgb32FImage {
    if (new_width, new_height) == image.dimensions() {
        return image.clone();
    }
    let pixels = resize_channels::<f32, 3>(
        image.as_raw(),
        image.dimensions(),
        (new_width, new_height),
        0.0,
        f32::INFINITY,
    );
    ImageBuffer::from_raw(new_width, new_height, pixels).expect("resized buffer size")
}

/// [`resize_scene_linear`] of a signed float image (a difference of two
/// images), without any clamp: the same taps and sums, so the resized
/// difference of two images is the difference of their resized copies
/// (before [`resize_scene_linear`]'s clamp at 0).
pub fn resize_signed(
    image: &image::Rgb32FImage,
    new_width: u32,
    new_height: u32,
) -> image::Rgb32FImage {
    if (new_width, new_height) == image.dimensions() {
        return image.clone();
    }
    let pixels = resize_channels::<f32, 3>(
        image.as_raw(),
        image.dimensions(),
        (new_width, new_height),
        f32::NEG_INFINITY,
        f32::INFINITY,
    );
    ImageBuffer::from_raw(new_width, new_height, pixels).expect("resized buffer size")
}

/// The value a reduced copy keeps of a signed Lanczos3 `sum` whose taps'
/// lowest source value is `lowest` (the rule of the geometry warp's
/// Catmull-Rom sample since `renderVersion` 2, `GeometryParams::keep_negative`):
/// it may go down to `lowest` where that is negative (a colour beyond the
/// working space's primaries keeps its negative channel), else it stops at 0
/// (the filter's ringing beside a bright edge is cut, as
/// [`resize_scene_linear`] cuts it). NaN becomes 0.
#[inline]
fn wide_gamut_value(sum: f32, lowest: f32) -> f32 {
    if lowest < 0.0 {
        if sum < lowest {
            lowest
        } else if sum.is_nan() {
            0.0
        } else {
            sum
        }
    } else {
        // `resize_scene_linear`'s clamp, then `max(0.0)` (NaN to 0), so
        // pixels without negative taps keep the clamped resize's bits.
        (if sum < 0.0 { 0.0 } else { sum }).max(0.0)
    }
}

/// Per output position, the lowest of each channel over the source
/// positions its Lanczos3 taps read (the taps' rectangle: the lowest of the
/// rows' lowest).
fn tap_lowest(image: &image::Rgb32FImage, new_width: u32, new_height: u32) -> Vec<f32> {
    let (width, height) = image.dimensions();
    let row = width as usize * 3;
    let pixels = image.as_raw();
    let rows = taps(height, new_height);
    let columns = taps(width, new_width);
    let new_row = new_width as usize * 3;
    let mut lowest = vec![0.0f32; new_row * new_height as usize];
    lowest
        .par_chunks_mut(new_row)
        .zip(rows.par_iter())
        .for_each(|(out, taps)| {
            let mut vertical = vec![f32::INFINITY; row];
            for y in taps.first..taps.first + taps.weights.len() {
                for (low, &value) in vertical.iter_mut().zip(&pixels[y * row..(y + 1) * row]) {
                    *low = low.min(value);
                }
            }
            horizontal_lowest(&vertical, &columns, out);
        });
    lowest
}

/// The lowest of each channel of `vertical` (one row, RGB) over each
/// output pixel's column taps, into `out`.
fn horizontal_lowest(vertical: &[f32], columns: &[Taps], out: &mut [f32]) {
    for (pixel, taps) in out.as_chunks_mut::<3>().0.iter_mut().zip(columns) {
        let mut low = [f32::INFINITY; 3];
        for x in taps.first..taps.first + taps.weights.len() {
            for (low, &value) in low.iter_mut().zip(&vertical[x * 3..x * 3 + 3]) {
                *low = low.min(value);
            }
        }
        *pixel = low;
    }
}

/// [`resize_signed`] for the reduced copies the display renders instead of
/// the photograph (`image_processing::resample_f32_image`): each value
/// floored at its taps' lowest where that is negative, else at 0
/// (`wide_gamut_value`), so colours beyond sRGB keep their negative
/// channels as the export's full-resolution develop does, and the filter's
/// ringing is cut at 0 as [`resize_scene_linear`] cuts it. An image without
/// negative values gets [`resize_scene_linear`]'s values with NaN as 0.
pub fn resize_wide_gamut(
    image: &image::Rgb32FImage,
    new_width: u32,
    new_height: u32,
) -> image::Rgb32FImage {
    if !image.as_raw().par_iter().any(|v| *v < 0.0) {
        let mut out = resize_scene_linear(image, new_width, new_height);
        out.iter_mut().for_each(|v| *v = wide_gamut_value(*v, 0.0));
        return out;
    }
    let mut out = resize_signed(image, new_width, new_height);
    if (new_width, new_height) == image.dimensions() {
        out.iter_mut().for_each(|v| *v = wide_gamut_value(*v, *v));
        return out;
    }
    let lowest = tap_lowest(image, new_width, new_height);
    out.iter_mut()
        .zip(&lowest)
        .for_each(|(v, &low)| *v = wide_gamut_value(*v, low));
    out
}

/// [`resize_signed`] of an RGB image that arrives in bands of whole rows,
/// top to bottom (a GPU pass read back band by band): the same taps and
/// the same sums in the same order, so the same values, while only the
/// output rows whose taps are still arriving are held (a few rows of the
/// source's width) instead of the whole source.
pub struct SignedRowStream {
    width: usize,
    new_width: u32,
    new_height: u32,
    rows: Vec<Taps>,
    columns: Vec<Taps>,
    /// The next source row expected.
    next: usize,
    /// The source's height.
    source_height: usize,
    /// The first output row not yet finished.
    first_open: usize,
    /// Vertical sums of the output rows being built, by output row.
    open: Vec<Option<Vec<f32>>>,
    /// With [`SignedRowStream::wide_gamut`]: the lowest value of each
    /// column over the rows read so far, by output row (as `open`).
    open_lowest: Option<Vec<Option<Vec<f32>>>>,
    out: Vec<f32>,
}

impl SignedRowStream {
    pub fn new(width: u32, height: u32, new_width: u32, new_height: u32) -> Self {
        Self {
            width: width as usize,
            new_width,
            new_height,
            rows: taps(height, new_height),
            columns: taps(width, new_width),
            next: 0,
            source_height: height as usize,
            first_open: 0,
            open: (0..new_height).map(|_| None).collect(),
            open_lowest: None,
            out: vec![0.0; new_width as usize * new_height as usize * 3],
        }
    }

    /// The stream of [`resize_wide_gamut`]'s values instead of
    /// [`resize_signed`]'s: the same sums, each floored at its taps' lowest
    /// where that is negative, else at 0.
    pub fn wide_gamut(width: u32, height: u32, new_width: u32, new_height: u32) -> Self {
        let mut stream = Self::new(width, height, new_width, new_height);
        stream.open_lowest = Some((0..new_height).map(|_| None).collect());
        stream
    }

    /// Adds the next whole rows (RGB, `width` pixels each).
    pub fn push_rows(&mut self, band: &[f32]) {
        let row = self.width * 3;
        assert!(row > 0 && band.len().is_multiple_of(row), "whole rows");
        let start = self.next;
        let end = start + band.len() / row;
        self.next = end;
        // As `resize_signed`: the same size is a copy.
        if self.width == self.new_width as usize && self.rows.len() == self.source_height {
            let out = &mut self.out[start * row..end * row];
            out.copy_from_slice(band);
            if self.open_lowest.is_some() {
                out.iter_mut().for_each(|v| *v = wide_gamut_value(*v, *v));
            }
            self.first_open = end;
            return;
        }
        // Output rows reading any of rows start..end (their taps are
        // contiguous and their first rows rise).
        let mut last = self.first_open;
        while last < self.rows.len() && self.rows[last].first < end {
            last += 1;
        }
        let touched = self.first_open..last;
        let rows = &self.rows;
        let columns = &self.columns;
        let new_row = self.new_width as usize * 3;
        let open = &mut self.open[touched.start..touched.end];
        let mut no_lowest: Vec<Option<Vec<f32>>> = Vec::new();
        let wide_gamut = self.open_lowest.is_some();
        let open_lowest = match self.open_lowest.as_mut() {
            Some(lowest) => &mut lowest[touched.start..touched.end],
            None => {
                no_lowest.resize_with(touched.len(), || None);
                &mut no_lowest[..]
            }
        };
        let out = &mut self.out[touched.start * new_row..];
        open.par_iter_mut()
            .zip(open_lowest.par_iter_mut())
            .zip(out.par_chunks_mut(new_row))
            .zip(touched.clone().into_par_iter())
            .for_each(|(((sums, lowest), out), j)| {
                let taps = &rows[j];
                let sums = sums.get_or_insert_with(|| vec![0.0; row]);
                let from = taps.first.max(start);
                let to = (taps.first + taps.weights.len()).min(end);
                for y in from..to {
                    let weight = taps.weights[y - taps.first];
                    let source = &band[(y - start) * row..(y - start + 1) * row];
                    for (sum, &value) in sums.iter_mut().zip(source) {
                        *sum += value * weight;
                    }
                }
                if wide_gamut {
                    let lowest = lowest.get_or_insert_with(|| vec![f32::INFINITY; row]);
                    for y in from..to {
                        let source = &band[(y - start) * row..(y - start + 1) * row];
                        for (low, &value) in lowest.iter_mut().zip(source) {
                            *low = low.min(value);
                        }
                    }
                }
                if taps.first + taps.weights.len() <= end {
                    for (pixel, taps) in out.as_chunks_mut::<3>().0.iter_mut().zip(columns) {
                        let mut sum = [0.0f32; 3];
                        for (i, &weight) in taps.weights.iter().enumerate() {
                            let start = (taps.first + i) * 3;
                            for (sum, &value) in sum.iter_mut().zip(&sums[start..start + 3]) {
                                *sum += value * weight;
                            }
                        }
                        *pixel = sum;
                    }
                    if let Some(vertical) = lowest.take() {
                        let mut floors = vec![0.0f32; new_row];
                        horizontal_lowest(&vertical, columns, &mut floors);
                        for (v, &low) in out[..new_row].iter_mut().zip(&floors) {
                            *v = wide_gamut_value(*v, low);
                        }
                    }
                    *sums = Vec::new();
                }
            });
        while self.first_open < last
            && self.rows[self.first_open].first + self.rows[self.first_open].weights.len() <= end
        {
            self.open[self.first_open] = None;
            if let Some(lowest) = self.open_lowest.as_mut() {
                lowest[self.first_open] = None;
            }
            self.first_open += 1;
        }
    }

    /// The resized image once every source row came; None before.
    pub fn finish(self) -> Option<image::Rgb32FImage> {
        if self.first_open < self.rows.len() {
            return None;
        }
        ImageBuffer::from_raw(self.new_width, self.new_height, self.out)
    }
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

    #[test]
    fn signed_row_stream_is_resize_signed() {
        for (w, h, nw, nh, band) in [
            (97, 61, 40, 25, 7),
            (64, 48, 64, 20, 1),
            (50, 50, 23, 37, 50),
            (31, 90, 31, 90, 16),
        ] {
            let values: Vec<f32> = pattern(w, h, 7)
                .chunks(4)
                .flat_map(|p| [p[0] - 0.5, p[1], -p[2]])
                .collect();
            let image = image::Rgb32FImage::from_raw(w, h, values.clone()).unwrap();
            let expected = resize_signed(&image, nw, nh);
            let mut stream = SignedRowStream::new(w, h, nw, nh);
            for rows in values.chunks(w as usize * 3 * band) {
                stream.push_rows(rows);
            }
            let got = stream.finish().unwrap();
            assert_eq!(
                got.as_raw().iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
                expected
                    .as_raw()
                    .iter()
                    .map(|v| v.to_bits())
                    .collect::<Vec<_>>(),
                "{w}x{h} -> {nw}x{nh}"
            );
            let expected = resize_wide_gamut(&image, nw, nh);
            let mut stream = SignedRowStream::wide_gamut(w, h, nw, nh);
            for rows in values.chunks(w as usize * 3 * band) {
                stream.push_rows(rows);
            }
            let got = stream.finish().unwrap();
            assert_eq!(
                got.as_raw().iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
                expected
                    .as_raw()
                    .iter()
                    .map(|v| v.to_bits())
                    .collect::<Vec<_>>(),
                "wide gamut {w}x{h} -> {nw}x{nh}"
            );
        }
    }

    /// A saturated blue beyond sRGB (linear sRGB, red below 0) beside a
    /// bright white edge on black.
    fn wide_gamut_scene(width: u32, height: u32) -> image::Rgb32FImage {
        image::Rgb32FImage::from_fn(width, height, |x, _| {
            if x < width / 3 {
                image::Rgb([-0.16, 0.087, 1.24])
            } else if x < width * 2 / 3 {
                image::Rgb([0.0, 0.0, 0.0])
            } else {
                image::Rgb([4.0, 4.0, 4.0])
            }
        })
    }

    #[test]
    fn wide_gamut_resize_keeps_negative_colours_and_cuts_ringing() {
        let (w, h) = (300, 40);
        let scene = wide_gamut_scene(w, h);
        let reduced = resize_wide_gamut(&scene, 100, 13);
        let signed = resize_signed(&scene, 100, 13);
        for y in 0..13 {
            for x in 0..100 {
                let p = reduced.get_pixel(x, y).0;
                let s = signed.get_pixel(x, y).0;
                // Inside the blue patch: its own colour, negative red kept.
                if x < 30 {
                    for c in 0..3 {
                        assert!((p[c] - s[c]).abs() < 1e-6, "({x},{y}) {p:?} vs {s:?}");
                    }
                    assert!((p[0] + 0.16).abs() < 1e-4, "({x},{y}) {p:?}");
                }
                // Never below the lowest source value (the blue's red).
                assert!(p.iter().all(|v| *v >= -0.16), "({x},{y}) {p:?}");
                // The black beside the white edge: ringing cut at 0.
                if (40..66).contains(&x) {
                    assert!(p.iter().all(|v| *v >= 0.0), "({x},{y}) {p:?}");
                }
            }
        }
        assert!(
            signed
                .pixels()
                .skip(40)
                .take(26)
                .any(|p| p.0.iter().any(|v| *v < 0.0)),
            "the scene rings below 0 beside the edge"
        );
    }

    #[test]
    fn wide_gamut_resize_of_an_image_without_negatives_is_the_clamped_resize() {
        let values: Vec<f32> = pattern(97, 61, 3)
            .chunks(4)
            .flat_map(|p| [p[0] * 3.0, p[1], p[2]])
            .collect();
        let image = image::Rgb32FImage::from_raw(97, 61, values).unwrap();
        for (nw, nh) in [(40, 25), (97, 61), (13, 50)] {
            let mut expected = resize_scene_linear(&image, nw, nh);
            expected.iter_mut().for_each(|v| *v = v.max(0.0));
            let got = resize_wide_gamut(&image, nw, nh);
            assert!(
                got.as_raw()
                    .iter()
                    .zip(expected.as_raw())
                    .all(|(a, b)| a.to_bits() == b.to_bits()),
                "{nw}x{nh}"
            );
        }
    }
}
