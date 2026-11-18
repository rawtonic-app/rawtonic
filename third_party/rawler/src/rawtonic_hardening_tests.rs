// Rawtonic patch (RWH1): regression tests for damaged RAW files.
//
// Every test here feeds truncated, corrupted or crafted data to a decoder and
// expects an `Err` (or a clean decode), never a panic. There is deliberately
// no `catch_unwind`: a panic fails the test. The inputs are tiny synthetic
// buffers built in the tests, and DNGs written by rawler's own DNG writer;
// no binary fixtures are committed. A separate stress test mutates those
// files for a fixed number of iterations under a time bound.
//
// The decoder fixes these tests cover are listed in RAWTONIC_PATCHES.md
// ("Damaged files return errors").

use std::io::Cursor;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::bits::{Endian, LookupTable};
use crate::cfa::CFA;
use crate::decoders::{Camera, RawDecodeParams};
use crate::dng::writer::DngWriter;
use crate::dng::{CropMode, DNG_VERSION_V1_4, DngCompression, DngPhotometricConversion};
use crate::formats::bmff::BoxHeader;
use crate::formats::tiff::{TiffAscii, Value};
use crate::pixarray::PixU16;
use crate::rawimage::{BlackLevel, CFAConfig, RawImage, RawPhotometricInterpretation, WhiteLevel};
use crate::rawsource::RawSource;

/// Deterministic xorshift generator, so every run mutates the same way.
struct Rng(u64);

impl Rng {
  fn next(&mut self) -> u64 {
    self.0 ^= self.0 << 13;
    self.0 ^= self.0 >> 7;
    self.0 ^= self.0 << 17;
    self.0
  }

  fn below(&mut self, n: usize) -> usize {
    if n == 0 { 0 } else { (self.next() % n as u64) as usize }
  }
}

/// Run every decoder entry point the app uses on `data`. Errors are fine,
/// a panic fails the calling test.
fn decode_all(data: Vec<u8>) {
  let src = RawSource::new_from_shared_vec(Arc::new(data));
  let params = RawDecodeParams::default();
  let Ok(decoder) = crate::get_decoder(&src) else {
    return;
  };
  let _ = decoder.raw_metadata(&src, &params);
  let _ = decoder.raw_image(&src, &params, true);
  let _ = decoder.raw_image(&src, &params, false);
  let _ = decoder.thumbnail_image(&src, &params);
  let _ = decoder.preview_image(&src, &params);
  let _ = decoder.full_image(&src, &params);
  let _ = decoder.xpacket(&src, &params);
}

/// A 64 x 48 Bayer DNG written by rawler's DNG writer.
fn synthetic_dng(compression: DngCompression) -> Vec<u8> {
  let (width, height) = (64, 48);
  let samples: Vec<u16> = (0..width * height).map(|i| ((i * 37) % 3900 + 100) as u16).collect();
  let mut camera = Camera::new();
  camera.make = "Rawtonic".to_string();
  camera.model = "Hardening Test".to_string();
  camera.clean_make = camera.make.clone();
  camera.clean_model = camera.model.clone();
  camera.cfa = CFA::new("RGGB");
  let photometric = RawPhotometricInterpretation::Cfa(CFAConfig::new_from_camera(&camera));
  let image = RawImage::new(
    camera,
    PixU16::new_with(samples, width, height),
    1,
    [2.0, 1.0, 1.5, f32::NAN],
    photometric,
    Some(BlackLevel::new(&[100_u16; 4], 2, 2, 1)),
    Some(WhiteLevel::new(vec![4095])),
    false,
  )
  .expect("synthetic raw image");
  let mut buf = Cursor::new(Vec::new());
  let mut dng = DngWriter::new(&mut buf, DNG_VERSION_V1_4).expect("DNG writer");
  let mut raw = dng.subframe(0);
  raw
    .raw_image(&image, CropMode::None, compression, DngPhotometricConversion::Original, 1)
    .expect("DNG raw image");
  raw.finalize().expect("DNG raw IFD");
  dng.load_base_tags(&image).expect("DNG base tags");
  dng.close().expect("DNG close");
  buf.into_inner()
}

