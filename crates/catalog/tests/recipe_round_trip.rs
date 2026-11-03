//! Recipe codec round trip through the catalog (notes:
//! plans/architecture-cleanup-2026-11.md, step 0.3b): an editor state saved
//! and read again without an edit keeps its recipe's JSON value — key
//! presence, explicit nulls, unknown and future keys — for the primary
//! version (`save_editor_state`/`editor_state`, `visit_editor_states`), a
//! virtual copy (`save_copy_state`/`copy_state`) and the recipe history
//! (`save_recipe`/`recipe`). Fixtures:
//! `crates/engine/tests/fixtures/recipe-codec.json` and the editor's
//! new-edit recipe; `crates/export/tests/recipe_round_trip.rs` pins
//! `is_new_recipe` on them.

use std::collections::HashSet;

use rawtonic_catalog::{Library, Mode};
use serde_json::{Value, json};

fn fixture_file(name: &str) -> Value {
    let path = format!(
        "{}/../engine/tests/fixtures/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

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

/// FINDING: `editor_state` and `visit_editor_states` write the library's
/// colour label into every recipe object (`apply_editor_metadata`:
/// `adjustments.colorLabel`, `""` without a label). A recipe read from a
/// library is therefore not the one saved: `{}` (Library Reset) and the
/// frame-only and `lensProfileFor`-only recipes come back as
/// `{"colorLabel": ""}`…, which `is_new_recipe` no longer counts as new
/// (pinned in `src-tauri/src/file_management/edits.rs`). Recorded, not
/// changed (plan step 0.3b).
fn as_read_from_the_library(recipe: &Value, label: &str) -> Value {
    let mut read = recipe.clone();
    if let Some(map) = read.as_object_mut() {
        map.insert("colorLabel".into(), json!(label));
    }
    read
}

#[test]
fn editor_states_copies_and_history_keep_their_recipes() {
    let temp = tempfile::tempdir().unwrap();
    let lib = Library::create(&temp.path().join("job"), "Codec", Mode::Session).unwrap();
    for (index, (name, recipe)) in fixtures().into_iter().enumerate() {
        let source = lib.directory.join(format!("Capture/photo{index}.arw"));
        std::fs::write(&source, format!("synthetic original {index}")).unwrap();
        let asset = lib.add_asset(&source).unwrap();
        let state = json!({"version": 1, "rating": 2, "tags": ["kept"], "adjustments": recipe});

        // Primary version: saved, read back, saved again unchanged.
        lib.save_editor_state(&asset.id, &state).unwrap();
        let loaded = lib.editor_state(&asset.id).unwrap().unwrap();
        assert_eq!(
            loaded["adjustments"],
            as_read_from_the_library(&recipe, ""),
            "{name}: editor_state"
        );
        lib.save_editor_state(&asset.id, &loaded).unwrap();
        let again = lib.editor_state(&asset.id).unwrap().unwrap();
        assert_eq!(again, loaded, "{name}: second round trip");

        // The library listing reads the same state.
        let mut listed = None;
        lib.visit_editor_states(&HashSet::from([source.canonicalize().unwrap()]), |_, s| {
            listed = s
        })
        .unwrap();
        assert_eq!(
            listed.unwrap()["adjustments"],
            as_read_from_the_library(&recipe, ""),
            "{name}: visit_editor_states"
        );

        // A virtual copy's state.
        lib.save_copy_state(&asset.id, "0a0b0c", &state).unwrap();
        let copy = lib.copy_state(&asset.id, "0a0b0c").unwrap().unwrap();
        assert_eq!(copy, state, "{name}: copy_state");

        // The recipe history (`recipes` table) of the version.
        let version = lib.primary_version(&asset.id).unwrap();
        let payload = json!({"schema_version": 1, "adjustments": recipe, "layers": []});
        lib.save_recipe(&version, &payload, None).unwrap();
        assert_eq!(
            lib.recipe(&version).unwrap().unwrap(),
            payload,
            "{name}: recipe history"
        );
    }
}
