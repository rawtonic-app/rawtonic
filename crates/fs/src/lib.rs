//! File-safety kernel shared by the core and engine crates: atomic
//! replacement and no-clobber publication of files, renaming without
//! replacing, temporary and staging files beside the user's files. It never
//! deletes or replaces a file other than the one a caller names to be
//! replaced.
pub mod no_clobber;
mod rename;
pub mod staging;
pub mod temp;

#[cfg(all(unix, any(test, feature = "test-support")))]
pub use rename::test_hooks;
pub use rename::{
    Claim, ClaimKind, Renamed, release_claim, rename_no_clobber, rename_no_clobber_noting,
};

use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{self, Read, Write},
    path::Path,
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

/// Same-directory temporary file, fsync, atomic replace. Reject symlink targets.
/// A replaced file keeps its permissions (the temporary file's are private).
pub fn atomic_write(path: &Path, bytes: impl AsRef<[u8]>) -> Result<()> {
    let parent = path.parent().context("File needs a parent directory")?;
    let replaced = fs::symlink_metadata(path).ok();
    if let Some(meta) = &replaced {
        ensure!(
            meta.is_file() && !meta.file_type().is_symlink(),
            "Refusing to replace a symlink or non-file"
        );
    }
    let mut file = temp::file_for(path)?;
    file.write_all(bytes.as_ref())?;
    file.as_file().sync_all()?;
    if let Some(meta) = &replaced {
        file.as_file().set_permissions(meta.permissions())?;
    }
    file.persist(path).map_err(|e| e.error)?;
    sync_dir(parent)?;
    Ok(())
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