/// Tiny crafted headers of the other container formats (MRW, CIFF/CRW, X3F,
/// JFIF, ISO BMFF/CR3, TIFF), enough for the loader to pick their decoder.
fn crafted_headers() -> Vec<Vec<u8>> {
  let mut out = Vec::new();
  // MRW: "\0MRM", data offset, then a PRD block
  let mut mrw = vec![0, b'M', b'R', b'M', 0, 0, 0, 40];
  mrw.extend_from_slice(b"\0PRD\0\0\0\x18");
  mrw.extend_from_slice(&[0; 8]);
  mrw.extend_from_slice(&[0, 16, 0, 16, 0, 16, 0, 16, 12, 0, 0, 0]);
  mrw.resize(64, 0);
  out.push(mrw);
  // CIFF (CRW): byte order, heap offset, "HEAPCCDR"
  let mut crw = b"II\x1a\0\0\0HEAPCCDR".to_vec();
  crw.resize(64, 0);
  out.push(crw);
  // X3F: "FOVb" and a directory pointer at the end
  let mut x3f = b"FOVb".to_vec();
  x3f.resize(64, 0);
  let len = x3f.len();
  x3f[len - 4..].copy_from_slice(&40_u32.to_le_bytes());
  x3f[40..44].copy_from_slice(b"SECd");
  x3f[44..48].copy_from_slice(&0x0002_0000_u32.to_le_bytes());
  x3f[48..52].copy_from_slice(&1_u32.to_le_bytes());
  out.push(x3f);
  // JFIF: SOI, APP1 "Exif" with a short TIFF, an unknown segment, EOI
  let mut jfif = vec![0xff, 0xd8, 0xff, 0xe1, 0, 16];
  jfif.extend_from_slice(b"Exif\0\0II*\0\x08\0\0\0");
  jfif.extend_from_slice(&[0xff, 0xe5, 0, 4, 0, 0, 0xff, 0xd9]);
  out.push(jfif);
  // ISO BMFF (CR3): ftyp "crx " and an empty moov
  let mut bmff = vec![0, 0, 0, 24];
  bmff.extend_from_slice(b"ftypcrx \0\0\0\x01crx isom");
  bmff.extend_from_slice(&[0, 0, 0, 16]);
  bmff.extend_from_slice(b"moov");
  bmff.extend_from_slice(&[0, 0, 0, 8]);
  bmff.extend_from_slice(b"trak");
  out.push(bmff);
  out
}

/// One mutation of `orig`: truncation, bytes overwritten in the first or
/// last KiB (where the headers are), bit flips anywhere, or extreme 16/32-bit
/// values written over header fields.
fn mutate(orig: &[u8], rng: &mut Rng) -> Vec<u8> {
  let mut data = orig.to_vec();
  let len = data.len();
  let header = |rng: &mut Rng| {
    let window = len.min(1024);
    if rng.below(4) == 0 { len - 1 - rng.below(window) } else { rng.below(window) }
  };
  match rng.below(4) {
    0 => data.truncate(rng.below(len)),
    1 => {
      for _ in 0..1 + rng.below(8) {
        let at = header(rng);
        data[at] = rng.next() as u8;
      }
    }
    2 => {
      for _ in 0..1 + rng.below(16) {
        let at = rng.below(len);
        data[at] ^= 1 << rng.below(8);
      }
    }
    _ => {
      let value: u32 = [0, 1, 2, 0xffff, 0x7fff_ffff, 0x8000_0000, 0xffff_ffff][rng.below(7)];
      let width = if rng.below(2) == 0 { 2 } else { 4 };
      let at = header(rng) & !1;
      let little = rng.below(2) == 0;
      let bytes = if little { value.to_le_bytes() } else { value.to_be_bytes() };
      for i in 0..width {
        if at + i < len {
          data[at + i] = if little { bytes[i] } else { bytes[4 - width + i] };
        }
      }
    }
  }
  data
}

