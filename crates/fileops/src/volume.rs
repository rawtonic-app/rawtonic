//! What a move to another volume needs (ADR 0016): which volume a path is
//! on (name, file system, free space, network or not), whether that volume
//! renames without replacing, an exclusive rename that never falls back to
//! check-then-rename, and a copy that is synced, read back from the drive
//! and compared byte for byte with its source.
use crate::xattr;
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

/// Free space a move to another volume leaves on it, beyond its files.
pub const SPACE_RESERVE: u64 = 64 * 1024 * 1024;

/// The volume a path is on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VolumeInfo {
    /// Where it is mounted.
    pub mount_point: PathBuf,
    /// What the user calls it: the mount point's last component (the
    /// drive's label on macOS and in Linux desktop mounts), or the mount
    /// point itself for `/`.
    pub name: String,
    /// The file system type as the system names it (`ext4`, `exfat`, `apfs`, …).
    pub file_system: String,
    /// Bytes free for this user.
    pub free_bytes: u64,
    /// A network file system (NFS, SMB, AFP, WebDAV, sshfs, …).
    pub network: bool,
}

/// The volume of `path`'s nearest existing ancestor.
pub fn volume_info(path: &Path) -> Result<VolumeInfo> {
    let existing = path
        .ancestors()
        .find(|p| fs::symlink_metadata(p).is_ok())
        .with_context(|| format!("{} has no existing folder", path.display()))?;
    let real = existing
        .canonicalize()
        .with_context(|| format!("Cannot resolve {}", existing.display()))?;
    let (mount_point, file_system, network) = mount_of(&real)?;
    let name = match mount_point.file_name() {
        Some(name) => name.to_string_lossy().into_owned(),
        None => mount_point.to_string_lossy().into_owned(),
    };
    Ok(VolumeInfo {
        name,
        free_bytes: free_bytes(&real)?,
        mount_point,
        file_system,
        network,
    })
}

/// File systems whose sync and read-back do not prove what reached the
/// disk, by the names Linux and macOS give them.
fn is_network_fs(name: &str) -> bool {
    const NETWORK: &[&str] = &[
        "nfs",
        "nfs4",
        "cifs",
        "smb",
        "smb3",
        "smbfs",
        "afpfs",
        "webdav",
        "davfs",
        "ncpfs",
        "afs",
        "ceph",
        "glusterfs",
        "9p",
        "fuse.sshfs",
        "fuse.rclone",
        "fuse.gvfsd-fuse",
        "fuse.s3fs",
    ];
    NETWORK.contains(&name)
}

/// Mount point, file system type and whether it is a network file system,
/// for a canonical path: the longest mount point above it in
/// `/proc/self/mountinfo` (the last one listed for a path mounted over).
#[cfg(target_os = "linux")]
fn mount_of(path: &Path) -> Result<(PathBuf, String, bool)> {
    let table =
        fs::read_to_string("/proc/self/mountinfo").context("Cannot read the mount table")?;
    let mut best: Option<(PathBuf, String)> = None;
    for line in table.lines() {
        let Some((left, right)) = line.split_once(" - ") else {
            continue;
        };
        let (Some(mount), Some(fs_type)) = (left.split(' ').nth(4), right.split(' ').next()) else {
            continue;
        };
        let mount = PathBuf::from(unescape_mount(mount));
        if path.starts_with(&mount)
            && best
                .as_ref()
                .is_none_or(|(b, _)| mount.as_os_str().len() >= b.as_os_str().len())
        {
            best = Some((mount, fs_type.to_string()));
        }
    }
    let (mount, fs_type) = best.context("The volume was not found in the mount table")?;
    let network = is_network_fs(&fs_type);
    Ok((mount, fs_type, network))
}

