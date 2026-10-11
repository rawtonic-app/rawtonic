use crate::RawImage;
use crate::cfa::PlaneColor;
use crate::cfa::*;
use crate::decoders::*;
use crate::formats::tiff::Entry;
use crate::formats::tiff::Rational;
use crate::formats::tiff::Value;
use crate::imgop::Dim2;
use crate::imgop::Point;
use crate::imgop::Rect;
use crate::imgop::matrix::*;
use crate::imgop::xyz::*;
use crate::tags::DngTag;
use crate::tags::TiffCommonTag;
use rayon::prelude::*;

#[derive(Debug, Clone)]
pub struct DngDecoder<'a> {
  rawloader: &'a RawLoader,
  tiff: GenericTiffReader,
}

impl<'a> DngDecoder<'a> {
  pub fn new(_file: &RawSource, tiff: GenericTiffReader, rawloader: &'a RawLoader) -> Result<DngDecoder<'a>> {
    Ok(DngDecoder { tiff, rawloader })
  }
}

/// DNG format encapsulation for analyzer
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DngFormat {
  tiff: GenericTiffReader,
}

impl<'a> Decoder for DngDecoder<'a> {
  fn raw_image(&self, file: &RawSource, _params: &RawDecodeParams, dummy: bool) -> Result<RawImage> {
    let raw = self.get_raw_ifd()?;
    let width = fetch_tiff_tag!(raw, TiffCommonTag::ImageWidth).force_usize(0);
    let height = fetch_tiff_tag!(raw, TiffCommonTag::ImageLength).force_usize(0);
    let cpp = fetch_tiff_tag!(raw, TiffCommonTag::SamplesPerPixel).force_usize(0);
    let bits = fetch_tiff_tag!(raw, TiffCommonTag::BitsPerSample).force_u32(0);
    let orientation = Orientation::from_tiff(self.tiff.root_ifd());

    let mut cam = self.make_camera(raw, width, height)?;
    // If we know the camera, re-use the clean names
    if let Ok(known_cam) = self
      .rawloader
      .check_supported_with_mode(self.tiff.root_ifd(), "dng")
      .or_else(|_| self.rawloader.check_supported(self.tiff.root_ifd()))
    {
      cam.clean_make = known_cam.clean_make;
      cam.clean_model = known_cam.clean_model;
      cam.hints.extend_from_slice(&known_cam.hints);
      cam.params.extend(known_cam.params.iter().map(|(k, v)| (k.clone(), v.clone())));
    } else {
      log::debug!("DNG: camera {} / {} is not in the camera catalog", cam.make, cam.model);
      // panic!("Camera {} / {} is not in the camera catalog", cam.make, cam.model);
    }

    let blacklevel = self.get_blacklevels(raw)?;
    let whitelevel = self.get_whitelevels(raw)?.or(Some(WhiteLevel::new_bits(bits, cpp)));

    let photometric = match fetch_tiff_tag!(raw, TiffCommonTag::PhotometricInt).force_u32(0) {
      1 => RawPhotometricInterpretation::BlackIsZero,
      32803 => RawPhotometricInterpretation::Cfa(CFAConfig::new_from_camera(&cam)),
      34892 => RawPhotometricInterpretation::LinearRaw,
      _ => todo!(),
    };

    // RAWTONIC: a dummy (metadata-only) decode skips the pixels, as the other
    // decoders do; it decompressed the whole image (a second full decode of
    // every DNG for its colour data).
    let raw_data = if dummy {
      match SampleFormat::extract(raw)?.unwrap_or(SampleFormat::Uint) {
        SampleFormat::IEEEFP => RawImageData::Float(Vec::new()),
        _ => RawImageData::Integer(Vec::new()),
      }
    } else {
      plain_image_from_ifd(raw, file)?
    };
    let mut raw_data = raw_data;
    if !dummy && cpp == 3 && matches!(photometric, RawPhotometricInterpretation::LinearRaw) {
      self.apply_transparency_mask(file, &mut raw_data, width, height, blacklevel.as_ref());
    }
    let wb_coeffs = self.get_wb(&cam)?;
    let mut image = RawImage::new_with_data(cam, raw_data, width * cpp, height, cpp, wb_coeffs, photometric, blacklevel, whitelevel, dummy);
    image.orientation = orientation;
    // Rawtonic: DNGs that state no black level but mark masked (covered)
    // sensor areas carry the black there; Camera Raw measures it (Adobe DNG
    // Converter files of the EOS-1Ds Mark II: BlackLevel 0, shadows 5 EV too
    // bright without it).
    if cpp == 1 && !dummy && image.blacklevel.levels.iter().all(|v| v.as_f32() == 0.0) {
      let areas = self.get_masked_areas(raw);
      if let (false, RawImageData::Integer(data)) = (areas.is_empty(), &image.data) {
        let (mut sum, mut count) = (0.0f64, 0usize);
        for area in &areas {
          for row in area.p.y..(area.p.y + area.d.h).min(height) {
            for col in area.p.x..(area.p.x + area.d.w).min(width) {
              sum += data[row * width + col] as f64;
              count += 1;
            }
          }
        }
        // Files whose data are already black-subtracted keep only a noise
        // floor of a few codes there (iPhone, Leica M10): left alone.
        let white = image.whitelevel.0.first().copied().unwrap_or(u16::MAX as u32) as f64;
        if count > 0 && sum / count as f64 >= 0.005 * white {
          image.blacklevel = BlackLevel::new(&[(sum / count as f64) as f32], 1, 1, 1);
        }
      }
    }
    Ok(image)
  }

