//! EXIF, GPS and XMP metadata for exported TIFFs.
//!
//! little_exif writes a TIFF by re-encoding the whole file from its own tag
//! model, which needs the image structure tags (strips, sample layout). Export
//! TIFFs come from the `image` encoder instead, so this module leaves the
//! encoded strips where they are and appends: the EXIF directory, the GPS
//! directory, and a copy of IFD0 with the extra entries, the XMP packet (tag
//! 700) and the two directory pointers. The header is then pointed at the new IFD0; the old IFD0 stays
//! behind as unreferenced bytes. Entries the encoder already wrote to IFD0 win
//! over metadata with the same tag, so the image structure cannot change.

use std::collections::BTreeMap;
use std::io::{self, Cursor, Read, Seek, SeekFrom, Write};

use little_exif::endian::Endian;
use little_exif::exif_tag::{ExifTag, TagType};
use little_exif::ifd::ExifTagGroup;
use little_exif::metadata::Metadata;

const EXIF_IFD_POINTER: u16 = 0x8769;
const GPS_IFD_POINTER: u16 = 0x8825;
const XMP_TAG: u16 = 700;
const TYPE_BYTE: u16 = 1;
const TYPE_LONG: u16 = 4;
const TYPE_UNDEFINED: u16 = 7;

struct Entry {
    field_type: u16,
    count: u32,
    value: EntryValue,
}

enum EntryValue {
    /// The value/offset field of an entry already in the file, kept as is:
    /// offsets stay valid because nothing before them moves.
    Existing([u8; 4]),
    Data(Vec<u8>),
}

#[derive(Clone, Copy)]
struct ByteOrder(bool);

impl ByteOrder {
    fn is_little(self) -> bool {
        self.0
    }

    fn read_u16(self, bytes: &[u8], at: usize) -> Option<u16> {
        let b: [u8; 2] = bytes.get(at..at + 2)?.try_into().ok()?;
        Some(if self.0 {
            u16::from_le_bytes(b)
        } else {
            u16::from_be_bytes(b)
        })
    }

    fn read_u32(self, bytes: &[u8], at: usize) -> Option<u32> {
        let b: [u8; 4] = bytes.get(at..at + 4)?.try_into().ok()?;
        Some(if self.0 {
            u32::from_le_bytes(b)
        } else {
            u32::from_be_bytes(b)
        })
    }

    fn u16_bytes(self, v: u16) -> [u8; 2] {
        if self.0 {
            v.to_le_bytes()
        } else {
            v.to_be_bytes()
        }
    }

    fn u32_bytes(self, v: u32) -> [u8; 4] {
        if self.0 {
            v.to_le_bytes()
        } else {
            v.to_be_bytes()
        }
    }
}

fn type_size(field_type: u16) -> Option<usize> {
    match field_type {
        1 | 2 | 6 | 7 => Some(1),
        3 | 8 => Some(2),
        4 | 9 | 11 => Some(4),
        5 | 10 | 12 => Some(8),
        _ => None,
    }
}

/// Adds the EXIF, GPS and IFD0 tags of `metadata`, and an XMP packet, to a
/// classic TIFF in place. On error the buffer is left unchanged.
pub fn append_exif(
    tiff: &mut Vec<u8>,
    metadata: &Metadata,
    xmp: Option<&[u8]>,
) -> Result<(), String> {
    append_exif_to(&mut Cursor::new(tiff), metadata, xmp).map_err(|e| e.to_string())
}

/// Why [`append_exif_to`] failed.
#[derive(Debug)]
pub enum AppendError {
    /// The file is not a TIFF this module can extend, or the metadata does
    /// not fit: nothing was written, the file is unchanged.
    Metadata(String),
    /// Reading or writing the file failed; it may be partly changed.
    Io(io::Error),
}

impl std::fmt::Display for AppendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AppendError::Metadata(e) => f.write_str(e),
            AppendError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl From<String> for AppendError {
    fn from(e: String) -> Self {
        AppendError::Metadata(e)
    }
}

