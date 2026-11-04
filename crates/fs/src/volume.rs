//! Volume detection, the one place that asks the system about mounts: which
//! mount a path is on (`statfs` on macOS, `/proc/self/mountinfo` on Linux,
//! `GetVolumePathNameW` on Windows), every mount (`getfsstat`, mountinfo),
//! what a volume calls itself (`getattrlist` on macOS, `/dev/disk/by-uuid`
//! and `by-label` on Linux), free space (`statvfs`, `GetDiskFreeSpaceExW`),
//! removable media and mount-change notifications (kqueue, a pollable
//! mountinfo). Reads only; nothing is written to a volume.
//!
//! What a volume *is* to a caller stays with the caller: the catalog builds
//! its volume identity (and relinks folders) from these facts, file
//! operations plan moves between volumes with [`VolumeInfo`].
use anyhow::{Context, Result};
use serde::Serialize;
use std::{
    fs, io,
    path::{Path, PathBuf},
};

/// File systems whose sync and read-back do not prove what reached the
/// disk, and whose volumes are classified as network shares, by the names
/// Linux (`/proc/self/mountinfo`) and macOS (`f_fstypename`) give them. On
/// macOS a mount the kernel does not mark `MNT_LOCAL` counts as network
/// too, whatever its name.
///
/// - `nfs`, `nfs4`: NFS (Linux names; macOS reports `nfs`).
/// - `cifs`, `smb3`: the Linux SMB client (`smb3` is its SMB3-only alias).
/// - `smb`, `smbfs`: SMB under other names (`smbfs` is macOS's and the old
///   Linux client's).
/// - `afpfs`: Apple Filing Protocol shares (macOS, Linux afpfs-ng).
/// - `webdav`, `davfs`: WebDAV (macOS's `webdav`, Linux davfs2).
/// - `ncpfs`: NetWare shares.
/// - `afs`: the Andrew File System (OpenAFS, kAFS).
/// - `ceph`, `glusterfs`: distributed file systems mounted over the network.
/// - `9p`: Plan 9 shares (virtual machines' host folders, WSL).
/// - `fuse.sshfs`, `fuse.rclone`, `fuse.s3fs`: FUSE clients of SSH, cloud
///   storage and S3 servers.
/// - `fuse.gvfsd-fuse`: GNOME's mounts of remote locations (SMB, SFTP,
///   WebDAV) opened in the file manager.
pub const NETWORK_FILE_SYSTEMS: &[&str] = &[
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

/// Whether `name` is one of the [`NETWORK_FILE_SYSTEMS`].
pub fn is_network_fs(name: &str) -> bool {
    NETWORK_FILE_SYSTEMS.contains(&name)
}

/// One mount, as the system lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mount {
    /// Where it is mounted.
    pub mount_point: PathBuf,
    /// What is mounted there: a device (`/dev/sdb1`, `/dev/disk4s1`) or a
    /// server's share (`//nas/Photos`); empty where the system does not say
    /// (Windows).
    pub source: PathBuf,
    /// The file system type as the system names it (`ext4`, `exfat`,
    /// `apfs`, …; `unknown` where Windows cannot tell).
    pub file_system: String,
    /// A network file system: a listed name ([`is_network_fs`]), a mount
    /// macOS does not mark local, or a Windows network drive.
    pub network: bool,
    /// The mount's `statfs` flags.
    #[cfg(target_vendor = "apple")]
    flags: u32,
}

impl Mount {
    /// Whether the medium is removable (a memory card, a card reader's
    /// slot, a USB stick): macOS `MNT_REMOVABLE`, on Linux the disk's
    /// `removable` attribute in sysfs for a device mount; `None` when the
    /// system does not say.
    pub fn removable(&self) -> Option<bool> {
        #[cfg(target_vendor = "apple")]
        return Some(self.flags & macos::MNT_REMOVABLE != 0);
        #[cfg(target_os = "linux")]
        return self
            .is_device()
            .then(|| linux::removable(&self.source))
            .flatten();
        #[cfg(not(any(target_vendor = "apple", target_os = "linux")))]
        None
    }

    /// Whether a device (`/dev/…`) is mounted, not a server's share or a
    /// virtual file system.
    pub fn is_device(&self) -> bool {
        self.source.starts_with("/dev/")
    }
}

/// The mount `path` (which must exist) is on: the system resolves it
/// (symbolic links included) and gives the mount holding it; on Linux the
/// longest mount point above it (the last one listed for a path mounted
/// over).
pub fn mount_of(path: &Path) -> Result<Mount> {
    imp::mount_of(path)
}