  fn format_dump(&self) -> FormatDump {
    FormatDump::Dng(DngFormat { tiff: self.tiff.clone() })
  }

  fn raw_metadata(&self, _file: &RawSource, _params: &RawDecodeParams) -> Result<RawMetadata> {
    let raw = self.get_raw_ifd()?;
    let width = fetch_tiff_tag!(raw, TiffCommonTag::ImageWidth).force_usize(0);
    let height = fetch_tiff_tag!(raw, TiffCommonTag::ImageLength).force_usize(0);
    let mut cam = self.make_camera(raw, width, height)?;
    // If we know the camera, re-use the clean names
    if let Ok(known_cam) = self.rawloader.check_supported(self.tiff.root_ifd()) {
      cam.clean_make = known_cam.clean_make;
      cam.clean_model = known_cam.clean_model;
    }
    let exif = Exif::new(self.tiff.root_ifd())?;
    let mdata = RawMetadata::new(&cam, exif);
    Ok(mdata)
  }

  fn thumbnail_image(&self, file: &RawSource, _params: &RawDecodeParams) -> Result<Option<DynamicImage>> {
    if let Some(thumb_ifd) = Some(self.tiff.root_ifd()).filter(|ifd| ifd.get_entry(TiffCommonTag::NewSubFileType).map(|entry| entry.force_u16(0)) == Some(1)) {
      Ok(Some(dynamic_image_from_ifd(thumb_ifd, file)?))
    } else {
      Ok(None)
    }
  }

  fn full_image(&self, file: &RawSource, params: &RawDecodeParams) -> Result<Option<DynamicImage>> {
    if params.image_index != 0 {
      return Ok(None);
    }
    if let Some(sub_ifds) = self.tiff.root_ifd().get_sub_ifd_all(TiffCommonTag::SubIFDs) {
      let first_ifd = sub_ifds
        .iter()
        .find(|ifd| ifd.get_entry(TiffCommonTag::NewSubFileType).map(|entry| entry.force_u32(0)) == Some(1));
      if let Some(preview_ifd) = first_ifd {
        return Ok(Some(dynamic_image_from_ifd(preview_ifd, file)?));
      }
    }
    Ok(None)
  }

