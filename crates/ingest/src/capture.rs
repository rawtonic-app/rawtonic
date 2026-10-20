//! What a photograph's own metadata says about it, for the naming tokens:
//! capture date and time, camera, lens and exposure from its EXIF, and
//! rating, label and descriptive fields from an XMP sidecar beside it on
//! the card. Read from the file's header only (no decode), with plain reads
//! rather than a memory map: a card pulled while a map of it is read kills
//! the process, a read only fails.
//!
//! EXIF comes from the RAW decoder the app develops with (rawler, which
//! knows the maker formats: TIFF-based RAWs, CR3, RAF, CRW, …) and, for
//! everything it does not read or leaves empty (JPEG, HEIF, PNG, TIFF), the
//! EXIF reader the app shows metadata with (kamadak-exif). The capture
//! date falls back as `rawtonic_naming::DateSource` documents: EXIF
//! DateTimeOriginal → DateTimeDigitized → the EXIF file date → the file's
//! modification time → the time of the import.
use rawtonic_naming::{Capture, DateSource};
use std::{
    collections::HashMap,
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::{Arc, LazyLock, Mutex},
    time::SystemTime,
};

/// How much of a file is read for its metadata, step by step: the first
/// step holds the EXIF of nearly every format (it sits at the start of
/// TIFF-based RAWs, CR3's `moov`, a RAF's embedded JPEG, a JPEG's APP1); a
/// file whose step holds no capture date is read further, at most
/// `WHOLE_FILE_LIMIT` bytes in all.
const READ_STEPS: [u64; 2] = [1 << 20, 16 << 20];
const WHOLE_FILE_LIMIT: u64 = 512 << 20;

/// Capture facts of `path` read from its header; never fails (a file
/// without metadata gets its modification time as date).
pub fn read_capture(path: &Path) -> Capture {
    let mut capture = Capture::default();
    if let Ok(mut file) = fs::File::open(path)
        && let Ok(meta) = file.metadata()
        && meta.is_file()
    {
        let len = meta.len();
        let mut buffer = Vec::new();
        for limit in READ_STEPS
            .into_iter()
            .chain([WHOLE_FILE_LIMIT])
            .map(|step| step.min(len))
        {
            if limit <= buffer.len() as u64 && !buffer.is_empty() {
                continue;
            }
            let wanted = limit - buffer.len() as u64;
            if (&mut file).take(wanted).read_to_end(&mut buffer).is_err() {
                break;
            }
            let read = from_bytes(&buffer);
            let done = read.captured.is_some() || buffer.len() as u64 >= len;
            capture = read;
            if done {
                break;
            }
        }
    }
    if capture.captured.is_none() {
        let modified = fs::metadata(path).and_then(|m| m.modified());
        (capture.captured, capture.date_source) = match modified {
            Ok(time) => (Some(local(time)), DateSource::Modified),
            Err(_) => (Some(chrono::Local::now().naive_local()), DateSource::Now),
        };
        capture.subsec = None;
        capture.offset = None;
    }
    capture
}

/// What `path`'s EXIF says for the library's search (`rawtonic_catalog::
/// CaptureFacts`); `None` when the file cannot be read now. A capture time
/// only from the file's own metadata, never its modification time.
pub fn capture_facts(path: &Path) -> Option<rawtonic_catalog::CaptureFacts> {
    fs::File::open(path)
        .ok()
        .filter(|f| f.metadata().is_ok_and(|m| m.is_file()))?;
    let capture = read_capture(path);
    let dated = matches!(
        capture.date_source,
        DateSource::Original | DateSource::Digitized | DateSource::FileDate
    );
    Some(rawtonic_catalog::CaptureFacts {
        captured: capture
            .captured
            .filter(|_| dated)
            .map(|time| time.format("%Y-%m-%d %H:%M:%S").to_string()),
        make: capture.make,
        model: capture.model,
        lens: capture.lens,
        focal_length: capture.focal_length,
        aperture: capture.aperture,
        iso: capture.iso,
        exposure: capture.exposure.map(|(n, d)| f64::from(n) / f64::from(d)),
    })
}

