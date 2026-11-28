// SPDX-License-Identifier: AGPL-3.0-or-later
//! File removal in an owned folder that may be renamed while in use.

use std::ffi::OsStr;
use std::fs::File;
use std::io::{self, Read};
use std::path::{Component, Path};
use std::time::SystemTime;

#[cfg(unix)]
#[path = "anchored/unix.rs"]
mod platform;
#[cfg(windows)]
#[path = "anchored/windows.rs"]
mod platform;

pub struct Directory(File);

impl Directory {
    /// Opens an owned directory without following links in its final two
    /// components. Any ancestors above its parent must be trusted.
    pub fn open(path: &Path) -> io::Result<Self> {
        let name = path.file_name().ok_or(io::ErrorKind::InvalidInput)?;
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let parent = platform::open_parent(parent)?;
        platform::open_directory(&parent, leaf(name)?).map(Self)
    }

    /// Opens a child directory relative to this handle, without following links.
    pub fn open_child(&self, name: &OsStr) -> io::Result<Self> {
        platform::open_directory(&self.0, leaf(name)?).map(Self)
    }

    /// Removes an empty child directory relative to this handle, without following links.
    pub fn remove_empty_child(&self, name: &OsStr) -> io::Result<()> {
        platform::remove_empty_directory(&self.0, leaf(name)?)
    }

    /// Reads a regular file by name, without traversing a link.
    pub fn read_to_string(&self, name: &OsStr) -> io::Result<String> {
        let mut file = platform::open_file(&self.0, leaf(name)?, false)?;
        let mut text = String::new();
        file.read_to_string(&mut text)?;
        Ok(text)
    }

    /// Reads a regular file by name, without traversing a link (Windows: a
    /// reparse point), with its metadata from the same open handle.
    pub fn read(&self, name: &OsStr) -> io::Result<(Vec<u8>, std::fs::Metadata)> {
        let mut file = platform::open_file(&self.0, leaf(name)?, false)?;
        let meta = file.metadata()?;
        let mut bytes = Vec::with_capacity(meta.len() as usize);
        file.read_to_end(&mut bytes)?;
        Ok((bytes, meta))
    }

    /// Removes a named regular file, optionally only if its modification
    /// time is at or before the cutoff. A concurrently replaced name on
    /// Unix can lose the link itself, but unlinkat never follows it.
    pub fn remove_file(&self, name: &OsStr, cutoff: Option<SystemTime>) -> io::Result<()> {
        let name = leaf(name)?;
        let file = platform::open_file(&self.0, name, true)?;
        if let Some(cutoff) = cutoff
            && file.metadata()?.modified()? > cutoff
        {
            return Ok(());
        }
        platform::unlink(&self.0, name, &file)
    }
}

