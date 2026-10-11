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
    /// Stable identity (filesystem UUID); survives renaming and remounting.
    pub id: String,
    /// The name the user knows the volume by.
    pub name: String,
    /// Where it is mounted now.
    pub mount_point: PathBuf,
}

/// The volume holding `path` (which must exist), if it can be identified.
pub fn volume_of(path: &Path) -> Option<Volume> {
    imp::volume_of(path)
}

/// Every mounted volume that has an identity.
pub fn mounted_volumes() -> Vec<Volume> {
    imp::mounted_volumes()
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

    /// Name and UUID of the volume mounted at `mount_point`.
    fn identify(mount_point: &Path) -> Option<Volume> {
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
        identify(&c_text(&stat.f_mntonname))
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
        statfs_list()
            .iter()
            .filter_map(|stat| identify(&c_text(&stat.f_mntonname)))
            .collect()
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

#[cfg(target_os = "linux")]
mod imp {
    use super::Volume;
    use std::fs;
    use std::path::{Path, PathBuf};

    /// `/proc/self/mountinfo` escapes spaces and other bytes as `\ooo`
    /// (octal); udev's `/dev/disk/by-label` names as `\xhh` (hex).
    fn unescape(text: &str, hex: bool) -> String {
        let bytes = text.as_bytes();
        let mut out = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            let digits = if hex {
                (bytes[i] == b'\\' && bytes.get(i + 1) == Some(&b'x'))
                    .then(|| bytes.get(i + 2..i + 4))
                    .flatten()
            } else {
                (bytes[i] == b'\\')
                    .then(|| bytes.get(i + 1..i + 4))
                    .flatten()
            };
            let value = digits
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

    /// (mount point, device) of every mount.
    fn mount_table() -> Vec<(PathBuf, PathBuf)> {
        let Ok(text) = fs::read_to_string("/proc/self/mountinfo") else {
            return Vec::new();
        };
        text.lines()
            .filter_map(|line| {
                let (before, after) = line.split_once(" - ")?;
                let mount_point = before.split(' ').nth(4)?;
                let source = after.split(' ').nth(1)?;
                Some((
                    PathBuf::from(unescape(mount_point, false)),
                    PathBuf::from(unescape(source, false)),
                ))
            })
            .collect()
    }

    /// The link name in `dir` (`/dev/disk/by-uuid`, `by-label`) pointing at `device`.
    fn link_to(dir: &str, device: &Path) -> Option<String> {
        let device = device.canonicalize().ok()?;
        fs::read_dir(dir).ok()?.flatten().find_map(|entry| {
            (entry.path().canonicalize().ok()? == device)
                .then(|| unescape(&entry.file_name().to_string_lossy(), true))
        })
    }

    fn identify(mount_point: &Path, device: &Path) -> Option<Volume> {
        let id = link_to("/dev/disk/by-uuid", device)?;
        let name = link_to("/dev/disk/by-label", device).unwrap_or_else(|| {
            mount_point
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "/".into())
        });
        Some(Volume {
            id,
            name,
            mount_point: mount_point.to_path_buf(),
        })
    }

    pub fn volume_of(path: &Path) -> Option<Volume> {
        let path = path.canonicalize().ok()?;
        let (mount_point, device) = mount_table()
            .into_iter()
            .filter(|(mount, _)| path.starts_with(mount))
            .max_by_key(|(mount, _)| mount.as_os_str().len())?;
        identify(&mount_point, &device)
    }

    pub fn mounted_volumes() -> Vec<Volume> {
        mount_table()
            .into_iter()
            .filter(|(_, device)| device.starts_with("/dev/"))
            .filter_map(|(mount, device)| identify(&mount, &device))
            .collect()
    }

    /// Mounts of drives and network shares (not the many virtual file
    /// systems, which come and go with containers and snaps).
    pub fn mounts() -> Vec<String> {
        mount_table()
            .into_iter()
            .filter(|(_, source)| {
                let source = source.to_string_lossy();
                source.starts_with("/dev/") || source.starts_with("//") || source.contains(":/")
            })
            .map(|(mount, source)| format!("{}={}", source.display(), mount.display()))
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
