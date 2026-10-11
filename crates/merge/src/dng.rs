//! The merged photo as a linear DNG: demosaiced camera RGB in 16-bit
//! floating point (tiles, Deflate with the floating-point predictor, as
//! DNG 1.4 specifies for HDR data), with the source's colour matrices,
//! As Shot balance and EXIF, so it develops like the source RAW: white
//! balance in Kelvin, the camera's look, highlight recovery from real data.
use anyhow::{Context, Result, anyhow};
use flate2::Compression;
use flate2::write::ZlibEncoder;
use half::f16;
use image::Rgb32FImage;
use rawler::decoders::RawMetadata;
use rawler::dng::writer::DngWriter;
use rawler::formats::tiff::{
    CompressionMethod, PhotometricInterpretation, Rational, SRational, Value,
};
use rawler::imgop::xyz::Illuminant;
use rawler::tags::{DngTag, TiffCommonTag};
use rawtonic_rawdecode::raw_processing::ImageColor;
use rayon::prelude::*;
use std::io::{Cursor, Write};

const TILE: usize = 256;
/// TIFF predictor 3: floating-point byte planes with horizontal differencing.
const PREDICTOR_FLOATING_POINT: u16 = 3;

/// XYZ (D65) → linear sRGB, for sources without camera colour (JPEG, TIFF):
/// their DNG holds linear sRGB as its "camera" space.
const XYZ_TO_SRGB: [[f32; 3]; 3] = [
    [3.2404542, -1.5371385, -0.4985314],
    [-0.969266, 1.8760108, 0.041556],
    [0.0556434, -0.2040259, 1.0572252],
];

/// What the merged DNG says about itself besides its pixels.
pub struct DngMeta<'a> {
    /// Colour of the reference frame (RAW sources).
    pub color: Option<&'a ImageColor>,
    /// EXIF of the reference frame (RAW sources).
    pub metadata: Option<&'a RawMetadata>,
    /// Keep the lens description so lens profiles still apply (HDR, focus
    /// stack: the merge keeps the reference frame's geometry). Panoramas are
    /// corrected before stitching and drop it.
    pub keep_lens: bool,
    /// DNG BaselineExposure (EV).
    pub baseline_exposure: Option<f64>,
    /// An 8-bit sRGB rendering for the embedded preview.
    pub preview: Option<&'a image::RgbImage>,
    pub software: &'a str,
}

/// The matrix from linear working RGB to camera RGB (not white-balanced)
/// for the DNG, and the AsShotNeutral that balances it again.
fn to_camera(color: Option<&ImageColor>) -> Result<([[f32; 3]; 3], [f64; 3])> {
    let (matrix, neutral) = match color {
        Some(color) => {
            let m = nalgebra::Matrix3::from_fn(|r, c| color.camera_to_srgb[r][c] as f64);
            let inv = m
                .try_inverse()
                .ok_or_else(|| anyhow!("Camera colour matrix is singular"))?;
            let wb: [f64; 3] = std::array::from_fn(|i| color.wb[i] as f64);
            if wb.iter().any(|w| !w.is_finite() || *w <= 0.0) {
                return Err(anyhow!("Invalid white balance {:?}", color.wb));
            }
            // camera = diag(1/wb) · inv(camera_to_srgb) · working
            let unbalance = nalgebra::Matrix3::from_diagonal(&nalgebra::Vector3::new(
                1.0 / wb[0],
                1.0 / wb[1],
                1.0 / wb[2],
            ));
            (unbalance * inv, std::array::from_fn(|i| 1.0 / wb[i]))
        }
        None => (nalgebra::Matrix3::identity(), [1.0; 3]),
    };
    let m: [[f32; 3]; 3] = std::array::from_fn(|r| std::array::from_fn(|c| matrix[(r, c)] as f32));
    // AsShotNeutral is the camera value of white, largest channel 1.
    let max = neutral.iter().cloned().fold(f64::MIN, f64::max);
    Ok((m, neutral.map(|n| n / max)))
}

