//! Memory cards connected to the computer, for the import dialog's source
//! list and for opening the import when a card is inserted (task IMPD).
//!
//! A card is a mounted volume with a camera's `DCIM` folder (DCF, every
//! camera's card layout) at its top, as `batch::classify_medium` counts a
//! path through `DCIM` as a card. Nothing on a card is written here; eject
//! only asks the system to eject it.
use serde::Serialize;
use std::path::{Path, PathBuf};

/// One connected memory card.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Card {
    /// Where the card is mounted; the import lists its photographs from
    /// here.
    pub mount_point: PathBuf,
    /// The card's name as the system shows it (its volume label, or the
    /// drive letter on Windows).
    pub name: String,
}

/// The cards among `candidates` (mount points of local volumes): those with
/// a `DCIM` folder at their top, in any letter case. The system's own
/// volume (`/`) is never one.
pub fn cards_among(candidates: impl IntoIterator<Item = PathBuf>) -> Vec<Card> {
    let mut cards: Vec<Card> = candidates
        .into_iter()
        .filter(|mount| mount != Path::new("/") && has_dcim(mount))
        .map(|mount_point| Card {
            name: volume_name(&mount_point),
            mount_point,
        })
        .collect();
    cards.sort_by(|a, b| a.mount_point.cmp(&b.mount_point));
    cards.dedup();
    cards
}

/// Whether `mount` has a camera's `DCIM` folder at its top.
fn has_dcim(mount: &Path) -> bool {
    std::fs::read_dir(mount).is_ok_and(|entries| {
        entries.flatten().any(|entry| {
            entry.file_name().eq_ignore_ascii_case("DCIM")
                && entry.file_type().is_ok_and(|t| t.is_dir())
        })
    })
}

fn volume_name(mount: &Path) -> String {
    mount
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| mount.to_string_lossy().trim_end_matches(['\\', '/']).into())
}

/// The cards connected now: the [`mount_points`], each checked for `DCIM`.
/// This reads the top folder of every such volume, which macOS lets an app
/// do only with the user's permission for removable volumes: call it on the
/// user's action (the import dialog), never at start-up.
pub fn connected_cards() -> Vec<Card> {
    cards_among(mount_points())
}

/// Where a memory card could be mounted now: local volumes on removable
/// media (or where the system does not say), not the system's own; on
/// Windows the drive letters from D:. Only the system's mount list is read,
/// nothing on the volumes.
pub fn mount_points() -> Vec<PathBuf> {
    candidate_mounts()
}

/// The cards among the volumes in `now` that were not in `before` (mount
/// points, [`mount_points`]): the cards just inserted. Only the new volumes
/// are looked into.
pub fn inserted_cards(before: &[PathBuf], now: &[PathBuf]) -> Vec<Card> {
    cards_among(now.iter().filter(|mount| !before.contains(mount)).cloned())
}

#[cfg(not(windows))]
fn candidate_mounts() -> Vec<PathBuf> {
    rawtonic_fs::volume::mounts()
        .into_iter()
        .filter(|mount| mount.is_storage() && !mount.network)
        // An internal or external drive is no card (statfs flags / sysfs).
        .filter(|mount| mount.removable() != Some(false))
        // macOS's own system and data volumes.
        .filter(|mount| !mount.mount_point.starts_with("/System"))
        .map(|mount| mount.mount_point)
        .collect()
}

#[cfg(windows)]
fn candidate_mounts() -> Vec<PathBuf> {
    // C: holds the system; cards get later letters.
    (b'D'..=b'Z')
        .map(|letter| PathBuf::from(format!("{}:\\", letter as char)))
        .filter(|root| root.is_dir())
        .collect()
}

/// Asks the system to eject the card mounted at `mount_point` (macOS
/// `diskutil eject`, Linux `udisksctl unmount` + `power-off`, Windows the
/// shell's Eject). Refused for anything that is not a connected card. The
/// system refuses when a file on it is still open; that error is returned.
pub fn eject(mount_point: &Path) -> Result<(), String> {
    let card = connected_cards()
        .into_iter()
        .find(|card| card.mount_point == mount_point)
        .ok_or_else(|| format!("{} is not a connected memory card", mount_point.display()))?;
    run_eject(&card.mount_point)
}

