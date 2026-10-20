use crate::guided_perspective::{GuideLine, count_valid_lines, guided_homography};
use bytemuck::{Pod, Zeroable};
use glam::{Mat3, Vec2, Vec3};
use image::{DynamicImage, GenericImageView, Rgb32FImage};
use imageproc::geometric_transformations::Projection;
use nalgebra::{Matrix3 as NaMatrix3, Vector3 as NaVector3};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use serde_json::json;
use std::borrow::Cow;
use std::f32::consts::PI;

use crate::masks::MaskDefinition;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};

pub trait IntoCowImage<'a> {
    fn into_cow(self) -> Cow<'a, DynamicImage>;
}

impl<'a> IntoCowImage<'a> for DynamicImage {
    fn into_cow(self) -> Cow<'a, DynamicImage> {
        Cow::Owned(self)
    }
}

impl<'a> IntoCowImage<'a> for &'a DynamicImage {
    fn into_cow(self) -> Cow<'a, DynamicImage> {
        Cow::Borrowed(self)
    }
}

impl<'a> IntoCowImage<'a> for Cow<'a, DynamicImage> {
    fn into_cow(self) -> Cow<'a, DynamicImage> {
        self
    }
}

impl<'a> IntoCowImage<'a> for &'a std::sync::Arc<DynamicImage> {
    fn into_cow(self) -> Cow<'a, DynamicImage> {
        Cow::Borrowed(self.as_ref())
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ImageMetadata {
    pub version: u32,
    pub rating: u8,
    pub adjustments: Value,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exif: Option<std::collections::HashMap<String, String>>,
}

impl Default for ImageMetadata {
    fn default() -> Self {
        ImageMetadata {
            version: 1,
            rating: 0,
            adjustments: Value::Null,
            tags: None,
            exif: None,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy)]
pub struct Crop {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct GeometryParams {
    pub distortion: f32,
    pub vertical: f32,
    pub horizontal: f32,
    pub rotate: f32,
    pub aspect: f32,
    pub scale: f32,
    pub x_offset: f32,
    pub y_offset: f32,
    pub lens_distortion_amount: f32,
    pub lens_vignette_amount: f32,
    pub lens_tca_amount: f32,
    pub lens_distortion_enabled: bool,
    pub lens_tca_enabled: bool,
    pub lens_vignette_enabled: bool,
    pub lens_dist_k1: f32,
    pub lens_dist_k2: f32,
    pub lens_dist_k3: f32,
    pub lens_model: u32,
    /// The profile is the camera's own measured data (lensCorrectionMode
    /// "camera"): applied exactly. Database profiles keep the inherited
    /// RapidRAW strengths (distortion ×2.5, vignetting ×0.8 at 100 %).
    pub lens_profile_exact: bool,
    /// Minimum zoom of the corrected frame (`zoom` of the lens parameters).
    pub lens_zoom: f32,
    pub tca_vr: f32,
    pub tca_vb: f32,
    /// Growth of tca_vr / tca_vb with the squared relative radius.
    #[serde(default)]
    pub tca_vr2: f32,
    #[serde(default)]
    pub tca_vb2: f32,
    pub vig_k1: f32,
    pub vig_k2: f32,
    pub vig_k3: f32,
    #[serde(default)]
    pub guided_lines: Vec<GuideLine>,
    #[serde(default)]
    pub guided_perspective_enabled: bool,
    /// Upright corrections (guidedPerspective.upright set) zoom to fill the
    /// frame unless `autoCrop` (Constrain crop) is false.
    pub guided_fill: bool,
    /// Lightroom's Remove Chromatic Aberration / Capture One's analysis:
    /// red and blue scaled to green by the photo's own edges
    /// (`lateral_ca::estimate`), instead of the lens profile's TCA.
    #[serde(default)]
    pub auto_ca: bool,
    /// The estimate `auto_ca` uses (`lateral_ca::version_of`).
    #[serde(default)]
    pub auto_ca_version: u64,
    /// `auto_ca` replaces the lens data's own TCA only where it measures
    /// clearly better (`lateral_ca::competes`, `estimate_versus`).
    #[serde(default)]
    pub auto_ca_versus_own: bool,
    /// Resampling keeps negative (wide-gamut) channels where the source has
    /// them (`renderVersion` 2, `bicubic_floor`).
    #[serde(default)]
    pub keep_negative: bool,
}

impl Default for GeometryParams {
    fn default() -> Self {
        Self {
            distortion: 0.0,
            vertical: 0.0,
            horizontal: 0.0,
            rotate: 0.0,
            aspect: 0.0,
            scale: 100.0,
            x_offset: 0.0,
            y_offset: 0.0,
            lens_distortion_amount: 1.0,
            lens_vignette_amount: 1.0,
            lens_tca_amount: 1.0,
            lens_distortion_enabled: true,
            lens_tca_enabled: true,
            lens_vignette_enabled: true,
            lens_dist_k1: 0.0,
            lens_dist_k2: 0.0,
            lens_dist_k3: 0.0,
            lens_model: 0,
            lens_profile_exact: false,
            lens_zoom: 1.0,
            tca_vr: 1.0,
            tca_vb: 1.0,
            tca_vr2: 0.0,
            tca_vb2: 0.0,
            vig_k1: 0.0,
            vig_k2: 0.0,
            vig_k3: 0.0,
            guided_lines: Vec::new(),
            guided_perspective_enabled: false,
            guided_fill: false,
            auto_ca: false,
            auto_ca_version: 0,
            auto_ca_versus_own: false,
            keep_negative: false,
        }
    }
}

pub fn get_geometry_params_from_json(adjustments: &serde_json::Value) -> GeometryParams {
    let lens_params = adjustments
        .get("lensDistortionParams")
        .and_then(|v| v.as_object());

    let guided = adjustments.get("guidedPerspective");
    let guided_perspective_enabled = guided
        .and_then(|g| g.get("enabled"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let guided_lines: Vec<GuideLine> = guided
        .and_then(|g| g.get("lines"))
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();

    let mut params = GeometryParams {
        distortion: adjustments["transformDistortion"].as_f64().unwrap_or(0.0) as f32,
        vertical: adjustments["transformVertical"].as_f64().unwrap_or(0.0) as f32,
        horizontal: adjustments["transformHorizontal"].as_f64().unwrap_or(0.0) as f32,
        rotate: adjustments["transformRotate"].as_f64().unwrap_or(0.0) as f32,
        aspect: adjustments["transformAspect"].as_f64().unwrap_or(0.0) as f32,
        scale: adjustments["transformScale"].as_f64().unwrap_or(100.0) as f32,
        x_offset: adjustments["transformXOffset"].as_f64().unwrap_or(0.0) as f32,
        y_offset: adjustments["transformYOffset"].as_f64().unwrap_or(0.0) as f32,

        lens_distortion_amount: adjustments["lensDistortionAmount"]
            .as_f64()
            .unwrap_or(100.0) as f32
            / 100.0,
        lens_vignette_amount: adjustments["lensVignetteAmount"].as_f64().unwrap_or(100.0) as f32
            / 100.0,
        lens_tca_amount: adjustments["lensTcaAmount"].as_f64().unwrap_or(100.0) as f32 / 100.0,
        lens_distortion_enabled: adjustments["lensDistortionEnabled"]
            .as_bool()
            .unwrap_or(true),
        lens_tca_enabled: adjustments["lensTcaEnabled"].as_bool().unwrap_or(true),
        lens_vignette_enabled: adjustments["lensVignetteEnabled"].as_bool().unwrap_or(true),

        lens_dist_k1: lens_params
            .and_then(|p| p.get("k1").and_then(|k| k.as_f64()))
            .unwrap_or(0.0) as f32,
        lens_dist_k2: lens_params
            .and_then(|p| p.get("k2").and_then(|k| k.as_f64()))
            .unwrap_or(0.0) as f32,
        lens_dist_k3: lens_params
            .and_then(|p| p.get("k3").and_then(|k| k.as_f64()))
            .unwrap_or(0.0) as f32,
        lens_model: lens_params
            .and_then(|p| p.get("model").and_then(|m| m.as_u64()))
            .unwrap_or(0) as u32,
        lens_profile_exact: adjustments["lensCorrectionMode"].as_str() == Some("camera"),
        lens_zoom: lens_params
            .and_then(|p| p.get("zoom").and_then(|k| k.as_f64()))
            .unwrap_or(1.0) as f32,
        tca_vr: lens_params
            .and_then(|p| p.get("tca_vr").and_then(|k| k.as_f64()))
            .unwrap_or(1.0) as f32,
        tca_vb: lens_params
            .and_then(|p| p.get("tca_vb").and_then(|k| k.as_f64()))
            .unwrap_or(1.0) as f32,
        tca_vr2: lens_params
            .and_then(|p| p.get("tca_vr2").and_then(|k| k.as_f64()))
            .unwrap_or(0.0) as f32,
        tca_vb2: lens_params
            .and_then(|p| p.get("tca_vb2").and_then(|k| k.as_f64()))
            .unwrap_or(0.0) as f32,
        vig_k1: lens_params
            .and_then(|p| p.get("vig_k1").and_then(|k| k.as_f64()))
            .unwrap_or(0.0) as f32,
        vig_k2: lens_params
            .and_then(|p| p.get("vig_k2").and_then(|k| k.as_f64()))
            .unwrap_or(0.0) as f32,
        vig_k3: lens_params
            .and_then(|p| p.get("vig_k3").and_then(|k| k.as_f64()))
            .unwrap_or(0.0) as f32,
        guided_lines,
        guided_perspective_enabled,
        // Lightroom's "Constrain crop", stored as `autoCrop` (missing = on).
        guided_fill: guided
            .and_then(|g| g.get("upright"))
            .and_then(|v| v.as_str())
            .is_some_and(|mode| mode != "off")
            && guided
                .and_then(|g| g.get("autoCrop"))
                .and_then(|v| v.as_bool())
                != Some(false),
        auto_ca: false,
        auto_ca_version: crate::lateral_ca::version_of(adjustments),
        auto_ca_versus_own: false,
        keep_negative: render_version(adjustments) >= 2,
    };
    let own = has_own_tca(&params);
    params.auto_ca = crate::lateral_ca::applies(adjustments, own);
    params.auto_ca_versus_own = !params.auto_ca
        && crate::lateral_ca::competes(
            adjustments,
            own && (params.lens_tca_amount - 1.0).abs() < 1e-4,
        );
    params.auto_ca |= params.auto_ca_versus_own;
    params
}

/// Whether the lens correction carries a lateral CA correction of its own
/// (camera data or a profile's TCA), which the automatic default leaves
/// in place.
fn has_own_tca(params: &GeometryParams) -> bool {
    params.lens_tca_enabled
        && ((params.tca_vr - 1.0).abs() > 1e-6
            || (params.tca_vb - 1.0).abs() > 1e-6
            || params.tca_vr2.abs() > 1e-9
            || params.tca_vb2.abs() > 1e-9)
}

/// The lens data's own lateral CA correction at its amount: red and blue
/// scale at the centre (`vr`, `vb`) and their growth to the corners
/// (`vr2`, `vb2`), as the warp applies them.
fn own_tca(params: &GeometryParams) -> (f32, f32, f32, f32) {
    let vr = if (params.tca_vr - 1.0).abs() > 1e-5 {
        params.tca_vr + (1.0 - params.tca_vr) * (1.0 - params.lens_tca_amount)
    } else {
        1.0
    };
    let vb = if (params.tca_vb - 1.0).abs() > 1e-5 {
        params.tca_vb + (1.0 - params.tca_vb) * (1.0 - params.lens_tca_amount)
    } else {
        1.0
    };
    let amount = params.lens_tca_amount;
    (vr, vb, params.tca_vr2 * amount, params.tca_vb2 * amount)
}

/// `own_tca` as the model the automatic estimate competes with
/// (`lateral_ca::estimate_versus`).
pub fn own_tca_model(params: &GeometryParams) -> crate::lateral_ca::CaModel {
    let (vr, vb, vr2, vb2) = own_tca(params);
    crate::lateral_ca::CaModel {
        red: (vr - 1.0, vr2),
        blue: (vb - 1.0, vb2),
    }
}

/// `DynamicImage::thumbnail`, also right for float images: the image crate
/// (0.25.10) rounds its box average with `+ n / 2` before an integer-style
/// division, which for f32 pixels adds 0.5 to every value. Integer images
/// keep the crate's thumbnail; float images (as the editor decodes every
/// photograph) are area-averaged by `downscale_f32_image`.
pub fn thumbnail(image: &DynamicImage, nwidth: u32, nheight: u32) -> DynamicImage {
    match image {
        DynamicImage::ImageRgb32F(_) | DynamicImage::ImageRgba32F(_) => {
            downscale_f32_image(image, nwidth, nheight)
        }
        _ => image.thumbnail(nwidth, nheight),
    }
}

pub fn downscale_f32_image(image: &DynamicImage, nwidth: u32, nheight: u32) -> DynamicImage {
    let start = std::time::Instant::now();

    let (width, height) = image.dimensions();
    if nwidth == 0 || nheight == 0 || (nwidth >= width && nheight >= height) {
        return image.clone();
    }

    let ratio = (nwidth as f32 / width as f32).min(nheight as f32 / height as f32);
    let new_w = (width as f32 * ratio).round() as u32;
    let new_h = (height as f32 * ratio).round() as u32;

    if new_w == 0 || new_h == 0 {
        return image.clone();
    }

    let tmp_img;
    let img_ref = if let Some(rgb) = image.as_rgb32f() {
        rgb
    } else {
        tmp_img = image.to_rgb32f();
        &tmp_img
    };
    let src: &[f32] = img_ref.as_raw();

    let x_ratio = width as f32 / new_w as f32;
    let y_ratio = height as f32 / new_h as f32;
    let width_usize = width as usize;

    let mut x_bounds = Vec::with_capacity(new_w as usize);
    let mut x_weights = Vec::new();
    for x_out in 0..new_w as usize {
        let x_start = x_out as f32 * x_ratio;
        let x_end = (x_out + 1) as f32 * x_ratio;
        let x_in_start = x_start.floor() as usize;
        let x_in_end = (x_end.ceil() as usize).min(width as usize);

        let weight_start_idx = x_weights.len();
        let mut w_sum = 0.0;
        let mut tmp_w = Vec::with_capacity(x_in_end.saturating_sub(x_in_start));

        let mut actual_start = x_in_end;
        let mut actual_end = x_in_start;

        for x_in in x_in_start..x_in_end {
            let overlap_start = x_start.max(x_in as f32);
            let overlap_end = x_end.min((x_in + 1) as f32);
            let w = (overlap_end - overlap_start).max(0.0);
            if w > 0.0 {
                actual_start = actual_start.min(x_in);
                actual_end = actual_end.max(x_in + 1);
                tmp_w.push(w);
                w_sum += w;
            }
        }

        if w_sum > 0.0 {
            let inv_w = 1.0 / w_sum;
            for w in tmp_w {
                x_weights.push(w * inv_w);
            }
            x_bounds.push((actual_start, actual_end, weight_start_idx));
        } else {
            x_bounds.push((0, 0, weight_start_idx));
        }
    }

    let mut y_bounds = Vec::with_capacity(new_h as usize);
    let mut y_weights = Vec::new();
    for y_out in 0..new_h as usize {
        let y_start = y_out as f32 * y_ratio;
        let y_end = (y_out + 1) as f32 * y_ratio;
        let y_in_start = y_start.floor() as usize;
        let y_in_end = (y_end.ceil() as usize).min(height as usize);

        let weight_start_idx = y_weights.len();
        let mut w_sum = 0.0;
        let mut tmp_w = Vec::with_capacity(y_in_end.saturating_sub(y_in_start));

        let mut actual_start = y_in_end;
        let mut actual_end = y_in_start;

        for y_in in y_in_start..y_in_end {
            let overlap_start = y_start.max(y_in as f32);
            let overlap_end = y_end.min((y_in + 1) as f32);
            let w = (overlap_end - overlap_start).max(0.0);
            if w > 0.0 {
                actual_start = actual_start.min(y_in);
                actual_end = actual_end.max(y_in + 1);
                tmp_w.push(w);
                w_sum += w;
            }
        }

        if w_sum > 0.0 {
            let inv_w = 1.0 / w_sum;
            for w in tmp_w {
                y_weights.push(w * inv_w);
            }
            y_bounds.push((actual_start, actual_end, weight_start_idx));
        } else {
            y_bounds.push((0, 0, weight_start_idx));
        }
    }

    let mut out_buf = vec![0.0f32; (new_w * new_h * 3) as usize];

    out_buf
        .par_chunks_exact_mut(new_w as usize * 3)
        .enumerate()
        .for_each(|(y_out, row)| {
            let (y_in_start, y_in_end, y_wt_offset) = y_bounds[y_out];
            let y_len = y_in_end - y_in_start;
            let y_wts = &y_weights[y_wt_offset..y_wt_offset + y_len];

            for (x_out, &(x_in_start, x_in_end, x_wt_offset)) in x_bounds.iter().enumerate() {
                let mut r_sum = 0.0;
                let mut g_sum = 0.0;
                let mut b_sum = 0.0;

                let x_len = x_in_end - x_in_start;
                let x_wts = &x_weights[x_wt_offset..x_wt_offset + x_len];

                for (dy, &w_y) in y_wts.iter().enumerate() {
                    let y_in = y_in_start + dy;
                    let row_offset = y_in * width_usize * 3;

                    let src_start = row_offset + x_in_start * 3;
                    let src_end = row_offset + x_in_end * 3;
                    let src_slice = &src[src_start..src_end];

                    for (&w_x, chunk) in x_wts.iter().zip(src_slice.as_chunks::<3>().0) {
                        let w = w_x * w_y;

                        let r = chunk[0].max(0.0);
                        let g = chunk[1].max(0.0);
                        let b = chunk[2].max(0.0);

                        r_sum += r * w;
                        g_sum += g * w;
                        b_sum += b * w;
                    }
                }

                let out_idx = x_out * 3;

                row[out_idx] = r_sum;
                row[out_idx + 1] = g_sum;
                row[out_idx + 2] = b_sum;
            }
        });

    let out = Rgb32FImage::from_raw(new_w, new_h, out_buf).expect("buffer size mismatch");
    let result = DynamicImage::ImageRgb32F(out);

    log::info!("downscale_f32_image took {:.2?}", start.elapsed());

    result
}

/// Catmull-Rom weights for the four samples around a fractional position.
#[inline(always)]
fn catmull_rom_weights(t: f32) -> [f32; 4] {
    let t2 = t * t;
    let t3 = t2 * t;
    [
        0.5 * (-t3 + 2.0 * t2 - t),
        0.5 * (3.0 * t3 - 5.0 * t2 + 2.0),
        0.5 * (-3.0 * t3 + 4.0 * t2 + t),
        0.5 * (t3 - t2),
    ]
}

/// Bicubic (Catmull-Rom) sample of one channel of an interleaved RGB float
/// image, or `None` within a pixel of the border. Lens and geometry warps
/// resample every pixel; bilinear sampling kept only 0.64-0.77 of the fine
/// detail there (notes: benchmark-results/noise-reduction-texture.json).
#[inline(always)]
fn bicubic_channel(
    src_raw: &[f32],
    src_width: usize,
    src_height: usize,
    x: f32,
    y: f32,
    channel: usize,
    keep_negative: bool,
) -> Option<f32> {
    if !(x >= 1.0 && y >= 1.0 && x < src_width as f32 - 2.0 && y < src_height as f32 - 2.0) {
        return None;
    }
    let x0 = x.floor() as usize;
    let y0 = y.floor() as usize;
    let wx = catmull_rom_weights(x - x0 as f32);
    let wy = catmull_rom_weights(y - y0 as f32);
    let stride = src_width * 3;
    let mut value = 0.0f32;
    for (j, wyj) in wy.iter().enumerate() {
        let row = (y0 + j - 1) * stride + (x0 - 1) * 3 + channel;
        // SAFETY: x0 - 1 .. x0 + 2 and y0 - 1 .. y0 + 2 lie inside the image
        // by the bounds check above.
        let line = unsafe {
            *src_raw.get_unchecked(row) * wx[0]
                + *src_raw.get_unchecked(row + 3) * wx[1]
                + *src_raw.get_unchecked(row + 6) * wx[2]
                + *src_raw.get_unchecked(row + 9) * wx[3]
        };
        value += line * wyj;
    }
    if value < 0.0 && keep_negative {
        let first = (y0 - 1) * stride + (x0 - 1) * 3 + channel;
        return Some(value.max(bicubic_floor(src_raw, stride, first)));
    }
    Some(value.max(0.0))
}

/// The lowest value a Catmull-Rom sample may take when negative channels are
/// kept (`GeometryParams::keep_negative`): its taps' lowest where one is
/// negative (a colour beyond sRGB in the source), else 0. The weights'
/// undershoot next to an edge stays cut off either way. `first` is the
/// upper-left tap of one channel of an interleaved RGB image.
#[inline]
fn bicubic_floor(src_raw: &[f32], stride: usize, first: usize) -> f32 {
    let mut floor = 0.0f32;
    for j in 0..4 {
        for i in 0..4 {
            floor = floor.min(src_raw[first + j * stride + i * 3]);
        }
    }
    floor
}

/// `bicubic_channel` for all three channels with the weights computed once;
/// each channel sums in the same order, so the results are identical.
#[inline(always)]
fn bicubic_rgb(
    src_raw: &[f32],
    src_width: usize,
    src_height: usize,
    x: f32,
    y: f32,
    keep_negative: bool,
) -> Option<[f32; 3]> {
    if !(x >= 1.0 && y >= 1.0 && x < src_width as f32 - 2.0 && y < src_height as f32 - 2.0) {
        return None;
    }
    let x0 = x.floor() as usize;
    let y0 = y.floor() as usize;
    let wx = catmull_rom_weights(x - x0 as f32);
    let wy = catmull_rom_weights(y - y0 as f32);
    let stride = src_width * 3;
    let mut value = [0.0f32; 3];
    for (j, wyj) in wy.iter().enumerate() {
        let row = (y0 + j - 1) * stride + (x0 - 1) * 3;
        // SAFETY: x0 - 1 .. x0 + 2 and y0 - 1 .. y0 + 2 lie inside the image
        // by the bounds check above.
        let taps = unsafe { src_raw.get_unchecked(row..row + 12) };
        for (channel, out) in value.iter_mut().enumerate() {
            let line = taps[channel] * wx[0]
                + taps[channel + 3] * wx[1]
                + taps[channel + 6] * wx[2]
                + taps[channel + 9] * wx[3];
            *out += line * wyj;
        }
    }
    if keep_negative {
        let first = (y0 - 1) * stride + (x0 - 1) * 3;
        for (channel, v) in value.iter_mut().enumerate() {
            if *v < 0.0 {
                *v = v.max(bicubic_floor(src_raw, stride, first + channel));
            }
        }
        return Some(value);
    }
    Some(value.map(|v| v.max(0.0)))
}

#[inline(always)]
fn interpolate_pixel(
    src_raw: &[f32],
    src_width: usize,
    src_height: usize,
    x: f32,
    y: f32,
    keep_negative: bool,
    pixel_out: &mut [f32],
) {
    // The last row and column are samples too (an unmoved pixel there maps
    // onto itself); their missing neighbour is the edge pixel, as in
    // `interpolate_pixel_with_tca`.
    if x.is_nan()
        || y.is_nan()
        || x < 0.0
        || y < 0.0
        || x > (src_width as f32 - 1.0)
        || y > (src_height as f32 - 1.0)
    {
        return;
    }
    if let Some(rgb) = bicubic_rgb(src_raw, src_width, src_height, x, y, keep_negative) {
        pixel_out[..3].copy_from_slice(&rgb);
        return;
    }

    let x0 = x.floor() as usize;
    let y0 = y.floor() as usize;

    let wx = x - x0 as f32;
    let wy = y - y0 as f32;
    let one_minus_wx = 1.0 - wx;
    let one_minus_wy = 1.0 - wy;

    let stride = src_width * 3;
    let idx_row0 = y0 * stride;
    let idx_row1 = (y0 + 1).min(src_height - 1) * stride;
    let (c0, c1) = (x0 * 3, (x0 + 1).min(src_width - 1) * 3);

    // SAFETY: x0, x0 + 1 (clamped) < src_width and y0, y0 + 1 (clamped) <
    // src_height by the bounds check above.
    unsafe {
        let p00 = src_raw.get_unchecked(idx_row0 + c0..idx_row0 + c0 + 3);
        let p10 = src_raw.get_unchecked(idx_row0 + c1..idx_row0 + c1 + 3);
        let p01 = src_raw.get_unchecked(idx_row1 + c0..idx_row1 + c0 + 3);
        let p11 = src_raw.get_unchecked(idx_row1 + c1..idx_row1 + c1 + 3);

        let top_r = p00[0] * one_minus_wx + p10[0] * wx;
        let top_g = p00[1] * one_minus_wx + p10[1] * wx;
        let top_b = p00[2] * one_minus_wx + p10[2] * wx;

        let bot_r = p01[0] * one_minus_wx + p11[0] * wx;
        let bot_g = p01[1] * one_minus_wx + p11[1] * wx;
        let bot_b = p01[2] * one_minus_wx + p11[2] * wx;

        pixel_out[0] = top_r * one_minus_wy + bot_r * wy;
        pixel_out[1] = top_g * one_minus_wy + bot_g * wy;
        pixel_out[2] = top_b * one_minus_wy + bot_b * wy;
    }
}

/// Lightroom's manual Transform sliders as a homography on centred pixel
/// coordinates (source to output), measured in Camera Raw (notes:
/// reference/transform.md). In units of the half diagonal: Rotate turns
/// first; Vertical/Horizontal then add the perspective row p = −0.006 ×
/// (Horizontal, Vertical) and stretch along p by k(|p|); then Aspect (area
/// kept, 1 + |A|/500 between the axes), Scale, and the Offsets (0.811 of the
/// width / height per 100).
pub fn manual_transform(params: &GeometryParams, width: f64, height: f64) -> [[f64; 3]; 3] {
    let hd = (width * width + height * height).sqrt() / 2.0;
    let (sin, cos) = (params.rotate as f64).to_radians().sin_cos();
    let rotate = [[cos, -sin, 0.0], [sin, cos, 0.0], [0.0, 0.0, 1.0]];
    let p = [
        -0.006 * params.horizontal as f64,
        -0.006 * params.vertical as f64,
    ];
    let magnitude = p[0].hypot(p[1]);
    let mut perspective = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [p[0], p[1], 1.0]];
    if magnitude > 1e-9 {
        // Fitted on 26 renders (|p| ≤ 0.6), within 0.0012.
        let k = 1.0 + magnitude.powi(2) * (0.0911 + magnitude * (4.3799 - 2.3034 * magnitude));
        let (ux, uy) = (p[0] / magnitude, p[1] / magnitude);
        perspective[0][0] += (k - 1.0) * ux * ux;
        perspective[0][1] += (k - 1.0) * ux * uy;
        perspective[1][0] += (k - 1.0) * uy * ux;
        perspective[1][1] += (k - 1.0) * uy * uy;
    }
    let ratio = (1.0 + (params.aspect as f64).abs() / 500.0).sqrt();
    let (ax, ay) = if params.aspect >= 0.0 {
        (1.0 / ratio, ratio)
    } else {
        (ratio, 1.0 / ratio)
    };
    let scale = params.scale as f64 / 100.0;
    let tx = params.x_offset as f64 / 100.0 * 0.811 * width / hd;
    let ty = -(params.y_offset as f64) / 100.0 * 0.811 * height / hd;
    let affine = [
        [scale * ax, 0.0, tx],
        [0.0, scale * ay, ty],
        [0.0, 0.0, 1.0],
    ];
    let mul = |a: [[f64; 3]; 3], b: [[f64; 3]; 3]| -> [[f64; 3]; 3] {
        std::array::from_fn(|r| std::array::from_fn(|c| (0..3).map(|i| a[r][i] * b[i][c]).sum()))
    };
    // In half diagonals, then back to pixels: D⁻¹ H D with D = diag(1/hd, 1/hd, 1).
    let h = mul(affine, mul(perspective, rotate));
    [
        [h[0][0], h[0][1], h[0][2] * hd],
        [h[1][0], h[1][1], h[1][2] * hd],
        [h[2][0] / hd, h[2][1] / hd, h[2][2]],
    ]
}

fn build_transform_matrices(
    params: &GeometryParams,
    width: f32,
    height: f32,
) -> (NaMatrix3<f32>, f32, f32, f64) {
    let cx = width / 2.0;
    let cy = height / 2.0;
    let manual = manual_transform(params, width as f64, height as f64);
    let m_manual = NaMatrix3::from_fn(|r, c| manual[r][c] as f32);
    let t_center = NaMatrix3::new(1.0, 0.0, cx, 0.0, 1.0, cy, 0.0, 0.0, 1.0);
    let t_uncenter = NaMatrix3::new(1.0, 0.0, -cx, 0.0, 1.0, -cy, 0.0, 0.0, 1.0);

    let guided_m = if params.guided_perspective_enabled
        && count_valid_lines(&params.guided_lines, width as f64, height as f64) >= 2
    {
        if let Some(res) = guided_homography(
            &params.guided_lines,
            width as f64,
            height as f64,
            params.guided_fill,
        ) {
            let h = res.forward_h;
            NaMatrix3::new(
                h[0][0] as f32,
                h[0][1] as f32,
                h[0][2] as f32,
                h[1][0] as f32,
                h[1][1] as f32,
                h[1][2] as f32,
                h[2][0] as f32,
                h[2][1] as f32,
                h[2][2] as f32,
            )
        } else {
            NaMatrix3::identity()
        }
    } else {
        NaMatrix3::identity()
    };

    let forward = t_center * m_manual * guided_m * t_uncenter;
    let half_diagonal =
        ((width as f64 * width as f64 + height as f64 * height as f64).sqrt()) / 2.0;

    (forward, cx, cy, half_diagonal)
}

struct TcaContext<'a> {
    src_raw: &'a [f32],
    src_width: usize,
    src_height: usize,
    cx: f32,
    cy: f32,
    /// 1 / half-diagonal², and the growth of the red and blue scales with
    /// the squared relative radius (automatic lateral CA, the camera's data).
    inv_hd2: f32,
    vr2: f32,
    vb2: f32,
    /// The frame row and column `src_raw` starts at (0: the whole frame; a
    /// region's source holds only the rows and columns it reads). Positions
    /// stay in frame pixels; `src_height` counts the rows from `row0` to the
    /// frame's end, `src_width` the columns held (the row stride).
    row0: f32,
    col0: f32,
    /// `GeometryParams::keep_negative`.
    keep_negative: bool,
}

#[inline(always)]
fn interpolate_pixel_with_tca(
    tca: &TcaContext,
    base_x: f32,
    base_y: f32,
    vr: f32,
    vb: f32,
    pixel_out: &mut [f32],
) {
    let src_raw = tca.src_raw;
    let src_width = tca.src_width;
    let src_height = tca.src_height;
    let cx = tca.cx;
    let cy = tca.cy;
    let gx = base_x;
    let gy = base_y;

    let (vr, vb) = if tca.vr2 != 0.0 || tca.vb2 != 0.0 {
        let r2 = ((base_x - cx) * (base_x - cx) + (base_y - cy) * (base_y - cy)) * tca.inv_hd2;
        (vr + tca.vr2 * r2, vb + tca.vb2 * r2)
    } else {
        (vr, vb)
    };
    let rx = cx + (base_x - cx) * vr;
    let ry = cy + (base_y - cy) * vr;

    let bx = cx + (base_x - cx) * vb;
    let by = cy + (base_y - cy) * vb;

    let sample_channel = |target_x: f32, target_y: f32, channel_idx: usize| -> f32 {
        let target_y = target_y - tca.row0;
        let target_x = target_x - tca.col0;
        if target_x.is_nan() || target_y.is_nan() {
            return 0.0;
        }

        if let Some(value) = bicubic_channel(
            src_raw,
            src_width,
            src_height,
            target_x,
            target_y,
            channel_idx,
            tca.keep_negative,
        ) {
            return value;
        }
        let x_clamped = target_x.clamp(0.0, src_width as f32 - 1.0);
        let y_clamped = target_y.clamp(0.0, src_height as f32 - 1.0);

        let mut x0 = x_clamped.floor() as usize;
        let mut y0 = y_clamped.floor() as usize;

        if x0 >= src_width - 1 {
            x0 = src_width.saturating_sub(2);
        }
        if y0 >= src_height - 1 {
            y0 = src_height.saturating_sub(2);
        }

        let wx = x_clamped - x0 as f32;
        let wy = y_clamped - y0 as f32;
        let one_minus_wx = 1.0 - wx;
        let one_minus_wy = 1.0 - wy;

        let stride = src_width * 3;
        let idx_row0 = y0 * stride;
        let idx_row1 = idx_row0 + stride;

        let idx_p00 = idx_row0 + x0 * 3 + channel_idx;

        unsafe {
            let p00 = *src_raw.get_unchecked(idx_p00);
            let p10 = *src_raw.get_unchecked(idx_p00 + 3);
            let p01 = *src_raw.get_unchecked(idx_row1 + x0 * 3 + channel_idx);
            let p11 = *src_raw.get_unchecked(idx_row1 + x0 * 3 + 3 + channel_idx);

            let top = p00 * one_minus_wx + p10 * wx;
            let bot = p01 * one_minus_wx + p11 * wx;
            top * one_minus_wy + bot * wy
        }
    };

    pixel_out[0] = sample_channel(rx, ry, 0);
    pixel_out[1] = sample_channel(gx, gy, 1);
    pixel_out[2] = sample_channel(bx, by, 2);
}

/// Lightroom's manual Distortion (Lens Corrections > Manual, −100…100) as
/// Camera Raw maps it, measured on a Z 7II frame (notes:
/// reference/manual-distortion.md): an output point at `r2` (squared radius
/// in half-diagonals) samples the source at `r · manual_distortion_factor`,
/// `1 + c (1 − r²)` with `c` = amount / 200 (barrel side) or amount / 250
/// (pincushion side). The corners stay where they are; the auto crop scale
/// then fills the frame. The inherited `1 + 2.5 a r²` was several times
/// stronger and folded the photo over at −100.
fn manual_distortion_factor(r2: f64, amount: f64) -> f64 {
    1.0 + manual_distortion_c(amount) * (1.0 - r2)
}

fn manual_distortion_c(amount: f64) -> f64 {
    amount / if amount < 0.0 { 200.0 } else { 250.0 }
}

/// The output radius (half-diagonals) whose `manual_distortion_factor`
/// mapping lands on source radius `r_source`.
fn manual_distortion_inverse(r_source: f64, amount: f64) -> f64 {
    let c = manual_distortion_c(amount);
    let mut r = r_source;
    for _ in 0..12 {
        let value = r * (1.0 + c * (1.0 - r * r)) - r_source;
        let slope = 1.0 + c - 3.0 * c * r * r;
        if slope.abs() < 1e-9 {
            break;
        }
        r -= value / slope;
    }
    r
}

/// Distortion and vignetting strength at 100 %: exact for the camera's own
/// data. lensfun profiles are converted to the warp's units since
/// 2026-10-05, so their distortion is exact too; RapidRAW's ×2.5 (made for
/// its unconverted coefficients) had over-corrected them 2.5-fold (EOS R
/// framing 2-3 % off Camera Raw's). Vignetting keeps ×0.8: closer to Camera
/// Raw than ×1.0 on the 19 files with a lensfun default (mean dE 0.891 vs
/// 0.935; notes: benchmark-results/lensfun-strength-2026-10-07.json).
fn lens_strength(params: &GeometryParams) -> (f64, f64) {
    if params.lens_profile_exact {
        (1.0, 1.0)
    } else {
        (1.0, 0.8)
    }
}

fn compute_lens_auto_crop_scale(params: &GeometryParams, width: f32, height: f32) -> f64 {
    let cx = (width / 2.0) as f64;
    let cy = (height / 2.0) as f64;
    let half_diagonal = (cx * cx + cy * cy).sqrt();
    let max_radius_sq_inv = 1.0 / (cx * cx + cy * cy);

    let lk1 = params.lens_dist_k1 as f64;
    let lk2 = params.lens_dist_k2 as f64;
    let lk3 = params.lens_dist_k3 as f64;
    let lens_dist_amt = (params.lens_distortion_amount as f64) * lens_strength(params).0;

    let k_distortion = params.distortion as f64;

    let has_lens_correction = params.lens_distortion_enabled
        && (lk1.abs() > 1e-6 || lk2.abs() > 1e-6 || lk3.abs() > 1e-6);
    let is_ptlens = params.lens_model == 1;

    let sample_points: [(f64, f64); 8] = [
        (cx, 0.0),
        (cx, height as f64),
        (0.0, cy),
        (width as f64, cy),
        (0.0, 0.0),
        (width as f64, 0.0),
        (0.0, height as f64),
        (width as f64, height as f64),
    ];

    // How far a point (offset from the centre) maps outwards: source radius
    // over its own radius, through the lens profile and the manual
    // distortion.
    let ratio = |dx: f64, dy: f64| -> f64 {
        let ru = (dx * dx + dy * dy).sqrt();
        if ru < 1e-6 {
            return 1.0;
        }

        let mut mapped_dx = dx;
        let mut mapped_dy = dy;

        if has_lens_correction {
            let ru_norm = ru / half_diagonal;
            let ru_norm2 = ru_norm * ru_norm;

            let rd_norm = if is_ptlens {
                let a = lk1;
                let b = lk2;
                let c = lk3;
                let d = 1.0 - a - b - c;
                ru_norm * (a * ru_norm2 * ru_norm + b * ru_norm2 + c * ru_norm + d)
            } else {
                ru_norm
                    * (1.0
                        + lk1 * ru_norm2
                        + lk2 * (ru_norm2 * ru_norm2)
                        + lk3 * (ru_norm2 * ru_norm2 * ru_norm2))
            };

            let effective_r_norm = ru_norm + (rd_norm - ru_norm) * lens_dist_amt;
            let scale = effective_r_norm / ru_norm;

            mapped_dx *= scale;
            mapped_dy *= scale;
        }

        if k_distortion.abs() > 1e-5 {
            let r2_norm = (mapped_dx * mapped_dx + mapped_dy * mapped_dy) * max_radius_sq_inv;
            let f = manual_distortion_factor(r2_norm, k_distortion);
            mapped_dx *= f;
            mapped_dy *= f;
        }

        (mapped_dx * mapped_dx + mapped_dy * mapped_dy).sqrt() / ru
    };

    let mut max_scale: f64 = 1.0;

    for &(px, py) in &sample_points {
        let (dx, dy) = (px - cx, py - cy);
        let scale = if k_distortion.abs() > 1e-5 {
            // The zoom divides the frame point before the mapping, and the
            // manual distortion pulls inner points out further than outer
            // ones: the smallest zoom `s` whose point `p / s` maps within the
            // frame (`ratio(p / s) <= s`), by bisection.
            if ratio(dx, dy) <= 1.0 {
                1.0
            } else {
                let (mut lo, mut hi) = (1.0f64, 8.0f64);
                for _ in 0..48 {
                    let mid = 0.5 * (lo + hi);
                    if ratio(dx / mid, dy / mid) <= mid {
                        hi = mid;
                    } else {
                        lo = mid;
                    }
                }
                hi
            }
        } else {
            ratio(dx, dy)
        };

        if scale > max_scale {
            max_scale = scale;
        }
    }

    let mut max_scale = if max_scale > 1.0 {
        max_scale * 1.002
    } else {
        max_scale
    };
    // The camera's own framing, scaled like the distortion by the amount.
    if has_lens_correction && params.lens_zoom > 1.0 {
        let zoom = 1.0 + (params.lens_zoom as f64 - 1.0) * lens_dist_amt.clamp(0.0, 1.0);
        max_scale = max_scale.max(zoom);
    }
    max_scale
}

/// The per-pixel mapping of [`warp_image_geometry`]: where an output pixel
/// samples the source and how it is shaded (lens vignetting), set up once
/// per frame. The full warp and [`transformed_region`] share it, so a region
/// is the same arithmetic as the whole frame.
struct WarpKernel {
    step_vec_x: NaVector3<f32>,
    step_vec_y: NaVector3<f32>,
    origin_vec: NaVector3<f32>,
    cx: f32,
    cy: f32,
    hd: f64,
    max_radius_sq_inv: f64,
    k_distortion: f64,
    lk1: f64,
    lk2: f64,
    lk3: f64,
    lens_dist_amt: f64,
    has_lens_correction: bool,
    is_ptlens: bool,
    auto_crop_scale: f32,
    vr: f32,
    vb: f32,
    vr2: f32,
    vb2: f32,
    has_tca: bool,
    vk1: f64,
    vk2: f64,
    vk3: f64,
    lens_vig_amt: f64,
    has_vignetting: bool,
    keep_negative: bool,
}

impl WarpKernel {
    /// `src_img` is needed only for the automatic lateral CA (`auto_ca`),
    /// which is measured on the whole photograph.
    fn new(
        params: &GeometryParams,
        width: u32,
        height: u32,
        src_img: Option<&Rgb32FImage>,
    ) -> Self {
        let estimate = MeasuredCa::of_params(params, src_img);
        Self::with_estimate(params, width, height, estimate)
    }

