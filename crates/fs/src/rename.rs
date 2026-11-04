//! Renaming without ever replacing a file (decision D3, measured
//! 2026-11-03): [`rename_no_clobber`] tries, in order,
//!
//! 1. the system's exclusive rename (`renamex_np(RENAME_EXCL)` on macOS,
//!    `renameat2(RENAME_NOREPLACE)` on Linux, `MoveFileExW` without
//!    `MOVEFILE_REPLACE_EXISTING` on Windows): atomic;
//! 2. where the volume has none (exFAT on macOS, some network mounts): an
//!    exclusive hard link under the new name, then the old name removed —
//!    a taken name refuses the link;
//! 3. where the volume has no hard links either (exFAT and FAT on macOS,
//!    FAT on Linux): the new name is claimed with an exclusive create (an
//!    empty file, or an empty folder for a folder), checked to still be
//!    that placeholder of ours just before, and the rename goes over it;
//! 4. else it fails with `Unsupported`.
//!
//! No step checks a name and then renames onto it: a file that had the new
//! name before the call is never replaced. The one window left is in step
//! 3, between the check of our placeholder and the rename over it: a file
//! another program puts there in that instant by itself replacing our
//! freshly created, empty placeholder (or what it writes into it) is
//! replaced (a folder placeholder that gained an entry is refused by the
//! system instead). No volume measured for D3 offers anything atomic here:
//! exFAT on macOS answers the exclusive rename, `RENAME_SWAP` and `link`
//! with ENOTSUP.
//!
//! A crash inside step 2 or 3 can leave a second hard link of the file, or
//! an empty placeholder, under the new name (never a lost file).
//! [`rename_no_clobber_noting`] tells its caller of the claim (device and
//! inode) before it goes on, so a journal can record it, and
//! [`release_claim`] removes it after a crash while it is still exactly that
//! entry (fileops' recovery does, and says so in the operation's note).
use serde::{Deserialize, Serialize};
use std::{fs, io, path::Path};

/// How [`rename_no_clobber`] put the file under its new name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Renamed {
    /// The system's exclusive rename.
    Exclusive,
    /// The same file under another spelling (a case-only rename on a
    /// case-insensitive volume): renamed in place.
    SameFile,
    /// A hard link under the new name, then the old name removed.
    Linked,
    /// Renamed over a placeholder it created under the new name.
    OverPlaceholder,
}

/// The new name a rename took before renaming (steps 2 and 3): the second
/// link of the file, or our empty placeholder. A crash before the rename
/// finished leaves it behind; a journal that records it can have
/// [`release_claim`] remove it again, only while it is still this entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Claim {
    pub kind: ClaimKind,
    pub dev: u64,
    pub ino: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ClaimKind {
    /// A hard link of the file under the new name (step 2).
    Link,
    /// An empty file created under the new name (step 3).
    Placeholder,
    /// An empty folder created under the new name (step 3, a folder).
    PlaceholderFolder,
}

/// Renames `from` to `to`, failing with `AlreadyExists` if `to` exists
/// (except for a case-only rename of the same file); see the module notes
/// for how, and `Unsupported` where the volume offers no exclusive way.
/// The caller syncs the folders.
pub fn rename_no_clobber(from: &Path, to: &Path) -> io::Result<Renamed> {
    rename_no_clobber_noting(from, to, |_| Ok(()))
}

/// [`rename_no_clobber`] that tells `note` of the name it claims (steps 2
/// and 3) before it goes on, so a journal can record it first. If `note`
/// fails, the claim is undone and its error returned; nothing is renamed.
pub fn rename_no_clobber_noting(
    from: &Path,
    to: &Path,
    mut note: impl FnMut(&Claim) -> io::Result<()>,
) -> io::Result<Renamed> {
    #[cfg(windows)]
    {
        // Windows MoveFileExW handles case-only renames itself. Never select
        // a replacing rename using a case-folded spelling: NTFS directories
        // can be case-sensitive, with two different entries named a.RAF and
        // A.RAF.
        let _ = &mut note;
        windows::move_exclusive(from, to).map(|()| Renamed::Exclusive)
    }
    #[cfg(unix)]
    {
        unix::rename_no_clobber(from, to, &mut note)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (from, to, &mut note);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "this system has no exclusive rename",
        ))
    }
}

/// Removes what an interrupted [`rename_no_clobber_noting`] left at `to`
/// under `claim`: only while `from` is still there (the rename did not
/// happen) and `to` is still exactly the claimed entry — the file's second
/// link under its own, separately listed name, or our placeholder, still
/// empty. Returns whether it removed it.
pub fn release_claim(from: &Path, to: &Path, claim: &Claim) -> io::Result<bool> {
    #[cfg(unix)]
    {
        unix::release_claim(from, to, claim)
    }
    #[cfg(not(unix))]
    {
        // Only Unix volumes take a claim.
        let _ = (from, to, claim);
        Ok(false)
    }
}

