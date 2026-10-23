//! Catalog folders grouped by the drive they are on, for the Folders panel:
//! each drive with its state (connected or not) and free space, and under it
//! the catalog folders on it (wherever their photographs were registered,
//! followed or relinked to) and the library's top-level folders. Lightroom
//! and Capture One both list catalog folders under their volume; folders on
//! a disconnected drive stay listed (nothing is removed automatically).
//!
//! Reads only. What is asked of the disk (whether a folder exists, which
//! volume holds it, free space) goes through [`Probe`] so the grouping is
//! tested without real drives.
use crate::{FolderRecord, Volume, volume};
use serde::Serialize;
use std::{
    collections::HashMap,
    path::{Component, Path, PathBuf},
};

/// A catalog folder in a [`VolumeFolders`] group.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CatalogFolder {
    pub path: PathBuf,
    /// Registered photographs in it.
    pub photos: usize,
    /// Of those, the ones offline at the last check.
    pub offline: usize,
    /// The folder is there now.
    pub exists: bool,
}

/// A drive and the catalog folders on it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VolumeFolders {
    /// Stable key: `id:<volume uuid>` or, for drives without an identity,
    /// `path:<mount point>`.
    pub key: String,
    pub id: Option<String>,
    /// Its name now (when connected) or when its folders were registered.
    pub name: String,
    /// Where it is (or was last) mounted.
    pub mount_point: PathBuf,
    /// Connected now.
    pub online: bool,
    pub free_bytes: Option<u64>,
    pub total_bytes: Option<u64>,
    /// Catalog folders on it, in path order.
    pub folders: Vec<CatalogFolder>,
    /// The library's top-level folders (`roots` asked for) on it.
    pub roots: Vec<PathBuf>,
}

/// What the grouping asks of the system.
pub trait Probe {
    fn mounted(&self) -> Vec<Volume>;
    fn volume_of(&self, path: &Path) -> Option<Volume>;
    fn is_dir(&self, path: &Path) -> bool;
    /// (free, total) bytes of the volume mounted at `mount`.
    fn space(&self, mount: &Path) -> Option<(u64, u64)>;
}

/// The real disks.
pub struct SystemProbe;

impl Probe for SystemProbe {
    fn mounted(&self) -> Vec<Volume> {
        volume::mounted_volumes()
    }
    fn volume_of(&self, path: &Path) -> Option<Volume> {
        volume::volume_of(path)
    }
    fn is_dir(&self, path: &Path) -> bool {
        path.is_dir()
    }
    fn space(&self, mount: &Path) -> Option<(u64, u64)> {
        volume::space(mount)
    }
}

/// Groups catalog folders (`records`) and the library's top-level folders
/// (`roots`) by drive. Drives are sorted by name; a drive is listed when it
/// holds a catalog folder or a root.
pub fn by_volume(
    records: &[FolderRecord],
    roots: &[PathBuf],
    probe: &dyn Probe,
) -> Vec<VolumeFolders> {
    let mounted = probe.mounted();
    let mut groups: Vec<VolumeFolders> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut place = |group: VolumeFolders| -> usize {
        *index.entry(group.key.clone()).or_insert_with(|| {
            groups.push(group);
            groups.len() - 1
        })
    };
    let mut slots: Vec<(usize, Option<CatalogFolder>, Option<PathBuf>)> = Vec::new();
    // Each drive is looked at once, however many folders it holds.
    let mut by_id: HashMap<String, usize> = HashMap::new();
    for record in records {
        let exists = probe.is_dir(&record.path);
        let at = match &record.volume_id {
            Some(id) => match by_id.get(id) {
                Some(&at) => at,
                None => {
                    let at = place(known_volume(id, record, &mounted, probe));
                    by_id.insert(id.clone(), at);
                    at
                }
            },
            None => place(unknown_volume(&record.path, exists, &mounted, probe)),
        };
        slots.push((
            at,
            Some(CatalogFolder {
                path: record.path.clone(),
                photos: record.photos,
                offline: record.missing,
                exists,
            }),
            None,
        ));
    }
    for root in roots {
        let exists = probe.is_dir(root);
        // A root holding catalog folders goes with them (its drive may be
        // disconnected now, when the system cannot say where it is).
        let with_records = records
            .iter()
            .filter(|r| r.path.starts_with(root) && r.volume_id.is_some())
            .find_map(|r| r.volume_id.as_deref().map(|id| (id, r)));
        let group = match (
            exists.then(|| probe.volume_of(root)).flatten(),
            with_records,
        ) {
            (Some(volume), _) => volume_group(&volume, true, probe),
            (None, Some((id, record))) => known_volume(id, record, &mounted, probe),
            (None, None) => unknown_volume(root, exists, &mounted, probe),
        };
        let at = place(group);
        slots.push((at, None, Some(root.clone())));
    }
    for (at, folder, root) in slots {
        let group = &mut groups[at];
        if let Some(folder) = folder {
            group.folders.push(folder);
        }
        if let Some(root) = root
            && !group.roots.contains(&root)
        {
            group.roots.push(root);
        }
    }
    for group in &mut groups {
        group.folders.sort_by(|a, b| a.path.cmp(&b.path));
    }
    groups.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.key.cmp(&b.key))
    });
    groups
}

