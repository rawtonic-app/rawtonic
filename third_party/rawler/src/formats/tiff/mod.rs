// SPDX-License-Identifier: MIT
// Copyright 2021 Daniel Vogelbacher <daniel@chaospixel.com>

use std::io::{Read, Seek, SeekFrom};

use thiserror::Error;

pub mod entry;
pub mod file;
pub mod ifd;
pub mod reader;
pub mod value;
pub mod writer;

pub use entry::Entry;
pub use ifd::IFD;
pub use reader::GenericTiffReader;
pub use value::{IntoTiffValue, Rational, SRational, TiffAscii, Value};
pub use writer::{DirectoryWriter, TiffWriter};

const TIFF_MAGIC: u16 = 42;

#[allow(clippy::upper_case_acronyms)]
#[derive(Debug, Copy, Clone, PartialEq, enumn::N)]
#[repr(u16)]
pub enum CompressionMethod {
  None = 1,
  Huffman = 2,
  Fax3 = 3,
  Fax4 = 4,
  LZW = 5,
  OldJPEG = 6, // TODO: RENAME
  // "Extended JPEG" or "new JPEG" style
  ModernJPEG = 7,
  Deflate = 8,
  OldDeflate = 0x80B2,
  PackBits = 0x8005,
  // DNG Extensions
  LossyJPEG = 34892,
  JPEGXL = 52546,
}

impl ExtractFromIFD for CompressionMethod {
  fn extract(ifd: &IFD) -> Result<Option<Self>> {
    let tag = TiffCommonTag::Compression;
    if let Some(entry) = ifd.get_entry(tag) {
      let x = entry.value.force_u16(0);
      Ok(Some(
        Self::n(x).ok_or_else(|| TiffError::General(format!("Unknown value {} for {:?}", x, tag)))?,
      ))
    } else {
      Ok(None)
    }
  }
}

impl From<CompressionMethod> for Value {
  fn from(value: CompressionMethod) -> Self {
    Value::Short(vec![value as u16])
  }
}

#[allow(clippy::upper_case_acronyms)]
#[derive(Debug, Copy, Clone, PartialEq, enumn::N)]
#[repr(u16)]
pub enum PhotometricInterpretation {
  WhiteIsZero = 0,
  BlackIsZero = 1,
  RGB = 2,
  RGBPalette = 3,
  TransparencyMask = 4,
  CMYK = 5,
  YCbCr = 6,
  CIELab = 8,
  // Defined by DNG
  CFA = 32803,
  LinearRaw = 34892,
}

impl ExtractFromIFD for PhotometricInterpretation {
  fn extract(ifd: &IFD) -> Result<Option<Self>> {
    let tag = TiffCommonTag::PhotometricInt;
    if let Some(entry) = ifd.get_entry(tag) {
      let x = entry.value.force_u16(0);
      Ok(Some(
        Self::n(x).ok_or_else(|| TiffError::General(format!("Unknown value {} for {:?}", x, tag)))?,
      ))
    } else {
      Ok(None)
    }
  }
}

impl From<PhotometricInterpretation> for Value {
  fn from(value: PhotometricInterpretation) -> Self {
    Value::Short(vec![value as u16])
  }
}

#[derive(Debug, Copy, Clone, PartialEq, enumn::N)]
#[repr(u16)]
pub enum PreviewColorSpace {
  Unknown = 0,
  GrayGamma = 1,
  SRgb = 2,
  AdobeRGB = 3,
  ProPhotoRGB = 5,
}

impl ExtractFromIFD for PreviewColorSpace {
  fn extract(ifd: &IFD) -> Result<Option<Self>> {
    let tag = DngTag::PreviewColorSpace;
    if let Some(entry) = ifd.get_entry(tag) {
      let x = entry.value.force_u16(0);
      Ok(Some(
        Self::n(x).ok_or_else(|| TiffError::General(format!("Unknown value {} for {:?}", x, tag)))?,
      ))
    } else {
      Ok(None)
    }
  }
}

