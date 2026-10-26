//! Decode fingerprints: the facts a RAW file decodes to, as plain values, so
//! a rawler update or a change to the decode path shows up as a changed
//! field instead of a silently different photo (`rawtonic fingerprint`,
//! docs/testing/README.md). The facts come from the same preparation the
//! development uses (`raw_processing::prepare_raw_image`); the raw data hash
//! is of the decoder's output before any of Rawtonic's corrections.
//!
//! Everything except `developed` is exact integer or rational data, or
//! floats computed the same way on every machine; the developed image's hash
//! may differ between CPU architectures.
use crate::raw_processing::{Prepared, prepare_raw_image};
use rawler::{
    decoders::{RawDecodeParams, WellKnownIFD},
    rawimage::{RawImage, RawPhotometricInterpretation},
    rawsource::RawSource,
};
use serde::Serialize;
use sha2::Digest;
use std::collections::BTreeMap;

/// The camera library's own name of a body (clean make, base model) for
/// the make and model a file's metadata gives, if its camera list has that
/// body; None for any other text. Usage statistics send only such names:
/// a file's metadata can be edited to say anything (an owner's name, a
/// serial), the camera list cannot.
pub fn known_camera(make: &str, model: &str) -> Option<(String, String)> {
    let clean = |text: &str| text.trim().trim_matches('"').trim().to_string();
    let (make, model) = (clean(make), clean(model));
    let cameras = rawler::global_loader().get_cameras();
    // The file's own names (`Canon`, `Canon EOS R6`) or the list's clean
    // ones (`Canon`, `EOS R6`), which the metadata reader gives for some
    // formats.
    let camera = cameras
        .get(&(make.clone(), model.clone(), String::new()))
        .or_else(|| {
            cameras
                .iter()
                .find(|((m, n, _), camera)| {
                    (*m == make || camera.clean_make == make)
                        && (*n == model || camera.clean_model == model)
                })
                .map(|(_, camera)| camera)
        })?;
    let model = if camera.canonical_model.is_empty() {
        &camera.clean_model
    } else {
        &camera.canonical_model
    };
    Some((camera.clean_make.clone(), model.clone()))
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct DecodeFingerprint {
    /// The step that failed: `decoder` (no decoder accepts the file),
    /// `unpack`, `prepare` (metadata, colour) or `develop`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failed: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Make and model from the file's metadata when a later step failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub camera: Option<String>,
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    pub facts: Option<DecodeFacts>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub developed: Option<Developed>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct DecodeFacts {
    /// rawler's format (`ARW`, `DNG`, …).
    pub format: String,
    /// TIFF Compression of the raw IFD, where the format has one.
    pub compression: Option<u32>,
    pub make: String,
    pub model: String,
    pub clean_make: String,
    pub clean_model: String,
    /// The base model of a regional alias (the name per-model tables use).
    pub canonical_model: String,
    /// rawler's camera mode (bit depth, crop or compression variant).
    pub mode: String,
    /// The decoded buffer, width × height.
    pub raw_size: [usize; 2],
    pub cpp: usize,
    pub bps: usize,
    /// `integer` (u16) or `float` samples.
    pub samples: &'static str,
    /// `cfa`, `linear_raw` or `black_is_zero`.
    pub photometric: &'static str,
    /// The colour pattern at the buffer origin (`RGGB`, 6 × 6 X-Trans…).
    pub cfa: Option<String>,
    /// Left, top, width, height in the buffer.
    pub active_area: Option<[usize; 4]>,
    /// The default crop (incl. in-camera aspect ratios and DNG user crops).
    pub crop_area: Option<[usize; 4]>,
    pub black: BlackLevels,
    pub white: Vec<u32>,
    /// The decoder's white balance (RGBE multipliers), `null` where unset.
    pub as_shot_wb: [Option<f32>; 4],
    /// The multipliers the development uses.
    pub wb: [Option<f32>; 4],
    /// The decoder's XYZ → camera matrices per illuminant.
    pub camera_matrices: BTreeMap<String, Vec<f32>>,
    /// The matrix the development uses (selected and adapted to `wb`).
    pub color_matrix: BTreeMap<String, Vec<f32>>,
    /// BaselineExposure + BaselineExposureOffset of a DNG, in EV.
    pub dng_baseline_exposure: f32,
    /// EXIF orientation value (1–8) the development applies.
    pub orientation: u16,
    pub iso: Option<u32>,
    pub lens: Lens,
    /// The DNG has a ProfileGainTableMap the development applies.
    pub gain_table_map: bool,
    /// SHA-256 of the decoder's samples (whole buffer, little-endian).
    pub raw_sha256: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct BlackLevels {
    /// Repeat pattern width, height and samples per pixel.
    pub repeat: [usize; 3],
    /// Exact rationals (`n/d`).
    pub levels: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Lens {
    pub make: Option<String>,
    pub model: Option<String>,
    /// Min/max focal length and their maximum apertures (`n/d`).
    pub spec: Option<[String; 4]>,
    pub focal_length: Option<String>,
    pub fnumber: Option<String>,
    /// The lens rawler identified from its database, `make model`.
    pub identified: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Developed {
    /// Oriented size, width × height.
    pub size: [u32; 2],
    /// Full-resolution development at default settings (as `rawtonic
    /// decode`): SHA-256 of the f32 RGB samples.
    pub sha256: String,
    /// Mean linear R, G, B, rounded to 1e-6, to tell the size of a change.
    pub mean: [f64; 3],
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex(&sha2::Sha256::digest(bytes))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The fingerprint of a RAW file's bytes; `develop` also develops it at
/// full resolution and hashes the result. Failures (and panics in the
/// decoder) are reported in the fingerprint, not as an error.
pub fn fingerprint(file_bytes: &[u8], develop: bool) -> DecodeFingerprint {
    let mut result = DecodeFingerprint {
        failed: None,
        error: None,
        camera: None,
        facts: None,
        developed: None,
    };
    let mut step = "decoder";
    let mut camera = None;
    let described = catch(|| describe(file_bytes, &mut step, &mut camera));
    match described {
        Ok(facts) => result.facts = Some(facts),
        Err(error) => {
            result.failed = Some(step);
            result.error = Some(error);
            result.camera = camera;
            return result;
        }
    }
    if develop {
        match catch(|| develop_hash(file_bytes)) {
            Ok(developed) => result.developed = Some(developed),
            Err(error) => {
                result.failed = Some("develop");
                result.error = Some(error);
            }
        }
    }
    result
}

/// `f`'s result, with its error or panic as text.
fn catch<T>(f: impl FnOnce() -> anyhow::Result<T>) -> Result<T, String> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(e)) => Err(format!("{e:#}")),
        Err(panic) => Err(format!("panic: {}", panic_message(&panic))),
    }
}

fn panic_message(panic: &Box<dyn std::any::Any + Send>) -> String {
    panic
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown".into())
}

fn describe(
    file_bytes: &[u8],
    step: &mut &'static str,
    camera: &mut Option<String>,
) -> anyhow::Result<DecodeFacts> {
    let source = RawSource::new_from_slice(file_bytes);
    *step = "decoder";
    let decoder = rawler::get_decoder(&source)?;
    *step = "unpack";
    *camera = catch(|| Ok(decoder.raw_metadata(&source, &RawDecodeParams::default())?))
        .ok()
        .map(|m| format!("{} {}", m.make, m.model).trim().to_string())
        .filter(|name| !name.is_empty());
    let mut raw_image = decoder.raw_image(&source, &RawDecodeParams::default(), false)?;
    let raw_sha256 = data_hash(&raw_image);
    let as_shot_wb = finite(raw_image.wb_coeffs);
    let camera_matrices = matrices(&raw_image);
    *step = "prepare";
    let Prepared {
        metadata,
        orientation,
        iso,
        gain_table_map_bytes,
    } = prepare_raw_image(decoder.as_ref(), &source, &mut raw_image, file_bytes, 0)?;
    if crate::raw_processing::is_canon_sraw(&raw_image, file_bytes) {
        raw_image.blacklevel = rawler::rawimage::BlackLevel::zero(1, 1, 3);
    }
    let compression = decoder
        .ifd(WellKnownIFD::Raw)
        .ok()
        .flatten()
        .and_then(|ifd| {
            ifd.get_entry(rawler::tags::TiffCommonTag::Compression)
                .map(|e| e.value.force_u32(0))
        });
    let rect = |r: rawler::imgop::Rect| [r.p.x, r.p.y, r.d.w, r.d.h];
    let rational = |r: &rawler::formats::tiff::Rational| format!("{}/{}", r.n, r.d);
    let exif = &metadata.exif;
    Ok(DecodeFacts {
        format: format!("{:?}", decoder.format_hint()),
        compression,
        make: raw_image.make.clone(),
        model: raw_image.model.clone(),
        clean_make: raw_image.clean_make.clone(),
        clean_model: raw_image.clean_model.clone(),
        canonical_model: metadata.canonical_model.clone(),
        mode: raw_image.camera.mode.clone(),
        raw_size: [raw_image.width, raw_image.height],
        cpp: raw_image.cpp,
        bps: raw_image.bps,
        samples: match raw_image.data {
            rawler::RawImageData::Integer(_) => "integer",
            rawler::RawImageData::Float(_) => "float",
        },
        photometric: match raw_image.photometric {
            RawPhotometricInterpretation::Cfa(_) => "cfa",
            RawPhotometricInterpretation::LinearRaw => "linear_raw",
            RawPhotometricInterpretation::BlackIsZero => "black_is_zero",
        },
        cfa: match &raw_image.photometric {
            RawPhotometricInterpretation::Cfa(config) => Some(config.cfa.name.clone()),
            _ => None,
        },
        active_area: raw_image.active_area.map(rect),
        crop_area: raw_image.crop_area.map(rect),
        black: BlackLevels {
            repeat: [
                raw_image.blacklevel.width,
                raw_image.blacklevel.height,
                raw_image.blacklevel.cpp,
            ],
            levels: raw_image.blacklevel.levels.iter().map(rational).collect(),
        },
        white: raw_image.whitelevel.0.clone(),
        as_shot_wb,
        wb: finite(raw_image.wb_coeffs),
        camera_matrices,
        color_matrix: matrices(&raw_image),
        dng_baseline_exposure: crate::raw_processing::dng_baseline_exposure(decoder.as_ref()),
        orientation: orientation.to_u16(),
        iso,
        lens: Lens {
            make: exif.lens_make.clone(),
            model: exif.lens_model.clone(),
            spec: exif.lens_spec.as_ref().map(|s| s.each_ref().map(rational)),
            focal_length: exif.focal_length.as_ref().map(rational),
            fnumber: exif.fnumber.as_ref().map(rational),
            identified: metadata
                .lens
                .as_ref()
                .map(|l| format!("{} {}", l.lens_make, l.lens_model)),
        },
        gain_table_map: gain_table_map_bytes.is_some(),
        raw_sha256,
    })
}

fn finite(wb: [f32; 4]) -> [Option<f32>; 4] {
    wb.map(|v| v.is_finite().then_some(v))
}

fn matrices(raw_image: &RawImage) -> BTreeMap<String, Vec<f32>> {
    raw_image
        .color_matrix
        .iter()
        .map(|(illuminant, matrix)| (format!("{illuminant:?}"), matrix.clone()))
        .collect()
}

fn data_hash(raw_image: &RawImage) -> String {
    let mut hash = sha2::Sha256::new();
    match &raw_image.data {
        rawler::RawImageData::Integer(samples) => {
            for chunk in samples.chunks(1 << 16) {
                let bytes: Vec<u8> = chunk.iter().flat_map(|v| v.to_le_bytes()).collect();
                hash.update(&bytes);
            }
        }
        rawler::RawImageData::Float(samples) => {
            for chunk in samples.chunks(1 << 16) {
                let bytes: Vec<u8> = chunk.iter().flat_map(|v| v.to_le_bytes()).collect();
                hash.update(&bytes);
            }
        }
    }
    hex(&hash.finalize())
}

fn develop_hash(file_bytes: &[u8]) -> anyhow::Result<Developed> {
    let image =
        crate::raw_processing::develop_raw_image(file_bytes, false, 2.5, "auto".into(), None)?
            .into_rgb32f();
    let mut hash = sha2::Sha256::new();
    let mut sums = [0.0f64; 3];
    for chunk in image.as_raw().chunks(3 << 15) {
        let bytes: Vec<u8> = chunk.iter().flat_map(|v| v.to_le_bytes()).collect();
        hash.update(&bytes);
        for pixel in chunk.as_chunks::<3>().0 {
            for (sum, v) in sums.iter_mut().zip(pixel) {
                *sum += *v as f64;
            }
        }
    }
    let count = (image.width() as f64 * image.height() as f64).max(1.0);
    Ok(Developed {
        size: [image.width(), image.height()],
        sha256: hex(&hash.finalize()),
        mean: sums.map(|s| (s / count * 1e6).round() / 1e6),
    })
}

/// A synthetic DNG for tests here and in other crates (`test-support`).
#[cfg(any(test, feature = "test-support"))]
pub mod synthetic {
    pub const WIDTH: u32 = 64;
    pub const HEIGHT: u32 = 48;

    pub fn sample(x: u32, y: u32) -> u16 {
        (256 + (x * 37 + y * 101) % 3800) as u16
    }

    /// A minimal uncompressed 12-bit RGGB DNG (little-endian TIFF), written
    /// here so the test needs no photograph.
    pub fn synthetic_dng() -> Vec<u8> {
        let samples: Vec<u16> = (0..HEIGHT)
            .flat_map(|y| (0..WIDTH).map(move |x| sample(x, y)))
            .collect();
        mosaic_dng(WIDTH, HEIGHT, (2, 2), &[0, 1, 1, 2], &samples)
    }

    /// An uncompressed 12-bit mosaic DNG of `samples` (row-major, black 256,
    /// white 4095) with the CFA pattern `cfa` (0 red, 1 green, 2 blue) of
    /// `cfa_size` (columns, rows): 2 x 2 Bayer or 6 x 6 X-Trans. The
    /// benchmarks' fallback when no RAW fixtures are present.
    pub fn mosaic_dng(
        width: u32,
        height: u32,
        cfa_size: (u16, u16),
        cfa: &[u8],
        samples: &[u16],
    ) -> Vec<u8> {
        assert_eq!(samples.len(), (width * height) as usize);
        assert_eq!(cfa.len(), cfa_size.0 as usize * cfa_size.1 as usize);
        enum V {
            Short(Vec<u16>),
            Long(Vec<u32>),
            Byte(Vec<u8>),
            Ascii(&'static str),
            Rational(Vec<(u32, u32)>),
            SRational(Vec<(i32, i32)>),
        }
        let pixels: Vec<u8> = samples.iter().flat_map(|v| v.to_le_bytes()).collect();
        let matrix = [
            (8000, 10000),
            (-2500, 10000),
            (-800, 10000),
            (-4000, 10000),
            (12000, 10000),
            (1800, 10000),
            (-500, 10000),
            (1500, 10000),
            (6000, 10000),
        ];
        let mut tags: Vec<(u16, V)> = vec![
            (254, V::Long(vec![0])),
            (256, V::Long(vec![width])),
            (257, V::Long(vec![height])),
            (258, V::Short(vec![16])),
            (259, V::Short(vec![1])),
            (262, V::Short(vec![32803])),
            (271, V::Ascii("Rawtonic")),
            (272, V::Ascii("Synthetic Mosaic")),
            (273, V::Long(vec![0])),
            (274, V::Short(vec![6])),
            (277, V::Short(vec![1])),
            (278, V::Long(vec![height])),
            (279, V::Long(vec![pixels.len() as u32])),
            (284, V::Short(vec![1])),
            (33421, V::Short(vec![cfa_size.1, cfa_size.0])),
            (33422, V::Byte(cfa.to_vec())),
            (50706, V::Byte(vec![1, 4, 0, 0])),
            (50708, V::Ascii("Rawtonic Synthetic Mosaic")),
            (50714, V::Short(vec![256])),
            (50717, V::Short(vec![4095])),
            (50721, V::SRational(matrix.to_vec())),
            (50728, V::Rational(vec![(1, 2), (1, 1), (2, 3)])),
            (50730, V::SRational(vec![(1, 2)])),
            (50778, V::Short(vec![21])),
        ];
        tags.sort_by_key(|(tag, _)| *tag);
        let ifd_size = 2 + tags.len() * 12 + 4;
        let mut extra = Vec::new();
        let extra_start = 8 + ifd_size;
        let data_start = |extra: &Vec<u8>| extra_start + extra.len();
        let mut entries = Vec::new();
        for (tag, value) in &tags {
            let (kind, count, bytes): (u16, usize, Vec<u8>) = match value {
                V::Short(v) => (3, v.len(), v.iter().flat_map(|x| x.to_le_bytes()).collect()),
                V::Long(v) => (4, v.len(), v.iter().flat_map(|x| x.to_le_bytes()).collect()),
                V::Byte(v) => (1, v.len(), v.clone()),
                V::Ascii(s) => {
                    let mut b = s.as_bytes().to_vec();
                    b.push(0);
                    (2, b.len(), b)
                }
                V::Rational(v) => (
                    5,
                    v.len(),
                    v.iter()
                        .flat_map(|(n, d)| [n.to_le_bytes(), d.to_le_bytes()].concat())
                        .collect(),
                ),
                V::SRational(v) => (
                    10,
                    v.len(),
                    v.iter()
                        .flat_map(|(n, d)| [n.to_le_bytes(), d.to_le_bytes()].concat())
                        .collect(),
                ),
            };
            let mut entry = Vec::new();
            entry.extend(tag.to_le_bytes());
            entry.extend(kind.to_le_bytes());
            entry.extend((count as u32).to_le_bytes());
            if bytes.len() <= 4 {
                let mut inline = bytes.clone();
                inline.resize(4, 0);
                entry.extend(inline);
            } else {
                entry.extend((data_start(&extra) as u32).to_le_bytes());
                extra.extend(&bytes);
                if extra.len() % 2 == 1 {
                    extra.push(0);
                }
            }
            entries.push((*tag, entry));
        }
        let strip_offset = (extra_start + extra.len()) as u32;
        let mut out = b"II*\0".to_vec();
        out.extend(8u32.to_le_bytes());
        out.extend((tags.len() as u16).to_le_bytes());
        for (tag, mut entry) in entries {
            if tag == 273 {
                entry[8..12].copy_from_slice(&strip_offset.to_le_bytes());
            }
            out.extend(entry);
        }
        out.extend(0u32.to_le_bytes());
        out.extend(extra);
        out.extend(pixels);
        out
    }
}

#[cfg(test)]
mod known_camera_tests {
    #[test]
    fn names_only_bodies_of_the_camera_list() {
        assert_eq!(
            super::known_camera("SONY", "ILCE-7M3"),
            Some(("Sony".to_string(), "ILCE-7M3".to_string()))
        );
        assert_eq!(
            super::known_camera(" \"SONY\" ", "\"ILCE-7M3\""),
            Some(("Sony".to_string(), "ILCE-7M3".to_string()))
        );
        let canon = Some(("Canon".to_string(), "EOS R6".to_string()));
        assert_eq!(super::known_camera("Canon", "Canon EOS R6"), canon);
        assert_eq!(super::known_camera("Canon", "EOS R6"), canon);
        // Metadata edited to carry a person or a serial number.
        assert_eq!(super::known_camera("SONY", "Anna Schmidt's camera"), None);
        assert_eq!(super::known_camera("SONY", "ILCE-7M3 SN 5012345"), None);
        assert_eq!(super::known_camera("Anna Schmidt", "ILCE-7M3"), None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use super::synthetic::*;

    #[test]
    fn synthetic_dng_facts_are_pinned() {
        let bytes = synthetic_dng();
        let fp = fingerprint(&bytes, false);
        assert_eq!(fp.failed, None, "{:?}", fp.error);
        let facts = fp.facts.expect("facts");
        let expected_samples: Vec<u8> = (0..HEIGHT)
            .flat_map(|y| (0..WIDTH).flat_map(move |x| sample(x, y).to_le_bytes()))
            .collect();
        assert_eq!(facts.raw_sha256, sha256_hex(&expected_samples));
        assert_eq!(facts.format, "DNG");
        assert_eq!(facts.compression, Some(1));
        assert_eq!(facts.make, "Rawtonic");
        assert_eq!(facts.model, "Synthetic Mosaic");
        assert_eq!(facts.raw_size, [64, 48]);
        assert_eq!(
            (facts.cpp, facts.samples, facts.photometric),
            (1, "integer", "cfa")
        );
        assert_eq!(facts.cfa.as_deref(), Some("RGGB"));
        assert_eq!(facts.white, vec![4095]);
        assert_eq!(facts.black.levels, vec!["256/1".to_string()]);
        assert_eq!(facts.orientation, 6);
        assert_eq!(facts.dng_baseline_exposure, 0.5);
        // AsShotNeutral 0.5, 1, 0.667 → multipliers 2, 1, 1.5; the
        // development keeps the smallest at 1.
        assert_eq!(&facts.as_shot_wb[..3], &[Some(2.0), Some(1.0), Some(1.5)]);
        assert_eq!(&facts.wb[..3], &[Some(2.0), Some(1.0), Some(1.5)]);
        assert_eq!(facts.camera_matrices.len(), 1);
        assert!(facts.camera_matrices.values().next().unwrap()[0] == 0.8);
        assert!(!facts.gain_table_map);
    }

    #[test]
    fn mosaic_dng_keeps_an_xtrans_pattern() {
        // The benchmarks' X-Trans stand-in (crates/rawdecode/benches).
        let cfa = [
            1, 1, 0, 1, 1, 2, 1, 1, 2, 1, 1, 0, 2, 0, 1, 0, 2, 1, //
            1, 1, 2, 1, 1, 0, 1, 1, 0, 1, 1, 2, 0, 2, 1, 2, 0, 1,
        ];
        let samples: Vec<u16> = (0..96 * 72).map(|i| 300 + (i * 7 % 3000) as u16).collect();
        let fp = fingerprint(&mosaic_dng(96, 72, (6, 6), &cfa, &samples), true);
        assert_eq!(fp.failed, None, "{:?}", fp.error);
        let facts = fp.facts.expect("facts");
        assert_eq!(facts.raw_size, [96, 72]);
        assert_eq!(facts.cfa.as_deref().map(str::len), Some(36));
        assert_eq!(fp.developed.expect("developed").size, [72, 96]);
    }

    #[test]
    fn development_hash_is_repeatable_and_oriented() {
        let bytes = synthetic_dng();
        let a = fingerprint(&bytes, true);
        let b = fingerprint(&bytes, true);
        assert_eq!(a.failed, None, "{:?}", a.error);
        let developed = a.developed.clone().expect("developed");
        // Orientation 6 turns the 64 × 48 frame upright.
        assert_eq!(developed.size, [48, 64]);
        assert!(developed.mean.iter().all(|m| m.is_finite() && *m > 0.0));
        assert_eq!(a, b);
    }

    #[test]
    fn changed_samples_change_only_the_data_hashes() {
        let bytes = synthetic_dng();
        let mut changed = bytes.clone();
        let last = changed.len() - 2;
        changed[last] ^= 1;
        let (a, b) = (fingerprint(&bytes, false), fingerprint(&changed, false));
        let (mut fa, fb) = (a.facts.unwrap(), b.facts.unwrap());
        assert_ne!(fa.raw_sha256, fb.raw_sha256);
        fa.raw_sha256 = fb.raw_sha256.clone();
        assert_eq!(fa, fb);
    }

    #[test]
    fn failures_name_their_step() {
        let fp = fingerprint(b"not a raw file at all", true);
        assert_eq!(fp.failed, Some("decoder"));
        assert!(fp.facts.is_none() && fp.error.is_some());
        // Strip data cut off: the decoder accepts the header, unpacking fails.
        let mut truncated = synthetic_dng();
        truncated.truncate(truncated.len() - 1000);
        let fp = fingerprint(&truncated, true);
        assert_eq!(fp.failed, Some("unpack"), "{:?}", fp.error);
        assert_eq!(fp.camera.as_deref(), Some("Rawtonic Synthetic Mosaic"));
    }
}
