//! Analysis scopes of the shown frame: histogram, waveform, RGB parade and
//! vectorscope. Their input is the renderer's `analytics_output`
//! (shader.wgsl): the displayed frame's 8-bit values in the shown output
//! space, without the clipping warnings or the colour range view. Every
//! scope counts every pixel, so a clipped one-pixel column always shows.
//!
//! Luma and the colour differences use the shown space's own weights (its
//! Y row of RGB → XYZ: Rec.709 for sRGB, the P3, Adobe RGB or ProPhoto
//! primaries for those spaces), applied to the encoded values as video
//! scopes do (Y′ = Kr·R′ + Kg·G′ + Kb·B′, Cb = (B′ − Y′) / 2(1 − Kb),
//! Cr = (R′ − Y′) / 2(1 − Kr)).

use crate::output_color_space::OutputColorSpace;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use image::DynamicImage;
use rayon::prelude::*;
use serde::Serialize;
use std::borrow::Cow;

/// Width and height of the waveform, parade and vectorscope images.
pub const SCOPE_SIZE: usize = 256;
/// Width of each channel's strip in the parade, and the gap between them.
pub const PARADE_STRIP: usize = 82;
pub const PARADE_GAP: usize = 5;
/// Pixels per unit of Cb or Cr in the vectorscope: the most saturated
/// colours (|C| up to 0.61: ProPhoto green and magenta; Rec.709 green 0.60)
/// stay inside radius 123 of 128.
pub const VECTORSCOPE_SCALE: f32 = 200.0;
/// The skin line of Rec.709 component scopes, degrees counter-clockwise from
/// +Cb (where the light and dark skin patches of the usual test charts fall).
const SKIN_LINE_REC709_DEGREES: f64 = 123.0;

/// Whether some pixel clips each side of the tone range, and in which of
/// red, green and blue: the clipping warnings' own tests (`shader.wgsl`
/// `clipping_channels`: a channel below 0.002 or above 0.998 before the
/// output encoding, in the output or proofed space), whether a warning is on
/// or not.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ClippedSides {
    pub shadows: bool,
    pub highlights: bool,
    pub shadow_channels: [bool; 3],
    pub highlight_channels: [bool; 3],
}

impl ClippedSides {
    /// From the OR of every pixel's `analytics_output` alpha: bit 0
    /// shadows, bit 1 highlights, bits 2–4 the highlight channels, bits 5–7
    /// the shadow channels.
    pub fn from_marks(marks: u8) -> Self {
        Self {
            shadows: marks & 1 != 0,
            highlights: marks & 2 != 0,
            highlight_channels: std::array::from_fn(|c| marks & (4 << c) != 0),
            shadow_channels: std::array::from_fn(|c| marks & (32 << c) != 0),
        }
    }
}

/// Luma and colour-difference weights of the space the scopes read.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScopeSpace {
    pub kr: f64,
    pub kg: f64,
    pub kb: f64,
}

impl ScopeSpace {
    /// The weights of `space`: the Y row of its RGB → XYZ matrix.
    pub fn of(space: OutputColorSpace) -> Self {
        let [kr, kg, kb] = space.to_xyz()[1];
        Self { kr, kg, kb }
    }

    pub fn luma(&self, [r, g, b]: [f64; 3]) -> f64 {
        self.kr * r + self.kg * g + self.kb * b
    }

    /// (Cb, Cr) of encoded values in 0–1, each in −0.5…0.5.
    pub fn chroma(&self, rgb: [f64; 3]) -> (f64, f64) {
        let y = self.luma(rgb);
        (
            (rgb[2] - y) / (2.0 * (1.0 - self.kb)),
            (rgb[0] - y) / (2.0 * (1.0 - self.kr)),
        )
    }

    /// Encoded values of luma `y` and colour differences (cb, cr).
    fn rgb_of(&self, y: f64, cb: f64, cr: f64) -> [f64; 3] {
        let r = y + 2.0 * (1.0 - self.kr) * cr;
        let b = y + 2.0 * (1.0 - self.kb) * cb;
        let g = (y - self.kr * r - self.kb * b) / self.kg;
        [r, g, b]
    }
}

/// The 8-bit RGB of `image` and its channel count; floats are rounded as
/// an 8-bit store rounds them.
fn rgb8(image: &DynamicImage) -> (Cow<'_, [u8]>, usize) {
    match image {
        DynamicImage::ImageRgb8(rgb) => (Cow::Borrowed(rgb.as_raw().as_slice()), 3),
        DynamicImage::ImageRgba8(rgba) => (Cow::Borrowed(rgba.as_raw().as_slice()), 4),
        DynamicImage::ImageRgb32F(f) => (
            Cow::Owned(
                f.as_raw()
                    .iter()
                    .map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8)
                    .collect(),
            ),
            3,
        ),
        _ => (Cow::Owned(image.to_rgb8().into_raw()), 3),
    }
}

/// Per-code weight tables of one luma or colour-difference sum.
struct Weights([[f32; 256]; 3]);

impl Weights {
    fn new(k: [f64; 3]) -> Self {
        Self(std::array::from_fn(|c| {
            std::array::from_fn(|v| (k[c] * v as f64) as f32)
        }))
    }

