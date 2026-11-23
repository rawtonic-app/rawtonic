//! The files an export would meet, found before anything is rendered, for
//! the Export form's "Ask" choice of "If a file exists": one answer per run
//! (Keep both, Skip or Replace if changed), asked once with the counts per
//! kind. Nothing is written or created here, and an original, or a file a
//! library lists as a photo, is only reported: Replace if changed never
//! replaces it (`rawtonic_export::existing_files`).
use crate::export_processing::{
    ExportTarget, PhotoItem, Planned, known_libraries, open_export_records, photo_inputs,
    pin_photos, plan_output_path, previous_export, target_inputs_hash,
};
use crate::export_queue::ExportRequest;
use rawtonic_export::existing_files::{ExistingFiles, ExportRecords, Outcome, Previous};
use serde::Serialize;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// File names listed per kind in the question; the counts hold all.
const NAMES_SHOWN: usize = 5;
/// Photos read at a time, as the queue pins them.
const PIN_CHUNK: usize = 32;

/// One kind of collision.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CollisionKind {
    pub count: usize,
    /// The first file names (`NAMES_SHOWN`), the way they are on disk.
    pub names: Vec<String>,
}

impl CollisionKind {
    fn add(&mut self, path: &Path) {
        self.count += 1;
        if self.names.len() < NAMES_SHOWN
            && let Some(name) = path.file_name()
        {
            self.names.push(name.to_string_lossy().into_owned());
        }
    }
}

/// What an export finds at the names it would write.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CollisionSummary {
    /// All collisions; the question is asked only when this is not 0.
    pub total: usize,
    /// Our own earlier export, untouched since and with the same settings:
    /// Replace if changed leaves it.
    pub unchanged_own: CollisionKind,
    /// Our own earlier export, untouched since, that this export changes:
    /// Replace if changed replaces it.
    pub changed_own: CollisionKind,
    /// Any other file: another program's, one edited since, an original.
    /// Replace if changed keeps it and numbers the new file.
    pub other: CollisionKind,
    /// Of `other`, the photos of a library (never replaced).
    pub originals: usize,
}

impl CollisionSummary {
    fn finish(mut self) -> Self {
        self.total = self.unchanged_own.count + self.changed_own.count + self.other.count;
        self
    }
}

