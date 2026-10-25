//! Film negatives: the Develop tool that turns a scanned (or camera-scanned)
//! negative into a positive without writing a new file.
//!
//! The pass runs on the decode before anything else that changes pixels
//! (`image_loader::prepare_render_base`: before retouching, reduction, the
//! geometry warp, the fringe pass, the blurs, noise reduction, white
//! balance, tone and curves, which all work on the positive), with the lens
//! vignetting corrected on the scan; the warp of the converted base
//! ([`converted_base_recipe`]) corrects none. Every path that builds a base
//! (previews, exports, thumbnails, masks, AI and Auto inputs) does this, so
//! the GPU display and the CPU-read exports render the same pixels.
//! Recipes without the key are not touched.
//!
//! Model (notes: parity-gaps/G64 "Negative"). A negative's density above
//! the film base, `D = log10(base / T)` per channel (`T` the transmitted
//! light, `base` that of the unexposed film), is close to linear in the
//! scene's log exposure over the useful range. Each channel is scaled so
//! that the frame's densest neutral area (`highlight`, density `Dh`) has the
//! same density in every channel; the film base is black for all of them.
//! Neutral greys between those two ends then stay neutral when the layers'
//! gammas are proportional, which per-channel Density balance corrects when
//! they are not. As a darkroom printer picks the paper grade that fits the
//! negative's density range onto the paper, the frame's range becomes a
//! fixed range of print exposure (`PRINT_STOPS`, times 2^(grade / 100)):
//! scans of the same film made with different scanner contrast give the
//! same positive. The positive is scene-linear:
//! `log2 E = S · (D / Dh − 1) + exposure + log2(HIGHLIGHT_LINEAR)`.

use std::borrow::Cow;

use image::{DynamicImage, Rgb32FImage};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The recipe key holding the tool's settings (an object, see
/// [`Negative`]); missing or `null` renders the photo as it is.
pub const KEY: &str = "negative";
/// The newest conversion: 1 (2026-10-25) as described above. Recipes with
/// a newer version than the build knows render without the conversion
/// rather than with a different one.
pub const VERSION: u64 = 1;

/// Stops of scene exposure between the film base (black) and the frame's
/// highlight at Print grade 0. A normally exposed negative holds about 1.0–
/// 1.3 density between the base and diffuse white at a gamma of about 0.6
/// (textbook figures): 5.5–7 stops, with middle grey about 0.4 density
/// below the highlight, so 6.5 puts middle grey near 0.18 (−2.3 EV from
/// `HIGHLIGHT_LINEAR`).
const PRINT_STOPS: f32 = 6.5;
/// Scene-linear level the frame's densest neutral area (its highlights)
/// becomes at Print exposure 0: diffuse white just under 1.0, so specular
/// highlights beyond it still have headroom in the tone mapper.
const HIGHLIGHT_LINEAR: f32 = 0.9;
/// The positive's range in log2: 2^12 = 4096 stays far below half-float's
/// 65504 (also sRGB-encoded); 2^-24 is black.
const MAX_LOG2: f32 = 12.0;
const MIN_LOG2: f32 = -24.0;
/// Transmittance floor: an opaque area (or a black frame edge) stays finite.
const MIN_TRANSMITTANCE: f32 = 1e-6;
/// The smallest density range a channel may have between base and
/// highlight: a nearly blank frame does not blow up.
const MIN_RANGE: f32 = 0.05;

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// A colour negative: each channel inverted on its own.
    #[default]
    Color,
    /// A black-and-white negative: one density for all channels (the mean
    /// of the balanced channels), a neutral positive.
    Bw,
}

/// The tool's settings (`adjustments.negative`, docs/RECIPE_SCHEMA.md).
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Negative {
    pub version: u64,
    pub enabled: bool,
    #[serde(rename = "type")]
    pub kind: Kind,
    /// Linear RGB of the unexposed film (the rebate between frames), as the
    /// base image holds it (linear light; sRGB-encoded sources decoded).
    pub base: [f32; 3],
    /// Linear RGB of the frame's densest neutral area (its highlights).
    pub highlight: [f32; 3],
    /// Density balance per channel, −100…100: scales that channel's density
    /// range by 2^(value / 200) (a crossover the base and highlight do not
    /// fix: colour that changes from shadows to highlights).
    pub balance: [f32; 3],
    /// Print exposure in EV.
    pub exposure: f32,
    /// Print grade, −100…100: the positive's contrast, the frame's range in
    /// stops times 2^(grade / 100).
    pub grade: f32,
}

impl Default for Negative {
    fn default() -> Self {
        Self {
            version: VERSION,
            enabled: false,
            kind: Kind::Color,
            base: [1.0; 3],
            highlight: [0.1; 3],
            balance: [0.0; 3],
            exposure: 0.0,
            grade: 0.0,
        }
    }
}

/// The settings of `adjustments` when the conversion is on and its version
/// is known, else None.
pub fn settings(adjustments: &Value) -> Option<Negative> {
    let value = adjustments.get(KEY).filter(|v| v.is_object())?;
    let negative: Negative = serde_json::from_value(value.clone()).ok()?;
    let finite = negative
        .base
        .iter()
        .chain(&negative.highlight)
        .chain(&negative.balance)
        .chain([&negative.exposure, &negative.grade])
        .all(|v| v.is_finite());
    (negative.enabled && (1..=VERSION).contains(&negative.version) && finite).then_some(negative)
}

