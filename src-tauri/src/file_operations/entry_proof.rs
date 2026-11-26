// SPDX-License-Identifier: AGPL-3.0-or-later
//! Which facts prove that a directory entry the Recycle Bin moved (or a
//! restore moved back) is still the entry recorded. NTFS and ReFS keep a
//! file ID across a move within the volume, and that ID is the proof. FAT,
//! FAT32 and exFAT have no file IDs: Windows numbers an entry by where its
//! directory record lies, so the number changes with every move (WQA7b:
//! Trash on an exFAT volume failed with "recycled entry identity changed";
//! notes benchmark-results/wqa7b-2026-11-26/REPORT.md). There the proof is
//! what such a move keeps of the record: the volume, the size, and the
//! creation and last-write times as the volume stores them (raw values read
//! from the same volume, so its 2 s or 10 ms resolution needs no tolerance).
//! FAT32 stores local time, which Windows reports as UTC with the current
//! time-zone offset: after a daylight-saving or time-zone change both times
//! of an untouched entry shift by the same whole quarter hours, and that
//! shift is accepted. Pure, so its tests run on every platform.
use serde::{Deserialize, Serialize};

/// Volume serial number and file index (high, low).
pub(super) type Identity = (u32, u32, u32);

/// What a move within a FAT-family volume keeps of an entry's record.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Stamp {
    pub size: u64,
    /// FILETIME values as the volume stores them.
    pub created: u64,
    pub written: u64,
    pub directory: bool,
}

/// One read of an entry: its identity and its stamp.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct EntryFacts {
    pub identity: Identity,
    pub stamp: Stamp,
}

/// Whether `file_system` (as `GetVolumeInformationByHandleW` names it)
/// keeps an entry's file ID when the entry moves within the volume.
pub(super) fn keeps_file_ids(file_system: &str) -> bool {
    ["NTFS", "ReFS"]
        .iter()
        .any(|kind| file_system.eq_ignore_ascii_case(kind))
}

/// The stamp a receipt records for an entry on `file_system`: none where
/// the file ID is the proof.
pub(super) fn stamp_to_record(file_system: &str, facts: &EntryFacts) -> Option<Stamp> {
    (!keeps_file_ids(file_system)).then_some(facts.stamp)
}

/// Whether `now` is the entry recorded as `identity` and, on a volume
/// without file IDs, `stamp`.
pub(super) fn same_entry(identity: Identity, stamp: Option<&Stamp>, now: &EntryFacts) -> bool {
    match stamp {
        None => now.identity == identity,
        Some(stamp) => now.identity.0 == identity.0 && same_record(stamp, &now.stamp),
    }
}

/// FILETIME units (100 ns) in a quarter hour, the finest time-zone step.
const QUARTER_HOUR: i128 = 15 * 60 * 10_000_000;
/// The largest difference between two UTC offsets: −12 h to +14 h.
const MAX_ZONE_SHIFT: i128 = 26 * 4 * QUARTER_HOUR;

fn same_record(recorded: &Stamp, now: &Stamp) -> bool {
    let created = i128::from(now.created) - i128::from(recorded.created);
    let written = i128::from(now.written) - i128::from(recorded.written);
    recorded.size == now.size
        && recorded.directory == now.directory
        && created == written
        && created % QUARTER_HOUR == 0
        && created.abs() <= MAX_ZONE_SHIFT
}

#[cfg(test)]
mod tests {
    use super::*;

    const SERIAL: u32 = 0x5A1E_00F3;

    fn facts(index: (u32, u32), stamp: Stamp) -> EntryFacts {
        EntryFacts {
            identity: (SERIAL, index.0, index.1),
            stamp,
        }
    }

    /// An exFAT file before and after Windows moves it into the volume's
    /// Recycle Bin (`S:\$Recycle.Bin\$R…`): the index, taken from where the
    /// directory record lies, changes; the record's size and times (exFAT
    /// keeps 10 ms of each) travel with it. The shape WQA7b recorded.
    fn exfat_move() -> (EntryFacts, EntryFacts) {
        let stamp = Stamp {
            size: 25_994_901,
            created: 0x01DB_5F3A_1C2B_4E80,
            written: 0x01DB_5F39_8F00_2A00,
            directory: false,
        };
        (
            facts((0, 0x0002_0081), stamp),
            facts((0, 0x0004_00C3), stamp),
        )
    }

