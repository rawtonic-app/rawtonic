//! Reading a RAW's embedded preview ahead of its mapping, in a few large
//! reads.
//!
//! A grid tile of a never-edited RAW is its embedded camera JPEG, read
//! through a mapping of the file (`file_io::read_file_mapped`). On a
//! network share the mapping's page faults wait for the network one
//! read-around window at a time. Reads through `pread` fill the same page
//! cache the mapping then reads from, so [`read_ahead`] finds where the
//! preview lies with the same container parsers the preview is then read
//! with (`image_loader::FileBytes`, over a probe that reads only the blocks
//! they ask for), reads those ranges in a few large reads, lets rawler's own structure
//! parsers read what its decoder reads for the camera crop through the
//! probe too, and leaves every decode to the mapping: the preview and its
//! pixels are the same as without it. On macOS smbfs over Wi-Fi the reads
//! of 500 never-edited RAWs' tiles went from 26.5–27.6 s to 19.0–19.4 s
//! (the link's throughput) and 1.19 to 1.02 GB (notes:
//! benchmark-results/grd3-2026-11-24). A file whose structure it cannot
//! read within a few reads, or a container it does not know, is read
//! through the mapping alone, as before.
use crate::image_loader::{self, FileBytes};
use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::fs::File;
use std::io;

/// The probe reads the file in blocks of this size …
const BLOCK: usize = 64 << 10;
/// … the first read being the head of the file, where the structure of
/// TIFF-based RAWs, RAF and the CR3 `moov` box lie (0–96 KB of a CR3,
/// 0–240 KB of a NEF: grd2-2026-11-24) …
const HEAD_BLOCKS: usize = 4;
/// … and gives up locating the preview after this many further reads: a
/// structure spread over more of the file is read through the mapping as
/// before …
const MAX_PROBE_READS: usize = 12;
/// … and reading the rest of the structure rawler's decoder reads after
/// this many more.
const MAX_STRUCTURE_READS: usize = 16;
/// Larger reads are made in parts of this size.
const READ_PART: usize = 4 << 20;
/// The probe keeps at most this many bytes of structure (an ORF's maker
/// note is 1.5 MB); a structure claiming more is left to the mapping.
const MAX_STRUCTURE_BYTES: u64 = 16 << 20;
/// A located range longer than this is no camera preview (a damaged
/// length) and is left to the mapping.
const MAX_RANGE_BYTES: u64 = 64 << 20;

/// What [`read_ahead`] read: the located preview ranges (offset, length;
/// all within the file), and the reads and bytes it took in all.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ReadAhead {
    pub ranges: Vec<(u64, u64)>,
    pub reads: usize,
    pub bytes: u64,
}

/// Reads the byte ranges `file`'s embedded preview and its camera crop are
/// read from (the JPEG, Canon's timed metadata, the structure rawler's
/// decoder parses) into the system's cache, in a few large reads, ahead of
/// the file's mapping reading them. Never fails the caller: an unknown or
/// unreadable structure reads only what the probe read. `len` is the
/// file's length.
pub fn read_ahead(file: &File, len: u64) -> ReadAhead {
    let _guard = SequentialReadAheadOff::new(file);
    let probe = Probe::new(file, len);
    // rawler's parsers panic on some damaged files (`camera_crop`).
    let located = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| located(&probe)))
        .ok()
        .flatten()
        .filter(|_| !probe.failed.get());
    let mut ranges = Vec::new();
    let mut part = Vec::new();
    for (offset, length) in located.unwrap_or_default() {
        // A range past the end of the file is one the parsers do not read
        // either (a truncated or damaged file): left to the mapping.
        let end = offset.saturating_add(length);
        if offset < end && end <= len && length <= MAX_RANGE_BYTES {
            ranges.push((offset, length));
            probe.read_through(offset, end, &mut part);
        }
    }
    if !probe.failed.get() {
        probe.limit.set(probe.reads.get() + MAX_STRUCTURE_READS);
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| read_structure(&probe)));
    }
    ReadAhead {
        ranges,
        reads: probe.reads.get(),
        bytes: probe.bytes.get(),
    }
}

