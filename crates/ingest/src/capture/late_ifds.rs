//! The IFDs of a TIFF-based file wherever they are (a DNG that keeps them
//! after its image data, as merges write them), read with seeks into
//! pieces the RAW decoder and the EXIF reader read again as the file
//! (`super::sparse_file`).
//!
//! A damaged file's offsets may point anywhere: back into a chain, at one
//! IFD from many pointers, into another IFD's entries. What the pieces
//! hold is therefore a sanitized copy of the structure walked, not the
//! file's own bytes where they would lead a reader elsewhere: every IFD is
//! walked once and pointed to once (a pointer to an IFD seen before, past
//! the limits below or overlapping what else was laid down is removed);
//! entries whose values were not read are removed, so neither reader takes
//! the empty space where such a value lies for it; the IFDs and the pointer
//! arrays written anew lie over every other piece and overlap none, so no
//! value read brings back what was cut.
use std::{
    collections::HashSet,
    io::{Read, Seek, SeekFrom},
};

/// At most this many IFDs are walked.
const IFD_LIMIT: usize = 256;
/// Entries of an IFD past this many are not kept (the RAW decoder reads at
/// most 4096; files hold a few dozen).
const ENTRY_LIMIT: usize = 1024;
/// How deep pointers are followed: the root chain is depth 0, its SubIFDs
/// and EXIF and GPS IFDs depth 1, the EXIF IFD's interoperability IFD
/// depth 2. Neither reader follows pointers deeper or chains of sub-IFDs.
const DEPTH_LIMIT: u8 = 2;
/// At most this many IFDs one pointer entry (SubIFDs) points to are kept.
const POINTER_LIMIT: usize = 16;
/// Values larger than this are not read.
const VALUE_LIMIT: u64 = 8 << 20;
/// At most this much of a file is read for its IFDs in all: every range
/// read counts, also one that entries share (a damaged file's entries may
/// point at the same large range thousands of times).
pub(super) const METADATA_LIMIT: u64 = 64 << 20;

/// Tags whose values point to IFDs: SubIFDs, EXIF, GPS, interoperability.
const POINTER_TAGS: [u16; 4] = [0x014a, 0x8769, 0x8825, 0xa005];
/// A tag the RAW decoder reads as an IFD that is not walked here (the ICC
/// profile): its entry is removed.
const ICC_PROFILE: u16 = 0x8773;

/// One IFD as walked.
struct Table {
    offset: u64,
    depth: u8,
    /// Its entries as read (at most `ENTRY_LIMIT`), each kept or not.
    entries: Vec<([u8; 12], bool)>,
    /// The next IFD of the chain: 0, or one walked.
    next: u32,
}

/// A value read for an entry: where it lies, its bytes, and its entry.
struct Value {
    at: u64,
    bytes: Vec<u8>,
    table: usize,
    entry: usize,
}

struct Walk<'f, F> {
    file: &'f mut F,
    len: u64,
    little: bool,
    budget: u64,
    seen: HashSet<u64>,
    tables: Vec<Table>,
    /// The root chain's tables, in order.
    chain: Vec<usize>,
    values: Vec<Value>,
    /// Pointer arrays written anew (fewer pointers than the file's).
    arrays: Vec<(u64, Vec<u8>)>,
    /// What is written anew (tables, pointer arrays): no two overlap, and
    /// no value laid under them overlaps one.
    claimed: Vec<(u64, u64)>,
}

/// The pieces (offset, bytes) of a TIFF-based file whose first bytes are
/// `header`: `header`, the values read, then the pointer arrays and IFDs as
/// walked. Empty when no root IFD could be walked; an error when reading
/// fails.
pub(super) fn pieces(
    file: &mut (impl Read + Seek),
    len: u64,
    header: &[u8],
) -> std::io::Result<Vec<(u64, Vec<u8>)>> {
    let Some(little) = super::tiff_byte_order(header) else {
        return Ok(Vec::new());
    };
    let Some(first) = header.get(4..8) else {
        return Ok(Vec::new());
    };
    let mut walk = Walk {
        file,
        len,
        little,
        budget: METADATA_LIMIT,
        seen: HashSet::new(),
        tables: Vec::new(),
        chain: Vec::new(),
        values: Vec::new(),
        arrays: Vec::new(),
        claimed: Vec::new(),
    };
    let first = u64::from(walk.u32_of(first));
    let Some(root) = walk.ifd(first, 0)? else {
        return Ok(Vec::new());
    };
    walk.chain.push(root);
    // Breadth first: each IFD's entries once, in the order they were found.
    let mut at = 0;
    while at < walk.tables.len() {
        walk.entries(at)?;
        at += 1;
    }
    Ok(walk.finish(header))
}