/// Whether `adjustments` converts the photo from a negative.
pub fn is_active(adjustments: &Value) -> bool {
    settings(adjustments).is_some()
}

/// Marks a recipe copy whose base [`apply_for_recipe`] has converted, the
/// lens vignetting corrected on the scan (never saved: base builders set
/// it on their own copy).
pub const CONVERTED_KEY: &str = "negativeConvertedBase";

/// The recipe a base converted by [`apply_for_recipe`] is warped with:
/// `adjustments` marked [`CONVERTED_KEY`] when the conversion is on, so the
/// warp does not correct the vignetting a second time.
pub fn converted_base_recipe(adjustments: &Value) -> Cow<'_, Value> {
    if !is_active(adjustments) {
        return Cow::Borrowed(adjustments);
    }
    let mut recipe = adjustments.clone();
    recipe[CONVERTED_KEY] = Value::Bool(true);
    Cow::Owned(recipe)
}

/// Whether `adjustments` is a recipe from [`converted_base_recipe`].
pub fn is_converted(adjustments: &Value) -> bool {
    adjustments.get(CONVERTED_KEY).and_then(Value::as_bool) == Some(true) && is_active(adjustments)
}

/// The per-pixel conversion of [`Negative`], prepared once per image.
#[derive(Debug, Clone, Copy)]
pub struct Conversion {
    kind: Kind,
    log_base: [f32; 3],
    /// 1 / the balanced density range of each channel.
    inv_range: [f32; 3],
    stops: f32,
    offset: f32,
}

impl Conversion {
    pub fn new(n: &Negative) -> Self {
        let mut log_base = [0.0; 3];
        let mut inv_range = [0.0; 3];
        for c in 0..3 {
            let base = n.base[c].max(MIN_TRANSMITTANCE);
            let highlight = n.highlight[c].max(MIN_TRANSMITTANCE);
            log_base[c] = base.log10();
            let range = (log_base[c] - highlight.log10()).max(MIN_RANGE);
            let balance = n.balance[c].clamp(-100.0, 100.0);
            inv_range[c] = (balance / 200.0).exp2() / range;
        }
        let stops = PRINT_STOPS * (n.grade.clamp(-100.0, 100.0) / 100.0).exp2();
        Self {
            kind: n.kind,
            log_base,
            inv_range,
            stops,
            offset: n.exposure.clamp(-10.0, 10.0) + HIGHLIGHT_LINEAR.log2(),
        }
    }

    /// The positive (scene-linear) of linear transmittance `t`.
    pub fn positive(&self, t: [f32; 3]) -> [f32; 3] {
        let mut u = [0.0f32; 3];
        for c in 0..3 {
            let density = self.log_base[c] - t[c].max(MIN_TRANSMITTANCE).log10();
            u[c] = density * self.inv_range[c];
        }
        if self.kind == Kind::Bw {
            let mean = (u[0] + u[1] + u[2]) / 3.0;
            u = [mean; 3];
        }
        // Within what the renderer's half-float input holds, whatever the
        // pixel (an opaque holder, a blank frame).
        u.map(|u| {
            (self.stops * (u - 1.0) + self.offset)
                .clamp(MIN_LOG2, MAX_LOG2)
                .exp2()
        })
    }
}

fn srgb_to_linear(v: f32) -> f32 {
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(v: f32) -> f32 {
    if v <= 0.0031308 {
        v * 12.92
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}

/// Converts `pixels` (interleaved rows of `width`, `channels` per pixel,
/// the first three RGB) in place. `linear`: the values are linear light
/// (RAW decodes), else sRGB-encoded (they are decoded, converted and encoded
/// again, as the renderer decodes such bases). `vignette`: the lens
/// vignetting correction, applied to the scan's values first, as the warp
/// applies it to a base.
pub fn apply(
    pixels: &mut [f32],
    width: usize,
    channels: usize,
    linear: bool,
    negative: &Negative,
    vignette: Option<crate::image_processing::VignetteGain>,
) {
    let conversion = Conversion::new(negative);
    pixels
        .par_chunks_mut(channels * width.max(1))
        .enumerate()
        .for_each(|(y, row)| {
            for (x, pixel) in row.chunks_exact_mut(channels).enumerate() {
                if let Some(vignette) = vignette {
                    let gain = vignette.at(x as f32, y as f32);
                    for value in &mut pixel[..3] {
                        *value *= gain;
                    }
                }
                let mut t = [pixel[0], pixel[1], pixel[2]];
                if !linear {
                    t = t.map(srgb_to_linear);
                }
                let mut e = conversion.positive(t);
                if !linear {
                    e = e.map(linear_to_srgb);
                }
                pixel[..3].copy_from_slice(&e);
            }
        });
}

/// The base image converted when `adjustments` holds an active negative;
/// otherwise `image` itself. `is_raw`: the base holds linear values (else
/// sRGB-encoded ones).
pub fn apply_for_recipe<'a>(
    image: Cow<'a, DynamicImage>,
    adjustments: &Value,
    is_raw: bool,
) -> Cow<'a, DynamicImage> {
    let Some(negative) = settings(adjustments) else {
        return image;
    };
    let mut image = match image.into_owned() {
        image @ (DynamicImage::ImageRgb32F(_) | DynamicImage::ImageRgba32F(_)) => image,
        other => DynamicImage::ImageRgb32F(other.into_rgb32f()),
    };
    let (width, height) = (image.width(), image.height());
    // The scan's vignetting, also when `adjustments` is a converted base's
    // recipe (whose warp leaves it to this pass).
    let unmarked = if adjustments.get(CONVERTED_KEY).is_some() {
        let mut recipe = adjustments.clone();
        if let Some(object) = recipe.as_object_mut() {
            object.remove(CONVERTED_KEY);
        }
        Cow::Owned(recipe)
    } else {
        Cow::Borrowed(adjustments)
    };
    let vignette = crate::image_processing::VignetteGain::of(
        &crate::image_processing::get_geometry_params_from_json(&unmarked),
        width,
        height,
    );
    let width = width as usize;
    match &mut image {
        DynamicImage::ImageRgb32F(buffer) => apply(buffer, width, 3, is_raw, &negative, vignette),
        DynamicImage::ImageRgba32F(buffer) => apply(buffer, width, 4, is_raw, &negative, vignette),
        _ => unreachable!("converted to 32-bit float above"),
    }
    Cow::Owned(image)
}

