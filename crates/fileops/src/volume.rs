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
    let name = volume_name(&mount_point);
    Ok(VolumeInfo {
        name,
        free_bytes: free_bytes(&real)?,
        mount_point,
        file_system,
        network,
    })
}

/// A volume's mount point is the identity `copy_needs` groups by (`C:\`
/// on Windows, not `C:\Temp` in one program and `\\?\C:\Temp` in
/// another), so it is compared by [`same_mount`], not by the path alone.
pub(super) fn same_mount(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}

/// What the user calls the volume at `mount_point`: a drive (`D:`), a
/// network share (`\\server\share`), a folder it is mounted into, the
/// root `/`, or the mount point itself.
fn volume_name(mount_point: &Path) -> String {
    if let Some(name) = mount_point.file_name() {
        return name.to_string_lossy().into_owned();
    }
    let text = mount_point.to_string_lossy();
    // A Windows drive root has no last component: `D:\` is `D:`. (A
    // `\\?\` form is not canonical here and only decides the fallback.)
    match text.strip_suffix(std::path::MAIN_SEPARATOR) {
        Some(without) if !without.is_empty() => without.to_string(),
        _ => text.into_owned(),
    }
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

/// A path as the NUL-terminated wide string the Windows API takes, from its
/// own text (a verbatim or UNC path is passed as it is); a NUL byte in it
/// is refused.
#[cfg(windows)]
fn wide_path(path: &Path) -> Result<Vec<u16>> {
    use std::os::windows::ffi::OsStrExt;
    let text: Vec<u16> = path.as_os_str().encode_wide().collect();
    ensure!(!text.contains(&0), "{} contains a NUL byte", path.display());
    Ok(text.into_iter().chain(Some(0)).collect())
}

/// What the system reports for the volume at `root` (`C:\`, a volume
/// mounted into a folder of another one, `\\server\share\`): whether the
/// call failed, its own file system name and whether the drive the path
/// is on is remote (`DRIVE_REMOTE`).
#[cfg(windows)]
fn volume_of_root(root: &Path) -> Result<(bool, String, bool)> {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetVolumeInformationW(
            root: *const u16,
            volume_name: *mut u16,
            volume_name_len: u32,
            serial: *mut u32,
            max_component_len: *mut u32,
            flags: *mut u32,
            file_system_name: *mut u16,
            file_system_name_len: u32,
        ) -> i32;
        fn GetDriveTypeW(root: *const u16) -> u32;
    }
    // DRIVE_REMOTE: a mapped network drive, or any `\\server\share` path.
    const DRIVE_REMOTE: u32 = 4;
    const MAX_PATH: usize = 260;
    let root = wide_path(root)?;
    let mut file_system = [0u16; MAX_PATH + 1];
    // SAFETY: every pointer is valid for the given length (null for the
    // outputs not asked for); `root` is NUL-terminated and outlives the calls.
    let known = unsafe {
        GetVolumeInformationW(
            root.as_ptr(),
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            file_system.as_mut_ptr(),
            file_system.len() as u32,
        )
    } != 0;
    let network = unsafe { GetDriveTypeW(root.as_ptr()) } == DRIVE_REMOTE;
    let len = file_system
        .iter()
        .position(|&c| c == 0)
        .unwrap_or(file_system.len());
    Ok((
        known,
        String::from_utf16_lossy(&file_system[..len]),
        network,
    ))
}

/// The volume of a Windows path: the mount root the system gives it
/// (`GetVolumePathNameW`: `C:\`, `C:\mounted\`, `\\server\share\`,
/// verbatim and UNC alike, from the path's own text), the file system
/// there and whether it is a network drive.
#[cfg(windows)]
fn mount_of(path: &Path) -> Result<(PathBuf, String, bool)> {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetVolumePathNameW(path: *const u16, volume_path: *mut u16, len: u32) -> i32;
    }
    // The canonical input is absolute. A mount root cannot exceed its full
    // path plus a trailing separator; allow an extra NUL as documented by
    // GetVolumePathNameW, which can otherwise omit a trailing separator.
    let wide = wide_path(path)?;
    let len = wide.len().max(260) + 1;
    let api_len = u32::try_from(len).context("Volume path is too long")?;
    let mut buffer = vec![0u16; len];
    // SAFETY: the input is NUL-terminated and the output has api_len units.
    if unsafe { GetVolumePathNameW(wide.as_ptr(), buffer.as_mut_ptr(), api_len) } == 0 {
        return Err(io::Error::last_os_error())
            .with_context(|| format!("Cannot read the volume of {}", path.display()));
    }
    let end = buffer.iter().position(|&c| c == 0).unwrap_or(0);
    let root = PathBuf::from(OsString::from_wide(&buffer[..end]));
    let (known, file_system, network) = volume_of_root(&root)?;
    // Unreadable details do not refuse the volume by themselves: free
    // space, asked separately and then required, decides whether a move
    // there may go ahead.
    let file_system = if known {
        file_system
    } else {
        String::from("unknown")
    };
    Ok((root, file_system, network))
}

#[cfg(not(any(target_os = "linux", target_vendor = "apple", windows)))]
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

