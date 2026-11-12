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
//! "slower" skips the GPU session altogether (`plan`).
//!
//! The app does not make the first use wait for the check (49 s of a 110 s
//! first AI Subject click on the 680M): the model loads on the CPU and the
//! check is queued until the app is idle (`queue`, `background`); a faster
//! GPU's session then goes to that model (`adopt`). The CLI and a forced
//! provider check before first use (`check`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::RecvTimeoutError;
use std::sync::{Arc, LazyLock, Mutex, Weak};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use ort::session::{RunOptions, Session};

use crate::background::{Ran, Scheduler, Stop};
use crate::bench::{Input, Output, Picture, compare, disagreement, inputs_for, run_once_with};
use crate::watch::Slot;
use crate::{Device, ModelKind, Provider};

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

    /// `ok` was written before the speed was compared: such a device stays
    /// in use as it was, so a model keeps the provider (and the output) it
    /// had; only new checks compare speed.
    fn parse(tag: &str) -> Option<Self> {
        if tag == "ok" {
            return Some(Verdict::Faster);
        }
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
pub(crate) fn outcome(verdict: Verdict, key: &str) -> Result<()> {
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

/// How long the check's GPU runs may take together: a device that stalls
/// (AI Denoise on a Radeon 680M under Windows stopped for over 13 minutes
/// after one tile, gap G76) is left behind and refused.
const CHECK_LIMIT: Duration = Duration::from_secs(300);

/// How long the app must be idle before a queued check runs
/// (`background`): longer than the pause between two clicks or two slider
/// moves, short enough that the check runs while the photographer looks.
const IDLE_AFTER: Duration = Duration::from_secs(10);

/// How often a waiting check looks whether it was stopped.
const LOOK: Duration = Duration::from_millis(250);

/// The GPU's output and run times: the first run and, when that was not
/// faster than `cpu`, a warm second one.
type GpuRuns<O> = (O, Duration, Option<Duration>);

/// How the check's GPU part ended.
enum GpuEnd<S, O> {
    Ran(S, GpuRuns<O>),
    Failed(anyhow::Error),
    /// Stopped (`background::Stop`); the thread ended or was left behind.
    Stopped,
    /// Did not finish within the limit; the thread is left behind.
    TimedOut,
}

/// Makes the GPU session (`create`; its creation may compile shaders) and
/// runs the check's GPU runs (`run`) on a thread of its own, and waits at
/// most `limit`. When `stop` stops it, it still waits for the thread up to
/// the limit, so a stopped check never leaves its GPU work running beside
/// the next one.
fn gpu_runs<S, O>(
    create: impl FnOnce() -> Result<S> + Send + 'static,
    run: impl Fn(&mut S) -> Result<O> + Send + 'static,
    cpu: Duration,
    limit: Duration,
    stop: &Arc<Stop>,
) -> Result<GpuEnd<S, O>>
where
    S: Send + 'static,
    O: Send + 'static,
{
    let (done, result) = std::sync::mpsc::channel();
    let stopped = stop.clone();
    std::thread::Builder::new()
        .name("ai-gpu-check".to_string())
        .spawn(move || {
            let runs = (|| {
                let mut gpu = create()?;
                let t = Instant::now();
                let outputs = run(&mut gpu)?;
                let first = t.elapsed();
                // The first GPU run may include its shader compiles.
                let warm = if speed_verdict(cpu, first) == Verdict::Slower && !stopped.stopped() {
                    let t = Instant::now();
                    run(&mut gpu)?;
                    Some(t.elapsed())
                } else {
                    None
                };
                Ok::<_, anyhow::Error>((gpu, (outputs, first, warm)))
            })();
            let _ = done.send(runs);
        })
        .map_err(|e| anyhow!("starting the GPU check: {e}"))?;
    let started = Instant::now();
    loop {
        let left = limit.saturating_sub(started.elapsed());
        match result.recv_timeout(left.min(LOOK)) {
            Ok(_) if stop.stopped() => return Ok(GpuEnd::Stopped),
            Ok(Ok((gpu, runs))) => return Ok(GpuEnd::Ran(gpu, runs)),
            Ok(Err(e)) => return Ok(GpuEnd::Failed(e)),
            Err(RecvTimeoutError::Disconnected) => {
                return Ok(GpuEnd::Failed(anyhow!(
                    "the GPU check ended without a result"
                )));
            }
            Err(RecvTimeoutError::Timeout) if left.is_zero() || started.elapsed() >= limit => {
                return Ok(if stop.stopped() {
                    GpuEnd::Stopped
                } else {
                    GpuEnd::TimedOut
                });
            }
            Err(RecvTimeoutError::Timeout) => {}
        }
    }
}

/// The CPU's part of a check: its inputs, output and time.
pub(crate) struct CpuRun {
    inputs: Arc<Vec<(String, Input)>>,
    reference: Vec<Output>,
    time: Duration,
}

/// Runs `path` once on the CPU with the check's input; `None` when `stop`
/// stopped it (a stopped run's time is not the CPU's).
fn cpu_run(path: &Path, kind: ModelKind, stop: &Arc<Stop>) -> Result<Option<CpuRun>> {
    let mut cpu = crate::session(path, kind, None)?;
    let inputs = inputs_for(&cpu, kind, &Picture::synthetic(1536, 1024))?;
    let options = stoppable(stop)?;
    if stop.stopped() {
        return Ok(None);
    }
    let t = Instant::now();
    let reference = run_once_with(&mut cpu, &inputs, Some(&options));
    let time = t.elapsed();
    if stop.stopped() {
        return Ok(None);
    }
    Ok(Some(CpuRun {
        inputs: Arc::new(inputs),
        reference: reference?,
        time,
    }))
}

/// Run options that `stop` terminates.
fn stoppable(stop: &Arc<Stop>) -> Result<Arc<RunOptions>> {
    let options = Arc::new(RunOptions::new().map_err(|e| anyhow!("run options: {e}"))?);
    let terminate = options.clone();
    stop.on_stop(move || {
        let _ = terminate.terminate();
    });
    Ok(options)
}

/// What a check found.
enum Measured {
    /// Matches the CPU and a warm run is faster: the GPU session.
    Faster(Session),
    /// Not to be used, and why.
    Refused(Verdict, anyhow::Error),
    /// Stopped before it knew (`background`).
    Stopped,
}

/// Times `path` on the CPU (or takes `cpu_done`, an earlier stopped
/// attempt's CPU run) and on `device`, and compares the outputs.
fn measure(
    path: &Path,
    kind: ModelKind,
    device: &Arc<Device>,
    cpu_done: &Mutex<Option<Arc<CpuRun>>>,
    stop: &Arc<Stop>,
    times: &mut String,
) -> Result<Measured> {
    let known = cpu_done.lock().unwrap().clone();
    let cpu = match known {
        Some(cpu) => cpu,
        None => match cpu_run(path, kind, stop)? {
            Some(cpu) => {
                let cpu = Arc::new(cpu);
                *cpu_done.lock().unwrap() = Some(cpu.clone());
                cpu
            }
            None => return Ok(Measured::Stopped),
        },
    };
    let options = stoppable(stop)?;
    let inputs = cpu.inputs.clone();
    let run = move |gpu: &mut Session| run_once_with(gpu, &inputs, Some(&options));
    let (gpu_path, gpu_device) = (path.to_path_buf(), device.clone());
    let create = move || crate::session(&gpu_path, kind, Some(&gpu_device));
    *times = format!("CPU {} ms", cpu.time.as_millis());
    let (gpu, (outputs, first, warm)) = match gpu_runs(create, run, cpu.time, CHECK_LIMIT, stop)? {
        GpuEnd::Ran(gpu, runs) => (gpu, runs),
        GpuEnd::Failed(e) => return Err(e),
        GpuEnd::Stopped => return Ok(Measured::Stopped),
        GpuEnd::TimedOut => {
            return Ok(Measured::Refused(
                Verdict::Slower,
                anyhow!(
                    "its runs on this device did not finish in {} s and were left behind",
                    CHECK_LIMIT.as_secs()
                ),
            ));
        }
    };
    times.push_str(&format!(", GPU {} ms", first.as_millis()));
    if let Some(warm) = warm {
        times.push_str(&format!(", warm GPU {} ms", warm.as_millis()));
    }
    if let Some(why) = disagreement(kind, &compare(kind, &cpu.reference, &outputs)) {
        return Ok(Measured::Refused(
            Verdict::Differs,
            anyhow!("its output differs from the CPU's: {why}"),
        ));
    }
    Ok(match speed_verdict(cpu.time, warm.unwrap_or(first)) {
        Verdict::Faster => Measured::Faster(gpu),
        _ => Measured::Refused(
            Verdict::Slower,
            anyhow!("it runs slower on this device than on the CPU ({times})"),
        ),
    })
}

/// Logs a check's result and records its verdict through `stop`, which
/// refuses after a stop, and in the same step gives a faster GPU's session
/// to `adopt` (which returns it if nobody took it). `None` when the check
/// was stopped; else `Ok` with the GPU session `adopt` left, or why the GPU
/// is not used.
#[allow(clippy::too_many_arguments)]
fn conclude(
    key: String,
    kind: ModelKind,
    (provider, label): (Provider, &str),
    file: Option<&PathBuf>,
    (started, times): (Instant, &str),
    result: Result<Measured>,
    stop: &Stop,
    adopt: impl FnOnce(&str, Session) -> Option<Session>,
) -> Option<Result<Option<Session>>> {
    let (verdict, outcome) = match result {
        Ok(Measured::Stopped) => {
            log::info!(
                "AI {}: {} check on {} stopped after {} ms; it runs again when the app is idle",
                kind.name(),
                provider,
                label,
                started.elapsed().as_millis()
            );
            return None;
        }
        Ok(Measured::Faster(gpu)) => (Verdict::Faster, Ok(gpu)),
        Ok(Measured::Refused(verdict, e)) => (verdict, Err(e)),
        // A run that failed outright: as before, the device is not used.
        Err(e) => (Verdict::Differs, Err(e)),
    };
    let found = match &outcome {
        Ok(_) => "matches the CPU and is faster".to_string(),
        Err(e) => format!("{e:#}"),
    };
    let mut kept = None;
    let recorded = stop.commit(|| {
        record(key.clone(), verdict, file);
        kept = Some(outcome.map(|gpu| adopt(&key, gpu)));
    });
    log::info!(
        "AI {}: {} check on {} in {} ms ({times}): {found}{}",
        kind.name(),
        provider,
        label,
        started.elapsed().as_millis(),
        if recorded { "" } else { " (stopped, not kept)" }
    );
    kept
}

/// Checks `path` on `device` now and returns its GPU session when it may be
/// used: its output matches the CPU's and a warm run is faster. For the
/// CLI and a forced provider; the app queues its checks (`queue`).
pub(crate) fn check(
    path: &Path,
    kind: ModelKind,
    device: &Arc<Device>,
    file: Option<&PathBuf>,
) -> Result<Session> {
    let key = key(path, kind, device);
    let started = Instant::now();
    let mut times = String::new();
    let never = Stop::new();
    let result = measure(path, kind, device, &Mutex::new(None), &never, &mut times);
    match conclude(
        key,
        kind,
        (device.provider, &device.label),
        file,
        (started, &times),
        result,
        &never,
        |_, gpu| Some(gpu),
    ) {
        Some(Ok(Some(gpu))) => Ok(gpu),
        Some(Err(e)) => Err(e),
        Some(Ok(None)) | None => Err(anyhow!("the GPU check was stopped")),
    }
}

/// A check queued to run when the app is idle.
pub(crate) struct Job {
    path: PathBuf,
    kind: ModelKind,
    device: Arc<Device>,
    file: Option<PathBuf>,
    /// The CPU run of an attempt that was stopped later, kept so a new
    /// attempt only repeats the GPU's part.
    cpu: Mutex<Option<Arc<CpuRun>>>,
}

fn run_job(job: &Job, stop: &Arc<Stop>) -> Ran {
    let key = key(&job.path, job.kind, &job.device);
    log::info!(
        "AI {}: {} check on {} starts (the app is idle)",
        job.kind.name(),
        job.device.provider,
        job.device.label
    );
    let started = Instant::now();
    let mut times = String::new();
    let result = measure(&job.path, job.kind, &job.device, &job.cpu, stop, &mut times);
    let stopped = matches!(result, Ok(Measured::Stopped));
    // A faster GPU's session, warm from the check, goes to the model loaded
    // on the CPU meanwhile; later loads use the verdict.
    let provider = job.device.provider;
    let concluded = conclude(
        key,
        job.kind,
        (provider, &job.device.label),
        job.file.as_ref(),
        (started, &times),
        result,
        stop,
        |key, gpu| adopt(key, gpu, provider),
    );
    if stopped || concluded.is_none() {
        Ran::Stopped
    } else {
        Ran::Done
    }
}

/// Models loaded on the CPU while their check waits, by verdict key.
type Waiting = Vec<(String, Weak<Slot<Session>>)>;
static WAITING: Mutex<Waiting> = Mutex::new(Vec::new());

/// `slot` (a model loaded on the CPU) takes the GPU session of the check
/// of `key` if it finds the GPU faster (`adopt`).
pub(crate) fn wait_for(key: String, slot: &Arc<Slot<Session>>) {
    let mut waiting = WAITING.lock().unwrap();
    waiting.retain(|(_, slot)| slot.strong_count() > 0);
    waiting.push((key, Arc::downgrade(slot)));
}

/// Gives `gpu` to the first model still loaded on the CPU that waits for
/// `key`'s check; `gpu` back if none takes it. Called inside the verdict's
/// commit, so never while an export holds the checks.
fn adopt(key: &str, gpu: Session, provider: Provider) -> Option<Session> {
    let waiting: Vec<_> = {
        let mut all = WAITING.lock().unwrap();
        let (mine, rest) = std::mem::take(&mut *all)
            .into_iter()
            .partition(|(k, _)| k == key);
        *all = rest;
        mine
    };
    adopt_into(
        waiting.into_iter().filter_map(|(_, slot)| slot.upgrade()),
        gpu,
        provider,
    )
}

fn adopt_into<S: Send + 'static>(
    slots: impl IntoIterator<Item = Arc<Slot<S>>>,
    mut gpu: S,
    provider: Provider,
) -> Option<S> {
    for slot in slots {
        match slot.adopt(gpu, provider) {
            Ok(()) => return None,
            Err(back) => gpu = back,
        }
    }
    Some(gpu)
}

pub(crate) static CHECKS: LazyLock<Arc<Scheduler<Job>>> =
    LazyLock::new(|| Arc::new(Scheduler::new(IDLE_AFTER, run_job)));

/// Queues the check of `path` on `device` (once per verdict key); the model
/// meanwhile runs on the CPU.
pub(crate) fn queue(path: &Path, kind: ModelKind, device: &Arc<Device>, file: Option<&PathBuf>) {
    let key = key(path, kind, device);
    let job = Job {
        path: path.to_path_buf(),
        kind,
        device: device.clone(),
        file: file.cloned(),
        cpu: Mutex::new(None),
    };
    if CHECKS.enqueue(key, job) {
        log::info!(
            "AI {}: on the CPU until its {} check on {} has run (queued for when the app is idle)",
            kind.name(),
            device.provider,
            device.label
        );
    }
}

/// What a load does with a GPU provider (`Model::load_checked`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Plan {
    /// Make a GPU session and use it.
    Use,
    /// Not this provider (a known "differs" or "slower").
    Skip,
    /// Check now and use the GPU if it passes (CLI, forced provider).
    CheckNow,
    /// Use the next provider (the CPU) now; check when the app is idle.
    Queue,
}

