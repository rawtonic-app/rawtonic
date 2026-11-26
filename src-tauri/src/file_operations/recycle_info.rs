// SPDX-License-Identifier: AGPL-3.0-or-later
//! The Windows Recycle Bin keeps each recycled entry as two files in
//! `<drive>\$Recycle.Bin\<SID>\` (on FAT-family volumes such as exFAT
//! directly in `<drive>\$Recycle.Bin\`, WQA7b): the data `$R<id><ext>` and its info stub
//! `$I<id><ext>` (original path, size, deletion time). Moving the `$R` entry
//! out of the Bin (as a staged restore does) leaves the `$I` stub behind,
//! invisible to the user but never cleaned up. These pure helpers name the
//! stub of one `$R` entry and prove that a stub describes the restored
//! entry, so that only that item's own stub is ever removed.
//!
//! Stub layouts (little endian): version 1 (Vista, 7) is a header 1, the
//! size and a FILETIME (8 bytes each), then a NUL-padded 260-character
//! UTF-16 path (544 bytes in all); version 2 (8 and later) is a header 2,
//! size, FILETIME, a u32 character count including the NUL, then the path.
use std::path::{Path, PathBuf};

const V1_LEN: usize = 24 + 520;
/// No real stub comes close: a path holds at most 32,767 characters.
pub(super) const MAX_STUB_LEN: u64 = 28 + 2 * 32_768;

/// The `$I` info stub belonging to the Bin entry `data` (`$R…` directly in
/// a per-user folder of a `$Recycle.Bin`, or in the `$Recycle.Bin` itself
/// as on exFAT and FAT32), or `None` when `data` is no such entry.
pub(super) fn info_stub_for(data: &Path) -> Option<PathBuf> {
    let name = data.file_name()?.to_str()?;
    let rest = name.strip_prefix("$R")?;
    if rest.is_empty() || rest.contains(['\\', '/']) {
        return None;
    }
    let is_bin = |folder: &Path| {
        folder
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.eq_ignore_ascii_case("$Recycle.Bin"))
    };
    let folder = data.parent()?;
    if !is_bin(folder) && !folder.parent().is_some_and(is_bin) {
        return None;
    }
    Some(folder.join(format!("$I{rest}")))
}

/// What an info stub records: the entry's original size and path.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct StubRecord {
    pub size: u64,
    pub path: String,
}

fn le_u64(bytes: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(bytes.get(at..at + 8)?.try_into().ok()?))
}

/// Reads a version 1 or 2 stub; anything else (another version, a length
/// that does not match its layout, a path that is not UTF-16) is `None`.
pub(super) fn parse_stub(bytes: &[u8]) -> Option<StubRecord> {
    let size = le_u64(bytes, 8)?;
    let units: &[u8] = match le_u64(bytes, 0)? {
        1 if bytes.len() == V1_LEN => &bytes[24..],
        2 => {
            let count = u32::from_le_bytes(bytes.get(24..28)?.try_into().ok()?) as usize;
            if count == 0 || bytes.len() != 28 + 2 * count {
                return None;
            }
            &bytes[28..]
        }
        _ => return None,
    };
    let wide: Vec<u16> = units
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes(*pair))
        .collect();
    let end = wide.iter().position(|&unit| unit == 0)?;
    let path = String::from_utf16(&wide[..end]).ok()?;
    (!path.is_empty()).then_some(StubRecord { size, path })
}

/// Windows compares names without letter case; a stub whose spelling
/// differs in any other way is not taken for the entry's (it then stays,
/// which loses nothing).
fn same_windows_path(a: &str, b: &str) -> bool {
    let fold = |s: &str| s.chars().flat_map(char::to_uppercase).collect::<String>();
    fold(a) == fold(b)
}

/// Whether `bytes`, read from the stub of the entry just restored to
/// `original` (its plain `C:\…` spelling), is that entry's own stub: same
/// path and, for a file, its `size` (a folder's recorded size is the
/// Shell's sum of its contents, so a folder is matched by path alone).
pub(super) fn stub_describes(bytes: &[u8], original: &str, size: Option<u64>) -> bool {
    parse_stub(bytes).is_some_and(|record| {
        size.is_none_or(|size| record.size == size) && same_windows_path(&record.path, original)
    })
}

