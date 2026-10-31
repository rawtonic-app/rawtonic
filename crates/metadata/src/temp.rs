//! Temporary files written beside the user's files (atomic sidecar writes,
//! export and ingest publication).
//!
//! Every one is named after the file it becomes:
//! `.rawtonic-tmp-<final name>-XXXXXX` ([`file_for`]). What a killed app or a
//! power cut leaves behind is therefore recognisable as ours and as whose:
//! a job that resumes removes the leftovers of the files it was writing at
//! once ([`remove_leftovers_of`]), and a later library open removes any of
//! ours older than [`STALE_AGE`] ([`remove_stale`]). Names other programs
//! might use (tempfile's default `.tmpXXXXXX`) are never touched.

use std::{
    ffi::OsStr,
    fs, io,
    path::Path,
    time::{Duration, SystemTime},
};

/// Name prefix of every temporary file Rawtonic writes beside a user's files.
pub const TEMP_PREFIX: &str = ".rawtonic-tmp-";

/// Age after which a temporary file of ours is a crash's leftover. A write
/// holds its temporary file for at most a few seconds (the largest export
/// or RAW copy); a quarter of an hour leaves a wide margin for a slow drive
/// and for a second process (the CLI) writing into the same folder.
pub const STALE_AGE: Duration = Duration::from_secs(15 * 60);

/// Longest part of the final name a temporary name repeats (names stay
/// within the usual 255-byte limit with the prefix and random suffix).
const NAME_PART: usize = 200;

/// The name prefix of the temporary files that become `dest`.
pub fn prefix_for(dest: &Path) -> String {
    let name = dest
        .file_name()
        .map(|n| n.to_string_lossy())
        .unwrap_or_default();
    let mut end = name.len().min(NAME_PART);
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    format!("{TEMP_PREFIX}{}-", &name[..end])
}

/// A new temporary file beside `dest`, to become `dest`.
pub fn file_for(dest: &Path) -> io::Result<tempfile::NamedTempFile> {
    let parent = dest.parent().ok_or(io::ErrorKind::InvalidInput)?;
    tempfile::Builder::new()
        .prefix(&prefix_for(dest))
        .tempfile_in(parent)
}

/// Removes the temporary files of ours that were to become `dest` (plain
/// files only), whatever their age: for a job that resumes the write of
/// `dest`, which only it makes. Returns how many.
pub fn remove_leftovers_of(dest: &Path) -> usize {
    let (Some(parent), prefix) = (dest.parent(), prefix_for(dest)) else {
        return 0;
    };
    let Ok(entries) = fs::read_dir(parent) else {
        return 0;
    };
    entries
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|n| n.starts_with(&prefix))
        })
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        .filter(|entry| fs::remove_file(entry.path()).is_ok())
        .count()
}

/// Whether a file name is one of our temporary files.
pub fn is_temp_name(name: &OsStr) -> bool {
    name.to_str()
        .is_some_and(|name| name.starts_with(TEMP_PREFIX))
}

/// Removes `path` if it is a leftover temporary file of ours: our prefix, a
/// plain file (never a symlink or folder), last modified at least
/// [`STALE_AGE`] before `now`. Returns whether it was removed.
pub fn remove_stale(path: &Path, now: SystemTime) -> io::Result<bool> {
    if !path.file_name().is_some_and(is_temp_name) {
        return Ok(false);
    }
    let meta = fs::symlink_metadata(path)?;
    if !meta.file_type().is_file() {
        return Ok(false);
    }
    let age = now
        .duration_since(meta.modified()?)
        .unwrap_or(Duration::ZERO);
    if age < STALE_AGE {
        return Ok(false);
    }
    fs::remove_file(path)?;
    Ok(true)
}

/// Removes the leftover temporary files of ours directly in `dir` (not in
/// its subfolders); returns how many. A folder that cannot be read counts as
/// none (it may be on a drive that is gone).
pub fn sweep_folder(dir: &Path, now: SystemTime) -> usize {
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .filter(|entry| is_temp_name(&entry.file_name()))
        .filter(|entry| remove_stale(&entry.path(), now).unwrap_or(false))
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_temporary_file_is_named_after_the_file_it_becomes() {
        let dir = tempfile::tempdir().unwrap();
        let file = file_for(&dir.path().join("DSC_0001.NEF")).unwrap();
        let name = file
            .path()
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        assert!(name.starts_with(".rawtonic-tmp-DSC_0001.NEF-"), "{name}");
        assert!(is_temp_name(file.path().file_name().unwrap()));
        let long = "é".repeat(120) + ".jpg";
        let prefix = prefix_for(Path::new(&long));
        assert!(prefix.len() <= TEMP_PREFIX.len() + NAME_PART + 1);
    }

    #[test]
    fn a_resumed_write_removes_only_its_own_leftovers() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("DSC_0001.NEF");
        let mine = file_for(&dest).unwrap().keep().unwrap().1;
        let other = file_for(&dir.path().join("DSC_0001.NEF.xmp"))
            .unwrap()
            .keep()
            .unwrap()
            .1;
        let foreign = dir.path().join(".tmpAbC123");
        fs::write(&foreign, b"x").unwrap();
        assert_eq!(remove_leftovers_of(&dest), 1);
        assert!(!mine.exists());
        assert!(other.exists() && foreign.exists());
    }

    #[test]
    fn only_old_temporary_files_of_ours_are_removed() {
        let dir = tempfile::tempdir().unwrap();
        let ours = file_for(&dir.path().join("photo.nef.xmp"))
            .unwrap()
            .keep()
            .unwrap()
            .1;
        let other = dir.path().join(".tmpabc123");
        let photo = dir.path().join("photo.nef");
        for path in [&other, &photo] {
            fs::write(path, b"x").unwrap();
        }
        let soon = SystemTime::now() + Duration::from_secs(60);
        assert!(
            !remove_stale(&ours, soon).unwrap(),
            "a fresh one may still be written"
        );
        let later = SystemTime::now() + STALE_AGE + Duration::from_secs(1);
        assert_eq!(sweep_folder(dir.path(), later), 1);
        assert!(!ours.exists());
        assert!(other.exists() && photo.exists());
        assert!(!remove_stale(&photo, later).unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_or_folder_with_the_prefix_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("photo.nef");
        fs::write(&target, b"x").unwrap();
        let link = dir.path().join(format!("{TEMP_PREFIX}link"));
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let folder = dir.path().join(format!("{TEMP_PREFIX}folder"));
        fs::create_dir(&folder).unwrap();
        let later = SystemTime::now() + STALE_AGE + Duration::from_secs(1);
        assert_eq!(sweep_folder(dir.path(), later), 0);
        assert!(target.exists() && folder.exists());
        assert!(fs::symlink_metadata(&link).is_ok());
    }
}