/// What [`analyze`] finds in a frame: the recipe's `base` and `highlight`.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Analysis {
    pub base: [f32; 3],
    pub highlight: [f32; 3],
    /// Whether the base was found on the frame border (unexposed film);
    /// false when it came from the thinnest part of the frame itself.
    pub base_from_border: bool,
}

/// The recipe [`analyze`]'s image is made with from the photo's recipe
/// `adjustments` (its geometry without the crop, the conversion, retouching
/// and lens blur: the scan as it is) and the crop (x, y, w, h in that
/// image's full-resolution pixels), if any.
pub fn analysis_recipe(adjustments: &Value) -> (Value, Option<[f32; 4]>) {
    let mut recipe = adjustments.clone();
    let crop = adjustments
        .get("crop")
        .and_then(|c| serde_json::from_value::<crate::image_processing::Crop>(c.clone()).ok())
        .filter(|c| c.width > 0.0 && c.height > 0.0)
        .map(|c| [c.x as f32, c.y as f32, c.width as f32, c.height as f32]);
    if let Some(object) = recipe.as_object_mut() {
        for key in [
            KEY,
            "crop",
            "aiPatches",
            "spots",
            "redEyes",
            "faceRetouch",
            "lensBlurEnabled",
        ] {
            object.remove(key);
        }
    }
    (recipe, crop)
}

/// Long side, in pixels, of the image [`analyze`] looks at.
pub const ANALYSIS_DIM: u32 = 1600;

/// The image [`analyze`] looks at and its frame: `source` (the photo's
/// decode, `is_raw` as for [`apply_for_recipe`]) reduced to [`ANALYSIS_DIM`],
/// warped and turned as `adjustments` show it, without crop, conversion or
/// retouching ([`analysis_recipe`]), and the crop in its pixels. The editor's
/// Auto and `rawtonic export --negative` both analyse this.
pub fn analysis_image(
    source: &DynamicImage,
    adjustments: &Value,
) -> (DynamicImage, Option<[f32; 4]>) {
    let (recipe, crop) = analysis_recipe(adjustments);
    let (w, h) = (source.width().max(1), source.height().max(1));
    let scale = (ANALYSIS_DIM as f32 / w.max(h) as f32).min(1.0);
    let reduced = if scale < 1.0 {
        Cow::Owned(crate::image_processing::downscale_f32_image(
            source,
            ((w as f32 * scale).round() as u32).max(1),
            ((h as f32 * scale).round() as u32).max(1),
        ))
    } else {
        Cow::Borrowed(source)
    };
    let actual = reduced.width() as f32 / w as f32;
    let warped = crate::image_processing::apply_geometry_warp(reduced, &recipe);
    let (shown, _) = crate::adjustment_utils::apply_spatial_transformations(warped, &recipe);
    (
        shown.into_owned(),
        crop.map(|rect| rect.map(|v| v * actual)),
    )
}

/// Without a crop: the image's border strip (share of each side) searched
/// for film base.
const BORDER: f32 = 0.06;
/// Without a crop: the share of each side left out of the frame whose
/// densities set the highlight (the scan around it).
const INTERIOR_MARGIN: f32 = 0.12;
/// With a crop: the share of each side of the crop left out of the frame
/// (a frame edge cropped tightly).
const CROP_INSET: f32 = 0.03;
/// Percentile of the frame's density taken as its highlight.
const HIGHLIGHT_PERCENTILE: f32 = 0.995;
/// Percentile of the frame's transmittance the film base must reach (the
/// base is thinner than anything exposed).
const THINNEST_PERCENTILE: f32 = 0.999;
/// Density the film base may lie below the frame's thinnest part (its
/// deepest shadows: fog and shadow exposure). The light source beside the
/// film lies a whole base density (a colour mask's blue density of about
/// 0.8, a black-and-white base of about 0.3) below the base, so a border
/// brighter than this is not film.
const BASE_BELOW_SHADOWS: f32 = 0.4;
/// Linear level from which a scan is clipped to white: a non-RAW's code 253
/// and up in any channel, a RAW decode's green (its white balance keeps
/// green at the sensor's scale; red and blue may lie above 1 unclipped).
const SCAN_WHITE: f32 = 0.98;

/// Whether linear sample `p` is clipped to white (never film).
fn clipped(p: &[f32; 3], is_raw: bool) -> bool {
    if is_raw {
        p[1] >= SCAN_WHITE
    } else {
        p.iter().any(|&v| v >= SCAN_WHITE)
    }
}

