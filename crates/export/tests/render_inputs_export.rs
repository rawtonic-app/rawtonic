//! The render-input recorder as `FileExporter::export` wires it. The coverage
//! test (`render_inputs_coverage.rs`) rebuilds the export preparation on the
//! CPU and calls the recorder directly, so it would keep passing if a
//! production hook were removed. This one exports through the real path and
//! checks that every stage records, that a recorder reused for several
//! exports holds only the last one's entries (more, fewer and zero masks) and
//! that a reused recorder equals a fresh one.
//!
//! Needs a GPU, hence opt-in:
//!
//! cargo test -p rawtonic-export --release --test render_inputs_export -- --ignored
//!
//! The lensfun database is the checkout's `src-tauri/lensfun_db` unless
//! RAWTONIC_LENSFUN_DB names one.

use std::fs;
use std::path::{Path, PathBuf};

use image::{Rgb, RgbImage};
use rawtonic_engine::image_loader::BaseImageOptions;
use rawtonic_export::export_processing::ExportSettings;
use rawtonic_export::file_export::{FileExport, FileExporter};
use rawtonic_export::render_inputs::RenderInputs;
use serde_json::{Value, json};

fn settings() -> ExportSettings {
    serde_json::from_value(json!({
        "jpegQuality": 90, "resize": null, "keepMetadata": false, "colorSpace": "srgb",
        "stripGps": false, "filenameTemplate": null, "watermark": null,
    }))
    .unwrap()
}

fn lensfun_db() -> PathBuf {
    std::env::var_os("RAWTONIC_LENSFUN_DB")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../src-tauri/lensfun_db"))
}

fn radial(id: &str, exposure: f64) -> Value {
    json!({
        "id": id, "name": id, "visible": true, "invert": false, "opacity": 100,
        "adjustments": {"exposure": exposure},
        "subMasks": [{
            "id": format!("{id}-sub"), "name": "Radial", "type": "radial", "visible": true,
            "invert": false, "opacity": 100, "mode": "additive",
            "parameters": {"centerX": 48, "centerY": 32, "radiusX": 20, "radiusY": 16,
                           "rotation": 0, "feather": 0.5},
        }],
    })
}

fn recipe(masks: &[Value]) -> Value {
    json!({"exposure": 0.2, "masks": masks})
}

fn export(exporter: &FileExporter, source: &Path, output: &Path, recipe: &Value) {
    exporter
        .export(&FileExport {
            source,
            output,
            format: "tiff",
            recipe: Some(recipe.clone()),
            settings: settings(),
            base: BaseImageOptions::default(),
            catalog: None,
            capture: None,
            library: None,
        })
        .unwrap();
}

fn mask_bitmaps(snapshot: &Value) -> usize {
    snapshot["mask_bitmaps"]
        .as_object()
        .map_or(0, |bitmaps| bitmaps.len())
}

#[test]
#[ignore = "needs a GPU"]
fn the_recorder_of_the_real_export_path_holds_only_the_last_export() {
    // Renders on a thread like the CLI's and the app's render workers.
    rawtonic_runtime::render_thread("recorded-exports")
        .spawn(recorded_exports)
        .unwrap()
        .join()
        .unwrap();
}

fn recorded_exports() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("photo.png");
    RgbImage::from_fn(96, 64, |x, y| {
        Rgb([(x * 2) as u8, (y * 3) as u8, 90 + ((x + y) % 40) as u8])
    })
    .save(&source)
    .unwrap();

    let inputs = RenderInputs::new();
    let recorded = FileExporter::new(&lensfun_db())
        .unwrap()
        .with_render_inputs(inputs.clone());
    let fresh = |recipe: &Value| {
        let inputs = RenderInputs::new();
        let exporter = FileExporter::new(&lensfun_db())
            .unwrap()
            .with_render_inputs(inputs.clone());
        export(&exporter, &source, &dir.path().join("fresh.tif"), recipe);
        let _ = fs::remove_file(dir.path().join("fresh.tif"));
        inputs.snapshot()
    };

    // More, fewer and zero masks through one recorder.
    let mut counts = Vec::new();
    for (index, masks) in [
        vec![radial("a", 1.0), radial("b", -1.0)],
        vec![radial("a", 1.0)],
        vec![],
        vec![radial("a", 1.0), radial("b", -1.0)],
    ]
    .into_iter()
    .enumerate()
    {
        let recipe = recipe(&masks);
        let output = dir.path().join(format!("out{index}.tif"));
        export(&recorded, &source, &output, &recipe);
        let snapshot = inputs.snapshot();
        for stage in ["decode", "source", "geometry", "render"] {
            assert!(
                !snapshot[stage].is_null(),
                "export {index}: the {stage} stage recorded nothing"
            );
        }
        assert_eq!(
            snapshot["render"]["mask_count"].as_u64().unwrap() as usize,
            masks.len(),
            "export {index}: uniform mask count"
        );
        assert_eq!(
            mask_bitmaps(&snapshot),
            masks.len(),
            "export {index}: mask bitmaps of an earlier export remain"
        );
        // A reused recorder equals a fresh one for the same export.
        assert_eq!(snapshot, fresh(&recipe), "export {index}: reused ≠ fresh");
        counts.push(mask_bitmaps(&snapshot));
    }
    assert_eq!(counts, [2, 1, 0, 2]);
}