    /// The kernel with `auto_ca`'s estimate given (measured on the whole
    /// photograph before).
    fn with_estimate(
        params: &GeometryParams,
        width: u32,
        height: u32,
        estimate: Option<MeasuredCa>,
    ) -> Self {
        let (forward_transform, cx, cy, half_diagonal) =
            build_transform_matrices(params, width as f32, height as f32);
        let inv = forward_transform
            .try_inverse()
            .unwrap_or(NaMatrix3::identity());

        let step_vec_x = NaVector3::new(inv[(0, 0)], inv[(1, 0)], inv[(2, 0)]);
        let step_vec_y = NaVector3::new(inv[(0, 1)], inv[(1, 1)], inv[(2, 1)]);
        let origin_vec = NaVector3::new(inv[(0, 2)], inv[(1, 2)], inv[(2, 2)]);

        let max_radius_sq_inv = 1.0 / ((cx * cx + cy * cy) as f64);
        let hd = half_diagonal;

        let k_distortion = params.distortion as f64;
        let lk1 = params.lens_dist_k1 as f64;
        let lk2 = params.lens_dist_k2 as f64;
        let lk3 = params.lens_dist_k3 as f64;
        let lens_dist_amt = (params.lens_distortion_amount as f64) * lens_strength(params).0;

        let has_lens_correction = params.lens_distortion_enabled
            && (lk1.abs() > 1e-6 || lk2.abs() > 1e-6 || lk3.abs() > 1e-6);
        let is_ptlens = params.lens_model == 1;

        let auto_crop_scale = if has_lens_correction || k_distortion.abs() > 1e-5 {
            compute_lens_auto_crop_scale(params, width as f32, height as f32) as f32
        } else {
            1.0
        };

        let measured = match estimate.filter(|_| params.auto_ca) {
            // Version 3 against the lens data's own TCA: the photo's own
            // measurement only where it leaves clearly fewer fringes.
            Some(MeasuredCa(estimate)) if params.auto_ca_versus_own => estimate,
            // Measured on the photo itself; it covers the whole lateral CA, so
            // it replaces the profile's.
            Some(MeasuredCa(estimate)) => {
                Some(estimate.unwrap_or(crate::lateral_ca::CaModel::NONE))
            }
            None if params.auto_ca && !params.auto_ca_versus_own => {
                Some(crate::lateral_ca::CaModel::NONE)
            }
            None => None,
        };
        let (vr, vb, vr2, vb2, tca_on) = match measured {
            Some(m) => (1.0 + m.red.0, 1.0 + m.blue.0, m.red.1, m.blue.1, true),
            None => {
                let (vr, vb, vr2, vb2) = own_tca(params);
                (vr, vb, vr2, vb2, params.lens_tca_enabled)
            }
        };
        let has_tca = tca_on
            && ((vr - 1.0).abs() > 1e-5
                || (vb - 1.0).abs() > 1e-5
                || vr2.abs() > 1e-6
                || vb2.abs() > 1e-6);

        let vk1 = params.vig_k1 as f64;
        let vk2 = params.vig_k2 as f64;
        let vk3 = params.vig_k3 as f64;
        let lens_vig_amt = (params.lens_vignette_amount as f64) * lens_strength(params).1;
        let has_vignetting = params.lens_vignette_enabled
            && (vk1.abs() > 1e-6 || vk2.abs() > 1e-6 || vk3.abs() > 1e-6)
            && lens_vig_amt > 0.01;

        Self {
            step_vec_x,
            step_vec_y,
            origin_vec,
            cx,
            cy,
            hd,
            max_radius_sq_inv,
            k_distortion,
            lk1,
            lk2,
            lk3,
            lens_dist_amt,
            has_lens_correction,
            is_ptlens,
            auto_crop_scale,
            vr,
            vb,
            vr2,
            vb2,
            has_tca,
            vk1,
            vk2,
            vk3,
            lens_vig_amt,
            has_vignetting,
            keep_negative: params.keep_negative,
        }
    }

    /// The projective position of output row `y`'s first pixel.
    #[inline(always)]
    fn row_start(&self, y: u32) -> NaVector3<f32> {
        let y_f = y as f32;
        self.origin_vec + (self.step_vec_y * y_f)
    }

    /// Where the output pixel at projective position `current_vec` samples
    /// the source; None leaves it black.
    #[inline(always)]
    fn source_point(&self, current_vec: &NaVector3<f32>) -> Option<(f32, f32)> {
        if current_vec.z.abs() <= 1e-6 {
            return None;
        }
        let (cx, cy, hd) = (self.cx, self.cy, self.hd);
        let inv_z = 1.0 / current_vec.z;
        let mut src_x = current_vec.x * inv_z;
        let mut src_y = current_vec.y * inv_z;

        if self.auto_crop_scale > 1.0 {
            src_x = cx + (src_x - cx) / self.auto_crop_scale;
            src_y = cy + (src_y - cy) / self.auto_crop_scale;
        }

        if self.has_lens_correction {
            let dx = (src_x - cx) as f64;
            let dy = (src_y - cy) as f64;
            let ru = (dx * dx + dy * dy).sqrt();

            if ru > 1e-6 {
                let ru_norm = ru / hd;
                let ru_norm2 = ru_norm * ru_norm;

                let rd_norm = if self.is_ptlens {
                    let a = self.lk1;
                    let b = self.lk2;
                    let c = self.lk3;
                    let d = 1.0 - a - b - c;
                    ru_norm * (a * ru_norm2 * ru_norm + b * ru_norm2 + c * ru_norm + d)
                } else {
                    ru_norm
                        * (1.0
                            + self.lk1 * ru_norm2
                            + self.lk2 * (ru_norm2 * ru_norm2)
                            + self.lk3 * (ru_norm2 * ru_norm2 * ru_norm2))
                };

                let effective_r_norm = ru_norm + (rd_norm - ru_norm) * self.lens_dist_amt;
                let scale = effective_r_norm / ru_norm;

                src_x = cx + (dx * scale) as f32;
                src_y = cy + (dy * scale) as f32;
            }
        }

        if self.k_distortion.abs() > 1e-5 {
            let dx = (src_x - cx) as f64;
            let dy = (src_y - cy) as f64;
            let r2_norm = (dx * dx + dy * dy) * self.max_radius_sq_inv;
            let f = manual_distortion_factor(r2_norm, self.k_distortion);

            src_x = cx + (dx * f) as f32;
            src_y = cy + (dy * f) as f32;
        }
        Some((src_x, src_y))
    }

    /// The leftmost, rightmost, lowest and highest source position (x0, x1,
    /// y0, y1) the sample at (`src_x`, `src_y`) reads in a `width` ×
    /// `height` frame, None when it reads nothing: `interpolate_pixel` leaves
    /// positions outside the frame black (the last row and column are
    /// inside); with lateral CA each channel reads its own position, clamped
    /// into the frame (`interpolate_pixel_with_tca`).
    #[inline(always)]
    fn sampled_area(&self, src_x: f32, src_y: f32, width: u32, height: u32) -> Option<[f32; 4]> {
        let (last_x, last) = (width as f32 - 1.0, height as f32 - 1.0);
        if !self.has_tca {
            let outside = src_x.is_nan()
                || src_y.is_nan()
                || src_x < 0.0
                || src_y < 0.0
                || src_x > last_x
                || src_y > last;
            return (!outside).then_some([src_x, src_x, src_y, src_y]);
        }
        let (cx, cy) = (self.cx, self.cy);
        let (vr, vb) = if self.vr2 != 0.0 || self.vb2 != 0.0 {
            let inv_hd2 = 1.0 / (cx * cx + cy * cy).max(1.0);
            let r2 = ((src_x - cx) * (src_x - cx) + (src_y - cy) * (src_y - cy)) * inv_hd2;
            (self.vr + self.vr2 * r2, self.vb + self.vb2 * r2)
        } else {
            (self.vr, self.vb)
        };
        let (rx, ry) = (cx + (src_x - cx) * vr, cy + (src_y - cy) * vr);
        let (bx, by) = (cx + (src_x - cx) * vb, cy + (src_y - cy) * vb);
        let mut area = [f32::MAX, f32::MIN, f32::MAX, f32::MIN];
        for (x, y) in [(rx, ry), (src_x, src_y), (bx, by)] {
            if !x.is_nan() && !y.is_nan() {
                let (x, y) = (x.clamp(0.0, last_x), y.clamp(0.0, last));
                area = [
                    area[0].min(x),
                    area[1].max(x),
                    area[2].min(y),
                    area[3].max(y),
                ];
            }
        }
        (area[0] <= area[1]).then_some(area)
    }

    /// Lens vignetting of the pixel sampled at (`src_x`, `src_y`).
    #[inline(always)]
    fn shade(&self, src_x: f32, src_y: f32, pixel: &mut [f32]) {
        if !self.has_vignetting {
            return;
        }
        let dx = (src_x - self.cx) as f64;
        let dy = (src_y - self.cy) as f64;
        let ru = (dx * dx + dy * dy).sqrt();
        let ru_norm = ru / self.hd;
        let ru_norm2 = ru_norm * ru_norm;

        let v_factor = 1.0
            + self.vk1 * ru_norm2
            + self.vk2 * (ru_norm2 * ru_norm2)
            + self.vk3 * (ru_norm2 * ru_norm2 * ru_norm2);

        if v_factor > 1e-6 {
            let correction_gain = 1.0 / v_factor;
            let final_gain = 1.0 + (correction_gain - 1.0) * self.lens_vig_amt;

            pixel[0] *= final_gain as f32;
            pixel[1] *= final_gain as f32;
            pixel[2] *= final_gain as f32;
        }
    }

    /// Fills `row` (RGB) with output pixels `x0..` of output row `y`,
    /// sampling `source` (the rows of the frame it holds).
    #[inline(always)]
    fn warp_row(&self, source: &SourceRows, y: u32, x0: u32, row: &mut [f32]) {
        let (stride, height, row0, col0) = (
            source.cols as usize,
            source.height as usize,
            source.row0 as usize,
            source.col0 as f32,
        );
        let source = source.pixels;
        let mut current_vec = self.row_start(y);
        // The same steps as the whole row takes, so a region is bit-identical.
        for _ in 0..x0 {
            current_vec += self.step_vec_x;
        }
        let tca_ctx = TcaContext {
            src_raw: source,
            src_width: stride,
            src_height: height - row0,
            cx: self.cx,
            cy: self.cy,
            inv_hd2: 1.0 / (self.cx * self.cx + self.cy * self.cy).max(1.0),
            vr2: self.vr2,
            vb2: self.vb2,
            row0: row0 as f32,
            col0,
            keep_negative: self.keep_negative,
        };
        for pixel in row.as_chunks_mut::<3>().0.iter_mut() {
            if let Some((src_x, src_y)) = self.source_point(&current_vec) {
                if self.has_tca {
                    interpolate_pixel_with_tca(&tca_ctx, src_x, src_y, self.vr, self.vb, pixel);
                } else {
                    interpolate_pixel(
                        source,
                        stride,
                        height - row0,
                        src_x - col0,
                        src_y - row0 as f32,
                        self.keep_negative,
                        pixel,
                    );
                }
                self.shade(src_x, src_y, pixel);
            }
            current_vec += self.step_vec_x;
        }
    }
}

/// Which pixels of [`warp_image_geometry`]'s output for a `width` × `height`
/// source sample inside the source (1) and which it leaves black (0), by the
/// warp's own per-pixel arithmetic; None when the geometry is the identity
/// and nothing is warped. Lateral CA clamps its channels into the frame, so
/// only the green sample decides.
pub fn geometry_valid_pixels(params: &GeometryParams, width: u32, height: u32) -> Option<Vec<u8>> {
    if is_geometry_identity(params) {
        return None;
    }
    let kernel = WarpKernel::new(params, width, height, None);
    let (last_x, last_y) = (width as f32 - 1.0, height as f32 - 1.0);
    let mut mask = vec![0u8; width as usize * height as usize];
    mask.par_chunks_mut(width as usize)
        .enumerate()
        .for_each(|(y, row)| {
            let mut current_vec = kernel.row_start(y as u32);
            for valid in row.iter_mut() {
                *valid = kernel.source_point(&current_vec).is_some_and(|(x, y)| {
                    !(x.is_nan() || y.is_nan() || x < 0.0 || y < 0.0 || x > last_x || y > last_y)
                }) as u8;
                current_vec += kernel.step_vec_x;
            }
        });
    Some(mask)
}

pub fn warp_image_geometry(image: &DynamicImage, params: GeometryParams) -> DynamicImage {
    // RAW sources are float RGB already; converting would copy the whole
    // frame (about 1 GB at 80 MP) before the warp.
    let converted;
    let src_img = match image.as_rgb32f() {
        Some(src) => src,
        None => {
            converted = image.to_rgb32f();
            &converted
        }
    };
    let (width, height) = src_img.dimensions();
    let mut out_buffer = vec![0.0f32; (width * height * 3) as usize];
    let kernel = WarpKernel::new(&params, width, height, Some(src_img));
    let source = SourceRows::whole(src_img.as_raw(), width, height);

    out_buffer
        .par_chunks_exact_mut(width as usize * 3)
        .enumerate()
        .for_each(|(y, row_pixel_data)| {
            kernel.warp_row(&source, y as u32, 0, row_pixel_data);
        });

    let out_img = Rgb32FImage::from_vec(width, height, out_buffer).unwrap();
    DynamicImage::ImageRgb32F(out_img)
}

pub fn unwarp_image_geometry(warped_image: &DynamicImage, params: GeometryParams) -> DynamicImage {
    let src_img = warped_image.to_rgb32f();
    let (width, height) = src_img.dimensions();
    let mut out_buffer = vec![0.0f32; (width * height * 3) as usize];

    let (forward_transform, cx, cy, half_diagonal) =
        build_transform_matrices(&params, width as f32, height as f32);
    let max_radius_sq_inv = 1.0 / ((cx * cx + cy * cy) as f64);
    let hd = half_diagonal;

    let k_distortion = params.distortion as f64;
    let lk1 = params.lens_dist_k1 as f64;
    let lk2 = params.lens_dist_k2 as f64;
    let lk3 = params.lens_dist_k3 as f64;
    let lens_dist_amt = (params.lens_distortion_amount as f64) * lens_strength(&params).0;

    let has_lens_correction = params.lens_distortion_enabled
        && (lk1.abs() > 1e-6 || lk2.abs() > 1e-6 || lk3.abs() > 1e-6);
    let is_ptlens = params.lens_model == 1;

    let auto_crop_scale = if has_lens_correction || k_distortion.abs() > 1e-5 {
        compute_lens_auto_crop_scale(&params, width as f32, height as f32) as f32
    } else {
        1.0
    };

    let src_raw = src_img.as_raw();
    let width_usize = width as usize;
    let height_usize = height as usize;

    out_buffer
        .par_chunks_exact_mut(width_usize * 3)
        .enumerate()
        .for_each(|(y, row_pixel_data)| {
            let y_f = y as f32;

            for (x, pixel) in row_pixel_data.as_chunks_mut::<3>().0.iter_mut().enumerate() {
                let x_f = x as f32;
                let mut current_x = x_f;
                let mut current_y = y_f;

                if k_distortion.abs() > 1e-5 {
                    let dx = (current_x - cx) as f64;
                    let dy = (current_y - cy) as f64;
                    let r_distorted = (dx * dx + dy * dy).sqrt();

                    if r_distorted > 1e-6 {
                        let half_diagonal = max_radius_sq_inv.sqrt().recip();
                        let r_straight =
                            manual_distortion_inverse(r_distorted / half_diagonal, k_distortion)
                                * half_diagonal;

                        let scale = r_straight / r_distorted;
                        current_x = cx + (dx * scale) as f32;
                        current_y = cy + (dy * scale) as f32;
                    }
                }

                if has_lens_correction {
                    let dx = (current_x - cx) as f64;
                    let dy = (current_y - cy) as f64;
                    let rd = (dx * dx + dy * dy).sqrt();

                    if rd > 1e-6 {
                        let mut ru = rd;

                        for _ in 0..8 {
                            let ru_norm = ru / hd;
                            let ru_norm2 = ru_norm * ru_norm;

                            let (f_val, f_prime) = if is_ptlens {
                                let a = lk1;
                                let b = lk2;
                                let c = lk3;
                                let d = 1.0 - a - b - c;
                                let poly = a * ru_norm2 * ru_norm + b * ru_norm2 + c * ru_norm + d;

                                let val = ru * poly;
                                let prime = 4.0 * a * ru_norm2 * ru_norm
                                    + 3.0 * b * ru_norm2
                                    + 2.0 * c * ru_norm
                                    + d;
                                (val, prime)
                            } else {
                                let poly = 1.0
                                    + lk1 * ru_norm2
                                    + lk2 * (ru_norm2 * ru_norm2)
                                    + lk3 * (ru_norm2 * ru_norm2 * ru_norm2);
                                let val = ru * poly;
                                let poly_prime = 2.0 * lk1 * ru_norm
                                    + 4.0 * lk2 * ru_norm2 * ru_norm
                                    + 6.0 * lk3 * (ru_norm2 * ru_norm2) * ru_norm;
                                let prime = poly + ru_norm * poly_prime;
                                (val, prime)
                            };

                            let g_val = ru + (f_val - ru) * lens_dist_amt - rd;
                            let g_prime = 1.0 + (f_prime - 1.0) * lens_dist_amt;

                            if g_prime.abs() < 1e-7 {
                                break;
                            }
                            let delta = g_val / g_prime;
                            ru -= delta;
                            if delta.abs() < 1e-4 {
                                break;
                            }
                        }

                        let scale = ru / rd;
                        current_x = cx + (dx * scale) as f32;
                        current_y = cy + (dy * scale) as f32;
                    }
                }

                if auto_crop_scale > 1.0 {
                    current_x = cx + (current_x - cx) * auto_crop_scale;
                    current_y = cy + (current_y - cy) * auto_crop_scale;
                }

                let target_vec = forward_transform * NaVector3::new(current_x, current_y, 1.0);

                if target_vec.z.abs() > 1e-6 {
                    let inv_z = 1.0 / target_vec.z;

                    let src_x = target_vec.x * inv_z;
                    let src_y = target_vec.y * inv_z;

                    interpolate_pixel(
                        src_raw,
                        width_usize,
                        height_usize,
                        src_x,
                        src_y,
                        params.keep_negative,
                        pixel,
                    );
                }
            }
        });

    let out_img = Rgb32FImage::from_vec(width, height, out_buffer).unwrap();
    DynamicImage::ImageRgb32F(out_img)
}

/// Single points through the lens and transform warp of a `width` × `height`
/// image: `to_source` is where `warp_image_geometry` samples the source for
/// an output pixel, `to_output` where `unwarp_image_geometry` finds a source
/// point in the output (the geometry only; vignetting and colour fringes do
/// not move pixels). For bitmaps kept in one warp's frame and shown in
/// another's (AI masks after a later lens or transform change).
pub struct WarpPoints {
    forward: NaMatrix3<f32>,
    inverse: NaMatrix3<f32>,
    cx: f32,
    cy: f32,
    half_diagonal: f64,
    max_radius_sq_inv: f64,
    k_distortion: f64,
    lens_k: (f64, f64, f64),
    lens_dist_amt: f64,
    has_lens_correction: bool,
    is_ptlens: bool,
    auto_crop_scale: f32,
}

impl WarpPoints {
    pub fn new(params: &GeometryParams, width: u32, height: u32) -> Self {
        let (width, height) = (width as f32, height as f32);
        let (forward, cx, cy, half_diagonal) = build_transform_matrices(params, width, height);
        let inverse = forward.try_inverse().unwrap_or(NaMatrix3::identity());
        let k_distortion = params.distortion as f64;
        let lens_k = (
            params.lens_dist_k1 as f64,
            params.lens_dist_k2 as f64,
            params.lens_dist_k3 as f64,
        );
        let has_lens_correction = params.lens_distortion_enabled
            && (lens_k.0.abs() > 1e-6 || lens_k.1.abs() > 1e-6 || lens_k.2.abs() > 1e-6);
        let auto_crop_scale = if has_lens_correction || k_distortion.abs() > 1e-5 {
            compute_lens_auto_crop_scale(params, width, height) as f32
        } else {
            1.0
        };
        Self {
            forward,
            inverse,
            cx,
            cy,
            half_diagonal,
            max_radius_sq_inv: 1.0 / ((cx * cx + cy * cy) as f64),
            k_distortion,
            lens_k,
            lens_dist_amt: (params.lens_distortion_amount as f64) * lens_strength(params).0,
            has_lens_correction,
            is_ptlens: params.lens_model == 1,
            auto_crop_scale,
        }
    }

    /// The source point output pixel (`x`, `y`) samples.
    pub fn to_source(&self, x: f32, y: f32) -> Option<(f32, f32)> {
        let (cx, cy) = (self.cx, self.cy);
        let v = self.inverse * NaVector3::new(x, y, 1.0);
        if v.z.abs() <= 1e-6 {
            return None;
        }
        let inv_z = 1.0 / v.z;
        let mut src_x = v.x * inv_z;
        let mut src_y = v.y * inv_z;
        if self.auto_crop_scale > 1.0 {
            src_x = cx + (src_x - cx) / self.auto_crop_scale;
            src_y = cy + (src_y - cy) / self.auto_crop_scale;
        }
        if self.has_lens_correction {
            let dx = (src_x - cx) as f64;
            let dy = (src_y - cy) as f64;
            let ru = (dx * dx + dy * dy).sqrt();
            if ru > 1e-6 {
                let ru_norm = ru / self.half_diagonal;
                let ru_norm2 = ru_norm * ru_norm;
                let (a, b, c) = self.lens_k;
                let rd_norm = if self.is_ptlens {
                    let d = 1.0 - a - b - c;
                    ru_norm * (a * ru_norm2 * ru_norm + b * ru_norm2 + c * ru_norm + d)
                } else {
                    ru_norm
                        * (1.0
                            + a * ru_norm2
                            + b * (ru_norm2 * ru_norm2)
                            + c * (ru_norm2 * ru_norm2 * ru_norm2))
                };
                let effective_r_norm = ru_norm + (rd_norm - ru_norm) * self.lens_dist_amt;
                let scale = effective_r_norm / ru_norm;
                src_x = cx + (dx * scale) as f32;
                src_y = cy + (dy * scale) as f32;
            }
        }
        if self.k_distortion.abs() > 1e-5 {
            let dx = (src_x - cx) as f64;
            let dy = (src_y - cy) as f64;
            let r2_norm = (dx * dx + dy * dy) * self.max_radius_sq_inv;
            let f = manual_distortion_factor(r2_norm, self.k_distortion);
            src_x = cx + (dx * f) as f32;
            src_y = cy + (dy * f) as f32;
        }
        Some((src_x, src_y))
    }

    /// The output point source point (`x`, `y`) lands on.
    pub fn to_output(&self, x: f32, y: f32) -> Option<(f32, f32)> {
        let (cx, cy) = (self.cx, self.cy);
        let (mut current_x, mut current_y) = (x, y);
        if self.k_distortion.abs() > 1e-5 {
            let dx = (current_x - cx) as f64;
            let dy = (current_y - cy) as f64;
            let r_distorted = (dx * dx + dy * dy).sqrt();
            if r_distorted > 1e-6 {
                let half_diagonal = self.max_radius_sq_inv.sqrt().recip();
                let r_straight =
                    manual_distortion_inverse(r_distorted / half_diagonal, self.k_distortion)
                        * half_diagonal;
                let scale = r_straight / r_distorted;
                current_x = cx + (dx * scale) as f32;
                current_y = cy + (dy * scale) as f32;
            }
        }
        if self.has_lens_correction {
            let dx = (current_x - cx) as f64;
            let dy = (current_y - cy) as f64;
            let rd = (dx * dx + dy * dy).sqrt();
            if rd > 1e-6 {
                let hd = self.half_diagonal;
                let (a, b, c) = self.lens_k;
                let mut ru = rd;
                for _ in 0..8 {
                    let ru_norm = ru / hd;
                    let ru_norm2 = ru_norm * ru_norm;
                    let (f_val, f_prime) = if self.is_ptlens {
                        let d = 1.0 - a - b - c;
                        let poly = a * ru_norm2 * ru_norm + b * ru_norm2 + c * ru_norm + d;
                        let prime = 4.0 * a * ru_norm2 * ru_norm
                            + 3.0 * b * ru_norm2
                            + 2.0 * c * ru_norm
                            + d;
                        (ru * poly, prime)
                    } else {
                        let poly = 1.0
                            + a * ru_norm2
                            + b * (ru_norm2 * ru_norm2)
                            + c * (ru_norm2 * ru_norm2 * ru_norm2);
                        let poly_prime = 2.0 * a * ru_norm
                            + 4.0 * b * ru_norm2 * ru_norm
                            + 6.0 * c * (ru_norm2 * ru_norm2) * ru_norm;
                        (ru * poly, poly + ru_norm * poly_prime)
                    };
                    let g_val = ru + (f_val - ru) * self.lens_dist_amt - rd;
                    let g_prime = 1.0 + (f_prime - 1.0) * self.lens_dist_amt;
                    if g_prime.abs() < 1e-7 {
                        break;
                    }
                    let delta = g_val / g_prime;
                    ru -= delta;
                    if delta.abs() < 1e-4 {
                        break;
                    }
                }
                let scale = ru / rd;
                current_x = cx + (dx * scale) as f32;
                current_y = cy + (dy * scale) as f32;
            }
        }
        if self.auto_crop_scale > 1.0 {
            current_x = cx + (current_x - cx) * self.auto_crop_scale;
            current_y = cy + (current_y - cy) * self.auto_crop_scale;
        }
        let v = self.forward * NaVector3::new(current_x, current_y, 1.0);
        (v.z.abs() > 1e-6).then(|| (v.x / v.z, v.y / v.z))
    }
}

pub fn inverse_transform_mask(
    mask: image::GrayImage,
    adjustments: &serde_json::Value,
) -> image::GrayImage {
    let rotation_degrees = adjustments
        .get("rotation")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0) as f32;
    let mask_dyn = image::DynamicImage::ImageLuma8(mask);

    let unrotated_fine = if rotation_degrees.abs() > 1e-5 {
        crate::image_processing::apply_rotation(mask_dyn, -rotation_degrees, false).into_owned()
    } else {
        mask_dyn
    };

    let flip_h = adjustments
        .get("flipHorizontal")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let flip_v = adjustments
        .get("flipVertical")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let flipped = apply_flip(unrotated_fine, flip_h, flip_v).into_owned();

    let steps = adjustments
        .get("orientationSteps")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u8;
    let inverse_steps = (4 - (steps % 4)) % 4;
    let unrotated_coarse = apply_coarse_rotation(flipped, inverse_steps).into_owned();

    let unwarped = apply_unwarp_geometry(unrotated_coarse, adjustments).into_owned();

    unwarped.into_luma8()
}

pub fn inverse_transform_point(
    mut x: f64,
    mut y: f64,
    mut curr_w: f64,
    mut curr_h: f64,
    adjustments: &serde_json::Value,
) -> (f64, f64) {
    let rotation_degrees = adjustments
        .get("rotation")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    if rotation_degrees.abs() > 1e-5 {
        let cx = curr_w / 2.0;
        let cy = curr_h / 2.0;
        let theta_rad = -rotation_degrees * std::f64::consts::PI / 180.0;
        let cos_t = theta_rad.cos();
        let sin_t = theta_rad.sin();

        let dx = x - cx;
        let dy = y - cy;
        x = cx + dx * cos_t - dy * sin_t;
        y = cy + dx * sin_t + dy * cos_t;
    }

    let flip_h = adjustments
        .get("flipHorizontal")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let flip_v = adjustments
        .get("flipVertical")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if flip_h {
        x = curr_w - x;
    }
    if flip_v {
        y = curr_h - y;
    }

    let steps = adjustments
        .get("orientationSteps")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u8;
    let inverse_steps = (4 - (steps % 4)) % 4;
    for _ in 0..inverse_steps {
        let new_x = curr_h - y;
        let new_y = x;
        x = new_x;
        y = new_y;
        std::mem::swap(&mut curr_w, &mut curr_h);
    }

    let params = get_geometry_params_from_json(adjustments);
    let width = curr_w as f32;
    let height = curr_h as f32;

    let (forward_transform, cx_f32, cy_f32, hd) = build_transform_matrices(&params, width, height);
    let cx = cx_f32 as f64;
    let cy = cy_f32 as f64;
    let inv = forward_transform
        .try_inverse()
        .unwrap_or(nalgebra::Matrix3::identity());

    let vec = inv * nalgebra::Vector3::new(x as f32, y as f32, 1.0);
    if vec.z.abs() > 1e-6 {
        let inv_z = 1.0 / (vec.z as f64);
        let mut src_x = (vec.x as f64) * inv_z;
        let mut src_y = (vec.y as f64) * inv_z;

        let k_distortion = params.distortion as f64;
        let lk1 = params.lens_dist_k1 as f64;
        let lk2 = params.lens_dist_k2 as f64;
        let lk3 = params.lens_dist_k3 as f64;
        let lens_dist_amt = (params.lens_distortion_amount as f64) * lens_strength(&params).0;

        let has_lens_correction = params.lens_distortion_enabled
            && (lk1.abs() > 1e-6 || lk2.abs() > 1e-6 || lk3.abs() > 1e-6);
        let is_ptlens = params.lens_model == 1;

        let auto_crop_scale = if has_lens_correction || k_distortion.abs() > 1e-5 {
            compute_lens_auto_crop_scale(&params, width, height)
        } else {
            1.0
        };

        if auto_crop_scale > 1.0 {
            src_x = cx + (src_x - cx) / auto_crop_scale;
            src_y = cy + (src_y - cy) / auto_crop_scale;
        }

        if has_lens_correction {
            let dx = src_x - cx;
            let dy = src_y - cy;
            let ru = (dx * dx + dy * dy).sqrt();

            if ru > 1e-6 {
                let ru_norm = ru / hd;
                let ru_norm2 = ru_norm * ru_norm;

                let rd_norm = if is_ptlens {
                    let a = lk1;
                    let b = lk2;
                    let c = lk3;
                    let d = 1.0 - a - b - c;
                    ru_norm * (a * ru_norm2 * ru_norm + b * ru_norm2 + c * ru_norm + d)
                } else {
                    ru_norm
                        * (1.0
                            + lk1 * ru_norm2
                            + lk2 * (ru_norm2 * ru_norm2)
                            + lk3 * (ru_norm2 * ru_norm2 * ru_norm2))
                };

                let effective_r_norm = ru_norm + (rd_norm - ru_norm) * lens_dist_amt;
                let scale = effective_r_norm / ru_norm;

                src_x = cx + (dx * scale);
                src_y = cy + (dy * scale);
            }
        }

        if k_distortion.abs() > 1e-5 {
            let max_radius_sq_inv = 1.0 / (cx * cx + cy * cy);
            let dx = src_x - cx;
            let dy = src_y - cy;
            let r2_norm = (dx * dx + dy * dy) * max_radius_sq_inv;
            let f = manual_distortion_factor(r2_norm, k_distortion);

            src_x = cx + (dx * f);
            src_y = cy + (dy * f);
        }

        return (src_x, src_y);
    }

    (x, y)
}

/// The CPU default raw curve of one channel value.
#[inline]
fn default_raw_curve(value: f32) -> f32 {
    const GAMMA: f32 = 2.38;
    const INV_GAMMA: f32 = 1.0 / GAMMA;
    const CONTRAST: f32 = 1.28;
    ((value.powf(INV_GAMMA) - 0.5) * CONTRAST + 0.5).clamp(0.0, 1.0)
}

pub fn apply_cpu_default_raw_processing(image: &mut DynamicImage) {
    let mut f32_image = image.to_rgb32f();
    f32_image.par_chunks_mut(3).for_each(|pixel_chunk| {
        for channel in pixel_chunk {
            *channel = default_raw_curve(*channel);
        }
    });
    *image = DynamicImage::ImageRgb32F(f32_image);
}

/// `apply_cpu_default_raw_processing` into a new image. A float source is read
/// in place instead of being copied first (two 40 MP copies were ~a third of
/// an Auto Mask source build); the values are the same.
pub fn cpu_default_raw_processed(image: &DynamicImage) -> DynamicImage {
    let (source, channels): (&[f32], usize) = match image {
        DynamicImage::ImageRgb32F(source) => (source.as_raw(), 3),
        DynamicImage::ImageRgba32F(source) => (source.as_raw(), 4),
        _ => {
            let mut copy = image.clone();
            apply_cpu_default_raw_processing(&mut copy);
            return copy;
        }
    };
    let (width, height) = image.dimensions();
    let mut values = vec![0.0_f32; width as usize * height as usize * 3];
    values
        .par_chunks_mut(3)
        .zip(source.par_chunks(channels))
        .for_each(|(out, pixel)| {
            for (out, value) in out.iter_mut().zip(pixel) {
                *out = default_raw_curve(*value);
            }
        });
    DynamicImage::ImageRgb32F(
        Rgb32FImage::from_raw(width, height, values).expect("buffer matches the dimensions"),
    )
}

pub fn apply_srgb_to_linear(mut image: DynamicImage) -> DynamicImage {
    let to_linear = |x: f32| -> f32 {
        let x = x.max(0.0);
        if x <= 0.04045 {
            x / 12.92
        } else {
            ((x + 0.055) / 1.055).powf(2.4)
        }
    };

    match &mut image {
        DynamicImage::ImageRgb32F(img) => {
            img.as_mut().par_iter_mut().for_each(|c| *c = to_linear(*c));
        }
        DynamicImage::ImageRgba32F(img) => {
            img.par_chunks_mut(4).for_each(|p| {
                p[0] = to_linear(p[0]);
                p[1] = to_linear(p[1]);
                p[2] = to_linear(p[2]);
            });
        }
        _ => {}
    }
    image
}

pub fn apply_linear_to_srgb(mut image: DynamicImage) -> DynamicImage {
    let to_srgb = |x: f32| -> f32 {
        let x = x.max(0.0);
        if x <= 0.0031308 {
            x * 12.92
        } else {
            1.055 * x.powf(1.0 / 2.4) - 0.055
        }
    };

    match &mut image {
        DynamicImage::ImageRgb32F(img) => {
            img.as_mut().par_iter_mut().for_each(|c| *c = to_srgb(*c));
        }
        DynamicImage::ImageRgba32F(img) => {
            img.par_chunks_mut(4).for_each(|p| {
                p[0] = to_srgb(p[0]);
                p[1] = to_srgb(p[1]);
                p[2] = to_srgb(p[2]);
            });
        }
        _ => {}
    }
    image
}

pub use rawtonic_rawdecode::orientation::apply_orientation;

pub fn apply_geometry_warp<'a>(
    image: impl IntoCowImage<'a>,
    adjustments: &serde_json::Value,
) -> Cow<'a, DynamicImage> {
    let image = image.into_cow();
    let params = get_geometry_params_from_json(adjustments);
    if !is_geometry_identity(&params) {
        Cow::Owned(warp_image_geometry(image.as_ref(), params))
    } else {
        image
    }
}

