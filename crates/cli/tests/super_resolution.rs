use std::io::Cursor;
use std::path::Path;
use std::process::Command;

#[test]
fn super_resolution_with_no_model_reports_how_to_install_it() {
    let work = tempfile::tempdir().unwrap();
    let source = work.path().join("photo.png");
    let output = work.path().join("enlarged.png");
    let models = work.path().join("models");
    std::fs::create_dir(&models).unwrap();
    let mut bytes = Cursor::new(Vec::new());
    image::RgbImage::from_pixel(32, 24, image::Rgb([70, 100, 140]))
        .write_to(&mut bytes, image::ImageFormat::Png)
        .unwrap();
    rawtonic_fs::atomic_write(&source, bytes.into_inner()).unwrap();
    let lensfun = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../src-tauri/lensfun_db");

    let result = Command::new(env!("CARGO_BIN_EXE_rawtonic"))
        .arg("export")
        .arg(source)
        .arg("--output")
        .arg(&output)
        .args(["--format", "png", "--super-resolution-quality", "fast"])
        .arg("--lensfun-db")
        .arg(lensfun)
        .arg("--models")
        .arg(models)
        // Never the user's own app data (its recent libraries may be of a
        // newer schema), as in catalog_export.rs.
        .env("HOME", work.path())
        .env("XDG_DATA_HOME", work.path())
        .env("APPDATA", work.path())
        .output()
        .unwrap();

    let error = String::from_utf8_lossy(&result.stderr);
    assert_eq!(result.status.code(), Some(1), "{error}");
    assert!(error.contains("Super Resolution"), "{error}");
    assert!(
        error.contains("scripts/models/install_models.sh"),
        "{error}"
    );
    assert!(!output.exists());
}