  fn ifd(&self, wk_ifd: WellKnownIFD) -> Result<Option<Rc<IFD>>> {
    Ok(match wk_ifd {
      WellKnownIFD::Root => Some(Rc::new(self.tiff.root_ifd().clone())),
      WellKnownIFD::Raw => Some(Rc::new(self.get_raw_ifd()?.clone())),
      WellKnownIFD::Exif => self
        .tiff
        .root_ifd()
        .get_sub_ifd_all(ExifTag::ExifOffset)
        .and_then(|list| list.get(0))
        .cloned()
        .map(Rc::new),
      WellKnownIFD::ExifGps => self
        .tiff
        .root_ifd()
        .get_sub_ifd_all(ExifTag::GPSInfo)
        .and_then(|list| list.get(0))
        .cloned()
        .map(Rc::new),
      WellKnownIFD::VirtualDngRawTags => {
        let mut ifd = IFD::default();
        IFD::copy_tag(&mut ifd, self.get_raw_ifd()?, DngTag::OpcodeList1);
        IFD::copy_tag(&mut ifd, self.get_raw_ifd()?, DngTag::OpcodeList2);
        IFD::copy_tag(&mut ifd, self.get_raw_ifd()?, DngTag::OpcodeList3);
        IFD::copy_tag(&mut ifd, self.get_raw_ifd()?, DngTag::NoiseProfile);
        IFD::copy_tag(&mut ifd, self.get_raw_ifd()?, DngTag::BayerGreenSplit);
        IFD::copy_tag(&mut ifd, self.get_raw_ifd()?, DngTag::ChromaBlurRadius);
        IFD::copy_tag(&mut ifd, self.get_raw_ifd()?, DngTag::AntiAliasStrength);
        IFD::copy_tag(&mut ifd, self.get_raw_ifd()?, DngTag::NoiseReductionApplied);
        IFD::copy_tag(&mut ifd, self.get_raw_ifd()?, DngTag::ProfileGainTableMap);
        IFD::copy_tag(&mut ifd, self.get_raw_ifd()?, DngTag::CameraCalibration1);
        IFD::copy_tag(&mut ifd, self.get_raw_ifd()?, DngTag::CameraCalibration2);
        IFD::copy_tag(&mut ifd, self.get_raw_ifd()?, DngTag::CameraCalibration3);
        IFD::copy_tag(&mut ifd, self.get_raw_ifd()?, DngTag::ForwardMatrix1);
        IFD::copy_tag(&mut ifd, self.get_raw_ifd()?, DngTag::ForwardMatrix2);
        IFD::copy_tag(&mut ifd, self.get_raw_ifd()?, DngTag::ForwardMatrix3);
        IFD::copy_tag(&mut ifd, self.get_raw_ifd()?, DngTag::DefaultUserCrop);
        IFD::copy_tag(&mut ifd, self.get_raw_ifd()?, DngTag::DefaultCropSize);
        Some(Rc::new(ifd))
      }
      WellKnownIFD::VirtualDngRootTags => {
        let mut ifd = IFD::default();
        IFD::copy_tag(&mut ifd, self.tiff.root_ifd(), DngTag::ProfileEmbedPolicy);
        IFD::copy_tag(&mut ifd, self.tiff.root_ifd(), DngTag::ProfileHueSatMapData1);
        IFD::copy_tag(&mut ifd, self.tiff.root_ifd(), DngTag::ProfileHueSatMapData2);
        IFD::copy_tag(&mut ifd, self.tiff.root_ifd(), DngTag::ProfileHueSatMapData3);
        IFD::copy_tag(&mut ifd, self.tiff.root_ifd(), DngTag::ProfileHueSatMapDims);
        IFD::copy_tag(&mut ifd, self.tiff.root_ifd(), DngTag::ProfileHueSatMapData1);
        IFD::copy_tag(&mut ifd, self.tiff.root_ifd(), DngTag::ProfileHueSatMapData2);
        IFD::copy_tag(&mut ifd, self.tiff.root_ifd(), DngTag::ProfileHueSatMapData3);
        IFD::copy_tag(&mut ifd, self.tiff.root_ifd(), DngTag::ProfileHueSatMapEncoding);
        IFD::copy_tag(&mut ifd, self.tiff.root_ifd(), DngTag::ProfileLookTableData);
        IFD::copy_tag(&mut ifd, self.tiff.root_ifd(), DngTag::ProfileLookTableDims);
        IFD::copy_tag(&mut ifd, self.tiff.root_ifd(), DngTag::ProfileLookTableEncoding);
        IFD::copy_tag(&mut ifd, self.tiff.root_ifd(), DngTag::ProfileName);
        IFD::copy_tag(&mut ifd, self.tiff.root_ifd(), DngTag::ProfileCopyright);
        IFD::copy_tag(&mut ifd, self.tiff.root_ifd(), DngTag::ProfileToneCurve);
        IFD::copy_tag(&mut ifd, self.tiff.root_ifd(), DngTag::DNGPrivateData);
        IFD::copy_tag(&mut ifd, self.tiff.root_ifd(), DngTag::MakerNoteSafety);
        IFD::copy_tag(&mut ifd, self.tiff.root_ifd(), DngTag::AnalogBalance);
        IFD::copy_tag(&mut ifd, self.tiff.root_ifd(), DngTag::BaselineExposure);
        IFD::copy_tag(&mut ifd, self.tiff.root_ifd(), DngTag::BaselineNoise);
        IFD::copy_tag(&mut ifd, self.tiff.root_ifd(), DngTag::BaselineSharpness);
        IFD::copy_tag(&mut ifd, self.tiff.root_ifd(), DngTag::LinearResponseLimit);
        IFD::copy_tag(&mut ifd, self.tiff.root_ifd(), DngTag::CameraSerialNumber);
        IFD::copy_tag(&mut ifd, self.tiff.root_ifd(), DngTag::AsShotICCProfile);
        IFD::copy_tag(&mut ifd, self.tiff.root_ifd(), DngTag::AsShotPreProfileMatrix);
        IFD::copy_tag(&mut ifd, self.tiff.root_ifd(), DngTag::CurrentICCProfile);
        IFD::copy_tag(&mut ifd, self.tiff.root_ifd(), DngTag::CurrentPreProfileMatrix);
        IFD::copy_tag(&mut ifd, self.tiff.root_ifd(), DngTag::AsShotProfileName);
        IFD::copy_tag(&mut ifd, self.tiff.root_ifd(), DngTag::DefaultBlackRender);
        IFD::copy_tag(&mut ifd, self.tiff.root_ifd(), DngTag::BaselineExposureOffset);
        IFD::copy_tag(&mut ifd, self.tiff.root_ifd(), DngTag::DepthFormat);
        IFD::copy_tag(&mut ifd, self.tiff.root_ifd(), DngTag::DepthNear);
        IFD::copy_tag(&mut ifd, self.tiff.root_ifd(), DngTag::DepthFar);
        IFD::copy_tag(&mut ifd, self.tiff.root_ifd(), DngTag::DepthUnits);
        IFD::copy_tag(&mut ifd, self.tiff.root_ifd(), DngTag::DepthMeasureType);
        IFD::copy_tag(&mut ifd, self.tiff.root_ifd(), DngTag::RGBTables);
        Some(Rc::new(ifd))
      }
      _ => return Ok(None),
    })
  }

