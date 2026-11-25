//! Native headless export regression using the desktop binary.
//!
//! Run on a host with a GPU. Set `TAURI_CONFIG`'s `identifier` and
//! `app.appDirectoriesOverride` to the values asserted by the selected test:
//! `cargo test --workspace --locked --test desktop_export TEST_NAME -- --ignored`.
#![cfg(unix)]

use rawtonic_export::existing_files::{ExportRecords, Outcome};
use rawtonic_fs::Canonical;
use std::fs::{self, File};
use std::io::{Cursor, Read};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use tauri::utils::config::AppDirectoriesOverride;

fn assert_isolated_app(identifier: &str, root: &str) {
    let context: tauri::Context<tauri::Wry> = tauri::generate_context!();
    assert_eq!(context.config().identifier, identifier);
    assert_eq!(
        context.config().app.app_directories_override,
        Some(AppDirectoriesOverride::Root(PathBuf::from(root)))
    );
}

#[test]
#[ignore = "requires a GPU, access to the model pack, and an isolated cc.rawtonic.qa.src1 build"]
fn super_resolution_flag_matches_the_export_recipe() {
    assert_isolated_app("cc.rawtonic.qa.src1", "$TEMP/src1-app-data");
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonical().unwrap();
    let source = root.join("photo.png");
    let recipes = root.join("recipes.json");
    let original = image::RgbImage::from_fn(32, 24, |x, y| {
        image::Rgb([(x * 7) as u8, (y * 9) as u8, 128])
    });
    let mut encoded = Cursor::new(Vec::new());
    original
        .write_to(&mut encoded, image::ImageFormat::Png)
        .unwrap();
    rawtonic_fs::atomic_write(&source, encoded.get_ref()).unwrap();
    rawtonic_fs::atomic_write(
        &recipes,
        br#"[{"format":"png","settings":{"jpegQuality":90,"keepMetadata":false,"stripGps":false,"nameTemplate":"enlarged","superResolution":true,"superResolutionQuality":"fast"}}]"#,
    )
    .unwrap();

    let data = root.join("src1-app-data");
    fs::create_dir(&data).unwrap();
    let run = |output: &Path, options: &[&str]| {
        let result = Command::new(env!("CARGO_BIN_EXE_rawtonic-app"))
            .arg("export")
            .arg(&source)
            .arg("--output")
            .arg(output)
            .args(options)
            .env("TMPDIR", &root)
            .output()
            .unwrap();
        let log = fs::read_to_string(data.join("logs/app.log")).unwrap_or_default();
        assert!(
            result.status.success(),
            "{}\n{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr),
            log.lines().rev().take(12).collect::<Vec<_>>().join("\n")
        );
        assert!(log.contains("Super Resolution (Fast)"), "{log}");
    };
    let flag_output = root.join("enlarged.png");
    run(
        &flag_output,
        &["--format", "png", "--super-resolution-quality", "fast"],
    );
    assert_eq!(
        image::open(&flag_output).unwrap().to_rgb8().dimensions(),
        (64, 48)
    );
    let recipe_output = root.join("recipe");
    run(&recipe_output, &["--recipes", recipes.to_str().unwrap()]);
    assert_eq!(
        fs::read(&flag_output).unwrap(),
        fs::read(recipe_output.join("enlarged.png")).unwrap()
    );
    assert_eq!(fs::read(&source).unwrap(), *encoded.get_ref());
}

#[test]
#[ignore = "requires a GPU and the isolated desktop QA build described above"]
fn replace_if_changed_through_the_desktop_worker() {
    assert_isolated_app("cc.rawtonic.qa.exs2", "$TEMP/exs2-app-data");

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

    let run = |explicit: Option<&Path>, exposure: f64, expected: Option<(Outcome, &str)>| {
        rawtonic_fs::atomic_write(
            &adjustments,
            serde_json::to_vec(&serde_json::json!({"exposure": exposure})).unwrap(),
        )
        .unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_rawtonic-app"));
        command
            .arg("export")
            .arg(&source)
            .arg("--output")
            .arg(explicit.unwrap_or(&output))
            .arg("--adjustments")
            .arg(&adjustments)
            .args(["--jobs", "1"])
            .env("TMPDIR", &root)
            .env("RUST_LOG", "debug");
        if explicit.is_some() {
            command.args(["--format", "png", "--on-conflict", "replace-if-changed"]);
        } else {
            command.arg("--recipes").arg(&recipes);
        }
        let result = command.output().unwrap();
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
    run(None, 0.0, None);
    let first = fs::read(&own).unwrap();
    let mut open_first = File::open(&own).unwrap();
    let first_inode = open_first.metadata().unwrap().ino();
    let records =
        ExportRecords::open(&rawtonic_export::existing_files::records_path(&data)).unwrap();
    let first_record = records.get(&own).unwrap().unwrap();

    run(None, 0.0, Some((Outcome::SkippedUnchanged, "delivery.png")));
    assert_eq!(fs::read(&own).unwrap(), first);
    assert_eq!(fs::metadata(&own).unwrap().ino(), first_inode);
    assert_eq!(records.get(&own).unwrap().unwrap(), first_record);
    assert!(!numbered.exists());

    run(None, 0.5, Some((Outcome::Replaced, "delivery.png")));
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
    run(None, 1.0, Some((Outcome::KeptBoth, "delivery_1.png")));
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

    let save_as = output.join("chosen-name.png");
    let sibling = output.join("chosen-name_1.png");
    rawtonic_fs::atomic_write(&save_as, b"another program's file").unwrap();
    assert!(records.get(&save_as).unwrap().is_none());
    let before = fs::read_dir(&output).unwrap().count();
    run(
        Some(&save_as),
        1.0,
        Some((Outcome::KeptBoth, "chosen-name_1.png")),
    );
    assert_eq!(fs::read(&save_as).unwrap(), b"another program's file");
    assert_eq!(fs::read_dir(&output).unwrap().count(), before + 1);
    assert_eq!(
        image::open(&sibling).unwrap().to_rgb8().dimensions(),
        original.dimensions()
    );
    assert!(
        records
            .get(&sibling)
            .unwrap()
            .unwrap()
            .is_export_of(&sibling, &first_record.source)
    );
    assert_eq!(fs::read(&source).unwrap(), *encoded.get_ref());
}
