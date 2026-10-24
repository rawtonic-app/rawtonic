//! Volume identity: which disk a catalog folder is on, so that a disconnected
//! drive is told apart from a missing file and a drive mounted under another
//! name or mount point (`/Volumes/Photos 1`, another Linux mount) is followed
//! without relinking. Reads only; nothing is written to the volume.
//!
//! macOS: the volume UUID and name (`getattrlist`), mounts from `getfsstat`.
//! Linux: the filesystem UUID and label from `/dev/disk/by-uuid` and
//! `/dev/disk/by-label` for the device in `/proc/self/mountinfo`. Elsewhere
//! no volume is known (the catalog then works by path alone).
//!
//! Mount changes ([`watch_mounts`]) come from the system where it tells:
//! kqueue's file system events on macOS, a pollable `/proc/self/mountinfo`
//! on Linux; elsewhere (Windows) the mount list is asked every two seconds.
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Blocks until the system reports a mount change; false when it cannot
/// (the caller falls back to polling).
type MountWaiter = Box<dyn FnMut() -> bool + Send>;

/// A mounted volume.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Volume {
    /// How the system classifies this mount; persisted for offline folders.
    #[serde(default)]
    pub kind: VolumeKind,
    /// Filesystem UUID when available; otherwise a hash of mount source and
    /// mount point (network shares and filesystems without UUIDs).
    pub id: String,
    /// The name the user knows the volume by.
    pub name: String,
    /// Where it is mounted now.
    pub mount_point: PathBuf,
}

impl Volume {
    /// Match a stored identity, including a pre-mount-ID macOS share UUID.
    /// Ordinary enumeration stays mount-table-only; only a saved UUID needs
    /// the legacy filesystem query. A mount path alone is never an identity.
    pub fn matches_id(&self, saved: &str) -> bool {
        if self.id == saved {
            return true;
        }
        #[cfg(target_os = "macos")]
        if self.kind == VolumeKind::Network && uuid_id(saved) {
            static BUSY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
            let mount = self.mount_point.clone();
            let kind = self.kind;
            return bounded_uuid_lookup(
                move || imp::uuid_volume(&mount, kind).map(|volume| volume.id),
                std::time::Duration::from_millis(100),
                &BUSY,
            )
            .is_some_and(|uuid| uuid.eq_ignore_ascii_case(saved));
        }
        false
    }
}

/// One legacy query may be in flight. A stuck server must neither hold up
/// folder listings nor create another stuck thread on every refresh.
#[cfg(any(target_os = "macos", test))]
fn bounded_uuid_lookup(
    lookup: impl FnOnce() -> Option<String> + Send + 'static,
    timeout: std::time::Duration,
    busy: &'static std::sync::atomic::AtomicBool,
) -> Option<String> {
    use std::sync::atomic::Ordering;
    if busy
        .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        return None;
    }
    struct Release(&'static std::sync::atomic::AtomicBool);
    impl Drop for Release {
        fn drop(&mut self) {
            self.0.store(false, Ordering::Release);
        }
    }
    let release = Release(busy);
    let (send, receive) = std::sync::mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("legacy-volume-uuid".into())
        .spawn(move || {
            let value = lookup();
            drop(release);
            let _ = send.send(value);
        })
        .ok()?;
    receive.recv_timeout(timeout).ok().flatten()
}

#[cfg(target_os = "macos")]
fn uuid_id(id: &str) -> bool {
    id.len() == 36
        && id.bytes().enumerate().all(|(i, b)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        })
}

/// The role of a volume in the Library, independent of its current availability.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VolumeKind {
    Startup,
    #[default]
    Local,
    Removable,
    Network,
}

impl VolumeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Startup => "startup",
            Self::Local => "local",
            Self::Removable => "removable",
            Self::Network => "network",
        }
    }

    pub fn from_record(value: &str) -> Self {
        match value {
            "startup" => Self::Startup,
            "removable" => Self::Removable,
            "network" => Self::Network,
            _ => Self::Local,
        }
    }
}