#[cfg(target_vendor = "apple")]
fn run_eject(mount: &Path) -> Result<(), String> {
    command_status(
        std::process::Command::new("/usr/sbin/diskutil")
            .arg("eject")
            .arg(mount),
    )
}

#[cfg(target_os = "linux")]
fn run_eject(mount: &Path) -> Result<(), String> {
    let device = rawtonic_fs::volume::mount_of(mount)
        .map_err(|e| e.to_string())?
        .source;
    command_status(
        std::process::Command::new("udisksctl")
            .args(["unmount", "--no-user-interaction", "-b"])
            .arg(&device),
    )?;
    // Powering the reader's slot off is a courtesy; the card is unmounted.
    let _ = command_status(
        std::process::Command::new("udisksctl")
            .args(["power-off", "--no-user-interaction", "-b"])
            .arg(&device),
    );
    Ok(())
}

#[cfg(windows)]
fn run_eject(mount: &Path) -> Result<(), String> {
    let drive = mount.to_string_lossy().trim_end_matches('\\').to_string();
    if !(drive.len() == 2 && drive.ends_with(':')) {
        return Err(format!("{drive} is not a drive letter"));
    }
    command_status(std::process::Command::new("powershell").args([
        "-NoProfile",
        "-NonInteractive",
        "-Command",
        &format!(
            "(New-Object -ComObject Shell.Application).Namespace(17).ParseName('{drive}').InvokeVerb('Eject')"
        ),
    ]))
}

#[cfg(not(any(target_vendor = "apple", target_os = "linux", windows)))]
fn run_eject(_mount: &Path) -> Result<(), String> {
    Err("Ejecting is not supported on this system".into())
}

#[cfg(any(target_vendor = "apple", target_os = "linux", windows))]
fn command_status(command: &mut std::process::Command) -> Result<(), String> {
    let output = command.output().map_err(|e| e.to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        let text = String::from_utf8_lossy(&output.stderr);
        let text = if text.trim().is_empty() {
            String::from_utf8_lossy(&output.stdout)
        } else {
            text
        };
        Err(text.trim().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_volume_with_dcim_at_its_top_is_a_card_in_any_letter_case() {
        let d = tempfile::tempdir().unwrap();
        let canon = d.path().join("EOS_DIGITAL");
        let fuji = d.path().join("Untitled");
        let drive = d.path().join("Backup");
        std::fs::create_dir_all(canon.join("DCIM/100CANON")).unwrap();
        std::fs::create_dir_all(fuji.join("dcim")).unwrap();
        std::fs::create_dir_all(drive.join("Photos/DCIM")).unwrap();
        // A file named DCIM is not the folder.
        let odd = d.path().join("Odd");
        std::fs::create_dir_all(&odd).unwrap();
        std::fs::write(odd.join("DCIM"), b"").unwrap();
        let cards = cards_among([
            drive.clone(),
            fuji.clone(),
            canon.clone(),
            odd,
            PathBuf::from("/"),
        ]);
        assert_eq!(
            cards,
            [
                Card {
                    mount_point: canon,
                    name: "EOS_DIGITAL".into()
                },
                Card {
                    mount_point: fuji,
                    name: "Untitled".into()
                },
            ]
        );
    }

    #[test]
    fn only_newly_mounted_volumes_are_looked_into_and_counted_as_inserted() {
        let d = tempfile::tempdir().unwrap();
        let [old, new, drive] = ["OLD", "NEW", "DRIVE"].map(|name| d.path().join(name));
        std::fs::create_dir_all(old.join("DCIM")).unwrap();
        std::fs::create_dir_all(new.join("DCIM")).unwrap();
        std::fs::create_dir_all(&drive).unwrap();
        let before = [old.clone()];
        let now = [old, new.clone(), drive];
        assert_eq!(
            inserted_cards(&before, &now),
            [Card {
                mount_point: new,
                name: "NEW".into()
            }]
        );
        assert!(inserted_cards(&now, &before).is_empty());
    }

    #[test]
    fn eject_refuses_anything_but_a_connected_card() {
        let d = tempfile::tempdir().unwrap();
        assert!(eject(d.path()).is_err());
    }

    #[test]
    fn the_card_list_serializes_as_the_dialog_reads_it() {
        let json = serde_json::to_value(Card {
            mount_point: PathBuf::from("/Volumes/CARD"),
            name: "CARD".into(),
        })
        .unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "mountPoint": "/Volumes/CARD", "name": "CARD" })
        );
    }
}
