// SPDX-License-Identifier: AGPL-3.0-or-later
//! A model is used on a GPU only after its output there matched the CPU's.
//!
//! An error or a non-finite value is easy to see, but a GPU can also return
//! wrong numbers without either (the SAM 2.1 encoder on WebGPU over Mesa's
//! software Vulkan: cosine -0.04 to the CPU's embedding). So the first time
//! a model loads on a device, it runs once on the GPU and once on the CPU
//! with the same deterministic input (`bench::Picture::synthetic`, prepared
//! as the app prepares a photo) and the outputs are compared
//! (`bench::disagreement`).
//!
//! A GPU that is right but slower than the CPU is not used either: both
//! runs are timed, and when the GPU's first run (which may include its
//! shader compiles) was not faster it runs once more; only a warm run
//! faster than the CPU's keeps the model on the GPU. An integrated Radeon
//! 680M under Windows ran BiRefNet in 14–19 s on WebGPU against 8.5 s on
//! the CPU, and AI Denoise stalled there (gap G76).
//!
//! The verdict is kept per model file, provider, device and plugin library,
//! in memory and in `Settings::checks_file`, so the check costs one CPU run
//! and one or two GPU runs per model and device, once; a known "differs" or
//! "slower" skips the GPU session altogether (`prior`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use ort::session::Session;

use crate::bench::{Picture, compare, disagreement, inputs_for, run_once};
use crate::{Device, ModelKind};

/// A device's verdict for a model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// Matches the CPU and is faster: used.
    Faster,
    /// Output differs from the CPU's.
    Differs,
    /// Matches, but a warm run was not faster than the CPU's.
    Slower,
}

impl Verdict {
    fn tag(self) -> &'static str {
        match self {
            Verdict::Faster => "faster",
            Verdict::Differs => "differs",
            Verdict::Slower => "slower",
        }
    }

    /// `ok` (written before the speed was compared) is not read back: such a
    /// device is checked again, speed included.
    fn parse(tag: &str) -> Option<Self> {
        [Verdict::Faster, Verdict::Differs, Verdict::Slower]
            .into_iter()
            .find(|v| v.tag() == tag)
    }
}

/// The speed verdict from the timed runs: the GPU only if its warm run beat
/// the CPU's.
pub(crate) fn speed_verdict(cpu: Duration, gpu: Duration) -> Verdict {
    if gpu < cpu {
        Verdict::Faster
    } else {
        Verdict::Slower
    }
}

static VERDICTS: Mutex<Option<HashMap<String, Verdict>>> = Mutex::new(None);

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

