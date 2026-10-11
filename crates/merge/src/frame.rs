//! Merge inputs: each photo developed to scene-linear RGB (the develop
//! pipeline's base image, before any tone or look), with the colour and
//! capture data the merged DNG needs to develop like the source RAW.
use anyhow::{Context, Result, anyhow};
use image::Rgb32FImage;
use nalgebra::Matrix3;
use rawler::decoders::RawMetadata;
use rawtonic_engine::formats::is_raw_file;
use rawtonic_engine::image_loader::{BaseImageOptions, load_base_image};
use rawtonic_rawdecode::raw_processing::{ImageColor, image_color};
use std::path::{Path, PathBuf};

/// One photo of a merge.
pub struct Frame {
    pub path: PathBuf,
    /// Linear RGB in the develop working space (sRGB primaries, negative
    /// channels out to the ProPhoto gamut), white-balanced at the camera's
    /// As Shot balance, upright; sensor white at 1.0.
    pub image: Rgb32FImage,
    /// The decode's colour (RAWs only): turns `image` back into camera RGB.
    pub color: Option<ImageColor>,
    /// The source's EXIF (RAWs only).
    pub metadata: Option<RawMetadata>,
    pub capture: Capture,
}

/// Exposure settings read from the file.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Capture {
    pub exposure_time: Option<f64>,
    pub f_number: Option<f64>,
    pub iso: Option<f64>,
}

impl Capture {
    /// Exposure in EV relative to 1 s, f/1, ISO 100 (larger = brighter
    /// image); `None` without exposure time.
    pub fn exposure_ev(&self) -> Option<f64> {
        let t = self.exposure_time.filter(|t| *t > 0.0)?;
        let n = self.f_number.filter(|n| *n > 0.0).unwrap_or(1.0);
        let iso = self.iso.filter(|i| *i > 0.0).unwrap_or(100.0);
        Some((t * iso / 100.0 / (n * n)).log2())
    }
}

/// The base-image options for merge inputs: the user's raw settings without
/// the colour noise filter and sharpening, which the merged DNG gets once
/// when it is developed.
pub fn merge_options(user: &BaseImageOptions) -> BaseImageOptions {
    BaseImageOptions {
        color_nr_amount: 0.0,
        sharpening_amount: 0.0,
        apply_to_non_raws: false,
        ..user.clone()
    }
}

/// Develops one photo for a merge.
pub fn load_frame(path: &Path, options: &BaseImageOptions) -> Result<Frame> {
    let bytes =
        std::fs::read(path).with_context(|| format!("Failed to read {}", path.display()))?;
    load_frame_from_bytes(path, &bytes, options)
}

pub fn load_frame_from_bytes(
    path: &Path,
    bytes: &[u8],
    options: &BaseImageOptions,
) -> Result<Frame> {
    let name = path.to_string_lossy();
    let options = merge_options(options);
    let base = load_base_image(bytes, &name, false, &options, None)
        .with_context(|| format!("Failed to develop {}", path.display()))?;
    let raw = is_raw_file(path);
    let mut image = base.into_rgb32f();
    if !raw {
        // Non-RAW sources arrive display-referred (sRGB transfer).
        image.par_iter_mut_pixels(|v| *v = srgb_to_linear(*v));
    }
    let (color, metadata) = if raw {
        (image_color(bytes), raw_metadata(bytes).ok())
    } else {
        (None, None)
    };
    let capture = metadata
        .as_ref()
        .map(capture_of)
        .unwrap_or_else(|| exif_capture(bytes));
    Ok(Frame {
        path: path.to_path_buf(),
        image,
        color,
        metadata,
        capture,
    })
}

/// The exposure settings of a photo without developing it.
pub fn read_capture(path: &Path) -> Capture {
    let Ok(bytes) = std::fs::read(path) else {
        return Capture::default();
    };
    if is_raw_file(path)
        && let Ok(metadata) = raw_metadata(&bytes)
    {
        return capture_of(&metadata);
    }
    exif_capture(&bytes)
}

fn raw_metadata(bytes: &[u8]) -> Result<RawMetadata> {
    let source = rawler::rawsource::RawSource::new_from_slice(bytes);
    let decoder = rawler::get_decoder(&source)?;
    Ok(decoder.raw_metadata(&source, &rawler::decoders::RawDecodeParams::default())?)
}

fn capture_of(metadata: &RawMetadata) -> Capture {
    let rational =
        |r: &rawler::formats::tiff::Rational| (r.d != 0).then(|| r.n as f64 / r.d as f64);
    let exif = &metadata.exif;
    Capture {
        exposure_time: exif.exposure_time.as_ref().and_then(rational),
        f_number: exif.fnumber.as_ref().and_then(rational),
        iso: exif
            .iso_speed_ratings
            .map(f64::from)
            .or(exif.recommended_exposure_index.map(f64::from))
            .or(exif.iso_speed.map(f64::from)),
    }
}

