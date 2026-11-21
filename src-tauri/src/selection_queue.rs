// SPDX-License-Identifier: AGPL-3.0-or-later
//! The selection queue: AI selections (Sky, Subject, People, depth range)
//! of photos that were given layers with them while closed — a preset or
//! style applied to many photos in the Library, settings pasted or synced,
//! a capture given a preset — are computed in the background, one photo at
//! a time, as the editor computes them when the photo opens
//! (`ai_selections::detect_missing`), and kept in each photo's recipe
//! (`export_processing::keep_detected_selections`); its thumbnail is made
//! again. Before, such photos waited for Develop (or their export). A photo
//! whose selection cannot be computed (no model installed, a person the
//! photo does not have, an unreadable file) is reported by name in
//! `PROGRESS_EVENT`, never skipped silently; its export computes or refuses
//! it again. Object selections are left to the user (they need a click on
//! each photo).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, OnceLock};

use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, Emitter};

/// Event with a `Progress` after each photo and once when the queue is
/// empty (`finished`).
pub const PROGRESS_EVENT: &str = "ai-selections-progress";

/// A photo whose selections were not computed, and why.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Failed {
    pub path: String,
    pub message: String,
}

/// Where the queue stands: photos queued since it was last empty
/// (`total`), those done (computed, nothing to compute or failed), the one
/// it computes now, the failures so far.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    pub total: usize,
    pub done: usize,
    pub current: Option<String>,
    pub failed: Vec<Failed>,
    pub finished: bool,
}

#[derive(Default)]
struct Queue {
    waiting: VecDeque<String>,
    running: bool,
    total: usize,
    done: usize,
    failed: Vec<Failed>,
}

impl Queue {
    /// Adds the photos not already waiting; whether a worker must start.
    fn add(&mut self, paths: impl IntoIterator<Item = String>) -> bool {
        for path in paths {
            if !self.waiting.contains(&path) {
                self.waiting.push_back(path);
                self.total += 1;
            }
        }
        if self.running || self.waiting.is_empty() {
            return false;
        }
        self.running = true;
        true
    }

    fn progress(&self, current: Option<String>, finished: bool) -> Progress {
        Progress {
            total: self.total,
            done: self.done,
            current,
            failed: self.failed.clone(),
            finished,
        }
    }

    /// The next photo with the progress to announce, or the final progress
    /// (the queue is then reset and its worker ends).
    fn next(&mut self) -> Result<(String, Progress), Progress> {
        match self.waiting.pop_front() {
            Some(path) => {
                let progress = self.progress(Some(path.clone()), false);
                Ok((path, progress))
            }
            None => {
                let progress = self.progress(None, true);
                *self = Queue::default();
                Err(progress)
            }
        }
    }

    fn settle(&mut self, path: String, result: Result<(), String>) {
        self.done += 1;
        if let Err(message) = result {
            self.failed.push(Failed { path, message });
        }
    }
}

static QUEUE: Mutex<Queue> = Mutex::new(Queue {
    waiting: VecDeque::new(),
    running: false,
    total: 0,
    done: 0,
    failed: Vec::new(),
});
static APP: OnceLock<AppHandle> = OnceLock::new();

/// Lets the queue run (at app start).
pub fn install(app: &AppHandle) {
    let _ = APP.set(app.clone());
}

/// Whether a recipe has a selection the queue computes (shown, without its
/// bitmap; `ai_selections::undetected`).
pub fn wants_detection(adjustments: &Value) -> bool {
    !rawtonic_engine::ai_selections::undetected(adjustments).is_empty()
}

/// Whether a recipe still waits for a selection after the queue ran: one
/// without anything to render (a migrated bitmap the queue could not
/// compute again renders as it is).
fn still_waiting(adjustments: &Value) -> bool {
    rawtonic_engine::ai_selections::missing_without_stand_ins(adjustments)
}