#[cfg(test)]
pub(super) fn v2_stub(size: u64, path: &str) -> Vec<u8> {
    let wide: Vec<u16> = path.encode_utf16().chain([0]).collect();
    let mut bytes = Vec::new();
    bytes.extend(2u64.to_le_bytes());
    bytes.extend(size.to_le_bytes());
    bytes.extend(0x01DB_0000_0000_0000u64.to_le_bytes());
    bytes.extend((wide.len() as u32).to_le_bytes());
    bytes.extend(wide.iter().flat_map(|unit| unit.to_le_bytes()));
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v1_stub(size: u64, path: &str) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend(1u64.to_le_bytes());
        bytes.extend(size.to_le_bytes());
        bytes.extend(0u64.to_le_bytes());
        let mut wide: Vec<u16> = path.encode_utf16().collect();
        wide.resize(260, 0);
        bytes.extend(wide.iter().flat_map(|unit| unit.to_le_bytes()));
        bytes
    }

    fn bin(name: &str) -> PathBuf {
        ["C:\\", "$Recycle.Bin", "S-1-5-21-1-2-3-1001", name]
            .iter()
            .collect()
    }

    #[test]
    fn the_stub_of_a_bin_entry_is_its_dollar_i_sibling() {
        assert_eq!(
            info_stub_for(&bin("$R19PIPG.NEF")),
            Some(bin("$I19PIPG.NEF"))
        );
        let lower: PathBuf = ["D:\\", "$RECYCLE.BIN", "S-1-5-18", "$RAB12CD"]
            .iter()
            .collect();
        assert_eq!(
            info_stub_for(&lower).unwrap().file_name().unwrap(),
            "$IAB12CD"
        );
    }

    /// exFAT and FAT32 volumes keep no per-user folders: WQA7b found
    /// `S:\$Recycle.Bin\$R…` and its `$I…` directly in the Bin.
    #[test]
    fn on_exfat_the_stub_sits_directly_in_the_bin() {
        let data: PathBuf = ["S:\\", "$Recycle.Bin", "$R7QK2ZD.srw"].iter().collect();
        let stub: PathBuf = ["S:\\", "$Recycle.Bin", "$I7QK2ZD.srw"].iter().collect();
        assert_eq!(info_stub_for(&data), Some(stub));
    }

    #[test]
    fn nothing_outside_a_bin_or_not_a_data_entry_has_a_stub() {
        for path in [
            bin("$I19PIPG.NEF"),
            bin("photo.NEF"),
            bin("$R"),
            ["C:\\", "Photos", "S-1", "$R19PIPG.NEF"].iter().collect(),
            ["C:\\", "Photos", "$R19PIPG.NEF"].iter().collect(),
            PathBuf::from("$R19PIPG.NEF"),
        ] {
            assert_eq!(info_stub_for(&path), None, "{}", path.display());
        }
    }

    /// The 80 and 86 byte stubs WQA7 found are version 2 stubs of these
    /// paths (notes: benchmark-results/wqa7-2026-11-26/REPORT.md).
    #[test]
    fn version_2_stubs_read_as_windows_writes_them() {
        let stub = v2_stub(25_994_901, r"C:\QA\wqa7\srcA\pair1.NEF");
        assert_eq!(stub.len(), 80);
        assert_eq!(v2_stub(1, r"C:\QA\wqa7\srcA\nikon_z6.NEF").len(), 86);
        assert_eq!(
            parse_stub(&stub),
            Some(StubRecord {
                size: 25_994_901,
                path: r"C:\QA\wqa7\srcA\pair1.NEF".into()
            })
        );
    }

    #[test]
    fn version_1_stubs_read_too() {
        let stub = v1_stub(42, r"C:\Photos\a.CR3");
        assert_eq!(stub.len(), 544);
        assert_eq!(parse_stub(&stub).unwrap().path, r"C:\Photos\a.CR3");
    }

    #[test]
    fn malformed_stubs_are_never_taken_for_an_entrys() {
        let good = v2_stub(42, r"C:\Photos\a.CR3");
        let mut other_version = good.clone();
        other_version[0] = 3;
        let mut long_count = good.clone();
        long_count[24] += 1;
        let mut no_nul = good.clone();
        let n = no_nul.len();
        no_nul[n - 2..].copy_from_slice(&u16::from(b'x').to_le_bytes());
        for bytes in [
            Vec::new(),
            good[..27].to_vec(),
            good[..good.len() - 1].to_vec(),
            [good.as_slice(), &[0, 0]].concat(),
            other_version,
            long_count,
            no_nul,
            v2_stub(42, ""),
            v1_stub(42, r"C:\Photos\a.CR3")[..543].to_vec(),
        ] {
            assert_eq!(parse_stub(&bytes), None, "{bytes:?}");
        }
    }

    #[test]
    fn only_the_restored_entrys_own_stub_matches() {
        let stub = v2_stub(42, r"C:\Photos\Shoot\a.CR3");
        let a = r"C:\Photos\Shoot\a.CR3";
        assert!(stub_describes(&stub, a, Some(42)));
        assert!(stub_describes(&stub, r"c:\photos\shoot\A.cr3", Some(42)));
        assert!(stub_describes(&stub, a, None), "a folder: by path");
        assert!(!stub_describes(&stub, a, Some(43)));
        assert!(!stub_describes(&stub, r"C:\Photos\Shoot\b.CR3", Some(42)));
        assert!(!stub_describes(&stub, r"C:\Photos\Shoot\b.CR3", None));
        assert!(!stub_describes(&stub, r"C:\Photos\a.CR3", Some(42)));
        assert!(!stub_describes(
            &stub,
            r"\\?\C:\Photos\Shoot\a.CR3",
            Some(42)
        ));
        assert!(!stub_describes(b"not a stub", a, None));
    }
}
