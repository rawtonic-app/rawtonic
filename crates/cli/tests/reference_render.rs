//! The reference render that measurement harnesses compare reduced views
//! with (`FileExporter::render_reference`) is the CLI's export: a 16-bit
//! TIFF of the same RAW and recipe holds the same pixels. A harness once
//! rendered its reference from the bare decode, without the export's warp,
//! automatic CA and edge-fringe pass, and its claims missed the real export
//! (notes: benchmark-results/tpx7-2026-11-17/REPORT.md). Opt-in, as it needs
//! a RAW and a GPU:
//!
//! RAWTONIC_RAW=/path/photo.NEF cargo test -p rawtonic-cli --release --test reference_render -- --ignored
//!
//! The RAW is any one the decoder reads (e.g. `bayer-nef.NEF` from
//! `scripts/bench/fetch_fixtures.sh`); it is read, never changed. The
//! lensfun database is the checkout's `src-tauri/lensfun_db` unless
//! RAWTONIC_LENSFUN_DB names one.

use rawtonic_export::export_processing::{ExportSettings, TiffBitDepth};
use rawtonic_export::file_export::{FileExport, FileExporter};
use std::path::{Path, PathBuf};
use std::process::Command;

fn settings() -> ExportSettings {
    ExportSettings {
        jpeg_quality: 95,
        tiff_bit_depth: TiffBitDepth::Sixteen,
        resize: None,
        border: None,
        pad: None,
        keep_metadata: false,
        color_space: Default::default(),
        preserve_timestamps: false,
        strip_gps: false,
        keep_creator_contact: false,
        filename_template: None,
        name_template: None,
        job_name: None,
        recipe_name: None,
        sequence_start: None,
        watermark: None,
        export_masks: false,
        preserve_folders: false,
        destination_type: None,
        subfolder: None,
        output_sharpening: None,
        super_resolution: false,
        provenance: None,
        existing_files: Default::default(),
        max_file_kb: None,
    }
}

#[test]
#[ignore = "needs RAWTONIC_RAW and a GPU"]
fn the_reference_render_is_the_cli_export() {
    let Some(raw) = std::env::var_os("RAWTONIC_RAW").map(PathBuf::from) else {
        eprintln!("SKIP: RAWTONIC_RAW not set");
        return;
    };
    let lensfun = std::env::var_os("RAWTONIC_LENSFUN_DB")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../src-tauri/lensfun_db")
        });
    let dir = tempfile::tempdir().unwrap();
    // The editor's new edit with colour noise reduction: lens corrections,
    // automatic CA and the edge-fringe pass as the app sets them.
    let mut recipe: serde_json::Value = serde_json::from_str(include_str!(
        "../../engine/tests/fixtures/new-edit-recipe.raw.json"
    ))
    .unwrap();
    recipe["colorNoiseReduction"] = 50.into();
    let recipe_path = dir.path().join("recipe.json");
    std::fs::write(&recipe_path, recipe.to_string()).unwrap();
    let tiff = dir.path().join("export.tif");
    let output = Command::new(env!("CARGO_BIN_EXE_rawtonic"))
        .arg("export")
        .arg(&raw)
        .arg("--output")
        .arg(&tiff)
        .args(["--tiff-bit-depth", "16", "--adjustments"])
        .arg(&recipe_path)
        .env("RAWTONIC_LENSFUN_DB", &lensfun)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "rawtonic export failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let exported = image::open(&tiff).unwrap().to_rgb16();
    let reference = FileExporter::new(&lensfun)
        .unwrap()
        .render_reference(&FileExport {
            source: &raw,
            output: &dir.path().join("unused.tif"),
            format: "tiff",
            recipe: Some(recipe),
            settings: settings(),
            base: rawtonic_engine::image_loader::BaseImageOptions::default(),
            catalog: None,
            capture: None,
            library: None,
            publish: rawtonic_export::output::Publish::New,
        })
        .unwrap()
        .to_rgb16();
    assert_eq!(exported.dimensions(), reference.dimensions());
    assert!(
        exported.as_raw() == reference.as_raw(),
        "the reference render differs from the CLI export"
    );
}