fn local(time: std::time::SystemTime) -> chrono::NaiveDateTime {
    chrono::DateTime::<chrono::Local>::from(time).naive_local()
}

/// A date as EXIF writes it (`2026:10:01 14:37:08`; also with dashes or a
/// `T`, and a date alone as midnight). Placeholder dates (`0000:00:00`) are
/// none.
pub fn parse_exif_date(text: &str) -> Option<chrono::NaiveDateTime> {
    let text = text.trim().trim_end_matches('\0').trim();
    let normal: String = text
        .chars()
        .take(19)
        .enumerate()
        .map(|(i, c)| match (i, c) {
            (4 | 7, ':' | '-') => '-',
            (10, 'T') => ' ',
            _ => c,
        })
        .collect();
    chrono::NaiveDateTime::parse_from_str(&normal, "%Y-%m-%d %H:%M:%S")
        .ok()
        .or_else(|| {
            chrono::NaiveDate::parse_from_str(normal.get(..10)?, "%Y-%m-%d")
                .ok()
                .and_then(|d| d.and_hms_opt(0, 0, 0))
        })
}

fn clean(value: Option<&str>) -> Option<String> {
    value
        .map(|v| v.trim_matches(|c: char| c.is_whitespace() || c == '\0'))
        .filter(|v| !v.is_empty())
        .map(String::from)
}

fn digits(value: Option<&str>) -> Option<String> {
    clean(value).filter(|v| v.chars().all(|c| c.is_ascii_digit()))
}

/// One capture date candidate: its source, text, sub-seconds and offset.
type DateCandidate = (DateSource, Option<String>, Option<String>, Option<String>);

/// Picks the best date of `candidates` (by source order).
fn best_date(capture: &mut Capture, candidates: impl IntoIterator<Item = DateCandidate>) {
    let mut found: Vec<_> = candidates
        .into_iter()
        .filter_map(|(source, text, subsec, offset)| {
            Some((source, parse_exif_date(text.as_deref()?)?, subsec, offset))
        })
        .collect();
    found.sort_by_key(|(source, ..)| *source as u8);
    if let Some((source, date, subsec, offset)) = found.into_iter().next()
        && (capture.captured.is_none() || (source as u8) < (capture.date_source as u8))
    {
        capture.captured = Some(date);
        capture.date_source = source;
        capture.subsec = digits(subsec.as_deref());
        capture.offset = clean(offset.as_deref());
    }
}

/// Capture facts of a file's first bytes (`bytes` may be a prefix); dates
/// only from its metadata (`captured` stays empty without one).
pub fn from_bytes(bytes: &[u8]) -> Capture {
    let mut capture = Capture::default();
    if let Some(metadata) = raw_metadata(bytes) {
        apply_raw(&mut capture, &metadata);
    }
    if let Ok(exif) = exif::Reader::new().read_from_container(&mut std::io::Cursor::new(bytes)) {
        apply_exif(&mut capture, &exif);
    }
    if capture.captured.is_none()
        && let Some(captured) = crw_capture_time(bytes)
    {
        capture.captured = Some(captured);
        capture.date_source = DateSource::Original;
    }
    capture
}

/// A Canon CRW's capture time, which it keeps outside EXIF: the CIFF
/// TimeStamp record (0x180e, its first word the local time as seconds since
/// 1970). Neither EXIF reader knows CIFF.
fn crw_capture_time(bytes: &[u8]) -> Option<chrono::NaiveDateTime> {
    if bytes.get(6..14) != Some(b"HEAPCCDR") || bytes.get(..2) != Some(b"II") {
        return None;
    }
    let header = u32::from_le_bytes(bytes.get(2..6)?.try_into().ok()?) as usize;
    let at = ciff_find(bytes, header, bytes.len().checked_sub(header)?, 0x180e, 0)?;
    let seconds = u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?);
    (seconds > 0)
        .then(|| chrono::DateTime::from_timestamp(i64::from(seconds), 0))
        .flatten()
        .map(|time| time.naive_utc())
}

