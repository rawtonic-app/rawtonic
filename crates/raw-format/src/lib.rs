//! Raw TIFFs: cameras that wrote their raw files with a `.tif` extension
//! (Phase One backs writing IIQ as TIFF, Canon EOS-1D/1Ds, Kodak DCS and the
//! Kodak-built Canon D2000). Their first TIFF image is a small RGB
//! thumbnail, so an ordinary image reader opens them as a 200-pixel picture
//! (notes: benchmark-results/rawc1-2026-11-18). [`classify`] looks at the
//! file's content to tell them from the RGB TIFFs editors and scanners write.

pub mod camera_names;
mod dng_sensor;
pub mod extensions;
pub use dng_sensor::{SensorKind, dng_sensor, dng_sensor_reader};

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::SystemTime;

use rawler::formats::tiff::GenericTiffReader;
use rawler::formats::tiff::reader::TiffReader;
use rawler::rawsource::RawSource;
use rawler::tags::{DngTag, TiffCommonTag};

/// What a TIFF file holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TiffKind {
    /// An RGB, grayscale or other raster image (or not a TIFF at all).
    Raster,
    /// Camera raw data identified by the container or decoder.
    Raw,
    /// Camera raw data whose decoder reported an error during classification.
    /// It must fail as an unsupported RAW instead of opening as a thumbnail.
    UnsupportedRaw,
}

impl TiffKind {
    /// Whether the file belongs on the RAW path (developed, or refused with
    /// the decoder's error).
    pub fn is_raw(self) -> bool {
        !matches!(self, TiffKind::Raster)
    }
}

/// TIFF photometric interpretations that only camera raw data uses
/// (`CFA` from TIFF/EP, `LinearRaw` from DNG).
const RAW_PHOTOMETRICS: [u32; 2] = [32803, 34892];

/// Positive evidence for overriding a camera RAW filename. Unknown containers
/// retain the filename's RAW route, including its embedded-preview fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RasterEvidence {
    Raster,
    Unknown,
}

/// Unlike [`classify`], this never treats a decoder failure as raster evidence.
/// It inspects TIFF directories only; no camera decoder is invoked.
pub fn raster_evidence(path: &Path) -> RasterEvidence {
    if rawtonic_fs::is_cloud_placeholder(path) {
        return RasterEvidence::Unknown;
    }
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let Ok(file) = std::fs::File::open(path) else {
            return RasterEvidence::Unknown;
        };
        // Read only directory data, without mapping a removable source.
        raster_evidence_reader(&mut std::io::BufReader::new(file))
    }))
    .unwrap_or(RasterEvidence::Unknown)
}

#[cfg(test)]
fn raster_evidence_inner(bytes: &[u8]) -> RasterEvidence {
    raster_evidence_reader(&mut std::io::Cursor::new(bytes))
}