/// Tests (also other crates', with the `test-support` feature): forced
/// answers of the system calls the chain makes.
#[cfg(all(unix, any(test, feature = "test-support")))]
pub use unix::hook as test_hooks;

#[cfg(unix)]
mod unix {
    use super::*;
    use crate::c_path;
    use std::fs::{File, OpenOptions};
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

    type Note<'a> = &'a mut dyn FnMut(&Claim) -> io::Result<()>;

    pub(super) fn rename_no_clobber(from: &Path, to: &Path, note: Note) -> io::Result<Renamed> {
        if same_file(from, to) {
            fs::rename(from, to)?;
            return Ok(Renamed::SameFile);
        }
        match exclusive_rename(from, to) {
            Ok(()) => return Ok(Renamed::Exclusive),
            Err(e) if !rename_unsupported(&e) => return Err(e),
            Err(_) => {}
        }
        let folder = fs::symlink_metadata(from)?.is_dir();
        // A folder never has hard links.
        if !folder {
            match hard_link(from, to) {
                Ok(()) => {
                    noted(note, ClaimKind::Link, to, || {
                        if same_file(from, to) {
                            let _ = fs::remove_file(to);
                        }
                    })?;
                    return unlink_old_name(from, to).map(|()| Renamed::Linked);
                }
                Err(e) if !link_unsupported(&e) => return Err(e),
                Err(_) => {}
            }
        }
        over_placeholder(from, to, folder, note).map(|()| Renamed::OverPlaceholder)
    }

    /// Tells `note` of the entry claimed at `to`; if that fails, `undo`
    /// takes the claim back.
    fn noted(note: Note, kind: ClaimKind, to: &Path, undo: impl FnOnce()) -> io::Result<()> {
        let result = fs::symlink_metadata(to).and_then(|meta| {
            note(&Claim {
                kind,
                dev: meta.dev(),
                ino: meta.ino(),
            })
        });
        if result.is_err() {
            undo();
        }
        result
    }

    pub(super) fn release_claim(from: &Path, to: &Path, claim: &Claim) -> io::Result<bool> {
        let (Ok(source), Ok(there)) = (fs::symlink_metadata(from), fs::symlink_metadata(to)) else {
            return Ok(false);
        };
        if there.dev() != claim.dev || there.ino() != claim.ino {
            return Ok(false);
        }
        match claim.kind {
            ClaimKind::Link => {
                // Two names of one file, both listed: never the one entry
                // a case-insensitive volume shows under either spelling.
                if Id::of(&source) != Id::of(&there)
                    || there.nlink() < 2
                    || from == to
                    || !listed(from)
                    || !listed(to)
                {
                    return Ok(false);
                }
                fs::remove_file(to)?;
            }
            ClaimKind::Placeholder => {
                if !there.is_file() || there.len() != 0 {
                    return Ok(false);
                }
                fs::remove_file(to)?;
            }
            ClaimKind::PlaceholderFolder => {
                if !there.is_dir() {
                    return Ok(false);
                }
                // The system removes only an empty folder.
                match fs::remove_dir(to) {
                    Ok(()) => {}
                    Err(e)
                        if e.raw_os_error()
                            .is_some_and(|c| c == libc::ENOTEMPTY || c == libc::EEXIST) =>
                    {
                        return Ok(false);
                    }
                    Err(e) => return Err(e),
                }
            }
        }
        Ok(true)
    }

