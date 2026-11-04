//! Desktop glue for `rawtonic_engine::adjustment_utils` (ADR 0009): the Tauri commands
//! and app-state access stay here, the logic lives in the engine crate.
pub use rawtonic_engine::adjustment_utils::*;

use rawtonic_export::normalize::{Bitmaps, Defaults, Normalized, RecipeSource, normalize};

use crate::state::develop::DevelopSession;

/// `normalize` for a recipe the editor sent for the photograph it has open:
/// the bitmaps it refers to by id come from the develop session's cache,
/// which also remembers the ones it carries; the editor completed the rest
/// itself.
pub fn normalize_editor_recipe(
    develop: &DevelopSession,
    adjustments: &mut serde_json::Value,
) -> Normalized {
    normalize(
        adjustments,
        &RecipeSource::open_in_editor(),
        Bitmaps::Shared(&develop.patch_cache),
        Defaults::None,
    )
}
