//! Synthetic mask-heavy recipes recorded on main before the masks extraction.
//! No update mode: later refactors must match the recorded bytes.
//!
//! The uniform bytes are recorded per platform (`<os>-<arch>`): a few
//! colour values go through the platform's maths library and differ in the
//! last bits between macOS and Linux. Each platform's bytes were recorded with
//! the unchanged baseline renderer (`baseline`, test commit 444a77f2):
//! macos-aarch64 on the Mac mini, linux-x86_64 on the Linux PC (2026-11-07).
//! A platform without a recording fails rather than passing unchecked.
//!
//! The fixture is stored compacted (see its `note`): [`expand`] restores
//! every recorded case exactly (proven against the recording when it was
//! compacted, notes: plans/arch-cleanup-list-2026-11-06.md row 68).
use rawtonic_engine::image_processing::{
    get_all_adjustments_from_json, get_all_adjustments_with_masks, mask_definitions_from_json,
};
use rawtonic_engine::recipe_versions::RecipeVersions;
use serde_json::Value;
use std::fmt::Write as _;

/// `{"$ref": name}` → `localAdjustments[name]`, everywhere in `value`.
fn substitute(value: &Value, blocks: &Value) -> Value {
    match value {
        Value::Object(map) if map.len() == 1 && map.contains_key("$ref") => {
            let block = &blocks[map["$ref"].as_str().unwrap()];
            assert!(block.is_object(), "unknown block {value}");
            block.clone()
        }
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), substitute(v, blocks)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(|v| substitute(v, blocks)).collect()),
        other => other.clone(),
    }
}

/// The recorded cases: shared blocks substituted, and a `recipeOf` case
/// given that earlier case's recipe (with `recipeChanges`) and definitions.
fn expand(fixture: &Value) -> Vec<Value> {
    let blocks = &fixture["localAdjustments"];
    let mut cases: Vec<Value> = Vec::new();
    for entry in fixture["cases"].as_array().unwrap() {
        let mut case = entry.as_object().unwrap().clone();
        if let Some(of) = case.remove("recipeOf") {
            let base = cases
                .iter()
                .find(|c| c["name"] == of)
                .expect("an earlier case");
            let mut recipe = base["recipe"].clone();
            for (key, value) in case.remove("recipeChanges").unwrap().as_object().unwrap() {
                recipe[key] = value.clone();
            }
            case.insert("recipe".into(), recipe);
            case.insert("definitions".into(), base["definitions"].clone());
        } else {
            for key in ["recipe", "definitions"] {
                let value = substitute(&case[key], blocks);
                case.insert(key.into(), value);
            }
        }
        cases.push(Value::Object(case));
    }
    cases
}

#[test]
fn the_compacted_fixture_keeps_every_recorded_case() {
    let fixture: Value =
        serde_json::from_str(include_str!("../src/fixtures/mask_adjustments.json")).unwrap();
    let cases = expand(&fixture);
    let names: Vec<&str> = cases.iter().map(|c| c["name"].as_str().unwrap()).collect();
    assert_eq!(
        names,
        [
            "all-types-r1-standard",
            "all-types-r1-classic",
            "all-types-r7-standard",
            "all-types-r7-classic",
            "local-hidden",
            "disabled-inverted",
            "local-null-empty",
            "malformed-container",
            "remaining-mask-types-and-blends",
        ]
    );
    let slots: u64 = cases.iter().map(|c| c["maskCount"].as_u64().unwrap()).sum();
    assert_eq!(slots, 61);
    for platform in ["linux-x86_64", "macos-aarch64"] {
        let bytes: usize = cases
            .iter()
            .map(|c| c["maskUniformHex"][platform].as_str().unwrap().len() / 2)
            .sum();
        assert_eq!(bytes, 147_376, "{platform}");
    }
    let keys = |c: &Value| {
        let mut keys: Vec<String> = c.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        keys
    };
    for case in &cases {
        assert_eq!(
            keys(case),
            [
                "definitions",
                "isRaw",
                "maskCount",
                "maskUniformHex",
                "name",
                "recipe"
            ],
            "{}",
            case["name"]
        );
        assert!(!case.to_string().contains("$ref"), "{}", case["name"]);
    }
}

#[test]
fn mask_adjustments_and_containers_match_recorded_main() {
    let fixture: Value =
        serde_json::from_str(include_str!("../src/fixtures/mask_adjustments.json")).unwrap();
    for case in &expand(&fixture) {
        let prepared =
            get_all_adjustments_from_json(&case["recipe"], case["isRaw"].as_bool().unwrap(), None);
        assert_eq!(
            prepared.uniforms.mask_count,
            case["maskCount"].as_u64().unwrap() as u32,
            "{}",
            case["name"]
        );
        let mut actual = String::new();
        for slot in prepared
            .uniforms
            .mask_adjustments
            .iter()
            .take(prepared.uniforms.mask_count as usize)
        {
            for byte in bytemuck::bytes_of(slot) {
                write!(actual, "{byte:02x}").unwrap();
            }
        }
        let platform = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);
        let recorded = case["maskUniformHex"][&platform]
            .as_str()
            .unwrap_or_else(|| {
                panic!(
                    "no recording for {platform}: record it with the baseline renderer {}",
                    fixture["baseline"]
                )
            });
        assert_eq!(
            actual, recorded,
            "{} differs from main {}",
            case["name"], fixture["baseline"]
        );
        let definitions = mask_definitions_from_json(&case["recipe"]);
        assert_eq!(
            serde_json::to_value(&definitions).unwrap(),
            case["definitions"],
            "{} containers differ",
            case["name"]
        );
        // The previews' and exports' path: the layers and versions read once
        // by the caller give the same uniforms.
        let is_raw = case["isRaw"].as_bool().unwrap();
        let with_masks = get_all_adjustments_with_masks(
            &case["recipe"],
            is_raw,
            None,
            &definitions,
            &RecipeVersions::of(&case["recipe"], is_raw),
        );
        assert_eq!(
            bytemuck::bytes_of(&*with_masks.uniforms),
            bytemuck::bytes_of(&*prepared.uniforms),
            "{}",
            case["name"]
        );
    }
}