fn volume_group(volume: &Volume, online: bool, probe: &dyn Probe) -> VolumeFolders {
    let (name, mount_point) = shown(volume, probe);
    let space = online.then(|| probe.space(&volume.mount_point)).flatten();
    VolumeFolders {
        key: format!("id:{}", volume.id),
        id: Some(volume.id.clone()),
        name,
        mount_point,
        online,
        free_bytes: space.map(|s| s.0),
        total_bytes: space.map(|s| s.1),
        folders: Vec::new(),
        roots: Vec::new(),
    }
}

/// How a volume is shown. macOS keeps the user's files on the system's data
/// volume (`/System/Volumes/Data`), whose folders appear under `/`: it is
/// shown as the startup disk, at `/`.
fn shown(volume: &Volume, probe: &dyn Probe) -> (String, PathBuf) {
    if cfg!(target_os = "macos") && volume.mount_point == Path::new("/System/Volumes/Data") {
        let name = probe
            .volume_of(Path::new("/"))
            .map(|root| root.name)
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| volume.name.clone());
        return (name, PathBuf::from("/"));
    }
    (volume.name.clone(), volume.mount_point.clone())
}

/// The name the Library shows for the drive `path` is on (macOS's data
/// volume under the startup disk's name), or `None` if unknown.
pub fn shown_volume_name(path: &Path) -> Option<String> {
    let probe = SystemProbe;
    let volume = probe.volume_of(path)?;
    Some(shown(&volume, &probe).0).filter(|name| !name.is_empty())
}

/// The group of a folder registered on the volume `id`: the volume as
/// mounted now, else as it was registered (disconnected).
fn known_volume(
    id: &str,
    record: &FolderRecord,
    mounted: &[Volume],
    probe: &dyn Probe,
) -> VolumeFolders {
    if let Some(volume) = mounted.iter().find(|v| v.id == id) {
        return volume_group(volume, true, probe);
    }
    let volume = Volume {
        id: id.to_string(),
        name: record
            .volume_name
            .clone()
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| {
                fallback_name(record.volume_path.as_deref().unwrap_or(&record.path))
            }),
        mount_point: record
            .volume_path
            .clone()
            .unwrap_or_else(|| drive_root(&record.path)),
    };
    volume_group(&volume, false, probe)
}

