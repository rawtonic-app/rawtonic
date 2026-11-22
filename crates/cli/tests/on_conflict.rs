//! `rawtonic export --on-conflict`: what happens when the output's name is
//! taken. Skip is decided before anything is read or rendered (the source
//! here is no photo at all, and no graphics card is needed); no choice ever
//! writes over a file that is not the photo's own recorded export.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn rawtonic(args: &[&str]) -> std::process::Output {
    let lensfun = std::env::var_os("RAWTONIC_LENSFUN_DB")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../src-tauri/lensfun_db")
        });
    Command::new(env!("CARGO_BIN_EXE_rawtonic"))
        .args(args)
        .env("RAWTONIC_LENSFUN_DB", lensfun)
        .output()
        .unwrap()
}

#[test]
fn skip_needs_no_render_and_no_choice_writes_over_a_foreign_file() {
    let dir = tempfile::tempdir().unwrap();
    // Not a photo: rendering it would fail.
    let source = dir.path().join("photo.CR3");
    fs::write(&source, b"not a photo").unwrap();
    let taken = dir.path().join("photo.jpg");
    fs::write(&taken, b"someone's file").unwrap();
    let records = dir.path().join("records.sqlite");
    let (source, taken_str, records_str) = (
        source.to_str().unwrap(),
        taken.to_str().unwrap(),
        records.to_str().unwrap(),
    );

    let skipped = rawtonic(&[
        "export",
        source,
        "--output",
        taken_str,
        "--on-conflict",
        "skip",
    ]);
    assert!(
        skipped.status.success(),
        "{}",
        String::from_utf8_lossy(&skipped.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&skipped.stdout).unwrap();
    assert_eq!(report["skipped"][0]["reason"], "skippedExists");
    assert_eq!(report["written"].as_array().unwrap().len(), 0);

    // Keep both cannot number a file the user named: it fails, as before.
    assert!(
        !rawtonic(&["export", source, "--output", taken_str])
            .status
            .success()
    );
    // Replace if changed needs the record of earlier exports.
    let without_records = rawtonic(&[
        "export",
        source,
        "--output",
        taken_str,
        "--on-conflict",
        "replace-if-changed",
    ]);
    assert!(!without_records.status.success());
    assert!(String::from_utf8_lossy(&without_records.stderr).contains("--export-records"));
    // With it, the file is still no export of this photo: never replaced.
    let foreign = rawtonic(&[
        "export",
        source,
        "--output",
        taken_str,
        "--on-conflict",
        "replace-if-changed",
        "--export-records",
        records_str,
    ]);
    assert!(!foreign.status.success());
    assert_eq!(fs::read(&taken).unwrap(), b"someone's file");
    let names: Vec<_> = fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| !name.starts_with("records.sqlite"))
        .collect();
    let mut names = names;
    names.sort();
    assert_eq!(names, ["photo.CR3", "photo.jpg"]);
}
