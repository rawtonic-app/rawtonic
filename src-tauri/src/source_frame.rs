//! Desktop glue for `rawtonic_engine::source_frame` (ADR 0020): which photos
//! "Update frame" would change, and the migration of a recipe to the newest
//! frame for the editor's history step. The batch update of closed photos
//! writes their sidecars (`file_management::edits::update_source_frame_for_paths`).
pub use rawtonic_engine::source_frame::*;

use serde_json::Value;
use std::path::Path;

use crate::file_management::parse_virtual_path;

/// `adjustments` (a recipe of the RAW at `source`) moved to frame version
/// `to`, its coordinates carried over (`migrate`); the warp the coordinates
/// are shown through is the recipe's with its lens correction filled in, as
/// renders fill it in. A new recipe (never edited) is left as it is: it
/// already develops the newest frame. Fails, changing nothing, when the
/// RAW's frames or lens correction cannot be read (never a new key over
/// coordinates that were not moved).
pub fn migrated(source: &Path, adjustments: &Value, to: u32) -> Result<Migrated, String> {
    if rawtonic_engine::image_processing::is_new_recipe(adjustments) {
        return Ok(Migrated {
            adjustments: adjustments.clone(),
            hidden_patches: 0,
        });
    }
    let source_str = source.to_string_lossy();
    let unreadable = || format!("The frames of {source_str} cannot be read");
    let from = requested(adjustments);
    let frames = rawtonic_engine::source_frame::frames(&source_str).ok_or_else(unreadable)?;
    let frame_of = |v: u32| frames.get(v as usize).or(frames.last()).copied();
    if frame_of(from) == frame_of(to) {
        return Ok(migrate(adjustments, None, adjustments, to));
    }
    let change = change(&source_str, from, to).ok_or_else(unreadable)?;
    let mut warp = adjustments.clone();
    rawtonic_export::exif_processing::prepare_recipe_lens(&mut warp, source)?;
    Ok(migrate(adjustments, Some(&change), &warp, to))
}

/// After the frame of the photo at `path` changed: the stored previews of
/// its RAW stand for its master recipe's decode, so when that is the one
/// updated they show the old frame and go (the photo shows the new one once
/// it is decoded; offline it is not shown on the wrong frame). A virtual
/// copy's update leaves its master's previews.
pub fn forget_previews_of(path: &str) {
    if path.contains("?vc=") {
        return;
    }
    let (source, _) = parse_virtual_path(path);
    crate::preview_proxy::discard_source(
        &source,
        &[
            crate::preview_proxy::ProxyKind::Standard,
            crate::preview_proxy::ProxyKind::Full,
        ],
    );
}

/// What "Update frame" would do to one photo.
#[derive(serde::Serialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FrameStatus {
    pub path: String,
    /// Its frame changes (the newest frame is another than its recipe's).
    pub changes: bool,
    /// Its developed size now and after the update (oriented).
    pub from: Option<(u32, u32)>,
    pub to: Option<(u32, u32)>,
    /// Visible AI patches that would be hidden for regeneration.
    pub hidden_patches: usize,
}

/// A photo asked about: its path (a virtual copy's too) and, for the open
/// photo, the editor's recipe (else its saved one is read).
#[derive(serde::Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct FrameQuery {
    pub path: String,
    #[serde(default)]
    pub adjustments: Option<Value>,
}

/// The status of one photo with recipe `adjustments`.
pub fn status(path: &str, adjustments: &Value) -> FrameStatus {
    let (source, _) = parse_virtual_path(path);
    let source_str = source.to_string_lossy();
    let change = (crate::formats::is_raw_file(&source)
        && !rawtonic_engine::image_processing::is_new_recipe(adjustments))
    .then(|| change(&source_str, requested(adjustments), VERSION))
    .flatten();
    let hidden_patches = change.as_ref().map_or(0, |change| {
        let (dx, dy) = change.offset;
        let fits = change.only_grows() && dx.fract() == 0.0 && dy.fract() == 0.0;
        if fits {
            return 0;
        }
        adjustments["aiPatches"].as_array().map_or(0, |patches| {
            patches
                .iter()
                .filter(|p| p["visible"].as_bool().unwrap_or(true) && p["patchData"].is_object())
                .count()
        })
    });
    FrameStatus {
        path: path.to_string(),
        changes: change.is_some(),
        from: change.map(|c| c.from),
        to: change.map(|c| c.to),
        hidden_patches,
    }
}

/// Which of `items` "Update frame" would change, and how many of their AI
/// patches it would hide (Develop's lens tool note, the batch command's
/// confirmation).
#[tauri::command]
pub async fn source_frame_status(items: Vec<FrameQuery>) -> Result<Vec<FrameStatus>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        use rayon::prelude::*;
        items
            .par_iter()
            .map(|item| {
                let adjustments = item.adjustments.clone().unwrap_or_else(|| {
                    let (_, sidecar) = parse_virtual_path(&item.path);
                    crate::exif_processing::load_sidecar(&sidecar).adjustments
                });
                status(&item.path, &adjustments)
            })
            .collect()
    })
    .await
    .map_err(|e| e.to_string())
}

/// The editor's recipe of the open photo, moved to the newest frame (or
/// `to`), for one history step (Update frame, Reset).
#[derive(serde::Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct MigratedRecipe {
    pub adjustments: Value,
    pub hidden_patches: usize,
}

#[tauri::command]
pub async fn migrate_source_frame(
    path: String,
    adjustments: Value,
    to: Option<u32>,
) -> Result<MigratedRecipe, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let (source, _) = parse_virtual_path(&path);
        let moved = migrated(&source, &adjustments, to.unwrap_or(VERSION).min(VERSION))?;
        Ok(MigratedRecipe {
            adjustments: moved.adjustments,
            hidden_patches: moved.hidden_patches,
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A frame that cannot be read is never taken for an unchanged one: the
    /// recipe is not moved and keeps its key.
    #[test]
    fn an_unreadable_raw_is_not_migrated() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("A.CR3");
        assert!(migrated(&missing, &json!({"exposure": 1}), VERSION).is_err());
        let garbage = dir.path().join("B.CR3");
        std::fs::write(&garbage, b"not a raw").unwrap();
        assert!(migrated(&garbage, &json!({"exposure": 1}), VERSION).is_err());
        // Files no frame version changes need not be read: only the key.
        let moved = migrated(&dir.path().join("C.NEF"), &json!({"exposure": 1}), VERSION).unwrap();
        assert_eq!(moved.adjustments, json!({"exposure": 1, KEY: VERSION}));
        // A never-edited recipe already develops the newest frame.
        assert_eq!(
            migrated(&missing, &json!({}), VERSION).unwrap().adjustments,
            json!({})
        );
    }
}