/// Every mount the system lists (on Linux the whole mount table, virtual
/// file systems included); nothing on Windows and elsewhere.
pub fn mounts() -> Vec<Mount> {
    imp::mounts()
}

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
    let mount = mount_of(&real)?;
    let name = volume_name(&mount.mount_point);
    // GetDiskFreeSpaceExW requires a directory; statvfs accepts the
    // resolved file itself, which avoids another mount-table lookup.
    #[cfg(windows)]
    let space_path = &mount.mount_point;
    #[cfg(not(windows))]
    let space_path = &real;
    Ok(VolumeInfo {
        name,
        free_bytes: space(space_path)?.free,
        mount_point: mount.mount_point,
        file_system: mount.file_system,
        network: mount.network,
    })
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

/// Bytes on the file system holding a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Space {
    /// Free for the calling user (`f_bavail`; `GetDiskFreeSpaceExW`'s
    /// caller's free bytes).
    pub free: u64,
    /// The file system's size.
    pub total: u64,
}

/// Free and total bytes of the file system holding `path` (an existing
/// path; a folder on Windows). A call that fails is an error, never a
/// guess.
pub fn space(path: &Path) -> Result<Space> {
    #[cfg(unix)]
    return statvfs(path)
        .with_context(|| format!("Cannot read the free space of {}", path.display()));
    #[cfg(windows)]
    return windows::space(path);
    #[cfg(not(any(unix, windows)))]
    anyhow::bail!(
        "Free space of {} cannot be read on this system",
        path.display()
    )
}

#[cfg(unix)]
pub(crate) fn statvfs(path: &Path) -> io::Result<Space> {
    let c = crate::c_path(path)?;
    // SAFETY: an all-zero statvfs (integers only) is a valid value; statvfs
    // only writes into it.
    let mut stats: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `c` is a NUL-terminated path and `stats` a valid out pointer,
    // both alive for the call's duration.
    if unsafe { libc::statvfs(c.as_ptr(), &mut stats) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // The field types differ per platform (u32 or u64).
    #[allow(clippy::useless_conversion)]
    let (unit, free, total) = (
        u64::from(stats.f_frsize),
        u64::from(stats.f_bavail),
        u64::from(stats.f_blocks),
    );
    Ok(Space {
        free: free.saturating_mul(unit),
        total: total.saturating_mul(unit),
    })
}

/// What the system says about the medium holding a path (import mode).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MediaInfo {
    /// The medium is removable (a memory card, a card reader's slot, a USB
    /// stick); `None` when the system does not say.
    pub removable: Option<bool>,
    /// Where the file system holding the path is mounted, if known.
    pub mount_point: Option<PathBuf>,
}

/// The medium holding `path` (which must exist): macOS `statfs`
/// (`MNT_REMOVABLE`), Linux sysfs (`/sys/class/block/<disk>/removable`),
/// Windows `GetDriveTypeW` (`DRIVE_REMOVABLE`). Reads nothing of the file.
pub fn media_info(path: &Path) -> MediaInfo {
    #[cfg(windows)]
    return windows::media_info(path);
    #[cfg(any(target_vendor = "apple", target_os = "linux"))]
    return mount_of(path)
        .map(|mount| MediaInfo {
            removable: mount.removable(),
            mount_point: Some(mount.mount_point),
        })
        .unwrap_or_default();
    #[cfg(not(any(windows, target_vendor = "apple", target_os = "linux")))]
    {
        let _ = path;
        MediaInfo::default()
    }
}

/// The UUID (uppercase, dashed) and name of the volume mounted at
/// `mount_point` (`getattrlist`: `ATTR_VOL_UUID`, `ATTR_VOL_NAME`); `None`
/// when the call fails or the volume has no UUID. It asks the file system,
/// so on a network share it may wait for the server.
#[cfg(target_vendor = "apple")]
pub fn volume_uuid_and_name(mount_point: &Path) -> Option<(String, String)> {
    macos::volume_uuid_and_name(mount_point)
}

#[cfg(target_os = "linux")]
impl Mount {
    /// The file system's UUID: the name of the `/dev/disk/by-uuid` link to
    /// this mount's device.
    pub fn disk_uuid(&self) -> Option<String> {
        self.is_device()
            .then(|| linux::link_to("/dev/disk/by-uuid", &self.source))
            .flatten()
    }

    /// The file system's label: the name of the `/dev/disk/by-label` link
    /// to this mount's device.
    pub fn disk_label(&self) -> Option<String> {
        self.is_device()
            .then(|| linux::link_to("/dev/disk/by-label", &self.source))
            .flatten()
    }
}

impl Mount {
    /// Whether this mount can hold photographs: on Linux not a runtime or
    /// virtual file system and not an application's or container's mount
    /// (`/run/media` stays, for removable drives); everything elsewhere.
    pub fn is_storage(&self) -> bool {
        !cfg!(target_os = "linux") || linux_storage(&self.file_system, &self.mount_point)
    }
}