/// Linear level at or below which a scan is clipped to black (an 8-bit
/// sRGB code of 0 or 1).
const SCAN_BLACK: f32 = 3.1e-4;
/// At most this many samples per region.
const MAX_SAMPLES: usize = 200_000;

fn luminance(p: [f32; 3]) -> f32 {
    0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2]
}

fn percentile(values: &mut [f32], p: f32) -> f32 {
    if values.is_empty() {
        return 0.0;
    }
    let index = ((values.len() - 1) as f32 * p).round() as usize;
    let (_, value, _) = values.select_nth_unstable_by(index, f32::total_cmp);
    *value
}

fn median3(samples: &[[f32; 3]]) -> [f32; 3] {
    let mut out = [0.0; 3];
    for (c, out) in out.iter_mut().enumerate() {
        let mut channel: Vec<f32> = samples.iter().map(|p| p[c]).collect();
        *out = percentile(&mut channel, 0.5);
    }
    out
}

/// Splits sorted `values` in two (Otsu's threshold); the index of the
/// first value of the upper class, and the gap between the class means.
fn otsu_split(values: &[f32]) -> (usize, f32) {
    let n = values.len();
    let total: f64 = values.iter().map(|&v| v as f64).sum();
    let (mut best, mut best_index, mut lower_sum) = (0.0f64, 0, 0.0f64);
    for i in 1..n {
        lower_sum += values[i - 1] as f64;
        let (w0, w1) = (i as f64, (n - i) as f64);
        let (m0, m1) = (lower_sum / w0, (total - lower_sum) / w1);
        let between = w0 * w1 * (m0 - m1) * (m0 - m1);
        if between > best {
            best = between;
            best_index = i;
        }
    }
    if best_index == 0 {
        return (0, 0.0);
    }
    let lower = values[..best_index].iter().sum::<f32>() / best_index as f32;
    let upper = values[best_index..].iter().sum::<f32>() / (n - best_index) as f32;
    (best_index, upper - lower)
}

