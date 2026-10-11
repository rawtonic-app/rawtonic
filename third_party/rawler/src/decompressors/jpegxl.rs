// SPDX-License-Identifier: LGPL-2.1
// Copyright 2025 Daniel Vogelbacher <daniel@chaospixel.com>

use jxl_oxide::JxlImage;

use crate::decompressors::{Decompressor, LineIteratorMut};

pub struct JpegXLDecompressor {
  bps: u32,
}

impl JpegXLDecompressor {
  pub fn new(bps: u32) -> Self {
    Self { bps }
  }
}

impl<'a> Decompressor<'a, u16> for JpegXLDecompressor {
  fn decompress(&self, src: &[u8], mut skip_rows: usize, lines: impl LineIteratorMut<'a, u16>, line_width: usize) -> std::result::Result<(), String> {
    // RAWTONIC: see `open_wide`. Lossy (XYB) integer tiles (Lightroom's
    // panoramas of integer raws) take the same route as float tiles, then
    // are quantised: jxl-oxide's conversion to the signalled space made
    // them pink and blown out.
    let (image, matrix) = open_wide(src)?;
    if let Some(m) = matrix {
      let frame = image.render_frame(0).map_err(|err| format!("Failed to render JPEG-XL image: {:?}", err))?;
      let mut stream = frame.stream_no_alpha();
      let channels = stream.channels() as usize;
      let scale = ((1u32 << self.bps.clamp(8, 16)) - 1) as f32;
      let mut tmp = vec![0.0f32; line_width];
      for line in lines {
        while skip_rows > 0 {
          stream.write_to_buffer(&mut tmp[..line.len()]);
          skip_rows -= 1;
        }
        let row = &mut tmp[..line.len()];
        let written = stream.write_to_buffer(row);
        if written != line.len() {
          return Err(format!("JPEG-XL tile row has {written} samples, expected {}", line.len()));
        }
        if channels == 3 {
          row.chunks_exact_mut(3).for_each(|px| m.apply(px));
        }
        for (p, v) in line.iter_mut().zip(row.iter()) {
          *p = (v * scale).round().clamp(0.0, scale) as u16;
        }
      }
      return Ok(());
    }
    if let Some(header) = image.frame_header(0) {
      if header.bit_depth.bits_per_sample() != 8 && header.bit_depth.bits_per_sample() != 16 {
        // jxl_oxide scales the pixels into full range of storage type.
        // If we get e.g. 12 bit compressed data, output is scaled to 16 bit.
        // This breaks blacklevel scaling. We need to scale back to given bps (from TIFF).
        unimplemented!("JPEG-XL bit-depth {} not supported yet", header.bit_depth.bits_per_sample());
      }
      //eprintln!("JPEG-XL Bit-Depth: {:?}", header.bit_depth);
    }
    let frame = image.render_frame(0).map_err(|err| format!("Failed to render JPEG-XL image: {:?}", err))?;

    let mut stream = frame.stream_no_alpha();

    match self.bps {
      8 => {
        let mut tmp = vec![0_u8; line_width];

        for line in lines.skip(skip_rows) {
          while skip_rows > 0 {
            let written = stream.write_to_buffer(&mut tmp);
            assert_eq!(line.len(), written);
            skip_rows -= 1;
          }
          let written = stream.write_to_buffer(&mut tmp);
          assert_eq!(line.len(), written);
          for (p, x) in line.iter_mut().zip(tmp.iter()) {
            *p = *x as u16; // Only change storage format, you MUST NOT scale up!
          }
        }
      }
      9..=16 => {
        for line in lines.skip(skip_rows) {
          while skip_rows > 0 {
            let written = stream.write_to_buffer(line);
            assert_eq!(line.len(), written);
            skip_rows -= 1;
          }
          let written = stream.write_to_buffer(line);
          assert_eq!(line.len(), written);
        }
      }
      _ => unimplemented!(),
    }

    /*
    let all_ch = frame.image_all_channels();

    let pixbuf = all_ch.buf();
    for (line, buf) in lines.zip(pixbuf.chunks_exact(line_width).skip(skip_rows)) {
      for (p, f) in line.iter_mut().zip(buf.iter()) {
        //debug_assert!(*f <= (1.0 + f32::EPSILON));
        // *p = (f * u16::MAX as f32) as u16;
        // *p = *f as u16;
      }
    }
    */
    Ok(())
  }
}

// RAWTONIC: floating-point DNGs (DNG 1.7: Lightroom's HDR and panorama
// merges, lossy VarDCT) store float samples, often far above 1.
//
// - Encoders set `modular_16bit_buffers` (the default) even for such data;
//   libjxl ignores the hint, jxl-oxide trusted it and overflowed
//   (InvalidAnsStream), so wide buffers are forced.
// - For XYB-coded tiles jxl-oxide's conversion into the signalled colour
//   space (e.g. linear BT.2100) gamut-maps for a display, which turned values
//   above 1 into grey. Linear sRGB comes straight out of XYB; it is turned
//   into the signalled primaries here, giving the stored values (checked
//   against libjxl's djxl on a Lightroom HDR tile: equal to 1e-4).
/// Linear sRGB → the signalled primaries, then the signalled transfer curve
/// (Lightroom's float tiles are linear; its integer tiles sRGB-encoded).
#[derive(Clone, Copy)]
struct ToSignalled {
  matrix: [[f32; 3]; 3],
  srgb_curve: bool,
}