  fn format_hint(&self) -> FormatHint {
    FormatHint::DNG
  }
}

impl<'a> DngDecoder<'a> {
  /// RAWTONIC: Lightroom's panoramas mark their empty edges in a DNG 1.6
  /// transparency mask (NewSubFileType 4); the image data there is padding
  /// (edge pixels smeared outwards). Transparent pixels are set to black,
  /// like the empty edges of Rawtonic's own merges, partly transparent ones
  /// scaled towards it. Lightroom codes the mask lossily: inside the image
  /// it wavers just below opaque, so only values below 0.9 count (ramp to
  /// black at 0.1).
  fn apply_transparency_mask(&self, file: &RawSource, data: &mut RawImageData, width: usize, height: usize, black: Option<&BlackLevel>) {
    let Some(mask_ifd) = self.tiff.find_ifds_with_tag(TiffCommonTag::NewSubFileType).into_iter().find(|ifd| {
      ifd.get_entry(TiffCommonTag::NewSubFileType).map(|e| e.force_u32(0)) == Some(4)
        && ifd.get_entry(TiffCommonTag::ImageWidth).map(|e| e.force_usize(0)) == Some(width)
        && ifd.get_entry(TiffCommonTag::ImageLength).map(|e| e.force_usize(0)) == Some(height)
    }) else {
      return;
    };
    let bits = mask_ifd
      .get_entry(TiffCommonTag::BitsPerSample)
      .map(|e| e.force_u32(0))
      .unwrap_or(8)
      .clamp(1, 16);
    let max = ((1u32 << bits) - 1) as f32;
    let mask = match plain_image_from_ifd(mask_ifd, file) {
      Ok(RawImageData::Integer(mask)) if mask.len() == width * height => mask,
      _ => {
        log::warn!("DNG: transparency mask could not be read; ignored");
        return;
      }
    };
    let black: Vec<f32> = black.map(|b| b.as_vec()).unwrap_or_default();
    let black_of = |c: usize| black.get(c).or(black.first()).copied().unwrap_or(0.0);
    let alpha = |m: u16| ((m as f32 / max - 0.1) / 0.8).clamp(0.0, 1.0);
    match data {
      RawImageData::Integer(samples) if samples.len() == width * height * 3 => {
        samples.par_chunks_exact_mut(3).zip(mask.par_iter()).for_each(|(px, &m)| {
          let a = alpha(m);
          if a < 1.0 {
            for (c, v) in px.iter_mut().enumerate() {
              let b = black_of(c);
              *v = (b + (*v as f32 - b) * a).round().clamp(0.0, u16::MAX as f32) as u16;
            }
          }
        });
      }
      RawImageData::Float(samples) if samples.len() == width * height * 3 => {
        samples.par_chunks_exact_mut(3).zip(mask.par_iter()).for_each(|(px, &m)| {
          let a = alpha(m);
          if a < 1.0 {
            for (c, v) in px.iter_mut().enumerate() {
              let b = black_of(c);
              *v = b + (*v - b) * a;
            }
          }
        });
      }
      _ => {}
    }
  }