/// How the files `targets` would write for one photo meet what is on disk.
/// Every recipe is planned as Replace if changed would, which tells the
/// three kinds apart; the folders are not created.
#[allow(clippy::too_many_arguments)]
fn collide_photo(
    summary: &mut CollisionSummary,
    targets: &[&ExportTarget],
    item: &PhotoItem,
    pinned: &crate::export_processing::PinnedPhoto,
    settings: &crate::app_settings::AppSettings,
    used: &Mutex<HashSet<PathBuf>>,
    previous: &dyn Fn(&Path) -> Previous,
) -> Result<(), String> {
    let (source_path, _) = crate::file_management::parse_virtual_path(item.image_path_str);
    let is_raw = crate::formats::is_raw_file(&source_path);
    let inputs = photo_inputs(pinned, settings, is_raw);
    let photo_key = rawtonic_export::existing_files::photo_key(
        item.image_path_str,
        pinned.source_identity.as_deref(),
    );
    let photo = photo_key.as_deref().unwrap_or_default();
    let file_date = pinned.naming_date(&source_path);
    let mask_layers =
        rawtonic_engine::image_processing::mask_definitions_from_json(&pinned.adjustments)
            .iter()
            .filter(|definition| definition.produces_bitmap())
            .take(crate::image_processing::MAX_MASKS)
            .count();
    for target in targets {
        let capture = target.export_settings.naming_needs_capture().then(|| {
            crate::export_processing::naming_capture(&source_path, pinned.metadata.as_ref())
        });
        let hash = target_inputs_hash(&inputs, target);
        let previous = |path: &Path| match &photo_key {
            Some(_) => previous(path),
            None => Previous::Unknown,
        };
        let as_asked = |policy: ExistingFiles| ExportTarget {
            export_settings: rawtonic_export::export_processing::ExportSettings {
                existing_files: policy,
                ..target.export_settings.clone()
            },
            output_folder_or_file: target.output_folder_or_file.clone(),
            is_explicit_file_path: target.is_explicit_file_path,
            base_origin_folders: target.base_origin_folders.clone(),
            output_format: target.output_format.clone(),
            tracking: None,
        };
        let plan = |policy: ExistingFiles, used: &Mutex<HashSet<PathBuf>>| {
            plan_output_path(
                &as_asked(policy),
                item,
                &source_path,
                &file_date,
                capture.as_ref(),
                mask_layers,
                used,
                photo,
                &hash,
                &previous,
                false,
            )
        };
        match plan(ExistingFiles::ReplaceIfChanged, used)? {
            Planned::Skip {
                path,
                outcome: Outcome::SkippedUnchanged,
            } => summary.unchanged_own.add(&path),
            Planned::Write {
                path,
                replace: Some(_),
                ..
            } => summary.changed_own.add(&path),
            Planned::Write {
                kept: Some(kept), ..
            } => {
                // The file that stays is the recipe's own name, which the
                // same plan under Skip names (its new file is the numbered one).
                let existing = match plan(ExistingFiles::Skip, &Mutex::new(HashSet::new()))? {
                    Planned::Skip { path, .. } => path,
                    Planned::Write { path, .. } => path,
                };
                summary.other.add(&existing);
                if kept == Outcome::KeptOriginal {
                    summary.originals += 1;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// What `requests` (the recipes of one export, as `export_images_batch`
/// takes them) would meet on disk. `records` and `known_libraries` as
/// the export itself reads them.
pub(crate) fn plan_collisions(
    requests: &[ExportRequest],
    settings: &crate::app_settings::AppSettings,
    records: Option<&ExportRecords>,
    known_libraries: &[PathBuf],
) -> Result<CollisionSummary, String> {
    let mut summary = CollisionSummary::default();
    let previous = |path: &Path| previous_export(path, known_libraries, records);
    // Recipes of one photo set read each photo once, as the queue does.
    let mut groups: Vec<Vec<&ExportRequest>> = Vec::new();
    for request in requests {
        match groups.iter_mut().find(|group| group[0].pins_like(request)) {
            Some(group) => group.push(request),
            None => groups.push(vec![request]),
        }
    }
    for group in groups {
        let first = group[0];
        let targets: Vec<ExportTarget> = group
            .iter()
            .map(|request| ExportTarget {
                output_folder_or_file: request.output_folder_or_file.clone(),
                is_explicit_file_path: request.is_explicit_file_path,
                base_origin_folders: request.base_origin_folders.clone(),
                export_settings: request.export_settings.clone(),
                output_format: request.output_format.clone(),
                tracking: None,
            })
            .collect();
        let targets: Vec<&ExportTarget> = targets.iter().collect();
        let items = rawtonic_export::sidecars::plan_export_items(first.paths.clone());
        let used = Mutex::new(HashSet::new());
        for chunk in items.chunks(PIN_CHUNK) {
            let paths: Vec<String> = chunk.iter().map(|item| item.1.clone()).collect();
            let photos = pin_photos(
                &paths,
                first.current_edit_path.as_deref(),
                first.current_edit_adjustments.as_ref(),
                first.library.directory(),
            )?;
            for ((global_index, path, appearance_count, explicit_vc), pinned) in
                chunk.iter().zip(&photos)
            {
                let item = PhotoItem {
                    global_index: *global_index,
                    image_path_str: path,
                    appearance_count: *appearance_count,
                    explicit_vc: *explicit_vc,
                    total_paths: first.paths.len(),
                };
                collide_photo(
                    &mut summary,
                    &targets,
                    &item,
                    pinned,
                    settings,
                    &used,
                    &previous,
                )?;
            }
        }
    }
    Ok(summary.finish())
}

/// The collisions of the recipes set to "Ask", found before the export is
/// queued, so the Export form asks once. Nothing is written.
#[tauri::command]
pub async fn plan_export_collisions(
    requests: Vec<ExportRequest>,
    app_handle: tauri::AppHandle,
) -> Result<CollisionSummary, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let settings = crate::state::settings::current(&app_handle);
        let records = open_export_records(&app_handle);
        plan_collisions(
            &requests,
            &settings,
            records.as_ref(),
            &known_libraries(&settings),
        )
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A RAW file in a temp folder, a request for it (beside the original,
    /// one recipe called "{orig_base}-edited" asking), and the
    /// `export_images_batch` JSON of both.
    fn request(dir: &Path, raw: &Path, template: &str) -> ExportRequest {
        serde_json::from_value(serde_json::json!({
            "paths": [raw.to_string_lossy()],
            "outputFolderOrFile": dir.to_string_lossy(),
            "isExplicitFilePath": false,
            "baseOriginFolders": [],
            "exportSettings": {
                "jpegQuality": 90, "keepMetadata": false, "stripGps": false,
                "nameTemplate": template, "destinationType": "originalFolder",
                "subfolder": "web",
                "existingFiles": "ask",
            },
            "outputFormat": "jpg",
            "currentEditPath": null,
            "currentEditAdjustments": null,
            "library": "noLibrary",
        }))
        .unwrap()
    }

    fn summary(requests: &[ExportRequest], records: &ExportRecords) -> CollisionSummary {
        plan_collisions(
            requests,
            &crate::app_settings::AppSettings::default(),
            Some(records),
            &[],
        )
        .unwrap()
    }

    /// Records `own` as the export `request` made of its photo, with
    /// `inputs` (the request's own hash when None).
    fn record(records: &ExportRecords, request: &ExportRequest, own: &Path, inputs: Option<&str>) {
        let settings = crate::app_settings::AppSettings::default();
        let pinned = pin_photos(&request.paths, None, None, None)
            .unwrap()
            .remove(0);
        let key = rawtonic_export::existing_files::photo_key(
            &request.paths[0],
            pinned.source_identity.as_deref(),
        )
        .unwrap();
        let target = ExportTarget {
            output_folder_or_file: request.output_folder_or_file.clone(),
            is_explicit_file_path: false,
            base_origin_folders: Vec::new(),
            export_settings: request.export_settings.clone(),
            output_format: request.output_format.clone(),
            tracking: None,
        };
        let hash = target_inputs_hash(&photo_inputs(&pinned, &settings, true), &target);
        records.record(own, &key, inputs.unwrap_or(&hash)).unwrap();
    }

    /// Before anything renders, the plan counts what the names meet: our
    /// own unchanged export, our own export that changed, any other file,
    /// and nothing at free names; it writes and creates nothing.
    #[test]
    fn the_plan_counts_each_kind_of_collision_without_writing() {
        let dir = tempfile::tempdir().unwrap();
        let raw = dir.path().join("a.CR3");
        rawtonic_fs::atomic_write(&raw, b"original RAW").unwrap();
        let records = ExportRecords::open(&dir.path().join("records.sqlite")).unwrap();
        let request = request(dir.path(), &raw, "{orig_base}-edited");
        let own = dir.path().join("web/a-edited.jpg");

        assert_eq!(summary(std::slice::from_ref(&request), &records).total, 0);
        assert!(!dir.path().join("web").exists(), "no folder is created");

        // A file nobody recorded.
        std::fs::create_dir(dir.path().join("web")).unwrap();
        rawtonic_fs::atomic_write(&own, b"someone else's file").unwrap();
        let found = summary(std::slice::from_ref(&request), &records);
        assert_eq!((found.total, found.other.count), (1, 1));
        assert_eq!(found.other.names, vec!["a-edited.jpg".to_string()]);
        assert_eq!(found.originals, 0);

        // Our own export with other inputs: it would be replaced.
        rawtonic_fs::atomic_write(&own, b"export v1").unwrap();
        record(&records, &request, &own, Some("old inputs"));
        let found = summary(std::slice::from_ref(&request), &records);
        assert_eq!((found.total, found.changed_own.count), (1, 1));
        assert_eq!(found.changed_own.names, vec!["a-edited.jpg".to_string()]);

        // Our own export with today's inputs: nothing to do.
        record(&records, &request, &own, None);
        let found = summary(std::slice::from_ref(&request), &records);
        assert_eq!((found.total, found.unchanged_own.count), (1, 1));

        // The file is not touched, nor any other made.
        assert_eq!(std::fs::read(&own).unwrap(), b"export v1");
        let mut names: Vec<_> = std::fs::read_dir(dir.path().join("web"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, ["a-edited.jpg"]);
    }

    /// A file a library lists as a photo is reported as the other kind and
    /// counted as an original: it is never offered for replacement.
    #[test]
    fn a_photo_of_a_library_is_an_original_not_an_own_export() {
        let dir = tempfile::tempdir().unwrap();
        let raw = dir.path().join("a.CR3");
        rawtonic_fs::atomic_write(&raw, b"original RAW").unwrap();
        let records = ExportRecords::open(&dir.path().join("records.sqlite")).unwrap();
        let request = request(dir.path(), &raw, "{orig_base}");
        // "a.jpg" is the camera JPEG beside it, edited in a library.
        std::fs::create_dir(dir.path().join("web")).unwrap();
        let jpg = dir.path().join("web/a.jpg");
        rawtonic_fs::atomic_write(&jpg, b"camera JPEG").unwrap();
        rawtonic_fs::atomic_write(
            &crate::exif_processing::get_primary_sidecar_path(&jpg),
            b"{}",
        )
        .unwrap();
        record(&records, &request, &jpg, None);
        let found = summary(std::slice::from_ref(&request), &records);
        assert_eq!((found.other.count, found.originals), (1, 1));
        assert_eq!(found.changed_own.count + found.unchanged_own.count, 0);
        assert_eq!(found.other.names, vec!["a.jpg".to_string()]);
    }

    /// The names listed are capped; the counts hold all.
    #[test]
    fn the_list_of_names_is_short_and_the_count_complete() {
        let mut kind = CollisionKind::default();
        for i in 0..8 {
            kind.add(Path::new(&format!("/x/{i}.jpg")));
        }
        assert_eq!((kind.count, kind.names.len()), (8, NAMES_SHOWN));
    }

    /// The summary as the Export form reads it (pinned on its side by
    /// src/contracts/exportCollisions.test.ts).
    #[test]
    fn collisions_contract() {
        let kind = |count, names: &[&str]| CollisionKind {
            count,
            names: names.iter().map(|n| n.to_string()).collect(),
        };
        let summary = CollisionSummary {
            unchanged_own: kind(2, &["a.jpg", "b.jpg"]),
            changed_own: kind(1, &["c.jpg"]),
            other: kind(1, &["d.jpg"]),
            originals: 1,
            ..Default::default()
        }
        .finish();
        let expected: serde_json::Value =
            serde_json::from_str(include_str!("../../src/contracts/exportCollisions.json"))
                .unwrap();
        assert_eq!(serde_json::to_value(&summary).unwrap(), expected["summary"]);
    }
}
