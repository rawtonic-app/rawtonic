// SPDX-License-Identifier: AGPL-3.0-or-later

use std::io::{Cursor, Read, Seek, SeekFrom};

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
    dng_sensor_reader(&mut Cursor::new(bytes))
}

/// The same tag-only probe through ordinary reads and seeks on a share
/// that refuses mmap.
pub fn dng_sensor_reader(reader: &mut (impl Read + Seek)) -> Option<SensorKind> {
    let (tiff, root) = Tiff::header(reader)?;
    let (entries, next) = tiff.directory(reader, root)?;
    tiff.has_dng_version(&entries).then(|| {
        tiff.sensor(reader, root, entries, next)
            .unwrap_or(SensorKind::Unknown)
    })
}

/// A TIFF-named DNG remains a RAW on a share that refuses mmap. Reading
/// the root directory by ordinary I/O is sufficient to route it to RAW.
pub(super) fn read_dng_header(reader: &mut (impl Read + Seek)) -> bool {
    Tiff::header(reader).is_some_and(|(tiff, root)| {
        tiff.directory(reader, root)
            .is_some_and(|(entries, _)| tiff.has_dng_version(&entries))
    })
}

struct Tiff {
    little_endian: bool,
}

impl Tiff {
    fn header(reader: &mut impl Read) -> Option<(Self, u32)> {
        let mut header = [0; 8];
        reader.read_exact(&mut header).ok()?;
        let little_endian = match &header[..4] {
            b"II*\0" => true,
            b"MM\0*" => false,
            _ => return None,
        };
        let tiff = Self { little_endian };
        let root = tiff.u32(&header[4..]);
        (root >= 8).then_some((tiff, root))
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

    fn directory(&self, reader: &mut (impl Read + Seek), offset: u32) -> Option<(Vec<u8>, u32)> {
        reader.seek(SeekFrom::Start(u64::from(offset))).ok()?;
        let mut count = [0; 2];
        reader.read_exact(&mut count).ok()?;
        let mut entries = vec![0; usize::from(self.u16(&count)) * 12];
        reader.read_exact(&mut entries).ok()?;
        let mut next = [0; 4];
        reader.read_exact(&mut next).ok()?;
        Some((entries, self.u32(&next)))
    }

    fn sensor(
        &self,
        reader: &mut (impl Read + Seek),
        root: u32,
        mut entries: Vec<u8>,
        mut following: u32,
    ) -> Option<SensorKind> {
        // Limit work on malformed files and follow the directories consulted
        // by the DNG decoder: main chain and its immediate SubIFDs.
        const MAX_DIRECTORIES: usize = 64;
        let mut next = root;
        let mut visited = Vec::new();
        let mut first = None;
        loop {
            let sub_offsets = entries
                .as_chunks::<12>()
                .0
                .iter()
                .find(|entry| self.u16(entry.as_slice()) == SUB_IFDS);
            let subs = if let Some(entry) = sub_offsets {
                self.sub_offsets(reader, entry, MAX_DIRECTORIES)?
            } else {
                Vec::new()
            };
            for offset in std::iter::once(next).chain(subs) {
                if visited.len() == MAX_DIRECTORIES || visited.contains(&offset) {
                    return None;
                }
                visited.push(offset);
                let sub_entries;
                let tags = if offset == next {
                    &entries
                } else {
                    sub_entries = self.directory(reader, offset)?.0;
                    &sub_entries
                };
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
            if following == 0 {
                return first;
            }
            next = following;
            (entries, following) = self.directory(reader, next)?;
        }
    }

    fn sub_offsets(
        &self,
        reader: &mut (impl Read + Seek),
        entry: &[u8; 12],
        limit: usize,
    ) -> Option<Vec<u32>> {
        let count = self.u32(&entry[4..]) as usize;
        if !matches!(self.u16(&entry[2..]), 4 | 13) || count > limit {
            return None;
        }
        let value = &entry[8..];
        if count <= 1 {
            return Some((count == 1).then(|| self.u32(value)).into_iter().collect());
        }
        let mut offsets = vec![0; count * 4];
        reader
            .seek(SeekFrom::Start(u64::from(self.u32(value))))
            .ok()?;
        reader.read_exact(&mut offsets).ok()?;
        Some(
            offsets
                .as_chunks::<4>()
                .0
                .iter()
                .map(|b| self.u32(b))
                .collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tag(tag: u16, kind: u16, count: u32, value: [u8; 4]) -> [u8; 12] {
        let mut entry = [0; 12];
        entry[..2].copy_from_slice(&tag.to_be_bytes());
        entry[2..4].copy_from_slice(&kind.to_be_bytes());
        entry[4..8].copy_from_slice(&count.to_be_bytes());
        entry[8..].copy_from_slice(&value);
        entry
    }

    fn ifd(bytes: &mut Vec<u8>, tags: &[[u8; 12]], next: u32) {
        bytes.extend((tags.len() as u16).to_be_bytes());
        bytes.extend(tags.as_flattened());
        bytes.extend(next.to_be_bytes());
    }

    #[test]
    fn reads_bayer_in_a_big_endian_subifd_and_prefers_enhanced_data() {
        let mut bytes = b"MM\0*\0\0\0\x08".to_vec();
        let version = tag(DngTag::DNGVersion as u16, 1, 4, [1, 4, 0, 0]);
        let compression = tag(COMPRESSION, 3, 1, [0, 1, 0, 0]);
        let cfa = tag(PHOTOMETRIC, 3, 1, [0x80, 0x23, 0, 0]);
        let xtrans = tag(CFA_DIMENSIONS, 3, 2, [0, 6, 0, 6]);
        let bayer = tag(CFA_DIMENSIONS, 3, 2, [0, 2, 0, 2]);
        let subfile = |flags: u32| tag(SUBFILE_TYPE, 4, 1, flags.to_be_bytes());

        // IFD 0 points to an X-Trans-looking mask and then the Bayer raw IFD.
        let sub_offsets = tag(SUB_IFDS, 4, 2, 74u32.to_be_bytes());
        ifd(
            &mut bytes,
            &[subfile(1), compression, cfa, sub_offsets, version],
            0,
        );
        bytes.extend(82u32.to_be_bytes());
        bytes.extend(136u32.to_be_bytes());
        ifd(&mut bytes, &[subfile(4), compression, cfa, xtrans], 0);
        ifd(&mut bytes, &[compression, cfa, bayer], 0);
        assert_eq!(dng_sensor(&bytes), Some(SensorKind::Bayer));
        assert!(read_dng_header(&mut Cursor::new(&bytes)));

        let enhanced = (bytes.len() as u32).to_be_bytes();
        bytes[70..74].copy_from_slice(&enhanced);
        let linear_raw = tag(PHOTOMETRIC, 3, 1, [0x88, 0x4c, 0, 0]);
        ifd(&mut bytes, &[subfile(16), compression, linear_raw], 0);
        assert_eq!(dng_sensor(&bytes), Some(SensorKind::Unknown));
    }

    #[test]
    fn cycles_and_excessive_directory_chains_cannot_choose_a_sensor() {
        let version = tag(DngTag::DNGVersion as u16, 1, 4, [1, 4, 0, 0]);
        let mut bytes = b"MM\0*\0\0\0\x08".to_vec();
        ifd(&mut bytes, &[version], 8);
        assert_eq!(dng_sensor(&bytes), Some(SensorKind::Unknown));
        bytes[22..26].copy_from_slice(&26u32.to_be_bytes());
        for _ in 0..63 {
            let next = bytes.len() as u32 + 6;
            ifd(&mut bytes, &[], next);
        }
        let raw = [
            tag(COMPRESSION, 3, 1, [0, 1, 0, 0]),
            tag(PHOTOMETRIC, 3, 1, [0x80, 0x23, 0, 0]),
            tag(CFA_DIMENSIONS, 3, 2, [0, 2, 0, 2]),
        ];
        ifd(&mut bytes, &raw, 0);
        assert_eq!(dng_sensor(&bytes), Some(SensorKind::Unknown));
    }
}
