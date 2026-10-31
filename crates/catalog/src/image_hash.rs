//! The identity of a photograph's image data as its file stores it, apart
//! from the metadata around it: two files hash alike when they hold the same
//! image even with other names, EXIF/XMP/IPTC blocks, previews or sidecars
//! (Lightroom Classic's Find Duplicates: "identical image data despite
//! different names, metadata or catalog edits").
//!
//! What counts as the image data, by container:
//! - TIFF-based RAWs and TIFF (DNG, NEF, CR2, ARW, ORF, RW2, PEF, SRW, 3FR,
//!   TIFF…): the strips or tiles of the IFD holding the most image bytes —
//!   the sensor data (DNG: the raw SubIFD; previews and thumbnails are
//!   smaller), wherever a metadata edit moved it in the file.
//! - Fujifilm RAF: the CFA section the header points at.
//! - ISO media (Canon CR3, other `ftyp` files with tracks): the samples of
//!   the track with the most bytes (CR3: the full-size raw track).
//! - JPEG: every segment but APPn (EXIF, XMP, ICC, maker data) and comments,
//!   so the tables, frame header and entropy-coded scans, up to the first
//!   image's end (an MPF second image is not the photograph).
//! - PNG: IHDR, PLTE, tRNS and IDAT chunks (not text or eXIf).
//!
//! Anything else (HEIF item files, unreadable layouts) is identified by its
//! whole content, marked `file`: the narrower whole-file identity.
//! Hashes carry the scheme version and the container, so a change of what
//! is hashed recomputes them, and different containers never compare.
use anyhow::{Context, Result, bail, ensure};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{BufReader, Read, Seek, SeekFrom},
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};

/// Version of what is hashed; stored hashes of another version are stale.
pub const SCHEME: &str = "1";

/// Which part of a file was hashed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Container {
    Tiff,
    Raf,
    Bmff,
    Jpeg,
    Png,
    /// The whole file (no known image-data layout).
    File,
}

impl Container {
    pub fn name(self) -> &'static str {
        match self {
            Container::Tiff => "tiff",
            Container::Raf => "raf",
            Container::Bmff => "bmff",
            Container::Jpeg => "jpeg",
            Container::Png => "png",
            Container::File => "file",
        }
    }
}

/// `SCHEME:container:sha256-hex` of the image data of the file at `path`;
/// `None` when `cancel` was set meanwhile.
pub fn image_hash(path: &Path, cancel: &AtomicBool) -> Result<Option<String>> {
    let mut file = File::open(path).with_context(|| format!("Cannot read {}", path.display()))?;
    let len = file.metadata()?.len();
    let mut head = [0u8; 16];
    let n = read_up_to(&mut file, 0, &mut head)?;
    let head = &head[..n];
    let mut hash = Sha256::new();
    let located = match sniff(head) {
        Some(Container::Jpeg) => jpeg(&mut file, &mut hash, cancel).map(|()| Container::Jpeg),
        Some(Container::Png) => png(&mut file, len, &mut hash, cancel).map(|()| Container::Png),
        Some(kind) => layout(&mut file, len, kind).and_then(|(how, ranges)| {
            hash.update((how.len() as u64).to_be_bytes());
            hash.update(&how);
            hash_ranges(&mut file, &ranges, &mut hash, cancel)?;
            Ok(kind)
        }),
        None => Err(anyhow::anyhow!("no known layout")),
    };
    let container = match located {
        Ok(kind) => kind,
        Err(e) => {
            if cancel.load(Ordering::Relaxed) {
                return Ok(None);
            }
            log::debug!("{}: whole-file identity ({e:#})", path.display());
            hash = Sha256::new();
            if let Err(e) = hash_ranges(&mut file, &[(0, len)], &mut hash, cancel) {
                if cancel.load(Ordering::Relaxed) {
                    return Ok(None);
                }
                return Err(e);
            }
            Container::File
        }
    };
    if cancel.load(Ordering::Relaxed) {
        return Ok(None);
    }
    let hex: String = hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
    Ok(Some(format!("{SCHEME}:{}:{hex}", container.name())))
}

/// Whether a stored hash was made by this scheme (else it is recomputed).
pub fn current(hash: &str) -> bool {
    hash.split_once(':')
        .is_some_and(|(scheme, _)| scheme == SCHEME)
}

fn sniff(head: &[u8]) -> Option<Container> {
    if head.starts_with(b"FUJIFILMCCD-RAW") {
        Some(Container::Raf)
    } else if head.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some(Container::Jpeg)
    } else if head.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(Container::Png)
    } else if head.len() >= 8 && &head[4..8] == b"ftyp" {
        Some(Container::Bmff)
    } else if head.len() >= 4 && (head[..2] == *b"II" || head[..2] == *b"MM") {
        // TIFF (42), BigTIFF is not read; Olympus ORF (IIRO, IIRS, MMOR) and
        // Panasonic RW2 (IIU) use other magic numbers in a TIFF layout.
        let little = head[0] == b'I';
        let magic = if little {
            u16::from_le_bytes([head[2], head[3]])
        } else {
            u16::from_be_bytes([head[2], head[3]])
        };
        matches!(magic, 42 | 0x4F52 | 0x5352 | 0x0055).then_some(Container::Tiff)
    } else {
        None
    }
}

/// How the image data decodes, and its byte ranges (offset, length).
type Layout = (Vec<u8>, Vec<(u64, u64)>);

