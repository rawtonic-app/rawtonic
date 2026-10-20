// SPDX-License-Identifier: AGPL-3.0-or-later
//! A model is used on a GPU only after its output there matched the CPU's.
//!
//! An error or a non-finite value is easy to see, but a GPU can also return
//! wrong numbers without either (the SAM 2.1 encoder on WebGPU over Mesa's
//! software Vulkan: cosine -0.04 to the CPU's embedding). So the first time
//! a model loads on a device, it runs once on the GPU and once on the CPU
//! with the same deterministic input (`bench::Picture::synthetic`, prepared
//! as the app prepares a photo) and the outputs are compared
//! (`bench::disagreement`). The verdict is kept per model file, provider,
//! device and plugin library, in memory and in `Settings::checks_file`, so
//! the check costs one CPU run per model and device, once.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

use anyhow::{Result, anyhow};
use ort::session::Session;

use crate::bench::{Picture, compare, disagreement, inputs_for, run_once};
use crate::{Device, ModelKind};

static VERDICTS: Mutex<Option<HashMap<String, bool>>> = Mutex::new(None);

/// What a verdict is about: the model file (name, size and modification
/// time, so a replaced file is checked again), the ONNX Runtime build, the
/// provider, the device with what the runtime reports about it (the
/// plugin's version; a driver version only where the runtime reports one,
/// not on Linux), the plugin library (size) and the provider's options for
/// the model.
fn key(path: &Path, kind: ModelKind, device: &Device) -> String {
    let options: String = kind
        .provider_options(device.provider)
        .iter()
        .map(|(k, v)| format!(" | {k}={v}"))
        .collect();
    let device_part = format!(
        "{} | {} [{}] | {}:{}",
        device.provider,
        device.label,
        device.identity,
        device
            .library
            .file_name()
            .unwrap_or_default()
            .to_string_lossy(),
        file_size(&device.library)
    );
    key_of(path, runtime_build(ort::info()), &device_part, &options)
}

fn file_size(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

/// The version part of ONNX Runtime's build info (branch and commit, not
/// the compiler flags).
fn runtime_build(info: &str) -> &str {
    let info = info.strip_prefix("ORT Build Info: ").unwrap_or(info);
    info.split(", cmake").next().unwrap_or(info)
}

fn key_of(path: &Path, runtime: &str, device: &str, options: &str) -> String {
    let meta = std::fs::metadata(path).ok();
    let modified = meta
        .as_ref()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos());
    format!(
        "{}:{}:{modified} | {runtime} | {device}{options}",
        path.file_name().unwrap_or_default().to_string_lossy(),
        meta.map_or(0, |m| m.len()),
    )
}