    fn sum(&self, r: u8, g: u8, b: u8) -> f32 {
        self.0[0][r as usize] + self.0[1][g as usize] + self.0[2][b as usize]
    }
}

/// Lookup tables of a space: luma in codes, Cb and Cr in vectorscope pixels.
struct Tables {
    luma: Weights,
    cb: Weights,
    cr: Weights,
}

impl Tables {
    fn new(space: ScopeSpace) -> Self {
        let ScopeSpace { kr, kg, kb } = space;
        let cb_scale = VECTORSCOPE_SCALE as f64 / 255.0 / (2.0 * (1.0 - kb));
        let cr_scale = VECTORSCOPE_SCALE as f64 / 255.0 / (2.0 * (1.0 - kr));
        Self {
            luma: Weights::new([kr, kg, kb]),
            cb: Weights::new([-kr * cb_scale, -kg * cb_scale, (1.0 - kb) * cb_scale]),
            cr: Weights::new([(1.0 - kr) * cr_scale, -kg * cr_scale, -kb * cr_scale]),
        }
    }

    fn luma(&self, r: u8, g: u8, b: u8) -> usize {
        (self.luma.sum(r, g, b).round() as usize).min(255)
    }

    /// The vectorscope pixel of a colour (x right = +Cb, y down = −Cr), the
    /// nearest one: neutrals are always pixel (128, 128).
    fn vector_bin(&self, r: u8, g: u8, b: u8) -> usize {
        let mut cb = self.cb.sum(r, g, b);
        let mut cr = self.cr.sum(r, g, b);
        let radius_sq = cb * cb + cr * cr;
        if radius_sq > 127.0 * 127.0 {
            let scale = 127.0 / radius_sq.sqrt();
            cb *= scale;
            cr *= scale;
        }
        let half = (SCOPE_SIZE / 2) as f32;
        let x = (half + cb).round().clamp(0.0, 255.0) as usize;
        let y = (half - cr).round().clamp(0.0, 255.0) as usize;
        y * SCOPE_SIZE + x
    }
}

/// Counts per 8-bit code of each channel and of luma, over every pixel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistogramCounts {
    pub red: [u32; 256],
    pub green: [u32; 256],
    pub blue: [u32; 256],
    pub luma: [u32; 256],
}

impl HistogramCounts {
    fn zero() -> Self {
        Self {
            red: [0; 256],
            green: [0; 256],
            blue: [0; 256],
            luma: [0; 256],
        }
    }

    fn add(mut self, other: Self) -> Self {
        for i in 0..256 {
            self.red[i] += other.red[i];
            self.green[i] += other.green[i];
            self.blue[i] += other.blue[i];
            self.luma[i] += other.luma[i];
        }
        self
    }
}

pub fn histogram_counts(image: &DynamicImage, space: ScopeSpace) -> HistogramCounts {
    let (raw, channels) = rgb8(image);
    let tables = Tables::new(space);
    raw.par_chunks(65_536 * channels)
        .fold(HistogramCounts::zero, |mut acc, chunk| {
            for pixel in chunk.chunks_exact(channels) {
                let (r, g, b) = (pixel[0], pixel[1], pixel[2]);
                acc.red[r as usize] += 1;
                acc.green[g as usize] += 1;
                acc.blue[b as usize] += 1;
                acc.luma[tables.luma(r, g, b)] += 1;
            }
            acc
        })
        .reduce(HistogramCounts::zero, HistogramCounts::add)
}

/// A histogram's bins (smoothed, normalised counts per channel) and whether
/// each side of the tone range clips somewhere in the image.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct HistogramData {
    red: Vec<f32>,
    green: Vec<f32>,
    blue: Vec<f32>,
    luma: Vec<f32>,
    clipped_shadows: bool,
    clipped_highlights: bool,
    /// Which of red, green and blue clip at each side (the renderer's
    /// tests): the colour of a lit clipping triangle (Lightroom's channel
    /// colours).
    shadow_channels: [bool; 3],
    highlight_channels: [bool; 3],
}

/// The histogram of `image` in `space` and the clipping sides the renderer
/// found in it (`clipped`). The inner codes are smoothed for display; codes
/// 0 and 255 keep their own counts, so clipped pixels stay at the ends
/// instead of spreading inwards.
pub fn calculate_histogram_from_image(
    image: &DynamicImage,
    space: ScopeSpace,
    clipped: ClippedSides,
) -> Result<HistogramData, String> {
    let counts = histogram_counts(image, space);
    let display = |bins: [u32; 256]| {
        let mut bins: Vec<f32> = bins.into_iter().map(|c| c as f32).collect();
        smooth_inner(&mut bins, 2.0);
        normalize_histogram_range(&mut bins, 0.99);
        bins
    };
    Ok(HistogramData {
        red: display(counts.red),
        green: display(counts.green),
        blue: display(counts.blue),
        luma: display(counts.luma),
        clipped_shadows: clipped.shadows,
        clipped_highlights: clipped.highlights,
        shadow_channels: clipped.shadow_channels,
        highlight_channels: clipped.highlight_channels,
    })
}