impl From<&str> for AppendError {
    fn from(e: &str) -> Self {
        AppendError::Metadata(e.to_string())
    }
}

/// [`append_exif`] on a classic TIFF in a file (or any seekable stream
/// starting at the TIFF's first byte): reads the header and IFD0, appends the
/// new directories at the end and points the header at the new IFD0, so a
/// TIFF streamed into its file gets its metadata without being read back
/// whole. Leaves the file unchanged on [`AppendError::Metadata`].
pub fn append_exif_to<S: Read + Write + Seek>(
    file: &mut S,
    metadata: &Metadata,
    xmp: Option<&[u8]>,
) -> Result<(), AppendError> {
    let Ifd0 {
        len,
        base,
        order,
        entries: mut ifd0,
        entry_count,
        next_ifd,
    } = read_ifd0(file)?;
    let endian = if order.is_little() {
        Endian::Little
    } else {
        Endian::Big
    };

    let mut exif = BTreeMap::new();
    let mut gps = BTreeMap::new();
    for ifd in metadata.get_ifds() {
        if ifd.get_generic_ifd_nr() != 0 {
            continue;
        }
        let target = match ifd.get_ifd_type() {
            ExifTagGroup::GENERIC => &mut ifd0,
            ExifTagGroup::EXIF => &mut exif,
            ExifTagGroup::GPS => &mut gps,
            ExifTagGroup::INTEROP => continue,
        };
        let is_ifd0 = ifd.get_ifd_type() == ExifTagGroup::GENERIC;
        for tag in ifd.get_tags() {
            if let Some((id, entry)) = metadata_entry(tag, &endian) {
                if is_ifd0 && describes_source_pixels(id) {
                    continue;
                }
                target.entry(id).or_insert(entry);
            }
        }
    }
    if let Some(xmp) = xmp {
        ifd0.insert(
            XMP_TAG,
            Entry {
                field_type: TYPE_BYTE,
                count: u32::try_from(xmp.len()).map_err(|_| "XMP packet too large")?,
                value: EntryValue::Data(xmp.to_vec()),
            },
        );
    }
    if exif.is_empty() && gps.is_empty() && ifd0.len() == entry_count && xmp.is_none() {
        return Ok(());
    }

    // Build everything in a separate tail so a failure leaves the file intact.
    let mut tail = Vec::new();
    let mut place = |entries: &BTreeMap<u16, Entry>, next: u32| -> Result<u32, String> {
        if (base + tail.len()) % 2 == 1 {
            tail.push(0);
        }
        let offset =
            u32::try_from(base + tail.len()).map_err(|_| "TIFF exceeds 4 GiB".to_string())?;
        tail.extend_from_slice(&encode_ifd(entries, next, offset, order)?);
        Ok(offset)
    };

    if !exif.is_empty() {
        let offset = place(&exif, 0)?;
        ifd0.insert(EXIF_IFD_POINTER, pointer_entry(offset, order));
    }
    if !gps.is_empty() {
        let offset = place(&gps, 0)?;
        ifd0.insert(GPS_IFD_POINTER, pointer_entry(offset, order));
    }
    let new_ifd0 = place(&ifd0, next_ifd)?;

    let mut write = || -> io::Result<()> {
        file.seek(SeekFrom::Start(len))?;
        file.write_all(&tail)?;
        file.seek(SeekFrom::Start(4))?;
        file.write_all(&order.u32_bytes(new_ifd0))?;
        file.seek(SeekFrom::End(0)).map(drop)
    };
    write().map_err(AppendError::Io)
}

/// A classic TIFF's IFD0 as [`append_exif_to`] and [`append_reserved_tag`]
/// read it.
struct Ifd0 {
    /// The file's length.
    len: u64,
    base: usize,
    order: ByteOrder,
    entries: BTreeMap<u16, Entry>,
    entry_count: usize,
    next_ifd: u32,
}

