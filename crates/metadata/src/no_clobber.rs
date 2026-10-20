//! Publishing a finished temporary file under a new name, never replacing
//! a file that is there.
//!
//! The temporary file is renamed exclusively (`renamex_np(RENAME_EXCL)` on
//! macOS, `renameat2(RENAME_NOREPLACE)` on Linux, else a hard link, through
//! `tempfile`). Volumes that support neither — exFAT and FAT on macOS
//! answer both with ENOTSUP, FAT on Linux refuses hard links with EPERM —
//! need another way:
//!
//! - [`persist`] copies: the destination is created with O_CREAT|O_EXCL
//!   (`create_new`), so it still fails rather than replace anything, the
//!   temporary file's bytes are copied into it and synced, then the
//!   temporary file goes. That copy is not atomic: while it runs, the new
//!   name shows a growing file, and a crash or power cut during it can
//!   leave a short file under the new name (never in place of an existing
//!   file). A copy that fails is kept, never removed by name: by then the
//!   name may belong to another file (ours moved away, another put there).
//!   Free space is checked first, so a full volume fails before the copy.
//!   The copy is handed back open, so a caller that verifies content
//!   (ingest) reads the very file it wrote.
//! - [`persist_in_own_folder`], for a folder only one writer uses (made by
//!   it, like a tether transfer slot), checks that the name is free and
//!   renames: atomic, and nothing else creates names there to race the
//!   check.
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::path::Path;
use tempfile::{NamedTempFile, PersistError};

/// How the file was published.
#[derive(Debug)]
pub enum Persisted {
    /// Renamed (or hard-linked) into place: atomic.
    Renamed,
    /// The volume has no exclusive rename or hard links: copied into a
    /// newly created file (see the module notes), open for reading.
    Copied(File),
}

/// Publishes `temp` (synced by the caller) at `path`; fails with
/// `AlreadyExists` if `path` exists. The caller syncs the folder.
pub fn persist(temp: NamedTempFile, path: &Path) -> io::Result<Persisted> {
    persist_with(temp, path, exclusive_rename, copy)
}

/// [`persist`] for a folder no other writer creates names in: without an
/// exclusive rename, it checks the name is free and renames (atomic).
pub fn persist_in_own_folder(temp: NamedTempFile, path: &Path) -> io::Result<Persisted> {
    persist_with(temp, path, exclusive_rename, rename_if_free)
}

fn exclusive_rename(temp: NamedTempFile, path: &Path) -> Result<(), PersistError> {
    temp.persist_noclobber(path).map(drop)
}

/// The exclusive rename, then `fallback` where the volume cannot do one
/// (a test injects such a volume).
fn persist_with(
    temp: NamedTempFile,
    path: &Path,
    exclusive: impl FnOnce(NamedTempFile, &Path) -> Result<(), PersistError>,
    fallback: impl FnOnce(NamedTempFile, &Path) -> io::Result<Persisted>,
) -> io::Result<Persisted> {
    match exclusive(temp, path) {
        Ok(()) => Ok(Persisted::Renamed),
        Err(e) if unsupported(&e.error) => fallback(e.file, path),
        Err(e) => Err(e.error),
    }
}

fn copy(temp: NamedTempFile, path: &Path) -> io::Result<Persisted> {
    copy_into_new(temp.as_file(), path).map(Persisted::Copied)
}

fn rename_if_free(temp: NamedTempFile, path: &Path) -> io::Result<Persisted> {
    if fs::symlink_metadata(path).is_ok() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("{} already exists", path.display()),
        ));
    }
    temp.persist(path).map_err(|e| e.error)?;
    Ok(Persisted::Renamed)
}

/// An exclusive rename or hard link the volume cannot do (rather than one
/// refused because the name exists, or for lack of space or permission).
/// The copy refuses an existing name too, so this only keeps real errors
/// from being retried as a copy.
fn unsupported(error: &io::Error) -> bool {
    #[cfg(unix)]
    return matches!(
        error.raw_os_error(),
        Some(libc::ENOTSUP | libc::EOPNOTSUPP | libc::EINVAL | libc::ENOSYS | libc::EPERM)
    );
    #[cfg(not(unix))]
    {
        let _ = error;
        false
    }
}

/// Copies `source` into a new file at `path` (never an existing one),
/// syncs it and returns it open.
fn copy_into_new(source: &File, path: &Path) -> io::Result<File> {
    let len = source.metadata()?.len();
    ensure_space(path, len)?;
    let mut options = OpenOptions::new();
    options.read(true).write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        // The mode the rename would have published.
        options.mode(source.metadata()?.permissions().mode() & 0o7777);
    }
    let output = options.open(path)?;
    let mut input = source.try_clone()?;
    io::Seek::rewind(&mut input)?;
    fill(&mut input, len, output, path)
}

/// Copies `len` bytes of `input` into `output` (the new file at `path`)
/// and syncs it. A failed copy is kept and named in the error.
fn fill(input: &mut impl Read, len: u64, mut output: File, path: &Path) -> io::Result<File> {
    let copied = (|| {
        if io::copy(input, &mut output)? != len {
            return Err(io::Error::other("the copy is incomplete"));
        }
        output.sync_all()
    })();
    match copied {
        Ok(()) => Ok(output),
        Err(e) => Err(io::Error::new(
            e.kind(),
            format!("{e} (the incomplete copy is kept at {})", path.display()),
        )),
    }
}

