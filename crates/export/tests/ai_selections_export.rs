//! A never-opened photo whose recipe holds Sky and Subject selections
//! without their bitmap (a Lightroom import) exports with the selections
//! computed, byte-identical to the export of the recipe that holds them (a
//! photo opened in Develop, or exported before: the app keeps the result);
//! without the models the export fails and writes nothing. Opt-in, as it
//! needs a RAW, a GPU and the app's Sky and Subject models:
//!
//! RAWTONIC_RAW=/path/photo.ORF RAWTONIC_MODELS=~/Library/Application\ Support/cc.rawtonic.qa/models \
//!   cargo test -p rawtonic-export --release --test ai_selections_export -- --ignored
//!
//! The lensfun database is the checkout's `src-tauri/lensfun_db` unless
//! RAWTONIC_LENSFUN_DB names one.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rawtonic_engine::ai_selections::{
    AutoSelection, SelectionDetector, detect_missing, undetected,
};
use rawtonic_engine::image_loader::{BaseImageOptions, load_base_image_measured};
use rawtonic_export::export_processing::ExportSettings;
use rawtonic_export::file_export::{FileExport, FileExporter};
use serde_json::{Value, json};

struct Models(rawtonic_segment::Models);

impl SelectionDetector for Models {
    fn detect(
        &self,
        selection: AutoSelection,
        version: u32,
        warped: &image::DynamicImage,
        linear: bool,
    ) -> Result<String, String> {
        let mask = match selection {
            AutoSelection::Sky => self
                .0
                .sky()
                .and_then(|m| rawtonic_segment::run_sky_seg_model(warped, &m, linear, None)),
            AutoSelection::Subject => self.0.subject().and_then(|m| {
                rawtonic_segment::subject_selection(warped, &m, linear, None, version)
            }),
        }
        .map_err(|e| format!("{e:#}"))?;
        rawtonic_segment::mask_data_url(&mask)
    }
}

/// Lightroom's Select Sky and Select Subject as the import translates them,
/// with a straightened, transformed photo.
fn imported() -> Value {
    json!({"exposure": 0.3, "rotation": 1.2, "transformVertical": 8.0, "masks": [
        {"id": "sky", "name": "Sky", "visible": true, "opacity": 100.0, "invert": false, "adjustments": {"exposure": -1.5, "saturation": 30},
         "subMasks": [{"id": "s", "type": "ai-sky", "visible": true, "opacity": 100.0, "invert": false, "mode": "additive",
                       "parameters": {"maskDataBase64": null, "grow": 0, "feather": 0}}]},
        {"id": "subject", "name": "Subject", "visible": true, "opacity": 100.0, "invert": false, "adjustments": {"exposure": 1.0},
         "subMasks": [{"id": "f", "type": "ai-foreground", "visible": true, "opacity": 100.0, "invert": false, "mode": "additive",
                       "parameters": {"maskDataBase64": null, "grow": 0, "feather": 0, "subjectVersion": 2}}]}]})
}

fn settings() -> ExportSettings {
    serde_json::from_value(json!({
        "jpegQuality": 90, "resize": null, "keepMetadata": false, "colorSpace": "srgb",
        "stripGps": false, "filenameTemplate": null, "watermark": null,
    }))
    .unwrap()
}

fn export(
    exporter: &FileExporter,
    source: &Path,
    output: &Path,
    recipe: &Value,
) -> Result<Vec<u8>, String> {
    exporter.export(&FileExport {
        source,
        output,
        format: "tiff",
        recipe: Some(recipe.clone()),
        settings: settings(),
        base: BaseImageOptions::default(),
        catalog: None,
        capture: None,
    })?;
    Ok(fs::read(output).unwrap())
}

#[test]
#[ignore = "needs RAWTONIC_RAW, RAWTONIC_MODELS and a GPU"]
fn a_never_opened_photo_exports_as_the_recipe_holding_its_selections() {
    // The export render keeps large frames on the stack (the CLI's and the
    // app's workers have 8 MiB).
    std::thread::Builder::new()
        .stack_size(8 << 20)
        .spawn(never_opened_as_kept)
        .unwrap()
        .join()
        .unwrap();
}

fn never_opened_as_kept() {
    let raw = PathBuf::from(std::env::var_os("RAWTONIC_RAW").expect("RAWTONIC_RAW"));
    let models = PathBuf::from(std::env::var_os("RAWTONIC_MODELS").expect("RAWTONIC_MODELS"));
    let lensfun = std::env::var_os("RAWTONIC_LENSFUN_DB")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../src-tauri/lensfun_db")
        });
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join(raw.file_name().unwrap());
    fs::copy(&raw, &source).unwrap();
    let recipe = imported();

    // Without models: refused, nothing written.
    let bare = FileExporter::new(&lensfun).unwrap();
    let error = export(&bare, &source, &dir.path().join("bare.tif"), &recipe).unwrap_err();
    assert!(error.contains("layer “Sky”: its Sky selection"), "{error}");
    assert!(!dir.path().join("bare.tif").exists());
    let missing = FileExporter::new(&lensfun)
        .unwrap()
        .with_selection_detector(Arc::new(Models(rawtonic_segment::Models::new(dir.path()))));
    let error = export(&missing, &source, &dir.path().join("missing.tif"), &recipe).unwrap_err();
    assert!(error.contains("is not installed"), "{error}");
    assert!(!dir.path().join("missing.tif").exists());

    // With them: the selections are computed at export.
    let detector = Arc::new(Models(rawtonic_segment::Models::new(&models)));
    let exporter = FileExporter::new(&lensfun)
        .unwrap()
        .with_selection_detector(detector.clone());
    let never_opened = export(&exporter, &source, &dir.path().join("never.tif"), &recipe).unwrap();

    // The recipe as kept afterwards (or as the editor saves it on open):
    // the same decode, the same detection, stored in its components.
    let mut kept = recipe.clone();
    rawtonic_export::exif_processing::prepare_recipe_lens(&mut kept, &source).unwrap();
    let path = source.to_string_lossy().to_string();
    let base = BaseImageOptions::default().for_recipe(&kept, &path);
    let bytes = fs::read(&source).unwrap();
    let (pristine, _) = load_base_image_measured(&bytes, &path, false, &base, None).unwrap();
    let detected = detect_missing(
        &mut kept,
        &Arc::new(pristine),
        rawtonic_engine::formats::is_raw_file(&path),
        Some(detector.as_ref()),
        &mut || {},
    )
    .unwrap();
    assert_eq!(detected.len(), 2);
    assert!(undetected(&kept).is_empty());
    for layer in kept["masks"].as_array().unwrap() {
        let data = layer["subMasks"][0]["parameters"]["maskDataBase64"]
            .as_str()
            .unwrap();
        assert!(data.starts_with("data:image/png;base64,"));
    }
    let opened = export(
        &FileExporter::new(&lensfun).unwrap(),
        &source,
        &dir.path().join("kept.tif"),
        &kept,
    )
    .unwrap();
    assert!(
        never_opened == opened,
        "never opened and opened must export the same bytes"
    );

    // The selections change the render (the test would pass without them).
    let mut without = recipe.clone();
    for layer in without["masks"].as_array_mut().unwrap() {
        layer["visible"] = json!(false);
    }
    let plain = export(&bare, &source, &dir.path().join("plain.tif"), &without).unwrap();
    assert!(
        plain != never_opened,
        "the selections must change the render"
    );
}
