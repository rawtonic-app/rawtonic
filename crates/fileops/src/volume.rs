//! What a move to another volume needs (ADR 0016), beyond which volume a
//! path is on ([`rawtonic_fs::volume::volume_info`]): whether that volume
//! renames without replacing, how copies are published there, and a copy
//! that is synced, read back from the drive and compared byte for byte with
//! its source.
use crate::FileOpsError;
use crate::error::{bail, ensure, stopped};
use crate::xattr;
use anyhow::{Context, Result};
use rawtonic_fs::Canonical;
#[cfg(unix)]
use rawtonic_fs::volume::volume_info;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::Path,
};

/// Free space a move to another volume leaves on it, beyond its files.
pub const SPACE_RESERVE: u64 = 64 * 1024 * 1024;

/// A volume's mount point is the identity `copy_needs` groups by (`C:\`
/// on Windows, not `C:\Temp` in one program and `\\?\C:\Temp` in
/// another), so it is compared by [`same_mount`], not by the path alone.
pub(super) fn same_mount(a: &Path, b: &Path) -> bool {
    match (a.canonical(), b.canonical()) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}

const PROBE_A: &str = ".rawtonic-probe-a";
const PROBE_B: &str = ".rawtonic-probe-b";

/// Whether the volume of `folder` (a folder only this operation writes in)
/// renames exclusively: a rename to a free name succeeds and one onto a
/// taken name is refused, both without replacing anything. The probe files
/// are removed again.
pub fn probe_exclusive_rename(folder: &Path) -> Result<(), FileOpsError> {
    let result = probe_rename(folder);
    remove_probe(folder);
    Ok(result.map_err(|e| cannot_publish(folder, &e))?)
}

fn probe_rename(folder: &Path) -> io::Result<()> {
    let (a, b) = (folder.join(PROBE_A), folder.join(PROBE_B));
    File::create_new(&a)?;
    // Only the system's own exclusive rename counts: on a volume without
    // one, copies are created in place under their final names instead.
    if rawtonic_fs::rename_no_clobber(&a, &b)? != rawtonic_fs::Renamed::Exclusive {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "the volume has no exclusive rename",
        ));
    }
    File::create_new(&a)?;
    match rawtonic_fs::rename_no_clobber(&a, &b) {
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(e),
        Ok(_) => Err(io::Error::other("it replaced an existing file")),
    }
}

fn cannot_publish(folder: &Path, error: &io::Error) -> anyhow::Error {
    anyhow::anyhow!(
        "The drive of {} cannot rename or create files without risking replacing one \
         ({error}); moving photos to it is not supported",
        folder.display()
    )
}

/// How copies are put under their final names on a volume (ADR 0016).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Publish {
    /// Staged, verified, then renamed exclusively.
    Rename,
    /// Created exclusively under the final name and copied and verified
    /// there: for volumes that cannot rename exclusively (exFAT on macOS).
    /// Files only; a folder is refused.
    InPlace,
}

/// How copies are published in `folder` (a folder only this operation
/// writes in): by exclusive rename where the volume has one; on Unix
/// volumes that answer it as unsupported, by exclusive creation, when that
/// holds (a second creation of a name is refused). Anything else, such as
/// a rename that replaced a file, refuses the volume. The probe files are
/// removed again.
pub fn publish_mode(folder: &Path) -> Result<Publish, FileOpsError> {
    #[cfg(test)]
    if FORCE_IN_PLACE.get() {
        return Ok(Publish::InPlace);
    }
    let renamed = probe_rename(folder);
    remove_probe(folder);
    let error = match renamed {
        Ok(()) => return Ok(Publish::Rename),
        Err(e) => e,
    };
    // Unix volumes that answer the rename as unsupported get exclusive
    // creation below; Windows has no such mode (a volume that refuses the
    // rename, or a network drive, is refused, never given a weaker way).
    #[cfg(unix)]
    if error.kind() == io::ErrorKind::Unsupported
        || matches!(
            error.raw_os_error(),
            Some(libc::ENOTSUP | libc::EINVAL | libc::ENOSYS)
        )
    {
        let a = folder.join(PROBE_A);
        let created = File::create_new(&a).and_then(|_| match File::create_new(&a) {
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(()),
            Err(e) => Err(e),
            Ok(_) => Err(io::Error::other("it created an existing file again")),
        });
        remove_probe(folder);
        return match created {
            Ok(()) => Ok(Publish::InPlace),
            Err(e) => Err(cannot_publish(folder, &e).into()),
        };
    }
    Err(cannot_publish(folder, &error).into())
}