    /// Whether `path`'s folder lists an entry spelled exactly as its name.
    fn listed(path: &Path) -> bool {
        let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
            return false;
        };
        let parent = if parent.as_os_str().is_empty() {
            Path::new(".")
        } else {
            parent
        };
        fs::read_dir(parent).is_ok_and(|entries| {
            entries
                .filter_map(Result::ok)
                .any(|entry| entry.file_name() == name)
        })
    }

    fn same_file(a: &Path, b: &Path) -> bool {
        match (fs::symlink_metadata(a), fs::symlink_metadata(b)) {
            (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
            _ => false,
        }
    }

    /// The volume or system has no exclusive rename (rather than refusing
    /// this one). A guard, not a pattern: ENOTSUP and EOPNOTSUPP are one
    /// value on Linux and two on macOS.
    fn rename_unsupported(error: &io::Error) -> bool {
        error.kind() == io::ErrorKind::Unsupported
            || error.raw_os_error().is_some_and(|code| {
                [libc::ENOTSUP, libc::EOPNOTSUPP, libc::EINVAL, libc::ENOSYS].contains(&code)
            })
    }

    /// The volume has no hard links (FAT on Linux answers EPERM).
    fn link_unsupported(error: &io::Error) -> bool {
        error.raw_os_error().is_some_and(|code| {
            [libc::ENOTSUP, libc::EOPNOTSUPP, libc::EPERM, libc::ENOSYS].contains(&code)
        })
    }

    fn exclusive_rename(from: &Path, to: &Path) -> io::Result<()> {
        #[cfg(any(test, feature = "test-support"))]
        if let Some(code) = hook::take(&hook::RENAME) {
            return Err(io::Error::from_raw_os_error(code));
        }
        let (f, t) = (c_path(from)?, c_path(to)?);
        #[cfg(target_vendor = "apple")]
        // SAFETY: both are valid NUL-terminated paths for the call's duration.
        let result = unsafe { libc::renamex_np(f.as_ptr(), t.as_ptr(), libc::RENAME_EXCL) };
        #[cfg(all(target_os = "linux", any(target_env = "gnu", target_env = "musl")))]
        // SAFETY: both are valid NUL-terminated paths for the call's duration.
        let result = unsafe {
            libc::renameat2(
                libc::AT_FDCWD,
                f.as_ptr(),
                libc::AT_FDCWD,
                t.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        #[cfg(not(any(
            target_vendor = "apple",
            all(target_os = "linux", any(target_env = "gnu", target_env = "musl"))
        )))]
        let result = {
            let _ = (f, t);
            return Err(io::Error::from_raw_os_error(libc::ENOSYS));
        };
        if result == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    /// A hard link at `to` (refused if the name is taken); a symbolic link
    /// is linked itself, never its target.
    fn hard_link(from: &Path, to: &Path) -> io::Result<()> {
        #[cfg(any(test, feature = "test-support"))]
        if let Some(code) = hook::take(&hook::LINK) {
            return Err(io::Error::from_raw_os_error(code));
        }
        let (f, t) = (c_path(from)?, c_path(to)?);
        // SAFETY: both are valid NUL-terminated paths for the call's duration.
        let result =
            unsafe { libc::linkat(libc::AT_FDCWD, f.as_ptr(), libc::AT_FDCWD, t.as_ptr(), 0) };
        if result == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    /// Removes `from` after it was linked at `to`. If that fails, the new
    /// link goes again (while it is still the same file), so the file keeps
    /// its old name only.
    fn unlink_old_name(from: &Path, to: &Path) -> io::Result<()> {
        #[cfg(any(test, feature = "test-support"))]
        let removed = match hook::take(&hook::UNLINK) {
            Some(code) => Err(io::Error::from_raw_os_error(code)),
            None => fs::remove_file(from),
        };
        #[cfg(not(any(test, feature = "test-support")))]
        let removed = fs::remove_file(from);
        if let Err(error) = removed {
            if same_file(from, to) {
                let _ = fs::remove_file(to);
            }
            return Err(error);
        }
        Ok(())
    }

    /// Identity of a placeholder of ours.
    #[derive(Clone, Copy, PartialEq, Eq)]
    struct Id {
        dev: u64,
        ino: u64,
    }

    impl Id {
        fn of(meta: &fs::Metadata) -> Self {
            Self {
                dev: meta.dev(),
                ino: meta.ino(),
            }
        }
    }

    /// Claims `to` with an exclusive create, then renames `from` over that
    /// placeholder while it is still ours and empty.
    fn over_placeholder(from: &Path, to: &Path, folder: bool, note: Note) -> io::Result<()> {
        let claimed = if folder {
            claim_folder(to)
        } else {
            claim_file(to)
        };
        let (id, file) = match claimed {
            Ok(claimed) => claimed,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => return Err(e),
            Err(e) if link_unsupported(&e) || rename_unsupported(&e) => {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    format!(
                        "the volume of {} can neither rename exclusively, link nor create a \
                         file exclusively ({e}), so renaming there could replace a file",
                        to.display()
                    ),
                ));
            }
            Err(e) => return Err(e),
        };
        let kind = if folder {
            ClaimKind::PlaceholderFolder
        } else {
            ClaimKind::Placeholder
        };
        noted(note, kind, to, || remove_placeholder(to, id, folder))?;
        #[cfg(any(test, feature = "test-support"))]
        if let Some(meddle) = hook::BETWEEN.take() {
            meddle(to);
        }
        let still_ours = fs::symlink_metadata(to).is_ok_and(|meta| {
            Id::of(&meta) == id && meta.is_dir() == folder && (folder || meta.len() == 0)
        }) && file
            .as_ref()
            .is_none_or(|file| file.metadata().is_ok_and(|meta| meta.len() == 0));
        if !still_ours {
            // Taken over by another program: whatever is there now is not
            // ours to remove.
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!(
                    "{} was taken by another program during the rename",
                    to.display()
                ),
            ));
        }
        match fs::rename(from, to) {
            Ok(()) => Ok(()),
            Err(error) => {
                remove_placeholder(to, id, folder);
                // A folder placeholder that gained an entry meanwhile.
                if folder
                    && error
                        .raw_os_error()
                        .is_some_and(|c| c == libc::ENOTEMPTY || c == libc::EEXIST)
                {
                    return Err(io::Error::new(io::ErrorKind::AlreadyExists, error));
                }
                Err(error)
            }
        }
    }

    fn claim_file(to: &Path) -> io::Result<(Id, Option<File>)> {
        #[cfg(any(test, feature = "test-support"))]
        if let Some(code) = hook::take(&hook::CLAIM) {
            return Err(io::Error::from_raw_os_error(code));
        }
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(to)?;
        Ok((Id::of(&file.metadata()?), Some(file)))
    }

    fn claim_folder(to: &Path) -> io::Result<(Id, Option<File>)> {
        #[cfg(any(test, feature = "test-support"))]
        if let Some(code) = hook::take(&hook::CLAIM) {
            return Err(io::Error::from_raw_os_error(code));
        }
        fs::create_dir(to)?;
        Ok((Id::of(&fs::symlink_metadata(to)?), None))
    }

    /// Removes the placeholder at `to` only while it is still ours (and,
    /// for a file, empty; a folder is only removed empty by the system).
    fn remove_placeholder(to: &Path, id: Id, folder: bool) {
        let Ok(meta) = fs::symlink_metadata(to) else {
            return;
        };
        if Id::of(&meta) != id {
            return;
        }
        let _ = if folder {
            fs::remove_dir(to)
        } else if meta.len() == 0 {
            fs::remove_file(to)
        } else {
            Ok(())
        };
    }

    /// Tests: forced answers of the system calls, one each, and a meddler
    /// between the placeholder's check and the rename.
    #[cfg(any(test, feature = "test-support"))]
    pub mod hook {
        use std::cell::Cell;
        use std::path::Path;
        thread_local! {
            pub static RENAME: Cell<Option<i32>> = const { Cell::new(None) };
            pub static LINK: Cell<Option<i32>> = const { Cell::new(None) };
            pub static UNLINK: Cell<Option<i32>> = const { Cell::new(None) };
            pub static CLAIM: Cell<Option<i32>> = const { Cell::new(None) };
            pub static BETWEEN: Cell<Option<fn(&Path)>> = const { Cell::new(None) };
        }
        pub fn take(key: &'static std::thread::LocalKey<Cell<Option<i32>>>) -> Option<i32> {
            key.take()
        }
    }
}