pub fn apply_unwarp_geometry<'a>(
    image: impl IntoCowImage<'a>,
    adjustments: &serde_json::Value,
) -> Cow<'a, DynamicImage> {
    let image = image.into_cow();
    let params = get_geometry_params_from_json(adjustments);
    if !is_geometry_identity(&params) {
        Cow::Owned(unwarp_image_geometry(image.as_ref(), params))
    } else {
        image
    }
}

pub fn apply_coarse_rotation<'a>(
    image: impl IntoCowImage<'a>,
    orientation_steps: u8,
) -> Cow<'a, DynamicImage> {
    let image = image.into_cow();
    match orientation_steps {
        1 => Cow::Owned(image.rotate90()),
        2 => Cow::Owned(image.rotate180()),
        3 => Cow::Owned(image.rotate270()),
        _ => image,
    }
}

/// Straightening by `rotation_degrees` (bicubic, `sample_bicubic_rgba`;
/// `keep_negative` as `GeometryParams::keep_negative`).
pub fn apply_rotation<'a>(
    image: impl IntoCowImage<'a>,
    rotation_degrees: f32,
    keep_negative: bool,
) -> Cow<'a, DynamicImage> {
    let image = image.into_cow();
    if rotation_degrees % 360.0 == 0.0 {
        return image;
    }

    let rgba_image = image.to_rgba32f();
    let (width, height) = (rgba_image.width(), rgba_image.height());
    let source = rgba_image.as_raw();
    let inverse = rotation_inverse(rotation_degrees, width, height);
    let fetch = |x: i64, y: i64| -> [f32; 4] {
        if x < 0 || x >= width as i64 || y < 0 || y >= height as i64 {
            return [0.0; 4];
        }
        let index = (y as usize * width as usize + x as usize) * 4;
        [
            source[index],
            source[index + 1],
            source[index + 2],
            source[index + 3],
        ]
    };
    let mut output = vec![0f32; width as usize * height as usize * 4];
    output
        .par_chunks_mut(width as usize * 4)
        .enumerate()
        .for_each(|(row, destination)| {
            for (column, pixel) in destination.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                let (px, py) = inverse * (column as f32, row as f32);
                *pixel = sample_bicubic_rgba(&fetch, px, py, keep_negative);
            }
        });
    let rotated = image::ImageBuffer::from_raw(width, height, output)
        .expect("rotation output matches its dimensions");
    Cow::Owned(DynamicImage::ImageRgba32F(rotated))
}

/// Output-to-source mapping of a rotation about the frame centre
/// (imageproc's `rotate_about_center` geometry).
pub(crate) fn rotation_inverse(rotation_degrees: f32, width: u32, height: u32) -> Projection {
    let theta = rotation_degrees * PI / 180.0;
    let (center_x, center_y) = (width as f32 / 2.0, height as f32 / 2.0);
    (Projection::translate(center_x, center_y)
        * Projection::rotate(theta)
        * Projection::translate(-center_x, -center_y))
    .invert()
}

/// Catmull-Rom sample of an RGBA float source at (`x`, `y`); `fetch` returns
/// transparent black outside the frame. Straightening resamples every pixel:
/// bicubic keeps more fine detail than bilinear (as for lens warps). Colour
/// is kept non-negative (with `keep_negative`, down to its taps' lowest, as
/// `bicubic_floor`), alpha within 0..1; values above 1 stay.
#[inline(always)]
fn sample_bicubic_rgba(
    fetch: &impl Fn(i64, i64) -> [f32; 4],
    x: f32,
    y: f32,
    keep_negative: bool,
) -> [f32; 4] {
    let left = x.floor() as i64 - 1;
    let top = y.floor() as i64 - 1;
    let wx = catmull_rom_weights(x - (left + 1) as f32);
    let wy = catmull_rom_weights(y - (top + 1) as f32);
    let mut out = [0.0f32; 4];
    let mut floor = [0.0f32; 3];
    for (j, wyj) in wy.iter().enumerate() {
        let row = top + j as i64;
        let (p0, p1, p2, p3) = (
            fetch(left, row),
            fetch(left + 1, row),
            fetch(left + 2, row),
            fetch(left + 3, row),
        );
        if keep_negative {
            for (channel, low) in floor.iter_mut().enumerate() {
                *low = low
                    .min(p0[channel])
                    .min(p1[channel])
                    .min(p2[channel])
                    .min(p3[channel]);
            }
        }
        for channel in 0..4 {
            let line = p0[channel] * wx[0]
                + p1[channel] * wx[1]
                + p2[channel] * wx[2]
                + p3[channel] * wx[3];
            out[channel] += line * wyj;
        }
    }
    [
        out[0].max(floor[0]),
        out[1].max(floor[1]),
        out[2].max(floor[2]),
        out[3].clamp(0.0, 1.0),
    ]
}

/// The region `apply_crop` keeps of an image of the given size, or `None`
/// when it keeps the whole image.
pub(crate) fn crop_rect(
    crop_value: &Value,
    img_w: u32,
    img_h: u32,
) -> Option<(u32, u32, u32, u32)> {
    if crop_value.is_null() {
        return None;
    }
    let crop = serde_json::from_value::<Crop>(crop_value.clone()).ok()?;
    let x = crop.x.round() as u32;
    let y = crop.y.round() as u32;
    let width = crop.width.round() as u32;
    let height = crop.height.round() as u32;
    if width == 0 || height == 0 || x >= img_w || y >= img_h {
        return None;
    }
    let new_width = (img_w - x).min(width);
    let new_height = (img_h - y).min(height);
    if x == 0 && y == 0 && new_width == img_w && new_height == img_h {
        return None;
    }
    Some((x, y, new_width, new_height))
}

pub fn apply_crop<'a>(image: impl IntoCowImage<'a>, crop_value: &Value) -> Cow<'a, DynamicImage> {
    let image = image.into_cow();
    let (img_w, img_h) = image.dimensions();
    match crop_rect(crop_value, img_w, img_h) {
        Some((x, y, width, height)) => Cow::Owned(image.crop_imm(x, y, width, height)),
        None => image,
    }
}

/// Source pixel coordinates of the frame that `apply_coarse_rotation` and
/// `apply_flip` produce: `source = origin + x * along_x + y * along_y`.
#[derive(Clone, Copy)]
struct OrientedFrame {
    width: u32,
    height: u32,
    origin: (i64, i64),
    along_x: (i64, i64),
    along_y: (i64, i64),
}

impl OrientedFrame {
    fn new(
        width: u32,
        height: u32,
        orientation_steps: u8,
        horizontal: bool,
        vertical: bool,
    ) -> Self {
        let (w, h) = (width as i64, height as i64);
        // Inverses of `image`'s clockwise rotate90/180/270.
        let mut frame = match orientation_steps {
            1 => Self {
                width: height,
                height: width,
                origin: (0, h - 1),
                along_x: (0, -1),
                along_y: (1, 0),
            },
            2 => Self {
                width,
                height,
                origin: (w - 1, h - 1),
                along_x: (-1, 0),
                along_y: (0, -1),
            },
            3 => Self {
                width: height,
                height: width,
                origin: (w - 1, 0),
                along_x: (0, 1),
                along_y: (-1, 0),
            },
            _ => Self {
                width,
                height,
                origin: (0, 0),
                along_x: (1, 0),
                along_y: (0, 1),
            },
        };
        if horizontal {
            let last = frame.width as i64 - 1;
            frame.origin.0 += frame.along_x.0 * last;
            frame.origin.1 += frame.along_x.1 * last;
            frame.along_x = (-frame.along_x.0, -frame.along_x.1);
        }
        if vertical {
            let last = frame.height as i64 - 1;
            frame.origin.0 += frame.along_y.0 * last;
            frame.origin.1 += frame.along_y.1 * last;
            frame.along_y = (-frame.along_y.0, -frame.along_y.1);
        }
        frame
    }

    /// Source pixel index of an in-bounds frame pixel.
    #[inline(always)]
    /// The source pixel (x, y) shown at oriented-frame (`x`, `y`).
    fn source_xy(&self, x: i64, y: i64) -> (i64, i64) {
        (
            self.origin.0 + x * self.along_x.0 + y * self.along_y.0,
            self.origin.1 + x * self.along_x.1 + y * self.along_y.1,
        )
    }

    fn source_pixel(&self, x: i64, y: i64, source_width: usize) -> usize {
        let source_x = self.origin.0 + x * self.along_x.0 + y * self.along_y.0;
        let source_y = self.origin.1 + x * self.along_x.1 + y * self.along_y.1;
        source_y as usize * source_width + source_x as usize
    }
}

/// `apply_coarse_rotation`, `apply_flip`, `apply_rotation` and `apply_crop`
/// in one pass over the kept pixels, for 32-bit float RGB(A) images.
///
/// The step-by-step chain copies the full frame for every step, and a fine
/// rotation first expands the whole frame to RGBA floats and rotates all of it
/// before cropping: about 3 GB of intermediates at 50 MP. Here every kept
/// pixel is computed directly from the source with the chain's arithmetic
/// (bicubic rotation about the oriented frame's centre with a transparent
/// border, `sample_bicubic_rgba`), so the result is bit-identical. `None` for other
/// formats.
pub fn fused_spatial_transform(
    image: &DynamicImage,
    orientation_steps: u8,
    flip_horizontal: bool,
    flip_vertical: bool,
    rotation_degrees: f32,
    crop_value: &Value,
    keep_negative: bool,
) -> Option<DynamicImage> {
    let (channels, source): (usize, &[f32]) = match image {
        DynamicImage::ImageRgb32F(rgb) => (3, rgb.as_raw()),
        DynamicImage::ImageRgba32F(rgba) => (4, rgba.as_raw()),
        _ => return None,
    };
    let source_width = image.width() as usize;
    let frame = OrientedFrame::new(
        image.width(),
        image.height(),
        orientation_steps,
        flip_horizontal,
        flip_vertical,
    );
    let (frame_w, frame_h) = (frame.width, frame.height);
    let (crop_x, crop_y, out_w, out_h) =
        crop_rect(crop_value, frame_w, frame_h).unwrap_or((0, 0, frame_w, frame_h));
    if out_w == 0 || out_h == 0 {
        return None;
    }

    if rotation_degrees % 360.0 == 0.0 {
        let mut output = vec![0f32; out_w as usize * out_h as usize * channels];
        output
            .par_chunks_mut(out_w as usize * channels)
            .enumerate()
            .for_each(|(row, destination)| {
                let y = (crop_y + row as u32) as i64;
                for (column, pixel) in destination.chunks_exact_mut(channels).enumerate() {
                    let index =
                        frame.source_pixel((crop_x + column as u32) as i64, y, source_width);
                    pixel.copy_from_slice(&source[index * channels..(index + 1) * channels]);
                }
            });
        return Some(if channels == 3 {
            DynamicImage::ImageRgb32F(image::ImageBuffer::from_raw(out_w, out_h, output)?)
        } else {
            DynamicImage::ImageRgba32F(image::ImageBuffer::from_raw(out_w, out_h, output)?)
        });
    }

    // Same mapping and sampler as `apply_rotation`.
    let inverse = rotation_inverse(rotation_degrees, frame_w, frame_h);
    let fetch = |x: i64, y: i64| -> [f32; 4] {
        if x < 0 || x >= frame_w as i64 || y < 0 || y >= frame_h as i64 {
            return [0.0; 4];
        }
        let index = frame.source_pixel(x, y, source_width) * channels;
        let alpha = if channels == 4 {
            source[index + 3]
        } else {
            1.0
        };
        [source[index], source[index + 1], source[index + 2], alpha]
    };
    let mut output = vec![0f32; out_w as usize * out_h as usize * 4];
    output
        .par_chunks_mut(out_w as usize * 4)
        .enumerate()
        .for_each(|(row, destination)| {
            let y = (crop_y + row as u32) as f32;
            for (column, pixel) in destination.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                let x = (crop_x + column as u32) as f32;
                let (px, py) = inverse * (x, y);
                *pixel = sample_bicubic_rgba(&fetch, px, py, keep_negative);
            }
        });
    Some(DynamicImage::ImageRgba32F(image::ImageBuffer::from_raw(
        out_w, out_h, output,
    )?))
}

/// Part of a source frame `width` × `height` (32-bit float RGB): `pixels`
/// holds columns `col0..col0 + cols` of rows `row0..row0 + rows()`.
pub struct SourceRows<'a> {
    pub pixels: &'a [f32],
    pub width: u32,
    pub height: u32,
    pub row0: u32,
    pub col0: u32,
    pub cols: u32,
}

impl<'a> SourceRows<'a> {
    /// The whole frame.
    pub fn whole(pixels: &'a [f32], width: u32, height: u32) -> Self {
        Self {
            pixels,
            width,
            height,
            row0: 0,
            col0: 0,
            cols: width,
        }
    }

    pub fn rows(&self) -> u32 {
        (self.pixels.len() / (self.cols as usize * 3).max(1)) as u32
    }
}

/// The automatic lateral CA estimate of a photograph (`lateral_ca::
/// estimate_for` of its whole decode at the recipe's version, against the
/// lens data's own correction where the version weighs it), for warps that
/// cannot measure it themselves: regions warped from some of its rows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MeasuredCa(pub Option<crate::lateral_ca::CaModel>);

impl MeasuredCa {
    /// The estimate `params` use for `image` (None: no automatic lateral
    /// CA, or no image to measure).
    fn of_params(params: &GeometryParams, image: Option<&Rgb32FImage>) -> Option<Self> {
        if !params.auto_ca {
            return None;
        }
        let own = params.auto_ca_versus_own.then(|| own_tca_model(params));
        image.map(|image| {
            MeasuredCa(crate::lateral_ca::estimate_for(
                image,
                params.auto_ca_version,
                own,
            ))
        })
    }

    /// The estimate the warp of `adjustments` uses for `image`, its whole
    /// decode; None when they warp without one.
    pub fn of(image: &Rgb32FImage, adjustments: &Value) -> Option<Self> {
        Self::of_params(&get_geometry_params_from_json(adjustments), Some(image))
    }
}

/// What a region warped from some of a photograph's rows needs that only
/// the whole photograph tells (its whole decode, at full resolution): the
/// automatic lateral CA estimate and the edge-fringe radius.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RegionMeasures {
    pub ca: Option<MeasuredCa>,
    /// `edge_fringe::full_resolution_radius` of the warped photograph.
    pub fringe_radius: Option<usize>,
}

impl RegionMeasures {
    /// The measures of `image`, a whole decode, for `adjustments`.
    pub fn of(image: &Rgb32FImage, adjustments: &Value) -> Self {
        let params = get_geometry_params_from_json(adjustments);
        let ca = MeasuredCa::of_params(&params, Some(image));
        let (width, height) = image.dimensions();
        let raw = image.as_raw();
        let row_len = width as usize * 3;
        let fringe_radius = if is_geometry_identity(&params) {
            crate::edge_fringe::full_resolution_radius(
                adjustments,
                width as usize,
                height as usize,
                |y| raw[y * row_len..(y + 1) * row_len].to_vec(),
            )
        } else {
            let kernel = WarpKernel::with_estimate(&params, width, height, ca);
            let source = SourceRows::whole(raw, width, height);
            crate::edge_fringe::full_resolution_radius(
                adjustments,
                width as usize,
                height as usize,
                |y| {
                    let mut row = vec![0.0f32; row_len];
                    kernel.warp_row(&source, y as u32, 0, &mut row);
                    row
                },
            )
        };
        Self { ca, fringe_radius }
    }

    /// What the measures of `adjustments` depend on besides the photograph:
    /// None when a region needs none.
    pub fn inputs_key(adjustments: &Value) -> Option<u64> {
        use std::hash::{Hash, Hasher};
        let params = get_geometry_params_from_json(adjustments);
        let fringe = crate::edge_fringe::version_of(adjustments);
        if !params.auto_ca && fringe == 0 {
            return None;
        }
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        fringe.hash(&mut hasher);
        if params.auto_ca {
            params.auto_ca_version.hash(&mut hasher);
            if params.auto_ca_versus_own {
                let own = own_tca_model(&params);
                for v in [own.red.0, own.red.1, own.blue.0, own.blue.1] {
                    v.to_bits().hash(&mut hasher);
                }
            }
        }
        // The fringe radius is measured on the warped photograph: callers
        // key it with the geometry too.
        Some(hasher.finish())
    }
}

/// How a region of the transformed frame is made (see
/// [`plan_transformed_region`]).
#[derive(Clone, Debug, PartialEq)]
pub struct RegionPlan {
    /// The transformed frame's size (warped, turned, flipped, straightened,
    /// cropped).
    pub frame: (u32, u32),
    /// The pixels asked for (x, y, w, h in the transformed frame), inside it.
    pub rect: [u32; 4],
    /// The source rows and columns they read (start, end exclusive).
    pub rows: (u32, u32),
    pub cols: (u32, u32),
    /// The warped frame's pixels they read (x, y, w, h).
    warped: [u32; 4],
    spatial: SpatialSteps,
    warp: bool,
    /// The automatic lateral CA estimate the warp uses.
    estimate: Option<MeasuredCa>,
    /// The edge-fringe radius, when the recipe suppresses fringes.
    fringe: Option<usize>,
}

#[derive(Clone, Debug, PartialEq)]
struct SpatialSteps {
    orientation_steps: u8,
    flip_horizontal: bool,
    flip_vertical: bool,
    rotation_degrees: f32,
    crop: Value,
    /// `fused_spatial_transform` (turned, flipped or straightened);
    /// otherwise a plain crop.
    fused: bool,
    /// `GeometryParams::keep_negative`.
    keep_negative: bool,
}

impl SpatialSteps {
    fn of(adjustments: &Value) -> Self {
        let orientation_steps = adjustments["orientationSteps"].as_u64().unwrap_or(0) as u8;
        let rotation_degrees = adjustments["rotation"].as_f64().unwrap_or(0.0) as f32;
        let flip_horizontal = adjustments["flipHorizontal"].as_bool().unwrap_or(false);
        let flip_vertical = adjustments["flipVertical"].as_bool().unwrap_or(false);
        let crop_data: Option<Crop> = serde_json::from_value(adjustments["crop"].clone()).ok();
        let crop = serde_json::to_value(crop_data).unwrap_or(Value::Null);
        let reorients = matches!(orientation_steps, 1..=3) || flip_horizontal || flip_vertical;
        Self {
            orientation_steps,
            flip_horizontal,
            flip_vertical,
            rotation_degrees,
            crop,
            fused: reorients || rotation_degrees % 360.0 != 0.0,
            keep_negative: render_version(adjustments) >= 2,
        }
    }

    fn oriented(&self, width: u32, height: u32) -> OrientedFrame {
        if self.fused {
            OrientedFrame::new(
                width,
                height,
                self.orientation_steps,
                self.flip_horizontal,
                self.flip_vertical,
            )
        } else {
            OrientedFrame::new(width, height, 0, false, false)
        }
    }

    fn straightens(&self) -> bool {
        self.fused && self.rotation_degrees % 360.0 != 0.0
    }
}

/// Plans pixels `rect` (x, y, w, h) of what [`apply_geometry_warp`] and then
/// `adjustment_utils::apply_spatial_transformations` make of a `width` ×
/// `height` source with `adjustments`, so that [`transformed_region`] can
/// make them from the source rows they read instead of the whole frame (a
/// zoomed multi view cell). None when that is not possible: lens blur,
/// automatic lateral CA (measured on the whole photograph), or nothing of
/// `rect` inside the frame.
pub fn plan_transformed_region(
    width: u32,
    height: u32,
    adjustments: &Value,
    rect: [u32; 4],
) -> Option<RegionPlan> {
    plan_transformed_region_with(width, height, adjustments, rect, None)
}

/// [`plan_transformed_region`] with what the whole photograph tells
/// (`RegionMeasures::of` its whole decode): a warp with automatic lateral CA
/// or a recipe that suppresses edge fringes has a plan only with them.
pub fn plan_transformed_region_with(
    width: u32,
    height: u32,
    adjustments: &Value,
    rect: [u32; 4],
    measures: Option<RegionMeasures>,
) -> Option<RegionPlan> {
    if width == 0 || height == 0 || adjustments["lensBlurEnabled"].as_bool().unwrap_or(false) {
        return None;
    }
    let params = get_geometry_params_from_json(adjustments);
    let warp = !is_geometry_identity(&params);
    let estimate = measures.and_then(|m| m.ca).filter(|_| params.auto_ca);
    if warp && params.auto_ca && estimate.is_none() {
        return None;
    }
    let fringe = if crate::edge_fringe::version_of(adjustments) > 0 {
        Some(measures?.fringe_radius?)
    } else {
        None
    };
    let spatial = SpatialSteps::of(adjustments);
    let frame = spatial.oriented(width, height);
    let (frame_w, frame_h) = (frame.width, frame.height);
    let (crop_x, crop_y, out_w, out_h) =
        crop_rect(&spatial.crop, frame_w, frame_h).unwrap_or((0, 0, frame_w, frame_h));
    if out_w == 0 || out_h == 0 {
        return None;
    }
    let x0 = rect[0].min(out_w - 1);
    let y0 = rect[1].min(out_h - 1);
    let x1 = rect[0].saturating_add(rect[2]).clamp(x0 + 1, out_w);
    let y1 = rect[1].saturating_add(rect[3]).clamp(y0 + 1, out_h);
    let rect = [x0, y0, x1 - x0, y1 - y0];

    // The oriented frame's pixels the output reads (inclusive bounds).
    let (fx0, fy0, fx1, fy1) = if spatial.straightens() {
        let inverse = rotation_inverse(spatial.rotation_degrees, frame_w, frame_h);
        let corners = [
            ((crop_x + x0) as f32, (crop_y + y0) as f32),
            ((crop_x + x1 - 1) as f32, (crop_y + y0) as f32),
            ((crop_x + x0) as f32, (crop_y + y1 - 1) as f32),
            ((crop_x + x1 - 1) as f32, (crop_y + y1 - 1) as f32),
        ];
        let (mut min_x, mut min_y, mut max_x, mut max_y) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for corner in corners {
            let (px, py) = inverse * corner;
            min_x = min_x.min(px);
            min_y = min_y.min(py);
            max_x = max_x.max(px);
            max_y = max_y.max(py);
        }
        if !(min_x.is_finite() && min_y.is_finite() && max_x.is_finite() && max_y.is_finite()) {
            return None;
        }
        // Bicubic taps reach one pixel before and two after; one more for
        // the rounding of interior points (an affine map's extremes are its
        // corners).
        let clamp_x = |v: f32| (v as i64).clamp(0, frame_w as i64 - 1) as u32;
        let clamp_y = |v: f32| (v as i64).clamp(0, frame_h as i64 - 1) as u32;
        (
            clamp_x(min_x.floor() - 2.0),
            clamp_y(min_y.floor() - 2.0),
            clamp_x(max_x.floor() + 3.0),
            clamp_y(max_y.floor() + 3.0),
        )
    } else {
        (crop_x + x0, crop_y + y0, crop_x + x1 - 1, crop_y + y1 - 1)
    };
    // Turned and flipped back to the warped frame (an axis-aligned map: its
    // extremes are the corners).
    let (mut wx0, mut wy0, mut wx1, mut wy1) = (i64::MAX, i64::MAX, i64::MIN, i64::MIN);
    for (x, y) in [(fx0, fy0), (fx1, fy0), (fx0, fy1), (fx1, fy1)] {
        let (sx, sy) = frame.source_xy(x as i64, y as i64);
        wx0 = wx0.min(sx);
        wy0 = wy0.min(sy);
        wx1 = wx1.max(sx);
        wy1 = wy1.max(sy);
    }
    // Fringe suppression reads around each pixel: the warped pixels it
    // reads too (clamped to the frame, where the whole frame clamps alike).
    let margin = fringe.map_or(0, crate::edge_fringe::reach) as i64;
    let (wx0, wy0) = ((wx0 - margin).max(0), (wy0 - margin).max(0));
    let (wx1, wy1) = (
        (wx1 + margin).min(width as i64 - 1),
        (wy1 + margin).min(height as i64 - 1),
    );
    let warped = [
        wx0 as u32,
        wy0 as u32,
        (wx1 - wx0 + 1) as u32,
        (wy1 - wy0 + 1) as u32,
    ];

    let (cols, rows) = if warp {
        let kernel = WarpKernel::with_estimate(&params, width, height, estimate);
        let none = [f32::MAX, f32::MIN, f32::MAX, f32::MIN];
        let area = (warped[1]..warped[1] + warped[3])
            .into_par_iter()
            .map(|y| {
                let mut current_vec = kernel.row_start(y);
                let mut area = none;
                for x in 0..warped[0] + warped[2] {
                    if x >= warped[0]
                        && let Some((sx, sy)) = kernel.source_point(&current_vec)
                        && let Some(a) = kernel.sampled_area(sx, sy, width, height)
                    {
                        area = [
                            area[0].min(a[0]),
                            area[1].max(a[1]),
                            area[2].min(a[2]),
                            area[3].max(a[3]),
                        ];
                    }
                    current_vec += kernel.step_vec_x;
                }
                area
            })
            .reduce(
                || none,
                |a, b| {
                    [
                        a[0].min(b[0]),
                        a[1].max(b[1]),
                        a[2].min(b[2]),
                        a[3].max(b[3]),
                    ]
                },
            );
        if area[0] > area[1] {
            // Nothing is sampled (all outside): any pixel will do.
            ((0, 1), (0, 1))
        } else {
            // Bicubic taps one pixel before and two after (bilinear and the
            // border cases one after); a region source starting more than
            // two pixels inside keeps the whole frame's border decisions.
            let span = |lo: f32, hi: f32, size: u32| {
                let start = (lo.floor() as i64 - 2).clamp(0, size as i64 - 1) as u32;
                let end = (hi.floor() as i64 + 4).clamp(start as i64 + 1, size as i64) as u32;
                (start, end)
            };
            (
                span(area[0], area[1], width),
                span(area[2], area[3], height),
            )
        }
    } else {
        (
            (warped[0], warped[0] + warped[2]),
            (warped[1], warped[1] + warped[3]),
        )
    };
    Some(RegionPlan {
        frame: (out_w, out_h),
        rect,
        rows,
        cols,
        warped,
        spatial,
        warp,
        estimate,
        fringe,
    })
}

/// The pixels `plan` asks for, from `source` (which must hold `plan.rows`):
/// bit-identical to the same pixels of the whole frame warped and
/// transformed (`apply_geometry_warp`, `apply_spatial_transformations`).
/// None when `source` does not hold the rows or does not fit the plan.
pub fn transformed_region(
    source: &SourceRows,
    adjustments: &Value,
    plan: &RegionPlan,
) -> Option<DynamicImage> {
    let (width, height) = (source.width, source.height);
    let source_rows = source.rows();
    if source.row0 > plan.rows.0
        || source.row0 + source_rows < plan.rows.1
        || source.col0 > plan.cols.0
        || source.col0 + source.cols < plan.cols.1
        || source.col0 + source.cols > width
        || source.pixels.len() != source_rows as usize * source.cols as usize * 3
    {
        return None;
    }
    let [wx, wy, ww, wh] = plan.warped;
    let row_len = ww as usize * 3;
    // The warped frame's pixels `warped`.
    let mut warped = vec![0.0f32; row_len * wh as usize];
    if plan.warp {
        let params = get_geometry_params_from_json(adjustments);
        let kernel = WarpKernel::with_estimate(&params, width, height, plan.estimate);
        warped
            .par_chunks_exact_mut(row_len)
            .enumerate()
            .for_each(|(r, row)| {
                kernel.warp_row(source, wy + r as u32, wx, row);
            });
    } else {
        let stride = source.cols as usize * 3;
        warped
            .par_chunks_exact_mut(row_len)
            .enumerate()
            .for_each(|(r, row)| {
                let start = (wy + r as u32 - source.row0) as usize * stride
                    + (wx - source.col0) as usize * 3;
                row.copy_from_slice(&source.pixels[start..start + row_len]);
            });
    }

    if let Some(r) = plan.fringe {
        crate::edge_fringe::suppress(&mut warped, ww as usize, wh as usize, 3, r);
    }

    let spatial = &plan.spatial;
    let frame = spatial.oriented(width, height);
    let (crop_x, crop_y, _, _) = crop_rect(&spatial.crop, frame.width, frame.height).unwrap_or((
        0,
        0,
        frame.width,
        frame.height,
    ));
    let [x0, y0, w, h] = plan.rect;
    let missed = std::sync::atomic::AtomicBool::new(false);
    // The warped pixel at oriented-frame (x, y); a miss (outside `warped`)
    // makes the region fail rather than differ.
    let warped_at = |x: i64, y: i64| -> Option<&[f32]> {
        let (sx, sy) = frame.source_xy(x, y);
        let (lx, ly) = (sx - wx as i64, sy - wy as i64);
        if lx < 0 || ly < 0 || lx >= ww as i64 || ly >= wh as i64 {
            missed.store(true, std::sync::atomic::Ordering::Relaxed);
            return None;
        }
        let i = (ly as usize * ww as usize + lx as usize) * 3;
        Some(&warped[i..i + 3])
    };
    let image = if spatial.straightens() {
        let (frame_w, frame_h) = (frame.width, frame.height);
        let inverse = rotation_inverse(spatial.rotation_degrees, frame_w, frame_h);
        let fetch = |x: i64, y: i64| -> [f32; 4] {
            if x < 0 || x >= frame_w as i64 || y < 0 || y >= frame_h as i64 {
                return [0.0; 4];
            }
            match warped_at(x, y) {
                Some(p) => [p[0], p[1], p[2], 1.0],
                None => [0.0; 4],
            }
        };
        let mut output = vec![0f32; w as usize * h as usize * 4];
        output
            .par_chunks_mut(w as usize * 4)
            .enumerate()
            .for_each(|(row, destination)| {
                let y = (crop_y + y0 + row as u32) as f32;
                for (column, pixel) in destination.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                    let x = (crop_x + x0 + column as u32) as f32;
                    let (px, py) = inverse * (x, y);
                    *pixel = sample_bicubic_rgba(&fetch, px, py, spatial.keep_negative);
                }
            });
        DynamicImage::ImageRgba32F(image::ImageBuffer::from_raw(w, h, output)?)
    } else {
        let mut output = vec![0f32; w as usize * h as usize * 3];
        output
            .par_chunks_mut(w as usize * 3)
            .enumerate()
            .for_each(|(row, destination)| {
                let y = (crop_y + y0 + row as u32) as i64;
                for (column, pixel) in destination.as_chunks_mut::<3>().0.iter_mut().enumerate() {
                    let x = (crop_x + x0 + column as u32) as i64;
                    if let Some(p) = warped_at(x, y) {
                        pixel.copy_from_slice(p);
                    }
                }
            });
        DynamicImage::ImageRgb32F(image::ImageBuffer::from_raw(w, h, output)?)
    };
    (!missed.load(std::sync::atomic::Ordering::Relaxed)).then_some(image)
}

pub fn apply_flip<'a>(
    image: impl IntoCowImage<'a>,
    horizontal: bool,
    vertical: bool,
) -> Cow<'a, DynamicImage> {
    let image = image.into_cow();
    if !horizontal && !vertical {
        return image;
    }

    let mut img = image.into_owned();
    if horizontal {
        img = img.fliph();
    }
    if vertical {
        img = img.flipv();
    }
    Cow::Owned(img)
}

pub fn is_geometry_identity(params: &GeometryParams) -> bool {
    if params.guided_perspective_enabled && params.guided_lines.len() >= 2 {
        return false;
    }

    let dist_identity = !params.lens_distortion_enabled
        || ((params.lens_distortion_amount - 1.0).abs() < 1e-4
            && params.lens_dist_k1.abs() < 1e-6
            && params.lens_dist_k2.abs() < 1e-6
            && params.lens_dist_k3.abs() < 1e-6);

    let tca_identity = !params.auto_ca
        && (!params.lens_tca_enabled
            || ((params.lens_tca_amount - 1.0).abs() < 1e-4
                && (params.tca_vr - 1.0).abs() < 1e-6
                && (params.tca_vb - 1.0).abs() < 1e-6
                && params.tca_vr2.abs() < 1e-9
                && params.tca_vb2.abs() < 1e-9));

    let vig_identity = !params.lens_vignette_enabled
        || ((params.lens_vignette_amount - 1.0).abs() < 1e-4
            && params.vig_k1.abs() < 1e-6
            && params.vig_k2.abs() < 1e-6
            && params.vig_k3.abs() < 1e-6);

    params.distortion == 0.0
        && params.vertical == 0.0
        && params.horizontal == 0.0
        && params.rotate == 0.0
        && params.aspect == 0.0
        && params.scale == 100.0
        && params.x_offset == 0.0
        && params.y_offset == 0.0
        && dist_identity
        && tca_identity
        && vig_identity
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct AutoAdjustmentResults {
    pub exposure: f64,
    pub brightness: f64,
    pub contrast: f64,
    pub highlights: f64,
    pub shadows: f64,
    pub vibrancy: f64,
    pub vignette_amount: f64,
    pub temperature: f64,
    pub tint: f64,
    pub dehaze: f64,
    pub clarity: f64,
    pub centre: f64,
    pub blacks: f64,
    pub whites: f64,
    /// Levels black point on 0..255 (Standard auto only).
    #[serde(default)]
    pub black_point: f64,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, Pod, Zeroable, Default)]
#[repr(C)]
pub struct Point {
    pub x: f32,
    pub y: f32,
    _pad1: f32,
    _pad2: f32,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, Pod, Zeroable, Default)]