#[cfg(test)]
thread_local! {
    /// Tests: every volume publishes in place.
    pub(crate) static FORCE_IN_PLACE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Removes what [`probe_exclusive_rename`] may have left in `folder`.
pub(crate) fn remove_probe(folder: &Path) {
    for name in [PROBE_A, PROBE_B, crate::xattr::PROBE_FILE] {
        let _ = fs::remove_file(folder.join(name));
    }
}

/// A file's size and modification time: whether it changed since.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Stamp {
    pub len: u64,
    pub modified_secs: i64,
    pub modified_nanos: u32,
}

impl Stamp {
    pub fn of(path: &Path) -> io::Result<Self> {
        Ok(Self::from_meta(&fs::symlink_metadata(path)?))
    }
    pub(crate) fn from_meta(meta: &fs::Metadata) -> Self {
        let (modified_secs, modified_nanos) = match meta.modified() {
            Ok(time) => match time.duration_since(std::time::UNIX_EPOCH) {
                Ok(d) => (d.as_secs() as i64, d.subsec_nanos()),
                Err(e) => (
                    -(e.duration().as_secs() as i64),
                    e.duration().subsec_nanos(),
                ),
            },
            Err(_) => (0, 0),
        };
        Self {
            len: meta.len(),
            modified_secs,
            modified_nanos,
        }
    }
}

/// Watches a copy: bytes copied so far, and whether the user stopped it.
pub trait CopyObserver {
    fn copied(&self, _done: u64, _total: u64) {}
    fn cancelled(&self) -> bool {
        false
    }
}

/// An observer that neither reports nor stops.
pub struct Unobserved;
impl CopyObserver for Unobserved {}

/// Bytes copied across the files of one operation.
pub(crate) struct Tally<'a> {
    pub observer: &'a dyn CopyObserver,
    pub done: u64,
    pub total: u64,
}

/// FAT-family and NTFS volumes synthesize Unix modes from mount options; those
/// bits cannot tell us which permissions the file had on a POSIX volume.
#[cfg(unix)]
pub(crate) fn unix_mode(path: &Path) -> Result<Option<u32>> {
    use std::os::unix::fs::PermissionsExt;
    let fs = volume_info(path)?.file_system;
    if matches!(
        fs.as_str(),
        "msdos" | "msdosfs" | "vfat" | "exfat" | "fuseblk" | "ntfs" | "ntfs3"
    ) {
        return Ok(None);
    }
    Ok(Some(fs::metadata(path)?.permissions().mode() & 0o7777))
}

/// A recorded first mode is a ceiling, as is a live POSIX source's mode.
/// Synthesized source modes impose no ceiling; without a recorded mode,
/// creation with this mode also lets the OS apply the current umask.
#[cfg(unix)]
fn safe_copy_mode(recorded: Option<u32>, live: Option<u32>) -> u32 {
    recorded.unwrap_or(0o644) & live.unwrap_or(0o666) & 0o666
}

pub(crate) const BLOCK: usize = 1 << 20;

/// Copies `source` into a new file `staged` (never an existing one) with
/// its recorded permissions (without execute/special bits), or 0644 masked
/// by umask when unrecorded, its modification time and the extended
/// attributes a copy keeps ([`xattr`]), syncs it and its folder, then
/// reads it back from the drive and compares it byte for byte (and its
/// attributes) with `source`. Returns the source's stamp, unchanged through the copy, and
/// the copy's. Stop is honoured while copying and comparing. On failure
/// the incomplete copy is left for the caller's staging cleanup.
pub(crate) fn copy_verified(
    source: &Path,
    staged: &Path,
    original_mode: Option<u32>,
    tally: &mut Tally,
    created: &mut dyn FnMut(&File) -> Result<()>,
    in_place: bool,
) -> Result<(Stamp, Stamp)> {
    #[cfg(unix)]
    let posix = (
        unix_mode(source)?.is_some(),
        unix_mode(staged.parent().context("The copy has no folder")?)?.is_some(),
    );
    #[cfg(not(unix))]
    let posix = (false, false);
    copy_file_verified(
        source,
        staged,
        original_mode,
        posix,
        tally,
        created,
        in_place,
    )
}

