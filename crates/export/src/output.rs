//! Writing export files: never over an existing file, except the previous
//! export a Replace if changed replaces and the file a user confirmed
//! replacing in Save As.
use std::fs;
use std::io;
use std::path::Path;

/// The folder a file at `path` is created in (`.` for a bare file name).
pub fn output_folder(path: &Path) -> Option<&Path> {
    path.parent().map(|parent| {
        if parent.as_os_str().is_empty() {
            Path::new(".")
        } else {
            parent
        }
    })
}

/// Writes `bytes` to a new file at `path` through a synced temporary file
/// beside it; fails if `path` exists. The file gets the permissions of any
/// new file (the umask's), not the temporary file's owner-only ones.
pub fn write_new_output(path: impl AsRef<Path>, bytes: impl AsRef<[u8]>) -> io::Result<()> {
    use std::io::Write;
    write_new_output_with(path.as_ref(), |file| file.write_all(bytes.as_ref()))
}

/// [`write_new_output`] with the file's content written by `fill` into the
/// temporary file (empty, at position 0), so a large file is streamed in
/// rather than built in memory. When `fill` fails, the temporary file is
/// removed and nothing is published.
pub fn write_new_output_with(
    path: &Path,
    fill: impl FnOnce(&mut fs::File) -> io::Result<()>,
) -> io::Result<()> {
    let parent = output_folder(path).ok_or_else(|| io::Error::other("Missing output folder"))?;
    let prefix = rawtonic_fs::temp::prefix_for(path);
    let mut builder = tempfile::Builder::new();
    builder.prefix(&prefix);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(fs::Permissions::from_mode(0o666));
    }
    let mut temp = builder.tempfile_in(parent)?;
    fill(temp.as_file_mut())?;
    temp.as_file().sync_all()?;
    rawtonic_fs::no_clobber::persist(temp, path)?;
    rawtonic_fs::sync_dir(parent)?;
    Ok(())
}

/// Replaces the previous export at `path` (`previous`, of `source`) with
/// the content `fill` writes into a synced temporary file beside it, in one
/// atomic rename (`rawtonic_fs::replace_existing`). Right before the rename
/// the file must still be that export, byte for byte; otherwise, or when
/// `fill` fails, nothing changes at `path` and the temporary file is gone.
pub fn replace_previous_output_with(
    path: &Path,
    previous: &crate::existing_files::Recorded,
    source: &str,
    fill: impl FnOnce(&mut fs::File) -> io::Result<()>,
) -> io::Result<()> {
    let parent = output_folder(path).ok_or_else(|| io::Error::other("Missing output folder"))?;
    let mut temp = tempfile::Builder::new()
        .prefix(&rawtonic_fs::temp::prefix_for(path))
        .tempfile_in(parent)?;
    fill(temp.as_file_mut())?;
    temp.as_file().sync_all()?;
    rawtonic_fs::replace_existing(temp, path, |path| previous.is_export_of(path, source))
        .map_err(io::Error::other)
}

/// Replaces the file at `path` the user confirmed replacing (Save As, the
/// save panel's Replace; `confirmed` its stamp then) with the content
/// `fill` writes into a synced temporary file beside it, in one atomic
/// rename (`rawtonic_fs::replace_confirmed`). A `protected` path (a photo,
/// its sidecar), a file changed since it was confirmed, or a failing
/// `fill` leave `path` as it was, without a temporary file.
pub fn replace_confirmed_output_with(
    path: &Path,
    confirmed: rawtonic_fs::FileStamp,
    protected: &dyn Fn(&Path) -> bool,
    fill: impl FnOnce(&mut fs::File) -> io::Result<()>,
) -> io::Result<()> {
    let parent = output_folder(path).ok_or_else(|| io::Error::other("Missing output folder"))?;
    if protected(path) {
        return Err(io::Error::other(format!(
            "{} is a photo or one of its files; it is not replaced",
            path.display()
        )));
    }
    // Owner-only while it is written: the replaced file's permissions are
    // given to it right before it is swapped in.
    let mut temp = tempfile::Builder::new()
        .prefix(&rawtonic_fs::temp::prefix_for(path))
        .tempfile_in(parent)?;
    fill(temp.as_file_mut())?;
    temp.as_file().sync_all()?;
    rawtonic_fs::replace_confirmed(temp, path, confirmed, protected).map_err(io::Error::other)
}

/// How an export file is published: as a new file, over the previous
/// export of the same photo (Replace if changed), or over the file the
/// user confirmed replacing (Save As).
#[derive(Clone, Copy)]
pub enum Publish<'a> {
    New,
    Replace {
        previous: &'a crate::existing_files::Recorded,
        source: &'a str,
    },
    Confirmed {
        stamp: rawtonic_fs::FileStamp,
        protected: &'a dyn Fn(&Path) -> bool,
    },
}

/// The size and SHA-256 of a file as an export wrote it, taken from its
/// temporary file before it is published: what its record certifies, never
/// whatever is at its path later.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Written {
    pub size: u64,
    pub sha256: String,
}

impl Written {
    fn of(file: &mut fs::File) -> io::Result<Self> {
        use std::io::{Seek, SeekFrom, Write};
        file.flush()?;
        let size = file.seek(SeekFrom::End(0))?;
        file.seek(SeekFrom::Start(0))?;
        let sha256 = rawtonic_fs::sha256_hex_read(file)?;
        Ok(Self { size, sha256 })
    }
}