/// Every mount (device and mount point) that holds drives or shares, as one
/// key that changes when a drive is connected, ejected or mounted
/// elsewhere. Cheap: no volume is opened.
pub fn mounts_key() -> String {
    let mut lines = imp::mount_lines();
    lines.sort();
    lines.join("\n")
}

/// Whether a macOS mount counts for [`mounts_key`]. Browsable mounts always
/// do. Of the hidden ones (`MNT_DONTBROWSE`) only drives under `/Volumes/`:
/// a drive or disk image mounted without a Finder icon (`hdiutil attach
/// -nobrowse`) can hold catalog photographs too, and was missed when it was
/// ejected or connected. System volumes (`/System/Volumes/…`, `/private/…`)
/// and Time Machine's snapshot mounts, which come and go without a drive
/// changing, stay out.
#[cfg_attr(not(target_vendor = "apple"), allow(dead_code))]
fn watched_mount(hidden: bool, mount_point: &Path) -> bool {
    if !hidden {
        return true;
    }
    let Ok(rest) = mount_point.strip_prefix("/Volumes") else {
        return false;
    };
    let mut parts = rest.components();
    match parts.next() {
        Some(std::path::Component::Normal(name)) => {
            let name = name.to_string_lossy();
            !name.starts_with('.') && !name.starts_with("com.apple.TimeMachine")
        }
        _ => false,
    }
}

/// Blocks until the system reports a mount change; false when it cannot
/// (the caller falls back to polling).
type MountWaiter = Box<dyn FnMut() -> bool + Send>;

/// How [`watch_mounts`] learns of mount changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum WatchKind {
    /// System notifications: kqueue `EVFILT_FS` on macOS (the kernel's
    /// mount/unmount events, what DiskArbitration builds on), `poll` on
    /// `/proc/self/mountinfo` on Linux.
    Notifications,
    /// Asking the mount list every [`POLL_INTERVAL`] (Windows, or when
    /// notifications cannot be set up).
    Polling,
}

/// How often the fallback asks the mount list.
pub const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

/// Calls `on_change` from a background thread whenever the mounts change
/// (a drive connected, ejected or mounted elsewhere), for as long as the
/// process runs. Notification bursts are coalesced: `on_change` runs once
/// the mount list differs from the last one it was called for.
pub fn watch_mounts(on_change: impl Fn() + Send + 'static) -> WatchKind {
    let waiter = imp::mount_waiter();
    let kind = if waiter.is_some() {
        WatchKind::Notifications
    } else {
        WatchKind::Polling
    };
    let spawned = std::thread::Builder::new()
        .name("mount-watch".into())
        .spawn(move || {
            let mut last = mounts_key();
            let mut waiter = waiter;
            loop {
                match waiter.as_mut().map(|wait| wait()) {
                    Some(true) => {}
                    Some(false) => {
                        log::warn!("Mount notifications failed; asking the mount list instead");
                        waiter = None;
                    }
                    None => std::thread::sleep(POLL_INTERVAL),
                }
                // The mount list may lag the notification briefly.
                for delay_ms in [0, 50, 250, 1000] {
                    std::thread::sleep(std::time::Duration::from_millis(delay_ms));
                    let key = mounts_key();
                    if key != last {
                        last = key;
                        on_change();
                        break;
                    }
                    if waiter.is_none() {
                        break;
                    }
                }
            }
        });
    if let Err(e) = spawned {
        log::warn!("No mount watcher: {e}");
    }
    kind
}

/// See [`Mount::is_storage`]: the Linux rule, by file system type and mount
/// point (other systems list no virtual file systems as mounts that could
/// be mistaken for storage).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn linux_storage(file_system: &str, mount: &Path) -> bool {
    if matches!(
        file_system,
        "proc"
            | "sysfs"
            | "devtmpfs"
            | "devpts"
            | "cgroup"
            | "cgroup2"
            | "securityfs"
            | "debugfs"
            | "tracefs"
            | "configfs"
            | "pstore"
            | "efivarfs"
            | "mqueue"
            | "hugetlbfs"
            | "rpc_pipefs"
            | "autofs"
            | "overlay"
            | "aufs"
            | "fuse.portal"
            | "nsfs"
            | "fusectl"
            | "binfmt_misc"
            | "bpf"
            | "selinuxfs"
            | "smackfs"
    ) {
        return false;
    }
    if ["/snap", "/var/lib/snapd", "/var/lib/flatpak"]
        .iter()
        .any(|p| mount.starts_with(p))
        || mount
            .components()
            .map(|c| c.as_os_str())
            .collect::<Vec<_>>()
            .windows(3)
            .any(|p| p == [".local", "share", "flatpak"])
    {
        return false;
    }
    !(file_system == "tmpfs"
        && ["/run", "/dev", "/tmp", "/var/run", "/var/lock"]
            .iter()
            .any(|p| mount.starts_with(p)))
}

