//! Shift Capture Time: correct a camera clock for the selected photographs
//! of a session or catalog (`rawtonic_catalog::capture_time`). Three steps,
//! each a command: `capture_time_preview` reads the selection's times
//! (writes nothing), `capture_time_plan` works out one shift's result
//! (writes nothing), `capture_time_apply` writes the planned corrections to
//! the library that owned them when previewed, all or none, then the EXIF
//! map of each photograph's editor state (its `.rrdata`) through the
//! sidecar path every EXIF edit takes. Camera files and file times are
//! never written.
use crate::file_management::parse_virtual_path;
use rawtonic_catalog::Library;
use rawtonic_catalog::capture_time::{
    CaptureChange, CaptureTime, Shift, ShiftField, ShiftParts, TimeError,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// One photograph (base asset) of a selection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewItem {
    pub asset_id: String,
    /// Its path as selected (the first of its virtual copies selected).
    pub path: String,
    /// Make and model as one name; empty when the file says none.
    pub camera: String,
    /// What its file says (`None`: offline, so not read).
    pub source: Option<CaptureTime>,
    /// Its correction, if it has one.
    pub corrected: Option<CaptureTime>,
}

impl PreviewItem {
    /// The time it has now: its correction, else its file's.
    fn current(&self) -> Option<&CaptureTime> {
        self.corrected.as_ref().or(self.source.as_ref())
    }
}

/// Why a selected photograph is left out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Excluded {
    /// Not in a session or catalog (v1 corrects managed photographs only).
    NotInLibrary,
    /// In another library than the selection's first photograph.
    OtherLibrary,
    /// Its file is not there to read its time from.
    Offline,
    /// Its file has no capture time (or an unusable one).
    NoCaptureTime,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExcludedPhoto {
    pub path: String,
    pub reason: Excluded,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapturePreview {
    /// The library that owns the photographs (`None`: none does).
    pub library: Option<String>,
    pub items: Vec<PreviewItem>,
    pub excluded: Vec<ExcludedPhoto>,
}

/// The capture time a file says: its date fields as the camera wrote them,
/// `None` when it has none (a file date or import time is no capture time).
fn source_time(path: &Path) -> Option<(CaptureTime, String)> {
    let capture = rawtonic_ingest::capture::read_capture(path);
    use rawtonic_naming::DateSource;
    if matches!(capture.date_source, DateSource::Modified | DateSource::Now) {
        return None;
    }
    let local = capture.captured?.format("%Y-%m-%d %H:%M:%S").to_string();
    let time = CaptureTime::new(&local, capture.subsec.as_deref(), capture.offset.as_deref()).ok()?;
    Some((time, capture.camera().unwrap_or_default()))
}

/// The selection's photographs: virtual copies of one photograph once, each
/// with what its file says and its correction, in `library` (the first
/// managed photograph's).
fn preview(paths: &[String]) -> CapturePreview {
    let mut result = CapturePreview::default();
    let mut libraries: HashMap<PathBuf, Option<Library>> = HashMap::new();
    let mut seen = std::collections::HashSet::new();
    for path in paths {
        let (source, _) = parse_virtual_path(path);
        let excluded = |reason| ExcludedPhoto {
            path: path.clone(),
            reason,
        };
        let Some(directory) = crate::session_bridge::library_dir_for(&source) else {
            result.excluded.push(excluded(Excluded::NotInLibrary));
            continue;
        };
        let owner = directory.to_string_lossy().into_owned();
        match &result.library {
            Some(library) if *library != owner => {
                result.excluded.push(excluded(Excluded::OtherLibrary));
                continue;
            }
            Some(_) => {}
            None => result.library = Some(owner),
        }
        let lib = libraries
            .entry(directory.clone())
            .or_insert_with(|| Library::open(&directory).ok());
        let Some(asset) = lib.as_ref().and_then(|lib| lib.asset_at(&source).ok().flatten()) else {
            result.excluded.push(excluded(Excluded::NotInLibrary));
            continue;
        };
        if !seen.insert(asset.id.clone()) {
            continue;
        }
        let corrected = lib
            .as_ref()
            .and_then(|lib| lib.capture_override(&asset.id).ok().flatten());
        let online = source.is_file();
        let read = online.then(|| source_time(&source)).flatten();
        if read.is_none() && corrected.is_none() {
            let reason = if online {
                Excluded::NoCaptureTime
            } else {
                Excluded::Offline
            };
            result.excluded.push(excluded(reason));
            continue;
        }
        let (source_time, camera) = match read {
            Some((time, camera)) => (Some(time), camera),
            None => (None, String::new()),
        };
        result.items.push(PreviewItem {
            asset_id: asset.id,
            path: path.clone(),
            camera,
            source: source_time,
            corrected,
        });
    }
    result
}

#[tauri::command]
pub async fn capture_time_preview(paths: Vec<String>) -> Result<CapturePreview, String> {
    tauri::async_runtime::spawn_blocking(move || preview(&paths))
        .await
        .map_err(|e| format!("Task failed: {e}"))
}

/// What a plan does.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum PlanRequest {
    /// Every photograph by one signed shift.
    Shift { parts: ShiftParts },
    /// The shift that sets photograph `asset_id` to `corrected` (local wall
    /// time, optional fraction), for every photograph.
    Reference { asset_id: String, corrected: String },
    /// Every corrected photograph back to its file's time.
    Reset,
}

