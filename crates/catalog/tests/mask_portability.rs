//! Mask data stays valid when a closed session folder is copied to another
//! path (G38): every mask kind's definition and bitmap is kept in the
//! session database (bitmaps of 16 KB and more as content-addressed
//! blobs), nothing in it names the session's old place, and the copy reads
//! back the same recipe byte for byte. The rendered comparison of such a
//! copy is `crates/cli/tests/moved_session_masks.rs` (needs a RAW and a GPU).
use rawtonic_catalog::{Library, Mode};
use serde_json::{Value, json};
use std::fs;
use std::path::Path;

/// A data URL of `bytes` pseudo-random bytes: large enough that the recipe
/// stores it as a blob.
fn bitmap(seed: u32, bytes: usize) -> String {
    let mut x = seed.wrapping_mul(2_654_435_761).max(1);
    let body: String = (0..bytes)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            char::from(
                b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"
                    [(x % 64) as usize],
            )
        })
        .collect();
    format!("data:image/png;base64,{body}")
}

fn sub(id: &str, kind: &str, parameters: Value) -> Value {
    json!({"id": id, "type": kind, "visible": true, "invert": false, "opacity": 100.0,
           "mode": "additive", "parameters": parameters})
}

/// One layer per mask kind the renderer knows (`mask_generation`).
pub fn every_mask_kind() -> Value {
    let line = json!({"tool": "brush", "brushSize": 40.0, "feather": 0.5,
                      "points": [{"x": 100.0, "y": 100.0}, {"x": 400.0, "y": 300.0}]});
    let subs = vec![
        sub(
            "radial",
            "radial",
            json!({"centerX": 300.0, "centerY": 200.0, "radiusX": 150.0, "radiusY": 100.0, "rotation": 10.0, "feather": 0.5}),
        ),
        sub(
            "linear",
            "linear",
            json!({"startX": 0.0, "startY": 0.0, "endX": 600.0, "endY": 400.0, "range": 50.0}),
        ),
        sub("brush", "brush", json!({"lines": [line]})),
        sub(
            "flow",
            "flow",
            json!({"lines": [{"tool": "brush", "brushSize": 30.0, "flow": 40.0, "points": [{"x": 50.0, "y": 50.0}, {"x": 90.0, "y": 70.0}]}]}),
        ),
        sub(
            "color",
            "color",
            json!({"targetX": 120.0, "targetY": 80.0, "tolerance": 30.0}),
        ),
        sub(
            "luma",
            "luma-range",
            json!({"rangeLow": 0.2, "rangeHigh": 0.8, "falloff": 0.1}),
        ),
        sub(
            "luminance",
            "luminance",
            json!({"targetX": 200.0, "targetY": 150.0, "tolerance": 25.0}),
        ),
        sub(
            "subject",
            "ai-subject",
            json!({"startX": 10.0, "startY": 10.0, "endX": 500.0, "endY": 380.0, "maskDataBase64": bitmap(1, 40_000)}),
        ),
        sub(
            "foreground",
            "ai-foreground",
            json!({"maskDataBase64": bitmap(2, 40_000), "subjectVersion": 2}),
        ),
        sub(
            "sky",
            "ai-sky",
            json!({"maskDataBase64": bitmap(3, 40_000), "grow": 5.0, "feather": 10.0}),
        ),
        sub(
            "people",
            "ai-people",
            json!({"maskDataBase64": bitmap(4, 20_000)}),
        ),
        sub(
            "depth",
            "ai-depth",
            json!({"minDepth": 0.1, "maxDepth": 0.6, "minFade": 0.05, "maxFade": 0.05, "maskDataBase64": bitmap(5, 30_000)}),
        ),
        sub(
            "eraser",
            "quick-eraser",
            json!({"startX": 0.0, "startY": 0.0, "endX": 10.0, "endY": 10.0, "maskDataBase64": bitmap(6, 18_000)}),
        ),
        sub("small", "ai-sky", json!({"maskDataBase64": bitmap(7, 200)})),
        sub("all", "all", json!({})),
    ];
    let layers: Vec<Value> = subs
        .into_iter()
        .enumerate()
        .map(|(i, sub)| {
            json!({"id": format!("layer{i}"), "name": format!("Layer {i}"), "visible": true,
                   "invert": false, "opacity": 100.0, "maskVersion": 4,
                   "adjustments": {"exposure": 0.1 * i as f64}, "subMasks": [sub]})
        })
        .collect();
    json!({"version": 1, "rating": 0, "tags": [],
           "adjustments": {"exposure": 0.2, "masks": layers,
                           "lutPath": "rawtonic-resource:resources/look.cube"}})
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).unwrap();
        }
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