#[repr(C)]
pub struct HslColor {
    pub hue: f32,
    pub saturation: f32,
    pub luminance: f32,
    _pad: f32,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, Pod, Zeroable, Default)]
#[repr(C)]
pub struct ColorGradeSettings {
    pub hue: f32,
    pub saturation: f32,
    pub luminance: f32,
    _pad: f32,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, Pod, Zeroable, Default)]
#[repr(C)]
pub struct ColorCalibrationSettings {
    pub shadows_tint: f32,
    pub red_hue: f32,
    pub red_saturation: f32,
    pub green_hue: f32,
    pub green_saturation: f32,
    pub blue_hue: f32,
    pub blue_saturation: f32,
    _pad1: f32,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, Pod, Zeroable)]
#[repr(C)]
pub struct GpuMat3 {
    col0: [f32; 4],
    col1: [f32; 4],
    col2: [f32; 4],
}

impl Default for GpuMat3 {
    fn default() -> Self {
        Self {
            col0: [1.0, 0.0, 0.0, 0.0],
            col1: [0.0, 1.0, 0.0, 0.0],
            col2: [0.0, 0.0, 1.0, 0.0],
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, Pod, Zeroable, Default)]
#[repr(C)]
pub struct GlobalAdjustments {
    pub exposure: f32,
    pub brightness: f32,
    pub contrast: f32,
    pub highlights: f32,
    pub shadows: f32,
    pub whites: f32,
    pub blacks: f32,
    pub saturation: f32,
    pub temperature: f32,
    pub tint: f32,
    pub vibrance: f32,
    pub hue: f32,
    /// The share of colour noise version 7's speck swap applied
    /// (`color_noise::Tuning::speck_strength`), set by the renderer with
    /// `color_noise`.
    pub color_noise_speck: f32,
    /// 1 when the recipe's luminance noise reduction also takes out
    /// isolated luminance specks (`lumaNoiseVersion` 2), else 0.
    pub luma_speck: f32,
    _pad_color3: f32,

    pub sharpness: f32,
    pub luma_noise_reduction: f32,
    pub color_noise_reduction: f32,
    pub clarity: f32,
    pub dehaze: f32,
    pub structure: f32,
    pub centré: f32,
    pub vignette_amount: f32,
    pub vignette_midpoint: f32,
    pub vignette_roundness: f32,
    pub vignette_feather: f32,
    pub grain_amount: f32,
    pub grain_size: f32,
    pub grain_roughness: f32,

    pub chromatic_aberration_red_cyan: f32,
    pub chromatic_aberration_blue_yellow: f32,
    pub show_clipping: u32,
    pub is_raw_image: u32,
    /// Grain structure (recipe key `grainStructure`, see
    /// `grain_structure_index`); 0 = Even, the original grain.
    pub grain_structure: u32,

    pub has_lut: u32,
    pub lut_intensity: f32,
    pub tonemapper_mode: u32,
    pub lut_is_scene_referred: u32,
    /// 90th and 10th percentile of the decode's linear luminance
    /// (image_key.rs), before exposure and baseline; 0 = unknown. Shadows
    /// adapts to them.
    pub image_key: f32,
    pub image_key_low: f32,
    /// The recipe's `toneVersion` (1 when missing): 2 lets Shadows above +50
    /// reach further into the quarter-tones (docs/RECIPE_SCHEMA.md).
    pub tone_version: f32,

    /// EV applied before the Standard tone mapper (DNG BaselineExposure).
    pub baseline_exposure: f32,
    /// Luminance noise of the full-resolution image (std of fine detail in
    /// square-root encoding, see noise_level.rs); 0 = unknown.
    pub noise_level: f32,
    /// Capture One-style curve choice: 0 standard, 1 high contrast,
    /// 2 extra shadow, 3 linear response (recipe key `baseCurve`).
    pub base_curve: f32,
    pub agx_pipe_to_rendering_matrix: GpuMat3,
    pub agx_rendering_to_pipe_matrix: GpuMat3,

    /// Squared range of the multi-scale colour noise reduction's aware
    /// expansion of the rebuilt level 1 (`Tuning::finest_aware2`), set by
    /// the renderer with `color_noise`; 0 = plain expansion.
    pub color_noise_aware2: f32,
    /// `Tuning::aware_far` for that expansion.
    pub color_noise_aware_far: f32,
    /// The guide of that expansion (`Tuning::finest_guide_code`): 0 the
    /// pixel's own chroma, 1 its 3 × 3 mean, 2 its 3 × 3 median, 3 the median
    /// where the pixel is farther than `color_noise_guide_k2`'s root times the
    /// noise from it; + 4 when the detail kept only by the finest edge guard
    /// is the guide's. Set by the renderer with `color_noise`.
    pub color_noise_guide: f32,
    pub color_noise_guide_k2: f32,
    pub color_grading_shadows: ColorGradeSettings,
    pub color_grading_midtones: ColorGradeSettings,
    pub color_grading_highlights: ColorGradeSettings,
    pub color_grading_global: ColorGradeSettings,
    pub color_grading_blending: f32,
    pub color_grading_balance: f32,
    /// 1 when `grading_lut` holds Lightroom-style colour grading
    /// (color_grading.rs); the legacy fields above are then zero.
    pub grading_active: u32,
    /// 1 when the photo is converted to black and white (`bw_mix`).
    pub bw_enabled: u32,

    pub color_calibration: ColorCalibrationSettings,

    pub hsl: [HslColor; 8],
    pub luma_curve: [Point; 16],
    pub red_curve: [Point; 16],
    pub green_curve: [Point; 16],
    pub blue_curve: [Point; 16],
    pub luma_curve_count: u32,
    pub red_curve_count: u32,
    pub green_curve_count: u32,
    pub blue_curve_count: u32,
    /// Preview pixels per original pixel (0 = full resolution); pixel-sized
    /// detail operations (capture sharpening) use it.
    pub detail_scale: f32,
    /// Lightroom's sharpening Radius, Detail and Masking as offsets from
    /// its defaults (1.0, 25, 0), so zeroed adjustments sharpen as before.
    pub sharpen_radius_offset: f32,
    pub sharpen_detail_offset: f32,
    pub sharpen_masking: f32,

    pub glow_amount: f32,
    pub halation_amount: f32,
    pub flare_amount: f32,
    pub sharpness_threshold: f32,
    /// Per-image base tone curve for the Camera Raw look: display EV every
    /// 0.25 scene EV from −12 EV (49 values, row-major), element 63 = 1 when
    /// set. Unset: the default tone curve (baseline_exposure.rs).
    pub profile_curve: [[f32; 4]; 16],
    /// Colour grading as Oklab (dL, da, db) shifts by display Oklab L at
    /// i / (GRADING_LUT_SIZE - 1) (color_grading.rs).
    pub grading_lut: [[f32; 4]; GRADING_LUT_SIZE],
    /// Black & white mix for Lightroom's eight channels (red, orange,
    /// yellow, green, aqua, blue, purple, magenta), slider / 100.
    pub bw_mix: [[f32; 4]; 2],
    /// Capture One's Levels on display values for RGB, R, G, B: (input
    /// black, input white, 1 / gamma, output black) in 0–1, and output white
    /// in `levels_white`; `levels_active` x = 1 when any channel is not
    /// identity.
    pub levels: [[f32; 4]; 4],
    pub levels_white: [f32; 4],
    pub levels_active: [u32; 4],
    /// Lightroom's Defringe: purple amount (0–1), hue range low/high in
    /// Oklab degrees, then green amount and range.
    pub defringe: [[f32; 4]; 2],
    /// Colour editor ranges, four vec4 each (see `color_ranges`):
    /// (hue°, chroma, lightness, lightness half-width), (hue half-width,
    /// chroma low, chroma high, softness), (hue shift°, saturation,
    /// lightness shift, 0), (hue, saturation, lightness uniformity, 0).
    pub color_ranges: [[f32; 4]; 32],
    /// x: number of ranges.
    pub color_range_count: [u32; 4],
    /// Lightroom's region tone curve as 256 output values (region_curve.rs);
    /// `region_curve_active` x = 1 when it is not identity.
    /// (Two halves of 32 vec4: serde and Default stop at 32 elements.)
    pub region_curve: [[[f32; 4]; 32]; 2],
    pub region_curve_active: [u32; 4],
    /// Lightroom's Calibration (calibration.rs) for the Standard and Camera
    /// Raw looks: linear ProPhoto matrix, and shadows tint (amount −1…1,
    /// strength, sigma, power); `calibration_active` x = 1 when set. Classic
    /// and AgX edits keep `color_calibration`.
    pub calibration_matrix: GpuMat3,
    pub calibration_tint: [f32; 4],
    pub calibration_active: [u32; 4],
    /// Lightroom's post-crop vignette style (x: 0 highlight priority, 1
    /// colour priority, 2 paint overlay) and Highlights (y: slider × 10).
    pub vignette_style: [u32; 4],
    /// x: share of the calibration matrix's luminance change undone.
    pub calibration_luma: [f32; 4],
    /// x: Capture One's Sharpness Falloff (0–1): extra sharpening towards
    /// the corners, up to twice the amount there. y, z: diffraction
    /// correction radius (original pixels) and amount (diffraction.rs).
    /// w: the recipe's sharpening response (`sharpenVersion`, 1 or 2).
    pub sharpen_falloff: [f32; 4],
    /// Camera profile (camera_profile.rs). Before the base look: x = key of
    /// the recipe's `cameraProfile`. After it: x = 1 when applied, y = key
    /// of its GPU tables, z = entry offset of the LookTable.
    pub camera_profile: [u32; 4],
    /// Decoded linear sRGB → the profile's linear sRGB.
    pub camera_profile_matrix: GpuMat3,
    /// HueSatMap and LookTable: hue, saturation, value divisions and
    /// sRGB-encoded value (zero when absent).
    pub camera_profile_hue_sat: [u32; 4],
    pub camera_profile_look: [u32; 4],
    /// Moiré (Capture One): x amount 0–1, y sampling step in original
    /// pixels (from Pattern).
    pub moire: [f32; 4],
    /// White balance in Kelvin (camera_profile.rs): x = 1 when set, y
    /// temperature, z tint; cleared once applied.
    pub white_balance: [f32; 4],
    /// Lightroom's noise reduction refinements, 0–1: luminance Detail,
    /// luminance Contrast, colour Detail, colour Smoothness.
    pub noise_detail: [f32; 4],
    /// Output colour space (output_color_space.rs): x = shader code of the
    /// export's or the display's space (0 = sRGB), ending with
    /// `output_matrix` and the space's transfer curve. Soft proofing of the
    /// display: y = code of the proofed space + 1 (0 = off), z = 1 marks
    /// colours outside it.
    pub output_space: [u32; 4],
    /// Linear sRGB → the output space's linear RGB.
    pub output_matrix: GpuMat3,
    /// Linear sRGB → the proofed space's linear RGB, and back.
    pub proof_matrix: GpuMat3,
    pub proof_inverse: GpuMat3,
    /// Four projected lights, five aligned vec4s each (relight.rs).
    pub relight: [[f32; 4]; 20],
    /// Registered depth (xy) and imported-pattern atlas (zw) keys.
    pub relight_depth: [u32; 4],
    /// Normal strength, original light, refined lighting flag, face reflection amount.
    pub relight_surface: [f32; 4],
    pub relight_camera: [f32; 4],
    pub relight_geometry: [f32; 4],
    /// Colour noise reduction on the chroma pyramid (color_noise.rs): x =
    /// the recipe's `colorNoiseVersion` − 1 for versions 2 and up (the
    /// shader only tests x > 0.5; the renderer picks the version's
    /// `Tuning` by it), 0 otherwise; the renderer sets y to the
    /// finest level's threshold, z to the gain's steepness and w to the
    /// finest level's squared chroma-edge guard threshold once the pyramid
    /// of its source is measured (x is cleared if it has none).
    pub color_noise: [f32; 4],
    /// The level-1 window of the renderer's chroma texture (origin xy) and
    /// level 1's size (zw); set by the renderer.
    pub color_noise_window: [u32; 4],
    /// The luminance noise by brightness relative to its median
    /// (`ChromaPyramid::luma_profile`, 8 bins of square-root luminance); set
    /// by the renderer with the colour noise pyramid, all 0 without it.
    pub luma_noise_profile: [[f32; 4]; 2],
    /// A version-five Relight subject: the photo's own lighting on its
    /// people, flags and the subject's depth (`relight::source_uniforms`).
    pub relight_source: [[f32; 4]; 4],
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, Pod, Zeroable, Default)]
#[repr(C)]
pub struct MaskAdjustments {
    pub exposure: f32,
    pub brightness: f32,
    pub contrast: f32,
    pub highlights: f32,
    pub shadows: f32,
    pub whites: f32,
    pub blacks: f32,
    pub saturation: f32,
    pub temperature: f32,
    pub tint: f32,
    pub vibrance: f32,

    pub sharpness: f32,
    pub luma_noise_reduction: f32,
    pub color_noise_reduction: f32,
    pub clarity: f32,
    pub dehaze: f32,
    pub structure: f32,

    pub glow_amount: f32,
    pub halation_amount: f32,
    pub flare_amount: f32,
    pub sharpness_threshold: f32,

    pub hue: f32,
    /// Lightroom's local Defringe, −1…1: above 0 colour along edges loses
    /// chroma whatever its hue, below 0 the photo's Defringe is held back.
    pub defringe: f32,
    _pad_cg2: f32,
    pub color_grading_shadows: ColorGradeSettings,
    pub color_grading_midtones: ColorGradeSettings,
    pub color_grading_highlights: ColorGradeSettings,
    pub color_grading_global: ColorGradeSettings,
    pub color_grading_blending: f32,
    pub color_grading_balance: f32,
    pub grading_active: u32,
    /// Number of colour editor ranges in `color_ranges` (at most four).
    pub color_range_count: u32,

    pub hsl: [HslColor; 8],
    pub luma_curve: [Point; 16],
    pub red_curve: [Point; 16],
    pub green_curve: [Point; 16],
    pub blue_curve: [Point; 16],
    pub luma_curve_count: u32,
    pub red_curve_count: u32,
    pub green_curve_count: u32,
    pub blue_curve_count: u32,
    _pad_end4: f32,
    _pad_end5: f32,
    _pad_end6: f32,
    _pad_end7: f32,
    pub grading_lut: [[f32; 4]; GRADING_LUT_SIZE],
    /// Colour editor ranges of the layer, packed like the photo's.
    pub color_ranges: [[f32; 4]; 16],
    /// Moiré of the layer, like the photo's (`moire_settings`).
    pub moire: [f32; 4],
}

pub const MAX_MASKS: usize = 32;
pub use crate::color_grading::GRADING_LUT_SIZE;

#[derive(Debug, Clone, Copy, Pod, Zeroable, Default)]
#[repr(C)]
pub struct AllAdjustments {
    pub global: GlobalAdjustments,
    pub mask_adjustments: [MaskAdjustments; MAX_MASKS],
    pub mask_count: u32,
    pub tile_offset_x: u32,
    pub tile_offset_y: u32,
    pub mask_atlas_cols: u32,
    /// Tile-local origin and dimensions of the pixels that will be retained.
    pub render_region: [u32; 4],
    /// x != 0: the bound detail texture holds the detail stage (noise
    /// reduction and sharpening at the global amounts) for this tile.
    pub detail_cache: [u32; 4],
    /// Origin (x, y) of the photo window the input texture holds and the
    /// photo's size (width, height); the renderer sets it for every tile.
    pub source_window: [u32; 4],
}

/// CPU-owned render inputs. Uniforms remain POD; the geometry allocation stays
/// alive until the last queued render using this preparation has completed.
#[derive(Debug, Clone, Default)]
pub struct PreparedAdjustments {
    pub uniforms: AllAdjustments,
    pub relight_surface: Option<std::sync::Arc<crate::relight::DepthSurface>>,
    /// Imported Relight patterns (atlas key in `global.relight_depth[2..]`).
    pub relight_patterns: Option<std::sync::Arc<image::GrayImage>>,
    /// The blurs read their whole radius at tile edges (`renderVersion` 2);
    /// before, the large blur stopped at the tiles' 128-pixel overlap.
    pub whole_blur_radius: bool,
}
impl std::ops::Deref for PreparedAdjustments {
    type Target = AllAdjustments;
    fn deref(&self) -> &Self::Target {
        &self.uniforms
    }
}
impl std::ops::DerefMut for PreparedAdjustments {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.uniforms
    }
}
impl From<AllAdjustments> for PreparedAdjustments {
    fn from(uniforms: AllAdjustments) -> Self {
        let relight_surface = crate::relight::depth(uniforms.global.relight_depth);
        let [.., low, high] = uniforms.global.relight_depth;
        let relight_patterns = crate::relight::pattern_atlas([low, high]);
        Self {
            uniforms,
            relight_surface,
            relight_patterns,
            whole_blur_radius: false,
        }
    }
}

struct AdjustmentScales {
    exposure: f32,
    brightness: f32,
    contrast: f32,
    highlights: f32,
    shadows: f32,
    whites: f32,
    blacks: f32,
    saturation: f32,
    temperature: f32,
    tint: f32,
    vibrance: f32,

    sharpness: f32,
    sharpness_threshold: f32,
    luma_noise_reduction: f32,
    color_noise_reduction: f32,
    clarity: f32,
    dehaze: f32,
    structure: f32,
    centré: f32,

    vignette_amount: f32,
    vignette_midpoint: f32,
    vignette_roundness: f32,
    vignette_feather: f32,
    grain_amount: f32,
    grain_size: f32,
    grain_roughness: f32,

    chromatic_aberration: f32,

    hsl_hue_multiplier: f32,
    hsl_saturation: f32,
    hsl_luminance: f32,

    color_grading_saturation: f32,
    color_grading_luminance: f32,
    color_grading_blending: f32,
    color_grading_balance: f32,

    color_calibration_hue: f32,
    color_calibration_saturation: f32,

    glow: f32,
    halation: f32,
    flares: f32,
}

/// Import defaults for RAW files (frontend `RAW_DETAIL_DEFAULTS`):
/// Lightroom's sharpening and colour noise reduction, and a light luminance
/// noise reduction: Camera Raw and Capture One render visibly cleaner at
/// their defaults (Camera Raw's Luminance 0 has 0.5–0.8 of Rawtonic's
/// unfiltered noise at ISO 6400–12800); 15 brings Rawtonic's absolute noise
/// near Camera Raw's and below Capture One's default on an ISO 200 GFX file
/// (notes: reference/detail.md).
pub const RAW_DEFAULT_SHARPNESS: f64 = 40.0;
pub const RAW_DEFAULT_COLOR_NOISE_REDUCTION: f64 = 25.0;
pub const RAW_DEFAULT_LUMA_NOISE_REDUCTION: f64 = 15.0;

const SCALES: AdjustmentScales = AdjustmentScales {
    exposure: 0.8,
    brightness: 0.8,
    contrast: 100.0,
    highlights: 120.0,
    shadows: 120.0,
    whites: 30.0,
    blacks: 40.0,
    saturation: 100.0,
    temperature: 25.0,
    tint: 100.0,
    vibrance: 100.0,

    sharpness: 50.0,
    sharpness_threshold: 100.0,
    luma_noise_reduction: 100.0,
    color_noise_reduction: 100.0,
    clarity: 125.0,
    dehaze: 750.0,
    structure: 125.0,
    centré: 250.0,

    vignette_amount: 100.0,
    vignette_midpoint: 100.0,
    vignette_roundness: 100.0,
    vignette_feather: 100.0,
    grain_amount: 200.0,
    grain_size: 50.0,
    grain_roughness: 100.0,

    chromatic_aberration: 10000.0,

    hsl_hue_multiplier: 0.3,
    hsl_saturation: 100.0,
    hsl_luminance: 100.0,

    color_grading_saturation: 500.0,
    color_grading_luminance: 500.0,
    color_grading_blending: 100.0,
    color_grading_balance: 200.0,

    color_calibration_hue: 400.0,
    color_calibration_saturation: 120.0,

    glow: 100.0,
    halation: 100.0,
    flares: 100.0,
};

fn parse_hsl_adjustments(js_hsl: &serde_json::Value) -> [HslColor; 8] {
    let mut hsl_array = [HslColor::default(); 8];
    if let Some(hsl_map) = js_hsl.as_object() {
        let color_map = [
            ("reds", 0),
            ("oranges", 1),
            ("yellows", 2),
            ("greens", 3),
            ("aquas", 4),
            ("blues", 5),
            ("purples", 6),
            ("magentas", 7),
        ];
        for (name, index) in color_map.iter() {
            if let Some(color_data) = hsl_map.get(*name) {
                hsl_array[*index] = HslColor {
                    hue: color_data["hue"].as_f64().unwrap_or(0.0) as f32
                        * SCALES.hsl_hue_multiplier,
                    saturation: color_data["saturation"].as_f64().unwrap_or(0.0) as f32
                        / SCALES.hsl_saturation,
                    luminance: color_data["luminance"].as_f64().unwrap_or(0.0) as f32
                        / SCALES.hsl_luminance,
                    _pad: 0.0,
                };
            }
        }
    }
    hsl_array
}

fn parse_color_grade_settings(js_cg: &serde_json::Value) -> ColorGradeSettings {
    if js_cg.is_null() {
        return ColorGradeSettings::default();
    }
    ColorGradeSettings {
        hue: js_cg["hue"].as_f64().unwrap_or(0.0) as f32,
        saturation: js_cg["saturation"].as_f64().unwrap_or(0.0) as f32
            / SCALES.color_grading_saturation,
        luminance: js_cg["luminance"].as_f64().unwrap_or(0.0) as f32
            / SCALES.color_grading_luminance,
        _pad: 0.0,
    }
}

fn convert_points_to_aligned(frontend_points: Vec<serde_json::Value>) -> [Point; 16] {
    let mut aligned_points = [Point::default(); 16];
    for (i, point) in frontend_points.iter().enumerate().take(16) {
        if let (Some(x), Some(y)) = (point["x"].as_f64(), point["y"].as_f64()) {
            aligned_points[i] = Point {
                x: x as f32,
                y: y as f32,
                _pad1: 0.0,
                _pad2: 0.0,
            };
        }
    }
    aligned_points
}

const WP_D65: Vec2 = Vec2::new(0.3127, 0.3290);
const PRIMARIES_SRGB: [Vec2; 3] = [
    Vec2::new(0.64, 0.33),
    Vec2::new(0.30, 0.60),
    Vec2::new(0.15, 0.06),
];
const PRIMARIES_REC2020: [Vec2; 3] = [
    Vec2::new(0.708, 0.292),
    Vec2::new(0.170, 0.797),
    Vec2::new(0.131, 0.046),
];

fn xy_to_xyz(xy: Vec2) -> Vec3 {
    if xy.y < 1e-6 {
        Vec3::ZERO
    } else {
        Vec3::new(xy.x / xy.y, 1.0, (1.0 - xy.x - xy.y) / xy.y)
    }
}

fn primaries_to_xyz_matrix(primaries: &[Vec2; 3], white_point: Vec2) -> Mat3 {
    let r_xyz = xy_to_xyz(primaries[0]);
    let g_xyz = xy_to_xyz(primaries[1]);
    let b_xyz = xy_to_xyz(primaries[2]);
    let primaries_matrix = Mat3::from_cols(r_xyz, g_xyz, b_xyz);
    let white_point_xyz = xy_to_xyz(white_point);
    let s = primaries_matrix.inverse() * white_point_xyz;
    Mat3::from_cols(r_xyz * s.x, g_xyz * s.y, b_xyz * s.z)
}

fn rotate_and_scale_primary(primary: Vec2, white_point: Vec2, scale: f32, rotation: f32) -> Vec2 {
    let p_rel = primary - white_point;
    let p_scaled = p_rel * scale;
    let (sin_r, cos_r) = rotation.sin_cos();
    let p_rotated = Vec2::new(
        p_scaled.x * cos_r - p_scaled.y * sin_r,
        p_scaled.x * sin_r + p_scaled.y * cos_r,
    );
    white_point + p_rotated
}

/// A row-major 3×3 matrix for the shader (columns).
pub fn gpu_mat3_from_rows(m: [[f32; 3]; 3]) -> GpuMat3 {
    GpuMat3 {
        col0: [m[0][0], m[1][0], m[2][0], 0.0],
        col1: [m[0][1], m[1][1], m[2][1], 0.0],
        col2: [m[0][2], m[1][2], m[2][2], 0.0],
    }
}

fn mat3_to_gpu_mat3(m: Mat3) -> GpuMat3 {
    GpuMat3 {
        col0: [m.x_axis.x, m.x_axis.y, m.x_axis.z, 0.0],
        col1: [m.y_axis.x, m.y_axis.y, m.y_axis.z, 0.0],
        col2: [m.z_axis.x, m.z_axis.y, m.z_axis.z, 0.0],
    }
}

fn calculate_agx_matrices_glam() -> (Mat3, Mat3) {
    let pipe_work_profile_to_xyz = primaries_to_xyz_matrix(&PRIMARIES_SRGB, WP_D65);
    let base_profile_to_xyz = primaries_to_xyz_matrix(&PRIMARIES_REC2020, WP_D65);
    let xyz_to_base_profile = base_profile_to_xyz.inverse();
    let pipe_to_base = xyz_to_base_profile * pipe_work_profile_to_xyz;

    let inset = [0.294_624_5, 0.25861925, 0.14641371];
    let rotation = [0.03540329, -0.02108586, -0.06305724];
    let outset = [0.290_776_4, 0.263_155_4, 0.045_810_72];
    let unrotation = [0.03540329, -0.02108586, -0.06305724];
    let master_outset_ratio = 1.0;
    let master_unrotation_ratio = 0.0;

    let mut inset_and_rotated_primaries = [Vec2::ZERO; 3];
    for i in 0..3 {
        inset_and_rotated_primaries[i] =
            rotate_and_scale_primary(PRIMARIES_REC2020[i], WP_D65, 1.0 - inset[i], rotation[i]);
    }
    let rendering_to_xyz = primaries_to_xyz_matrix(&inset_and_rotated_primaries, WP_D65);
    let base_to_rendering = xyz_to_base_profile * rendering_to_xyz;

    let mut outset_and_unrotated_primaries = [Vec2::ZERO; 3];
    for i in 0..3 {
        outset_and_unrotated_primaries[i] = rotate_and_scale_primary(
            PRIMARIES_REC2020[i],
            WP_D65,
            1.0 - master_outset_ratio * outset[i],
            master_unrotation_ratio * unrotation[i],
        );
    }
    let outset_to_xyz = primaries_to_xyz_matrix(&outset_and_unrotated_primaries, WP_D65);
    let temp_matrix = xyz_to_base_profile * outset_to_xyz;
    let rendering_to_base = temp_matrix.inverse();

    let pipe_to_rendering = base_to_rendering * pipe_to_base;
    let rendering_to_pipe = pipe_to_base.inverse() * rendering_to_base;

    (pipe_to_rendering, rendering_to_pipe)
}

fn calculate_agx_matrices() -> (GpuMat3, GpuMat3) {
    let (pipe_to_rendering, rendering_to_pipe) = calculate_agx_matrices_glam();
    (
        mat3_to_gpu_mat3(pipe_to_rendering),
        mat3_to_gpu_mat3(rendering_to_pipe),
    )
}

/// Shader tone mapper: 0 basic (inherited), 1 AgX, 2 Standard (Capture One's
/// base look, notes: reference/tone-controls.md).
pub fn tonemapper_mode_of(name: &str) -> u32 {
    match name {
        "agx" => 1,
        "standard" => 2,
        _ => 0,
    }
}

pub fn apply_cpu_agx_tonemap(image: &mut DynamicImage) {
    const AGX_EPSILON: f32 = 1.0e-6;
    const AGX_MIN_EV: f32 = -15.2;
    const AGX_MAX_EV: f32 = 5.0;
    const AGX_RANGE_EV: f32 = AGX_MAX_EV - AGX_MIN_EV;
    const AGX_GAMMA: f32 = 2.4;
    const AGX_SLOPE: f32 = 2.3843;
    const AGX_TOE_POWER: f32 = 1.5;
    const AGX_SHOULDER_POWER: f32 = 1.5;
    const AGX_TOE_TRANSITION_X: f32 = 0.6060606;
    const AGX_TOE_TRANSITION_Y: f32 = 0.43446;
    const AGX_SHOULDER_TRANSITION_X: f32 = 0.6060606;
    const AGX_SHOULDER_TRANSITION_Y: f32 = 0.43446;
    const AGX_INTERCEPT: f32 = -1.0112;
    const AGX_TOE_SCALE: f32 = -1.0359;
    const AGX_SHOULDER_SCALE: f32 = 1.3475;

    fn agx_sigmoid(x: f32, power: f32) -> f32 {
        x / (1.0 + x.powf(power)).powf(1.0 / power)
    }

    fn agx_scaled_sigmoid(x: f32, scale: f32, slope: f32, power: f32, tx: f32, ty: f32) -> f32 {
        scale * agx_sigmoid(slope * (x - tx) / scale, power) + ty
    }

    fn agx_curve_channel(x: f32) -> f32 {
        let result = if x < AGX_TOE_TRANSITION_X {
            agx_scaled_sigmoid(
                x,
                AGX_TOE_SCALE,
                AGX_SLOPE,
                AGX_TOE_POWER,
                AGX_TOE_TRANSITION_X,
                AGX_TOE_TRANSITION_Y,
            )
        } else if x <= AGX_SHOULDER_TRANSITION_X {
            AGX_SLOPE * x + AGX_INTERCEPT
        } else {
            agx_scaled_sigmoid(
                x,
                AGX_SHOULDER_SCALE,
                AGX_SLOPE,
                AGX_SHOULDER_POWER,
                AGX_SHOULDER_TRANSITION_X,
                AGX_SHOULDER_TRANSITION_Y,
            )
        };
        result.clamp(0.0, 1.0)
    }

    const LUT_SIZE: usize = 4096;
    let mut curve_lut = [0.0f32; LUT_SIZE];
    for (i, slot) in curve_lut.iter_mut().enumerate() {
        let x = i as f32 / (LUT_SIZE - 1) as f32;
        *slot = agx_curve_channel(x).max(0.0).powf(AGX_GAMMA);
    }

    let (pipe_to_rendering, rendering_to_pipe) = calculate_agx_matrices_glam();

    let mut f32_image = image.to_rgb32f();

    f32_image.par_chunks_mut(3).for_each(|pixel_chunk| {
        let r = pixel_chunk[0];
        let g = pixel_chunk[1];
        let b = pixel_chunk[2];

        let min_c = r.min(g).min(b);
        let (r, g, b) = if min_c < 0.0 {
            (r - min_c, g - min_c, b - min_c)
        } else {
            (r, g, b)
        };

        let in_rendering = pipe_to_rendering * Vec3::new(r, g, b);

        let x = Vec3::new(
            (in_rendering.x / 0.18).max(AGX_EPSILON),
            (in_rendering.y / 0.18).max(AGX_EPSILON),
            (in_rendering.z / 0.18).max(AGX_EPSILON),
        );
        let log_encoded = Vec3::new(
            (x.x.log2() - AGX_MIN_EV) / AGX_RANGE_EV,
            (x.y.log2() - AGX_MIN_EV) / AGX_RANGE_EV,
            (x.z.log2() - AGX_MIN_EV) / AGX_RANGE_EV,
        );
        let mapped = Vec3::new(
            log_encoded.x.clamp(0.0, 1.0),
            log_encoded.y.clamp(0.0, 1.0),
            log_encoded.z.clamp(0.0, 1.0),
        );

        let lut_lookup = |v: f32| -> f32 {
            let idx = (v * (LUT_SIZE - 1) as f32) as usize;
            curve_lut[idx.min(LUT_SIZE - 1)]
        };
        let curved = Vec3::new(
            lut_lookup(mapped.x),
            lut_lookup(mapped.y),
            lut_lookup(mapped.z),
        );

        let final_color = rendering_to_pipe * curved;

        pixel_chunk[0] = final_color.x.clamp(0.0, 1.0);
        pixel_chunk[1] = final_color.y.clamp(0.0, 1.0);
        pixel_chunk[2] = final_color.z.clamp(0.0, 1.0);
    });

    *image = DynamicImage::ImageRgb32F(f32_image);
}

pub fn is_image_edited(
    adj: &serde_json::Value,
    is_raw: bool,
    tonemapper_override: Option<u32>,
) -> bool {
    if adj.is_null() || adj.as_object().is_none() {
        return false;
    }

    if let Some(patches) = adj.get("aiPatches").and_then(|v| v.as_array())
        && !patches.is_empty()
    {
        return true;
    }
    if crate::spot_removal::has_spots(adj) || crate::face_retouch::is_active(adj) {
        return true;
    }
    if let Some(masks) = adj.get("masks").and_then(|v| v.as_array())
        && !masks.is_empty()
    {
        return true;
    }

    if let Some(crop_val) = adj.get("crop")
        && !crop_val.is_null()
        && let Ok(crop) = serde_json::from_value::<Crop>(crop_val.clone())
        && (crop.x.abs() > 0.1 || crop.y.abs() > 0.1)
    {
        return true;
    }

    if adj
        .get("orientationSteps")
        .and_then(|v| v.as_u64())
        .unwrap_or(0)
        != 0
    {
        return true;
    }
    if adj
        .get("rotation")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0)
        .abs()
        > 0.001
    {
        return true;
    }
    if adj
        .get("flipHorizontal")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        return true;
    }
    if adj
        .get("flipVertical")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        return true;
    }

    let geo = get_geometry_params_from_json(adj);
    if !is_geometry_identity(&geo) {
        return true;
    }

    let current_adj = get_all_adjustments_from_json(adj, is_raw, tonemapper_override);
    let default_adj =
        get_all_adjustments_from_json(&serde_json::json!({}), is_raw, tonemapper_override);

    bytemuck::bytes_of(&current_adj.uniforms) != bytemuck::bytes_of(&default_adj.uniforms)
}

/// Lightroom-style per-tool on/off switches in `sectionVisibility`, finer
/// than the inherited sections (which stay as master switches). A recipe
/// without the key keeps the tool on.
fn tool_switch(key: &str) -> Option<&'static str> {
    match key {
        "temperature" | "tint" => Some("whiteBalance"),
        "clarity" | "dehaze" | "structure" | "vibrance" | "saturation" => Some("presence"),
        "hsl" => Some("colorMixer"),
        "colorGrading" => Some("colorGrading"),
        "levels" => Some("levels"),
        "colorCalibration" => Some("calibration"),
        "colorRanges" => Some("colorEditor"),
        "sharpness" | "sharpnessThreshold" | "sharpenRadius" | "sharpenDetail"
        | "sharpenMasking" => Some("sharpening"),
        "lumaNoiseReduction" | "colorNoiseReduction" => Some("noiseReduction"),
        _ => None,
    }
}

