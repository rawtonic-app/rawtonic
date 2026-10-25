//! Capture-mode details that change Adobe's baseline exposure for a file.
//!
//! Adobe DNG Converter and Camera Raw do not use one BaselineExposure per
//! camera. Probing the converter with patched copies of raw.pixls.us files
//! (notes: benchmark-results/adobe-baseline-capture-modes.json) showed what
//! the value depends on:
//!
//! - Canon: the SpecularWhiteLevel of the ColorData maker note (lower at
//!   ISO 100 and for pulled intermediate ISOs on many bodies) through
//!   `log2((white − black) / (reference white − black))`, capped at the
//!   camera's value; an ISO table for older bodies; highlight tone
//!   priority (LightingOpt or custom function 0x0203) adds about one stop;
//!   sRAW/mRAW files have their own table.
//! - Pentax: the maker note's DynamicRangeExpansion (highlight correction,
//!   tag 0x0069) adds a stop, like Canon's highlight tone priority.
//! - Olympus / OM System: `0.5 − log2(SensorCalibration[1] / 256)` from the
//!   ImageProcessing maker-note directory (0x2040, tag 0x0805), exact on
//!   every body sampled (extended low ISO: 512 → −0.5; OM-5: 290 → 0.32).
//! - Other makers: the EXIF ISO (Nikon "Lo", Canon 50, Phase One gain),
//!   the image size (Sony α1 II small/medium lossless, Phase One Sensor+).
//!
//! This module only reads those details; `camera_exposure.rs` holds the
//! rules. Reading is cheap: TIFF directories and, for CR3, the CTMD track
//! metadata; no pixel decode.
use rawler::formats::bmff::Bmff;
use rawler::formats::tiff::reader::TiffReader;
use rawler::formats::tiff::{GenericTiffReader, IFD, Value};
use std::fs::File;
use std::io::{BufReader, Read, Seek};

const EXIF_IFD_POINTER: u16 = 0x8769;
const MAKER_NOTE: u16 = 0x927c;
const CANON_CAMERA_SETTINGS: u16 = 0x0001;
const CANON_CUSTOM_FUNCTIONS2: u16 = 0x0099;
const CANON_COLOR_DATA: u16 = 0x4001;
const CANON_LIGHTING_OPT: u16 = 0x4018;
const CANON_CF2_HIGHLIGHT_TONE_PRIORITY: u32 = 0x0203;
const CTMD_BLOCK_MAKERNOTES: u16 = 0x927c;
const PENTAX_DYNAMIC_RANGE_EXPANSION: u16 = 0x0069;
const OLYMPUS_IMAGE_PROCESSING: u16 = 0x2040;
const NIKON_ISO: u16 = 0x0002;
const SONY_VIGNETTING_SETTING: u16 = 0x2011;
const SONY_DISTORTION_SETTING: u16 = 0x2013;
const NIKON_ISO_INFO: u16 = 0x0025;
const NIKON_COLOR_SPACE: u16 = 0x001e;
const EXIF_COLOR_SPACE: u16 = 0xa001;
const EXIF_INTEROP_POINTER: u16 = 0xa005;
const INTEROP_INDEX: u16 = 0x0001;
const OLYMPUS_SENSOR_CALIBRATION: u16 = 0x0805;
const OLYMPUS_DISTORTION: u16 = 0x150a;
const OLYMPUS_CHROMATIC_ABERRATION: u16 = 0x150c;
const PANASONIC_DISTORTION_INFO: u16 = 0x0119;
const PANASONIC_CA_INFO: u16 = 0x011b;
const OLYMPUS_ASPECT_FRAME: u16 = 0x1113;
const SUB_IFDS: u16 = 0x014a;

/// Maker-note facts of other makers.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct MakerFlags {
    /// Pentax DynamicRangeExpansion (highlight correction) on.
    pub pentax_highlight_correction: bool,
    /// Olympus / OM System SensorCalibration second value (256 = nominal gain).
    pub olympus_calibration: Option<u16>,
}

/// Reads the Pentax / Olympus maker-note facts of a TIFF-based raw file.
pub fn read_maker_flags(path: &str) -> MakerFlags {
    let mut flags = MakerFlags::default();
    let Some(data) = maker_note_bytes(path) else {
        return flags;
    };
    if let Some(header) = data.get(..4)
        && (header == b"AOC\0" || data.starts_with(b"PENTAX"))
    {
        // "AOC\0" + "MM"/"II" + IFD, or "PENTAX \0" + "MM"/"II" + IFD; offsets
        // are relative to the maker note.
        let at = if header == b"AOC\0" { 4 } else { 8 };
        let little = data.get(at..at + 2) == Some(b"II");
        let ifd = SliceIfd::new(&data, at + 2, little);
        if let Some(value) = ifd.bytes(PENTAX_DYNAMIC_RANGE_EXPANSION) {
            flags.pentax_highlight_correction = value.first() == Some(&1);
        }
    } else if data.starts_with(b"OLYMPUS\0") || data.starts_with(b"OM SYSTEM\0") {
        let (endian_at, ifd_at) = if data.starts_with(b"OLYMPUS\0") {
            (8, 12)
        } else {
            (12, 16)
        };
        let little = data.get(endian_at..endian_at + 2) == Some(b"II");
        let ifd = SliceIfd::new(&data, ifd_at, little);
        if let Some(sub) = ifd.sub_ifd(OLYMPUS_IMAGE_PROCESSING)
            && let Some(values) = sub.shorts(OLYMPUS_SENSOR_CALIBRATION)
            && values.len() >= 2
            && values[1] > 0
        {
            flags.olympus_calibration = Some(values[1]);
        }
    }
    flags
}

/// Nikon maker-note ISO (tag 0x0002: [0, ISO]). Nikon writes no EXIF ISO
/// for the expanded settings Lo/Hi (a D800 at Lo 1 records 50 only here),
/// and Adobe's baseline depends on them.
pub fn nikon_maker_iso(path: &str) -> Option<u32> {
    let data = maker_note_bytes(path)?;
    nikon_iso_from_maker_note(&data)
}

fn nikon_iso_from_maker_note(data: &[u8]) -> Option<u32> {
    // "Nikon\0" + version + 2 bytes, then a TIFF header; offsets are
    // relative to that header.
    if !data.starts_with(b"Nikon\0") {
        return None;
    }
    let tiff = data.get(10..)?;
    let little = match tiff.get(..2)? {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let ifd_at = SliceIfd::new(tiff, 0, little).u32(4)? as usize;
    let ifd = SliceIfd::new(tiff, ifd_at, little);
    let iso = ifd
        .shorts(NIKON_ISO)
        .and_then(|values| values.get(1).copied())
        .filter(|&iso| iso > 0)
        .map(u32::from);
    // Lo/Hi settings leave 0x0002 at zero; ISOInfo (0x0025) byte 0 holds
    // 12 × (log2(ISO / 100) + 5).
    iso.or_else(|| {
        let code = *ifd.bytes(NIKON_ISO_INFO)?.first()?;
        (code > 0).then(|| (100.0 * ((f64::from(code) / 12.0) - 5.0).exp2()).round() as u32)
    })
}

/// Sony's in-camera lens compensation settings (maker note, little-endian
/// IFD at offset 0): 0x2013 distortion and 0x2011 vignetting (shading)
/// compensation, 0 = Off, 2 = Auto; fixed-lens bodies store 0xffffffff.
/// Camera Raw applies none of the file's correction data when distortion
/// compensation was off, and the vignetting data only when shading
/// compensation was Auto (60 raw.pixls.us ARW files, 2026-10-05).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct SonyLensSettings {
    pub distortion_off: bool,
    pub vignetting_auto: bool,
}

pub fn sony_lens_settings(path: &str) -> Option<SonyLensSettings> {
    sony_lens_settings_from(&maker_note_bytes(path)?)
}