impl From<PreviewColorSpace> for Value {
  fn from(value: PreviewColorSpace) -> Self {
    Value::Long(vec![value as u32])
  }
}

#[derive(Debug, Copy, Clone, PartialEq, enumn::N)]
#[repr(u16)]
pub enum PlanarConfiguration {
  Chunky = 1,
  Planar = 2,
}

impl ExtractFromIFD for PlanarConfiguration {
  fn extract(ifd: &IFD) -> Result<Option<Self>> {
    let tag = ExifTag::PlanarConfiguration;
    if let Some(entry) = ifd.get_entry(tag) {
      let x = entry.value.force_u16(0);
      Ok(Some(
        Self::n(x).ok_or_else(|| TiffError::General(format!("Unknown value {} for {:?}", x, tag)))?,
      ))
    } else {
      Ok(None)
    }
  }
}

impl From<PlanarConfiguration> for Value {
  fn from(value: PlanarConfiguration) -> Self {
    Value::Short(vec![value as u16])
  }
}

#[derive(Debug, Copy, Clone, PartialEq, enumn::N)]
#[repr(u16)]
pub enum Predictor {
  None = 1,
  Horizontal = 2,
}

impl ExtractFromIFD for Predictor {
  fn extract(ifd: &IFD) -> Result<Option<Self>> {
    let tag = TiffCommonTag::Predictor;
    if let Some(entry) = ifd.get_entry(tag) {
      let x = entry.value.force_u16(0);
      Ok(Some(
        Self::n(x).ok_or_else(|| TiffError::General(format!("Unknown value {} for {:?}", x, tag)))?,
      ))
    } else {
      Ok(None)
    }
  }
}

impl From<Predictor> for Value {
  fn from(value: Predictor) -> Self {
    Value::Short(vec![value as u16])
  }
}

/// Type to represent resolution units
#[derive(Debug, Copy, Clone, PartialEq, enumn::N)]
#[repr(u16)]
pub enum ResolutionUnit {
  None = 1,
  Inch = 2,
  Centimeter = 3,
}

impl ExtractFromIFD for ResolutionUnit {
  fn extract(ifd: &IFD) -> Result<Option<Self>> {
    let tag = TiffCommonTag::Predictor;
    if let Some(entry) = ifd.get_entry(tag) {
      let x = entry.value.force_u16(0);
      Ok(Some(
        Self::n(x).ok_or_else(|| TiffError::General(format!("Unknown value {} for {:?}", x, tag)))?,
      ))
    } else {
      Ok(None)
    }
  }
}

impl From<ResolutionUnit> for Value {
  fn from(value: ResolutionUnit) -> Self {
    Value::Short(vec![value as u16])
  }
}

#[allow(clippy::upper_case_acronyms)]
#[derive(Debug, Copy, Clone, PartialEq, enumn::N)]
#[repr(u16)]
pub enum SampleFormat {
  Uint = 1,
  Int = 2,
  IEEEFP = 3,
  Void = 4,
}

impl From<SampleFormat> for Value {
  fn from(value: SampleFormat) -> Self {
    Value::Short(vec![value as u16])
  }
}

impl ExtractFromIFD for SampleFormat {
  fn extract(ifd: &IFD) -> Result<Option<Self>> {
    let tag = TiffCommonTag::SampleFormat;
    if let Some(entry) = ifd.get_entry(tag) {
      let x = entry.value.force_u16(0);
      Ok(Some(
        Self::n(x).ok_or_else(|| TiffError::General(format!("Unknown value {} for {:?}", x, tag)))?,
      ))
    } else {
      Ok(None)
    }
  }
}

pub trait ExtractFromIFD
where
  Self: Sized,
{
  fn extract(ifd: &IFD) -> Result<Option<Self>>;
}

