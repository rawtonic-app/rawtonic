//! One entry that makes a recipe renderable (plan 2.1, notes:
//! plans/architecture-cleanup-2026-11.md): every render of a photograph —
//! export, CLI, multi-view, collage, thumbnails, the editor's previews —
//! calls [`normalize`] once instead of the defaulting steps one by one.
//!
//! The steps and their order are those the callers applied one by one
//! (the render-input snapshots and exports stayed byte-identical), plus the
//! new edit's tool responses of step 3 (2026-11-04):
//! 1. bitmaps the recipe refers to by id restored (`hydrate_adjustments_from_cache`);
//! 2. lens parameters saved while the photograph could not be read
//!    resolved (`exif_processing::resolve_unresolved_lens`);
//! 3. a new recipe started as a new edit: render, sharpening and frame
//!    versions and the camera's crop (`camera_crop::apply_default`), and,
//!    after step 4, the tool responses of the editor's new edit
//!    (`image_processing::new_edit_responses`), so a never-edited or reset
//!    photo renders as it does opened in the editor;
//! 4. the camera's lens correction and the new edit's fringe and lateral
//!    CA passes for a recipe without a lens mode (`embedded_lens::apply_default`);
//! 5. the photograph's orientation in Lightroom retouches imported before
//!    imports recorded it (`exif_processing::record_lightroom_orientation`).
//!
//! Steps 2–5 are `exif_processing::prepare_recipe_lens`. Not every caller
//! applies every step today ([`Defaults`]): those differences are kept, not
//! unified, so nothing renders differently (notes: the PR of plan 2.1 lists
//! them).

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use rawtonic_engine::recipe_versions::RecipeVersions;
use serde_json::Value;

/// The photograph a recipe renders.
#[derive(Clone, Copy, Debug)]
pub struct RecipeSource<'a> {
    path: Option<&'a Path>,
    is_raw: Option<bool>,
}

impl<'a> RecipeSource<'a> {
    /// The photograph at `path` (its source file, not a virtual copy path).
    pub fn file(path: &'a Path) -> Self {
        Self {
            path: Some(path),
            is_raw: Some(rawtonic_engine::formats::is_raw_file(path)),
        }
    }

    /// The photograph open in the editor, whose recipes the editor has
    /// completed itself ([`Defaults::None`]): neither its file nor whether
    /// it is a RAW are needed (nor read, so no lock of the open image is
    /// taken), and no versions are read.
    pub fn open_in_editor() -> Self {
        Self {
            path: None,
            is_raw: None,
        }
    }
}

/// Where the bitmaps a recipe refers to by id come from (step 1).
pub enum Bitmaps<'a> {
    /// Not restored.
    None,
    /// Only from the recipe itself (a bitmap it carries once, referred to
    /// again later in it): exports of stored recipes.
    Own,
    /// From the app's cache of the bitmaps the editor sent before, which
    /// also remembers the ones this recipe carries (locked for this step
    /// only).
    Shared(&'a Mutex<HashMap<String, Value>>),
}

/// Which camera and lens defaults (steps 2–5) a caller applies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Defaults {
    /// All of them (`prepare_recipe_lens`): every render of a stored recipe.
    Render,
    /// Steps 3 and 4 only, without resolving a lens saved offline (step 2)
    /// and without the Lightroom orientation (step 5): the thumbnails of
    /// never-edited RAWs.
    NewEdit,
    /// None: the editor's own recipes, which it completes itself.
    None,
}

/// A recipe ready to render: the recipe itself is completed in place.
#[derive(Debug)]
pub struct Normalized {
    /// Its versions, read once; None for [`RecipeSource::open_in_editor`],
    /// whose RAW-ness (the colour noise default) is not known here.
    pub versions: Option<RecipeVersions>,
    /// Step 2's outcome: an error when a lens saved offline could not be
    /// resolved (the other steps ran all the same). Callers that write
    /// images refuse to render without it, previews and estimates go on.
    pub lens: Result<(), String>,
}

/// Completes `recipe` for rendering `source` (see the module): `bitmaps`
/// for step 1, `defaults` for steps 2–5. Defaults other than
/// [`Defaults::None`] need [`RecipeSource::file`].
pub fn normalize(
    recipe: &mut Value,
    source: &RecipeSource,
    bitmaps: Bitmaps,
    defaults: Defaults,
) -> Normalized {
    match bitmaps {
        Bitmaps::None => {}
        Bitmaps::Own => {
            rawtonic_engine::adjustment_utils::hydrate_adjustments_from_cache(
                &mut HashMap::new(),
                recipe,
            );
        }
        Bitmaps::Shared(cache) => {
            let mut cache = cache.lock().unwrap();
            rawtonic_engine::adjustment_utils::hydrate_adjustments_from_cache(&mut cache, recipe);
        }
    }
    // A new recipe (none, or a Library Reset's) renders as the editor's
    // new edit of the photograph: after its defaults (which need it new),
    // the tool responses a new edit starts with.
    let new_edit = defaults != Defaults::None
        && source.path.is_some()
        && rawtonic_engine::image_processing::is_new_recipe(recipe);
    let lens = match (defaults, source.path) {
        (Defaults::None, _) => Ok(()),
        (Defaults::Render, Some(path)) => crate::exif_processing::prepare_recipe_lens(recipe, path),
        (Defaults::NewEdit, Some(path)) => {
            rawtonic_engine::camera_crop::apply_default(recipe, path);
            rawtonic_engine::embedded_lens::apply_default(recipe, path);
            Ok(())
        }
        (_, None) => Err("The camera defaults need the photograph's file.".to_string()),
    };
    if new_edit {
        start_new_edit_responses(recipe, source.is_raw.unwrap_or(false));
    }
    Normalized {
        versions: source
            .is_raw
            .map(|is_raw| RecipeVersions::of(recipe, is_raw)),
        lens,
    }
}

