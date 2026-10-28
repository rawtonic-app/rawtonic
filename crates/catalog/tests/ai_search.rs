//! AI and provenance facts in the library's search (N5): what recipes say
//! about AI is indexed per version whatever writes them (edits, virtual
//! copies, undo, imports with sidecars, older libraries), found by the `ai`
//! rule and listed with counts; a file's own Digital Source Type by the
//! `sourceType` rule.
use rawtonic_catalog::{CaptureFacts, Field, Library, Mode, Rule};
use serde_json::{Value, json};
use std::{collections::BTreeMap, fs, path::Path};

fn rule(value: Value) -> Rule {
    serde_json::from_value(value).unwrap()
}

/// File names the rule finds, sorted.
fn found(lib: &Library, value: Value) -> Vec<String> {
    let mut names: Vec<String> = lib
        .search(&rule(value))
        .unwrap()
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn ai(value: impl Into<Value>) -> Value {
    let value: Value = value.into();
    json!({"field": "ai", "op": "is", "value": value})
}

/// A removal as Remove saves it (`model`: the model that guided it, `None`
/// for one made before that was recorded).
fn removal(model: Option<&str>) -> Value {
    let mut data = json!({"color": "AAAA", "mask": "BBBB", "offsetX": 0, "offsetY": 0,
        "width": 4, "height": 4, "isSrgbEncoded": true});
    if let Some(model) = model {
        data["model"] = json!(model);
    }
    json!({"id": "p1", "name": "Remove", "visible": true, "invert": false, "prompt": "",
        "isLoading": false, "subMasks": [{"type": "brush"}], "patchData": data})
}

fn state(adjustments: Value) -> Value {
    json!({"version": 1, "rating": 0, "tags": [], "adjustments": adjustments})
}

fn library(dir: &Path, names: &[&str]) -> (Library, Vec<String>) {
    let lib = Library::create(&dir.join("Job"), "Job", Mode::Session).unwrap();
    let capture = lib.directory.join("Capture");
    let ids = names
        .iter()
        .map(|name| {
            fs::write(capture.join(name), name).unwrap();
            lib.add_asset(&capture.join(name)).unwrap().id
        })
        .collect();
    (lib, ids)
}

/// The value lists of `field` as (value, count).
fn listed(lib: &Library, field: Field) -> BTreeMap<String, usize> {
    lib.facets(&Rule::everything(), &[field]).unwrap()[&field]
        .iter()
        .map(|v| (v.value.clone(), v.count))
        .collect()
}

#[test]
fn edits_are_found_by_what_they_do_with_ai() {
    let d = tempfile::tempdir().unwrap();
    let (lib, ids) = library(
        d.path(),
        &["A.ARW", "B.ARW", "C.ARW", "D.ARW", "E.jpg", "F.ARW"],
    );
    let masks = json!([{"visible": true, "subMasks": [
        {"type": "ai-sky", "visible": true, "parameters": {"maskDataBase64": "x"}}]}]);
    let pasted = json!([{"visible": true, "subMasks": [
        {"type": "ai-subject", "visible": true, "parameters": {"detectPending": true}}]}]);
    for (id, adjustments) in ids.iter().zip([
        json!({"aiPatches": [removal(Some("moebius"))]}),
        json!({"aiPatches": [removal(Some("mi-gan"))], "masks": masks}),
        json!({"aiPatches": [removal(None)], "masks": pasted}),
        json!({"aiDenoise": 40}),
        // AI Denoise does nothing on a JPEG.
        json!({"aiDenoise": 40, "aiDenoiseVersion": 3}),
        json!({"exposure": 1.0, "aiPatches": [{"id": "q", "visible": true, "patchData": null}]}),
    ]) {
        lib.save_editor_state(id, &state(adjustments)).unwrap();
    }
    assert_eq!(found(&lib, ai("generated")), ["A.ARW", "B.ARW", "C.ARW"]);
    assert_eq!(found(&lib, ai("fillMoebius")), ["A.ARW"]);
    assert_eq!(found(&lib, ai("fillMiGan")), ["B.ARW"]);
    assert_eq!(found(&lib, ai("fillUnrecorded")), ["C.ARW"]);
    assert_eq!(
        found(&lib, ai(json!(["fillMoebius", "fillMiGan"]))),
        ["A.ARW", "B.ARW"]
    );
    assert_eq!(found(&lib, ai("aiMask")), ["B.ARW", "C.ARW"]);
    assert_eq!(found(&lib, ai("pendingDetection")), ["C.ARW"]);
    assert_eq!(found(&lib, ai("missingFill")), ["F.ARW"]);
    assert_eq!(found(&lib, ai("aiDenoise")), ["D.ARW"]);
    assert_eq!(found(&lib, ai("olderDenoise")), ["D.ARW"]);
    assert_eq!(found(&lib, ai("none")), ["E.jpg"]);
    assert_eq!(
        found(&lib, json!({"field": "ai", "op": "empty"})),
        ["E.jpg"]
    );
    assert_eq!(
        found(
            &lib,
            json!({"field": "ai", "op": "isNot", "value": "generated"})
        ),
        ["D.ARW", "E.jpg", "F.ARW"]
    );
    // Names are the catalog's, in any case; unknown ones are refused.
    assert_eq!(found(&lib, ai("FILLMOEBIUS")), ["A.ARW"]);
    assert!(lib.search(&rule(ai("sparkles"))).is_err());
    assert!(
        rule(json!({"field": "ai", "op": "contains", "value": "generated"}))
            .validate()
            .is_err()
    );

    let lists = listed(&lib, Field::Ai);
    let expected: BTreeMap<String, usize> = [
        ("generated", 3),
        ("fillMoebius", 1),
        ("fillMiGan", 1),
        ("fillUnrecorded", 1),
        ("aiMask", 2),
        ("pendingDetection", 1),
        ("missingFill", 1),
        ("aiDenoise", 1),
        ("olderDenoise", 1),
        ("", 1),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect();
    assert_eq!(lists, expected);
    // Listed in the catalog's order, "none" last.
    let order: Vec<String> = lib.facets(&Rule::everything(), &[Field::Ai]).unwrap()[&Field::Ai]
        .iter()
        .map(|v| v.value.clone())
        .collect();
    assert_eq!(
        order,
        [
            "generated",
            "fillMoebius",
            "fillMiGan",
            "fillUnrecorded",
            "aiMask",
            "aiDenoise",
            "pendingDetection",
            "missingFill",
            "olderDenoise",
            ""
        ]
    );
    // Each listed value selects what it counts.
    for (value, count) in &lists {
        let rule = if value.is_empty() {
            json!({"field": "ai", "op": "empty"})
        } else {
            ai(json!(value))
        };
        assert_eq!(found(&lib, rule).len(), *count, "{value}");
    }
}

#[test]
fn edits_undo_and_copies_keep_the_facts_current() {
    let d = tempfile::tempdir().unwrap();
    let (lib, ids) = library(d.path(), &["A.ARW", "B.ARW"]);
    lib.save_editor_state(&ids[0], &state(json!({"exposure": 0.5})))
        .unwrap();
    assert!(found(&lib, ai("generated")).is_empty());
    lib.save_editor_state(
        &ids[0],
        &state(json!({"exposure": 0.5, "aiPatches": [removal(Some("moebius"))]})),
    )
    .unwrap();
    assert_eq!(found(&lib, ai("generated")), ["A.ARW"]);
    assert_eq!(lib.ai_facts(&ids[0]).unwrap(), 1 | (1 << 3));
    // The removal hidden: nothing generated shows.
    let mut hidden = removal(Some("moebius"));
    hidden["visible"] = json!(false);
    lib.save_editor_state(&ids[0], &state(json!({"aiPatches": [hidden]})))
        .unwrap();
    assert!(found(&lib, ai("generated")).is_empty());
    // Undo goes back to the shown removal.
    assert!(lib.undo(&lib.primary_version(&ids[0]).unwrap()).unwrap());
    assert_eq!(found(&lib, ai("generated")), ["A.ARW"]);

    // A virtual copy's removal is its photograph's (copies are found as
    // their photograph), and goes when the copy goes.
    lib.save_copy_state(
        &ids[1],
        "a1b2c3",
        &state(json!({"aiPatches": [removal(Some("mi-gan"))]})),
    )
    .unwrap();
    assert_eq!(found(&lib, ai("fillMiGan")), ["B.ARW"]);
    lib.save_copy_state(
        &ids[1],
        "a1b2c3",
        &state(json!({"aiDenoise": 20, "aiDenoiseVersion": 3})),
    )
    .unwrap();
    assert!(found(&lib, ai("fillMiGan")).is_empty());
    assert_eq!(found(&lib, ai("aiDenoise")), ["B.ARW"]);
    assert!(lib.remove_copy(&ids[1], "a1b2c3").unwrap());
    assert!(found(&lib, ai("aiDenoise")).is_empty());
    assert_eq!(found(&lib, ai("none")), ["B.ARW"]);
}

/// Recipes stored with their large strings moved out (`recipe_blobs`) and
/// photographs added with their editor sidecar are indexed as they are.
#[test]
fn imports_with_sidecars_and_stored_bitmaps_are_indexed() {
    let d = tempfile::tempdir().unwrap();
    let lib = Library::create(&d.path().join("Job"), "Job", Mode::Session).unwrap();
    let capture = lib.directory.join("Capture");
    let photo = capture.join("Sidecar.ARW");
    fs::write(&photo, b"raw").unwrap();
    let mut big = removal(Some("moebius"));
    // Long enough to be stored once by its hash.
    big["patchData"]["color"] = json!("A".repeat(64 * 1024));
    fs::write(
        capture.join("Sidecar.ARW.rrdata"),
        serde_json::to_vec(&state(json!({"aiPatches": [big]}))).unwrap(),
    )
    .unwrap();
    let report = lib.add_files(std::slice::from_ref(&photo)).unwrap();
    assert_eq!(report.added.len(), 1, "{report:?}");
    let blobs: i64 = lib
        .search_count(&rule(ai("fillMoebius")))
        .map(|n| n as i64)
        .unwrap();
    assert_eq!(blobs, 1);
}

/// A schema 16 library (before AI facts were indexed) is indexed when it
/// is opened, from its recipes as stored: no recipe is changed, copies and
/// all. Its non-RAW files are read again for their digital source type.
#[test]
fn a_schema_16_library_is_indexed_without_changing_its_recipes() {
    let d = tempfile::tempdir().unwrap();
    let (lib, ids) = library(d.path(), &["A.ARW", "B.jpg", "C.ARW"]);
    lib.save_editor_state(&ids[0], &state(json!({"aiPatches": [removal(None)]})))
        .unwrap();
    lib.save_copy_state(&ids[2], "00ff00", &state(json!({"aiDenoise": 50})))
        .unwrap();
    for id in &ids {
        lib.record_facts(id, &CaptureFacts::default()).unwrap();
    }
    drop(lib);
    let db_path = d.path().join("Job/session.sqlite");
    let db = rusqlite::Connection::open(&db_path).unwrap();
    db.execute_batch(include_str!("undo_0017.sql")).unwrap();
    let stored = |db: &rusqlite::Connection| -> Vec<String> {
        db.prepare("SELECT payload FROM recipes UNION ALL SELECT legacy_state FROM versions WHERE legacy_state IS NOT NULL ORDER BY 1")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    let before = stored(&db);
    drop(db);
    let lib = Library::open(&d.path().join("Job")).unwrap();
    assert_eq!(found(&lib, ai("fillUnrecorded")), ["A.ARW"]);
    assert_eq!(found(&lib, ai("olderDenoise")), ["C.ARW"]);
    assert_eq!(found(&lib, ai("none")), ["B.jpg"]);
    assert_eq!(lib.facts_backlog().unwrap(), 1, "B.jpg is read again");
    drop(lib);
    let db = rusqlite::Connection::open(&db_path).unwrap();
    assert_eq!(stored(&db), before);
}

#[test]
fn a_files_digital_source_type_is_found_and_listed() {
    let d = tempfile::tempdir().unwrap();
    let (lib, ids) = library(d.path(), &["gen.png", "comp.jpg", "cam.jpg", "plain.jpg"]);
    for (id, source) in ids.iter().zip([
        Some("http://cv.iptc.org/newscodes/digitalsourcetype/trainedAlgorithmicMedia"),
        Some(
            " https://cv.iptc.org/newscodes/digitalsourcetype/compositeWithTrainedAlgorithmicMedia",
        ),
        Some("http://cv.iptc.org/newscodes/digitalsourcetype/digitalCapture"),
        None,
    ]) {
        lib.record_facts(
            id,
            &CaptureFacts {
                source_type: source.map(Into::into),
                ..Default::default()
            },
        )
        .unwrap();
    }
    assert_eq!(
        lib.facts(&ids[0])
            .unwrap()
            .unwrap()
            .0
            .source_type
            .as_deref(),
        Some("trainedAlgorithmicMedia")
    );
    let source = |value: Value| {
        found(
            &lib,
            json!({"field": "sourceType", "op": "is", "value": value}),
        )
    };
    assert_eq!(source(json!("trainedAlgorithmicMedia")), ["gen.png"]);
    // A full IPTC URI selects its code.
    assert_eq!(
        source(json!(
            "http://cv.iptc.org/newscodes/digitalsourcetype/compositeWithTrainedAlgorithmicMedia"
        )),
        ["comp.jpg"]
    );
    assert_eq!(
        source(json!(["none", "digitalCapture"])),
        ["cam.jpg", "plain.jpg"]
    );
    assert_eq!(
        found(
            &lib,
            json!({"field": "sourceType", "op": "contains", "value": "algorithmic"})
        ),
        ["comp.jpg", "gen.png"]
    );
    let lists = listed(&lib, Field::SourceType);
    assert_eq!(lists.get("trainedAlgorithmicMedia"), Some(&1));
    assert_eq!(lists.get("compositeWithTrainedAlgorithmicMedia"), Some(&1));
    assert_eq!(lists.get(""), Some(&1));
}