impl ToSignalled {
  fn apply(&self, px: &mut [f32]) {
    let (r, g, b) = (px[0], px[1], px[2]);
    for (o, row) in px.iter_mut().zip(&self.matrix) {
      let v = row[0] * r + row[1] * g + row[2] * b;
      *o = if self.srgb_curve { srgb_encode(v) } else { v };
    }
  }
}

fn srgb_encode(v: f32) -> f32 {
  if v <= 0.003_130_8 { v * 12.92 } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 }
}

fn open_wide(src: &[u8]) -> std::result::Result<(JxlImage, Option<ToSignalled>), String> {
  use jxl_oxide::color::{ColourEncoding, EnumColourEncoding, Primaries, RenderingIntent, TransferFunction, WhitePoint};
  let mut image = JxlImage::builder()
    .force_wide_buffers(true)
    .read(src)
    .map_err(|err| format!("Failed to read JPEG-XL image: {:?}", err))?;
  let metadata = &image.image_header().metadata;
  if !metadata.xyb_encoded {
    return Ok((image, None));
  }
  // Linear sRGB → the signalled primaries (D65) and curve.
  let (primaries, srgb_curve) = match &metadata.colour_encoding {
    ColourEncoding::Enum(EnumColourEncoding {
      primaries,
      white_point: WhitePoint::D65,
      tf: TransferFunction::Linear,
      ..
    }) => (Some(*primaries), false),
    ColourEncoding::Enum(EnumColourEncoding {
      primaries,
      white_point: WhitePoint::D65,
      tf: TransferFunction::Srgb,
      ..
    }) => (Some(*primaries), true),
    _ => (None, false),
  };
  let matrix = match primaries {
    Some(primaries) => match primaries {
      Primaries::Srgb => Some([[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]),
      Primaries::Bt2100 => Some([
        [0.627_391_8, 0.329_289_3, 0.043_318_9],
        [0.069_107_29, 0.919_537_4, 0.011_355_42],
        [0.016_385_54, 0.088_004_62, 0.895_609_8],
      ]),
      Primaries::P3 => Some([
        [0.822_462_1, 0.177_538, 0.0],
        [0.033_194_1, 0.966_805_8, 0.0],
        [0.017_082_7, 0.072_397_4, 0.910_519_9],
      ]),
      _ => None,
    },
    None => None,
  };
  if matrix.is_some() {
    image.request_color_encoding(EnumColourEncoding::srgb_linear(RenderingIntent::Relative));
  }
  Ok((image, matrix.map(|matrix| ToSignalled { matrix, srgb_curve })))
}

impl<'a> Decompressor<'a, f32> for JpegXLDecompressor {
  fn decompress(&self, src: &[u8], mut skip_rows: usize, lines: impl LineIteratorMut<'a, f32>, _line_width: usize) -> std::result::Result<(), String> {
    let (image, matrix) = open_wide(src)?;
    let frame = image.render_frame(0).map_err(|err| format!("Failed to render JPEG-XL image: {:?}", err))?;
    let mut stream = frame.stream_no_alpha();
    let channels = stream.channels() as usize;
    for line in lines {
      while skip_rows > 0 {
        let written = stream.write_to_buffer(line);
        assert_eq!(line.len(), written);
        skip_rows -= 1;
      }
      let written = stream.write_to_buffer(line);
      if written != line.len() {
        return Err(format!("JPEG-XL tile row has {written} samples, expected {}", line.len()));
      }
      if let (Some(m), 3) = (matrix, channels) {
        line.chunks_exact_mut(3).for_each(|px| m.apply(px));
      }
    }
    Ok(())
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  /// Decodes a 16-bit integer tile (`RAWLER_JXL_TILE`, square) to raw
  /// little-endian u16 RGB at `RAWLER_JXL_OUT`, for comparing with djxl.
  #[test]
  #[ignore]
  fn dump_integer_tile() {
    let (Some(tile), Some(out)) = (std::env::var_os("RAWLER_JXL_TILE"), std::env::var_os("RAWLER_JXL_OUT")) else {
      return;
    };
    let src = std::fs::read(tile).unwrap();
    let side = JxlImage::builder().read(&src[..]).unwrap().width() as usize;
    let mut buf = vec![0u16; side * side * 3];
    JpegXLDecompressor::new(16)
      .decompress(&src, 0, buf.chunks_exact_mut(side * 3), side * 3)
      .unwrap();
    std::fs::write(out, buf.iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<u8>>()).unwrap();
  }

  /// A 64×64 lossy (XYB) float tile in linear BT.2100, R/G/B = 0.4/1/0.97
  /// of a ramp from 0.01 to 3000 (made with libjxl's cjxl -d 1): values far
  /// above 1 must come back, not grey or wrapped.
  #[test]
  fn hdr_float_xyb_tile_keeps_its_values() {
    let src = include_bytes!("jpegxl_hdr_float_test.jxl");
    let mut buf = vec![0.0f32; 64 * 64 * 3];
    JpegXLDecompressor::new(32).decompress(src, 0, buf.chunks_exact_mut(64 * 3), 64 * 3).unwrap();
    for i in [0usize, 1000, 2048, 3500, 4095] {
      let v = (0.01f64.ln() + (3000f64.ln() - 0.01f64.ln()) * i as f64 / 4095.0).exp() as f32;
      let p = &buf[i * 3..i * 3 + 3];
      for (got, want) in p.iter().zip([0.4 * v, v, 0.97 * v]) {
        assert!((got - want).abs() <= 0.08 * want + 0.01, "pixel {i}: {p:?}, want {want}");
      }
    }
  }
}