/// [`copy_verified`] with whether the source's and the copy's volumes
/// keep Unix modes (`posix`) known; `created` runs once the new file
/// exists, before anything is written to it; `in_place`: the new file is
/// visible under its final name while it is copied.
pub(crate) fn copy_file_verified(
    source: &Path,
    staged: &Path,
    original_mode: Option<u32>,
    posix: (bool, bool),
    tally: &mut Tally,
    created: &mut dyn FnMut(&File) -> Result<()>,
    in_place: bool,
) -> Result<(Stamp, Stamp)> {
    let mut input =
        File::open(source).with_context(|| format!("Cannot read {}", source.display()))?;
    let meta = input.metadata()?;
    ensure!(meta.is_file(), "{} is not a file", source.display());
    let before = Stamp::from_meta(&meta);
    // Read before the bytes: a change while copying is seen at the end.
    let attributes = xattr::read(&input).with_context(|| {
        format!(
            "Cannot read the extended attributes of {}",
            source.display()
        )
    })?;
    #[cfg(unix)]
    let mode = {
        use std::os::unix::fs::PermissionsExt;
        let live = posix.0.then(|| meta.permissions().mode());
        safe_copy_mode(original_mode, live)
    };
    #[cfg(unix)]
    let restore_mode = if original_mode.is_some() && posix.1 {
        Some(mode)
    } else {
        None
    };
    #[cfg(not(unix))]
    let _ = posix;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    // Writable by its owner until its attributes are set (Linux asks for
    // write permission to set them, also through an open descriptor).
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(mode | 0o200);
    }
    let mut output = options
        .open(staged)
        .with_context(|| format!("Cannot create {}", staged.display()))?;
    // Set first too where the copy is visible: a modification time before
    // the file's creation moves its creation time back on some volumes
    // (APFS), so the creation time `created` sees is the one the copy keeps.
    if in_place && let Ok(modified) = meta.modified() {
        let _ = output.set_modified(modified);
    }
    created(&output)?;
    // The mode it was made with (the umask applied), without the owner's
    // write bit when `mode` has none: what an unrecorded mode ends as.
    #[cfg(unix)]
    let made_mode = {
        use std::os::unix::fs::PermissionsExt;
        output.metadata()?.permissions().mode() & !(0o200 & !mode) & 0o7777
    };
    let mut buffer = vec![0u8; BLOCK];
    loop {
        if tally.observer.cancelled() {
            bail!(stopped());
        }
        let read = input.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        output
            .write_all(&buffer[..read])
            .with_context(|| format!("Cannot write {}", staged.display()))?;
        tally.done += read as u64;
        tally.observer.copied(tally.done, tally.total);
        #[cfg(test)]
        crate::crash::point("copy block written");
    }
    // A copy made in place is visible: an attribute another program gave
    // it meanwhile is never replaced (the copy then fails its check).
    let set = if in_place {
        xattr::write_new(&output, &attributes)
    } else {
        xattr::write(&output, &attributes)
    };
    set.map_err(|e| {
        if in_place && e.kind() == io::ErrorKind::AlreadyExists {
            anyhow::anyhow!(
                "Another program gave {} extended attributes while it was copied; \
                 nothing was moved",
                staged.display()
            )
        } else {
            xattr::cannot_keep(staged.parent().unwrap_or(staged), source, &attributes, &e)
        }
    })?;
    #[cfg(unix)]
    if let Some(mode) = restore_mode {
        use std::os::unix::fs::PermissionsExt;
        output.set_permissions(fs::Permissions::from_mode(mode & 0o666))?;
    } else if posix.1 && mode & 0o200 == 0 {
        use std::os::unix::fs::PermissionsExt;
        output.set_permissions(fs::Permissions::from_mode(made_mode))?;
    }
    #[cfg(not(unix))]
    let _ = original_mode;
    // Times are best effort on volumes that cannot represent them exactly.
    if let Ok(modified) = meta.modified() {
        let _ = output.set_modified(modified);
    }
    output
        .sync_all()
        .with_context(|| format!("Cannot sync {}", staged.display()))?;
    drop(output);
    sync_folder(staged.parent().context("The copy has no folder")?)?;
    ensure!(
        same_content(source, staged, true, Some(tally.observer))?,
        "The copy of {} differs from the original after writing it; nothing was moved",
        source.display()
    );
    ensure!(
        xattr::copied(staged, &attributes)?,
        "The extended attributes of the copy of {} differ from the original's after writing \
         them; nothing was moved",
        source.display()
    );
    let after = Stamp::of(source)?;
    ensure!(
        after == before && xattr::read_path(source)? == attributes,
        "{} changed while it was copied; nothing was moved",
        source.display()
    );
    Ok((before, Stamp::of(staged)?))
}

