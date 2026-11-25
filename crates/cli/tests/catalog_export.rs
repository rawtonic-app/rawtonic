//! CLI forwards catalog inputs to the shared export encoder; compare its encoded output.
//! Run explicitly on a GPU host: cargo test -p rawtonic-cli --test catalog_export -- --ignored
use rawtonic_catalog::{Library, Mode};
use rawtonic_export::export_processing::{ExportSettings, TiffBitDepth};
use rawtonic_export::file_export::{FileExport, FileExporter};
use std::path::Path;
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
#[ignore = "needs a GPU, as the shared app export does"]
fn catalog_export_matches_the_shared_app_export() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("external.tiff");
    image::RgbImage::from_fn(24, 16, |x, y| {
        image::Rgb([(x * 9) as u8, (y * 13) as u8, 80])
    })
    .save(&source)
    .unwrap();
    let lib = Library::create(&dir.path().join("catalog"), "catalog", Mode::Catalog).unwrap();
    let id = lib.add_asset(&source).unwrap().id;
    let metadata = rawtonic_metadata::Metadata {
        title: "Catalog title".into(),
        creator: "Photographer".into(),
        rating: 4,
        ..Default::default()
    };
    lib.set_metadata(&id, &metadata).unwrap();
    // Simulate a returned Edit With render whose external editor stripped XMP.
    lib.mark_generated_provenance(&id, "compositeWithTrainedAlgorithmicMedia")
        .unwrap();
    let correction =
        rawtonic_catalog::capture_time::time("2026-10-31 14:05:09", Some("66"), Some("+02:00"))
            .unwrap();
    lib.set_capture_times(&[rawtonic_catalog::capture_time::CaptureChange {
        asset_id: id.clone(),
        expected: None,
        new: Some(correction.clone()),
    }])
    .unwrap();
    let recipe = serde_json::json!({"exposure": 0.7, "renderVersion": 9});
    lib.save_editor_state(
        &id,
        &serde_json::json!({"version": 1, "rating": 4, "adjustments": recipe}),
    )
    .unwrap();
    let lensfun = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../src-tauri/lensfun_db");
    let cli = dir.path().join("cli.tiff");
    let result = Command::new(env!("CARGO_BIN_EXE_rawtonic"))
        .arg("export")
        .arg(&source)
        .arg("--catalog")
        .arg(&lib.directory)
        .arg("--output")
        .arg(&cli)
        .arg("--keep-metadata")
        .env("RAWTONIC_LENSFUN_DB", &lensfun)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let app = dir.path().join("app.tiff");
    let mut settings = settings();
    settings.keep_metadata = true;
    settings.preserve_timestamps = true;
    FileExporter::new(&lensfun)
        .unwrap()
        .export(&FileExport {
            source: &source,
            output: &app,
            format: "tiff",
            recipe: Some(recipe),
            settings,
            base: Default::default(),
            catalog: Some(lib.metadata(&id).unwrap()),
            capture: Some(correction.exif_fields()),
            library: Some(&lib.directory),
            publish: rawtonic_export::output::Publish::New,
            source_generated: true,
        })
        .unwrap();
    assert!(
        std::fs::read(&cli).unwrap() == std::fs::read(&app).unwrap(),
        "CLI and shared app encoded TIFF differ"
    );
    let encoded = std::fs::read(&cli).unwrap();
    let packet = String::from_utf8_lossy(&encoded);
    assert!(packet.contains("compositeWithTrainedAlgorithmicMedia"));
    assert!(packet.contains("Catalog title"));
    // Automatic lookup reads the actual app registry schema in an isolated home.
    let home = dir.path().join("home");
    let base = if cfg!(target_os = "macos") {
        home.join("Library/Application Support")
    } else {
        home.join("data")
    };
    let data = base.join("cc.rawtonic.editor");
    std::fs::create_dir_all(&data).unwrap();
    let registry = data.join("settings.json");
    std::fs::write(
        &registry,
        serde_json::json!({"recentLibraries": [lib.directory]}).to_string(),
    )
    .unwrap();
    let auto = dir.path().join("auto.tiff");
    let run_auto = |output: &Path| {
        Command::new(env!("CARGO_BIN_EXE_rawtonic"))
            .arg("export")
            .arg(&source)
            .arg("--output")
            .arg(output)
            .arg("--keep-metadata")
            .env("RAWTONIC_LENSFUN_DB", &lensfun)
            .env("HOME", &home)
            .env("XDG_DATA_HOME", &base)
            .env("APPDATA", &base)
            .output()
            .unwrap()
    };
    let result = run_auto(&auto);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(
        std::fs::read(&auto).unwrap() == std::fs::read(&app).unwrap(),
        "Registry and shared app encoded TIFF differ"
    );
    let other = Library::create(&dir.path().join("other"), "other", Mode::Catalog).unwrap();
    other.add_asset(&source).unwrap();
    std::fs::write(
        &registry,
        serde_json::json!({"recentLibraries": [lib.directory, other.directory]}).to_string(),
    )
    .unwrap();
    let ambiguous = dir.path().join("ambiguous.tiff");
    let result = run_auto(&ambiguous);
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("--catalog"));
    assert!(!ambiguous.exists());
}
