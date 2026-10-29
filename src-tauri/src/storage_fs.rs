// SPDX-License-Identifier: AGPL-3.0-or-later
//! Cache maintenance. Callers supply an owned root and a strict descendant,
//! never a user-selected deletion path. Descendant links are never followed.
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

static IN_USE_FILES: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<PathBuf, usize>>,
> = std::sync::LazyLock::new(Default::default);

pub(crate) struct FileUse(PathBuf);
impl Drop for FileUse {
    fn drop(&mut self) {
        let mut files = IN_USE_FILES.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(count) = files.get_mut(&self.0) {
            *count -= 1;
            if *count == 0 {
                files.remove(&self.0);
            }
        }
    }
}
fn file_key(path: &Path) -> PathBuf {
    path.parent()
        .and_then(|parent| parent.canonicalize().ok())
        .and_then(|parent| path.file_name().map(|name| parent.join(name)))
        .unwrap_or_else(|| path.to_path_buf())
}
pub(crate) fn pin_file(path: &Path) -> FileUse {
    let key = file_key(path);
    *IN_USE_FILES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .entry(key.clone())
        .or_default() += 1;
    FileUse(key)
}
fn while_unpinned<T>(path: &Path, work: impl FnOnce() -> T) -> Option<T> {
    let files = IN_USE_FILES.lock().unwrap_or_else(|e| e.into_inner());
    if files.get(&file_key(path)).copied().unwrap_or(0) > 0 {
        return None;
    }
    Some(work())
}
pub(crate) fn remove_if_unpinned(path: &Path) -> bool {
    while_unpinned(path, || fs::remove_file(path).is_ok()).unwrap_or(false)
}

#[derive(Clone, Debug, Default)]
pub struct Usage {
    pub bytes: u64,
    pub files: u64,
    pub incomplete: bool,
}

pub fn owned_directory(root: &Path, directory: &Path) -> io::Result<PathBuf> {
    // Derive the relative path before canonicalizing the root. Resolving the
    // candidate's parent first would hide a symlink through a sibling such
    // as <session>/.cache/link -> <session>/Capture.
    let canonical_root = root.canonicalize()?;
    let relative = directory
        .strip_prefix(root)
        .or_else(|_| directory.strip_prefix(&canonical_root))
        .map_err(|_| refused())?
        .to_path_buf();
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err(refused());
    }
    let mut current = canonical_root;
    for part in relative.components() {
        current.push(part);
        let metadata = fs::symlink_metadata(&current)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(refused());
        }
    }
    Ok(current)
}

fn refused() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "Refusing a path outside the owned cache directory or through a link",
    )
}

/// Files and directories may disappear between read_dir and stat when an
/// atomic cache writer publishes or drops its staging file. On Windows a
/// concurrent writer also makes a listed entry vanish before its stat
/// (ERROR_FILE_INVALID) and closes a folder being replaced to others
/// (ERROR_ACCESS_DENIED), which hides the entry; skipping it is no error.
/// A folder that cannot be listed at all still is one.
fn if_present<T>(result: io::Result<T>) -> io::Result<Option<T>> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        #[cfg(windows)]
        Err(error) if a_concurrent_writer_hides(error.kind()) => Ok(None),
        Err(error) => Err(error),
    }
}

/// `if_present` for the folder a walk starts from: none once it is gone.
#[cfg(windows)]
fn if_vanished<T>(result: io::Result<T>) -> io::Result<Option<T>> {
    if_present(result)
}

/// The Windows errors a concurrent cache writer causes on paths it
/// publishes to (a definitive answer is still `NotFound`).
#[cfg(windows)]
fn a_concurrent_writer_hides(kind: io::ErrorKind) -> bool {
    matches!(kind, io::ErrorKind::PermissionDenied | io::ErrorKind::Other)
        || kind == io::ErrorKind::InvalidData // ERROR_FILE_INVALID: deleted meanwhile
}