/// How the image data is to be decoded (the layout facts stored apart from
/// it: TIFF dimensions, sample layout and photometric tags, the RAF CFA
/// header, the ISO media sample description), and its byte ranges (offset,
/// length) in file order as stored. Both are hashed: the same bytes decoded
/// another way are another image.
fn layout(file: &mut File, len: u64, kind: Container) -> Result<Layout> {
    let (how, ranges) = match kind {
        Container::Tiff => tiff_layout(file, len)?,
        Container::Raf => {
            let mut header = [0u8; 108];
            file.seek(SeekFrom::Start(0))?;
            file.read_exact(&mut header)?;
            let be =
                |at: usize| u64::from(u32::from_be_bytes(header[at..at + 4].try_into().unwrap()));
            let (at, n) = (be(92), be(96));
            ensure!(n <= 1 << 20 && at + n <= len, "CFA header outside the file");
            (read_at(file, at, n as usize)?, vec![(be(100), be(104))])
        }
        Container::Bmff => bmff_layout(file, len)?,
        _ => bail!("not a range layout"),
    };
    ensure!(!ranges.is_empty(), "no image data");
    for &(offset, length) in &ranges {
        ensure!(
            length > 0 && offset.checked_add(length).is_some_and(|end| end <= len),
            "image data outside the file"
        );
    }
    Ok((how, ranges))
}

fn read_up_to(file: &mut File, offset: u64, buffer: &mut [u8]) -> Result<usize> {
    file.seek(SeekFrom::Start(offset))?;
    let mut filled = 0;
    while filled < buffer.len() {
        let n = file.read(&mut buffer[filled..])?;
        if n == 0 {
            break;
        }
        filled += n;
    }
    Ok(filled)
}

fn read_at(file: &mut File, offset: u64, len: usize) -> Result<Vec<u8>> {
    let mut buffer = vec![0; len];
    file.seek(SeekFrom::Start(offset))?;
    file.read_exact(&mut buffer)?;
    Ok(buffer)
}

fn hash_ranges(
    file: &mut File,
    ranges: &[(u64, u64)],
    hash: &mut Sha256,
    cancel: &AtomicBool,
) -> Result<()> {
    let mut buffer = vec![0; 1024 * 1024];
    for &(offset, length) in ranges {
        file.seek(SeekFrom::Start(offset))?;
        let mut left = length;
        while left > 0 {
            if cancel.load(Ordering::Relaxed) {
                bail!("cancelled");
            }
            let n = left.min(buffer.len() as u64) as usize;
            file.read_exact(&mut buffer[..n])?;
            hash.update(&buffer[..n]);
            left -= n as u64;
        }
    }
    Ok(())
}

// TIFF tags.
const STRIP_OFFSETS: u16 = 0x111;
const STRIP_BYTE_COUNTS: u16 = 0x117;
const TILE_OFFSETS: u16 = 0x144;
const TILE_BYTE_COUNTS: u16 = 0x145;
const SUB_IFDS: u16 = 0x14A;
const EXIF_IFD: u16 = 0x8769;
/// NewSubfileType: bit 0 marks a reduced-resolution image (a preview).
const NEW_SUBFILE_TYPE: u16 = 0xFE;
/// Panasonic RW2: where the raw data starts.
const RW2_RAW_OFFSET: u16 = 0x118;

/// Tags of the image IFD left out of its identity: where its data and the
/// other IFDs are (they move when metadata is rewritten), and descriptive
/// metadata (names, dates, software, resolution, orientation, rating,
/// XMP/IPTC, maker and DNG private data, profile names). Every other tag
/// of that IFD (dimensions, sample layout, compression, photometric,
/// palette, CFA, levels, vendor decoding tags) is hashed with the data, so
/// an unknown tag can only make two copies look different, never two
/// images alike.
const NOT_IMAGE_TAGS: &[u16] = &[
    // Positions and pointers.
    0x111, 0x117, 0x144, 0x145, 0x14A, 0x201, 0x202, 0x7200, 0x8769, 0x8825, 0xA005, 0xC634,
    // Descriptive metadata.
    0x10D, 0x10E, 0x10F, 0x110, 0x112, 0x11A, 0x11B, 0x11D, 0x128, 0x131, 0x132, 0x13B, 0x13C,
    0x2BC, 0x4746, 0x4749, 0x8298, 0x83BB, 0x927C, 0x9C9B, 0x9C9C, 0x9C9D, 0x9C9E, 0x9C9F, 0xC4A5,
    0xC612, 0xC613, 0xC614, 0xC615, 0xC62F, 0xC630, 0xC68B, 0xC68C, 0xC6F3, 0xC6F4, 0xC6F8,
];

/// EXIF fields some writers put in an image IFD (dates, exposure, lens).
fn exif_field(tag: u16) -> bool {
    (0x8822..=0x8832).contains(&tag)
        || (0x9000..=0x9FFF).contains(&tag)
        || (0xA000..=0xAFFF).contains(&tag)
}

/// An IFD with image data.
struct ImageIfd {
    /// NewSubfileType says reduced resolution (a preview).
    reduced: bool,
    /// Byte order and its tags but positions and descriptions.
    how: Vec<u8>,
    ranges: Vec<(u64, u64)>,
    total: u64,
    /// A field could not be read.
    unreadable: bool,
}

