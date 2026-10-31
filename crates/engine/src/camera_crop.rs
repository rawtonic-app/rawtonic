//! The aspect ratio set in the camera as the crop new edits start with
//! (ADR 0020, option C; the frame itself: `rawtonic_rawdecode::camera_crop`).
//!
//! A RAW that keeps the whole sensor although the camera was set to another
//! aspect ratio (Samsung EX2F 16:9, Canon 1:1 / 4:3 / 16:9 / 4:5) opens
//! cropped to the camera's frame, as in Lightroom and Camera Raw: an
//! ordinary crop the photographer can widen or reset. It belongs to new
//! recipes only (`image_processing::is_new_recipe`: never edited, or a
//! Reset): a never-edited photo renders with it, its first edit writes it
//! into the recipe, a Reset starts from it again; an edited recipe keeps
//! its own crop (none included), so its exports stay byte-identical.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use serde_json::Value;

pub use rawtonic_rawdecode::camera_crop::CameraCrop;

/// For each frame version up to the newest (the index), the camera's crop
/// of the RAW at `path` in that frame (`rawdecode::camera_crop`): from the
/// file (read once per size and modification time), else, offline, from
/// what its previews keep (`source_facts`, newest frame only). Empty when
/// neither knows; None entries where the camera set no other aspect ratio.
pub fn crops(path: &str) -> Vec<Option<CameraCrop>> {
    let versions = crate::source_frame::VERSION as usize + 1;
    if !crate::formats::is_raw_file(path) {
        return vec![None; versions];
    }
    if let Some(facts) = crate::source_facts::registered(path) {
        return offline(&facts);
    }
    type Cache = Mutex<HashMap<String, (u64, std::time::SystemTime, Vec<Option<CameraCrop>>)>>;
    static CACHE: OnceLock<Cache> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    let Some((len, modified)) = std::fs::metadata(path)
        .ok()
        .and_then(|m| Some((m.len(), m.modified().ok()?)))
    else {
        return Vec::new();
    };
    if let Some((l, m, found)) = cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(path)
        .cloned()
        && l == len
        && m == modified
    {
        return found;
    }
    let Ok(source) = rawler::rawsource::RawSource::new(std::path::Path::new(path)) else {
        return Vec::new();
    };
    let found: Vec<_> = (0..=crate::source_frame::VERSION)
        .map(|version| rawtonic_rawdecode::camera_crop::camera_crop(&source, version))
        .collect();
    let mut cache = cache.lock().unwrap_or_else(|e| e.into_inner());
    if cache.len() > 4096 {
        cache.clear();
    }
    cache.insert(path.to_string(), (len, modified, found.clone()));
    found
}

/// `crops` from an offline photograph's facts: the newest frame's crop for
/// every version that frames the file as the newest does.
fn offline(facts: &crate::source_facts::SourceFacts) -> Vec<Option<CameraCrop>> {
    let Some(kept) = facts.camera_crop.as_deref() else {
        return Vec::new();
    };
    let newest = crate::source_frame::VERSION;
    let frames = facts.frames.as_deref().unwrap_or(&[]);
    let crop = match *kept {
        [x, y, width, height] => Some(CameraCrop {
            x,
            y,
            width,
            height,
        }),
        _ => None,
    };
    (0..=newest)
        .map(|version| {
            (crate::source_frame::effective_in(frames, version)
                == crate::source_frame::effective_in(frames, newest))
            .then_some(crop)
            .flatten()
        })
        .collect()
}

/// The camera's crop as recipe values: `crop` in the developed frame's
/// pixels and `aspectRatio` locked to it (Lightroom's "As Shot").
pub fn recipe_values(crop: CameraCrop) -> (Value, Value) {
    (
        serde_json::json!({
            "unit": "px",
            "x": crop.x,
            "y": crop.y,
            "width": crop.width,
            "height": crop.height,
        }),
        serde_json::json!(crop.width as f64 / crop.height as f64),
    )
}

/// Puts the camera's crop into a new recipe (`is_new_recipe`, before
/// `apply_render_default` makes it an edit) of the RAW at `source`, for the
/// frame the recipe develops (`source_frame::requested`). Saved edits keep
/// their own crop; files without another camera frame are left alone.
pub fn apply_default(adjustments: &mut Value, source: &std::path::Path) {
    if !crate::image_processing::is_new_recipe(adjustments) {
        return;
    }
    let path = source.to_string_lossy();
    let version = crate::source_frame::requested(adjustments) as usize;
    let Some(crop) = crops(&path).get(version).copied().flatten() else {
        return;
    };
    if adjustments.is_null() {
        *adjustments = serde_json::json!({});
    }
    if let Some(map) = adjustments.as_object_mut() {
        let (crop, aspect) = recipe_values(crop);
        map.insert("crop".to_string(), crop);
        map.insert("aspectRatio".to_string(), aspect);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_new_recipes_get_the_camera_crop() {
        // Not a RAW: nothing to read, nothing changes.
        let mut recipe = json!({});
        apply_default(&mut recipe, std::path::Path::new("/p/A.jpg"));
        assert_eq!(recipe, json!({}));
        // An edit keeps its own crop (none): never read.
        let mut edited = json!({"exposure": 1.0});
        apply_default(&mut edited, std::path::Path::new("/missing/A.SRW"));
        assert_eq!(edited, json!({"exposure": 1.0}));
        assert_eq!(
            recipe_values(CameraCrop {
                x: 0,
                y: 376,
                width: 4000,
                height: 2248
            }),
            (
                json!({"unit": "px", "x": 0, "y": 376, "width": 4000, "height": 2248}),
                json!(4000.0 / 2248.0)
            )
        );
    }

    #[test]
    fn offline_crops_come_from_the_newest_frame() {
        let mut facts = crate::source_facts::SourceFacts {
            version: crate::source_facts::VERSION,
            frames: Some(vec![0, 1]),
            camera_crop: Some(vec![0, 376, 4000, 2248]),
            ..Default::default()
        };
        let crop = Some(CameraCrop {
            x: 0,
            y: 376,
            width: 4000,
            height: 2248,
        });
        // Frame 0 differs from the newest: its crop is not known.
        assert_eq!(offline(&facts), vec![None, crop]);
        facts.frames = Some(vec![0, 0]);
        assert_eq!(offline(&facts), vec![crop, crop]);
        // Kept as none: none for every frame.
        facts.camera_crop = Some(Vec::new());
        assert_eq!(offline(&facts), vec![None, None]);
        // Facts from before the crop was kept tell nothing.
        facts.camera_crop = None;
        assert_eq!(offline(&facts), Vec::new());
    }
}