/// The rows of `/proc/self/mountinfo` (proc(5)): mount point (field 5),
/// then after the ` - ` separator the file system type and the source;
/// malformed rows are skipped.
#[cfg(any(target_os = "linux", test))]
fn parse_mountinfo(text: &str) -> Vec<Mount> {
    text.lines()
        .filter_map(|line| {
            let (before, after) = line.split_once(" - ")?;
            let mut fields = after.split_whitespace();
            let mount_point = unescape(before.split_whitespace().nth(4)?, false).into();
            let file_system: String = fields.next()?.into();
            Some(Mount {
                mount_point,
                source: unescape(fields.next()?, false).into(),
                network: is_network_fs(&file_system),
                file_system,
                #[cfg(target_vendor = "apple")]
                flags: 0,
            })
        })
        .collect()
}

/// Undoes the escapes of `/proc/self/mountinfo` (octal, `\040` for a space,
/// `hex == false`) and of udev's `/dev/disk/by-*` names (`\x20`, `hex`).
#[cfg(any(target_os = "linux", test))]
fn unescape(text: &str, hex: bool) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let start = if hex && bytes.get(i..i + 2) == Some(b"\\x") {
            Some(i + 2)
        } else if !hex && bytes[i] == b'\\' {
            Some(i + 1)
        } else {
            None
        };
        let len = if hex { 2 } else { 3 };
        let value = start
            .and_then(|start| bytes.get(start..start + len))
            .and_then(|digits| std::str::from_utf8(digits).ok())
            .and_then(|digits| u8::from_str_radix(digits, if hex { 16 } else { 8 }).ok());
        if let Some(value) = value {
            out.push(value);
            i += 4;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(target_vendor = "apple")]
use macos as imp;

#[cfg(target_vendor = "apple")]
mod macos {
    use super::Mount;
    use anyhow::{Context, Result};
    use std::ffi::{CStr, OsStr};
    use std::io;
    use std::os::unix::ffi::OsStrExt;
    use std::path::{Path, PathBuf};

    /// `MNT_REMOVABLE` (sys/mount.h): the file system is on removable media.
    pub(super) const MNT_REMOVABLE: u32 = 0x0000_0200;

    /// `MNT_DONTBROWSE` (sys/mount.h): system volumes, Time Machine snapshot
    /// mounts, and drives or disk images mounted without a Finder icon.
    const MNT_DONTBROWSE: u32 = 0x0010_0000;

    fn c_text(bytes: &[libc::c_char]) -> PathBuf {
        // SAFETY: the kernel fills statfs's name fields as NUL-terminated
        // strings within their fixed-size arrays, which outlive this borrow.
        let text = unsafe { CStr::from_ptr(bytes.as_ptr()) };
        PathBuf::from(OsStr::from_bytes(text.to_bytes()))
    }

    /// Mount-table facts only: never getattrlist or statfs a server here.
    fn mount(stat: &libc::statfs) -> Mount {
        let file_system = c_text(&stat.f_fstypename).to_string_lossy().into_owned();
        let local = stat.f_flags & libc::MNT_LOCAL as u32 != 0;
        Mount {
            mount_point: c_text(&stat.f_mntonname),
            source: c_text(&stat.f_mntfromname),
            network: !local || super::is_network_fs(&file_system),
            file_system,
            flags: stat.f_flags,
        }
    }

    pub fn mount_of(path: &Path) -> Result<Mount> {
        let c = crate::c_path(path)?;
        // SAFETY: an all-zero statfs (integers and char arrays) is a valid
        // value; statfs only writes into it.
        let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
        // SAFETY: `c` is a NUL-terminated path and `stat` a valid out
        // pointer, both alive for the call's duration.
        if unsafe { libc::statfs(c.as_ptr(), &mut stat) } != 0 {
            return Err(io::Error::last_os_error())
                .with_context(|| format!("Cannot read the volume of {}", path.display()));
        }
        Ok(mount(&stat))
    }

    fn statfs_list() -> Vec<libc::statfs> {
        // SAFETY: a null buffer of size 0 only asks for the count.
        let count = unsafe { libc::getfsstat(std::ptr::null_mut(), 0, libc::MNT_NOWAIT) };
        if count <= 0 {
            return Vec::new();
        }
        // SAFETY: an all-zero statfs (integers and char arrays) is a valid value.
        let mut mounts: Vec<libc::statfs> = vec![unsafe { std::mem::zeroed() }; count as usize + 4];
        let size = (mounts.len() * std::mem::size_of::<libc::statfs>()) as libc::c_int;
        // SAFETY: the buffer holds `size` bytes of statfs values, which
        // getfsstat fills at most.
        let filled = unsafe { libc::getfsstat(mounts.as_mut_ptr(), size, libc::MNT_NOWAIT) };
        if filled <= 0 {
            return Vec::new();
        }
        mounts.truncate(filled as usize);
        mounts
    }