/// The same settings from Sony maker-note bytes (for DNGs: the "MakN" block
/// Adobe DNG Converter keeps).
pub fn sony_lens_settings_from(data: &[u8]) -> Option<SonyLensSettings> {
    let ifd = SliceIfd::new(data, 0, true);
    let setting = |tag: u16| {
        let &(_, typ, count, at) = ifd.entry(tag)?;
        (typ == 4 && count == 1).then(|| ifd.u32(at)).flatten()
    };
    Some(SonyLensSettings {
        distortion_off: setting(SONY_DISTORTION_SETTING) == Some(0),
        vignetting_auto: setting(SONY_VIGNETTING_SETTING) == Some(2),
    })
}

/// Lens correction an Olympus / OM System body stores in the ImageProcessing
/// maker-note directory: distortion [k2, k4, k6, corner radius] and, when
/// present, chromatic aberration [red 0, 2, 4, blue 0, 2, 4] (ExifTool's
/// Olympus ImageProcessing tags 0x150a / 0x150c).
pub fn olympus_lens_correction(path: &str) -> Option<(Vec<f64>, Option<Vec<f64>>)> {
    let data = maker_note_bytes(path)?;
    let (endian_at, ifd_at) = if data.starts_with(b"OLYMPUS\0") {
        (8, 12)
    } else if data.starts_with(b"OM SYSTEM\0") {
        (12, 16)
    } else {
        return None;
    };
    let little = data.get(endian_at..endian_at + 2) == Some(b"II");
    let ifd = SliceIfd::new(&data, ifd_at, little);
    let sub = ifd.sub_ifd(OLYMPUS_IMAGE_PROCESSING)?;
    let distortion = sub.reals(OLYMPUS_DISTORTION).filter(|v| v.len() == 4)?;
    let ca = sub
        .reals(OLYMPUS_CHROMATIC_ABERRATION)
        .filter(|v| v.len() == 6);
    Some((distortion, ca))
}

/// Olympus / OM System white-balance presets of the ImageProcessing
/// maker-note directory: (tag, [red, blue, green, green]) for the four-value
/// entries from WB_RBLevels (0x0100) through the Kelvin presets.
pub fn olympus_white_balance_presets(path: &str) -> Vec<(u16, [u16; 4])> {
    let Some(data) = maker_note_bytes(path) else {
        return Vec::new();
    };
    let (endian_at, ifd_at) = if data.starts_with(b"OLYMPUS\0") {
        (8, 12)
    } else if data.starts_with(b"OM SYSTEM\0") {
        (12, 16)
    } else {
        return Vec::new();
    };
    let little = data.get(endian_at..endian_at + 2) == Some(b"II");
    let ifd = SliceIfd::new(&data, ifd_at, little);
    let Some(sub) = ifd.sub_ifd(OLYMPUS_IMAGE_PROCESSING) else {
        return Vec::new();
    };
    (0x0100..=0x0121)
        .filter_map(|tag| {
            let v = sub.shorts(tag)?;
            (v.len() == 4).then(|| (tag, [v[0], v[1], v[2], v[3]]))
        })
        .collect()
}

/// Fujifilm's per-camera white-balance references (WB_GRBLevelsStandard,
/// tag 0xf00c of the RAF's CFA-section directory): (EXIF light source,
/// [green, red, blue]) pairs, e.g. standard light A (17) and D65 (21).
pub fn fujifilm_standard_white_balance(bytes: &[u8]) -> Vec<(u32, [u32; 3])> {
    fujifilm_standard_white_balance_inner(bytes).unwrap_or_default()
}

fn fujifilm_standard_white_balance_inner(bytes: &[u8]) -> Option<Vec<(u32, [u32; 3])>> {
    if bytes.get(..8) != Some(b"FUJIFILM") {
        return None;
    }
    let section = u32::from_be_bytes(bytes.get(100..104)?.try_into().ok()?) as usize;
    let data = bytes.get(section..)?;
    let little = match data.get(..4)? {
        b"II*\0" => true,
        b"MM\0*" => false,
        _ => return None,
    };
    let root = SliceIfd::new(
        data,
        SliceIfd::new(data, 0, little).u32(4)? as usize,
        little,
    );
    let fuji = root.sub_ifd(0xf000)?;
    let (_, typ, count, at) = *fuji.entry(0xf00c)?;
    if typ != 4 || count % 4 != 0 {
        return None;
    }
    let values: Vec<u32> = (0..count as usize)
        .map(|i| fuji.u32(at + 4 * i))
        .collect::<Option<_>>()?;
    Some(
        values
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| (c[3], [c[0], c[1], c[2]]))
            .collect(),
    )
}