/// Whether `copy` holds nothing its `source` does not: its bytes are the
/// start of the source's (or all of them) and each of its kept extended
/// attributes is the source's. Moving such a file to the Trash cannot take
/// anything from anyone, whoever made it.
pub(crate) fn holds_only_source(copy: &Path, source: &Path) -> Result<bool> {
    let mut a = File::open(copy).with_context(|| format!("Cannot read {}", copy.display()))?;
    let mut b = File::open(source).with_context(|| format!("Cannot read {}", source.display()))?;
    if a.metadata()?.len() > b.metadata()?.len() {
        return Ok(false);
    }
    let (mut x, mut y) = (vec![0u8; BLOCK], vec![0u8; BLOCK]);
    loop {
        let read = a.read(&mut x)?;
        if read == 0 {
            break;
        }
        b.read_exact(&mut y[..read])?;
        if x[..read] != y[..read] {
            return Ok(false);
        }
    }
    let theirs = xattr::read(&b)?;
    Ok(xattr::read(&a)?
        .iter()
        .all(|attribute| theirs.contains(attribute)))
}

/// Syncs a folder's entries (a new or renamed name in it) to the drive.
pub(crate) fn sync_folder(folder: &Path) -> Result<()> {
    rawtonic_fs::sync_dir(folder)
        .with_context(|| format!("Cannot sync the folder {}", folder.display()))
}

/// Whether two files hold the same bytes. With `from_drive`, `b`'s cached
/// pages are dropped first where the system allows (Linux), or it is read
/// uncached (macOS: pages already cached may still be served), so that a
/// just-synced copy is read from the drive. Fails with "Stopped" when
/// `stop` is cancelled meanwhile.
pub(crate) fn same_content(
    a: &Path,
    b: &Path,
    from_drive: bool,
    stop: Option<&dyn CopyObserver>,
) -> Result<bool> {
    let mut a = File::open(a).with_context(|| format!("Cannot read {}", a.display()))?;
    let mut b = File::open(b).with_context(|| format!("Cannot read {}", b.display()))?;
    if a.metadata()?.len() != b.metadata()?.len() {
        return Ok(false);
    }
    if from_drive {
        uncached(&b);
    }
    let (mut x, mut y) = (vec![0u8; BLOCK], vec![0u8; BLOCK]);
    loop {
        if stop.is_some_and(|s| s.cancelled()) {
            bail!(stopped());
        }
        let n = read_full(&mut a, &mut x)?;
        let m = read_full(&mut b, &mut y)?;
        if n != m || x[..n] != y[..m] {
            return Ok(false);
        }
        if n == 0 {
            return Ok(true);
        }
    }
}

fn read_full(file: &mut File, buffer: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buffer.len() {
        match file.read(&mut buffer[filled..])? {
            0 => break,
            n => filled += n,
        }
    }
    Ok(filled)
}