fn load_file(file: &Path) -> HashMap<String, bool> {
    std::fs::read_to_string(file)
        .ok()
        .map(|text| {
            text.lines()
                .filter_map(|line| {
                    let (verdict, key) = line.split_once('\t')?;
                    Some((key.to_string(), verdict == "ok"))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Writes all verdicts, a temporary file renamed over the old one.
fn save_file(file: &Path, verdicts: &HashMap<String, bool>) -> std::io::Result<()> {
    let mut lines: Vec<String> = verdicts
        .iter()
        .map(|(key, ok)| format!("{}\t{key}", if *ok { "ok" } else { "differs" }))
        .collect();
    lines.sort();
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // The app and the CLI may save at once: a temporary name per process.
    let partial = file.with_extension(format!("partial-{}", std::process::id()));
    std::fs::write(&partial, lines.join("\n") + "\n")?;
    std::fs::rename(&partial, file)
}

fn verdict(key: &str, file: Option<&PathBuf>) -> Option<bool> {
    let mut verdicts = VERDICTS.lock().unwrap();
    let verdicts = verdicts.get_or_insert_with(|| file.map(|f| load_file(f)).unwrap_or_default());
    verdicts.get(key).copied()
}

fn record(key: String, ok: bool, file: Option<&PathBuf>) {
    let mut verdicts = VERDICTS.lock().unwrap();
    let verdicts = verdicts.get_or_insert_with(HashMap::new);
    verdicts.insert(key, ok);
    if let Some(file) = file
        && let Err(e) = save_file(file, verdicts)
    {
        log::warn!("AI: GPU check results not saved ({}): {e}", file.display());
    }
}

/// `Ok` when `gpu` (a session of `path` on `device`) may be used.
pub(crate) fn check(
    path: &Path,
    kind: ModelKind,
    gpu: &mut Session,
    device: &Device,
    file: Option<&PathBuf>,
) -> Result<()> {
    let key = key(path, kind, device);
    match verdict(&key, file) {
        Some(true) => return Ok(()),
        Some(false) => {
            return Err(anyhow!(
                "its output on this device differed from the CPU's before ({key})"
            ));
        }
        None => {}
    }
    let started = Instant::now();
    let result = (|| {
        let mut cpu = crate::session(path, kind, None)?;
        let inputs = inputs_for(&cpu, kind, &Picture::synthetic(1536, 1024))?;
        let reference = run_once(&mut cpu, &inputs)?;
        drop(cpu);
        let outputs = run_once(gpu, &inputs)?;
        match disagreement(kind, &compare(kind, &reference, &outputs)) {
            Some(why) => Err(anyhow!("its output differs from the CPU's: {why}")),
            None => Ok(()),
        }
    })();
    log::info!(
        "AI {}: {} check on {} in {} ms: {}",
        kind.name(),
        device.provider,
        device.label,
        started.elapsed().as_millis(),
        match &result {
            Ok(()) => "matches the CPU".to_string(),
            Err(e) => format!("{e:#}"),
        }
    );
    record(key, result.is_ok(), file);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A replaced model file, another ONNX Runtime build or another plugin
    /// version as the runtime reports it gets a check of its own.
    #[test]
    fn verdicts_are_per_model_file_runtime_and_plugin_version() {
        let dir = tempfile_dir("key");
        let model = dir.join("model.onnx");
        std::fs::write(&model, b"weights").unwrap();
        let device = "WebGpu |  GPU 10de:2216 [Discrete=1 pci_bus_id=0000:26:00.0 version=0.4.0] | plugin.so:7";
        let first = key_of(
            &model,
            "git-branch=rel-1.30.0, git-commit-id=abc",
            device,
            "",
        );
        assert_eq!(
            key_of(
                &model,
                "git-branch=rel-1.30.0, git-commit-id=abc",
                device,
                ""
            ),
            first
        );
        let other_runtime = key_of(
            &model,
            "git-branch=rel-1.31.0, git-commit-id=def",
            device,
            "",
        );
        assert_ne!(other_runtime, first);
        let other_plugin = device.replace("version=0.4.0", "version=0.5.0");
        assert_ne!(
            key_of(
                &model,
                "git-branch=rel-1.30.0, git-commit-id=abc",
                &other_plugin,
                ""
            ),
            first
        );
        // The same size, written again (a new download or install).
        let file = std::fs::File::options().write(true).open(&model).unwrap();
        file.set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(5))
            .unwrap();
        assert_ne!(
            key_of(
                &model,
                "git-branch=rel-1.30.0, git-commit-id=abc",
                device,
                ""
            ),
            first
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_runtime_build_is_its_version_without_compiler_flags() {
        assert_eq!(
            runtime_build(
                "ORT Build Info: git-branch=rel-1.19.0, git-commit-id=26250ae, build type=Release, cmake cxx flags: /DWIN32"
            ),
            "git-branch=rel-1.19.0, git-commit-id=26250ae, build type=Release"
        );
    }

    fn tempfile_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rawtonic-onnx-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn verdicts_survive_a_save_and_load() {
        let dir = std::env::temp_dir().join(format!("rawtonic-onnx-checks-{}", std::process::id()));
        let file = dir.join("gpu-checks.tsv");
        let mut verdicts = HashMap::new();
        verdicts.insert(
            "a.onnx:1 | WebGPU | GPU 10de:2684 | p.so:2".to_string(),
            true,
        );
        verdicts.insert(
            "b.onnx:3 | WebGPU | GPU 10de:2684 | p.so:2".to_string(),
            false,
        );
        save_file(&file, &verdicts).unwrap();
        assert_eq!(load_file(&file), verdicts);
        assert!(!file.with_extension("partial").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_missing_file_has_no_verdicts() {
        assert!(load_file(Path::new("/nonexistent/rawtonic/gpu-checks.tsv")).is_empty());
    }
}
