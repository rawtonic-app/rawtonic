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
//! modification time → the time of the import. A rating the camera wrote
//! into the file (its XMP packet, or the EXIF Rating tag) is the rating
//! when no sidecar gives one.
use rawtonic_naming::{Capture, DateSource};
use std::{
    collections::HashMap,
    fs,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::Arc,
};

mod late_ifds;

/// How much of a file is read for its metadata, step by step: the first
/// step holds the EXIF of nearly every format (it sits at the start of
/// TIFF-based RAWs, CR3's `moov`, a RAF's embedded JPEG, a JPEG's APP1); a
/// file whose step holds no capture date is read further, at most
/// `WHOLE_FILE_LIMIT` bytes in all. An ISO BMFF RAW (CR3) is read box by
/// box instead (`bmff_metadata`): its metadata is not all in its header.
const READ_STEPS: [u64; 2] = [1 << 20, 16 << 20];
const WHOLE_FILE_LIMIT: u64 = 512 << 20;

/// Capture facts of `path` read from its header; never fails (a file
/// without metadata gets its modification time as date).
pub fn read_capture(path: &Path) -> Capture {
    read_capture_within(path, WHOLE_FILE_LIMIT)
}

/// Rights and descriptive metadata embedded in the photograph's own XMP
/// packet (not its thumbnail's), with EXIF creator/copyright as fallback.
/// Used only when an import preset is selected, after verified publication.
pub(crate) fn import_metadata(
    path: &Path,
) -> anyhow::Result<(rawtonic_metadata::Metadata, Option<i32>)> {
    use chrono::Datelike;
    let read_packet = || -> anyhow::Result<rawtonic_metadata::Metadata> {
        let mut file = fs::File::open(path)?;
        let len = file.metadata()?.len();
        let mut header = vec![0; len.min(64 * 1024) as usize];
        file.read_exact(&mut header)?;
        Ok(match container_xmp(&mut file, len, &header)? {
            Container::Packet(packet) => {
                let text =
                    std::str::from_utf8(&packet)?.trim_end_matches(['\0', ' ', '\t', '\r', '\n']);
                rawtonic_metadata::read_xmp(text)?.0
            }
            _ => rawtonic_metadata::Metadata::default(),
        })
    };
    // Optional enrichment follows read_header's tolerance: damaged or
    // unreadable embedded packets must not veto a verified photograph.
    let mut metadata = read_packet().unwrap_or_default();
    let capture = read_capture_within(path, FACTS_LIMIT);
    let year = capture
        .captured
        .filter(|_| {
            matches!(
                capture.date_source,
                DateSource::Original | DateSource::Digitized | DateSource::FileDate
            )
        })
        .map(|date| date.year());
    // XMP is validated by read_xmp; validate each optional EXIF fallback
    // before adopting it, retaining valid embedded fields if one is bad.
    let camera_text = |value: Option<String>| {
        let candidate = rawtonic_metadata::Metadata {
            creator: value.unwrap_or_default(),
            ..Default::default()
        };
        if candidate.validate().is_ok() {
            candidate.creator
        } else {
            String::new()
        }
    };
    if metadata.creator.trim().is_empty() {
        metadata.creator = camera_text(capture.creator);
    }
    if metadata.copyright.trim().is_empty() {
        metadata.copyright = camera_text(capture.copyright);
    }
    if metadata.rating == 0 && metadata.flag == 0 {
        metadata.rating = capture.rating.unwrap_or_default();
    }
    Ok((metadata, year))
}

/// How much of a file the library's capture facts are read from: the
/// header steps only, never the rest of a large file without a date (the
/// naming of an ingest reads further for its date).
const FACTS_LIMIT: u64 = READ_STEPS[READ_STEPS.len() - 1];

