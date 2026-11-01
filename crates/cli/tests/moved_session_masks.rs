//! A closed session copied whole to another place (another volume where
//! there is one) renders its photo with every mask kind byte-identical to
//! the original session (G38): mask definitions and bitmaps live in the
//! session database, referenced relative to the session. Opt-in, as it
//! needs a RAW and a GPU:
//!
//! RAWTONIC_RAW=/path/photo.NEF cargo test -p rawtonic-cli --release --test moved_session_masks -- --ignored
//!
//! The RAW is any one the decoder reads (e.g. `bayer-nef.NEF` from
//! `scripts/bench/fetch_fixtures.sh`); it is copied, never changed. The
//! lensfun database is the checkout's `src-tauri/lensfun_db` unless
//! RAWTONIC_LENSFUN_DB names one. The AI masks hold synthetic bitmaps (no
//! models are needed): what is compared is that the copy renders what the
//! original does.

use rawtonic_catalog::{Library, Mode};
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn rawtonic(args: &[&Path]) {
    let lensfun = std::env::var_os("RAWTONIC_LENSFUN_DB")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../src-tauri/lensfun_db")
        });
    let output = Command::new(env!("CARGO_BIN_EXE_rawtonic"))
        .args(args)
        .env("RAWTONIC_LENSFUN_DB", lensfun)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "rawtonic {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A gradient bitmap with a disc, as an AI mask stores it (PNG data URL).
fn bitmap(seed: u32) -> String {
    let (w, h) = (640u32, 427u32);
    let image = image::GrayImage::from_fn(w, h, |x, y| {
        let (dx, dy) = (x as f32 - w as f32 * 0.5, y as f32 - h as f32 * 0.5);
        let disc = if dx * dx + dy * dy < (120.0 + seed as f32 * 10.0).powi(2) {
            255.0
        } else {
            0.0
        };
        let ramp = (x + y * seed) as f32 / (w + h * seed) as f32 * 255.0;
        image::Luma([((disc + ramp) * 0.5) as u8])
    });
    rawtonic_segment::mask_data_url(&image).unwrap()
}

fn sub(id: &str, kind: &str, parameters: Value) -> Value {
    json!({"id": id, "type": kind, "visible": true, "invert": false, "opacity": 100.0,
           "mode": "additive", "parameters": parameters})
}

