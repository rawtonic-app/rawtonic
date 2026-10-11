use std::io;

use image::{DynamicImage, ImageBuffer};

use crate::{
  RawImage,
  decoders::RawMetadata,
  formats::tiff::{DirectoryWriter, TiffWriter},
  pixarray::{Color2D, PixF32},
  rawimage::RawPhotometricInterpretation,
  tags::{ExifTag, TiffCommonTag},
};

use super::{
  Dim2, Rect, convert_from_f32_scaled_u16,
  raw::{map_3ch_to_rgb, map_4ch_to_rgb},
  sensor::bayer::{
    Demosaic,
    bilinear::Bilinear4Channel,
    ppg::PPGDemosaic,
    superpixel::{Superpixel4Channel, SuperpixelQuarterRes3Channel},
  },
  sensor::xtrans::demosaic::{XTransDemosaic, XTransSuperpixelDemosaic},
  xyz::Illuminant,
};

#[derive(PartialEq, Eq, Debug, Clone, Copy)]
pub enum ProcessingStep {
  Rescale,
  Demosaic,
  CropActiveArea,
  WhiteBalance,
  Calibrate,
  CropDefault,
  SRgb,
}

/// The demosaicing algorithm to use.
#[derive(PartialEq, Eq, Debug, Clone, Copy, Default)]
pub enum DemosaicAlgorithm {
  /// High-quality demosaicing (PPG for Bayer, Full-Res for X-Trans).
  #[default]
  Quality,
  /// High-speed demosaicing using a superpixel algorithm (e.g. for thumbnails).
  /// This reduces image dimensions by a factor of four (quarter width and height).
  Speed,
}

pub struct RawDevelopBuilder {}

#[derive(Clone)]
pub enum Intermediate {
  Monochrome(PixF32),
  ThreeColor(Color2D<f32, 3>),
  FourColor(Color2D<f32, 4>),
}

impl Intermediate {
  pub fn dim(&self) -> Dim2 {
    match self {
      Intermediate::Monochrome(pixels) => pixels.dim(),
      Intermediate::ThreeColor(pixels) => pixels.dim(),
      Intermediate::FourColor(pixels) => pixels.dim(),
    }
  }

  pub fn rect(&self) -> Rect {
    match self {
      Intermediate::Monochrome(pixels) => pixels.rect(),
      Intermediate::ThreeColor(pixels) => pixels.rect(),
      Intermediate::FourColor(pixels) => pixels.rect(),
    }
  }

  pub fn to_dynamic_image(self) -> Option<DynamicImage> {
    Some(match self {
      Intermediate::Monochrome(pixels) => {
        let data = convert_from_f32_scaled_u16(&pixels.data, 0, u16::MAX);
        DynamicImage::ImageLuma16(ImageBuffer::from_raw(pixels.dim().w as u32, pixels.dim().h as u32, data)?)
      }
      Intermediate::ThreeColor(pixels) => {
        let data = convert_from_f32_scaled_u16(&pixels.flatten(), 0, u16::MAX);
        DynamicImage::ImageRgb16(ImageBuffer::from_raw(pixels.dim().w as u32, pixels.dim().h as u32, data)?)
      }
      Intermediate::FourColor(pixels) => {
        let data = convert_from_f32_scaled_u16(&pixels.flatten(), 0, u16::MAX);
        DynamicImage::ImageRgba16(ImageBuffer::from_raw(pixels.dim().w as u32, pixels.dim().h as u32, data)?)
      }
    })
  }
}

#[derive(Clone)]
pub struct RawDevelop {
  pub steps: Vec<ProcessingStep>,
  pub demosaic_algorithm: DemosaicAlgorithm,
  /// Keep negative channels after Calibrate (colours outside sRGB) instead
  /// of clipping them; the caller limits the gamut.
  pub keep_negative: bool,
}

