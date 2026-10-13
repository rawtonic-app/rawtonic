use std::borrow::Cow;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use image::{
    DynamicImage, GenericImageView, GrayImage, ImageBuffer, ImageEncoder, ImageFormat, imageops,
};
use jxl_encoder::{
    LosslessConfig, LossyConfig, PixelLayout,
    api::{calibrated_jxl_quality, quality_to_distance},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::adjustment_utils::apply_all_transformations;
use crate::cache_utils::calculate_full_job_hash;
use crate::gpu_processing::{
    GpuContext, GpuImageCache, GpuProcessorState, ReadbackShader, RenderOutputPrecision,
    RenderRequest, RenderSource, process_and_get_dynamic_image_inner, upload_render_source,
};
use crate::image_processing::{AllAdjustments, get_all_adjustments_from_json};
use crate::lut::Lut;
use crate::mask_generation::{
    MaskDefinition, PhotoGeometry, generate_mask_bitmap, prepare_parametric_mask_source,
};
use crate::output_color_space::OutputColorSpace;

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(try_from = "u8", into = "u8")]
pub enum TiffBitDepth {
    Eight = 8,
    #[default]
    Sixteen = 16,
}

impl TryFrom<u8> for TiffBitDepth {
    type Error = String;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            8 => Ok(Self::Eight),
            16 => Ok(Self::Sixteen),
            _ => Err(format!(
                "Invalid TIFF bit depth '{}'; expected 8 or 16.",
                value
            )),
        }
    }
}