/// Step 3's tool responses (`image_processing::new_edit_responses`) on a
/// recipe that was new before its defaults; the passes the defaults set
/// (none of these keys) stay.
fn start_new_edit_responses(recipe: &mut Value, is_raw: bool) {
    if recipe.is_null() {
        *recipe = Value::Object(Default::default());
    }
    if let Some(map) = recipe.as_object_mut() {
        for (key, value) in rawtonic_engine::image_processing::new_edit_responses(is_raw) {
            map.entry(key).or_insert(value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The steps one by one, as the callers applied them before `normalize`.
    fn by_steps(recipe: &Value, path: &Path, defaults: Defaults) -> Value {
        let mut recipe = recipe.clone();
        rawtonic_engine::adjustment_utils::hydrate_adjustments_from_cache(
            &mut HashMap::new(),
            &mut recipe,
        );
        let new = rawtonic_engine::image_processing::is_new_recipe(&recipe);
        match defaults {
            Defaults::Render => {
                let _ = crate::exif_processing::prepare_recipe_lens(&mut recipe, path);
            }
            Defaults::NewEdit => {
                rawtonic_engine::camera_crop::apply_default(&mut recipe, path);
                rawtonic_engine::embedded_lens::apply_default(&mut recipe, path);
            }
            Defaults::None => {}
        }
        if new && defaults != Defaults::None {
            start_new_edit_responses(&mut recipe, rawtonic_engine::formats::is_raw_file(path));
        }
        recipe
    }

    #[test]
    fn normalize_applies_the_steps_each_caller_applied() {
        let recipes = [
            Value::Null,
            json!({}),
            json!({"exposure": 0.5}),
            json!({"lensCorrectionMode": "manual", "renderVersion": 3}),
            json!({"masks": [{"subMasks": [{"id": "a", "parameters": {"maskDataBase64": "x"}}]}]}),
        ];
        let paths = [
            "/offline/IMG_1.CR3",
            "/offline/scan.png",
            "/offline/IMG_2.ARW",
        ];
        for recipe in &recipes {
            for path in paths.map(Path::new) {
                for defaults in [Defaults::Render, Defaults::NewEdit, Defaults::None] {
                    let mut normalized = recipe.clone();
                    let source = RecipeSource::file(path);
                    let result = normalize(&mut normalized, &source, Bitmaps::Own, defaults);
                    assert_eq!(
                        normalized,
                        by_steps(recipe, path, defaults),
                        "{recipe} {path:?}"
                    );
                    assert_eq!(
                        result.versions,
                        source
                            .is_raw
                            .map(|is_raw| RecipeVersions::of(&normalized, is_raw)),
                        "{recipe} {path:?}"
                    );
                }
            }
        }
    }

    /// A never-edited or reset RAW (no recipe, `{}`) renders with every
    /// version the editor's new edit saves (its pinned recipe,
    /// src/contracts/rustContract.test.ts), so opening it and saving without
    /// a change keeps its look; the camera's lens versions need its data
    /// (none offline). A saved recipe keeps the versions it has.
    #[test]
    fn a_new_recipe_renders_with_the_versions_of_the_editors_new_edit() {
        let new_edit: Value = serde_json::from_str(include_str!(
            "../../engine/tests/fixtures/new-edit-recipe.raw.json"
        ))
        .unwrap();
        let raw = Path::new("/offline/IMG_2.ARW");
        for given in [Value::Null, json!({}), json!({"sourceFrameVersion": 3})] {
            for defaults in [Defaults::Render, Defaults::NewEdit] {
                let mut recipe = given.clone();
                normalize(
                    &mut recipe,
                    &RecipeSource::file(raw),
                    Bitmaps::None,
                    defaults,
                );
                for (key, value) in new_edit.as_object().unwrap() {
                    if key.ends_with("Version")
                        && key != "cameraCaVersion"
                        && key != "cameraGeometryVersion"
                    {
                        assert_eq!(recipe.get(key), Some(value), "{given} {defaults:?} {key}");
                    }
                }
            }
        }
        let mut other = json!({});
        let png = Path::new("/offline/scan.png");
        normalize(
            &mut other,
            &RecipeSource::file(png),
            Bitmaps::None,
            Defaults::Render,
        );
        assert_eq!(other["lumaNoiseVersion"], new_edit["lumaNoiseVersion"]);
        assert!(other.get("aiDenoiseVersion").is_none());
        let mut saved = json!({"exposure": 0.5});
        normalize(
            &mut saved,
            &RecipeSource::file(raw),
            Bitmaps::None,
            Defaults::Render,
        );
        assert!(saved.get("lumaNoiseVersion").is_none());
        assert!(saved.get("toneVersion").is_none());
        let mut open = json!({});
        normalize(
            &mut open,
            &RecipeSource::open_in_editor(),
            Bitmaps::None,
            Defaults::None,
        );
        assert_eq!(open, json!({}));
    }

    #[test]
    fn the_shared_cache_restores_a_bitmap_the_editor_sent_before() {
        let cache = Mutex::new(HashMap::new());
        let sent = json!({"aiPatches": [{"id": "p", "patchData": {"color": "c"}}]});
        let mut first = sent.clone();
        let source = RecipeSource::open_in_editor();
        normalize(&mut first, &source, Bitmaps::Shared(&cache), Defaults::None);
        let mut later = json!({"aiPatches": [{"id": "p", "patchData": null}]});
        normalize(&mut later, &source, Bitmaps::Shared(&cache), Defaults::None);
        assert_eq!(later, sent);
    }
}
