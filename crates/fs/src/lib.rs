//! File-safety kernel shared by the core and engine crates: atomic
//! replacement and no-clobber publication of files, renaming without
//! replacing, temporary and staging files beside the user's files. It never
//! deletes or replaces a file other than the one a caller names to be
//! replaced. It also detects volumes ([`volume`]).
#![warn(clippy::undocumented_unsafe_blocks)]
#[cfg(any(unix, windows))]
pub mod anchored;
pub mod no_clobber;
mod rename;
pub mod staging;
pub mod temp;
pub mod volume;

#[cfg(all(unix, any(test, feature = "test-support")))]
pub use rename::test_hooks;
pub use rename::{
    Claim, ClaimKind, Renamed, claim_left, rename_no_clobber, rename_no_clobber_noting,
};

use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

/// Whether this metadata describes a file evicted by macOS File Provider.
#[cfg(target_os = "macos")]
pub fn is_cloud_placeholder_metadata(metadata: &fs::Metadata) -> bool {
    use std::os::macos::fs::MetadataExt;

    const SF_DATALESS: u32 = 0x4000_0000;
    metadata.st_flags() & SF_DATALESS != 0
}

#[cfg(not(target_os = "macos"))]
pub fn is_cloud_placeholder_metadata(_metadata: &fs::Metadata) -> bool {
    false
}

/// Whether a path points to an evicted cloud file, without opening it.
#[cfg(target_os = "macos")]
pub fn is_cloud_placeholder(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|metadata| is_cloud_placeholder_metadata(&metadata))
}

#[cfg(not(target_os = "macos"))]
pub fn is_cloud_placeholder(_path: &Path) -> bool {
    false
}

/// Lowercase hex of `bytes` (a digest), two digits per byte.
pub fn hex(bytes: impl AsRef<[u8]>) -> String {
    use std::fmt::Write as _;
    let bytes = bytes.as_ref();
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(text, "{byte:02x}");
    }
    text
}

/// Lowercase hex SHA-256 of `bytes`.
pub fn sha256_hex(bytes: impl AsRef<[u8]>) -> String {
    hex(Sha256::digest(bytes.as_ref()))
}

/// Lowercase hex SHA-256 of everything `reader` holds, read in 1 MiB
/// pieces.
pub fn sha256_hex_read(reader: &mut impl Read) -> io::Result<String> {
    sha256_hex_read_while(reader, || true).map(|hex| hex.unwrap_or_default())
}

/// [`sha256_hex_read`], asking `keep_going` before each piece; when it says
/// no, the read stops and the result is `None`.
pub fn sha256_hex_read_while(
    reader: &mut impl Read,
    mut keep_going: impl FnMut() -> bool,
) -> io::Result<Option<String>> {
    let mut hash = Sha256::new();
    let mut buffer = vec![0; 1024 * 1024];
    loop {
        if !keep_going() {
            return Ok(None);
        }
        let n = reader.read(&mut buffer)?;
        if n == 0 {
            return Ok(Some(hex(hash.finalize())));
        }
        hash.update(&buffer[..n]);
    }
}

/// `path` resolved to its real, absolute form (symlinks and `..` followed),
/// as `std::fs::canonicalize` does — but on Windows in the plain form
/// (`C:\…`, `\\server\share\…`) wherever that names the same file.
/// Windows' own canonical form is verbatim (`\\?\C:\…`): users should not
/// see it, it differs from the same path picked in a dialog or listed from a
/// plain folder, and a `/` appended to it (the frontend joins with `/`) makes
/// it invalid (os error 123), because verbatim paths are not normalised. The
/// verbatim form is kept only where a name in it has no plain spelling
/// (reserved names such as `NUL`, trailing dots or spaces), never for length
/// alone: a library and everything below it keep one spelling, and long
/// plain paths work (`std::fs` and [`wide_path`] extend them). Everywhere
/// else the same as the standard library; clippy refuses that one
/// (`clippy.toml`).
pub fn canonicalize(path: impl AsRef<Path>) -> io::Result<PathBuf> {
    #[allow(clippy::disallowed_methods)]
    let real = fs::canonicalize(path)?;
    Ok(simplified(&real))
}

/// `path` without its verbatim prefix (`\\?\C:\…` → `C:\…`, `\\?\UNC\srv\…`
/// → `\\srv\…`) when every name in it has a plain spelling (see
/// [`canonicalize`]); unchanged otherwise and off Windows. For paths read
/// back from older catalogs and from the system.
pub fn simplified(path: &Path) -> PathBuf {
    #[cfg(windows)]
    if let Some(plain) = plain_form(path) {
        return plain;
    }
    path.to_path_buf()
}

