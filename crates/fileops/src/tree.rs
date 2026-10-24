//! Folders moved to another volume (ADR 0016): everything below a folder
//! is listed without following links, copied into the staging folder and
//! verified file by file, and listed again before the original folder goes
//! to the Trash, so that nothing in it is left out or changed unseen.
use crate::volume::{self, Stamp, Tally, same_content, sync_folder};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

/// One file or folder below a moved folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TreeEntry {
    /// Below the moved folder.
    pub path: PathBuf,
    /// A folder; otherwise a file.
    #[serde(default)]
    pub folder: bool,
    /// A file: its source's size and modification time (when its copy was
    /// verified).
    #[serde(default)]
    pub stamp: Option<Stamp>,
    /// A file: its verified copy's.
    #[serde(default)]
    pub copy_stamp: Option<Stamp>,
}

/// Every entry below the real folder `root`, each folder before what is in
/// it. A link or special file anywhere in it refuses the folder: a copy
/// move takes files and folders only.
pub(crate) fn list(root: &Path) -> Result<Vec<TreeEntry>> {
    let meta =
        fs::symlink_metadata(root).with_context(|| format!("{} was not found", root.display()))?;
    ensure!(meta.is_dir(), "{} is not a folder", root.display());
    let skip_apple_double = apple_double_volume(root);
    let mut entries = Vec::new();
    walk(root, Path::new(""), skip_apple_double, &mut entries)?;
    Ok(entries)
}

fn walk(
    root: &Path,
    below: &Path,
    skip_apple_double: bool,
    entries: &mut Vec<TreeEntry>,
) -> Result<()> {
    let dir = root.join(below);
    let mut names: Vec<_> = fs::read_dir(&dir)
        .with_context(|| format!("Cannot read the folder {}", dir.display()))?
        .map(|entry| entry.map(|e| e.file_name()))
        .collect::<std::io::Result<_>>()?;
    names.sort();
    for name in &names {
        let path = below.join(name);
        let meta = fs::symlink_metadata(root.join(&path))?;
        if skip_apple_double && is_companion(&root.join(&path), &meta, &names) {
            continue;
        }
        if meta.is_dir() {
            // Another drive mounted inside would escape the checks of the
            // folder's own drive (network, permissions, free space).
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                ensure!(
                    meta.dev() == fs::symlink_metadata(root)?.dev(),
                    "{} is another drive mounted inside the folder; a folder holding one \
                     cannot be moved to another drive",
                    root.join(&path).display()
                );
            }
            entries.push(TreeEntry {
                path: path.clone(),
                folder: true,
                stamp: None,
                copy_stamp: None,
            });
            walk(root, &path, skip_apple_double, entries)?;
        } else if meta.is_file() {
            entries.push(TreeEntry {
                path,
                folder: false,
                stamp: Some(Stamp::from_meta(&meta)),
                copy_stamp: None,
            });
        } else {
            bail!(
                "{} is a link or special file; a folder with one cannot be moved to another drive",
                root.join(&path).display()
            );
        }
    }
    Ok(())
}

/// Whether the entry at `path` (with `meta`, among its folder's `names`)
/// is the AppleDouble companion `._name` in which macOS keeps the extended
/// attributes of the file `name` beside it on FAT and exFAT (it renames
/// and removes it with its file): a file named so, beside such a file, that
/// starts with the format's magic number. Anything else is listed.
fn is_companion(path: &Path, meta: &fs::Metadata, names: &[std::ffi::OsString]) -> bool {
    use std::io::Read;
    let of = path
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.strip_prefix("._"));
    let mut magic = [0u8; 4];
    meta.is_file()
        && of.is_some_and(|of| names.iter().any(|m| m.to_str() == Some(of)))
        && fs::File::open(path).is_ok_and(|mut f| f.read_exact(&mut magic).is_ok())
        && magic == [0x00, 0x05, 0x16, 0x07]
}

/// Volumes on which macOS stores extended attributes in `._name` files.
#[cfg(target_vendor = "apple")]
fn apple_double_volume(root: &Path) -> bool {
    volume::volume_info(root).is_ok_and(|v| {
        matches!(
            v.file_system.as_str(),
            "msdos" | "msdosfs" | "exfat" | "smbfs" | "webdav"
        )
    })
}

