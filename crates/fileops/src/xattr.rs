//! Extended attributes a copy to another volume keeps (ADR 0016): on Linux
//! the user attributes (`user.*`); on macOS every attribute a program may
//! set — Finder tags and comments, resource forks, download origins and
//! the like — except those the system keeps itself. Attributes are copied
//! with the file, read back from the copy and compared; a volume that
//! cannot keep them refuses the move before anything is published, so the
//! source stays where it is. Other systems keep none.
use std::{fs::File, io, path::Path};

/// One attribute: its name (without the trailing NUL) and value.
pub(crate) type Attribute = (Vec<u8>, Vec<u8>);

/// Whether a copy keeps the attribute `name`.
pub(crate) fn kept(name: &[u8]) -> bool {
    #[cfg(target_os = "linux")]
    {
        name.starts_with(b"user.")
    }
    #[cfg(target_vendor = "apple")]
    {
        // The system's own: where a file came from (it sets one on every
        // file a program writes), protection and system attributes.
        !(name == b"com.apple.provenance"
            || name == b"com.apple.rootless"
            || name.starts_with(b"com.apple.system."))
    }
    #[cfg(not(any(target_os = "linux", target_vendor = "apple")))]
    {
        let _ = name;
        false
    }
}

/// The attributes of the open file (or folder) `file` that a copy keeps,
/// sorted by name. None where its volume has no extended attributes.
pub(crate) fn read(file: &File) -> io::Result<Vec<Attribute>> {
    let mut attributes = Vec::new();
    for name in sys::list(file)?
        .split(|&b| b == 0)
        .filter(|n| !n.is_empty())
    {
        if !kept(name) {
            continue;
        }
        match sys::get(file, name) {
            Ok(value) => attributes.push((name.to_vec(), value)),
            // Removed since it was listed.
            Err(e) if sys::is_missing(&e) => {}
            Err(e) => return Err(e),
        }
    }
    attributes.sort();
    Ok(attributes)
}

/// The kept attributes of the file or folder at `path`.
pub(crate) fn read_path(path: &Path) -> io::Result<Vec<Attribute>> {
    read(&File::open(path)?)
}

/// Gives the open file (or folder) `file` the `attributes`.
pub(crate) fn write(file: &File, attributes: &[Attribute]) -> io::Result<()> {
    attributes
        .iter()
        .try_for_each(|(name, value)| sys::set(file, name, value))
}

/// Why a move to the drive of `folder` is refused: `source` has
/// `attributes`, which that drive cannot keep (`error`).
pub(crate) fn cannot_keep(
    folder: &Path,
    source: &Path,
    attributes: &[Attribute],
    error: &io::Error,
) -> anyhow::Error {
    let drive = crate::volume::volume_info(folder)
        .map(|v| v.name)
        .unwrap_or_else(|_| folder.display().to_string());
    let names: Vec<_> = attributes
        .iter()
        .map(|(n, _)| String::from_utf8_lossy(n).into_owned())
        .collect();
    anyhow::anyhow!(
        "{drive} cannot keep extended attributes ({error}), and {} has {}; \
         nothing was moved, the original stays where it is",
        source.display(),
        names.join(", ")
    )
}

/// Whether the copy at `copy` has exactly `attributes` (of those a copy
/// keeps), read back from it.
pub(crate) fn copied(copy: &Path, attributes: &[Attribute]) -> io::Result<bool> {
    Ok(read_path(copy)? == attributes)
}