/// Reads through the probe what rawler's decoder parses of the file's
/// structure, for the camera crop (`camera_crop::crops_in`) and the
/// preview of formats it gives: the whole TIFF tree with its values (an
/// ORF's 1.5 MB maker note, a Panasonic RW2's JPEG) or a RAF's tag block
/// and raw-data IFD (`decoders::raf::parse_raf`; its first TIFF lies in the
/// JPEG). A CR3's boxes were read in locating its preview.
fn read_structure(probe: &Probe) {
    let mut reader = ProbeReader { probe, at: 0 };
    if image_loader::raf_jpeg_range(probe).is_some() {
        let word = |at| bytes_at::<4>(probe, at).map(u32::from_be_bytes);
        if let (Some(tags), Some(length)) = (word(92), word(96))
            && u64::from(length) <= MAX_RANGE_BYTES
            && let Some(end) = u64::from(tags)
                .checked_add(u64::from(length))
                .filter(|&end| end <= probe.len)
        {
            probe.read_through(u64::from(tags), end, &mut Vec::new());
        }
        if let Some(raw) = word(100) {
            let _ = rawler::formats::tiff::IFD::new_root_with_correction(
                &mut reader,
                0,
                raw,
                0,
                10,
                &[rawler::decoders::raf::FujiIFD::FujiIFD as u16],
            );
        }
    } else if matches!(probe.at(0, 2).as_deref(), Some(b"II" | b"MM")) {
        let _ = rawler::formats::tiff::GenericTiffReader::new(&mut reader, 0, 0, None, &[]);
    }
}

/// `N` bytes at `at`.
fn bytes_at<const N: usize>(probe: &Probe, at: usize) -> Option<[u8; N]> {
    probe.at(at, N)?.as_ref().try_into().ok()
}

/// The ranges of the file the embedded preview is read from, found by the
/// parsers `image_loader::embedded_preview_with` reads it with, in its
/// order: a RAF's JPEG, the largest JPEG of a TIFF-based RAW that is not
/// its lossless raw data, an ORF's maker-note JPEG, a CR3's preview track
/// and timed metadata. `None` for other containers (read as before).
fn located(probe: &Probe) -> Option<Vec<(u64, u64)>> {
    let range = |(offset, length): (usize, usize)| (offset as u64, length as u64);
    if let Some(jpeg) = image_loader::raf_jpeg_range(probe) {
        return Some(vec![range(jpeg)]);
    }
    if let Some(candidates) = image_loader::tiff_jpeg_candidate_ranges(probe) {
        // `tiff_jpeg_previews`: candidates within the file, without lossless
        // raw data; the largest one that decodes is the preview.
        let within = |&(offset, length): &(u64, u64)| {
            offset
                .checked_add(length)
                .is_some_and(|end| end <= probe.len)
        };
        for (offset, length) in candidates.into_iter().filter(within) {
            let lossless = image_loader::lossless_jpeg_at(probe, offset as usize, length as usize);
            if probe.failed.get() {
                return None;
            }
            if !lossless {
                return Some(vec![(offset, length)]);
            }
        }
    }
    if let Some(parts) = image_loader::orf_preview_parts(probe) {
        return Some(vec![range(parts.jpeg)]);
    }
    cr3_ranges(probe)
}

/// A CR3's preview JPEG (its first track's first sample: rawler's
/// `Cr3Decoder::full_image`, which its preview falls back to) and the
/// timed metadata its orientation and camera crop are read from (the CTMD
/// track's first sample: `Cr3Decoder::read_ctmd`), found by rawler's own
/// box parser reading through the probe.
fn cr3_ranges(probe: &Probe) -> Option<Vec<(u64, u64)>> {
    if probe.at(4, 8)?.as_ref() != b"ftypcrx " {
        return None;
    }
    let bmff = rawler::formats::bmff::Bmff::new(ProbeReader { probe, at: 0 }).ok()?;
    let traks = &bmff.filebox.moov.traks;
    let stbl = &traks.first()?.mdia.minf.stbl;
    let offset = *stbl.co64.as_ref()?.entries.first()?;
    let length = *stbl.stsz.sample_sizes.first()? as u64;
    let mut ranges = vec![(offset, length)];
    if let Some((offset, length)) = traks
        .iter()
        .find(|trak| trak.mdia.minf.stbl.stsd.ctmd.is_some())
        .and_then(|trak| trak.mdia.minf.stbl.get_sample_offset(1))
    {
        ranges.push((offset as u64, length as u64));
    }
    Some(ranges)
}

/// A file read in `BLOCK`s as the parsers ask for its bytes: the head at
/// once, any other block with one read for each run of missing blocks, up
/// to `limit` reads in all (`MAX_PROBE_READS` after the head at first) and
/// `MAX_STRUCTURE_BYTES` kept. A read it may not or cannot make marks it
/// `failed` (its answers are then incomplete, unlike a `None` for bytes
/// past the end of the file).
struct Probe<'f> {
    file: &'f File,
    len: u64,
    blocks: RefCell<BTreeMap<u64, Vec<u8>>>,
    reads: Cell<usize>,
    bytes: Cell<u64>,
    kept: Cell<u64>,
    limit: Cell<usize>,
    failed: Cell<bool>,
}

