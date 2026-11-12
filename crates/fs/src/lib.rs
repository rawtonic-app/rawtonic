//! File-safety kernel shared by the core and engine crates: atomic
//! replacement and no-clobber publication of files, renaming without
//! replacing, temporary and staging files beside the user's files. It never
//! deletes or replaces a file other than the one a caller names to be
//! replaced. It also detects volumes ([`volume`]).
#![warn(clippy::undocumented_unsafe_blocks)]
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
    let parent = path.parent().context("File needs a parent directory")?;
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
    let mut file = temp::file_for(path)?;
    file.write_all(bytes.as_ref())?;
    file.as_file().sync_all()?;
    if let Some(meta) = &replaced {
        file.as_file().set_permissions(meta.permissions())?;
    }
    replace(file, path)?;
    sync_dir(parent)?;
    Ok(())
}

/// Renames `file` over `path`; on Windows retried while another program
/// holds `path` open (see [`atomic_write`]).
fn replace(file: tempfile::NamedTempFile, path: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        let mut file = file;
        // About 3 s in all: a scanner's or indexer's read of a sidecar
        // takes milliseconds; a program that keeps it open will not let go.
        for wait_ms in [10, 20, 50, 100, 200, 400, 800, 1500] {
            match file.persist(path) {
                Ok(_) => return Ok(()),
                Err(e) if in_use(&e.error) => {
                    file = e.file;
                    std::thread::sleep(std::time::Duration::from_millis(wait_ms));
                }
                Err(e) => return Err(e.error.into()),
            }
        }
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