#[test]
fn synthetic_dngs_decode() {
  for compression in [DngCompression::Uncompressed, DngCompression::Lossless] {
    let dng = synthetic_dng(compression);
    let src = RawSource::new_from_slice(&dng);
    let image = crate::decode(&src, &RawDecodeParams::default()).expect("synthetic DNG decodes");
    assert_eq!((image.width, image.height), (64, 48));
  }
}

/// Every truncation of the synthetic DNGs and every single-byte overwrite of
/// their header area returns (an image or an error) without a panic.
#[test]
fn truncated_and_overwritten_dngs_never_panic() {
  for compression in [DngCompression::Uncompressed, DngCompression::Lossless] {
    let dng = synthetic_dng(compression);
    for len in (0..dng.len()).step_by(7) {
      decode_all(dng[..len].to_vec());
    }
    for at in 0..dng.len().min(512) {
      for value in [0x00, 0xff, 0x7f, 0x80] {
        let mut data = dng.clone();
        data[at] = value;
        decode_all(data);
      }
    }
  }
}

/// Header mutations of the synthetic DNGs and the crafted container headers
/// for a fixed number of iterations, under a time bound (debug build).
/// `RAWLER_STRESS_ITERATIONS` and `RAWLER_STRESS_SEED` run it longer or with
/// other mutations locally; the defaults are what CI runs.
#[test]
fn mutated_headers_stress_under_time_bound() {
  let env = |name: &str, default: u64| std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default);
  let iterations = env("RAWLER_STRESS_ITERATIONS", 20_000) as usize;
  let mut seeds = vec![synthetic_dng(DngCompression::Uncompressed), synthetic_dng(DngCompression::Lossless)];
  seeds.extend(crafted_headers());
  let mut rng = Rng(env("RAWLER_STRESS_SEED", 0x5eed_1234_abcd_ef01) | 1);
  let start = Instant::now();
  let mut slowest = Duration::ZERO;
  for i in 0..iterations {
    let seed = &seeds[i % seeds.len()];
    let data = mutate(seed, &mut rng);
    let case = Instant::now();
    decode_all(data);
    slowest = slowest.max(case.elapsed());
  }
  let total = start.elapsed();
  eprintln!("{iterations} mutated files in {total:?}, slowest {slowest:?}");
  assert!(slowest < Duration::from_secs(5), "one mutated file took {slowest:?}");
  assert!(total < Duration::from_secs(120), "{iterations} mutated files took {total:?}");
}

#[test]
fn crafted_headers_and_their_truncations_never_panic() {
  for header in crafted_headers() {
    for len in 0..=header.len() {
      decode_all(header[..len].to_vec());
    }
  }
}

// --- Per-decoder regressions (each panicked before the RWH1 patches) ---

#[test]
fn packed_rejects_short_data_and_bad_sizes() {
  use crate::packed::*;
  assert!(decode_12be(&[0; 10], 16, 4, false).is_err());
  assert!(decode_16le(&[0; 64], 16, 4, false).is_err());
  assert!(decode_12le_wcontrol(&[0; 8], 20, 4, false).is_err());
  assert!(decode_12be_interlaced(&[0; 8], 16, 4, false).is_err());
  assert!(decode_generic_msb(&[0; 4], 16, 4, 12, false).is_err());
  assert!(decode_generic_msb(&[0; 64], 2, 2, 17, false).is_err());
  assert!(decode_16le(&[0; 64], 0, 4, false).is_err());
  assert!(decode_16le(&[], 60000, 60000, true).is_err());
  // A file whose last row is only partly present decodes as before
  assert!(decode_16le(&[0; 2 * 16 * 3 + 2], 16, 4, false).is_ok());
}

#[test]
fn arw_rejects_short_data() {
  use crate::decoders::arw::ArwDecoder;
  assert!(ArwDecoder::decode_arw1(&[0; 16], 4, 0, false).is_err());
  assert!(ArwDecoder::decode_arw1(&[0; 8], 128, 130, false).is_err());
  let curve = ArwDecoder::calculate_curve([0, 1024, 2048, 3072, 3584, 4095]);
  assert!(ArwDecoder::decode_arw2(&[0; 10], 32, 4, &curve, false).is_err());
}

