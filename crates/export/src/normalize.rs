//! One entry that makes a recipe renderable (plan 2.1, notes:
//! plans/architecture-cleanup-2026-11.md): every render of a photograph —
//! export, CLI, multi-view, collage, thumbnails, the editor's previews —
//! calls [`normalize`] once instead of the defaulting steps one by one.
//!
//! The steps and their order are today's, unchanged (the render-input
//! snapshots and exports stay byte-identical):
//! 1. bitmaps the recipe refers to by id restored (`hydrate_adjustments_from_cache`);
//! 2. lens parameters saved while the photograph could not be read
//!    resolved (`exif_processing::resolve_unresolved_lens`);
//! 3. a new recipe started as a new edit: render, sharpening and frame
//!    versions and the camera's crop (`camera_crop::apply_default`);
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
    Normalized {
        versions: source
            .is_raw
            .map(|is_raw| RecipeVersions::of(recipe, is_raw)),
        lens,
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
                        source.is_raw.map(|is_raw| RecipeVersions::of(&normalized, is_raw)),
                        "{recipe} {path:?}"
                    );
                }
            }
        }
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