fn read_ifd0<S: Read + Seek>(file: &mut S) -> Result<Ifd0, AppendError> {
    let len = file.seek(SeekFrom::End(0)).map_err(AppendError::Io)?;
    let base = usize::try_from(len).map_err(|_| "TIFF exceeds 4 GiB")?;
    let header = read_at(file, 0, 8.min(base))?;
    let order = match header.get(0..4) {
        Some([0x49, 0x49, 0x2a, 0x00]) => ByteOrder(true),
        Some([0x4d, 0x4d, 0x00, 0x2a]) => ByteOrder(false),
        _ => return Err("not a classic TIFF".into()),
    };
    let ifd0_offset = order
        .read_u32(&header, 4)
        .ok_or("TIFF header is truncated")? as usize;
    let count_bytes = read_at(file, ifd0_offset, 2.min(base.saturating_sub(ifd0_offset)))?;
    let entry_count = order
        .read_u16(&count_bytes, 0)
        .ok_or("IFD0 is out of bounds")? as usize;
    // IFD0's table relative to its offset: the count, the entries, the next pointer.
    let table_len = 2 + entry_count * 12 + 4;
    let table = read_at(
        file,
        ifd0_offset,
        table_len.min(base.saturating_sub(ifd0_offset)),
    )?;
    let next_ifd = order
        .read_u32(&table, 2 + entry_count * 12)
        .ok_or("IFD0 is truncated")?;

    let mut entries = BTreeMap::new();
    for i in 0..entry_count {
        let at = 2 + i * 12;
        let tag = order.read_u16(&table, at).ok_or("IFD0 is truncated")?;
        let field_type = order.read_u16(&table, at + 2).ok_or("IFD0 is truncated")?;
        let count = order.read_u32(&table, at + 4).ok_or("IFD0 is truncated")?;
        let value: [u8; 4] = table
            .get(at + 8..at + 12)
            .and_then(|b| b.try_into().ok())
            .ok_or("IFD0 is truncated")?;
        entries.insert(
            tag,
            Entry {
                field_type,
                count,
                value: EntryValue::Existing(value),
            },
        );
    }
    Ok(Ifd0 {
        len,
        base,
        order,
        entries,
        entry_count,
        next_ifd,
    })
}

/// Appends a copy of IFD0 with `tag` (UNDEFINED) whose value is the last
/// bytes of the file, and points the header at it, as [`append_exif_to`]
/// does. `size(start)` gives the value's length for its offset `start`;
/// the value is zeros, for the caller to fill. Returns its offset and
/// length. A tag IFD0 has already is refused.
pub fn append_reserved_tag<S: Read + Write + Seek>(
    file: &mut S,
    tag: u16,
    size: impl FnOnce(u64) -> Result<u32, String>,
) -> Result<(u64, u32), AppendError> {
    let Ifd0 {
        len,
        base,
        order,
        mut entries,
        next_ifd,
        ..
    } = read_ifd0(file)?;
    if entries.contains_key(&tag) {
        return Err(format!("IFD0 already has tag {tag:#06x}").into());
    }
    let ifd_offset = base + base % 2;
    let table_len = 2 + (entries.len() + 1) * 12 + 4;
    let start = (ifd_offset + table_len) as u64;
    let value_len = size(start)?;
    if value_len <= 4 {
        // It would sit in the entry itself, not at `start`.
        return Err("a reserved value takes more than four bytes".into());
    }
    entries.insert(
        tag,
        Entry {
            field_type: TYPE_UNDEFINED,
            count: value_len,
            value: EntryValue::Data(vec![0; value_len as usize]),
        },
    );
    let ifd_offset_u32 = u32::try_from(ifd_offset).map_err(|_| "TIFF exceeds 4 GiB")?;
    let mut tail = vec![0; ifd_offset - base];
    tail.extend_from_slice(&encode_ifd(&entries, next_ifd, ifd_offset_u32, order)?);
    if u32::try_from(ifd_offset + tail.len()).is_err() {
        return Err("TIFF exceeds 4 GiB".into());
    }
    let mut write = || -> io::Result<()> {
        file.seek(SeekFrom::Start(len))?;
        file.write_all(&tail)?;
        file.seek(SeekFrom::Start(4))?;
        file.write_all(&order.u32_bytes(ifd_offset_u32))?;
        file.seek(SeekFrom::End(0)).map(drop)
    };
    write().map_err(AppendError::Io)?;
    Ok((start, value_len))
}

