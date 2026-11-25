//! Raw TIFFs: cameras that wrote their raw files with a `.tif` extension
//! (Phase One backs writing IIQ as TIFF, Canon EOS-1D/1Ds, Kodak DCS and the
//! Kodak-built Canon D2000). Their first TIFF image is a small RGB
//! thumbnail, so an ordinary image reader opens them as a 200-pixel picture
//! (notes: benchmark-results/rawc1-2026-11-18). [`classify`] looks at the
//! file's content to tell them from the RGB TIFFs editors and scanners write.

pub mod camera_names;
mod dng_sensor;
pub mod extensions;
pub use dng_sensor::{SensorKind, dng_sensor};

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
    /// Camera raw data the decoder can develop.
    Raw,
    /// Unmistakably camera raw data (a Phase One IIQ header, a colour filter
    /// array image) the decoder cannot read: it must fail as an unsupported
    /// RAW, never open as its thumbnail.
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