impl Default for RawDevelop {
  fn default() -> Self {
    Self {
      steps: vec![
        ProcessingStep::Rescale,
        ProcessingStep::Demosaic,
        ProcessingStep::CropActiveArea,
        ProcessingStep::WhiteBalance,
        ProcessingStep::Calibrate,
        ProcessingStep::CropDefault,
        ProcessingStep::SRgb,
      ],
      demosaic_algorithm: DemosaicAlgorithm::default(),
      keep_negative: false,
    }
  }
}

impl RawDevelop {
  /*
  pub fn linearize(rawimage: &RawImage) -> crate::Result<RgbF32> {
    todo!()
  }

  pub fn develop_monochrome_image(&self, rawimage: &RawImage) -> crate::Result<PixF32> {
    todo!()
  }

  pub fn develop_rgb_image(&self, rawimage: &RawImage) -> crate::Result<RgbF32> {
    todo!()
  }
   */

  /// RAWTONIC: `develop_intermediate` for linear three-colour images
  /// (LinearRaw DNGs: merges, phones) without its copies: the samples are
  /// taken out of `rawimage` (left empty) and rescaled and calibrated in
  /// place, with the same arithmetic. A 50 MP float DNG developed with four
  /// full-size buffers alive (the image, its clone, the pixel array and the
  /// calibrated array); now one. Other images and steps take the copying
  /// path.
  pub fn develop_linear_in_place(&self, rawimage: &mut RawImage) -> crate::Result<Intermediate> {
    let supported = rawimage.cpp == 3
      && matches!(rawimage.photometric, RawPhotometricInterpretation::LinearRaw)
      && self
        .steps
        .iter()
        .all(|s| matches!(s, ProcessingStep::Rescale | ProcessingStep::WhiteBalance | ProcessingStep::Calibrate));
    if !supported {
      return self.develop_intermediate(rawimage);
    }
    if self.steps.contains(&ProcessingStep::Rescale) {
      rawimage.apply_scaling()?;
    }
    let samples = match std::mem::replace(&mut rawimage.data, crate::RawImageData::Float(Vec::new())) {
      crate::RawImageData::Float(v) => v,
      other @ crate::RawImageData::Integer(_) => other.as_f32().into_owned(),
    };
    let mut pixels = into_rgb_pixels(samples);
    if self.steps.contains(&ProcessingStep::Calibrate) {
      let found_matrix = rawimage
        .color_matrix
        .iter()
        .find(|(illuminant, _m)| **illuminant == Illuminant::D65)
        .or_else(|| rawimage.color_matrix.iter().next());
      if let Some((_, color_matrix)) = found_matrix.filter(|(_, m)| m.len() % 3 == 0) {
        let mut xyz2cam: [[f32; 3]; 4] = [[0.0; 3]; 4];
        for i in 0..color_matrix.len() / 3 {
          for j in 0..3 {
            xyz2cam[i][j] = color_matrix[i * 3 + j];
          }
        }
        let mut wb = if rawimage.wb_coeffs[0].is_nan() {
          [1.0, 1.0, 1.0, 1.0]
        } else {
          rawimage.wb_coeffs
        };
        if !self.steps.contains(&ProcessingStep::WhiteBalance) {
          wb = [1.0, 1.0, 1.0, 1.0];
        }
        super::raw::map_3ch_to_rgb_in_place(&mut pixels, &wb, xyz2cam, self.keep_negative);
      }
    }
    Ok(Intermediate::ThreeColor(Color2D::<f32, 3>::new_with(pixels, rawimage.width, rawimage.height)))
  }