#[cfg(windows)]
fn plain_form(path: &Path) -> Option<PathBuf> {
    use std::path::{Component, Prefix};
    let mut components = path.components();
    let Some(Component::Prefix(prefix)) = components.next() else {
        return None;
    };
    let mut plain = match prefix.kind() {
        Prefix::VerbatimDisk(letter) => PathBuf::from(format!("{}:\\", letter as char)),
        Prefix::VerbatimUNC(server, share) => {
            let mut root = std::ffi::OsString::from(r"\\");
            root.push(server);
            root.push(r"\");
            root.push(share);
            root.push(r"\");
            PathBuf::from(root)
        }
        _ => return None,
    };
    for component in components {
        match component {
            Component::RootDir => {}
            Component::Normal(name) if plain_name(&name.to_string_lossy()) => plain.push(name),
            _ => return None,
        }
    }
    Some(plain)
}

/// Whether Win32 reads `name` as itself in a plain path (it drops trailing
/// dots and spaces, maps device names and refuses some characters).
#[cfg(windows)]
fn plain_name(name: &str) -> bool {
    const DEVICES: [&str; 22] = [
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    let stem = name.split('.').next().unwrap_or(name).trim_end();
    !name.is_empty()
        && !name.ends_with(['.', ' '])
        && !name.chars().any(|c| c < ' ' || "<>:\"/\\|?*".contains(c))
        && !DEVICES
            .iter()
            .any(|device| stem.eq_ignore_ascii_case(device))
}

/// The last component of `path` as the volume stores it, where that can
/// differ from the spelling asked for (Windows: `FindFirstFileW` on a
/// case-insensitive folder; `a.xmp` finds `a.XMP`). `None` off Windows,
/// for a wildcard in the name, or when nothing is there.
pub fn stored_name(path: &Path) -> Option<std::ffi::OsString> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;
        #[repr(C)]
        struct FindData {
            attributes: u32,
            times: [u32; 6],
            size: [u32; 2],
            reserved: [u32; 2],
            name: [u16; 260],
            short_name: [u16; 14],
        }
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn FindFirstFileW(name: *const u16, data: *mut FindData) -> isize;
            fn FindClose(handle: isize) -> i32;
        }
        let text = path.file_name()?.to_string_lossy();
        if text.contains(['*', '?']) {
            return None;
        }
        let wide = wide_path(path).ok()?;
        // SAFETY: an all-zero FindData (integers only) is a valid value.
        let mut data: FindData = unsafe { std::mem::zeroed() };
        // SAFETY: `wide` is NUL-terminated and `data` a valid out pointer.
        let handle = unsafe { FindFirstFileW(wide.as_ptr(), &mut data) };
        if handle == -1 {
            return None;
        }
        // SAFETY: a handle FindFirstFileW returned, closed once.
        unsafe { FindClose(handle) };
        let len = data.name.iter().position(|&c| c == 0)?;
        Some(std::ffi::OsString::from_wide(&data.name[..len]))
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        None
    }
}

/// [`canonicalize`] as a method, in place of `Path::canonicalize`.
pub trait Canonical {
    fn canonical(&self) -> io::Result<PathBuf>;
}

impl<P: AsRef<Path> + ?Sized> Canonical for P {
    fn canonical(&self) -> io::Result<PathBuf> {
        canonicalize(self)
    }
}

/// Syncs a folder's entries (a new, renamed or removed name in it) to the
/// drive, where folders can be synced (Unix); elsewhere nothing to do.
pub fn sync_dir(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    fs::File::open(dir)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = dir;
    Ok(())
}

/// `path` as a NUL-terminated string for a system call.
#[cfg(unix)]
pub fn c_path(path: &Path) -> io::Result<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "Path contains a NUL byte"))
}