/// The group of a folder whose drive has no recorded identity (Windows, a
/// network share, a folder registered before identities were kept): the
/// volume holding it when it is there, else the drive its path names.
fn unknown_volume(
    path: &Path,
    exists: bool,
    mounted: &[Volume],
    probe: &dyn Probe,
) -> VolumeFolders {
    if exists && let Some(volume) = probe.volume_of(path) {
        return volume_group(&volume, true, probe);
    }
    let root = drive_root(path);
    if let Some(volume) = mounted.iter().find(|v| v.mount_point == root) {
        return volume_group(volume, true, probe);
    }
    let online = probe.is_dir(&root);
    let space = online.then(|| probe.space(&root)).flatten();
    VolumeFolders {
        key: format!("path:{}", root.display()),
        id: None,
        name: fallback_name(&root),
        mount_point: root,
        online,
        free_bytes: space.map(|s| s.0),
        total_bytes: space.map(|s| s.1),
        folders: Vec::new(),
        roots: Vec::new(),
    }
}

fn fallback_name(root: &Path) -> String {
    root.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| root.display().to_string())
}

/// The drive a path names by the usual mount places: `/Volumes/<drive>`
/// (macOS), `/media/<user>/<drive>`, `/run/media/<user>/<drive>`,
/// `/mnt/<drive>` (Linux), the drive letter or share (Windows); else the
/// file system root.
pub fn drive_root(path: &Path) -> PathBuf {
    let mut parts = path.components();
    let mut root = PathBuf::new();
    match parts.next() {
        Some(prefix @ Component::Prefix(_)) => {
            root.push(prefix.as_os_str());
            if let Some(Component::RootDir) = parts.next() {
                root.push(Component::RootDir.as_os_str());
            }
            return root;
        }
        Some(Component::RootDir) => root.push("/"),
        _ => return PathBuf::from("/"),
    }
    let names: Vec<String> = parts
        .take(4)
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    let depth = match names
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["Volumes", _, ..] | ["mnt", _, ..] => 2,
        ["media", _, _, ..] => 3,
        ["run", "media", _, _, ..] => 4,
        _ => 0,
    };
    for name in names.iter().take(depth) {
        root.push(name);
    }
    root
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    struct Fake {
        mounted: Vec<Volume>,
        dirs: HashSet<PathBuf>,
    }

    impl Probe for Fake {
        fn mounted(&self) -> Vec<Volume> {
            self.mounted.clone()
        }
        fn volume_of(&self, path: &Path) -> Option<Volume> {
            self.mounted
                .iter()
                .filter(|v| path.starts_with(&v.mount_point))
                .max_by_key(|v| v.mount_point.as_os_str().len())
                .cloned()
        }
        fn is_dir(&self, path: &Path) -> bool {
            self.dirs.contains(path)
        }
        fn space(&self, _: &Path) -> Option<(u64, u64)> {
            Some((10, 100))
        }
    }

    fn volume(id: &str, name: &str, mount: &str) -> Volume {
        Volume {
            id: id.into(),
            name: name.into(),
            mount_point: mount.into(),
        }
    }

    fn record(path: &str, volume: Option<&Volume>, photos: usize, missing: usize) -> FolderRecord {
        FolderRecord {
            path: path.into(),
            photos,
            missing,
            volume_id: volume.map(|v| v.id.clone()),
            volume_name: volume.map(|v| v.name.clone()),
            volume_path: volume.map(|v| v.mount_point.clone()),
        }
    }

    /// The user's folders on macOS live on the data volume; the drive is
    /// named as the startup disk, as in Finder.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_data_volume_is_named_as_the_startup_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let root = volume::volume_of(Path::new("/")).unwrap().name;
        assert!(!root.is_empty());
        assert_eq!(shown_volume_name(tmp.path()), Some(root));
    }

    #[test]
    fn folders_are_grouped_under_their_drive_connected_or_not() {
        let system = volume("SYS", "Macintosh HD", "/");
        let card = volume("CARD", "Card", "/Volumes/Card");
        let backup = volume("BAK", "Backup", "/Volumes/Backup");
        let probe = Fake {
            // Card is connected; Backup is not.
            mounted: vec![system.clone(), card.clone()],
            dirs: [
                "/Users/me/Shoots",
                "/Volumes/Card/2026",
                "/Users/me/Lib.catalog",
            ]
            .into_iter()
            .map(PathBuf::from)
            .collect(),
        };
        let records = [
            record("/Volumes/Card/2026", Some(&card), 3, 0),
            record("/Volumes/Card/2025", Some(&card), 2, 2),
            record("/Volumes/Backup/Old", Some(&backup), 5, 5),
            record("/Users/me/Shoots", Some(&system), 1, 0),
        ];
        let roots = [PathBuf::from("/Users/me/Lib.catalog")];
        let groups = by_volume(&records, &roots, &probe);
        let names: Vec<_> = groups.iter().map(|g| (g.name.as_str(), g.online)).collect();
        assert_eq!(
            names,
            [("Backup", false), ("Card", true), ("Macintosh HD", true)]
        );
        let card_group = &groups[1];
        assert_eq!(card_group.free_bytes, Some(10));
        assert_eq!(
            card_group
                .folders
                .iter()
                .map(|f| (f.path.to_str().unwrap(), f.exists, f.offline))
                .collect::<Vec<_>>(),
            [
                ("/Volumes/Card/2025", false, 2),
                ("/Volumes/Card/2026", true, 0)
            ]
        );
        assert_eq!(
            groups[0].free_bytes, None,
            "no space for a disconnected drive"
        );
        assert_eq!(groups[0].mount_point, Path::new("/Volumes/Backup"));
        assert_eq!(groups[2].roots, [PathBuf::from("/Users/me/Lib.catalog")]);
    }

    #[test]
    fn a_drive_mounted_elsewhere_is_one_group_with_its_current_name() {
        let card = volume("CARD", "Card", "/Volumes/Card");
        let now = volume("CARD", "Card 1", "/Volumes/Card 1");
        let probe = Fake {
            mounted: vec![now],
            dirs: HashSet::from([PathBuf::from("/Volumes/Card 1/2026")]),
        };
        let records = [
            // Followed to the new mount point; one photograph left behind.
            record("/Volumes/Card 1/2026", Some(&card), 2, 0),
            record("/Volumes/Card/2026", Some(&card), 1, 1),
        ];
        let groups = by_volume(&records, &[], &probe);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].name, "Card 1");
        assert!(groups[0].online);
        assert_eq!(groups[0].mount_point, Path::new("/Volumes/Card 1"));
        assert_eq!(groups[0].folders.len(), 2);
    }

    #[test]
    fn folders_without_an_identity_go_by_the_drive_their_path_names() {
        let probe = Fake {
            mounted: vec![],
            dirs: HashSet::new(),
        };
        let records = [
            record("/Volumes/Share/Jobs/A", None, 1, 1),
            record("/Volumes/Share/Jobs/B", None, 1, 1),
        ];
        let roots = [PathBuf::from("/Volumes/Share/Jobs")];
        let groups = by_volume(&records, &roots, &probe);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].key, "path:/Volumes/Share");
        assert_eq!(groups[0].name, "Share");
        assert!(!groups[0].online);
        assert_eq!(groups[0].roots.len(), 1);
    }

    #[test]
    fn a_root_on_a_disconnected_drive_goes_with_its_catalog_folders() {
        let card = volume("CARD", "Card", "/mnt/card");
        let probe = Fake {
            mounted: vec![],
            dirs: HashSet::new(),
        };
        let records = [record("/mnt/card/2026/Day 1", Some(&card), 4, 4)];
        let groups = by_volume(&records, &[PathBuf::from("/mnt/card/2026")], &probe);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].key, "id:CARD");
        assert_eq!(groups[0].roots, [PathBuf::from("/mnt/card/2026")]);
    }

    #[test]
    fn drive_roots_follow_the_usual_mount_places() {
        for (path, root) in [
            ("/Volumes/Card/DCIM/100", "/Volumes/Card"),
            ("/media/me/Card/DCIM", "/media/me/Card"),
            ("/run/media/me/Card/DCIM", "/run/media/me/Card"),
            ("/mnt/photos/2026", "/mnt/photos"),
            ("/home/me/Pictures", "/"),
            ("/Volumes", "/"),
        ] {
            assert_eq!(drive_root(Path::new(path)), Path::new(root), "{path}");
        }
    }
}