    pub fn mounts() -> Vec<Mount> {
        statfs_list().iter().map(mount).collect()
    }

    pub fn mount_lines() -> Vec<String> {
        statfs_list()
            .iter()
            .filter(|stat| {
                super::watched_mount(
                    stat.f_flags & MNT_DONTBROWSE != 0,
                    &c_text(&stat.f_mntonname),
                )
            })
            .map(|stat| {
                format!(
                    "{}={}",
                    c_text(&stat.f_mntfromname).display(),
                    c_text(&stat.f_mntonname).display()
                )
            })
            .collect()
    }

    pub(super) fn volume_uuid_and_name(mount_point: &Path) -> Option<(String, String)> {
        let path = crate::c_path(mount_point).ok()?;
        let mut request = libc::attrlist {
            bitmapcount: libc::ATTR_BIT_MAP_COUNT,
            reserved: 0,
            commonattr: 0,
            volattr: libc::ATTR_VOL_INFO | libc::ATTR_VOL_NAME | libc::ATTR_VOL_UUID,
            dirattr: 0,
            fileattr: 0,
            forkattr: 0,
        };
        let mut buffer = [0u8; 4 + 8 + 16 + 1024];
        // SAFETY: `path` is NUL-terminated; the request and the buffer
        // outlive the call, and the buffer's size is passed with it.
        let status = unsafe {
            libc::getattrlist(
                path.as_ptr(),
                (&mut request as *mut libc::attrlist).cast(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                0,
            )
        };
        if status != 0 {
            return None;
        }
        // Layout: u32 length, attrreference_t of the name, uuid_t; the name's
        // offset is relative to its attrreference_t.
        let length = u32::from_ne_bytes(buffer[0..4].try_into().ok()?) as usize;
        let offset = i32::from_ne_bytes(buffer[4..8].try_into().ok()?);
        let name_len = u32::from_ne_bytes(buffer[8..12].try_into().ok()?) as usize;
        let uuid = &buffer[12..28];
        if length < 28 || uuid.iter().all(|&b| b == 0) {
            return None;
        }
        let start = usize::try_from(4 + offset).ok()?;
        let name = buffer
            .get(start..start + name_len)
            .map(|bytes| {
                String::from_utf8_lossy(bytes)
                    .trim_end_matches('\0')
                    .to_string()
            })
            .unwrap_or_default();
        let id = uuid
            .iter()
            .enumerate()
            .map(|(i, b)| {
                let dash = if matches!(i, 4 | 6 | 8 | 10) { "-" } else { "" };
                format!("{dash}{b:02X}")
            })
            .collect();
        Some((id, name))
    }

    /// kqueue's file system filter: the kernel posts an event for every
    /// mount and unmount (`VQ_MOUNT`, `VQ_UNMOUNT`).
    pub fn mount_waiter() -> Option<super::MountWaiter> {
        // SAFETY: kqueue takes no arguments; a negative result is handled.
        let queue = unsafe { libc::kqueue() };
        if queue < 0 {
            return None;
        }
        let change = libc::kevent {
            ident: 0,
            filter: libc::EVFILT_FS,
            flags: libc::EV_ADD | libc::EV_CLEAR,
            fflags: 0,
            data: 0,
            udata: std::ptr::null_mut(),
        };
        // SAFETY: `queue` is our open kqueue; one valid change is passed
        // and no event buffer (count 0, null pointer).
        let added =
            unsafe { libc::kevent(queue, &change, 1, std::ptr::null_mut(), 0, std::ptr::null()) };
        if added < 0 {
            // SAFETY: `queue` is our own descriptor, closed once, unused after.
            unsafe { libc::close(queue) };
            return None;
        }
        Some(Box::new(move || {
            loop {
                // SAFETY: an all-zero kevent (integers and a null pointer)
                // is a valid value; kevent only writes into it.
                let mut event: libc::kevent = unsafe { std::mem::zeroed() };
                // SAFETY: `queue` stays open for the closure's life; no
                // changes, room for one event in `event`, no timeout.
                let n = unsafe {
                    libc::kevent(queue, std::ptr::null(), 0, &mut event, 1, std::ptr::null())
                };
                if n > 0 {
                    return true;
                }
                if n < 0 && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
                    return false;
                }
            }
        }))
    }
}

#[cfg(target_os = "linux")]
use linux as imp;