/// Walks a CIFF heap (its directory at its end) for a record's data offset,
/// as `rawtonic_rawdecode::capture_info` does for the CRW ISO.
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
    let table = offset.checked_add(u32_at(offset + length - 4)? as usize)?;
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
        if tag == wanted {
            return Some(offset + data);
        }
        let kind = tag >> 8;
        if (kind == 0x28 || kind == 0x30)
            && let Some(found) = ciff_find(bytes, offset + data, size, wanted, depth + 1)
        {
            return Some(found);
        }
    }
    None
}

/// The RAW decoder's metadata; `None` for files it does not read, and
/// when it panics on a damaged or cut-off file.
fn raw_metadata(bytes: &[u8]) -> Option<rawler::decoders::RawMetadata> {
    std::panic::catch_unwind(|| {
        let source = rawler::rawsource::RawSource::new_from_slice(bytes);
        let decoder = rawler::get_decoder(&source).ok()?;
        decoder
            .raw_metadata(&source, &rawler::decoders::RawDecodeParams::default())
            .ok()
    })
    .ok()
    .flatten()
}

fn rational(value: Option<rawler::formats::tiff::Rational>) -> Option<f64> {
    value
        .filter(|r| r.d != 0 && r.n != 0)
        .map(|r| r.n as f64 / r.d as f64)
}

fn apply_raw(capture: &mut Capture, metadata: &rawler::decoders::RawMetadata) {
    let exif = &metadata.exif;
    best_date(
        capture,
        [
            (
                DateSource::Original,
                exif.date_time_original.clone(),
                exif.sub_sec_time_original.clone(),
                exif.offset_time_original.clone(),
            ),
            (
                DateSource::Digitized,
                exif.create_date.clone(),
                exif.sub_sec_time_digitized.clone(),
                exif.offset_time_digitized.clone(),
            ),
            (
                DateSource::FileDate,
                exif.modify_date.clone(),
                exif.sub_sec_time.clone(),
                exif.offset_time.clone(),
            ),
        ],
    );
    capture.make = capture.make.take().or(clean(Some(&metadata.make)));
    capture.model = capture.model.take().or(clean(Some(&metadata.model)));
    capture.serial = capture
        .serial
        .take()
        .or(clean(exif.serial_number.as_deref()));
    capture.lens = capture
        .lens
        .take()
        .or(clean(exif.lens_model.as_deref()))
        .or_else(|| {
            metadata
                .lens
                .as_ref()
                .and_then(|lens| clean(Some(&format!("{} {}", lens.lens_make, lens.lens_model))))
        });
    capture.iso = capture.iso.or(exif
        .iso_speed_ratings
        .map(u32::from)
        .filter(|iso| *iso > 0)
        .or(exif.iso_speed)
        .or(exif.recommended_exposure_index)
        .filter(|iso| *iso > 0));
    capture.focal_length = capture.focal_length.or(rational(exif.focal_length));
    capture.aperture = capture.aperture.or(rational(exif.fnumber)).or_else(|| {
        // ApertureValue is APEX: f-number = 2^(Av/2).
        rational(exif.aperture_value).map(|av| (2f64.powf(av / 2.0) * 10.0).round() / 10.0)
    });
    capture.exposure = capture.exposure.or(exif
        .exposure_time
        .filter(|r| r.n != 0 && r.d != 0)
        .map(|r| (r.n, r.d)));
    capture.creator = capture.creator.take().or(clean(exif.artist.as_deref()));
    capture.copyright = capture
        .copyright
        .take()
        .or(clean(exif.copyright.as_deref()));
}

fn ascii(exif: &exif::Exif, tag: exif::Tag) -> Option<String> {
    match &exif.get_field(tag, exif::In::PRIMARY)?.value {
        exif::Value::Ascii(values) => {
            clean(values.first().and_then(|v| std::str::from_utf8(v).ok()))
        }
        _ => None,
    }
}

