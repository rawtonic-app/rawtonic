//! File-safety kernel shared by the core and engine crates: atomic
//! replacement and no-clobber publication of files, temporary and staging
//! files beside the user's files. It never deletes or replaces a file
//! other than the one a caller names to be replaced.
pub mod no_clobber;
pub mod staging;
pub mod temp;

use anyhow::{Context, Result, ensure};
use std::{fs, io::Write, path::Path};

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
    #[cfg(unix)]
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