/// One photograph's planned change, which `capture_time_apply` writes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlannedChange {
    pub asset_id: String,
    pub path: String,
    /// Its correction now (what the library must still have).
    pub expected: Option<CaptureTime>,
    /// Its correction after (`None`: its file's time).
    pub new: Option<CaptureTime>,
    /// Its time before and after (`new`, else its file's): what its EXIF
    /// map has and takes.
    pub before: CaptureTime,
    pub after: CaptureTime,
}

impl PlannedChange {
    /// The change that takes it back.
    fn reversed(&self) -> Self {
        Self {
            asset_id: self.asset_id.clone(),
            path: self.path.clone(),
            expected: self.new.clone(),
            new: self.expected.clone(),
            before: self.after.clone(),
            after: self.before.clone(),
        }
    }
}

/// Why a plan cannot be applied.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum PlanError {
    /// A part of the shift cannot be read.
    Field { field: ShiftField },
    /// The corrected reference time cannot be read.
    Reference { problem: TimeError },
    /// The reference photograph is not among the selection's.
    NoReference,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Plan {
    /// The shift every photograph moves by (none for a reset).
    pub shift: Option<ShiftParts>,
    pub error: Option<PlanError>,
    pub changes: Vec<PlannedChange>,
    /// Those the shift would take out of range: Apply is refused while any is.
    pub out_of_range: Vec<String>,
    /// Earliest and latest time of the planned photographs, before and after.
    pub before_range: Option<(CaptureTime, CaptureTime)>,
    pub after_range: Option<(CaptureTime, CaptureTime)>,
}

fn range<'a>(times: impl Iterator<Item = &'a CaptureTime>) -> Option<(CaptureTime, CaptureTime)> {
    // Same-width text: the order of the text is the order of the times.
    let key = |t: &CaptureTime| format!("{}.{:0<9}", t.local, t.subsec.as_deref().unwrap_or(""));
    let mut sorted: Vec<&CaptureTime> = times.collect();
    sorted.sort_by_cached_key(|t| key(t));
    Some(((*sorted.first()?).clone(), (*sorted.last()?).clone()))
}