/// `path` as the NUL-terminated wide string the Windows API takes; a NUL in
/// it is refused. A long path (from 248 units, as the standard library
/// counts) gets the extended-length form (`\\?\C:\…`, `\\?\UNC\server\…`)
/// after normalising it, as `std::fs` does for its own calls: without it the
/// system stops at 260 characters (os error 3), and our paths are plain
/// (see [`canonicalize`]). Verbatim and device paths are passed as they are.
#[cfg(windows)]
pub fn wide_path(path: &Path) -> io::Result<Vec<u16>> {
    use std::os::windows::ffi::OsStrExt;
    let mut text: Vec<u16> = path.as_os_str().encode_wide().collect();
    if text.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} contains a NUL byte", path.display()),
        ));
    }
    let wide = |s: &str| s.encode_utf16().collect::<Vec<u16>>();
    if text.len() >= 248 && !text.starts_with(&wide(r"\\?\")) && !text.starts_with(&wide(r"\\.\")) {
        let full: Vec<u16> = std::path::absolute(path)?
            .as_os_str()
            .encode_wide()
            .collect();
        text = if let Some(share) = full.strip_prefix(&wide(r"\\")[..]) {
            [wide(r"\\?\UNC\"), share.to_vec()].concat()
        } else {
            [wide(r"\\?\"), full].concat()
        };
    }
    Ok(text.into_iter().chain(Some(0)).collect())
}

/// Same-directory temporary file, fsync, atomic replace. Reject symlink targets.
/// A replaced file keeps its permissions (the temporary file's are private).
///
/// Windows: a file marked read-only is refused (the system would refuse
/// the replace too, after the work). A file another program holds open
/// without delete sharing (a virus scanner or the search indexer reading
/// it, another editor) cannot be replaced while it does; the replace is
/// retried for a while and then fails, naming the cause, with the old file
/// untouched.
pub fn atomic_write(path: &Path, bytes: impl AsRef<[u8]>) -> Result<()> {
    let mut writer = AtomicWriter::new(path)?;
    writer.write_all(bytes.as_ref())?;
    writer.commit()
}

/// [`atomic_write`] fed piece by piece (a download streamed to disk): the
/// bytes go to a temporary file beside `path` ([`temp::file_for`]); only
/// [`AtomicWriter::commit`] replaces `path`. Dropped without a commit (an
/// error, a cut connection, a wrong checksum), the temporary file is
/// removed and `path` is left as it was.
pub struct AtomicWriter {
    file: tempfile::NamedTempFile,
    path: std::path::PathBuf,
    replaced: Option<fs::Metadata>,
}

impl AtomicWriter {
    /// Refuses a symlink or non-file at `path` (and on Windows a read-only
    /// file) before anything is written.
    pub fn new(path: &Path) -> Result<Self> {
        path.parent().context("File needs a parent directory")?;
        let replaced = fs::symlink_metadata(path).ok();
        if let Some(meta) = &replaced {
            ensure!(
                meta.is_file() && !meta.file_type().is_symlink(),
                "Refusing to replace a symlink or non-file"
            );
            #[cfg(windows)]
            ensure!(
                !meta.permissions().readonly(),
                "{} is read-only; it is not replaced",
                path.display()
            );
        }
        Ok(Self {
            file: temp::file_for(path)?,
            path: path.to_path_buf(),
            replaced,
        })
    }

    pub fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.file.write_all(bytes)
    }

    /// Syncs the temporary file, keeps a replaced file's permissions and
    /// renames it over `path`, then syncs the folder.
    pub fn commit(self) -> Result<()> {
        let Self {
            file,
            path,
            replaced,
        } = self;
        file.as_file().sync_all()?;
        if let Some(meta) = &replaced {
            file.as_file().set_permissions(meta.permissions())?;
        }
        replace(file, &path)?;
        sync_dir(path.parent().context("File needs a parent directory")?)?;
        Ok(())
    }
}

/// The bytes at `path`, or None if it does not exist. Other read errors
/// must not be mistaken for an empty slot when publishing a rewrite.
pub fn read_optional(path: &Path) -> io::Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Publishes a rewrite only if the file still holds `expected` (None
/// means the file was absent). Hold the writers' lock across this call.
/// For external writers, the final check and rename have the same
/// portable filesystem limitation as [`replace_existing`].
pub fn write_if_unchanged(path: &Path, expected: Option<&[u8]>, bytes: &[u8]) -> Result<bool> {
    let mut temp = temp::file_for(path)?;
    temp.write_all(bytes)?;
    temp.as_file().sync_all()?;
    if let Some(expected) = expected {
        replace_matching(temp, path, |path| {
            let file = match fs::File::open(path) {
                Ok(file) => file,
                Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
                Err(e) => return Err(e.into()),
            };
            // A concurrent replacement might be much larger than the read.
            let mut current = Vec::new();
            file.take(expected.len() as u64 + 1)
                .read_to_end(&mut current)?;
            Ok(current == expected)
        })
    } else {
        match no_clobber::persist_atomic(temp, path) {
            Ok(()) => {
                sync_dir(
                    path.parent()
                        .filter(|p| !p.as_os_str().is_empty())
                        .unwrap_or(Path::new(".")),
                )?;
                Ok(true)
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(false),
            Err(e) => Err(e.into()),
        }
    }
}

/// Publishes the finished temporary file `temp` (created beside `path`,
/// written and synced by the caller) over the existing regular file at
/// `path` in one atomic rename, after `still_expected` confirmed that
/// `path` is the file the caller means to replace (e.g. by its digest).
/// Right before each rename attempt the file at `path` must still be that
/// same file, unchanged since it was confirmed (same size, modification
/// time and, on Unix, device and inode): a file put there meanwhile (a
/// sync client's copy) is not replaced. The new file keeps the replaced
/// one's permissions. Refuses a missing file, a symlink or a non-file
/// (nothing is created), and on Windows a read-only file; on any refusal or
/// failure `temp` is removed and `path` is untouched.
///
/// The check and the rename are two steps: no portable call replaces a
/// file only if it is still a given one, so a program writing `path`
/// between them (microseconds) is not caught.
pub fn replace_existing(
    temp: tempfile::NamedTempFile,
    path: &Path,
    still_expected: impl FnOnce(&Path) -> bool,
) -> Result<()> {
    ensure!(
        replace_matching(temp, path, |path| Ok(still_expected(path)))?,
        "{} changed since it was checked; it is not replaced",
        path.display()
    );
    Ok(())
}

fn replace_matching(
    temp: tempfile::NamedTempFile,
    path: &Path,
    still_expected: impl FnOnce(&Path) -> Result<bool>,
) -> Result<bool> {
    replace_matching_unless(temp, path, still_expected, &|| Ok(()))
}

/// [`replace_matching`] with `go_on` passing right before each rename
/// attempt too (a cancelled export stops there).
fn replace_matching_unless(
    temp: tempfile::NamedTempFile,
    path: &Path,
    still_expected: impl FnOnce(&Path) -> Result<bool>,
    go_on: &dyn Fn() -> Result<()>,
) -> Result<bool> {
    let parent = match path.parent() {
        Some(parent) if parent.as_os_str().is_empty() => Path::new("."),
        Some(parent) => parent,
        None => anyhow::bail!("File needs a parent directory"),
    };
    let stamp = |path: &Path| -> Result<(fs::Metadata, FileStamp)> {
        let meta = fs::symlink_metadata(path)
            .with_context(|| format!("{} is not there to be replaced", path.display()))?;
        ensure!(
            meta.is_file() && !meta.file_type().is_symlink(),
            "Refusing to replace a symlink or non-file"
        );
        let stamp = FileStamp::of(path, &meta);
        Ok((meta, stamp))
    };
    let (meta, before) = match stamp(path) {
        Ok(stamped) => stamped,
        Err(e)
            if e.downcast_ref::<io::Error>()
                .is_some_and(|e| e.kind() == io::ErrorKind::NotFound) =>
        {
            return Ok(false);
        }
        Err(e) => return Err(e),
    };
    #[cfg(windows)]
    ensure!(
        !meta.permissions().readonly(),
        "{} is read-only; it is not replaced",
        path.display()
    );
    if !still_expected(path)? {
        return Ok(false);
    }
    let unchanged = || -> Result<()> {
        go_on()?;
        let (_, now) = stamp(path)?;
        ensure!(
            now == before && stamp(path)?.1 == before,
            "{} changed while it was checked; it is not replaced",
            path.display()
        );
        Ok(())
    };
    unchanged()?;
    temp.as_file().set_permissions(meta.permissions())?;
    replace_checked(temp, path, &unchanged)?;
    sync_dir(parent)?;
    Ok(true)
}

/// Publishes the finished temporary file `temp` (created beside `path`,
/// written and synced by the caller) over the file at `path` that the user
/// confirmed replacing (the save panel's Replace): `confirmed` is that
/// file as read when the choice was made ([`ConfirmedFile::read`]).
/// One atomic rename, never a delete followed by a write: until it, `path`
/// holds the old file, and on any refusal or failure it still does and
/// `temp` is removed. Refused: a `path` that `protected` names (a photo,
/// its sidecar), a symlink or non-file, on Windows a read-only file, and a
/// file changed or put in its place since it was confirmed (its content
/// digest checked first, its identity, size and modification time right
/// before each rename attempt, as [`replace_existing`] does), and a
/// confirmed file gone meanwhile (nothing is created in its place: the
/// temporary file's owner-only permissions are for a replace, which gives
/// the new file the old one's). `cancelled` is asked right before each
/// rename attempt: once it says yes, nothing is replaced.
///
/// Only a failure before the rename leaves the old file: should the folder
/// fail to sync after it (a network share dropping), the error says so.
pub fn replace_confirmed(
    temp: tempfile::NamedTempFile,
    path: &Path,
    confirmed: &ConfirmedFile,
    protected: impl Fn(&Path) -> bool,
    cancelled: &dyn Fn() -> bool,
) -> Result<()> {
    ensure!(
        fs::symlink_metadata(path).is_ok(),
        "{} is gone since you chose to replace it; nothing is written",
        path.display()
    );
    ensure!(
        !protected(path),
        "{} is a photo or one of its files; it is not replaced",
        path.display()
    );
    ensure!(
        replace_matching_unless(temp, path, |path| Ok(confirmed.matches(path)), &|| {
            ensure!(
                !cancelled(),
                "Cancelled; {} is not replaced",
                path.display()
            );
            Ok(())
        })?,
        "{} changed since you chose to replace it; it is not replaced",
        path.display()
    );
    Ok(())
}

/// A file the user confirmed replacing, as it was then: its stamp and the
/// SHA-256 of its content (an edit that keeps size and modification time,
/// as some metadata editors do, still tells it apart).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ConfirmedFile {
    stamp: FileStamp,
    sha256: String,
}

impl ConfirmedFile {
    /// Reads the regular file at `path` (a symlink or non-file is refused);
    /// fails if it changes while it is read.
    pub fn read(path: &Path) -> io::Result<Self> {
        let stamp = FileStamp::of_file(path)?;
        let sha256 = sha256_hex_read(&mut fs::File::open(path)?)?;
        if FileStamp::of_file(path)? != stamp {
            return Err(io::Error::other(format!(
                "{} changed while it was read",
                path.display()
            )));
        }
        Ok(Self { stamp, sha256 })
    }

    /// Whether the file at `path` is still this one, unchanged.
    pub fn matches(&self, path: &Path) -> bool {
        Self::read(path).is_ok_and(|now| now == *self)
    }
}

/// Whether `a` and `b` name the same file (one directory entry reached
/// through a symlink, a junction or another spelling, or a hard link of
/// it), told by its identity: device and inode on Unix, volume serial
/// number and file index on Windows. False when either cannot be read.
pub fn same_file(a: &Path, b: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        matches!((fs::metadata(a), fs::metadata(b)),
            (Ok(a), Ok(b)) if (a.dev(), a.ino()) == (b.dev(), b.ino()))
    }
    #[cfg(windows)]
    {
        matches!((file_id(a), file_id(b)), (Some(a), Some(b)) if a == b)
    }
    #[cfg(not(any(unix, windows)))]
    {
        canonicalize(a)
            .ok()
            .is_some_and(|a| canonicalize(b).ok() == Some(a))
    }
}

/// What tells a file at a path from one put there in its place: its size,
/// modification time and file identity (device and inode on Unix, volume
/// serial number and file index on Windows).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FileStamp {
    len: u64,
    modified: Option<std::time::SystemTime>,
    id: Option<(u64, u64)>,
}

impl FileStamp {
    /// Stamps the file named by `path` with metadata only, without opening
    /// it to query its identity.
    pub fn read(path: &Path) -> io::Result<Self> {
        fs::metadata(path).map(|meta| Self::from_metadata(&meta))
    }

    /// Stamps the file described by `meta`, including its identity on Unix.
    /// Use the open descriptor's metadata when stamping bytes read from it.
    pub fn from_metadata(meta: &fs::Metadata) -> Self {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;

        Self {
            len: meta.len(),
            modified: meta.modified().ok(),
            #[cfg(unix)]
            id: Some((meta.dev(), meta.ino())),
            #[cfg(not(unix))]
            id: None,
        }
    }

    /// Stamps the regular file at `path` as the replace checks compare it
    /// ([`replace_confirmed`]): a symlink or non-file is refused.
    pub fn of_file(path: &Path) -> io::Result<Self> {
        let meta = fs::symlink_metadata(path)?;
        if !meta.is_file() || meta.file_type().is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} is not a regular file", path.display()),
            ));
        }
        Ok(Self::of(path, &meta))
    }

    fn of(_path: &Path, meta: &fs::Metadata) -> Self {
        let stamp = Self::from_metadata(meta);
        #[cfg(windows)]
        let stamp = Self {
            id: file_id(_path),
            ..stamp
        };
        stamp
    }
}