impl<F: Read + Seek> Walk<'_, F> {
    fn u16_of(&self, b: &[u8]) -> u16 {
        let b = [b[0], b[1]];
        if self.little {
            u16::from_le_bytes(b)
        } else {
            u16::from_be_bytes(b)
        }
    }

    fn u32_of(&self, b: &[u8]) -> u32 {
        let b = [b[0], b[1], b[2], b[3]];
        if self.little {
            u32::from_le_bytes(b)
        } else {
            u32::from_be_bytes(b)
        }
    }

    fn u16_bytes(&self, n: u16) -> [u8; 2] {
        if self.little {
            n.to_le_bytes()
        } else {
            n.to_be_bytes()
        }
    }

    fn u32_bytes(&self, n: u32) -> [u8; 4] {
        if self.little {
            n.to_le_bytes()
        } else {
            n.to_be_bytes()
        }
    }

    /// `size` bytes at `offset`, within the file and the budget.
    fn read(&mut self, offset: u64, size: u64) -> std::io::Result<Option<Vec<u8>>> {
        if offset.checked_add(size).is_none_or(|end| end > self.len) || size > self.budget {
            return Ok(None);
        }
        self.budget -= size;
        self.file.seek(SeekFrom::Start(offset))?;
        let mut bytes = vec![0; size as usize];
        self.file.read_exact(&mut bytes)?;
        Ok(Some(bytes))
    }

    /// Claims `start..end` for what is written anew, when nothing claimed
    /// overlaps it.
    fn claim(&mut self, start: u64, end: u64) -> bool {
        if self.claimed.iter().any(|&(s, e)| start < e && s < end) {
            return false;
        }
        self.claimed.push((start, end));
        true
    }

    /// The IFD at `offset` walked at `depth`: read and claimed, its index;
    /// `None` when it was walked before, lies outside the file or the
    /// budget, past the limits, or over what is claimed.
    fn ifd(&mut self, offset: u64, depth: u8) -> std::io::Result<Option<usize>> {
        if offset < 8
            || depth > DEPTH_LIMIT
            || self.tables.len() >= IFD_LIMIT
            || self.seen.contains(&offset)
        {
            return Ok(None);
        }
        let Some(count) = self.read(offset, 2)? else {
            return Ok(None);
        };
        let count = usize::from(self.u16_of(&count));
        let kept = count.min(ENTRY_LIMIT);
        let Some(entries) = self.read(offset + 2, 12 * kept as u64)? else {
            return Ok(None);
        };
        // The next IFD's offset follows all its entries, kept or not.
        let Some(next) = self.read(offset + 2 + 12 * count as u64, 4)? else {
            return Ok(None);
        };
        let next = self.u32_of(&next);
        // The table as it is written anew: no longer than this.
        if !self.claim(offset, offset + 2 + 12 * kept as u64 + 4) {
            return Ok(None);
        }
        self.seen.insert(offset);
        let entries = entries
            .as_chunks::<12>()
            .0
            .iter()
            .map(|entry| (*entry, true))
            .collect();
        self.tables.push(Table {
            offset,
            depth,
            entries,
            next,
        });
        Ok(Some(self.tables.len() - 1))
    }

    /// Walks the entries of table `t`: values read, pointers followed, its
    /// chain's next IFD claimed.
    fn entries(&mut self, t: usize) -> std::io::Result<()> {
        let depth = self.tables[t].depth;
        for i in 0..self.tables[t].entries.len() {
            let entry = self.tables[t].entries[i].0;
            let (tag, kind, number) = (
                self.u16_of(&entry[..2]),
                self.u16_of(&entry[2..4]),
                self.u32_of(&entry[4..8]),
            );
            let unit: u64 = match kind {
                1 | 2 | 6 | 7 => 1,
                3 | 8 => 2,
                4 | 9 | 11 | 13 => 4,
                5 | 10 | 12 => 8,
                _ => {
                    self.tables[t].entries[i].1 = false;
                    continue;
                }
            };
            let size = unit * u64::from(number);
            let at = u64::from(self.u32_of(&entry[8..12]));
            if tag == ICC_PROFILE {
                self.tables[t].entries[i].1 = false;
            } else if POINTER_TAGS.contains(&tag) {
                self.pointers(t, i, kind, number, at)?;
            } else if size > 4 {
                match (size <= VALUE_LIMIT)
                    .then(|| self.read(at, size))
                    .transpose()?
                    .flatten()
                {
                    Some(bytes) => self.values.push(Value {
                        at,
                        bytes,
                        table: t,
                        entry: i,
                    }),
                    None => self.tables[t].entries[i].1 = false,
                }
            }
        }
        // Only the root chain is followed; a sub-IFD's next IFD is cut (the
        // EXIF reader refuses a sub-IFD with one).
        let next = u64::from(self.tables[t].next);
        let next = if depth == 0 && next != 0 {
            self.ifd(next, 0)?
        } else {
            None
        };
        match next {
            Some(n) => self.chain.push(n),
            None => self.tables[t].next = 0,
        }
        Ok(())
    }

    /// Follows pointer entry `i` of table `t` (`number` LONG/IFD offsets,
    /// inline or at `at`): the IFDs it points to that can be walked are
    /// kept, in their order, the entry rewritten to point to just those.
    fn pointers(
        &mut self,
        t: usize,
        i: usize,
        kind: u16,
        number: u32,
        at: u64,
    ) -> std::io::Result<()> {
        let depth = self.tables[t].depth;
        if !matches!(kind, 4 | 13) || number == 0 || depth >= DEPTH_LIMIT {
            self.tables[t].entries[i].1 = false;
            return Ok(());
        }
        let offsets: Vec<u32> = if number == 1 {
            vec![at as u32]
        } else {
            let size = 4 * u64::from(number);
            // The array is claimed before the IFDs it points to: none of
            // them may lie over it, where the array written anew goes.
            let read = if size <= VALUE_LIMIT && self.claim(at, at + size) {
                self.read(at, size)?
            } else {
                None
            };
            let Some(bytes) = read else {
                self.tables[t].entries[i].1 = false;
                return Ok(());
            };
            bytes
                .as_chunks::<4>()
                .0
                .iter()
                .map(|b| self.u32_of(b))
                .collect()
        };
        let mut kept = Vec::new();
        for offset in offsets {
            if kept.len() >= POINTER_LIMIT {
                break;
            }
            if self.ifd(u64::from(offset), depth + 1)?.is_some() {
                kept.push(offset);
            }
        }
        let count = self.u32_bytes(kept.len() as u32);
        match kept[..] {
            [] => self.tables[t].entries[i].1 = false,
            // One pointer: inline.
            [one] => {
                let value = self.u32_bytes(one);
                let entry = &mut self.tables[t].entries[i].0;
                entry[4..8].copy_from_slice(&count);
                entry[8..12].copy_from_slice(&value);
            }
            _ => {
                self.tables[t].entries[i].0[4..8].copy_from_slice(&count);
                let array = kept.iter().flat_map(|&o| self.u32_bytes(o)).collect();
                self.arrays.push((at, array));
            }
        }
        Ok(())
    }

    /// The pieces: `header`, the values that overlap nothing claimed (an
    /// entry whose value does is removed), the arrays, then the tables
    /// written anew. The chain ends before an IFD left without entries (the
    /// decoder refuses a chain holding one).
    fn finish(mut self, header: &[u8]) -> Vec<(u64, Vec<u8>)> {
        let mut claimed = std::mem::take(&mut self.claimed);
        claimed.sort_unstable();
        let mut pieces = vec![(0, header.to_vec())];
        for value in std::mem::take(&mut self.values) {
            let end = value.at + value.bytes.len() as u64;
            // The first claimed range ending after the value's start (they
            // do not overlap each other, so their ends are sorted too).
            let first = claimed.partition_point(|&(_, e)| e <= value.at);
            if claimed.get(first).is_some_and(|&(s, _)| s < end) {
                self.tables[value.table].entries[value.entry].1 = false;
            } else {
                pieces.push((value.at, value.bytes));
            }
        }
        let kept = |table: &Table| table.entries.iter().any(|(_, kept)| *kept);
        if !kept(&self.tables[self.chain[0]]) {
            return Vec::new();
        }
        if let Some(empty) = self.chain.iter().position(|&t| !kept(&self.tables[t])) {
            let last = self.chain[empty - 1];
            self.tables[last].next = 0;
        }
        pieces.append(&mut self.arrays);
        for table in &self.tables {
            let entries: Vec<&[u8; 12]> = table
                .entries
                .iter()
                .filter(|(_, kept)| *kept)
                .map(|(entry, _)| entry)
                .collect();
            let mut bytes = self.u16_bytes(entries.len() as u16).to_vec();
            bytes.extend(entries.into_iter().flatten());
            bytes.extend(self.u32_bytes(table.next));
            pieces.push((table.offset, bytes));
        }
        pieces
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// A little-endian IFD entry.
    fn entry(tag: u16, kind: u16, count: u32, value: u32) -> Vec<u8> {
        [
            &tag.to_le_bytes()[..],
            &kind.to_le_bytes(),
            &count.to_le_bytes(),
            &value.to_le_bytes(),
        ]
        .concat()
    }

    /// An IFD of `entries` with `next`.
    fn ifd(entries: &[Vec<u8>], next: u32) -> Vec<u8> {
        let mut table = (entries.len() as u16).to_le_bytes().to_vec();
        table.extend(entries.concat());
        table.extend(next.to_le_bytes());
        table
    }

    /// `parts` (offset, bytes) laid into a file after a little-endian TIFF
    /// header pointing to `root`.
    fn file(root: u32, parts: &[(u32, Vec<u8>)]) -> Vec<u8> {
        let mut bytes = b"II*\0".to_vec();
        bytes.extend(root.to_le_bytes());
        for (at, part) in parts {
            let end = *at as usize + part.len();
            if bytes.len() < end {
                bytes.resize(end, 0);
            }
            bytes[*at as usize..end].copy_from_slice(part);
        }
        bytes
    }

    fn orientation() -> Vec<u8> {
        entry(0x0112, 3, 1, 1)
    }

    /// The pieces of `bytes` (its first 8 bytes read as the header), laid
    /// into the buffer the decoders read, with the bytes read for them.
    fn sparse(bytes: &[u8]) -> (Vec<u8>, u64) {
        let mut reader = super::super::tests::Counted(Cursor::new(bytes), 0);
        let pieces = pieces(&mut reader, bytes.len() as u64, &bytes[..8]).unwrap();
        let read = reader.1;
        (super::super::sparse_file(&pieces).unwrap_or_default(), read)
    }

    /// What the decoders reading `file` again are led to, as they follow
    /// it (the root chain, its pointer entries one level down, the EXIF
    /// IFD's interoperability IFD): each IFD reached once, every value an
    /// entry declares inside the buffer, at most the budget in all, and no
    /// pointer past the depth limit. The IFDs reached, in order.
    fn replay(file: &[u8]) -> Vec<u64> {
        let u16_at = |at: usize| u16::from_le_bytes([file[at], file[at + 1]]);
        let u32_at = |at: usize| u32::from_le_bytes(file[at..at + 4].try_into().unwrap());
        let mut reached = Vec::new();
        let mut declared = 0u64;
        let mut todo = vec![(u64::from(u32_at(4)), 0u8)];
        while let Some((offset, depth)) = todo.pop() {
            assert!(!reached.contains(&offset), "{offset} reached twice");
            assert!(reached.len() < IFD_LIMIT);
            reached.push(offset);
            let at = offset as usize;
            let count = usize::from(u16_at(at));
            assert!(count <= ENTRY_LIMIT);
            for i in 0..count {
                let e = at + 2 + 12 * i;
                let (tag, kind, number, value) =
                    (u16_at(e), u16_at(e + 2), u32_at(e + 4), u32_at(e + 8));
                let unit = match kind {
                    1 | 2 | 6 | 7 => 1,
                    3 | 8 => 2,
                    4 | 9 | 11 | 13 => 4,
                    5 | 10 | 12 => 8,
                    _ => panic!("type {kind} kept"),
                };
                assert_ne!(tag, ICC_PROFILE);
                let size = unit * u64::from(number);
                if size > 4 {
                    assert!(u64::from(value) + size <= file.len() as u64);
                    declared += size;
                }
                if POINTER_TAGS.contains(&tag) {
                    assert!(depth < DEPTH_LIMIT && matches!(kind, 4 | 13));
                    assert!(number as usize <= POINTER_LIMIT);
                    let children: Vec<u32> = if number == 1 {
                        vec![value]
                    } else {
                        (0..number as usize)
                            .map(|n| u32_at(value as usize + 4 * n))
                            .collect()
                    };
                    todo.extend(children.into_iter().map(|c| (u64::from(c), depth + 1)));
                }
            }
            let next = u32_at(at + 2 + 12 * count);
            if depth > 0 {
                assert_eq!(next, 0, "a sub-IFD's chain kept");
            } else if next != 0 {
                todo.push((u64::from(next), 0));
            }
        }
        assert!(declared <= METADATA_LIMIT);
        reached
    }

    /// Both readers read the pieces of `bytes` again to the end.
    fn decoders_end(bytes: &[u8]) {
        let pieces = pieces(&mut Cursor::new(bytes), bytes.len() as u64, &bytes[..8]).unwrap();
        if !pieces.is_empty() {
            let _ = super::super::sparse_capture(&pieces);
        }
    }

    #[test]
    fn a_chain_back_to_itself_or_an_earlier_ifd_ends() {
        let (a, b) = (64u32 << 10, 80u32 << 10);
        for bytes in [
            file(a, &[(a, ifd(&[orientation()], a))]),
            file(
                a,
                &[(a, ifd(&[orientation()], b)), (b, ifd(&[orientation()], a))],
            ),
        ] {
            let (laid, _) = sparse(&bytes);
            let reached = replay(&laid);
            assert_eq!(reached.len(), if bytes.len() > b as usize { 2 } else { 1 });
            decoders_end(&bytes);
        }
    }

    /// One IFD listed thousands of times by a SubIFDs entry, holding a large
    /// value: walked, read and pointed to once (the decoder read it and its
    /// value once per pointer, 8 GiB for these).
    #[test]
    fn a_repeated_sub_ifd_is_walked_and_kept_once() {
        let (root, array, child, value) = (64u32 << 10, 128u32 << 10, 192u32 << 10, 256u32 << 10);
        let bytes = file(
            root,
            &[
                (root, ifd(&[entry(0x014a, 4, 8192, array)], 0)),
                (array, (0..8192).flat_map(|_| child.to_le_bytes()).collect()),
                (child, ifd(&[entry(0xc000, 1, 1 << 20, value)], 0)),
                (value + (1 << 20) - 1, vec![0]),
            ],
        );
        let (laid, read) = sparse(&bytes);
        assert_eq!(replay(&laid), [u64::from(root), u64::from(child)]);
        // The root's entry now points to the child inline.
        let pointer = &laid[root as usize + 2..root as usize + 14];
        assert_eq!(pointer, entry(0x014a, 4, 1, child));
        assert!(read < (1 << 20) + (64 << 10), "{read} bytes read");
        decoders_end(&bytes);
    }

    /// Pointers back to the root, to the IFD itself, to an IFD seen
    /// before, into another IFD's entries or past the end are removed;
    /// the others kept in their order.
    #[test]
    fn only_pointers_to_ifds_not_seen_are_kept() {
        let (root, array, a, b, past) = (8u32, 4096u32, 8192u32, 12288u32, 1u32 << 30);
        let pointers = [a, root, a, a + 6, b, past, b];
        let bytes = file(
            root,
            &[
                (
                    root,
                    ifd(&[entry(0x014a, 4, pointers.len() as u32, array)], 0),
                ),
                (
                    array,
                    pointers.iter().flat_map(|p| p.to_le_bytes()).collect(),
                ),
                (a, ifd(&[orientation(), orientation()], 0)),
                (b, ifd(&[orientation()], 0)),
            ],
        );
        let (laid, _) = sparse(&bytes);
        assert_eq!(replay(&laid), [8, u64::from(b), u64::from(a)]);
        assert_eq!(
            &laid[array as usize..array as usize + 8],
            [a.to_le_bytes(), b.to_le_bytes()].concat()
        );
        decoders_end(&bytes);
    }

    /// A value that covers a walked IFD (here the EXIF IFD's value laid
    /// over the root, whose chain points to itself) is removed, so it does
    /// not bring back what was cut; nor does a value over a pointer array.
    #[test]
    fn a_value_over_what_is_written_anew_is_removed() {
        let (root, exif, array) = (64u32 << 10, 80u32 << 10, 96u32 << 10);
        let a = 112u32 << 10;
        let bytes = file(
            root,
            &[
                (
                    root,
                    ifd(
                        &[entry(0x014a, 4, 2, array), entry(0x8769, 4, 1, exif)],
                        root,
                    ),
                ),
                (
                    array,
                    [a, root].iter().flat_map(|p| p.to_le_bytes()).collect(),
                ),
                (a, ifd(&[orientation()], 0)),
                (
                    exif,
                    ifd(
                        &[
                            entry(0x9000, 7, 4, 0x3032_3330),
                            entry(0xa420, 7, 30, root),
                            entry(0xa430, 7, 8, array),
                        ],
                        0,
                    ),
                ),
            ],
        );
        let (laid, _) = sparse(&bytes);
        assert_eq!(
            replay(&laid),
            [u64::from(root), u64::from(exif), u64::from(a)]
        );
        let next = &laid[root as usize + 2 + 24..root as usize + 30];
        assert_eq!(next, [0; 4]);
        // The EXIF IFD keeps only its inline entry.
        assert_eq!(
            u16::from_le_bytes([laid[exif as usize], laid[exif as usize + 1]]),
            1
        );
        decoders_end(&bytes);
    }

    /// IFDs whose tables would overlap are not both walked.
    #[test]
    fn overlapping_ifds_are_not_both_walked() {
        let (root, a) = (64u32 << 10, 80u32 << 10);
        let bytes = file(
            root,
            &[
                (
                    root,
                    ifd(&[entry(0x8769, 4, 1, a), entry(0x8825, 4, 1, a + 6)], 0),
                ),
                (a, ifd(&[orientation(), orientation(), orientation()], 0)),
            ],
        );
        let (laid, _) = sparse(&bytes);
        assert_eq!(replay(&laid), [u64::from(root), u64::from(a)]);
        decoders_end(&bytes);
    }

    /// Pointers past the depth limit, a sub-IFD's chain and the ICC
    /// profile (an IFD to the decoder) are removed; entries past the entry
    /// limit too.
    #[test]
    fn depth_entry_and_chain_limits_hold() {
        let (root, exif, interop, deeper) = (8u32, 4096u32, 8192u32, 12288u32);
        let many: Vec<Vec<u8>> = (0..ENTRY_LIMIT as u16 + 50)
            .map(|i| entry(0xc000 + i, 3, 1, 1))
            .collect();
        let bytes = file(
            root,
            &[
                (
                    root,
                    ifd(
                        &[entry(0x8769, 4, 1, exif), entry(ICC_PROFILE, 7, 64, 2048)],
                        0,
                    ),
                ),
                (exif, ifd(&[entry(0xa005, 4, 1, interop)], deeper)),
                (interop, ifd(&[entry(0x8769, 4, 1, deeper)], 0)),
                (deeper, ifd(&many, 0)),
            ],
        );
        let (laid, _) = sparse(&bytes);
        assert_eq!(replay(&laid), [8, u64::from(exif), u64::from(interop)]);
        let root_count = u16::from_le_bytes([laid[8], laid[9]]);
        assert_eq!(root_count, 1);
        // The same IFDs as the chain: entries past the limit are not kept.
        let bytes = file(root, &[(root, ifd(&many, 0))]);
        let (laid, _) = sparse(&bytes);
        assert_eq!(
            usize::from(u16::from_le_bytes([laid[8], laid[9]])),
            ENTRY_LIMIT
        );
        decoders_end(&bytes);
    }

    /// Entries whose values are not read (too large, past the budget, of
    /// an unknown type) are removed; entries sharing a value keep it, each
    /// counted against the budget.
    #[test]
    fn only_values_read_are_declared() {
        let root = 64u32 << 10;
        let large = VALUE_LIMIT as u32;
        let mut entries = vec![
            entry(0x010e, 2, 8, 1024),         // read
            entry(0x0110, 2, 8, 1024),         // the same value
            entry(0xc000, 1, large + 1, 2048), // too large
            entry(0xc003, 0, 1000, 4096),      // unknown type
        ];
        entries.extend((0..9).map(|i| entry(0xc100 + i, 1, large, 128 << 10)));
        let mut bytes = file(
            root,
            &[(root, ifd(&entries, 0)), (1024, b"RAW Six\0".to_vec())],
        );
        bytes.resize((128 << 10) + large as usize, 0);
        let (laid, read) = sparse(&bytes);
        replay(&laid);
        let count = usize::from(u16::from_le_bytes([
            laid[root as usize],
            laid[root as usize + 1],
        ]));
        let kept = count - 2;
        assert!(kept > 0 && kept < 9, "{kept}");
        assert!(read <= METADATA_LIMIT + (64 << 10));
        let pieces = pieces(&mut Cursor::new(&bytes), bytes.len() as u64, &bytes[..8]).unwrap();
        let model = super::super::sparse_capture(&pieces).unwrap().model;
        assert_eq!(model.as_deref(), Some("RAW Six"));
    }

    /// Random damage to a real DNG's IFDs (as merges write them, at the
    /// end): whatever the bytes say, the walk ends within its budget, what
    /// it lays down leads the decoders to each IFD once, and both read it
    /// to the end.
    #[test]
    fn damaged_ifds_are_walked_within_the_limits() {
        let mut metadata = rawler::decoders::RawMetadata {
            make: "Testmaker".into(),
            model: "RAW Fuzz".into(),
            ..Default::default()
        };
        metadata.exif.date_time_original = Some("2024:07:14 06:15:30".into());
        metadata.exif.iso_speed_ratings = Some(200);
        let image = image::Rgb32FImage::from_fn(32, 32, |x, y| {
            image::Rgb([x as f32 / 32.0, y as f32 / 32.0, 0.5])
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
        // Undamaged, the walk finds what the decoder finds in the whole file.
        let pieces = pieces(&mut Cursor::new(&dng), dng.len() as u64, &dng[..8]).unwrap();
        let whole = super::super::from_bytes(&dng);
        let walked = super::super::sparse_capture(&pieces).unwrap();
        assert_eq!(
            (walked.model, walked.iso, walked.captured),
            (whole.model, whole.iso, whole.captured)
        );
        assert!(whole.captured.is_some());
        let root = u32::from_le_bytes(dng[4..8].try_into().unwrap()) as usize;
        let mut seed = 0x2545_f491_u32;
        let mut random = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        let mut walked = 0;
        for _ in 0..2000 {
            let mut bytes = dng.clone();
            for _ in 0..1 + random() % 8 {
                // Mostly within the IFDs, sometimes offsets written over
                // entries (pointing back into them, or anywhere).
                let at = root.min(bytes.len() - 4)
                    + random() as usize % (bytes.len() - root.min(bytes.len() - 4) - 3);
                let word = match random() % 4 {
                    0 => root as u32,
                    1 => random() % bytes.len() as u32,
                    2 => random() & 0xffff,
                    _ => random(),
                };
                bytes[at..at + 4].copy_from_slice(&word.to_le_bytes());
            }
            let (laid, read) = sparse(&bytes);
            assert!(read <= METADATA_LIMIT + (1 << 20), "{read} bytes read");
            if !laid.is_empty() && replay(&laid).len() > 1 {
                walked += 1;
            }
            decoders_end(&bytes);
        }
        // Most damage leaves more than the root to walk.
        assert!(walked > 1000, "{walked}");
    }
}
