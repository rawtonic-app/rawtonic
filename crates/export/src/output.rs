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
    stage_new_output_with(path, fill)?.publish()
}

/// A file written and synced beside its `path` but not published yet
/// ([`stage_new_output_with`]): [`StagedOutput::publish`] puts it at its
/// path; dropped unpublished, it is removed and nothing is published.
pub struct StagedOutput {
    temp: tempfile::NamedTempFile,
    path: std::path::PathBuf,
}

impl StagedOutput {
    /// The file it is published as.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Publishes the file at its path; fails if a file is there by now.
    pub fn publish(self) -> io::Result<()> {
        let parent = output_folder(&self.path)
            .ok_or_else(|| io::Error::other("Missing output folder"))?
            .to_path_buf();
        rawtonic_fs::no_clobber::persist(self.temp, &self.path)?;
        rawtonic_fs::sync_dir(&parent)?;
        Ok(())
    }
}

/// The first half of [`write_new_output_with`]: the file written by `fill`
/// into a synced temporary file beside `path`, to be published later (a
/// file of a render group written while an earlier recipe's render runs,
/// published at its own turn). When `fill` fails, the temporary file is
/// removed.
pub fn stage_new_output_with(
    path: &Path,
    fill: impl FnOnce(&mut fs::File) -> io::Result<()>,
) -> io::Result<StagedOutput> {
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
    Ok(StagedOutput {
        temp,
        path: path.to_path_buf(),
    })
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

/// [`write_new_export`] staged, to be published later
/// ([`stage_new_output_with`]).
pub fn stage_new_export(
    path: &Path,
    encoded: crate::export_processing::EncodedExport,
) -> Result<StagedOutput, String> {
    stage_new_output_with(path, |file| {
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

    #[test]
    fn a_staged_file_is_published_only_when_asked_and_never_over_a_file() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("photo.tif");
        let staged = stage_new_output_with(&path, |file| file.write_all(b"staged")).unwrap();
        assert!(!path.exists());
        assert_eq!(staged.path(), path);
        staged.publish().unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"staged");

        // Dropped unpublished: nothing is left.
        let other = dir.path().join("other.tif");
        drop(stage_new_output_with(&other, |file| file.write_all(b"x")).unwrap());
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);

        // A file that appeared meanwhile is never replaced.
        let staged = stage_new_output_with(&other, |file| file.write_all(b"late")).unwrap();
        fs::write(&other, b"first").unwrap();
        assert!(staged.publish().is_err());
        assert_eq!(fs::read(&other).unwrap(), b"first");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 2);
    }
}