#[cfg(not(target_vendor = "apple"))]
fn apple_double_volume(_root: &Path) -> bool {
    false
}

/// The bytes and the number of files below `root`.
pub(crate) fn size(root: &Path) -> Result<(u64, usize)> {
    let files: Vec<_> = list(root)?.into_iter().filter(|e| !e.folder).collect();
    let bytes = files.iter().filter_map(|e| e.stamp).map(|s| s.len).sum();
    Ok((bytes, files.len()))
}

/// The Unix permission bits of everything below `root`, when its volume
/// supplies them (none for FAT-family volumes): the ceilings its copies
/// keep across undo and redo.
#[cfg(unix)]
pub(crate) fn modes(root: &Path) -> Result<BTreeMap<PathBuf, u32>> {
    use std::os::unix::fs::PermissionsExt;
    if volume::unix_mode(root)?.is_none() {
        return Ok(BTreeMap::new());
    }
    list(root)?
        .into_iter()
        .map(|e| {
            let mode = fs::symlink_metadata(root.join(&e.path))?
                .permissions()
                .mode();
            Ok((e.path, mode & 0o7777))
        })
        .collect()
}

/// Copies the folder `source` to the new folder `staged` with everything
/// in it, each file as [`volume::copy_verified`] copies one (synced, read
/// back from the drive and compared), then syncs the folders and lists the
/// source again: a file added, removed or changed meanwhile fails the
/// copy. Folders are made with their source's permission bits (bounded by
/// `modes` and the umask), keeping the owner's access so that the copy
/// can be managed; files get theirs as a single copy does. Returns the
/// entries with both stamps. On failure what was made is left for the
/// caller's staging cleanup.
pub(crate) fn copy_verified(
    source: &Path,
    staged: &Path,
    root_mode: Option<u32>,
    modes: &BTreeMap<PathBuf, u32>,
    tally: &mut Tally,
) -> Result<Vec<TreeEntry>> {
    let mut entries = list(source)?;
    let parent = staged.parent().context("The copy has no folder")?;
    #[cfg(unix)]
    let posix = (
        volume::unix_mode(source)?.is_some(),
        volume::unix_mode(parent)?.is_some(),
    );
    #[cfg(not(unix))]
    let posix = (false, false);
    make_folder(source, staged, root_mode, posix.0)?;
    for entry in &mut entries {
        let (from, to) = (source.join(&entry.path), staged.join(&entry.path));
        let recorded = modes.get(&entry.path).copied();
        if entry.folder {
            make_folder(&from, &to, recorded, posix.0)?;
            continue;
        }
        let (stamp, copy_stamp) = volume::copy_file_verified(&from, &to, recorded, posix, tally)?;
        ensure!(
            Some(stamp) == entry.stamp,
            "{} changed while it was copied; nothing was moved",
            from.display()
        );
        entry.copy_stamp = Some(copy_stamp);
    }
    for entry in entries.iter().rev().filter(|e| e.folder) {
        sync_folder(&staged.join(&entry.path))?;
    }
    sync_folder(staged)?;
    sync_folder(parent)?;
    ensure!(
        unchanged(source, &entries)?,
        "{} changed while it was copied; nothing was moved",
        source.display()
    );
    Ok(entries)
}

/// Makes the folder `to` (never an existing one) for the folder `from`.
fn make_folder(from: &Path, to: &Path, recorded: Option<u32>, posix_source: bool) -> Result<()> {
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        let live = if posix_source {
            Some(fs::symlink_metadata(from)?.permissions().mode())
        } else {
            None
        };
        builder.mode(folder_mode(recorded, live));
    }
    #[cfg(not(unix))]
    let _ = (from, recorded, posix_source);
    builder
        .create(to)
        .with_context(|| format!("Cannot create the folder {}", to.display()))
}

/// A copied folder's mode before the umask: its recorded and live modes
/// as ceilings, without special bits, always with the owner's access.
#[cfg(unix)]
fn folder_mode(recorded: Option<u32>, live: Option<u32>) -> u32 {
    (recorded.unwrap_or(0o777) & live.unwrap_or(0o777) & 0o777) | 0o700
}