/// `read_capture` reading at most `most` bytes of the file.
fn read_capture_within(path: &Path, most: u64) -> Capture {
    let mut capture = fs::File::open(path)
        .and_then(|file| {
            let meta = file.metadata()?;
            if !meta.is_file() {
                return Err(std::io::Error::other("not a file"));
            }
            Ok(read_header(file, meta.len(), most).0)
        })
        .unwrap_or_default();
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

/// The capture facts in the first `most` bytes of `file` (`len` long), read
/// step by step until a capture date is found; an ISO BMFF file's from its
/// metadata boxes wherever they are (`bmff_metadata`). A read error gives
/// what was read before it with the error: the caller decides whether a
/// part will do.
fn read_header(
    mut file: impl Read + Seek,
    len: u64,
    most: u64,
) -> (Capture, Option<std::io::Error>) {
    let mut capture = Capture::default();
    let mut buffer = Vec::new();
    if let Err(error) = (&mut file).take(12.min(most)).read_to_end(&mut buffer) {
        return (capture, Some(error));
    }
    let mut done = false;
    if buffer.get(4..8) == Some(b"ftyp") {
        match bmff_metadata(&mut file, len) {
            Err(error) => return (capture, Some(error)),
            Ok(Some(read)) if has_facts(&read) => (capture, done) = (read, true),
            Ok(_) => {}
        }
        if let Err(error) = file.seek(SeekFrom::Start(buffer.len() as u64)) {
            return (capture, Some(error));
        }
    }
    for limit in READ_STEPS
        .into_iter()
        .chain([WHOLE_FILE_LIMIT])
        .map(|step| step.min(most).min(len))
    {
        if done || (limit <= buffer.len() as u64 && !buffer.is_empty()) {
            continue;
        }
        let wanted = limit.saturating_sub(buffer.len() as u64);
        if let Err(error) = (&mut file).take(wanted).read_to_end(&mut buffer) {
            return (capture, Some(error));
        }
        let read = from_bytes(&buffer);
        let done = read.captured.is_some() || buffer.len() as u64 >= len.min(most);
        capture = read;
        if done {
            break;
        }
    }
    // A TIFF-based file whose metadata lies past what was read (a DNG that
    // keeps its IFDs after its image data, as merges write them): its IFDs
    // are followed wherever they are.
    if capture.captured.is_none()
        && (buffer.len() as u64) < len
        && tiff_byte_order(&buffer).is_some()
    {
        match tiff_metadata(&mut file, len, &buffer) {
            Err(error) => return (capture, Some(error)),
            // What the whole IFDs give, a date or not (a superset of what
            // the steps read: their bytes are among the pieces).
            Ok(Some(read)) if has_facts(&read) => capture = read,
            Ok(_) => {}
        }
    }
    // The rating of the file's own XMP packet (where its format keeps it,
    // wherever in the file that is), over the EXIF Rating tag; packets of
    // embedded previews do not count. Formats this does not know: the
    // first packet with a rating in what was read.
    // Its IPTC Digital Source Type likewise (the file's own packet only).
    match container_xmp(&mut file, len, &buffer) {
        Err(error) => return (capture, Some(error)),
        Ok(Container::Packet(packet)) => {
            // Located by the container: parsed as XML whatever its prefixes,
            // without the NUL bytes cameras end or pad it with (RW2).
            let doc = std::str::from_utf8(&packet)
                .ok()
                .map(|text| text.trim_end_matches(['\0', ' ', '\t', '\r', '\n']));
            capture.rating = doc.and_then(packet_rating).or(capture.rating);
            capture.source_type = doc.and_then(packet_source_type);
        }
        Ok(Container::NoPacket) => {}
        Ok(Container::Unknown) => {
            capture.rating = xmp_rating(&buffer).or(capture.rating);
            capture.source_type = first_packet(&buffer, packet_source_type);
        }
    }
    (capture, None)
}

/// Whether `capture` holds anything the file's metadata said.
fn has_facts(capture: &Capture) -> bool {
    capture.captured.is_some()
        || capture.make.is_some()
        || capture.model.is_some()
        || capture.lens.is_some()
        || capture.iso.is_some()
        || capture.exposure.is_some()
        || capture.focal_length.is_some()
        || capture.aperture.is_some()
        || capture.rating.is_some()
        || capture.creator.is_some()
        || capture.copyright.is_some()
        || capture.serial.is_some()
}

/// Where a file keeps its own XMP packet (`container_xmp`).
enum Container {
    /// Its packet.
    Packet(Vec<u8>),
    /// A format with a place for one, without one.
    NoPacket,
    /// A format whose place for it is not known here.
    Unknown,
}

/// The ISO BMFF box (`uuid`) a CR3 keeps its XMP packet in.
const BMFF_XMP_UUID: [u8; 16] = [
    0xbe, 0x7a, 0xcf, 0xcb, 0x97, 0xa9, 0x42, 0xe8, 0x9c, 0x71, 0x99, 0x94, 0x91, 0xe3, 0xaf, 0xac,
];

/// A file's own XMP packet: a JPEG's APP1 segment, IFD0's XMP tag (700) of
/// a TIFF-based file, a CR3's XMP box — wherever in the file it is.
fn container_xmp(
    file: &mut (impl Read + Seek),
    len: u64,
    header: &[u8],
) -> std::io::Result<Container> {
    const XMP_HEADER: &[u8] = b"http://ns.adobe.com/xap/1.0/\0";
    let packet = |bytes: Option<Vec<u8>>| bytes.map_or(Container::NoPacket, Container::Packet);
    if header.get(4..8) == Some(b"ftyp") {
        // Top-level boxes: size, type, a uuid box's uuid.
        let mut at = 0;
        for _ in 0..BMFF_BOX_LIMIT {
            let Some(head) = bytes_at(file, len, header, at, 8)? else {
                break;
            };
            let mut size = u64::from(u32::from_be_bytes([head[0], head[1], head[2], head[3]]));
            let mut start = at + 8;
            if size == 1 {
                let Some(large) = bytes_at(file, len, header, at + 8, 8)? else {
                    break;
                };
                size = u64::from_be_bytes(large.try_into().unwrap_or_default());
                start += 8;
            }
            // A box smaller than its header or ending past the file: damaged.
            if size < start - at || at.checked_add(size).is_none_or(|end| end > len) {
                break;
            }
            if &head[4..8] == b"uuid"
                && bytes_at(file, len, header, start, 16)?.as_deref() == Some(&BMFF_XMP_UUID)
            {
                // A size too small for its own uuid: a damaged box.
                let Some(size) = size.checked_sub(start + 16 - at) else {
                    return Ok(Container::NoPacket);
                };
                return Ok(packet(if size <= XMP_LIMIT {
                    bytes_at(file, len, header, start + 16, size)?
                } else {
                    None
                }));
            }
            at += size;
        }
        return Ok(Container::NoPacket);
    }
    if header.starts_with(&[0xFF, 0xD8]) {
        let mut at = 2;
        for _ in 0..JPEG_SEGMENT_LIMIT {
            let Some(marker) = bytes_at(file, len, header, at, 4)? else {
                break;
            };
            // Image data (start of scan) or the end: no metadata after.
            if marker[0] != 0xFF || matches!(marker[1], 0xDA | 0xD9) {
                break;
            }
            let size = u64::from(u16::from_be_bytes([marker[2], marker[3]]));
            if marker[1] == 0xE1 && size > 2 + XMP_HEADER.len() as u64 {
                let start = bytes_at(file, len, header, at + 4, XMP_HEADER.len() as u64)?;
                if start.as_deref() == Some(XMP_HEADER) {
                    let start = at + 4 + XMP_HEADER.len() as u64;
                    let rest = size - 2 - XMP_HEADER.len() as u64;
                    return Ok(packet(bytes_at(file, len, header, start, rest)?));
                }
            }
            at += 2 + size;
        }
        return Ok(Container::NoPacket);
    }
    let Some(little) = tiff_byte_order(header) else {
        return Ok(Container::Unknown);
    };
    let u16_of = |b: &[u8]| {
        let b = [b[0], b[1]];
        if little {
            u16::from_le_bytes(b)
        } else {
            u16::from_be_bytes(b)
        }
    };
    let u32_of = |b: &[u8]| {
        let b = [b[0], b[1], b[2], b[3]];
        if little {
            u32::from_le_bytes(b)
        } else {
            u32::from_be_bytes(b)
        }
    };
    let Some(ifd) = bytes_at(file, len, header, 4, 4)?.map(|b| u64::from(u32_of(&b))) else {
        return Ok(Container::NoPacket);
    };
    let Some(count) = bytes_at(file, len, header, ifd, 2)?.map(|b| u64::from(u16_of(&b))) else {
        return Ok(Container::NoPacket);
    };
    let Some(table) = bytes_at(file, len, header, ifd + 2, 12 * count)? else {
        return Ok(Container::NoPacket);
    };
    for entry in table.as_chunks::<12>().0 {
        let (tag, kind, number) = (
            u16_of(&entry[..2]),
            u16_of(&entry[2..4]),
            u32_of(&entry[4..8]),
        );
        if tag == 700 && matches!(kind, 1 | 7) && u64::from(number) <= XMP_LIMIT {
            return Ok(packet(if number <= 4 {
                Some(entry[8..8 + number as usize].to_vec())
            } else {
                bytes_at(
                    file,
                    len,
                    header,
                    u64::from(u32_of(&entry[8..12])),
                    u64::from(number),
                )?
            }));
        }
    }
    Ok(Container::NoPacket)
}

/// At most this many JPEG segments or top-level ISO BMFF boxes are walked,
/// and an XMP packet this large read, looking for a file's own packet.
const JPEG_SEGMENT_LIMIT: usize = 4096;
const BMFF_BOX_LIMIT: usize = 4096;
const XMP_LIMIT: u64 = 16 << 20;

/// `size` bytes at `offset` of a file whose first bytes are `header`:
/// from `header` when it holds them, else read; `None` past the end.
fn bytes_at(
    file: &mut (impl Read + Seek),
    len: u64,
    header: &[u8],
    offset: u64,
    size: u64,
) -> std::io::Result<Option<Vec<u8>>> {
    let Some(end) = offset.checked_add(size).filter(|end| *end <= len) else {
        return Ok(None);
    };
    if end <= header.len() as u64 {
        return Ok(Some(header[offset as usize..end as usize].to_vec()));
    }
    file.seek(SeekFrom::Start(offset))?;
    let mut bytes = vec![0; size as usize];
    file.read_exact(&mut bytes)?;
    Ok(Some(bytes))
}

/// Whether a TIFF-based file (TIFF, DNG and the RAWs built on it, whatever
/// their magic number: RW2's, ORF's) starts `bytes`, little-endian or not.
fn tiff_byte_order(bytes: &[u8]) -> Option<bool> {
    match bytes.get(..2)? {
        b"II" => Some(true),
        b"MM" => Some(false),
        _ => None,
    }
}

/// The capture facts of a TIFF-based file from its IFDs wherever they are
/// (`late_ifds`): the IFD chain, its sub-IFDs (SubIFDs, EXIF, GPS,
/// interoperability) and each entry's value (a maker note, an XMP packet)
/// are read with seeks, without the image data their offsets point to.
/// `None` when nothing could be followed; an error when reading fails.
fn tiff_metadata(
    file: &mut (impl Read + Seek),
    len: u64,
    header: &[u8],
) -> std::io::Result<Option<Capture>> {
    let pieces = late_ifds::pieces(file, len, header)?;
    if pieces.is_empty() {
        return Ok(None);
    }
    Ok(sparse_capture(&pieces))
}

/// At most this much of an ISO BMFF file's boxes besides its image data
/// is read for its metadata (a damaged file's sizes may claim anything).
const BMFF_METADATA_LIMIT: u64 = 64 << 20;

/// Metadata read past this offset is not looked at through a sparse
/// buffer (`sparse_capture`): its untouched pages cost no memory where
/// the system allocates lazily, but its address space is still reserved.
const SPARSE_LIMIT: u64 = 1 << 30;

/// `pieces` (offset, bytes) of a file laid at their offsets, each over
/// those before it, into an otherwise empty buffer that stands for the file; `None` past
/// `SPARSE_LIMIT`. Its untouched pages are the system's zero pages.
fn sparse_file(pieces: &[(u64, Vec<u8>)]) -> Option<Vec<u8>> {
    let end = pieces
        .iter()
        .map(|(at, bytes)| at + bytes.len() as u64)
        .max()
        .filter(|end| *end <= SPARSE_LIMIT)?;
    let mut buffer = vec![0u8; usize::try_from(end).ok()?];
    for (at, bytes) in pieces {
        buffer[*at as usize..*at as usize + bytes.len()].copy_from_slice(bytes);
    }
    Some(buffer)
}

/// The capture facts of a file of which only `pieces` were read
/// (`sparse_file`): the RAW decoder's and, for a TIFF-based file, the EXIF
/// reader's, each reading only what its metadata needs. `None` when they
/// lie past `SPARSE_LIMIT`.
fn sparse_capture(pieces: &[(u64, Vec<u8>)]) -> Option<Capture> {
    let buffer = Arc::new(sparse_file(pieces)?);
    let source = rawler::rawsource::RawSource::new_from_shared_vec(buffer.clone());
    let mut capture = Capture::default();
    if let Some(metadata) = raw_metadata(&source) {
        apply_raw(&mut capture, &metadata);
    }
    drop(source);
    if tiff_byte_order(&buffer).is_some() {
        let buffer = Arc::try_unwrap(buffer).unwrap_or_else(|shared| (*shared).clone());
        if let Ok(exif) = exif::Reader::new().read_raw(buffer) {
            apply_exif(&mut capture, &exif);
        }
    }
    Some(capture)
}

/// The capture facts of an ISO BMFF RAW (Canon CR3, CRM) without reading
/// its image data: its box tree is walked with seeks, then every top-level
/// box but `mdat` (its header only) and the CTMD record the decoder takes
/// the rest of its metadata from (stored in `mdat`, after the image, often
/// tens of MB into the file) are read into an otherwise empty buffer at
/// their own offsets, which the RAW decoder reads as the file. `None` when
/// it is no BMFF file the decoder reads; an error when reading fails.
fn bmff_metadata(file: &mut (impl Read + Seek), len: u64) -> std::io::Result<Option<Capture>> {
    bmff_metadata_within(file, len, BMFF_METADATA_LIMIT)
}

/// A reader that reads at most `left` bytes in all, then fails with
/// `InvalidData` (a damaged file's sizes claiming more than metadata).
struct Budgeted<R> {
    inner: R,
    left: u64,
}

impl<R: Read> Read for Budgeted<R> {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if self.left == 0 && !out.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "more metadata than a file of this kind holds",
            ));
        }
        let most = out
            .len()
            .min(usize::try_from(self.left).unwrap_or(usize::MAX));
        let n = self.inner.read(&mut out[..most])?;
        self.left -= n as u64;
        Ok(n)
    }
}