/// Queues `paths` (photos or virtual copies) whose recipes were just given
/// layers with AI selections; a photo already waiting is not queued twice.
pub fn enqueue(paths: impl IntoIterator<Item = String>) {
    let Some(app) = APP.get() else {
        return;
    };
    let start = QUEUE.lock().unwrap_or_else(|e| e.into_inner()).add(paths);
    if !start {
        return;
    }
    let app = app.clone();
    let spawned = std::thread::Builder::new()
        .name("ai-selections".into())
        // The decode and the warp keep large frames on the stack, as the
        // export workers do.
        .stack_size(8 << 20)
        .spawn(move || run(&app));
    if let Err(e) = spawned {
        log::warn!("AI selections: the queue could not start: {e}");
        *QUEUE.lock().unwrap_or_else(|e| e.into_inner()) = Queue::default();
    }
}

fn run(app: &AppHandle) {
    loop {
        let next = QUEUE.lock().unwrap_or_else(|e| e.into_inner()).next();
        let path = match next {
            Ok((path, progress)) => {
                let _ = app.emit(PROGRESS_EVENT, progress);
                path
            }
            Err(finished) => {
                if !finished.failed.is_empty() {
                    log::warn!(
                        "AI selections: {} of {} photos not computed",
                        finished.failed.len(),
                        finished.total
                    );
                }
                let _ = app.emit(PROGRESS_EVENT, finished);
                return;
            }
        };
        let started = std::time::Instant::now();
        let result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| detect_photo(app, &path)))
                .unwrap_or_else(|_| Err("the computation stopped unexpectedly".into()));
        match &result {
            Ok(()) => log::info!(
                "AI selections of {path} in {} ms",
                started.elapsed().as_millis()
            ),
            Err(e) => log::warn!("AI selections of {path} not computed: {e}"),
        }
        QUEUE
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .settle(path, result);
    }
}