/// One layer per mask kind the renderer knows (`mask_generation`), in
/// coordinates of a photo of about 6000 × 4000 pixels.
fn every_mask_kind() -> Value {
    let line = json!({"tool": "brush", "brushSize": 400.0, "feather": 0.5,
                      "points": [{"x": 1000.0, "y": 1000.0}, {"x": 4000.0, "y": 3000.0}]});
    let subs = vec![
        sub(
            "radial",
            "radial",
            json!({"centerX": 3000.0, "centerY": 2000.0, "radiusX": 1500.0, "radiusY": 1000.0, "rotation": 10.0, "feather": 0.5}),
        ),
        sub(
            "linear",
            "linear",
            json!({"startX": 0.0, "startY": 0.0, "endX": 6000.0, "endY": 4000.0, "range": 500.0}),
        ),
        sub("brush", "brush", json!({"lines": [line]})),
        sub(
            "flow",
            "flow",
            json!({"lines": [{"tool": "brush", "brushSize": 300.0, "flow": 40.0, "points": [{"x": 500.0, "y": 500.0}, {"x": 900.0, "y": 700.0}]}]}),
        ),
        sub(
            "color",
            "color",
            json!({"targetX": 1200.0, "targetY": 800.0, "tolerance": 30.0}),
        ),
        sub(
            "luma",
            "luma-range",
            json!({"rangeLow": 0.2, "rangeHigh": 0.8, "falloff": 0.1}),
        ),
        sub(
            "luminance",
            "luminance",
            json!({"targetX": 2000.0, "targetY": 1500.0, "tolerance": 25.0}),
        ),
        sub(
            "subject",
            "ai-subject",
            json!({"startX": 100.0, "startY": 100.0, "endX": 5000.0, "endY": 3800.0, "maskDataBase64": bitmap(1)}),
        ),
        sub(
            "foreground",
            "ai-foreground",
            json!({"maskDataBase64": bitmap(2), "subjectVersion": 2}),
        ),
        sub(
            "sky",
            "ai-sky",
            json!({"maskDataBase64": bitmap(3), "grow": 5.0, "feather": 10.0}),
        ),
        sub("people", "ai-people", json!({"maskDataBase64": bitmap(4)})),
        sub(
            "depth",
            "ai-depth",
            json!({"minDepth": 0.1, "maxDepth": 0.6, "minFade": 0.05, "maxFade": 0.05, "maskDataBase64": bitmap(5)}),
        ),
        sub("all", "all", json!({})),
    ];
    let layers: Vec<Value> = subs
        .into_iter()
        .enumerate()
        .map(|(i, sub)| {
            json!({"id": format!("layer{i}"), "name": format!("Layer {i}"), "visible": true,
                   "invert": false, "opacity": 100.0, "maskVersion": 4,
                   "adjustments": {"exposure": 0.15 * ((i % 5) as f64 - 2.0), "saturation": 5.0 * i as f64},
                   "subMasks": [sub]})
        })
        .collect();
    json!({"version": 1, "rating": 0, "tags": [],
           "adjustments": {"exposure": 0.2, "masks": layers}})
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

#[test]
#[ignore = "needs RAWTONIC_RAW and a GPU"]
fn a_copied_session_renders_every_mask_kind_byte_identical() {
    let Some(raw) = std::env::var_os("RAWTONIC_RAW").map(PathBuf::from) else {
        eprintln!("SKIP: RAWTONIC_RAW is not set");
        return;
    };
    let work = tempfile::tempdir().unwrap();
    let lib = Library::create(&work.path().join("Studio/Job"), "Job", Mode::Session).unwrap();
    let session = lib.directory.clone();
    let photo = session.join("Capture").join(raw.file_name().unwrap());
    fs::copy(&raw, &photo).unwrap();
    let asset = lib.add_asset(&photo).unwrap();
    lib.save_editor_state(&asset.id, &every_mask_kind())
        .unwrap();
    // The recipe the export reads is the session's, with every layer.
    let state = lib.editor_state(&asset.id).unwrap().unwrap();
    assert_eq!(state["adjustments"]["masks"].as_array().unwrap().len(), 13);
    lib.checkpoint().unwrap();
    drop(lib);

    let original = work.path().join("original.tiff");
    rawtonic(&[
        Path::new("export"),
        &photo,
        Path::new("--output"),
        &original,
    ]);
    // Without the masks it renders differently: they took part.
    let plain = work.path().join("plain.tiff");
    let plain_photo = work.path().join(raw.file_name().unwrap());
    fs::copy(&raw, &plain_photo).unwrap();
    rawtonic(&[
        Path::new("export"),
        &plain_photo,
        Path::new("--output"),
        &plain,
    ]);
    assert_ne!(fs::read(&original).unwrap(), fs::read(&plain).unwrap());

    // The whole closed session copied to another place (/dev/shm: another
    // volume on Linux), the original then gone.
    let shm = Path::new("/dev/shm");
    let other = if shm.is_dir() {
        tempfile::tempdir_in(shm).unwrap()
    } else {
        tempfile::tempdir().unwrap()
    };
    let copy = other.path().join("Archive/Job copy");
    copy_tree(&session, &copy);
    fs::rename(&session, work.path().join("gone")).unwrap();

    let moved = work.path().join("moved.tiff");
    let photo_there = copy.join("Capture").join(raw.file_name().unwrap());
    rawtonic(&[
        Path::new("export"),
        &photo_there,
        Path::new("--output"),
        &moved,
    ]);
    let (a, b) = (fs::read(&original).unwrap(), fs::read(&moved).unwrap());
    assert!(
        a == b,
        "the copied session renders differently ({} vs {} bytes)",
        a.len(),
        b.len()
    );
    eprintln!(
        "byte-identical: {} bytes, {} mask layers, copy on {}",
        a.len(),
        13,
        copy.display()
    );
}