impl<'f> Probe<'f> {
    fn new(file: &'f File, len: u64) -> Self {
        let probe = Probe {
            file,
            len,
            blocks: RefCell::default(),
            reads: Cell::new(0),
            bytes: Cell::new(0),
            kept: Cell::new(0),
            limit: Cell::new(1 + MAX_PROBE_READS),
            failed: Cell::new(false),
        };
        probe.fetch(0, HEAD_BLOCKS as u64);
        probe
    }

    /// Reads blocks `first..first + count` (clipped to the file) with one
    /// read per `READ_PART`.
    fn fetch(&self, first: u64, count: u64) {
        let part = (READ_PART / BLOCK) as u64;
        if count > part {
            self.fetch(first, part);
            return self.fetch(first + part, count - part);
        }
        let start = first * BLOCK as u64;
        let end = ((first + count) * BLOCK as u64).min(self.len);
        if start >= end {
            return;
        }
        if self.kept.get() + (end - start) > MAX_STRUCTURE_BYTES {
            self.failed.set(true);
            return;
        }
        let mut data = vec![0; (end - start) as usize];
        if read_exact_at(self.file, &mut data, start).is_err() {
            self.failed.set(true);
            return;
        }
        self.reads.set(self.reads.get() + 1);
        self.bytes.set(self.bytes.get() + data.len() as u64);
        self.kept.set(self.kept.get() + data.len() as u64);
        let mut blocks = self.blocks.borrow_mut();
        for (index, block) in data.chunks(BLOCK).enumerate() {
            blocks.insert(first + index as u64, block.to_vec());
        }
    }

    /// Whether the blocks of `start..end` are read, reading the missing
    /// ones while reads are left.
    fn ensure(&self, start: u64, end: u64) -> bool {
        let blocks = start / BLOCK as u64..end.div_ceil(BLOCK as u64);
        let mut first_missing = None;
        let mut runs = Vec::new();
        {
            let read = self.blocks.borrow();
            for block in blocks.clone() {
                match (read.contains_key(&block), first_missing) {
                    (false, None) => first_missing = Some(block),
                    (true, Some(first)) => {
                        runs.push((first, block - first));
                        first_missing = None;
                    }
                    _ => {}
                }
            }
        }
        if let Some(first) = first_missing {
            runs.push((first, blocks.end - first));
        }
        for (first, count) in runs {
            if self.failed.get() || self.reads.get() >= self.limit.get() {
                self.failed.set(true);
                return false;
            }
            self.fetch(first, count);
        }
        !self.failed.get()
    }

    /// Reads the parts of `start..end` outside its blocks into the system's
    /// cache through `part` (in `READ_PART`s, whatever its limit, keeping
    /// nothing): a located range, which the mapping reads next.
    fn read_through(&self, start: u64, end: u64, part: &mut Vec<u8>) {
        let kept = |at: u64| self.blocks.borrow().contains_key(&(at / BLOCK as u64));
        let block_end = |at: u64| ((at / BLOCK as u64 + 1) * BLOCK as u64).min(end);
        let mut at = start;
        while at < end {
            if kept(at) {
                at = block_end(at);
                continue;
            }
            // The run of blocks not kept from `at`, up to `READ_PART`.
            let mut stop = block_end(at);
            while stop < end && !kept(stop) && stop - at < READ_PART as u64 {
                stop = block_end(stop);
            }
            let stop = stop.min(at + READ_PART as u64);
            part.resize((stop - at) as usize, 0);
            if read_exact_at(self.file, part, at).is_err() {
                return;
            }
            self.reads.set(self.reads.get() + 1);
            self.bytes.set(self.bytes.get() + (stop - at));
            at = stop;
        }
    }
}

impl FileBytes for Probe<'_> {
    fn at(&self, offset: usize, len: usize) -> Option<Cow<'_, [u8]>> {
        let start = offset as u64;
        let end = start.checked_add(len as u64)?;
        if end > self.len || !self.ensure(start, end) {
            return None;
        }
        let read = self.blocks.borrow();
        let mut bytes = Vec::with_capacity(len);
        let mut at = start;
        while at < end {
            let block = at / BLOCK as u64;
            let data = read.get(&block)?;
            let from = (at - block * BLOCK as u64) as usize;
            let to = ((end - block * BLOCK as u64) as usize).min(data.len());
            bytes.extend_from_slice(data.get(from..to)?);
            at = block * BLOCK as u64 + to as u64;
        }
        Some(Cow::Owned(bytes))
    }
}