/// Error variants for compressor
#[derive(Debug, Error)]
pub enum TiffError {
  /// Overflow of input, size constraints...
  #[error("Overflow error: {}", _0)]
  Overflow(String),

  #[error("General error: {}", _0)]
  General(String),

  #[error("Format mismatch: {}", _0)]
  FormatMismatch(String),

  /// Error on internal cursor type
  #[error("I/O error: {:?}", _0)]
  Io(#[from] std::io::Error),
}

/// Result type for Compressor results
pub type Result<T> = std::result::Result<T, TiffError>;

use crate::tags::{DngTag, ExifTag, TiffCommonTag};

/*
impl From<Value> for Entry {
  fn from(value: Value) -> Self {
    Entry { value, embedded: None }
  }
}
 */

pub struct DataOffset {
  pub offset: usize,
}

fn apply_corr(offset: u32, corr: i32) -> u32 {
  ((offset as i64) + (corr as i64)) as u32
}

pub struct DirReader {}

fn read_from_file<R: Read + Seek>(file: &mut R, offset: u32, size: usize) -> Result<Vec<u8>> {
  file.seek(SeekFrom::Start(offset as u64))?;
  let mut buf = vec![0; size];
  file.read_exact(&mut buf)?;
  Ok(buf)
}

#[cfg(test)]
mod tests {
  use std::io::{Cursor, Seek, SeekFrom};

  use crate::{formats::tiff::reader::TiffReader, tags::TiffCommonTag};

  use super::*;

  /// RAWTONIC: a chain pointing back to an IFD read before ends there
  /// when no chain limit is given (it was read forever).
  #[test]
  fn a_cyclic_ifd_chain_ends() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut bytes = b"II*\0".to_vec();
    bytes.extend(8u32.to_le_bytes());
    for ifd in [8u32, 26] {
      bytes.extend(1u16.to_le_bytes());
      bytes.extend(0x0112u16.to_le_bytes());
      bytes.extend(3u16.to_le_bytes());
      bytes.extend(1u32.to_le_bytes());
      bytes.extend(1u32.to_le_bytes());
      // 8 → 26 → 8
      bytes.extend((if ifd == 8 { 26u32 } else { 8 }).to_le_bytes());
    }
    let reader = GenericTiffReader::new(&mut Cursor::new(&bytes), 0, 0, None, &[])?;
    assert_eq!(reader.file().chain.len(), 2);
    let root = IFD::new_root_with_correction(&mut Cursor::new(&bytes), 0, 0, 0, 0, &[])?;
    assert_eq!(root.chain.len(), 1);
    Ok(())
  }

  /// RAWTONIC: little-endian IFD entry (tag, type, count, value).
  fn entry(tag: u16, typ: u16, count: u32, value: u32) -> Vec<u8> {
    [&tag.to_le_bytes()[..], &typ.to_le_bytes(), &count.to_le_bytes(), &value.to_le_bytes()].concat()
  }

