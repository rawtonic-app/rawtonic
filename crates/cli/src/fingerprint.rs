//! `rawtonic fingerprint`: decode facts per RAW file, keyed by the file's
//! SHA-256 (never by path), and their comparison with a baseline, to see
//! what a rawler update or a decode change altered. See
//! docs/testing/README.md.
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

pub const SCHEMA: u64 = 1;

#[derive(clap::Args)]
pub struct FingerprintArgs {
    /// RAW files or folders (searched recursively). Files are only read.
    #[arg(required = true)]
    pub paths: Vec<PathBuf>,
    /// Write the fingerprints as JSON to this new file (never overwrites).
    #[arg(long)]
    pub out: Option<PathBuf>,
    /// Compare with a baseline written by --out; prints the differences and
    /// fails when any file changed, is new or is missing.
    #[arg(long)]
    pub check: Option<PathBuf>,
    /// Skip the full-resolution development and its hash (decode facts only).
    #[arg(long)]
    pub no_develop: bool,
}

/// RAW files below `paths`, sorted, each once.
fn raw_files(paths: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for path in paths {
        if path.is_dir() {
            for entry in walkdir::WalkDir::new(path) {
                let entry = entry?;
                if entry.file_type().is_file()
                    && rawtonic_engine::formats::is_raw_file(entry.path())
                {
                    files.push(entry.into_path());
                }
            }
        } else {
            anyhow::ensure!(path.is_file(), "Not found: {}", path.display());
            files.push(path.clone());
        }
    }
    files.sort();
    files.dedup();
    Ok(files)
}

/// The fingerprint of one file: rawdecode's facts plus what the engine
/// derives from the file (base look, embedded lens corrections).
pub fn fingerprint_file(path: &Path, bytes: &[u8], develop: bool) -> Value {
    let fp = rawtonic_rawdecode::fingerprint::fingerprint(bytes, develop);
    let failed = fp.failed.is_some() && fp.facts.is_none();
    let mut value = serde_json::to_value(&fp).unwrap_or(Value::Null);
    let extension = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    value["extension"] = json!(extension);
    value["bytes"] = json!(bytes.len());
    if !failed {
        let name = path.to_string_lossy();
        let look =
            std::panic::catch_unwind(|| rawtonic_engine::baseline_exposure::base_look(&name));
        value["base_look"] = match look {
            Ok(look) => json!({
                "baseline_ev": look.baseline_ev,
                "profile_curve": look.profile_curve.is_some(),
                "no_black_render": look.no_black_render,
                "phone_defaults": look.phone_defaults,
            }),
            Err(_) => json!("panic"),
        };
        let lens = std::panic::catch_unwind(|| rawtonic_engine::embedded_lens::read(path));
        value["embedded_lens"] = match lens {
            Ok(Some(lens)) => json!({
                "maker": lens.maker,
                "distortion_points": lens.distortion.len(),
                "vignetting_points": lens.vignetting.as_ref().map(Vec::len),
                "chromatic_points": lens.chromatic.as_ref().map(Vec::len),
                "zoom": lens.zoom,
            }),
            Ok(None) => Value::Null,
            Err(_) => json!("panic"),
        };
    }
    value
}