#[allow(unused_variables)]
fn uncached(file: &File) {
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsRawFd;
        // SAFETY: a valid open descriptor; advice only, failures ignored.
        unsafe {
            libc::posix_fadvise(file.as_raw_fd(), 0, 0, libc::POSIX_FADV_DONTNEED);
        }
    }
    #[cfg(target_vendor = "apple")]
    {
        use std::os::fd::AsRawFd;
        // SAFETY: a valid open descriptor; failures ignored.
        unsafe {
            libc::fcntl(file.as_raw_fd(), libc::F_NOCACHE, 1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Windows folder's canonical form may be verbatim (`\\?\C:\…`)
    /// while its mount root is not, so identity is compared by
    /// [`same_mount`]s rule, not by one path prefixing the other.
    #[cfg(windows)]
    #[test]
    fn a_windows_folders_mount_root_is_compared_by_identity() {
        use rawtonic_fs::volume::volume_info;
        let tmp = tempfile::tempdir().unwrap();
        let existing = tmp.path().canonical().unwrap();
        let folder = existing.join("not").join("yet").join("there");
        let info = volume_info(&folder).unwrap();
        let root = info.mount_point.canonical().unwrap();
        let child = folder.with_file_name("also");
        let child_volume = volume_info(&child).unwrap();
        assert!(same_mount(&child_volume.mount_point, &info.mount_point));
        // Canonical and ordinary spellings of an existing root compare alike.
        assert!(same_mount(&root, &info.mount_point));
    }

    /// A Windows volume renames exclusively by the system's own move that
    /// never replaces: a free name succeeds and leaves one file, a taken
    /// name is refused and neither file changes.
    #[cfg(windows)]
    #[test]
    fn windows_renames_never_replace_and_never_copy() {
        let tmp = tempfile::tempdir().unwrap();
        let folder = tmp.path().canonical().unwrap();
        let (a, b, c) = (folder.join("a"), folder.join("b"), folder.join("c"));
        fs::write(&a, "a").unwrap();
        fs::write(&b, "b").unwrap();
        fs::write(&c, "c").unwrap();
        // A free name: the file moves, nothing else appears.
        rawtonic_fs::rename_no_clobber(&c, &folder.join("d")).unwrap();
        assert_eq!(fs::read(folder.join("d")).unwrap(), b"c");
        assert!(!c.exists());
        rawtonic_fs::rename_no_clobber(&folder.join("d"), &c).unwrap();
        // A taken one: refused as taken, the file to move stays where it
        // is and the target keeps its bytes (never replaced, also by the
        // system's own semantics).
        let error = rawtonic_fs::rename_no_clobber(&a, &b).unwrap_err();
        assert!(
            error.kind() == io::ErrorKind::AlreadyExists
                || error.kind() == io::ErrorKind::PermissionDenied,
            "{error:?}"
        );
        assert_eq!(fs::read(&a).unwrap(), b"a");
        assert_eq!(fs::read(&b).unwrap(), b"b");
        // The probe finds the volume able to publish by exclusive rename.
        probe_exclusive_rename(&folder).unwrap();
        assert_eq!(
            publish_mode(&folder).unwrap(),
            Publish::Rename,
            "the probe files are cleaned up"
        );
        let mut names: Vec<_> = fs::read_dir(&folder)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        names.sort();
        assert_eq!(names.len(), 3, "{names:?}");
        assert_eq!(fs::read(&a).unwrap(), b"a");
        assert_eq!(fs::read(&b).unwrap(), b"b");
        assert_eq!(fs::read(&c).unwrap(), b"c");
    }

    #[cfg(unix)]
    #[test]
    fn copy_permissions_obey_both_recorded_and_live_ceilings() {
        for (recorded, live, expected) in [
            (None, None, 0o644),
            (Some(0o640), None, 0o640),
            (None, Some(0o600), 0o600),
            (Some(0o640), Some(0o600), 0o600),
            (Some(0o600), Some(0o644), 0o600),
            (Some(0o6751), Some(0o777), 0o640),
            (Some(0o640), Some(0o700), 0o600),
        ] {
            assert_eq!(safe_copy_mode(recorded, live), expected);
        }
    }

    #[test]
    fn a_local_volume_renames_exclusively() {
        let tmp = tempfile::tempdir().unwrap();
        probe_exclusive_rename(tmp.path()).unwrap();
        assert_eq!(
            fs::read_dir(tmp.path()).unwrap().count(),
            0,
            "probe removed"
        );
        fs::write(tmp.path().join("a"), "a").unwrap();
        fs::write(tmp.path().join("b"), "b").unwrap();
        let error = rawtonic_fs::rename_no_clobber(&tmp.path().join("a"), &tmp.path().join("b"))
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(tmp.path().join("b")).unwrap(), b"b");
    }
}