#[test]
fn every_mask_kind_reads_back_byte_identical_from_a_copied_session() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = Library::create(&tmp.path().join("Studio/Job"), "Job", Mode::Session).unwrap();
    let original_dir = lib.directory.clone();
    fs::create_dir_all(original_dir.join("resources")).unwrap();
    fs::write(original_dir.join("resources/look.cube"), b"LUT_3D_SIZE 2\n").unwrap();
    let photo = original_dir.join("Capture/A.ARW");
    fs::write(&photo, b"synthetic original").unwrap();
    let asset = lib.add_asset(&photo).unwrap();
    let state = every_mask_kind();
    lib.save_editor_state(&asset.id, &state).unwrap();
    let version = lib.primary_version(&asset.id).unwrap();
    let stored = lib.recipe(&version).unwrap().unwrap();
    let state_here = lib.editor_state(&asset.id).unwrap().unwrap();
    lib.checkpoint().unwrap();
    drop(lib);

    // Copied whole (closed) to another place, the original then gone, so
    // nothing can be read from it. /dev/shm is another volume on Linux.
    let elsewhere = Path::new("/dev/shm");
    let other = if elsewhere.is_dir() {
        tempfile::tempdir_in(elsewhere).unwrap()
    } else {
        tempfile::tempdir().unwrap()
    };
    let copy = other.path().join("Archive/Job copy");
    copy_tree(&original_dir, &copy);
    fs::rename(&original_dir, tmp.path().join("gone")).unwrap();

    // The database names no absolute place of the session.
    let database = fs::read(copy.join("session.sqlite")).unwrap();
    let old = original_dir.to_string_lossy();
    assert!(
        !contains(&database, old.as_bytes()),
        "the database names {old}"
    );

    let lib = Library::open(&copy).unwrap();
    let asset = lib.asset(&asset.id).unwrap();
    assert!(!asset.missing);
    assert_eq!(
        asset.path,
        copy.canonicalize().unwrap().join("Capture/A.ARW")
    );
    let read = lib.recipe(&version).unwrap().unwrap();
    assert_eq!(
        serde_json::to_vec(&read).unwrap(),
        serde_json::to_vec(&stored).unwrap()
    );
    // The editor state the renderer gets: the same masks, bitmaps included,
    // and the LUT resolved inside the copy.
    let state_there = lib.editor_state(&asset.id).unwrap().unwrap();
    assert_eq!(
        serde_json::to_vec(&state_there["adjustments"]["masks"]).unwrap(),
        serde_json::to_vec(&state_here["adjustments"]["masks"]).unwrap()
    );
    // Every layer and bitmap made it into the library in the first place.
    let masks = state_here["adjustments"]["masks"].as_array().unwrap();
    assert_eq!(
        masks.len(),
        state["adjustments"]["masks"].as_array().unwrap().len()
    );
    for (kept, given) in masks
        .iter()
        .zip(state["adjustments"]["masks"].as_array().unwrap())
    {
        assert_eq!(kept["subMasks"][0]["type"], given["subMasks"][0]["type"]);
        assert_eq!(
            kept["subMasks"][0]["parameters"]["maskDataBase64"],
            given["subMasks"][0]["parameters"]["maskDataBase64"]
        );
    }
    assert_eq!(
        Path::new(state_there["adjustments"]["lutPath"].as_str().unwrap()),
        copy.canonicalize().unwrap().join("resources/look.cube")
    );
}