fn raster_evidence_reader<R: std::io::Read + std::io::Seek>(reader: &mut R) -> RasterEvidence {
    use TiffCommonTag as Tag;
    let mut bytes = [0; 12];
    if reader.read_exact(&mut bytes).is_err() {
        return RasterEvidence::Unknown;
    }
    if !(bytes.starts_with(b"II*\0") || bytes.starts_with(b"MM\0*"))
        || bytes.get(8..12) == Some(b"IIII".as_slice())
        || bytes.get(8..12) == Some(b"CR\x02\0".as_slice())
    {
        return RasterEvidence::Unknown;
    }
    if reader.seek(std::io::SeekFrom::Start(0)).is_err() {
        return RasterEvidence::Unknown;
    }
    let Ok(tiff) = GenericTiffReader::new(reader, 0, 0, None, &[]) else {
        return RasterEvidence::Unknown;
    };
    let with_tag = |tag| {
        tiff.chains()
            .iter()
            .flat_map(|root| root.find_ifds_with_tag(tag))
            .collect::<Vec<_>>()
    };
    let raw_tags = [
        Tag::DNGVersion,
        Tag::CFAPattern,
        Tag::CFARepeatPatternDim,
        Tag::Cr2Id,
        Tag::Cr2OldOffset,
        Tag::NefMeta1,
        Tag::NefMeta2,
        Tag::NefKey,
        Tag::SonyOffset,
        Tag::SonyLength,
        Tag::SonyKey,
        Tag::KodakIFD,
    ];
    if raw_tags.into_iter().any(|tag| !with_tag(tag).is_empty())
        || with_tag(Tag::PhotometricInt).iter().any(|ifd| {
            ifd.get_entry(Tag::PhotometricInt)
                .is_some_and(|entry| RAW_PHOTOMETRICS.contains(&entry.value.force_u32(0)))
        })
    {
        return RasterEvidence::Unknown;
    }
    let number = |ifd: &rawler::formats::tiff::IFD, tag: Tag| {
        ifd.get_entry(tag).map(|entry| entry.value.force_u32(0))
    };
    let images = with_tag(Tag::ImageWidth);
    let full: Vec<_> = images
        .iter()
        .filter(|ifd| number(ifd, Tag::NewSubFileType).unwrap_or(0) & 1 == 0)
        .collect();
    // Incomplete image directories cannot prove which image is full size.
    if full.iter().any(|ifd| {
        number(ifd, Tag::ImageWidth).unwrap_or(0) == 0
            || number(ifd, Tag::ImageLength).unwrap_or(0) == 0
    }) {
        return RasterEvidence::Unknown;
    }
    let area = |ifd: &rawler::formats::tiff::IFD| {
        u64::from(number(ifd, Tag::ImageWidth).unwrap_or(0))
            * u64::from(number(ifd, Tag::ImageLength).unwrap_or(0))
    };
    let largest = full.iter().map(|ifd| area(ifd)).max().unwrap_or(0);
    if largest == 0 {
        return RasterEvidence::Unknown;
    }
    let plain = full.iter().filter(|ifd| area(ifd) == largest).all(|image| {
        match (
            number(image, Tag::PhotometricInt),
            number(image, Tag::SamplesPerPixel),
        ) {
            (Some(2 | 6), Some(samples)) => samples >= 3,
            (Some(0 | 1), Some(1)) => true,
            _ => false,
        }
    });
    if plain {
        RasterEvidence::Raster
    } else {
        RasterEvidence::Unknown
    }
}

/// Classifies the TIFF at `path` by its content. Files that cannot be read
/// count as [`TiffKind::Raster`], so the image path reports the error as
/// before.
pub fn classify(path: &Path) -> TiffKind {
    if rawtonic_fs::is_cloud_placeholder(path) {
        return TiffKind::Raster;
    }
    classify_local(path)
}

fn classify_local(path: &Path) -> TiffKind {
    match RawSource::new_lazy(path) {
        Ok(source) => classify_source(&source),
        Err(_) => {
            if std::fs::File::open(path)
                .is_ok_and(|mut file| dng_sensor::read_dng_header(&mut file))
            {
                TiffKind::Raw
            } else {
                TiffKind::Raster
            }
        }
    }
}

/// A cached kind with the file size and modification time it was found for.
type Stamped = (u64, Option<SystemTime>, TiffKind);

/// [`classify`] with a cache keyed by path, size and modification time, for
/// callers that ask about the same file many times (listing, thumbnails,
/// develop and export all route by it).
pub fn classify_cached(path: &Path) -> TiffKind {
    static CACHE: LazyLock<Mutex<HashMap<PathBuf, Stamped>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));
    const CAPACITY: usize = 16_384;

    let Ok(meta) = std::fs::metadata(path) else {
        return TiffKind::Raster;
    };
    // Listing a folder must leave evicted files offline. Do not cache
    // this answer: downloading a placeholder can preserve size and mtime.
    if rawtonic_fs::is_cloud_placeholder_metadata(&meta) {
        return TiffKind::Raster;
    }
    let stamp = (meta.len(), meta.modified().ok());
    if let Ok(cache) = CACHE.lock()
        && let Some(&(len, modified, kind)) = cache.get(path)
        && (len, modified) == stamp
    {
        return kind;
    }
    let kind = classify_local(path);
    if let Ok(mut cache) = CACHE.lock() {
        if cache.len() >= CAPACITY {
            cache.clear();
        }
        cache.insert(path.to_path_buf(), (stamp.0, stamp.1, kind));
    }
    kind
}