/// The probe as a reader, for rawler's box and TIFF parsers.
struct ProbeReader<'p, 'f> {
    probe: &'p Probe<'f>,
    at: u64,
}

impl io::Read for ProbeReader<'_, '_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        // All of a large value at once: one read of its missing blocks.
        let left = self.probe.len.saturating_sub(self.at);
        let size = (buf.len() as u64).min(left).min(READ_PART as u64) as usize;
        if size == 0 {
            return Ok(0);
        }
        let bytes = self
            .probe
            .at(self.at as usize, size)
            .ok_or_else(|| io::Error::other("not read ahead"))?;
        buf[..size].copy_from_slice(&bytes);
        self.at += size as u64;
        Ok(size)
    }
}

impl io::Seek for ProbeReader<'_, '_> {
    fn seek(&mut self, to: io::SeekFrom) -> io::Result<u64> {
        let at = match to {
            io::SeekFrom::Start(at) => Some(at),
            io::SeekFrom::End(by) => self.probe.len.checked_add_signed(by),
            io::SeekFrom::Current(by) => self.at.checked_add_signed(by),
        };
        self.at = at.ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
        Ok(self.at)
    }
}

/// The system's own read-ahead of `file` off while the ranges are read,
/// back on when dropped (before the mapping reads). macOS's SMB client
/// takes a read right after the previous one (the head, then a preview
/// starting where it ends) as a sequential read and reads up to as much
/// again beyond it: 3.4 instead of 2.2 MB per file of a share's folder
/// (notes: benchmark-results/grd3-2026-11-24).
struct SequentialReadAheadOff<'f> {
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    file: &'f File,
}

impl<'f> SequentialReadAheadOff<'f> {
    fn new(file: &'f File) -> Self {
        #[cfg(target_os = "macos")]
        set_read_ahead(file, false);
        SequentialReadAheadOff { file }
    }
}

impl Drop for SequentialReadAheadOff<'_> {
    fn drop(&mut self) {
        #[cfg(target_os = "macos")]
        set_read_ahead(self.file, true);
    }
}

/// `F_RDAHEAD` of `file`'s descriptor (on by default for a file opened).
#[cfg(target_os = "macos")]
fn set_read_ahead(file: &File, on: bool) {
    use std::os::fd::AsRawFd;
    // SAFETY: an fcntl of an open descriptor with an int argument; a
    // failure leaves the read-ahead as it was.
    unsafe {
        libc::fcntl(file.as_raw_fd(), libc::F_RDAHEAD, libc::c_int::from(on));
    }
}