impl From<TiffBitDepth> for u8 {
    fn from(value: TiffBitDepth) -> Self {
        value as u8
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub enum ResizeMode {
    LongEdge,
    ShortEdge,
    Width,
    Height,
    /// `value` is the pixel budget in millions (1 MP = 1,000,000 pixels).
    Megapixels,
    /// `value` scales both sides, 100 = unchanged.
    Percentage,
    /// Fit inside `value` × `second_value` pixels, the box turned to the
    /// photo's orientation (Lightroom's Dimensions: 1920 × 1080 keeps a
    /// portrait photo within 1080 × 1920).
    Dimensions,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ResizeOptions {
    pub mode: ResizeMode,
    /// Pixels for the edge modes (rounded), megapixels or percent otherwise.
    pub value: f64,
    pub dont_enlarge: bool,
    /// The box's other side for `Dimensions`.
    #[serde(default)]
    pub second_value: f64,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
#[allow(clippy::enum_variant_names)]
pub enum BorderBasis {
    #[default]
    LongEdge,
    ShortEdge,
    EachEdge,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct BorderOptions {
    #[serde(default)]
    pub basis: BorderBasis,
    pub horizontal_percent: f32,
    pub vertical_percent: f32,
    pub color: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PadOptions {
    pub ratio_width: f32,
    pub ratio_height: f32,
    pub color: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ExportSettings {
    pub jpeg_quality: u8,
    #[serde(default)]
    pub tiff_bit_depth: TiffBitDepth,
    pub resize: Option<ResizeOptions>,
    #[serde(default)]
    pub border: Option<BorderOptions>,
    #[serde(default)]
    pub pad: Option<PadOptions>,
    pub keep_metadata: bool,
    /// Output colour space; the file carries its ICC profile.
    #[serde(default)]
    pub color_space: OutputColorSpace,
    #[serde(default)]
    pub preserve_timestamps: bool,
    pub strip_gps: bool,
    pub filename_template: Option<String>,
    /// Initial counter for this job. Absent in old recipes and queued jobs.
    #[serde(default)]
    pub sequence_start: Option<u32>,
    pub watermark: Option<WatermarkSettings>,
    #[serde(default)]
    pub export_masks: bool,
    #[serde(default)]
    pub preserve_folders: bool,
    #[serde(default)]
    pub destination_type: Option<String>,
    #[serde(default)]
    pub subfolder: Option<String>,
    /// Sharpening of the finished output, after the resize.
    #[serde(default)]
    pub output_sharpening: Option<crate::output_sharpening::OutputSharpening>,
}

impl ExportSettings {
    /// Original job index, including on resume. The last counter sets padding.
    pub fn filename_counter(&self, index: usize, total: usize) -> (usize, usize) {
        let start = self.sequence_start.unwrap_or(1) as usize;
        (start + index, start + total.saturating_sub(1))
    }
}

#[derive(Clone)]
pub enum ExportAdjustmentsMode {
    UseSidecars {
        active_path: Option<String>,
        active_adjustments: Option<Value>,
    },
    GlobalOverride(Value),
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub enum WatermarkAnchor {
    TopLeft,
    TopCenter,
    TopRight,
    CenterLeft,
    Center,
    CenterRight,
    BottomLeft,
    BottomCenter,
    BottomRight,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct WatermarkSettings {
    /// The watermark image; unused for a text watermark.
    #[serde(default)]
    pub path: String,
    pub anchor: WatermarkAnchor,
    /// Width in percent of the photograph's short edge.
    pub scale: f32,
    pub spacing: f32,
    pub opacity: f32,
    /// A text watermark instead of the image.
    #[serde(default)]
    pub text: Option<crate::text_watermark::TextWatermark>,
}

pub fn apply_watermark(
    base_image: &mut DynamicImage,
    watermark_settings: &WatermarkSettings,
) -> Result<(), String> {
    let (base_w, base_h) = base_image.dimensions();
    let base_min_dim = base_w.min(base_h) as f32;
    let watermark_img = match &watermark_settings.text {
        // Text is drawn at its final width, so it stays sharp.
        Some(text) => {
            let width = (base_min_dim * (watermark_settings.scale / 100.0)).round() as u32;
            match crate::text_watermark::render(text, width) {
                Some(rendered) => DynamicImage::ImageRgba8(rendered),
                None => return Ok(()),
            }
        }
        None => image::open(&watermark_settings.path)
            .map_err(|e| format!("Failed to open watermark image: {}", e))?,
    };

    let watermark_scale_factor =
        (base_min_dim * (watermark_settings.scale / 100.0)) / watermark_img.width().max(1) as f32;
    let new_wm_w = (watermark_img.width() as f32 * watermark_scale_factor).round() as u32;
    let new_wm_h = (watermark_img.height() as f32 * watermark_scale_factor).round() as u32;

    if new_wm_w == 0 || new_wm_h == 0 {
        return Ok(());
    }

    let scaled_watermark =
        watermark_img.resize_exact(new_wm_w, new_wm_h, image::imageops::FilterType::Lanczos3);
    let opacity_factor = (watermark_settings.opacity / 100.0).clamp(0.0, 1.0);

    let spacing_pixels = (base_min_dim * (watermark_settings.spacing / 100.0)) as i64;
    let (wm_w, wm_h) = scaled_watermark.dimensions();

    let x = match watermark_settings.anchor {
        WatermarkAnchor::TopLeft | WatermarkAnchor::CenterLeft | WatermarkAnchor::BottomLeft => {
            spacing_pixels
        }
        WatermarkAnchor::TopCenter | WatermarkAnchor::Center | WatermarkAnchor::BottomCenter => {
            (base_w as i64 - wm_w as i64) / 2
        }
        WatermarkAnchor::TopRight | WatermarkAnchor::CenterRight | WatermarkAnchor::BottomRight => {
            base_w as i64 - wm_w as i64 - spacing_pixels
        }
    };

    let y = match watermark_settings.anchor {
        WatermarkAnchor::TopLeft | WatermarkAnchor::TopCenter | WatermarkAnchor::TopRight => {
            spacing_pixels
        }
        WatermarkAnchor::CenterLeft | WatermarkAnchor::Center | WatermarkAnchor::CenterRight => {
            (base_h as i64 - wm_h as i64) / 2
        }
        WatermarkAnchor::BottomLeft
        | WatermarkAnchor::BottomCenter
        | WatermarkAnchor::BottomRight => base_h as i64 - wm_h as i64 - spacing_pixels,
    };

    if matches!(
        base_image,
        DynamicImage::ImageRgb16(_) | DynamicImage::ImageRgba16(_)
    ) {
        let mut base_rgba = base_image.to_rgba16();
        let mut watermark_rgba = scaled_watermark.to_rgba16();
        for pixel in watermark_rgba.pixels_mut() {
            pixel[3] = (pixel[3] as f32 * opacity_factor).round() as u16;
        }
        image::imageops::overlay(&mut base_rgba, &watermark_rgba, x, y);
        *base_image = DynamicImage::ImageRgba16(base_rgba);
    } else if matches!(
        base_image,
        DynamicImage::ImageRgb32F(_) | DynamicImage::ImageRgba32F(_)
    ) {
        let mut base_rgba = base_image.to_rgba32f();
        let mut watermark_rgba = scaled_watermark.to_rgba32f();
        for pixel in watermark_rgba.pixels_mut() {
            pixel[3] *= opacity_factor;
        }
        image::imageops::overlay(&mut base_rgba, &watermark_rgba, x, y);
        *base_image = DynamicImage::ImageRgba32F(base_rgba);
    } else {
        let mut base_rgba = base_image.to_rgba8();
        let mut watermark_rgba = scaled_watermark.to_rgba8();
        for pixel in watermark_rgba.pixels_mut() {
            pixel[3] = (pixel[3] as f32 * opacity_factor).round() as u8;
        }
        image::imageops::overlay(&mut base_rgba, &watermark_rgba, x, y);
        *base_image = DynamicImage::ImageRgba8(base_rgba);
    }

    Ok(())
}

const PAD_RATIO_EPSILON: f64 = 0.001;
const MAX_PAD_DIMENSION: u32 = 65_535;
const MAX_BORDER_PERCENT: f64 = 100.0;

#[derive(Debug, Clone, Copy)]
pub struct ExportGeometry {
    pub canvas_w: u32,
    pub canvas_h: u32,
    pub photo_w: u32,
    pub photo_h: u32,
    pub offset_x: u32,
    pub offset_y: u32,
    pub border_x: u32,
    pub border_y: u32,
}

impl ExportGeometry {
    fn has_border(&self) -> bool {
        self.border_x > 0 || self.border_y > 0
    }

    pub fn is_identity(&self) -> bool {
        self.canvas_w == self.photo_w && self.canvas_h == self.photo_h
    }
}

pub fn calculate_resize_target(
    current_w: u32,
    current_h: u32,
    resize_opts: &ResizeOptions,
) -> (u32, u32) {
    let value = resize_opts.value;
    if !(value.is_finite() && value > 0.0) {
        return (current_w, current_h);
    }
    if matches!(resize_opts.mode, ResizeMode::Dimensions) {
        let other = resize_opts.second_value;
        if !(other.is_finite() && other > 0.0) || current_w == 0 || current_h == 0 {
            return (current_w, current_h);
        }
        let (box_long, box_short) = (value.max(other), value.min(other));
        let (long, short) = (
            current_w.max(current_h) as f64,
            current_w.min(current_h) as f64,
        );
        let scale = (box_long / long).min(box_short / short);
        if resize_opts.dont_enlarge && scale >= 1.0 {
            return (current_w, current_h);
        }
        // Rounded down: the box is a limit on both sides.
        let fit = |side: u32| ((side as f64 * scale + 1e-9).floor() as u32).max(1);
        return (fit(current_w), fit(current_h));
    }
    let pixels = value.round() as u32;
    // (measured edge, whether the width is the edge that is fixed)
    let edge = match resize_opts.mode {
        ResizeMode::LongEdge => (current_w.max(current_h), current_w >= current_h),
        ResizeMode::ShortEdge => (current_w.min(current_h), current_w <= current_h),
        ResizeMode::Width => (current_w, true),
        ResizeMode::Height => (current_h, false),
        ResizeMode::Dimensions => return (current_w, current_h),
        ResizeMode::Megapixels | ResizeMode::Percentage => {
            let scale = if matches!(resize_opts.mode, ResizeMode::Megapixels) {
                (value * 1_000_000.0 / (current_w as f64 * current_h as f64).max(1.0)).sqrt()
            } else {
                value / 100.0
            };
            if resize_opts.dont_enlarge && scale >= 1.0 {
                return (current_w, current_h);
            }
            let (w, h) = (current_w as f64 * scale, current_h as f64 * scale);
            // A megapixel budget is a ceiling: round down so it is never exceeded.
            let (w, h) = if matches!(resize_opts.mode, ResizeMode::Megapixels) {
                (w.floor(), h.floor())
            } else {
                (w.round(), h.round())
            };
            return ((w as u32).max(1), (h as u32).max(1));
        }
    };
    let (measured, fix_width) = edge;
    if resize_opts.dont_enlarge && measured <= pixels {
        return (current_w, current_h);
    }

    if fix_width {
        let h = (pixels as f32 * (current_h as f32 / current_w as f32)).round() as u32;
        (pixels, h)
    } else {
        let w = (pixels as f32 * (current_w as f32 / current_h as f32)).round() as u32;
        (w, pixels)
    }
}

pub fn parse_hex_color(hex: &str) -> Option<[u8; 3]> {
    let raw = hex.trim().trim_start_matches('#');
    let normalized: String = match raw.len() {
        3 => raw.chars().flat_map(|c| [c, c]).collect(),
        6 => raw.to_string(),
        8 => raw[..6].to_string(),
        _ => return None,
    };
    Some([
        u8::from_str_radix(&normalized[0..2], 16).ok()?,
        u8::from_str_radix(&normalized[2..4], 16).ok()?,
        u8::from_str_radix(&normalized[4..6], 16).ok()?,
    ])
}

pub fn fill_color(hex: Option<&str>) -> [u8; 3] {
    hex.and_then(parse_hex_color).unwrap_or([0, 0, 0])
}

pub fn resize_target(width: u32, height: u32, resize_opts: Option<&ResizeOptions>) -> (u32, u32) {
    match resize_opts {
        Some(opts) => calculate_resize_target(width, height, opts),
        None => (width, height),
    }
}

pub fn bordered_size(width: u32, height: u32, border_x: u32, border_y: u32) -> (u32, u32) {
    (
        width.saturating_add(border_x.saturating_mul(2)),
        height.saturating_add(border_y.saturating_mul(2)),
    )
}

pub fn exceeds_max_dimension(label: &str, width: u32, height: u32) -> bool {
    let exceeds = width > MAX_PAD_DIMENSION || height > MAX_PAD_DIMENSION;
    if exceeds {
        log::warn!(
            "{} {}x{} exceeds the maximum supported dimension; skipping.",
            label,
            width,
            height
        );
    }
    exceeds
}

pub fn calculate_pad_target(
    current_w: u32,
    current_h: u32,
    pad: &PadOptions,
) -> Option<(u32, u32)> {
    let ratio_w = pad.ratio_width as f64;
    let ratio_h = pad.ratio_height as f64;
    if !ratio_w.is_finite() || !ratio_h.is_finite() || ratio_w <= 0.0 || ratio_h <= 0.0 {
        return None;
    }

    let target_ratio = ratio_w / ratio_h;
    let original_ratio = current_w as f64 / current_h as f64;
    if (original_ratio - target_ratio).abs() < PAD_RATIO_EPSILON {
        return None;
    }

    let (new_w, new_h) = if original_ratio > target_ratio {
        (current_w, (current_w as f64 / target_ratio).round() as u32)
    } else {
        ((current_h as f64 * target_ratio).round() as u32, current_h)
    };

    let new_w = new_w.max(current_w);
    let new_h = new_h.max(current_h);

    if exceeds_max_dimension("Pad target", new_w, new_h)
        || (new_w == current_w && new_h == current_h)
    {
        return None;
    }

    Some((new_w, new_h))
}

pub fn calculate_border_thickness(
    src_w: u32,
    src_h: u32,
    border: &BorderOptions,
) -> Option<(u32, u32)> {
    let horizontal = border.horizontal_percent as f64;
    let vertical = border.vertical_percent as f64;
    if !horizontal.is_finite() || !vertical.is_finite() {
        return None;
    }

    if horizontal < 0.0
        || vertical < 0.0
        || horizontal > MAX_BORDER_PERCENT
        || vertical > MAX_BORDER_PERCENT
    {
        log::warn!(
            "Border percentages {}/{} are outside 0..={}; skipping border.",
            horizontal,
            vertical,
            MAX_BORDER_PERCENT
        );
        return None;
    }

    let (basis_x, basis_y) = match border.basis {
        BorderBasis::LongEdge => {
            let edge = src_w.max(src_h) as f64;
            (edge, edge)
        }
        BorderBasis::ShortEdge => {
            let edge = src_w.min(src_h) as f64;
            (edge, edge)
        }
        BorderBasis::EachEdge => (src_w as f64, src_h as f64),
    };

    let border_x = (basis_x * horizontal / 100.0).round() as u32;
    let border_y = (basis_y * vertical / 100.0).round() as u32;

    if border_x == 0 && border_y == 0 {
        return None;
    }

    let (bordered_w, bordered_h) = bordered_size(src_w, src_h, border_x, border_y);
    if exceeds_max_dimension("Bordered size", bordered_w, bordered_h) {
        return None;
    }

    Some((border_x, border_y))
}

pub fn compute_export_geometry(
    src_w: u32,
    src_h: u32,
    settings: &ExportSettings,
) -> Option<ExportGeometry> {
    if src_w == 0 || src_h == 0 {
        return None;
    }

    let border = settings
        .border
        .as_ref()
        .and_then(|border| calculate_border_thickness(src_w, src_h, border));
    let (border_src_x, border_src_y) = border.unwrap_or((0, 0));
    let (bordered_w, bordered_h) = bordered_size(src_w, src_h, border_src_x, border_src_y);

    let pad_target = settings
        .pad
        .as_ref()
        .and_then(|pad| calculate_pad_target(bordered_w, bordered_h, pad));

    if border.is_none() && pad_target.is_none() {
        return None;
    }

    let (canvas_w, canvas_h) = pad_target.unwrap_or((bordered_w, bordered_h));

    Some(compute_fused_geometry(
        src_w,
        src_h,
        border_src_x,
        border_src_y,
        canvas_w,
        canvas_h,
        settings.resize.as_ref(),
    ))
}

#[allow(clippy::too_many_arguments)]
pub fn compute_fused_geometry(
    src_w: u32,
    src_h: u32,
    border_src_x: u32,
    border_src_y: u32,
    canvas_w: u32,
    canvas_h: u32,
    resize_opts: Option<&ResizeOptions>,
) -> ExportGeometry {
    let (final_w, final_h) = resize_target(canvas_w, canvas_h, resize_opts);
    let (final_w, final_h) = (final_w.max(1), final_h.max(1));

    let photo_w =
        ((src_w as f64 * final_w as f64 / canvas_w as f64).round() as u32).clamp(1, final_w);
    let photo_h =
        ((src_h as f64 * final_h as f64 / canvas_h as f64).round() as u32).clamp(1, final_h);

    let border_x = ((border_src_x as f64 * final_w as f64 / canvas_w as f64).round() as u32)
        .min(final_w.saturating_sub(photo_w) / 2);
    let border_y = ((border_src_y as f64 * final_h as f64 / canvas_h as f64).round() as u32)
        .min(final_h.saturating_sub(photo_h) / 2);

    ExportGeometry {
        canvas_w: final_w,
        canvas_h: final_h,
        photo_w,
        photo_h,
        offset_x: (final_w - photo_w) / 2,
        offset_y: (final_h - photo_h) / 2,
        border_x,
        border_y,
    }
}

pub fn compute_export_output_size(src_w: u32, src_h: u32, settings: &ExportSettings) -> (u32, u32) {
    match compute_export_geometry(src_w, src_h, settings) {
        Some(geometry) => (geometry.canvas_w, geometry.canvas_h),
        None => resize_target(src_w, src_h, settings.resize.as_ref()),
    }
}

trait FillChannel: image::Primitive {
    fn from_u8(value: u8) -> Self;
}

impl FillChannel for u8 {
    fn from_u8(value: u8) -> Self {
        value
    }
}

impl FillChannel for u16 {
    fn from_u8(value: u8) -> Self {
        value as u16 * 257
    }
}

impl FillChannel for f32 {
    fn from_u8(value: u8) -> Self {
        value as f32 / 255.0
    }
}

trait SolidFill: image::Pixel + 'static {
    fn from_rgb8(color: [u8; 3]) -> Self;
}

impl<T: FillChannel> SolidFill for image::Rgb<T>
where
    image::Rgb<T>: image::Pixel<Subpixel = T> + 'static,
{
    fn from_rgb8([r, g, b]: [u8; 3]) -> Self {
        image::Rgb([T::from_u8(r), T::from_u8(g), T::from_u8(b)])
    }
}

impl<T: FillChannel> SolidFill for image::Rgba<T>
where
    image::Rgba<T>: image::Pixel<Subpixel = T> + 'static,
{
    fn from_rgb8([r, g, b]: [u8; 3]) -> Self {
        image::Rgba([
            T::from_u8(r),
            T::from_u8(g),
            T::from_u8(b),
            T::DEFAULT_MAX_VALUE,
        ])
    }
}

pub fn fill_border_frame<P>(
    canvas: &mut ImageBuffer<P, Vec<P::Subpixel>>,
    geometry: &ExportGeometry,
    fill: P,
) where
    P: image::Pixel + 'static,
    P::Subpixel: 'static,
{
    let box_x = geometry.offset_x.saturating_sub(geometry.border_x);
    let box_y = geometry.offset_y.saturating_sub(geometry.border_y);
    let box_w = geometry
        .photo_w
        .saturating_add(geometry.border_x.saturating_mul(2));
    let below_photo = geometry.offset_y.saturating_add(geometry.photo_h);
    let right_of_photo = geometry.offset_x.saturating_add(geometry.photo_w);

    let strips = [
        (box_x, box_y, box_w, geometry.border_y),
        (box_x, below_photo, box_w, geometry.border_y),
        (
            box_x,
            geometry.offset_y,
            geometry.border_x,
            geometry.photo_h,
        ),
        (
            right_of_photo,
            geometry.offset_y,
            geometry.border_x,
            geometry.photo_h,
        ),
    ];

    for (x, y, width, height) in strips {
        let x_end = x.saturating_add(width).min(canvas.width());
        let y_end = y.saturating_add(height).min(canvas.height());
        for row in y..y_end {
            for col in x..x_end {
                canvas.put_pixel(col, row, fill);
            }
        }
    }
}

pub fn place_photo_on_canvas<P>(
    src: &ImageBuffer<P, Vec<P::Subpixel>>,
    geometry: &ExportGeometry,
    outer_fill: P,
    border_fill: Option<P>,
) -> ImageBuffer<P, Vec<P::Subpixel>>
where
    P: image::Pixel + 'static,
    P::Subpixel: 'static,
{
    let mut canvas = ImageBuffer::from_pixel(geometry.canvas_w, geometry.canvas_h, outer_fill);

    if let Some(fill) = border_fill
        && geometry.has_border()
    {
        fill_border_frame(&mut canvas, geometry, fill);
    }

    imageops::replace(
        &mut canvas,
        src,
        geometry.offset_x as i64,
        geometry.offset_y as i64,
    );
    canvas
}

pub fn compose_image_on_canvas(
    image: &DynamicImage,
    geometry: &ExportGeometry,
    pad_color: [u8; 3],
    border_color: [u8; 3],
) -> DynamicImage {
    fn compose<P: SolidFill>(
        src: &ImageBuffer<P, Vec<P::Subpixel>>,
        geometry: &ExportGeometry,
        pad_color: [u8; 3],
        border_color: [u8; 3],
    ) -> ImageBuffer<P, Vec<P::Subpixel>>
    where
        P::Subpixel: 'static,
    {
        place_photo_on_canvas(
            src,
            geometry,
            P::from_rgb8(pad_color),
            Some(P::from_rgb8(border_color)),
        )
    }

    match image {
        DynamicImage::ImageRgb8(buffer) => {
            DynamicImage::ImageRgb8(compose(buffer, geometry, pad_color, border_color))
        }
        DynamicImage::ImageRgba8(buffer) => {
            DynamicImage::ImageRgba8(compose(buffer, geometry, pad_color, border_color))
        }
        DynamicImage::ImageRgb16(buffer) => {
            DynamicImage::ImageRgb16(compose(buffer, geometry, pad_color, border_color))
        }
        DynamicImage::ImageRgba16(buffer) => {
            DynamicImage::ImageRgba16(compose(buffer, geometry, pad_color, border_color))
        }
        DynamicImage::ImageRgb32F(buffer) => {
            DynamicImage::ImageRgb32F(compose(buffer, geometry, pad_color, border_color))
        }
        DynamicImage::ImageRgba32F(buffer) => {
            DynamicImage::ImageRgba32F(compose(buffer, geometry, pad_color, border_color))
        }
        other => DynamicImage::ImageRgba8(compose(
            &other.to_rgba8(),
            geometry,
            pad_color,
            border_color,
        )),
    }
}

pub fn relative_dir_is_safe(rel_dir: &Path) -> bool {
    rel_dir.components().all(|component| {
        matches!(
            component,
            std::path::Component::Normal(_) | std::path::Component::CurDir
        )
    })
}

#[cfg(windows)]
pub fn component_matches(left: std::path::Component<'_>, right: std::path::Component<'_>) -> bool {
    left.as_os_str()
        .to_string_lossy()
        .eq_ignore_ascii_case(&right.as_os_str().to_string_lossy())
}

#[cfg(not(windows))]
pub fn component_matches(left: std::path::Component<'_>, right: std::path::Component<'_>) -> bool {
    left == right
}

pub fn strip_prefix_preserving_source_case(
    source_path: &Path,
    base_path: &Path,
) -> Option<PathBuf> {
    let source_components: Vec<_> = source_path.components().collect();
    let base_components: Vec<_> = base_path.components().collect();

    if base_components.len() > source_components.len() {
        return None;
    }

    if !source_components
        .iter()
        .zip(base_components.iter())
        .all(|(source, base)| component_matches(*source, *base))
    {
        return None;
    }

    Some(source_components[base_components.len()..].iter().collect())
}

pub fn relative_export_dir_for_preserved_folders(
    source_path: &Path,
    base_origin_folders: &[String],
) -> Option<PathBuf> {
    base_origin_folders
        .iter()
        .filter_map(|base| {
            let base_path = Path::new(base);
            strip_prefix_preserving_source_case(source_path, base_path)
                .map(|rel_path| (base_path.components().count(), rel_path))
        })
        .max_by_key(|(component_count, _)| *component_count)
        .and_then(|(_, rel_path)| {
            let rel_dir = rel_path.parent().unwrap_or_else(|| Path::new(""));
            if relative_dir_is_safe(rel_dir) {
                Some(rel_dir.to_path_buf())
            } else {
                None
            }
        })
}

/// The folder one photograph is exported to: beside its original
/// (`originalFolder`) or in the chosen folder, then the subfolder (for every
/// destination, as Lightroom's "Put in Subfolder" and Capture One's output
/// sub folder), then, in a chosen folder, the photograph's folders below its
/// library root when `preserve_folders` is set. A subfolder that would leave
/// the destination is refused.
pub fn export_directory(
    source_path: &Path,
    output_folder: &Path,
    settings: &ExportSettings,
    base_origin_folders: &[String],
) -> Result<PathBuf, String> {
    let beside_original = settings.destination_type.as_deref() == Some("originalFolder");
    let mut dir = if beside_original {
        source_path
            .parent()
            .unwrap_or_else(|| Path::new(""))
            .to_path_buf()
    } else {
        output_folder.to_path_buf()
    };
    if let Some(sub) = settings.subfolder.as_deref() {
        let sub = sub.trim().trim_start_matches(['/', '\\']);
        if !sub.is_empty() {
            let sub = Path::new(sub);
            if !relative_dir_is_safe(sub) {
                return Err(format!(
                    "The export subfolder '{}' must stay inside the destination folder.",
                    sub.display()
                ));
            }
            dir.push(sub);
        }
    }
    if !beside_original
        && settings.preserve_folders
        && let Some(rel_dir) =
            relative_export_dir_for_preserved_folders(source_path, base_origin_folders)
    {
        dir.push(rel_dir);
    }
    Ok(dir)
}

pub fn apply_export_geometry(
    mut image: DynamicImage,
    export_settings: &ExportSettings,
) -> Result<(DynamicImage, ExportGeometry), String> {
    let (src_w, src_h) = image.dimensions();

    match compute_export_geometry(src_w, src_h, export_settings) {
        None => {
            let (target_w, target_h) = resize_target(src_w, src_h, export_settings.resize.as_ref());
            if target_w != src_w || target_h != src_h {
                image = image.resize(target_w, target_h, imageops::FilterType::Lanczos3);
            }
            if let Some(sharpening) = &export_settings.output_sharpening {
                crate::output_sharpening::apply(&mut image, sharpening);
            }

            let (width, height) = image.dimensions();
            Ok((
                image,
                ExportGeometry {
                    canvas_w: width,
                    canvas_h: height,
                    photo_w: width,
                    photo_h: height,
                    offset_x: 0,
                    offset_y: 0,
                    border_x: 0,
                    border_y: 0,
                },
            ))
        }
        Some(geometry) => {
            if geometry.photo_w != src_w || geometry.photo_h != src_h {
                image = image.resize_exact(
                    geometry.photo_w,
                    geometry.photo_h,
                    imageops::FilterType::Lanczos3,
                );
            }
            if let Some(sharpening) = &export_settings.output_sharpening {
                crate::output_sharpening::apply(&mut image, sharpening);
            }

            let pad_color = fill_color(export_settings.pad.as_ref().map(|pad| pad.color.as_str()));
            let border_color = fill_color(
                export_settings
                    .border
                    .as_ref()
                    .map(|border| border.color.as_str()),
            );

            Ok((
                compose_image_on_canvas(&image, &geometry, pad_color, border_color),
                geometry,
            ))
        }
    }
}

pub fn apply_export_geometry_and_watermark(
    image: DynamicImage,
    export_settings: &ExportSettings,
) -> Result<(DynamicImage, ExportGeometry), String> {
    let (mut image, geometry) = apply_export_geometry(image, export_settings)?;

    if let Some(watermark_settings) = &export_settings.watermark {
        apply_watermark(&mut image, watermark_settings)?;
    }
    Ok((image, geometry))
}

pub fn ensure_export_not_cancelled(cancellation_token: &AtomicBool) -> Result<(), String> {
    if cancellation_token.load(Ordering::SeqCst) {
        Err("Export cancelled".to_string())
    } else {
        Ok(())
    }
}

/// What an export render needs besides the source and recipe. The app fills
/// it from its state and preferences; the CLI builds its own.
pub struct RenderEnv<'a> {
    pub context: &'a GpuContext,
    /// The GPU processor, created or resized by the render as needed.
    pub processor: &'a Mutex<Option<GpuProcessorState>>,
    /// Uploaded sources kept between renders of the same photo.
    pub image_cache: &'a Mutex<Option<GpuImageCache>>,
    /// The tone mapper forced by the preferences, by whether the source is a RAW.
    pub tonemapper_override: &'a dyn Fn(bool) -> Option<u32>,
    /// Loads the recipe's `lutPath`; `None` renders without the LUT.
    pub load_lut: &'a dyn Fn(&str) -> Option<Arc<Lut>>,
}

/// EXIF tag names and display values, as the app caches them per source.
pub type ExifMap = std::collections::HashMap<String, String>;

/// Writes encoded export bytes to a path, given the lower-case extension.
pub type WriteExport = dyn Fn(&Path, &str, Vec<u8>) -> Result<(), String>;

/// Where an export's file goes and where its metadata comes from: the app
/// reads its EXIF cache and the session catalog and may write to the
/// Android gallery; the CLI reads the source and writes the file.
pub struct ExportOutput<'a> {
    pub path: &'a Path,
    /// Format when `path` has no extension.
    pub format: &'a str,
    /// The source's cached EXIF map (see [`export_metadata_with`](crate::exif_processing::export_metadata_with)).
    pub exif_sidecar: &'a dyn Fn() -> Option<ExifMap>,
    pub catalog: &'a dyn Fn() -> Option<rawtonic_metadata::Metadata>,
    /// Writes the encoded bytes for `path` with the given lower-case extension.
    pub write: &'a WriteExport,
}

/// The source pixels of one export: refuses a RAW shown only through its
/// embedded preview, then composites AI patches and spots.
pub fn prepare_export_source(
    source_path_str: &str,
    pristine_source: Arc<DynamicImage>,
    js_adjustments: &Value,
) -> Result<ExportSource, String> {
    rawtonic_engine::image_loader::ensure_raw_developed(source_path_str)
        .map_err(|e| e.to_string())?;
    let patched = match rawtonic_engine::image_loader::composite_patches_for_render(
        &pristine_source,
        js_adjustments,
    )
    .map_err(|e| format!("Failed to composite AI patches: {}", e))?
    {
        Cow::Borrowed(_) => None,
        Cow::Owned(image) => Some(Arc::new(image)),
    };
    Ok(ExportSource {
        pristine: pristine_source,
        patched,
    })
}

/// Encodes a rendered export with its metadata and writes it.
pub fn save_export_image(
    image: DynamicImage,
    source_path_str: &str,
    export_settings: &ExportSettings,
    generated: bool,
    output: &ExportOutput,
) -> Result<(), String> {
    let extension = output
        .path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_lowercase();

    let (width, height) = image.dimensions();
    let metadata = crate::exif_processing::export_metadata_with(
        source_path_str,
        &extension,
        export_settings.keep_metadata,
        export_settings.strip_gps,
        width,
        height,
        generated,
        output.exif_sidecar,
        output.catalog,
    );
    let image_bytes = encode_with_metadata(image, &extension, export_settings, metadata.as_ref())?;
    #[cfg(feature = "performance-profiling")]
    let _write_stage = crate::performance::PerformanceStage::new("export_write");
    (output.write)(output.path, &extension, image_bytes)
}

/// Renders the main export file of a photo and saves it. With separate mask
/// files the main file is rendered without its mask layers. `cancelled`
/// runs between the render and the write.
#[allow(clippy::too_many_arguments)]
pub fn render_and_save(
    source_path_str: &str,
    source: ExportSource,
    js_adjustments: &Value,
    export_settings: &ExportSettings,
    env: &RenderEnv,
    is_raw: bool,
    output: &ExportOutput,
    cancelled: &dyn Fn() -> Result<(), String>,
) -> Result<(), String> {
    let mut main_export_adjustments = js_adjustments.clone();
    if export_settings.export_masks
        && let Some(obj) = main_export_adjustments.as_object_mut()
    {
        obj.insert("masks".to_string(), serde_json::json!([]));
    }

    let actual_output_format = output
        .path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or(output.format);

    #[cfg(feature = "performance-profiling")]
    let render_stage = crate::performance::PerformanceStage::new("export_render");
    let final_image = process_image_for_export(
        source_path_str,
        source,
        &main_export_adjustments,
        export_settings,
        env,
        is_raw,
        actual_output_format,
    )?;
    #[cfg(feature = "performance-profiling")]
    drop(render_stage);
    cancelled()?;
    let generated = rawtonic_engine::image_loader::has_generated_areas(js_adjustments);
    save_export_image(
        final_image,
        source_path_str,
        export_settings,
        generated,
        output,
    )
}

/// Full-resolution pixels an export renders from. Rendering takes ownership,
/// so copies only this export holds are released once they are on the GPU.
#[derive(Clone)]
pub struct ExportSource {
    pub pristine: Arc<DynamicImage>,
    /// AI patches composited over `pristine`, when the recipe has any.
    pub patched: Option<Arc<DynamicImage>>,
}

impl ExportSource {
    pub fn base(&self) -> &DynamicImage {
        self.patched.as_deref().unwrap_or(&self.pristine)
    }
}

#[allow(clippy::too_many_arguments)]
pub fn process_image_for_export_pipeline(
    path: &str,
    source: ExportSource,
    js_adjustments: &Value,
    env: &RenderEnv,
    is_raw: bool,
    debug_tag: &str,
    output_precision: RenderOutputPrecision,
    color_space: OutputColorSpace,
) -> Result<DynamicImage, String> {
    let context = env.context;
    #[cfg(feature = "performance-profiling")]
    let transform_stage = crate::performance::PerformanceStage::new("export_transform");
    let (transformed_image, unscaled_crop_offset) =
        apply_all_transformations(Cow::Borrowed(source.base()), js_adjustments);
    let (img_w, img_h) = transformed_image.dimensions();
    #[cfg(feature = "performance-profiling")]
    drop(transform_stage);
    #[cfg(feature = "performance-profiling")]
    let masks_stage = crate::performance::PerformanceStage::new("export_masks");

    let mask_definitions: Vec<MaskDefinition> = js_adjustments
        .get("masks")
        .and_then(|m| serde_json::from_value(m.clone()).ok())
        .unwrap_or_default();

    let warped_image = mask_definitions
        .iter()
        .any(MaskDefinition::requires_warped_image)
        .then(|| prepare_parametric_mask_source(&source.pristine, is_raw, 1.0, js_adjustments))
        .transpose()?;
    let (photo_w, photo_h) = source.pristine.dimensions();
    let mask_geometry = PhotoGeometry::from_adjustments(js_adjustments)
        .with_photo_size(photo_w as f64, photo_h as f64);
    let mask_bitmaps: Vec<Arc<image::GrayImage>> = mask_definitions
        .iter()
        .filter_map(|def| {
            generate_mask_bitmap(
                def,
                img_w,
                img_h,
                1.0,
                unscaled_crop_offset,
                &mask_geometry,
                warped_image.as_ref(),
            )
            .map(Arc::new)
        })
        .collect();
    drop(warped_image);
    #[cfg(feature = "performance-profiling")]
    drop(masks_stage);

    let tm_override = (env.tonemapper_override)(is_raw);
    let mut all_adjustments = get_all_adjustments_from_json(js_adjustments, is_raw, tm_override);
    crate::baseline_exposure::apply_base_look(&mut all_adjustments.global, path);
    all_adjustments.global.show_clipping = 0;
    set_output_color_space(&mut all_adjustments, color_space);

    let lut_path = js_adjustments["lutPath"].as_str();
    let lut = lut_path.and_then(|p| (env.load_lut)(p));

    let unique_hash = calculate_full_job_hash(path, js_adjustments);
    let request = RenderRequest {
        adjustments: all_adjustments,
        mask_bitmaps: &mask_bitmaps,
        lut,
        roi: None,
    };

    let Some(uploaded) = upload_render_source(context, &transformed_image, unique_hash)? else {
        return process_and_get_dynamic_image_inner(
            context,
            env.processor,
            env.image_cache,
            RenderSource::Image {
                image: transformed_image.as_ref(),
                transform_hash: unique_hash,
            },
            request,
            debug_tag,
            output_precision,
            false,
            None,
            None,
            ReadbackShader::Export,
        );
    };
    // The GPU holds the pixels now. A source shared with the editor stays
    // alive there; a source loaded for this export is freed (0.6 GB at 50 MP).
    drop(transformed_image);
    drop(source);
    crate::allocator::release_freed_memory();

    process_and_get_dynamic_image_inner(
        context,
        env.processor,
        env.image_cache,
        RenderSource::Uploaded(uploaded),
        request,
        debug_tag,
        output_precision,
        false,
        None,
        None,
        ReadbackShader::Export,
    )
}

/// Encodes an export render for `extension` and embeds `metadata` (EXIF and
/// XMP from [`exif_processing::export_metadata_with`](crate::exif_processing::export_metadata_with)).
pub fn encode_with_metadata(
    image: DynamicImage,
    extension: &str,
    export_settings: &ExportSettings,
    metadata: Option<&crate::exif_processing::ExportMetadata>,
) -> Result<Vec<u8>, String> {
    let mut image_bytes = {
        #[cfg(feature = "performance-profiling")]
        let _stage = crate::performance::PerformanceStage::new("export_encode");
        // TIFF stores RGB; the 8- or 16-bit render matches the chosen depth.
        let image = if matches!(extension, "tif" | "tiff") {
            drop_alpha_in_place(image)
        } else {
            image
        };
        let avif_exif = match (metadata, extension) {
            (Some(metadata), "avif") => metadata
                .exif
                .encode()
                .map_err(|e| log::warn!("AVIF EXIF not written: {e}"))
                .ok(),
            _ => None,
        };
        encode_image_to_bytes(
            &image,
            extension,
            export_settings.jpeg_quality,
            export_settings.tiff_bit_depth,
            export_settings.color_space,
            avif_exif,
        )?
    };
    #[cfg(feature = "performance-profiling")]
    let metadata_stage = crate::performance::PerformanceStage::new("export_metadata");

    if let Some(metadata) = metadata
        && let Err(e) =
            crate::exif_processing::embed_export_metadata(&mut image_bytes, extension, metadata)
    {
        log::warn!("Failed to write metadata: {}", e);
    }
    #[cfg(feature = "performance-profiling")]
    drop(metadata_stage);
    Ok(image_bytes)
}

/// Sets an export file's access and modification times to the capture time;
/// a missing or pre-1970 capture date leaves "now".
pub fn set_capture_file_times(dst: &Path, capture_dt: chrono::DateTime<chrono::Utc>) {
    if capture_dt.timestamp() <= 0 {
        let now = filetime::FileTime::now();
        if let Err(e) = filetime::set_file_times(dst, now, now) {
            log::warn!("Could not set timestamps on '{}': {}", dst.display(), e);
        }
        return;
    }

    let ft = filetime::FileTime::from_unix_time(
        capture_dt.timestamp(),
        capture_dt.timestamp_subsec_nanos(),
    );
    if let Err(e) = filetime::set_file_times(dst, ft, ft) {
        log::warn!("Could not set timestamps on '{}': {}", dst.display(), e);
    }
}

/// An export file name from a template: {original_filename}, {sequence}
/// (zero-padded to the batch size), {YYYY} {MM} {DD} {hh} {mm} in local time.
pub fn generate_filename_from_template(
    template: &str,
    original_path: &std::path::Path,
    sequence: usize,
    total: usize,
    file_date: &chrono::DateTime<chrono::Utc>,
) -> String {
    let stem = original_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("image");
    let sequence_str = format!(
        "{:0width$}",
        sequence,
        width = total.to_string().len().max(1)
    );
    let local_date = file_date.with_timezone(&chrono::Local);

    let mut result = template.to_string();
    result = result.replace("{original_filename}", stem);
    result = result.replace("{sequence}", &sequence_str);
    result = result.replace("{YYYY}", &local_date.format("%Y").to_string());
    result = result.replace("{MM}", &local_date.format("%m").to_string());
    result = result.replace("{DD}", &local_date.format("%d").to_string());
    result = result.replace("{hh}", &local_date.format("%H").to_string());
    result = result.replace("{mm}", &local_date.format("%M").to_string());

    result
}

pub fn render_output_precision(
    output_format: &str,
    export_settings: &ExportSettings,
) -> RenderOutputPrecision {
    if matches!(output_format.to_lowercase().as_str(), "tif" | "tiff")
        && export_settings.tiff_bit_depth == TiffBitDepth::Sixteen
    {
        RenderOutputPrecision::SixteenBit
    } else {
        RenderOutputPrecision::EightBit
    }
}

/// Drops the alpha channel of 8- and 16-bit RGBA in place, exactly as
/// `to_rgb8`/`to_rgb16` would, without a second full-size buffer (300 MB for
/// a 16-bit 50 MP TIFF). Other images are returned unchanged.
pub fn drop_alpha_in_place(image: DynamicImage) -> DynamicImage {
    fn compact<T: Copy>(samples: &mut Vec<T>) {
        let pixels = samples.len() / 4;
        for pixel in 0..pixels {
            let (source, destination) = (pixel * 4, pixel * 3);
            samples[destination] = samples[source];
            samples[destination + 1] = samples[source + 1];
            samples[destination + 2] = samples[source + 2];
        }
        samples.truncate(pixels * 3);
    }
    match image {
        DynamicImage::ImageRgba8(rgba) => {
            let (width, height) = rgba.dimensions();
            let mut samples = rgba.into_raw();
            compact(&mut samples);
            DynamicImage::ImageRgb8(ImageBuffer::from_raw(width, height, samples).unwrap())
        }
        DynamicImage::ImageRgba16(rgba) => {
            let (width, height) = rgba.dimensions();
            let mut samples = rgba.into_raw();
            compact(&mut samples);
            DynamicImage::ImageRgb16(ImageBuffer::from_raw(width, height, samples).unwrap())
        }
        other => other,
    }
}

#[allow(clippy::too_many_arguments)]
/// Renders into `space`: sRGB leaves the uniforms at their defaults.
pub fn set_output_color_space(adjustments: &mut AllAdjustments, space: OutputColorSpace) {
    // Exports never soft-proof.
    adjustments.global.output_space = [space.shader_code(), 0, 0, 0];
    adjustments.global.output_matrix =
        crate::image_processing::gpu_mat3_from_rows(space.from_linear_srgb_f32());
}

#[allow(clippy::too_many_arguments)]
pub fn process_image_for_export(
    path: &str,
    source: ExportSource,
    js_adjustments: &Value,
    export_settings: &ExportSettings,
    env: &RenderEnv,
    is_raw: bool,
    output_format: &str,
) -> Result<DynamicImage, String> {
    let processed_image = process_image_for_export_pipeline(
        path,
        source,
        js_adjustments,
        env,
        is_raw,
        "process_image_for_export",
        render_output_precision(output_format, export_settings),
        export_settings.color_space,
    )?;

    apply_export_geometry_and_watermark(processed_image, export_settings).map(|(image, _)| image)
}

pub fn build_single_mask_adjustments(
    all: &crate::image_processing::PreparedAdjustments,
    mask_index: usize,
) -> crate::image_processing::PreparedAdjustments {
    let mut single = AllAdjustments {
        global: all.global,
        mask_adjustments: all.mask_adjustments,
        mask_count: 1,
        tile_offset_x: all.tile_offset_x,
        tile_offset_y: all.tile_offset_y,
        mask_atlas_cols: all.mask_atlas_cols,
        render_region: all.render_region,
        detail_cache: all.detail_cache,
    };
    single.mask_adjustments[0] = all.mask_adjustments[mask_index];
    for i in 1..single.mask_adjustments.len() {
        single.mask_adjustments[i] = Default::default();
    }
    crate::image_processing::PreparedAdjustments {
        uniforms: single,
        relight_surface: all.relight_surface.clone(),
    }
}

pub fn encode_grayscale_to_png(bitmap: &GrayImage) -> Result<Vec<u8>, String> {
    let mut buf = Vec::new();
    let mut cursor = Cursor::new(&mut buf);
    bitmap
        .write_to(&mut cursor, ImageFormat::Png)
        .map_err(|e| e.to_string())?;
    Ok(buf)
}

/// Encodes an export with the sRGB profile the renderer's output is in: as
/// an ICC profile in JPEG, PNG, TIFF and WebP; JPEG XL declares sRGB in its
/// codestream and AVIF without a `colr` box is read as sRGB. `avif_exif` is
/// TIFF-structured EXIF for AVIF, whose encoder writes it as an item.
pub fn encode_image_to_bytes(
    image: &DynamicImage,
    output_format: &str,
    jpeg_quality: u8,
    tiff_bit_depth: TiffBitDepth,
    color_space: OutputColorSpace,
    avif_exif: Option<Vec<u8>>,
) -> Result<Vec<u8>, String> {
    let icc = || crate::icc_profile::for_space(color_space).to_vec();
    let mut image_bytes = Vec::new();
    let mut cursor = Cursor::new(&mut image_bytes);

    match output_format.to_lowercase().as_str() {
        "jxl" if color_space.is_wide() => {
            return encode_wide_jxl(image, jpeg_quality, color_space);
        }
        "jxl" => {
            let (width, height) = image.dimensions();
            let has_alpha = image.color().has_alpha();

            let jxl_data = if jpeg_quality == 100 {
                if has_alpha {
                    let rgba = image.to_rgba8();
                    LosslessConfig::new()
                        .encode(rgba.as_raw(), width, height, PixelLayout::Rgba8)
                        .map_err(|e| format!("Failed to encode lossless JXL: {}", e))?
                } else {
                    let rgb = image.to_rgb8();
                    LosslessConfig::new()
                        .encode(rgb.as_raw(), width, height, PixelLayout::Rgb8)
                        .map_err(|e| format!("Failed to encode lossless JXL: {}", e))?
                }
            } else {
                let jxl_quality = calibrated_jxl_quality(jpeg_quality as f32);
                let distance = quality_to_distance(jxl_quality);

                if has_alpha {
                    let rgba = image.to_rgba8();
                    LossyConfig::new(distance)
                        .encode(rgba.as_raw(), width, height, PixelLayout::Rgba8)
                        .map_err(|e| format!("Failed to encode lossy JXL: {}", e))?
                } else {
                    let rgb = image.to_rgb8();
                    LossyConfig::new(distance)
                        .encode(rgb.as_raw(), width, height, PixelLayout::Rgb8)
                        .map_err(|e| format!("Failed to encode lossy JXL: {}", e))?
                }
            };

            return Ok(jxl_data);
        }
        "webp" => {
            let encoder = webp::Encoder::from_image(image)
                .map_err(|_| "Failed to create WebP encoder".to_string())?;
            let webp_mem = encoder.encode(jpeg_quality as f32);
            return crate::webp_container::set_chunks(
                &webp_mem,
                Some(crate::icc_profile::for_space(color_space)),
                None,
                None,
            );
        }
        "jpg" | "jpeg" => {
            // libjpeg-turbo, 4:4:4 as before (image's encoder took 2.3x as
            // long). GPU readback is RGBA8; the alpha byte is skipped, so no
            // second full-resolution RGB copy is made.
            let converted;
            let (pixels, format, width, height) = match image {
                DynamicImage::ImageRgb8(rgb) => (
                    rgb.as_raw().as_slice(),
                    turbojpeg::PixelFormat::RGB,
                    rgb.width(),
                    rgb.height(),
                ),
                DynamicImage::ImageRgba8(rgba) => (
                    rgba.as_raw().as_slice(),
                    turbojpeg::PixelFormat::RGBX,
                    rgba.width(),
                    rgba.height(),
                ),
                other => {
                    converted = other.to_rgb8();
                    (
                        converted.as_raw().as_slice(),
                        turbojpeg::PixelFormat::RGB,
                        converted.width(),
                        converted.height(),
                    )
                }
            };
            let channels = format.size();
            let mut compressor = turbojpeg::Compressor::new().map_err(|e| e.to_string())?;
            compressor
                .set_quality(i32::from(jpeg_quality.clamp(1, 100)))
                .map_err(|e| e.to_string())?;
            compressor
                .set_subsamp(turbojpeg::Subsamp::None)
                .map_err(|e| e.to_string())?;
            let jpeg = compressor
                .compress_to_vec(turbojpeg::Image {
                    pixels,
                    width: width as usize,
                    pitch: width as usize * channels,
                    height: height as usize,
                    format,
                })
                .map_err(|e| e.to_string())?;
            std::io::Write::write_all(&mut cursor, &with_icc_profile(&jpeg, &icc()))
                .map_err(|e| e.to_string())?;
        }
        "png" => {
            let image_to_encode = if image.as_rgb32f().is_some() {
                Cow::Owned(DynamicImage::ImageRgb16(image.to_rgb16()))
            } else {
                Cow::Borrowed(image)
            };

            let mut encoder = image::codecs::png::PngEncoder::new(&mut cursor);
            encoder.set_icc_profile(icc()).map_err(|e| e.to_string())?;
            image_to_encode
                .write_with_encoder(encoder)
                .map_err(|e| e.to_string())?;
        }
        "tif" | "tiff" => {
            // Uncompressed: reserve the pixel data and headers up front rather
            // than growing by doubling (up to 1.5x the file in flight).
            let (width, height) = image.dimensions();
            let bytes_per_pixel = match tiff_bit_depth {
                TiffBitDepth::Eight => 3,
                TiffBitDepth::Sixteen => 6,
            };
            cursor
                .get_mut()
                .reserve(width as usize * height as usize * bytes_per_pixel + 64 * 1024);
            let image_to_encode = match tiff_bit_depth {
                TiffBitDepth::Eight if image.as_rgb8().is_some() => Cow::Borrowed(image),
                TiffBitDepth::Sixteen if image.as_rgb16().is_some() => Cow::Borrowed(image),
                TiffBitDepth::Eight => Cow::Owned(DynamicImage::ImageRgb8(image.to_rgb8())),
                TiffBitDepth::Sixteen => Cow::Owned(DynamicImage::ImageRgb16(image.to_rgb16())),
            };
            let mut encoder = image::codecs::tiff::TiffEncoder::new(&mut cursor);
            encoder.set_icc_profile(icc()).map_err(|e| e.to_string())?;
            image_to_encode
                .write_with_encoder(encoder)
                .map_err(|e| e.to_string())?;
        }
        "avif" => {
            // The export's quality as for JPEG and WebP (rav1e speed 4, as
            // the encoder's own default).
            let mut encoder = image::codecs::avif::AvifEncoder::new_with_speed_quality(
                &mut cursor,
                4,
                jpeg_quality.clamp(1, 100),
            );
            if let Some(exif) = avif_exif {
                encoder.set_exif_metadata(exif).map_err(|e| e.to_string())?;
            }
            image
                .write_with_encoder(encoder)
                .map_err(|e| e.to_string())?;
            // The encoder signals sRGB in the AV1 stream only; a wide space
            // gets its ICC profile as a colr property.
            if color_space.is_wide() {
                image_bytes = crate::avif_container::set_metadata(
                    &image_bytes,
                    Some(crate::icc_profile::for_space(color_space)),
                    None,
                )?;
            }
        }
        _ => return Err(format!("Unsupported file format: {}", output_format)),
    };
    Ok(image_bytes)
}

/// The JPEG XL colour encoding of a wide output space.
fn jxl_color_encoding(space: OutputColorSpace) -> jxl_encoder::ColorEncoding {
    use jxl_encoder::{CIExy, ColorEncoding, CustomPrimaries};
    let [r, g, b] = space.primaries().map(|[x, y]| CIExy::new(x, y));
    let primaries = CustomPrimaries {
        red: r,
        green: g,
        blue: b,
    };
    let [wx, wy] = space.white();
    let mut encoding = match space {
        OutputColorSpace::Srgb => ColorEncoding::srgb(),
        OutputColorSpace::DisplayP3 => ColorEncoding::display_p3(),
        OutputColorSpace::AdobeRgb => ColorEncoding::with_custom_primaries(primaries),
        OutputColorSpace::ProPhoto => {
            ColorEncoding::with_custom_white_point_and_primaries(CIExy::new(wx, wy), primaries)
        }
    };
    if let crate::output_color_space::TransferCurve::Gamma(gamma) = space.transfer() {
        encoding.gamma = Some((1.0 / gamma) as f32);
    }
    encoding
}

/// JPEG XL in a wide output space. Lossless keeps the rendered values and
/// declares the space. Lossy takes them linear in the space's primaries; the
/// encoder converts those to linear sRGB for XYB as if every white were D65,
/// so ProPhoto's D50 values are first adapted to compensate.
fn encode_wide_jxl(
    image: &DynamicImage,
    jpeg_quality: u8,
    space: OutputColorSpace,
) -> Result<Vec<u8>, String> {
    let (width, height) = image.dimensions();
    let has_alpha = image.color().has_alpha();
    let encoding = jxl_color_encoding(space);
    if jpeg_quality == 100 {
        let (pixels, layout) = if has_alpha {
            (image.to_rgba8().into_raw(), PixelLayout::Rgba8)
        } else {
            (image.to_rgb8().into_raw(), PixelLayout::Rgb8)
        };
        return LosslessConfig::new()
            .encode_request(width, height, layout)
            .with_color_encoding(encoding)
            .encode(&pixels)
            .map_err(|e| format!("Failed to encode lossless JXL: {}", e));
    }
    let compensate = (space.white() != OutputColorSpace::Srgb.white()).then(|| {
        use crate::output_color_space::{invert, multiply};
        multiply(
            &invert(&space.to_linear_srgb_with_d65_white()),
            &space.to_linear_srgb(),
        )
    });
    let transfer = space.transfer();
    let decode: Vec<f32> = (0..=255)
        .map(|v| transfer.to_linear(v as f64 / 255.0) as f32)
        .collect();
    let channels = if has_alpha { 4 } else { 3 };
    let source = image.to_rgba8();
    let mut linear = Vec::with_capacity(width as usize * height as usize * channels);
    for pixel in source.pixels() {
        let [r, g, b, a] = pixel.0.map(usize::from);
        let rgb = [decode[r], decode[g], decode[b]];
        match &compensate {
            Some(m) => {
                for row in m {
                    linear.push(
                        (row[0] * f64::from(rgb[0])
                            + row[1] * f64::from(rgb[1])
                            + row[2] * f64::from(rgb[2])) as f32,
                    );
                }
            }
            None => linear.extend(rgb),
        }
        if has_alpha {
            linear.push(a as f32 / 255.0);
        }
    }
    let layout = if has_alpha {
        PixelLayout::RgbaLinearF32
    } else {
        PixelLayout::RgbLinearF32
    };
    let distance = quality_to_distance(calibrated_jxl_quality(jpeg_quality as f32));
    LossyConfig::new(distance)
        .encode_request(width, height, layout)
        .with_color_encoding(encoding)
        .encode(
            &linear
                .iter()
                .flat_map(|v| v.to_ne_bytes())
                .collect::<Vec<u8>>(),
        )
        .map_err(|e| format!("Failed to encode lossy JXL: {}", e))
}

/// Inserts an ICC profile into a baseline JPEG as APP2 "ICC_PROFILE"
/// segments (after SOI and a JFIF APP0), as image's encoder wrote it.
pub fn with_icc_profile(jpeg: &[u8], icc: &[u8]) -> Vec<u8> {
    const MAX_CHUNK: usize = 65_519;
    if icc.is_empty() || jpeg.len() < 4 || jpeg[..2] != [0xFF, 0xD8] {
        return jpeg.to_vec();
    }
    let mut at = 2;
    if jpeg[2..4] == [0xFF, 0xE0] && jpeg.len() >= 6 {
        at = 4 + u16::from_be_bytes([jpeg[4], jpeg[5]]) as usize;
    }
    let chunks: Vec<&[u8]> = icc.chunks(MAX_CHUNK).collect();
    let mut out = Vec::with_capacity(jpeg.len() + icc.len() + 18 * chunks.len());
    out.extend_from_slice(&jpeg[..at]);
    for (index, chunk) in chunks.iter().enumerate() {
        out.extend_from_slice(&[0xFF, 0xE2]);
        out.extend_from_slice(&((chunk.len() + 16) as u16).to_be_bytes());
        out.extend_from_slice(b"ICC_PROFILE\0");
        out.push(index as u8 + 1);
        out.push(chunks.len() as u8);
        out.extend_from_slice(chunk);
    }
    out.extend_from_slice(&jpeg[at..]);
    out
}

#[cfg(test)]
mod directory_tests {
    use super::*;

    fn settings(
        destination: Option<&str>,
        subfolder: Option<&str>,
        preserve: bool,
    ) -> ExportSettings {
        serde_json::from_value(serde_json::json!({
            "jpegQuality": 90, "resize": null, "keepMetadata": true, "stripGps": false,
            "filenameTemplate": null, "watermark": null, "preserveFolders": preserve,
            "destinationType": destination, "subfolder": subfolder,
        }))
        .unwrap()
    }

    #[test]
    fn subfolder_applies_to_every_destination() {
        let source = Path::new("/lib/2026/day/a.raf");
        let out = Path::new("/out");
        let roots = ["/lib".to_string()];
        let dir = |s: ExportSettings| export_directory(source, out, &s, &roots).unwrap();
        assert_eq!(dir(settings(None, None, false)), Path::new("/out"));
        assert_eq!(
            dir(settings(None, Some(" Web "), false)),
            Path::new("/out/Web")
        );
        assert_eq!(
            dir(settings(None, Some("/Web/small"), false)),
            Path::new("/out/Web/small")
        );
        assert_eq!(
            dir(settings(Some("originalFolder"), Some("Exports"), false)),
            Path::new("/lib/2026/day/Exports")
        );
        // Folders below the library root are kept inside the subfolder.
        assert_eq!(
            dir(settings(None, Some("Web"), true)),
            Path::new("/out/Web/2026/day")
        );
        // Beside the original, the folder structure already is the original's.
        assert_eq!(
            dir(settings(Some("originalFolder"), None, true)),
            Path::new("/lib/2026/day")
        );
    }

    #[test]
    fn subfolder_cannot_leave_the_destination() {
        let roots: [String; 0] = [];
        for sub in ["..", "Web/../../etc", "a/.."] {
            let result = export_directory(
                Path::new("/lib/a.raf"),
                Path::new("/out"),
                &settings(None, Some(sub), false),
                &roots,
            );
            assert!(result.is_err(), "{sub}");
        }
    }
}

#[cfg(test)]
mod resize_tests {
    use super::*;

    fn fit(mode: ResizeMode, value: f64, dont_enlarge: bool, w: u32, h: u32) -> (u32, u32) {
        calculate_resize_target(
            w,
            h,
            &ResizeOptions {
                mode,
                value,
                dont_enlarge,
                second_value: 0.0,
            },
        )
    }

    #[test]
    fn edge_modes_fit_the_named_side() {
        assert_eq!(
            fit(ResizeMode::LongEdge, 3000.0, true, 6000, 4000),
            (3000, 2000)
        );
        assert_eq!(
            fit(ResizeMode::LongEdge, 3000.0, true, 4000, 6000),
            (2000, 3000)
        );
        assert_eq!(
            fit(ResizeMode::ShortEdge, 1000.0, true, 6000, 4000),
            (1500, 1000)
        );
        assert_eq!(
            fit(ResizeMode::Width, 1200.0, true, 6000, 4000),
            (1200, 800)
        );
        assert_eq!(
            fit(ResizeMode::Height, 1200.0, true, 6000, 4000),
            (1800, 1200)
        );
        // Smaller photographs stay unless enlarging is allowed.
        assert_eq!(
            fit(ResizeMode::LongEdge, 8000.0, true, 6000, 4000),
            (6000, 4000)
        );
        assert_eq!(
            fit(ResizeMode::LongEdge, 8000.0, false, 6000, 4000),
            (8000, 5333)
        );
    }

    #[test]
    fn dimensions_fit_a_box_turned_to_the_photo() {
        let boxed = |value, second_value, dont_enlarge, w, h| {
            calculate_resize_target(
                w,
                h,
                &ResizeOptions {
                    mode: ResizeMode::Dimensions,
                    value,
                    second_value,
                    dont_enlarge,
                },
            )
        };
        // Landscape and portrait in the same 1920 x 1080 box.
        assert_eq!(boxed(1920.0, 1080.0, true, 6000, 4000), (1620, 1080));
        assert_eq!(boxed(1920.0, 1080.0, true, 4000, 6000), (1080, 1620));
        // A panorama is limited by its long side; the order of the sides does not matter.
        assert_eq!(boxed(1080.0, 1920.0, true, 9000, 3000), (1920, 640));
        assert_eq!(boxed(1920.0, 1080.0, true, 1000, 800), (1000, 800));
        assert_eq!(boxed(1920.0, 1080.0, false, 1000, 800), (1350, 1080));
        assert_eq!(boxed(1920.0, 0.0, true, 6000, 4000), (6000, 4000));
    }

    #[test]
    fn megapixels_never_exceed_the_budget() {
        let (w, h) = fit(ResizeMode::Megapixels, 2.0, true, 6000, 4000);
        assert!(w as u64 * h as u64 <= 2_000_000, "{w}x{h}");
        assert_eq!((w, h), (1732, 1154));
        // Aspect ratio is kept within a pixel.
        assert!((w as f64 / h as f64 - 1.5).abs() < 0.002);
        assert_eq!(
            fit(ResizeMode::Megapixels, 30.0, true, 6000, 4000),
            (6000, 4000)
        );
        let (w, h) = fit(ResizeMode::Megapixels, 96.0, false, 6000, 4000);
        assert!(w as u64 * h as u64 <= 96_000_000 && (w, h) == (12000, 8000));
    }

    #[test]
    fn percentage_scales_both_sides() {
        assert_eq!(
            fit(ResizeMode::Percentage, 50.0, true, 6000, 4001),
            (3000, 2001)
        );
        assert_eq!(
            fit(ResizeMode::Percentage, 150.0, true, 600, 400),
            (600, 400)
        );
        assert_eq!(
            fit(ResizeMode::Percentage, 150.0, false, 600, 400),
            (900, 600)
        );
        assert_eq!(fit(ResizeMode::Percentage, 0.001, true, 600, 400), (1, 1));
    }

    #[test]
    fn invalid_values_leave_the_size_alone() {
        for value in [0.0, -5.0, f64::NAN, f64::INFINITY] {
            assert_eq!(fit(ResizeMode::LongEdge, value, true, 600, 400), (600, 400));
            assert_eq!(
                fit(ResizeMode::Megapixels, value, false, 600, 400),
                (600, 400)
            );
        }
    }

    #[test]
    fn resize_settings_read_whole_and_fractional_values() {
        let old: ResizeOptions =
            serde_json::from_str(r#"{"mode":"longEdge","value":3000,"dontEnlarge":true}"#).unwrap();
        assert_eq!(old.value, 3000.0);
        let mp: ResizeOptions =
            serde_json::from_str(r#"{"mode":"megapixels","value":2.5,"dontEnlarge":false}"#)
                .unwrap();
        assert!(matches!(mp.mode, ResizeMode::Megapixels));
        let pc: ResizeOptions =
            serde_json::from_str(r#"{"mode":"percentage","value":50,"dontEnlarge":true}"#).unwrap();
        assert!(matches!(pc.mode, ResizeMode::Percentage));
    }
}

#[cfg(test)]
mod encoding_memory_tests {
    use super::*;

    #[test]
    fn borrowed_jpeg_encoding_matches_previous_rgb_conversion_byte_for_byte() {
        // Odd sizes exercise partial JPEG blocks; transparent pixels must retain
        // their RGB values exactly as they did when alpha was discarded first.
        let rgba = image::RgbaImage::from_fn(19, 13, |x, y| {
            image::Rgba([
                ((x * 37 + y * 17) % 256) as u8,
                ((x * 11 + y * 41) % 256) as u8,
                ((x * 73 + y * 3) % 256) as u8,
                ((x * 29 + y * 7) % 256) as u8,
            ])
        });
        let rgba = DynamicImage::ImageRgba8(rgba);
        let variants = [
            rgba.clone(),
            DynamicImage::ImageRgb8(rgba.to_rgb8()),
            DynamicImage::ImageRgb16(rgba.to_rgb16()),
            DynamicImage::ImageLuma8(rgba.to_luma8()),
        ];
        for source in &variants {
            for quality in [1, 75, 100] {
                let rgb = DynamicImage::ImageRgb8(source.to_rgb8());
                let previous = encode_image_to_bytes(
                    &rgb,
                    "jpeg",
                    quality,
                    TiffBitDepth::Sixteen,
                    OutputColorSpace::Srgb,
                    None,
                )
                .unwrap();
                let encoded = encode_image_to_bytes(
                    source,
                    "jpeg",
                    quality,
                    TiffBitDepth::Sixteen,
                    OutputColorSpace::Srgb,
                    None,
                )
                .unwrap();
                assert_eq!(
                    encoded,
                    previous,
                    "{:?}, quality {}",
                    source.color(),
                    quality
                );
                // sRGB profile embedded, file decodes at its size.
                let icc = crate::icc_profile::srgb();
                assert!(
                    encoded
                        .windows(icc.len().min(64))
                        .any(|w| w == &icc[..icc.len().min(64)])
                );
                assert!(encoded.windows(12).any(|w| w == b"ICC_PROFILE\0"));
                let decoded = image::load_from_memory(&encoded).unwrap();
                assert_eq!((decoded.width(), decoded.height()), (19, 13));
                if quality == 100 {
                    let want = rgb.to_rgb8();
                    let got = decoded.to_rgb8();
                    let worst = want
                        .as_raw()
                        .iter()
                        .zip(got.as_raw())
                        .map(|(a, b)| (*a as i32 - *b as i32).abs())
                        .max()
                        .unwrap();
                    assert!(worst <= 24, "worst {worst}");
                }
            }
        }
    }

    #[test]
    fn alpha_is_dropped_in_place_exactly_like_rgb_conversion() {
        let rgba16 = ImageBuffer::<image::Rgba<u16>, Vec<u16>>::from_fn(13, 7, |x, y| {
            image::Rgba([
                (x * 5003) as u16,
                (y * 9001) as u16,
                (x ^ y) as u16,
                (x * y) as u16,
            ])
        });
        let rgba16 = DynamicImage::ImageRgba16(rgba16);
        let rgba8 = DynamicImage::ImageRgba8(rgba16.to_rgba8());
        for (source, expected) in [
            (rgba16.clone(), DynamicImage::ImageRgb16(rgba16.to_rgb16())),
            (rgba8.clone(), DynamicImage::ImageRgb8(rgba8.to_rgb8())),
        ] {
            let compacted = drop_alpha_in_place(source);
            assert_eq!(compacted.color(), expected.color());
            assert_eq!(compacted.dimensions(), expected.dimensions());
            assert_eq!(compacted.as_bytes(), expected.as_bytes());
        }
        let luma = DynamicImage::ImageLuma8(rgba8.to_luma8());
        assert_eq!(
            drop_alpha_in_place(luma.clone()).as_bytes(),
            luma.as_bytes()
        );
    }

    #[test]
    fn borrowed_png_and_tiff_encoding_preserves_depth_and_pixels() {
        let rgb = DynamicImage::ImageRgb16(ImageBuffer::<image::Rgb<u16>, Vec<u16>>::from_fn(
            7,
            5,
            |x, y| image::Rgb([(x * 771 + y * 31) as u16, (y * 1201) as u16, 65535]),
        ));
        for source in [rgb.clone(), DynamicImage::ImageRgb8(rgb.to_rgb8())] {
            for (format, depth) in [
                ("png", TiffBitDepth::Sixteen),
                ("tiff", TiffBitDepth::Sixteen),
                ("tiff", TiffBitDepth::Eight),
            ] {
                let expected = match (format, depth) {
                    ("tiff", TiffBitDepth::Sixteen) => DynamicImage::ImageRgb16(source.to_rgb16()),
                    ("tiff", TiffBitDepth::Eight) => DynamicImage::ImageRgb8(source.to_rgb8()),
                    _ => source.clone(),
                };
                let encoded = encode_image_to_bytes(
                    &source,
                    format,
                    100,
                    depth,
                    OutputColorSpace::Srgb,
                    None,
                )
                .unwrap();
                let decoded = image::load_from_memory(&encoded).unwrap();
                assert_eq!(decoded.color(), expected.color());
                assert_eq!(decoded.as_bytes(), expected.as_bytes());
            }
        }
    }

    fn wide_test_image() -> DynamicImage {
        // Saturated primaries and secondaries of the output space, greys and
        // a ramp: a P3 or ProPhoto green is far outside sRGB.
        let mut img = image::RgbImage::new(64, 64);
        for (x, y, p) in img.enumerate_pixels_mut() {
            let v = (x * 4) as u8;
            *p = image::Rgb(match y / 8 {
                0 => [255, 0, 0],
                1 => [0, 255, 0],
                2 => [0, 0, 255],
                3 => [0, 255, 255],
                4 => [255, 0, 255],
                5 => [v, v, v],
                6 => [v, 255 - v, 40],
                _ => [200, 120, 30],
            });
        }
        DynamicImage::ImageRgb8(img)
    }

    fn decode_jxl(bytes: &[u8]) -> Vec<f32> {
        let image = jxl_oxide::JxlImage::read_with_defaults(Cursor::new(bytes)).unwrap();
        let render = image.render_frame(0).unwrap();
        render.image_all_channels().buf().to_vec()
    }

    #[test]
    fn wide_jpeg_xl_decodes_to_the_colours_of_its_space() {
        let image = wide_test_image();
        let source = image.to_rgb8();
        for space in [
            OutputColorSpace::DisplayP3,
            OutputColorSpace::AdobeRgb,
            OutputColorSpace::ProPhoto,
        ] {
            for (quality, tolerance) in [(100, 0.5 / 255.0), (95, 0.03)] {
                let bytes =
                    encode_image_to_bytes(&image, "jxl", quality, TiffBitDepth::Eight, space, None)
                        .unwrap();
                if let Ok(dir) = std::env::var("RAWTONIC_WIDE_JXL_DUMP") {
                    std::fs::write(format!("{dir}/{space:?}_{quality}.jxl"), &bytes).unwrap();
                }
                let decoded = decode_jxl(&bytes);
                assert_eq!(decoded.len(), source.as_raw().len(), "{space:?}");
                // jxl-oxide gamut-maps the lossy (XYB) images it renders, so
                // colour is judged on lossless files and lossy ones on the
                // greys; libjxl's djxl decodes the lossy files' saturated
                // rows to their values, mean error 0.005-0.016 (checked
                // 2026-10-09 with RAWTONIC_WIDE_JXL_DUMP).
                let in_srgb = |row: usize| quality == 100 || (40..48).contains(&row);
                let errors: Vec<f32> = decoded
                    .iter()
                    .zip(source.as_raw())
                    .enumerate()
                    .filter(|(i, _)| in_srgb(i / (64 * 3)))
                    .map(|(_, (d, s))| (d - *s as f32 / 255.0).abs())
                    .collect();
                let mean = errors.iter().sum::<f32>() / errors.len() as f32;
                assert!(
                    mean < tolerance,
                    "{space:?} quality {quality}: mean error {mean}"
                );
            }
        }
    }

    #[test]
    fn wide_exports_carry_their_profile() {
        let image = wide_test_image();
        for space in [
            OutputColorSpace::DisplayP3,
            OutputColorSpace::AdobeRgb,
            OutputColorSpace::ProPhoto,
        ] {
            let icc = crate::icc_profile::for_space(space);
            for format in ["jpg", "png", "tiff", "webp"] {
                let bytes =
                    encode_image_to_bytes(&image, format, 90, TiffBitDepth::Sixteen, space, None)
                        .unwrap();
                // PNG compresses its profile; JPEG splits nothing this small.
                let embedded = if format == "png" {
                    use image::ImageDecoder;
                    image::codecs::png::PngDecoder::new(Cursor::new(&bytes))
                        .unwrap()
                        .icc_profile()
                        .unwrap()
                        .unwrap()
                } else {
                    bytes.clone()
                };
                assert!(
                    embedded.windows(icc.len()).any(|w| w == icc),
                    "{format} {space:?} lacks its profile"
                );
            }
            let avif = encode_image_to_bytes(&image, "avif", 90, TiffBitDepth::Eight, space, None)
                .unwrap();
            assert!(avif.windows(icc.len()).any(|w| w == icc), "avif {space:?}");
        }
    }
}

#[cfg(test)]
mod naming_counter_tests {
    use super::*;

    #[test]
    fn counter_survives_queue_serialization_and_resume() {
        let old = serde_json::json!({"jpegQuality":90, "keepMetadata":true, "stripGps":false});
        let mut settings: ExportSettings = serde_json::from_value(old).unwrap();
        assert_eq!(settings.filename_counter(0, 3), (1, 3));
        settings.sequence_start = Some(98);
        let saved = serde_json::to_value(&settings).unwrap();
        let restored: ExportSettings = serde_json::from_value(saved).unwrap();
        let date = chrono::Utc::now();
        let (first, last) = restored.filename_counter(0, 4);
        assert_eq!(
            generate_filename_from_template(
                "{sequence}",
                std::path::Path::new("a.raf"),
                first,
                last,
                &date
            ),
            "098"
        );
        let (resumed, last) = restored.filename_counter(2, 4);
        assert_eq!(
            generate_filename_from_template(
                "{sequence}",
                std::path::Path::new("a.raf"),
                resumed,
                last,
                &date
            ),
            "100"
        );
        settings.sequence_start = Some(0);
        assert_eq!(settings.filename_counter(0, 1), (0, 0));
    }
}