  fn get_raw_ifd(&self) -> Result<&IFD> {
    let ifds = self
      .tiff
      .find_ifds_with_tag(TiffCommonTag::Compression)
      .into_iter()
      .filter(|ifd| {
        let compression = (**ifd)
          .get_entry(TiffCommonTag::Compression)
          .expect("This IFD must contains this tag")
          .force_u32(0);
        // RAWTONIC: transparency masks (NewSubFileType 4) are not image data.
        let subsampled = match (**ifd).get_entry(TiffCommonTag::NewSubFileType) {
          Some(e) => e.force_u32(0) & (1 | 4) != 0,
          None => false,
        };
        !subsampled && (compression == 7 || compression == 8 || compression == 1 || compression == 0x884c || compression == 52546)
      })
      .collect::<Vec<&IFD>>();
    // RAWTONIC: DNG 1.7 "enhanced image data" (NewSubFileType 16, e.g.
    // Lightroom's Denoise result stored next to the original CFA data) is
    // what Camera Raw renders; prefer it.
    let enhanced = ifds
      .iter()
      .find(|ifd| (**ifd).get_entry(TiffCommonTag::NewSubFileType).map(|e| e.force_u32(0)) == Some(16));
    if let Some(first) = enhanced.or(ifds.first()) {
      Ok(first)
    } else {
      Err(RawlerError::DecoderFailed(format!("TODO: Unsupported DNG compression")))
    }
  }

  fn make_camera(&self, raw: &IFD, width: usize, height: usize) -> Result<Camera> {
    let mode = String::from("dng");

    let make = self
      .tiff
      .root_ifd()
      .get_entry(TiffCommonTag::Make)
      .and_then(|x| x.as_string())
      .cloned()
      .unwrap_or_default();
    let model = self
      .tiff
      .root_ifd()
      .get_entry(TiffCommonTag::Model)
      .and_then(|x| x.as_string())
      .cloned()
      .unwrap_or_default();

    let active_area = self.get_active_area(raw, width, height);
    let crop_area = if let Some(crops) = self.get_crop(raw) {
      if let Some(active_area) = &active_area {
        let mut full = crops;
        full.p.x += active_area[0]; // left
        full.p.y += active_area[1]; // Top
        Some(full.as_ltrb_offsets(width, height))
      } else {
        Some(crops.as_ltrb_offsets(width, height))
      }
    } else {
      None
    };

    let linear = fetch_tiff_tag!(raw, TiffCommonTag::PhotometricInt).force_usize(0) == 34892;
    let cfa = if linear { CFA::default() } else { self.get_cfa(raw)? };
    let color_matrix = self.get_color_matrix()?;
    let real_bps = if raw.has_entry(TiffCommonTag::Linearization) {
      // If DNG contains linearization table, output is always 16 bits
      16
    } else {
      raw.get_entry(TiffCommonTag::BitsPerSample).map(|v| v.force_usize(0)).unwrap_or(16)
    };

    Ok(Camera {
      clean_make: make.clone(),
      clean_model: model.clone(),
      make,
      model,
      mode,
      whitelevel: None,
      blacklevel: None,
      blackareah: None,
      blackareav: None,
      xyz_to_cam: Default::default(),
      color_matrix,
      cfa,
      active_area,
      crop_area,
      real_bps,
      // RAWTONIC: four-colour DNGs (RGB + cyan/emerald, CMYG) name their
      // planes in CFAPlaneColor; the default three planes made the
      // demosaic panic ("needs 4 color planes, but 3 given").
      plane_color: self.get_plane_color(raw).unwrap_or_default(),
      ..Default::default()
    })
  }