/// Bytes free to the calling user on the volume of `path`
/// (`GetDiskFreeSpaceExW`, as `f_bavail` is on Unix). A call that fails is
/// an error, never a guess.
#[cfg(windows)]
fn free_bytes(path: &Path) -> Result<u64> {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetDiskFreeSpaceExW(
            directory: *const u16,
            free_for_caller: *mut u64,
            total: *mut u64,
            total_free: *mut u64,
        ) -> i32;
    }
    let directory = wide_path(path)?;
    let mut free_for_caller = 0u64;
    // SAFETY: a NUL-terminated wide path that outlives the call, a valid
    // out value, and null for the totals not asked for.
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            directory.as_ptr(),
            &mut free_for_caller,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    } != 0;
    if !ok {
        let error = io::Error::last_os_error();
        return Err(anyhow::anyhow!(
            "Cannot read the free space of {} ({error})",
            path.display()
        ));
    }
    Ok(free_for_caller)
}

#[cfg(not(any(unix, windows)))]
fn free_bytes(path: &Path) -> Result<u64> {
    bail!(
        "Free space of {} cannot be read on this system",
        path.display()
    )
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
    // `MoveFileExW` with no flags: no `MOVEFILE_REPLACE_EXISTING` (a taken
    // name is refused), no `MOVEFILE_COPY_ALLOWED` (the system never copies
    // and deletes in place of a rename, which would not be atomic).
    #[cfg(windows)]
    {
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn MoveFileExW(from: *const u16, to: *const u16, flags: u32) -> i32;
        }
        // ERROR_FILE_EXISTS, ERROR_ALREADY_EXISTS.
        const ERROR_FILE_EXISTS: i32 = 80;
        const ERROR_ALREADY_EXISTS: i32 = 183;
        let as_io = |path: &Path| {
            wide_path(path).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))
        };
        let (f, t) = (as_io(from)?, as_io(to)?);
        // SAFETY: both are NUL-terminated wide strings that outlive the call.
        if unsafe { MoveFileExW(f.as_ptr(), t.as_ptr(), 0) } != 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        return Err(match error.raw_os_error() {
            Some(ERROR_FILE_EXISTS | ERROR_ALREADY_EXISTS) => {
                io::Error::new(io::ErrorKind::AlreadyExists, error)
            }
            _ => error,
        });
    }
    #[cfg(not(any(
        target_vendor = "apple",
        all(target_os = "linux", any(target_env = "gnu", target_env = "musl")),
        windows
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

    /// A Windows path is on the volume root the system gives it (`C:\\`
    /// for a temporary folder), even when it does not exist yet, and the
    /// free space is the user's to write there (a real query, not a
    /// stand-in). The folder's canonical form may be verbatim (`\\?\C:\…`)
    /// while the mount root is not, so identity is compared by
    /// [`same_mount`]s rule, not by one path prefixing the other.
    #[cfg(windows)]
    #[test]
    fn a_windows_folder_reports_its_mount_root_and_its_real_free_space() {
        let tmp = tempfile::tempdir().unwrap();
        let existing = tmp.path().canonicalize().unwrap();
        let folder = existing.join("not").join("yet").join("there");
        let info = volume_info(&folder).unwrap();
        let root = info.mount_point.canonicalize().unwrap();
        assert!(
            existing.starts_with(&root),
            "{info:?} is not its mount root"
        );
        assert!(!info.name.is_empty());
        assert!(!info.network);
        assert!(!info.file_system.is_empty(), "{info:?}");
        let child = folder.with_file_name("also");
        let child_volume = volume_info(&child).unwrap();
        assert!(same_mount(&child_volume.mount_point, &info.mount_point));
        // Canonical and ordinary spellings of an existing root compare alike.
        assert!(same_mount(&root, &info.mount_point));
        // The drive the temporary folder is on needs room to make one.
        assert!(
            info.free_bytes > 0,
            "a temporary folder needs free space, got {info:?}"
        );
        // Free space that cannot be read is an error, never a figure.
        let unreadable = Path::new(r"\\?\GLOBALROOT\Device\Null");
        assert!(free_bytes(unreadable).is_err(), "{unreadable:?}");
    }

    /// A Windows volume renames exclusively by the system's own move that
    /// never replaces: a free name succeeds and leaves one file, a taken
    /// name is refused and neither file changes.
    #[cfg(windows)]
    #[test]
    fn windows_renames_never_replace_and_never_copy() {
        let tmp = tempfile::tempdir().unwrap();
        let folder = tmp.path().canonicalize().unwrap();
        let (a, b, c) = (folder.join("a"), folder.join("b"), folder.join("c"));
        fs::write(&a, "a").unwrap();
        fs::write(&b, "b").unwrap();
        fs::write(&c, "c").unwrap();
        // A free name: the file moves, nothing else appears.
        rename_exclusive(&c, &folder.join("d")).unwrap();
        assert_eq!(fs::read(folder.join("d")).unwrap(), b"c");
        assert!(!c.exists());
        rename_exclusive(&folder.join("d"), &c).unwrap();
        // A taken one: refused as taken, the file to move stays where it
        // is and the target keeps its bytes (never replaced, also by the
        // system's own semantics).
        let error = rename_exclusive(&a, &b).unwrap_err();
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
        let error = rename_exclusive(&tmp.path().join("a"), &tmp.path().join("b")).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(tmp.path().join("b")).unwrap(), b"b");
    }
}