#[cfg(target_os = "linux")]
mod linux {
    use super::Mount;
    use anyhow::{Context, Result};
    use std::fs;
    use std::path::{Path, PathBuf};

    fn mount_table() -> std::io::Result<Vec<Mount>> {
        fs::read_to_string("/proc/self/mountinfo").map(|text| super::parse_mountinfo(&text))
    }

    pub fn mount_of(path: &Path) -> Result<Mount> {
        let path = path
            .canonicalize()
            .with_context(|| format!("Cannot resolve {}", path.display()))?;
        mount_table()
            .context("Cannot read the mount table")?
            .into_iter()
            .filter(|entry| path.starts_with(&entry.mount_point))
            // The last of equally long ones: a path mounted over.
            .max_by_key(|entry| entry.mount_point.as_os_str().len())
            .context("The volume was not found in the mount table")
    }

    pub fn mounts() -> Vec<Mount> {
        mount_table().unwrap_or_default()
    }

    /// Mounts of drives and network shares (not the many virtual file
    /// systems, which come and go with containers and snaps).
    pub fn mount_lines() -> Vec<String> {
        mounts()
            .into_iter()
            .filter(|entry| entry.is_storage() && (entry.is_device() || entry.network))
            .map(|entry| format!("{}={}", entry.source.display(), entry.mount_point.display()))
            .collect()
    }

    /// The link name in `dir` (`/dev/disk/by-uuid`, `by-label`) pointing at `device`.
    pub(super) fn link_to(dir: &str, device: &Path) -> Option<String> {
        let device = device.canonicalize().ok()?;
        fs::read_dir(dir).ok()?.flatten().find_map(|entry| {
            (entry.path().canonicalize().ok()? == device)
                .then(|| super::unescape(&entry.file_name().to_string_lossy(), true))
        })
    }

    /// The disk's `removable` attribute in sysfs (a partition's is its
    /// disk's).
    pub(super) fn removable(device: &Path) -> Option<bool> {
        let name = device.canonicalize().ok()?.file_name()?.to_owned();
        let mut block = PathBuf::from("/sys/class/block")
            .join(name)
            .canonicalize()
            .ok()?;
        if block.join("partition").exists() {
            block = block.parent()?.to_path_buf();
        }
        let flag = fs::read_to_string(block.join("removable")).ok()?;
        Some(flag.trim() == "1")
    }

    /// `/proc/self/mountinfo` is pollable: the kernel flags it (POLLPRI)
    /// whenever the mount table changes (proc(5)).
    pub fn mount_waiter() -> Option<super::MountWaiter> {
        use std::os::fd::AsRawFd;
        let file = fs::File::open("/proc/self/mountinfo").ok()?;
        Some(Box::new(move || {
            loop {
                let mut watch = libc::pollfd {
                    fd: file.as_raw_fd(),
                    events: libc::POLLPRI,
                    revents: 0,
                };
                // SAFETY: one valid pollfd (an open descriptor the closure
                // owns) and its count; blocks until an event.
                let n = unsafe { libc::poll(&mut watch, 1, -1) };
                if n > 0 {
                    return watch.revents & (libc::POLLPRI | libc::POLLERR) != 0;
                }
                if n < 0
                    && std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted
                {
                    return false;
                }
            }
        }))
    }
}

#[cfg(windows)]
use windows as imp;