fn exif_rational(exif: &exif::Exif, tag: exif::Tag) -> Option<(u32, u32)> {
    match &exif.get_field(tag, exif::In::PRIMARY)?.value {
        exif::Value::Rational(values) => values
            .first()
            .filter(|r| r.num != 0 && r.denom != 0)
            .map(|r| (r.num, r.denom)),
        _ => None,
    }
}

fn apply_exif(capture: &mut Capture, exif: &exif::Exif) {
    use exif::Tag;
    best_date(
        capture,
        [
            (
                DateSource::Original,
                ascii(exif, Tag::DateTimeOriginal),
                ascii(exif, Tag::SubSecTimeOriginal),
                ascii(exif, Tag::OffsetTimeOriginal),
            ),
            (
                DateSource::Digitized,
                ascii(exif, Tag::DateTimeDigitized),
                ascii(exif, Tag::SubSecTimeDigitized),
                ascii(exif, Tag::OffsetTimeDigitized),
            ),
            (
                DateSource::FileDate,
                ascii(exif, Tag::DateTime),
                ascii(exif, Tag::SubSecTime),
                ascii(exif, Tag::OffsetTime),
            ),
        ],
    );
    capture.make = capture.make.take().or_else(|| ascii(exif, Tag::Make));
    capture.model = capture.model.take().or_else(|| ascii(exif, Tag::Model));
    capture.serial = capture
        .serial
        .take()
        .or_else(|| ascii(exif, Tag::BodySerialNumber));
    capture.lens = capture.lens.take().or_else(|| ascii(exif, Tag::LensModel));
    capture.iso = capture.iso.or_else(|| {
        exif.get_field(Tag::PhotographicSensitivity, exif::In::PRIMARY)
            .and_then(|f| f.value.get_uint(0))
            .filter(|iso| *iso > 0)
    });
    let ratio = |(n, d): (u32, u32)| n as f64 / d as f64;
    capture.focal_length = capture
        .focal_length
        .or_else(|| exif_rational(exif, Tag::FocalLength).map(ratio));
    capture.aperture = capture
        .aperture
        .or_else(|| exif_rational(exif, Tag::FNumber).map(ratio));
    capture.exposure = capture
        .exposure
        .or_else(|| exif_rational(exif, Tag::ExposureTime));
    capture.creator = capture.creator.take().or_else(|| ascii(exif, Tag::Artist));
    capture.copyright = capture
        .copyright
        .take()
        .or_else(|| ascii(exif, Tag::Copyright));
}

/// Formats whose files are developed images rather than RAWs: a sidecar
/// named after the stem alone (`IMG_0001.xmp`) belongs to the RAW of a
/// RAW+JPEG pair, not to them.
const IMAGE_EXTENSIONS: &[&str] = &[
    "jpg", "jpeg", "heic", "heif", "hif", "png", "webp", "avif", "jxl", "tif", "tiff",
];