/// Panasonic's DistortionInfo (RW2/RWL IFD0 tag 0x0119): sixteen signed
/// 16-bit little-endian values, and the half-diagonal in pixels of the frame
/// the raw data covers (from the sensor border tags 0x0004-0x0007), if
/// readable.
pub fn panasonic_distortion_info(path: &str) -> Option<([i16; 16], Option<f64>)> {
    let mut file = File::open(path).ok()?;
    let mut data = Vec::new();
    file.by_ref().take(1 << 20).read_to_end(&mut data).ok()?;
    let little = match data.get(..2)? {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let root_at = SliceIfd::new(&data, 0, little).u32(4)? as usize;
    let root = SliceIfd::new(&data, root_at, little);
    let bytes = root.bytes(PANASONIC_DISTORTION_INFO)?;
    if bytes.len() != 32 {
        return None;
    }
    let mut values = [0i16; 16];
    for (i, value) in values.iter_mut().enumerate() {
        *value = i16::from_le_bytes([bytes[2 * i], bytes[2 * i + 1]]);
    }
    let border = |tag: u16| root.shorts(tag).and_then(|v| v.first().copied());
    // The frame the photo shows: the aspect-ratio crop when the camera
    // recorded one, else the sensor area.
    let tags = if border(0x002f).is_some() {
        [0x002f, 0x0030, 0x0031, 0x0032]
    } else {
        [0x0004, 0x0005, 0x0006, 0x0007]
    };
    let half_diagonal = match (
        border(tags[0]),
        border(tags[1]),
        border(tags[2]),
        border(tags[3]),
    ) {
        (Some(top), Some(left), Some(bottom), Some(right)) if bottom > top && right > left => {
            let (w, h) = ((right - left) as f64, (bottom - top) as f64);
            Some(0.5 * (w * w + h * h).sqrt())
        }
        _ => None,
    };
    Some((values, half_diagonal))
}

/// Panasonic's lateral CA data (RW2/RWL IFD0 tag 0x011b, undocumented):
/// thirty-two signed 16-bit little-endian values, and the half-diagonal in
/// pixels of the frame the raw data covers (the aspect-ratio crop when it
/// holds a frame, else the sensor area). Layout in
/// `embedded_lens::panasonic_chromatic`.
pub fn panasonic_ca_info(path: &str) -> Option<([i16; 32], f64)> {
    let mut file = File::open(path).ok()?;
    let mut data = Vec::new();
    file.by_ref().take(1 << 20).read_to_end(&mut data).ok()?;
    let little = match data.get(..2)? {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let root_at = SliceIfd::new(&data, 0, little).u32(4)? as usize;
    let root = SliceIfd::new(&data, root_at, little);
    let bytes = root.bytes(PANASONIC_CA_INFO)?;
    if bytes.len() != 64 {
        return None;
    }
    let mut values = [0i16; 32];
    for (i, value) in values.iter_mut().enumerate() {
        *value = i16::from_le_bytes([bytes[2 * i], bytes[2 * i + 1]]);
    }
    let border = |tag: u16| root.shorts(tag).and_then(|v| v.first().copied());
    // Some bodies (G7, G70) write the crop tags as zeros.
    let half_diagonal = [
        [0x002f, 0x0030, 0x0031, 0x0032],
        [0x0004, 0x0005, 0x0006, 0x0007],
    ]
    .iter()
    .find_map(|tags| {
        match (
            border(tags[0]),
            border(tags[1]),
            border(tags[2]),
            border(tags[3]),
        ) {
            (Some(top), Some(left), Some(bottom), Some(right)) if bottom > top && right > left => {
                let (w, h) = ((right - left) as f64, (bottom - top) as f64);
                Some(0.5 * (w * w + h * h).sqrt())
            }
            _ => None,
        }
    })?;
    Some((values, half_diagonal))
}

/// Whether the camera wrote its JPEGs (and the embedded previews of a RAW)
/// in Adobe RGB rather than sRGB, from a TIFF-based file's bytes: EXIF
/// ColorSpace 2, or Uncalibrated (0xffff) with the interoperability index
/// "R03" (DCF's Adobe RGB option), else Nikon's maker-note ColorSpace
/// (0x001e: 1 sRGB, 2 Adobe RGB; Nikon's embedded previews carry no EXIF
/// of their own). `None` when the file says neither.
pub fn camera_adobe_rgb(data: &[u8]) -> Option<bool> {
    let little = match data.get(..2)? {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let root_at = SliceIfd::new(data, 0, little).u32(4)? as usize;
    let root = SliceIfd::new(data, root_at, little);
    let exif_at = root.entry(EXIF_IFD_POINTER).map(|e| e.3)?;
    let exif = SliceIfd::new(data, root.u32(exif_at)? as usize, little);
    let short = |ifd: &SliceIfd, tag: u16| ifd.shorts(tag).and_then(|v| v.first().copied());
    match short(&exif, EXIF_COLOR_SPACE) {
        Some(1) => return Some(false),
        Some(2) => return Some(true),
        Some(0xffff) => {
            let interop = exif
                .entry(EXIF_INTEROP_POINTER)
                .and_then(|e| exif.u32(e.3))
                .map(|at| SliceIfd::new(data, at as usize, little));
            if let Some(index) = interop.as_ref().and_then(|ifd| ifd.bytes(INTEROP_INDEX)) {
                return Some(index.starts_with(b"R03"));
            }
        }
        _ => {}
    }
    nikon_adobe_rgb(exif.bytes(MAKER_NOTE)?)
}

fn nikon_adobe_rgb(note: &[u8]) -> Option<bool> {
    if !note.starts_with(b"Nikon\0") {
        return None;
    }
    let tiff = note.get(10..)?;
    let little = match tiff.get(..2)? {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let ifd_at = SliceIfd::new(tiff, 0, little).u32(4)? as usize;
    match SliceIfd::new(tiff, ifd_at, little)
        .shorts(NIKON_COLOR_SPACE)?
        .first()?
    {
        1 => Some(false),
        2 => Some(true),
        _ => None,
    }
}

/// The EXIF MakerNote bytes of a TIFF-based file (also ORF and RW2 with
/// their own magic numbers): header, IFD0, Exif IFD, tag 0x927c.
fn maker_note_bytes(path: &str) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut file = File::open(path).ok()?;
    let mut data = Vec::new();
    // Maker notes sit in the first megabytes; the raw data is not needed.
    file.by_ref().take(8 << 20).read_to_end(&mut data).ok()?;
    maker_note_slice(&data).map(<[u8]>::to_vec)
}

fn maker_note_slice(data: &[u8]) -> Option<&[u8]> {
    let little = match data.get(..2)? {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let root_at = SliceIfd::new(data, 0, little).u32(4)? as usize;
    let root = SliceIfd::new(data, root_at, little);
    let exif_at = root.entry(EXIF_IFD_POINTER).map(|e| e.3)?;
    let exif = SliceIfd::new(data, root.u32(exif_at)? as usize, little);
    exif.bytes(MAKER_NOTE)
}

/// The frame of the aspect ratio set in the camera (left, top, width,
/// height), which Camera Raw uses as the default crop.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AspectCrop {
    /// Panasonic RW2 CropTop/Left/Bottom/Right (IFD0 0x002f-0x0032), in the
    /// raw file's pixel coordinates.
    Absolute(usize, usize, usize, usize),
    /// Olympus / OM System ImageProcessing AspectFrame (0x1113), relative to
    /// the image area the decoder crops to.
    Relative(usize, usize, usize, usize),
    /// Sony ARW SonyCropSize (width, height), centred in the image area the
    /// decoder crops to.
    Centered(usize, usize),
}

/// Reads the camera's aspect-ratio frame from a RW2/RWL, ARW or ORF file's
/// bytes.
pub fn aspect_crop(data: &[u8]) -> Option<AspectCrop> {
    let little = match data.get(..2)? {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let root_at = SliceIfd::new(data, 0, little).u32(4)? as usize;
    let root = SliceIfd::new(data, root_at, little);
    let short = |ifd: &SliceIfd, tag: u16| ifd.shorts(tag).and_then(|v| v.first().copied());
    if let (Some(top), Some(left), Some(bottom), Some(right)) = (
        short(&root, 0x002f),
        short(&root, 0x0030),
        short(&root, 0x0031),
        short(&root, 0x0032),
    ) {
        return (bottom > top && right > left).then_some(AspectCrop::Absolute(
            left as usize,
            top as usize,
            (right - left) as usize,
            (bottom - top) as usize,
        ));
    }
    // Sony SonyCropTopLeft / SonyCropSize (raw SubIFD 0x74c7 / 0x74c8) of
    // 16:9, 4:3 or 1:1 frames. Adobe centres the size in the default crop
    // (the stored left edge of an A7R III A 16:9 frame is 0, not 24).
    if let Some(raw) = root.sub_ifd(SUB_IFDS) {
        let pair = |tag: u16| -> Option<(usize, usize)> {
            match raw.reals(tag)?[..] {
                [a, b] if a >= 0.0 && b >= 0.0 => Some((a as usize, b as usize)),
                _ => None,
            }
        };
        if let (Some(_), Some((width, height))) = (pair(0x74c7), pair(0x74c8)) {
            return (width > 0 && height > 0).then_some(AspectCrop::Centered(width, height));
        }
    }
    let note = maker_note_slice(data)?;
    // Older Sony bodies (A7S II) only reduce the maker note's FullImageSize
    // (0xb02b, height and width) for a 16:9 frame; its offsets are relative
    // to the file. Bodies that report the whole sensor there are not
    // cropped (Camera Raw ignores their AspectRatio setting).
    if let Some(note_at) = (note.as_ptr() as usize).checked_sub(data.as_ptr() as usize)
        && ![&b"OLYMPUS"[..], b"OM SYSTEM", b"Panasonic"]
            .iter()
            .any(|header| note.starts_with(header))
    {
        let sony = SliceIfd::new(data, note_at, little);
        if let Some(&(_, 4, 2, at)) = sony.entry(0xb02b)
            && let (Some(height), Some(width)) = (sony.u32(at), sony.u32(at + 4))
            && width > 0
            && height > 0
            && width < 20000
            && height < 20000
        {
            return Some(AspectCrop::Centered(width as usize, height as usize));
        }
    }
    let (endian_at, ifd_at) = if note.starts_with(b"OLYMPUS\0") {
        (8, 12)
    } else if note.starts_with(b"OM SYSTEM\0") {
        (12, 16)
    } else {
        return None;
    };
    let little = note.get(endian_at..endian_at + 2) == Some(b"II");
    let ifd = SliceIfd::new(note, ifd_at, little);
    let frame = ifd
        .sub_ifd(OLYMPUS_IMAGE_PROCESSING)?
        .shorts(OLYMPUS_ASPECT_FRAME)?;
    let [left, top, right, bottom] = frame[..] else {
        return None;
    };
    (right > left && bottom > top).then_some(AspectCrop::Relative(
        left as usize,
        top as usize,
        (right - left) as usize + 1,
        (bottom - top) as usize + 1,
    ))
}

/// Minimal TIFF directory over a byte slice with offsets relative to the
/// slice start (maker-note directories).
struct SliceIfd<'a> {
    data: &'a [u8],
    entries: Vec<(u16, u16, u32, usize)>,
    little: bool,
}

impl<'a> SliceIfd<'a> {
    fn new(data: &'a [u8], at: usize, little: bool) -> Self {
        let mut ifd = SliceIfd {
            data,
            entries: Vec::new(),
            little,
        };
        let Some(count) = ifd.u16(at) else {
            return ifd;
        };
        for i in 0..(count as usize).min(512) {
            let e = at + 2 + 12 * i;
            let (Some(tag), Some(typ), Some(count)) = (ifd.u16(e), ifd.u16(e + 2), ifd.u32(e + 4))
            else {
                break;
            };
            let size = count as usize * type_size(typ);
            let value_at = if size <= 4 {
                e + 8
            } else {
                match ifd.u32(e + 8) {
                    Some(offset) => offset as usize,
                    None => break,
                }
            };
            ifd.entries.push((tag, typ, count, value_at));
        }
        ifd
    }

    fn u16(&self, at: usize) -> Option<u16> {
        let b = self.data.get(at..at + 2)?;
        Some(if self.little {
            u16::from_le_bytes([b[0], b[1]])
        } else {
            u16::from_be_bytes([b[0], b[1]])
        })
    }

    fn u32(&self, at: usize) -> Option<u32> {
        let b = self.data.get(at..at + 4)?;
        Some(if self.little {
            u32::from_le_bytes([b[0], b[1], b[2], b[3]])
        } else {
            u32::from_be_bytes([b[0], b[1], b[2], b[3]])
        })
    }

    fn entry(&self, tag: u16) -> Option<&(u16, u16, u32, usize)> {
        self.entries.iter().find(|e| e.0 == tag)
    }

    fn bytes(&self, tag: u16) -> Option<&'a [u8]> {
        let (_, typ, count, at) = *self.entry(tag)?;
        self.data.get(at..at + count as usize * type_size(typ))
    }

    /// SHORT or SSHORT values (Olympus writes SensorCalibration as SSHORT).
    fn shorts(&self, tag: u16) -> Option<Vec<u16>> {
        let (_, typ, count, at) = *self.entry(tag)?;
        if typ != 3 && typ != 8 {
            return None;
        }
        (0..count as usize).map(|i| self.u16(at + 2 * i)).collect()
    }

    /// Numeric values of any integer, rational or floating-point type.
    fn reals(&self, tag: u16) -> Option<Vec<f64>> {
        let (_, typ, count, at) = *self.entry(tag)?;
        let size = type_size(typ);
        (0..count as usize)
            .map(|i| {
                let p = at + i * size;
                Some(match typ {
                    3 => self.u16(p)? as f64,
                    8 => self.u16(p)? as i16 as f64,
                    4 => self.u32(p)? as f64,
                    9 => self.u32(p)? as i32 as f64,
                    5 => self.u32(p)? as f64 / (self.u32(p + 4)?.max(1)) as f64,
                    10 => {
                        let d = self.u32(p + 4)? as i32;
                        if d == 0 {
                            return None;
                        }
                        self.u32(p)? as i32 as f64 / d as f64
                    }
                    11 => f32::from_bits(self.u32(p)?) as f64,
                    12 => {
                        let (a, b) = (self.u32(p)? as u64, self.u32(p + 4)? as u64);
                        f64::from_bits(if self.little {
                            a | (b << 32)
                        } else {
                            (a << 32) | b
                        })
                    }
                    _ => return None,
                })
            })
            .collect()
    }

    /// Olympus sub-directories: an IFD-typed offset, a LONG offset, or
    /// UNDEFINED bytes holding the directory, all relative to the slice.
    fn sub_ifd(&self, tag: u16) -> Option<SliceIfd<'a>> {
        let (_, typ, _, at) = *self.entry(tag)?;
        let offset = match typ {
            4 | 13 => self.u32(at)? as usize,
            7 | 1 => at,
            _ => return None,
        };
        Some(SliceIfd::new(self.data, offset, self.little))
    }
}

fn type_size(typ: u16) -> usize {
    match typ {
        1 | 2 | 6 | 7 => 1,
        3 | 8 => 2,
        4 | 9 | 11 | 13 => 4,
        5 | 10 | 12 => 8,
        _ => 1,
    }
}

/// Canon maker-note facts Adobe's rules use.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct CanonCapture {
    /// ColorData per-channel black (the four values before NormalWhiteLevel).
    pub black: Option<f32>,
    /// ColorData SpecularWhiteLevel.
    pub specular_white: Option<u32>,
    /// Highlight tone priority enabled.
    pub highlight_tone_priority: bool,
    /// CameraSettings SRAWQuality: 0 = full RAW, 1 = sRAW1 (mRAW), 2 = sRAW2 (sRAW).
    pub sraw_quality: u16,
}

