//! The frame a recipe develops of its RAW (`sourceFrameVersion`, ADR 0020;
//! the frames themselves: `rawtonic_rawdecode::source_frame`).
//!
//! The key belongs to the photo, like its spots, and a photo never changes
//! its frame once it is edited (decision 2026-10-30, "new photos only"): a
//! never-edited photo develops the newest frame and its first edit writes
//! that version (`image_processing::apply_render_default` and the editor's
//! new edits); Reset, undo, paste, presets and sync keep the photo's own. A
//! recipe without the key develops the frame it always did, so its exports
//! stay byte-identical.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use serde_json::Value;

pub use rawtonic_rawdecode::source_frame::VERSION;

/// The recipe key.
pub const KEY: &str = "sourceFrameVersion";

/// The frame version a recipe asks for: its own (versions this build does
/// not know read as its newest; a Reset keeps it, `reset_recipe`), else the
/// newest for a new recipe (`image_processing::is_new_recipe`: it renders
/// as a new edit), 0 for an edit from before the key.
pub fn requested(adjustments: &Value) -> u32 {
    match adjustments.get(KEY).and_then(Value::as_u64) {
        Some(version) => version.min(u64::from(VERSION)) as u32,
        None if crate::image_processing::is_new_recipe(adjustments) => VERSION,
        None => 0,
    }
}

/// The recipe a Reset of a photograph with recipe `adjustments` saves: a
/// new edit (`{}`), which keeps the photograph's frame once it has an edit
/// (a key only, which `is_new_recipe` ignores), so a Reset never changes
/// the photograph's size.
pub fn reset_recipe(adjustments: &Value) -> Value {
    if adjustments.get(KEY).is_none() && crate::image_processing::is_new_recipe(adjustments) {
        return serde_json::json!({});
    }
    serde_json::json!({ KEY: requested(adjustments) })
}

/// For each frame version up to the newest (the index), the oldest one
/// that develops the RAW at `path` the same (`rawdecode::source_frame::effective`):
/// all 0 for files no frame version changes. From the file (read once per
/// size and modification time), else, offline, from what its previews keep
/// (`source_facts`); None when neither knows.
pub fn frames(path: &str) -> Option<Vec<u32>> {
    let extension = std::path::Path::new(path)
        .extension()
        .map(|e| e.to_string_lossy().into_owned())
        .unwrap_or_default();
    if !crate::formats::is_raw_file(path)
        || !rawtonic_rawdecode::source_frame::may_change(&extension)
    {
        return Some(vec![0; VERSION as usize + 1]);
    }
    // Offline: what its previews keep (nothing is registered while the
    // file can be read).
    if let Some(frames) = crate::source_facts::registered(path)
        .and_then(|facts| facts.frames.clone())
        .filter(|f| !f.is_empty())
    {
        return Some(frames);
    }
    type Cache = Mutex<HashMap<String, (u64, std::time::SystemTime, Vec<u32>)>>;
    static CACHE: OnceLock<Cache> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    let stamp = std::fs::metadata(path)
        .ok()
        .and_then(|m| Some((m.len(), m.modified().ok()?)));
    if let Some((len, modified)) = stamp {
        if let Some((l, m, found)) = cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(path)
            .cloned()
            && l == len
            && m == modified
        {
            return Some(found);
        }
        if let Some(found) = rawler::rawsource::RawSource::new(std::path::Path::new(path))
            .ok()
            .filter(|source| rawtonic_rawdecode::source_frame::frame(source, 0).is_some())
            .map(|source| {
                (0..=VERSION)
                    .map(|v| rawtonic_rawdecode::source_frame::effective(&source, v))
                    .collect::<Vec<u32>>()
            })
        {
            cache
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(path.to_string(), (len, modified, found.clone()));
            return Some(found);
        }
    }
    None
}

/// The frame version that develops the RAW at `path` as `version` does
/// (see `frames`): the frame part of a decode variant, so the bases of two
/// versions that frame a file alike are shared. `version` itself when the
/// file and its previews tell nothing (an offline photograph from before
/// frames were recorded).
pub fn effective(path: &str, version: u32) -> u32 {
    if version == 0 {
        return 0;
    }
    frames(path).map_or(version, |frames| effective_in(&frames, version))
}

/// `effective` from a file's `frames` list (`frames`, or what a preview's
/// facts keep of them).
pub fn effective_in(frames: &[u32], version: u32) -> u32 {
    frames
        .get(version as usize)
        .or(frames.last())
        .copied()
        .unwrap_or(version)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_requested_version() {
        assert_eq!(requested(&json!({})), VERSION);
        assert_eq!(requested(&Value::Null), VERSION);
        assert_eq!(requested(&json!({"exposure": 1.0})), 0);
        assert_eq!(requested(&json!({"exposure": 1.0, KEY: 1})), 1);
        assert_eq!(requested(&json!({"exposure": 1.0, KEY: 99})), VERSION);
        // A reset edit keeps its frame and still renders as a new edit.
        assert_eq!(requested(&json!({KEY: 0})), 0);
        assert!(crate::image_processing::is_new_recipe(&json!({KEY: 0})));
        // Files no frame version can change develop version 0's frame.
        assert_eq!(effective("/p/A.NEF", 1), 0);
        assert_eq!(effective("/p/A.jpg", 1), 0);
        // A preview's facts tell an offline file's frames.
        assert_eq!(effective_in(&[0, 0], 1), 0);
        assert_eq!(effective_in(&[0, 1], 1), 1);
        assert_eq!(effective_in(&[0, 1], 0), 0);
    }

    /// A Reset keeps the frame of an edited photograph (decision
    /// 2026-10-30): never the newest frame over the size it was edited in.
    #[test]
    fn a_reset_keeps_the_frame() {
        // An edit from before the key: frame 0, kept through two Resets.
        let once = reset_recipe(&json!({"exposure": 1.0}));
        assert_eq!(once, json!({KEY: 0}));
        assert_eq!(reset_recipe(&once), once);
        assert_eq!(requested(&once), 0);
        assert_eq!(reset_recipe(&json!({"exposure": 1.0, KEY: 1})), json!({KEY: 1}));
        // A never-edited photograph stays one (the newest frame).
        assert_eq!(reset_recipe(&json!({})), json!({}));
        assert_eq!(reset_recipe(&Value::Null), json!({}));
    }
}