pub fn usage(directory: &Path, cancelled: &AtomicBool, budget: Duration) -> io::Result<Usage> {
    if !directory.exists() {
        return Ok(Usage::default());
    }
    #[cfg(windows)]
    if if_vanished(fs::symlink_metadata(directory))?.is_none() {
        return Ok(Usage::default());
    }
    if fs::symlink_metadata(directory)?.file_type().is_symlink() {
        return Err(refused());
    }
    let started = Instant::now();
    let mut result = Usage::default();
    let mut pending = vec![directory.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Some(entries) = if_present(fs::read_dir(dir))? else {
            continue;
        };
        for entry in entries {
            if cancelled.load(Ordering::Relaxed) {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "Storage scan cancelled",
                ));
            }
            if started.elapsed() >= budget {
                result.incomplete = true;
                return Ok(result);
            }
            let Some(entry) = if_present(entry)? else {
                continue;
            };
            let Some(metadata) = if_present(fs::symlink_metadata(entry.path()))? else {
                continue;
            };
            if metadata.file_type().is_symlink() {
                continue;
            }
            if metadata.is_dir() {
                pending.push(entry.path());
            } else if metadata.is_file() {
                result.bytes = result.bytes.saturating_add(metadata.len());
                result.files += 1;
            }
        }
    }
    Ok(result)
}

/// Deletes only regular files directly inside an owned cache. Nested folders
/// are intentionally left alone: a cache must opt into each known subfolder.
/// Pinned/in-flight entries and staging files are not eligible.
pub fn evict(
    root: &Path,
    directory: &Path,
    limit: u64,
    eligible: impl Fn(&Path) -> bool,
    pinned: impl Fn(&Path) -> bool,
) -> io::Result<Usage> {
    let Some(directory) = if_present(owned_directory(root, directory))? else {
        return Ok(Usage::default());
    };
    let mut files = Vec::new();
    let mut total = Usage::default();
    let Some(entries) = if_present(fs::read_dir(&directory))? else {
        return Ok(total);
    };
    for entry in entries {
        let Some(entry) = if_present(entry)? else {
            continue;
        };
        let Some(meta) = if_present(fs::symlink_metadata(entry.path()))? else {
            continue;
        };
        if !meta.is_file() || meta.file_type().is_symlink() {
            continue;
        }
        total.bytes = total.bytes.saturating_add(meta.len());
        total.files += 1;
        if eligible(&entry.path()) && !pinned(&entry.path()) {
            files.push((
                meta.modified().unwrap_or(UNIX_EPOCH),
                entry.path(),
                meta.len(),
            ));
        }
    }
    files.sort();
    for (_, path, bytes) in files {
        if total.bytes <= limit {
            break;
        }
        let name = path.file_name().ok_or_else(refused)?;
        // A worker may have used this entry after the listing. Recheck its
        // recent/staging pin while the in-use registry excludes new readers.
        let removed = while_unpinned(&path, || -> io::Result<bool> {
            if pinned(&path) {
                return Ok(false);
            }
            remove_owned_file(root, &directory, name)?;
            Ok(true)
        })
        .transpose();
        let removed = match removed {
            Ok(removed) => removed.unwrap_or(false),
            Err(error) => {
                log::debug!("Cache entry could not be removed: {error}");
                total.incomplete = true;
                continue;
            }
        };
        if !removed {
            continue;
        }
        total.bytes = total.bytes.saturating_sub(bytes);
        total.files -= 1;
    }
    Ok(total)
}

