//! A session photograph the session keeps outside its folder (the app moved
//! it to another drive, ADR 0016) exports exactly as inside the session: its
//! recipe's session camera profile (`profiles/…dcp`) resolves through the
//! session that owns it, not the folder the file is in. Opt-in, as it needs
//! a RAW and a GPU:
//!
//! RAWTONIC_RAW=/path/bayer-nef.NEF cargo test -p rawtonic-export --test outside_library -- --ignored
//!
//! The RAW is any one the decoder reads (e.g. `bayer-nef.NEF` from
//! `scripts/bench/fetch_fixtures.sh`); the lensfun database is the
//! checkout's `src-tauri/lensfun_db` unless RAWTONIC_LENSFUN_DB names one.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use rawtonic_engine::image_loader::BaseImageOptions;
use rawtonic_engine::session_layout::set_outside_photos;
use rawtonic_export::export_processing::ExportSettings;
use rawtonic_export::file_export::{FileExport, FileExporter};

/// A minimal DCP whose colour matrix visibly changes the render.
fn profile() -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(b"IIRC");
    b.extend_from_slice(&8u32.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    let matrix_at = 8 + 2 + 12 + 4;
    b.extend_from_slice(&50721u16.to_le_bytes()); // ColorMatrix1
    b.extend_from_slice(&10u16.to_le_bytes()); // SRATIONAL
    b.extend_from_slice(&9u32.to_le_bytes());
    b.extend_from_slice(&(matrix_at as u32).to_le_bytes());
    b.extend_from_slice(&0u32.to_le_bytes());
    for v in [12, 3, -1, -2, 9, 1, 1, -3, 11] {
        b.extend_from_slice(&(v as i32).to_le_bytes());
        b.extend_from_slice(&10i32.to_le_bytes());
    }
    b
}

fn settings() -> ExportSettings {
    serde_json::from_value(serde_json::json!({
        "jpegQuality": 90, "resize": null, "keepMetadata": false, "colorSpace": "srgb",
        "stripGps": false, "filenameTemplate": null, "watermark": null,
    }))
    .unwrap()
}

fn export(exporter: &FileExporter, source: &Path, output: &Path) -> Vec<u8> {
    exporter
        .export(&FileExport {
            source,
            output,
            format: "tiff",
            recipe: Some(serde_json::json!({"cameraProfile": "profiles/Test.dcp"})),
            settings: settings(),
            base: BaseImageOptions::default(),
            catalog: None,
        })
        .unwrap();
    fs::read(output).unwrap()
}

#[test]
#[ignore = "needs RAWTONIC_RAW and a GPU"]
fn a_session_profile_renders_the_same_on_a_photo_outside_the_session() {
    let raw = PathBuf::from(std::env::var_os("RAWTONIC_RAW").expect("RAWTONIC_RAW"));
    let lensfun = std::env::var_os("RAWTONIC_LENSFUN_DB")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../src-tauri/lensfun_db")
        });
    let dir = tempfile::tempdir().unwrap();
    let session = dir.path().join("Job");
    fs::create_dir_all(session.join("Capture")).unwrap();
    fs::create_dir_all(session.join("profiles")).unwrap();
    fs::write(session.join("session.json"), "{}").unwrap();
    fs::write(session.join("session.sqlite"), "").unwrap();
    fs::write(session.join("profiles/Test.dcp"), profile()).unwrap();
    let name = raw.file_name().unwrap();
    let inside = session.join("Capture").join(name);
    let drive = dir.path().join("Drive");
    fs::create_dir(&drive).unwrap();
    let outside = drive.join(name);
    fs::copy(&raw, &inside).unwrap();
    fs::copy(&raw, &outside).unwrap();
    let out = dir.path().join("out");
    fs::create_dir(&out).unwrap();

    let exporter = FileExporter::new(&lensfun).unwrap();
    let expected = export(&exporter, &inside, &out.join("inside.tif"));
    // Not the session's photograph: the session's profile is not its own.
    let foreign = export(&exporter, &outside, &out.join("foreign.tif"));
    assert_ne!(foreign, expected, "the profile must change the render");
    set_outside_photos(&session, HashSet::from([outside.canonicalize().unwrap()]));
    let owned = export(&exporter, &outside, &out.join("owned.tif"));
    set_outside_photos(&session, HashSet::new());
    assert!(
        owned == expected,
        "outside the session it must render byte-identically"
    );
}