fn plan(items: &[PreviewItem], request: &PlanRequest) -> Plan {
    let mut plan = Plan::default();
    let shift = match request {
        PlanRequest::Reset => None,
        PlanRequest::Shift { parts } => match Shift::from_parts(parts) {
            Ok(shift) => Some(shift),
            Err(field) => {
                plan.error = Some(PlanError::Field { field });
                return plan;
            }
        },
        PlanRequest::Reference {
            asset_id,
            corrected,
        } => {
            let Some(reference) = items
                .iter()
                .find(|item| item.asset_id == *asset_id)
                .and_then(PreviewItem::current)
            else {
                plan.error = Some(PlanError::NoReference);
                return plan;
            };
            match reference.shift_to(corrected) {
                Ok(shift) => Some(shift),
                Err(problem) => {
                    plan.error = Some(PlanError::Reference { problem });
                    return plan;
                }
            }
        }
    };
    plan.shift = shift.map(|s| s.parts());
    for item in items {
        let Some(current) = item.current() else {
            continue;
        };
        let after = match &shift {
            None if item.corrected.is_none() => continue,
            None => match &item.source {
                Some(source) => source.clone(),
                // Offline: its file's time cannot be read to go back to.
                None => continue,
            },
            Some(shift) if shift.is_zero() => continue,
            Some(shift) => match current.shifted(shift) {
                Ok(after) => after,
                Err(_) => {
                    plan.out_of_range.push(item.path.clone());
                    continue;
                }
            },
        };
        plan.changes.push(PlannedChange {
            asset_id: item.asset_id.clone(),
            path: item.path.clone(),
            expected: item.corrected.clone(),
            // Back on its file's time: no correction left to keep.
            new: (item.source.as_ref() != Some(&after)).then(|| after.clone()),
            before: current.clone(),
            after,
        });
    }
    if !plan.out_of_range.is_empty() {
        plan.changes.clear();
        return plan;
    }
    plan.before_range = range(plan.changes.iter().map(|c| &c.before));
    plan.after_range = range(plan.changes.iter().map(|c| &c.after));
    plan
}