  /// Develop raw image and write result into TIFF.
  /// If demosaic is disabled or camera raw is monochrome, the TIFF
  /// has only one color channel.
  pub fn develop_intermediate(&self, rawimage: &RawImage) -> crate::Result<Intermediate> {
    let mut rawimage = rawimage.clone();
    if self.steps.contains(&ProcessingStep::Rescale) {
      rawimage.apply_scaling()?;
    }

    let mut intermediate = match rawimage.cpp {
      1 => Intermediate::Monochrome(PixF32::new_with(rawimage.data.as_f32().into_owned(), rawimage.width, rawimage.height)),
      3 => Intermediate::ThreeColor(Color2D::<f32, 3>::new_with(
        rawimage.data.as_f32().chunks_exact(3).map(|x| [x[0], x[1], x[2]]).collect(),
        rawimage.width,
        rawimage.height,
      )),
      4 => Intermediate::FourColor(Color2D::<f32, 4>::new_with(
        rawimage.data.as_f32().chunks_exact(4).map(|x| [x[0], x[1], x[2], x[3]]).collect(),
        rawimage.width,
        rawimage.height,
      )),
      _ => todo!(),
    };

    if self.steps.contains(&ProcessingStep::Demosaic) {
      intermediate = match &rawimage.photometric {
        RawPhotometricInterpretation::Cfa(config) => {
          log::info!(
            "Demosaicing check for '{} {}': CFA name='{}', width={}, height={}, is_rgb={}",
            rawimage.clean_make,
            rawimage.clean_model,
            config.cfa.name,
            config.cfa.width,
            config.cfa.height,
            config.cfa.is_rgb()
          );
          if let Intermediate::Monochrome(ref pixels) = intermediate {
            let roi = if self.steps.contains(&ProcessingStep::CropActiveArea) {
              rawimage.active_area.unwrap_or(pixels.rect())
            } else {
              pixels.rect()
            };
            if config.cfa.is_rgb() {
              // Check if this is an X-Trans sensor (6x6 pattern)
              if config.cfa.width == 6 && config.cfa.height == 6 {
                log::info!("X-Trans pattern (6x6) detected. Applying X-Trans demosaicing ({:?}).", self.demosaic_algorithm);
                match self.demosaic_algorithm {
                  DemosaicAlgorithm::Quality => {
                    let xtrans_demosaic = XTransDemosaic::new();
                    Intermediate::ThreeColor(xtrans_demosaic.demosaic(pixels, &config.cfa, &config.colors, roi))
                  }
                  DemosaicAlgorithm::Speed => {
                    let xtrans_demosaic = XTransSuperpixelDemosaic::new();
                    Intermediate::ThreeColor(xtrans_demosaic.demosaic(pixels, &config.cfa, &config.colors, roi))
                  }
                }
              } else {
                log::info!("RGB Bayer-like pattern detected. Applying Bayer demosaicing.");
                match self.demosaic_algorithm {
                  DemosaicAlgorithm::Quality => {
                    let ppg = PPGDemosaic::new();
                    Intermediate::ThreeColor(ppg.demosaic(pixels, &config.cfa, &config.colors, roi))
                  }
                  DemosaicAlgorithm::Speed => {
                    let superpixel = SuperpixelQuarterRes3Channel::new();
                    Intermediate::ThreeColor(superpixel.demosaic(pixels, &config.cfa, &config.colors, roi))
                  }
                }
              }
            } else if config.cfa.unique_colors() == 4 {
              log::info!("4-Color pattern detected. Applying 4-channel demosaicing.");
              match self.demosaic_algorithm {
                DemosaicAlgorithm::Quality => {
                  let linear = Bilinear4Channel::new();
                  Intermediate::FourColor(linear.demosaic(&pixels, &config.cfa, &config.colors, roi))
                }
                DemosaicAlgorithm::Speed => {
                  let superpixel = Superpixel4Channel::new();
                  Intermediate::FourColor(superpixel.demosaic(&pixels, &config.cfa, &config.colors, roi))
                }
              }
            } else {
              log::warn!(
                "Unsupported CFA pattern '{}' for demosaicing. Passing through without demosaicing.",
                config.cfa.name
              );
              Intermediate::Monochrome(pixels.clone())
            }
          } else {
            intermediate
          }
        }
        _ => intermediate,
      };
    }

    if self.steps.contains(&ProcessingStep::Calibrate) {
      let found_matrix = rawimage
        .color_matrix
        .iter()
        .find(|(illuminant, _m)| **illuminant == Illuminant::D65)
        .or_else(|| rawimage.color_matrix.iter().next());

      if let Some((_illuminant, color_matrix)) = found_matrix {
        // Safety check: ensure matrix is valid length
        if color_matrix.len() % 3 == 0 {
          let mut xyz2cam: [[f32; 3]; 4] = [[0.0; 3]; 4];
          let components = color_matrix.len() / 3;
          for i in 0..components {
            for j in 0..3 {
              xyz2cam[i][j] = color_matrix[i * 3 + j];
            }
          }

          let mut wb = if rawimage.wb_coeffs[0].is_nan() {
            [1.0, 1.0, 1.0, 1.0]
          } else {
            rawimage.wb_coeffs
          };
          if !self.steps.contains(&ProcessingStep::WhiteBalance) {
            wb = [1.0, 1.0, 1.0, 1.0];
          }

          log::info!("Applying calibration with wb: {:?}, xyz2cam: {:?}", wb, xyz2cam);

          intermediate = match intermediate {
            Intermediate::Monochrome(_) => intermediate,
            Intermediate::ThreeColor(pixels) => Intermediate::ThreeColor(map_3ch_to_rgb(&pixels, &wb, xyz2cam, self.keep_negative)),
            Intermediate::FourColor(pixels) => Intermediate::ThreeColor(map_4ch_to_rgb(&pixels, &wb, xyz2cam, self.keep_negative)),
          };
        } else {
          log::warn!("Color matrix found but has invalid length. Skipping calibration.");
        }
      } else {
        log::warn!("Illuminant matrix D65 not found and no fallback available. Skipping calibration.");
      }
    }

    if self.steps.contains(&ProcessingStep::CropDefault) {
      if let Some(mut crop) = rawimage.crop_area {
        if self.steps.contains(&ProcessingStep::Demosaic) && self.steps.contains(&ProcessingStep::CropActiveArea) {
          if let Some(active_area) = rawimage.active_area {
            let intersection = crop.intersection(&active_area);
            crop = intersection.adapt(&active_area);
          }
        }
        let original_width = rawimage.active_area.map(|area| area.d.w).unwrap_or(rawimage.dim().w);
        if original_width > 0 {
          let scale_factor = intermediate.dim().w as f32 / original_width as f32;
          if (scale_factor - 1.0).abs() > 1e-6 {
            crop.scale(scale_factor);
          }
        }
        if !crop.is_empty() && crop.d != intermediate.dim() {
          log::info!("crop: {:?}, intermediate dim: {:?}, rawimage: {:?}", crop, intermediate.dim(), rawimage.dim());
          intermediate = match intermediate {
            Intermediate::Monochrome(pixels) => Intermediate::Monochrome(pixels.crop(crop)),
            Intermediate::ThreeColor(pixels) => Intermediate::ThreeColor(pixels.crop(crop)),
            Intermediate::FourColor(pixels) => Intermediate::FourColor(pixels.crop(crop)),
          };
        }
      }
    }

    if self.steps.contains(&ProcessingStep::SRgb) {
      match &mut intermediate {
        Intermediate::Monochrome(pixels) => pixels.for_each(super::srgb::srgb_apply_gamma),
        Intermediate::ThreeColor(pixels) => pixels.for_each(super::srgb::srgb_apply_gamma_n),
        Intermediate::FourColor(pixels) => pixels.for_each(super::srgb::srgb_apply_gamma_n),
      };
    }

    Ok(intermediate)
  }

