// SPDX-License-Identifier: AGPL-3.0-or-later

use std::io::{Read, Seek, SeekFrom};

use rawler::tags::{DngTag, TiffCommonTag};

const SUBFILE_TYPE: u16 = TiffCommonTag::NewSubFileType as u16;
const COMPRESSION: u16 = TiffCommonTag::Compression as u16;
const PHOTOMETRIC: u16 = TiffCommonTag::PhotometricInt as u16;
const SUB_IFDS: u16 = TiffCommonTag::SubIFDs as u16;
const CFA_DIMENSIONS: u16 = TiffCommonTag::CFARepeatPatternDim as u16;

/// The mosaic layouts supported by AI Denoise.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SensorKind {
    Bayer,
    XTrans,
    Unknown,
}

/// Reads the DNG sensor tags without loading any TIFF tag payloads except
/// SubIFD offsets. In particular, OriginalRawFileData can be hundreds of MB.
/// None means the root directory did not identify the file as a DNG.
pub fn dng_sensor(bytes: &[u8]) -> Option<SensorKind> {
    let tiff = Tiff::new(bytes)?;
    let root = tiff.u32(bytes.get(4..8)?);
    let (entries, _) = tiff.directory(root)?;
    tiff.has_dng_version(entries)
        .then(|| tiff.sensor(root).unwrap_or(SensorKind::Unknown))
}

/// A TIFF-named DNG remains a RAW on a share that refuses mmap. Reading
/// the root directory by ordinary I/O is sufficient to route it to RAW.
pub(super) fn read_dng_header(reader: &mut (impl Read + Seek)) -> bool {
    let mut probe = || -> Option<bool> {
        let mut header = [0; 8];
        reader.read_exact(&mut header).ok()?;
        let tiff = Tiff::new(&header)?;
        let root = tiff.u32(&header[4..]);
        reader.seek(SeekFrom::Start(u64::from(root))).ok()?;
        let mut count = [0; 2];
        reader.read_exact(&mut count).ok()?;
        let mut entries = vec![0; usize::from(tiff.u16(&count)) * 12];
        reader.read_exact(&mut entries).ok()?;
        Some(tiff.has_dng_version(&entries))
    };
    probe().unwrap_or(false)
}

struct Tiff<'a> {
    bytes: &'a [u8],
    little_endian: bool,
}

impl<'a> Tiff<'a> {
    fn new(bytes: &'a [u8]) -> Option<Self> {
        let little_endian = match bytes.get(..4)? {
            b"II*\0" => true,
            b"MM\0*" => false,
            _ => return None,
        };
        Some(Self {
            bytes,
            little_endian,
        })
    }

    fn u16(&self, bytes: &[u8]) -> u16 {
        let pair = [bytes[0], bytes[1]];
        if self.little_endian {
            u16::from_le_bytes(pair)
        } else {
            u16::from_be_bytes(pair)
        }
    }

    fn u32(&self, bytes: &[u8]) -> u32 {
        let quad = [bytes[0], bytes[1], bytes[2], bytes[3]];
        if self.little_endian {
            u32::from_le_bytes(quad)
        } else {
            u32::from_be_bytes(quad)
        }
    }

    fn has_dng_version(&self, entries: &[u8]) -> bool {
        entries.as_chunks::<12>().0.iter().any(|entry| {
            self.u16(entry) == DngTag::DNGVersion as u16
                && self.u16(&entry[2..]) == 1
                && self.u32(&entry[4..]) == 4
        })
    }

    fn directory(&self, offset: u32) -> Option<(&'a [u8], u32)> {
        let data = self.bytes.get(offset as usize..)?;
        let len = usize::from(self.u16(data.get(..2)?)) * 12;
        Some((data.get(2..2 + len)?, self.u32(data.get(2 + len..6 + len)?)))
    }

    fn sensor(&self, root: u32) -> Option<SensorKind> {
        // Limit work on malformed files and follow the directories consulted
        // by the DNG decoder: main chain and its immediate SubIFDs.
        const MAX_DIRECTORIES: usize = 64;
        let mut next = root;
        let mut visited = Vec::new();
        let mut first = None;
        while next != 0 {
            let (entries, following) = self.directory(next)?;
            let sub_offsets = entries
                .as_chunks::<12>()
                .0
                .iter()
                .find(|entry| self.u16(entry.as_slice()) == SUB_IFDS);
            let subs = if let Some(entry) = sub_offsets {
                self.sub_offsets(entry.as_slice(), MAX_DIRECTORIES)?
            } else {
                &[]
            };
            let offsets =
                std::iter::once(next).chain(subs.as_chunks::<4>().0.iter().map(|b| self.u32(b)));
            for offset in offsets {
                if visited.len() == MAX_DIRECTORIES || visited.contains(&offset) {
                    return None;
                }
                visited.push(offset);
                let (tags, _) = self.directory(offset)?;
                let mut subfile_type = 0;
                let mut compression = 0;
                let mut photometric = 0;
                let mut dimensions = None;
                for tag in tags.as_chunks::<12>().0 {
                    let kind = self.u16(&tag[2..]);
                    let count = self.u32(&tag[4..]);
                    let value = &tag[8..];
                    match (self.u16(tag), kind, count) {
                        (SUBFILE_TYPE, 4, 1) => subfile_type = self.u32(value),
                        (SUBFILE_TYPE, 3, 1) => subfile_type = u32::from(self.u16(value)),
                        (COMPRESSION, 3, 1) => compression = self.u16(value),
                        (PHOTOMETRIC, 3, 1) => photometric = self.u16(value),
                        (CFA_DIMENSIONS, 3, 2) => {
                            dimensions = Some((self.u16(value), self.u16(&value[2..])))
                        }
                        _ => {}
                    }
                }
                if subfile_type & (1 | 4) != 0 || !matches!(compression, 1 | 7 | 8 | 0x884c | 52546)
                {
                    continue;
                }
                let sensor = match (photometric, dimensions) {
                    (32803, Some((2, 2))) => SensorKind::Bayer,
                    (32803, Some((6, 6))) => SensorKind::XTrans,
                    _ => SensorKind::Unknown,
                };
                // Like the decoder, prefer enhanced image data when present.
                if subfile_type == 16 {
                    return Some(sensor);
                }
                first.get_or_insert(sensor);
            }
            next = following;
        }
        first
    }

    fn sub_offsets(&self, entry: &'a [u8], limit: usize) -> Option<&'a [u8]> {
        let count = self.u32(&entry[4..]) as usize;
        if !matches!(self.u16(&entry[2..]), 4 | 13) || count > limit {
            return None;
        }
        let value = &entry[8..];
        if count <= 1 {
            return value.get(..count * 4);
        }
        self.bytes.get(self.u32(value) as usize..)?.get(..count * 4)
    }
}