/// One tile row: f16 big-endian byte planes, then byte differences with a
/// stride of one pixel (TIFF predictor 3).
fn encode_tile(
    working: &[f32],
    m: &[[f32; 3]; 3],
    width: usize,
    height: usize,
    tx: usize,
    ty: usize,
) -> Result<Vec<u8>> {
    let row_samples = TILE * 3;
    let mut raw = Vec::with_capacity(row_samples * 2 * TILE);
    let mut row = vec![0u8; row_samples * 2];
    for y in 0..TILE {
        let sy = (ty * TILE + y).min(height - 1);
        for x in 0..TILE {
            let sx = (tx * TILE + x).min(width - 1);
            let p = &working[(sy * width + sx) * 3..][..3];
            let camera: [f32; 3] =
                std::array::from_fn(|r| m[r][0] * p[0] + m[r][1] * p[1] + m[r][2] * p[2]);
            for (c, &value) in camera.iter().enumerate() {
                let bits = f16::from_f32(value).to_bits().to_be_bytes();
                let i = x * 3 + c;
                row[i] = bits[0];
                row[i + row_samples] = bits[1];
            }
        }
        for i in (3..row.len()).rev() {
            row[i] = row[i].wrapping_sub(row[i - 3]);
        }
        raw.extend_from_slice(&row);
    }
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::new(6));
    encoder.write_all(&raw)?;
    Ok(encoder.finish()?)
}

fn rational(v: f64) -> Rational {
    let d = 1_000_000u32;
    Rational::new((v * d as f64).round().max(0.0) as u32, d)
}

fn srational(v: f64) -> SRational {
    let d = 10_000i32;
    SRational::new((v * d as f64).round() as i32, d)
}

fn matrix_values(m: &[f32]) -> Vec<SRational> {
    m.iter().map(|&v| srational(v as f64)).collect()
}

/// The calibration matrices to write, warmest illuminant first, at most two
/// (the decode reads its D65 matrix; Kelvin conversions read both).
fn calibrations(color: &ImageColor) -> Vec<(Illuminant, Vec<f32>)> {
    let mut list: Vec<(Illuminant, Vec<f32>)> = color
        .calibrations
        .iter()
        .filter(|(_, m)| m.len() == 9)
        .map(|(i, m)| (*i, m.clone()))
        .collect();
    let temperature = |i: &Illuminant| match i {
        Illuminant::A | Illuminant::Tungsten => 2856,
        Illuminant::D50 => 5003,
        Illuminant::D55 | Illuminant::Daylight | Illuminant::FineWeather => 5503,
        Illuminant::D65 | Illuminant::CloudyWeather => 6504,
        Illuminant::D75 | Illuminant::Shade => 7504,
        _ => 4000,
    };
    list.sort_by_key(|(i, _)| temperature(i));
    if list.len() > 2 {
        // Keep the warmest and D65 (or the coolest).
        let last = list
            .iter()
            .position(|(i, _)| *i == Illuminant::D65)
            .unwrap_or(list.len() - 1);
        list = vec![list[0].clone(), list[last].clone()];
    }
    list
}