  /// RAWTONIC: a SubIFDs entry listing one IFD many times reads it once.
  #[test]
  fn a_repeated_sub_ifd_is_read_once() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let (array, child) = (64u32, 64u32 + 4 * 1000);
    let mut bytes = b"II*\0".to_vec();
    bytes.extend(8u32.to_le_bytes());
    bytes.extend(1u16.to_le_bytes());
    bytes.extend(entry(0x014a, 4, 1000, array));
    bytes.extend(0u32.to_le_bytes());
    bytes.resize(array as usize, 0);
    bytes.extend((0..1000).flat_map(|_| child.to_le_bytes()));
    bytes.extend(1u16.to_le_bytes());
    bytes.extend(entry(0x0112, 3, 1, 1));
    bytes.extend(0u32.to_le_bytes());
    let reader = GenericTiffReader::new(&mut Cursor::new(&bytes), 0, 0, None, &[])?;
    let subs = reader.root_ifd().get_sub_ifd_all(TiffCommonTag::SubIFDs).unwrap();
    assert_eq!(subs.len(), 1);
    Ok(())
  }

  /// RAWTONIC: an IFD whose entries each claim (nearly) the whole file
  /// allocates at most twice the file for their values, its sub-IFDs'
  /// included; a valid IFD's values are all read.
  #[test]
  fn the_values_of_an_ifd_stay_within_its_budget() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let len = 1u32 << 20;
    let mut bytes = b"II*\0".to_vec();
    bytes.extend(8u32.to_le_bytes());
    let entries = 100u16;
    bytes.extend(entries.to_le_bytes());
    for tag in 0..entries {
      bytes.extend(entry(0xc000 + tag, 1, len - 4096, 4096));
    }
    bytes.extend(0u32.to_le_bytes());
    bytes.resize(len as usize, 0);
    let root = IFD::new_root_with_correction(&mut Cursor::new(&bytes), 0, 0, 0, 0, &[])?;
    let read: usize = root.entries().values().map(|entry| entry.count() as usize).sum();
    assert!(read > 0 && read <= 2 * len as usize, "{read}");
    Ok(())
  }

  #[test]
  fn encode_tiff_test() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut buf = Cursor::new(Vec::new());
    let mut tiff = TiffWriter::new(&mut buf)?;
    let mut dir = tiff.new_directory();

    let offset = {
      let mut dir2 = tiff.new_directory();
      dir2.add_tag(32_u16, 23_u16);
      dir2.build(&mut tiff)?
    };

    dir.add_tag(TiffCommonTag::ActiveArea, offset as u16);
    dir.add_tag(TiffCommonTag::ActiveArea, [23_u16, 45_u16]);
    dir.add_tag(TiffCommonTag::ActiveArea, &[23_u16, 45_u16][..]);
    dir.add_tag(TiffCommonTag::ActiveArea, "Fobbar");

    Ok(())
  }

  #[test]
  fn write_tiff_file_basic() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut output = Cursor::new(Vec::new());
    let mut tiff = TiffWriter::new(&mut output)?;

    let mut dir = tiff.new_directory();

    let offset = {
      let mut dir2 = tiff.new_directory();
      dir2.add_tag(32_u16, 23_u16);
      dir2.build(&mut tiff)?
    };

    dir.add_tag(TiffCommonTag::ExifIFDPointer, offset);

    dir.add_tag(TiffCommonTag::ActiveArea, [9_u16, 10_u16, 11_u16, 12, 13, 14]);
    dir.add_tag(TiffCommonTag::BlackLevels, [9_u16, 10_u16]);
    dir.add_tag(TiffCommonTag::WhiteLevel, [11_u16]);
    dir.add_tag(TiffCommonTag::BitsPerSample, [12_u32]);
    dir.add_tag(TiffCommonTag::ResolutionUnit, [-5_i32]);
    dir.add_tag(TiffCommonTag::Artist, "AT");

    tiff.build(dir)?;

    //assert!(TiffReader::is_tiff(&mut output) == true);

    let mut garbage_output: Vec<u8> = vec![0x4a, 0xee]; // Garbage
    garbage_output.extend_from_slice(&output.into_inner());

    let mut garbage_output = Cursor::new(garbage_output);

    garbage_output.seek(SeekFrom::Start(2))?;

    let reader = GenericTiffReader::new(&mut garbage_output, 1, 1, Some(16), &[])?; // 1 byte offset correction

    assert_eq!(reader.root_ifd().entry_count(), 7);
    assert!(reader.root_ifd().get_entry(TiffCommonTag::WhiteLevel).is_some());

    assert!(matches!(
      reader.root_ifd().get_entry(TiffCommonTag::ExifIFDPointer).unwrap().value,
      Value::Long { .. }
    ));
    assert!(matches!(
      reader.root_ifd().get_entry(TiffCommonTag::WhiteLevel).unwrap().value,
      Value::Short { .. }
    ));

    Ok(())
  }
}
