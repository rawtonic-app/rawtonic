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

/// Event with the path of a photo whose selections the queue computed: the
/// editor that has it open takes them (`adopt_selections`) into the recipe
/// it holds, which may be ahead of the saved one (an edit made meanwhile),
/// so its next save does not write the selections back as waiting.
pub const DETECTED_EVENT: &str = "ai-selections-detected";

/// The selections computed last, per photo, until its editor takes them
/// (`adopt_selections`); a few only (one photo is open at a time; the
/// others' are in their recipes).
const HANDED_KEPT: usize = 4;
static HANDED: Mutex<VecDeque<(String, Vec<rawtonic_engine::ai_selections::Detected>)>> =
    Mutex::new(VecDeque::new());

/// Keeps `detected` of `path` for its editor, over what it was given before.
fn hand(path: &str, detected: Vec<rawtonic_engine::ai_selections::Detected>) {
    let mut handed = HANDED.lock().unwrap_or_else(|e| e.into_inner());
    handed.retain(|(p, _)| p != path);
    handed.push_back((path.to_string(), detected));
    while handed.len() > HANDED_KEPT {
        handed.pop_front();
    }
}

/// What was handed for `path` (kept: an editor whose recipe changed while
/// it asked asks again).
fn handed(path: &str) -> Option<Vec<rawtonic_engine::ai_selections::Detected>> {
    let handed = HANDED.lock().unwrap_or_else(|e| e.into_inner());
    handed
        .iter()
        .find(|(p, _)| p == path)
        .map(|(_, detected)| detected.clone())
}

/// The computed selections a saved recipe holds (shown or not), as the
/// queue would hand them: each component of an automatic kind with its own
/// bitmap (not a camera TIFF's migrated stand-in).
fn saved_selections(adjustments: &Value) -> Vec<rawtonic_engine::ai_selections::Detected> {
    use rawtonic_engine::ai_selections::{AutoSelection, Detected, detects_again};
    let mut found = Vec::new();
    for layer in adjustments["masks"].as_array().into_iter().flatten() {
        let Some(layer_id) = layer["id"].as_str() else {
            continue;
        };
        for sub in layer["subMasks"].as_array().into_iter().flatten() {
            let parameters = &sub["parameters"];
            let (Some(id), Some(selection)) = (
                sub["id"].as_str(),
                sub["type"].as_str().and_then(AutoSelection::of),
            ) else {
                continue;
            };
            if rawtonic_engine::masks::has_bitmap(parameters) && !detects_again(parameters) {
                found.push(Detected {
                    layer_id: layer_id.to_string(),
                    component_id: id.to_string(),
                    selection,
                    parameters: parameters.clone(),
                });
            }
        }
    }
    found
}

/// The components `adopted` filled that `recipe` left waiting: component id
/// → its parameters with the selection.
fn filled_components(recipe: &Value, adopted: &Value) -> serde_json::Map<String, Value> {
    let components = |recipe: &Value| -> Vec<(String, Value)> {
        recipe["masks"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|layer| layer["subMasks"].as_array().into_iter().flatten())
            .filter_map(|sub| Some((sub["id"].as_str()?.to_string(), sub["parameters"].clone())))
            .collect()
    };
    let before: std::collections::HashMap<String, Value> = components(recipe).into_iter().collect();
    components(adopted)
        .into_iter()
        .filter(|(id, parameters)| before.get(id) != Some(parameters))
        .collect()
}

/// `recipes` (the open photo's recipe and its History entries) with the
/// selections the queue computed for `path` put in, by the rule the queue
/// keeps them in the saved recipe (`ai_selections::keep_detected_in`):
/// only into components still waiting (same layer and component id and
/// type, no bitmap, the same parts or version) in the geometry they were
/// computed in, and not into a recipe with AI Denoise (`denoise_gate`).
/// Everything else of a recipe stays as it is. None for a recipe that
/// takes nothing.
pub fn adopt_into(
    recipes: Vec<Value>,
    detected: &[rawtonic_engine::ai_selections::Detected],
    frame_of: impl Fn(&Value) -> Value,
) -> Vec<Option<Value>> {
    recipes
        .into_iter()
        .map(|mut recipe| {
            // Given AI Denoise: takes no selection made without it.
            denoise_gate(&recipe).ok()?;
            let frame = frame_of(&recipe);
            rawtonic_engine::ai_selections::keep_detected_in(&mut recipe, &frame, detected)
                .then_some(recipe)
        })
        .collect()
}

