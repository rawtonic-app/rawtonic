//! Writing export files: never over an existing file.
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

/// How an export file is published: as a new file, or over the previous
/// export of the same photo (Replace if changed).
#[derive(Clone, Copy)]
pub enum Publish<'a> {
    New,
    Replace {
        previous: &'a crate::existing_files::Recorded,
        source: &'a str,
    },
}

impl Publish<'_> {
    /// Publishes the file `fill` writes at `path` (see
    /// [`write_new_output_with`], [`replace_previous_output_with`]).
    pub fn write_with(
        self,
        path: &Path,
        fill: impl FnOnce(&mut fs::File) -> io::Result<()>,
    ) -> io::Result<()> {
        match self {
            Publish::New => write_new_output_with(path, fill),
            Publish::Replace { previous, source } => {
                replace_previous_output_with(path, previous, source, fill)
            }
        }
    }

    /// [`Self::write_with`] of an encoded export.
    pub fn write_export(
        self,
        path: &Path,
        encoded: crate::export_processing::EncodedExport,
    ) -> Result<(), String> {
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