/// `/proc/self/mountinfo` writes space, tab, newline and backslash as
/// octal escapes (`\040`).
#[cfg(target_os = "linux")]
fn unescape_mount(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let octal = bytes
            .get(i + 1..i + 4)
            .filter(|d| d.iter().all(|c| (b'0'..=b'7').contains(c)) && d[0] <= b'3');
        if let (b'\\', Some(d)) = (bytes[i], octal) {
            out.push((d[0] - b'0') * 64 + (d[1] - b'0') * 8 + (d[2] - b'0'));
            i += 4;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(target_vendor = "apple")]
fn mount_of(path: &Path) -> Result<(PathBuf, String, bool)> {
    use std::ffi::CStr;
    let c = c_path(path)?;
    // SAFETY: a zeroed statfs is a valid out value; the path is a valid
    // NUL-terminated string for the call's duration.
    let mut stats: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(c.as_ptr(), &mut stats) } != 0 {
        return Err(io::Error::last_os_error())
            .with_context(|| format!("Cannot read the volume of {}", path.display()));
    }
    // SAFETY: the kernel fills both as NUL-terminated strings.
    let (mount, fs_type) = unsafe {
        (
            CStr::from_ptr(stats.f_mntonname.as_ptr()),
            CStr::from_ptr(stats.f_fstypename.as_ptr()),
        )
    };
    let fs_type = fs_type.to_string_lossy().into_owned();
    let local = stats.f_flags & (libc::MNT_LOCAL as u32) != 0;
    let network = !local || is_network_fs(&fs_type);
    Ok((
        PathBuf::from(mount.to_string_lossy().into_owned()),
        fs_type,
        network,
    ))
}

#[cfg(not(any(target_os = "linux", target_vendor = "apple")))]
fn mount_of(path: &Path) -> Result<(PathBuf, String, bool)> {
    let root = path.ancestors().last().unwrap_or(path).to_path_buf();
    Ok((root, "unknown".into(), false))
}

#[cfg(unix)]
fn free_bytes(path: &Path) -> Result<u64> {
    let c = c_path(path)?;
    // SAFETY: a zeroed statvfs is a valid out value; the path is a valid
    // NUL-terminated string for the call's duration.
    let mut stats: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut stats) } != 0 {
        return Err(io::Error::last_os_error())
            .with_context(|| format!("Cannot read the free space of {}", path.display()));
    }
    // The field types differ per platform (u32 or u64).
    #[allow(clippy::useless_conversion)]
    Ok(u64::from(stats.f_bavail).saturating_mul(u64::from(stats.f_frsize)))
}

#[cfg(not(unix))]
fn free_bytes(_path: &Path) -> Result<u64> {
    bail!("Free space cannot be read on this system")
}

/// Renames `from` to `to` only if `to` does not exist, atomically, or fails:
/// `AlreadyExists` when the name is taken, `Unsupported` (or the system's
/// own error) where the volume or system cannot rename exclusively. Never
/// falls back to a check followed by a rename.
pub fn rename_exclusive(from: &Path, to: &Path) -> io::Result<()> {
    #[cfg(target_vendor = "apple")]
    {
        let (f, t) = (c_path(from)?, c_path(to)?);
        // SAFETY: both are valid NUL-terminated paths for the call's duration.
        if unsafe { libc::renamex_np(f.as_ptr(), t.as_ptr(), libc::RENAME_EXCL) } == 0 {
            return Ok(());
        }
        Err(io::Error::last_os_error())
    }
    #[cfg(all(target_os = "linux", any(target_env = "gnu", target_env = "musl")))]
    {
        let (f, t) = (c_path(from)?, c_path(to)?);
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
        if result == 0 {
            return Ok(());
        }
        Err(io::Error::last_os_error())
    }
    #[cfg(not(any(
        target_vendor = "apple",
        all(target_os = "linux", any(target_env = "gnu", target_env = "musl"))
    )))]
    {
        let _ = (from, to);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "this system has no exclusive rename",
        ))
    }
}

const PROBE_A: &str = ".rawtonic-probe-a";
const PROBE_B: &str = ".rawtonic-probe-b";

/// Whether the volume of `folder` (a folder only this operation writes in)
/// renames exclusively: a rename to a free name succeeds and one onto a
/// taken name is refused, both without replacing anything. The probe files
/// are removed again.
pub fn probe_exclusive_rename(folder: &Path) -> Result<()> {
    let result = probe_rename(folder);
    remove_probe(folder);
    result.map_err(|e| cannot_publish(folder, &e))
}

fn probe_rename(folder: &Path) -> io::Result<()> {
    let (a, b) = (folder.join(PROBE_A), folder.join(PROBE_B));
    File::create_new(&a)?;
    rename_exclusive(&a, &b)?;
    File::create_new(&a)?;
    match rename_exclusive(&a, &b) {
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(e),
        Ok(()) => Err(io::Error::other("it replaced an existing file")),
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
pub fn publish_mode(folder: &Path) -> Result<Publish> {
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
            Err(e) => Err(cannot_publish(folder, &e)),
        };
    }
    Err(cannot_publish(folder, &error))
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
    created: &mut dyn FnMut() -> Result<()>,
) -> Result<(Stamp, Stamp)> {
    #[cfg(unix)]
    let posix = (
        unix_mode(source)?.is_some(),
        unix_mode(staged.parent().context("The copy has no folder")?)?.is_some(),
    );
    #[cfg(not(unix))]
    let posix = (false, false);
    copy_file_verified(source, staged, original_mode, posix, tally, created)
}