/// Computes and keeps the missing selections of one photo; `Ok` also when
/// it has none (left by an edit, or computed when it was opened).
fn detect_photo(app: &AppHandle, path: &str) -> Result<(), String> {
    let (source_path, sidecar_path) = rawtonic_export::sidecars::parse_virtual_path(path);
    // The recipe without touching the original (no EXIF read yet).
    let mut recipe = crate::exif_processing::load_sidecar(&sidecar_path).adjustments;
    if !wants_detection(&recipe) {
        return Ok(());
    }
    // An original that is not here is never fetched for a preset (a cloud
    // download, a drive to connect): reported, computed when the photo is
    // opened or exported.
    if let Some(missing) = crate::export_processing::missing_source(&source_path) {
        return Err(missing);
    }
    if crate::file_management::is_cloud_placeholder(&source_path) {
        return Err(
            "the original is in the cloud, not on this computer; its selections are computed when it is opened or exported"
                .into(),
        );
    }
    rawtonic_export::file_export::require_local_tiff(&source_path)?;
    if crate::export_processing::needs_frame_migration(&recipe, &source_path) {
        return Err(
            "an edit from before this camera TIFF developed as a RAW; its selections are computed when it is opened or exported"
                .into(),
        );
    }
    // As the export prepares it: the lens correction resolved for the photo
    // (the selections are computed on the corrected photo).
    rawtonic_export::normalize::normalize(
        &mut recipe,
        &rawtonic_export::normalize::RecipeSource::file(&source_path),
        rawtonic_export::normalize::Bitmaps::None,
        rawtonic_export::normalize::Defaults::Render,
    )
    .lens?;
    let source = source_path.to_string_lossy().to_string();
    let is_raw = rawtonic_engine::formats::is_raw_file(&source);
    let bytes =
        std::fs::read(&source_path).map_err(|e| format!("the photo's file cannot be read: {e}"))?;
    // The decode without AI Denoise (a minute per photo without a GPU, and
    // its model may not be loaded). The networks see the photo at their own
    // small input size; that this matches a detection on the denoised
    // decode (as the export's) is not measured (G28).
    let mut decode = recipe.clone();
    if let Some(map) = decode.as_object_mut() {
        map.remove("aiDenoise");
    }
    let settings = crate::state::settings::current(app);
    let base = crate::image_loader::load_base_image_for_recipe(
        &bytes, &source, false, &settings, None, None, &decode,
    )
    .map_err(|e| format!("the photo cannot be decoded: {e}"))?;
    drop(bytes);
    let detected = rawtonic_engine::ai_selections::detect_missing(
        &mut recipe,
        &Arc::new(base),
        is_raw,
        Some(&crate::ai_commands::ExportSelections { app_handle: app }),
        &mut || {},
    )?;
    if crate::export_processing::keep_detected_selections(path, &detected)? {
        crate::file_management::refresh_thumbnail(app, path);
        return Ok(());
    }
    // Not kept: the photo changed meanwhile (its geometry, the layer). Still
    // waiting is reported, never taken for done; a camera TIFF's migrated
    // bitmap kept because its model is not installed here is no change.
    let now = crate::exif_processing::load_sidecar(&sidecar_path).adjustments;
    if still_waiting(&now) {
        return Err(
            "the photo was changed while its selections were computed; they are computed when it is opened or exported"
                .into(),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn photos_are_queued_once_and_every_failure_is_reported() {
        let mut queue = Queue::default();
        assert!(queue.add(["a".to_string(), "b".to_string(), "a".to_string()]));
        // Running: more photos join without a second worker.
        assert!(!queue.add(["b".to_string(), "c".to_string()]));
        assert_eq!(queue.total, 3);
        let (first, progress) = queue.next().unwrap();
        assert_eq!(first, "a");
        assert_eq!(progress.current.as_deref(), Some("a"));
        queue.settle(first, Ok(()));
        let (second, _) = queue.next().unwrap();
        queue.settle(second, Err("the face model is not installed".into()));
        let (third, progress) = queue.next().unwrap();
        assert_eq!(progress.done, 2);
        assert_eq!(progress.failed.len(), 1);
        queue.settle(third, Ok(()));
        let finished = queue.next().unwrap_err();
        assert_eq!(
            finished,
            Progress {
                total: 3,
                done: 3,
                current: None,
                failed: vec![Failed {
                    path: "b".into(),
                    message: "the face model is not installed".into()
                }],
                finished: true
            }
        );
        // Empty again: the next photos start a new worker and a new count.
        assert!(queue.add(["d".to_string()]));
        assert_eq!(queue.total, 1);
    }

    #[test]
    fn only_recipes_with_a_selection_to_compute_are_queued() {
        let layer = |kind: &str, parameters: Value| {
            json!({"masks": [{"id": "l", "visible": true, "subMasks": [
                {"id": "s", "type": kind, "visible": true, "parameters": parameters}]}]})
        };
        assert!(wants_detection(&layer(
            "ai-sky",
            json!({"maskDataBase64": null, "detectPending": true})
        )));
        assert!(wants_detection(&layer(
            "ai-people",
            json!({"maskDataBase64": null, "detectPending": true, "parts": ["faceSkin"]})
        )));
        assert!(wants_detection(&layer(
            "ai-depth",
            json!({"maskDataBase64": null, "detectPending": true, "minDepth": 50})
        )));
        assert!(!wants_detection(&layer(
            "ai-sky",
            json!({"maskDataBase64": "data:image/png;base64,AAAA"})
        )));
        // An Object selection waits for the user's click on the photo.
        assert!(!wants_detection(&layer(
            "ai-subject",
            json!({"maskDataBase64": null, "detectPending": true})
        )));
        assert!(!wants_detection(&json!({"exposure": 1.0})));
    }

    /// A camera TIFF's migrated bitmap the queue kept (its model is not
    /// installed) is not taken for a photo changed meanwhile.
    #[test]
    fn a_kept_migrated_bitmap_is_not_a_change() {
        let layer = |parameters: Value| {
            json!({"masks": [{"id": "l", "visible": true, "subMasks": [
                {"id": "s", "type": "ai-sky", "visible": true, "parameters": parameters}]}]})
        };
        let migrated = layer(json!({"maskDataBase64": "data:thumb", "detectAgain": true,
            "bitmapFrame": {"width": 288, "height": 192}}));
        assert!(wants_detection(&migrated));
        assert!(!still_waiting(&migrated));
        assert!(still_waiting(&layer(json!({"maskDataBase64": null}))));
    }
}