impl<R: Seek> Seek for Budgeted<R> {
    fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
        self.inner.seek(to)
    }
}

/// `bmff_metadata` reading at most `budget` bytes while the box tree is
/// parsed (its parser reads box payloads and sample tables as their sizes
/// and counts say; a damaged file's may claim the whole image).
fn bmff_metadata_within(
    file: &mut (impl Read + Seek),
    len: u64,
    budget: u64,
) -> std::io::Result<Option<Capture>> {
    use rawler::formats::bmff::FileBox;
    file.seek(SeekFrom::Start(0))?;
    let parsed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let reader = Budgeted {
            inner: &mut *file,
            left: budget,
        };
        let filebox = FileBox::parse(std::io::BufReader::new(reader))?;
        let mut ranges = vec![
            (filebox.ftyp.header.offset, filebox.ftyp.header.size),
            (filebox.moov.header.offset, filebox.moov.header.size),
            (filebox.mdat.header.offset, filebox.mdat.header.header_len),
        ];
        ranges.extend(
            filebox
                .vendor
                .iter()
                .map(|b| (b.header.offset, b.header.size)),
        );
        ranges.extend(
            filebox
                .cr3xpacket
                .iter()
                .map(|b| (b.header.offset, b.header.size)),
        );
        // The decoder reads the first sample of the CTMD track.
        ranges.extend(
            filebox
                .moov
                .traks
                .iter()
                .filter(|trak| trak.mdia.minf.stbl.stsd.ctmd.is_some())
                .filter_map(|trak| trak.mdia.minf.stbl.get_sample_offset(1))
                .map(|(offset, size)| (offset as u64, size as u64)),
        );
        Ok::<_, rawler::formats::bmff::BmffError>(ranges)
    }));
    let ranges = match parsed {
        Ok(Ok(ranges)) => ranges,
        // A box running past the end or past the budget is a damaged
        // file, not a failed read.
        Ok(Err(rawler::formats::bmff::BmffError::Io(error)))
            if !matches!(
                error.kind(),
                std::io::ErrorKind::UnexpectedEof | std::io::ErrorKind::InvalidData
            ) =>
        {
            return Err(error);
        }
        _ => return Ok(None),
    };
    let ranges: Vec<(u64, u64)> = ranges
        .into_iter()
        .filter(|&(offset, size)| offset.checked_add(size).is_some_and(|end| end <= len))
        .collect();
    if ranges.iter().map(|(_, size)| size).sum::<u64>() > BMFF_METADATA_LIMIT {
        return Ok(None);
    }
    if ranges
        .iter()
        .any(|(offset, size)| offset + size > SPARSE_LIMIT)
    {
        return Ok(None);
    }
    let mut pieces = Vec::with_capacity(ranges.len());
    for (offset, size) in ranges {
        file.seek(SeekFrom::Start(offset))?;
        let mut bytes = vec![0; size as usize];
        file.read_exact(&mut bytes)?;
        pieces.push((offset, bytes));
    }
    Ok(sparse_capture(&pieces))
}

/// What `path`'s EXIF says for the library's search (`rawtonic_catalog::
/// CaptureFacts`), from at most its first 16 MiB (an ISO BMFF RAW's from its
/// metadata boxes, wherever they are); `None` when the file
/// cannot be read now (gone, its drive disconnected, a read error), so it
/// is tried again later. A capture time only from the file's own metadata,
/// never its modification time.
pub fn capture_facts(path: &Path) -> Option<rawtonic_catalog::CaptureFacts> {
    let file = fs::File::open(path).ok()?;
    let meta = file.metadata().ok().filter(|m| m.is_file())?;
    match read_header(file, meta.len(), FACTS_LIMIT) {
        (capture, None) => facts_of(capture),
        (_, Some(_)) => None,
    }
}