  /// Develop raw image and write result into TIFF.
  /// If demosaic is disabled or camera raw is monochrome, the TIFF
  /// has only one color channel.
  pub fn develop<W>(&self, rawimage: &RawImage, md: &RawMetadata, writer: W) -> crate::Result<()>
  where
    W: io::Write + io::Seek,
  {
    let intermediate = self.develop_intermediate(rawimage)?;

    let mut tiff = TiffWriter::new(writer)?;
    let mut root_ifd = DirectoryWriter::new();
    let mut exif_ifd = DirectoryWriter::new();

    // Add EXIF version 0220
    exif_ifd.add_tag_undefined(ExifTag::ExifVersion, vec![48, 50, 50, 48]);

    md.write_exif_tags(&mut tiff, &mut root_ifd, &mut exif_ifd)?;
    root_ifd.add_tag(TiffCommonTag::Make, rawimage.clean_make.as_str());
    root_ifd.add_tag(TiffCommonTag::Model, rawimage.clean_model.as_str());

    let exif_offset = exif_ifd.build(&mut tiff)?;

    root_ifd.add_tag(TiffCommonTag::ExifIFDPointer, exif_offset);

    match intermediate {
      Intermediate::Monochrome(pixels) => {
        let data = convert_from_f32_scaled_u16(&pixels.data, 0, u16::MAX);
        let (strip_rows, strips) = tiff.write_strips_lzw(&data, 1, pixels.dim(), 0)?;
        let strip_offsets: Vec<u32> = strips.iter().map(|(offset, _)| *offset).collect();
        let strip_bytes: Vec<u32> = strips.iter().map(|(_, bytes)| *bytes).collect();
        root_ifd.add_tag(TiffCommonTag::Compression, 5);
        root_ifd.add_tag(TiffCommonTag::Predictor, 1);
        root_ifd.add_tag(TiffCommonTag::StripOffsets, &strip_offsets);
        root_ifd.add_tag(TiffCommonTag::StripByteCounts, &strip_bytes);
        root_ifd.add_tag(TiffCommonTag::BitsPerSample, [16_u16]);
        root_ifd.add_tag(TiffCommonTag::SamplesPerPixel, [1_u16]);
        root_ifd.add_tag(TiffCommonTag::PhotometricInt, [1_u16]);
        root_ifd.add_tag(TiffCommonTag::RowsPerStrip, strip_rows);
        root_ifd.add_tag(TiffCommonTag::ImageWidth, pixels.width as u16);
        root_ifd.add_tag(TiffCommonTag::ImageLength, pixels.height as u16);
      }
      Intermediate::ThreeColor(pixels) => {
        let data = convert_from_f32_scaled_u16(&pixels.flatten(), 0, u16::MAX);
        let (strip_rows, strips) = tiff.write_strips_lzw(&data, 3, pixels.dim(), 0)?;
        let strip_offsets: Vec<u32> = strips.iter().map(|(offset, _)| *offset).collect();
        let strip_bytes: Vec<u32> = strips.iter().map(|(_, bytes)| *bytes).collect();
        root_ifd.add_tag(TiffCommonTag::Compression, 5);
        root_ifd.add_tag(TiffCommonTag::Predictor, 1);
        root_ifd.add_tag(TiffCommonTag::StripOffsets, &strip_offsets);
        root_ifd.add_tag(TiffCommonTag::StripByteCounts, &strip_bytes);
        root_ifd.add_tag(TiffCommonTag::BitsPerSample, [16_u16, 16, 16]);
        root_ifd.add_tag(TiffCommonTag::SamplesPerPixel, [3_u16]);
        root_ifd.add_tag(TiffCommonTag::PhotometricInt, [2_u16]);
        root_ifd.add_tag(TiffCommonTag::RowsPerStrip, strip_rows);
        root_ifd.add_tag(TiffCommonTag::ImageWidth, pixels.width as u16);
        root_ifd.add_tag(TiffCommonTag::ImageLength, pixels.height as u16);
      }
      Intermediate::FourColor(pixels) => {
        let data = convert_from_f32_scaled_u16(&pixels.flatten(), 0, u16::MAX);
        let (strip_rows, strips) = tiff.write_strips_lzw(&data, 4, pixels.dim(), 0)?;
        let strip_offsets: Vec<u32> = strips.iter().map(|(offset, _)| *offset).collect();
        let strip_bytes: Vec<u32> = strips.iter().map(|(_, bytes)| *bytes).collect();
        root_ifd.add_tag(TiffCommonTag::Compression, 5);
        root_ifd.add_tag(TiffCommonTag::Predictor, 1);
        root_ifd.add_tag(TiffCommonTag::StripOffsets, &strip_offsets);
        root_ifd.add_tag(TiffCommonTag::StripByteCounts, &strip_bytes);
        root_ifd.add_tag(TiffCommonTag::BitsPerSample, [16_u16, 16, 16, 16]); // Extra-channel, even if PhotometricInt is RGB!
        root_ifd.add_tag(TiffCommonTag::SamplesPerPixel, [4_u16]);
        root_ifd.add_tag(TiffCommonTag::PhotometricInt, [2_u16]);
        root_ifd.add_tag(TiffCommonTag::RowsPerStrip, strip_rows);
        root_ifd.add_tag(TiffCommonTag::ImageWidth, pixels.width as u16);
        root_ifd.add_tag(TiffCommonTag::ImageLength, pixels.height as u16);
      }
    }

    tiff.build(root_ifd)?;

    Ok(())
  }
}