/// Fails with `StorageFull` if the volume of `path` has less than `len`
/// bytes free (where it can tell).
fn ensure_space(path: &Path, len: u64) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let folder = match path.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent,
            _ => Path::new("."),
        };
        let Ok(folder) = std::ffi::CString::new(folder.as_os_str().as_bytes()) else {
            return Ok(());
        };
        // SAFETY: a zeroed statvfs is a valid out value; the path is a
        // valid NUL-terminated string for the call's duration.
        let mut stats: libc::statvfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::statvfs(folder.as_ptr(), &mut stats) } != 0 {
            return Ok(());
        }
        // The field types differ per platform (u32 or u64).
        #[allow(clippy::useless_conversion)]
        let free = u64::from(stats.f_bavail).saturating_mul(u64::from(stats.f_frsize));
        if free < len {
            return Err(io::Error::new(
                io::ErrorKind::StorageFull,
                format!("{len} bytes do not fit on the volume of {}", path.display()),
            ));
        }
    }
    #[cfg(not(unix))]
    let _ = (path, len);
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp_with(dir: &Path, bytes: &[u8]) -> NamedTempFile {
        let mut temp = NamedTempFile::new_in(dir).unwrap();
        temp.write_all(bytes).unwrap();
        temp.as_file().sync_all().unwrap();
        temp
    }

    /// A volume without exclusive rename or hard links (exFAT on macOS).
    fn no_exclusive_rename(
        errno: i32,
    ) -> impl FnOnce(NamedTempFile, &Path) -> Result<(), PersistError> {
        move |file, _| {
            Err(PersistError {
                error: io::Error::from_raw_os_error(errno),
                file,
            })
        }
    }

    fn entries(dir: &Path) -> Vec<String> {
        let mut names: Vec<_> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn a_volume_without_exclusive_rename_gets_a_copy_and_no_leftover() {
        for errno in [
            libc::ENOTSUP,
            libc::EOPNOTSUPP,
            libc::EINVAL,
            libc::ENOSYS,
            libc::EPERM,
        ] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("photo.jpg");
            let temp = temp_with(dir.path(), b"pixels");
            let mode = {
                use std::os::unix::fs::PermissionsExt;
                temp.as_file().metadata().unwrap().permissions().mode()
            };
            let how = persist_with(temp, &path, no_exclusive_rename(errno), copy).unwrap();
            assert!(matches!(how, Persisted::Copied(_)), "errno {errno}");
            assert_eq!(fs::read(&path).unwrap(), b"pixels");
            assert_eq!(entries(dir.path()), ["photo.jpg"]);
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(&path).unwrap().permissions().mode(), mode);
        }
    }

    #[test]
    fn the_copy_never_replaces_an_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("photo.jpg");
        fs::write(&path, b"the user's").unwrap();
        let temp = temp_with(dir.path(), b"new");
        let error =
            persist_with(temp, &path, no_exclusive_rename(libc::ENOTSUP), copy).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&path).unwrap(), b"the user's");
        assert_eq!(entries(dir.path()), ["photo.jpg"]);
    }

    #[test]
    fn other_errors_are_not_retried_as_a_copy() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("photo.jpg");
        let temp = temp_with(dir.path(), b"new");
        let error = persist_with(temp, &path, no_exclusive_rename(libc::ENOSPC), copy).unwrap_err();
        assert_eq!(error.raw_os_error(), Some(libc::ENOSPC));
        assert!(!path.exists());
    }

    #[test]
    fn a_volume_with_exclusive_rename_renames_and_refuses_existing_names() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("photo.jpg");
        let how = persist(temp_with(dir.path(), b"one"), &path).unwrap();
        assert!(matches!(how, Persisted::Renamed));
        let error = persist(temp_with(dir.path(), b"two"), &path).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&path).unwrap(), b"one");
        assert_eq!(entries(dir.path()), ["photo.jpg"]);
    }

    #[test]
    fn in_its_own_folder_a_volume_without_exclusive_rename_gets_a_checked_rename() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("DSC00001.ARW");
        let temp = temp_with(dir.path(), b"raw");
        let how = persist_with(
            temp,
            &path,
            no_exclusive_rename(libc::ENOTSUP),
            rename_if_free,
        );
        assert!(matches!(how.unwrap(), Persisted::Renamed));
        assert_eq!(fs::read(&path).unwrap(), b"raw");
        let temp = temp_with(dir.path(), b"other");
        let error = persist_with(
            temp,
            &path,
            no_exclusive_rename(libc::ENOTSUP),
            rename_if_free,
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&path).unwrap(), b"raw");
        assert_eq!(entries(dir.path()), ["DSC00001.ARW"]);
    }

    #[test]
    fn a_failed_copy_never_removes_a_file_that_took_its_name() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("photo.jpg");
        let output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        // While the copy runs, its file is moved away and another takes the name.
        struct Interrupted<'a>(&'a Path);
        impl Read for Interrupted<'_> {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                fs::rename(self.0, self.0.with_extension("moved"))?;
                fs::write(self.0, b"another photo")?;
                Err(io::Error::other("the card was pulled"))
            }
        }
        let error = fill(&mut Interrupted(&path), 6, output, &path).unwrap_err();
        assert!(error.to_string().contains("kept at"), "{error}");
        assert_eq!(fs::read(&path).unwrap(), b"another photo");
    }

    #[test]
    fn a_copy_that_cannot_fit_fails_before_making_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("photo.jpg");
        let error = ensure_space(&path, u64::MAX).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::StorageFull);
        assert!(ensure_space(&path, 1).is_ok());
    }
}
