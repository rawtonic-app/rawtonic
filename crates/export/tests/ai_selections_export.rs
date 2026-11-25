//! A never-opened photo whose recipe holds Sky, Subject, People or Landscape
//! selections without their bitmap (a Lightroom import, an adaptive preset
//! applied in the Library) exports with the selections
//! computed, byte-identical to the export of the recipe that holds the
//! masks this export path computed (as the app keeps them after an export;
//! Develop's and the selection queue's masks are not compared here);
//! without the models the export fails and writes nothing. Opt-in, as it
//! needs a RAW, a GPU and the selection models:
//!
//! RAWTONIC_RAW=/path/photo.ORF RAWTONIC_MODELS=~/Library/Application\ Support/cc.rawtonic.qa/models \
//!   cargo test -p rawtonic-export --release --test ai_selections_export -- a_never_opened --ignored
//!
//! For Landscape, use a RAW with sky and replace `a_never_opened` with `landscape`.
//! Set ORT_DYLIB_PATH if ONNX Runtime is outside the loader's search path.
//!
//! The lensfun database is the checkout's `src-tauri/lensfun_db` unless
//! RAWTONIC_LENSFUN_DB names one.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rawtonic_engine::ai_selections::{
    AutoSelection, LandscapeParts, SelectionDetector, detect_missing, undetected,
};
use rawtonic_engine::image_loader::{BaseImageOptions, load_base_image_measured};
use rawtonic_export::export_processing::ExportSettings;
use rawtonic_export::file_export::{FileExport, FileExporter};
use serde_json::{Value, json};

struct Models {
    segment: rawtonic_segment::Models,
    faces: Option<rawtonic_faces::FaceModels>,
}

impl Models {
    fn new(dir: &Path) -> Self {
        Self {
            segment: rawtonic_segment::Models::new(dir),
            faces: rawtonic_faces::FaceModels::load(dir).ok(),
        }
    }
}

impl SelectionDetector for Models {
    fn detect(
        &self,
        selection: AutoSelection,
        parameters: &Value,
        warped: &image::DynamicImage,
        linear: bool,
    ) -> Result<String, String> {
        use rawtonic_engine::ai_selections as sel;
        let mask = match selection {
            AutoSelection::Sky => self
                .segment
                .sky()
                .and_then(|m| rawtonic_segment::run_sky_seg_model(warped, &m, linear, None)),
            AutoSelection::Subject => self.segment.subject().and_then(|m| {
                rawtonic_segment::subject_selection(
                    warped,
                    &m,
                    linear,
                    None,
                    sel::subject_version(parameters),
                )
            }),
            AutoSelection::People => {
                let faces = self
                    .faces
                    .as_ref()
                    .ok_or("the face models are not installed")?;
                let mask = rawtonic_faces::recipe::people_selection(
                    warped,
                    linear,
                    faces,
                    &sel::people_parts(parameters),
                    sel::people_person(parameters),
                )?;
                return rawtonic_segment::mask_data_url(&mask);
            }
            other => return Err(format!("{} is not tested here", other.name())),
        }
        .map_err(|e| format!("{e:#}"))?;
        rawtonic_segment::mask_data_url(&mask)
    }

    fn detect_landscape(
        &self,
        sets: &[LandscapeParts],
        warped: &image::DynamicImage,
        linear: bool,
    ) -> Result<Vec<String>, String> {
        let model = self.segment.landscape().map_err(|e| format!("{e:#}"))?;
        let probabilities = rawtonic_segment::landscape_probabilities(warped, &model, linear, None)
            .map_err(|e| format!("{e:#}"))?;
        sets.iter()
            .map(|set| {
                let parts =
                    rawtonic_segment::landscape_parts(&set.keys()).map_err(|e| format!("{e:#}"))?;
                rawtonic_segment::mask_data_url(&rawtonic_segment::landscape_mask_grid(
                    &probabilities,
                    &parts,
                    set.version,
                ))
            })
            .collect()
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
                       "parameters": {"maskDataBase64": null, "grow": 0, "feather": 0, "subjectVersion": 2}}]},
        {"id": "skin", "name": "Skin", "visible": true, "opacity": 100.0, "invert": false, "adjustments": {"clarity": -20},
         "subMasks": [{"id": "p", "type": "ai-people", "visible": true, "opacity": 100.0, "invert": false, "mode": "additive",
                       "parameters": {"maskDataBase64": null, "detectPending": true, "grow": 0, "feather": 0,
                                      "parts": ["faceSkin", "bodySkin"], "person": null}}]}]})
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
        library: None,
        publish: rawtonic_export::output::Publish::New,
        source_generated: false,
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
    let lensfun = lensfun_dir();
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
        .with_selection_detector(Arc::new(Models::new(dir.path())));
    let error = export(&missing, &source, &dir.path().join("missing.tif"), &recipe).unwrap_err();
    assert!(error.contains("is not installed"), "{error}");
    assert!(!dir.path().join("missing.tif").exists());

    compare_pending_and_kept(&source, dir.path(), &lensfun, &recipe, &models);
}