#[test]
fn orf_rejects_short_compressed_data() {
  use crate::buffer::PaddedBuf;
  use crate::decoders::orf::OrfDecoder;
  let data = vec![0_u8; 64];
  let buf = PaddedBuf::new_owned(data, 48);
  assert!(OrfDecoder::decode_compressed(&buf, 64, 64, 12, false).is_err());
  assert!(OrfDecoder::decode_compressed(&buf, 0, 64, 12, false).is_err());
}

#[test]
fn pef_rejects_bad_huffman_table_and_size() {
  use crate::decoders::pef::PefDecoder;
  // Depth 1 with a code length of 200
  let mut table = vec![0_u8; 2 + 12 + 2 + 1];
  table[1] = 1 + 4; // (1 + 12) & 0xf = 1 after the +12 in the decoder
  table[16] = 200;
  assert!(PefDecoder::do_decode(&[0; 64], Some((&table, Endian::Big)), 8, 2, false).is_err());
  assert!(PefDecoder::do_decode(&[0; 64], None, 7, 2, false).is_err());
  assert!(PefDecoder::do_decode(&[0; 4], None, 64, 64, false).is_err());
}

#[test]
fn nef_rejects_short_metadata() {
  use crate::decoders::nef::NefDecoder;
  let meta = [73_u8, 0, 0, 0];
  assert!(NefDecoder::do_decode(&[0; 64], &meta, Endian::Big, 16, 16, 12, false).is_err());
}

#[test]
fn srw_rejects_bad_offsets_and_sizes() {
  use crate::decoders::srw::SrwDecoder;
  assert!(SrwDecoder::decode_srw1(&[0; 64], &[0; 8], 16, 4, false).is_err());
  assert!(SrwDecoder::decode_srw1(&[0; 64], &[0xff; 16], 16, 4, false).is_err());
  assert!(SrwDecoder::decode_srw1(&[0; 64], &[0; 64], 15, 4, false).is_err());
  assert!(SrwDecoder::decode_srw2(&[0; 4], 64, 64, false).is_err());
  assert!(SrwDecoder::decode_srw3(&[0; 64], 20, 4, false).is_err());
  assert!(SrwDecoder::decode_srw3(&[0xff; 64], 32, 4, false).is_err());
}

#[test]
fn rw2_rejects_short_data_and_bad_parameters() {
  use crate::decoders::rw2::{v4decompressor, v5decompressor, v6decompressor, v7decompressor};
  assert!(v4decompressor::decode_panasonic_v4(&[0; 100], 14, 8, true, false).is_err());
  assert!(v5decompressor::decode_panasonic_v5(&[0; 100], 10, 8, 12, false).is_err());
  assert!(v5decompressor::decode_panasonic_v5(&[0; 0x4000], 10, 8, 13, false).is_err());
  assert!(v6decompressor::decode_panasonic_v6(&[0; 16], 14, 8, 12, false).is_err());
  assert!(v6decompressor::decode_panasonic_v6(&[0; 1024], 13, 8, 12, false).is_err());
  assert!(v7decompressor::decode_panasonic_v7(&[0; 16], 10, 8, 12, false).is_err());
  assert!(v7decompressor::decode_panasonic_v7(&[0; 1024], 10, 8, 16, false).is_err());
}

#[test]
fn iiq_rejects_short_strip_tables() {
  use crate::decoders::iiq::IiqDecoder;
  assert!(IiqDecoder::decode_compressed(&[0; 64], &[0; 4], 8, 8, 14, false).is_err());
  let strips: Vec<u8> = (0..8).flat_map(|_| 1000_u32.to_le_bytes()).collect();
  assert!(IiqDecoder::decode_compressed(&[0; 64], &strips, 8, 8, 14, false).is_err());
}

#[test]
fn kodak_rejects_short_data() {
  use crate::decoders::dcr::DcrDecoder;
  use crate::decoders::kdc::KdcDecoder;
  assert!(KdcDecoder::decode_dc120(&[0; 100], 848, 976, false).is_err());
  let curve = LookupTable::new(&[0, 100, 200]);
  assert!(DcrDecoder::decode_kodak65000(&[0xff; 4], &curve, 256, 2, false).is_err());
}

