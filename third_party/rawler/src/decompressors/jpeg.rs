use crate::decompressors::ljpeg::LjpegDecompressor;
use crate::decompressors::{Decompressor, LineIteratorMut};
use crate::pixarray::PixU16;
pub struct LJpegDecompressor {
  /// Rawtonic patch (RAWL1): category 16 differences carry 16 extra bits
  /// (DNG files before version 1.1).
  dng_bug: bool,
}

impl LJpegDecompressor {
  pub fn new() -> Self {
    Self { dng_bug: false }
  }

  /// Rawtonic patch (RAWL1): for the lossless JPEG of DNG files before
  /// version 1.1, whose category 16 differences carry 16 extra bits.
  pub fn new_with_dng_bug(dng_bug: bool) -> Self {
    Self { dng_bug }
  }
}

impl<'a> Decompressor<'a, u16> for LJpegDecompressor {
  fn decompress(&self, src: &[u8], skip_rows: usize, lines: impl LineIteratorMut<'a, u16>, line_width: usize) -> std::result::Result<(), String> {
    let decompressor = if self.dng_bug {
      LjpegDecompressor::new_old_dng(src)?
    } else {
      LjpegDecompressor::new(src)?
    };
    let mut pixbuf = PixU16::new(decompressor.width(), decompressor.height());

    decompressor.decode(pixbuf.pixels_mut(), 0, decompressor.width(), decompressor.width(), decompressor.height(), false)?;

    for (dst, src) in lines.zip(pixbuf.pixels().chunks_exact(line_width).skip(skip_rows)) {
      dst.copy_from_slice(src);
    }

    Ok(())
  }

  fn tile_optimized(&self) -> bool {
    true
  }
}

pub struct JpegDecompressor {}

impl JpegDecompressor {
  pub fn new() -> Self {
    Self {}
  }
}

impl<'a> Decompressor<'a, u16> for JpegDecompressor {
  fn decompress(&self, src: &[u8], skip_rows: usize, lines: impl LineIteratorMut<'a, u16>, line_width: usize) -> std::result::Result<(), String> {
    let img = image::load_from_memory_with_format(src, image::ImageFormat::Jpeg).map_err(|err| format!("Lossy JPEG decompression failed: {:?}", err))?;
    match img {
      image::DynamicImage::ImageRgb8(image_buffer) => {
        for (dst, src) in lines.zip(image_buffer.chunks_exact(line_width).skip(skip_rows)) {
          for (dst, src) in dst.iter_mut().zip(src.iter()) {
            *dst = *src as u16; // Only change storage format, you MUST NOT scale up!
          }
        }
      }
      // Rawtonic patch (RWH1): an error, not a panic.
      _ => return Err("JPEG: unsupported pixel format".to_string()),
    }

    Ok(())
  }

  fn tile_optimized(&self) -> bool {
    true
  }
}