fn load_file(file: &Path) -> HashMap<String, Verdict> {
    std::fs::read_to_string(file)
        .ok()
        .map(|text| {
            text.lines()
                .filter_map(|line| {
                    let (verdict, key) = line.split_once('\t')?;
                    Some((key.to_string(), Verdict::parse(verdict)?))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Writes all verdicts, a temporary file renamed over the old one.
fn save_file(file: &Path, verdicts: &HashMap<String, Verdict>) -> std::io::Result<()> {
    let mut lines: Vec<String> = verdicts
        .iter()
        .map(|(key, verdict)| format!("{}\t{key}", verdict.tag()))
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

fn verdict(key: &str, file: Option<&PathBuf>) -> Option<Verdict> {
    let mut verdicts = VERDICTS.lock().unwrap();
    let verdicts = verdicts.get_or_insert_with(|| file.map(|f| load_file(f)).unwrap_or_default());
    verdicts.get(key).copied()
}

fn record(key: String, verdict: Verdict, file: Option<&PathBuf>) {
    let mut verdicts = VERDICTS.lock().unwrap();
    let verdicts = verdicts.get_or_insert_with(|| file.map(|f| load_file(f)).unwrap_or_default());
    verdicts.insert(key, verdict);
    if let Some(file) = file
        && let Err(e) = save_file(file, verdicts)
    {
        log::warn!("AI: GPU check results not saved ({}): {e}", file.display());
    }
}

/// `Ok` for a GPU to use, else why not.
fn outcome(verdict: Verdict, key: &str) -> Result<()> {
    match verdict {
        Verdict::Faster => Ok(()),
        Verdict::Differs => Err(anyhow!(
            "its output on this device differed from the CPU's before ({key})"
        )),
        Verdict::Slower => Err(anyhow!(
            "it ran slower on this device than on the CPU before ({key})"
        )),
    }
}

/// The verdict already known for `path` on `device`: `Some(Err)` when the
/// GPU is not to be used (so no GPU session is made), `Some(Ok)` when it
/// is, `None` when it still needs its `check`.
pub(crate) fn prior(
    path: &Path,
    kind: ModelKind,
    device: &Device,
    file: Option<&PathBuf>,
) -> Option<Result<()>> {
    let key = key(path, kind, device);
    verdict(&key, file).map(|known| outcome(known, &key))
}

/// `Ok` when `gpu` (a session of `path` on `device`) may be used: its
/// output matches the CPU's and a warm run is faster.
pub(crate) fn check(
    path: &Path,
    kind: ModelKind,
    gpu: &mut Session,
    device: &Device,
    file: Option<&PathBuf>,
) -> Result<()> {
    let key = key(path, kind, device);
    if let Some(known) = verdict(&key, file) {
        return outcome(known, &key);
    }
    let started = Instant::now();
    let mut times = String::new();
    let result = (|| {
        let mut cpu = crate::session(path, kind, None)?;
        let inputs = inputs_for(&cpu, kind, &Picture::synthetic(1536, 1024))?;
        let t = Instant::now();
        let reference = run_once(&mut cpu, &inputs)?;
        let cpu_time = t.elapsed();
        drop(cpu);
        let t = Instant::now();
        let outputs = run_once(gpu, &inputs)?;
        let mut gpu_time = t.elapsed();
        times = format!(
            "CPU {} ms, GPU {} ms",
            cpu_time.as_millis(),
            gpu_time.as_millis()
        );
        if let Some(why) = disagreement(kind, &compare(kind, &reference, &outputs)) {
            return Ok(Err((
                Verdict::Differs,
                anyhow!("its output differs from the CPU's: {why}"),
            )));
        }
        if speed_verdict(cpu_time, gpu_time) == Verdict::Slower {
            // The first GPU run may include its shader compiles.
            let t = Instant::now();
            run_once(gpu, &inputs)?;
            gpu_time = t.elapsed();
            times.push_str(&format!(", warm GPU {} ms", gpu_time.as_millis()));
        }
        Ok(match speed_verdict(cpu_time, gpu_time) {
            Verdict::Faster => Ok(()),
            _ => Err((
                Verdict::Slower,
                anyhow!("it runs slower on this device than on the CPU ({times})"),
            )),
        })
    })();
    log::info!(
        "AI {}: {} check on {} in {} ms ({times}): {}",
        kind.name(),
        device.provider,
        device.label,
        started.elapsed().as_millis(),
        match &result {
            Ok(Ok(())) => "matches the CPU and is faster".to_string(),
            Ok(Err((_, e))) | Err(e) => format!("{e:#}"),
        }
    );
    match result {
        Ok(Ok(())) => {
            record(key, Verdict::Faster, file);
            Ok(())
        }
        Ok(Err((verdict, e))) => {
            record(key, verdict, file);
            Err(e)
        }
        // A run that failed outright: as before, the device is not used.
        Err(e) => {
            record(key, Verdict::Differs, file);
            Err(e)
        }
    }
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
            Verdict::Faster,
        );
        verdicts.insert(
            "b.onnx:3 | WebGPU | GPU 10de:2684 | p.so:2".to_string(),
            Verdict::Differs,
        );
        verdicts.insert(
            "c.onnx:3 | WebGPU | GPU 1002:1681 | p.dll:2".to_string(),
            Verdict::Slower,
        );
        save_file(&file, &verdicts).unwrap();
        assert_eq!(load_file(&file), verdicts);
        assert!(!file.with_extension("partial").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Verdicts from before the speed comparison are checked again.
    #[test]
    fn an_old_ok_without_a_speed_comparison_is_not_read_back() {
        let dir = tempfile_dir("old-ok");
        let file = dir.join("gpu-checks.tsv");
        std::fs::write(&file, "ok\ta | WebGPU\ndiffers\tb | WebGPU\n").unwrap();
        let verdicts = load_file(&file);
        assert_eq!(verdicts.get("a | WebGPU"), None);
        assert_eq!(verdicts.get("b | WebGPU"), Some(&Verdict::Differs));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn only_a_faster_warm_gpu_run_is_used() {
        let ms = Duration::from_millis;
        // The 680M's BiRefNet (gap G76): 8.5 s on the CPU, 14-19 s warm.
        assert_eq!(speed_verdict(ms(8_500), ms(14_000)), Verdict::Slower);
        assert_eq!(speed_verdict(ms(8_500), ms(8_500)), Verdict::Slower);
        assert_eq!(speed_verdict(ms(9_500), ms(730)), Verdict::Faster);
    }

    #[test]
    fn a_missing_file_has_no_verdicts() {
        assert!(load_file(Path::new("/nonexistent/rawtonic/gpu-checks.tsv")).is_empty());
    }
}
