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

/// What a verdict is about: the model file (name and size), the provider,
/// the device, the plugin library (size) and the provider's options for the
/// model, so a new driver-visible device, plugin or option is checked again.
fn key(path: &Path, kind: ModelKind, device: &Device) -> String {
    let size = |p: &Path| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
    let options: String = kind
        .provider_options(device.provider)
        .iter()
        .map(|(k, v)| format!(" | {k}={v}"))
        .collect();
    format!(
        "{}:{} | {} | {} | {}:{}{options}",
        path.file_name().unwrap_or_default().to_string_lossy(),
        size(path),
        device.provider,
        device.label,
        device
            .library
            .file_name()
            .unwrap_or_default()
            .to_string_lossy(),
        size(&device.library)
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
    let partial = file.with_extension("partial");
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