/// The identity of the file at `path`: device and inode on Unix, volume
/// serial number and file index on Windows (where `fs::Metadata` carries
/// neither, and NTFS gives a file put in another's place within seconds the
/// old one's creation time). `None` when it cannot be read.
pub fn file_identity(path: &Path) -> Option<(u64, u64)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        fs::metadata(path).ok().map(|meta| (meta.dev(), meta.ino()))
    }
    #[cfg(windows)]
    {
        file_id(path)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        None
    }
}

#[cfg(windows)]
fn file_id(path: &Path) -> Option<(u64, u64)> {
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        GetFileInformationByHandle,
    };
    // Opened for its attributes only, sharing everything: it blocks no
    // rename and no other program.
    let file = fs::OpenOptions::new()
        .access_mode(0x80) // FILE_READ_ATTRIBUTES
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .open(path)
        .ok()?;
    // SAFETY: a plain C structure of integers; all zeros is a valid value.
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: a valid handle of an open file and a writable out-structure.
    let ok = unsafe { GetFileInformationByHandle(file.as_raw_handle() as _, &mut info) } != 0;
    ok.then(|| {
        (
            u64::from(info.dwVolumeSerialNumber),
            (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
        )
    })
}

/// Renames `file` over `path`; on Windows retried while another program
/// holds `path` open (see [`atomic_write`]).
fn replace(file: tempfile::NamedTempFile, path: &Path) -> Result<()> {
    replace_checked(file, path, &|| Ok(()))
}

