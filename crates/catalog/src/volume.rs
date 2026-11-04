//! Volume identity: which disk a catalog folder is on, so that a disconnected
//! drive is told apart from a missing file and a drive mounted under another
//! name or mount point (`/Volumes/Photos 1`, another Linux mount) is followed
//! without relinking. Reads only; nothing is written to the volume.
//!
//! Built from the mount facts of [`rawtonic_fs::volume`]. macOS: the volume
//! UUID and name (`getattrlist`). Linux: the filesystem UUID and label from
//! `/dev/disk/by-uuid` and `/dev/disk/by-label` for the mounted device.
//! Network shares and file systems without a UUID: a hash of mount source
//! and mount point. Elsewhere no volume is known (the catalog then works by
//! path alone). Mount changes: [`rawtonic_fs::volume::watch_mounts`].
#[cfg(any(target_os = "macos", target_os = "linux"))]
use rawtonic_fs::volume::Mount;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

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
            return bounded_uuid_lookup(
                move || rawtonic_fs::volume::volume_uuid_and_name(&mount).map(|(id, _)| id),
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
#[cfg(any(target_os = "macos", target_os = "linux", test))]
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

/// Mount-derived identity when a filesystem UUID is unavailable (e.g. SMB).
/// Hash the source so connection strings are never exposed in the catalog/UI.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn mount_id(source: &str, mount: &Path) -> String {
    format!(
        "mount:{:016x}",
        xxhash_rust::xxh3::xxh3_64(format!("{source}\0{}", mount.display()).as_bytes())
    )
}

/// The volume holding `path` (which must exist), if it can be identified.
pub fn volume_of(path: &Path) -> Option<Volume> {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    return identify(&rawtonic_fs::volume::mount_of(path).ok()?);
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = path;
        None
    }
}

/// Every mounted volume that has an identity.
pub fn mounted_volumes() -> Vec<Volume> {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    return rawtonic_fs::volume::mounts()
        .iter()
        .filter_map(identify)
        .collect();
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    Vec::new()
}

/// The identity of a mount. macOS: a remote mount is identified from
/// mount-table facts only (never getattrlist or statfs a server here); a
/// local one by its volume UUID and name, or not at all. Linux: the device's
/// file system UUID and label where they exist, else the mount-derived
/// identity and the mount point's name; runtime and application mounts
/// have none.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn identify(mount: &Mount) -> Option<Volume> {
    if !mount.is_storage() {
        return None;
    }
    let mount_point = &mount.mount_point;
    let kind = classify(
        mount_point,
        mount.network,
        mount.removable().unwrap_or(false),
    );
    #[cfg(target_os = "macos")]
    let (id, name) = if mount.network {
        (
            mount_id(&mount.source.to_string_lossy(), mount_point),
            mount_point.file_name()?.to_string_lossy().into_owned(),
        )
    } else {
        rawtonic_fs::volume::volume_uuid_and_name(mount_point)?
    };
    #[cfg(target_os = "linux")]
    let (id, name) = (
        mount
            .disk_uuid()
            .unwrap_or_else(|| mount_id(&mount.source.to_string_lossy(), mount_point)),
        mount.disk_label().unwrap_or_else(|| {
            mount_point
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "/".into())
        }),
    );
    Some(Volume {
        id,
        name,
        mount_point: mount_point.to_path_buf(),
        kind,
    })
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
}
