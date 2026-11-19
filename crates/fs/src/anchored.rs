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
}