/// The image IFDs of a TIFF layout (the one with the most image bytes and
/// the other full-resolution ones): their tags but positions and
/// descriptions, and their strips or tiles.
fn tiff_layout(file: &mut File, len: u64) -> Result<Layout> {
    let head = read_at(file, 0, 8)?;
    let little = head[0] == b'I';
    let u16_at = |b: &[u8], at: usize| {
        let v = [b[at], b[at + 1]];
        if little {
            u16::from_le_bytes(v)
        } else {
            u16::from_be_bytes(v)
        }
    };
    let u32_at = |b: &[u8], at: usize| {
        let v = [b[at], b[at + 1], b[at + 2], b[at + 3]];
        if little {
            u32::from_le_bytes(v)
        } else {
            u32::from_be_bytes(v)
        }
    };
    let rw2 = u16_at(&head, 2) == 0x55;
    let mut queue = vec![u64::from(u32_at(&head, 4))];
    let mut seen = std::collections::HashSet::new();
    let mut images: Vec<ImageIfd> = Vec::new();
    while let Some(ifd) = queue.pop() {
        if ifd == 0 || seen.len() >= 64 || !seen.insert(ifd) || ifd + 2 > len {
            continue;
        }
        let count = usize::from(u16_at(&read_at(file, ifd, 2)?, 0));
        ensure!(count <= 4096, "implausible IFD");
        let entries = read_at(file, ifd + 2, count * 12 + 4)?;
        // A field's values as numbers (rationals as their two parts), in
        // the file's byte order whatever it is.
        let mut values = |at: usize| -> Result<Vec<u64>> {
            let kind = u16_at(&entries, at + 2);
            let n = u32_at(&entries, at + 4) as usize;
            ensure!(n <= 1 << 20, "implausible value count");
            let (size, parts) = match kind {
                1 | 2 | 6 | 7 => (1, 1),
                3 | 8 => (2, 1),
                4 | 9 | 11 | 13 => (4, 1),
                5 | 10 | 12 => (4, 2),
                _ => bail!("values of type {kind}"),
            };
            let bytes = n * size * parts;
            let data = if bytes <= 4 {
                entries[at + 8..at + 12].to_vec()
            } else {
                read_at(file, u64::from(u32_at(&entries, at + 8)), bytes)?
            };
            Ok((0..n * parts)
                .map(|i| match size {
                    1 => u64::from(data[i]),
                    2 => u64::from(u16_at(&data, i * 2)),
                    _ => u64::from(u32_at(&data, i * 4)),
                })
                .collect())
        };
        let mut offsets = Vec::new();
        let mut counts = Vec::new();
        let mut raw_offset = None;
        // By tag, whatever order a writer put them in; a field that cannot
        // be read makes the IFD unusable as an identity.
        let mut decoding = std::collections::BTreeMap::new();
        let mut unreadable = false;
        for i in 0..count {
            let at = i * 12;
            match u16_at(&entries, at) {
                STRIP_OFFSETS | TILE_OFFSETS => offsets = values(at)?,
                STRIP_BYTE_COUNTS | TILE_BYTE_COUNTS => counts = values(at)?,
                RW2_RAW_OFFSET if rw2 => raw_offset = values(at)?.first().copied(),
                SUB_IFDS | EXIF_IFD => queue.extend(values(at)?),
                tag if NOT_IMAGE_TAGS.contains(&tag) || exif_field(tag) => {}
                tag => match values(at) {
                    Ok(found) => {
                        decoding.insert(tag, found);
                    }
                    Err(_) => unreadable = true,
                },
            }
        }
        // RW2 (magic 0x55) gives the raw data's start in tag 0x118
        // (MinSampleValue elsewhere) and an unusable strip offset; the data
        // runs for the strip's byte count, or to the end of the file.
        if let Some(start) = raw_offset {
            let to_end = len.saturating_sub(start);
            offsets = vec![start];
            counts = match counts[..] {
                [n] if n <= to_end => vec![n],
                _ => vec![to_end],
            };
        }
        if !offsets.is_empty() && offsets.len() == counts.len() {
            let total: u64 = counts.iter().sum();
            let reduced = decoding
                .get(&NEW_SUBFILE_TYPE)
                .and_then(|v| v.first())
                .is_some_and(|kind| kind & 1 == 1);
            // The byte order says how samples wider than a byte read.
            let mut how = vec![head[0]];
            for (tag, found) in decoding {
                how.extend(tag.to_be_bytes());
                how.extend((found.len() as u32).to_be_bytes());
                for value in found {
                    how.extend(value.to_be_bytes());
                }
            }
            images.push(ImageIfd {
                reduced,
                how,
                ranges: offsets.into_iter().zip(counts).collect(),
                total,
                unreadable,
            });
        }
        queue.push(u64::from(u32_at(&entries, count * 12)));
    }
    // The IFD with the most image bytes (the sensor data), then every other
    // full-resolution one (a DNG's transparency mask, depth map or other
    // planes the decoder combines with it), not reduced-resolution
    // previews; ordered by their descriptor and size, not their place.
    let main = (0..images.len())
        .max_by_key(|&i| images[i].total)
        .context("no image data")?;
    let mut rest: Vec<usize> = (0..images.len())
        .filter(|&i| i != main && !images[i].reduced)
        .collect();
    rest.sort_by(|&a, &b| {
        (&images[a].how, images[a].total).cmp(&(&images[b].how, images[b].total))
    });
    let mut layout: Layout = (Vec::new(), Vec::new());
    for i in std::iter::once(main).chain(rest) {
        let ImageIfd {
            how,
            ranges,
            unreadable,
            ..
        } = &images[i];
        ensure!(!unreadable, "image IFD with unreadable fields");
        layout.0.extend((how.len() as u64).to_be_bytes());
        layout.0.extend(how);
        layout.0.extend((ranges.len() as u64).to_be_bytes());
        layout.1.extend(ranges);
    }
    Ok(layout)
}

/// The sample description and samples of the track with the most bytes in
/// an ISO media file.
fn bmff_layout(file: &mut File, len: u64) -> Result<Layout> {
    let moov = boxes(file, 0, len)?
        .into_iter()
        .find(|b| &b.kind == b"moov")
        .context("no moov")?;
    let mut best: Layout = (Vec::new(), Vec::new());
    let mut best_total = 0;
    for trak in boxes(file, moov.body, moov.end)? {
        if &trak.kind != b"trak" {
            continue;
        }
        let Some(stbl) = path(file, &trak, &[b"mdia", b"minf", b"stbl"])? else {
            continue;
        };
        let samples = track_samples(file, &stbl)?;
        let total: u64 = samples.iter().map(|s| s.1).sum();
        if total > best_total {
            best_total = total;
            let description = match boxes(file, stbl.body, stbl.end)?
                .into_iter()
                .find(|b| &b.kind == b"stsd")
            {
                Some(b) if b.end - b.body <= 1 << 20 => {
                    read_at(file, b.body, (b.end - b.body) as usize)?
                }
                _ => Vec::new(),
            };
            best = (description, samples);
        }
    }
    Ok(best)
}