pub fn run(args: FingerprintArgs) -> Result<()> {
    let files = raw_files(&args.paths)?;
    anyhow::ensure!(!files.is_empty(), "No RAW files found");
    let baseline = args
        .check
        .as_deref()
        .map(|path| -> Result<Value> {
            let text = std::fs::read_to_string(path)
                .with_context(|| format!("Read {}", path.display()))?;
            Ok(serde_json::from_str(&text)?)
        })
        .transpose()?;
    if let Some(out) = &args.out {
        anyhow::ensure!(
            !out.exists(),
            "Output exists; Rawtonic never overwrites: {}",
            out.display()
        );
    }
    let develop = !args.no_develop;
    let started = Instant::now();
    let mut entries: BTreeMap<String, Value> = BTreeMap::new();
    let mut timings = Vec::new();
    let mut failures: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (i, path) in files.iter().enumerate() {
        let now = Instant::now();
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) => {
                eprintln!("{}: {e}", path.display());
                failures
                    .entry("unreadable".into())
                    .or_default()
                    .push("read".into());
                continue;
            }
        };
        let key = rawtonic_rawdecode::fingerprint::sha256_hex(&bytes);
        if entries.contains_key(&key) {
            continue;
        }
        let value = fingerprint_file(path, &bytes, develop);
        timings.push(now.elapsed().as_secs_f64() * 1000.0);
        if let Some(step) = value.get("failed").and_then(Value::as_str) {
            failures
                .entry(camera_of(&value).unwrap_or_else(|| {
                    format!("(.{})", value["extension"].as_str().unwrap_or("?"))
                }))
                .or_default()
                .push(step.to_string());
        }
        entries.insert(key, value);
        if (i + 1) % 50 == 0 {
            eprintln!("{} / {} files", i + 1, files.len());
        }
    }
    let report = json!({
        "schema": SCHEMA,
        "rawtonic": env!("CARGO_PKG_VERSION"),
        "platform": format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
        "develop": develop,
        "files": entries,
    });
    if let Some(out) = &args.out {
        rawtonic_export::output::write_new_output(out, serde_json::to_vec_pretty(&report)?)
            .with_context(|| format!("Write {}", out.display()))?;
    }
    timings.sort_by(f64::total_cmp);
    let pick = |q: f64| {
        timings
            .get(((timings.len().max(1) - 1) as f64 * q).round() as usize)
            .copied()
    };
    eprintln!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "files": entries.len(),
            "failed": failures.values().map(Vec::len).sum::<usize>(),
            "failures_by_camera": failures,
            "seconds": started.elapsed().as_secs_f64(),
            "per_file_ms": {"median": pick(0.5), "p95": pick(0.95), "max": timings.last()},
            "develop": develop,
            "measures": "file read, SHA-256, decode facts and (unless --no-develop) one full-resolution CPU development per file",
        }))?
    );
    match baseline {
        Some(baseline) => {
            // Compared as written: numbers parsed from text, like the baseline's.
            let report: Value = serde_json::from_slice(&serde_json::to_vec_pretty(&report)?)?;
            let diff = compare(&baseline, &report);
            for line in &diff.lines {
                println!("{line}");
            }
            eprintln!(
                "{} changed, {} new, {} missing, {} unchanged",
                diff.changed, diff.new, diff.missing, diff.unchanged
            );
            anyhow::ensure!(
                diff.changed + diff.new + diff.missing == 0,
                "Decode fingerprints differ from {}",
                args.check.as_deref().unwrap_or(Path::new("")).display()
            );
            Ok(())
        }
        None if args.out.is_none() => {
            println!("{}", serde_json::to_string_pretty(&report)?);
            Ok(())
        }
        None => Ok(()),
    }
}

fn camera_of(value: &Value) -> Option<String> {
    if let Some(camera) = value.get("camera").and_then(Value::as_str) {
        return Some(camera.to_string());
    }
    Some(format!(
        "{} {}",
        value.get("clean_make")?.as_str()?,
        value.get("clean_model")?.as_str()?
    ))
}

#[derive(Debug, Default, PartialEq)]
pub struct Comparison {
    pub lines: Vec<String>,
    pub changed: usize,
    pub new: usize,
    pub missing: usize,
    pub unchanged: usize,
}

/// The differences between two reports, one line per changed field. The
/// developed image is compared only when both reports have it, and only on
/// the same platform (its float hash may differ between architectures).
pub fn compare(baseline: &Value, current: &Value) -> Comparison {
    let mut result = Comparison::default();
    let empty = serde_json::Map::new();
    let files = |report: &Value| report.get("files").and_then(Value::as_object).cloned();
    let (old, new) = (
        files(baseline).unwrap_or_else(|| empty.clone()),
        files(current).unwrap_or(empty),
    );
    let same_platform = baseline.get("platform") == current.get("platform");
    if baseline.get("schema") != current.get("schema") {
        result.lines.push(format!(
            "schema {} → {}: fields may differ by format",
            baseline["schema"], current["schema"]
        ));
    }
    if !same_platform {
        result.lines.push(format!(
            "platform {} → {}: developed hashes not compared",
            baseline["platform"], current["platform"]
        ));
    }
    let label = |key: &str, value: &Value| {
        format!(
            "{} {}",
            &key[..key.len().min(12)],
            camera_of(value).unwrap_or_else(|| "(undecoded)".into())
        )
    };
    for (key, value) in &new {
        let Some(before) = old.get(key) else {
            result.new += 1;
            result.lines.push(format!("{}: new", label(key, value)));
            continue;
        };
        let (mut a, mut b) = (before.clone(), value.clone());
        if !same_platform || a.get("developed").is_none() || b.get("developed").is_none() {
            for v in [&mut a, &mut b] {
                if let Some(map) = v.as_object_mut() {
                    map.remove("developed");
                }
            }
        }
        let mut changes = Vec::new();
        diff_values("", &a, &b, &mut changes);
        if changes.is_empty() {
            result.unchanged += 1;
        } else {
            result.changed += 1;
            for change in changes {
                result
                    .lines
                    .push(format!("{}: {change}", label(key, value)));
            }
        }
    }
    for (key, value) in &old {
        if !new.contains_key(key) {
            result.missing += 1;
            result.lines.push(format!("{}: missing", label(key, value)));
        }
    }
    result
}