/// Up to `len` bytes of `file` from `at` (fewer at its end).
fn read_at<S: Read + Seek>(file: &mut S, at: usize, len: usize) -> Result<Vec<u8>, AppendError> {
    let mut bytes = Vec::with_capacity(len);
    file.seek(SeekFrom::Start(at as u64))
        .map_err(AppendError::Io)?;
    file.by_ref()
        .take(len as u64)
        .read_to_end(&mut bytes)
        .map_err(AppendError::Io)?;
    Ok(bytes)
}

fn pointer_entry(offset: u32, order: ByteOrder) -> Entry {
    Entry {
        field_type: TYPE_LONG,
        count: 1,
        value: EntryValue::Existing(order.u32_bytes(offset)),
    }
}

/// IFD0 tags that describe how the *source's* pixels are stored (tiles,
/// predictor, sample format, sub-images, JPEG/YCbCr layout). The export's
/// strips are the encoder's; a stale tile tag next to them makes ImageIO and
/// other readers refuse the file.
fn describes_source_pixels(tag: u16) -> bool {
    matches!(
        tag,
        0x13d // Predictor
            | 0x142..=0x145 // TileWidth, TileLength, TileOffsets, TileByteCounts
            | 0x11c // PlanarConfiguration (the export is interleaved)
            | 0x14a // SubIFDs
            | 0x152 // ExtraSamples
            | 0x153 // SampleFormat
            | 0x201 | 0x202 // JPEGInterchangeFormat(+Length)
            | 0x211..=0x213 // YCbCr coefficients, subsampling, positioning
    )
}

fn metadata_entry(tag: &ExifTag, endian: &Endian) -> Option<(u16, Entry)> {
    if tag.get_tag_type() != TagType::VALUE || !tag.is_writable() {
        return None;
    }
    let field_type = tag.format().as_u16();
    let size = type_size(field_type)?;
    let mut data = tag.value_as_u8_vec(endian);
    let count = if tag.is_string() {
        // Strings are stored NUL-terminated.
        if data.last() != Some(&0) {
            data.push(0);
        }
        data.len()
    } else {
        if data.is_empty() || !data.len().is_multiple_of(size) {
            return None;
        }
        data.len() / size
    };
    Some((
        tag.as_u16(),
        Entry {
            field_type,
            count: u32::try_from(count).ok()?,
            value: EntryValue::Data(data),
        },
    ))
}

/// Encodes a directory placed at `offset`, followed by its out-of-line values.
fn encode_ifd(
    entries: &BTreeMap<u16, Entry>,
    next: u32,
    offset: u32,
    order: ByteOrder,
) -> Result<Vec<u8>, String> {
    let count = u16::try_from(entries.len()).map_err(|_| "too many TIFF entries".to_string())?;
    let table_len = 2 + entries.len() * 12 + 4;
    let mut table = Vec::with_capacity(table_len);
    let mut values = Vec::new();
    table.extend_from_slice(&order.u16_bytes(count));
    for (tag, entry) in entries {
        table.extend_from_slice(&order.u16_bytes(*tag));
        table.extend_from_slice(&order.u16_bytes(entry.field_type));
        table.extend_from_slice(&order.u32_bytes(entry.count));
        match &entry.value {
            EntryValue::Existing(field) => table.extend_from_slice(field),
            EntryValue::Data(data) if data.len() <= 4 => {
                let mut field = [0u8; 4];
                field[..data.len()].copy_from_slice(data);
                table.extend_from_slice(&field);
            }
            EntryValue::Data(data) => {
                if (table_len + values.len()) % 2 == 1 {
                    values.push(0);
                }
                let at = u32::try_from(offset as usize + table_len + values.len())
                    .map_err(|_| "TIFF exceeds 4 GiB".to_string())?;
                table.extend_from_slice(&order.u32_bytes(at));
                values.extend_from_slice(data);
            }
        }
    }
    table.extend_from_slice(&order.u32_bytes(next));
    table.extend_from_slice(&values);
    Ok(table)
}