/// EXIF exposure of a non-RAW file (JPEG/TIFF/…).
fn exif_capture(bytes: &[u8]) -> Capture {
    let Ok(exif) = exif::Reader::new().read_from_container(&mut std::io::Cursor::new(bytes)) else {
        return Capture::default();
    };
    let number = |tag| {
        exif.get_field(tag, exif::In::PRIMARY)
            .and_then(|f| match &f.value {
                exif::Value::Rational(v) => v.first().map(|r| r.to_f64()),
                exif::Value::Short(v) => v.first().map(|&s| s as f64),
                exif::Value::Long(v) => v.first().map(|&s| s as f64),
                _ => None,
            })
    };
    Capture {
        exposure_time: number(exif::Tag::ExposureTime),
        f_number: number(exif::Tag::FNumber),
        iso: number(exif::Tag::PhotographicSensitivity),
    }
}

pub(crate) fn srgb_to_linear(v: f32) -> f32 {
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

/// Fails unless all frames have the same size.
pub fn check_same_size(frames: &[Frame]) -> Result<(u32, u32)> {
    let first = frames.first().ok_or_else(|| anyhow!("No frames"))?;
    let dims = first.image.dimensions();
    for frame in &frames[1..] {
        if frame.image.dimensions() != dims {
            return Err(anyhow!(
                "{} is {}×{}, {} is {}×{}: all photos of this merge must have the same size and orientation",
                first.path.file_name().unwrap_or_default().to_string_lossy(),
                dims.0,
                dims.1,
                frame.path.file_name().unwrap_or_default().to_string_lossy(),
                frame.image.width(),
                frame.image.height()
            ));
        }
    }
    Ok(dims)
}

trait ParPixels {
    fn par_iter_mut_pixels(&mut self, f: impl Fn(&mut f32) + Sync + Send);
}

impl ParPixels for Rgb32FImage {
    fn par_iter_mut_pixels(&mut self, f: impl Fn(&mut f32) + Sync + Send) {
        use rayon::prelude::*;
        let raw: &mut [f32] = self;
        raw.par_iter_mut().for_each(f);
    }
}

/// Working RGB → camera units (sensor white 1) of a frame; identity for
/// non-RAW sources.
pub fn working_to_camera(color: Option<&ImageColor>) -> Matrix3<f32> {
    match color {
        Some(color) => {
            let m = Matrix3::from_fn(|r, c| color.camera_to_srgb[r][c]);
            let inv = m.try_inverse().unwrap_or_else(Matrix3::identity);
            Matrix3::from_diagonal(&nalgebra::Vector3::from_fn(|i, _| {
                1.0 / color.wb[i].max(1e-6)
            })) * inv
        }
        None => Matrix3::identity(),
    }
}

/// Working RGB of `from`'s balance → working RGB at `to`'s balance.
pub fn rebalance(from: Option<&ImageColor>, to: Option<&ImageColor>) -> Option<Matrix3<f32>> {
    let (from, to) = (from?, to?);
    if from.wb[..3] == to.wb[..3] && from.camera_to_srgb == to.camera_to_srgb {
        return None;
    }
    let to_camera = working_to_camera(Some(from));
    let m_to = Matrix3::from_fn(|r, c| to.camera_to_srgb[r][c]);
    let balance = Matrix3::from_diagonal(&nalgebra::Vector3::from_fn(|i, _| to.wb[i]));
    Some(m_to * balance * to_camera)
}

/// A frame developed for a merge that changes its geometry: lens-corrected
/// with its own profile (distortion, vignetting, lateral CA) and brought to
/// `reference`'s white balance.
pub fn load_corrected(
    path: &Path,
    options: &BaseImageOptions,
    reference: Option<&ImageColor>,
) -> Result<Frame> {
    use rayon::prelude::*;
    let mut frame = load_frame(path, options)?;
    let adjustments = crate::initial_adjustments(path, false);
    let corrected = rawtonic_engine::image_processing::apply_geometry_warp(
        image::DynamicImage::ImageRgb32F(std::mem::take(&mut frame.image)),
        &adjustments,
    );
    frame.image = corrected.into_owned().into_rgb32f();
    if let Some(m) = rebalance(frame.color.as_ref(), reference) {
        let raw: &mut [f32] = &mut frame.image;
        raw.par_chunks_mut(3).for_each(|p| {
            let v = m * nalgebra::Vector3::new(p[0], p[1], p[2]);
            p.copy_from_slice(v.as_slice());
        });
    }
    Ok(frame)
}