#[cfg(unix)]
fn remove_owned_file(root: &Path, directory: &Path, name: &std::ffi::OsStr) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::OpenOptionsExt;
    let root = root.canonicalize()?;
    let relative = directory.strip_prefix(&root).map_err(|_| refused())?;
    let mut handle: OwnedFd = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(&root)?
        .into();
    for component in relative.components() {
        let Component::Normal(part) = component else {
            return Err(refused());
        };
        let part = CString::new(part.as_bytes()).map_err(|_| refused())?;
        // Open every ancestor relative to the held directory, so replacing a
        // directory by a symlink during maintenance cannot redirect deletion.
        let fd = unsafe {
            libc::openat(
                handle.as_raw_fd(),
                part.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        handle = unsafe { OwnedFd::from_raw_fd(fd) };
    }
    let name = CString::new(name.as_bytes()).map_err(|_| refused())?;
    if unsafe { libc::unlinkat(handle.as_raw_fd(), name.as_ptr(), 0) } != 0 {
        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::NotFound {
            return Err(err);
        }
    }
    Ok(())
}

#[cfg(windows)]
fn remove_owned_file(root: &Path, directory: &Path, name: &std::ffi::OsStr) -> io::Result<()> {
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
    let root = root.canonicalize()?;
    let relative = directory.strip_prefix(&root).map_err(|_| refused())?;
    let mut current = root.clone();
    let mut handles = Vec::new();
    // Somebody else (another maintenance pass finished first) may drop the
    // folder with its files between the checks above and here: then there
    // is nothing left to remove. All other errors are reported.
    let gone = |error: &io::Error| error.kind() == io::ErrorKind::NotFound;
    let pin_directory = |path: &Path| -> io::Result<fs::File> {
        // OPEN_REPARSE_POINT + BACKUP_SEMANTICS; omit FILE_SHARE_DELETE so
        // ancestors cannot be renamed while a child is removed.
        let file = fs::OpenOptions::new()
            .read(true)
            .share_mode(3)
            .custom_flags(0x0020_0000 | 0x0200_0000)
            .open(path)?;
        let metadata = file.metadata()?;
        if !metadata.is_dir() || metadata.file_attributes() & 0x400 != 0 {
            return Err(refused());
        }
        Ok(file)
    };
    match pin_directory(&current) {
        Ok(handle) => handles.push(handle),
        Err(error) if gone(&error) => return Ok(()),
        Err(error) => return Err(error),
    }
    for part in relative.components() {
        let Component::Normal(part) = part else {
            return Err(refused());
        };
        current.push(part);
        match pin_directory(&current) {
            Ok(handle) => handles.push(handle),
            Err(error) if gone(&error) => return Ok(()),
            Err(error) => return Err(error),
        }
    }
    // Anything but "already gone" is reported: the folder is pinned and
    // the entry sits in it, so a denial is the entry's own (Windows reports
    // a read-only attribute that way), a file a reader still holds is a
    // sharing violation, and the count of files left says so.
    match fs::remove_file(current.join(name)) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

#[cfg(not(any(unix, windows)))]
fn remove_owned_file(_root: &Path, _directory: &Path, _name: &std::ffi::OsStr) -> io::Result<()> {
    // No path-based fallback: deletion needs an anchored directory handle.
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Safe cache removal is unavailable on this platform",
    ))
}

pub fn mark_used(path: &Path) {
    if let Ok(file) = open_cache_file(path) {
        let _ = file.set_times(fs::FileTimes::new().set_modified(SystemTime::now()));
    }
}

#[cfg(unix)]
fn open_cache_file(path: &Path) -> io::Result<fs::File> {
    use std::ffi::CString;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::{ffi::OsStrExt, fs::OpenOptionsExt};
    let directory = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(path.parent().ok_or_else(refused)?)?;
    let name =
        CString::new(path.file_name().ok_or_else(refused)?.as_bytes()).map_err(|_| refused())?;
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { fs::File::from_raw_fd(fd) })
}

#[cfg(windows)]
fn open_cache_file(path: &Path) -> io::Result<fs::File> {
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
    let parent = fs::OpenOptions::new()
        .read(true)
        .share_mode(3)
        .custom_flags(0x0020_0000 | 0x0200_0000)
        .open(path.parent().ok_or_else(refused)?)?;
    if parent.metadata()?.file_attributes() & 0x400 != 0 {
        return Err(refused());
    }
    let file = fs::OpenOptions::new()
        .access_mode(0x8000_0100) // GENERIC_READ | FILE_WRITE_ATTRIBUTES
        .custom_flags(0x0020_0000)
        .open(path)?;
    if file.metadata()?.file_attributes() & 0x400 != 0 {
        return Err(refused());
    }
    Ok(file)
}