/// The editor of the open photo `path` takes the selections the queue
/// computed for it (`DETECTED_EVENT`) into `recipes` (`adopt_into`): per
/// recipe the components it fills (id → parameters), None when it fills
/// none (none computed, or none waits as they were computed).
#[tauri::command]
pub async fn adopt_selections(
    path: String,
    recipes: Vec<Value>,
) -> Result<Vec<Option<serde_json::Map<String, Value>>>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let (source_path, sidecar_path) = rawtonic_export::sidecars::parse_virtual_path(&path);
        // Handed by the queue, or (handed to others since) as kept in the
        // saved recipe.
        let detected = handed(&path).unwrap_or_else(|| {
            saved_selections(&crate::exif_processing::load_sidecar(&sidecar_path).adjustments)
        });
        let adopted = adopt_into(recipes.clone(), &detected, |recipe| {
            crate::export_processing::detection_frame(recipe, &source_path)
        });
        recipes
            .iter()
            .zip(adopted)
            .map(|(recipe, adopted)| adopted.map(|adopted| filled_components(recipe, &adopted)))
            .collect()
    })
    .await
    .map_err(|e| e.to_string())
}

/// Queues the photos among `paths` whose saved recipes wait for AI
/// selections: the Library's active photograph, which the editor writes
/// itself (the batch leaves it out), after its save. Its editor takes what
/// was computed (`DETECTED_EVENT`). Photos that wait for nothing are left
/// alone.
#[tauri::command]
pub async fn queue_selections(paths: Vec<String>) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        enqueue(paths.into_iter().filter(|path| {
            let (_, sidecar) = rawtonic_export::sidecars::parse_virtual_path(path);
            wants_detection(&crate::exif_processing::load_sidecar(&sidecar).adjustments)
        }));
    })
    .await
    .map_err(|e| e.to_string())
}

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