/// Reads the Canon capture details of a CR2 or CR3 file; `None` when the
/// file has no readable Canon maker note.
pub fn read_canon(path: &str) -> Option<CanonCapture> {
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".cr3") {
        read_cr3(path)
    } else if lower.ends_with(".cr2") {
        read_cr2(path)
    } else {
        None
    }
}

/// The Canon ColorData maker-note array (tag 0x4001) of a CR2 or CR3 file.
pub fn canon_color_data(path: &str) -> Option<Vec<u16>> {
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".cr3") {
        let mut file = BufReader::new(File::open(path).ok()?);
        let bmff = Bmff::new(&mut file).ok()?;
        let data = read_ctmd_record(&mut file, &bmff, 8)?;
        let tiff = GenericTiffReader::new_with_buffer(data, 0, 0, Some(0)).ok()?;
        colordata_words(tiff.root_ifd())
    } else if lower.ends_with(".cr2") {
        let mut file = BufReader::new(File::open(path).ok()?);
        let tiff = GenericTiffReader::new(&mut file, 0, 0, None, &[]).ok()?;
        let exif = tiff
            .root_ifd()
            .sub_ifds()
            .get(&EXIF_IFD_POINTER)
            .and_then(|subs| subs.first())?;
        let offset = exif.get_entry(MAKER_NOTE)?.offset()?;
        let makernote = tiff
            .parse_ifd(&mut file, offset as u32, 0, 0, exif.endian, &[])
            .ok()?;
        colordata_words(&makernote)
    } else {
        None
    }
}

fn colordata_words(ifd: &IFD) -> Option<Vec<u16>> {
    let entry = ifd.get_entry(CANON_COLOR_DATA)?;
    let data: Vec<u16> = match &entry.value {
        Value::Short(values) => values.clone(),
        Value::Undefined(bytes) | Value::Byte(bytes) => bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| u16::from_le_bytes(*b))
            .collect(),
        _ => return None,
    };
    (!data.is_empty()).then_some(data)
}

fn read_cr2(path: &str) -> Option<CanonCapture> {
    let mut file = BufReader::new(File::open(path).ok()?);
    let tiff = GenericTiffReader::new(&mut file, 0, 0, None, &[]).ok()?;
    let exif = tiff
        .root_ifd()
        .sub_ifds()
        .get(&EXIF_IFD_POINTER)
        .and_then(|subs| subs.first())?;
    let offset = exif.get_entry(MAKER_NOTE)?.offset()?;
    // Canon maker notes are a plain IFD with offsets relative to the TIFF header.
    let makernote = tiff
        .parse_ifd(&mut file, offset as u32, 0, 0, exif.endian, &[])
        .ok()?;
    Some(canon_from_ifds(&makernote, Some(&makernote)))
}

fn read_cr3(path: &str) -> Option<CanonCapture> {
    let mut file = BufReader::new(File::open(path).ok()?);
    let bmff = Bmff::new(&mut file).ok()?;
    let desc = bmff.filebox.moov.cr3desc.as_ref()?;
    let makernote = desc.cmt3.tiff.root_ifd();
    // The ColorData lives in the per-frame CTMD track (record 8), not in CMT3.
    let colordata_ifd = read_ctmd_record(&mut file, &bmff, 8)
        .and_then(|data| GenericTiffReader::new_with_buffer(data, 0, 0, Some(0)).ok());
    Some(canon_from_ifds(
        makernote,
        colordata_ifd.as_ref().map(|tiff| tiff.root_ifd()),
    ))
}

