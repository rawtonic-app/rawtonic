// SPDX-License-Identifier: AGPL-3.0-or-later

use std::ffi::{CString, OsStr};
use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::{ffi::OsStrExt, fs::OpenOptionsExt};
use std::path::Path;

pub(super) fn open_parent(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(path)
        .map_err(directory_error)
}

pub(super) fn open_directory(parent: &File, name: &OsStr) -> io::Result<File> {
    open_at(parent, name, libc::O_DIRECTORY).map_err(directory_error)
}

fn directory_error(error: io::Error) -> io::Error {
    if matches!(error.raw_os_error(), Some(libc::ELOOP | libc::ENOTDIR)) {
        return io::Error::new(io::ErrorKind::InvalidInput, error);
    }
    error
}

pub(super) fn open_file(parent: &File, name: &OsStr, _delete: bool) -> io::Result<File> {
    // A FIFO with a cache filename must never block opening the file.
    let file = open_at(parent, name, libc::O_NONBLOCK)?;
    if !file.metadata()?.is_file() {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    Ok(file)
}

fn open_at(parent: &File, name: &OsStr, flags: libc::c_int) -> io::Result<File> {
    let name = CString::new(name.as_bytes())?;
    // SAFETY: the directory descriptor is borrowed and the name is NUL-terminated.
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | flags,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a successful openat returns a new owned descriptor.
    Ok(unsafe { File::from_raw_fd(fd) })
}

pub(super) fn unlink(parent: &File, name: &OsStr, _file: &File) -> io::Result<()> {
    let name = CString::new(name.as_bytes())?;
    // SAFETY: name is a NUL-terminated filename relative to the borrowed directory.
    // With flags zero, unlinkat cannot follow a symlink or remove a directory.
    if unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), 0) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