/// Interleaved samples as pixels, reusing the allocation when it holds
/// whole pixels.
fn into_rgb_pixels(samples: Vec<f32>) -> Vec<[f32; 3]> {
  if samples.len() % 3 == 0 && samples.capacity() % 3 == 0 {
    let mut samples = std::mem::ManuallyDrop::new(samples);
    let (ptr, len, cap) = (samples.as_mut_ptr(), samples.len(), samples.capacity());
    // SAFETY: [f32; 3] has the alignment of f32 and three times its size;
    // length and capacity are whole pixels, and the allocation is handed over.
    unsafe { Vec::from_raw_parts(ptr as *mut [f32; 3], len / 3, cap / 3) }
  } else {
    samples.chunks_exact(3).map(|x| [x[0], x[1], x[2]]).collect()
  }
}

#[cfg(test)]
mod linear_in_place_tests {
  use super::*;
  use crate::decoders::Camera;
  use crate::pixarray::PixU16;
  use crate::rawimage::{BlackLevel, WhiteLevel};

  fn linear_image(float: bool) -> RawImage {
    let (w, h) = (37usize, 11usize);
    let samples: Vec<u16> = (0..w * h * 3).map(|i| ((i * 7919) % 15000 + 300) as u16).collect();
    let mut image = RawImage::new(
      Camera::new(),
      PixU16::new_with(samples.clone(), w * 3, h),
      3,
      [1.9, 1.0, 1.4, f32::NAN],
      RawPhotometricInterpretation::LinearRaw,
      Some(BlackLevel::new(&[256u32, 256, 256], 1, 1, 3)),
      Some(WhiteLevel::new(vec![16000u32, 16000, 16000])),
      false,
    );
    if float {
      image.data = crate::RawImageData::Float(samples.iter().map(|&v| v as f32 * 1.37).collect());
    }
    image
      .color_matrix
      .insert(Illuminant::D65, vec![0.71, -0.12, -0.08, -0.36, 1.11, 0.27, -0.05, 0.16, 0.62]);
    image
  }

  /// The in-place path gives exactly what the copying path gives.
  #[test]
  fn linear_in_place_matches_develop_intermediate() {
    for float in [false, true] {
      for keep_negative in [false, true] {
        let develop = RawDevelop {
          steps: vec![ProcessingStep::Rescale, ProcessingStep::WhiteBalance, ProcessingStep::Calibrate],
          keep_negative,
          ..RawDevelop::default()
        };
        let reference = develop.develop_intermediate(&linear_image(float)).unwrap();
        let mut image = linear_image(float);
        let in_place = develop.develop_linear_in_place(&mut image).unwrap();
        match (reference, in_place) {
          (Intermediate::ThreeColor(a), Intermediate::ThreeColor(b)) => {
            assert_eq!((a.width, a.height), (b.width, b.height));
            assert!(a.data.iter().zip(&b.data).all(|(x, y)| x.map(f32::to_bits) == y.map(f32::to_bits)));
          }
          _ => panic!("three-colour results expected"),
        }
      }
    }
  }
}