/// Gaussian smoothing of bins 1–254 among themselves (edges clamped to them).
fn smooth_inner(histogram: &mut [f32], sigma: f32) {
    let len = histogram.len();
    if len < 3 {
        return;
    }
    let radius = (sigma * 3.0).ceil() as i32;
    let kernel: Vec<f32> = (-radius..=radius)
        .map(|x| (-(x * x) as f32 / (2.0 * sigma * sigma)).exp())
        .collect();
    let total: f32 = kernel.iter().sum();
    let original = histogram.to_owned();
    let (first, last) = (1i32, len as i32 - 2);
    for i in first..=last {
        histogram[i as usize] = kernel
            .iter()
            .zip(-radius..=radius)
            .map(|(k, offset)| original[(i + offset).clamp(first, last) as usize] * k)
            .sum::<f32>()
            / total;
    }
}

fn normalize_histogram_range(histogram: &mut [f32], percentile_clip: f32) {
    if histogram.is_empty() {
        return;
    }
    let mut sorted_data = histogram.to_owned();
    sorted_data.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let clip_index = ((sorted_data.len() - 1) as f32 * percentile_clip).round() as usize;
    // A tall bin or two (a clipped end) saturate at the top. When the
    // percentile is empty (only an end or a few codes filled, as when a
    // channel is blown out) the tallest bin sets the scale instead, so a
    // spike never disappears.
    let percentile = sorted_data[clip_index.min(sorted_data.len() - 1)];
    let max_val = if percentile > 1e-6 {
        percentile
    } else {
        sorted_data[sorted_data.len() - 1]
    };
    let scale = if max_val > 1e-6 { 1.0 / max_val } else { 0.0 };
    for value in histogram.iter_mut() {
        *value = (*value * scale).min(1.0);
    }
}

/// Which scopes a request wants: all of them, or one mode while dragging.
#[derive(Clone, Copy, Debug)]
struct Wanted {
    rgb: bool,
    luma: bool,
    parade: bool,
    vectorscope: bool,
}

impl Wanted {
    fn of(active_channel: Option<&str>) -> Self {
        let all = active_channel.is_none();
        Self {
            rgb: all || active_channel == Some("rgb"),
            luma: all || matches!(active_channel, Some("luma" | "rgb")),
            parade: all || active_channel == Some("parade"),
            vectorscope: all || active_channel == Some("vectorscope"),
        }
    }
}

/// Counts per scope pixel (`SCOPE_SIZE`², row-major). Waveforms: x is the
/// image column (scaled to 256), row 0 is code 255 (100 %), row 255 code 0.
/// The parade holds red, green and blue strips (`PARADE_STRIP` wide,
/// `PARADE_GAP` apart). The vectorscope's centre is neutral, +Cb to the
/// right, +Cr up, `VECTORSCOPE_SCALE` pixels per unit, a colour in its
/// nearest pixel (centres at whole coordinates). Empty when not wanted.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScopeCounts {
    pub red: Vec<u32>,
    pub green: Vec<u32>,
    pub blue: Vec<u32>,
    pub luma: Vec<u32>,
    pub parade: Vec<u32>,
    pub vectorscope: Vec<u32>,
}

impl ScopeCounts {
    fn zero(wanted: Wanted) -> Self {
        let bins = |on: bool| {
            if on {
                vec![0; SCOPE_SIZE * SCOPE_SIZE]
            } else {
                vec![]
            }
        };
        Self {
            red: bins(wanted.rgb),
            green: bins(wanted.rgb),
            blue: bins(wanted.rgb),
            luma: bins(wanted.luma),
            parade: bins(wanted.parade),
            vectorscope: bins(wanted.vectorscope),
        }
    }

    /// Waveforms counted column by column (`col * 256 + code`) in their
    /// rows (row 0 = code 255).
    fn transposed(mut self) -> Self {
        for bins in [
            &mut self.red,
            &mut self.green,
            &mut self.blue,
            &mut self.luma,
            &mut self.parade,
        ] {
            if bins.is_empty() {
                continue;
            }
            let columns = std::mem::take(bins);
            *bins = (0..SCOPE_SIZE * SCOPE_SIZE)
                .map(|i| columns[(i % SCOPE_SIZE) * SCOPE_SIZE + 255 - i / SCOPE_SIZE])
                .collect();
        }
        self
    }

    fn add(mut self, other: Self) -> Self {
        for (a, b) in [
            (&mut self.red, &other.red),
            (&mut self.green, &other.green),
            (&mut self.blue, &other.blue),
            (&mut self.luma, &other.luma),
            (&mut self.parade, &other.parade),
            (&mut self.vectorscope, &other.vectorscope),
        ] {
            a.iter_mut().zip(b).for_each(|(a, b)| *a += b);
        }
        self
    }
}

/// The waveform column of image column `x` of `width`.
pub fn waveform_column(x: usize, width: usize) -> usize {
    (x * SCOPE_SIZE / width).min(SCOPE_SIZE - 1)
}

/// The parade column of image column `x` of `width` in channel `c`'s strip.
pub fn parade_column(x: usize, width: usize, c: usize) -> usize {
    c * (PARADE_STRIP + PARADE_GAP) + (x * PARADE_STRIP / width).min(PARADE_STRIP - 1)
}