    #[test]
    fn an_entry_moved_on_exfat_is_proven_by_its_record_not_its_index() {
        let (before, after) = exfat_move();
        for kind in ["exFAT", "FAT32", "FAT", "fat32"] {
            assert!(!keeps_file_ids(kind), "{kind}");
            let stamp = stamp_to_record(kind, &before);
            assert_eq!(stamp, Some(before.stamp));
            assert!(
                same_entry(before.identity, stamp.as_ref(), &after),
                "{kind}"
            );
        }
        // The index proof alone fails exactly as on the Lenovo.
        assert!(!same_entry(before.identity, None, &after));
    }

    #[test]
    fn on_exfat_another_size_time_or_volume_is_another_entry() {
        let (before, after) = exfat_move();
        let stamp = stamp_to_record("exFAT", &before);
        let changed = [
            Stamp {
                size: after.stamp.size + 1,
                ..after.stamp
            },
            Stamp {
                written: after.stamp.written + 20_000_000, // 2 s later
                ..after.stamp
            },
            Stamp {
                created: after.stamp.created + 100_000, // 10 ms later
                ..after.stamp
            },
            Stamp {
                directory: true,
                ..after.stamp
            },
        ];
        for stamp_now in changed {
            let now = EntryFacts {
                stamp: stamp_now,
                ..after
            };
            assert!(!same_entry(before.identity, stamp.as_ref(), &now));
        }
        let other_volume = EntryFacts {
            identity: (SERIAL ^ 1, after.identity.1, after.identity.2),
            ..after
        };
        assert!(!same_entry(before.identity, stamp.as_ref(), &other_volume));
    }

    /// FAT32 keeps local time: after a daylight-saving change Windows
    /// reports both times of an untouched entry one hour off.
    #[test]
    fn a_time_zone_shift_of_both_times_is_the_same_entry() {
        let (before, after) = exfat_move();
        let stamp = stamp_to_record("FAT32", &before);
        let hour = 4 * QUARTER_HOUR as u64;
        for shift in [hour, 13 * hour / 2] {
            let shifted = EntryFacts {
                stamp: Stamp {
                    created: after.stamp.created + shift,
                    written: after.stamp.written + shift,
                    ..after.stamp
                },
                ..after
            };
            assert!(same_entry(before.identity, stamp.as_ref(), &shifted));
            let back = EntryFacts {
                stamp: Stamp {
                    created: after.stamp.created - shift,
                    written: after.stamp.written - shift,
                    ..after.stamp
                },
                ..after
            };
            assert!(same_entry(before.identity, stamp.as_ref(), &back));
        }
        for (created, written) in [
            (hour, 0),
            (hour, 2 * hour),
            (hour + 1, hour + 1),
            (27 * hour, 27 * hour),
        ] {
            let other = EntryFacts {
                stamp: Stamp {
                    created: after.stamp.created + created,
                    written: after.stamp.written + written,
                    ..after.stamp
                },
                ..after
            };
            assert!(
                !same_entry(before.identity, stamp.as_ref(), &other),
                "{created} {written}"
            );
        }
    }

    #[test]
    fn on_ntfs_and_refs_the_file_id_stays_the_proof() {
        let stamp = Stamp {
            size: 20,
            created: 1,
            written: 2,
            directory: false,
        };
        let before = facts((0x0001_0000, 0x0000_1234), stamp);
        for kind in ["NTFS", "ReFS", "ntfs"] {
            assert!(keeps_file_ids(kind), "{kind}");
            let recorded = stamp_to_record(kind, &before);
            assert_eq!(recorded, None, "{kind}: receipts stay as before");
            // Touched while in the Bin: still the same file by its ID.
            let touched = EntryFacts {
                stamp: Stamp {
                    written: 3,
                    ..stamp
                },
                ..before
            };
            assert!(same_entry(before.identity, recorded.as_ref(), &touched));
            // Same record facts, another ID: another file.
            let other = facts((0x0001_0000, 0x0000_1235), stamp);
            assert!(!same_entry(before.identity, recorded.as_ref(), &other));
        }
    }
}
