//! This integration test has its own process because the app's saved
//! develop defaults are global.

use std::path::Path;

use rawtonic_engine::develop_defaults::{DevelopDefaults, set_current};
use rawtonic_engine::recipe_versions::SHADOWS_TRAVEL_TONE_VERSION;
use rawtonic_export::normalize::{Bitmaps, Defaults, RecipeSource, normalize};
use serde_json::json;

#[test]
fn export_normalization_preserves_the_amount_of_an_unversioned_develop_default() {
    set_current(
        serde_json::from_value(json!({
            "global": {"settings": {"shadows": 61}},
        }))
        .unwrap(),
    );
    let source = RecipeSource::file(Path::new("/offline/never-edited.NEF"));
    let mut recipe = json!({});
    let normalized = normalize(&mut recipe, &source, Bitmaps::Own, Defaults::Render);
    normalized.lens.unwrap();
    let tone = normalized.versions.unwrap().tone;
    assert!(tone.get() >= SHADOWS_TRAVEL_TONE_VERSION);
    assert_eq!(recipe["toneVersion"], tone.get());
    assert_eq!(recipe["developDefaults"], json!(["global"]));
    assert_eq!(recipe["shadows"], 50.0);
    assert_eq!(
        tone.shadows_amount(recipe["shadows"].as_f64().unwrap()),
        61.0
    );

    // After saving the normalized recipe, later preference changes leave it alone.
    set_current(DevelopDefaults::default());
    let saved = recipe.clone();
    let normalized = normalize(&mut recipe, &source, Bitmaps::Own, Defaults::Render);
    assert_eq!(normalized.file_versions().tone, tone);
    assert_eq!(recipe, saved);
}