/// [`replace`] with `check` passing right before each rename attempt.
fn replace_checked(
    file: tempfile::NamedTempFile,
    path: &Path,
    check: &dyn Fn() -> Result<()>,
) -> Result<()> {
    #[cfg(windows)]
    {
        let mut file = file;
        // About 3 s in all: a scanner's or indexer's read of a sidecar
        // takes milliseconds; a program that keeps it open will not let go.
        for wait_ms in [10, 20, 50, 100, 200, 400, 800, 1500] {
            check()?;
            match file.persist(path) {
                Ok(_) => return Ok(()),
                Err(e) if in_use(&e.error) => {
                    file = e.file;
                    std::thread::sleep(std::time::Duration::from_millis(wait_ms));
                }
                Err(e) => return Err(e.error.into()),
            }
        }
        check()?;
        file.persist(path).map(drop).map_err(|e| {
            let in_use = in_use(&e.error);
            let error = anyhow::Error::from(e.error);
            if in_use {
                error.context(format!(
                    "{} is in use by another program and was not replaced",
                    path.display()
                ))
            } else {
                error
            }
        })
    }
    #[cfg(not(windows))]
    {
        check()?;
        file.persist(path).map_err(|e| e.error)?;
        Ok(())
    }
}

/// A Windows rename refused because another program has the file open
/// (`ERROR_ACCESS_DENIED` is what a replace over a file open without
/// delete sharing returns; read-only files are refused before).
#[cfg(windows)]
fn in_use(error: &io::Error) -> bool {
    const ERROR_ACCESS_DENIED: i32 = 5;
    const ERROR_SHARING_VIOLATION: i32 = 32;
    const ERROR_LOCK_VIOLATION: i32 = 33;
    matches!(
        error.raw_os_error(),
        Some(ERROR_ACCESS_DENIED | ERROR_SHARING_VIOLATION | ERROR_LOCK_VIOLATION)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_if_unchanged_checks_bytes_and_absence() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("photo.rrdata");
        assert!(write_if_unchanged(&path, None, b"save").unwrap());
        assert!(!write_if_unchanged(&path, None, b"new").unwrap());
        assert!(!write_if_unchanged(&path, Some(b"edit"), b"new").unwrap());
        assert_eq!(fs::read(&path).unwrap(), b"save");
        assert!(write_if_unchanged(&path, Some(b"save"), b"new").unwrap());
        assert_eq!(fs::read(&path).unwrap(), b"new");
        fs::remove_file(&path).unwrap();
        assert!(!write_if_unchanged(&path, Some(b"new"), b"repair").unwrap());
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn a_confirmed_replace_swaps_in_the_new_file_atomically() {
        use std::io::Write as _;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("print.jpg");
        let temp_with = |bytes: &[u8]| {
            let mut temp = temp::file_for(&path).unwrap();
            temp.write_all(bytes).unwrap();
            temp.as_file().sync_all().unwrap();
            temp
        };
        fs::write(&path, b"someone else's print").unwrap();
        #[cfg(unix)]
        let old_inode = {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
            fs::metadata(&path).unwrap().ino()
        };
        let confirmed = ConfirmedFile::read(&path).unwrap();
        let only_print = || {
            let names: Vec<_> = fs::read_dir(dir.path())
                .unwrap()
                .map(|e| e.unwrap().file_name())
                .collect();
            assert_eq!(names, ["print.jpg"]);
        };
        // Unix: the old file is open while it is replaced and keeps its
        // bytes (a rename, never a truncate or a delete-then-write).
        // Windows: a program holding the old file open without delete
        // sharing (as `File::open` does) blocks the rename; the replace is
        // refused naming the cause, the old file and no temporary are left,
        // and it succeeds once the file is closed.
        let mut open = fs::File::open(&path).unwrap();
        #[cfg(windows)]
        {
            let error = replace_confirmed(
                temp_with(b"new export"),
                &path,
                &confirmed,
                |_| false,
                &|| false,
            )
            .unwrap_err();
            assert!(format!("{error:#}").contains("in use"), "{error:#}");
            assert_eq!(fs::read(&path).unwrap(), b"someone else's print");
            only_print();
            let mut old = Vec::new();
            open.read_to_end(&mut old).unwrap();
            assert_eq!(old, b"someone else's print");
            drop(open);
        }
        replace_confirmed(
            temp_with(b"new export"),
            &path,
            &confirmed,
            |_| false,
            &|| false,
        )
        .unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"new export");
        #[cfg(unix)]
        {
            let mut old = Vec::new();
            open.read_to_end(&mut old).unwrap();
            assert_eq!(old, b"someone else's print");
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            let meta = fs::metadata(&path).unwrap();
            assert_ne!(meta.ino(), old_inode);
            assert_eq!(meta.permissions().mode() & 0o777, 0o644);
        }
        only_print();
    }

    #[test]
    fn a_confirmed_replace_leaves_the_old_file_on_every_refusal() {
        use std::io::Write as _;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("print.jpg");
        let temp_with = |bytes: &[u8]| {
            let mut temp = temp::file_for(&path).unwrap();
            temp.write_all(bytes).unwrap();
            temp
        };
        let only_print = || {
            let names: Vec<_> = fs::read_dir(dir.path())
                .unwrap()
                .map(|e| e.unwrap().file_name())
                .collect();
            assert_eq!(names, ["print.jpg"]);
        };
        fs::write(&path, b"old").unwrap();
        let confirmed = ConfirmedFile::read(&path).unwrap();
        // A protected file (the photo, its sidecar) is never replaced.
        let error = replace_confirmed(temp_with(b"new"), &path, &confirmed, |p| p == path, &|| {
            false
        });
        assert!(error.unwrap_err().to_string().contains("not replaced"));
        assert_eq!(fs::read(&path).unwrap(), b"old");
        only_print();
        // Changed since the user confirmed it: untouched.
        fs::write(&path, b"retouched since").unwrap();
        assert!(
            replace_confirmed(temp_with(b"new"), &path, &confirmed, |_| false, &|| false).is_err()
        );
        assert_eq!(fs::read(&path).unwrap(), b"retouched since");
        only_print();
        // The export is cancelled while the old file is checked: nothing
        // is replaced, though everything else would allow it.
        let confirmed = ConfirmedFile::read(&path).unwrap();
        assert!(
            replace_confirmed(temp_with(b"new"), &path, &confirmed, |_| false, &|| true).is_err()
        );
        assert_eq!(fs::read(&path).unwrap(), b"retouched since");
        only_print();
        // Edited in place keeping size and modification time (a metadata
        // editor that preserves timestamps): told by its content.
        let confirmed = ConfirmedFile::read(&path).unwrap();
        let modified = fs::metadata(&path).unwrap().modified().unwrap();
        fs::write(&path, b"RETOUCHED SINCE").unwrap();
        fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(modified)
            .unwrap();
        assert!(
            replace_confirmed(temp_with(b"new"), &path, &confirmed, |_| false, &|| false).is_err()
        );
        assert_eq!(fs::read(&path).unwrap(), b"RETOUCHED SINCE");
        only_print();
        // Another file put in its place right before the rename: untouched.
        let confirmed = ConfirmedFile::read(&path).unwrap();
        let swapped = replace_confirmed(
            temp_with(b"new"),
            &path,
            &confirmed,
            |p| {
                let copy = p.with_extension("synced");
                fs::write(&copy, b"a synced copy, longer").unwrap();
                fs::rename(&copy, p).unwrap();
                false
            },
            &|| false,
        );
        assert!(swapped.is_err());
        assert_eq!(fs::read(&path).unwrap(), b"a synced copy, longer");
        only_print();
        #[cfg(unix)]
        {
            let link = dir.path().join("link.jpg");
            std::os::unix::fs::symlink(&path, &link).unwrap();
            assert!(ConfirmedFile::read(&link).is_err());
            let confirmed = ConfirmedFile::read(&path).unwrap();
            let mut temp = temp::file_for(&link).unwrap();
            temp.write_all(b"via link").unwrap();
            assert!(replace_confirmed(temp, &link, &confirmed, |_| false, &|| false).is_err());
            assert_eq!(fs::read(&path).unwrap(), b"a synced copy, longer");
            fs::remove_file(&link).unwrap();
        }
        // Gone since it was confirmed: nothing is created in its place.
        fs::remove_file(&path).unwrap();
        assert!(
            replace_confirmed(temp_with(b"new"), &path, &confirmed, |_| false, &|| false).is_err()
        );
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn same_file_sees_through_links_and_spellings() {
        let dir = tempfile::tempdir().unwrap();
        let photo = dir.path().join("photo.jpg");
        let other = dir.path().join("other.jpg");
        fs::write(&photo, b"a").unwrap();
        fs::write(&other, b"a").unwrap();
        assert!(same_file(&photo, &dir.path().join(".").join("photo.jpg")));
        assert!(!same_file(&photo, &other));
        assert!(!same_file(&photo, &dir.path().join("missing.jpg")));
        fs::hard_link(&photo, dir.path().join("linked.jpg")).unwrap();
        assert!(same_file(&photo, &dir.path().join("linked.jpg")));
        #[cfg(unix)]
        {
            let folder = dir.path().join("folder link");
            std::os::unix::fs::symlink(dir.path(), &folder).unwrap();
            assert!(same_file(&photo, &folder.join("photo.jpg")));
        }
    }

    #[test]
    fn replace_existing_replaces_only_a_file_still_as_expected() {
        use std::io::Write as _;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("photo.jpg");
        let temp_with = |bytes: &[u8]| {
            let mut temp = temp::file_for(&path).unwrap();
            temp.write_all(bytes).unwrap();
            temp
        };
        // Nothing there: nothing is created.
        assert!(replace_existing(temp_with(b"new"), &path, |_| true).is_err());
        assert!(!path.exists());
        fs::write(&path, b"old").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        }
        // Changed since it was checked: untouched.
        assert!(replace_existing(temp_with(b"new"), &path, |_| false).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"old");
        // Another file put in its place while it was checked (a sync
        // client's copy): untouched.
        let swapped = replace_existing(temp_with(b"new"), &path, |path| {
            let copy = path.with_extension("synced");
            fs::write(&copy, b"retouched").unwrap();
            fs::rename(&copy, path).unwrap();
            true
        });
        assert!(swapped.is_err());
        assert_eq!(fs::read(&path).unwrap(), b"retouched");
        fs::write(&path, b"old").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        }
        replace_existing(temp_with(b"new"), &path, |_| true).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"new");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o640
            );
            let link = dir.path().join("link.jpg");
            std::os::unix::fs::symlink(&path, &link).unwrap();
            let mut temp = temp::file_for(&link).unwrap();
            temp.write_all(b"via link").unwrap();
            assert!(replace_existing(temp, &link, |_| true).is_err());
            assert_eq!(fs::read(&path).unwrap(), b"new");
        }
        // Only the published file is left in the folder (no temporary files).
        let names: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .filter(|n| temp::is_temp_name(n))
            .collect();
        assert!(names.is_empty(), "{names:?}");
    }

    /// The digests stored in catalogs, sidecars and manifests are this
    /// lowercase form (FIPS 180-2 test vector).
    #[test]
    fn sha256_is_lowercase_hex() {
        let abc = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        assert_eq!(sha256_hex(b"abc"), abc);
        assert_eq!(hex([0x00, 0x0a, 0xff]), "000aff");
        assert_eq!(sha256_hex_read(&mut &b"abc"[..]).unwrap(), abc);
        let big = vec![7u8; 3 * 1024 * 1024 + 5];
        assert_eq!(sha256_hex_read(&mut &big[..]).unwrap(), sha256_hex(&big));
        let mut pieces = 0;
        let stopped = sha256_hex_read_while(&mut &big[..], || {
            pieces += 1;
            pieces < 3
        });
        assert_eq!(stopped.unwrap(), None);
        assert_eq!(
            sha256_hex_read_while(&mut &b"abc"[..], || true)
                .unwrap()
                .as_deref(),
            Some(abc)
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_path_with_a_nul_byte_is_refused() {
        use std::os::unix::ffi::OsStrExt;
        let path = Path::new(std::ffi::OsStr::from_bytes(b"a\0b"));
        assert_eq!(
            c_path(path).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(c_path(Path::new("/a")).unwrap().as_bytes(), b"/a");
    }

    #[cfg(unix)]
    #[test]
    fn a_replaced_file_keeps_its_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.xmp");
        std::fs::write(&path, "old").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        atomic_write(&path, "new").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o644);
    }

    #[cfg(windows)]
    #[test]
    fn a_read_only_file_is_refused_and_kept() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.xmp");
        fs::write(&path, "old").unwrap();
        let mut permissions = fs::metadata(&path).unwrap().permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&path, permissions).unwrap();
        let error = atomic_write(&path, "new").unwrap_err().to_string();
        assert!(error.contains("read-only"), "{error}");
        assert_eq!(fs::read_to_string(&path).unwrap(), "old");
        assert_eq!(
            fs::read_dir(dir.path()).unwrap().count(),
            1,
            "no temporary file left"
        );
    }

    /// A scanner or indexer that reads the file briefly (open without
    /// delete sharing): the replace waits for it.
    #[cfg(windows)]
    #[test]
    fn a_file_held_open_briefly_is_replaced_after_it_is_closed() {
        use std::os::windows::fs::OpenOptionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.xmp");
        fs::write(&path, "old").unwrap();
        // FILE_SHARE_READ only: no delete sharing, so no rename over it.
        let held = fs::OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(&path)
            .unwrap();
        let release = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(300));
            drop(held);
        });
        atomic_write(&path, "new").unwrap();
        release.join().unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "new");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[cfg(windows)]
    #[test]
    fn a_file_kept_open_fails_clearly_and_is_kept() {
        use std::os::windows::fs::OpenOptionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.xmp");
        fs::write(&path, "old").unwrap();
        let held = fs::OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(&path)
            .unwrap();
        let error = format!("{:#}", atomic_write(&path, "new").unwrap_err());
        assert!(error.contains("in use by another program"), "{error}");
        drop(held);
        assert_eq!(fs::read_to_string(&path).unwrap(), "old");
        assert_eq!(
            fs::read_dir(dir.path()).unwrap().count(),
            1,
            "no temporary file left"
        );
    }

    #[cfg(windows)]
    #[test]
    fn verbatim_paths_are_plain_unless_a_name_needs_the_verbatim_form() {
        let long = "d".repeat(300);
        let plain = |text: &str| simplified(Path::new(text)).to_string_lossy().into_owned();
        assert_eq!(plain(r"\\?\C:\Photos\a.RAF"), r"C:\Photos\a.RAF");
        assert_eq!(
            plain(&format!(r"\\?\C:\{long}\a.RAF")),
            format!(r"C:\{long}\a.RAF")
        );
        assert_eq!(plain(r"\\?\UNC\srv\share\a.RAF"), r"\\srv\share\a.RAF");
        for kept in [
            r"\\?\C:\Day \a.RAF",
            r"\\?\C:\a.",
            r"\\?\C:\x\NUL.txt",
            r"\\?\C:\com1",
        ] {
            assert_eq!(plain(kept), kept);
        }
        assert_eq!(plain(r"C:\Photos"), r"C:\Photos");
        // A library deeper than 260 characters keeps one spelling.
        let dir = tempfile::tempdir().unwrap();
        let deep = canonicalize(dir.path())
            .unwrap()
            .join(&long[..150])
            .join(&long[..150]);
        fs::create_dir_all(deep.join("Capture")).unwrap();
        fs::write(deep.join("Capture").join("a.RAF"), b"x").unwrap();
        let photo = canonicalize(deep.join("Capture").join("a.RAF")).unwrap();
        assert!(
            photo.starts_with(canonicalize(&deep).unwrap()),
            "{}",
            photo.display()
        );
        assert!(!photo.to_string_lossy().starts_with(r"\\?\"));
    }

    #[cfg(windows)]
    #[test]
    fn long_paths_are_given_the_extended_form() {
        let text = |path: &str| String::from_utf16(&wide_path(Path::new(path)).unwrap()).unwrap();
        let long = "d".repeat(250);
        assert_eq!(text(r"C:\a"), "C:\\a\0");
        assert_eq!(
            text(&format!(r"C:\x/{long}")),
            format!(r"\\?\C:\x\{long}") + "\0"
        );
        assert_eq!(
            text(&format!(r"\\srv\share\{long}")),
            format!(r"\\?\UNC\srv\share\{long}") + "\0"
        );
        let verbatim = format!(r"\\?\C:\x/{long}");
        assert_eq!(text(&verbatim), verbatim + "\0");
        // A file and folder past 260 characters work with our own calls.
        let dir = tempfile::tempdir().unwrap();
        let deep = canonicalize(dir.path())
            .unwrap()
            .join(&long[..200])
            .join(&long[..200]);
        fs::create_dir_all(&deep).unwrap();
        fs::write(deep.join("a.RAF"), b"x").unwrap();
        rename_no_clobber(&deep.join("a.RAF"), &deep.join("b.RAF")).unwrap();
        assert_eq!(fs::read(deep.join("b.RAF")).unwrap(), b"x");
        assert_eq!(stored_name(&deep.join("B.raf")).unwrap(), "b.RAF");
        assert!(volume::space(&deep).unwrap().total > 0);
    }

    #[cfg(windows)]
    #[test]
    fn the_stored_name_is_found_under_another_case() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.XMP"), "x").unwrap();
        assert_eq!(stored_name(&dir.path().join("a.xmp")).unwrap(), "a.XMP");
        assert_eq!(stored_name(&dir.path().join("*.xmp")), None);
        assert_eq!(stored_name(&dir.path().join("b.xmp")), None);
    }

    #[cfg(windows)]
    #[test]
    fn canonical_paths_are_plain_where_that_names_the_same_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.xmp");
        fs::write(&file, "x").unwrap();
        let canonical = canonicalize(&file).unwrap();
        assert!(
            !canonical.to_string_lossy().starts_with(r"\\?\"),
            "{}",
            canonical.display()
        );
        assert!(canonical.is_absolute());
        // A '/' appended by the frontend still names a file.
        let joined =
            std::path::PathBuf::from(format!("{}/b.xmp", canonical.parent().unwrap().display()));
        fs::write(&joined, "y").unwrap();
        assert_eq!(fs::read_to_string(dir.path().join("b.xmp")).unwrap(), "y");
        assert_eq!(Path::new(&file).canonical().unwrap(), canonical);
        // A path from an older catalog reads the same.
        assert_eq!(
            simplified(&PathBuf::from(format!(r"\\?\{}", canonical.display()))),
            canonical
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlink_target_is_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("a.arw");
        let sidecar = dir.path().join("a.xmp");
        fs::write(&original, b"sacred").unwrap();
        std::os::unix::fs::symlink(&original, &sidecar).unwrap();
        assert!(atomic_write(&sidecar, "changed").is_err());
        assert_eq!(fs::read(original).unwrap(), b"sacred");
    }
}