/// A photo developed with AI Denoise is left to its export and to Develop,
/// which compute its selections on the denoised decode: the queue's decode
/// has no AI Denoise (a minute per photo without a GPU, its model perhaps
/// not loaded), and that a selection made on it equals one made on the
/// denoised decode is not measured (G28). Reported, never taken for done.
fn denoise_gate(recipe: &Value) -> Result<(), String> {
    if rawtonic_engine::image_loader::ai_denoise_settings(recipe).is_some() {
        return Err(
            "AI Denoise is on; its selections are computed on the denoised photo when it is opened or exported"
                .into(),
        );
    }
    Ok(())
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
    denoise_gate(&recipe)?;
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
    // The decode without AI Denoise, which no recipe here has
    // (`denoise_gate`); a stale key renders nothing.
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
    // Into the recipe as it is now: one given AI Denoise meanwhile is left
    // to its export (`denoise_gate`).
    let kept = crate::export_processing::keep_detected_selections_where(path, &detected, |now| {
        denoise_gate(now).is_ok()
    });
    // Also for a photo changed meanwhile: its editor, if it has it open,
    // takes what still fits the recipe it holds.
    if !detected.is_empty() {
        hand(path, detected);
        let _ = app.emit(DETECTED_EVENT, path);
    }
    if kept? {
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

    /// A photo with AI Denoise waits for its export or Develop, which
    /// detect on the denoised decode.
    #[test]
    fn photos_with_ai_denoise_are_left_to_their_export() {
        assert!(denoise_gate(&json!({"exposure": 1})).is_ok());
        assert!(denoise_gate(&json!({"aiDenoise": 0})).is_ok());
        let error = denoise_gate(&json!({"aiDenoise": 60})).unwrap_err();
        assert!(error.contains("AI Denoise"), "{error}");
        // Switched off with its section: rendered without it.
        assert!(
            denoise_gate(&json!({"aiDenoise": 60, "sectionVisibility": {"noiseReduction": false}}))
                .is_ok()
        );
    }

    /// The open photo's editor takes the queue's selections into the recipe
    /// it holds: an edit made while they were computed (a slider) stays,
    /// only the waiting component is filled; a recipe in another frame, or
    /// whose component holds a bitmap already, takes nothing.
    #[test]
    fn the_editor_takes_the_queues_selections_without_losing_its_edits() {
        use rawtonic_engine::ai_selections::{AutoSelection, Detected, detected_parameters};
        let saved = json!({"exposure": 0.0, "masks": [{"id": "sky-layer", "visible": true,
            "subMasks": [{"id": "sky-1", "type": "ai-sky", "visible": true,
                "parameters": {"maskDataBase64": null, "detectPending": true, "grow": 0, "feather": 0}}]}]});
        let found = vec![Detected {
            layer_id: "sky-layer".into(),
            component_id: "sky-1".into(),
            selection: AutoSelection::Sky,
            parameters: detected_parameters(
                &saved["masks"][0]["subMasks"][0]["parameters"],
                &saved,
                "data:image/png;base64,SKY".into(),
            ),
        }];
        // Edited while the queue computed: Exposure, and the layer's Feather.
        let mut edited = saved.clone();
        edited["exposure"] = json!(1.5);
        edited["masks"][0]["subMasks"][0]["parameters"]["feather"] = json!(30);
        let mut turned = saved.clone();
        turned["orientationSteps"] = json!(1);
        let mut filled = saved.clone();
        filled["masks"][0]["subMasks"][0]["parameters"]["maskDataBase64"] = json!("data:own");
        let plain = json!({"exposure": 0.3});
        // AI Denoise switched on while the queue computed.
        let mut denoised = edited.clone();
        denoised["aiDenoise"] = json!(60);
        let adopted = adopt_into(
            vec![edited.clone(), turned, filled, plain, denoised],
            &found,
            |recipe| recipe.clone(),
        );
        let first = adopted[0]
            .as_ref()
            .expect("the waiting component is filled");
        assert_eq!(first["exposure"], 1.5);
        let parameters = &first["masks"][0]["subMasks"][0]["parameters"];
        assert_eq!(parameters["maskDataBase64"], "data:image/png;base64,SKY");
        assert_eq!(parameters["feather"], 30);
        assert!(parameters.get("detectPending").is_none());
        // Nothing else of the recipe changed.
        let mut back = first.clone();
        back["masks"][0]["subMasks"][0]["parameters"] =
            edited["masks"][0]["subMasks"][0]["parameters"].clone();
        assert_eq!(back, edited);
        assert_eq!(adopted[1..], [None, None, None, None]);
    }

    /// Handed to other photos since: the editor finds them in the saved
    /// recipe the queue kept them in; a recipe given AI Denoise takes none.
    #[test]
    fn the_saved_recipe_stands_in_for_an_evicted_hand_off() {
        let saved = json!({"masks": [{"id": "sky-layer", "visible": true, "subMasks": [
            {"id": "sky-1", "type": "ai-sky", "visible": true,
             "parameters": {"maskDataBase64": "data:sky", "rotation": 0.0, "flipHorizontal": false,
                "flipVertical": false, "orientationSteps": 0,
                "warp": rawtonic_engine::mask_generation::mask_warp(&json!({}))}},
            {"id": "stand-in", "type": "ai-sky", "visible": true,
             "parameters": {"maskDataBase64": "data:thumb", "detectAgain": true}},
            {"id": "drawn", "type": "brush", "visible": true, "parameters": {}}]}]});
        let found = saved_selections(&saved);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].component_id, "sky-1");
        let waiting = json!({"masks": [{"id": "sky-layer", "visible": true, "subMasks": [
            {"id": "sky-1", "type": "ai-sky", "visible": true,
             "parameters": {"maskDataBase64": null, "detectPending": true}}]}]});
        let adopted = adopt_into(vec![waiting], &found, |recipe| recipe.clone());
        assert_eq!(
            adopted[0].as_ref().unwrap()["masks"][0]["subMasks"][0]["parameters"]["maskDataBase64"],
            "data:sky"
        );
    }

    /// The newest selections per photo are handed, for a few photos.
    #[test]
    fn the_newest_selections_of_a_few_photos_are_handed() {
        use rawtonic_engine::ai_selections::{AutoSelection, Detected};
        let one = |id: &str| Detected {
            layer_id: "l".into(),
            component_id: id.into(),
            selection: AutoSelection::Sky,
            parameters: json!({}),
        };
        hand("/handed/a.nef", vec![one("old")]);
        hand("/handed/a.nef", vec![one("new")]);
        assert_eq!(handed("/handed/a.nef").unwrap()[0].component_id, "new");
        for i in 0..HANDED_KEPT {
            hand(&format!("/handed/other{i}.nef"), vec![one("x")]);
        }
        assert!(handed("/handed/a.nef").is_none());
    }

    #[test]
    fn only_the_filled_components_are_answered() {
        let recipe = json!({"masks": [{"id": "l", "subMasks": [
            {"id": "a", "parameters": {"maskDataBase64": null}},
            {"id": "b", "parameters": {"maskDataBase64": "data:kept"}}]}]});
        let mut adopted = recipe.clone();
        adopted["masks"][0]["subMasks"][0]["parameters"] = json!({"maskDataBase64": "data:sky"});
        let filled = filled_components(&recipe, &adopted);
        assert_eq!(filled.len(), 1);
        assert_eq!(filled["a"], json!({"maskDataBase64": "data:sky"}));
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