/// [`classify`] for a file already in memory.
pub fn classify_bytes(bytes: &[u8]) -> TiffKind {
    classify_source(&RawSource::new_from_slice(bytes))
}

fn classify_source(source: &RawSource) -> TiffKind {
    // The TIFF parser and the decoders turn most malformed files into
    // errors; a panic on one must not take down a folder listing.
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| classify_tiff(source)))
        .unwrap_or(TiffKind::Raster)
}

fn classify_tiff(source: &RawSource) -> TiffKind {
    let buf = source.buf();
    if !(buf.starts_with(b"II*\0") || buf.starts_with(b"MM\0*")) {
        return TiffKind::Raster;
    }
    if dng_sensor(buf).is_some() {
        return TiffKind::Raw;
    }
    // Phase One writes its IIQ container behind a TIFF header: "IIII" at
    // byte 8 is the IIQ directory's magic.
    let iiq_header = buf.get(8..12) == Some(b"IIII".as_slice());
    let Ok(tiff) = GenericTiffReader::new_with_buffer(buf, 0, 0, None) else {
        return if iiq_header {
            TiffKind::UnsupportedRaw
        } else {
            TiffKind::Raster
        };
    };
    if tiff.chains().is_empty() {
        return if iiq_header {
            TiffKind::UnsupportedRaw
        } else {
            TiffKind::Raster
        };
    }
    let raw_photometric = tiff
        .find_ifds_with_tag(TiffCommonTag::PhotometricInt)
        .iter()
        .filter_map(|ifd| ifd.get_entry(TiffCommonTag::PhotometricInt))
        .any(|entry| RAW_PHOTOMETRICS.contains(&entry.value.force_u32(0)));
    let marked_raw = iiq_header || raw_photometric || tiff.has_entry(DngTag::DNGVersion);
    // Without a camera make no raw decoder applies, and the decoder's last
    // resort (matching the file size) would read the whole file.
    if !marked_raw && !tiff.has_entry(TiffCommonTag::Make) {
        return TiffKind::Raster;
    }
    match rawler::decode_dummy(source) {
        // Editors keep the camera's make and model in the RGB TIFFs they
        // write; the decoder then finds no raw data, or, should it mistake
        // the RGB image for raw data, the file's own RGB image is about as
        // large as that "raw" image. A raw TIFF's RGB images are thumbnails.
        Ok(raw) if marked_raw || !has_large_rgb_image(&tiff, raw.width, raw.height) => {
            TiffKind::Raw
        }
        _ if marked_raw => TiffKind::UnsupportedRaw,
        _ => TiffKind::Raster,
    }
}

/// Whether the TIFF holds an RGB (or YCbCr / palette) image at least half
/// the raw image's width and height.
fn has_large_rgb_image(tiff: &GenericTiffReader, raw_width: usize, raw_height: usize) -> bool {
    tiff.find_ifds_with_tag(TiffCommonTag::ImageWidth)
        .iter()
        .any(|ifd| {
            let photometric = ifd
                .get_entry(TiffCommonTag::PhotometricInt)
                .map(|entry| entry.value.force_u32(0));
            let colour = matches!(photometric, Some(2 | 3 | 6));
            let width = ifd
                .get_entry(TiffCommonTag::ImageWidth)
                .map_or(0, |entry| entry.value.force_usize(0));
            let height = ifd
                .get_entry(TiffCommonTag::ImageLength)
                .map_or(0, |entry| entry.value.force_usize(0));
            colour && width * 2 >= raw_width && height * 2 >= raw_height
        })
}

#[cfg(test)]
#[path = "raw_tiff_tests.rs"]
mod tests;