/// [`copy_verified`] with whether the source's and the copy's volumes
/// keep Unix modes (`posix`) known; `created` runs once the new file
/// exists, before anything is written to it.
pub(crate) fn copy_file_verified(
    source: &Path,
    staged: &Path,
    original_mode: Option<u32>,
    posix: (bool, bool),
    tally: &mut Tally,
    created: &mut dyn FnMut() -> Result<()>,
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
    created()?;
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
            bail!(io::Error::new(io::ErrorKind::Interrupted, "Stopped"));
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
    xattr::write(&output, &attributes).map_err(|e| {
        xattr::cannot_keep(staged.parent().unwrap_or(staged), source, &attributes, &e)
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

/// Syncs a folder's entries (a new or renamed name in it) to the drive.
pub(crate) fn sync_folder(folder: &Path) -> Result<()> {
    #[cfg(unix)]
    File::open(folder)
        .and_then(|f| f.sync_all())
        .with_context(|| format!("Cannot sync the folder {}", folder.display()))?;
    #[cfg(not(unix))]
    let _ = folder;
    Ok(())
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
            bail!(io::Error::new(io::ErrorKind::Interrupted, "Stopped"));
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

/// Whether the file `part` holds the first bytes of the file `whole` (all
/// of them, or none: an empty file is the start of any).
pub(crate) fn is_prefix_of(part: &Path, whole: &Path) -> Result<bool> {
    let mut a = File::open(part).with_context(|| format!("Cannot read {}", part.display()))?;
    let mut b = File::open(whole).with_context(|| format!("Cannot read {}", whole.display()))?;
    if a.metadata()?.len() > b.metadata()?.len() {
        return Ok(false);
    }
    let (mut x, mut y) = (vec![0u8; BLOCK], vec![0u8; BLOCK]);
    loop {
        let n = read_full(&mut a, &mut x)?;
        if n == 0 {
            return Ok(true);
        }
        if read_full(&mut b, &mut y[..n])? != n || x[..n] != y[..n] {
            return Ok(false);
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

#[cfg(unix)]
pub(crate) fn c_path(path: &Path) -> io::Result<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "Path contains a NUL byte"))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn mount_escapes_are_read() {
        assert_eq!(
            unescape_mount(r"/run/media/u/My\040Disk"),
            "/run/media/u/My Disk"
        );
        assert_eq!(unescape_mount(r"/a\134b"), r"/a\b");
        assert_eq!(unescape_mount("/plain"), "/plain");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_root_volume_is_found_and_local() {
        let info = volume_info(Path::new("/proc/self")).unwrap();
        assert_eq!(info.file_system, "proc");
        assert!(!info.network);
        let tmp = tempfile::tempdir().unwrap();
        let info = volume_info(&tmp.path().join("not/yet/there")).unwrap();
        assert!(
            tmp.path()
                .canonicalize()
                .unwrap()
                .starts_with(&info.mount_point)
        );
        assert!(info.free_bytes > 0);
    }

    #[cfg(target_vendor = "apple")]
    #[test]
    fn the_temporary_folders_volume_is_found_and_local() {
        // The temporary folder is on the data volume, mounted at
        // /System/Volumes/Data but reached through /private (firmlinks).
        let tmp = tempfile::tempdir().unwrap();
        let info = volume_info(&tmp.path().join("not/yet/there")).unwrap();
        assert!(info.mount_point.is_dir(), "{info:?}");
        assert_eq!(info.file_system, "apfs");
        assert!(!info.network);
        assert!(info.free_bytes > 0);
    }

    #[test]
    fn network_file_systems_are_named() {
        for name in ["nfs4", "cifs", "smb3", "fuse.sshfs", "fuse.gvfsd-fuse"] {
            assert!(is_network_fs(name), "{name}");
        }
        for name in ["ext4", "btrfs", "exfat", "vfat", "fuseblk", "tmpfs"] {
            assert!(!is_network_fs(name), "{name}");
        }
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
        let error = rename_exclusive(&tmp.path().join("a"), &tmp.path().join("b")).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(tmp.path().join("b")).unwrap(), b"b");
    }
}