/// Whether the folder `root` holds exactly `entries` now, each file with
/// its recorded source stamp.
pub(crate) fn unchanged(root: &Path, entries: &[TreeEntry]) -> Result<bool> {
    let now = list(root)?;
    Ok(now.len() == entries.len()
        && now
            .iter()
            .zip(entries)
            .all(|(a, b)| a.path == b.path && a.folder == b.folder && a.stamp == b.stamp))
}

/// Whether every entry's verified copy is still in the real folder `root`
/// as it was verified (files by size and modification time). Files added
/// there since do not matter.
pub(crate) fn copy_unchanged(root: &Path, entries: &[TreeEntry]) -> bool {
    let real_folder = |p: &Path| fs::symlink_metadata(p).is_ok_and(|m| m.is_dir());
    real_folder(root)
        && entries.iter().all(|entry| {
            let path = root.join(&entry.path);
            if entry.folder {
                real_folder(&path)
            } else {
                fs::symlink_metadata(&path)
                    .is_ok_and(|m| m.is_file() && Some(Stamp::from_meta(&m)) == entry.copy_stamp)
            }
        })
}

/// Whether the folders `a` and `b` hold the same files and folders with
/// the same bytes.
pub(crate) fn same(a: &Path, b: &Path) -> Result<bool> {
    let (x, y) = (list(a)?, list(b)?);
    if x.len() != y.len()
        || x.iter()
            .zip(&y)
            .any(|(p, q)| p.path != q.path || p.folder != q.folder)
    {
        return Ok(false);
    }
    for entry in x.iter().filter(|e| !e.folder) {
        if !same_content(&a.join(&entry.path), &b.join(&entry.path), false, None)? {
            return Ok(false);
        }
    }
    Ok(true)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn only_real_appledouble_companions_of_a_file_count_as_companions() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let magic = [0x00, 0x05, 0x16, 0x07, 0, 2, 0, 0];
        fs::write(dir.join("A.RAF"), b"raw").unwrap();
        fs::write(dir.join("._A.RAF"), magic).unwrap();
        fs::write(dir.join("._B.RAF"), magic).unwrap();
        fs::write(dir.join("C.RAF"), b"raw").unwrap();
        fs::write(dir.join("._C.RAF"), b"a photo of its own").unwrap();
        fs::create_dir(dir.join("Shoot")).unwrap();
        fs::create_dir(dir.join("._Shoot")).unwrap();
        fs::write(dir.join("._Shoot/D.RAF"), b"raw").unwrap();
        let names: Vec<_> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        let companion = |name: &str| {
            let path = dir.join(name);
            is_companion(&path, &fs::symlink_metadata(&path).unwrap(), &names)
        };
        assert!(companion("._A.RAF"));
        assert!(!companion("._B.RAF"), "no file beside it");
        assert!(!companion("._C.RAF"), "not AppleDouble");
        assert!(!companion("._Shoot"), "a folder");
        assert!(!companion("A.RAF"));
    }

    /// A folder with another drive mounted in it, when the environment
    /// names one (`RAWTONIC_TEST_FOLDER_WITH_MOUNT`, e.g. the folder a
    /// desktop mounts drives in, with a drive attached): refused.
    #[test]
    fn a_folder_with_another_drive_mounted_inside_is_refused() {
        let Some(folder) = std::env::var_os("RAWTONIC_TEST_FOLDER_WITH_MOUNT") else {
            eprintln!("SKIP: RAWTONIC_TEST_FOLDER_WITH_MOUNT not set");
            return;
        };
        let error = list(Path::new(&folder)).unwrap_err();
        assert!(format!("{error:#}").contains("mounted inside"), "{error:#}");
    }

    #[test]
    fn folder_modes_keep_the_owners_access_and_obey_their_ceilings() {
        for (recorded, live, expected) in [
            (None, None, 0o777),
            (Some(0o750), None, 0o750),
            (None, Some(0o555), 0o755),
            (Some(0o2775), Some(0o755), 0o755),
            (Some(0o700), Some(0o777), 0o700),
        ] {
            assert_eq!(folder_mode(recorded, live), expected);
        }
    }
}
