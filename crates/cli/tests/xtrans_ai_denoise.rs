// SPDX-License-Identifier: AGPL-3.0-or-later
//! AI Denoise on an X-Trans RAF end to end through the CLI's export (the
//! app's render path): version 6 denoises with the X-Trans network, fresh,
//! cached and freshly computed again in another cache give the same bytes,
//! the quality setting does not apply, an edit from before version 6 renders
//! without AI Denoise as it always did, and the RAW is never changed. Opt-in,
//! as it needs an X-Trans RAF, the network and ONNX Runtime:
//!
//! RAWTONIC_XTRANS_RAF=/path/photo.RAF RAWTONIC_MODELS=/path/models \
//! ORT_DYLIB_PATH=/path/libonnxruntime.so \
//!   cargo test -p rawtonic-cli --release --test xtrans_ai_denoise -- --ignored
//!
//! `RAWTONIC_MODELS` holds `xtrans_denoise_v1.onnx` (model pack, from
//! pack-2026-11-25d) and `nlmf.onnx`: an edit from before version 6 loads
//! MatchFilter as it always did, which then leaves the X-Trans mosaic alone.
//! The RAF is copied first; the copy is checked unchanged.
//! Runs on ONNX Runtime's CPU provider (`RAWTONIC_AI_PROVIDER=cpu`) so the
//! result does not depend on a GPU provider's last bits.

use std::path::{Path, PathBuf};
use std::process::Command;

fn export(
    raw: &Path,
    recipe: &serde_json::Value,
    models: &Path,
    out: &Path,
    lensfun: &Path,
) -> Vec<u8> {
    let recipe_path = out.with_extension("json");
    std::fs::write(&recipe_path, recipe.to_string()).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_rawtonic"))
        .arg("export")
        .arg(raw)
        .arg("--output")
        .arg(out)
        .args(["--tiff-bit-depth", "16", "--adjustments"])
        .arg(&recipe_path)
        .arg("--models")
        .arg(models)
        .env("RAWTONIC_LENSFUN_DB", lensfun)
        .env("RAWTONIC_AI_PROVIDER", "cpu")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "rawtonic export failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    std::fs::read(out).unwrap()
}

/// A models folder of its own (its cache is beside it) with the networks
/// of `from`.
fn models_in(dir: &Path, from: &Path) -> PathBuf {
    let models = dir.join("models");
    std::fs::create_dir_all(&models).unwrap();
    for name in [
        rawtonic_denoise::XTRANS_ONNX_MODEL,
        rawtonic_denoise::ONNX_MODEL,
    ] {
        std::fs::copy(from.join(name), models.join(name)).unwrap();
    }
    models
}

fn cached_results(models: &Path) -> usize {
    std::fs::read_dir(models.parent().unwrap().join("ai-denoise-cache"))
        .map_or(0, |dir| dir.count())
}

#[test]
#[ignore = "needs RAWTONIC_XTRANS_RAF, RAWTONIC_MODELS and ORT_DYLIB_PATH"]
fn x_trans_ai_denoise_exports_deterministically_and_leaves_old_edits_alone() {
    let (Some(raf), Some(models)) = (
        std::env::var_os("RAWTONIC_XTRANS_RAF").map(PathBuf::from),
        std::env::var_os("RAWTONIC_MODELS").map(PathBuf::from),
    ) else {
        eprintln!("SKIP: RAWTONIC_XTRANS_RAF or RAWTONIC_MODELS not set");
        return;
    };
    let lensfun = std::env::var_os("RAWTONIC_LENSFUN_DB")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../src-tauri/lensfun_db")
        });
    let dir = tempfile::tempdir().unwrap();
    let raw = dir.path().join(raf.file_name().unwrap());
    std::fs::copy(&raf, &raw).unwrap();
    let original = std::fs::read(&raw).unwrap();
    assert!(
        rawtonic_rawdecode::raw_processing::is_xtrans_at(&raw),
        "not an X-Trans RAF"
    );
    let base: serde_json::Value = serde_json::from_str(include_str!(
        "../../engine/tests/fixtures/new-edit-recipe.raw.json"
    ))
    .unwrap();
    assert_eq!(base["aiDenoiseVersion"], 6, "new edits denoise X-Trans");
    let with = |amount: u32, version: u32, quality: &str| {
        let mut recipe = base.clone();
        recipe["aiDenoise"] = amount.into();
        recipe["aiDenoiseVersion"] = version.into();
        recipe["aiDenoiseQuality"] = quality.into();
        recipe
    };
    let (first, second) = (dir.path().join("a"), dir.path().join("b"));
    let (models_a, models_b) = (models_in(&first, &models), models_in(&second, &models));
    let out = |name: &str| dir.path().join(format!("{name}.tif"));

    let fresh = export(
        &raw,
        &with(50, 6, "full"),
        &models_a,
        &out("fresh"),
        &lensfun,
    );
    assert_eq!(cached_results(&models_a), 1, "the network ran and cached");
    let cached = export(
        &raw,
        &with(50, 6, "full"),
        &models_a,
        &out("cached"),
        &lensfun,
    );
    assert_eq!(cached_results(&models_a), 1, "read from the cache");
    assert!(
        fresh == cached,
        "a cached export differs from the fresh one"
    );
    let again = export(
        &raw,
        &with(50, 6, "full"),
        &models_b,
        &out("again"),
        &lensfun,
    );
    assert!(fresh == again, "a second fresh export differs");
    // One network for X-Trans: the quality does not apply.
    let fast = export(
        &raw,
        &with(50, 6, "fast"),
        &models_a,
        &out("fast"),
        &lensfun,
    );
    assert!(fresh == fast, "the quality changed an X-Trans export");

    let off = export(&raw, &with(0, 6, "full"), &models_a, &out("off"), &lensfun);
    assert!(fresh != off, "AI Denoise changed nothing");
    // An edit made before version 6 rendered without AI Denoise; so it does.
    let older = export(
        &raw,
        &with(50, 5, "full"),
        &models_a,
        &out("older"),
        &lensfun,
    );
    assert!(older == off, "an edit from before version 6 changed");

    assert!(
        std::fs::read(&raw).unwrap() == original,
        "the RAW was changed"
    );
}