pub fn scope_counts(
    image: &DynamicImage,
    space: ScopeSpace,
    active_channel: Option<&str>,
) -> Result<ScopeCounts, String> {
    let wanted = Wanted::of(active_channel);
    let (width, height) = (image.width() as usize, image.height() as usize);
    if width == 0 || height == 0 {
        return Err("Image has zero dimensions.".to_string());
    }
    let (raw, channels) = rgb8(image);
    let tables = Tables::new(space);
    let columns: Vec<usize> = (0..width).map(|x| waveform_column(x, width)).collect();
    let parade: Vec<[usize; 3]> = (0..width)
        .map(|x| std::array::from_fn(|c| parade_column(x, width, c)))
        .collect();
    let stride = width * channels;
    // Two bands of rows per thread: each job zeroes and merges its own
    // tables (up to 1.5 MB), so more jobs cost more than they balance.
    let rows_per_job = height.div_ceil(2 * rayon::current_num_threads()).max(1);
    Ok(raw
        .par_chunks(stride * rows_per_job)
        .fold(
            || ScopeCounts::zero(wanted),
            |mut acc, rows| {
                for row in rows.chunks_exact(stride) {
                    for (x, pixel) in row.chunks_exact(channels).enumerate() {
                        let (r, g, b) = (pixel[0], pixel[1], pixel[2]);
                        // Counted column by column (`col * 256 + code`):
                        // neighbouring pixels share a column, so they stay
                        // in its 1 KB; `transposed` turns it into rows.
                        let col = columns[x] * SCOPE_SIZE;
                        if wanted.rgb {
                            acc.red[col + r as usize] += 1;
                            acc.green[col + g as usize] += 1;
                            acc.blue[col + b as usize] += 1;
                        }
                        if wanted.luma {
                            acc.luma[col + tables.luma(r, g, b)] += 1;
                        }
                        if wanted.parade {
                            let [pr, pg, pb] = parade[x];
                            acc.parade[pr * SCOPE_SIZE + r as usize] += 1;
                            acc.parade[pg * SCOPE_SIZE + g as usize] += 1;
                            acc.parade[pb * SCOPE_SIZE + b as usize] += 1;
                        }
                        if wanted.vectorscope {
                            acc.vectorscope[tables.vector_bin(r, g, b)] += 1;
                        }
                    }
                }
                acc
            },
        )
        .reduce(|| ScopeCounts::zero(wanted), ScopeCounts::add)
        .transposed())
}

/// A vectorscope target: where a primary or secondary at `percent` of full
/// signal lands, in scope pixels.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct VectorscopeTarget {
    pub name: &'static str,
    pub percent: u8,
    pub x: f32,
    pub y: f32,
}

/// The vectorscope position of encoded values in 0–1.
fn vector_point(space: ScopeSpace, rgb: [f64; 3]) -> (f32, f32) {
    let (cb, cr) = space.chroma(rgb);
    let half = (SCOPE_SIZE / 2) as f64;
    let scale = VECTORSCOPE_SCALE as f64;
    ((half + cb * scale) as f32, (half - cr * scale) as f32)
}

/// The 75 % and 100 % targets of red, yellow, green, cyan, blue, magenta.
pub fn vectorscope_targets(space: ScopeSpace) -> Vec<VectorscopeTarget> {
    const COLOURS: [(&str, [f64; 3]); 6] = [
        ("R", [1.0, 0.0, 0.0]),
        ("Yl", [1.0, 1.0, 0.0]),
        ("G", [0.0, 1.0, 0.0]),
        ("Cy", [0.0, 1.0, 1.0]),
        ("B", [0.0, 0.0, 1.0]),
        ("Mg", [1.0, 0.0, 1.0]),
    ];
    [75u8, 100]
        .into_iter()
        .flat_map(|percent| {
            COLOURS.map(|(name, rgb)| {
                let (x, y) = vector_point(space, rgb.map(|v| v * f64::from(percent) / 100.0));
                VectorscopeTarget {
                    name,
                    percent,
                    x,
                    y,
                }
            })
        })
        .collect()
}

/// The skin line's angle in `space`, degrees counter-clockwise from +Cb: the
/// hue of a colour on the Rec.709 skin line (123°), carried into `space`.
pub fn skin_line_degrees(space: OutputColorSpace) -> f64 {
    let srgb = ScopeSpace::of(OutputColorSpace::Srgb);
    let angle = SKIN_LINE_REC709_DEGREES.to_radians();
    let encoded = srgb.rgb_of(0.6, 0.1 * angle.cos(), 0.1 * angle.sin());
    let linear = encoded.map(|v| OutputColorSpace::Srgb.transfer().to_linear(v));
    let m = space.from_linear_srgb();
    let target: [f64; 3] = std::array::from_fn(|r| {
        let v: f64 = (0..3).map(|c| m[r][c] * linear[c]).sum();
        space.transfer().from_linear(v)
    });
    let (cb, cr) = ScopeSpace::of(space).chroma(target);
    cr.atan2(cb).to_degrees()
}

#[derive(serde::Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct WaveformData {
    pub rgb: String,
    pub luma: String,
    pub parade: String,
    pub vectorscope: String,
    pub width: u32,
    pub height: u32,
    /// Where the vectorscope's 75 % and 100 % targets are (scope pixels).
    pub vectorscope_targets: Vec<VectorscopeTarget>,
    /// The skin line, degrees counter-clockwise from +Cb.
    pub skin_line_degrees: f32,
}