#[cfg(test)]
mod tests {
    use super::*;
    use little_exif::rational::uR64;

    fn encoded_tiff() -> (Vec<u8>, image::ImageBuffer<image::Rgb<u16>, Vec<u16>>) {
        let pixels = image::ImageBuffer::from_fn(5, 3, |x, y| {
            image::Rgb([x as u16 * 1000, y as u16 * 2000, 65535 - x as u16 * 7])
        });
        let mut bytes = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb16(pixels.clone())
            .write_to(&mut bytes, image::ImageFormat::Tiff)
            .unwrap();
        (bytes.into_inner(), pixels)
    }

    fn sample_metadata() -> Metadata {
        let mut metadata = Metadata::new();
        metadata.set_tag(ExifTag::Software("Rawtonic test".to_string()));
        metadata.set_tag(ExifTag::Make("SONY".to_string()));
        metadata.set_tag(ExifTag::Artist("Jane Example".to_string()));
        metadata.set_tag(ExifTag::Orientation(vec![1]));
        metadata.set_tag(ExifTag::ImageWidth(vec![999]));
        metadata.set_tag(ExifTag::ExposureTime(vec![uR64 {
            nominator: 1,
            denominator: 250,
        }]));
        metadata.set_tag(ExifTag::ISO(vec![400]));
        metadata.set_tag(ExifTag::GPSLatitudeRef("N".to_string()));
        metadata.set_tag(ExifTag::GPSLatitude(vec![
            uR64 {
                nominator: 52,
                denominator: 1,
            },
            uR64 {
                nominator: 31,
                denominator: 1,
            },
            uR64 {
                nominator: 1234,
                denominator: 100,
            },
        ]));
        metadata
    }

    fn ascii(exif: &exif::Exif, tag: exif::Tag) -> String {
        exif.get_field(tag, exif::In::PRIMARY)
            .unwrap_or_else(|| panic!("{tag} missing"))
            .display_value()
            .to_string()
    }

    #[test]
    fn appends_ifd0_exif_and_gps_tags_without_touching_pixels() {
        let (mut bytes, pixels) = encoded_tiff();
        let before = bytes.len();
        append_exif(&mut bytes, &sample_metadata(), None).unwrap();
        assert!(bytes.len() > before);

        let decoded = image::load_from_memory_with_format(&bytes, image::ImageFormat::Tiff)
            .unwrap()
            .into_rgb16();
        assert_eq!(decoded, pixels);

        let exif = exif::Reader::new()
            .read_from_container(&mut Cursor::new(&bytes))
            .unwrap();
        assert_eq!(ascii(&exif, exif::Tag::Software), "\"Rawtonic test\"");
        assert_eq!(ascii(&exif, exif::Tag::Make), "\"SONY\"");
        assert_eq!(ascii(&exif, exif::Tag::Artist), "\"Jane Example\"");
        // The encoder's structure entries win over metadata with the same tag.
        assert_eq!(ascii(&exif, exif::Tag::ImageWidth), "5");
        assert_eq!(ascii(&exif, exif::Tag::ExposureTime), "1/250");
        assert_eq!(ascii(&exif, exif::Tag::PhotographicSensitivity), "400");
        assert_eq!(ascii(&exif, exif::Tag::GPSLatitudeRef), "N");
        let latitude = exif
            .get_field(exif::Tag::GPSLatitude, exif::In::PRIMARY)
            .unwrap();
        assert!(
            matches!(&latitude.value, exif::Value::Rational(v) if v.len() == 3 && v[2].num == 1234)
        );
    }