#[cfg(windows)]
mod windows {
    use std::{io, path::Path};

    fn wide(path: &Path) -> io::Result<Vec<u16>> {
        use std::os::windows::ffi::OsStrExt;
        let text: Vec<u16> = path.as_os_str().encode_wide().collect();
        if text.contains(&0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} contains a NUL byte", path.display()),
            ));
        }
        Ok(text.into_iter().chain(Some(0)).collect())
    }

    /// `MoveFileExW` with no flags: no `MOVEFILE_REPLACE_EXISTING` (a taken
    /// name is refused), no `MOVEFILE_COPY_ALLOWED` (the system never copies
    /// and deletes in place of a rename, which would not be atomic).
    pub(super) fn move_exclusive(from: &Path, to: &Path) -> io::Result<()> {
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn MoveFileExW(from: *const u16, to: *const u16, flags: u32) -> i32;
        }
        const ERROR_FILE_EXISTS: i32 = 80;
        const ERROR_ALREADY_EXISTS: i32 = 183;
        let (f, t) = (wide(from)?, wide(to)?);
        // SAFETY: both are NUL-terminated wide strings that outlive the call.
        if unsafe { MoveFileExW(f.as_ptr(), t.as_ptr(), 0) } != 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        Err(match error.raw_os_error() {
            Some(ERROR_FILE_EXISTS | ERROR_ALREADY_EXISTS) => {
                io::Error::new(io::ErrorKind::AlreadyExists, error)
            }
            _ => error,
        })
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::unix::hook;
    use super::*;
    use std::os::unix::fs::MetadataExt;

    fn entries(dir: &Path) -> Vec<String> {
        let mut names: Vec<_> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// A volume without an exclusive rename (exFAT on macOS, some network
    /// mounts) and, with `link`, without hard links either.
    fn volume_without(rename: i32, link: Option<i32>) {
        hook::RENAME.set(Some(rename));
        hook::LINK.set(link);
    }

    fn reset() {
        hook::RENAME.set(None);
        hook::LINK.set(None);
        hook::UNLINK.set(None);
        hook::CLAIM.set(None);
        hook::BETWEEN.set(None);
    }

    #[test]
    fn the_exclusive_rename_moves_a_file_and_refuses_a_taken_name() {
        reset();
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        fs::write(&a, "a").unwrap();
        assert_eq!(rename_no_clobber(&a, &b).unwrap(), Renamed::Exclusive);
        fs::write(&a, "other").unwrap();
        let error = rename_no_clobber(&a, &b).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&a).unwrap(), b"other");
        assert_eq!(fs::read(&b).unwrap(), b"a");
    }

    #[test]
    fn the_same_file_under_another_name_is_renamed_in_place() {
        reset();
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        fs::write(&a, "a").unwrap();
        fs::hard_link(&a, &b).unwrap();
        assert_eq!(rename_no_clobber(&a, &b).unwrap(), Renamed::SameFile);
        assert_eq!(fs::read(&b).unwrap(), b"a");
    }

    #[test]
    fn without_an_exclusive_rename_a_file_is_linked_and_its_old_name_removed() {
        for errno in [libc::ENOTSUP, libc::EOPNOTSUPP, libc::EINVAL, libc::ENOSYS] {
            reset();
            let dir = tempfile::tempdir().unwrap();
            let (a, b) = (dir.path().join("a.ARW"), dir.path().join("b.ARW"));
            fs::write(&a, "raw").unwrap();
            let inode = fs::metadata(&a).unwrap().ino();
            volume_without(errno, None);
            assert_eq!(
                rename_no_clobber(&a, &b).unwrap(),
                Renamed::Linked,
                "{errno}"
            );
            assert_eq!(fs::read(&b).unwrap(), b"raw");
            assert_eq!(fs::metadata(&b).unwrap().ino(), inode);
            assert_eq!(fs::metadata(&b).unwrap().nlink(), 1);
            assert_eq!(entries(dir.path()), ["b.ARW"]);
        }
    }

    #[test]
    fn a_link_onto_a_taken_name_is_refused_and_changes_nothing() {
        reset();
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        fs::write(&a, "a").unwrap();
        fs::write(&b, "the user's").unwrap();
        volume_without(libc::ENOTSUP, None);
        let error = rename_no_clobber(&a, &b).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&a).unwrap(), b"a");
        assert_eq!(fs::read(&b).unwrap(), b"the user's");
    }

    #[test]
    fn a_link_whose_old_name_cannot_go_is_taken_back() {
        reset();
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        fs::write(&a, "a").unwrap();
        volume_without(libc::ENOTSUP, None);
        hook::UNLINK.set(Some(libc::EIO));
        let error = rename_no_clobber(&a, &b).unwrap_err();
        assert_eq!(error.raw_os_error(), Some(libc::EIO));
        assert_eq!(entries(dir.path()), ["a"]);
        assert_eq!(fs::read(&a).unwrap(), b"a");
    }

    #[test]
    fn without_hard_links_a_file_is_renamed_over_its_own_placeholder() {
        for link in [libc::ENOTSUP, libc::EOPNOTSUPP, libc::EPERM] {
            reset();
            let dir = tempfile::tempdir().unwrap();
            let (a, b) = (dir.path().join("a.ARW"), dir.path().join("b.ARW"));
            fs::write(&a, "raw").unwrap();
            let inode = fs::metadata(&a).unwrap().ino();
            volume_without(libc::ENOTSUP, Some(link));
            let how = rename_no_clobber(&a, &b).unwrap();
            assert_eq!(how, Renamed::OverPlaceholder, "{link}");
            assert_eq!(fs::read(&b).unwrap(), b"raw");
            assert_eq!(
                fs::metadata(&b).unwrap().ino(),
                inode,
                "the file itself, not a copy"
            );
            assert_eq!(entries(dir.path()), ["b.ARW"]);
        }
    }

    #[test]
    fn a_placeholder_onto_a_taken_name_is_refused_and_changes_nothing() {
        reset();
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        fs::write(&a, "a").unwrap();
        fs::write(&b, "the user's").unwrap();
        volume_without(libc::ENOTSUP, Some(libc::ENOTSUP));
        let error = rename_no_clobber(&a, &b).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&a).unwrap(), b"a");
        assert_eq!(fs::read(&b).unwrap(), b"the user's");
    }

    /// Another program takes the name between our claim and the rename:
    /// its file is never replaced, and not removed either.
    #[test]
    fn a_placeholder_replaced_meanwhile_is_never_renamed_over() {
        reset();
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        fs::write(&a, "a").unwrap();
        volume_without(libc::ENOTSUP, Some(libc::ENOTSUP));
        hook::BETWEEN.set(Some(|to: &Path| {
            fs::remove_file(to).unwrap();
            fs::write(to, "another program's").unwrap();
        }));
        let error = rename_no_clobber(&a, &b).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&a).unwrap(), b"a");
        assert_eq!(fs::read(&b).unwrap(), b"another program's");
    }

    /// Another program writes into our placeholder before the check: its
    /// bytes are kept, nothing is renamed over them.
    #[test]
    fn a_placeholder_written_to_meanwhile_is_kept() {
        reset();
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        fs::write(&a, "a").unwrap();
        volume_without(libc::ENOTSUP, Some(libc::ENOTSUP));
        hook::BETWEEN.set(Some(|to: &Path| fs::write(to, "written in").unwrap()));
        let error = rename_no_clobber(&a, &b).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&a).unwrap(), b"a");
        assert_eq!(fs::read(&b).unwrap(), b"written in");
    }

    #[test]
    fn a_failed_rename_over_a_placeholder_removes_only_the_placeholder() {
        reset();
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        fs::write(&a, "a").unwrap();
        volume_without(libc::ENOTSUP, Some(libc::ENOTSUP));
        // The file to rename goes away before the rename.
        hook::BETWEEN.set(Some(|to: &Path| {
            fs::rename(to.with_file_name("a"), to.with_file_name("moved")).unwrap()
        }));
        let error = rename_no_clobber(&a, &b).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert_eq!(entries(dir.path()), ["moved"]);
    }

    #[test]
    fn a_folder_is_renamed_over_an_empty_placeholder_folder() {
        reset();
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("stage"), dir.path().join("Job"));
        fs::create_dir(&a).unwrap();
        fs::write(a.join("photo.ARW"), "raw").unwrap();
        // A folder is never hard-linked: no link answer is needed.
        volume_without(libc::ENOTSUP, None);
        assert_eq!(rename_no_clobber(&a, &b).unwrap(), Renamed::OverPlaceholder);
        assert_eq!(fs::read(b.join("photo.ARW")).unwrap(), b"raw");
        assert_eq!(entries(dir.path()), ["Job"]);
        // A taken name.
        fs::create_dir(&a).unwrap();
        volume_without(libc::ENOTSUP, None);
        let error = rename_no_clobber(&a, &b).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(entries(&b), ["photo.ARW"]);
    }

    #[test]
    fn a_placeholder_folder_that_gained_an_entry_is_refused_by_the_system() {
        reset();
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("stage"), dir.path().join("Job"));
        fs::create_dir(&a).unwrap();
        fs::write(a.join("photo.ARW"), "raw").unwrap();
        volume_without(libc::ENOTSUP, None);
        // After the check, before the rename: simulated by meddling before
        // the check with an entry the check does not look at.
        hook::BETWEEN.set(Some(|to: &Path| fs::write(to.join("theirs"), "x").unwrap()));
        let error = rename_no_clobber(&a, &b).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(b.join("theirs")).unwrap(), b"x");
        assert_eq!(fs::read(a.join("photo.ARW")).unwrap(), b"raw");
    }

    #[test]
    fn a_volume_that_cannot_even_create_exclusively_is_unsupported() {
        reset();
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        fs::write(&a, "a").unwrap();
        volume_without(libc::ENOTSUP, Some(libc::ENOTSUP));
        hook::CLAIM.set(Some(libc::ENOTSUP));
        let error = rename_no_clobber(&a, &b).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
        assert!(
            error.to_string().contains("could replace a file"),
            "{error}"
        );
        assert_eq!(entries(dir.path()), ["a"]);
    }

    #[test]
    fn other_errors_are_not_retried_another_way() {
        reset();
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        fs::write(&a, "a").unwrap();
        volume_without(libc::ENOSPC, None);
        assert_eq!(
            rename_no_clobber(&a, &b).unwrap_err().raw_os_error(),
            Some(libc::ENOSPC)
        );
        volume_without(libc::ENOTSUP, Some(libc::EXDEV));
        assert_eq!(
            rename_no_clobber(&a, &b).unwrap_err().raw_os_error(),
            Some(libc::EXDEV)
        );
        assert_eq!(entries(dir.path()), ["a"]);
    }

    /// The chain on a real volume: `RAWTONIC_FS_TEST_DIR` names a folder
    /// on it (an exFAT, FAT32 or APFS disk image; default a temporary
    /// folder). Prints the step each rename took.
    #[test]
    fn renames_on_the_volume_of_rawtonic_fs_test_dir() {
        reset();
        let base = std::env::var_os("RAWTONIC_FS_TEST_DIR").map(std::path::PathBuf::from);
        let dir = match &base {
            Some(base) => tempfile::tempdir_in(base).unwrap(),
            None => tempfile::tempdir().unwrap(),
        };
        let (a, b) = (dir.path().join("a.ARW"), dir.path().join("b.ARW"));
        fs::write(&a, "raw").unwrap();
        let how = rename_no_clobber(&a, &b).unwrap();
        eprintln!("file to a free name: {how:?}");
        assert_eq!(fs::read(&b).unwrap(), b"raw");
        fs::write(&a, "other").unwrap();
        let error = rename_no_clobber(&a, &b).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists, "{error}");
        assert_eq!(fs::read(&a).unwrap(), b"other");
        assert_eq!(fs::read(&b).unwrap(), b"raw");
        let (f, g) = (dir.path().join("stage"), dir.path().join("Job"));
        fs::create_dir(&f).unwrap();
        fs::write(f.join("x"), "x").unwrap();
        let how = rename_no_clobber(&f, &g).unwrap();
        eprintln!("folder to a free name: {how:?}");
        assert_eq!(fs::read(g.join("x")).unwrap(), b"x");
        fs::create_dir(&f).unwrap();
        let error = rename_no_clobber(&f, &g).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists, "{error}");
        // The forced steps on this volume, where it supports them.
        for (label, rename, link) in [
            ("link", libc::ENOTSUP, None),
            ("placeholder", libc::ENOTSUP, Some(libc::ENOTSUP)),
        ] {
            let (c, d) = (
                dir.path().join(format!("{label}-a")),
                dir.path().join(format!("{label}-b")),
            );
            fs::write(&c, label).unwrap();
            volume_without(rename, link);
            match rename_no_clobber(&c, &d) {
                Ok(how) => {
                    eprintln!("forced {label}: {how:?}");
                    assert_eq!(fs::read(&d).unwrap(), label.as_bytes());
                }
                Err(e) => eprintln!("forced {label}: {e} ({:?})", e.kind()),
            }
            reset();
        }
        let mut left = entries(dir.path());
        left.sort();
        eprintln!("left: {left:?}");
        assert!(!left.iter().any(|n| n.starts_with(".rawtonic")), "{left:?}");
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn native_no_clobber_retains_case_only_rename_on_ordinary_windows_directory() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("a.RAF");
        let destination = temp.path().join("A.RAF");
        fs::write(&source, b"synthetic original").unwrap();
        rename_no_clobber(&source, &destination).unwrap();
        assert_eq!(fs::read(&destination).unwrap(), b"synthetic original");
        let names: Vec<_> = fs::read_dir(temp.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, [std::ffi::OsString::from("A.RAF")]);
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "Enables case sensitivity only on a fresh synthetic directory; run explicitly on NTFS"]
    fn native_no_clobber_refuses_distinct_case_sensitive_windows_entries() {
        let temp = tempfile::tempdir().unwrap();
        let enabled = std::process::Command::new("fsutil.exe")
            .args(["file", "setCaseSensitiveInfo"])
            .arg(temp.path())
            .arg("enable")
            .output()
            .unwrap();
        assert!(
            enabled.status.success(),
            "Case-sensitive NTFS setup is unavailable: {}",
            String::from_utf8_lossy(&enabled.stdout)
        );
        let source = temp.path().join("a.RAF");
        let destination = temp.path().join("A.RAF");
        fs::write(&source, b"source").unwrap();
        fs::write(&destination, b"occupied").unwrap();
        assert!(rename_no_clobber(&source, &destination).is_err());
        assert_eq!(fs::read(&source).unwrap(), b"source");
        assert_eq!(fs::read(&destination).unwrap(), b"occupied");
    }
}