#[cfg(windows)]
mod windows {
    use super::{MediaInfo, Mount, Space};
    use anyhow::{Context, Result};
    use std::io;
    use std::path::{Path, PathBuf};

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
        fn GetVolumePathNameW(path: *const u16, volume_path: *mut u16, len: u32) -> i32;
        fn GetDiskFreeSpaceExW(
            directory: *const u16,
            free_for_caller: *mut u64,
            total: *mut u64,
            total_free: *mut u64,
        ) -> i32;
    }

    /// `GetDriveTypeW`: a card reader or USB stick.
    const DRIVE_REMOVABLE: u32 = 2;
    /// `GetDriveTypeW`: a mapped network drive, or any `\\server\share` path.
    const DRIVE_REMOTE: u32 = 4;

    /// What the system reports for the volume at `root` (`C:\`, a volume
    /// mounted into a folder of another one, `\\server\share\`): whether the
    /// call failed, its own file system name and whether the drive the path
    /// is on is remote (`DRIVE_REMOTE`).
    fn volume_of_root(root: &Path) -> Result<(bool, String, bool)> {
        const MAX_PATH: usize = 260;
        let root = crate::wide_path(root)?;
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
        // SAFETY: `root` is a NUL-terminated wide string alive for the call.
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
    pub fn mount_of(path: &Path) -> Result<Mount> {
        use std::ffi::OsString;
        use std::os::windows::ffi::OsStringExt;
        // The canonical input is absolute. A mount root cannot exceed its full
        // path plus a trailing separator; allow an extra NUL as documented by
        // GetVolumePathNameW, which can otherwise omit a trailing separator.
        let wide = crate::wide_path(path)?;
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
        Ok(Mount {
            mount_point: root,
            source: PathBuf::new(),
            file_system,
            network,
        })
    }

    pub fn mounts() -> Vec<Mount> {
        Vec::new()
    }

    /// The drive letters in use.
    pub fn mount_lines() -> Vec<String> {
        (b'A'..=b'Z')
            .map(|letter| format!("{}:\\", letter as char))
            .filter(|root| Path::new(root).exists())
            .collect()
    }

    pub fn mount_waiter() -> Option<super::MountWaiter> {
        None
    }

    /// Bytes free to the calling user on the volume of `path`
    /// (`GetDiskFreeSpaceExW`, as `f_bavail` is on Unix), and its size.
    pub fn space(path: &Path) -> Result<Space> {
        let directory = crate::wide_path(path)?;
        let (mut free_for_caller, mut total) = (0u64, 0u64);
        // SAFETY: a NUL-terminated wide path that outlives the call, valid
        // out values, and null for the total free bytes not asked for.
        let ok = unsafe {
            GetDiskFreeSpaceExW(
                directory.as_ptr(),
                &mut free_for_caller,
                &mut total,
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
        Ok(Space {
            free: free_for_caller,
            total,
        })
    }

    /// The drive's type (`GetDriveTypeW`): `DRIVE_REMOVABLE` is a card
    /// reader or USB stick.
    pub fn media_info(path: &Path) -> MediaInfo {
        use std::os::windows::ffi::OsStrExt;
        use std::path::{Component, Prefix};
        let letter = match path.components().next() {
            Some(Component::Prefix(prefix)) => match prefix.kind() {
                Prefix::Disk(letter) | Prefix::VerbatimDisk(letter) => letter as char,
                _ => return MediaInfo::default(),
            },
            _ => return MediaInfo::default(),
        };
        let root = format!("{letter}:\\");
        let wide: Vec<u16> = std::ffi::OsStr::new(&root)
            .encode_wide()
            .chain(Some(0))
            .collect();
        // SAFETY: a NUL-terminated wide string that outlives the call.
        let kind = unsafe { GetDriveTypeW(wide.as_ptr()) };
        MediaInfo {
            removable: Some(kind == DRIVE_REMOVABLE),
            mount_point: Some(PathBuf::from(root)),
        }
    }
}

#[cfg(not(any(target_vendor = "apple", target_os = "linux", windows)))]
use other as imp;

#[cfg(not(any(target_vendor = "apple", target_os = "linux", windows)))]
mod other {
    use super::Mount;
    use anyhow::Result;
    use std::path::{Path, PathBuf};

    pub fn mount_of(path: &Path) -> Result<Mount> {
        let root = path.ancestors().last().unwrap_or(path).to_path_buf();
        Ok(Mount {
            mount_point: root,
            source: PathBuf::new(),
            file_system: "unknown".into(),
            network: false,
        })
    }

    pub fn mounts() -> Vec<Mount> {
        Vec::new()
    }

    pub fn mount_lines() -> Vec<String> {
        Vec::new()
    }

    pub fn mount_waiter() -> Option<super::MountWaiter> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn network_file_systems_are_named() {
        for name in ["nfs4", "cifs", "smb3", "fuse.sshfs", "fuse.gvfsd-fuse"] {
            assert!(is_network_fs(name), "{name}");
        }
        for name in ["ext4", "btrfs", "exfat", "vfat", "fuseblk", "tmpfs"] {
            assert!(!is_network_fs(name), "{name}");
        }
    }

    #[test]
    fn mount_escapes_are_read() {
        assert_eq!(
            unescape(r"/run/media/u/My\040Disk", false),
            "/run/media/u/My Disk"
        );
        assert_eq!(unescape(r"/a\134b", false), r"/a\b");
        assert_eq!(unescape("/plain", false), "/plain");
        assert_eq!(unescape(r"My\x20Disk", true), "My Disk");
    }

    #[test]
    fn linux_storage_ignores_pseudo_and_application_mounts() {
        let mounts = parse_mountinfo(concat!(
            "1 0 8:1 / / rw - ext4 /dev/sda1 rw\n",
            "2 1 0:1 / /proc rw - proc proc rw\n",
            "3 1 0:2 / /sys rw - sysfs sysfs rw\n",
            "4 1 0:3 / /run rw - tmpfs tmpfs rw\n",
            "5 1 0:4 / /run/user/1000 rw - tmpfs tmpfs rw\n",
            "6 1 0:5 / /var/lib/containers/merged rw - overlay overlay rw\n",
            "7 1 7:0 / /snap/app/1 ro - squashfs /dev/loop0 ro\n",
            "8 1 8:1 /app /var/lib/flatpak/app rw - ext4 /dev/sda1 rw\n",
            "9 1 0:6 / /run/user/1000/doc rw - fuse.portal portal rw\n",
            "10 1 8:2 / /run/media/me/Photos rw - ext4 /dev/sdb1 rw\n",
            "11 1 0:7 / /mnt/Photos rw - cifs //nas/Photos rw\n",
            "12 1 8:3 / /srv/library rw - btrfs /dev/sdc1 rw\n",
        ));
        let paths: Vec<_> = mounts
            .iter()
            .filter(|m| linux_storage(&m.file_system, &m.mount_point))
            .map(|m| m.mount_point.as_path())
            .collect();
        assert_eq!(
            paths,
            [
                Path::new("/"),
                Path::new("/run/media/me/Photos"),
                Path::new("/mnt/Photos"),
                Path::new("/srv/library")
            ]
        );
    }

    #[test]
    fn linux_mountinfo_fixture_preserves_mounts_and_network_types() {
        let mounts = parse_mountinfo(concat!(
            "21 1 8:1 / / rw - ext4 /dev/sda1 rw\n",
            "22 21 8:2 / /media/me/My\\040Disk rw shared:1 - exfat /dev/sdb1 rw\n",
            "23 21 0:7 / /mnt/Photos rw - cifs //nas/Photos rw\n",
            "24 21 0:8 / /srv/archive rw - nfs4 nas:/archive rw\n",
            "25 21 0:9 / /home/me/remote rw - fuse.sshfs me@host:/photos rw\n",
            "malformed row\n",
        ));
        assert_eq!(mounts.len(), 5);
        assert_eq!(mounts[1].mount_point, Path::new("/media/me/My Disk"));
        assert_eq!(mounts[1].source, Path::new("/dev/sdb1"));
        assert_eq!(mounts[1].file_system, "exfat");
        assert!(mounts[1].is_device());
        assert!(!mounts[0].network);
        assert!(!mounts[1].network);
        assert!(mounts[2..].iter().all(|m| m.network && !m.is_device()));
    }

    #[test]
    fn hidden_drives_under_volumes_count_but_system_and_snapshot_mounts_do_not() {
        // Browsable: always.
        assert!(watched_mount(false, Path::new("/Volumes/Photos")));
        assert!(watched_mount(false, Path::new("/private/tmp/mnt")));
        // A disk image or drive mounted without a Finder icon.
        assert!(watched_mount(true, Path::new("/Volumes/LRCDISK")));
        assert!(watched_mount(true, Path::new("/Volumes/Photos 1")));
        // System volumes and Time Machine snapshot mounts.
        assert!(!watched_mount(true, Path::new("/")));
        assert!(!watched_mount(true, Path::new("/System/Volumes/Data")));
        assert!(!watched_mount(true, Path::new("/System/Volumes/VM")));
        assert!(!watched_mount(
            true,
            Path::new("/Volumes/.timemachine/ABC/2026-10-10-100000.backup")
        ));
        assert!(!watched_mount(
            true,
            Path::new("/Volumes/com.apple.TimeMachine.localsnapshots/Backups.backupdb")
        ));
        assert!(!watched_mount(true, Path::new("/Volumes")));
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

    /// The mount a path is on is one of the mounts listed, and the space
    /// figures of its folder are consistent.
    #[cfg(any(target_vendor = "apple", target_os = "linux"))]
    #[test]
    fn a_folders_mount_is_among_the_listed_ones() {
        let tmp = tempfile::tempdir().unwrap();
        let mount = mount_of(tmp.path()).unwrap();
        assert!(
            mounts().iter().any(|m| m.mount_point == mount.mount_point),
            "{mount:?} not listed"
        );
        let space = space(tmp.path()).unwrap();
        assert!(space.free > 0 && space.free <= space.total, "{space:?}");
        let media = media_info(tmp.path());
        assert_eq!(media.mount_point.as_ref(), Some(&mount.mount_point));
        assert!(!mounts_key().is_empty());
    }

    /// A Windows path is on the volume root the system gives it (`C:\\`
    /// for a temporary folder), even when it does not exist yet, and the
    /// free space is the user's to write there (a real query, not a
    /// stand-in).
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
        // The drive the temporary folder is on needs room to make one.
        assert!(
            info.free_bytes > 0,
            "a temporary folder needs free space, got {info:?}"
        );
        // Free space that cannot be read is an error, never a figure.
        let unreadable = Path::new(r"\\?\GLOBALROOT\Device\Null");
        assert!(space(unreadable).is_err(), "{unreadable:?}");
    }
}
