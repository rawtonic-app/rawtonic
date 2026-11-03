//! Recipe codec round trip (notes: plans/architecture-cleanup-2026-11.md,
//! step 0.3b): a recipe read from an editor sidecar and written again without
//! an edit keeps its JSON value — key presence, explicit nulls, unknown and
//! future keys — and `is_new_recipe` decides on each fixture as it does on
//! `aa8e7f3c`. This pins today's behaviour for the later `StoredRecipe`
//! work; a change here is a change of saved recipes.
//!
//! Fixtures: `crates/engine/tests/fixtures/recipe-codec.json` (synthetic)
//! and the editor's new-edit recipe `new-edit-recipe.raw.json`. The app's
//! own load/save (`edit_sidecars`, session database) and the catalog have
//! their round-trip tests in `src-tauri/src/file_management/edits.rs` and
//! `crates/catalog/tests/recipe_round_trip.rs`.

use rawtonic_engine::image_processing::{is_new_recipe, render_version};
use rawtonic_engine::lens_recipe::{LENS_PROFILE_FOR, stamp_loaded_lens_profile};
use rawtonic_export::sidecars::read_sidecar_file;
use serde_json::{Value, json};

fn fixture_file(name: &str) -> Value {
    let path = format!(
        "{}/../engine/tests/fixtures/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// (name, recipe) of every codec fixture and the new-edit recipe.
fn fixtures() -> Vec<(String, Value)> {
    let mut out: Vec<(String, Value)> = fixture_file("recipe-codec.json")
        .as_object()
        .unwrap()
        .iter()
        .map(|(name, recipe)| (name.clone(), recipe.clone()))
        .collect();
    out.push(("new-edit".into(), fixture_file("new-edit-recipe.raw.json")));
    out
}

/// What `aa8e7f3c` decides per fixture: (name, is_new_recipe, render_version).
const PINNED: &[(&str, bool, u64)] = &[
    ("null", true, 1),
    ("empty", true, 1),
    ("frame-only", true, 1),
    ("lens-profile-for-only", true, 1),
    ("frame-and-lens-profile-for", true, 1),
    ("old-no-version-keys", false, 1),
    ("partial-render-v1", false, 1),
    ("partial-render-v2", false, 2),
    ("partial-render-v7", false, 7),
    ("explicit-nulls", false, 11),
    ("nested-unknown", false, 11),
    // Clamped: a newer build's recipe renders with this build's newest version.
    ("future-version", false, 11),
    ("number-forms", false, 11),
    ("new-edit", false, 11),
];

#[test]
fn is_new_recipe_decides_each_fixture_as_on_the_baseline() {
    let fixtures = fixtures();
    assert_eq!(fixtures.len(), PINNED.len(), "every fixture pinned");
    for (name, recipe) in &fixtures {
        let &(_, new, version) = PINNED
            .iter()
            .find(|(pinned, ..)| pinned == name)
            .unwrap_or_else(|| panic!("{name} not pinned"));
        assert_eq!(is_new_recipe(recipe), new, "{name}: is_new_recipe");
        assert_eq!(render_version(recipe), version, "{name}: render_version");
        // The editor's load stamps `lensProfileFor` on every recipe object
        // without it (`stamp_loaded_lens_profile`); that alone never makes
        // a saved recipe new or a new one saved.
        let mut loaded = recipe.clone();
        stamp_loaded_lens_profile(&mut loaded);
        assert_eq!(is_new_recipe(&loaded), new, "{name}: after the load stamp");
        assert_eq!(
            loaded.get(LENS_PROFILE_FOR).is_some(),
            recipe.is_object(),
            "{name}: the load stamp is on every recipe object"
        );
        if recipe.get(LENS_PROFILE_FOR).is_some() {
            assert_eq!(&loaded, recipe, "{name}: an existing stamp is kept");
        }
    }
}

/// A sidecar read as the CLI and the app's file fallback read it
/// (`read_sidecar_file`) and written as the app writes it (`save_sidecar`:
/// `ImageMetadata` pretty-printed) keeps its recipe and every other field.
#[test]
fn a_sidecar_read_and_written_again_keeps_its_recipe() {
    let dir = tempfile::tempdir().unwrap();
    for (index, (name, recipe)) in fixtures().into_iter().enumerate() {
        let sidecar = dir.path().join(format!("photo{index}.ARW.rrdata"));
        let stored = json!({
            "version": 1,
            "rating": 3,
            "tags": ["kept"],
            "adjustments": recipe,
            "exif": {"Make": "Maker"},
        });
        std::fs::write(&sidecar, serde_json::to_string_pretty(&stored).unwrap()).unwrap();
        let (meta, healed) = read_sidecar_file(&sidecar);
        assert!(!healed, "{name}");
        let written: Value =
            serde_json::from_str(&serde_json::to_string_pretty(&meta).unwrap()).unwrap();
        assert_eq!(written["adjustments"], recipe, "{name}: recipe");
        assert_eq!(written, stored, "{name}: whole sidecar");
    }
}

/// Sidecar fields around the recipe that a read and write do not keep as
/// they were. They leave the recipe alone, so they are pinned as findings,
/// not fixed here (plan step 0.3b: record, do not change).
#[test]
fn sidecar_fields_a_round_trip_changes_are_pinned() {
    let dir = tempfile::tempdir().unwrap();
    let round_trip = |name: &str, stored: &Value| -> Value {
        let sidecar = dir.path().join(format!("{name}.ARW.rrdata"));
        std::fs::write(&sidecar, stored.to_string()).unwrap();
        let (meta, _) = read_sidecar_file(&sidecar);
        serde_json::from_str(&serde_json::to_string_pretty(&meta).unwrap()).unwrap()
    };
    // FINDING: a sidecar without `tags` is written back with `"tags": null`
    // (`ImageMetadata::tags` is `Option` without `skip_serializing_if`).
    let written = round_trip(
        "no-tags",
        &json!({"version": 1, "rating": 2, "adjustments": {"exposure": 1}}),
    );
    assert_eq!(written["tags"], Value::Null);
    assert!(written.as_object().unwrap().contains_key("tags"));
    assert_eq!(written["adjustments"], json!({"exposure": 1}));
    // FINDING: a sidecar without `adjustments` (or with a field of the wrong
    // type) does not parse as `ImageMetadata`; it is read as the default, so
    // a write without an edit replaces its rating and tags with defaults.
    let written = round_trip(
        "no-adjustments",
        &json!({"version": 1, "rating": 4, "tags": ["a"]}),
    );
    assert_eq!(
        written,
        json!({"version": 1, "rating": 0, "tags": null, "adjustments": null})
    );
    // Unknown top-level sidecar fields are dropped (serde ignores them).
    // FINDING: so a sidecar written by a newer build loses its new fields.
    let written = round_trip(
        "unknown-field",
        &json!({"version": 1, "rating": 1, "tags": [], "adjustments": {}, "futureField": 1}),
    );
    assert!(written.get("futureField").is_none());
    assert_eq!(written["adjustments"], json!({}));
}