struct BmffBox {
    kind: [u8; 4],
    body: u64,
    end: u64,
}

fn boxes(file: &mut File, start: u64, end: u64) -> Result<Vec<BmffBox>> {
    let mut found = Vec::new();
    let mut at = start;
    while at + 8 <= end && found.len() < 4096 {
        let head = read_at(file, at, 8)?;
        let size32 = u64::from(u32::from_be_bytes(head[..4].try_into().unwrap()));
        let kind: [u8; 4] = head[4..8].try_into().unwrap();
        let (size, header) = match size32 {
            0 => (end - at, 8),
            1 => (
                u64::from_be_bytes(read_at(file, at + 8, 8)?.try_into().unwrap()),
                16,
            ),
            n => (n, 8),
        };
        ensure!(size >= header && at + size <= end, "box outside its parent");
        found.push(BmffBox {
            kind,
            body: at + header,
            end: at + size,
        });
        at += size;
    }
    Ok(found)
}

fn path(file: &mut File, from: &BmffBox, names: &[&[u8; 4]]) -> Result<Option<BmffBox>> {
    let mut current = BmffBox {
        kind: from.kind,
        body: from.body,
        end: from.end,
    };
    for name in names {
        match boxes(file, current.body, current.end)?
            .into_iter()
            .find(|b| &b.kind == *name)
        {
            Some(b) => current = b,
            None => return Ok(None),
        }
    }
    Ok(Some(current))
}

/// (offset, size) of every sample of a track's sample table.
fn track_samples(file: &mut File, stbl: &BmffBox) -> Result<Vec<(u64, u64)>> {
    let children = boxes(file, stbl.body, stbl.end)?;
    let body = |file: &mut File, kind: &[u8; 4]| -> Result<Option<Vec<u8>>> {
        match children.iter().find(|b| &b.kind == kind) {
            Some(b) => {
                let n = usize::try_from(b.end - b.body)?;
                ensure!(n <= 64 << 20, "implausible sample table");
                Ok(Some(read_at(file, b.body, n)?))
            }
            None => Ok(None),
        }
    };
    let be32 = |b: &[u8], at: usize| -> Result<u32> {
        Ok(u32::from_be_bytes(
            b.get(at..at + 4)
                .context("short table")?
                .try_into()
                .unwrap(),
        ))
    };
    let stsz = body(file, b"stsz")?.context("no stsz")?;
    let fixed = be32(&stsz, 4)?;
    let count = be32(&stsz, 8)? as usize;
    let sizes = (0..count)
        .map(|i| {
            if fixed != 0 {
                Ok(u64::from(fixed))
            } else {
                be32(&stsz, 12 + i * 4).map(u64::from)
            }
        })
        .collect::<Result<Vec<_>>>()?;
    let chunks: Vec<u64> = if let Some(co64) = body(file, b"co64")? {
        let n = be32(&co64, 4)? as usize;
        (0..n)
            .map(|i| {
                Ok(u64::from_be_bytes(
                    co64.get(8 + i * 8..16 + i * 8)
                        .context("short table")?
                        .try_into()
                        .unwrap(),
                ))
            })
            .collect::<Result<_>>()?
    } else {
        let stco = body(file, b"stco")?.context("no chunk offsets")?;
        let n = be32(&stco, 4)? as usize;
        (0..n)
            .map(|i| be32(&stco, 8 + i * 4).map(u64::from))
            .collect::<Result<_>>()?
    };
    // Samples per chunk (stsc runs: first chunk, samples per chunk); one per
    // chunk without a table.
    let mut per_chunk = vec![1u64; chunks.len()];
    if let Some(stsc) = body(file, b"stsc")? {
        let runs = be32(&stsc, 4)? as usize;
        for r in 0..runs {
            let first = be32(&stsc, 8 + r * 12)? as usize;
            let n = u64::from(be32(&stsc, 12 + r * 12)?);
            let next = if r + 1 < runs {
                be32(&stsc, 8 + (r + 1) * 12)? as usize
            } else {
                chunks.len() + 1
            };
            for c in first.max(1)..next.min(chunks.len() + 1) {
                per_chunk[c - 1] = n;
            }
        }
    }
    let mut samples = Vec::with_capacity(sizes.len());
    let mut sizes_left = sizes.iter();
    for (offset, n) in chunks.iter().zip(per_chunk) {
        let mut at = *offset;
        for _ in 0..n {
            let Some(&size) = sizes_left.next() else {
                break;
            };
            samples.push((at, size));
            at += size;
        }
    }
    ensure!(samples.len() == sizes.len(), "sample table does not add up");
    Ok(samples)
}

