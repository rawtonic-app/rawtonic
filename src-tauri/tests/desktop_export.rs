//! Native headless export regression using the desktop binary.
//!
//! Run on a host with a GPU:
//! `TAURI_CONFIG='{"identifier":"cc.rawtonic.qa.exs2","app":{"appDirectoriesOverride":"$TEMP/exs2-app-data"}}' cargo test --workspace --locked --test desktop_export -- --ignored`
#![cfg(unix)]

use rawtonic_export::existing_files::{ExportRecords, Outcome};
use rawtonic_fs::Canonical;
use std::fs::{self, File};
use std::io::{Cursor, Read};
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::process::Command;
use tauri::utils::config::AppDirectoriesOverride;

#[test]
#[ignore = "requires a GPU and the isolated desktop QA build described above"]
fn replace_if_changed_through_the_desktop_worker() {
    let context: tauri::Context<tauri::Wry> = tauri::generate_context!();
    assert_eq!(context.config().identifier, "cc.rawtonic.qa.exs2");
    assert_eq!(
        context.config().app.app_directories_override,
        Some(AppDirectoriesOverride::Root(PathBuf::from(
            "$TEMP/exs2-app-data"
        )))
    );

    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonical().unwrap();
    let data = root.join("exs2-app-data");
    let output = root.join("output");
    fs::create_dir(&data).unwrap();
    let source = root.join("photo.png");
    let recipes = root.join("recipes.json");
    let adjustments = root.join("adjustments.json");
    let original = image::RgbImage::from_pixel(64, 48, image::Rgb([64, 96, 128]));
    let mut encoded = Cursor::new(Vec::new());
    original
        .write_to(&mut encoded, image::ImageFormat::Png)
        .unwrap();
    rawtonic_fs::atomic_write(&source, encoded.get_ref()).unwrap();
    rawtonic_fs::atomic_write(
        &recipes,
        br#"[{"format":"png","settings":{"jpegQuality":90,"keepMetadata":false,"stripGps":false,"nameTemplate":"delivery","existingFiles":"replaceIfChanged"}}]"#,
    )
    .unwrap();

    let run = |exposure: f64, expected: Option<(Outcome, &str)>| {
        rawtonic_fs::atomic_write(
            &adjustments,
            serde_json::to_vec(&serde_json::json!({"exposure": exposure})).unwrap(),
        )
        .unwrap();
        let result = Command::new(env!("CARGO_BIN_EXE_rawtonic-app"))
            .arg("export")
            .arg(&source)
            .arg("--output")
            .arg(&output)
            .arg("--recipes")
            .arg(&recipes)
            .arg("--adjustments")
            .arg(&adjustments)
            .args(["--jobs", "1"])
            .env("TMPDIR", &root)
            .env("RUST_LOG", "debug")
            .output()
            .unwrap();
        let log = fs::read_to_string(data.join("logs/app.log")).unwrap_or_default();
        assert!(
            result.status.success(),
            "Desktop export failed (exposure {exposure}):\n{}\n{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr),
            log.lines().rev().take(12).collect::<Vec<_>>().join("\n"),
        );
        let outcomes = log
            .lines()
            .filter_map(|line| {
                line.split_once("Export outcome ")
                    .map(|(_, outcome)| outcome)
            })
            .collect::<Vec<_>>();
        let expected = expected.map(|(outcome, name)| {
            format!("{}: {}", outcome.as_str(), output.join(name).display())
        });
        assert_eq!(
            outcomes,
            expected.as_deref().into_iter().collect::<Vec<_>>()
        );
    };

    let own = output.join("delivery.png");
    let numbered = output.join("delivery_1.png");
    run(0.0, None);
    let first = fs::read(&own).unwrap();
    let mut open_first = File::open(&own).unwrap();
    let first_inode = open_first.metadata().unwrap().ino();
    let records =
        ExportRecords::open(&rawtonic_export::existing_files::records_path(&data)).unwrap();
    let first_record = records.get(&own).unwrap().unwrap();

    run(0.0, Some((Outcome::SkippedUnchanged, "delivery.png")));
    assert_eq!(fs::read(&own).unwrap(), first);
    assert_eq!(fs::metadata(&own).unwrap().ino(), first_inode);
    assert_eq!(records.get(&own).unwrap().unwrap(), first_record);
    assert!(!numbered.exists());

    run(0.5, Some((Outcome::Replaced, "delivery.png")));
    let replacement = fs::read(&own).unwrap();
    assert_ne!(
        image::open(&own).unwrap(),
        image::load_from_memory(&first).unwrap()
    );
    // A reader holding the original file sees its complete contents after
    // the replacement, while the same path now refers to the new file.
    let mut held = Vec::new();
    open_first.read_to_end(&mut held).unwrap();
    assert_eq!(held, first);
    assert_ne!(fs::metadata(&own).unwrap().ino(), first_inode);
    assert!(!numbered.exists());
    let replacement_record = records.get(&own).unwrap().unwrap();
    assert_ne!(replacement_record.inputs, first_record.inputs);
    assert!(replacement_record.is_export_of(&own, &first_record.source));

    // Simulate an edit by another program with the same file size, so
    // recognizing the foreign contents requires checking the digest.
    let mut foreign = replacement;
    foreign[0] ^= 1;
    rawtonic_fs::atomic_write(&own, &foreign).unwrap();
    run(1.0, Some((Outcome::KeptBoth, "delivery_1.png")));
    assert_eq!(fs::read(&own).unwrap(), foreign);
    assert_eq!(records.get(&own).unwrap().unwrap(), replacement_record);
    assert!(
        records
            .get(&numbered)
            .unwrap()
            .unwrap()
            .is_export_of(&numbered, &first_record.source)
    );
    assert_eq!(
        image::open(&numbered).unwrap().to_rgb8().dimensions(),
        original.dimensions()
    );
    assert_eq!(fs::read(&source).unwrap(), *encoded.get_ref());
}