/// Finds the film base and the frame's highlight in `image`: the decode as
/// shown before its crop (warped, turned, straightened, see
/// [`analysis_recipe`]; `is_raw` as for [`apply_for_recipe`]), `frame` the
/// crop in its pixels (x, y, w, h), if any. The highlight is the frame's
/// densest area, per channel. The base is the unexposed film beside the
/// frame (outside the crop, else on the image's border): pixels at least as
/// thin as the frame's thinnest (its deepest shadows) and at most
/// `BASE_BELOW_SHADOWS` thinner (so not the light source beside the film),
/// not clipped to white, and of those the darker group
/// when two groups a clear density apart remain. Without such pixels (a scan
/// cropped to the frame or masked black) the frame's own thinnest part
/// stands in.
pub fn analyze(image: &DynamicImage, is_raw: bool, frame: Option<[f32; 4]>) -> Option<Analysis> {
    let rgb = match image {
        DynamicImage::ImageRgb32F(buffer) => Cow::Borrowed(buffer),
        other => Cow::<Rgb32FImage>::Owned(other.to_rgb32f()),
    };
    let (w, h) = rgb.dimensions();
    if w < 8 || h < 8 {
        return None;
    }
    let linear = |p: &image::Rgb<f32>| -> [f32; 3] {
        let p = [p[0], p[1], p[2]];
        if is_raw { p } else { p.map(srgb_to_linear) }
    };
    let (fw, fh) = (w as f32, h as f32);
    // The frame (x0, y0, x1, y1) and whether a pixel is beside it.
    let crop = frame
        .map(|[x, y, cw, ch]| {
            let x0 = x.clamp(0.0, fw);
            let y0 = y.clamp(0.0, fh);
            [x0, y0, (x + cw).clamp(x0, fw), (y + ch).clamp(y0, fh)]
        })
        .filter(|[x0, y0, x1, y1]| x1 - x0 >= 8.0 && y1 - y0 >= 8.0);
    let interior = match crop {
        Some([x0, y0, x1, y1]) => {
            let (dx, dy) = ((x1 - x0) * CROP_INSET, (y1 - y0) * CROP_INSET);
            [x0 + dx, y0 + dy, x1 - dx, y1 - dy]
        }
        None => [
            fw * INTERIOR_MARGIN,
            fh * INTERIOR_MARGIN,
            fw * (1.0 - INTERIOR_MARGIN),
            fh * (1.0 - INTERIOR_MARGIN),
        ],
    };
    let beside = |x: f32, y: f32| match crop {
        Some([x0, y0, x1, y1]) => x < x0 || x >= x1 || y < y0 || y >= y1,
        None => {
            let (bx, by) = ((fw * BORDER).max(1.0), (fh * BORDER).max(1.0));
            x < bx || x >= fw - bx || y < by || y >= fh - by
        }
    };
    let inside =
        |x: f32, y: f32, [x0, y0, x1, y1]: [f32; 4]| x >= x0 && x < x1 && y >= y0 && y < y1;
    let step = (((w as f64 * h as f64) / MAX_SAMPLES as f64).sqrt().ceil() as u32).max(1);
    let mut border = Vec::new();
    let mut frame_samples = Vec::new();
    for y in (0..h).step_by(step as usize) {
        for x in (0..w).step_by(step as usize) {
            let p = linear(rgb.get_pixel(x, y));
            if !p.iter().all(|v| v.is_finite()) {
                continue;
            }
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            if beside(px, py) {
                border.push(p);
            } else if inside(px, py, interior) {
                frame_samples.push(p);
            }
        }
    }
    if frame_samples.is_empty() {
        return None;
    }
    // The frame's thinnest per channel: the film base is at least that.
    let mut thinnest = [0.0f32; 3];
    for (c, thinnest) in thinnest.iter_mut().enumerate() {
        let mut channel: Vec<f32> = frame_samples.iter().map(|p| p[c]).collect();
        *thinnest = percentile(&mut channel, THINNEST_PERCENTILE);
    }
    let thinner = 10f32.powf(BASE_BELOW_SHADOWS);
    let mut candidates: Vec<[f32; 3]> = border
        .into_iter()
        .filter(|p| {
            !clipped(p, is_raw)
                && (0..3).all(|c| p[c] >= 0.9 * thinnest[c] && p[c] <= thinner * thinnest[c])
        })
        .collect();
    // Enough of the border must be film for a reliable base.
    let base_from_border = candidates.len() >= 64;
    let base = if base_from_border {
        candidates.sort_by(|a, b| luminance(*a).total_cmp(&luminance(*b)));
        let logs: Vec<f32> = candidates
            .iter()
            .map(|p| luminance(*p).max(MIN_TRANSMITTANCE).log10())
            .collect();
        let (split, gap) = otsu_split(&logs);
        // Two groups a clear density apart: film and something thinner.
        let chosen = if gap > 0.1 && split >= 16 {
            &candidates[..split]
        } else {
            &candidates[..]
        };
        median3(chosen)
    } else {
        thinnest
    };
    let base = base.map(|v| v.max(MIN_TRANSMITTANCE));
    let mut highlight = [0.0f32; 3];
    for c in 0..3 {
        // A scan clipped to black there (an 8-bit scan of dense
        // highlights) tells no density.
        let mut density: Vec<f32> = frame_samples
            .iter()
            .filter(|p| p[c] > SCAN_BLACK)
            .map(|p| (base[c] / p[c]).log10())
            .collect();
        let dh = percentile(&mut density, HIGHLIGHT_PERCENTILE).max(MIN_RANGE);
        highlight[c] = base[c] / 10f32.powf(dh);
    }
    Some(Analysis {
        base,
        highlight,
        base_from_border,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn recipe(negative: Value) -> Value {
        json!({ "exposure": 0.3, KEY: negative })
    }

    fn on() -> Value {
        json!({
            "version": 1, "enabled": true, "type": "color",
            "base": [0.8, 0.5, 0.3], "highlight": [0.08, 0.04, 0.02],
            "balance": [0, 0, 0], "exposure": 0, "grade": 0
        })
    }

    #[test]
    fn recipes_without_the_tool_are_left_alone() {
        let image = DynamicImage::ImageRgb32F(Rgb32FImage::from_pixel(4, 4, image::Rgb([0.2; 3])));
        for adjustments in [
            json!({}),
            recipe(Value::Null),
            recipe(json!({ "enabled": false, "base": [0.8, 0.5, 0.3] })),
            // A version this build does not know.
            recipe(json!({ "version": 2, "enabled": true })),
        ] {
            let out = apply_for_recipe(Cow::Borrowed(&image), &adjustments, true);
            assert!(matches!(out, Cow::Borrowed(_)), "{adjustments}");
        }
    }

    #[test]
    fn the_film_base_is_black_and_the_highlight_is_white() {
        let n: Negative = serde_json::from_value(on()).unwrap();
        let conversion = Conversion::new(&n);
        let black = conversion.positive(n.base);
        let white = conversion.positive(n.highlight);
        for c in 0..3 {
            // About six stops below the highlight, the same in every channel.
            assert!(black[c] < 0.02 * white[c], "{black:?} {white:?}");
            assert!((black[c] - black[0]).abs() < 1e-6, "{black:?}");
            assert!((white[c] - HIGHLIGHT_LINEAR).abs() < 1e-5, "{white:?}");
        }
    }

    #[test]
    fn densities_between_base_and_highlight_stay_neutral() {
        // A film whose three layers have different densities (an orange
        // mask, unequal ranges): a grey at the same share of each channel's
        // range in density is grey in the positive.
        let n: Negative = serde_json::from_value(on()).unwrap();
        let conversion = Conversion::new(&n);
        for share in [0.1f32, 0.3, 0.5, 0.7, 0.9] {
            let t: [f32; 3] =
                std::array::from_fn(|c| n.base[c] * (n.highlight[c] / n.base[c]).powf(share));
            let p = conversion.positive(t);
            assert!((p[0] - p[1]).abs() < 1e-4 * p[1] && (p[2] - p[1]).abs() < 1e-4 * p[1]);
        }
    }

    #[test]
    fn denser_negative_is_brighter_and_the_frame_spans_the_print_range() {
        let n: Negative = serde_json::from_value(on()).unwrap();
        let conversion = Conversion::new(&n);
        let range = |c: usize| (n.base[c] / n.highlight[c]).log10();
        let stops = PRINT_STOPS;
        let a = conversion.positive([0.4, 0.25, 0.15]);
        let b = conversion.positive([0.2, 0.125, 0.075]);
        // Half the light through the film (+0.301 density) in red is
        // 0.301 / range(0) of the frame's range brighter.
        let expected = stops * std::f32::consts::LOG10_2 / range(0);
        assert!(b[0] > a[0]);
        assert!(((b[0] / a[0]).log2() - expected).abs() < 1e-3);
    }

    #[test]
    fn print_exposure_and_grade() {
        let mut n: Negative = serde_json::from_value(on()).unwrap();
        let mid: [f32; 3] = std::array::from_fn(|c| (n.base[c] * n.highlight[c]).sqrt());
        let plain = Conversion::new(&n).positive(mid);
        n.exposure = 1.0;
        let brighter = Conversion::new(&n).positive(mid);
        assert!((brighter[1] / plain[1] - 2.0).abs() < 1e-4);
        n.exposure = 0.0;
        n.grade = 100.0;
        let harder = Conversion::new(&n);
        // Twice the stops: the mid density half way down is twice as many
        // stops below the (unchanged) highlight.
        let hard = harder.positive(mid);
        let ratio = (HIGHLIGHT_LINEAR / hard[1]).log2() / (HIGHLIGHT_LINEAR / plain[1]).log2();
        assert!((ratio - 2.0).abs() < 1e-3, "{ratio}");
        assert!((harder.positive(n.highlight)[1] - HIGHLIGHT_LINEAR).abs() < 1e-5);
    }

    #[test]
    fn density_balance_moves_one_channel() {
        let mut n: Negative = serde_json::from_value(on()).unwrap();
        let mid: [f32; 3] = std::array::from_fn(|c| (n.base[c] * n.highlight[c]).sqrt());
        let plain = Conversion::new(&n).positive(mid);
        n.balance = [0.0, 0.0, 50.0];
        let warm = Conversion::new(&n).positive(mid);
        assert_eq!(plain[0], warm[0]);
        assert_eq!(plain[1], warm[1]);
        assert!(warm[2] > plain[2]);
    }

    #[test]
    fn opaque_and_blank_pixels_stay_within_half_float() {
        // An opaque holder (0), a clipped light (huge), NaN, and a nearly
        // blank frame (highlight at the base).
        let mut n: Negative = serde_json::from_value(on()).unwrap();
        for highlight in [n.highlight, n.base] {
            n.highlight = highlight;
            let conversion = Conversion::new(&n);
            for t in [[0.0; 3], [1e9; 3], [f32::NAN; 3], [-1.0; 3]] {
                for v in conversion.positive(t) {
                    assert!(v.is_finite() && (0.0..65504.0).contains(&v), "{t:?} -> {v}");
                    assert!(linear_to_srgb(v) < 65504.0);
                }
            }
        }
    }

    #[test]
    fn the_scan_is_corrected_for_vignetting_once() {
        use crate::image_processing::{apply_geometry_warp, get_geometry_params_from_json};
        let lens = json!({"lensCorrectionMode": "manual", "lensVignetteAmount": 100.0,
            "lensDistortionParams": {"vig_k1": -0.4, "vig_k2": 0.1}});
        for (kind, is_raw) in [("color", true), ("bw", true), ("color", false)] {
            let mut v = on();
            v["type"] = json!(kind);
            let mut adjustments = lens.clone();
            adjustments[KEY] = v;
            assert!(get_geometry_params_from_json(&adjustments).lens_vignette_enabled);
            let scan = DynamicImage::ImageRgb32F(Rgb32FImage::from_fn(64, 48, |x, y| {
                image::Rgb([
                    0.1 + 0.01 * x as f32,
                    0.05 + 0.005 * y as f32,
                    0.03 + 0.002 * (x + y) as f32,
                ])
            }));
            // The scan corrected by the warp, then converted (no geometry
            // but vignetting: the warp samples whole pixels).
            let mut plain = adjustments.clone();
            plain.as_object_mut().unwrap().remove(KEY);
            let corrected = apply_geometry_warp(&scan, &plain).into_owned();
            let mut expected = corrected.to_rgb32f();
            apply(
                &mut expected,
                64,
                3,
                is_raw,
                &settings(&adjustments).unwrap(),
                None,
            );
            // Converted with the correction, then warped with the marked
            // recipe: no second correction.
            let converted = apply_for_recipe(Cow::Borrowed(&scan), &adjustments, is_raw);
            let marked = converted_base_recipe(&adjustments);
            assert!(is_converted(&marked) && !is_converted(&adjustments));
            assert!(!get_geometry_params_from_json(&marked).lens_vignette_enabled);
            let actual = apply_geometry_warp(converted, &marked).to_rgb32f();
            for (a, e) in actual.pixels().zip(expected.pixels()) {
                for c in 0..3 {
                    assert!(
                        (a[c] / e[c] - 1.0).abs() < 1e-3,
                        "{kind} {is_raw}: {a:?} vs {e:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn black_and_white_negatives_give_a_neutral_positive() {
        let mut v = on();
        v["type"] = json!("bw");
        let n: Negative = serde_json::from_value(v).unwrap();
        let p = Conversion::new(&n).positive([0.5, 0.2, 0.25]);
        assert_eq!(p[0], p[1]);
        assert_eq!(p[1], p[2]);
    }

    #[test]
    fn srgb_encoded_bases_are_converted_in_linear_light() {
        let adjustments = recipe(on());
        let n = settings(&adjustments).unwrap();
        let t = [0.3f32, 0.2, 0.1];
        let linear = DynamicImage::ImageRgb32F(Rgb32FImage::from_pixel(2, 2, image::Rgb(t)));
        let encoded = DynamicImage::ImageRgb32F(Rgb32FImage::from_pixel(
            2,
            2,
            image::Rgb(t.map(linear_to_srgb)),
        ));
        let a = apply_for_recipe(Cow::Borrowed(&linear), &adjustments, true).into_owned();
        let b = apply_for_recipe(Cow::Borrowed(&encoded), &adjustments, false).into_owned();
        let expected = Conversion::new(&n).positive(t);
        let (a, b) = (a.to_rgb32f(), b.to_rgb32f());
        for (c, expected) in expected.into_iter().enumerate() {
            assert!((a.get_pixel(0, 0)[c] - expected).abs() < 1e-6);
            assert!((srgb_to_linear(b.get_pixel(1, 1)[c]) - expected).abs() < 1e-4 * expected);
        }
    }

    #[test]
    fn alpha_is_kept() {
        let adjustments = recipe(on());
        let image = DynamicImage::ImageRgba32F(image::Rgba32FImage::from_pixel(
            2,
            2,
            image::Rgba([0.3, 0.2, 0.1, 0.25]),
        ));
        let out = apply_for_recipe(Cow::Borrowed(&image), &adjustments, true);
        assert_eq!(out.as_rgba32f().unwrap().get_pixel(0, 0)[3], 0.25);
    }

    /// A synthetic camera scan: light table (bright, neutral) at the left
    /// edge, film rebate (orange base) around the frame, a frame of greys
    /// whose density rises from base to `dmax` per channel.
    fn scan(light_table: Option<f32>) -> (DynamicImage, [f32; 3], [f32; 3]) {
        let base = [0.7f32, 0.42, 0.2];
        let dmax = [1.1f32, 1.3, 1.5];
        let (w, h) = (400u32, 300u32);
        let image = Rgb32FImage::from_fn(w, h, |x, y| {
            if let Some(level) = light_table
                && x < 10
            {
                return image::Rgb([level; 3]);
            }
            let inside = x >= 30 && x < w - 30 && y >= 30 && y < h - 30;
            if !inside {
                return image::Rgb(base);
            }
            // Unexposed film (deep shadows) up to x 60, the densest from
            // x 247 on.
            let share = ((x as f32 - 60.0) / (w as f32 - 120.0) * 1.5).clamp(0.0, 1.0);
            image::Rgb(std::array::from_fn(|c| {
                base[c] / 10f32.powf(dmax[c] * share)
            }))
        });
        let highlight = std::array::from_fn(|c| base[c] / 10f32.powf(dmax[c]));
        (DynamicImage::ImageRgb32F(image), base, highlight)
    }

    #[test]
    fn the_film_base_is_found_on_the_rebate_beside_the_light_table() {
        // No light table, an unclipped one and a clipped one.
        for light_table in [None, Some(0.95), Some(1.0)] {
            let (image, base, highlight) = scan(light_table);
            let found = analyze(&image, true, None).unwrap();
            assert!(found.base_from_border);
            for c in 0..3 {
                assert!((found.base[c] / base[c] - 1.0).abs() < 0.01, "{found:?}");
                assert!(
                    (found.highlight[c] / highlight[c] - 1.0).abs() < 0.01,
                    "{found:?}"
                );
            }
        }
    }

    #[test]
    fn a_crop_is_the_frame_and_the_film_beside_it_the_base() {
        // A strip: dense coloured marks (pre-exposed frame numbers) and
        // rebate around the frame, a bright light source at the left edge
        // with no rebate on the image's border at all.
        let (scan, base, highlight) = scan(None);
        let mut rgb = scan.into_rgb32f();
        let (w, h) = rgb.dimensions();
        for (x, y, p) in rgb.enumerate_pixels_mut() {
            if x < 12 || y < 6 || y >= h - 6 {
                *p = image::Rgb([0.9; 3]);
            } else if (12..30).contains(&x) && (100..140).contains(&y) {
                *p = image::Rgb([0.01, 0.002, 0.03]);
            }
        }
        let image = DynamicImage::ImageRgb32F(rgb);
        // The frame as cropped (the ramp's frame).
        let crop = [30.0, 30.0, w as f32 - 60.0, h as f32 - 60.0];
        let found = analyze(&image, true, Some(crop)).unwrap();
        assert!(found.base_from_border);
        for c in 0..3 {
            assert!((found.base[c] / base[c] - 1.0).abs() < 0.01, "{found:?}");
            assert!(
                (found.highlight[c] / highlight[c] - 1.0).abs() < 0.01,
                "{found:?}"
            );
        }
        // Uncropped, the marks are not taken for the frame's highlight
        // either when they lie outside its middle.
        let uncropped = analyze(&image, true, None).unwrap();
        assert!(
            (uncropped.base[2] / base[2] - 1.0).abs() < 0.01,
            "{uncropped:?}"
        );
    }

    #[test]
    fn pixels_a_scan_clipped_to_black_tell_no_density() {
        // An 8-bit scan whose densest highlights read 0 in blue.
        let (scan, base, highlight) = scan(None);
        let mut rgb = scan.into_rgb32f();
        for (x, _, p) in rgb.enumerate_pixels_mut() {
            if (300..330).contains(&x) {
                p[2] = 0.0;
            }
        }
        let found = analyze(&DynamicImage::ImageRgb32F(rgb), true, None).unwrap();
        assert!(
            (found.highlight[2] / highlight[2] - 1.0).abs() < 0.01,
            "{found:?}"
        );
        assert!((found.base[2] / base[2] - 1.0).abs() < 0.01, "{found:?}");
    }

    #[test]
    fn a_converted_base_recipe_converts_a_decode_alike() {
        // Mask sources convert the pristine decode with the render's
        // (marked) recipe: the scan's vignetting is corrected all the same.
        let mut adjustments = json!({"lensCorrectionMode": "manual", "lensVignetteAmount": 100.0,
            "lensDistortionParams": {"vig_k1": -0.4}});
        adjustments[KEY] = on();
        let scan = DynamicImage::ImageRgb32F(Rgb32FImage::from_pixel(
            32,
            24,
            image::Rgb([0.2, 0.1, 0.05]),
        ));
        let plain = apply_for_recipe(Cow::Borrowed(&scan), &adjustments, true).into_owned();
        let marked = apply_for_recipe(
            Cow::Borrowed(&scan),
            &converted_base_recipe(&adjustments),
            true,
        )
        .into_owned();
        assert_eq!(plain.to_rgb32f().as_raw(), marked.to_rgb32f().as_raw());
        // And the vignetting is in it (a corner differs from the centre).
        let rgb = plain.to_rgb32f();
        assert_ne!(rgb.get_pixel(0, 0), rgb.get_pixel(16, 12));
    }

    #[test]
    fn a_clipped_border_is_not_film() {
        // A neutral scan whose thinnest frame pixels are 0.5, framed by a
        // border clipped to white and no rebate (within the density bound).
        for is_raw in [true, false] {
            let image = DynamicImage::ImageRgb32F(Rgb32FImage::from_fn(200, 150, |x, y| {
                let border = !(12..188).contains(&x) || !(12..138).contains(&y);
                let v = if border {
                    1.0
                } else {
                    0.5 * 0.1f32.powf(x as f32 / 200.0)
                };
                let v = if is_raw { v } else { linear_to_srgb(v) };
                image::Rgb([v; 3])
            }));
            let found = analyze(&image, is_raw, None).unwrap();
            assert!(!found.base_from_border, "{is_raw}: {found:?}");
            assert!(found.base[1] < 0.6, "{is_raw}: {found:?}");
        }
    }

    #[test]
    fn the_analysis_image_is_reduced_with_its_frame() {
        // The synthetic scan at 4×, cropped to its frame.
        let (small, base, highlight) = scan(Some(0.95));
        let big = DynamicImage::ImageRgb32F(image::imageops::resize(
            &small.to_rgb32f(),
            1600 * 2,
            1200 * 2,
            image::imageops::FilterType::Nearest,
        ));
        let crop = json!({"crop": {"x": 240.0, "y": 240.0, "width": 2720.0, "height": 1920.0}});
        let (image, frame) = analysis_image(&big, &crop);
        assert_eq!((image.width(), image.height()), (1600, 1200));
        assert_eq!(frame, Some([120.0, 120.0, 1360.0, 960.0]));
        let found = analyze(&image, true, frame).unwrap();
        for c in 0..3 {
            assert!((found.base[c] / base[c] - 1.0).abs() < 0.01, "{found:?}");
            assert!(
                (found.highlight[c] / highlight[c] - 1.0).abs() < 0.02,
                "{found:?}"
            );
        }
    }

    #[test]
    fn the_analysis_sees_the_scan_without_crop_or_conversion() {
        let adjustments = json!({
            "rotation": 1.5, "crop": {"x": 10.0, "y": 20.0, "width": 300.0, "height": 200.0},
            KEY: on(), "spots": [], "exposure": 1.0
        });
        let (recipe, crop) = analysis_recipe(&adjustments);
        assert_eq!(crop, Some([10.0, 20.0, 300.0, 200.0]));
        assert_eq!(recipe, json!({"rotation": 1.5, "exposure": 1.0}));
        assert_eq!(analysis_recipe(&json!({"crop": null})).1, None);
    }

    #[test]
    fn without_rebate_the_thinnest_part_of_the_frame_stands_in() {
        // The border masked black (a holder), nothing thinner than the frame.
        let (image, base, _) = scan(None);
        let mut rgb = image.into_rgb32f();
        let (w, h) = rgb.dimensions();
        for (x, y, p) in rgb.enumerate_pixels_mut() {
            if x < 30 || x >= w - 30 || y < 30 || y >= h - 30 {
                *p = image::Rgb([0.001; 3]);
            }
        }
        let found = analyze(&DynamicImage::ImageRgb32F(rgb), true, None).unwrap();
        assert!(!found.base_from_border);
        // The frame's unexposed shadows are the base itself.
        for (found_base, base) in found.base.into_iter().zip(base) {
            assert!((found_base / base - 1.0).abs() < 0.01, "{found:?}");
        }
    }

    #[test]
    fn an_analysed_scan_converts_its_ramp_to_neutral_greys() {
        let (image, _, _) = scan(Some(0.95));
        let found = analyze(&image, true, None).unwrap();
        let mut negative: Negative = serde_json::from_value(on()).unwrap();
        negative.base = found.base;
        negative.highlight = found.highlight;
        let conversion = Conversion::new(&negative);
        let rgb = image.to_rgb32f();
        for x in [60u32, 150, 250, 340] {
            let p = conversion.positive(rgb.get_pixel(x, 150).0);
            let spread = (p[0].max(p[1]).max(p[2]) / p[0].min(p[1]).min(p[2])).log2();
            assert!(spread < 0.05, "x {x}: {p:?}");
        }
    }
}
