//! A whole export fits in `RENDER_FRAME_BUDGET` of stack, driver included
//! (gap G74). The app's export lanes once ran on Rust's 2 MiB default while
//! the render's own frames used 2.0 MB of it, so every app export on AMD's
//! Linux driver (RADV) aborted with a stack overflow. Render threads now get
//! `RENDER_STACK_SIZE` (8 MiB); this test renders on a thread of an eighth
//! of that, so a frame that grows again fails here (the test process aborts
//! with "has overflowed its stack"), not in a user's export. Opt-in, as it
//! needs a GPU:
//!
//! cargo test -p rawtonic-export --test export_stack -- --ignored

use std::fs;
use std::path::Path;

use image::{ImageBuffer, Rgb};
use rawtonic_engine::image_loader::BaseImageOptions;
use rawtonic_export::export_processing::ExportSettings;
use rawtonic_export::file_export::{FileExport, FileExporter};
use serde_json::{Value, json};

fn settings() -> ExportSettings {
    serde_json::from_value(json!({
        "jpegQuality": 90, "resize": null, "keepMetadata": false, "colorSpace": "srgb",
        "stripGps": false, "filenameTemplate": null, "watermark": null,
    }))
    .unwrap()
}

fn layer(id: &str, adjustments: Value) -> Value {
    json!({"id": id, "name": id, "visible": true, "invert": false, "opacity": 100,
        "subMasks": [{"id": format!("{id}-all"), "type": "all", "visible": true,
                      "mode": "additive", "parameters": {}}],
        "adjustments": adjustments})
}

/// Global tone, detail, effects and two local layers: the stages a typical
/// edited photo runs through.
fn recipe() -> Value {
    json!({
        "exposure": 0.4, "contrast": 20, "highlights": -30, "shadows": 25,
        "clarity": 15, "dehaze": 10, "saturation": 12,
        "sharpness": 40, "lumaNoiseReduction": 20, "colorNoiseReduction": 25,
        "vignetteAmount": -20, "grainAmount": 15,
        "masks": [layer("warm", json!({"temperature": 10, "exposure": -0.3})),
                  layer("bright", json!({"exposure": 0.5, "clarity": 20}))],
    })
}

fn export_on_budget_stack(source: &Path, output: &Path) {
    let lensfun = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../src-tauri/lensfun_db");
    let exporter = FileExporter::new(&lensfun).unwrap();
    exporter
        .export(&FileExport {
            source,
            output,
            format: "tiff",
            recipe: Some(recipe()),
            settings: settings(),
            base: BaseImageOptions::default(),
            catalog: None,
            capture: None,
            library: None,
        })
        .unwrap();
}

#[test]
#[ignore = "needs a GPU"]
fn a_whole_export_fits_in_the_render_frame_budget() {
    let dir = tempfile::tempdir().unwrap();
    // Larger than one render tile in both directions.
    let (width, height) = (3000, 2200);
    let photo = ImageBuffer::from_fn(width, height, |x, y| {
        Rgb([
            (x * 65535 / width) as u16,
            (y * 65535 / height) as u16,
            (((x + y) * 37) % 65536) as u16,
        ])
    });
    let source = dir.path().join("photo.png");
    photo.save(&source).unwrap();
    let output = dir.path().join("photo.tif");
    let (source_path, output_path) = (source.clone(), output.clone());
    std::thread::Builder::new()
        .name("export-stack-budget".into())
        .stack_size(rawtonic_runtime::RENDER_FRAME_BUDGET)
        .spawn(move || export_on_budget_stack(&source_path, &output_path))
        .unwrap()
        .join()
        .unwrap();
    let written = image::open(&output).unwrap();
    assert_eq!((written.width(), written.height()), (width, height));
    assert!(fs::metadata(&output).unwrap().len() > 0);
}