/// Encodes `image` (linear working RGB, see `Frame::image`) as a DNG.
pub fn encode_linear_dng(image: &Rgb32FImage, meta: &DngMeta) -> Result<Vec<u8>> {
    let (width, height) = (image.width() as usize, image.height() as usize);
    if width == 0 || height == 0 {
        return Err(anyhow!("Empty image"));
    }
    let (matrix, neutral) = to_camera(meta.color)?;
    let working: &[f32] = image;
    let tiles_x = width.div_ceil(TILE);
    let tiles_y = height.div_ceil(TILE);
    let tiles: Vec<Vec<u8>> = (0..tiles_x * tiles_y)
        .into_par_iter()
        .map(|i| encode_tile(working, &matrix, width, height, i % tiles_x, i / tiles_x))
        .collect::<Result<_>>()?;

    let mut buf = Cursor::new(Vec::new());
    let mut dng = DngWriter::new(&mut buf, rawler::dng::DNG_VERSION_V1_4).context("DNG header")?;

    let mut offsets = Vec::with_capacity(tiles.len());
    let mut counts = Vec::with_capacity(tiles.len());
    for tile in &tiles {
        offsets.push(dng.dng.write_data(tile)?);
        counts.push(tile.len() as u32);
    }
    drop(tiles);

    // Preview in a sub-IFD.
    if let Some(preview) = meta.preview {
        let mut sub = dng.subframe(1);
        sub.preview(&image::DynamicImage::ImageRgb8(preview.clone()), 0.9)?;
        sub.finalize()?;
    }

    if let Some(metadata) = meta.metadata {
        let mut metadata = metadata.clone();
        if !meta.keep_lens {
            metadata.exif.lens_make = None;
            metadata.exif.lens_model = None;
            metadata.exif.lens_spec = None;
            metadata.exif.lens_serial_number = None;
            metadata.lens = None;
        }
        metadata.exif.orientation = Some(1);
        dng.load_metadata(&metadata)?;
        let make = metadata.make.clone();
        let model = metadata.model.clone();
        let root = dng.root_ifd_mut();
        root.add_tag(TiffCommonTag::Make, make.as_str());
        root.add_tag(TiffCommonTag::Model, model.as_str());
        root.add_tag(
            DngTag::UniqueCameraModel,
            format!("{make} {model}").as_str(),
        );
    } else {
        dng.root_ifd_mut()
            .add_tag(DngTag::UniqueCameraModel, "Rawtonic linear sRGB");
    }

    match meta.color {
        Some(color) => {
            for (slot, (illuminant, m)) in calibrations(color).into_iter().enumerate() {
                dng.color_matrix(slot + 1, illuminant, matrix_values(&m));
            }
        }
        None => {
            let flat: Vec<f32> = XYZ_TO_SRGB.iter().flatten().copied().collect();
            dng.color_matrix(1, Illuminant::D65, matrix_values(&flat));
        }
    }
    dng.as_shot_neutral(neutral.map(rational));

    let root = dng.root_ifd_mut();
    root.add_tag(TiffCommonTag::NewSubFileType, 0u32);
    root.add_tag(TiffCommonTag::ImageWidth, width as u32);
    root.add_tag(TiffCommonTag::ImageLength, height as u32);
    root.add_tag(TiffCommonTag::BitsPerSample, [16u16, 16, 16]);
    root.add_tag(TiffCommonTag::SampleFormat, [3u16, 3, 3]);
    root.add_tag(TiffCommonTag::SamplesPerPixel, 3u16);
    root.add_tag(
        TiffCommonTag::PhotometricInt,
        PhotometricInterpretation::LinearRaw,
    );
    root.add_tag(TiffCommonTag::Compression, CompressionMethod::Deflate);
    root.add_tag(TiffCommonTag::Predictor, PREDICTOR_FLOATING_POINT);
    root.add_tag(rawler::tags::ExifTag::PlanarConfiguration, 1u16);
    root.add_tag(TiffCommonTag::Orientation, 1u16);
    root.add_tag(TiffCommonTag::TileWidth, TILE as u32);
    root.add_tag(TiffCommonTag::TileLength, TILE as u32);
    root.add_value(TiffCommonTag::TileOffsets, Value::Long(offsets));
    root.add_tag(TiffCommonTag::TileByteCounts, Value::Long(counts));
    root.add_tag(DngTag::BlackLevel, [0u32, 0, 0]);
    root.add_tag(DngTag::WhiteLevel, [1u32, 1, 1]);
    root.add_tag(TiffCommonTag::Software, meta.software);
    if let Some(ev) = meta.baseline_exposure {
        root.add_tag(DngTag::BaselineExposure, srational(ev));
    }
    dng.close()?;
    Ok(buf.into_inner())
}