#[test]
#[ignore = "needs RAWTONIC_RAW with sky, the Landscape model and a GPU"]
fn landscape_grid_exports_match_saved_bitmaps_for_both_versions() {
    let Some(models) = std::env::var_os("RAWTONIC_MODELS").map(PathBuf::from) else {
        eprintln!("SKIP: set RAWTONIC_MODELS to the model directory");
        return;
    };
    if !models.join(rawtonic_segment::LANDSCAPE_FILENAME).is_file() {
        eprintln!("SKIP: the Landscape model is not installed");
        return;
    }
    let raw = PathBuf::from(std::env::var_os("RAWTONIC_RAW").expect("RAWTONIC_RAW with sky"));
    std::thread::Builder::new()
        .stack_size(8 << 20)
        .spawn(move || {
            let dir = tempfile::tempdir().unwrap();
            let source = dir.path().join(raw.file_name().unwrap());
            fs::copy(&raw, &source).unwrap();
            let layer = |id: &str, parameters: Value| {
                json!({
                    "id": id, "name": id, "visible": true, "opacity": 100.0, "invert": false,
                    "adjustments": {"exposure": -0.75},
                    "subMasks": [{
                        "id": format!("{id}-sky"), "type": "ai-landscape", "visible": true,
                        "opacity": 100.0, "invert": false, "mode": "additive",
                        "parameters": parameters,
                    }],
                })
            };
            let legacy = json!({"parts": ["sky"], "maskDataBase64": null, "grow": 0, "feather": 0});
            let mut current = legacy.clone();
            current["landscapeVersion"] = json!(2);
            let recipe = json!({"rotation": 1.2, "transformVertical": 8.0, "masks": [
                layer("Legacy landscape", legacy), layer("Current landscape", current),
            ]});
            let kept =
                compare_pending_and_kept(&source, dir.path(), &lensfun_dir(), &recipe, &models);
            let parameters = |layer: usize| &kept["masks"][layer]["subMasks"][0]["parameters"];
            for layer in 0..2 {
                let params = parameters(layer);
                let grid = &params[rawtonic_engine::mask_generation::BITMAP_GRID_OF];
                let (w, h) = rawtonic_engine::mask_generation::ai_bitmap_dimensions(
                    params["maskDataBase64"].as_str().unwrap(),
                )
                .unwrap();
                let width = grid["width"].as_u64().expect("recorded photo width");
                let height = grid["height"].as_u64().expect("recorded photo height");
                assert!(u64::from(w) < width || u64::from(h) < height);
            }
            assert!(parameters(0).get("landscapeVersion").is_none());
            assert_eq!(parameters(1)["landscapeVersion"], 2);
            println!(
                "Both Landscape versions used saved grids and matched pending exports byte for byte"
            );
        })
        .unwrap()
        .join()
        .unwrap();
}

fn lensfun_dir() -> PathBuf {
    std::env::var_os("RAWTONIC_LENSFUN_DB")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../src-tauri/lensfun_db"))
}

fn compare_pending_and_kept(
    source: &Path,
    dir: &Path,
    lensfun: &Path,
    recipe: &Value,
    models: &Path,
) -> Value {
    let detector = Arc::new(Models::new(models));
    let exporter = FileExporter::new(lensfun)
        .unwrap()
        .with_selection_detector(detector.clone());
    let never_opened = export(&exporter, source, &dir.join("never.tif"), recipe).unwrap();

    // The recipe as kept afterwards (or as the editor saves it on open):
    // the same decode, the same detection, stored in its components.
    let mut kept = recipe.clone();
    rawtonic_export::exif_processing::prepare_recipe_lens(&mut kept, source).unwrap();
    let path = source.to_string_lossy().to_string();
    let base = BaseImageOptions::default().for_recipe(&kept, &path);
    let bytes = fs::read(source).unwrap();
    let (pristine, _) = load_base_image_measured(&bytes, &path, false, &base, None).unwrap();
    let detected = detect_missing(
        &mut kept,
        &Arc::new(pristine),
        rawtonic_engine::formats::is_raw_file(&path),
        Some(detector.as_ref()),
        &mut || {},
    )
    .unwrap();
    assert_eq!(detected.len(), recipe["masks"].as_array().unwrap().len());
    assert!(undetected(&kept).is_empty());
    for layer in kept["masks"].as_array().unwrap() {
        let data = layer["subMasks"][0]["parameters"]["maskDataBase64"]
            .as_str()
            .unwrap();
        assert!(data.starts_with("data:image/png;base64,"));
    }
    let bare = FileExporter::new(lensfun).unwrap();
    let opened = export(&bare, source, &dir.join("kept.tif"), &kept).unwrap();
    assert!(
        never_opened == opened,
        "never opened and opened must export the same bytes"
    );

    // The selections change the render (the test would pass without them).
    let mut without = recipe.clone();
    for layer in without["masks"].as_array_mut().unwrap() {
        layer["visible"] = json!(false);
    }
    let plain = export(&bare, source, &dir.join("plain.tif"), &without).unwrap();
    assert!(
        plain != never_opened,
        "the selections must change the render"
    );
    kept
}