    #[test]
    fn source_image_structure_tags_never_reach_the_exported_ifd0() {
        // A source's IFD0 (a HEIC converted from a tiled TIFF, a DNG's
        // SubIFDs) can carry the layout of *its* pixels; the export is
        // strips, and ImageIO refuses a TIFF with strips and tile tags.
        let (mut bytes, pixels) = encoded_tiff();
        let mut metadata = sample_metadata();
        for (tag, value) in [(0x142u16, 512u16), (0x143, 512), (0x152, 2), (0x11c, 2)] {
            metadata.set_tag(ExifTag::UnknownINT16U(
                vec![value],
                tag,
                ExifTagGroup::GENERIC,
            ));
        }
        append_exif(&mut bytes, &metadata, None).unwrap();
        let decoded = image::load_from_memory_with_format(&bytes, image::ImageFormat::Tiff)
            .unwrap()
            .into_rgb16();
        assert_eq!(decoded, pixels);
        let ifd0 = read_ifd0(&mut Cursor::new(&bytes)).unwrap();
        for tag in [0x142u16, 0x143, 0x144, 0x145, 0x152, 0x11c] {
            assert!(!ifd0.entries.contains_key(&tag), "tag {tag:#x} copied");
        }
    }

    #[test]
    fn handles_big_endian_files() {
        // 2×1 grey 8-bit, Motorola byte order, IFD0 after the strip.
        let mut bytes = vec![0x4d, 0x4d, 0x00, 0x2a, 0, 0, 0, 10, 0x40, 0xc0];
        let entries: [(u16, u16, u32); 8] = [
            (256, 3, 2),
            (257, 3, 1),
            (258, 3, 8),
            (259, 3, 1),
            (262, 3, 1),
            (273, 4, 8),
            (278, 3, 1),
            (279, 4, 2),
        ];
        bytes.extend_from_slice(&(entries.len() as u16).to_be_bytes());
        for (tag, ty, value) in entries {
            bytes.extend_from_slice(&tag.to_be_bytes());
            bytes.extend_from_slice(&ty.to_be_bytes());
            bytes.extend_from_slice(&1u32.to_be_bytes());
            if ty == 3 {
                bytes.extend_from_slice(&(value as u16).to_be_bytes());
                bytes.extend_from_slice(&[0, 0]);
            } else {
                bytes.extend_from_slice(&value.to_be_bytes());
            }
        }
        bytes.extend_from_slice(&[0, 0, 0, 0]);

        append_exif(&mut bytes, &sample_metadata(), None).unwrap();

        let decoded = image::load_from_memory_with_format(&bytes, image::ImageFormat::Tiff)
            .unwrap()
            .into_luma8();
        assert_eq!(decoded.as_raw(), &vec![0x40, 0xc0]);
        let exif = exif::Reader::new()
            .read_from_container(&mut Cursor::new(&bytes))
            .unwrap();
        assert_eq!(ascii(&exif, exif::Tag::Make), "\"SONY\"");
        assert_eq!(ascii(&exif, exif::Tag::ExposureTime), "1/250");
    }

    #[test]
    fn writes_the_xmp_packet_as_tag_700() {
        let (mut bytes, pixels) = encoded_tiff();
        let packet = b"<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"/>";
        append_exif(&mut bytes, &Metadata::new(), Some(packet)).unwrap();
        let exif = exif::Reader::new()
            .read_from_container(&mut Cursor::new(&bytes))
            .unwrap();
        let field = exif
            .get_field(exif::Tag(exif::Context::Tiff, 700), exif::In::PRIMARY)
            .unwrap();
        assert!(matches!(&field.value, exif::Value::Byte(v) if v == packet));
        let decoded = image::load_from_memory_with_format(&bytes, image::ImageFormat::Tiff)
            .unwrap()
            .into_rgb16();
        assert_eq!(decoded, pixels);
    }

    #[test]
    fn rejects_non_tiff_input_unchanged() {
        let mut bytes = b"\xff\xd8\xff\xe0 not a tiff".to_vec();
        let original = bytes.clone();
        assert!(append_exif(&mut bytes, &sample_metadata(), None).is_err());
        assert_eq!(bytes, original);
    }

    #[test]
    fn empty_metadata_leaves_file_unchanged() {
        let (mut bytes, _) = encoded_tiff();
        let original = bytes.clone();
        append_exif(&mut bytes, &Metadata::new(), None).unwrap();
        assert_eq!(bytes, original);
    }
}