/// Fills `buf` from `file` at `offset` without moving a cursor the mapping
/// would use (it uses none).
fn read_exact_at(file: &File, buf: &mut [u8], offset: u64) -> io::Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::FileExt::read_exact_at(file, buf, offset)
    }
    #[cfg(windows)]
    {
        let (mut done, mut at) = (0, offset);
        while done < buf.len() {
            match std::os::windows::fs::FileExt::seek_read(file, &mut buf[done..], at)? {
                0 => return Err(io::ErrorKind::UnexpectedEof.into()),
                n => {
                    done += n;
                    at += n as u64;
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image_loader::private_preview_tests::{orf_with_jpeg, raf_with_jpeg};
    use std::io::Cursor;

    fn jpeg(width: u32, height: u32) -> Vec<u8> {
        let mut out = Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(width, height)
            .write_to(&mut out, image::ImageFormat::Jpeg)
            .unwrap();
        out.into_inner()
    }

    /// `bytes` written to a file, and what `read_ahead` read of it.
    fn read_ahead_of(bytes: &[u8]) -> ReadAhead {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("photo.raw");
        std::fs::write(&path, bytes).unwrap();
        let file = File::open(&path).unwrap();
        read_ahead(&file, bytes.len() as u64)
    }

    /// Where `part` (a slice of `whole`) lies in it.
    fn range_of(whole: &[u8], part: &[u8]) -> (u64, u64) {
        let offset = part.as_ptr() as usize - whole.as_ptr() as usize;
        (offset as u64, part.len() as u64)
    }

    /// The bytes read for a file of `len` bytes: the head's and the
    /// `structure`'s blocks (kept by the probe), and the parts of `ranges`
    /// outside them (read through).
    fn read_for(structure: &[(u64, u64)], ranges: &[(u64, u64)], len: u64) -> u64 {
        let block = BLOCK as u64;
        let mut blocks: std::collections::BTreeSet<u64> = (0..HEAD_BLOCKS as u64).collect();
        for &(offset, length) in structure {
            blocks.extend(offset / block..(offset + length).div_ceil(block));
        }
        let kept: u64 = blocks
            .iter()
            .map(|b| ((b + 1) * block).min(len).saturating_sub(b * block))
            .sum();
        let outside: u64 = ranges
            .iter()
            .flat_map(|&(offset, length)| offset..offset + length)
            .filter(|at| !blocks.contains(&(at / block)))
            .count() as u64;
        kept + outside
    }

    /// A file of `len` bytes (sparse beyond `bytes`).
    fn read_ahead_of_sparse(bytes: &[u8], len: u64) -> ReadAhead {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("photo.raw");
        std::fs::write(&path, bytes).unwrap();
        let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.set_len(len).unwrap();
        let file = File::open(&path).unwrap();
        read_ahead(&file, len)
    }

    #[test]
    fn a_raf_preview_is_read_with_one_read_beyond_the_head() {
        let preview = jpeg(64, 48);
        let mut raf = raf_with_jpeg(&preview);
        // The JPEG 300 KB into the file, then 3 MB of raw data.
        let at = 300 << 10;
        let header_end = raf.len() - preview.len();
        raf.truncate(header_end);
        raf[84..88].copy_from_slice(&(at as u32).to_be_bytes());
        raf.resize(at, 0);
        raf.extend_from_slice(&preview);
        raf.resize(raf.len() + (3 << 20), 7);
        let expected = range_of(&raf, image_loader::raf_embedded_jpeg(&raf).unwrap());

        let done = read_ahead_of(&raf);
        assert_eq!(done.ranges, vec![expected]);
        // The head, then the JPEG past it: two reads, not the raw data.
        assert_eq!(done.reads, 2);
        assert_eq!(done.bytes, read_for(&[], &done.ranges, raf.len() as u64));
    }

    /// A little-endian TIFF RAW: IFD0 (orientation) with two SubIFDs, a
    /// lossless JPEG raw strip (larger) and a baseline JPEG preview, at
    /// `raw_at` and `preview_at`, padded to `len`.
    fn tiff_with(preview: &[u8], raw_at: usize, preview_at: usize, len: usize) -> (Vec<u8>, usize) {
        let entry = |tag: u16, kind: u16, count: u32, value: u32| {
            let mut out = tag.to_le_bytes().to_vec();
            out.extend(kind.to_le_bytes());
            out.extend(count.to_le_bytes());
            out.extend(value.to_le_bytes());
            out
        };
        let ifd = |entries: Vec<Vec<u8>>| {
            let mut out = (entries.len() as u16).to_le_bytes().to_vec();
            out.extend(entries.concat());
            out.extend(0u32.to_le_bytes());
            out
        };
        // A lossless JPEG (SOF3) of 2 MB.
        let mut raw = vec![
            0xFF, 0xD8, 0xFF, 0xC4, 0x00, 0x04, 0, 0, 0xFF, 0xC3, 0x00, 0x0B,
        ];
        raw.resize(2 << 20, 1);
        let (ifd0, sub_raw, sub_preview, pointers) = (8u32, 200u32, 300u32, 400u32);
        let mut tiff = b"II\x2a\x00".to_vec();
        tiff.extend(ifd0.to_le_bytes());
        tiff.extend(ifd(vec![
            entry(0x0112, 3, 1, 1),
            entry(330, 4, 2, pointers),
        ]));
        tiff.resize(sub_raw as usize, 0);
        tiff.extend(ifd(vec![
            entry(259, 3, 1, 7),
            entry(273, 4, 1, raw_at as u32),
            entry(279, 4, 1, raw.len() as u32),
        ]));
        tiff.resize(sub_preview as usize, 0);
        tiff.extend(ifd(vec![
            entry(259, 3, 1, 6),
            entry(513, 4, 1, preview_at as u32),
            entry(514, 4, 1, preview.len() as u32),
        ]));
        tiff.resize(pointers as usize, 0);
        tiff.extend(sub_raw.to_le_bytes());
        tiff.extend(sub_preview.to_le_bytes());
        tiff.resize(raw_at, 0);
        tiff.extend(&raw);
        tiff.resize(preview_at, 0);
        tiff.extend(preview);
        tiff.resize(len, 3);
        (tiff, raw.len())
    }

    #[test]
    fn a_tiff_preview_is_read_without_the_lossless_raw_data() {
        let preview = jpeg(64, 48);
        let (raw_at, preview_at) = (1 << 20, 4 << 20);
        let (tiff, raw_len) = tiff_with(&preview, raw_at, preview_at, 6 << 20);
        // The parsers take the preview, not the larger raw strip.
        let previews: Vec<_> = image_loader::tiff_jpeg_previews(&tiff).unwrap().collect();
        let expected = range_of(&tiff, previews[0]);
        assert_eq!(expected, (preview_at as u64, preview.len() as u64));
        assert!(image_loader::largest_tiff_jpeg_preview(&tiff).is_some());

        let done = read_ahead_of(&tiff);
        assert_eq!(done.ranges, vec![expected]);
        // The head, one block where the raw strip starts (its frame header
        // says lossless), one where the preview starts (holding all of this
        // small one): not the raw strip's 2 MB.
        assert_eq!(done.reads, 3);
        assert_eq!(done.bytes, ((HEAD_BLOCKS + 2) * BLOCK) as u64);
        assert!(done.bytes < raw_len as u64);
    }

    #[test]
    fn an_orf_preview_is_located_by_its_maker_note() {
        let preview = jpeg(64, 48);
        let mut orf = orf_with_jpeg(&preview, b"OLYMPUS\0", 13, 1, None);
        orf.resize(orf.len() + (2 << 20), 5);
        let (jpeg, _) = image_loader::orf_embedded_jpeg(&orf).unwrap();
        let expected = range_of(&orf, jpeg);

        let done = read_ahead_of(&orf);
        assert_eq!(done.ranges, vec![expected]);
        // All within the head: one read, not the 2 MB after it.
        assert_eq!((done.reads, done.bytes), (1, (HEAD_BLOCKS * BLOCK) as u64));
    }

    #[test]
    fn an_orf_maker_note_rawler_reads_is_read_with_one_read() {
        // rawler reads the maker note's whole value for the camera crop: an
        // E-M10's 1.45 MB, up to the raw data. Here 1 MB past the head.
        let preview = jpeg(64, 48);
        let mut orf = orf_with_jpeg(&preview, b"OLYMPUS\0", 13, 1, None);
        let entry = orf
            .windows(4)
            .position(|w| w == [0x7C, 0x92, 7, 0])
            .unwrap();
        let note = u32::from_le_bytes(orf[entry + 8..entry + 12].try_into().unwrap()) as u64;
        let note_len = (1u64 << 20) + 1000;
        orf[entry + 4..entry + 8].copy_from_slice(&(note_len as u32).to_le_bytes());
        orf.resize(4 << 20, 5);
        let done = read_ahead_of(&orf);
        assert_eq!(done.ranges.len(), 1);
        // The head, then the rest of the note: not the raw data after it.
        assert_eq!(done.reads, 2);
        assert_eq!(
            done.bytes,
            read_for(&[(note, note_len)], &[], orf.len() as u64)
        );
    }

    #[test]
    fn a_raf_tag_block_and_raw_ifd_are_read_with_one_read_each() {
        let preview = jpeg(64, 48);
        let mut raf = raf_with_jpeg(&preview);
        let (tags, tags_len, raw) = (400u32 << 10, 20u32 << 10, 1u32 << 20);
        raf[92..96].copy_from_slice(&tags.to_be_bytes());
        raf[96..100].copy_from_slice(&tags_len.to_be_bytes());
        raf[100..104].copy_from_slice(&raw.to_be_bytes());
        raf.resize(tags as usize, 0);
        raf.extend(1u32.to_be_bytes());
        raf.resize(raw as usize, 0);
        // The raw data's TIFF: one IFD with the raw image's width.
        raf.extend(b"MM\0\x2a\0\0\0\x08\0\x01\xf0\x01\0\x04\0\0\0\x01\0\0\x10\0\0\0\0\0");
        raf.resize(8 << 20, 6);
        let done = read_ahead_of(&raf);
        assert_eq!(done.ranges, vec![(148, preview.len() as u64)]);
        // The head (with the JPEG), the tag block, the raw data's IFD.
        assert_eq!(done.reads, 3);
        // The tag block read through, the IFD's block kept.
        let read = read_for(
            &[(raw as u64, 26)],
            &[(tags as u64, tags_len as u64)],
            raf.len() as u64,
        );
        assert_eq!(done.bytes, read);
    }

    #[test]
    fn a_jpeg_in_a_tiff_tag_value_is_read_with_the_structure() {
        // A Panasonic RW2's preview is its IFD0's JpgFromRaw (0x2E) value,
        // which rawler reads with the TIFF tree.
        let preview = jpeg(64, 48);
        let at = 1u32 << 20;
        let mut rw2 = b"IIU\0\x08\0\0\0\x01\0\x2e\0\x07\0".to_vec();
        rw2.extend((preview.len() as u32).to_le_bytes());
        rw2.extend(at.to_le_bytes());
        rw2.extend(0u32.to_le_bytes());
        rw2.resize(at as usize, 0);
        rw2.extend(&preview);
        rw2.resize(3 << 20, 4);
        let done = read_ahead_of(&rw2);
        assert_eq!(done.ranges, Vec::new());
        assert_eq!(done.reads, 2);
        assert_eq!(
            done.bytes,
            read_for(&[(at as u64, preview.len() as u64)], &[], rw2.len() as u64)
        );
    }

    #[test]
    fn a_damaged_length_reads_no_more_than_the_caps() {
        // A RAF whose JPEG claims 100 MB: no camera preview, left to the
        // mapping.
        let mut raf = raf_with_jpeg(&jpeg(8, 8));
        raf[88..92].copy_from_slice(&(100u32 << 20).to_be_bytes());
        let done = read_ahead_of_sparse(&raf, 101 << 20);
        assert_eq!(done.ranges, Vec::new());
        assert_eq!((done.reads, done.bytes), (1, (HEAD_BLOCKS * BLOCK) as u64));
        // A TIFF tag value claiming 40 MB: the probe keeps at most
        // `MAX_STRUCTURE_BYTES` and then stops.
        let mut tiff = b"II\x2a\0\x08\0\0\0\x01\0\x2e\0\x07\0".to_vec();
        tiff.extend((40u32 << 20).to_le_bytes());
        tiff.extend((1u32 << 20).to_le_bytes());
        tiff.extend(0u32.to_le_bytes());
        let done = read_ahead_of_sparse(&tiff, 48 << 20);
        assert!(done.bytes <= (HEAD_BLOCKS * BLOCK) as u64 + MAX_STRUCTURE_BYTES);
    }

    #[test]
    fn a_range_past_the_end_of_the_file_is_not_read() {
        // A truncated RAF: its JPEG (3 MB from 148) and its tag block
        // (1 MB from 1.5 MB) end past the 2 MB file. The parsers read
        // neither; clipped, both would be under the caps.
        let mut raf = raf_with_jpeg(&jpeg(8, 8));
        raf[88..92].copy_from_slice(&(3u32 << 20).to_be_bytes());
        raf[92..96].copy_from_slice(&(3u32 << 19).to_be_bytes());
        raf[96..100].copy_from_slice(&(1u32 << 20).to_be_bytes());
        raf.resize(2 << 20, 3);
        assert!(image_loader::raf_embedded_jpeg(&raf).is_none());
        let done = read_ahead_of(&raf);
        assert_eq!(done.ranges, Vec::new());
        assert_eq!((done.reads, done.bytes), (1, (HEAD_BLOCKS * BLOCK) as u64));
    }

    /// An ISO BMFF box.
    fn boxed(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut out = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
        out.extend(kind);
        out.extend(payload);
        out
    }

    /// A full box (version 0, no flags).
    fn full(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        boxed(kind, &[&[0u8; 4][..], payload].concat())
    }

    /// A track whose one sample of `size` bytes lies at `offset`; `ctmd`
    /// marks it as Canon's timed metadata track.
    fn trak(offset: u64, size: u32, ctmd: bool) -> Vec<u8> {
        let mut stsd = 1u32.to_be_bytes().to_vec();
        if ctmd {
            // Reserved, data reference index, no records.
            stsd.extend(boxed(b"CTMD", &[0u8; 12]));
        }
        let stbl = [
            full(b"stsd", &stsd),
            full(b"stts", &0u32.to_be_bytes()),
            full(b"stsc", &[1u32, 1, 1, 1].map(u32::to_be_bytes).concat()),
            full(b"stsz", &[0u32, 1, size].map(u32::to_be_bytes).concat()),
            full(
                b"co64",
                &[&1u32.to_be_bytes()[..], &offset.to_be_bytes()].concat(),
            ),
        ]
        .concat();
        let minf = [boxed(b"dinf", &[]), boxed(b"stbl", &stbl)].concat();
        let mdia = [
            full(b"mdhd", &[0u8; 20]),
            full(b"hdlr", &[]),
            boxed(b"minf", &minf),
        ]
        .concat();
        boxed(
            b"trak",
            &[full(b"tkhd", &[]), boxed(b"mdia", &mdia)].concat(),
        )
    }

    /// A CR3: ftyp, moov with the preview track and the CTMD track, an mdat
    /// holding the preview JPEG at `preview_at` and 100 KB of metadata at
    /// its end, `len` bytes in all.
    fn cr3_with(preview: &[u8], preview_at: usize, len: usize) -> (Vec<u8>, (u64, u64)) {
        let ftyp = boxed(b"ftyp", b"crx \0\0\0\x01crx isom");
        let ctmd = ((len - (100 << 10)) as u64, 100 << 10);
        let moov = boxed(
            b"moov",
            &[
                full(b"mvhd", &[0u8; 16]),
                trak(preview_at as u64, preview.len() as u32, false),
                trak(ctmd.0, ctmd.1 as u32, true),
            ]
            .concat(),
        );
        let mut cr3 = [ftyp, moov].concat();
        let mdat_len = len - cr3.len();
        cr3.extend((mdat_len as u32).to_be_bytes());
        cr3.extend(b"mdat");
        cr3.resize(preview_at, 9);
        cr3.extend(preview);
        cr3.resize(len, 8);
        (cr3, ctmd)
    }

    #[test]
    fn a_cr3_preview_and_its_timed_metadata_are_located_by_its_boxes() {
        let preview = jpeg(64, 48);
        let (preview_at, len) = (300 << 10, 8 << 20);
        let (cr3, ctmd) = cr3_with(&preview, preview_at, len);
        // rawler reads the same boxes from the whole file.
        let bmff = rawler::formats::bmff::Bmff::new_buf(&cr3).unwrap();
        assert_eq!(bmff.filebox.moov.traks.len(), 2);

        let done = read_ahead_of(&cr3);
        assert_eq!(
            done.ranges,
            vec![(preview_at as u64, preview.len() as u64), ctmd]
        );
        // The head (ftyp, moov, mdat's header), the preview, the metadata.
        assert_eq!(done.reads, 3);
        assert_eq!(done.bytes, read_for(&[], &done.ranges, len as u64));
    }

    #[test]
    fn an_unknown_container_reads_only_the_head() {
        // A Panasonic RW2 starts "IIU\0": read through its mapping as before.
        let mut rw2 = b"IIU\0\x08\0\0\0".to_vec();
        rw2.resize(1 << 20, 2);
        let done = read_ahead_of(&rw2);
        assert_eq!(done.ranges, Vec::new());
        assert_eq!((done.reads, done.bytes), (1, (HEAD_BLOCKS * BLOCK) as u64));
    }

    #[test]
    fn a_structure_spread_over_the_file_is_left_to_the_mapping() {
        // An IFD chain hopping 1 MB further each IFD: the probe stops after
        // its reads and nothing more is read ahead.
        let hops = MAX_PROBE_READS + 4;
        let mut tiff = b"II\x2a\x00".to_vec();
        tiff.extend((1u32 << 20).to_le_bytes());
        for hop in 1..=hops {
            tiff.resize(hop << 20, 0);
            tiff.extend(0u16.to_le_bytes());
            let next = if hop == hops { 0 } else { (hop + 1) << 20 };
            tiff.extend((next as u32).to_le_bytes());
        }
        tiff.resize((hops + 1) << 20, 0);
        let done = read_ahead_of(&tiff);
        assert_eq!(done.ranges, Vec::new());
        assert_eq!(done.reads, 1 + MAX_PROBE_READS);
        assert_eq!(
            done.bytes,
            (HEAD_BLOCKS * BLOCK + MAX_PROBE_READS * BLOCK) as u64
        );
    }

    #[test]
    fn a_preview_read_ahead_leaves_the_mapping_and_its_preview_unchanged() {
        let preview = jpeg(64, 48);
        let (tiff, _) = tiff_with(&preview, 1 << 20, 4 << 20, 6 << 20);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("photo.nef");
        std::fs::write(&path, &tiff).unwrap();
        let (ahead, _) = crate::file_io::read_preview_mapped_with_metadata(&path).unwrap();
        let (plain, _) = crate::file_io::read_file_mapped_with_metadata(&path).unwrap();
        assert_eq!(&ahead[..], &plain[..]);
        let path = path.to_string_lossy();
        let a = image_loader::safe_embedded_preview_fallback(&ahead, &path).unwrap();
        let b = image_loader::safe_embedded_preview_fallback(&plain, &path).unwrap();
        assert_eq!(a.as_bytes(), b.as_bytes());
    }
}