/// The maker-note block (tag 0x927c) of a CTMD record.
fn read_ctmd_record<R: Read + Seek>(file: &mut R, bmff: &Bmff, record: u16) -> Option<Vec<u8>> {
    let trak = bmff
        .filebox
        .moov
        .traks
        .iter()
        .find(|trak| trak.mdia.minf.stbl.stsd.ctmd.is_some())?;
    let (offset, size) = trak.mdia.minf.stbl.get_sample_offset(1)?;
    if size > 1 << 24 {
        return None;
    }
    file.seek(std::io::SeekFrom::Start(offset as u64)).ok()?;
    let mut data = vec![0u8; size];
    file.read_exact(&mut data).ok()?;
    let u32_at = |at: usize| {
        data.get(at..at + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    };
    let u16_at = |at: usize| {
        data.get(at..at + 2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
    };
    let mut at = 0;
    while at + 12 <= data.len() {
        let rec_size = u32_at(at)? as usize;
        let rec_type = u16_at(at + 4)?;
        if rec_size < 12 || at + rec_size > data.len() {
            return None;
        }
        if rec_type == record {
            let mut block = at + 12;
            let end = at + rec_size;
            while block + 8 <= end {
                let size = u32_at(block)? as usize;
                let tag = u16_at(block + 4)?;
                if size < 8 || block + size > end {
                    return None;
                }
                if tag == CTMD_BLOCK_MAKERNOTES {
                    return Some(data[block + 8..block + size].to_vec());
                }
                block += size;
            }
            return None;
        }
        at += rec_size;
    }
    None
}

fn canon_from_ifds(makernote: &IFD, colordata_ifd: Option<&IFD>) -> CanonCapture {
    let mut out = CanonCapture::default();
    if let Some(entry) = makernote.get_entry(CANON_CAMERA_SETTINGS)
        && entry.count() > 46
    {
        out.sraw_quality = entry.force_u16(46);
    }
    out.highlight_tone_priority = highlight_tone_priority(makernote);
    if let Some(data) = colordata_ifd.and_then(colordata_words)
        && let Some((black, white)) = colordata_levels(&data)
    {
        out.black = Some(black);
        out.specular_white = Some(white);
    }
    out
}

/// Highlight tone priority: LightingOpt (0x4018) value 3 on bodies that
/// write it, else custom function 0x0203 in CustomFunctions2 (0x0099).
fn highlight_tone_priority(makernote: &IFD) -> bool {
    if let Some(entry) = makernote.get_entry(CANON_LIGHTING_OPT)
        && entry.count() > 3
    {
        return entry.force_u32(3) == 1;
    }
    let Some(entry) = makernote.get_entry(CANON_CUSTOM_FUNCTIONS2) else {
        return false;
    };
    let values: Vec<u32> = (0..entry.count() as usize)
        .map(|i| entry.force_u32(i))
        .collect();
    custom_function(&values, CANON_CF2_HIGHLIGHT_TONE_PRIORITY) == Some(1)
}

/// CustomFunctions2 layout: total size, group count, then per group
/// (group, size in bytes, entry count, entries of tag, count, values…).
fn custom_function(values: &[u32], wanted: u32) -> Option<u32> {
    let groups = *values.get(1)? as usize;
    let mut at = 2;
    for _ in 0..groups.min(16) {
        let count = *values.get(at + 2)? as usize;
        at += 3;
        for _ in 0..count.min(256) {
            let tag = *values.get(at)?;
            let n = *values.get(at + 1)? as usize;
            if tag == wanted && n == 1 {
                return values.get(at + 2).copied();
            }
            at += 2 + n;
        }
    }
    None
}

/// (black, SpecularWhiteLevel) from a Canon ColorData array, by the
/// version-specific offset rawler also uses, validated against the layout
/// (four similar blacks, two white levels, then the linearity margin of
/// 10000 to 12735); falls back to a search for the layout.
fn colordata_levels(data: &[u16]) -> Option<(f32, u32)> {
    if data.len() < 2 {
        return None;
    }
    let version = data[0] as i16;
    let specular = match version {
        4 | 5 => Some(0x2b9),
        6 | 7 => Some(0x2d0),
        9 => Some(0x2d4),
        -4 => Some(0x56a),
        10 if data.len() == 1273 || data.len() == 1275 => Some(0x1e4),
        10 => Some(0x1fd),
        11 => Some(0x2dd),
        12 | 13 | 15 => Some(0x30f),
        14 => Some(0x231),
        16..=19 => Some(0x31d),
        32 | 33 => Some(0x32b),
        34 => Some(0x281),
        // 48 (R7/R10/R50): rawler's offset points one past the specular level.
        48 => Some(0x281),
        64..=66 => Some(0x295),
        _ => None,
    };
    if let Some(at) = specular
        && let Some(levels) = levels_at(data, at)
    {
        return Some(levels);
    }
    (6..data.len().saturating_sub(1)).find_map(|at| levels_at(data, at))
}

fn levels_at(data: &[u16], specular: usize) -> Option<(f32, u32)> {
    if specular < 5 || specular + 1 >= data.len() {
        return None;
    }
    let blacks = &data[specular - 5..specular - 1];
    let (lo, hi) = (*blacks.iter().min().unwrap(), *blacks.iter().max().unwrap());
    let normal = data[specular - 1];
    let white = data[specular];
    let margin = data[specular + 1];
    if hi - lo > 8
        || hi > 4200
        || !(6000..=16383).contains(&white)
        || !(6000..=16383).contains(&normal)
        || !(9000..=13500).contains(&margin)
    {
        return None;
    }
    let black = blacks.iter().map(|b| *b as f32).sum::<f32>() / 4.0;
    Some((black, white as u32))
}

/// Camera make, model and the model's base name (for regional aliases) as
/// rawler names them (its camera database), and the EXIF ISO.
pub fn read_camera(path: &str) -> Option<(String, String, String, Option<u32>)> {
    let source = rawler::rawsource::RawSource::new(std::path::Path::new(path)).ok()?;
    let decoder = rawler::get_decoder(&source).ok()?;
    let metadata = decoder
        .raw_metadata(&source, &rawler::decoders::RawDecodeParams::default())
        .ok()?;
    let iso = metadata
        .exif
        .iso_speed_ratings
        .map(u32::from)
        .filter(|iso| *iso > 0)
        .or(metadata.exif.iso_speed)
        .or(metadata.exif.recommended_exposure_index)
        .filter(|iso| *iso > 0)
        .or_else(|| {
            metadata
                .make
                .eq_ignore_ascii_case("nikon")
                .then(|| nikon_maker_iso(path))
                .flatten()
        })
        .or_else(|| {
            path.to_ascii_lowercase()
                .ends_with(".crw")
                .then(|| std::fs::read(path).ok())
                .flatten()
                .and_then(|bytes| crw_iso(&bytes))
        });
    Some((metadata.make, metadata.model, metadata.canonical_model, iso))
}

#[cfg(test)]
mod aspect_tests {
    use super::*;

    /// A little-endian TIFF with IFD0 holding SHORT tags.
    fn tiff_with_shorts(tags: &[(u16, u16)]) -> Vec<u8> {
        let mut data = b"II*\0".to_vec();
        data.extend(8u32.to_le_bytes());
        data.extend((tags.len() as u16).to_le_bytes());
        for &(tag, value) in tags {
            data.extend(tag.to_le_bytes());
            data.extend(3u16.to_le_bytes());
            data.extend(1u32.to_le_bytes());
            data.extend(value.to_le_bytes());
            data.extend([0u8, 0]);
        }
        data.extend(0u32.to_le_bytes());
        data
    }

    #[test]
    fn panasonic_crop_tags_give_the_aspect_frame() {
        // LX15 shot at 16:9: CropTop 292, CropLeft 8, CropBottom 3372, CropRight 5480.
        let data = tiff_with_shorts(&[(0x002f, 292), (0x0030, 8), (0x0031, 3372), (0x0032, 5480)]);
        assert_eq!(
            aspect_crop(&data),
            Some(AspectCrop::Absolute(8, 292, 5472, 3080))
        );
        assert_eq!(aspect_crop(&tiff_with_shorts(&[(0x0004, 8)])), None);
    }
}

#[cfg(test)]
mod iso_tests {
    use super::{crw_iso, nikon_iso_from_maker_note};

    /// "Nikon\0" maker note (type 3) with one big-endian IFD entry.
    fn nikon_note(tag: u16, typ: u16, count: u32, value: [u8; 4]) -> Vec<u8> {
        let mut data = b"Nikon\0\x02\x10\0\0MM\0\x2a".to_vec();
        data.extend(8u32.to_be_bytes());
        data.extend(1u16.to_be_bytes());
        data.extend(tag.to_be_bytes());
        data.extend(typ.to_be_bytes());
        data.extend(count.to_be_bytes());
        data.extend(value);
        data.extend(0u32.to_be_bytes());
        data
    }

    /// Little-endian TIFF: IFD0 → Exif IFD (ColorSpace `space`) → Interop
    /// IFD (index `index`), each with inline values.
    fn exif_colour_tiff(space: u16, index: &[u8; 4]) -> Vec<u8> {
        let entry = |tag: u16, typ: u16, count: u32, value: [u8; 4]| {
            [
                &tag.to_le_bytes()[..],
                &typ.to_le_bytes(),
                &count.to_le_bytes(),
                &value,
            ]
            .concat()
        };
        let mut data = b"II*\0".to_vec();
        data.extend(8u32.to_le_bytes());
        // IFD0 at 8 (one entry, 18 bytes), Exif IFD at 26 (two entries, 30
        // bytes), Interop IFD at 56.
        data.extend(1u16.to_le_bytes());
        data.extend(entry(0x8769, 4, 1, 26u32.to_le_bytes()));
        data.extend(0u32.to_le_bytes());
        data.extend(2u16.to_le_bytes());
        let [a, b] = space.to_le_bytes();
        data.extend(entry(0xa001, 3, 1, [a, b, 0, 0]));
        data.extend(entry(0xa005, 4, 1, 56u32.to_le_bytes()));
        data.extend(0u32.to_le_bytes());
        data.extend(1u16.to_le_bytes());
        data.extend(entry(0x0001, 2, 4, *index));
        data.extend(0u32.to_le_bytes());
        data
    }

    #[test]
    fn camera_colour_space_comes_from_exif() {
        use super::camera_adobe_rgb;
        assert_eq!(
            camera_adobe_rgb(&exif_colour_tiff(1, b"R98\0")),
            Some(false)
        );
        assert_eq!(camera_adobe_rgb(&exif_colour_tiff(2, b"R98\0")), Some(true));
        assert_eq!(
            camera_adobe_rgb(&exif_colour_tiff(0xffff, b"R03\0")),
            Some(true)
        );
        assert_eq!(
            camera_adobe_rgb(&exif_colour_tiff(0xffff, b"R98\0")),
            Some(false)
        );
        assert_eq!(camera_adobe_rgb(b"not a tiff"), None);
    }

    #[test]
    fn nikon_colour_space_comes_from_the_maker_note() {
        assert_eq!(
            super::nikon_adobe_rgb(&nikon_note(0x001e, 3, 1, [0, 2, 0, 0])),
            Some(true)
        );
        assert_eq!(
            super::nikon_adobe_rgb(&nikon_note(0x001e, 3, 1, [0, 1, 0, 0])),
            Some(false)
        );
        assert_eq!(
            super::nikon_adobe_rgb(&nikon_note(0x0002, 3, 1, [0, 2, 0, 0])),
            None
        );
    }

    #[test]
    fn nikon_iso_comes_from_the_maker_note() {
        // ISO tag: [0, 640].
        let note = nikon_note(0x0002, 3, 2, [0, 0, 0x02, 0x80]);
        assert_eq!(nikon_iso_from_maker_note(&note), Some(640));
        // Lo 1 on a D800: 0x0002 is zero, ISOInfo byte 0 is 48 (ISO 50).
        let mut lo = nikon_note(0x0025, 7, 4, [48, 1, 12, 0]);
        assert_eq!(nikon_iso_from_maker_note(&lo), Some(50));
        lo[0] = b'X';
        assert_eq!(nikon_iso_from_maker_note(&lo), None);
    }

    #[test]
    fn crw_iso_comes_from_shot_info() {
        // A CIFF heap with one ShotInfo record whose word 2 is 160 (ISO 100).
        let mut data = b"II".to_vec();
        data.extend(14u32.to_le_bytes());
        data.extend(b"HEAPCCDR");
        let heap = data.len();
        let mut shot = vec![0u8; 16];
        shot[4..6].copy_from_slice(&160i16.to_le_bytes());
        data.extend(&shot);
        let table = data.len() - heap;
        data.extend(1u16.to_le_bytes());
        data.extend(0x102au16.to_le_bytes());
        data.extend((shot.len() as u32).to_le_bytes());
        data.extend(0u32.to_le_bytes());
        data.extend((table as u32).to_le_bytes());
        assert_eq!(crw_iso(&data), Some(100));
    }
}

#[cfg(test)]
mod tests {
    use super::{colordata_levels, custom_function};

    #[test]
    fn colordata_layout_is_found_by_version_and_by_search() {
        // Version 7 (550D): blacks at 0x2cb, white levels at 0x2cf/0x2d0.
        let mut data = vec![0u16; 1338];
        data[0] = 7;
        data[0x2cb..0x2cf].copy_from_slice(&[2047, 2048, 2048, 2048]);
        data[0x2cf] = 11767;
        data[0x2d0] = 12279;
        data[0x2d1] = 10000;
        assert_eq!(colordata_levels(&data), Some((2047.75, 12279)));
        // Unknown version: the layout search finds the same fields.
        data[0] = 99;
        assert_eq!(colordata_levels(&data), Some((2047.75, 12279)));
        // Nothing plausible.
        assert_eq!(colordata_levels(&[7, 0, 0, 0]), None);
    }

    #[test]
    fn slice_ifd_reads_inline_and_offset_values_and_sub_directories() {
        // Big-endian directory with a SHORT[3] at an offset and an IFD-typed
        // sub-directory holding four inline bytes.
        let mut data = vec![0u8; 64];
        data[0..2].copy_from_slice(&2u16.to_be_bytes());
        // entry 0: tag 0x0805, SHORT, count 3 (6 bytes, so stored at offset 40)
        data[2..4].copy_from_slice(&0x0805u16.to_be_bytes());
        data[4..6].copy_from_slice(&3u16.to_be_bytes());
        data[6..10].copy_from_slice(&3u32.to_be_bytes());
        data[10..14].copy_from_slice(&40u32.to_be_bytes());
        data[40..42].copy_from_slice(&4094u16.to_be_bytes());
        data[42..44].copy_from_slice(&290u16.to_be_bytes());
        data[44..46].copy_from_slice(&7u16.to_be_bytes());
        // entry 1: tag 0x2040, IFD type, count 1, offset 48 → directory with one inline SHORT[2]
        data[14..16].copy_from_slice(&0x2040u16.to_be_bytes());
        data[16..18].copy_from_slice(&13u16.to_be_bytes());
        data[18..22].copy_from_slice(&1u32.to_be_bytes());
        data[22..26].copy_from_slice(&48u32.to_be_bytes());
        data[48..50].copy_from_slice(&1u16.to_be_bytes());
        data[50..52].copy_from_slice(&0x0069u16.to_be_bytes());
        data[52..54].copy_from_slice(&1u16.to_be_bytes());
        data[54..58].copy_from_slice(&4u32.to_be_bytes());
        data[58..62].copy_from_slice(&[1, 2, 0, 0]);
        let ifd = super::SliceIfd::new(&data, 0, false);
        assert_eq!(ifd.shorts(0x0805), Some(vec![4094, 290, 7]));
        let sub = ifd.sub_ifd(0x2040).unwrap();
        assert_eq!(sub.bytes(0x0069), Some(&[1u8, 2, 0, 0][..]));
        assert_eq!(sub.bytes(0x0070), None);
    }

    #[test]
    fn custom_function_records_are_walked() {
        // 760D layout: total, 4 groups; group 1 (two entries), group 2 (0x0203 = 1).
        let values = [
            224, 4, 1, 32, 2, 257, 1, 0, 259, 1, 0, 2, 20, 1, 515, 1, 1, 3, 80, 6, 1294, 1, 2,
        ];
        assert_eq!(custom_function(&values, 0x0203), Some(1));
        assert_eq!(custom_function(&values, 0x0103), Some(0));
        assert_eq!(custom_function(&values, 0x9999), None);
        assert_eq!(custom_function(&[224], 0x0203), None);
    }
}

#[cfg(test)]
mod file_tests {
    /// Prints the Canon capture details of the files in
    /// `RAWTONIC_CAPTURE_FILES` (colon separated), for checking the reader
    /// against the Python probe tooling.
    #[test]
    #[ignore]
    fn print_canon_capture_of_env_files() {
        let Ok(files) = std::env::var("RAWTONIC_CAPTURE_FILES") else {
            return;
        };
        for path in files.split(':').filter(|p| !p.is_empty()) {
            println!("{path}: {:?}", super::read_canon(path));
        }
    }
}

/// Orientation of a Canon CRW as the EXIF orientation value, from the CIFF
/// ImageSpec record (0x1810: width, height, pixel aspect, rotation in
/// degrees), which rawler does not read. `None` when the file is not CIFF.
pub fn crw_orientation(bytes: &[u8]) -> Option<u16> {
    if bytes.get(6..14) != Some(b"HEAPCCDR") || bytes.get(..2) != Some(b"II") {
        return None;
    }
    let header_len = u32::from_le_bytes(bytes.get(2..6)?.try_into().ok()?) as usize;
    let rotation = ciff_find(
        bytes,
        header_len,
        bytes.len().checked_sub(header_len)?,
        0x1810,
        0,
    )?;
    let degrees = u32::from_le_bytes(bytes.get(rotation + 12..rotation + 16)?.try_into().ok()?);
    Some(match degrees {
        0 => 1,
        90 => 6,
        180 => 3,
        270 => 8,
        _ => return None,
    })
}

/// Canon CRW ISO from the CIFF ShotInfo record (0x102a, word 2:
/// 50 × 2^(value / 32 − 4), as in LibRaw); rawler reads no ISO from CRW
/// files, and Adobe's baseline depends on it (10D / 300D ISO 100).
pub fn crw_iso(bytes: &[u8]) -> Option<u32> {
    if bytes.get(6..14) != Some(b"HEAPCCDR") || bytes.get(..2) != Some(b"II") {
        return None;
    }
    let header_len = u32::from_le_bytes(bytes.get(2..6)?.try_into().ok()?) as usize;
    let shot = ciff_find(
        bytes,
        header_len,
        bytes.len().checked_sub(header_len)?,
        0x102a,
        0,
    )?;
    let value = i16::from_le_bytes(bytes.get(shot + 4..shot + 6)?.try_into().ok()?);
    let iso = 50.0 * (f64::from(value) / 32.0 - 4.0).exp2();
    (1.0..1.0e6).contains(&iso).then(|| iso.round() as u32)
}

/// Walks a CIFF heap (directory at its end) for the record's data offset.
fn ciff_find(bytes: &[u8], offset: usize, length: usize, wanted: u16, depth: u32) -> Option<usize> {
    let u32_at = |at: usize| {
        bytes
            .get(at..at + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    };
    let u16_at = |at: usize| {
        bytes
            .get(at..at + 2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
    };
    if depth > 4 || length < 4 {
        return None;
    }
    let table = offset + u32_at(offset + length - 4)? as usize;
    let count = u16_at(table)? as usize;
    for i in 0..count.min(1024) {
        let entry = table + 2 + 10 * i;
        let (tag, size, data) = (
            u16_at(entry)?,
            u32_at(entry + 2)? as usize,
            u32_at(entry + 6)? as usize,
        );
        if tag & 0x4000 != 0 {
            continue; // value stored in the entry itself
        }
        let kind = tag >> 8;
        if tag == wanted {
            return Some(offset + data);
        }
        if (kind == 0x28 || kind == 0x30)
            && let Some(found) = ciff_find(bytes, offset + data, size, wanted, depth + 1)
        {
            return Some(found);
        }
    }
    None
}

/// Fujifilm RAF: the sensor's raw size (height, width) from the CFA header
/// (tag 0x0100), needed to undo rawler's 45° layout of SuperCCD sensors.
pub fn raf_raw_size(bytes: &[u8]) -> Option<(usize, usize)> {
    if bytes.get(..8) != Some(b"FUJIFILM") {
        return None;
    }
    let be32 = |at: usize| {
        bytes
            .get(at..at + 4)
            .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    };
    let (offset, length) = (be32(92)? as usize, be32(96)? as usize);
    let block = bytes.get(offset..offset.checked_add(length)?)?;
    let count = u32::from_be_bytes(block.get(..4)?.try_into().ok()?) as usize;
    let mut at = 4;
    for _ in 0..count.min(4096) {
        let tag = u16::from_be_bytes(block.get(at..at + 2)?.try_into().ok()?);
        let size = u16::from_be_bytes(block.get(at + 2..at + 4)?.try_into().ok()?) as usize;
        let value = block.get(at + 4..at + 4 + size)?;
        if tag == 0x0100 && size >= 4 {
            let height = u16::from_be_bytes([value[0], value[1]]) as usize;
            let width = u16::from_be_bytes([value[2], value[3]]) as usize;
            return (width > 0 && height > 0).then_some((height, width));
        }
        at += 4 + size;
    }
    None
}

#[cfg(test)]
mod format_tests {
    use super::{crw_orientation, fujifilm_standard_white_balance, raf_raw_size};

    #[test]
    fn ciff_image_spec_rotation_maps_to_exif_orientation() {
        // Header (26 bytes), then a heap whose directory holds one 0x1810
        // record of 16 bytes (rotation 270 at offset 12) and the table offset.
        let mut bytes = vec![0u8; 26];
        bytes[..2].copy_from_slice(b"II");
        bytes[2..6].copy_from_slice(&26u32.to_le_bytes());
        bytes[6..14].copy_from_slice(b"HEAPCCDR");
        let mut heap = vec![0u8; 16];
        heap[12..16].copy_from_slice(&270u32.to_le_bytes());
        let table_offset = heap.len() as u32;
        heap.extend_from_slice(&1u16.to_le_bytes());
        heap.extend_from_slice(&0x1810u16.to_le_bytes());
        heap.extend_from_slice(&16u32.to_le_bytes());
        heap.extend_from_slice(&0u32.to_le_bytes());
        heap.extend_from_slice(&table_offset.to_le_bytes());
        bytes.extend_from_slice(&heap);
        assert_eq!(crw_orientation(&bytes), Some(8));
        assert_eq!(crw_orientation(b"II*\0"), None);
    }

    #[test]
    fn fujifilm_standard_references_are_read_from_the_cfa_section() {
        // RAF header with the CFA section at 0x100: a little-endian TIFF
        // whose IFD holds the Fuji directory (0xf000), which holds
        // WB_GRBLevelsStandard (0xf00c) for light A (17) and D65 (21).
        let mut bytes = vec![0u8; 0x100];
        bytes[..8].copy_from_slice(b"FUJIFILM");
        bytes[100..104].copy_from_slice(&0x100u32.to_be_bytes());
        let mut section = Vec::new();
        section.extend_from_slice(b"II*\0");
        section.extend_from_slice(&8u32.to_le_bytes());
        let entry = |tag: u16, typ: u16, count: u32, value: u32| {
            let mut e = Vec::new();
            e.extend_from_slice(&tag.to_le_bytes());
            e.extend_from_slice(&typ.to_le_bytes());
            e.extend_from_slice(&count.to_le_bytes());
            e.extend_from_slice(&value.to_le_bytes());
            e
        };
        // Root IFD at 8: one entry, then the Fuji IFD at 26.
        section.extend_from_slice(&1u16.to_le_bytes());
        section.extend(entry(0xf000, 13, 1, 26));
        section.extend_from_slice(&0u32.to_le_bytes());
        // Fuji IFD at 26: one entry pointing to the values at 44.
        section.extend_from_slice(&1u16.to_le_bytes());
        section.extend(entry(0xf00c, 4, 8, 44));
        section.extend_from_slice(&0u32.to_le_bytes());
        for v in [302u32, 366, 866, 17, 302, 646, 498, 21] {
            section.extend_from_slice(&v.to_le_bytes());
        }
        bytes.extend_from_slice(&section);
        assert_eq!(
            fujifilm_standard_white_balance(&bytes),
            vec![(17, [302, 366, 866]), (21, [302, 646, 498])]
        );
        assert!(fujifilm_standard_white_balance(b"FUJIFILM").is_empty());
    }

    #[test]
    fn raf_raw_size_is_read_from_the_cfa_header() {
        let mut bytes = vec![0u8; 100];
        bytes[..8].copy_from_slice(b"FUJIFILM");
        bytes[92..96].copy_from_slice(&100u32.to_be_bytes());
        let mut block = vec![0, 0, 0, 1, 0x01, 0x00, 0, 4];
        block.extend_from_slice(&2384u16.to_be_bytes());
        block.extend_from_slice(&2944u16.to_be_bytes());
        bytes[96..100].copy_from_slice(&(block.len() as u32).to_be_bytes());
        bytes.extend_from_slice(&block);
        assert_eq!(raf_raw_size(&bytes), Some((2384, 2944)));
    }
}

/// Fujifilm's dynamic range of a RAF in percent, from the maker notes of
/// its embedded JPEG: DevelopmentDynamicRange (0x1403), else the
/// DynamicRangeSetting (0x1402: 0x200 Wide 1 = 230 %, 0x201 Wide 2 =
/// 400 %), else 400 for "Wide" (0x1400 = 3, the F700's SuperCCD SR mode).
pub fn fujifilm_dynamic_range(path: &str) -> Option<u32> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path).ok()?;
    let mut header = [0u8; 100];
    file.read_exact(&mut header).ok()?;
    if &header[..8] != b"FUJIFILM" {
        return None;
    }
    let be32 = |b: &[u8]| u32::from_be_bytes([b[0], b[1], b[2], b[3]]);
    let (offset, length) = (be32(&header[84..88]) as u64, be32(&header[88..92]) as usize);
    if !(16..=(64 << 20)).contains(&length) {
        return None;
    }
    file.seek(SeekFrom::Start(offset)).ok()?;
    let mut jpeg = vec![0u8; length];
    file.read_exact(&mut jpeg).ok()?;
    let exif = exif::Reader::new()
        .read_from_container(&mut std::io::Cursor::new(&jpeg))
        .ok()?;
    let note = match &exif
        .get_field(exif::Tag::MakerNote, exif::In::PRIMARY)?
        .value
    {
        exif::Value::Undefined(bytes, _) => bytes.clone(),
        _ => return None,
    };
    fujifilm_dynamic_range_from_maker_note(&note)
}

fn fujifilm_dynamic_range_from_maker_note(note: &[u8]) -> Option<u32> {
    if !note.starts_with(b"FUJIFILM") {
        return None;
    }
    let le16 = |at: usize| {
        note.get(at..at + 2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
    };
    let le32 = |at: usize| {
        note.get(at..at + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    };
    let ifd = le32(8)? as usize;
    let count = le16(ifd)? as usize;
    let (mut dynamic_range, mut setting, mut development) = (None, None, None);
    for i in 0..count.min(512) {
        let at = ifd + 2 + 12 * i;
        let (tag, kind) = (le16(at)?, le16(at + 2)?);
        if kind != 3 {
            continue;
        }
        let value = le16(at + 8)?;
        match tag {
            0x1400 => dynamic_range = Some(value),
            0x1402 => setting = Some(value),
            0x1403 => development = Some(value),
            _ => {}
        }
    }
    development
        .map(u32::from)
        .filter(|dr| *dr >= 100)
        .or(match setting {
            Some(0x200) => Some(230),
            Some(0x201) => Some(400),
            Some(0x100) => Some(100),
            _ => None,
        })
        .or((dynamic_range == Some(3)).then_some(400))
        .or((dynamic_range == Some(1)).then_some(100))
}

#[cfg(test)]
mod fujifilm_dynamic_range_tests {
    use super::fujifilm_dynamic_range_from_maker_note;

    fn note(entries: &[(u16, u16)]) -> Vec<u8> {
        let mut n = b"FUJIFILM".to_vec();
        n.extend_from_slice(&12u32.to_le_bytes());
        n.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        for (tag, value) in entries {
            n.extend_from_slice(&tag.to_le_bytes());
            n.extend_from_slice(&3u16.to_le_bytes());
            n.extend_from_slice(&1u32.to_le_bytes());
            n.extend_from_slice(&value.to_le_bytes());
            n.extend_from_slice(&[0, 0]);
        }
        n
    }

    #[test]
    fn dynamic_range_tags_are_read_in_order_of_precedence() {
        // S5 Pro: Wide, Manual, DR 400.
        assert_eq!(
            fujifilm_dynamic_range_from_maker_note(&note(&[
                (0x1400, 3),
                (0x1402, 1),
                (0x1403, 400)
            ])),
            Some(400)
        );
        // S3 Pro: Wide 2 (400 %).
        assert_eq!(
            fujifilm_dynamic_range_from_maker_note(&note(&[(0x1400, 3), (0x1402, 0x201)])),
            Some(400)
        );
        // F700: only "Wide".
        assert_eq!(
            fujifilm_dynamic_range_from_maker_note(&note(&[(0x1400, 3)])),
            Some(400)
        );
        // Standard.
        assert_eq!(
            fujifilm_dynamic_range_from_maker_note(&note(&[(0x1400, 1)])),
            Some(100)
        );
        assert_eq!(fujifilm_dynamic_range_from_maker_note(b"NIKON\0"), None);
    }
}

/// Phase One's camera orientation (tag 0x100 of the IIQ directory that
/// follows the TIFF header at offset 8) as an EXIF orientation, when it
/// says the camera was rotated: H 25, P20+ and P25+ backs write "normal" in
/// EXIF and the rotation only here, which Camera Raw follows.
pub fn phase_one_orientation(bytes: &[u8]) -> Option<u16> {
    const BASE: usize = 8;
    if bytes.get(BASE..BASE + 4)? != b"IIII" {
        return None;
    }
    let le32 = |at: usize| {
        bytes
            .get(at..at + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    };
    if le32(BASE + 4)? >> 8 != 0x0052_6177 {
        return None;
    }
    let directory = BASE + le32(BASE + 8)? as usize;
    let entries = le32(directory)? as usize;
    (0..entries.min(4096)).find_map(|i| {
        let at = directory + 8 + 16 * i;
        (le32(at)? == 0x100).then(|| match le32(at + 12)? & 3 {
            1 => Some(6),
            2 => Some(8),
            3 => Some(3),
            _ => None,
        })?
    })
}

#[cfg(test)]
mod phase_one_orientation_tests {
    use super::phase_one_orientation;

    fn iiq(orientation: u32) -> Vec<u8> {
        let mut b = b"II*\0\0\0\0\0IIII".to_vec();
        b.extend_from_slice(&0x5261_7743u32.to_le_bytes());
        b.extend_from_slice(&16u32.to_le_bytes()); // directory at base + 16
        b.resize(8 + 16, 0);
        b.extend_from_slice(&1u32.to_le_bytes()); // one entry
        b.extend_from_slice(&0u32.to_le_bytes());
        for v in [0x100u32, 1, 4, orientation] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        b
    }

    #[test]
    fn camera_orientation_maps_to_exif() {
        assert_eq!(phase_one_orientation(&iiq(1)), Some(6));
        assert_eq!(phase_one_orientation(&iiq(2)), Some(8));
        assert_eq!(phase_one_orientation(&iiq(3)), Some(3));
        assert_eq!(phase_one_orientation(&iiq(0)), None);
        assert_eq!(phase_one_orientation(b"II*\0\0\0\0\0MMMM"), None);
    }
}