#[tauri::command]
pub fn capture_time_plan(items: Vec<PreviewItem>, request: PlanRequest) -> Plan {
    plan(&items, &request)
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Applied {
    /// Photographs whose correction changed since the plan: nothing was
    /// written; plan again.
    pub conflicts: Vec<String>,
    /// Photographs corrected whose `.rrdata` could not be written (the
    /// library has the correction; Try again writes it).
    pub failed: Vec<String>,
    /// What takes the written changes back (the undo step).
    pub undo: Vec<PlannedChange>,
}

/// The EXIF fields `time` sets in a photograph's editor state (an empty
/// value removes one, `update_exif_of`).
fn exif_updates(time: &CaptureTime) -> HashMap<String, String> {
    time.exif_fields()
        .into_iter()
        .map(|(key, value)| (key.to_string(), value.unwrap_or_default()))
        .collect()
}

fn apply(library: &Path, changes: &[PlannedChange]) -> Result<Applied, String> {
    let lib = Library::open(library)
        .map_err(|e| format!("The library {} cannot be read: {e:#}", library.display()))?;
    let written: Vec<CaptureChange> = changes
        .iter()
        .map(|change| CaptureChange {
            asset_id: change.asset_id.clone(),
            expected: change.expected.clone(),
            new: change.new.clone(),
        })
        .collect();
    let conflicts = lib.set_capture_times(&written).map_err(|e| format!("{e:#}"))?;
    if !conflicts.is_empty() {
        return Ok(Applied {
            conflicts: changes
                .iter()
                .filter(|c| conflicts.contains(&c.asset_id))
                .map(|c| c.path.clone())
                .collect(),
            ..Applied::default()
        });
    }
    drop(lib);
    let owner = library.to_string_lossy().into_owned();
    let mut failed = Vec::new();
    crate::session_bridge::with_culling_owner(Some(&owner), || {
        for change in changes {
            let (source, _) = parse_virtual_path(&change.path);
            let source = source.to_string_lossy().into_owned();
            // Offline without an EXIF map: there is nothing beside the
            // file to write, and a map of dates alone would hide the
            // camera's other fields. The library's correction holds.
            if crate::exif_processing::read_rrexif_sidecar(Path::new(&source)).is_none()
                && !Path::new(&source).is_file()
            {
                continue;
            }
            if let Err(e) =
                crate::file_management::update_exif_of(&source, &exif_updates(&change.after))
            {
                log::warn!("Capture time corrected in the library; sidecar not written: {e}");
                failed.push(change.path.clone());
            }
        }
    });
    Ok(Applied {
        conflicts: Vec::new(),
        failed,
        undo: changes.iter().map(PlannedChange::reversed).collect(),
    })
}

/// Writes `changes` (a plan's, or an undo step's) to `library`, the library
/// that owned the photographs when they were previewed, whichever is open
/// now. Returns the changes that take them back (the undo step).
#[tauri::command]
pub async fn capture_time_apply(
    library: String,
    changes: Vec<PlannedChange>,
) -> Result<Applied, String> {
    tauri::async_runtime::spawn_blocking(move || apply(Path::new(&library), &changes))
        .await
        .map_err(|e| format!("Task failed: {e}"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(local: &str, subsec: Option<&str>) -> CaptureTime {
        CaptureTime::new(local, subsec, Some("+01:00")).unwrap()
    }

    fn item(id: &str, source: Option<CaptureTime>, corrected: Option<CaptureTime>) -> PreviewItem {
        PreviewItem {
            asset_id: id.into(),
            path: format!("/job/Capture/{id}.ARW"),
            camera: "Camera".into(),
            source,
            corrected,
        }
    }

    fn hours(negative: bool, h: &str) -> PlanRequest {
        PlanRequest::Shift {
            parts: ShiftParts {
                negative,
                hours: h.into(),
                ..Default::default()
            },
        }
    }

    #[test]
    fn a_shift_moves_every_photograph_from_its_current_time() {
        let items = [
            item("a", Some(at("2026-10-30 12:00:00", Some("1"))), None),
            item(
                "b",
                Some(at("2026-10-30 12:00:00", None)),
                Some(at("2026-10-30 13:00:00", None)),
            ),
        ];
        let plan = plan(&items, &hours(false, "1"));
        assert_eq!(plan.error, None);
        assert_eq!(plan.changes.len(), 2);
        assert_eq!(plan.changes[0].after, at("2026-10-30 13:00:00", Some("1")));
        assert_eq!(plan.changes[0].expected, None);
        // +1 h again from its corrected time, not from the file's.
        assert_eq!(plan.changes[1].after, at("2026-10-30 14:00:00", None));
        assert_eq!(plan.changes[1].expected, Some(at("2026-10-30 13:00:00", None)));
        assert_eq!(
            plan.before_range,
            Some((at("2026-10-30 12:00:00", Some("1")), at("2026-10-30 13:00:00", None)))
        );
    }

    #[test]
    fn a_shift_back_to_the_file_s_time_leaves_no_correction() {
        let items = [item(
            "a",
            Some(at("2026-10-30 12:00:00", None)),
            Some(at("2026-10-30 13:00:00", None)),
        )];
        let plan = plan(&items, &hours(true, "1"));
        assert_eq!(plan.changes[0].new, None);
        assert_eq!(plan.changes[0].after, at("2026-10-30 12:00:00", None));
    }

    #[test]
    fn a_reference_time_shifts_all_by_its_difference() {
        let items = [
            item("a", Some(at("2026-10-30 12:00:00", Some("5"))), None),
            item("b", Some(at("2026-10-30 12:30:00", Some("7"))), None),
        ];
        let request = PlanRequest::Reference {
            asset_id: "b".into(),
            corrected: "2026-10-30 10:30:00".into(),
        };
        let plan = plan(&items, &request);
        assert_eq!(plan.shift.as_ref().unwrap().hours, "2");
        assert!(plan.shift.as_ref().unwrap().negative);
        assert_eq!(plan.changes[0].after, at("2026-10-30 10:00:00", Some("5")));
        assert_eq!(plan.changes[1].after, at("2026-10-30 10:30:00", Some("7")));
        let bad = PlanRequest::Reference {
            asset_id: "b".into(),
            corrected: "30.10.2026".into(),
        };
        assert_eq!(
            super::plan(&items, &bad).error,
            Some(PlanError::Reference {
                problem: TimeError::Invalid
            })
        );
    }

    #[test]
    fn a_zero_shift_plans_nothing_and_out_of_range_blocks_all() {
        let items = [
            item("a", Some(at("2026-10-30 12:00:00", None)), None),
            item("b", Some(at("0001-01-01 00:30:00", None)), None),
        ];
        assert!(plan(&items, &hours(false, "0")).changes.is_empty());
        let blocked = plan(&items, &hours(true, "1"));
        assert!(blocked.changes.is_empty());
        assert_eq!(blocked.out_of_range, ["/job/Capture/b.ARW"]);
        let bad = PlanRequest::Shift {
            parts: ShiftParts {
                minutes: "1.5".into(),
                ..Default::default()
            },
        };
        assert_eq!(
            plan(&items, &bad).error,
            Some(PlanError::Field {
                field: ShiftField::Minutes
            })
        );
    }

    #[test]
    fn reset_takes_corrected_photographs_back_unless_offline() {
        let items = [
            item("a", Some(at("2026-10-30 12:00:00", None)), None),
            item(
                "b",
                Some(at("2026-10-30 12:00:00", Some("3"))),
                Some(at("2026-10-30 15:00:00", Some("3"))),
            ),
            item("c", None, Some(at("2026-10-30 15:00:00", None))),
        ];
        let plan = plan(&items, &PlanRequest::Reset);
        assert_eq!(plan.changes.len(), 1);
        assert_eq!(plan.changes[0].asset_id, "b");
        assert_eq!(plan.changes[0].new, None);
        assert_eq!(plan.changes[0].after, at("2026-10-30 12:00:00", Some("3")));
    }

    /// A camera JPEG in a session's Capture folder, with its capture time,
    /// fraction and offset.
    fn camera_jpeg(lib: &Library, name: &str, local: &str) -> PathBuf {
        use little_exif::{exif_tag::ExifTag, metadata::Metadata};
        let path = lib.directory.join("Capture").join(name);
        image::DynamicImage::ImageRgb8(image::RgbImage::new(8, 6))
            .save(&path)
            .unwrap();
        let mut exif = Metadata::new();
        exif.set_tag(ExifTag::Make("Maker".into()));
        exif.set_tag(ExifTag::Model("Body".into()));
        exif.set_tag(ExifTag::DateTimeOriginal(local.into()));
        exif.set_tag(ExifTag::SubSecTimeOriginal("05".into()));
        exif.set_tag(ExifTag::OffsetTimeOriginal("+02:00".into()));
        exif.write_to_file(&path).unwrap();
        path
    }

    fn sha256(path: &Path) -> String {
        use sha2::Digest;
        hex::encode(sha2::Sha256::digest(std::fs::read(path).unwrap()))
    }

    #[test]
    fn applying_corrects_library_sidecar_and_export_but_never_the_file() {
        use rawtonic_catalog::{CaptureFacts, Mode};
        let temp = tempfile::tempdir().unwrap();
        let lib = Library::create(&temp.path().join("Job"), "Job", Mode::Session).unwrap();
        let a = camera_jpeg(&lib, "A.jpg", "2026:10:30 23:30:00");
        let b = camera_jpeg(&lib, "B.jpg", "2026:10:30 23:45:00");
        let ids: Vec<String> = [&a, &b].iter().map(|p| lib.add_asset(p).unwrap().id).collect();
        let hashes = [sha256(&a), sha256(&b)];
        let a_path = a.to_string_lossy().into_owned();
        let selection = vec![
            a_path.clone(),
            format!("{a_path}?vc=copy1"),
            b.to_string_lossy().into_owned(),
            temp.path().join("elsewhere.jpg").to_string_lossy().into_owned(),
        ];

        let found = preview(&selection);
        assert_eq!(found.items.len(), 2, "a virtual copy counts once");
        assert_eq!(found.excluded.len(), 1);
        assert_eq!(found.excluded[0].reason, Excluded::NotInLibrary);
        assert_eq!(found.items[0].camera, "Maker Body");
        assert_eq!(
            found.items[0].source,
            Some(CaptureTime::new("2026-10-30 23:30:00", Some("05"), Some("+02:00")).unwrap())
        );
        let library = PathBuf::from(found.library.clone().unwrap());

        // Only B: A stays as it was.
        let request = hours(false, "1");
        let planned = plan(&found.items[1..], &request);
        let applied = apply(&library, &planned.changes).unwrap();
        assert!(applied.conflicts.is_empty() && applied.failed.is_empty());
        let lib = Library::open(&library).unwrap();
        let corrected = lib.capture_override(&ids[1]).unwrap().unwrap();
        assert_eq!(corrected.local, "2026-10-31 00:45:00");
        assert_eq!(lib.capture_override(&ids[0]).unwrap(), None);

        // The facts pass rereads the file: the correction holds.
        lib.record_facts(
            &ids[1],
            &CaptureFacts {
                captured: Some("2026-10-30 23:45:00".into()),
                ..CaptureFacts::default()
            },
        )
        .unwrap();
        assert_eq!(lib.capture_override(&ids[1]).unwrap(), Some(corrected.clone()));

        // The .rrdata beside B and what the app reads carry it.
        let shown = crate::exif_processing::read_rrexif_sidecar(&b).unwrap();
        assert_eq!(shown["DateTimeOriginal"], "2026-10-31 00:45:00");
        assert_eq!(shown["SubSecTimeOriginal"], "05");
        assert_eq!(shown["OffsetTimeOriginal"], "+02:00");
        assert_eq!(shown["Make"], "Maker", "the camera's other fields stay");
        let written = std::fs::read_to_string(b.with_file_name("B.jpg.rrdata")).unwrap();
        assert!(written.contains("2026-10-31 00:45:00"), "{written}");

        // An export pins the corrected time for its EXIF and its names.
        let pinned = crate::export_processing::pin_photos(
            &[b.to_string_lossy().into_owned()],
            None,
            None,
            None,
        )
        .unwrap();
        let exif = pinned[0].exif(&b).unwrap();
        assert_eq!(exif["DateTimeOriginal"], "2026-10-31 00:45:00");
        assert!(exif.contains_key(crate::exif_processing::CAPTURE_CORRECTED));
        use chrono::{Datelike, Timelike};
        let date = pinned[0].capture_date(&b).with_timezone(&chrono::Local);
        assert_eq!((date.day(), date.hour(), date.minute()), (31, 0, 45));
        let untouched =
            crate::export_processing::pin_photos(&[a_path.clone()], None, None, None).unwrap();
        assert!(untouched[0].capture.is_none());

        // A plan made before the correction conflicts and writes nothing.
        let stale = apply(&library, &planned.changes).unwrap();
        assert_eq!(stale.conflicts, [b.to_string_lossy().into_owned()]);
        assert_eq!(lib.capture_override(&ids[1]).unwrap(), Some(corrected));

        // Undo takes it back to the file's time, sidecar included.
        let undone = apply(&library, &applied.undo).unwrap();
        assert!(undone.conflicts.is_empty());
        assert_eq!(lib.capture_override(&ids[1]).unwrap(), None);
        let shown = crate::exif_processing::read_rrexif_sidecar(&b).unwrap();
        assert_eq!(shown["DateTimeOriginal"], "2026-10-30 23:45:00");
        assert_eq!(shown["SubSecTimeOriginal"], "05");

        // The camera files were never written.
        assert_eq!([sha256(&a), sha256(&b)], hashes);
    }

    #[test]
    fn exif_updates_remove_absent_fields() {
        let updates = exif_updates(&CaptureTime::new("2026-10-30 12:00:00", None, None).unwrap());
        assert_eq!(updates["DateTimeOriginal"], "2026-10-30 12:00:00");
        assert_eq!(updates["SubSecTimeOriginal"], "");
        assert_eq!(updates["OffsetTimeOriginal"], "");
    }
}