fn leaf(name: &OsStr) -> io::Result<&OsStr> {
    let mut components = Path::new(name).components();
    if !matches!(components.next(), Some(Component::Normal(part)) if part == name)
        || components.next().is_some()
    {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    Ok(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[cfg(unix)]
    use std::os::unix::fs::{symlink as symlink_dir, symlink as symlink_file};
    #[cfg(windows)]
    use std::os::windows::fs::{symlink_dir, symlink_file};

    /// Windows: symbolic links need Developer Mode or the symlink privilege;
    /// without it (ERROR_PRIVILEGE_NOT_HELD) the test is skipped, SKIP printed.
    fn link_or_skip(made: io::Result<()>) -> bool {
        match made {
            Ok(()) => true,
            #[cfg(windows)]
            Err(e) if e.raw_os_error() == Some(1314) => {
                eprintln!("SKIP: no right to make symbolic links ({e})");
                false
            }
            Err(e) => panic!("cannot make a symbolic link: {e}"),
        }
    }

    #[test]
    fn directory_replaced_after_open_cannot_redirect_removal() {
        let root = tempfile::tempdir().unwrap();
        let cache = root.path().join("cache");
        let moved = root.path().join("old-cache");
        let other = root.path().join("other");
        let name = OsStr::new("entry");
        fs::create_dir(&cache).unwrap();
        fs::create_dir(&other).unwrap();
        fs::write(cache.join(name), b"cached").unwrap();
        fs::write(other.join(name), b"keep").unwrap();

        let directory = Directory::open(&cache).unwrap();
        fs::rename(&cache, &moved).unwrap();
        if !link_or_skip(symlink_dir(&other, &cache)) {
            return;
        }

        directory.remove_file(name, None).unwrap();
        assert!(!moved.join(name).exists());
        assert_eq!(fs::read(other.join(name)).unwrap(), b"keep");
    }

    #[test]
    fn read_gives_a_regular_files_bytes_and_never_follows_a_link() {
        let root = tempfile::tempdir().unwrap();
        let cache = root.path().join("cache");
        let other = root.path().join("other");
        fs::create_dir(&cache).unwrap();
        fs::write(&other, b"elsewhere").unwrap();
        fs::write(cache.join("entry"), b"cached").unwrap();
        let directory = Directory::open(&cache).unwrap();
        let (bytes, meta) = directory.read(OsStr::new("entry")).unwrap();
        assert_eq!(bytes, b"cached");
        assert_eq!(meta.len(), 6);
        assert!(directory.read(OsStr::new("../other")).is_err());
        assert!(directory.read(OsStr::new("missing")).is_err());
        if !link_or_skip(symlink_file(&other, cache.join("link"))) {
            return;
        }
        assert!(directory.read(OsStr::new("link")).is_err());
    }

    #[test]
    fn file_replaced_by_symlink_after_listing_is_not_followed() {
        let root = tempfile::tempdir().unwrap();
        let cache = root.path().join("cache");
        let other = root.path().join("other");
        fs::create_dir(&cache).unwrap();
        fs::write(&other, b"keep").unwrap();
        let directory = Directory::open(&cache).unwrap();
        let path = cache.join("entry");
        fs::write(&path, b"cached").unwrap();
        let name = fs::read_dir(&cache)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .file_name();
        fs::remove_file(&path).unwrap();
        if !link_or_skip(symlink_file(&other, &path)) {
            return;
        }

        assert!(directory.remove_file(&name, None).is_err());
        assert!(fs::symlink_metadata(path).unwrap().is_symlink());
        assert_eq!(fs::read(other).unwrap(), b"keep");
    }

    #[cfg(windows)]
    #[test]
    fn removes_empty_staging_child() {
        let temp = tempfile::tempdir().unwrap();
        let updates = temp.path().join("updates");
        let name = OsStr::new("0.1.0-AA1234");
        let staged = updates.join(name);
        fs::create_dir_all(&staged).unwrap();
        let directory = Directory::open(&updates).unwrap();

        let child = directory.open_child(name).unwrap();
        let error = child
            .remove_file(OsStr::new("Rawtonic_0.1.0_x64-setup.exe"), None)
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        drop(child);
        directory.remove_empty_child(name).unwrap();

        assert!(!staged.try_exists().unwrap());
    }

    #[cfg(windows)]
    #[test]
    fn preserves_nonempty_staging_child() {
        let temp = tempfile::tempdir().unwrap();
        let updates = temp.path().join("updates");
        let name = OsStr::new("0.1.0-AA1234");
        let staged = updates.join(name);
        fs::create_dir_all(&staged).unwrap();
        let extra = staged.join("keep.txt");
        fs::write(&extra, b"keep").unwrap();
        let directory = Directory::open(&updates).unwrap();

        assert!(directory.remove_empty_child(name).is_err());
        assert_eq!(fs::read(extra).unwrap(), b"keep");
    }

    #[cfg(windows)]
    #[test]
    fn preserves_symlinked_staging_child_and_target() {
        let temp = tempfile::tempdir().unwrap();
        let updates = temp.path().join("updates");
        let target = temp.path().join("outside-updates");
        let name = OsStr::new("0.1.0-AA1234");
        let staged = updates.join(name);
        fs::create_dir(&updates).unwrap();
        fs::create_dir(&target).unwrap();
        if !link_or_skip(symlink_dir(&target, &staged)) {
            return;
        }
        let directory = Directory::open(&updates).unwrap();

        assert!(directory.remove_empty_child(name).is_err());
        assert!(fs::symlink_metadata(staged).unwrap().is_symlink());
        assert!(fs::metadata(target).unwrap().is_dir());
        eprintln!("Checked removal of a symlinked staging child");
    }
}