/// The XMP sidecar of `photo` on its card: `NAME.EXT.xmp` (unambiguous),
/// else `NAME.xmp` (Lightroom's and most tools' form). A developed image
/// (JPEG, HEIF, …) takes `NAME.xmp` only when no other file shares its
/// name, since in a RAW+JPEG pair that sidecar is the RAW's. Names compare
/// ignoring ASCII case (`.XMP`, `name.arw.xmp`: cards, cameras and other
/// programs mix them, and case-insensitive volumes do not tell them apart);
/// only regular files count. Two files that differ only in case for the
/// same form (`A.xmp` and `A.XMP` on a case-sensitive volume) are an error:
/// which one holds the photographer's metadata cannot be told.
pub fn card_sidecar(photo: &Path) -> Result<Option<PathBuf>, String> {
    let (Some(name), Some(stem), Some(parent)) = (
        photo.file_name().and_then(|n| n.to_str()),
        photo.file_stem().and_then(|s| s.to_str()),
        photo.parent(),
    ) else {
        return Ok(None);
    };
    let extension = photo.extension().and_then(|e| e.to_str());
    let parent = if parent.as_os_str().is_empty() {
        Path::new(".")
    } else {
        parent
    };
    let Some(folder) = folder_files(parent) else {
        return Ok(None);
    };
    let one = |wanted: &str| -> Result<Option<PathBuf>, String> {
        match folder
            .by_name
            .get(&wanted.to_ascii_lowercase())
            .map(Vec::as_slice)
        {
            None | Some([]) => Ok(None),
            Some([file]) => Ok(Some(photo.with_file_name(file))),
            Some([a, b, ..]) => Err(format!(
                "Two sidecars that differ only in letter case, {} and {}: neither was copied",
                photo.with_file_name(a).display(),
                photo.with_file_name(b).display()
            )),
        }
    };
    let full = one(&format!("{name}.xmp"))?;
    if full.is_some() || extension.is_none() {
        return Ok(full);
    }
    let Some(short) = one(&format!("{stem}.xmp"))? else {
        return Ok(None);
    };
    if extension.is_some_and(is_developed_image) {
        // Another file of its name (not a sidecar) besides itself.
        let named = folder
            .stems
            .get(&stem.to_ascii_lowercase())
            .copied()
            .unwrap_or(0);
        let itself = usize::from(folder.by_name.contains_key(&name.to_ascii_lowercase()));
        if named > itself {
            return Ok(None);
        }
    }
    Ok(Some(short))
}

/// A folder's regular files (links and folders left out), by name and by
/// stem ignoring ASCII case.
#[derive(Default)]
struct FolderFiles {
    /// Lower-cased name → the names of that spelling.
    by_name: HashMap<String, Vec<String>>,
    /// Lower-cased stem → how many files (not `.xmp`) have it.
    stems: HashMap<String, usize>,
}

/// `folder`'s files (`FolderFiles`), listed once per version of the folder
/// (its modification time): an import of a card folder's photographs reads
/// it once, not once per photograph. `None` when it cannot be listed.
fn folder_files(folder: &Path) -> Option<Arc<FolderFiles>> {
    type Listed = HashMap<PathBuf, (SystemTime, Arc<FolderFiles>)>;
    static LISTED: LazyLock<Mutex<Listed>> = LazyLock::new(Default::default);
    let modified = fs::metadata(folder).ok()?.modified().ok();
    let mut listed = LISTED.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(modified) = modified
        && let Some((at, files)) = listed.get(folder)
        && *at == modified
    {
        return Some(files.clone());
    }
    let mut files = FolderFiles::default();
    for entry in fs::read_dir(folder).ok()?.flatten() {
        if !entry.file_type().is_ok_and(|t| t.is_file()) {
            continue;
        }
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        let path = Path::new(&name);
        let sidecar = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("xmp"));
        if !sidecar && let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
            *files.stems.entry(stem.to_ascii_lowercase()).or_default() += 1;
        }
        files
            .by_name
            .entry(name.to_ascii_lowercase())
            .or_default()
            .push(name);
    }
    let files = Arc::new(files);
    if let Some(modified) = modified {
        if listed.len() >= 64 {
            listed.clear();
        }
        listed.insert(folder.to_path_buf(), (modified, files.clone()));
    }
    Some(files)
}

/// Whether `extension` is a developed image's (JPEG, HEIF, TIFF, …), not
/// a RAW's.
pub fn is_developed_image(extension: &str) -> bool {
    IMAGE_EXTENSIONS
        .iter()
        .any(|known| extension.eq_ignore_ascii_case(known))
}