/// How checks run for a load.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Checks {
    /// No check (benchmarks report the GPU's output as it is).
    Off,
    /// Before the model is used.
    Now,
    /// In the background (`queue`), the model on the CPU meanwhile.
    Idle,
}

/// `cpu_after`: the load may fall back to the CPU. A GPU-only load (Moebius)
/// has nothing to run on meanwhile, so it checks before use.
pub(crate) fn plan(known: Option<Verdict>, checks: Checks, cpu_after: bool) -> Plan {
    match (checks, known) {
        (Checks::Off, _) | (_, Some(Verdict::Faster)) => Plan::Use,
        (_, Some(Verdict::Differs | Verdict::Slower)) => Plan::Skip,
        (Checks::Idle, None) if cpu_after => Plan::Queue,
        (Checks::Now | Checks::Idle, None) => Plan::CheckNow,
    }
}

/// The verdict known for `path` on `device`, and its key.
pub(crate) fn known(
    path: &Path,
    kind: ModelKind,
    device: &Device,
    file: Option<&PathBuf>,
) -> (Option<Verdict>, String) {
    let key = key(path, kind, device);
    (verdict(&key, file), key)
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

    /// A device accepted before the speed comparison keeps its models.
    #[test]
    fn an_old_ok_without_a_speed_comparison_stays_accepted() {
        let dir = tempfile_dir("old-ok");
        let file = dir.join("gpu-checks.tsv");
        std::fs::write(&file, "ok\ta | WebGPU\ndiffers\tb | WebGPU\n").unwrap();
        let verdicts = load_file(&file);
        assert_eq!(verdicts.get("a | WebGPU"), Some(&Verdict::Faster));
        assert_eq!(verdicts.get("b | WebGPU"), Some(&Verdict::Differs));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_stalled_gpu_check_is_left_behind() {
        let (_never, hang) = std::sync::mpsc::channel::<()>();
        let hang = Mutex::new(hang);
        let started = Instant::now();
        let end = gpu_runs(
            || Ok(()),
            move |_: &mut ()| {
                let _ = hang.lock().unwrap().recv();
                Ok(())
            },
            Duration::from_millis(10),
            Duration::from_millis(200),
            &Stop::new(),
        )
        .unwrap();
        assert!(matches!(end, GpuEnd::TimedOut));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    /// A session whose creation hangs (shader compiles) is bounded too.
    #[test]
    fn a_gpu_session_that_never_gets_made_is_left_behind() {
        let (_never, hang) = std::sync::mpsc::channel::<()>();
        let end = gpu_runs(
            move || {
                let _ = hang.recv();
                Ok(())
            },
            |_: &mut ()| Ok(()),
            Duration::from_millis(10),
            Duration::from_millis(200),
            &Stop::new(),
        )
        .unwrap();
        assert!(matches!(end, GpuEnd::TimedOut));
    }

    /// A stopped check waits for its GPU thread (here it ends at the stop,
    /// as a terminated ONNX run does), skips the warm run and keeps nothing.
    #[test]
    fn a_stopped_gpu_check_ends_its_runs_and_has_no_result() {
        let stop = Stop::new();
        let stopped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let seen = stopped.clone();
        stop.on_stop(move || seen.store(true, std::sync::atomic::Ordering::SeqCst));
        let calls = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let counted = calls.clone();
        let stopper = stop.clone();
        let flag = stopped.clone();
        let end: GpuEnd<(), ()> = gpu_runs(
            || Ok(()),
            move |_: &mut ()| {
                counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                stopper.stop();
                while !flag.load(std::sync::atomic::Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(1));
                }
                // Slower than the CPU's 1 ms: a warm run would follow.
                std::thread::sleep(Duration::from_millis(5));
                Err(anyhow!("Exiting due to terminate flag being set to true."))
            },
            Duration::from_millis(1),
            Duration::from_secs(5),
            &stop,
        )
        .unwrap();
        assert!(matches!(end, GpuEnd::Stopped));
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn a_slow_first_gpu_run_is_timed_again_warm() {
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let counted = calls.clone();
        let GpuEnd::Ran(_, (_, _, warm)) = gpu_runs(
            || Ok(()),
            move |_: &mut ()| {
                // The first run compiles: slower than the CPU's 20 ms.
                if counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                    std::thread::sleep(Duration::from_millis(40));
                }
                Ok(())
            },
            Duration::from_millis(20),
            Duration::from_secs(5),
            &Stop::new(),
        )
        .unwrap() else {
            panic!("the runs did not finish");
        };
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
        assert!(warm.unwrap() < Duration::from_millis(20));
    }

    /// The app's first load of a model without a verdict uses the CPU and
    /// queues the check; the CLI and a forced provider check at once; a
    /// known verdict decides without a check.
    #[test]
    fn a_load_without_a_verdict_goes_to_the_cpu_and_queues_the_check() {
        assert_eq!(plan(None, Checks::Idle, true), Plan::Queue);
        // Without a CPU to fall back to (Moebius), the check runs first.
        assert_eq!(plan(None, Checks::Idle, false), Plan::CheckNow);
        assert_eq!(plan(None, Checks::Now, true), Plan::CheckNow);
        assert_eq!(plan(None, Checks::Off, true), Plan::Use);
        for checks in [Checks::Idle, Checks::Now] {
            assert_eq!(plan(Some(Verdict::Faster), checks, true), Plan::Use);
            // A GPU whose output differed stays off, also from the queue.
            assert_eq!(plan(Some(Verdict::Differs), checks, true), Plan::Skip);
            assert_eq!(plan(Some(Verdict::Slower), checks, true), Plan::Skip);
        }
    }

    /// A verdict a background check commits is in the file and in memory,
    /// so the next load (and the CLI, and the next session) uses it; a
    /// stopped check's verdict is not.
    #[test]
    fn a_committed_verdict_decides_the_next_load_and_a_stopped_one_does_not() {
        let dir = tempfile_dir("committed");
        let file = dir.join("ai-gpu-checks.tsv");
        let key = format!("committed-{}.onnx | WebGPU", std::process::id());
        let stopped_key = format!("stopped-{}.onnx | WebGPU", std::process::id());
        assert_eq!(
            plan(verdict(&key, Some(&file)), Checks::Idle, true),
            Plan::Queue
        );
        let started = Instant::now();
        let stop = Stop::new();
        let gpu = conclude(
            key.clone(),
            ModelKind::Subject,
            (Provider::WebGpu, "test GPU"),
            Some(&file),
            (started, ""),
            Ok(Measured::Refused(Verdict::Slower, anyhow!("slower"))),
            &stop,
            |_, gpu| Some(gpu),
        );
        assert!(gpu.is_some_and(|r| r.is_err()));
        assert_eq!(
            plan(verdict(&key, Some(&file)), Checks::Idle, true),
            Plan::Skip
        );
        assert_eq!(load_file(&file).get(&key), Some(&Verdict::Slower));
        let stop = Stop::new();
        stop.stop();
        assert!(
            conclude(
                stopped_key.clone(),
                ModelKind::Subject,
                (Provider::WebGpu, "test GPU"),
                Some(&file),
                (started, ""),
                Ok(Measured::Refused(Verdict::Differs, anyhow!("differs"))),
                &stop,
                |_, gpu| Some(gpu),
            )
            .is_none()
        );
        assert_eq!(verdict(&stopped_key, Some(&file)), None);
        assert!(!load_file(&file).contains_key(&stopped_key));
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

    /// A faster GPU's session goes to the first model still on the CPU; a
    /// model that is on a GPU already (or none) leaves it to be dropped.
    #[test]
    fn a_faster_verdict_moves_the_waiting_cpu_model_to_the_gpu() {
        let on_gpu = Arc::new(Slot::new("subject", "gpu", Provider::WebGpu));
        let on_cpu = Arc::new(Slot::new("subject", "cpu", Provider::Cpu));
        assert_eq!(
            adopt_into(
                [on_gpu.clone(), on_cpu.clone()],
                "checked",
                Provider::WebGpu
            ),
            None
        );
        assert_eq!(on_cpu.provider(), Provider::WebGpu);
        assert_eq!(on_cpu.session().with(|s| *s), "checked");
        assert_eq!(on_gpu.session().with(|s| *s), "gpu");
        assert_eq!(
            adopt_into([on_cpu], "again", Provider::WebGpu),
            Some("again")
        );
        assert_eq!(
            adopt_into(Vec::<Arc<Slot<&str>>>::new(), "x", Provider::WebGpu),
            Some("x")
        );
    }

    #[test]
    fn a_missing_file_has_no_verdicts() {
        assert!(load_file(Path::new("/nonexistent/rawtonic/gpu-checks.tsv")).is_empty());
    }
}