/// Hashes a JPEG's segments except APPn and comments, through the first
/// image's end.
fn jpeg(file: &mut File, hash: &mut Sha256, cancel: &AtomicBool) -> Result<()> {
    file.seek(SeekFrom::Start(0))?;
    let mut r = BufReader::with_capacity(1 << 20, file);
    let mut byte = || -> Result<u8> {
        let mut b = [0u8; 1];
        r.read_exact(&mut b)?;
        Ok(b[0])
    };
    ensure!(byte()? == 0xFF && byte()? == 0xD8, "no SOI");
    let mut scanned = false;
    let mut marker = next_marker(&mut byte)?;
    let mut steps = 0u64;
    loop {
        if steps.is_multiple_of(1 << 20) && cancel.load(Ordering::Relaxed) {
            bail!("cancelled");
        }
        steps += 1;
        match marker {
            0xD9 => {
                ensure!(scanned, "no image");
                hash.update([0xFF, 0xD9]);
                return Ok(());
            }
            0x01 | 0xD0..=0xD7 => {
                hash.update([0xFF, marker]);
                marker = next_marker(&mut byte)?;
            }
            _ => {
                let length = usize::from(u16::from_be_bytes([byte()?, byte()?]));
                ensure!(length >= 2, "bad segment length");
                let mut body = vec![0; length - 2];
                for b in body.iter_mut() {
                    *b = byte()?;
                }
                let metadata = (0xE0..=0xEF).contains(&marker) || marker == 0xFE;
                if !metadata {
                    hash.update([0xFF, marker]);
                    hash.update((length as u16).to_be_bytes());
                    hash.update(&body);
                }
                if marker == 0xDA {
                    scanned = true;
                    // Entropy-coded data up to the next marker that is not a
                    // stuffed byte or a restart.
                    let mut data = Vec::with_capacity(1 << 16);
                    marker = loop {
                        let b = byte()?;
                        if b != 0xFF {
                            data.push(b);
                            continue;
                        }
                        let mut m = byte()?;
                        while m == 0xFF {
                            m = byte()?;
                        }
                        match m {
                            0x00 | 0xD0..=0xD7 => {
                                data.extend_from_slice(&[0xFF, m]);
                            }
                            _ => break m,
                        }
                        if data.len() >= 1 << 20 {
                            hash.update(&data);
                            data.clear();
                            if cancel.load(Ordering::Relaxed) {
                                bail!("cancelled");
                            }
                        }
                    };
                    hash.update(&data);
                } else {
                    marker = next_marker(&mut byte)?;
                }
            }
        }
    }
}

fn next_marker(byte: &mut impl FnMut() -> Result<u8>) -> Result<u8> {
    ensure!(byte()? == 0xFF, "expected a marker");
    let mut m = byte()?;
    while m == 0xFF {
        m = byte()?;
    }
    Ok(m)
}