#[cfg(all(test, unix))]
mod claim_tests {
    use super::unix::hook;
    use super::*;

    fn volume_without(link: Option<i32>) {
        hook::RENAME.set(Some(libc::ENOTSUP));
        hook::LINK.set(link);
        hook::UNLINK.set(None);
        hook::CLAIM.set(None);
        hook::BETWEEN.set(None);
    }

    fn noted(from: &Path, to: &Path) -> (io::Result<Renamed>, Vec<Claim>) {
        let mut claims = Vec::new();
        let result = rename_no_clobber_noting(from, to, |claim| {
            claims.push(*claim);
            Ok(())
        });
        (result, claims)
    }

    #[test]
    fn the_exclusive_rename_claims_nothing_and_the_fallbacks_note_their_claim() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        fs::write(&a, "a").unwrap();
        hook::RENAME.set(None);
        assert_eq!(noted(&a, &b).1, []);
        volume_without(None);
        let (result, claims) = noted(&b, &a);
        assert_eq!(result.unwrap(), Renamed::Linked);
        assert_eq!(claims.len(), 1);
        assert_eq!(claims[0].kind, ClaimKind::Link);
        volume_without(Some(libc::ENOTSUP));
        let (result, claims) = noted(&a, &b);
        assert_eq!(result.unwrap(), Renamed::OverPlaceholder);
        assert_eq!(claims.len(), 1);
        assert_eq!(claims[0].kind, ClaimKind::Placeholder);
        let folder = dir.path().join("stage");
        fs::create_dir(&folder).unwrap();
        volume_without(None);
        let (_, claims) = noted(&folder, &dir.path().join("Job"));
        assert_eq!(claims[0].kind, ClaimKind::PlaceholderFolder);
    }

    /// A journal that cannot record the claim stops the rename and takes
    /// the claim back.
    #[test]
    fn a_claim_that_cannot_be_noted_is_taken_back() {
        for link in [None, Some(libc::ENOTSUP)] {
            let dir = tempfile::tempdir().unwrap();
            let (a, b) = (dir.path().join("a"), dir.path().join("b"));
            fs::write(&a, "a").unwrap();
            volume_without(link);
            let error = rename_no_clobber_noting(&a, &b, |_| Err(io::Error::other("journal full")))
                .unwrap_err();
            assert_eq!(error.to_string(), "journal full");
            assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
            assert_eq!(fs::read(&a).unwrap(), b"a");
        }
    }

    /// What a crash between the claim and the rename leaves: the noted
    /// claim is removed, and only while it is still that entry.
    #[test]
    fn an_interrupted_claim_is_released_only_while_it_is_still_ours() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        fs::write(&a, "a").unwrap();
        let claim_of = |kind, path: &Path| {
            use std::os::unix::fs::MetadataExt;
            let meta = fs::symlink_metadata(path).unwrap();
            Claim {
                kind,
                dev: meta.dev(),
                ino: meta.ino(),
            }
        };
        // A crash after the link.
        fs::hard_link(&a, &b).unwrap();
        let link = claim_of(ClaimKind::Link, &b);
        assert!(release_claim(&a, &b, &link).unwrap());
        assert!(!b.exists() && fs::read(&a).unwrap() == b"a");
        // After the unlink the link is the file: never removed.
        fs::hard_link(&a, &b).unwrap();
        fs::remove_file(&a).unwrap();
        assert!(!release_claim(&a, &b, &link).unwrap());
        assert_eq!(fs::read(&b).unwrap(), b"a");
        fs::rename(&b, &a).unwrap();
        // A crash after the placeholder.
        fs::write(&b, "").unwrap();
        let placeholder = claim_of(ClaimKind::Placeholder, &b);
        assert!(release_claim(&a, &b, &placeholder).unwrap());
        assert!(!b.exists());
        // Written to since, or another file under the name: kept.
        fs::write(&b, "").unwrap();
        let placeholder = claim_of(ClaimKind::Placeholder, &b);
        fs::write(&b, "theirs").unwrap();
        assert!(!release_claim(&a, &b, &placeholder).unwrap());
        fs::remove_file(&b).unwrap();
        fs::write(&b, "").unwrap();
        let now = claim_of(ClaimKind::Placeholder, &b);
        let theirs = Claim {
            ino: now.ino ^ 1,
            ..now
        };
        assert!(!release_claim(&a, &b, &theirs).unwrap());
        // The rename happened (no file at the old name): nothing to do.
        let ours = claim_of(ClaimKind::Placeholder, &b);
        fs::remove_file(&a).unwrap();
        assert!(!release_claim(&a, &b, &ours).unwrap());
        assert!(b.exists());
        // A folder placeholder: removed empty, kept with an entry.
        let (f, g) = (dir.path().join("stage"), dir.path().join("Job"));
        fs::create_dir(&f).unwrap();
        fs::create_dir(&g).unwrap();
        let folder = claim_of(ClaimKind::PlaceholderFolder, &g);
        fs::write(g.join("theirs"), "x").unwrap();
        assert!(!release_claim(&f, &g, &folder).unwrap());
        fs::remove_file(g.join("theirs")).unwrap();
        assert!(release_claim(&f, &g, &folder).unwrap());
        assert!(!g.exists());
    }
}