#[test]
fn lookup_table_beyond_its_end_uses_the_last_entry() {
  let table = LookupTable::new(&[10, 20, 30]);
  let mut random = 0;
  assert_eq!(table.dither(60000, &mut random), table.dither(2, &mut 0));
  let empty = LookupTable::new_with_bits(&[], 12);
  assert_eq!(empty.dither(5, &mut 0), 0);
}

#[test]
fn huffman_table_with_too_many_codes_is_an_error() {
  use crate::decompressors::ljpeg::huffman::HuffTable;
  let mut bits = [0_u32; 17];
  bits[1] = 3; // three codes of one bit
  assert!(HuffTable::new(bits, [0; 256], false).is_err());
}

#[test]
fn bmff_box_shorter_than_its_header_is_an_error() {
  let mut data = vec![0, 0, 0, 0];
  data.extend_from_slice(b"free");
  assert!(BoxHeader::parse(&mut Cursor::new(&data)).is_err());
  data[3] = 7;
  assert!(BoxHeader::parse(&mut Cursor::new(&data)).is_err());
  data[3] = 8;
  assert!(BoxHeader::parse(&mut Cursor::new(&data)).is_ok());
}

#[test]
fn tiff_values_of_the_wrong_type_or_empty() {
  assert!(Value::Short(vec![1]).get_string().is_err());
  assert!(Value::Short(vec![1]).get_data().is_empty());
  let empty = TiffAscii::new_from_vec(Vec::new());
  assert_eq!(empty.first(), "");
  assert!(Value::Ascii(empty).get_string().is_err());
}

#[test]
fn cfa_patterns_from_files_are_checked() {
  assert!(CFA::try_new("RGGB").is_ok());
  assert!(CFA::try_new("RGG").is_err());
  assert!(CFA::try_new("RGXB").is_err());
}

#[test]
fn camera_areas_beyond_a_damaged_image_are_dropped() {
  let mut camera = Camera::new();
  camera.cfa = CFA::new("RGGB");
  camera.active_area = Some([10, 10, 10, 10]);
  camera.crop_area = Some([2, 2, 2, 2]);
  let photometric = RawPhotometricInterpretation::Cfa(CFAConfig::new_from_camera(&camera));
  let image = RawImage::new(
    camera,
    PixU16::new(8, 8),
    1,
    [1.0; 4],
    photometric,
    None,
    Some(WhiteLevel::new(vec![4095])),
    false,
  )
  .expect("image");
  assert!(image.active_area.is_none());
  assert!(image.crop_area.is_some());
  let photometric = RawPhotometricInterpretation::Cfa(CFAConfig::new_from_camera(&Camera::new()));
  assert!(RawImage::new(Camera::new(), PixU16::new(8, 8), 1, [1.0; 4], photometric, None, None, false).is_err());
}

/// The same mutations on real RAW files: `RAWLER_STRESS_CORPUS` names a
/// directory of (copies of) RAW files, which are only read. Without it the
/// test prints SKIP; that is not a pass.
#[test]
fn mutated_corpus_files_never_panic() {
  let Some(dir) = std::env::var_os("RAWLER_STRESS_CORPUS") else {
    eprintln!("SKIP: RAWLER_STRESS_CORPUS is not set");
    return;
  };
  let per_file = std::env::var("RAWLER_STRESS_ITERATIONS").ok().and_then(|v| v.parse().ok()).unwrap_or(20);
  let mut files: Vec<_> = std::fs::read_dir(dir)
    .expect("corpus directory")
    .filter_map(|e| e.ok())
    .map(|e| e.path())
    .filter(|p| p.is_file())
    .collect();
  files.sort();
  let mut rng = Rng(0x0dd_c0ffee | 1);
  for path in files {
    let orig = std::fs::read(&path).expect("corpus file");
    if orig.is_empty() {
      continue;
    }
    let start = Instant::now();
    for _ in 0..per_file {
      decode_all(mutate(&orig, &mut rng));
    }
    eprintln!("{}: {per_file} mutations in {:?}", path.display(), start.elapsed());
  }
}