/// Classification from mount facts only. No connection to a server is opened.
#[cfg(any(unix, test))]
fn classify(mount: &Path, network: bool, removable: bool) -> VolumeKind {
    if network {
        VolumeKind::Network
    } else if mount == Path::new("/")
        || (cfg!(target_os = "macos") && mount == Path::new("/System/Volumes/Data"))
    {
        VolumeKind::Startup
    } else if removable {
        VolumeKind::Removable
    } else {
        VolumeKind::Local
    }
}

#[cfg(any(target_os = "linux", test))]
fn network_fs(name: &str) -> bool {
    matches!(
        name,
        "nfs"
            | "nfs4"
            | "cifs"
            | "smb"
            | "smb3"
            | "smbfs"
            | "afpfs"
            | "webdav"
            | "davfs"
            | "ncpfs"
            | "afs"
            | "ceph"
            | "glusterfs"
            | "9p"
            | "fuse.sshfs"
            | "fuse.rclone"
            | "fuse.gvfsd-fuse"
            | "fuse.s3fs"
    )
}

/// Mount-derived identity when a filesystem UUID is unavailable (e.g. SMB).
/// Hash the source so connection strings are never exposed in the catalog/UI.
#[cfg(unix)]
fn mount_id(source: &str, mount: &Path) -> String {
    format!(
        "mount:{:016x}",
        xxhash_rust::xxh3::xxh3_64(format!("{source}\0{}", mount.display()).as_bytes())
    )
}

/// The volume holding `path` (which must exist), if it can be identified.
pub fn volume_of(path: &Path) -> Option<Volume> {
    imp::volume_of(path)
}

/// Every mounted volume that has an identity.
pub fn mounted_volumes() -> Vec<Volume> {
    imp::mounted_volumes()
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
    imp::media_info(path)
}

/// (free, total) bytes of the file system holding `path`.
pub fn space(path: &Path) -> Option<(u64, u64)> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
        // SAFETY: an all-zero statvfs is valid; statvfs fills it.
        let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
        // SAFETY: valid path and out pointer.
        if unsafe { libc::statvfs(c.as_ptr(), &mut stat) } != 0 {
            return None;
        }
        let unit = stat.f_frsize as u64;
        Some((stat.f_bavail as u64 * unit, stat.f_blocks as u64 * unit))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