/// Whether the files (or folders) `a` and `b` have the same kept
/// attributes now.
pub(crate) fn same(a: &Path, b: &Path) -> bool {
    match (read_path(a), read_path(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
}

/// Whether a file in the folder `folder` (one only this operation writes
/// in) can be given an attribute a copy keeps. The probe file is removed.
pub(crate) fn probe(folder: &Path) -> io::Result<()> {
    let path = folder.join(PROBE_FILE);
    let result = File::create_new(&path).and_then(|file| sys::set(&file, PROBE_NAME, b"1"));
    let _ = std::fs::remove_file(&path);
    result
}

pub(crate) const PROBE_FILE: &str = ".rawtonic-probe-attributes";

#[cfg(target_vendor = "apple")]
const PROBE_NAME: &[u8] = b"cc.rawtonic.probe";
#[cfg(not(target_vendor = "apple"))]
const PROBE_NAME: &[u8] = b"user.rawtonic.probe";

#[cfg(any(target_os = "linux", target_vendor = "apple"))]
mod sys {
    use std::{ffi::CString, fs::File, io, os::fd::AsRawFd};

    fn c_name(name: &[u8]) -> io::Result<CString> {
        CString::new(name).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))
    }

    /// Reads a list or value by size first; grows when it grew meanwhile.
    fn sized(mut call: impl FnMut(*mut libc::c_void, usize) -> isize) -> io::Result<Vec<u8>> {
        loop {
            let size = call(std::ptr::null_mut(), 0);
            if size < 0 {
                return Err(io::Error::last_os_error());
            }
            let mut buffer = vec![0u8; size as usize];
            let got = call(buffer.as_mut_ptr().cast(), buffer.len());
            if got >= 0 {
                buffer.truncate(got as usize);
                return Ok(buffer);
            }
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ERANGE) {
                return Err(error);
            }
        }
    }

    /// The names, each NUL-terminated; empty where the volume has none.
    pub fn list(file: &File) -> io::Result<Vec<u8>> {
        let fd = file.as_raw_fd();
        // SAFETY: a valid descriptor; the buffer is valid for `size` bytes
        // (or null with size 0, which asks for the size).
        let result = sized(|buffer, size| unsafe {
            #[cfg(target_os = "linux")]
            {
                libc::flistxattr(fd, buffer.cast(), size)
            }
            #[cfg(target_vendor = "apple")]
            {
                libc::flistxattr(fd, buffer.cast(), size, 0)
            }
        });
        match result {
            Err(e) if e.raw_os_error() == Some(libc::ENOTSUP) => Ok(Vec::new()),
            other => other,
        }
    }

    pub fn get(file: &File, name: &[u8]) -> io::Result<Vec<u8>> {
        let (fd, name) = (file.as_raw_fd(), c_name(name)?);
        // SAFETY: as in `list`; the name is NUL-terminated.
        sized(|buffer, size| unsafe {
            #[cfg(target_os = "linux")]
            {
                libc::fgetxattr(fd, name.as_ptr(), buffer, size)
            }
            #[cfg(target_vendor = "apple")]
            {
                libc::fgetxattr(fd, name.as_ptr(), buffer, size, 0, 0)
            }
        })
    }

    pub fn set(file: &File, name: &[u8], value: &[u8]) -> io::Result<()> {
        let (fd, name) = (file.as_raw_fd(), c_name(name)?);
        // SAFETY: a valid descriptor, a NUL-terminated name and a value
        // valid for its length.
        let result = unsafe {
            #[cfg(target_os = "linux")]
            {
                libc::fsetxattr(fd, name.as_ptr(), value.as_ptr().cast(), value.len(), 0)
            }
            #[cfg(target_vendor = "apple")]
            {
                libc::fsetxattr(fd, name.as_ptr(), value.as_ptr().cast(), value.len(), 0, 0)
            }
        };
        if result == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    pub fn is_missing(error: &io::Error) -> bool {
        #[cfg(target_os = "linux")]
        let missing = libc::ENODATA;
        #[cfg(target_vendor = "apple")]
        let missing = libc::ENOATTR;
        error.raw_os_error() == Some(missing)
    }
}

#[cfg(not(any(target_os = "linux", target_vendor = "apple")))]
mod sys {
    use std::{fs::File, io};
    pub fn list(_file: &File) -> io::Result<Vec<u8>> {
        Ok(Vec::new())
    }
    pub fn get(_file: &File, _name: &[u8]) -> io::Result<Vec<u8>> {
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }
    pub fn set(_file: &File, _name: &[u8], _value: &[u8]) -> io::Result<()> {
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }
    pub fn is_missing(_error: &io::Error) -> bool {
        false
    }
}