fn diff_values(path: &str, a: &Value, b: &Value, out: &mut Vec<String>) {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            let keys: std::collections::BTreeSet<&String> = x.keys().chain(y.keys()).collect();
            for key in keys {
                let sub = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                diff_values(
                    &sub,
                    x.get(key).unwrap_or(&Value::Null),
                    y.get(key).unwrap_or(&Value::Null),
                    out,
                );
            }
        }
        _ if a != b => out.push(format!("{path} {a} → {b}")),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(files: Value) -> Value {
        json!({"schema": SCHEMA, "platform": "macos-aarch64", "develop": true, "files": files})
    }

    #[test]
    fn identical_reports_have_no_differences() {
        let files = json!({"aa": {"clean_make": "Sony", "clean_model": "A1", "white": [16383]}});
        let c = compare(&report(files.clone()), &report(files));
        assert_eq!((c.changed, c.new, c.missing, c.unchanged), (0, 0, 0, 1));
        assert!(c.lines.is_empty());
    }

    #[test]
    fn changed_new_and_missing_files_are_listed() {
        let old = report(json!({
            "aa": {"clean_make": "Sony", "clean_model": "A1", "white": [16383], "black": {"levels": ["512/1"]}},
            "bb": {"clean_make": "Nikon", "clean_model": "Z 8"},
        }));
        let new = report(json!({
            "aa": {"clean_make": "Sony", "clean_model": "A1", "white": [16380], "black": {"levels": ["512/1"]}},
            "cc": {"failed": "decoder", "error": "unsupported"},
        }));
        let c = compare(&old, &new);
        assert_eq!((c.changed, c.new, c.missing, c.unchanged), (1, 1, 1, 0));
        assert!(
            c.lines
                .contains(&"aa Sony A1: white [16383] → [16380]".to_string())
        );
        assert!(c.lines.contains(&"cc (undecoded): new".to_string()));
        assert!(c.lines.contains(&"bb Nikon Z 8: missing".to_string()));
    }

    #[test]
    fn developed_hashes_need_both_reports_and_one_platform() {
        let with = json!({"aa": {"white": [1], "developed": {"sha256": "x"}}});
        let other = json!({"aa": {"white": [1], "developed": {"sha256": "y"}}});
        let without = json!({"aa": {"white": [1]}});
        assert_eq!(
            compare(&report(with.clone()), &report(other.clone())).changed,
            1
        );
        assert_eq!(compare(&report(with.clone()), &report(without)).changed, 0);
        let mut linux = report(other);
        linux["platform"] = json!("linux-x86_64");
        assert_eq!(compare(&report(with), &linux).changed, 0);
    }

    #[test]
    fn baseline_round_trip_on_a_synthetic_dng() {
        let dir = tempfile::tempdir().unwrap();
        let photos = dir.path().join("photos");
        std::fs::create_dir(&photos).unwrap();
        let bytes = rawtonic_rawdecode::fingerprint::synthetic::synthetic_dng();
        std::fs::write(photos.join("frame.dng"), &bytes).unwrap();
        // The same content under another name is one entry.
        std::fs::write(photos.join("copy.dng"), &bytes).unwrap();
        let baseline = dir.path().join("baseline.json");
        let args = |out: Option<PathBuf>, check: Option<PathBuf>| FingerprintArgs {
            paths: vec![photos.clone()],
            out,
            check,
            no_develop: false,
        };
        run(args(Some(baseline.clone()), None)).unwrap();
        let text = std::fs::read_to_string(&baseline).unwrap();
        assert!(!text.contains("frame") && !text.contains("photos"));
        let report: Value = serde_json::from_str(&text).unwrap();
        let files = report["files"].as_object().unwrap();
        assert_eq!(files.len(), 1);
        let entry = &files[&rawtonic_rawdecode::fingerprint::sha256_hex(&bytes)];
        assert_eq!(entry["clean_model"], "Synthetic Mosaic");
        assert_eq!(entry["extension"], "dng");
        assert_eq!(entry["developed"]["size"], json!([48, 64]));
        assert!(entry["base_look"]["baseline_ev"].is_number());
        run(args(None, Some(baseline.clone()))).unwrap();
        // Never overwrites a baseline.
        assert!(run(args(Some(baseline.clone()), None)).is_err());
        // Another file: one new, one missing.
        let mut changed = bytes.clone();
        let last = changed.len() - 2;
        changed[last] ^= 1;
        std::fs::write(photos.join("frame.dng"), &changed).unwrap();
        std::fs::remove_file(photos.join("copy.dng")).unwrap();
        assert!(run(args(None, Some(baseline))).is_err());
    }

    #[test]
    fn folders_yield_raw_files_only() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        for name in ["a.dng", "sub/b.ARW", "c.jpg", "notes.txt"] {
            std::fs::write(dir.path().join(name), b"x").unwrap();
        }
        let files = raw_files(&[dir.path().to_path_buf()]).unwrap();
        let names: Vec<_> = files
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["a.dng", "b.ARW"]);
    }
}