fn facts_of(capture: Capture) -> Option<rawtonic_catalog::CaptureFacts> {
    let dated = matches!(
        capture.date_source,
        DateSource::Original | DateSource::Digitized | DateSource::FileDate
    ) && capture.captured.is_some();
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
        source_type: capture.source_type,
        camera_rating: capture.rating.filter(|rating| (1..=5).contains(rating)),
        exposure_mode: capture.exposure_mode,
        exposure_bias: capture.exposure_bias,
        sequence: capture.sequence,
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
    let source = rawler::rawsource::RawSource::new_from_slice(bytes);
    let mut capture = Capture::default();
    if let Some(metadata) = raw_metadata(&source) {
        apply_raw(&mut capture, &metadata);
    }
    if let Ok(exif) = exif::Reader::new().read_from_container(&mut std::io::Cursor::new(bytes)) {
        apply_exif(&mut capture, &exif);
    }
    if let Some(jpeg) = raf_jpeg(bytes)
        && let Ok(exif) = exif::Reader::new().read_from_container(&mut std::io::Cursor::new(jpeg))
    {
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

/// The rating of the first XMP packet in `bytes` that has one (the
/// packet a camera writes into its RAWs, found by scanning as the XMP
/// specification provides for files whose format is not parsed): 1–5, or
/// 0 for one cleared or rejected (0, -1), which no other rating replaces.
/// `None` when no packet has a rating.
fn xmp_rating(bytes: &[u8]) -> Option<u8> {
    first_packet(bytes, packet_rating)
}

/// What `read` finds in the first XMP packet in `bytes` it finds anything
/// in (packets found by scanning, `xmp_rating`).
fn first_packet<T>(bytes: &[u8], read: impl Fn(&str) -> Option<T>) -> Option<T> {
    const START: &[u8] = b"<x:xmpmeta";
    const END: &[u8] = b"</x:xmpmeta>";
    let mut rest = bytes;
    while let Some(start) = memchr::memmem::find(rest, START) {
        rest = &rest[start..];
        let end = memchr::memmem::find(rest, END)? + END.len();
        if let Some(found) = std::str::from_utf8(&rest[..end]).ok().and_then(&read) {
            return Some(found);
        }
        rest = &rest[end..];
    }
    None
}

/// The `Iptc4xmpExt:DigitalSourceType` of one XMP packet (an attribute, an
/// element, or an element's `rdf:resource`); `None` without one or for a
/// packet that is no XML.
fn packet_source_type(text: &str) -> Option<String> {
    use rawtonic_metadata::{IPTC_EXT, RDF};
    let doc = roxmltree::Document::parse(text).ok()?;
    doc.descendants()
        .filter(|n| n.is_element())
        .find_map(|node| {
            node.attribute((IPTC_EXT, "DigitalSourceType")).or_else(|| {
                node.has_tag_name((IPTC_EXT, "DigitalSourceType"))
                    .then(|| node.attribute((RDF, "resource")).or(node.text()))
                    .flatten()
            })
        })
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(String::from)
}

/// The `xmp:Rating` of one XMP packet (an attribute or an element), 0 for
/// a rejected one; `None` without one or for a packet that is no XML.
fn packet_rating(text: &str) -> Option<u8> {
    const XMP: &str = "http://ns.adobe.com/xap/1.0/";
    let doc = roxmltree::Document::parse(text).ok()?;
    let value = doc
        .descendants()
        .filter(|n| n.is_element())
        .find_map(|node| {
            node.attribute((XMP, "Rating")).or_else(|| {
                node.has_tag_name((XMP, "Rating"))
                    .then(|| node.text().unwrap_or(""))
            })
        })?;
    let rating: f64 = value.trim().parse().ok()?;
    rating
        .is_finite()
        .then(|| rating.round().clamp(0.0, 5.0) as u8)
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
fn raw_metadata(source: &rawler::rawsource::RawSource) -> Option<rawler::decoders::RawMetadata> {
    std::panic::catch_unwind(|| {
        let decoder = rawler::get_decoder(source).ok()?;
        decoder
            .raw_metadata(source, &rawler::decoders::RawDecodeParams::default())
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
    capture.exposure_mode = capture
        .exposure_mode
        .or(exif.exposure_mode.and_then(|m| u8::try_from(m).ok()));
    capture.exposure_bias = capture.exposure_bias.or(exif
        .exposure_bias
        .filter(|r| r.d != 0)
        .map(|r| f64::from(r.n) / f64::from(r.d)));
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
    // The rating tag Windows and some cameras write (0x4746, 0–5).
    capture.rating = capture.rating.or_else(|| {
        exif.get_field(Tag(exif::Context::Tiff, 0x4746), exif::In::PRIMARY)
            .and_then(|f| f.value.get_uint(0))
            .filter(|rating| (1..=5).contains(rating))
            .map(|rating| rating as u8)
    });
    capture.copyright = capture
        .copyright
        .take()
        .or_else(|| ascii(exif, Tag::Copyright));
    capture.exposure_mode = capture.exposure_mode.or_else(|| {
        exif.get_field(Tag::ExposureMode, exif::In::PRIMARY)
            .and_then(|f| f.value.get_uint(0))
            .and_then(|m| u8::try_from(m).ok())
    });
    capture.exposure_bias = capture.exposure_bias.or_else(|| {
        match &exif
            .get_field(Tag::ExposureBiasValue, exif::In::PRIMARY)?
            .value
        {
            exif::Value::SRational(values) => values
                .first()
                .filter(|r| r.denom != 0)
                .map(|r| f64::from(r.num) / f64::from(r.denom)),
            _ => None,
        }
    });
    capture.sequence = capture.sequence.or_else(|| {
        match &exif.get_field(Tag::MakerNote, exif::In::PRIMARY)?.value {
            exif::Value::Undefined(note, _) => burst_frame(note),
            _ => None,
        }
    });
}

/// The frame number within a continuous-drive burst a maker note states (1
/// for its first frame; `None` for a single shot or a maker note not
/// known). Read: Fujifilm's SequenceNumber (tag 0x1101 of its maker note,
/// 0 for a single shot). Other makers' notes give no frame number this
/// reader knows; their bursts are grouped by capture time.
fn burst_frame(note: &[u8]) -> Option<u32> {
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
    (0..count.min(512)).find_map(|i| {
        let at = ifd + 2 + 12 * i;
        (le16(at)? == 0x1101 && le16(at + 2)? == 3)
            .then(|| le16(at + 8))
            .flatten()
            .map(u32::from)
            .filter(|n| *n > 0)
    })
}

/// A Fujifilm RAF's embedded JPEG within `bytes` (its offset and length
/// at bytes 84 and 88 of the RAF header), cut at the end of `bytes`: its
/// EXIF holds the maker note.
fn raf_jpeg(bytes: &[u8]) -> Option<&[u8]> {
    if !bytes.starts_with(b"FUJIFILM") {
        return None;
    }
    let be32 = |at: usize| {
        bytes
            .get(at..at + 4)
            .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize)
    };
    let (offset, length) = (be32(84)?, be32(88)?);
    bytes.get(offset..offset.saturating_add(length).min(bytes.len()))
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

type ListedFolders = HashMap<PathBuf, Arc<FolderFiles>>;

thread_local! {
    static LISTED: std::cell::RefCell<Option<ListedFolders>> = const {
        std::cell::RefCell::new(None)
    };
}

/// One import's snapshot of sidecar filenames, discarded before another
/// import can observe it. Folder modification times are not change tokens:
/// NTFS can keep them unchanged across new entries; FAT has coarse clocks.
pub(crate) struct SidecarScope {
    previous: Option<ListedFolders>,
    // The guard must be dropped on the thread whose cache it owns.
    _thread: std::marker::PhantomData<std::rc::Rc<()>>,
}

impl SidecarScope {
    pub(crate) fn new() -> Self {
        Self {
            previous: LISTED.with(|listed| listed.replace(Some(HashMap::new()))),
            _thread: std::marker::PhantomData,
        }
    }
}

impl Drop for SidecarScope {
    fn drop(&mut self) {
        LISTED.with(|listed| listed.replace(self.previous.take()));
    }
}

/// An import lists each folder once. A standalone query always lists it
/// afresh, as does the next import. `None` when it cannot be listed.
fn folder_files(folder: &Path) -> Option<Arc<FolderFiles>> {
    if let Some(files) = LISTED.with(|listed| {
        listed
            .borrow()
            .as_ref()
            .and_then(|files| files.get(folder).cloned())
    }) {
        return Some(files);
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
    LISTED.with(|listed| {
        if let Some(listed) = listed.borrow_mut().as_mut() {
            if listed.len() >= 64 {
                listed.clear();
            }
            listed.insert(folder.to_path_buf(), files.clone());
        }
    });
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

    #[test]
    fn a_fujifilm_maker_note_gives_the_burst_frame_number() {
        let note = |value: u16| {
            let mut n = b"FUJIFILM".to_vec();
            n.extend_from_slice(&12u32.to_le_bytes());
            n.extend_from_slice(&2u16.to_le_bytes());
            for (tag, value) in [(0x1100u16, 0u16), (0x1101, value)] {
                n.extend_from_slice(&tag.to_le_bytes());
                n.extend_from_slice(&3u16.to_le_bytes());
                n.extend_from_slice(&1u32.to_le_bytes());
                n.extend_from_slice(&value.to_le_bytes());
                n.extend_from_slice(&[0, 0]);
            }
            n
        };
        assert_eq!(burst_frame(&note(3)), Some(3));
        // 0: a single shot.
        assert_eq!(burst_frame(&note(0)), None);
        assert_eq!(burst_frame(b"Nikon\0\x02"), None);
        assert_eq!(burst_frame(&note(3)[..20]), None);
    }

    #[test]
    fn the_exif_exposure_mode_marks_an_auto_bracket() {
        let capture = from_bytes(&tiff_exif("2026:01:02 03:04:05"));
        assert_eq!(capture.exposure_mode, Some(2));
    }

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
                (0xa402, V::Short(2)),
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
                (0x4746, V::Short(4)),
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
        assert_eq!(capture.rating, Some(4), "the EXIF Rating tag");
    }

    /// A JPEG whose APP1 segments are `tiff`'s EXIF, then an XMP packet.
    fn jpeg_with_xmp(tiff: &[u8], xmp: &str) -> Vec<u8> {
        let mut out = jpeg_with_exif(tiff);
        out.truncate(out.len() - 2);
        let mut segment = b"http://ns.adobe.com/xap/1.0/\0".to_vec();
        segment.extend(xmp.as_bytes());
        out.extend([0xFF, 0xE1]);
        out.extend(((segment.len() + 2) as u16).to_be_bytes());
        out.extend(segment);
        out.extend([0xFF, 0xD9]);
        out
    }

    fn camera_xmp(rating: &str) -> String {
        format!(
            r#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?><x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description rdf:about="" xmlns:xmp="http://ns.adobe.com/xap/1.0/">{rating}</rdf:Description></rdf:RDF></x:xmpmeta><?xpacket end="w"?>"#
        )
    }

    /// The rating a camera writes into its file's XMP packet (as an element
    /// or an attribute) is read, over the EXIF Rating tag; none (0, a
    /// reject) is no rating.
    /// A file's own IPTC Digital Source Type (a generator or another
    /// editor marked it) is read with its facts, as an attribute, an
    /// element or a resource; a packet without one gives none.
    #[test]
    fn reads_the_digital_source_type_from_the_files_xmp() {
        const AI: &str = "http://cv.iptc.org/newscodes/digitalsourcetype/trainedAlgorithmicMedia";
        let ext = |inner: &str, attribute: &str| {
            camera_xmp(inner).replace(
                r#"rdf:about="""#,
                &format!(
                    r#"rdf:about="" xmlns:Iptc4xmpExt="{}"{attribute}"#,
                    rawtonic_metadata::IPTC_EXT
                ),
            )
        };
        let exif = tiff_exif("2025:12:24 18:30:15");
        let d = tempfile::tempdir().unwrap();
        for (name, xmp, expected) in [
            (
                "attribute.jpg",
                ext("", &format!(r#" Iptc4xmpExt:DigitalSourceType="{AI}""#)),
                Some(AI),
            ),
            (
                "element.jpg",
                ext(
                    &format!("<Iptc4xmpExt:DigitalSourceType>{AI}</Iptc4xmpExt:DigitalSourceType>"),
                    "",
                ),
                Some(AI),
            ),
            (
                "resource.jpg",
                ext(
                    &format!(r#"<Iptc4xmpExt:DigitalSourceType rdf:resource="{AI}"/>"#),
                    "",
                ),
                Some(AI),
            ),
            ("none.jpg", camera_xmp("<xmp:Rating>3</xmp:Rating>"), None),
        ] {
            let photo = d.path().join(name);
            fs::write(&photo, jpeg_with_xmp(&exif, &xmp)).unwrap();
            let facts = capture_facts(&photo).unwrap();
            assert_eq!(facts.source_type.as_deref(), expected, "{name}");
            assert_eq!(
                facts.captured.as_deref(),
                Some("2025-12-24 18:30:15"),
                "{name}"
            );
        }
        assert_eq!(packet_source_type("not xml"), None);
    }

    #[test]
    fn reads_the_camera_rating_from_the_files_xmp() {
        let exif = tiff_exif("2025:12:24 18:30:15");
        let rated = |xmp: &str| {
            let bytes = jpeg_with_xmp(&exif, xmp);
            let len = bytes.len() as u64;
            read_header(std::io::Cursor::new(bytes), len, FACTS_LIMIT)
                .0
                .rating
        };
        assert_eq!(rated(&camera_xmp("<xmp:Rating>3</xmp:Rating>")), Some(3));
        let attribute = camera_xmp("").replace(r#"rdf:about="""#, r#"rdf:about="" xmp:Rating="2""#);
        assert_eq!(rated(&attribute), Some(2));
        // Cleared (0) or rejected (-1) in the XMP: none, over the EXIF's 4.
        assert_eq!(rated(&camera_xmp("<xmp:Rating>0</xmp:Rating>")), Some(0));
        assert_eq!(rated(&camera_xmp("<xmp:Rating>-1</xmp:Rating>")), Some(0));
        assert_eq!(rated(&camera_xmp("")), Some(4), "no XMP rating: the EXIF's");
        // Another prefix for the same namespaces: still the file's packet.
        let prefixed = camera_xmp("<xmp:Rating>0</xmp:Rating>")
            .replace("x:xmpmeta", "m:xmpmeta")
            .replace("xmlns:x=", "xmlns:m=");
        assert_eq!(rated(&prefixed), Some(0), "a clear over the EXIF's 4");
        // A packet without a rating, then one with: the one with.
        let two = format!(
            "{}{}",
            camera_xmp(""),
            camera_xmp("<xmp:Rating>5</xmp:Rating>")
        );
        assert_eq!(xmp_rating(two.as_bytes()), Some(5));
        assert_eq!(xmp_rating(b"<x:xmpmeta never closed"), None);

        let d = tempfile::tempdir().unwrap();
        let photo = d.path().join("rated.jpg");
        let xmp = camera_xmp("<xmp:Rating>3</xmp:Rating>");
        fs::write(&photo, jpeg_with_xmp(&exif, &xmp)).unwrap();
        assert_eq!(capture_facts(&photo).unwrap().camera_rating, Some(3));
        let cleared = d.path().join("cleared.jpg");
        let xmp = camera_xmp("<xmp:Rating>0</xmp:Rating>");
        fs::write(&cleared, jpeg_with_xmp(&exif, &xmp)).unwrap();
        assert_eq!(capture_facts(&cleared).unwrap().camera_rating, None);
    }

    /// A TIFF-based RAW's own packet (IFD0's XMP tag) counts, not one of an
    /// embedded preview that comes first in the file.
    #[test]
    fn the_files_own_xmp_packet_wins_over_a_previews() {
        let preview = camera_xmp("<xmp:Rating>1</xmp:Rating>");
        // NUL-terminated, as a Panasonic RW2's tag 700 holds it.
        let own = camera_xmp("<xmp:Rating>4</xmp:Rating>") + "\0";
        let mut bytes = b"II*\0".to_vec();
        let ifd = 8 + preview.len() + own.len();
        bytes.extend((ifd as u32).to_le_bytes());
        bytes.extend(preview.as_bytes());
        let own_at = bytes.len() as u32;
        bytes.extend(own.as_bytes());
        bytes.extend(1u16.to_le_bytes());
        bytes.extend(700u16.to_le_bytes());
        bytes.extend(7u16.to_le_bytes());
        bytes.extend((own.len() as u32).to_le_bytes());
        bytes.extend(own_at.to_le_bytes());
        bytes.extend(0u32.to_le_bytes());
        let len = bytes.len() as u64;
        assert_eq!(xmp_rating(&bytes), Some(1), "scanning finds the preview's");
        let read = read_header(std::io::Cursor::new(bytes), len, FACTS_LIMIT).0;
        assert_eq!(read.rating, Some(4));
    }

    /// A damaged CR3 whose metadata box claims more than metadata holds is
    /// parsed within the budget, not read (and held) as far as it claims.
    #[test]
    fn a_damaged_bmff_box_is_read_within_the_budget() {
        let boxed = |kind: &[u8], body: &[u8]| {
            let mut out = ((8 + body.len()) as u32).to_be_bytes().to_vec();
            out.extend(kind);
            out.extend(body);
            out
        };
        let cmt1 = boxed(b"CMT1", &vec![0u8; 1 << 20]);
        let mut uuid = rawler::formats::bmff::ext_cr3::cr3desc::Cr3DescBox::UUID.to_vec();
        uuid.extend(&cmt1);
        let moov = boxed(b"moov", &boxed(b"uuid", &uuid));
        let mut bytes = boxed(b"ftyp", b"crx \0\0\0\x01crx ");
        bytes.extend(moov);
        bytes.extend(boxed(b"mdat", &[0u8; 64]));
        let len = bytes.len() as u64;
        let mut file = Counted(std::io::Cursor::new(&bytes), 0);
        let budget = 64 << 10;
        assert!(
            bmff_metadata_within(&mut file, len, budget)
                .unwrap()
                .is_none()
        );
        // Within the budget and one buffer of the reader above it.
        assert!(file.1 <= budget + (8 << 10), "{} bytes read", file.1);
    }

    /// A box whose extended size runs past the file ends the walk (and
    /// does not overflow the offset).
    #[test]
    fn a_bmff_box_past_the_end_ends_the_walk() {
        let mut bytes = 16u32.to_be_bytes().to_vec();
        bytes.extend(b"ftypcrx \0\0\0\x01");
        bytes.extend(1u32.to_be_bytes());
        bytes.extend(b"free");
        bytes.extend(u64::MAX.to_be_bytes());
        bytes.extend([0u8; 64]);
        let len = bytes.len() as u64;
        let header = bytes.clone();
        let found = container_xmp(&mut std::io::Cursor::new(&bytes), len, &header).unwrap();
        assert!(matches!(found, Container::NoPacket));
    }

    /// Metadata past what was read is kept without a capture date too (a
    /// DNG whose camera wrote none).
    #[test]
    fn keeps_late_metadata_without_a_date() {
        let mut metadata = rawler::decoders::RawMetadata {
            make: "Testmaker".into(),
            model: "RAW Two".into(),
            ..Default::default()
        };
        metadata.exif.iso_speed_ratings = Some(400);
        let mut seed = 0x9e37_79b9_u32;
        let image = image::Rgb32FImage::from_fn(640, 640, |_, _| {
            image::Rgb(std::array::from_fn(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                (seed % 1000) as f32 / 1000.0
            }))
        });
        let dng = rawtonic_merge::dng::encode_linear_dng(
            &image,
            &rawtonic_merge::dng::DngMeta {
                color: None,
                metadata: Some(&metadata),
                keep_lens: true,
                baseline_exposure: None,
                preview: None,
                software: "test",
            },
        )
        .unwrap();
        let len = dng.len() as u64;
        let limit = 64 << 10;
        assert!(len > 4 * limit, "{len}");
        let (capture, _) = read_header(std::io::Cursor::new(&dng), len, limit);
        assert_eq!(capture.captured, None);
        assert_eq!(
            (capture.model.as_deref(), capture.iso),
            (Some("RAW Two"), Some(400))
        );
    }

    /// An undated late IFD holding only the camera's EXIF rating keeps it.
    #[test]
    fn keeps_a_late_rating_without_other_facts() {
        let ifd = 64u32 << 10;
        let mut bytes = b"II*\0".to_vec();
        bytes.extend(ifd.to_le_bytes());
        bytes.resize(ifd as usize, 0);
        bytes.extend(1u16.to_le_bytes());
        for field in [
            &0x4746u16.to_le_bytes()[..],
            &3u16.to_le_bytes(),
            &1u32.to_le_bytes(),
            &4u32.to_le_bytes(),
        ] {
            bytes.extend(field);
        }
        bytes.extend(0u32.to_le_bytes());
        let len = bytes.len() as u64;
        let (capture, error) = read_header(std::io::Cursor::new(&bytes), len, 1024);
        assert!(error.is_none());
        assert_eq!(capture.rating, Some(4));
    }

    /// Descriptive metadata alone must survive the late-IFD read too.
    #[test]
    fn keeps_late_creator_copyright_and_serial_without_other_facts() {
        for tag in [0x013bu16, 0x8298, 0xa431] {
            let ifd = 64u32 << 10;
            let value = b"Test value\0";
            let mut bytes = b"II*\0".to_vec();
            bytes.extend(ifd.to_le_bytes());
            bytes.resize(ifd as usize, 0);
            if tag == 0xa431 {
                // BodySerialNumber belongs to the EXIF sub-IFD.
                bytes.extend(1u16.to_le_bytes());
                bytes.extend(0x8769u16.to_le_bytes());
                bytes.extend(4u16.to_le_bytes());
                bytes.extend(1u32.to_le_bytes());
                bytes.extend((ifd + 18).to_le_bytes());
                bytes.extend(0u32.to_le_bytes());
            }
            let table = bytes.len() as u32;
            bytes.extend(1u16.to_le_bytes());
            bytes.extend(tag.to_le_bytes());
            bytes.extend(2u16.to_le_bytes());
            bytes.extend((value.len() as u32).to_le_bytes());
            bytes.extend((table + 18).to_le_bytes());
            bytes.extend(0u32.to_le_bytes());
            bytes.extend(value);
            let len = bytes.len() as u64;
            let (capture, error) = read_header(std::io::Cursor::new(&bytes), len, 1024);
            assert!(error.is_none());
            assert_eq!(capture.captured, None);
            let found = match tag {
                0x013b => capture.creator,
                0x8298 => capture.copyright,
                _ => capture.serial,
            };
            assert_eq!(found.as_deref(), Some("Test value"), "tag {tag:04x}");
        }
    }

    /// A DNG (as merges write it: image data first, its IFDs at the end)
    /// larger than what is read of a file: its IFDs are followed where
    /// they are, without reading its image data.
    #[test]
    fn follows_tiff_ifds_past_the_read_limit() {
        let mut metadata = rawler::decoders::RawMetadata {
            make: "Testmaker".into(),
            model: "RAW One".into(),
            ..Default::default()
        };
        metadata.exif.date_time_original = Some("2024:07:14 06:15:30".into());
        metadata.exif.iso_speed_ratings = Some(200);
        metadata.exif.lens_model = Some("85mm F1.4".into());
        let mut seed = 0x2545_f491_u32;
        let image = image::Rgb32FImage::from_fn(640, 640, |_, _| {
            image::Rgb(std::array::from_fn(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                (seed % 1000) as f32 / 1000.0
            }))
        });
        let dng = rawtonic_merge::dng::encode_linear_dng(
            &image,
            &rawtonic_merge::dng::DngMeta {
                color: None,
                metadata: Some(&metadata),
                keep_lens: true,
                baseline_exposure: None,
                preview: None,
                software: "test",
            },
        )
        .unwrap();
        let len = dng.len() as u64;
        let limit = 256 << 10;
        assert!(len > 4 * limit, "{len}");
        assert_eq!(from_bytes(&dng[..limit as usize]).captured, None);
        let mut file = Counted(std::io::Cursor::new(&dng), 0);
        let (capture, error) = read_header(&mut file, len, limit);
        assert!(error.is_none());
        assert_eq!(
            capture.captured.map(|d| d.to_string()).as_deref(),
            Some("2024-07-14 06:15:30")
        );
        assert_eq!(
            (
                capture.model.as_deref(),
                capture.iso,
                capture.lens.as_deref()
            ),
            (Some("RAW One"), Some(200), Some("85mm F1.4"))
        );
        assert!(file.1 < limit + (64 << 10), "{} bytes read", file.1);
    }

    /// A file served from a few parts laid into zeros, as long as it says:
    /// a file of gigabytes without holding them.
    struct Virtual {
        len: u64,
        parts: Vec<(u64, Vec<u8>)>,
        at: u64,
    }
    impl Read for Virtual {
        fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
            let n = (out.len() as u64).min(self.len.saturating_sub(self.at)) as usize;
            out[..n].fill(0);
            for (start, bytes) in &self.parts {
                let (from, to) = (
                    self.at.max(*start),
                    (self.at + n as u64).min(start + bytes.len() as u64),
                );
                if from < to {
                    out[(from - self.at) as usize..(to - self.at) as usize]
                        .copy_from_slice(&bytes[(from - start) as usize..(to - start) as usize]);
                }
            }
            self.at += n as u64;
            Ok(n)
        }
    }
    impl Seek for Virtual {
        fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
            self.at = match to {
                SeekFrom::Start(n) => n,
                SeekFrom::End(n) => self.len.saturating_add_signed(n),
                SeekFrom::Current(n) => self.at.saturating_add_signed(n),
            };
            Ok(self.at)
        }
    }

    /// A DNG whose IFDs lie past 1 GiB (a large panorama merge) gets its
    /// facts: what is walked is laid after its header, not at its offsets.
    #[test]
    fn follows_ifds_past_a_gigabyte() {
        let le = |n: u32| n.to_le_bytes();
        let entry = |tag: u16, kind: u16, count: u32, value: u32| {
            [
                &tag.to_le_bytes()[..],
                &kind.to_le_bytes(),
                &le(count),
                &le(value),
            ]
            .concat()
        };
        let (root, exif, model, date) = (0x5000_0000u32, 0x5000_1000, 0x5000_2000, 0x5000_3000);
        let mut table = 2u16.to_le_bytes().to_vec();
        table.extend(entry(0x0110, 2, 9, model));
        table.extend(entry(0x8769, 4, 1, exif));
        table.extend(le(0));
        let mut exif_table = 1u16.to_le_bytes().to_vec();
        exif_table.extend(entry(0x9003, 2, 20, date));
        exif_table.extend(le(0));
        let mut header = b"II*\0".to_vec();
        header.extend(le(root));
        let len = u64::from(date) + 4096;
        let file = Virtual {
            len,
            parts: vec![
                (0, header),
                (u64::from(root), table),
                (u64::from(exif), exif_table),
                (u64::from(model), b"RAW Huge\0".to_vec()),
                (u64::from(date), b"2024:07:14 06:15:30\0".to_vec()),
            ],
            at: 0,
        };
        let mut file = Counted(file, 0);
        let (capture, error) = read_header(&mut file, len, FACTS_LIMIT);
        assert!(error.is_none());
        assert_eq!(
            (
                capture.model.as_deref(),
                capture.captured.map(|d| d.to_string()).as_deref()
            ),
            (Some("RAW Huge"), Some("2024-07-14 06:15:30"))
        );
        assert!(file.1 < FACTS_LIMIT + (64 << 10), "{} bytes read", file.1);
    }

    /// A uuid box too small for its own uuid is a damaged box, not a
    /// packet (and no overflow).
    #[test]
    fn a_uuid_box_smaller_than_its_uuid_is_no_packet() {
        let mut bytes = 16u32.to_be_bytes().to_vec();
        bytes.extend(b"ftypcrx \0\0\0\x01");
        bytes.extend(8u32.to_be_bytes());
        bytes.extend(b"uuid");
        bytes.extend(BMFF_XMP_UUID);
        bytes.extend([0u8; 64]);
        let len = bytes.len() as u64;
        let header = bytes.clone();
        let found = container_xmp(&mut std::io::Cursor::new(&bytes), len, &header).unwrap();
        assert!(matches!(found, Container::NoPacket));
    }

    /// An XMP packet past what is read for the date (after more than the
    /// first read step of other segments) is still found for the rating.
    #[test]
    fn reads_a_rating_past_the_date() {
        let exif = tiff_exif("2025:12:24 18:30:15");
        let early = jpeg_with_exif(&exif);
        let mut bytes = early[..early.len() - 2].to_vec();
        for _ in 0..40 {
            bytes.extend([0xFF, 0xE2]);
            bytes.extend(60_000u16.to_be_bytes());
            bytes.extend(vec![0u8; 59_998]);
        }
        let late = jpeg_with_xmp(&[], &camera_xmp("<xmp:Rating>2</xmp:Rating>"));
        // Its XMP segment only (after its empty EXIF segment).
        let exif_segment = 2 + 2 + 6;
        bytes.extend(&late[2 + exif_segment..]);
        assert!(bytes.len() > 2 << 20);
        let d = tempfile::tempdir().unwrap();
        let photo = d.path().join("late-xmp.jpg");
        fs::write(&photo, &bytes).unwrap();
        let facts = capture_facts(&photo).unwrap();
        assert_eq!(facts.captured.as_deref(), Some("2025-12-24 18:30:15"));
        assert_eq!(facts.camera_rating, Some(2));
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

    /// The library's facts come from the header steps only: a date past
    /// them is not looked for (the ingest's naming reads on for it).
    #[test]
    fn capture_facts_never_read_past_the_header_steps() {
        let d = tempfile::tempdir().unwrap();
        let photo = d.path().join("very-late.jpg");
        let mut bytes = vec![0xFF, 0xD8];
        // About 18 MB of APP2 segments, then the EXIF.
        for _ in 0..300 {
            bytes.extend([0xFF, 0xE2]);
            bytes.extend(60_000u16.to_be_bytes());
            bytes.extend(vec![0u8; 59_998]);
        }
        bytes.extend(&jpeg_with_exif(&tiff_exif("2024:05:06 07:08:09"))[2..]);
        assert!(bytes.len() as u64 > FACTS_LIMIT);
        fs::write(&photo, &bytes).unwrap();
        assert_eq!(read_capture(&photo).date_source, DateSource::Original);
        let facts = capture_facts(&photo).unwrap();
        assert_eq!(facts.captured, None);
        assert_eq!(facts.model, None);
    }

    /// A read that fails (a card or drive gone mid-read) gives no facts,
    /// so the photograph is read again later instead of kept as read.
    #[test]
    fn a_failed_read_gives_no_facts() {
        struct Breaks(usize);
        impl Read for Breaks {
            fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
                if self.0 == 0 {
                    return Err(std::io::Error::other("device gone"));
                }
                let n = out.len().min(self.0);
                out[..n].fill(0);
                self.0 -= n;
                Ok(n)
            }
        }
        impl Seek for Breaks {
            fn seek(&mut self, _: SeekFrom) -> std::io::Result<u64> {
                Err(std::io::Error::other("device gone"))
            }
        }
        assert!(read_header(Breaks(4096), 8 << 20, FACTS_LIMIT).1.is_some());
        let short = std::io::Cursor::new(&b"short file"[..]);
        assert!(read_header(short, 10, FACTS_LIMIT).1.is_none());
        let d = tempfile::tempdir().unwrap();
        assert_eq!(capture_facts(d.path()), None);
        assert_eq!(capture_facts(&d.path().join("gone.ARW")), None);
    }

    /// A file reader that counts the bytes read.
    pub(crate) struct Counted<R>(pub(crate) R, pub(crate) u64);
    impl<R: Read> Read for Counted<R> {
        fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
            let n = self.0.read(out)?;
            self.1 += n as u64;
            Ok(n)
        }
    }
    impl<R: Seek> Seek for Counted<R> {
        fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
            self.0.seek(to)
        }
    }

    /// Real RAWs (local fixtures, never in the repository): the library's
    /// facts of every file are what the whole file gives, however large it
    /// is (a CR3 keeps part of its metadata after its image data, tens of
    /// MB in), and a large file is not read whole for them.
    #[test]
    #[ignore = "requires RAWTONIC_RAW_FIXTURES pointing to a local sample directory"]
    fn capture_facts_of_real_raws_are_those_of_the_whole_file() {
        let dir = std::env::var("RAWTONIC_RAW_FIXTURES").expect("local fixture directory");
        let (mut large, mut most) = (0, 0);
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if !path.is_file() || path.file_name().unwrap().to_string_lossy().starts_with('.') {
                continue;
            }
            let whole = from_bytes(&fs::read(&path).unwrap());
            if whole.captured.is_none() {
                continue;
            }
            let len = fs::metadata(&path).unwrap().len();
            let mut file = Counted(fs::File::open(&path).unwrap(), 0);
            let (read, error) = read_header(&mut file, len, FACTS_LIMIT);
            assert!(error.is_none(), "{}", path.display());
            // Facts as a whole-file read gives them; the rating comes
            // from the file's own packet (`container_xmp`), not the decoders.
            let (mut read, whole) = (facts_of(read).unwrap(), facts_of(whole).unwrap());
            let rating = read.camera_rating.take();
            if let Some(rating) = rating {
                println!("{}: rated {rating}", path.display());
            }
            // The file's own packet, wherever it is, gives the rating its
            // text holds (it parses as XML).
            let bytes = fs::read(&path).unwrap();
            let header = &bytes[..bytes.len().min(1 << 16)];
            if let Container::Packet(packet) =
                container_xmp(&mut std::io::Cursor::new(&bytes), len, header).unwrap()
            {
                let held = xmp_rating(&packet).filter(|r| (1..=5).contains(r));
                assert_eq!(rating, held, "{}", path.display());
            }
            let whole = rawtonic_catalog::CaptureFacts {
                camera_rating: None,
                ..whole
            };
            assert_eq!(read, whole, "{}", path.display());
            if len > FACTS_LIMIT {
                large += 1;
                // The header steps, and a TIFF-based file's IFDs past them.
                assert!(
                    file.1 <= FACTS_LIMIT + late_ifds::METADATA_LIMIT,
                    "{}: {} bytes read",
                    path.display(),
                    file.1
                );
                println!("{}: {} of {len} bytes read", path.display(), file.1);
            }
            most = most.max(file.1);
        }
        assert!(large > 0, "no fixture larger than {FACTS_LIMIT} bytes");
        println!("{large} files over {FACTS_LIMIT} bytes; at most {most} bytes read of one file");
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
    fn sidecar_snapshots_are_reused_only_within_their_import() {
        let d = tempfile::tempdir().unwrap();
        let photo = d.path().join("A.ARW");
        let sidecar = d.path().join("A.xmp");
        fs::write(&photo, b"synthetic").unwrap();
        {
            let _scope = SidecarScope::new();
            let first = folder_files(d.path()).unwrap();
            assert_eq!(card_sidecar(&photo).unwrap(), None);
            fs::write(&sidecar, b"synthetic sidecar").unwrap();
            for _ in 0..1000 {
                let next = folder_files(d.path()).unwrap();
                assert!(Arc::ptr_eq(&first, &next), "one listing per import folder");
            }
        }
        {
            let _scope = SidecarScope::new();
            assert_eq!(card_sidecar(&photo).unwrap(), Some(sidecar.clone()));
        }
        // A standalone query must not inherit the preceding import's index.
        fs::remove_file(&sidecar).unwrap();
        assert_eq!(card_sidecar(&photo).unwrap(), None);
    }

    #[test]
    fn a_nested_or_panicking_import_does_not_leak_its_sidecar_snapshot() {
        let d = tempfile::tempdir().unwrap();
        let photo = d.path().join("A.ARW");
        let sidecar = d.path().join("A.xmp");
        fs::write(&photo, b"synthetic").unwrap();
        {
            let _outer = SidecarScope::new();
            assert_eq!(card_sidecar(&photo).unwrap(), None);
            fs::write(&sidecar, b"synthetic sidecar").unwrap();
            assert!(
                std::panic::catch_unwind(|| {
                    let _nested = SidecarScope::new();
                    assert_eq!(card_sidecar(&photo).unwrap(), Some(sidecar.clone()));
                    panic!("synthetic interrupted import");
                })
                .is_err()
            );
            assert_eq!(card_sidecar(&photo).unwrap(), None);
        }
        assert_eq!(card_sidecar(&photo).unwrap(), Some(sidecar));
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