#[cfg(not(any(unix, windows)))]
fn open_cache_file(_path: &Path) -> io::Result<fs::File> {
    Err(refused())
}

/// Recurses only through validated cache subdirectories, never through links.
/// The root itself and directory structure stay in place for active writers.
pub fn clear_tree(
    root: &Path,
    directory: &Path,
    pinned: &impl Fn(&Path) -> bool,
) -> io::Result<()> {
    let Some(directory) = if_present(owned_directory(root, directory))? else {
        return Ok(());
    };
    let mut failure = None;
    if let Some(entries) = if_present(fs::read_dir(&directory))? {
        for entry in entries {
            let Some(entry) = if_present(entry)? else {
                continue;
            };
            let Some(metadata) = if_present(fs::symlink_metadata(entry.path()))? else {
                continue;
            };
            if metadata.is_dir()
                && !metadata.file_type().is_symlink()
                && let Err(error) = clear_tree(root, &entry.path(), pinned)
            {
                failure.get_or_insert(error);
            }
        }
    }
    match evict(root, &directory, 0, |_| true, pinned) {
        Ok(remaining) if remaining.incomplete => {
            failure
                .get_or_insert_with(|| io::Error::other("Some cache files could not be removed"));
        }
        Err(error) => {
            failure.get_or_insert(error);
        }
        _ => {}
    }
    failure.map_or(Ok(()), Err)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let cache = root.join("thumbnails");
        fs::create_dir(&cache).unwrap();
        (temp, root, cache)
    }
    #[test]
    fn measures_nested_files_and_cancels() {
        let (_temp, _root, cache) = fixture();
        fs::create_dir(cache.join("nested")).unwrap();
        fs::write(cache.join("a"), [0; 3]).unwrap();
        fs::write(cache.join("nested/b"), [0; 5]).unwrap();
        let cancel = AtomicBool::new(false);
        let measured = usage(&cache, &cancel, Duration::from_secs(2)).unwrap();
        assert_eq!((measured.bytes, measured.files), (8, 2));
        cancel.store(true, Ordering::Relaxed);
        assert_eq!(
            usage(&cache, &cancel, Duration::from_secs(2))
                .unwrap_err()
                .kind(),
            io::ErrorKind::Interrupted
        );
        assert!(
            usage(&cache, &AtomicBool::new(false), Duration::ZERO)
                .unwrap()
                .incomplete
        );
    }
    #[test]
    fn size_walk_tolerates_staging_files_disappearing() {
        let (_temp, _root, cache) = fixture();
        let stop = AtomicBool::new(false);
        let ready = std::sync::Barrier::new(2);
        for index in 0..100 {
            fs::write(cache.join(format!("stable-{index}")), b"cache").unwrap();
        }
        std::thread::scope(|scope| {
            scope.spawn(|| {
                let first = tempfile::NamedTempFile::new_in(&cache).unwrap();
                ready.wait();
                drop(first);
                while !stop.load(Ordering::Relaxed) {
                    let mut file = tempfile::NamedTempFile::new_in(&cache).unwrap();
                    use std::io::Write;
                    file.write_all(b"temporary cache data").unwrap();
                }
            });
            ready.wait();
            let mut failure = None;
            for _ in 0..1000 {
                if let Err(error) = usage(&cache, &AtomicBool::new(false), Duration::from_secs(2)) {
                    failure = Some(error);
                    break;
                }
            }
            stop.store(true, Ordering::Relaxed);
            assert!(
                failure.is_none(),
                "a vanished staging file aborted the size walk: {failure:?}"
            );
        });
    }

    /// A folder fulfilling every rule removes itself just before its files
    /// are: as a concurrent writer's publish-and-replace does on Windows,
    /// where the removal then meets a folder that is gone. That is not a
    /// failure; what is there is removed, as `storage_clear`'s own pinned
    /// check keeps a folder that is still there.
    #[test]
    fn an_entry_of_a_folder_that_vanishes_first_is_not_a_failure() {
        let (_temp, root, cache) = fixture();
        let nested = cache.join("nested");
        fs::create_dir(&nested).unwrap();
        fs::write(nested.join("result"), b"pixels").unwrap();
        let vanished = |path: &Path| {
            path.parent() == Some(nested.as_path()) && fs::remove_dir_all(&nested).is_ok()
        };
        let remaining = evict(&root, &cache, 0, vanished, |_| false).unwrap();
        assert!(!remaining.incomplete, "{remaining:?}");
        assert!(!nested.exists());
        assert_eq!(
            usage(&cache, &AtomicBool::new(false), Duration::from_secs(2))
                .unwrap()
                .bytes,
            0
        );
    }

    #[test]
    fn clear_refuses_root_parent_and_outside_paths() {
        let (_temp, root, cache) = fixture();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("photo.RAW"), b"original").unwrap();
        for path in [
            &root,
            root.parent().unwrap(),
            outside.path(),
            &cache.join(".."),
        ] {
            assert!(evict(&root, path, 0, |_| true, |_| false).is_err());
        }
        assert_eq!(
            fs::read(outside.path().join("photo.RAW")).unwrap(),
            b"original"
        );
    }
    #[cfg(unix)]
    #[test]
    fn linked_roots_ancestors_and_children_never_delete_the_target() {
        use std::os::unix::fs::symlink;
        let (_temp, root, cache) = fixture();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("photo.RAW"), b"original").unwrap();
        symlink(outside.path(), root.join("alias")).unwrap();
        symlink(outside.path().join("photo.RAW"), cache.join("link")).unwrap();
        assert!(evict(&root, &root.join("alias"), 0, |_| true, |_| false).is_err());
        evict(&root, &cache, 0, |_| true, |_| false).unwrap();
        assert_eq!(
            usage(&cache, &AtomicBool::new(false), Duration::from_secs(2))
                .unwrap()
                .bytes,
            0
        );
        assert_eq!(
            fs::read(outside.path().join("photo.RAW")).unwrap(),
            b"original"
        );
    }
    #[test]
    fn lru_keeps_recent_pinned_inflight_and_staging_files() {
        let (_temp, root, cache) = fixture();
        for (name, used) in [
            ("old.jpg", 2),
            ("recent.jpg", 6),
            ("pinned.jpg", 1),
            ("busy.jpg", 1),
            (".staging", 1),
        ] {
            let path = cache.join(name);
            fs::write(&path, [0; 4]).unwrap();
            fs::File::open(path)
                .unwrap()
                .set_times(
                    fs::FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(used)),
                )
                .unwrap();
        }
        let remaining = evict(
            &root,
            &cache,
            16,
            |p| p.extension().is_some_and(|e| e == "jpg"),
            |p| {
                matches!(
                    p.file_name().unwrap().to_str().unwrap(),
                    "pinned.jpg" | "busy.jpg"
                )
            },
        )
        .unwrap();
        assert_eq!(remaining.bytes, 16);
        assert!(!cache.join("old.jpg").exists());
        for name in ["recent.jpg", "pinned.jpg", "busy.jpg", ".staging"] {
            assert!(cache.join(name).exists());
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_link_through_a_sibling_never_authorizes_a_photo_directory() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let cache = root.join(".cache");
        let photos = root.join("Capture/subfolder");
        fs::create_dir_all(&cache).unwrap();
        fs::create_dir_all(&photos).unwrap();
        fs::write(photos.join("original.RAW"), b"original").unwrap();
        symlink(root.join("Capture"), cache.join("alias")).unwrap();
        assert!(
            evict(
                &root,
                &cache.join("alias/subfolder"),
                0,
                |_| true,
                |_| false
            )
            .is_err()
        );
        assert_eq!(fs::read(photos.join("original.RAW")).unwrap(), b"original");
    }

    #[cfg(unix)]
    #[test]
    fn active_reader_survives_clear_until_its_lease_ends() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let cache = root.join("previews");
        fs::create_dir(&cache).unwrap();
        let path = cache.join("preview.rtpx");
        fs::write(&path, b"pixels").unwrap();
        let reader = pin_file(&path);
        assert_eq!(
            evict(&root, &cache, 0, |_| true, |_| false).unwrap().bytes,
            6
        );
        assert!(!remove_if_unpinned(&path));
        drop(reader);
        assert_eq!(
            evict(&root, &cache, 0, |_| true, |_| false).unwrap().bytes,
            0
        );
        assert!(!path.exists());
    }
    #[cfg(unix)]
    #[test]
    fn owned_root_aliases_and_canonical_descendants_work_together() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let owned = root.join("owned");
        let alias = root.join("alias");
        fs::create_dir_all(owned.join("cache/nested")).unwrap();
        fs::write(owned.join("cache/nested/result"), b"pixels").unwrap();
        symlink(&owned, &alias).unwrap();
        clear_tree(&alias, &alias.join("cache"), &|_| false).unwrap();
        assert!(!owned.join("cache/nested/result").exists());
    }

    #[cfg(unix)]
    #[test]
    fn an_entry_pinned_after_listing_is_kept_even_above_the_limit() {
        use std::sync::atomic::AtomicUsize;
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let cache = root.join("cache");
        fs::create_dir(&cache).unwrap();
        let path = cache.join("just-used.jpg");
        fs::write(&path, b"pixels").unwrap();
        let checks = AtomicUsize::new(0);
        let remaining = evict(
            &root,
            &cache,
            0,
            |_| true,
            |_| checks.fetch_add(1, Ordering::SeqCst) > 0,
        )
        .unwrap();
        assert_eq!(remaining.bytes, 6);
        assert!(path.exists());
    }

    #[test]
    fn an_unremovable_entry_does_not_prevent_evicting_other_files() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let cache = root.join("cache");
        fs::create_dir(&cache).unwrap();
        let old = cache.join("old.jpg");
        let next = cache.join("next.jpg");
        for (path, age) in [(&old, 1), (&next, 2)] {
            fs::write(path, b"data").unwrap();
            fs::File::open(path)
                .unwrap()
                .set_times(fs::FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(age)))
                .unwrap();
        }
        let result = evict(
            &root,
            &cache,
            4,
            |path| {
                if path == old {
                    // Dropped only just before both remove it: a deletion
                    // Windows still carries would reject the new folder.
                    fs::remove_file(path).unwrap();
                    fs::create_dir(path).unwrap();
                }
                true
            },
            |_| false,
        )
        .unwrap();
        assert_eq!(result.bytes, 4);
        assert!(result.incomplete);
        assert!(old.is_dir());
        assert!(!next.exists());
    }

    #[cfg(unix)]
    #[test]
    fn marking_use_does_not_follow_a_link_to_an_original() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let original = root.join("original.RAW");
        fs::write(&original, b"original").unwrap();
        let before = fs::metadata(&original).unwrap().modified().unwrap();
        let cache = root.join("cache");
        fs::create_dir(&cache).unwrap();
        symlink(&original, cache.join("linked.f16")).unwrap();
        mark_used(&cache.join("linked.f16"));
        assert_eq!(fs::metadata(&original).unwrap().modified().unwrap(), before);
        symlink(&root, root.join("linked-dir")).unwrap();
        mark_used(&root.join("linked-dir/original.RAW"));
        assert_eq!(fs::metadata(&original).unwrap().modified().unwrap(), before);
        assert_eq!(fs::read(original).unwrap(), b"original");
    }
}