impl Publish<'_> {
    /// Publishes the file `fill` writes at `path` (see
    /// [`write_new_output_with`], [`replace_previous_output_with`]); returns
    /// what was written.
    pub fn write_with(
        self,
        path: &Path,
        fill: impl FnOnce(&mut fs::File) -> io::Result<()>,
    ) -> io::Result<Written> {
        let mut written = None;
        let fill = |file: &mut fs::File| {
            fill(file)?;
            written = Some(Written::of(file)?);
            Ok(())
        };
        match self {
            Publish::New => write_new_output_with(path, fill)?,
            Publish::Replace { previous, source } => {
                replace_previous_output_with(path, previous, source, fill)?
            }
            Publish::Confirmed { stamp, protected } => {
                replace_confirmed_output_with(path, stamp, protected, fill)?
            }
        }
        written.ok_or_else(|| io::Error::other("Nothing was written"))
    }

    /// [`Self::write_with`] of an encoded export.
    pub fn write_export(
        self,
        path: &Path,
        encoded: crate::export_processing::EncodedExport,
    ) -> Result<Written, String> {
        self.write_with(path, |file| {
            encoded.write_to(file).map_err(io::Error::other)
        })
        .map_err(|e| format!("Failed to write file to '{}': {}", path.display(), e))
    }
}

/// Writes an export file to a new file at `path` (never over an existing
/// one), a TIFF encoded straight into the temporary file.
pub fn write_new_export(
    path: &Path,
    encoded: crate::export_processing::EncodedExport,
) -> Result<(), String> {
    write_new_output_with(path, |file| {
        encoded.write_to(file).map_err(io::Error::other)
    })
    .map_err(|e| format!("Failed to write file to '{}': {}", path.display(), e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outputs_are_never_written_over_and_open_like_any_new_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("photo.jpg");
        write_new_output(&path, b"one").unwrap();
        assert!(write_new_output(&path, b"two").is_err());
        assert_eq!(fs::read(&path).unwrap(), b"one");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let plain = dir.path().join("plain");
            fs::write(&plain, b"").unwrap();
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                fs::metadata(&plain).unwrap().permissions().mode() & 0o777
            );
        }
    }

    #[test]
    fn only_the_recorded_previous_export_is_replaced() {
        use crate::existing_files::ExportRecords;
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let records = ExportRecords::open(&dir.path().join("records.sqlite")).unwrap();
        let path = dir.path().join("photo.jpg");
        write_new_output(&path, b"export one").unwrap();
        records.record(&path, "/a.raf", "v1").unwrap();
        let previous = records.get(&path).unwrap().unwrap();
        let replace = Publish::Replace {
            previous: &previous,
            source: "/a.raf",
        };
        replace
            .write_with(&path, |file| file.write_all(b"export two"))
            .unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"export two");
        // The file is no longer the recorded one: the same replace refuses.
        assert!(
            replace
                .write_with(&path, |file| file.write_all(b"export three"))
                .is_err()
        );
        assert_eq!(fs::read(&path).unwrap(), b"export two");
        // Never another photo's, and a failed encode publishes nothing.
        records.record(&path, "/a.raf", "v2").unwrap();
        let previous = records.get(&path).unwrap().unwrap();
        let other = Publish::Replace {
            previous: &previous,
            source: "/b.raf",
        };
        assert!(other.write_with(&path, |f| f.write_all(b"b")).is_err());
        let own = Publish::Replace {
            previous: &previous,
            source: "/a.raf",
        };
        assert!(
            own.write_with(&path, |_| Err(io::Error::other("encoder failed")))
                .is_err()
        );
        assert_eq!(fs::read(&path).unwrap(), b"export two");
        let names: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .filter(|name| rawtonic_fs::temp::is_temp_name(name))
            .collect();
        assert!(names.is_empty(), "{names:?}");
    }

    #[test]
    fn a_confirmed_file_is_replaced_and_kept_when_writing_fails() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("print.jpg");
        let original = dir.path().join("DSC_0001.NEF");
        fs::write(&path, b"another program's print").unwrap();
        fs::write(&original, b"raw").unwrap();
        let protected = |p: &Path| p == original;
        let confirmed = |p: &Path| Publish::Confirmed {
            stamp: rawtonic_fs::FileStamp::of_file(p).unwrap(),
            protected: &protected,
        };
        // The encoder fails: the confirmed file stays, no temporary file.
        let failed = confirmed(&path).write_with(&path, |file| {
            file.write_all(b"half")?;
            Err(io::Error::other("encoder failed"))
        });
        assert!(failed.is_err());
        assert_eq!(fs::read(&path).unwrap(), b"another program's print");
        // The photo itself is never replaced, even confirmed.
        assert!(
            confirmed(&original)
                .write_with(&original, |f| f.write_all(b"jpeg"))
                .is_err()
        );
        assert_eq!(fs::read(&original).unwrap(), b"raw");
        let written = confirmed(&path)
            .write_with(&path, |file| file.write_all(b"the export"))
            .unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"the export");
        assert_eq!(written.size, 10);
        let mut names: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, ["DSC_0001.NEF", "print.jpg"]);
    }

    #[test]
    fn a_file_whose_writing_fails_is_never_published() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("photo.tif");
        let failed = write_new_output_with(&path, |file| {
            file.write_all(b"half a file")?;
            Err(io::Error::other("encoder failed"))
        });
        assert!(failed.is_err());
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
    }
}