/// Fills the descriptive tokens from a card sidecar's metadata: rating,
/// colour label and title, and creator and copyright over the EXIF ones
/// (what the photographer set beats the camera's preset).
pub fn apply_sidecar(capture: &mut Capture, metadata: &rawtonic_metadata::Metadata) {
    capture.rating = Some(metadata.rating);
    capture.label = clean(Some(&metadata.label)).or(capture.label.take());
    capture.title = clean(Some(&metadata.title)).or(capture.title.take());
    capture.creator = clean(Some(&metadata.creator)).or(capture.creator.take());
    capture.copyright = clean(Some(&metadata.copyright)).or(capture.copyright.take());
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A little-endian TIFF/EXIF block: IFD0 with Make, Model, DateTime
    /// and an EXIF sub-IFD with the exposure and the original date.
    pub(crate) fn tiff_exif(original: &str) -> Vec<u8> {
        enum V<'a> {
            Ascii(&'a str),
            Short(u16),
            Rational(u32, u32),
            Long(u32),
        }
        fn ifd(entries: &[(u16, V<'_>)], start: u32) -> Vec<u8> {
            // Entries, next-IFD 0, then the values that do not fit inline.
            let mut out = (entries.len() as u16).to_le_bytes().to_vec();
            let mut data = Vec::new();
            let data_start = start + 2 + 12 * entries.len() as u32 + 4;
            for (tag, value) in entries {
                out.extend(tag.to_le_bytes());
                let (kind, count, bytes): (u16, u32, Vec<u8>) = match value {
                    V::Ascii(s) => {
                        let mut b = s.as_bytes().to_vec();
                        b.push(0);
                        (2, b.len() as u32, b)
                    }
                    V::Short(v) => (3, 1, v.to_le_bytes().to_vec()),
                    V::Long(v) => (4, 1, v.to_le_bytes().to_vec()),
                    V::Rational(n, d) => {
                        let mut b = n.to_le_bytes().to_vec();
                        b.extend(d.to_le_bytes());
                        (5, 1, b)
                    }
                };
                out.extend(kind.to_le_bytes());
                out.extend(count.to_le_bytes());
                if bytes.len() <= 4 {
                    let mut inline = bytes.clone();
                    inline.resize(4, 0);
                    out.extend(inline);
                } else {
                    out.extend((data_start + data.len() as u32).to_le_bytes());
                    data.extend(&bytes);
                    if data.len() % 2 == 1 {
                        data.push(0);
                    }
                }
            }
            out.extend(0u32.to_le_bytes());
            out.extend(data);
            out
        }
        let exif_entries = |_: u32| {
            vec![
                (0x829a, V::Rational(1, 250)),
                (0x829d, V::Rational(28, 10)),
                (0x8827, V::Short(800)),
                (0x9003, V::Ascii(original)),
                (0x9011, V::Ascii("+02:00")),
                (0x920a, V::Rational(35, 1)),
                (0x9291, V::Ascii("042")),
                (0xa431, V::Ascii("012345678901")),
                (0xa434, V::Ascii("Test Lens 35mm F2.8")),
            ]
        };
        // IFD0 size is fixed by its entries; place the EXIF IFD after it.
        let ifd0 = |exif_at: u32| {
            vec![
                (0x010f, V::Ascii("Testmaker")),
                (0x0110, V::Ascii("Model One")),
                (0x0132, V::Ascii("2026:01:02 03:04:05")),
                (0x013b, V::Ascii("A. Photographer")),
                (0x8769, V::Long(exif_at)),
            ]
        };
        let first = ifd(&ifd0(0), 8);
        let exif_at = 8 + first.len() as u32;
        let mut out = b"II*\0".to_vec();
        out.extend(8u32.to_le_bytes());
        out.extend(ifd(&ifd0(exif_at), 8));
        out.extend(ifd(&exif_entries(exif_at), exif_at));
        out
    }

    /// A JPEG with `tiff` as its EXIF segment.
    pub(crate) fn jpeg_with_exif(tiff: &[u8]) -> Vec<u8> {
        let mut out = vec![0xFF, 0xD8, 0xFF, 0xE1];
        out.extend(((tiff.len() + 8) as u16).to_be_bytes());
        out.extend(b"Exif\0\0");
        out.extend(tiff);
        out.extend([0xFF, 0xD9]);
        out
    }

    #[test]
    fn exif_dates_parse_in_their_written_forms() {
        let at = |s| parse_exif_date(s).map(|d| d.to_string());
        assert_eq!(
            at("2026:10:01 14:37:08").as_deref(),
            Some("2026-10-01 14:37:08")
        );
        assert_eq!(
            at("2026-10-01T14:37:08+02:00").as_deref(),
            Some("2026-10-01 14:37:08")
        );
        assert_eq!(at("2026:10:01\0").as_deref(), Some("2026-10-01 00:00:00"));
        assert_eq!(at("0000:00:00 00:00:00"), None);
        assert_eq!(at("    "), None);
    }

    #[test]
    fn reads_exif_facts_from_a_jpeg_header() {
        let capture = from_bytes(&jpeg_with_exif(&tiff_exif("2025:12:24 18:30:15")));
        assert_eq!(
            capture.captured.map(|d| d.to_string()).as_deref(),
            Some("2025-12-24 18:30:15")
        );
        assert_eq!(capture.date_source, DateSource::Original);
        assert_eq!(capture.subsec.as_deref(), Some("042"));
        assert_eq!(capture.offset.as_deref(), Some("+02:00"));
        assert_eq!(capture.make.as_deref(), Some("Testmaker"));
        assert_eq!(capture.model.as_deref(), Some("Model One"));
        assert_eq!(capture.serial.as_deref(), Some("012345678901"));
        assert_eq!(capture.lens.as_deref(), Some("Test Lens 35mm F2.8"));
        assert_eq!(capture.iso, Some(800));
        assert_eq!(capture.focal_length, Some(35.0));
        assert_eq!(capture.aperture, Some(2.8));
        assert_eq!(capture.exposure, Some((1, 250)));
        assert_eq!(capture.creator.as_deref(), Some("A. Photographer"));
    }

    /// Without DateTimeOriginal the camera's file date is next; without
    /// any EXIF date, the file's modification time.
    #[test]
    fn the_date_falls_back_in_documented_order() {
        let capture = from_bytes(&tiff_exif("not a date"));
        assert_eq!(capture.date_source, DateSource::FileDate);
        assert_eq!(
            capture.captured.map(|d| d.to_string()).as_deref(),
            Some("2026-01-02 03:04:05")
        );
        assert_eq!(
            capture.subsec, None,
            "sub-seconds belong to the original date"
        );

        let d = tempfile::tempdir().unwrap();
        let plain = d.path().join("plain.ARW");
        fs::write(&plain, vec![7u8; 3 << 20]).unwrap();
        let capture = read_capture(&plain);
        assert_eq!(capture.date_source, DateSource::Modified);
        assert!(capture.captured.is_some());
    }

    /// A TIFF-based file whose EXIF is past the first read step is read
    /// further until its capture date is found.
    #[test]
    fn reads_further_when_the_header_holds_no_date() {
        let d = tempfile::tempdir().unwrap();
        let photo = d.path().join("late.jpg");
        let exif = tiff_exif("2024:05:06 07:08:09");
        // A JPEG whose EXIF segment follows 2 MB of another APP segment
        // chain: the first step has no date, the second has.
        let mut bytes = vec![0xFF, 0xD8];
        for _ in 0..40 {
            bytes.extend([0xFF, 0xE2]);
            bytes.extend(60_000u16.to_be_bytes());
            bytes.extend(vec![0u8; 59_998]);
        }
        let jpeg = jpeg_with_exif(&exif);
        bytes.extend(&jpeg[2..]);
        fs::write(&photo, &bytes).unwrap();
        assert!(bytes.len() > 1 << 20);
        let capture = read_capture(&photo);
        assert_eq!(capture.date_source, DateSource::Original);
        assert_eq!(
            capture.captured.map(|d| d.to_string()).as_deref(),
            Some("2024-05-06 07:08:09")
        );
    }

    /// A CIFF heap with one TimeStamp record: the CRW's capture time.
    #[test]
    fn crw_capture_time_comes_from_its_ciff_timestamp() {
        let mut data = b"II".to_vec();
        data.extend(14u32.to_le_bytes());
        data.extend(b"HEAPCCDR");
        let heap = data.len();
        // 2004-02-03 04:05:06 as local seconds, then time zone words.
        data.extend(1_075_781_106u32.to_le_bytes());
        data.extend([0u8; 8]);
        let table = data.len() - heap;
        data.extend(1u16.to_le_bytes());
        data.extend(0x180eu16.to_le_bytes());
        data.extend(12u32.to_le_bytes());
        data.extend(0u32.to_le_bytes());
        data.extend((table as u32).to_le_bytes());
        let capture = from_bytes(&data);
        assert_eq!(capture.date_source, DateSource::Original);
        assert_eq!(
            capture.captured.map(|d| d.to_string()).as_deref(),
            Some("2004-02-03 04:05:06")
        );
        data[0] = b'M';
        assert_eq!(from_bytes(&data).captured, None);
    }

    #[test]
    fn finds_the_card_sidecar_of_a_photo() {
        let d = tempfile::tempdir().unwrap();
        let card = d.path();
        let touch = |name: &str| fs::write(card.join(name), b"x").unwrap();
        touch("A.ARW");
        touch("A.xmp");
        assert_eq!(
            card_sidecar(&card.join("A.ARW")).unwrap(),
            Some(card.join("A.xmp"))
        );
        touch("B.NEF");
        touch("B.NEF.xmp");
        touch("B.xmp");
        assert_eq!(
            card_sidecar(&card.join("B.NEF")).unwrap(),
            Some(card.join("B.NEF.xmp"))
        );
        // RAW+JPEG: the stem's sidecar is the RAW's, not the JPEG's.
        touch("C.CR3");
        touch("C.JPG");
        touch("C.xmp");
        assert_eq!(
            card_sidecar(&card.join("C.CR3")).unwrap(),
            Some(card.join("C.xmp"))
        );
        assert_eq!(card_sidecar(&card.join("C.JPG")).unwrap(), None);
        // A JPEG alone owns its stem's sidecar.
        touch("D.JPG");
        touch("D.xmp");
        assert_eq!(
            card_sidecar(&card.join("D.JPG")).unwrap(),
            Some(card.join("D.xmp"))
        );
        touch("E.ARW");
        assert_eq!(card_sidecar(&card.join("E.ARW")).unwrap(), None);
        fs::create_dir(card.join("F.xmp")).unwrap();
        touch("F.ARW");
        assert_eq!(
            card_sidecar(&card.join("F.ARW")).unwrap(),
            None,
            "a folder is no sidecar"
        );
    }

    #[test]
    fn card_sidecars_are_found_in_any_letter_case() {
        let d = tempfile::tempdir().unwrap();
        let card = d.path();
        let touch = |name: &str| fs::write(card.join(name), b"x").unwrap();
        let found = |photo: &str| card_sidecar(&card.join(photo)).unwrap();
        touch("G.ARW");
        touch("g.arw.XMP");
        assert_eq!(found("G.ARW"), Some(card.join("g.arw.XMP")));
        touch("H.ARW");
        touch("H.XMP");
        assert_eq!(found("H.ARW"), Some(card.join("H.XMP")));
        touch("i.arw");
        touch("I.Xmp");
        assert_eq!(found("i.arw"), Some(card.join("I.Xmp")));
        // RAW+JPEG whose names differ in case: still the RAW's sidecar.
        touch("K.CR3");
        touch("k.jpg");
        touch("K.xmp");
        assert_eq!(found("K.CR3"), Some(card.join("K.xmp")));
        assert_eq!(found("k.jpg"), None);
        // Two that differ only in case (a case-sensitive volume): neither.
        // On a volume that folds case (macOS APFS by default) they are one
        // file, so this part needs a case-sensitive one.
        touch("J.ARW");
        touch("J.xmp");
        touch("J.XMP");
        if fs::read_dir(card)
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .eq_ignore_ascii_case("J.xmp")
            })
            .count()
            < 2
        {
            return;
        }
        let error = card_sidecar(&card.join("J.ARW")).unwrap_err();
        assert!(error.contains("letter case"), "{error}");
        // The full form wins over an ambiguous short one.
        touch("J.ARW.xmp");
        assert_eq!(found("J.ARW"), Some(card.join("J.ARW.xmp")));
    }
}