/// Every mount (device and mount point, identified or not) as one key that
/// changes when a drive is connected, ejected or mounted elsewhere. Cheap:
/// no volume is opened.
pub fn mounts_key() -> String {
    let mut lines = imp::mounts();
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
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
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

#[cfg(target_os = "macos")]
mod imp {
    use super::Volume;
    use std::ffi::{CStr, CString, OsStr};
    use std::os::unix::ffi::OsStrExt;
    use std::path::{Path, PathBuf};

    fn c_path(path: &Path) -> Option<CString> {
        CString::new(path.as_os_str().as_bytes()).ok()
    }

    fn c_text(bytes: &[libc::c_char]) -> PathBuf {
        // SAFETY: statfs fills NUL-terminated strings.
        let text = unsafe { CStr::from_ptr(bytes.as_ptr()) };
        PathBuf::from(OsStr::from_bytes(text.to_bytes()))
    }

    /// Name and UUID from an already-read mount entry. Remote mounts use
    /// mount-table facts only: never getattrlist/statfs a server here.
    fn identify(stat: &libc::statfs) -> Option<Volume> {
        let mount_point = c_text(&stat.f_mntonname);
        let network = stat.f_flags & libc::MNT_LOCAL as u32 == 0;
        let kind = super::classify(&mount_point, network, stat.f_flags & MNT_REMOVABLE != 0);
        if network {
            let source = c_text(&stat.f_mntfromname);
            return Some(Volume {
                id: super::mount_id(&source.to_string_lossy(), &mount_point),
                name: mount_point.file_name()?.to_string_lossy().into_owned(),
                mount_point,
                kind,
            });
        }
        uuid_volume(&mount_point, kind)
    }

    /// The UUID form used before network mounts gained mount-derived IDs.
    pub(super) fn uuid_volume(mount_point: &Path, kind: super::VolumeKind) -> Option<Volume> {
        let path = c_path(mount_point)?;
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
        // SAFETY: the request and buffer outlive the call; the buffer size is passed.
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
        Some(Volume {
            id,
            name,
            mount_point: mount_point.to_path_buf(),
            kind,
        })
    }

    pub fn volume_of(path: &Path) -> Option<Volume> {
        let c = c_path(path)?;
        // SAFETY: an all-zero statfs is valid; statfs fills it.
        let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
        // SAFETY: valid path and out pointer.
        if unsafe { libc::statfs(c.as_ptr(), &mut stat) } != 0 {
            return None;
        }
        identify(&stat)
    }

    /// `MNT_REMOVABLE` (sys/mount.h): the file system is on removable media.
    const MNT_REMOVABLE: u32 = 0x0000_0200;

    pub fn media_info(path: &Path) -> super::MediaInfo {
        let Some(c) = c_path(path) else {
            return super::MediaInfo::default();
        };
        // SAFETY: an all-zero statfs is valid; statfs fills it.
        let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
        // SAFETY: valid path and out pointer.
        if unsafe { libc::statfs(c.as_ptr(), &mut stat) } != 0 {
            return super::MediaInfo::default();
        }
        super::MediaInfo {
            removable: Some(stat.f_flags & MNT_REMOVABLE != 0),
            mount_point: Some(c_text(&stat.f_mntonname)),
        }
    }

    fn statfs_list() -> Vec<libc::statfs> {
        // SAFETY: a null buffer asks for the count.
        let count = unsafe { libc::getfsstat(std::ptr::null_mut(), 0, libc::MNT_NOWAIT) };
        if count <= 0 {
            return Vec::new();
        }
        // SAFETY: all-zero statfs values are valid.
        let mut mounts: Vec<libc::statfs> = vec![unsafe { std::mem::zeroed() }; count as usize + 4];
        let size = (mounts.len() * std::mem::size_of::<libc::statfs>()) as libc::c_int;
        // SAFETY: the buffer holds `size` bytes.
        let filled = unsafe { libc::getfsstat(mounts.as_mut_ptr(), size, libc::MNT_NOWAIT) };
        if filled <= 0 {
            return Vec::new();
        }
        mounts.truncate(filled as usize);
        mounts
    }

    pub fn mounted_volumes() -> Vec<Volume> {
        statfs_list().iter().filter_map(identify).collect()
    }

    /// `MNT_DONTBROWSE` (sys/mount.h): system volumes, Time Machine snapshot
    /// mounts, and drives or disk images mounted without a Finder icon.
    const MNT_DONTBROWSE: u32 = 0x0010_0000;

    pub fn mounts() -> Vec<String> {
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

    /// kqueue's file system filter: the kernel posts an event for every
    /// mount and unmount (`VQ_MOUNT`, `VQ_UNMOUNT`).
    pub fn mount_waiter() -> Option<super::MountWaiter> {
        // SAFETY: plain system call.
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
        // SAFETY: one valid change, no event buffer.
        let added =
            unsafe { libc::kevent(queue, &change, 1, std::ptr::null_mut(), 0, std::ptr::null()) };
        if added < 0 {
            // SAFETY: our descriptor.
            unsafe { libc::close(queue) };
            return None;
        }
        Some(Box::new(move || {
            loop {
                // SAFETY: an all-zero kevent is valid; kevent fills it.
                let mut event: libc::kevent = unsafe { std::mem::zeroed() };
                // SAFETY: waits for one event into `event`.
                let n = unsafe {
                    libc::kevent(queue, std::ptr::null(), 0, &mut event, 1, std::ptr::null())
                };
                if n > 0 {
                    return true;
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

#[cfg(any(target_os = "linux", test))]
#[derive(Debug)]
struct LinuxMount {
    mount: PathBuf,
    source: PathBuf,
    fs_type: String,
}

#[cfg(any(target_os = "linux", test))]
fn linux_mounts(text: &str) -> Vec<LinuxMount> {
    text.lines()
        .filter_map(|line| {
            let (before, after) = line.split_once(" - ")?;
            let mut fields = after.split_whitespace();
            Some(LinuxMount {
                mount: unescape(before.split_whitespace().nth(4)?, false).into(),
                fs_type: fields.next()?.into(),
                source: unescape(fields.next()?, false).into(),
            })
        })
        .collect()
}

/// Keep the full table for longest-mount lookup, but do not classify runtime
/// filesystems or application/container mounts as photo storage. In particular,
/// /run/media remains available for actual removable drives.
#[cfg(any(target_os = "linux", test))]
fn linux_storage_mount(entry: &LinuxMount) -> bool {
    let mount = &entry.mount;
    if matches!(
        entry.fs_type.as_str(),
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
    !(entry.fs_type == "tmpfs"
        && ["/run", "/dev", "/tmp", "/var/run", "/var/lock"]
            .iter()
            .any(|p| mount.starts_with(p)))
}

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

#[cfg(target_os = "linux")]
mod imp {
    use super::Volume;
    use std::fs;
    use std::path::{Path, PathBuf};

    fn mount_table() -> Vec<super::LinuxMount> {
        fs::read_to_string("/proc/self/mountinfo")
            .map(|text| super::linux_mounts(&text))
            .unwrap_or_default()
    }

    /// The link name in `dir` (`/dev/disk/by-uuid`, `by-label`) pointing at `device`.
    fn link_to(dir: &str, device: &Path) -> Option<String> {
        let device = device.canonicalize().ok()?;
        fs::read_dir(dir).ok()?.flatten().find_map(|entry| {
            (entry.path().canonicalize().ok()? == device)
                .then(|| super::unescape(&entry.file_name().to_string_lossy(), true))
        })
    }

    fn identify(entry: &super::LinuxMount) -> Option<Volume> {
        if !super::linux_storage_mount(entry) {
            return None;
        }
        let (mount_point, device) = (&entry.mount, &entry.source);
        let network = super::network_fs(&entry.fs_type);
        let block = device.starts_with("/dev/");
        let id = block
            .then(|| link_to("/dev/disk/by-uuid", device))
            .flatten()
            .unwrap_or_else(|| super::mount_id(&device.to_string_lossy(), mount_point));
        let kind = super::classify(
            mount_point,
            network,
            block && removable(device).unwrap_or(false),
        );
        let name = block
            .then(|| link_to("/dev/disk/by-label", device))
            .flatten()
            .unwrap_or_else(|| {
                mount_point
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "/".into())
            });
        Some(Volume {
            id,
            name,
            mount_point: mount_point.to_path_buf(),
            kind,
        })
    }

    pub fn volume_of(path: &Path) -> Option<Volume> {
        let path = path.canonicalize().ok()?;
        let entry = mount_table()
            .into_iter()
            .filter(|entry| path.starts_with(&entry.mount))
            .max_by_key(|entry| entry.mount.as_os_str().len())?;
        identify(&entry)
    }

    /// The disk's `removable` attribute in sysfs (a partition's is its
    /// disk's).
    fn removable(device: &Path) -> Option<bool> {
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

    pub fn media_info(path: &Path) -> super::MediaInfo {
        let Ok(path) = path.canonicalize() else {
            return super::MediaInfo::default();
        };
        let Some(entry) = mount_table()
            .into_iter()
            .filter(|entry| path.starts_with(&entry.mount))
            .max_by_key(|entry| entry.mount.as_os_str().len())
        else {
            return super::MediaInfo::default();
        };
        super::MediaInfo {
            removable: entry
                .source
                .starts_with("/dev/")
                .then(|| removable(&entry.source))
                .flatten(),
            mount_point: Some(entry.mount),
        }
    }

    pub fn mounted_volumes() -> Vec<Volume> {
        mount_table().iter().filter_map(identify).collect()
    }

    /// Mounts of drives and network shares (not the many virtual file
    /// systems, which come and go with containers and snaps).
    pub fn mounts() -> Vec<String> {
        mount_table()
            .into_iter()
            .filter(|entry| {
                let source = entry.source.to_string_lossy();
                super::linux_storage_mount(entry)
                    && (source.starts_with("/dev/") || super::network_fs(&entry.fs_type))
            })
            .map(|entry| format!("{}={}", entry.source.display(), entry.mount.display()))
            .collect()
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
                // SAFETY: one valid pollfd; blocks until an event.
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

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod imp {
    use super::Volume;
    use std::path::Path;

    pub fn volume_of(_: &Path) -> Option<Volume> {
        None
    }

    /// The drive's type (`GetDriveTypeW`): `DRIVE_REMOVABLE` is a card
    /// reader or USB stick.
    #[cfg(windows)]
    pub fn media_info(path: &Path) -> super::MediaInfo {
        use std::os::windows::ffi::OsStrExt;
        use std::path::{Component, PathBuf, Prefix};
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetDriveTypeW(root: *const u16) -> u32;
        }
        const DRIVE_REMOVABLE: u32 = 2;
        let letter = match path.components().next() {
            Some(Component::Prefix(prefix)) => match prefix.kind() {
                Prefix::Disk(letter) | Prefix::VerbatimDisk(letter) => letter as char,
                _ => return super::MediaInfo::default(),
            },
            _ => return super::MediaInfo::default(),
        };
        let root = format!("{letter}:\\");
        let wide: Vec<u16> = std::ffi::OsStr::new(&root)
            .encode_wide()
            .chain(Some(0))
            .collect();
        // SAFETY: a NUL-terminated wide string that outlives the call.
        let kind = unsafe { GetDriveTypeW(wide.as_ptr()) };
        super::MediaInfo {
            removable: Some(kind == DRIVE_REMOVABLE),
            mount_point: Some(PathBuf::from(root)),
        }
    }

    #[cfg(not(windows))]
    pub fn media_info(_: &Path) -> super::MediaInfo {
        super::MediaInfo::default()
    }

    pub fn mounted_volumes() -> Vec<Volume> {
        Vec::new()
    }

    /// The drive letters in use (Windows); nothing elsewhere.
    pub fn mounts() -> Vec<String> {
        if !cfg!(windows) {
            return Vec::new();
        }
        (b'A'..=b'Z')
            .map(|letter| format!("{}:\\", letter as char))
            .filter(|root| Path::new(root).exists())
            .collect()
    }

    pub fn mount_waiter() -> Option<super::MountWaiter> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stalled_legacy_lookup_leaves_folders_offline_and_is_cached() {
        use crate::folders::{Probe, by_volume};
        use std::sync::{
            Mutex,
            atomic::{AtomicBool, Ordering},
            mpsc,
        };
        static BUSY: AtomicBool = AtomicBool::new(false);
        struct Slow {
            wait: Mutex<Option<mpsc::Receiver<()>>>,
            finished: mpsc::SyncSender<()>,
            calls: std::cell::Cell<usize>,
        }
        impl Probe for Slow {
            fn mounted(&self) -> Vec<Volume> {
                vec![Volume {
                    kind: VolumeKind::Network,
                    id: "mount:share".into(),
                    name: "Photos".into(),
                    mount_point: "/Volumes/Photos".into(),
                }]
            }
            fn volume_of(&self, _: &Path) -> Option<Volume> {
                None
            }
            fn is_dir(&self, _: &Path) -> bool {
                false
            }
            fn space(&self, _: &Path) -> Option<(u64, u64)> {
                None
            }
            fn matches_id(&self, _: &Volume, saved: &str) -> bool {
                self.calls.set(self.calls.get() + 1);
                let wait = self.wait.lock().unwrap().take().unwrap();
                let finished = self.finished.clone();
                bounded_uuid_lookup(
                    move || {
                        wait.recv().unwrap();
                        finished.send(()).unwrap();
                        Some("12345678-1234-1234-1234-123456789ABC".into())
                    },
                    std::time::Duration::from_millis(10),
                    &BUSY,
                )
                .is_some_and(|id| id == saved)
            }
        }
        let (release, wait) = mpsc::sync_channel(1);
        let (finished, done) = mpsc::sync_channel(1);
        let probe = Slow {
            wait: Mutex::new(Some(wait)),
            finished,
            calls: Default::default(),
        };
        let root = PathBuf::from("/Volumes/Photos/Jobs");
        let record = crate::FolderRecord {
            path: root.clone(),
            photos: 1,
            missing: 1,
            volume_id: Some("12345678-1234-1234-1234-123456789ABC".into()),
            volume_name: Some("Photos".into()),
            volume_path: Some("/Volumes/Photos".into()),
            volume_kind: None,
        };
        let groups = by_volume(&[record], &[root], &probe);
        assert_eq!(groups.len(), 1);
        assert!(!groups[0].online, "a timeout gives no verified identity");
        assert_eq!(probe.calls.get(), 1, "folder and root share one lookup");
        assert!(BUSY.load(Ordering::Acquire));
        assert_eq!(
            bounded_uuid_lookup(
                || panic!("must not start a second query"),
                std::time::Duration::from_millis(10),
                &BUSY
            ),
            None
        );
        release.send(()).unwrap();
        done.recv_timeout(std::time::Duration::from_secs(1))
            .unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn saved_macos_share_uuid_is_verified_before_matching_a_mount_id() {
        let dir = tempfile::tempdir().unwrap();
        // Exercise the real legacy getattrlist decoder without a live share.
        let original = volume_of(dir.path()).unwrap();
        let current = Volume {
            id: "mount:new-form".into(),
            kind: VolumeKind::Network,
            ..original.clone()
        };
        assert!(current.matches_id(&original.id));
        assert!(current.matches_id(&original.id.to_ascii_lowercase()));
        assert!(!current.matches_id("00000000-0000-0000-0000-000000000000"));
        assert!(!current.matches_id("mount:other"));
        assert!(!uuid_id("not-a-uuid"));
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

    #[test]
    fn the_volume_of_a_folder_is_among_the_mounted_ones() {
        let dir = tempfile::tempdir().unwrap();
        let volume = volume_of(dir.path());
        if cfg!(target_os = "macos") {
            assert!(volume.is_some(), "macOS volumes carry a UUID");
        }
        if let Some(volume) = volume {
            assert!(!volume.id.is_empty());
            assert!(
                mounted_volumes().iter().any(|v| v.id == volume.id),
                "{volume:?} not listed"
            );
        }
    }
}

#[cfg(test)]
mod classification_tests {
    use super::*;

    #[test]
    fn mount_facts_classify_without_using_folder_names() {
        for (mount, network, removable, expected) in [
            ("/", false, false, VolumeKind::Startup),
            ("/Volumes/External", false, false, VolumeKind::Local),
            ("/media/me/Card", false, true, VolumeKind::Removable),
            ("/Volumes/Photos", true, false, VolumeKind::Network),
            ("/mnt/card", true, true, VolumeKind::Network),
        ] {
            assert_eq!(classify(Path::new(mount), network, removable), expected);
        }
        #[cfg(target_os = "macos")]
        assert_eq!(
            classify(Path::new("/System/Volumes/Data"), false, false),
            VolumeKind::Startup
        );
    }

    #[test]
    fn linux_classification_ignores_pseudo_and_application_mounts() {
        let mounts = linux_mounts(concat!(
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
            .filter(|m| linux_storage_mount(m))
            .map(|m| m.mount.as_path())
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
        let mounts = linux_mounts(concat!(
            "21 1 8:1 / / rw - ext4 /dev/sda1 rw\n",
            "22 21 8:2 / /media/me/My\\040Disk rw shared:1 - exfat /dev/sdb1 rw\n",
            "23 21 0:7 / /mnt/Photos rw - cifs //nas/Photos rw\n",
            "24 21 0:8 / /srv/archive rw - nfs4 nas:/archive rw\n",
            "25 21 0:9 / /home/me/remote rw - fuse.sshfs me@host:/photos rw\n",
            "malformed row\n",
        ));
        assert_eq!(mounts.len(), 5);
        assert_eq!(mounts[1].mount, Path::new("/media/me/My Disk"));
        assert_eq!(mounts[1].source, Path::new("/dev/sdb1"));
        assert!(!network_fs(&mounts[0].fs_type));
        assert!(!network_fs(&mounts[1].fs_type));
        for entry in &mounts[2..] {
            assert_eq!(
                classify(&entry.mount, network_fs(&entry.fs_type), false),
                VolumeKind::Network
            );
        }
        assert_eq!(unescape(r"My\x20Disk", true), "My Disk");
        assert_eq!(unescape(r"/a\134b", false), r"/a\b");
    }
}
