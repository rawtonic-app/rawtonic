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
    let path = path.as_ref();
    let parent = output_folder(path).ok_or_else(|| io::Error::other("Missing output folder"))?;
    let mut builder = tempfile::Builder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(fs::Permissions::from_mode(0o666));
    }
    let mut temp = builder.tempfile_in(parent)?;
    temp.write_all(bytes.as_ref())?;
    temp.as_file().sync_all()?;
    temp.persist_noclobber(path).map_err(|e| e.error)?;
    #[cfg(unix)]
    fs::File::open(parent)?.sync_all()?;
    Ok(())
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
}