/// Hashes a PNG's image chunks (header, palette, transparency, data).
fn png(file: &mut File, len: u64, hash: &mut Sha256, cancel: &AtomicBool) -> Result<()> {
    let mut at = 8;
    let mut found = false;
    while at + 12 <= len {
        let head = read_at(file, at, 8)?;
        let n = u64::from(u32::from_be_bytes(head[..4].try_into().unwrap()));
        let kind: [u8; 4] = head[4..8].try_into().unwrap();
        ensure!(at + 12 + n <= len, "chunk outside the file");
        if matches!(&kind, b"IHDR" | b"PLTE" | b"tRNS" | b"IDAT") {
            hash.update(kind);
            hash_ranges(file, &[(at + 8, n)], hash, cancel)?;
            found |= &kind == b"IDAT";
        }
        if &kind == b"IEND" {
            break;
        }
        at += 12 + n;
    }
    ensure!(found, "no image data");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, path::PathBuf};

    fn none() -> AtomicBool {
        AtomicBool::new(false)
    }

    fn hash(path: &Path) -> String {
        image_hash(path, &none()).unwrap().unwrap()
    }

    /// A little-endian TIFF: IFD0 (a small preview strip, an EXIF IFD with
    /// `exif` as its text, a SubIFD) and the SubIFD's raw strips; `pad`
    /// bytes of padding before the raw data move it, as a metadata edit can.
    fn tiff(raw: &[&[u8]], exif: &[u8], pad: usize) -> Vec<u8> {
        tiff_as(raw, exif, pad, 1, false)
    }

    /// `tiff` with the raw IFD's PhotometricInterpretation, and its
    /// RowsPerStrip first or last (writers order tags differently).
    fn tiff_as(
        raw: &[&[u8]],
        exif: &[u8],
        pad: usize,
        photometric: u16,
        rows_last: bool,
    ) -> Vec<u8> {
        let mut out = b"II*\0".to_vec();
        out.extend(8u32.to_le_bytes());
        let entry = |out: &mut Vec<u8>, tag: u16, kind: u16, n: u32, value: u32| {
            out.extend(tag.to_le_bytes());
            out.extend(kind.to_le_bytes());
            out.extend(n.to_le_bytes());
            out.extend(value.to_le_bytes());
        };
        // IFD0 at 8: 4 entries.
        let ifd0_len = 2 + 4 * 12 + 4;
        let preview_at = 8 + ifd0_len;
        let preview = [7u8; 16];
        let exif_ifd_at = preview_at + preview.len();
        let exif_ifd_len = 2 + 12 + 4;
        let exif_text_at = exif_ifd_at + exif_ifd_len;
        let sub_at = exif_text_at + exif.len();
        let sub_len = 2 + 4 * 12 + 4;
        let arrays_at = sub_at + sub_len;
        let data_at = arrays_at + raw.len() * 8 + pad;
        out.extend(4u16.to_le_bytes());
        entry(&mut out, STRIP_OFFSETS, 4, 1, preview_at as u32);
        entry(&mut out, STRIP_BYTE_COUNTS, 4, 1, preview.len() as u32);
        entry(&mut out, SUB_IFDS, 4, 1, sub_at as u32);
        entry(&mut out, EXIF_IFD, 4, 1, exif_ifd_at as u32);
        out.extend(0u32.to_le_bytes());
        out.extend(preview);
        out.extend(1u16.to_le_bytes());
        entry(&mut out, 0x9286, 7, exif.len() as u32, exif_text_at as u32);
        out.extend(0u32.to_le_bytes());
        out.extend(exif);
        out.extend(4u16.to_le_bytes());
        if !rows_last {
            entry(&mut out, 0x116, 4, 1, 64);
        }
        entry(&mut out, 0x106, 3, 1, u32::from(photometric));
        let (offsets_at, counts_at) = (arrays_at, arrays_at + raw.len() * 4);
        let one = raw.len() == 1;
        entry(
            &mut out,
            STRIP_OFFSETS,
            4,
            raw.len() as u32,
            if one {
                data_at as u32
            } else {
                offsets_at as u32
            },
        );
        entry(
            &mut out,
            STRIP_BYTE_COUNTS,
            4,
            raw.len() as u32,
            if one {
                raw[0].len() as u32
            } else {
                counts_at as u32
            },
        );
        if rows_last {
            entry(&mut out, 0x116, 4, 1, 64);
        }
        out.extend(0u32.to_le_bytes());
        let mut at = data_at;
        for strip in raw {
            out.extend((at as u32).to_le_bytes());
            at += strip.len();
        }
        for strip in raw {
            out.extend((strip.len() as u32).to_le_bytes());
        }
        out.extend(vec![0u8; pad]);
        for strip in raw {
            out.extend(*strip);
        }
        out
    }

    fn write(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn tiff_raw_data_identity_ignores_metadata_and_its_place() {
        let dir = tempfile::tempdir().unwrap();
        let sensor: Vec<u8> = (0..5000u32).map(|i| (i * 7 % 251) as u8).collect();
        let (a, b) = sensor.split_at(2400);
        let original = write(dir.path(), "a.dng", &tiff(&[a, b], b"rating 0", 0));
        let edited = write(
            dir.path(),
            "b.dng",
            &tiff(&[a, b], b"rating 5 and a longer caption", 37),
        );
        let mut other = sensor.clone();
        other[4000] ^= 1;
        let (c, d) = other.split_at(2400);
        let different = write(dir.path(), "c.dng", &tiff(&[c, d], b"rating 0", 0));
        assert_ne!(fs::read(&original).unwrap(), fs::read(&edited).unwrap());
        assert!(hash(&original).starts_with("1:tiff:"));
        assert_eq!(hash(&original), hash(&edited));
        assert_ne!(hash(&original), hash(&different));
        let single = write(dir.path(), "d.nef", &tiff(&[&sensor], b"x", 3));
        assert!(hash(&single).starts_with("1:tiff:"));
        // The same bytes decoded another way (black is zero instead of
        // white) are another image.
        let inverted = write(
            dir.path(),
            "e.tif",
            &tiff_as(&[a, b], b"rating 0", 0, 0, false),
        );
        assert_ne!(hash(&original), hash(&inverted));
        let reordered = write(dir.path(), "f.dng", &tiff_as(&[a, b], b"x", 5, 1, true));
        assert_eq!(hash(&original), hash(&reordered));
    }

    /// A one-IFD TIFF in either byte order: one strip of `data` and `extra`
    /// fields (tag, type, values as bytes in the file's order), sorted by
    /// tag as written.
    fn plain_tiff(big: bool, extra: &[(u16, u16, Vec<u8>)], data: &[u8]) -> Vec<u8> {
        let n16 = |v: u16| {
            if big {
                v.to_be_bytes()
            } else {
                v.to_le_bytes()
            }
        };
        let n32 = |v: u32| {
            if big {
                v.to_be_bytes()
            } else {
                v.to_le_bytes()
            }
        };
        let mut fields: Vec<(u16, u16, u32, Vec<u8>)> = vec![
            (0x100, 3, 1, n16(2).to_vec()),
            (0x101, 3, 1, n16(1).to_vec()),
            (0x102, 3, 1, n16(16).to_vec()),
            (STRIP_OFFSETS, 4, 1, Vec::new()),
            (STRIP_BYTE_COUNTS, 4, 1, n32(data.len() as u32).to_vec()),
        ];
        for (tag, kind, bytes) in extra {
            let size = if *kind == 3 { 2 } else { 1 };
            fields.push((*tag, *kind, (bytes.len() / size) as u32, bytes.clone()));
        }
        fields.sort_by_key(|f| f.0);
        let mut out = if big {
            b"MM\0*".to_vec()
        } else {
            b"II*\0".to_vec()
        };
        out.extend(n32(8));
        let ifd_len = 2 + fields.len() * 12 + 4;
        let mut tail_at = 8 + ifd_len;
        let mut tail: Vec<u8> = Vec::new();
        let data_at = tail_at
            + fields
                .iter()
                .filter(|f| f.3.len() > 4)
                .map(|f| f.3.len())
                .sum::<usize>();
        out.extend(n16(fields.len() as u16));
        for (tag, kind, count, bytes) in &fields {
            out.extend(n16(*tag));
            out.extend(n16(*kind));
            out.extend(n32(*count));
            if *tag == STRIP_OFFSETS {
                out.extend(n32(data_at as u32));
            } else if bytes.len() > 4 {
                out.extend(n32(tail_at as u32));
                tail_at += bytes.len();
                tail.extend(bytes);
            } else {
                let mut inline = bytes.clone();
                inline.resize(4, 0);
                out.extend(inline);
            }
        }
        out.extend(n32(0));
        out.extend(tail);
        out.extend(data);
        out
    }

    /// A little-endian TIFF with a main image IFD (16-bit, `main` as its
    /// strip) chained to a second one of NewSubfileType `kind` (4: a DNG
    /// transparency mask, 1: a preview) holding `extra`.
    fn two_images(main: &[u8], kind: u32, extra: &[u8]) -> Vec<u8> {
        let ifd = |out: &mut Vec<u8>, kind: u32, data_at: usize, len: usize, next: usize| {
            out.extend(5u16.to_le_bytes());
            for (tag, typ, value) in [
                (NEW_SUBFILE_TYPE, 4u16, kind),
                (0x100, 3, 2),
                (0x101, 3, 1),
                (STRIP_OFFSETS, 4, data_at as u32),
                (STRIP_BYTE_COUNTS, 4, len as u32),
            ] {
                out.extend(tag.to_le_bytes());
                out.extend(typ.to_le_bytes());
                out.extend(1u32.to_le_bytes());
                out.extend(value.to_le_bytes());
            }
            out.extend((next as u32).to_le_bytes());
        };
        let ifd_len = 2 + 5 * 12 + 4;
        let (first, second) = (8, 8 + ifd_len);
        let main_at = second + ifd_len;
        let extra_at = main_at + main.len();
        let mut out = b"II*\0".to_vec();
        out.extend((first as u32).to_le_bytes());
        ifd(&mut out, 0, main_at, main.len(), second);
        ifd(&mut out, kind, extra_at, extra.len(), 0);
        out.extend(main);
        out.extend(extra);
        out
    }

    #[test]
    fn full_resolution_planes_count_and_previews_do_not() {
        let dir = tempfile::tempdir().unwrap();
        let main = [10u8; 64];
        let masked = write(
            dir.path(),
            "a.dng",
            &two_images(&main, 4, &[255, 255, 0, 0]),
        );
        let unmasked = write(
            dir.path(),
            "b.dng",
            &two_images(&main, 4, &[255, 255, 255, 255]),
        );
        assert_ne!(hash(&masked), hash(&unmasked));
        let preview = write(dir.path(), "c.dng", &two_images(&main, 1, &[1, 2, 3, 4]));
        let new_preview = write(dir.path(), "d.dng", &two_images(&main, 1, &[4, 3, 2, 1]));
        assert_eq!(hash(&preview), hash(&new_preview));
    }

    #[test]
    fn tiff_identity_includes_byte_order_and_palette_but_not_descriptions() {
        let dir = tempfile::tempdir().unwrap();
        let data = [1u8, 2, 2, 1];
        let little = write(dir.path(), "le.tif", &plain_tiff(false, &[], &data));
        let big = write(dir.path(), "be.tif", &plain_tiff(true, &[], &data));
        // Same bytes, 16-bit samples read the other way round.
        assert_ne!(hash(&little), hash(&big));
        let palette = |colour: u8| (0x140u16, 3u16, [colour, 0].repeat(6));
        let red = write(
            dir.path(),
            "red.tif",
            &plain_tiff(false, &[palette(9)], &data),
        );
        let blue = write(
            dir.path(),
            "blue.tif",
            &plain_tiff(false, &[palette(7)], &data),
        );
        assert_ne!(hash(&red), hash(&blue));
        let artist = |name: &str| (0x13Bu16, 2u16, format!("{name}\0").into_bytes());
        let one = write(
            dir.path(),
            "a.tif",
            &plain_tiff(false, &[artist("Ann")], &data),
        );
        let other = write(
            dir.path(),
            "b.tif",
            &plain_tiff(false, &[artist("Someone else")], &data),
        );
        assert!(hash(&one).starts_with("1:tiff:"));
        assert_eq!(hash(&one), hash(&other));
        assert_eq!(hash(&one), hash(&little));
    }

    fn jpeg_file(app: &[u8], scan: &[u8], trailer: &[u8]) -> Vec<u8> {
        let mut out = vec![0xFF, 0xD8];
        out.extend([0xFF, 0xE1]);
        out.extend(((app.len() + 2) as u16).to_be_bytes());
        out.extend(app);
        out.extend([0xFF, 0xDB, 0, 4, 1, 2]);
        out.extend([0xFF, 0xFE, 0, 5, b'h', b'i', b'!']);
        out.extend([0xFF, 0xDA, 0, 3, 9]);
        out.extend(scan);
        out.extend([0xFF, 0xD9]);
        out.extend(trailer);
        out
    }

    #[test]
    fn jpeg_identity_is_its_tables_and_scans() {
        let dir = tempfile::tempdir().unwrap();
        let scan = [1, 2, 0xFF, 0x00, 3, 0xFF, 0xD3, 4, 5];
        let a = write(dir.path(), "a.jpg", &jpeg_file(b"Exif one", &scan, b""));
        let b = write(
            dir.path(),
            "b.jpg",
            &jpeg_file(b"XMP rating 4, keywords", &scan, b"MPF image"),
        );
        let c = write(
            dir.path(),
            "c.jpg",
            &jpeg_file(b"Exif one", &[1, 2, 0xFF, 0x00, 3, 0xFF, 0xD3, 4, 6], b""),
        );
        assert!(hash(&a).starts_with("1:jpeg:"));
        assert_eq!(hash(&a), hash(&b));
        assert_ne!(hash(&a), hash(&c));
    }

    fn raf(cfa: &[u8], jpeg: &[u8]) -> Vec<u8> {
        raf_with(cfa, jpeg, b"6000x4000")
    }

    /// `raf` with `header` as its CFA header (dimensions and layout).
    fn raf_with(cfa: &[u8], jpeg: &[u8], header: &[u8]) -> Vec<u8> {
        let mut out = b"FUJIFILMCCD-RAW 0201FF383501".to_vec();
        out.resize(108, 0);
        let jpeg_at = 108;
        let header_at = jpeg_at + jpeg.len();
        let cfa_at = header_at + header.len();
        let mut put = |at: usize, value: usize| {
            out[at..at + 4].copy_from_slice(&(value as u32).to_be_bytes())
        };
        put(84, jpeg_at);
        put(88, jpeg.len());
        put(92, header_at);
        put(96, header.len());
        put(100, cfa_at);
        put(104, cfa.len());
        out.extend(jpeg);
        out.extend(header);
        out.extend(cfa);
        out
    }

    #[test]
    fn raf_identity_is_its_cfa_section() {
        let dir = tempfile::tempdir().unwrap();
        let a = write(
            dir.path(),
            "a.raf",
            &raf(b"sensor data", b"preview with exif"),
        );
        let b = write(
            dir.path(),
            "b.raf",
            &raf(b"sensor data", b"preview, exif edited"),
        );
        let c = write(
            dir.path(),
            "c.raf",
            &raf(b"sensor date", b"preview with exif"),
        );
        assert!(hash(&a).starts_with("1:raf:"));
        assert_eq!(hash(&a), hash(&b));
        assert_ne!(hash(&a), hash(&c));
        let d = write(
            dir.path(),
            "d.raf",
            &raf_with(b"sensor data", b"preview with exif", b"4000x6000"),
        );
        assert_ne!(hash(&a), hash(&d));
    }

    fn bx(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut out = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        out.extend(kind);
        out.extend(body);
        out
    }

    /// An ISO media file: ftyp, a metadata box of `meta` bytes, moov with
    /// two one-sample tracks (a preview and the raw), then mdat.
    fn cr3(raw: &[u8], meta: &[u8]) -> Vec<u8> {
        cr3_with(raw, meta, b"CRAW 6000x4000")
    }

    /// `cr3` with `description` as each track's sample description.
    fn cr3_with(raw: &[u8], meta: &[u8], description: &[u8]) -> Vec<u8> {
        let preview = b"small preview";
        let ftyp = bx(b"ftyp", b"crx \0\0\0\x01");
        let uuid = bx(b"uuid", meta);
        let track = |offset: u32, size: u32| {
            let mut stsz = vec![0; 4];
            stsz.extend(0u32.to_be_bytes());
            stsz.extend(1u32.to_be_bytes());
            stsz.extend(size.to_be_bytes());
            let mut stco = vec![0; 4];
            stco.extend(1u32.to_be_bytes());
            stco.extend(offset.to_be_bytes());
            let stbl = bx(
                b"stbl",
                &[
                    bx(b"stsd", description),
                    bx(b"stsz", &stsz),
                    bx(b"stco", &stco),
                ]
                .concat(),
            );
            bx(b"trak", &bx(b"mdia", &bx(b"minf", &stbl)))
        };
        // Lengths do not depend on the offsets: lay out once to measure.
        let moov_len = bx(b"moov", &[track(0, 0), track(0, 0)].concat()).len();
        let mdat_at = (ftyp.len() + uuid.len() + moov_len + 8) as u32;
        let moov = bx(
            b"moov",
            &[
                track(mdat_at, preview.len() as u32),
                track(mdat_at + preview.len() as u32, raw.len() as u32),
            ]
            .concat(),
        );
        [ftyp, uuid, moov, bx(b"mdat", &[&preview[..], raw].concat())].concat()
    }

    #[test]
    fn iso_media_identity_is_its_largest_track() {
        let dir = tempfile::tempdir().unwrap();
        let raw: Vec<u8> = (0..300u32).map(|i| i as u8).collect();
        let a = write(dir.path(), "a.cr3", &cr3(&raw, b"CMT1 rating 0"));
        let b = write(
            dir.path(),
            "b.cr3",
            &cr3(&raw, b"CMT1 rating 3 with a longer block"),
        );
        let mut other = raw.clone();
        other[0] = 9;
        let c = write(dir.path(), "c.cr3", &cr3(&other, b"CMT1 rating 0"));
        assert!(hash(&a).starts_with("1:bmff:"));
        assert_eq!(hash(&a), hash(&b));
        assert_ne!(hash(&a), hash(&c));
        let other_size = write(
            dir.path(),
            "d.cr3",
            &cr3_with(&raw, b"CMT1 rating 0", b"CRAW 4000x6000"),
        );
        assert_ne!(hash(&a), hash(&other_size));
    }

    fn png_file(text: &[u8], data: &[u8]) -> Vec<u8> {
        let chunk = |kind: &[u8; 4], body: &[u8]| {
            let mut out = (body.len() as u32).to_be_bytes().to_vec();
            out.extend(kind);
            out.extend(body);
            out.extend([0, 0, 0, 0]);
            out
        };
        [
            b"\x89PNG\r\n\x1a\n".to_vec(),
            chunk(b"IHDR", &[0; 13]),
            chunk(b"tEXt", text),
            chunk(b"IDAT", data),
            chunk(b"IEND", b""),
        ]
        .concat()
    }

    #[test]
    fn png_identity_is_its_image_chunks() {
        let dir = tempfile::tempdir().unwrap();
        let a = write(dir.path(), "a.png", &png_file(b"Title\0one", b"pixels"));
        let b = write(
            dir.path(),
            "b.png",
            &png_file(b"Title\0two words", b"pixels"),
        );
        let c = write(dir.path(), "c.png", &png_file(b"Title\0one", b"pixelz"));
        assert!(hash(&a).starts_with("1:png:"));
        assert_eq!(hash(&a), hash(&b));
        assert_ne!(hash(&a), hash(&c));
    }

    #[test]
    fn unknown_or_broken_layouts_fall_back_to_the_whole_file() {
        let dir = tempfile::tempdir().unwrap();
        let a = write(dir.path(), "a.heic", b"\0\0\0\x18ftypheic but no moov");
        let b = write(dir.path(), "b.heic", b"\0\0\0\x18ftypheic but no moov");
        let c = write(dir.path(), "c.xyz", b"something else");
        let mut broken = tiff(&[&[5u8; 64]], b"x", 0);
        broken.truncate(broken.len() - 2);
        let d = write(dir.path(), "d.tif", &broken);
        assert!(hash(&a).starts_with("1:file:"));
        assert_eq!(hash(&a), hash(&b));
        assert!(hash(&c).starts_with("1:file:"));
        assert!(hash(&d).starts_with("1:file:"));
        assert!(current(&hash(&a)));
        assert!(!current("0:tiff:00"));
        assert_eq!(image_hash(&a, &AtomicBool::new(true)).unwrap(), None);
    }
}
