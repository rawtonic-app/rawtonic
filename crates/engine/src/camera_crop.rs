//! The aspect ratio set in the camera as the crop new edits start with
//! (ADR 0020, option C; the frame itself: `rawtonic_rawdecode::camera_crop`).
//!
//! A RAW that keeps the whole sensor although the camera was set to another
//! aspect ratio (Samsung EX2F 16:9, Canon 1:1 / 4:3 / 16:9 / 4:5, Fujifilm
//! 16:9 / 1:1 / 5:4) opens
//! cropped to the camera's frame, as in Lightroom and Camera Raw: an
//! ordinary crop the photographer can widen or reset. It belongs to new
//! recipes only (`image_processing::is_new_recipe`: never edited, or a
//! Reset): a never-edited photo renders with it, its first edit writes it
//! into the recipe, a Reset starts from it again; an edited recipe keeps
//! its own crop (none included), so its exports stay byte-identical.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use rawtonic_recipe::geometry;
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
        return offline(&facts, &crate::source_frame::extension_of(path));
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
    // Mapped without read-ahead: the crop tags are a few kilobytes, and
    // `RawSource::new` would read the whole file (on a share, every byte).
    let Ok(source) = rawler::rawsource::RawSource::new_lazy(std::path::Path::new(path)) else {
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
fn offline(facts: &crate::source_facts::SourceFacts, extension: &str) -> Vec<Option<CameraCrop>> {
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
            (crate::source_frame::effective_in(frames, version, extension)
                == crate::source_frame::effective_in(frames, newest, extension))
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

/// Starts a new recipe (`is_new_recipe`) of the photo at `source` as a new
/// edit: every version the editor's new edit starts with
/// (`image_processing::start_new_edit`), then the camera's crop of the
/// frame it develops (`source_frame::requested`). Both together, since the
/// crop alone would make the recipe an edit and its versions would be
/// skipped. Every batch edit of a never-edited photo starts here, so it
/// gets the edit the photo opened first gets. Saved edits keep their own
/// crop (and versions); files without another camera frame get only the
/// versions. Then the user's develop defaults for the photograph's camera
/// (`develop_defaults`), unless the recipe has its defaults decided.
pub fn apply_default(adjustments: &mut Value, source: &std::path::Path) {
    if !crate::image_processing::is_new_recipe(adjustments) {
        return;
    }
    let follows = crate::develop_defaults::follows_defaults(adjustments);
    let is_raw = crate::formats::is_raw_file(source);
    start_new_edit(adjustments, &crops(&source.to_string_lossy()), is_raw);
    // The user's defaults for its camera (none set: nothing read).
    if follows {
        crate::develop_defaults::apply_current(adjustments, is_raw, || {
            crate::develop_defaults::camera_of(source)
        });
    }
}

/// The camera's crops (`crops`) the cached `facts` of the photograph at
/// `path` (kept with its previews, `source_facts`) tell, None when they do
/// not say (previews from before facts version 15). Never reads the file.
pub fn crops_from_facts(
    facts: &crate::source_facts::SourceFacts,
    path: &str,
) -> Option<Vec<Option<CameraCrop>>> {
    let extension = crate::source_frame::extension_of(path);
    // Facts from before a RAF's aspect ratio was read (version 23) kept an
    // empty crop for every RAF: not an answer.
    if extension.eq_ignore_ascii_case("raf")
        && facts.version < crate::source_facts::RAF_CROP_VERSION
    {
        return None;
    }
    facts
        .camera_crop
        .as_ref()
        .map(|_| offline(facts, &extension))
}

/// `apply_default` for an original that must not be read (a cloud file not
/// downloaded: opening it downloads it): the camera's crop from `crops`
/// (`crops_from_facts`; None: not cached), the user's develop defaults for
/// `camera` (from cached EXIF). Without a cached crop the edit starts with
/// the versions only; returns whether the crop was known, so that the
/// caller can tell the photographer it is missing.
pub fn apply_default_cached(
    adjustments: &mut Value,
    source: &std::path::Path,
    crops: Option<Vec<Option<CameraCrop>>>,
    camera: Option<crate::develop_defaults::CameraIdentity>,
) -> bool {
    if !crate::image_processing::is_new_recipe(adjustments) {
        return true;
    }
    let known = crops.is_some();
    let follows = crate::develop_defaults::follows_defaults(adjustments);
    let is_raw = crate::formats::is_raw_file(source);
    start_new_edit(adjustments, &crops.unwrap_or_default(), is_raw);
    if follows {
        crate::develop_defaults::apply_current(adjustments, is_raw, || camera);
    }
    known || !is_raw
}

/// Starts a new edit from already-read RAW bytes, without reopening its path.
pub fn apply_default_from_source(adjustments: &mut Value, source: &rawler::rawsource::RawSource) {
    if !crate::image_processing::is_new_recipe(adjustments) {
        return;
    }
    let follows = crate::develop_defaults::follows_defaults(adjustments);
    let crops: Vec<_> = (0..=crate::source_frame::VERSION)
        .map(|version| rawtonic_rawdecode::camera_crop::camera_crop(source, version))
        .collect();
    start_new_edit(adjustments, &crops, true);
    if follows {
        crate::develop_defaults::apply_current(adjustments, true, || {
            crate::develop_defaults::camera_of_raw(source)
        });
    }
}

/// The part of the developed frame the camera's own image (a RAW's
/// embedded JPEG, `image` its width and height either way up, None when
/// unknown) shows, for the recipe `adjustments` of the photo at `path`:
/// Some(the camera's crop in the frame version the recipe develops) where
/// the image has its shape (`shows_crop`: Canon, EX2F), Some(None) where it
/// is the whole frame (a Fujifilm RAF's JPEG, a file without another
/// camera frame, files other than RAWs). None when there is a crop but the
/// image's size is unknown (an offline photograph): either could be.
pub fn camera_image_region(
    path: &str,
    adjustments: &Value,
    image: Option<(u32, u32)>,
) -> Option<Option<CameraCrop>> {
    let version = crate::source_frame::requested(adjustments) as usize;
    match crops(path).get(version).copied().flatten() {
        None => Some(None),
        Some(crop) => {
            let (width, height) = image?;
            Some(shows_crop(crop, width, height).then_some(crop))
        }
    }
}

pub use rawtonic_rawdecode::camera_crop::shows_crop;

/// The crop a new recipe (`is_new_recipe`) starts with, from the file's
/// `crops` (`crops`): the one of the frame the recipe asks for
/// (`source_frame::requested`). The single rule of every renderer, the GPU
/// path through `apply_default` and the thumbnails without a GPU.
pub fn initial_crop(adjustments: &Value, crops: &[Option<CameraCrop>]) -> Option<CameraCrop> {
    if !crate::image_processing::is_new_recipe(adjustments) {
        return None;
    }
    let version = crate::source_frame::requested(adjustments) as usize;
    crops.get(version).copied().flatten()
}

/// `apply_default` with the file's `crops` (`crops`).
fn start_new_edit(adjustments: &mut Value, crops: &[Option<CameraCrop>], is_raw: bool) {
    if !crate::image_processing::is_new_recipe(adjustments) {
        return;
    }
    let crop = initial_crop(adjustments, crops);
    crate::image_processing::start_new_edit(adjustments, is_raw);
    let Some(crop) = crop else {
        return;
    };
    if let Some(map) = adjustments.as_object_mut() {
        let (crop, aspect) = recipe_values(crop);
        map.insert(geometry::CROP.name().to_string(), crop);
        map.insert(geometry::ASPECT_RATIO.name().to_string(), aspect);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_camera_image_shows_the_crop_only_where_it_has_its_shape() {
        let path = "/offline/camera-image-region.RAF";
        crate::source_facts::register(
            path,
            crate::source_facts::SourceFacts {
                version: crate::source_facts::VERSION,
                frames: Some(vec![0; crate::source_frame::VERSION as usize + 1]),
                camera_crop: Some(vec![0, 312, 6000, 3375]),
                ..Default::default()
            },
        );
        let crop = CameraCrop {
            x: 0,
            y: 312,
            width: 6000,
            height: 3375,
        };
        let region = |image| camera_image_region(path, &json!({}), image);
        // A 16:9 camera JPEG (Canon, EX2F) shows the crop; a 3:2 one (a
        // Fujifilm RAF's) the whole frame; unknown: either could be.
        assert_eq!(region(Some((1920, 1080))), Some(Some(crop)));
        assert_eq!(region(Some((1920, 1280))), Some(None));
        assert_eq!(region(None), None);
        // A file without another camera frame: the whole frame either way.
        assert_eq!(
            camera_image_region("/offline/none.RAF", &json!({}), None),
            Some(None)
        );
    }

    #[test]
    fn only_new_recipes_get_the_camera_crop() {
        let crop = CameraCrop {
            x: 0,
            y: 376,
            width: 4000,
            height: 2248,
        };
        // A new recipe becomes a new edit with the crop: its versions are
        // set too (the crop alone would make it an edit without them).
        for new in [json!({}), Value::Null] {
            let mut recipe = new;
            start_new_edit(&mut recipe, &[Some(crop); 4], true);
            assert_eq!(
                recipe["renderVersion"],
                json!(crate::image_processing::RENDER_VERSION)
            );
            assert_eq!(
                recipe[crate::source_frame::KEY],
                json!(crate::source_frame::VERSION)
            );
            assert!(recipe["sharpenVersion"].is_u64());
            assert_eq!(recipe["crop"], recipe_values(crop).0);
            assert_eq!(recipe["aspectRatio"], json!(4000.0 / 2248.0));
        }
        // A Reset's recipe keeps its frame and takes that frame's crop.
        let mut reset = json!({crate::source_frame::KEY: 0});
        start_new_edit(
            &mut reset,
            &[None, Some(crop), Some(crop), Some(crop)],
            true,
        );
        assert_eq!(reset[crate::source_frame::KEY], json!(0));
        assert!(reset.get("crop").is_none());
        assert_eq!(
            reset["renderVersion"],
            json!(crate::image_processing::RENDER_VERSION)
        );
        // An edit keeps its own crop (none) and versions.
        let mut edited = json!({"exposure": 1.0});
        start_new_edit(&mut edited, &[Some(crop); 4], true);
        assert_eq!(edited, json!({"exposure": 1.0}));
        // Not a RAW: only the versions, the file is never read.
        let mut other = json!({});
        apply_default(&mut other, std::path::Path::new("/p/A.jpg"));
        assert!(other.get("crop").is_none());
        assert!(other["renderVersion"].is_u64());
        assert_eq!(
            recipe_values(crop),
            (
                json!({"unit": "px", "x": 0, "y": 376, "width": 4000, "height": 2248}),
                json!(4000.0 / 2248.0)
            )
        );
    }

    /// Every batch writer (paste, preset, style, Auto, spots, set values)
    /// starts a never-edited RAW here: with every version the editor's new
    /// edit saves (its pinned recipe), the camera's lens versions aside
    /// (they need its data); a non-RAW without the RAW-only ones.
    #[test]
    fn a_new_edit_starts_with_every_version_of_the_editors_new_edit() {
        let new_edit: Value =
            serde_json::from_str(include_str!("../tests/fixtures/new-edit-recipe.raw.json"))
                .unwrap();
        let mut raw = json!({});
        apply_default(&mut raw, std::path::Path::new("/offline/never-opened.NEF"));
        for (key, value) in new_edit.as_object().unwrap() {
            if key.ends_with("Version")
                && key != "cameraCaVersion"
                && key != "cameraGeometryVersion"
            {
                assert_eq!(raw.get(key), Some(value), "{key}");
            }
        }
        let mut other = json!({});
        apply_default(&mut other, std::path::Path::new("/offline/scan.png"));
        assert_eq!(other["toneVersion"], new_edit["toneVersion"]);
        for key in ["aiDenoiseVersion", "fringeVersion", "autoCaVersion"] {
            assert!(other.get(key).is_none(), "{key}");
        }
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
        // Frame 0 differs from the newest: its crop is not known (facts
        // from before frame version 2 list two frames; the newest reads as
        // their last).
        assert_eq!(offline(&facts, "CR3"), vec![None, crop, crop, crop]);
        // An ARW's list from before version 2 does not tell its newest
        // frame: only that frame gets the crop.
        facts.frames = Some(vec![0, 0]);
        assert_eq!(offline(&facts, "ARW"), vec![None, None, None, crop]);
        facts.frames = Some(vec![0, 1]);
        facts.frames = Some(vec![0, 0, 0]);
        assert_eq!(offline(&facts, "CR3"), vec![crop; 4]);
        facts.frames = Some(vec![0, 0, 2]);
        assert_eq!(offline(&facts, "CR3"), vec![None, None, crop, crop]);
        // Kept as none: none for every frame.
        facts.camera_crop = Some(Vec::new());
        assert_eq!(offline(&facts, "CR3"), vec![None; 4]);
        // Facts from before the crop was kept tell nothing.
        facts.camera_crop = None;
        assert_eq!(offline(&facts, "CR3"), Vec::new());
    }

    #[test]
    fn facts_from_before_a_rafs_crop_was_read_are_not_an_answer() {
        let facts = |version| crate::source_facts::SourceFacts {
            version,
            camera_crop: Some(Vec::new()),
            ..Default::default()
        };
        assert!(crops_from_facts(&facts(22), "/p/a.RAF").is_none());
        assert!(
            crops_from_facts(&facts(crate::source_facts::RAF_CROP_VERSION), "/p/a.RAF").is_some()
        );
        // Other makes kept their crop from version 15 on.
        assert!(crops_from_facts(&facts(22), "/p/a.CR3").is_some());
    }
}