  fn get_plane_color(&self, raw: &IFD) -> Option<PlaneColor> {
    let planes = raw.get_entry(DngTag::CFAPlaneColor)?;
    if planes.count() != 4 {
      return None;
    }
    let name: String = (0..4)
      .map(|i| match planes.force_u32(i) {
        0 => Some('R'),
        1 => Some('G'),
        2 => Some('B'),
        3 => Some('C'),
        4 => Some('M'),
        5 => Some('Y'),
        _ => None,
      })
      .collect::<Option<_>>()?;
    Some(PlaneColor::new(&name))
  }

  fn get_wb(&self, cam: &Camera) -> Result<[f32; 4]> {
    if let Some(levels) = self.tiff.get_entry(DngTag::AsShotNeutral) {
      Ok([1.0 / levels.force_f32(0), 1.0 / levels.force_f32(1), 1.0 / levels.force_f32(2), f32::NAN])
    } else if let Some(levels) = self.tiff.get_entry(DngTag::AsShotWhiteXY) {
      // TODO: improve once AnalogBalance and CC is properly implemented
      if let Some(flat_colormatrix) = cam.color_matrix.get(&Illuminant::D65)
        && let Some(colormatrix) = transform_1d::<3, 3>(flat_colormatrix)
      {
        let wb_coeff = xy_whitepoint_to_wb_coeff(levels.force_f32(0), levels.force_f32(1), &colormatrix);
        Ok([wb_coeff[0], wb_coeff[1], wb_coeff[2], f32::NAN])
      } else {
        Ok([f32::NAN, f32::NAN, f32::NAN, f32::NAN])
      }
    } else {
      Ok([f32::NAN, f32::NAN, f32::NAN, f32::NAN])
    }
  }

  fn get_blacklevels(&self, raw: &IFD) -> Result<Option<BlackLevel>> {
    let cpp = raw.get_entry(TiffCommonTag::SamplesPerPixel).map(|entry| entry.force_usize(0)).unwrap_or(1);
    if let Some(entry) = raw.get_entry(TiffCommonTag::BlackLevels) {
      let levels = match &entry.value {
        Value::Short(black) => black.iter().copied().map(Rational::from).collect(),
        Value::Long(black) => black.iter().copied().map(Rational::from).collect(),
        Value::Rational(black) => black.clone(),
        _ => return Err(format!("Unsupported BlackLevel type: {}", entry.value_type_name()).into()),
      };
      let mut repeat = (1, 1);
      if let Some(Entry {
        value: Value::Short(value), ..
      }) = raw.get_entry(DngTag::BlackLevelRepeatDim)
      {
        if value.len() == 2 {
          repeat = (value[0] as usize, value[1] as usize);
        } else {
          // Pentax K-3 Mark III Monochrome is known to has invalid tag
          log::warn!("File has BlackLevelRepeatDim tag but with invalid length: {}", value.len());
        }
      }
      Ok(Some(BlackLevel::new(&levels, repeat.1, repeat.0, cpp)))
    } else {
      Ok(None)
    }
  }

  fn get_whitelevels(&self, raw: &IFD) -> Result<Option<WhiteLevel>> {
    let cpp = fetch_tiff_tag!(raw, TiffCommonTag::SamplesPerPixel).force_usize(0);
    if let Some(levels) = raw.get_entry(TiffCommonTag::WhiteLevel) {
      let mut whitelevels = WhiteLevel((0..levels.count()).map(|i| levels.force_u32(i as usize)).collect());
      // Fixes a bug where only a single whitelevel value is given.
      if whitelevels.0.len() == 1 && cpp > 1 {
        whitelevels.0 = vec![whitelevels.0[0]; cpp];
      }
      return Ok(Some(whitelevels));
    }
    Ok(None)
  }