/// Brightness per count: logarithmic, so sparse values stay visible.
fn log_scale(bins: &[u32]) -> impl Fn(u32) -> u8 + '_ {
    let max = bins.iter().copied().max().unwrap_or(0);
    let scale = if max > 0 {
        255.0 / (1.0 + max as f32).ln()
    } else {
        0.0
    };
    move |v| ((1.0 + v as f32).ln() * scale) as u8
}

/// The scopes of `image` in `space` as RGBA images (`SCOPE_SIZE`²), the
/// vectorscope's targets and skin line. `active_channel` limits the work to
/// one mode ("rgb" also computes luma).
pub fn calculate_waveform_from_image(
    image: &DynamicImage,
    space: OutputColorSpace,
    active_channel: Option<&str>,
) -> Result<WaveformData, String> {
    let weights = ScopeSpace::of(space);
    let counts = scope_counts(image, weights, active_channel)?;
    let pixels = SCOPE_SIZE * SCOPE_SIZE;
    let encode = |rgba: Vec<u8>| {
        if rgba.is_empty() {
            String::new()
        } else {
            BASE64.encode(&rgba)
        }
    };

    let mut rgb = vec![];
    if !counts.red.is_empty() {
        let (lr, lg, lb) = (
            log_scale(&counts.red),
            log_scale(&counts.green),
            log_scale(&counts.blue),
        );
        rgb = vec![0u8; pixels * 4];
        for i in 0..pixels {
            let c = [lr(counts.red[i]), lg(counts.green[i]), lb(counts.blue[i])];
            rgb[i * 4..i * 4 + 3].copy_from_slice(&c);
            rgb[i * 4 + 3] = c[0].max(c[1]).max(c[2]);
        }
    }

    let mut luma = vec![];
    if !counts.luma.is_empty() {
        let l = log_scale(&counts.luma);
        luma = vec![0u8; pixels * 4];
        for i in 0..pixels {
            if counts.luma[i] > 0 {
                luma[i * 4..i * 4 + 4].copy_from_slice(&[255, 255, 255, l(counts.luma[i])]);
            }
        }
    }

    let mut parade = vec![];
    if !counts.parade.is_empty() {
        let l = log_scale(&counts.parade);
        parade = vec![0u8; pixels * 4];
        for i in 0..pixels {
            if counts.parade[i] > 0 {
                let channel = (i % SCOPE_SIZE) / (PARADE_STRIP + PARADE_GAP);
                parade[i * 4 + channel] = 255;
                parade[i * 4 + 3] = l(counts.parade[i]);
            }
        }
    }

    let mut vector = vec![];
    if !counts.vectorscope.is_empty() {
        let l = log_scale(&counts.vectorscope);
        let half = (SCOPE_SIZE / 2) as f64;
        vector = vec![0u8; pixels * 4];
        for i in 0..pixels {
            if counts.vectorscope[i] > 0 {
                // Each point in the hue it stands for, at mid luma.
                let cb = ((i % SCOPE_SIZE) as f64 - half) / VECTORSCOPE_SCALE as f64;
                let cr = (half - (i / SCOPE_SIZE) as f64) / VECTORSCOPE_SCALE as f64;
                let colour = weights.rgb_of(0.5, cb, cr);
                for c in 0..3 {
                    vector[i * 4 + c] = (colour[c].clamp(0.0, 1.0) * 255.0).round() as u8;
                }
                vector[i * 4 + 3] = l(counts.vectorscope[i]);
            }
        }
    }

    Ok(WaveformData {
        rgb: encode(rgb),
        luma: encode(luma),
        parade: encode(parade),
        vectorscope: encode(vector),
        width: SCOPE_SIZE as u32,
        height: SCOPE_SIZE as u32,
        vectorscope_targets: vectorscope_targets(weights),
        skin_line_degrees: skin_line_degrees(space) as f32,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgb, RgbImage, RgbaImage};

    const SPACES: [OutputColorSpace; 4] = OutputColorSpace::ALL;

    /// Ground truth of a code's luma: the space's weights in f64, rounded.
    fn true_luma(space: ScopeSpace, [r, g, b]: [u8; 3]) -> usize {
        space
            .luma([r, g, b].map(f64::from))
            .round()
            .clamp(0.0, 255.0) as usize
    }

    /// A 16-bit-style test frame quantised to the 8-bit output codes the
    /// renderer stores: a grey ramp over all codes (rows 0–3), the six
    /// primaries and secondaries at 50 %, 75 % and 100 % (rows 4–9), a
    /// clipped one-pixel column at an odd x (x = 101) and a black one at
    /// x = 0.
    fn test_frame(width: u32) -> RgbImage {
        RgbImage::from_fn(width, 10, |x, y| {
            if x == 101 {
                return Rgb([255; 3]);
            }
            if x == 0 {
                return Rgb([0; 3]);
            }
            match y {
                0..=3 => Rgb([(x * 255 / (width - 1)) as u8; 3]),
                _ => {
                    let colours = [
                        [1, 0, 0],
                        [1, 1, 0],
                        [0, 1, 0],
                        [0, 1, 1],
                        [0, 0, 1],
                        [1, 0, 1],
                    ];
                    let level = [128u32, 191, 255][(x as usize * 3 / width as usize).min(2)];
                    Rgb(colours[y as usize - 4].map(|c| (c * level) as u8))
                }
            }
        })
    }

    #[test]
    fn luma_weights_are_each_spaces_own() {
        let srgb = ScopeSpace::of(OutputColorSpace::Srgb);
        // Rec.709 / sRGB: 0.2126, 0.7152, 0.0722.
        assert!((srgb.kr - 0.2126).abs() < 1e-4, "{srgb:?}");
        assert!((srgb.kg - 0.7152).abs() < 1e-4, "{srgb:?}");
        assert!((srgb.kb - 0.0722).abs() < 1e-4, "{srgb:?}");
        // Display P3 (SMPTE EG 432-1, D65): 0.2290, 0.6917, 0.0793.
        let p3 = ScopeSpace::of(OutputColorSpace::DisplayP3);
        assert!((p3.kr - 0.2290).abs() < 1e-4, "{p3:?}");
        assert!((p3.kg - 0.6917).abs() < 1e-4, "{p3:?}");
        assert!((p3.kb - 0.0793).abs() < 1e-4, "{p3:?}");
        for space in SPACES {
            let k = ScopeSpace::of(space);
            assert!((k.kr + k.kg + k.kb - 1.0).abs() < 1e-9, "{space:?}");
        }
    }

    #[test]
    fn histogram_counts_every_pixel_at_its_code() {
        for width in [256u32, 257, 600] {
            let frame = test_frame(width);
            for space in SPACES {
                let k = ScopeSpace::of(space);
                let mut truth = HistogramCounts::zero();
                for p in frame.pixels() {
                    truth.red[p[0] as usize] += 1;
                    truth.green[p[1] as usize] += 1;
                    truth.blue[p[2] as usize] += 1;
                    truth.luma[true_luma(k, p.0)] += 1;
                }
                for image in [
                    DynamicImage::ImageRgb8(frame.clone()),
                    DynamicImage::ImageRgba8(DynamicImage::ImageRgb8(frame.clone()).to_rgba8()),
                    DynamicImage::ImageRgb32F(DynamicImage::ImageRgb8(frame.clone()).to_rgb32f()),
                ] {
                    assert_eq!(histogram_counts(&image, k), truth, "{width} {space:?}");
                }
                // The clipped column (10 pixels) and the black one are all there.
                assert!(truth.red[255] >= 10 && truth.red[0] >= 10);
            }
        }
    }

    #[test]
    fn histogram_keeps_clipped_counts_at_the_ends() {
        // Mid grey with a clipped one-pixel column at an odd x.
        let frame = RgbImage::from_fn(400, 300, |x, _| {
            Rgb(if x == 101 { [255; 3] } else { [128; 3] })
        });
        let k = ScopeSpace::of(OutputColorSpace::Srgb);
        let data = serde_json::to_value(
            calculate_histogram_from_image(
                &DynamicImage::ImageRgb8(frame),
                k,
                ClippedSides::default(),
            )
            .unwrap(),
        )
        .unwrap();
        let red: Vec<f64> = serde_json::from_value(data["red"].clone()).unwrap();
        assert!(red[255] > 0.0, "the clipped column is lost");
        // Nothing of it spreads to 249–254.
        assert!(red[249..255].iter().all(|v| *v == 0.0), "{:?}", &red[249..]);
    }

    #[test]
    fn clipping_marks_carry_the_renderers_channels() {
        // Shadows in blue, highlights in red and green.
        let clipped = ClippedSides::from_marks(1 | 2 | (0b011 << 2) | (0b100 << 5));
        assert!(clipped.shadows && clipped.highlights);
        assert_eq!(clipped.highlight_channels, [true, true, false]);
        assert_eq!(clipped.shadow_channels, [false, false, true]);
        assert_eq!(ClippedSides::from_marks(0), ClippedSides::default());
        let data = serde_json::to_value(
            calculate_histogram_from_image(
                &DynamicImage::ImageRgb8(test_frame(64)),
                ScopeSpace::of(OutputColorSpace::Srgb),
                clipped,
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            data["highlightChannels"],
            serde_json::json!([true, true, false])
        );
        assert_eq!(
            data["shadowChannels"],
            serde_json::json!([false, false, true])
        );
    }

    #[test]
    fn a_blown_out_channel_keeps_its_spike() {
        // Red entirely at 255, green at both ends, blue in a few codes.
        let frame = RgbImage::from_fn(300, 200, |x, _| {
            Rgb([255, if x % 2 == 0 { 0 } else { 255 }, 100 + (x % 3) as u8])
        });
        let data = serde_json::to_value(
            calculate_histogram_from_image(
                &DynamicImage::ImageRgb8(frame),
                ScopeSpace::of(OutputColorSpace::Srgb),
                ClippedSides::default(),
            )
            .unwrap(),
        )
        .unwrap();
        let bins = |c: &str| -> Vec<f64> { serde_json::from_value(data[c].clone()).unwrap() };
        assert_eq!(bins("red")[255], 1.0);
        assert_eq!((bins("green")[0], bins("green")[255]), (1.0, 1.0));
        assert!(bins("blue")[100..103].iter().all(|v| *v > 0.0));
    }

    #[test]
    fn waveform_and_parade_put_each_pixel_at_its_column_and_value() {
        for width in [256u32, 257, 600] {
            let frame = test_frame(width);
            let image = DynamicImage::ImageRgb8(frame.clone());
            for space in SPACES {
                let k = ScopeSpace::of(space);
                let counts = scope_counts(&image, k, None).unwrap();
                let mut truth = ScopeCounts::zero(Wanted::of(None));
                let w = width as usize;
                for (x, _, p) in frame.enumerate_pixels() {
                    let x = x as usize;
                    // Column = the image column scaled to 256; row 0 = 100 %.
                    let col = x * 256 / w;
                    let at = |v: u8, col: usize| (255 - v as usize) * 256 + col;
                    truth.red[at(p[0], col)] += 1;
                    truth.green[at(p[1], col)] += 1;
                    truth.blue[at(p[2], col)] += 1;
                    truth.luma[at(true_luma(k, p.0) as u8, col)] += 1;
                    for c in 0..3 {
                        truth.parade[at(p[c], c * 87 + x * 82 / w)] += 1;
                    }
                    let (vx, vy) = vector_point(k, p.0.map(|v| f64::from(v) / 255.0));
                    truth.vectorscope[vy.round() as usize * 256 + vx.round() as usize] += 1;
                }
                assert_eq!(counts.red, truth.red, "{width} {space:?}");
                assert_eq!(counts.green, truth.green);
                assert_eq!(counts.blue, truth.blue);
                assert_eq!(counts.luma, truth.luma, "{width} {space:?}");
                assert_eq!(counts.parade, truth.parade, "{width} {space:?}");
                let moved: u32 = counts
                    .vectorscope
                    .iter()
                    .zip(&truth.vectorscope)
                    .map(|(a, b)| a.abs_diff(*b))
                    .sum();
                // f32 tables against f64: a point may land one pixel over.
                assert!(
                    moved <= 2 * (width * 10 / 100),
                    "{width} {space:?}: {moved}"
                );
                // The clipped column at 100 % in its own waveform column.
                let clipped_col = 101 * 256 / w;
                assert!(counts.luma[clipped_col] >= 10, "{width} {space:?}");
                // The parade strips stay apart.
                for row in 0..256 {
                    for gap in [82..87, 169..174] {
                        assert!(gap.clone().all(|x| counts.parade[row * 256 + x] == 0));
                    }
                }
            }
        }
    }

    #[test]
    fn vectorscope_targets_are_where_the_colours_land() {
        for space in SPACES {
            let k = ScopeSpace::of(space);
            let targets = vectorscope_targets(k);
            assert_eq!(targets.len(), 12);
            for t in &targets {
                // A patch of exactly the target's codes falls in its pixel.
                let level = (f32::from(t.percent) / 100.0 * 255.0).round() as u8;
                let rgb = match t.name {
                    "R" => [level, 0, 0],
                    "Yl" => [level, level, 0],
                    "G" => [0, level, 0],
                    "Cy" => [0, level, level],
                    "B" => [0, 0, level],
                    _ => [level, 0, level],
                };
                let patch = DynamicImage::ImageRgb8(RgbImage::from_pixel(4, 4, Rgb(rgb)));
                let counts = scope_counts(&patch, k, Some("vectorscope")).unwrap();
                let bin = counts.vectorscope.iter().position(|&c| c > 0).unwrap();
                let (bx, by) = ((bin % 256) as f32, (bin / 256) as f32);
                // 75 % codes round (191.25 → 191): within a pixel and a half.
                assert!(
                    (bx - t.x).abs() < 1.5 && (by - t.y).abs() < 1.5,
                    "{space:?} {t:?} at {bx},{by}"
                );
                // Inside the scope, never at its clamp radius.
                let r = ((t.x - 128.0).powi(2) + (t.y - 128.0).powi(2)).sqrt();
                assert!(r < 124.0, "{space:?} {t:?}: {r}");
            }
            // Colour bars order counter-clockwise from +Cb: B, Mg, R, Yl, G, Cy.
            let angle = |name: &str| {
                let t = targets
                    .iter()
                    .find(|t| t.name == name && t.percent == 100)
                    .unwrap();
                (128.0 - t.y)
                    .atan2(t.x - 128.0)
                    .to_degrees()
                    .rem_euclid(360.0)
            };
            let order: Vec<f32> = ["B", "Mg", "R", "Yl", "G", "Cy"].map(angle).to_vec();
            let start = order[0];
            let unwrapped: Vec<f32> = order
                .iter()
                .map(|a| (a - start).rem_euclid(360.0))
                .collect();
            assert!(
                unwrapped.windows(2).all(|w| w[0] < w[1]),
                "{space:?} {order:?}"
            );
        }
        // sRGB / Rec.709: red at 102.9°, the classic component position.
        let srgb = vectorscope_targets(ScopeSpace::of(OutputColorSpace::Srgb));
        let red = srgb
            .iter()
            .find(|t| t.name == "R" && t.percent == 100)
            .unwrap();
        let angle = (128.0 - red.y).atan2(red.x - 128.0).to_degrees();
        assert!((angle - 102.9).abs() < 0.2, "{angle}");
    }

    #[test]
    fn skin_line_is_123_degrees_in_srgb_and_follows_the_space() {
        assert!((skin_line_degrees(OutputColorSpace::Srgb) - 123.0).abs() < 1e-6);
        for space in SPACES {
            let a = skin_line_degrees(space);
            // Between the red and yellow targets in every space.
            let t = vectorscope_targets(ScopeSpace::of(space));
            let at = |name: &str| {
                let t = t
                    .iter()
                    .find(|t| t.name == name && t.percent == 100)
                    .unwrap();
                f64::from((128.0 - t.y).atan2(t.x - 128.0).to_degrees())
            };
            assert!(at("R") < a && a < at("Yl"), "{space:?}: {a}");
        }
    }

    #[test]
    fn one_mode_computes_only_that_scope() {
        let image = DynamicImage::ImageRgb8(test_frame(300));
        let k = ScopeSpace::of(OutputColorSpace::Srgb);
        let all = scope_counts(&image, k, None).unwrap();
        let luma = scope_counts(&image, k, Some("luma")).unwrap();
        assert!(luma.red.is_empty() && luma.parade.is_empty() && luma.vectorscope.is_empty());
        assert_eq!(luma.luma, all.luma);
        let vector = scope_counts(&image, k, Some("vectorscope")).unwrap();
        assert_eq!(vector.vectorscope, all.vectorscope);
        assert!(vector.luma.is_empty());
        let data =
            calculate_waveform_from_image(&image, OutputColorSpace::Srgb, Some("parade")).unwrap();
        assert!(data.rgb.is_empty() && !data.parade.is_empty());
    }

    /// Time per scope update (histogram + all scopes, as a settled frame
    /// asks; and one mode, as a drag asks) on frames of the preview's and
    /// the camera's sizes: `cargo test --release -p rawtonic-engine --lib
    /// scopes::tests::timing -- --ignored --nocapture`.
    #[test]
    #[ignore = "timing; run in release"]
    fn timing() {
        for (label, w, h) in [
            ("preview 2560x1707", 2560u32, 1707u32),
            ("preview 3840x2560", 3840, 2560),
            ("24 MP 6000x4000", 6000, 4000),
            ("45 MP 8256x5504", 8256, 5504),
        ] {
            let mut seed = 0x9e37_79b9u32;
            let rgba = RgbaImage::from_fn(w, h, |_, _| {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                image::Rgba(seed.to_le_bytes())
            });
            // Photo-like: smooth gradients with a little grain.
            let smooth = RgbaImage::from_fn(w, h, |x, y| {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                let n = (seed % 9) as f32 - 4.0;
                let fx = x as f32 / w as f32;
                let fy = y as f32 / h as f32;
                let v = |a: f32| (a * 255.0 + n).clamp(0.0, 255.0) as u8;
                image::Rgba([v(fx), v(0.5 * fx + 0.5 * fy), v(fy * fy), 255])
            });
            for (content, image) in [
                ("noise", DynamicImage::ImageRgba8(rgba.clone())),
                ("smooth", DynamicImage::ImageRgba8(smooth)),
            ] {
                let k = ScopeSpace::of(OutputColorSpace::DisplayP3);
                let time = |f: &dyn Fn()| {
                    f();
                    let runs = 5;
                    let start = std::time::Instant::now();
                    for _ in 0..runs {
                        f();
                    }
                    start.elapsed().as_secs_f64() * 1000.0 / runs as f64
                };
                let histogram = time(&|| {
                    calculate_histogram_from_image(&image, k, ClippedSides::default()).unwrap();
                });
                let all = time(&|| {
                    calculate_waveform_from_image(&image, OutputColorSpace::DisplayP3, None)
                        .unwrap();
                });
                let luma = time(&|| {
                    calculate_waveform_from_image(
                        &image,
                        OutputColorSpace::DisplayP3,
                        Some("luma"),
                    )
                    .unwrap();
                });
                let vector = time(&|| {
                    calculate_waveform_from_image(
                        &image,
                        OutputColorSpace::DisplayP3,
                        Some("vectorscope"),
                    )
                    .unwrap();
                });
                let counts = time(&|| {
                    scope_counts(&image, k, None).unwrap();
                });
                println!(
                    "{label} {content}: histogram {histogram:.1} ms, all scopes {all:.1} ms (counting {counts:.1}), luma {luma:.1} ms, vectorscope {vector:.1} ms ({} threads)",
                    rayon::current_num_threads()
                );
            }
        }
    }

    #[test]
    fn rgba_alpha_never_counts() {
        let frame = test_frame(257);
        let mut rgba = RgbaImage::new(257, 10);
        for (x, y, p) in frame.enumerate_pixels() {
            rgba.put_pixel(x, y, image::Rgba([p[0], p[1], p[2], ((x * 7) % 256) as u8]));
        }
        let k = ScopeSpace::of(OutputColorSpace::DisplayP3);
        let a = scope_counts(&DynamicImage::ImageRgba8(rgba), k, None).unwrap();
        let b = scope_counts(&DynamicImage::ImageRgb8(frame), k, None).unwrap();
        assert_eq!(a, b);
    }
}