fn get_global_adjustments_from_json(
    js_adjustments: &serde_json::Value,
    is_raw: bool,
    tonemapper_override: Option<u32>,
) -> GlobalAdjustments {
    let visibility = js_adjustments.get("sectionVisibility");
    let is_visible = |section: &str| -> bool {
        visibility
            .and_then(|v| v.get(section))
            .and_then(|s| s.as_bool())
            .unwrap_or(true)
    };

    // New RAW photos get Lightroom's capture defaults (Sharpening 40,
    // Color noise reduction 25; notes: reference/detail.md) when the recipe
    // has no value; saved edits store their values and keep them.
    let tool_on = |key: &str| tool_switch(key).is_none_or(&is_visible);
    let raw_detail_default = |key: &str, raw_default: f64, scale: f32| -> f32 {
        if !is_visible("details") || !tool_on(key) {
            return 0.0;
        }
        let fallback = if is_raw { raw_default } else { 0.0 };
        js_adjustments[key].as_f64().unwrap_or(fallback) as f32 / scale
    };
    let get_val = |section: &str, key: &str, scale: f32, default: Option<f64>| -> f32 {
        if is_visible(section) && tool_on(key) {
            js_adjustments[key]
                .as_f64()
                .unwrap_or(default.unwrap_or(0.0)) as f32
                / scale
        } else {
            if let Some(d) = default {
                d as f32 / scale
            } else {
                0.0
            }
        }
    };

    let default_curve = serde_json::json!([{"x": 0.0, "y": 0.0}, {"x": 255.0, "y": 255.0}]);
    let curves_obj = js_adjustments.get("curves").cloned().unwrap_or_default();

    let luma_points: Vec<serde_json::Value> = if is_visible("curves") {
        curves_obj
            .get("luma")
            .unwrap_or(&default_curve)
            .as_array()
            .cloned()
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let red_points: Vec<serde_json::Value> = if is_visible("curves") {
        curves_obj
            .get("red")
            .unwrap_or(&default_curve)
            .as_array()
            .cloned()
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let green_points: Vec<serde_json::Value> = if is_visible("curves") {
        curves_obj
            .get("green")
            .unwrap_or(&default_curve)
            .as_array()
            .cloned()
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let blue_points: Vec<serde_json::Value> = if is_visible("curves") {
        curves_obj
            .get("blue")
            .unwrap_or(&default_curve)
            .as_array()
            .cloned()
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    let cg_obj = js_adjustments
        .get("colorGrading")
        .cloned()
        .unwrap_or_default();

    let cal_obj = js_adjustments
        .get("colorCalibration")
        .cloned()
        .unwrap_or_default();

    // The Classic and AgX looks' inherited grading and calibration follow
    // their tool switches from `renderVersion` 2 (the other looks always did).
    let switches = render_version(js_adjustments) >= 2;
    let classic_grading = is_visible("color") && (!switches || tool_on("colorGrading"));
    let classic_calibration = is_visible("color") && (!switches || tool_on("colorCalibration"));
    let color_cal_settings = if classic_calibration {
        ColorCalibrationSettings {
            shadows_tint: cal_obj["shadowsTint"].as_f64().unwrap_or(0.0) as f32
                / SCALES.color_calibration_hue,
            red_hue: cal_obj["redHue"].as_f64().unwrap_or(0.0) as f32
                / SCALES.color_calibration_hue,
            red_saturation: cal_obj["redSaturation"].as_f64().unwrap_or(0.0) as f32
                / SCALES.color_calibration_saturation,
            green_hue: cal_obj["greenHue"].as_f64().unwrap_or(0.0) as f32
                / SCALES.color_calibration_hue,
            green_saturation: cal_obj["greenSaturation"].as_f64().unwrap_or(0.0) as f32
                / SCALES.color_calibration_saturation,
            blue_hue: cal_obj["blueHue"].as_f64().unwrap_or(0.0) as f32
                / SCALES.color_calibration_hue,
            blue_saturation: cal_obj["blueSaturation"].as_f64().unwrap_or(0.0) as f32
                / SCALES.color_calibration_saturation,
            _pad1: 0.0,
        }
    } else {
        ColorCalibrationSettings::default()
    };

    // Recipes without a tone mapper (new and unedited photographs) use the
    // Standard base look; saved edits name theirs ("basic" or "agx").
    let tone_mapper = js_adjustments["toneMapper"].as_str().unwrap_or("standard");
    let (pipe_to_rendering, rendering_to_pipe) = calculate_agx_matrices();

    let (has_lut, lut_intensity, lut_is_scene_referred) = if is_visible("effects") {
        (
            if js_adjustments["lutPath"].is_string() {
                1
            } else {
                0
            },
            js_adjustments["lutIntensity"].as_f64().unwrap_or(100.0) as f32 / 100.0,
            if js_adjustments["lutIsSceneReferred"]
                .as_bool()
                .unwrap_or(false)
            {
                1
            } else {
                0
            },
        )
    } else {
        (0, 1.0, 0)
    };

    let mut global = GlobalAdjustments {
        exposure: get_val("basic", "exposure", SCALES.exposure, None),
        brightness: get_val("basic", "brightness", SCALES.brightness, None),
        contrast: get_val("basic", "contrast", SCALES.contrast, None),
        highlights: get_val("basic", "highlights", SCALES.highlights, None),
        shadows: get_val("basic", "shadows", SCALES.shadows, None),
        whites: get_val("basic", "whites", SCALES.whites, None),
        blacks: get_val("basic", "blacks", SCALES.blacks, None),

        saturation: get_val("color", "saturation", SCALES.saturation, None),
        temperature: get_val("color", "temperature", SCALES.temperature, None),
        tint: get_val("color", "tint", SCALES.tint, None),
        vibrance: get_val("color", "vibrance", SCALES.vibrance, None),
        hue: get_val("color", "hue", 1.0, None),
        color_noise_speck: 0.0,
        luma_speck: if luma_noise_version(js_adjustments) >= 2 {
            1.0
        } else {
            0.0
        },
        _pad_color3: 0.0,

        sharpness: raw_detail_default("sharpness", RAW_DEFAULT_SHARPNESS, SCALES.sharpness),
        luma_noise_reduction: raw_detail_default(
            "lumaNoiseReduction",
            RAW_DEFAULT_LUMA_NOISE_REDUCTION,
            SCALES.luma_noise_reduction,
        ),
        color_noise_reduction: raw_detail_default(
            "colorNoiseReduction",
            RAW_DEFAULT_COLOR_NOISE_REDUCTION,
            SCALES.color_noise_reduction,
        ),

        clarity: get_val("details", "clarity", SCALES.clarity, None),
        dehaze: get_val("details", "dehaze", SCALES.dehaze, None),
        structure: get_val("details", "structure", SCALES.structure, None),
        centré: get_val("details", "centré", SCALES.centré, None),
        vignette_amount: get_val("effects", "vignetteAmount", SCALES.vignette_amount, None),
        vignette_midpoint: get_val(
            "effects",
            "vignetteMidpoint",
            SCALES.vignette_midpoint,
            Some(50.0),
        ),
        vignette_roundness: get_val(
            "effects",
            "vignetteRoundness",
            SCALES.vignette_roundness,
            Some(0.0),
        ),
        vignette_feather: get_val(
            "effects",
            "vignetteFeather",
            SCALES.vignette_feather,
            Some(50.0),
        ),
        grain_amount: get_val("effects", "grainAmount", SCALES.grain_amount, None),
        grain_size: get_val("effects", "grainSize", SCALES.grain_size, Some(25.0)),
        grain_roughness: get_val(
            "effects",
            "grainRoughness",
            SCALES.grain_roughness,
            Some(50.0),
        ),

        chromatic_aberration_red_cyan: get_val(
            "details",
            "chromaticAberrationRedCyan",
            SCALES.chromatic_aberration,
            None,
        ),
        chromatic_aberration_blue_yellow: get_val(
            "details",
            "chromaticAberrationBlueYellow",
            SCALES.chromatic_aberration,
            None,
        ),
        show_clipping: if js_adjustments["showClipping"].as_bool().unwrap_or(false) {
            1
        } else {
            0
        },
        is_raw_image: if is_raw { 1 } else { 0 },
        grain_structure: grain_structure_index(js_adjustments["grainStructure"].as_str()),

        has_lut,
        lut_intensity,

        tonemapper_mode: tonemapper_override.unwrap_or_else(|| tonemapper_mode_of(tone_mapper)),
        lut_is_scene_referred,
        image_key: 0.0,
        image_key_low: 0.0,
        tone_version: tone_version_of(js_adjustments) as f32,

        baseline_exposure: 0.0,
        noise_level: 0.0,
        base_curve: base_curve_index(js_adjustments["baseCurve"].as_str()) as f32,
        agx_pipe_to_rendering_matrix: pipe_to_rendering,
        agx_rendering_to_pipe_matrix: rendering_to_pipe,

        color_noise_aware2: 0.0,
        color_noise_aware_far: 0.0,
        color_noise_guide: 0.0,
        color_noise_guide_k2: 0.0,
        color_grading_shadows: if classic_grading {
            parse_color_grade_settings(&cg_obj["shadows"])
        } else {
            ColorGradeSettings::default()
        },
        color_grading_midtones: if classic_grading {
            parse_color_grade_settings(&cg_obj["midtones"])
        } else {
            ColorGradeSettings::default()
        },
        color_grading_highlights: if classic_grading {
            parse_color_grade_settings(&cg_obj["highlights"])
        } else {
            ColorGradeSettings::default()
        },
        color_grading_global: if classic_grading {
            parse_color_grade_settings(&cg_obj["global"])
        } else {
            ColorGradeSettings::default()
        },
        color_grading_blending: if classic_grading {
            cg_obj["blending"].as_f64().unwrap_or(50.0) as f32 / SCALES.color_grading_blending
        } else {
            0.5
        },
        color_grading_balance: if classic_grading {
            cg_obj["balance"].as_f64().unwrap_or(0.0) as f32 / SCALES.color_grading_balance
        } else {
            0.0
        },
        grading_active: 0,
        bw_enabled: u32::from(
            is_visible("color") && js_adjustments["blackAndWhite"].as_bool().unwrap_or(false),
        ),

        color_calibration: color_cal_settings,

        hsl: if is_visible("color") && tool_on("hsl") {
            parse_hsl_adjustments(&js_adjustments.get("hsl").cloned().unwrap_or_default())
        } else {
            [HslColor::default(); 8]
        },
        luma_curve: convert_points_to_aligned(luma_points.clone()),
        red_curve: convert_points_to_aligned(red_points.clone()),
        green_curve: convert_points_to_aligned(green_points.clone()),
        blue_curve: convert_points_to_aligned(blue_points.clone()),
        luma_curve_count: luma_points.len().min(16) as u32,
        red_curve_count: red_points.len().min(16) as u32,
        green_curve_count: green_points.len().min(16) as u32,
        blue_curve_count: blue_points.len().min(16) as u32,
        detail_scale: 0.0,
        sharpen_radius_offset: get_val("details", "sharpenRadius", 1.0, Some(1.0)).clamp(0.5, 3.0)
            - 1.0,
        sharpen_detail_offset: get_val("details", "sharpenDetail", 1.0, Some(25.0))
            .clamp(0.0, 100.0)
            - 25.0,
        sharpen_masking: get_val("details", "sharpenMasking", 1.0, Some(0.0)).clamp(0.0, 100.0),

        glow_amount: get_val("effects", "glowAmount", SCALES.glow, None),
        halation_amount: get_val("effects", "halationAmount", SCALES.halation, None),
        flare_amount: get_val("effects", "flareAmount", SCALES.flares, None),
        sharpness_threshold: get_val(
            "details",
            "sharpnessThreshold",
            SCALES.sharpness_threshold,
            Some(15.0),
        ),
        profile_curve: [[0.0; 4]; 16],
        grading_lut: [[0.0; 4]; GRADING_LUT_SIZE],
        bw_mix: gray_mixer(&js_adjustments["grayMixer"]),
        levels: [[0.0, 1.0, 1.0, 0.0]; 4],
        levels_white: [1.0; 4],
        levels_active: [0; 4],
        defringe: if is_visible("details") {
            defringe_settings(js_adjustments)
        } else {
            [
                [0.0, 0.0, 0.0, defringe_version(js_adjustments) as f32],
                [0.0; 4],
            ]
        },
        color_ranges: [[0.0; 4]; 32],
        color_range_count: [0; 4],
        region_curve: [[[0.0; 4]; 32]; 2],
        region_curve_active: [0; 4],
        calibration_matrix: GpuMat3::default(),
        calibration_tint: [0.0; 4],
        calibration_active: [0; 4],
        vignette_style: if is_visible("effects") {
            [
                match js_adjustments["vignetteStyle"].as_str() {
                    Some("colorPriority") => 1,
                    Some("paintOverlay") => 2,
                    _ => 0,
                },
                (js_adjustments["vignetteHighlights"]
                    .as_f64()
                    .unwrap_or(0.0)
                    .clamp(0.0, 100.0)
                    * 10.0) as u32,
                0,
                0,
            ]
        } else {
            [0; 4]
        },
        calibration_luma: [0.0; 4],
        sharpen_falloff: [
            if is_visible("details") && tool_on("sharpness") {
                (js_adjustments["sharpnessFalloff"].as_f64().unwrap_or(0.0) / 100.0).clamp(0.0, 1.0)
                    as f32
            } else {
                0.0
            },
            0.0,
            // Diffraction correction requested (its version); diffraction.rs
            // fills y/z.
            crate::diffraction::requested_flag(js_adjustments),
            sharpen_version(js_adjustments) as f32,
        ],
        camera_profile: [
            crate::camera_profile::register(js_adjustments["cameraProfile"].as_str().unwrap_or("")),
            0,
            0,
            0,
        ],
        camera_profile_matrix: GpuMat3::default(),
        camera_profile_hue_sat: [0; 4],
        camera_profile_look: [0; 4],
        moire: if is_visible("details") {
            moire_settings(js_adjustments)
        } else {
            [0.0; 4]
        },
        white_balance: if is_visible("color") && is_visible("whiteBalance") {
            kelvin_white_balance(js_adjustments)
        } else {
            [0.0; 4]
        },
        noise_detail: noise_detail(js_adjustments),
        output_space: soft_proof(js_adjustments),
        output_matrix: GpuMat3::default(),
        proof_matrix: proof_matrices(js_adjustments).0,
        proof_inverse: proof_matrices(js_adjustments).1,
        relight: crate::relight::gpu_lights(js_adjustments),
        relight_depth: [0; 4],
        relight_camera: crate::relight::Camera::uniforms(js_adjustments).0,
        relight_geometry: crate::relight::Camera::uniforms(js_adjustments).1,
        color_noise: [
            match crate::color_noise::version(js_adjustments, is_raw) {
                version if version >= crate::color_noise::MULTISCALE_VERSION => {
                    (version - 1) as f32
                }
                _ => 0.0,
            },
            0.0,
            0.0,
            0.0,
        ],
        color_noise_window: [0; 4],
        luma_noise_profile: [[0.0; 4]; 2],
        relight_source: crate::relight::source_uniforms(js_adjustments),
        relight_surface: [
            (js_adjustments["relightDepthAmount"]
                .as_f64()
                .unwrap_or(60.0)
                .clamp(0.0, 100.0)
                / 100.0) as f32,
            (js_adjustments["relightOriginalLight"]
                .as_f64()
                .unwrap_or(100.0)
                .clamp(0.0, 100.0)
                / 100.0) as f32,
            if js_adjustments["relightDepth"]["version"]
                .as_u64()
                .unwrap_or(0)
                >= 2
            {
                1.
            } else {
                0.
            },
            (js_adjustments["relightSkinReflections"]
                .as_f64()
                .unwrap_or_else(|| {
                    if js_adjustments["relightDepth"]["version"]
                        .as_u64()
                        .unwrap_or(0)
                        >= 3
                    {
                        25.
                    } else {
                        0.
                    }
                })
                .clamp(0., 100.)
                / 100.) as f32,
        ],
    };
    if modern_grading(global.tonemapper_mode) && is_visible("color") {
        // The Standard and Camera Raw looks use Camera Raw's calibration.
        global.color_calibration = ColorCalibrationSettings::default();
        let calibration = crate::calibration::Calibration::from_json(&cal_obj);
        if tool_on("colorCalibration") && !calibration.is_neutral() {
            global.calibration_matrix = mat3_to_gpu_mat3(calibration.matrix());
            let [strength, sigma, power] = crate::calibration::SHADOW_TINT;
            global.calibration_tint = [calibration.shadows_tint / 100.0, strength, sigma, power];
            global.calibration_active[0] = 1;
            global.calibration_luma[0] = calibration.luminance_keep();
        }
    }
    if is_visible("curves") {
        let region = crate::region_curve::RegionCurve::from_json(&js_adjustments["regionCurve"]);
        if !region.is_neutral() {
            let lut = region.lut();
            for (i, entry) in lut.iter().enumerate() {
                global.region_curve[i / 32][i % 32] = *entry;
            }
            global.region_curve_active[0] = 1;
        }
    }
    if is_visible("color") && tool_on("colorRanges") {
        global.apply_color_ranges(js_adjustments);
    }
    if is_visible("curves") && tool_on("levels") {
        global.apply_levels(&js_adjustments["levels"]);
    }
    if modern_grading(global.tonemapper_mode) && is_visible("color") {
        let off = serde_json::Value::Null;
        global.apply_grading(if tool_on("colorGrading") {
            &cg_obj
        } else {
            &off
        });
    }
    global
}

/// Colour grading after Lightroom (color_grading.rs) for the Standard and
/// Camera Raw looks; Classic and AgX edits keep the inherited operator.
fn modern_grading(tonemapper_mode: u32) -> bool {
    tonemapper_mode >= tonemapper_mode_of("standard")
}

macro_rules! impl_apply_grading {
    ($t:ty) => {
        impl $t {
            fn apply_grading(&mut self, color_grading: &serde_json::Value) {
                let grading = crate::color_grading::Grading::from_json(color_grading);
                self.color_grading_shadows = ColorGradeSettings::default();
                self.color_grading_midtones = ColorGradeSettings::default();
                self.color_grading_highlights = ColorGradeSettings::default();
                self.color_grading_global = ColorGradeSettings::default();
                if grading.is_neutral() {
                    self.grading_active = 0;
                    return;
                }
                self.grading_lut = crate::color_grading::lut(&grading);
                self.grading_active = 1;
            }
        }
    };
}
impl_apply_grading!(GlobalAdjustments);
impl_apply_grading!(MaskAdjustments);

impl GlobalAdjustments {
    /// `levels`: {rgb, red, green, blue} each {inBlack, inWhite, gamma,
    /// outBlack, outWhite} with levels in 0–255 and gamma 0.1–10 (Capture
    /// One's Levels; Photoshop's semantics).
    fn apply_levels(&mut self, levels: &serde_json::Value) {
        let mut active = false;
        for (i, channel) in ["rgb", "red", "green", "blue"].iter().enumerate() {
            let c = &levels[channel];
            let get = |key: &str, default: f64| c[key].as_f64().unwrap_or(default);
            let in_black = get("inBlack", 0.0).clamp(0.0, 254.0);
            let in_white = get("inWhite", 255.0).clamp(in_black + 1.0, 255.0);
            let gamma = get("gamma", 1.0).clamp(0.1, 10.0);
            let out_black = get("outBlack", 0.0).clamp(0.0, 255.0);
            let out_white = get("outWhite", 255.0).clamp(0.0, 255.0);
            if in_black != 0.0
                || in_white != 255.0
                || gamma != 1.0
                || out_black != 0.0
                || out_white != 255.0
            {
                active = true;
            }
            self.levels[i] = [
                (in_black / 255.0) as f32,
                (in_white / 255.0) as f32,
                (1.0 / gamma) as f32,
                (out_black / 255.0) as f32,
            ];
            self.levels_white[i] = (out_white / 255.0) as f32;
        }
        self.levels_active[0] = u32::from(active);
    }
}

impl GlobalAdjustments {
    /// The colour editor's ranges (`colorRanges`, at most eight) and, when
    /// `colorRangeShow` names one of them and clipping warnings are off,
    /// its selection view (`show_clipping` = 2 + index; exports reset it).
    fn apply_color_ranges(&mut self, adjustments: &serde_json::Value) {
        let ranges = pack_color_ranges(&adjustments["colorRanges"], 8);
        let show = adjustments["colorRangeShow"].as_str();
        for (i, range) in ranges.iter().enumerate() {
            self.color_ranges[i * 4..i * 4 + 4].copy_from_slice(range);
            if self.show_clipping == 0
                && show.is_some()
                && adjustments["colorRanges"][i]["id"].as_str() == show
            {
                self.show_clipping = 2 + i as u32;
            }
        }
        self.color_range_count[0] = ranges.len() as u32;
    }
}

/// The colour editor's ranges (`colorRanges`) as the shader reads them, four
/// vec4 each: (hue°, chroma, lightness, lightness half-width), (hue
/// half-width, chroma low, chroma high, softness), (hue shift°, saturation,
/// lightness shift, 0), (hue, saturation, lightness uniformity, 0).
fn pack_color_ranges(ranges: &serde_json::Value, limit: usize) -> Vec<[[f32; 4]; 4]> {
    let Some(ranges) = ranges.as_array() else {
        return Vec::new();
    };
    ranges
        .iter()
        .take(limit)
        .map(|range| {
            let get = |key: &str, default: f64| range[key].as_f64().unwrap_or(default);
            let chroma = get("chroma", 0.0).max(0.0);
            let chroma_range = get("chromaRange", 70.0).clamp(0.0, 100.0) / 100.0;
            let lightness_range = get("lightnessRange", 100.0).clamp(0.0, 100.0) / 100.0;
            [
                [
                    get("hue", 0.0).rem_euclid(360.0) as f32,
                    chroma as f32,
                    get("lightness", 0.5).clamp(0.0, 1.0) as f32,
                    (0.02 + lightness_range) as f32,
                ],
                [
                    get("hueRange", 25.0).clamp(5.0, 90.0) as f32,
                    (chroma * (1.0 - chroma_range)) as f32,
                    (chroma * (1.0 + 2.0 * chroma_range) + 0.01) as f32,
                    (0.1 + 0.9 * get("smoothness", 50.0).clamp(0.0, 100.0) / 100.0) as f32,
                ],
                [
                    (get("hueShift", 0.0).clamp(-100.0, 100.0) * 0.5) as f32,
                    (get("saturation", 0.0).clamp(-100.0, 100.0) / 100.0) as f32,
                    (get("lightnessShift", 0.0).clamp(-100.0, 100.0) / 100.0 * 0.2) as f32,
                    0.0,
                ],
                [
                    (get("hueUniformity", 0.0).clamp(0.0, 100.0) / 100.0) as f32,
                    (get("saturationUniformity", 0.0).clamp(0.0, 100.0) / 100.0) as f32,
                    (get("lightnessUniformity", 0.0).clamp(0.0, 100.0) / 100.0) as f32,
                    0.0,
                ],
            ]
        })
        .collect()
}

/// The recipe's white balance in Kelvin: `whiteBalance` "custom" with
/// `wbTemperature` / `wbTint`, or one of Lightroom's presets; "asShot" (or
/// none) keeps the camera's.
fn kelvin_white_balance(adjustments: &serde_json::Value) -> [f32; 4] {
    let preset = match adjustments["whiteBalance"].as_str() {
        Some("daylight") => Some((5500.0, 10.0)),
        Some("cloudy") => Some((6500.0, 10.0)),
        Some("shade") => Some((7500.0, 10.0)),
        Some("tungsten") => Some((2850.0, 0.0)),
        Some("fluorescent") => Some((3800.0, 21.0)),
        Some("flash") => Some((5500.0, 0.0)),
        // Auto stores the balance it found when chosen.
        Some("custom" | "auto") => adjustments["wbTemperature"].as_f64().map(|t| {
            (
                t.clamp(2000.0, 50_000.0),
                adjustments["wbTint"]
                    .as_f64()
                    .unwrap_or(0.0)
                    .clamp(-150.0, 150.0),
            )
        }),
        _ => None,
    };
    preset.map_or([0.0; 4], |(t, tint)| [1.0, t as f32, tint as f32, 0.0])
}

/// The display's soft proofing (view state, never in recipes): `softProof`
/// names an output colour space, `softProofWarning` marks colours outside it.
fn soft_proof_space(
    adjustments: &serde_json::Value,
) -> Option<crate::output_color_space::OutputColorSpace> {
    serde_json::from_value(adjustments.get("softProof")?.clone()).ok()
}

fn soft_proof(adjustments: &serde_json::Value) -> [u32; 4] {
    let Some(space) = soft_proof_space(adjustments) else {
        return [0; 4];
    };
    let warn = adjustments["softProofWarning"].as_bool().unwrap_or(false);
    [0, space.shader_code() + 1, u32::from(warn), 0]
}

fn proof_matrices(adjustments: &serde_json::Value) -> (GpuMat3, GpuMat3) {
    match soft_proof_space(adjustments) {
        Some(space) => {
            let inverse = crate::output_color_space::invert(&space.from_linear_srgb());
            (
                gpu_mat3_from_rows(space.from_linear_srgb_f32()),
                gpu_mat3_from_rows(std::array::from_fn(|r| {
                    std::array::from_fn(|c| inverse[r][c] as f32)
                })),
            )
        }
        None => (GpuMat3::default(), GpuMat3::default()),
    }
}

fn noise_detail(adjustments: &serde_json::Value) -> [f32; 4] {
    let get = |key: &str, default: f64| {
        (adjustments[key].as_f64().unwrap_or(default) / 100.0).clamp(0.0, 1.0) as f32
    };
    [
        get("lumaNoiseDetail", 50.0),
        get("lumaNoiseContrast", 0.0),
        get("colorNoiseDetail", 50.0),
        get("colorNoiseSmoothness", 50.0),
    ]
}

/// Moiré `moireAmount` (0–100) and `moirePattern` (0–100, the size of the
/// pattern it looks for) as the shader's amount and sampling step (1–8
/// original pixels).
fn moire_settings(adjustments: &serde_json::Value) -> [f32; 4] {
    let get = |key: &str, default: f64| adjustments[key].as_f64().unwrap_or(default);
    let amount = (get("moireAmount", 0.0) / 100.0).clamp(0.0, 1.0) as f32;
    if amount <= 0.0 {
        return [0.0; 4];
    }
    let step = 1.0 + 7.0 * (get("moirePattern", 30.0) / 100.0).clamp(0.0, 1.0) as f32;
    [amount, step, 0.0, 0.0]
}

/// Lightroom's Defringe sliders (`defringePurpleAmount` 0–20,
/// `defringePurpleHueLo/Hi` 0–100, same for green) as the shader's amounts
/// (0–1) and Oklab hue ranges, as measured in Camera Raw
/// (notes: reference/defringe.md): purple hue 0–100 spans Oklab 225° to 415°
/// (blue-violet over magenta to orange-red), green 0–100 spans 60° to 250°
/// (yellow over green to blue), 1.9° per step.
fn defringe_settings(adjustments: &serde_json::Value) -> [[f32; 4]; 2] {
    let get = |key: &str, default: f64| adjustments[key].as_f64().unwrap_or(default);
    let purple = (get("defringePurpleAmount", 0.0) / 20.0).clamp(0.0, 1.0) as f32;
    let green = (get("defringeGreenAmount", 0.0) / 20.0).clamp(0.0, 1.0) as f32;
    let range = |lo: f64, hi: f64, start: f64, span: f64| {
        let (lo, hi) = (lo.clamp(0.0, 100.0), hi.clamp(0.0, 100.0));
        let a = start + span * lo.min(hi) / 100.0;
        let b = start + span * lo.max(hi) / 100.0;
        (a.min(b) as f32, a.max(b) as f32)
    };
    let (p_lo, p_hi) = range(
        get("defringePurpleHueLo", 30.0),
        get("defringePurpleHueHi", 70.0),
        225.0,
        190.0,
    );
    let (g_lo, g_hi) = range(
        get("defringeGreenHueLo", 40.0),
        get("defringeGreenHueHi", 60.0),
        60.0,
        190.0,
    );
    let version = defringe_version(adjustments) as f32;
    [[purple, p_lo, p_hi, version], [green, g_lo, g_hi, 0.0]]
}

/// The recipe key naming the sharpening response a recipe was made with.
pub const SHARPEN_VERSION_KEY: &str = "sharpenVersion";
/// The newest response: 2 (2026-10-19) keeps the halo ceiling nearly
/// constant over the amount, grows the small-signal gain as Camera Raw's
/// does and bounds the Detail tilt, so high amounts follow Camera Raw's
/// band gains with halos that grow far less than version 1's (at most 2.7
/// Oklab L ×100 above Camera Raw's on the measured frames, version 1 up to
/// 21; notes: reference/detail.md "Sharpening response 2"); at Amount 40 with Radius, Detail and Masking
/// at their defaults it renders as 1. Photo-wide: layers' sharpening amounts
/// follow it. Recipes without the key keep version 1.
pub const SHARPEN_VERSION: u64 = 2;

/// The recipe's sharpening response, 1 when missing, never newer than this
/// renderer knows.
pub fn sharpen_version(adjustments: &serde_json::Value) -> u64 {
    adjustments
        .get(SHARPEN_VERSION_KEY)
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(1)
        .clamp(1, SHARPEN_VERSION)
}

/// The recipe key naming the luminance noise reduction a recipe was made with.
pub const LUMA_NOISE_VERSION_KEY: &str = "lumaNoiseVersion";
/// The newest: 2 (2026-10-19) also takes isolated luminance specks out of
/// noisy photos (single pixels darker or brighter than all their
/// neighbours, left by demosaicing and the noise reduction and lifted by
/// the sharpening): such a pixel is kept within its eight neighbours' range
/// plus half its noise, and the sharpening leaves it alone, unless it stands
/// far out of the noise (a point light); it fades in between measured noise
/// levels 0.002 and 0.004 and is skipped at or below 0.002, where photos
/// render exactly as 1 (most base-ISO photos; notes:
/// reference/luma-noise-reference.md "Isolated luminance specks").
/// Photo-wide: layers' luminance amounts follow it. Recipes without the key
/// keep version 1.
pub const LUMA_NOISE_VERSION: u64 = 2;

/// The recipe's luminance noise reduction, 1 when missing, never newer than
/// this renderer knows.
pub fn luma_noise_version(adjustments: &serde_json::Value) -> u64 {
    adjustments
        .get(LUMA_NOISE_VERSION_KEY)
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(1)
        .clamp(1, LUMA_NOISE_VERSION)
}

/// The recipe key naming the render pipeline's fixes a recipe was made
/// with (2026-10-20), for corrections that change the pixels of saved edits
/// without a tool of their own.
pub const RENDER_VERSION_KEY: &str = "renderVersion";
/// The newest: 2 keeps a geometry warp's or straightening's negative (wide
/// gamut) channels where the source has them, gives the large blur of
/// Clarity, Dehaze and Glow its whole radius at tile edges on photos with a
/// short side above 3456 pixels (no seams), and turns the Classic and AgX
/// looks' Color Grading and Calibration off with their tool switches.
/// Recipes without the key keep version 1.
pub const RENDER_VERSION: u64 = 2;

/// The recipe's render pipeline version, 1 when missing, never newer than
/// this renderer knows.
pub fn render_version(adjustments: &serde_json::Value) -> u64 {
    adjustments
        .get(RENDER_VERSION_KEY)
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(1)
        .clamp(1, RENDER_VERSION)
}

/// Sets the current render version on the recipe of a never-edited photo
/// (as the editor does for new edits, RAW or not); saved recipes are left
/// as they are.
pub fn apply_render_default(adjustments: &mut serde_json::Value) {
    if adjustments.get(RENDER_VERSION_KEY).is_some() {
        return;
    }
    if adjustments.is_null() {
        *adjustments = serde_json::json!({});
    }
    if let Some(map) = adjustments.as_object_mut() {
        map.insert(
            RENDER_VERSION_KEY.to_string(),
            serde_json::json!(RENDER_VERSION),
        );
    }
}

/// The recipe key naming the Defringe fringe detector a recipe was made with.
pub const DEFRINGE_VERSION_KEY: &str = "defringeVersion";
/// The newest detector: 2 (2026-10-17) takes only colour that the edge
/// across it does not explain (beyond the linear-light mix of its two sides
/// at the pixel's luminance, next to a much brighter side, not a patch of
/// one colour) and moves it to that mix (along its own hue, and across it
/// where the mix lies outside the selected ranges), for the photo's and the
/// layers' Defringe alike (notes: reference/defringe.md "Fringe detector"); 1
/// desaturates every colour in range near an edge. Recipes without the key
/// keep version 1.
pub const DEFRINGE_VERSION: u64 = 2;

/// The recipe's Defringe detector, 1 when missing, never newer than this
/// renderer knows.
pub fn defringe_version(adjustments: &serde_json::Value) -> u64 {
    adjustments
        .get(DEFRINGE_VERSION_KEY)
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(1)
        .clamp(1, DEFRINGE_VERSION)
}

/// Lightroom's B&W mix (`grayMixer`, −100…100 per channel) as slider / 100
/// in channel order red, orange, yellow, green, aqua, blue, purple, magenta.
fn gray_mixer(mixer: &serde_json::Value) -> [[f32; 4]; 2] {
    const CHANNELS: [&str; 8] = [
        "red", "orange", "yellow", "green", "aqua", "blue", "purple", "magenta",
    ];
    let mut mix = [[0.0; 4]; 2];
    for (i, channel) in CHANNELS.iter().enumerate() {
        mix[i / 4][i % 4] =
            (mixer[channel].as_f64().unwrap_or(0.0) / 100.0).clamp(-1.0, 1.0) as f32;
    }
    mix
}

fn get_mask_adjustments_from_json(
    adj: &serde_json::Value,
    tonemapper_mode: u32,
    render_version: u64,
) -> MaskAdjustments {
    if adj.is_null() {
        return MaskAdjustments::default();
    }

    let visibility = adj.get("sectionVisibility");
    let is_visible = |section: &str| -> bool {
        visibility
            .and_then(|v| v.get(section))
            .and_then(|s| s.as_bool())
            .unwrap_or(true)
    };

    let tool_on = |key: &str| tool_switch(key).is_none_or(&is_visible);
    // As for the photo (`get_global_adjustments_from_json`).
    let classic_grading = is_visible("color") && (render_version < 2 || tool_on("colorGrading"));
    let get_val = |section: &str, key: &str, scale: f32| -> f32 {
        if is_visible(section) && tool_on(key) {
            adj[key].as_f64().unwrap_or(0.0) as f32 / scale
        } else {
            0.0
        }
    };

    let curves_obj = adj.get("curves").cloned().unwrap_or_default();
    let luma_points: Vec<serde_json::Value> = if is_visible("curves") {
        curves_obj["luma"].as_array().cloned().unwrap_or_default()
    } else {
        Vec::new()
    };
    let red_points: Vec<serde_json::Value> = if is_visible("curves") {
        curves_obj["red"].as_array().cloned().unwrap_or_default()
    } else {
        Vec::new()
    };
    let green_points: Vec<serde_json::Value> = if is_visible("curves") {
        curves_obj["green"].as_array().cloned().unwrap_or_default()
    } else {
        Vec::new()
    };
    let blue_points: Vec<serde_json::Value> = if is_visible("curves") {
        curves_obj["blue"].as_array().cloned().unwrap_or_default()
    } else {
        Vec::new()
    };
    let cg_obj = adj.get("colorGrading").cloned().unwrap_or_default();

    let mut mask = MaskAdjustments {
        exposure: get_val("basic", "exposure", SCALES.exposure),
        brightness: get_val("basic", "brightness", SCALES.brightness),
        contrast: get_val("basic", "contrast", SCALES.contrast),
        highlights: get_val("basic", "highlights", SCALES.highlights),
        shadows: get_val("basic", "shadows", SCALES.shadows),
        whites: get_val("basic", "whites", SCALES.whites),
        blacks: get_val("basic", "blacks", SCALES.blacks),

        saturation: get_val("color", "saturation", SCALES.saturation),
        temperature: get_val("color", "temperature", SCALES.temperature),
        tint: get_val("color", "tint", SCALES.tint),
        vibrance: get_val("color", "vibrance", SCALES.vibrance),

        sharpness: get_val("details", "sharpness", SCALES.sharpness),
        luma_noise_reduction: get_val("details", "lumaNoiseReduction", SCALES.luma_noise_reduction),
        color_noise_reduction: get_val(
            "details",
            "colorNoiseReduction",
            SCALES.color_noise_reduction,
        ),

        clarity: get_val("details", "clarity", SCALES.clarity),
        dehaze: get_val("details", "dehaze", SCALES.dehaze),
        structure: get_val("details", "structure", SCALES.structure),

        glow_amount: get_val("effects", "glowAmount", SCALES.glow),
        halation_amount: get_val("effects", "halationAmount", SCALES.halation),
        flare_amount: get_val("effects", "flareAmount", SCALES.flares),
        sharpness_threshold: get_val("details", "sharpnessThreshold", SCALES.sharpness_threshold),

        hue: get_val("color", "hue", 1.0),
        defringe: (get_val("details", "defringe", 100.0)).clamp(-1.0, 1.0),
        _pad_cg2: 0.0,
        color_grading_shadows: if classic_grading {
            parse_color_grade_settings(&cg_obj["shadows"])
        } else {
            ColorGradeSettings::default()
        },
        color_grading_midtones: if classic_grading {
            parse_color_grade_settings(&cg_obj["midtones"])
        } else {
            ColorGradeSettings::default()
        },
        color_grading_highlights: if classic_grading {
            parse_color_grade_settings(&cg_obj["highlights"])
        } else {
            ColorGradeSettings::default()
        },
        color_grading_global: if classic_grading {
            parse_color_grade_settings(&cg_obj["global"])
        } else {
            ColorGradeSettings::default()
        },
        color_grading_blending: if classic_grading {
            cg_obj["blending"].as_f64().unwrap_or(50.0) as f32 / SCALES.color_grading_blending
        } else {
            0.5
        },
        color_grading_balance: if classic_grading {
            cg_obj["balance"].as_f64().unwrap_or(0.0) as f32 / SCALES.color_grading_balance
        } else {
            0.0
        },
        grading_active: 0,
        color_range_count: 0,

        hsl: if is_visible("color") && tool_on("hsl") {
            parse_hsl_adjustments(&adj.get("hsl").cloned().unwrap_or_default())
        } else {
            [HslColor::default(); 8]
        },
        luma_curve: convert_points_to_aligned(luma_points.clone()),
        red_curve: convert_points_to_aligned(red_points.clone()),
        green_curve: convert_points_to_aligned(green_points.clone()),
        blue_curve: convert_points_to_aligned(blue_points.clone()),
        luma_curve_count: luma_points.len().min(16) as u32,
        red_curve_count: red_points.len().min(16) as u32,
        green_curve_count: green_points.len().min(16) as u32,
        blue_curve_count: blue_points.len().min(16) as u32,
        _pad_end4: 0.0,
        _pad_end5: 0.0,
        _pad_end6: 0.0,
        _pad_end7: 0.0,
        grading_lut: [[0.0; 4]; GRADING_LUT_SIZE],
        color_ranges: [[0.0; 4]; 16],
        moire: if is_visible("details") {
            moire_settings(adj)
        } else {
            [0.0; 4]
        },
    };
    if is_visible("color") && tool_on("colorRanges") {
        let ranges = pack_color_ranges(&adj["colorRanges"], 4);
        mask.color_range_count = ranges.len() as u32;
        for (i, range) in ranges.iter().enumerate() {
            mask.color_ranges[i * 4..i * 4 + 4].copy_from_slice(range);
        }
    }
    if modern_grading(tonemapper_mode) && is_visible("color") {
        let off = serde_json::Value::Null;
        mask.apply_grading(if tool_on("colorGrading") {
            &cg_obj
        } else {
            &off
        });
    }
    mask
}

pub fn get_all_adjustments_from_json(
    js_adjustments: &serde_json::Value,
    is_raw: bool,
    tonemapper_override: Option<u32>,
) -> PreparedAdjustments {
    get_all_adjustments_with_masks(
        js_adjustments,
        is_raw,
        tonemapper_override,
        &mask_definitions_from_json(js_adjustments),
    )
}

/// The layers of an adjustments JSON (`masks`), none if any is malformed.
/// Deserialized in place: cloning the `Value` first doubled the cost of a
/// stack of brush layers with thousands of points.
pub fn mask_definitions_from_json(js_adjustments: &serde_json::Value) -> Vec<MaskDefinition> {
    js_adjustments
        .get("masks")
        .and_then(|m| Vec::<MaskDefinition>::deserialize(m).ok())
        .unwrap_or_default()
}

/// [`get_all_adjustments_from_json`] with the layers already read by
/// [`mask_definitions_from_json`] (a preview reads them once for the masks
/// and the adjustments).
pub fn get_all_adjustments_with_masks(
    js_adjustments: &serde_json::Value,
    is_raw: bool,
    tonemapper_override: Option<u32>,
    mask_definitions: &[MaskDefinition],
) -> PreparedAdjustments {
    let mut global = get_global_adjustments_from_json(js_adjustments, is_raw, tonemapper_override);
    let (depth_key, relight_surface) = crate::relight::prepare_depth(js_adjustments);
    let ([pattern_low, pattern_high], relight_patterns) =
        crate::relight::prepare_patterns(js_adjustments);
    global.relight_depth = [depth_key[0], depth_key[1], pattern_low, pattern_high];

    // The inherited Exposure slider applies 1/0.8 EV per unit. With the
    // Standard base look one unit is one stop, as in Capture One and
    // Lightroom (notes: reference/tone-controls.md); Classic and AgX edits keep
    // the inherited scale.
    let exposure_scale = if global.tonemapper_mode == tonemapper_mode_of("standard") {
        SCALES.exposure
    } else {
        1.0
    };
    global.exposure *= exposure_scale;
    let version = render_version(js_adjustments);
    let mut mask_adjustments = [MaskAdjustments::default(); MAX_MASKS];
    let mut mask_count = 0;

    for (i, mask_def) in mask_definitions
        .iter()
        .filter(|m| m.produces_bitmap())
        .take(MAX_MASKS)
        .enumerate()
    {
        mask_adjustments[i] =
            get_mask_adjustments_from_json(&mask_def.adjustments, global.tonemapper_mode, version);
        mask_adjustments[i].exposure *= exposure_scale;
        mask_count += 1;
    }

    PreparedAdjustments {
        relight_surface,
        relight_patterns,
        whole_blur_radius: version >= 2,
        uniforms: AllAdjustments {
            global,
            mask_adjustments,
            mask_count,
            tile_offset_x: 0,
            tile_offset_y: 0,
            mask_atlas_cols: 1,
            render_region: [0; 4],
            detail_cache: [0; 4],
            source_window: [0; 4],
        },
    }
}

#[inline(always)]
fn rgb_to_yc_only(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let y = 0.299 * r + 0.587 * g + 0.114 * b;
    let cb = -0.168736 * r - 0.331264 * g + 0.5 * b;
    let cr = 0.5 * r - 0.418688 * g - 0.081312 * b;
    (y, cb, cr)
}

#[inline(always)]
fn yc_to_rgb(y: f32, cb: f32, cr: f32) -> (f32, f32, f32) {
    let r = y + 1.402 * cr;
    let g = y - 0.344136 * cb - 0.714136 * cr;
    let b = y + 1.772 * cb;
    (r, g, b)
}

/// Rows of luma/chroma context the filters read around an output row: chroma
/// noise reduction samples rows -5..=+3 and the detail pass a 5-row box blur.
const ENHANCE_ROWS_ABOVE: usize = 5;
const ENHANCE_ROWS_BELOW: usize = 3;
/// Output rows per band. Bands bound the YCbCr and blur scratch to a few MB
/// instead of 16 bytes per pixel of the whole image (about 800 MB at 50 MP).
const ENHANCE_BAND_ROWS: usize = 256;

/// Chroma noise reduction and gentle detail enhancement after demosaicing.
/// Every output row reads YCbCr of the original pixels in a small row window,
/// so the image is processed in bands: each band's window is converted before
/// its rows are overwritten, keeping the rows above from the previous window.
/// The arithmetic and summation order per pixel are those of the full-frame
/// version, so results are bit-identical.
pub fn remove_raw_artifacts_and_enhance(
    image: &mut DynamicImage,
    color_nr_inv_sigma: f32,
    sharpening_amount: f32,
) {
    // Consume owned RGB input so RAW cleanup does not keep a full duplicate.
    let mut buffer = std::mem::take(image).into_rgb32f();
    let w = buffer.width() as usize;
    let h = buffer.height() as usize;
    if w == 0 || h == 0 || (color_nr_inv_sigma <= 0.0 && sharpening_amount <= 0.0) {
        *image = DynamicImage::ImageRgb32F(buffer);
        return;
    }

    let row_len = w * 3;
    let window_capacity = ENHANCE_ROWS_ABOVE + ENHANCE_BAND_ROWS + ENHANCE_ROWS_BELOW;
    let mut window = vec![0.0f32; window_capacity.min(h) * row_len];
    let mut blur = vec![0.0f32; (ENHANCE_BAND_ROWS + 4).min(h) * w];
    // Global rows [window_start, window_end) currently held in `window`.
    let (mut window_start, mut window_end) = (0usize, 0usize);

    let raw = buffer.as_mut();
    let mut band_start = 0;
    while band_start < h {
        let band_end = (band_start + ENHANCE_BAND_ROWS).min(h);
        let new_start = band_start.saturating_sub(ENHANCE_ROWS_ABOVE);
        let new_end = (band_end + ENHANCE_ROWS_BELOW).min(h);

        // Rows above this band were already overwritten; reuse their original
        // YCbCr from the previous window, then convert the rest from pixels
        // that no earlier band has written.
        let kept_end = window_end.min(new_end);
        if new_start < kept_end {
            window.copy_within(
                (new_start - window_start) * row_len..(kept_end - window_start) * row_len,
                0,
            );
        }
        let converted_from = kept_end.max(new_start);
        window[(converted_from - new_start) * row_len..(new_end - new_start) * row_len]
            .par_chunks_mut(3)
            .zip(raw[converted_from * row_len..new_end * row_len].par_chunks(3))
            .for_each(|(dest, pixel)| {
                let (y, cb, cr) = rgb_to_yc_only(pixel[0], pixel[1], pixel[2]);
                dest[0] = y;
                dest[1] = cb;
                dest[2] = cr;
            });
        window_start = new_start;
        window_end = new_end;
        let ycbcr = &window[..(new_end - new_start) * row_len];
        // Index of global row `row`, column `x`, channel 0 within the window.
        let at = |row: usize, x: usize| ((row - window_start) * w + x) * 3;

        let band = &mut raw[band_start * row_len..band_end * row_len];
        if color_nr_inv_sigma > 0.0 {
            enhance_chroma_rows(band, band_start, ycbcr, at, w, h, color_nr_inv_sigma);
        }
        if sharpening_amount > 0.0 {
            // The detail pass blurs luma horizontally over rows y-2..=y+2.
            let blur_start = band_start.saturating_sub(2);
            let blur_end = (band_end + 2).min(h);
            blur[..(blur_end - blur_start) * w]
                .par_chunks_mut(w)
                .enumerate()
                .for_each(|(offset, row)| {
                    let y = blur_start + offset;
                    for (x, value) in row.iter_mut().enumerate() {
                        let mut sum = 0.0;
                        let mut count = 0;
                        for kx in -2i32..=2 {
                            let sx = (x as i32 + kx).clamp(0, (w as i32) - 1) as usize;
                            sum += ycbcr[at(y, sx)];
                            count += 1;
                        }
                        *value = sum / count as f32;
                    }
                });
            let blur_at = |row: usize, x: usize| (row - blur_start) * w + x;
            enhance_detail_rows(
                band,
                band_start,
                ycbcr,
                at,
                &blur,
                blur_at,
                w,
                h,
                sharpening_amount,
            );
        }
        band_start = band_end;
    }

    *image = DynamicImage::ImageRgb32F(buffer);
}

#[allow(clippy::too_many_arguments)]
fn enhance_chroma_rows(
    band: &mut [f32],
    band_start: usize,
    ycbcr: &[f32],
    at: impl Fn(usize, usize) -> usize + Sync,
    w: usize,
    h: usize,
    base_inv_sigma: f32,
) {
    const OFFSETS: [isize; 3] = [-5, -1, 3];
    const OFFSET_SQUARES: [f32; 3] = [25.0, 1.0, 9.0];
    const LANES: usize = 4;
    band.par_chunks_mut(w * 3)
        .enumerate()
        .for_each(|(offset, row)| {
            let y = band_start + offset;
            let h_isize = h as isize;
            let w_isize = w as isize;
            let y_isize = y as isize;
            // All nine taps inside the image: rows y-5..=y+3, columns x-5..=x+3.
            let interior_row = y >= 5 && y + 3 < h;

            let mut x = 0;
            while x < w {
                if interior_row && x >= 5 && x + LANES + 3 <= w {
                    // Four neighbouring pixels at once; every lane runs the
                    // scalar operations below in the same order, so the
                    // output is identical, but the divisions vectorise.
                    let center_idx = at(y, x);
                    let rows: [usize; 3] = OFFSETS.map(|ky| at((y_isize + ky) as usize, x));
                    let cy: [f32; LANES] = std::array::from_fn(|l| ycbcr[center_idx + 3 * l]);
                    let mut cb_sum = [0.0f32; LANES];
                    let mut cr_sum = [0.0f32; LANES];
                    let mut w_sum = [0.0f32; LANES];
                    for (ki, &row_idx) in rows.iter().enumerate() {
                        let ky_sq_div_50 = OFFSET_SQUARES[ki] * 0.02;
                        for (kj, &kx) in OFFSETS.iter().enumerate() {
                            let spatial_penalty = OFFSET_SQUARES[kj] * 0.02 + ky_sq_div_50;
                            let base = (row_idx as isize + 3 * kx) as usize;
                            for l in 0..LANES {
                                let neighbor_idx = base + 3 * l;
                                let y_diff = (cy[l] - ycbcr[neighbor_idx]).abs();
                                let val = y_diff * base_inv_sigma;
                                let weight = 1.0 / (1.0 + val * val + spatial_penalty);
                                cb_sum[l] += ycbcr[neighbor_idx + 1] * weight;
                                cr_sum[l] += ycbcr[neighbor_idx + 2] * weight;
                                w_sum[l] += weight;
                            }
                        }
                    }
                    for l in 0..LANES {
                        let idx = center_idx + 3 * l;
                        let rgb = chroma_result(
                            cy[l],
                            ycbcr[idx + 1],
                            ycbcr[idx + 2],
                            cb_sum[l],
                            cr_sum[l],
                            w_sum[l],
                        );
                        row[(x + l) * 3..(x + l) * 3 + 3].copy_from_slice(&rgb);
                    }
                    x += LANES;
                    continue;
                }

                let center_idx = at(y, x);

                let cy = ycbcr[center_idx];
                let ccb = ycbcr[center_idx + 1];
                let ccr = ycbcr[center_idx + 2];

                let mut cb_sum = 0.0;
                let mut cr_sum = 0.0;
                let mut w_sum = 0.0;

                for (ki, &ky) in OFFSETS.iter().enumerate() {
                    let sy = y_isize + ky;
                    if sy < 0 || sy >= h_isize {
                        continue;
                    }
                    let ky_sq_div_50 = OFFSET_SQUARES[ki] * 0.02;

                    for (kj, &kx) in OFFSETS.iter().enumerate() {
                        let sx = (x as isize) + kx;
                        if sx < 0 || sx >= w_isize {
                            continue;
                        }

                        let neighbor_idx = at(sy as usize, sx as usize);

                        let neighbor_y = ycbcr[neighbor_idx];
                        let y_diff = (cy - neighbor_y).abs();

                        let val = y_diff * base_inv_sigma;
                        let spatial_penalty = OFFSET_SQUARES[kj] * 0.02 + ky_sq_div_50;

                        let weight = 1.0 / (1.0 + val * val + spatial_penalty);

                        cb_sum += ycbcr[neighbor_idx + 1] * weight;
                        cr_sum += ycbcr[neighbor_idx + 2] * weight;
                        w_sum += weight;
                    }
                }

                let rgb = chroma_result(cy, ccb, ccr, cb_sum, cr_sum, w_sum);
                row[x * 3..x * 3 + 3].copy_from_slice(&rgb);
                x += 1;
            }
        });
}

/// Filtered chroma (never more saturated than the original) back to RGB.
#[inline(always)]
fn chroma_result(cy: f32, ccb: f32, ccr: f32, cb_sum: f32, cr_sum: f32, w_sum: f32) -> [f32; 3] {
    let (out_cb, out_cr) = if w_sum > 1e-4 {
        let inv_w_sum = 1.0 / w_sum;
        let filtered_cb = cb_sum * inv_w_sum;
        let filtered_cr = cr_sum * inv_w_sum;

        let orig_mag_sq = ccb * ccb + ccr * ccr;
        let filt_mag_sq = filtered_cb * filtered_cb + filtered_cr * filtered_cr;

        if filt_mag_sq > orig_mag_sq && orig_mag_sq > 1e-12 {
            let scale = (orig_mag_sq / filt_mag_sq).sqrt();
            (filtered_cb * scale, filtered_cr * scale)
        } else {
            (filtered_cb, filtered_cr)
        }
    } else {
        (ccb, ccr)
    };

    let (r, g, b) = yc_to_rgb(cy, out_cb, out_cr);
    // Colours beyond sRGB keep the ProPhoto gamut.
    rawtonic_rawdecode::gamut::floor_to_prophoto([r, g, b])
}

#[allow(clippy::too_many_arguments)]
fn enhance_detail_rows(
    band: &mut [f32],
    band_start: usize,
    ycbcr: &[f32],
    at: impl Fn(usize, usize) -> usize + Sync,
    blur: &[f32],
    blur_at: impl Fn(usize, usize) -> usize + Sync,
    w: usize,
    h: usize,
    amount: f32,
) {
    let radius = 2i32;
    band.par_chunks_mut(w * 3)
        .enumerate()
        .for_each(|(offset, rgb_row)| {
            let y = band_start + offset;
            for x in 0..w {
                let mut blur_sum = 0.0;
                let mut count = 0;
                for ky in -radius..=radius {
                    let sy = (y as i32 + ky).clamp(0, (h as i32) - 1) as usize;
                    blur_sum += blur[blur_at(sy, x)];
                    count += 1;
                }
                let blurred_val = blur_sum / count as f32;

                let original_luma = ycbcr[at(y, x)];

                let detail = original_luma - blurred_val;

                let edge_strength = detail.abs();
                let adaptive_amount = if edge_strength > 0.1 {
                    amount * 0.3
                } else {
                    amount
                };
                let boost = detail * adaptive_amount;

                let r_idx = x * 3;
                let g_idx = r_idx + 1;
                let b_idx = r_idx + 2;

                let r = rgb_row[r_idx];
                let g = rgb_row[g_idx];
                let b = rgb_row[b_idx];

                let new_r = r + boost;
                let new_g = g + boost;
                let new_b = b + boost;

                let max_val = new_r.max(new_g).max(new_b);
                let min_val = new_r.min(new_g).min(new_b);

                let scale = if max_val > 1.0 || min_val < 0.0 {
                    if max_val > 1.0 && min_val < 0.0 {
                        0.0
                    } else if max_val > 1.0 {
                        (1.0 - r.max(g).max(b)) / boost.max(0.001)
                    } else {
                        r.min(g).min(b) / (-boost).max(0.001)
                    }
                } else {
                    1.0
                };

                let safe_boost = boost * scale.clamp(0.0, 1.0);

                // Negative channels are colours beyond sRGB.
                rgb_row[r_idx] = r + safe_boost;
                rgb_row[g_idx] = g + safe_boost;
                rgb_row[b_idx] = b + safe_boost;
            }
        });
}

/// The former full-frame implementation, kept as the exact test reference.
#[cfg(test)]
fn reference_remove_raw_artifacts_and_enhance(
    image: &mut DynamicImage,
    color_nr_inv_sigma: f32,
    sharpening_amount: f32,
) {
    // Consume owned RGB input so RAW cleanup does not keep a full duplicate.
    let mut buffer = std::mem::take(image).into_rgb32f();
    let w = buffer.width() as usize;
    let h = buffer.height() as usize;

    let mut ycbcr_buffer = vec![0.0f32; w * h * 3];

    let src = buffer.as_raw();

    ycbcr_buffer
        .par_chunks_mut(3)
        .zip(src.par_chunks(3))
        .for_each(|(dest, pixel)| {
            let (y, cb, cr) = rgb_to_yc_only(pixel[0], pixel[1], pixel[2]);
            dest[0] = y;
            dest[1] = cb;
            dest[2] = cr;
        });

    if color_nr_inv_sigma > 0.0 {
        let base_inv_sigma = color_nr_inv_sigma;
        const OFFSETS: [isize; 3] = [-5, -1, 3];
        const OFFSET_SQUARES: [f32; 3] = [25.0, 1.0, 9.0];

        buffer
            .par_chunks_mut(w * 3)
            .enumerate()
            .for_each(|(y, row)| {
                let row_offset = y * w;
                let h_isize = h as isize;
                let w_isize = w as isize;
                let y_isize = y as isize;

                for x in 0..w {
                    let center_idx = (row_offset + x) * 3;

                    let cy = ycbcr_buffer[center_idx];
                    let ccb = ycbcr_buffer[center_idx + 1];
                    let ccr = ycbcr_buffer[center_idx + 2];

                    let mut cb_sum = 0.0;
                    let mut cr_sum = 0.0;
                    let mut w_sum = 0.0;

                    for (ki, &ky) in OFFSETS.iter().enumerate() {
                        let sy = y_isize + ky;
                        if sy < 0 || sy >= h_isize {
                            continue;
                        }

                        let neighbor_row_idx = (sy as usize) * w;
                        let ky_sq_div_50 = OFFSET_SQUARES[ki] * 0.02;

                        for (kj, &kx) in OFFSETS.iter().enumerate() {
                            let sx = (x as isize) + kx;
                            if sx < 0 || sx >= w_isize {
                                continue;
                            }

                            let neighbor_idx = (neighbor_row_idx + sx as usize) * 3;

                            let neighbor_y = ycbcr_buffer[neighbor_idx];
                            let y_diff = (cy - neighbor_y).abs();

                            let val = y_diff * base_inv_sigma;
                            let spatial_penalty = OFFSET_SQUARES[kj] * 0.02 + ky_sq_div_50;

                            let weight = 1.0 / (1.0 + val * val + spatial_penalty);

                            cb_sum += ycbcr_buffer[neighbor_idx + 1] * weight;
                            cr_sum += ycbcr_buffer[neighbor_idx + 2] * weight;
                            w_sum += weight;
                        }
                    }

                    let (out_cb, out_cr) = if w_sum > 1e-4 {
                        let inv_w_sum = 1.0 / w_sum;
                        let filtered_cb = cb_sum * inv_w_sum;
                        let filtered_cr = cr_sum * inv_w_sum;

                        let orig_mag_sq = ccb * ccb + ccr * ccr;
                        let filt_mag_sq = filtered_cb * filtered_cb + filtered_cr * filtered_cr;

                        if filt_mag_sq > orig_mag_sq && orig_mag_sq > 1e-12 {
                            let scale = (orig_mag_sq / filt_mag_sq).sqrt();
                            (filtered_cb * scale, filtered_cr * scale)
                        } else {
                            (filtered_cb, filtered_cr)
                        }
                    } else {
                        (ccb, ccr)
                    };

                    let (r, g, b) = yc_to_rgb(cy, out_cb, out_cr);

                    let o = x * 3;
                    let [r, g, b] = rawtonic_rawdecode::gamut::floor_to_prophoto([r, g, b]);
                    row[o] = r;
                    row[o + 1] = g;
                    row[o + 2] = b;
                }
            });
    }

    if sharpening_amount > 0.0 {
        reference_gentle_detail_enhance(&mut buffer, &ycbcr_buffer, sharpening_amount);
    }

    *image = DynamicImage::ImageRgb32F(buffer);
}

#[cfg(test)]
fn reference_gentle_detail_enhance(
    buffer: &mut image::ImageBuffer<image::Rgb<f32>, Vec<f32>>,
    ycbcr_source: &[f32],
    amount: f32,
) {
    let w = buffer.width() as usize;
    let h = buffer.height() as usize;

    let mut temp_blur = vec![0.0; w * h];
    let radius = 2i32;

    temp_blur
        .par_chunks_mut(w)
        .enumerate()
        .for_each(|(y, row)| {
            let row_offset = y * w;
            for (x, row_val) in row.iter_mut().enumerate() {
                let mut sum = 0.0;
                let mut count = 0;
                for kx in -radius..=radius {
                    let sx = (x as i32 + kx).clamp(0, (w as i32) - 1) as usize;
                    sum += ycbcr_source[(row_offset + sx) * 3];
                    count += 1;
                }
                *row_val = sum / count as f32;
            }
        });

    let output = buffer.as_mut();

    output
        .par_chunks_mut(w * 3)
        .enumerate()
        .for_each(|(y, rgb_row)| {
            for x in 0..w {
                let mut blur_sum = 0.0;
                let mut count = 0;
                for ky in -radius..=radius {
                    let sy = (y as i32 + ky).clamp(0, (h as i32) - 1) as usize;
                    blur_sum += temp_blur[sy * w + x];
                    count += 1;
                }
                let blurred_val = blur_sum / count as f32;

                let original_luma = ycbcr_source[(y * w + x) * 3];

                let detail = original_luma - blurred_val;

                let edge_strength = detail.abs();
                let adaptive_amount = if edge_strength > 0.1 {
                    amount * 0.3
                } else {
                    amount
                };
                let boost = detail * adaptive_amount;

                let r_idx = x * 3;
                let g_idx = r_idx + 1;
                let b_idx = r_idx + 2;

                let r = rgb_row[r_idx];
                let g = rgb_row[g_idx];
                let b = rgb_row[b_idx];

                let new_r = r + boost;
                let new_g = g + boost;
                let new_b = b + boost;

                let max_val = new_r.max(new_g).max(new_b);
                let min_val = new_r.min(new_g).min(new_b);

                let scale = if max_val > 1.0 || min_val < 0.0 {
                    if max_val > 1.0 && min_val < 0.0 {
                        0.0
                    } else if max_val > 1.0 {
                        (1.0 - r.max(g).max(b)) / boost.max(0.001)
                    } else {
                        r.min(g).min(b) / (-boost).max(0.001)
                    }
                } else {
                    1.0
                };

                let safe_boost = boost * scale.clamp(0.0, 1.0);

                rgb_row[r_idx] = r + safe_boost;
                rgb_row[g_idx] = g + safe_boost;
                rgb_row[b_idx] = b + safe_boost;
            }
        });
}

#[derive(Serialize, Clone)]
pub struct HistogramData {
    red: Vec<f32>,
    green: Vec<f32>,
    blue: Vec<f32>,
    luma: Vec<f32>,
}

pub fn calculate_histogram_from_image(image: &DynamicImage) -> Result<HistogramData, String> {
    let init_hist = || ([0u32; 256], [0u32; 256], [0u32; 256], [0u32; 256]);

    let reduce_hist = |mut a: ([u32; 256], [u32; 256], [u32; 256], [u32; 256]),
                       b: ([u32; 256], [u32; 256], [u32; 256], [u32; 256])| {
        for i in 0..256 {
            a.0[i] += b.0[i];
            a.1[i] += b.1[i];
            a.2[i] += b.2[i];
            a.3[i] += b.3[i];
        }
        a
    };

    // Chunk by the same 10,000 pixels and sample every second pixel as before,
    // including at row/chunk boundaries. Alpha never contributes to analytics.
    let histogram_u8 = |raw: &[u8], channels: usize| {
        raw.par_chunks(10_000 * channels)
            .fold(init_hist, |mut acc, chunk| {
                for pixel in chunk.chunks_exact(channels).step_by(2) {
                    let r = pixel[0] as usize;
                    let g = pixel[1] as usize;
                    let b = pixel[2] as usize;
                    acc.0[r] += 1;
                    acc.1[g] += 1;
                    acc.2[b] += 1;
                    let luma = (r * 218 + g * 732 + b * 74) >> 10;
                    acc.3[luma.min(255)] += 1;
                }
                acc
            })
            .reduce(init_hist, reduce_hist)
    };

    let (r_c, g_c, b_c, l_c) = match image {
        DynamicImage::ImageRgb32F(f32_img) => {
            let raw = f32_img.as_raw();
            raw.par_chunks(30_000)
                .fold(init_hist, |mut acc, chunk| {
                    for pixel in chunk.as_chunks::<3>().0.iter().step_by(2) {
                        let r = (pixel[0].clamp(0.0, 1.0) * 255.0) as usize;
                        let g = (pixel[1].clamp(0.0, 1.0) * 255.0) as usize;
                        let b = (pixel[2].clamp(0.0, 1.0) * 255.0) as usize;

                        acc.0[r] += 1;
                        acc.1[g] += 1;
                        acc.2[b] += 1;

                        let luma = (r * 218 + g * 732 + b * 74) >> 10;
                        acc.3[luma.min(255)] += 1;
                    }
                    acc
                })
                .reduce(init_hist, reduce_hist)
        }
        DynamicImage::ImageRgb8(rgb) => histogram_u8(rgb.as_raw(), 3),
        DynamicImage::ImageRgba8(rgba) => histogram_u8(rgba.as_raw(), 4),
        _ => histogram_u8(image.to_rgb8().as_raw(), 3),
    };

    let mut red: Vec<f32> = r_c.into_iter().map(|c| c as f32).collect();
    let mut green: Vec<f32> = g_c.into_iter().map(|c| c as f32).collect();
    let mut blue: Vec<f32> = b_c.into_iter().map(|c| c as f32).collect();
    let mut luma: Vec<f32> = l_c.into_iter().map(|c| c as f32).collect();

    let smoothing_sigma = 2.0;
    apply_gaussian_smoothing(&mut red, smoothing_sigma);
    apply_gaussian_smoothing(&mut green, smoothing_sigma);
    apply_gaussian_smoothing(&mut blue, smoothing_sigma);
    apply_gaussian_smoothing(&mut luma, smoothing_sigma);

    normalize_histogram_range(&mut red, 0.99);
    normalize_histogram_range(&mut green, 0.99);
    normalize_histogram_range(&mut blue, 0.99);
    normalize_histogram_range(&mut luma, 0.99);

    Ok(HistogramData {
        red,
        green,
        blue,
        luma,
    })
}

fn apply_gaussian_smoothing(histogram: &mut [f32], sigma: f32) {
    if sigma <= 0.0 {
        return;
    }

    let kernel_radius = (sigma * 3.0).ceil() as usize;
    if kernel_radius == 0 || kernel_radius >= histogram.len() {
        return;
    }

    let kernel_size = 2 * kernel_radius + 1;
    let mut kernel = vec![0.0; kernel_size];
    let mut kernel_sum = 0.0;

    let two_sigma_sq = 2.0 * sigma * sigma;
    for (i, kernel_val) in kernel.iter_mut().enumerate() {
        let x = (i as i32 - kernel_radius as i32) as f32;
        let val = (-x * x / two_sigma_sq).exp();
        *kernel_val = val;
        kernel_sum += val;
    }

    if kernel_sum > 0.0 {
        for val in &mut kernel {
            *val /= kernel_sum;
        }
    }

    let original = histogram.to_owned();
    let len = histogram.len();

    for (i, hist_val) in histogram.iter_mut().enumerate() {
        let mut smoothed_val = 0.0;
        for (k, &kernel_val) in kernel.iter().enumerate() {
            let offset = k as i32 - kernel_radius as i32;
            let sample_index = i as i32 + offset;
            let clamped_index = sample_index.clamp(0, len as i32 - 1) as usize;
            smoothed_val += original[clamped_index] * kernel_val;
        }
        *hist_val = smoothed_val;
    }
}

fn normalize_histogram_range(histogram: &mut [f32], percentile_clip: f32) {
    if histogram.is_empty() {
        return;
    }

    let mut sorted_data = histogram.to_owned();
    sorted_data.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    let clip_index = ((sorted_data.len() - 1) as f32 * percentile_clip).round() as usize;
    let max_val = sorted_data[clip_index.min(sorted_data.len() - 1)];

    if max_val > 1e-6 {
        let scale_factor = 1.0 / max_val;
        for value in histogram.iter_mut() {
            *value = (*value * scale_factor).min(1.0);
        }
    } else {
        for value in histogram.iter_mut() {
            *value = 0.0;
        }
    }
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
}

pub fn calculate_waveform_from_image(
    image: &DynamicImage,
    active_channel: Option<&str>,
) -> Result<WaveformData, String> {
    const W: usize = 256;
    const H: usize = 256;

    let (orig_w, orig_h) = image.dimensions();
    if orig_w == 0 || orig_h == 0 {
        return Err("Image has zero dimensions.".to_string());
    }

    let do_rgb = active_channel.is_none() || active_channel == Some("rgb");
    let do_luma =
        active_channel.is_none() || active_channel == Some("luma") || active_channel == Some("rgb");
    let do_parade = active_channel.is_none() || active_channel == Some("parade");
    let do_vectorscope = active_channel.is_none() || active_channel == Some("vectorscope");

    let mut red_bins = if do_rgb { vec![0u32; W * H] } else { vec![] };
    let mut green_bins = if do_rgb { vec![0u32; W * H] } else { vec![] };
    let mut blue_bins = if do_rgb { vec![0u32; W * H] } else { vec![] };
    let mut luma_bins = if do_luma { vec![0u32; W * H] } else { vec![] };
    let mut parade_bins = if do_parade { vec![0u32; W * H] } else { vec![] };
    let mut vector_bins = if do_vectorscope {
        vec![0u32; W * H]
    } else {
        vec![]
    };

    let x_scale = W as f32 / orig_w as f32;
    let mut x_buckets = vec![0usize; orig_w as usize];

    let mut x_buckets_parade_r = vec![0usize; orig_w as usize];
    let mut x_buckets_parade_g = vec![0usize; orig_w as usize];
    let mut x_buckets_parade_b = vec![0usize; orig_w as usize];

    for x in 0..(orig_w as usize) {
        x_buckets[x] = ((x as f32 * x_scale) as usize).min(W - 1);
        if do_parade {
            let relative_x = x as f32 / orig_w as f32;
            x_buckets_parade_r[x] = (relative_x * 82.0) as usize % 82;
            x_buckets_parade_g[x] = 87 + (relative_x * 82.0) as usize % 82;
            x_buckets_parade_b[x] = 174 + (relative_x * 82.0) as usize % 82;
        }
    }

    let mut process_pixel = |r: u8, g: u8, b: u8, out_x: usize, orig_x: usize| {
        if do_rgb {
            red_bins[(255 - r as usize) * W + out_x] += 1;
            green_bins[(255 - g as usize) * W + out_x] += 1;
            blue_bins[(255 - b as usize) * W + out_x] += 1;
        }
        if do_luma {
            let l = ((r as u32 * 218 + g as u32 * 732 + b as u32 * 74) >> 10).min(255) as usize;
            luma_bins[(255 - l) * W + out_x] += 1;
        }
        if do_parade {
            parade_bins[(255 - r as usize) * W + x_buckets_parade_r[orig_x]] += 1;
            parade_bins[(255 - g as usize) * W + x_buckets_parade_g[orig_x]] += 1;
            parade_bins[(255 - b as usize) * W + x_buckets_parade_b[orig_x]] += 1;
        }
        if do_vectorscope {
            let r_f = r as f32;
            let g_f = g as f32;
            let b_f = b as f32;

            let mut cb = (-0.1146 * r_f - 0.3854 * g_f + 0.5 * b_f) * 0.836;
            let mut cr = (0.5 * r_f - 0.4542 * g_f - 0.0458 * b_f) * 0.836;

            let dist_sq = cb * cb + cr * cr;
            if dist_sq > 16129.0 {
                let scale = 127.0 / dist_sq.sqrt();
                cb *= scale;
                cr *= scale;
            }

            let vx = (cb + 128.0).clamp(0.0, 255.0) as usize;
            let vy = (128.0 - cr).clamp(0.0, 255.0) as usize;
            vector_bins[vy * W + vx] += 1;
        }
    };

    match image {
        DynamicImage::ImageRgb32F(f32_img) => {
            let raw = f32_img.as_raw();
            let stride = orig_w as usize * 3;
            for y in 0..(orig_h as usize) {
                let row = y * stride;
                for (x, &x_bucket) in x_buckets.iter().enumerate() {
                    let i = row + x * 3;
                    process_pixel(
                        (raw[i].clamp(0.0, 1.0) * 255.0) as u8,
                        (raw[i + 1].clamp(0.0, 1.0) * 255.0) as u8,
                        (raw[i + 2].clamp(0.0, 1.0) * 255.0) as u8,
                        x_bucket,
                        x,
                    );
                }
            }
        }
        _ => {
            let converted;
            let (raw, channels) = match image {
                DynamicImage::ImageRgb8(rgb) => (rgb.as_raw().as_slice(), 3),
                DynamicImage::ImageRgba8(rgba) => (rgba.as_raw().as_slice(), 4),
                _ => {
                    converted = image.to_rgb8();
                    (converted.as_raw().as_slice(), 3)
                }
            };
            let stride = orig_w as usize * channels;
            for y in 0..(orig_h as usize) {
                let row = y * stride;
                for (x, &x_bucket) in x_buckets.iter().enumerate() {
                    let i = row + x * channels;
                    process_pixel(raw[i], raw[i + 1], raw[i + 2], x_bucket, x);
                }
            }
        }
    }

    let build_lut = |bins: &[u32], do_calc: bool| -> (Vec<u8>, u32) {
        if !do_calc {
            return (vec![0; 1], 0);
        }
        let max_val = *bins.iter().max().unwrap_or(&0);
        if max_val == 0 {
            return (vec![0; 1], 0);
        }
        let scale = 255.0 / (1.0 + max_val as f32).ln();
        let lut = (0..=max_val)
            .map(|v| {
                if v == 0 {
                    0
                } else {
                    ((1.0 + v as f32).ln() * scale) as u8
                }
            })
            .collect();
        (lut, max_val)
    };

    let (lut_r, max_r) = build_lut(&red_bins, do_rgb);
    let (lut_g, max_g) = build_lut(&green_bins, do_rgb);
    let (lut_b, max_b) = build_lut(&blue_bins, do_rgb);
    let (lut_l, max_l) = build_lut(&luma_bins, do_luma);
    let (lut_p, max_p) = build_lut(&parade_bins, do_parade);
    let (lut_v, max_v) = build_lut(&vector_bins, do_vectorscope);

    let pixel_count = W * H;
    let byte_count = pixel_count * 4;

    let mut rgba_rgb = if do_rgb {
        vec![0u8; byte_count]
    } else {
        vec![]
    };
    let mut rgba_luma = if do_luma {
        vec![0u8; byte_count]
    } else {
        vec![]
    };
    let mut rgba_parade = if do_parade {
        vec![0u8; byte_count]
    } else {
        vec![]
    };
    let mut rgba_vector = if do_vectorscope {
        vec![0u8; byte_count]
    } else {
        vec![]
    };

    for i in 0..pixel_count {
        let x = i % W;
        let y = i / W;
        let off = i * 4;

        if do_rgb {
            let r = if red_bins[i] <= max_r {
                lut_r[red_bins[i] as usize]
            } else {
                0
            };
            let g = if green_bins[i] <= max_g {
                lut_g[green_bins[i] as usize]
            } else {
                0
            };
            let b = if blue_bins[i] <= max_b {
                lut_b[blue_bins[i] as usize]
            } else {
                0
            };
            if r > 0 || g > 0 || b > 0 {
                rgba_rgb[off] = r;
                rgba_rgb[off + 1] = g;
                rgba_rgb[off + 2] = b;
                rgba_rgb[off + 3] = r.max(g).max(b);
            }
        }

        if do_luma && luma_bins[i] > 0 && luma_bins[i] <= max_l {
            let l = lut_l[luma_bins[i] as usize];
            rgba_luma[off] = 255;
            rgba_luma[off + 1] = 255;
            rgba_luma[off + 2] = 255;
            rgba_luma[off + 3] = l;
        }

        if do_parade && parade_bins[i] > 0 && parade_bins[i] <= max_p {
            let bright = lut_p[parade_bins[i] as usize];
            if x < 82 {
                rgba_parade[off] = 255;
                rgba_parade[off + 3] = bright;
            } else if (87..169).contains(&x) {
                rgba_parade[off + 1] = 255;
                rgba_parade[off + 3] = bright;
            } else if x >= 174 {
                rgba_parade[off + 2] = 255;
                rgba_parade[off + 3] = bright;
            }
        }

        if do_vectorscope {
            let val = vector_bins[i];

            let dx = x as f32 - 128.0;
            let dy = 128.0 - y as f32;
            let min_d = dx.abs().min(dy.abs());
            let dist = (dx * dx + dy * dy).sqrt();

            if val > 0 && val <= max_v {
                let bright = lut_v[val as usize];

                let y_mid = 128.0;
                rgba_vector[off] = (y_mid + 1.402 * (dy / 0.836)).clamp(0.0, 255.0) as u8;
                rgba_vector[off + 1] = (y_mid - 0.344136 * (dx / 0.836) - 0.714136 * (dy / 0.836))
                    .clamp(0.0, 255.0) as u8;
                rgba_vector[off + 2] = (y_mid + 1.772 * (dx / 0.836)).clamp(0.0, 255.0) as u8;
                rgba_vector[off + 3] = bright;
            } else if min_d <= 1.0 {
                let alpha = (40.0 - min_d * 30.0).clamp(0.0, 255.0) as u8;
                rgba_vector[off] = 255;
                rgba_vector[off + 1] = 255;
                rgba_vector[off + 2] = 255;
                rgba_vector[off + 3] = alpha;
            } else if (dist - 127.0).abs() < 0.8 || (dist - 64.0).abs() < 0.8 {
                rgba_vector[off] = 255;
                rgba_vector[off + 1] = 255;
                rgba_vector[off + 2] = 255;
                rgba_vector[off + 3] = 15;
            } else if dx < 0.0 && dy > 0.0 && (dy + 1.53 * dx).abs() < 1.0 {
                rgba_vector[off] = 255;
                rgba_vector[off + 1] = 200;
                rgba_vector[off + 2] = 150;
                rgba_vector[off + 3] = 120;
            }
        }
    }

    Ok(WaveformData {
        rgb: if do_rgb {
            BASE64.encode(&rgba_rgb)
        } else {
            String::new()
        },
        luma: if do_luma {
            BASE64.encode(&rgba_luma)
        } else {
            String::new()
        },
        parade: if do_parade {
            BASE64.encode(&rgba_parade)
        } else {
            String::new()
        },
        vectorscope: if do_vectorscope {
            BASE64.encode(&rgba_vector)
        } else {
            String::new()
        },
        width: W as u32,
        height: H as u32,
    })
}

pub fn perform_auto_analysis(image: &DynamicImage) -> AutoAdjustmentResults {
    const ANALYSIS_MAX_DIM: u32 = 1024;

    const LUMA_R: f32 = 0.2126;
    const LUMA_G: f32 = 0.7152;
    const LUMA_B: f32 = 0.0722;

    const EXPOSURE_MIDPOINT: f64 = 128.0;
    const EXPOSURE_SCALE: f64 = 0.125;
    const WHITE_POINT_HARD_LIMIT: usize = 245;
    const HIGHLIGHT_LUMA_THRESHOLD: usize = 240;
    const CLIPPED_LUMA_THRESHOLD: usize = 250;
    const HIGHLIGHT_PERCENT_THRESHOLD: f64 = 0.02;
    const CLIPPED_PERCENT_THRESHOLD: f64 = 0.005;
    const EXPOSURE_CEILING: f64 = 250.0;

    const TARGET_RANGE: f64 = 220.0;
    const CONTRAST_SCALE: f64 = 10.0;
    const HIGHLIGHT_CONTRAST_REDUCE: f64 = 0.5;

    const SHADOW_LUMA_MAX: usize = 32;
    const SHADOW_PERCENT_THRESHOLD: f64 = 0.05;
    const SHADOW_BOOST_SCALE: f64 = 40.0;
    const SHADOW_MAX: f64 = 50.0;
    const HIGHLIGHT_BOOST_SCALE: f64 = 120.0;
    const HIGHLIGHT_MAX: f64 = 70.0;

    const VIBRANCY_SAT_THRESHOLD: f32 = 0.2;
    const VIBRANCY_SCALE: f64 = 120.0;

    const DEHAZE_RANGE_THRESHOLD: f64 = 120.0;
    const DEHAZE_SAT_THRESHOLD: f32 = 0.15;
    const DEHAZE_SCALE: f64 = 35.0;
    const CLARITY_RANGE_THRESHOLD: f64 = 180.0;
    const CLARITY_SCALE: f64 = 50.0;

    const VIGNETTE_CENTER_LOW: f32 = 0.25;
    const VIGNETTE_CENTER_HIGH: f32 = 0.75;

    const VIGNETTE_SCALE: f64 = 100.0;
    const VIGNETTE_CENTRE_DIFF_THRESHOLD: f32 = 0.05;
    const CENTRE_SCALE: f64 = 100.0;
    const CENTRE_MAX: f64 = 60.0;

    const MID_GRAY: f64 = 128.0;
    const BLACKS_SCALE: f64 = 0.5;
    const WHITES_SCALE: f64 = 0.2;
    const EXPOSURE_OUTPUT_SCALE: f64 = 20.0;
    const BRIGHTNESS_SCALE: f64 = 0.007;

    let analysis_preview = downscale_f32_image(image, ANALYSIS_MAX_DIM, ANALYSIS_MAX_DIM);
    let rgb_image = analysis_preview.to_rgb8();
    let total_pixels = (rgb_image.width() * rgb_image.height()) as f64;

    let (width, height) = rgb_image.dimensions();
    let cx0 = (width as f32 * VIGNETTE_CENTER_LOW) as u32;
    let cx1 = (width as f32 * VIGNETTE_CENTER_HIGH) as u32;
    let cy0 = (height as f32 * VIGNETTE_CENTER_LOW) as u32;
    let cy1 = (height as f32 * VIGNETTE_CENTER_HIGH) as u32;

    let mut luma_hist = vec![0u32; 256];
    let mut mean_saturation = 0.0f32;
    let mut center_sum = 0.0f32;
    let mut edge_sum = 0.0f32;
    let mut center_n = 0u32;
    let mut edge_n = 0u32;

    for (x, y, pixel) in rgb_image.enumerate_pixels() {
        let r = pixel[0] as f32;
        let g = pixel[1] as f32;
        let b = pixel[2] as f32;

        let luma_f = LUMA_R * r + LUMA_G * g + LUMA_B * b;
        luma_hist[(luma_f.round() as usize).min(255)] += 1;

        let r_n = r / 255.0;
        let g_n = g / 255.0;
        let b_n = b / 255.0;
        let max_c = r_n.max(g_n).max(b_n);
        let min_c = r_n.min(g_n).min(b_n);
        if max_c > 0.0 {
            let s = (max_c - min_c) / max_c;
            mean_saturation += s;
        }

        let luma_norm = luma_f / 255.0;
        if x >= cx0 && x < cx1 && y >= cy0 && y < cy1 {
            center_sum += luma_norm;
            center_n += 1;
        } else {
            edge_sum += luma_norm;
            edge_n += 1;
        }
    }

    mean_saturation /= total_pixels as f32;

    let percentile = |hist: &Vec<u32>, p: f64| -> usize {
        let target = (total_pixels * p) as u32;
        let mut cumulative = 0u32;
        for (i, &v) in hist.iter().enumerate() {
            cumulative += v;
            if cumulative >= target {
                return i;
            }
        }
        255
    };

    let p1 = percentile(&luma_hist, 0.01);
    let p50 = percentile(&luma_hist, 0.50);
    let p99 = percentile(&luma_hist, 0.99);

    let black_point = p1;
    let white_point = p99;
    let range = (white_point as f64 - black_point as f64).max(1.0);

    let highlight_percent =
        luma_hist[HIGHLIGHT_LUMA_THRESHOLD..256].iter().sum::<u32>() as f64 / total_pixels;
    let clipped_percent =
        luma_hist[CLIPPED_LUMA_THRESHOLD..256].iter().sum::<u32>() as f64 / total_pixels;

    let mut exposure = (EXPOSURE_MIDPOINT - p50 as f64) * EXPOSURE_SCALE;

    if white_point > WHITE_POINT_HARD_LIMIT
        || highlight_percent > HIGHLIGHT_PERCENT_THRESHOLD
        || clipped_percent > CLIPPED_PERCENT_THRESHOLD
    {
        exposure = exposure.min(0.0);
    }

    if white_point as f64 + exposure > EXPOSURE_CEILING {
        exposure = EXPOSURE_CEILING - white_point as f64;
    }

    let mut contrast = 0.0f64;
    if range < TARGET_RANGE {
        contrast = ((TARGET_RANGE / range) - 1.0) * CONTRAST_SCALE;
    }
    if highlight_percent > HIGHLIGHT_PERCENT_THRESHOLD {
        contrast *= HIGHLIGHT_CONTRAST_REDUCE;
    }

    let shadow_percent = luma_hist[0..SHADOW_LUMA_MAX].iter().sum::<u32>() as f64 / total_pixels;

    let mut shadows = 0.0f64;
    if shadow_percent > SHADOW_PERCENT_THRESHOLD {
        shadows = (shadow_percent * SHADOW_BOOST_SCALE).min(SHADOW_MAX);
    }

    let mut highlights = 0.0f64;
    if highlight_percent > HIGHLIGHT_PERCENT_THRESHOLD {
        highlights = -(highlight_percent * HIGHLIGHT_BOOST_SCALE).min(HIGHLIGHT_MAX);
    }

    let mut vibrancy = 0.0f64;
    if mean_saturation < VIBRANCY_SAT_THRESHOLD {
        vibrancy = (VIBRANCY_SAT_THRESHOLD - mean_saturation) as f64 * VIBRANCY_SCALE;
    }

    let mut dehaze = 0.0f64;
    if range < DEHAZE_RANGE_THRESHOLD && mean_saturation < DEHAZE_SAT_THRESHOLD {
        dehaze = (1.0 - range / DEHAZE_RANGE_THRESHOLD) * DEHAZE_SCALE;
    }

    let mut clarity = 0.0f64;
    if range < CLARITY_RANGE_THRESHOLD {
        clarity = (1.0 - range / CLARITY_RANGE_THRESHOLD) * CLARITY_SCALE;
    }

    let mut vignette_amount = 0.0f64;
    let mut centre = 0.0f64;

    if center_n > 0 && edge_n > 0 {
        let c_avg = center_sum / center_n as f32;
        let e_avg = edge_sum / edge_n as f32;

        if e_avg < c_avg {
            let diff = c_avg - e_avg;
            vignette_amount = -(diff as f64 * VIGNETTE_SCALE);

            if diff > VIGNETTE_CENTRE_DIFF_THRESHOLD {
                centre = (diff as f64 * CENTRE_SCALE).min(CENTRE_MAX);
            }
        }
    }

    let mut adjusted_luma_hist = vec![0u32; 256];
    for pixel in rgb_image.pixels() {
        let r = pixel[0] as f64;
        let g = pixel[1] as f64;
        let b = pixel[2] as f64;
        let mut luma = LUMA_R as f64 * r + LUMA_G as f64 * g + LUMA_B as f64 * b;
        luma += exposure;
        luma = (luma - MID_GRAY) * (1.0 + contrast / 100.0) + MID_GRAY;
        adjusted_luma_hist[luma.clamp(0.0, 255.0).round() as usize] += 1;
    }

    let adj_p1 = percentile(&adjusted_luma_hist, 0.01);
    let adj_p50 = percentile(&adjusted_luma_hist, 0.50);
    let adj_p99 = percentile(&adjusted_luma_hist, 0.99);
    let blacks: f64 = -(adj_p1 as f64 * BLACKS_SCALE);
    let whites: f64 = (adj_p99 as f64 - 255.0) * WHITES_SCALE;
    let brightness: f64 = (MID_GRAY - adj_p50 as f64) * BRIGHTNESS_SCALE;

    AutoAdjustmentResults {
        exposure: (exposure / EXPOSURE_OUTPUT_SCALE).clamp(-5.0, 5.0),
        brightness: brightness.clamp(-5.0, 5.0),
        contrast: contrast.clamp(-100.0, 100.0),
        highlights: highlights.clamp(-100.0, 100.0),
        shadows: shadows.clamp(-100.0, 100.0),
        vibrancy: vibrancy.clamp(-100.0, 100.0),
        vignette_amount: vignette_amount.clamp(-100.0, 100.0),
        temperature: 0.0,
        tint: 0.0,
        dehaze: dehaze.clamp(-100.0, 100.0),
        clarity: clarity.clamp(-100.0, 100.0),
        centre: centre.clamp(-100.0, 100.0),
        whites: whites.clamp(-100.0, 100.0),
        blacks: blacks.clamp(-100.0, 100.0),
        black_point: 0.0,
    }
}

pub fn auto_results_to_json(results: &AutoAdjustmentResults) -> serde_json::Value {
    json!({
        "exposure": results.exposure,
        "brightness": results.brightness,
        "contrast": results.contrast,
        "highlights": results.highlights,
        "shadows": results.shadows,
        "vibrance": results.vibrancy,
        "vignetteAmount": results.vignette_amount,
        "clarity": results.clarity,
        "centré": results.centre,

        "dehaze": results.dehaze,
        "sectionVisibility": {
            "basic": true,
            "color": true,
            "effects": true
        },
        "whites": results.whites,
        "blacks": results.blacks
    })
}

/// Recipe `grainStructure` names as the shader's grain structure. Missing
/// and unknown names use Even, the original grain, so older recipes render
/// unchanged.
pub fn grain_structure_index(name: Option<&str>) -> u32 {
    match name {
        Some("soft") => 1,
        Some("clumped") => 2,
        Some("speckled") => 3,
        Some("gritty") => 4,
        _ => 0,
    }
}

/// The recipe key naming which tone-slider response it was made with.
pub const TONE_VERSION_KEY: &str = "toneVersion";
/// The newest tone-slider response: 2 (2026-10-16) lets Shadows above +50
/// lift the quarter-tones further, at +100 as far as Camera Raw's and close
/// to Capture One's, and refits lowering Highlights (moderate values like
/// both editors, the same display levels on every camera, −100 at least
/// Camera Raw's reach; notes: reference/highlight-recovery-gfx.md, G05).
/// New edits record it; recipes without the key keep version 1, the
/// response they were made with.
pub const TONE_VERSION: u64 = 2;

/// The recipe's tone-slider version: `toneVersion`, 1 when missing, never
/// newer than this renderer knows.
pub fn tone_version_of(adjustments: &serde_json::Value) -> u64 {
    adjustments[TONE_VERSION_KEY]
        .as_u64()
        .unwrap_or(1)
        .clamp(1, TONE_VERSION)
}

/// Recipe `baseCurve` names as the shader's curve index. Unknown names use
/// the standard curve.
pub fn base_curve_index(name: Option<&str>) -> u32 {
    match name {
        Some("highContrast") => 1,
        Some("extraShadow") => 2,
        Some("linear") => 3,
        _ => 0,
    }
}

/// Auto adjust for one image: Capture One-like for RAWs rendered with the
/// Standard base look (new photographs), the inherited heuristics otherwise
/// (Classic and AgX edits, non-RAW files).
pub fn auto_adjustments_json(
    image: &DynamicImage,
    path: &str,
    is_raw: bool,
    tone_mapper: Option<&str>,
    base_curve: Option<&str>,
) -> serde_json::Value {
    if is_raw && tone_mapper.is_none_or(|name| name == "standard") {
        let look = crate::baseline_exposure::base_look(path);
        crate::auto_adjust::standard_auto_to_json(&crate::auto_adjust::standard_auto_adjustments(
            image,
            look.baseline_ev,
            crate::auto_adjust::DisplayCurve::for_look(&look),
            base_curve_index(base_curve),
        ))
    } else {
        auto_results_to_json(&perform_auto_analysis(image))
    }
}

#[cfg(test)]
mod analytics_borrow_tests {
    use super::*;

    #[test]
    fn borrowed_rgb_and_rgba_analytics_match_converted_samples() {
        for (width, height) in [(3, 7), (257, 49)] {
            let rgba = image::RgbaImage::from_fn(width, height, |x, y| {
                image::Rgba([
                    ((x * 29 + y * 17) % 256) as u8,
                    ((x * 11 + y * 7) % 256) as u8,
                    ((x * 3 + y * 37) % 256) as u8,
                    ((x * 7 + y * 19) % 256) as u8,
                ])
            });
            let rgba = DynamicImage::ImageRgba8(rgba);
            let rgb = DynamicImage::ImageRgb8(rgba.to_rgb8());
            // 16-bit fallback still takes the prior to_rgb8 conversion path.
            let reference = DynamicImage::ImageRgb16(rgb.to_rgb16());
            let expected_hist =
                serde_json::to_value(calculate_histogram_from_image(&reference).unwrap()).unwrap();
            for image in [&rgb, &rgba] {
                assert_eq!(
                    serde_json::to_value(calculate_histogram_from_image(image).unwrap()).unwrap(),
                    expected_hist
                );
                for channel in [
                    None,
                    Some("rgb"),
                    Some("luma"),
                    Some("parade"),
                    Some("vectorscope"),
                ] {
                    let expected = serde_json::to_value(
                        calculate_waveform_from_image(&reference, channel).unwrap(),
                    )
                    .unwrap();
                    let actual = serde_json::to_value(
                        calculate_waveform_from_image(image, channel).unwrap(),
                    )
                    .unwrap();
                    assert!(
                        actual == expected,
                        "analytics changed for {width}x{height}, {channel:?}"
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod raw_enhance_tests {
    use super::*;

    fn image(width: u32, height: u32, seed: u32) -> DynamicImage {
        let mut state = seed;
        DynamicImage::ImageRgb32F(image::ImageBuffer::from_fn(width, height, |_, _| {
            let mut next = || {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                // Values span shadows, highlights above 1.0 and exact zeros.
                match state % 11 {
                    0 => 0.0,
                    1 => 1.0 + (state % 1000) as f32 / 400.0,
                    _ => (state % 10_000) as f32 / 10_000.0,
                }
            };
            image::Rgb([next(), next(), next()])
        }))
    }

    #[test]
    fn banded_enhancement_matches_the_full_frame_version_bit_for_bit() {
        let mut checked = 0;
        for (width, height) in [
            (1, 1),
            (3, 7),
            (17, 255),
            (9, 256),
            (13, 257),
            (5, 520),
            (40, 600),
        ] {
            for (nr, sharpening) in [(0.0, 0.0), (8.0, 0.0), (0.0, 0.35), (8.0, 0.35), (2.5, 1.2)] {
                let source = image(width, height, width * 7919 + height);
                let mut banded = source.clone();
                remove_raw_artifacts_and_enhance(&mut banded, nr, sharpening);
                let mut reference = source;
                reference_remove_raw_artifacts_and_enhance(&mut reference, nr, sharpening);
                let (banded, reference) = (banded.into_rgb32f(), reference.into_rgb32f());
                assert!(
                    banded
                        .as_raw()
                        .iter()
                        .zip(reference.as_raw())
                        .all(|(a, b)| a.to_bits() == b.to_bits()),
                    "{width}x{height} nr={nr} sharpening={sharpening}"
                );
                checked += 1;
            }
        }
        println!("{checked} banded enhancement cases match bit for bit");
    }
}

#[cfg(test)]
mod spatial_transform_tests {
    use super::*;
    use image::ImageBuffer;

    fn chain(
        image: &DynamicImage,
        steps: u8,
        h: bool,
        v: bool,
        degrees: f32,
        crop: &Value,
    ) -> DynamicImage {
        let coarse = apply_coarse_rotation(image, steps);
        let flipped = apply_flip(coarse, h, v);
        let rotated = apply_rotation(flipped, degrees, false);
        apply_crop(rotated, crop).into_owned()
    }

    fn bits(image: &DynamicImage) -> Vec<u32> {
        match image {
            DynamicImage::ImageRgb32F(rgb) => rgb.as_raw().iter().map(|v| v.to_bits()).collect(),
            DynamicImage::ImageRgba32F(rgba) => rgba.as_raw().iter().map(|v| v.to_bits()).collect(),
            other => panic!("unexpected {:?}", other.color()),
        }
    }

    #[test]
    fn fused_spatial_transform_matches_the_step_by_step_chain_bit_for_bit() {
        let value =
            |x: u32, y: u32, c: u32| ((x * 37 + y * 101 + c * 13) % 211) as f32 / 53.0 - 1.25;
        let crops = [
            Value::Null,
            json!({"x": 0.0, "y": 0.0, "width": 10_000.0, "height": 10_000.0}),
            json!({"x": 1.0, "y": 2.0, "width": 7.0, "height": 5.0}),
            json!({"x": 2.5, "y": 0.49, "width": 3.5, "height": 100.0}),
            json!({"x": 0.0, "y": 1.0, "width": 1.0, "height": 1.0}),
            json!({"x": 50.0, "y": 0.0, "width": 4.0, "height": 4.0}),
            json!({"x": -3.0, "y": 1.0, "width": 0.4, "height": 2.0}),
            json!({"x": "bad"}),
        ];
        let angles = [0.0, 90.0, -3.5, 0.7, 45.0, 179.9, 360.0, -720.0, 12_345.6];
        let mut checked = 0;
        for (width, height) in [(1, 1), (2, 3), (17, 11), (40, 64)] {
            let images = [
                DynamicImage::ImageRgb32F(ImageBuffer::from_fn(width, height, |x, y| {
                    image::Rgb([value(x, y, 0), value(x, y, 1), value(x, y, 2)])
                })),
                DynamicImage::ImageRgba32F(ImageBuffer::from_fn(width, height, |x, y| {
                    image::Rgba([
                        value(x, y, 0),
                        value(x, y, 1),
                        value(x, y, 2),
                        value(x, y, 3),
                    ])
                })),
            ];
            for image in &images {
                for steps in 0..5 {
                    for (h, v) in [(false, false), (true, false), (false, true), (true, true)] {
                        for degrees in angles {
                            for crop in &crops {
                                let expected = chain(image, steps, h, v, degrees, crop);
                                let fused = fused_spatial_transform(
                                    image, steps, h, v, degrees, crop, false,
                                )
                                .expect("float images are supported");
                                let case = format!(
                                    "{width}x{height} {:?} steps {steps} flips {h}/{v} {degrees}° crop {crop}",
                                    image.color()
                                );
                                assert_eq!(fused.color(), expected.color(), "{case}");
                                assert_eq!(fused.dimensions(), expected.dimensions(), "{case}");
                                assert_eq!(bits(&fused), bits(&expected), "{case}");
                                checked += 1;
                            }
                        }
                    }
                }
            }
        }
        println!("{checked} fused spatial transforms match the chain");
    }

    #[test]
    fn fused_spatial_transform_leaves_integer_images_to_the_chain() {
        let image = DynamicImage::ImageRgb8(ImageBuffer::new(4, 4));
        assert!(
            fused_spatial_transform(&image, 1, true, false, 3.0, &Value::Null, false).is_none()
        );
    }
}

#[cfg(test)]
mod base_look_exposure_tests {
    use super::get_all_adjustments_from_json;
    use serde_json::json;

    #[test]
    fn standard_exposure_is_one_stop_per_unit_and_classic_keeps_its_scale() {
        let mask = json!({"id":"m","name":"M","visible":true,"invert":false,"opacity":100,
            "subMasks":[{"id":"s","type":"all","visible":true,"mode":"additive","parameters":{}}],
            "adjustments":{"exposure":-0.5}});
        let standard = get_all_adjustments_from_json(
            &json!({"exposure":1.0,"masks":[mask.clone()]}),
            true,
            None,
        );
        assert_eq!(standard.global.tonemapper_mode, 2);
        assert!((standard.global.exposure - 1.0).abs() < 1e-6);
        assert!((standard.mask_adjustments[0].exposure + 0.5).abs() < 1e-6);
        let classic = get_all_adjustments_from_json(
            &json!({"toneMapper":"basic","exposure":1.0,"masks":[mask]}),
            true,
            None,
        );
        assert!((classic.global.exposure - 1.25).abs() < 1e-6);
        assert!((classic.mask_adjustments[0].exposure + 0.625).abs() < 1e-6);
    }
}

#[cfg(test)]
mod grain_structure_tests {
    use super::get_all_adjustments_from_json;
    use serde_json::json;

    #[test]
    fn recipes_without_a_structure_keep_the_even_grain() {
        let structure = |recipe| {
            get_all_adjustments_from_json(&recipe, true, None)
                .global
                .grain_structure
        };
        assert_eq!(structure(json!({"grainAmount": 40})), 0);
        assert_eq!(
            structure(json!({"grainAmount": 40, "grainStructure": "even"})),
            0
        );
        assert_eq!(
            structure(json!({"grainAmount": 40, "grainStructure": "unknown"})),
            0
        );
        for (name, index) in [("soft", 1), ("clumped", 2), ("speckled", 3), ("gritty", 4)] {
            assert_eq!(
                structure(json!({"grainAmount": 40, "grainStructure": name})),
                index
            );
        }
    }
}

#[cfg(test)]
mod tone_version_tests {
    use super::{TONE_VERSION, get_all_adjustments_from_json};
    use serde_json::json;

    #[test]
    fn recipes_without_a_tone_version_keep_the_first_response() {
        let version = |recipe| {
            get_all_adjustments_from_json(&recipe, true, None)
                .global
                .tone_version
        };
        assert_eq!(version(json!({"shadows": 80})), 1.0);
        assert_eq!(version(json!({"shadows": 80, "toneVersion": 1})), 1.0);
        assert_eq!(version(json!({"shadows": 80, "toneVersion": 2})), 2.0);
        // A newer version than this renderer knows renders with its newest.
        assert_eq!(version(json!({"toneVersion": 9})), TONE_VERSION as f32);
    }
}

#[cfg(test)]
mod raw_detail_default_tests {
    use super::get_all_adjustments_from_json;
    use serde_json::json;

    #[test]
    fn new_raw_photos_get_lightroom_detail_defaults_only_when_unset() {
        let fresh = get_all_adjustments_from_json(&json!({}), true, None).global;
        assert_eq!(fresh.sharpness, 40.0 / 50.0);
        assert_eq!(fresh.color_noise_reduction, 0.25);
        let saved = get_all_adjustments_from_json(
            &json!({"sharpness": 0, "colorNoiseReduction": 0}),
            true,
            None,
        )
        .global;
        assert_eq!((saved.sharpness, saved.color_noise_reduction), (0.0, 0.0));
        let jpeg = get_all_adjustments_from_json(&json!({}), false, None).global;
        assert_eq!((jpeg.sharpness, jpeg.color_noise_reduction), (0.0, 0.0));
        let hidden = get_all_adjustments_from_json(
            &json!({"sectionVisibility": {"details": false}}),
            true,
            None,
        )
        .global;
        assert_eq!((hidden.sharpness, hidden.color_noise_reduction), (0.0, 0.0));
    }

    #[test]
    fn per_tool_switches_turn_off_only_their_tool() {
        let recipe = |visibility: serde_json::Value| {
            get_all_adjustments_from_json(
                &json!({
                    "temperature": 20, "saturation": 30, "clarity": 10, "exposure": 1,
                    "sharpness": 50, "colorNoiseReduction": 40,
                    "sectionVisibility": visibility,
                }),
                true,
                None,
            )
            .global
        };
        let all = recipe(json!({}));
        let no_wb = recipe(json!({"whiteBalance": false}));
        assert_eq!(no_wb.temperature, 0.0);
        assert_eq!(no_wb.saturation, all.saturation);
        let no_presence = recipe(json!({"presence": false}));
        assert_eq!((no_presence.saturation, no_presence.clarity), (0.0, 0.0));
        assert_eq!(no_presence.temperature, all.temperature);
        assert_eq!(no_presence.exposure, all.exposure);
        let no_sharpening = recipe(json!({"sharpening": false}));
        assert_eq!(no_sharpening.sharpness, 0.0);
        assert_eq!(
            no_sharpening.color_noise_reduction,
            all.color_noise_reduction
        );
        let no_nr = recipe(json!({"noiseReduction": false}));
        assert_eq!(no_nr.color_noise_reduction, 0.0);
        assert_eq!(no_nr.sharpness, all.sharpness);
        assert_ne!(all.temperature, 0.0);
    }
}

#[cfg(test)]
mod editing_tool_tests {
    #[test]
    fn relight_reflections_preserve_old_recipes() {
        let settings = |v: serde_json::Value| {
            super::get_all_adjustments_from_json(&v, false, None)
                .global
                .relight_surface[3]
        };
        assert_eq!(settings(serde_json::json!({})), 0.);
        assert_eq!(
            settings(serde_json::json!({"relightDepth":{"version":2}})),
            0.
        );
        assert_eq!(
            settings(serde_json::json!({"relightDepth":{"version":3}})),
            0.25
        );
        assert_eq!(
            settings(serde_json::json!({"relightDepth":{"version":3},"relightSkinReflections":0})),
            0.
        );
        assert_eq!(
            settings(serde_json::json!({"relightSkinReflections":120})),
            1.
        );
        assert_eq!(
            settings(serde_json::json!({"relightSkinReflections":-10})),
            0.
        );
    }

    use super::*;
    use serde_json::json;

    #[test]
    fn levels_are_off_until_a_channel_leaves_identity() {
        let neutral = get_global_adjustments_from_json(&json!({"levels": {}}), true, None);
        assert_eq!(neutral.levels_active[0], 0);
        let set = get_global_adjustments_from_json(
            &json!({"levels": {"green": {"inBlack": 51, "gamma": 2.0, "outWhite": 204}}}),
            true,
            None,
        );
        assert_eq!(set.levels_active[0], 1);
        assert!((set.levels[2][0] - 0.2).abs() < 1e-6);
        assert!((set.levels[2][2] - 0.5).abs() < 1e-6);
        assert!((set.levels_white[2] - 0.8).abs() < 1e-6);
        // Composite and other channels stay identity.
        assert_eq!(set.levels[0], [0.0, 1.0, 1.0, 0.0]);
        // The tool switch turns it off.
        let off = get_global_adjustments_from_json(
            &json!({"levels": {"rgb": {"inBlack": 30}}, "sectionVisibility": {"levels": false}}),
            true,
            None,
        );
        assert_eq!(off.levels_active[0], 0);
    }

    #[test]
    fn moire_amount_and_pattern_map_to_the_shader() {
        let off = get_global_adjustments_from_json(&json!({"moirePattern": 80}), true, None);
        assert_eq!(off.moire, [0.0; 4]);
        let on = get_global_adjustments_from_json(
            &json!({"moireAmount": 50, "moirePattern": 100}),
            true,
            None,
        );
        assert_eq!(on.moire[0], 0.5);
        assert_eq!(on.moire[1], 8.0);
        let default_pattern =
            get_global_adjustments_from_json(&json!({"moireAmount": 100}), true, None);
        assert!((default_pattern.moire[1] - 3.1).abs() < 1e-6);
    }

    #[test]
    fn defringe_ranges_follow_lightroom_sliders() {
        let g = get_global_adjustments_from_json(
            &json!({"defringePurpleAmount": 10, "defringeGreenAmount": 20}),
            true,
            None,
        );
        assert_eq!(g.defringe[0][0], 0.5);
        // Default purple 30–70 spans Oklab 282°–358°, green 40–60 136°–174°
        // (Camera Raw's slider mapping).
        assert!((g.defringe[0][1] - 282.0).abs() < 1e-3 && (g.defringe[0][2] - 358.0).abs() < 1e-3);
        assert_eq!(g.defringe[1][0], 1.0);
        assert!((g.defringe[1][1] - 136.0).abs() < 1e-3 && (g.defringe[1][2] - 174.0).abs() < 1e-3);
    }

    #[test]
    fn sharpen_version_reaches_the_shader() {
        let version = |recipe: serde_json::Value| {
            get_global_adjustments_from_json(&recipe, true, None).sharpen_falloff[3]
        };
        // Recipes without the key keep the first response.
        assert_eq!(version(json!({"sharpness": 80})), 1.0);
        assert_eq!(version(json!({"sharpness": 80, "sharpenVersion": 2})), 2.0);
        assert_eq!(
            version(json!({"sharpenVersion": 9})),
            SHARPEN_VERSION as f32
        );
        assert_eq!(version(json!({"sharpenVersion": 0})), 1.0);
        // Layers' sharpening follows it when the photo's Detail tool is hidden.
        assert_eq!(
            version(json!({"sharpenVersion": 2, "sectionVisibility": {"details": false}})),
            2.0
        );
    }

    #[test]
    fn luma_noise_version_reaches_the_shader() {
        let speck = |recipe: serde_json::Value| {
            get_global_adjustments_from_json(&recipe, true, None).luma_speck
        };
        // Recipes without the key keep the first luminance noise reduction.
        assert_eq!(speck(json!({"lumaNoiseReduction": 30})), 0.0);
        assert_eq!(speck(json!({"lumaNoiseVersion": 1})), 0.0);
        assert_eq!(speck(json!({"lumaNoiseVersion": 2})), 1.0);
        assert_eq!(speck(json!({"lumaNoiseVersion": 9})), 1.0);
        assert_eq!(luma_noise_version(&json!({"lumaNoiseVersion": 0})), 1);
        // Layers' luminance noise reduction follows it when the photo's
        // Detail tool is hidden.
        assert_eq!(
            speck(json!({"lumaNoiseVersion": 2, "sectionVisibility": {"details": false}})),
            1.0
        );
    }

    #[test]
    fn defringe_version_reaches_the_shader() {
        let version = |recipe: serde_json::Value| {
            get_global_adjustments_from_json(&recipe, true, None).defringe[0][3]
        };
        // Recipes without the key keep the first detector.
        assert_eq!(version(json!({"defringePurpleAmount": 10})), 1.0);
        assert_eq!(
            version(json!({"defringePurpleAmount": 10, "defringeVersion": 2})),
            2.0
        );
        assert_eq!(
            version(json!({"defringeVersion": 7})),
            DEFRINGE_VERSION as f32
        );
        // Layers' Defringe reads it too when the photo's Detail tool is hidden.
        assert_eq!(
            version(json!({"defringeVersion": 2, "sectionVisibility": {"details": false}})),
            2.0
        );
    }

    #[test]
    fn black_and_white_reads_the_mix() {
        let g = get_global_adjustments_from_json(
            &json!({"blackAndWhite": true, "grayMixer": {"red": -50, "magenta": 150}}),
            true,
            None,
        );
        assert_eq!(g.bw_enabled, 1);
        assert_eq!(g.bw_mix[0][0], -0.5);
        assert_eq!(g.bw_mix[1][3], 1.0);
        let colour =
            get_global_adjustments_from_json(&json!({"grayMixer": {"red": 20}}), true, None);
        assert_eq!(colour.bw_enabled, 0);
    }

    #[test]
    fn colour_ranges_reach_photo_and_layers() {
        let range =
            |hue: f64| json!({"id": format!("r{hue}"), "hue": hue, "chroma": 0.1, "hueShift": 50});
        let ranges: Vec<_> = (0..10).map(|i| range(i as f64 * 30.0)).collect();
        let all = get_all_adjustments_from_json(
            &json!({"colorRanges": ranges, "colorRangeShow": "r60",
                "masks": [{"id": "a", "name": "A", "visible": true, "invert": false, "opacity": 100,
                "subMasks": [{"id": "s", "type": "all", "visible": true, "mode": "additive", "parameters": {}}],
                "adjustments": {"colorRanges": ranges}}]}),
            true,
            None,
        );
        // Eight for the photo, four per layer; the shown range is the third.
        assert_eq!(all.global.color_range_count[0], 8);
        assert_eq!(all.global.show_clipping, 2 + 2);
        assert_eq!(all.mask_adjustments[0].color_range_count, 4);
        assert!((all.mask_adjustments[0].color_ranges[4][0] - 30.0).abs() < 1e-6);
        assert!((all.mask_adjustments[0].color_ranges[2][0] - 25.0).abs() < 1e-6);
    }

    #[test]
    fn soft_proofing_is_read_from_the_view_state() {
        let off = get_global_adjustments_from_json(&json!({}), true, None);
        assert_eq!(off.output_space, [0; 4]);
        let on = get_global_adjustments_from_json(
            &json!({ "softProof": "proPhoto", "softProofWarning": true }),
            true,
            None,
        );
        assert_eq!(on.output_space, [0, 4, 1, 0]);
        let srgb = get_global_adjustments_from_json(&json!({ "softProof": "srgb" }), true, None);
        assert_eq!(srgb.output_space, [0, 1, 0, 0]);
        assert_eq!(
            get_global_adjustments_from_json(&json!({ "softProof": "cmyk" }), true, None)
                .output_space,
            [0; 4]
        );
    }

    #[test]
    fn noise_reduction_refinements_default_to_lightroom_and_clamp() {
        let defaults = get_global_adjustments_from_json(&json!({}), true, None);
        assert_eq!(defaults.noise_detail, [0.5, 0.0, 0.5, 0.5]);
        let set = get_global_adjustments_from_json(
            &json!({"lumaNoiseDetail": 80, "lumaNoiseContrast": 140, "colorNoiseDetail": 0,
                "colorNoiseSmoothness": 25}),
            true,
            None,
        );
        assert_eq!(set.noise_detail, [0.8, 1.0, 0.0, 0.25]);
    }

    #[test]
    fn modern_grading_replaces_the_inherited_operator() {
        let recipe =
            json!({"colorGrading": {"shadows": {"hue": 220, "saturation": 40, "luminance": 10}}});
        let standard = get_global_adjustments_from_json(&recipe, true, None);
        assert_eq!(standard.grading_active, 1);
        assert_eq!(standard.color_grading_shadows.saturation, 0.0);
        assert!(standard.grading_lut.iter().any(|row| row[2] != 0.0));
        // Classic edits keep the inherited colour grading.
        let classic = get_global_adjustments_from_json(
            &json!({"toneMapper": "basic", "colorGrading": recipe["colorGrading"]}),
            true,
            None,
        );
        assert_eq!(classic.grading_active, 0);
        assert!(classic.color_grading_shadows.saturation > 0.0);
        // Layers follow the photo's tone mapper.
        let all = get_all_adjustments_from_json(
            &json!({"masks": [{"id": "a", "name": "A", "visible": true, "invert": false, "opacity": 100,
                "subMasks": [{"id": "s", "type": "all", "visible": true, "mode": "additive", "parameters": {}}],
                "adjustments": recipe}]}),
            true,
            None,
        );
        assert_eq!(all.mask_count, 1);
        assert_eq!(all.mask_adjustments[0].grading_active, 1);
    }
}

#[cfg(test)]
mod manual_distortion_tests {
    use super::*;

    #[test]
    fn manual_distortion_follows_camera_raw_and_keeps_the_corners() {
        // Camera Raw at −50: centre sampled at 0.75 of its radius; +100 at
        // 1.4; the corners (r = 1) stay put.
        assert!((manual_distortion_factor(0.0, -50.0) - 0.75).abs() < 1e-12);
        assert!((manual_distortion_factor(0.0, 100.0) - 1.4).abs() < 1e-12);
        for amount in [-100.0, -30.0, 40.0, 100.0] {
            assert!((manual_distortion_factor(1.0, amount) - 1.0).abs() < 1e-12);
            // The inverse undoes the mapping.
            for r in [0.1, 0.4, 0.8] {
                let source = r * manual_distortion_factor(r * r, amount);
                assert!(
                    (manual_distortion_inverse(source, amount) - r).abs() < 1e-9,
                    "{amount} {r}"
                );
            }
        }
    }

    #[test]
    fn the_auto_crop_scale_fills_the_frame_for_pincushion_amounts() {
        let (w, h) = (3000.0f32, 2000.0f32);
        let (cx, cy) = (w as f64 / 2.0, h as f64 / 2.0);
        let half_diagonal = (cx * cx + cy * cy).sqrt();
        for amount in [30.0f32, 100.0] {
            let params = GeometryParams {
                distortion: amount,
                ..Default::default()
            };
            let s = compute_lens_auto_crop_scale(&params, w, h);
            // The top edge's middle, divided by the zoom and mapped, stays
            // inside the frame (the inherited one-pass scale left black rows).
            let r = cy / s;
            let source = r * manual_distortion_factor((r / half_diagonal).powi(2), amount as f64);
            assert!(
                source <= cy + 1e-6,
                "{amount}: zoom {s} maps the edge to {source}"
            );
            assert!(s > 1.0);
        }
        // Barrel amounts need no zoom.
        let params = GeometryParams {
            distortion: -80.0,
            ..Default::default()
        };
        assert_eq!(compute_lens_auto_crop_scale(&params, w, h), 1.0);
    }
}

#[cfg(test)]
mod manual_transform_tests {
    use super::*;

    /// The homography in half diagonals, normalised to h[2][2] = 1.
    fn normalised(params: &GeometryParams, width: f64, height: f64) -> [[f64; 3]; 3] {
        let hd = width.hypot(height) / 2.0;
        let m = manual_transform(params, width, height);
        let h = [
            [m[0][0], m[0][1], m[0][2] / hd],
            [m[1][0], m[1][1], m[1][2] / hd],
            [m[2][0] * hd, m[2][1] * hd, m[2][2]],
        ];
        h.map(|row| row.map(|v| v / h[2][2]))
    }

    #[test]
    fn transform_sliders_follow_camera_raw() {
        let (w, h) = (6000.0, 4000.0);
        let close = |a: f64, b: f64| (a - b).abs() < 2e-3;
        // Camera Raw, D850 frame (notes: reference/transform.md).
        let v50 = normalised(
            &GeometryParams {
                vertical: 50.0,
                ..Default::default()
            },
            w,
            h,
        );
        assert!(close(v50[2][1], -0.3) && close(v50[1][1], 1.1074) && close(v50[0][0], 1.0));
        let v100 = normalised(
            &GeometryParams {
                vertical: 100.0,
                ..Default::default()
            },
            w,
            h,
        );
        assert!(close(v100[2][1], -0.6) && close(v100[1][1], 1.6799));
        // Combined perspective stretches along p only.
        let both = normalised(
            &GeometryParams {
                vertical: 50.0,
                horizontal: 50.0,
                ..Default::default()
            },
            w,
            h,
        );
        assert!(close(both[0][0], 1.1381) && close(both[0][1], 0.1379));
        let a50 = normalised(
            &GeometryParams {
                aspect: 50.0,
                ..Default::default()
            },
            w,
            h,
        );
        assert!(close(a50[0][0], 0.9535) && close(a50[1][1], 1.0488));
        let r5 = normalised(
            &GeometryParams {
                rotate: 5.0,
                ..Default::default()
            },
            w,
            h,
        );
        assert!(close(r5[1][0], 0.0872) && close(r5[0][1], -0.0871));
        // Offsets: 0.811 of the width / height per 100, Y up.
        let x20 = manual_transform(
            &GeometryParams {
                x_offset: 20.0,
                ..Default::default()
            },
            w,
            h,
        );
        assert!((x20[0][2] - 0.2 * 0.811 * w).abs() < 1e-6);
        let y20 = manual_transform(
            &GeometryParams {
                y_offset: 20.0,
                ..Default::default()
            },
            w,
            h,
        );
        assert!((y20[1][2] + 0.2 * 0.811 * h).abs() < 1e-6);
        // Rotate first, then perspective: p turns with the frame.
        let v50r5 = normalised(
            &GeometryParams {
                vertical: 50.0,
                rotate: 5.0,
                ..Default::default()
            },
            w,
            h,
        );
        assert!(close(v50r5[2][0], -0.0262) && close(v50r5[1][0], 0.0965));
        assert_eq!(
            manual_transform(&GeometryParams::default(), w, h),
            [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]
        );
    }
}

#[cfg(test)]
mod warp_points_tests {
    use super::*;

    /// A frame whose red and green channels are each pixel's own x and y.
    fn coordinates(width: u32, height: u32) -> DynamicImage {
        DynamicImage::ImageRgb32F(image::ImageBuffer::from_fn(width, height, |x, y| {
            image::Rgb([x as f32, y as f32, 0.0])
        }))
    }

    /// A warp that moves no pixel (vignetting only, automatic CA that found
    /// nothing) keeps the last row and column instead of leaving them black.
    #[test]
    fn unmoved_warps_keep_the_last_row_and_column() {
        let (w, h) = (64u32, 48u32);
        let grey = image::Rgb([0.5f32, 0.5, 0.5]);
        let flat = DynamicImage::ImageRgb32F(image::ImageBuffer::from_pixel(w, h, grey));
        let vignette = get_geometry_params_from_json(&serde_json::json!({
            "lensVignetteAmount": 50.0, "lensDistortionParams": {"vig_k1": -0.3}
        }));
        let auto_ca = GeometryParams {
            auto_ca: true,
            auto_ca_version: 3,
            ..Default::default()
        };
        for (name, params) in [("vignette", vignette), ("auto CA", auto_ca)] {
            assert!(!is_geometry_identity(&params), "{name}");
            let out = warp_image_geometry(&flat, params).into_rgb32f();
            for (x, y) in [(w - 1, 10), (10, h - 1), (w - 1, h - 1)] {
                let inner = out.get_pixel(x.min(w - 2), y.min(h - 2))[0];
                let edge = out.get_pixel(x, y)[0];
                assert!(
                    edge > 0.4 && (edge - inner).abs() < 0.05,
                    "{name} ({x},{y}): {edge} vs {inner}"
                );
            }
        }
    }

    #[test]
    fn warp_points_land_where_the_image_warp_samples() {
        let (w, h) = (240u32, 160u32);
        let image = coordinates(w, h);
        for adjustments in [
            serde_json::json!({"transformVertical": 30.0, "transformRotate": -4.0}),
            serde_json::json!({"transformHorizontal": -20.0, "transformScale": 90.0, "transformXOffset": 5.0}),
            serde_json::json!({"lensDistortionParams": {"k1": -0.1, "k2": 0.02, "k3": 0.0, "model": 0}}),
            serde_json::json!({"transformDistortion": 35.0, "transformAspect": 20.0}),
        ] {
            let params = get_geometry_params_from_json(&adjustments);
            let points = WarpPoints::new(&params, w, h);
            let warped = warp_image_geometry(&image, params.clone()).into_rgb32f();
            let unwarped = unwarp_image_geometry(&image, params).into_rgb32f();
            let mut checked = 0;
            for y in (20..h - 20).step_by(7) {
                for x in (20..w - 20).step_by(7) {
                    // A linear ramp interpolates exactly: the warped image's
                    // value is the source point it sampled.
                    let p = warped.get_pixel(x, y);
                    if p[0] > 2.0 && p[1] > 2.0 && p[0] < w as f32 - 3.0 && p[1] < h as f32 - 3.0 {
                        let (sx, sy) = points.to_source(x as f32, y as f32).unwrap();
                        assert!(
                            (sx - p[0]).abs() < 0.05 && (sy - p[1]).abs() < 0.05,
                            "{adjustments} to_source({x},{y}) = ({sx},{sy}), warp sampled ({},{})",
                            p[0],
                            p[1]
                        );
                        // And back: the source point lands on the pixel.
                        let (ox, oy) = points.to_output(sx, sy).unwrap();
                        assert!((ox - x as f32).abs() < 0.05 && (oy - y as f32).abs() < 0.05);
                        checked += 1;
                    }
                    let q = unwarped.get_pixel(x, y);
                    if q[0] > 2.0 && q[1] > 2.0 && q[0] < w as f32 - 3.0 && q[1] < h as f32 - 3.0 {
                        let (ox, oy) = points.to_output(x as f32, y as f32).unwrap();
                        assert!(
                            (ox - q[0]).abs() < 0.05 && (oy - q[1]).abs() < 0.05,
                            "{adjustments} to_output({x},{y}) = ({ox},{oy}), unwarp sampled ({},{})",
                            q[0],
                            q[1]
                        );
                    }
                }
            }
            assert!(checked > 300, "{adjustments}: {checked}");
        }
    }
}

#[cfg(test)]
mod region_tests {
    use super::*;
    use crate::adjustment_utils::apply_spatial_transformations;

    fn frame(width: u32, height: u32) -> DynamicImage {
        DynamicImage::ImageRgb32F(image::ImageBuffer::from_fn(width, height, |x, y| {
            let v = |c: u32| ((x * 37 + y * 101 + c * 13 + x * y % 7) % 211) as f32 / 97.0;
            image::Rgb([v(0), v(1) * 0.5, v(2) + 0.25])
        }))
    }

    fn bits(image: &DynamicImage) -> Vec<u32> {
        match image {
            DynamicImage::ImageRgb32F(i) => i.as_raw().iter().map(|v| v.to_bits()).collect(),
            DynamicImage::ImageRgba32F(i) => i.as_raw().iter().map(|v| v.to_bits()).collect(),
            _ => panic!("float images only"),
        }
    }

    /// Columns `cols` of rows `rows` of an RGB frame `width` wide.
    fn crop_rows(raw: &[f32], width: u32, cols: (u32, u32), rows: (u32, u32)) -> Vec<f32> {
        let stride = width as usize * 3;
        (rows.0..rows.1)
            .flat_map(|y| {
                let start = y as usize * stride;
                raw[start + cols.0 as usize * 3..start + cols.1 as usize * 3].to_vec()
            })
            .collect()
    }

    fn recipes() -> Vec<Value> {
        let lens = json!({"k1": -0.12, "k2": 0.03, "k3": 0.0, "model": 0,
            "tca_vr": 1.0012, "tca_vb": 0.9991, "tca_vr2": 0.0004, "vig_k1": -0.4, "vig_k2": 0.1});
        vec![
            json!({}),
            json!({"crop": {"x": 31.0, "y": 17.4, "width": 180.0, "height": 120.0}}),
            json!({"lensDistortionParams": lens}),
            json!({"lensDistortionParams": {"k1": 0.02, "k2": -0.01, "k3": 0.004, "model": 1},
                "lensCorrectionMode": "camera"}),
            json!({"lensDistortionParams": lens, "orientationSteps": 1, "flipHorizontal": true,
                "crop": {"x": 10.0, "y": 22.0, "width": 150.0, "height": 230.0}}),
            json!({"transformVertical": 25.0, "transformRotate": -3.0, "rotation": 3.5,
                "crop": {"x": 20.0, "y": 15.0, "width": 240.0, "height": 160.0}}),
            json!({"transformDistortion": 35.0, "orientationSteps": 2, "flipVertical": true}),
            json!({"rotation": -12.25, "orientationSteps": 3}),
            // A warp that moves no pixel samples the last row and column.
            json!({"lensVignetteAmount": 50.0, "lensDistortionParams": {"vig_k1": -0.3}}),
            json!({"transformHorizontal": -20.0, "transformScale": 90.0, "transformXOffset": 5.0,
                "lensDistortionParams": lens, "rotation": 1.0}),
        ]
    }

    #[test]
    fn a_region_is_the_same_pixels_as_the_whole_frame() {
        let (w, h) = (301u32, 211u32);
        let image = frame(w, h);
        let raw = image.as_rgb32f().unwrap().as_raw().clone();
        let mut checked = 0;
        for adjustments in recipes() {
            let warped = apply_geometry_warp(&image, &adjustments);
            let (whole, _) = apply_spatial_transformations(warped, &adjustments);
            let (fw, fh) = whole.dimensions();
            for rect in [
                [0, 0, fw, fh],
                [5, 7, 40, 30],
                [fw / 3, fh / 2, 50, 41],
                [fw - 20, fh - 9, 60, 60],
                [0, fh - 1, 10, 10],
                [fw / 2, 0, 1, 1],
            ] {
                let plan = plan_transformed_region(w, h, &adjustments, rect).expect("plannable");
                assert_eq!(plan.frame, (fw, fh), "{adjustments}");
                let [x, y, rw, rh] = plan.rect;
                let expected = whole.crop_imm(x, y, rw, rh);
                // Only the planned rows and columns.
                let ((c0, c1), (r0, r1)) = (plan.cols, plan.rows);
                let part = crop_rows(&raw, w, (c0, c1), (r0, r1));
                let source = SourceRows {
                    pixels: &part,
                    width: w,
                    height: h,
                    row0: r0,
                    col0: c0,
                    cols: c1 - c0,
                };
                let region = transformed_region(&source, &adjustments, &plan)
                    .unwrap_or_else(|| panic!("{adjustments} {rect:?} {plan:?}"));
                assert_eq!(region.color(), expected.color(), "{adjustments} {rect:?}");
                assert_eq!(
                    region.dimensions(),
                    expected.dimensions(),
                    "{adjustments} {rect:?}"
                );
                assert_eq!(bits(&region), bits(&expected), "{adjustments} {rect:?}");
                // The whole frame as the source gives the same.
                let all = SourceRows::whole(&raw, w, h);
                let again = transformed_region(&all, &adjustments, &plan).unwrap();
                assert_eq!(bits(&again), bits(&expected), "{adjustments} {rect:?}");
                checked += 1;
            }
        }
        assert_eq!(checked, recipes().len() * 6);
    }

    #[test]
    fn a_region_with_measured_ca_is_the_same_pixels_given_the_estimate() {
        let (w, h) = (301u32, 211u32);
        let image = frame(w, h);
        let rgb = image.as_rgb32f().unwrap();
        let raw = rgb.as_raw().clone();
        let lens = json!({"k1": -0.12, "k2": 0.03, "k3": 0.0, "model": 0,
            "tca_vr": 1.0012, "tca_vb": 0.9991, "tca_vr2": 0.0004});
        for adjustments in [
            json!({"lensDistortionParams": {"k1": -0.1}, "autoCaVersion": 2}),
            json!({"lensDistortionParams": lens, "autoChromaticAberration": true,
                "rotation": 2.0, "crop": {"x": 20.0, "y": 15.0, "width": 240.0, "height": 160.0}}),
            json!({"lensDistortionParams": lens, "lensCorrectionMode": "camera", "autoCaVersion": 3}),
        ] {
            let params = get_geometry_params_from_json(&adjustments);
            assert!(params.auto_ca, "{adjustments}");
            let measures = RegionMeasures::of(rgb, &adjustments);
            assert!(measures.ca.is_some());
            assert!(RegionMeasures::inputs_key(&adjustments).is_some());
            let warped = apply_geometry_warp(&image, &adjustments);
            let (whole, _) = apply_spatial_transformations(warped, &adjustments);
            let (fw, fh) = whole.dimensions();
            for rect in [[0, 0, fw, fh], [5, 7, 40, 30], [fw - 20, fh - 9, 60, 60]] {
                // Without the estimate the warp cannot be planned.
                assert!(plan_transformed_region(w, h, &adjustments, rect).is_none());
                let plan = plan_transformed_region_with(w, h, &adjustments, rect, Some(measures))
                    .expect("plannable with the estimate");
                let [x, y, rw, rh] = plan.rect;
                let expected = whole.crop_imm(x, y, rw, rh);
                let ((c0, c1), (r0, r1)) = (plan.cols, plan.rows);
                let part = crop_rows(&raw, w, (c0, c1), (r0, r1));
                let source = SourceRows {
                    pixels: &part,
                    width: w,
                    height: h,
                    row0: r0,
                    col0: c0,
                    cols: c1 - c0,
                };
                let region = transformed_region(&source, &adjustments, &plan)
                    .unwrap_or_else(|| panic!("{adjustments} {rect:?}"));
                assert_eq!(bits(&region), bits(&expected), "{adjustments} {rect:?}");
            }
        }
        // No automatic lateral CA: no estimate, and none needed.
        assert_eq!(MeasuredCa::of(rgb, &json!({})), None);
        assert_eq!(RegionMeasures::inputs_key(&json!({})), None);
    }

    /// A smooth photograph with a fringed edge: low noise (fringe radius 3).
    fn fringed_frame(width: u32, height: u32) -> DynamicImage {
        DynamicImage::ImageRgb32F(image::ImageBuffer::from_fn(width, height, |x, y| {
            let base = 0.1 + 0.3 * x as f32 / width as f32 + 0.1 * y as f32 / height as f32;
            let edge = x as i64 - (width as i64 / 2 + (y as i64 / 9));
            let bright = if edge >= 0 { 0.6 } else { 0.0 };
            // Red leads and blue trails the edge by a pixel or two.
            let red = if (-1..2).contains(&edge) { 0.25 } else { 0.0 };
            let blue = if (-3..0).contains(&edge) { 0.2 } else { 0.0 };
            image::Rgb([base + bright + red, base + bright, base + bright + blue])
        }))
    }

    #[test]
    fn a_region_with_fringe_suppression_is_the_same_pixels_given_the_radius() {
        let (w, h) = (301u32, 211u32);
        let lens = json!({"k1": -0.12, "k2": 0.03, "k3": 0.0, "model": 0,
            "tca_vr": 1.0012, "tca_vb": 0.9991, "tca_vr2": 0.0004});
        let mut radii = std::collections::BTreeSet::new();
        for image in [frame(w, h), fringed_frame(w, h)] {
            let rgb = image.as_rgb32f().unwrap();
            let raw = rgb.as_raw().clone();
            for adjustments in [
                json!({"fringeVersion": 2}),
                json!({"fringeVersion": 1, "crop": {"x": 31.0, "y": 17.4, "width": 180.0, "height": 120.0}}),
                json!({"fringeVersion": 2, "lensDistortionParams": lens, "rotation": 2.0}),
                json!({"fringeVersion": 2, "lensDistortionParams": {"k1": -0.1}, "autoCaVersion": 2,
                    "orientationSteps": 1, "flipHorizontal": true}),
            ] {
                let measures = RegionMeasures::of(rgb, &adjustments);
                radii.insert(measures.fringe_radius.expect("a fringe radius"));
                assert!(RegionMeasures::inputs_key(&adjustments).is_some());
                let warped = apply_geometry_warp(&image, &adjustments);
                let plain = apply_spatial_transformations(warped.clone(), &adjustments)
                    .0
                    .into_owned();
                let fringed = crate::edge_fringe::apply_for_recipe(warped, &adjustments, 1.0);
                let (whole, _) = apply_spatial_transformations(fringed, &adjustments);
                assert_ne!(bits(&plain), bits(&whole), "the pass changes {adjustments}");
                let (fw, fh) = whole.dimensions();
                for rect in [
                    [0, 0, fw, fh],
                    [5, 7, 40, 30],
                    [fw / 3, fh / 2, 50, 41],
                    [fw - 20, fh - 9, 60, 60],
                ] {
                    // Without the radius the fringe pass cannot be planned.
                    assert!(plan_transformed_region(w, h, &adjustments, rect).is_none());
                    let plan =
                        plan_transformed_region_with(w, h, &adjustments, rect, Some(measures))
                            .expect("plannable with the measures");
                    let [x, y, rw, rh] = plan.rect;
                    let expected = whole.crop_imm(x, y, rw, rh);
                    let ((c0, c1), (r0, r1)) = (plan.cols, plan.rows);
                    let part = crop_rows(&raw, w, (c0, c1), (r0, r1));
                    let source = SourceRows {
                        pixels: &part,
                        width: w,
                        height: h,
                        row0: r0,
                        col0: c0,
                        cols: c1 - c0,
                    };
                    let region = transformed_region(&source, &adjustments, &plan)
                        .unwrap_or_else(|| panic!("{adjustments} {rect:?}"));
                    assert_eq!(bits(&region), bits(&expected), "{adjustments} {rect:?}");
                }
            }
        }
        // Both radii occur (noisy texture, smooth photograph).
        assert_eq!(radii.into_iter().collect::<Vec<_>>(), vec![2, 3]);
    }

    #[test]
    fn a_small_region_reads_few_rows_and_refuses_too_few() {
        let (w, h) = (400u32, 300u32);
        let adjustments =
            json!({"lensDistortionParams": {"k1": -0.05, "k2": 0.0, "k3": 0.0, "model": 0}});
        let plan = plan_transformed_region(w, h, &adjustments, [180, 140, 40, 20]).unwrap();
        let ((c0, c1), (r0, r1)) = (plan.cols, plan.rows);
        assert!(r0 > 100 && r1 < 200, "{plan:?}");
        assert!(c0 > 150 && c1 < 250, "{plan:?}");
        let image = frame(w, h);
        let raw = image.as_rgb32f().unwrap().as_raw();
        let short_rows = crop_rows(raw, w, (c0, c1), (r0 + 1, r1));
        let short = SourceRows {
            pixels: &short_rows,
            width: w,
            height: h,
            row0: r0 + 1,
            col0: c0,
            cols: c1 - c0,
        };
        assert!(transformed_region(&short, &adjustments, &plan).is_none());
        let narrow_cols = crop_rows(raw, w, (c0 + 1, c1), (r0, r1));
        let narrow = SourceRows {
            pixels: &narrow_cols,
            width: w,
            height: h,
            row0: r0,
            col0: c0 + 1,
            cols: c1 - c0 - 1,
        };
        assert!(transformed_region(&narrow, &adjustments, &plan).is_none());
    }

    #[test]
    fn lens_blur_and_measured_ca_need_the_whole_frame() {
        let blur = json!({"lensBlurEnabled": true});
        assert!(plan_transformed_region(100, 80, &blur, [0, 0, 10, 10]).is_none());
        let auto_ca =
            json!({"lensDistortionParams": {"k1": -0.1}, "autoChromaticAberration": true});
        assert!(plan_transformed_region(100, 80, &auto_ca, [0, 0, 10, 10]).is_none());
        // New RAW edits measure it by default.
        let new_edit = json!({"lensDistortionParams": {"k1": -0.1}, "autoCaVersion": 2});
        assert!(plan_transformed_region(100, 80, &new_edit, [0, 0, 10, 10]).is_none());
    }

    #[test]
    fn the_measured_ca_default_gives_way_to_the_lens_data() {
        let params = |v: serde_json::Value| get_geometry_params_from_json(&v);
        // Saved recipes: only when switched on, with the first estimate.
        assert!(!params(json!({})).auto_ca);
        let on = params(json!({"autoChromaticAberration": true}));
        assert!(on.auto_ca && on.auto_ca_version == 1);
        // New edits: by default, unless the camera data or profile corrects CA.
        let new = params(json!({"autoCaVersion": 2}));
        assert!(new.auto_ca && new.auto_ca_version == 2 && !is_geometry_identity(&new));
        let camera = json!({"autoCaVersion": 2, "lensCorrectionMode": "camera",
            "lensDistortionParams": {"k1": 0.0, "tca_vr": 1.0004, "tca_vb": 0.9997}});
        assert!(!params(camera.clone()).auto_ca);
        let mut tca_off = camera.clone();
        tca_off["lensTcaEnabled"] = json!(false);
        assert!(params(tca_off).auto_ca);
        let mut forced = camera;
        forced["autoChromaticAberration"] = json!(true);
        assert!(params(forced).auto_ca);
        assert!(!params(json!({"autoCaVersion": 2, "autoChromaticAberration": false})).auto_ca);
    }

    #[test]
    fn version_3_competes_with_the_lens_data_unless_the_user_chose() {
        let params = |v: serde_json::Value| get_geometry_params_from_json(&v);
        let camera = json!({"autoCaVersion": 3, "lensCorrectionMode": "camera",
            "lensDistortionParams": {"k1": 0.0, "tca_vr": 1.0004, "tca_vb": 0.9997, "tca_vr2": 0.0002}});
        let p = params(camera.clone());
        assert!(p.auto_ca && p.auto_ca_versus_own);
        let own = own_tca_model(&p);
        assert!((own.red.0 - 0.0004).abs() < 1e-6 && (own.blue.0 + 0.0003).abs() < 1e-6);
        assert!((own.red.1 - 0.0002).abs() < 1e-9);
        // Without lens CA data version 3 is version 2: always measured.
        let p = params(json!({"autoCaVersion": 3}));
        assert!(p.auto_ca && !p.auto_ca_versus_own);
        // The user's choices: off keeps the camera data, on replaces it, a
        // changed CA amount is kept as set.
        let mut off = camera.clone();
        off["autoChromaticAberration"] = json!(false);
        assert!(!params(off).auto_ca);
        let mut on = camera.clone();
        on["autoChromaticAberration"] = json!(true);
        let p = params(on);
        assert!(p.auto_ca && !p.auto_ca_versus_own);
        let mut amount = camera.clone();
        amount["lensTcaAmount"] = json!(150);
        assert!(!params(amount).auto_ca);
        let mut disabled = camera;
        disabled["lensTcaEnabled"] = json!(false);
        let p = params(disabled);
        assert!(p.auto_ca && !p.auto_ca_versus_own);
    }
}

#[cfg(test)]
mod thumbnail_tests {
    use super::*;

    #[test]
    fn float_thumbnails_keep_their_values() {
        let image = DynamicImage::ImageRgb32F(image::ImageBuffer::from_pixel(
            400,
            300,
            image::Rgb([0.1f32, 0.5, 0.9]),
        ));
        let small = thumbnail(&image, 160, 160).to_rgb32f();
        assert_eq!(small.dimensions(), (160, 120));
        let p = small.get_pixel(80, 60);
        for (v, want) in p.0.iter().zip([0.1, 0.5, 0.9]) {
            assert!((v - want).abs() < 1e-5, "{v} vs {want}");
        }
        // Integer images keep the crate's thumbnail.
        let eight = DynamicImage::ImageRgb8(image::ImageBuffer::from_pixel(
            400,
            300,
            image::Rgb([10, 128, 230]),
        ));
        assert_eq!(thumbnail(&eight, 160, 160), eight.thumbnail(160, 160));
    }
}

#[cfg(test)]
mod render_version_tests {
    use super::*;
    use serde_json::json;

    /// The Classic look's inherited Color Grading and Calibration, on the
    /// photo and a layer, with both tool switches off.
    fn classic(version: u64, on: bool) -> PreparedAdjustments {
        let grading = json!({"shadows": {"hue": 220, "saturation": 40, "luminance": 10},
            "highlights": {"hue": 40, "saturation": 30, "luminance": 0}, "balance": 20});
        let switches = json!({"colorGrading": on, "calibration": on});
        let layer = json!({"id": "a", "name": "A", "visible": true, "invert": false, "opacity": 100,
            "subMasks": [{"id": "s", "type": "all", "visible": true, "mode": "additive", "parameters": {}}],
            "adjustments": {"colorGrading": grading, "sectionVisibility": switches}});
        get_all_adjustments_from_json(
            &json!({"toneMapper": "basic", "colorGrading": grading,
                "colorCalibration": {"redHue": 30, "blueSaturation": -20, "shadowsTint": 15},
                "sectionVisibility": switches, "masks": [layer], "renderVersion": version}),
            true,
            None,
        )
    }

    /// From `renderVersion` 2 the Classic and AgX looks' grading and
    /// calibration follow their tool switches, as the other looks' do;
    /// switched on, nothing changes.
    #[test]
    fn classic_grading_and_calibration_follow_their_switches() {
        let old = classic(1, false);
        assert!(old.global.color_grading_shadows.saturation > 0.0);
        assert!(old.global.color_calibration.red_hue != 0.0);
        assert!(old.mask_adjustments[0].color_grading_shadows.saturation > 0.0);
        let new = classic(2, false);
        let none = get_all_adjustments_from_json(
            &json!({"toneMapper": "basic", "renderVersion": 2,
                "masks": [{"id": "a", "name": "A", "visible": true, "invert": false, "opacity": 100,
                "subMasks": [{"id": "s", "type": "all", "visible": true, "mode": "additive", "parameters": {}}],
                "adjustments": {}}]}),
            true,
            None,
        );
        assert_eq!(
            bytemuck::bytes_of(&new.global.color_grading_shadows),
            bytemuck::bytes_of(&none.global.color_grading_shadows)
        );
        assert_eq!(
            new.global.color_grading_balance,
            none.global.color_grading_balance
        );
        assert_eq!(
            bytemuck::bytes_of(&new.global.color_calibration),
            bytemuck::bytes_of(&none.global.color_calibration)
        );
        assert_eq!(
            bytemuck::bytes_of(&new.mask_adjustments[0]),
            bytemuck::bytes_of(&none.mask_adjustments[0])
        );
        assert_eq!(
            bytemuck::bytes_of(&classic(1, true).uniforms),
            bytemuck::bytes_of(&classic(2, true).uniforms)
        );
    }

    /// A frame whose left half holds a colour beyond sRGB (negative red),
    /// with an edge for the bicubic weights to ring at.
    fn wide_gamut(width: u32, height: u32) -> DynamicImage {
        DynamicImage::ImageRgb32F(Rgb32FImage::from_fn(width, height, |x, y| {
            if x < width / 2 {
                image::Rgb([-0.2, 0.3 + 0.001 * y as f32, 0.6])
            } else {
                image::Rgb([0.9, 0.05, 0.05])
            }
        }))
    }

    /// The lowest red away from the frame's edge (the last pixels are
    /// sampled bilinearly, which never clipped).
    fn lowest_red(image: &DynamicImage) -> f32 {
        let rgb = image.to_rgb32f();
        let (w, h) = rgb.dimensions();
        rgb.enumerate_pixels()
            .filter(|(x, y, _)| (8..w - 8).contains(x) && (8..h - 8).contains(y))
            .map(|(_, _, p)| p.0[0])
            .fold(f32::MAX, f32::min)
    }

    /// From `renderVersion` 2 lens warps and straightening keep negative
    /// channels the source has, instead of clipping them to 0; an image
    /// without negative values renders exactly as before.
    #[test]
    fn warps_and_straightening_keep_wide_gamut_channels() {
        let source = wide_gamut(160, 120);
        let recipe = |version: u64| {
            json!({"lensDistortionParams": {"k1": -0.05, "k2": 0.02, "k3": 0.0, "model": 1,
                "tca_vr": 1.001, "tca_vb": 0.999, "vig_k1": 0.0, "vig_k2": 0.0, "vig_k3": 0.0},
                "lensCorrectionMode": "manual", "rotation": 3.0, "renderVersion": version})
        };
        for version in [1, 2] {
            let warped = apply_geometry_warp(&source, &recipe(version));
            let (turned, _) =
                crate::adjustment_utils::apply_spatial_transformations(&source, &recipe(version));
            for (name, image) in [("warp", warped.as_ref()), ("straighten", turned.as_ref())] {
                let low = lowest_red(image);
                if version == 1 {
                    assert!(low >= 0.0, "{name} v1: {low}");
                } else {
                    assert!((low + 0.2).abs() < 1e-4, "{name} v2: {low}");
                }
            }
        }
        // Without negative channels both versions are the same, ringing cut
        // off at 0 included.
        let plain = DynamicImage::ImageRgb32F(Rgb32FImage::from_fn(160, 120, |x, _| {
            if x < 80 {
                image::Rgb([0.0, 0.01, 0.0])
            } else {
                image::Rgb([1.0, 0.9, 0.8])
            }
        }));
        let v1 = apply_geometry_warp(&plain, &recipe(1)).to_rgb32f();
        let v2 = apply_geometry_warp(&plain, &recipe(2)).to_rgb32f();
        assert_eq!(v1.as_raw(), v2.as_raw());
        let (t1, _) = crate::adjustment_utils::apply_spatial_transformations(&plain, &recipe(1));
        let (t2, _) = crate::adjustment_utils::apply_spatial_transformations(&plain, &recipe(2));
        assert_eq!(t1.to_rgba32f().as_raw(), t2.to_rgba32f().as_raw());
    }
}