  fn get_cfa(&self, raw: &IFD) -> Result<CFA> {
    let pattern = fetch_tiff_tag!(raw, TiffCommonTag::CFAPattern);
    let cfa = CFA::new_from_tag(pattern);
    // If DNG has active area, we need to calulate back the CFA pattern,
    // because for DNG the CFAPattern is relative to ActiveArea and we
    // use (0, 0) as starting point.
    if let Some(active_area) = self.get_active_area_borders(raw) {
      let top = active_area[0];
      let left = active_area[1];
      Ok(cfa.shift(left % cfa.width, top % cfa.height))
    } else {
      Ok(cfa)
    }
  }

  fn get_active_area_borders(&self, raw: &IFD) -> Option<[usize; 4]> {
    if let Some(crops) = raw.get_entry(DngTag::ActiveArea) {
      let rect = [crops.force_usize(0), crops.force_usize(1), crops.force_usize(2), crops.force_usize(3)];
      Some(rect)
    } else {
      // Ignore missing crops, at least some pentax DNGs don't have it
      None
    }
  }

  fn get_active_area(&self, raw: &IFD, width: usize, height: usize) -> Option<[usize; 4]> {
    if let Some(rect) = self.get_active_area_borders(raw) {
      Some(Rect::new_with_dng(&rect).as_ltrb_offsets(width, height))
    } else {
      None
    }
  }

  fn get_crop(&self, raw: &IFD) -> Option<Rect> {
    // RAWTONIC: an enhanced image (NewSubFileType 16) covers the main
    // image's active area and carries no crop of its own; the main image's
    // default crop (relative to its active area) applies.
    let enhanced = raw.get_entry(TiffCommonTag::NewSubFileType).map(|e| e.force_u32(0)) == Some(16);
    if enhanced && !raw.has_entry(DngTag::DefaultCropOrigin) {
      let main = self
        .tiff
        .find_ifds_with_tag(TiffCommonTag::NewSubFileType)
        .into_iter()
        .find(|ifd| ifd.get_entry(TiffCommonTag::NewSubFileType).map(|e| e.force_u32(0)) == Some(0) && ifd.has_entry(DngTag::DefaultCropOrigin));
      if let Some(main) = main {
        return self.get_crop(main);
      }
    }
    if let Some(crops) = raw.get_entry(DngTag::DefaultCropOrigin) {
      let p = Point::new(crops.force_usize(0), crops.force_usize(1));
      if let Some(size) = raw.get_entry(DngTag::DefaultCropSize) {
        let d = Dim2::new(size.force_usize(0), size.force_usize(1));
        return Some(Rect::new(p, d));
      }
    }
    None
  }

  /// Rawtonic: MaskedAreas are (top, left, bottom, right) per area.
  fn get_masked_areas(&self, raw: &IFD) -> Vec<Rect> {
    let mut areas = Vec::new();

    if let Some(masked_area) = raw.get_entry(TiffCommonTag::MaskedAreas) {
      for x in (0..masked_area.count() as usize).step_by(4) {
        areas.push(Rect::new_with_points(
          Point::new(masked_area.force_usize(x + 1), masked_area.force_usize(x)),
          Point::new(masked_area.force_usize(x + 3), masked_area.force_usize(x + 2)),
        ));
      }
    }

    areas
  }

  fn get_color_matrix(&self) -> Result<HashMap<Illuminant, FlatColorMatrix>> {
    let mut result = HashMap::new();

    let mut read_matrix = |cal: DngTag, mat: DngTag| -> Result<()> {
      if let Some(c) = self.tiff.get_entry(mat) {
        let illuminant_val = self.tiff.get_entry(cal).map(|e| e.force_u16(0)).unwrap_or(21);
        let illuminant: Illuminant = illuminant_val.try_into().unwrap_or(Illuminant::D65);

        let mut matrix = FlatColorMatrix::new();
        for i in 0..c.count() as usize {
          matrix.push(c.force_f32(i));
        }

        if !matrix.is_empty() && matrix.len() <= 12 {
          result.insert(illuminant, matrix);
        } else {
          log::warn!("Invalid ColorMatrix dimensions for illuminant {:?}: length {}", illuminant, matrix.len());
        }
      }
      Ok(())
    };

    let _ = read_matrix(DngTag::CalibrationIlluminant1, DngTag::ColorMatrix1);
    let _ = read_matrix(DngTag::CalibrationIlluminant2, DngTag::ColorMatrix2);
    // TODO: add 3

    Ok(result)
  }
}
