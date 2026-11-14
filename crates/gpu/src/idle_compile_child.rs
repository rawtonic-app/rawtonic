//! Idle compiles in a helper process (G76/WCC11).
//!
//! A driver compile is one call that cannot be stopped midway. On AMD's
//! Windows Vulkan driver (the only one with a compile limit) a display
//! variant takes 25–60 s, so an edit that needed another variant than the
//! idle compile running waited for it to end before its own could start:
//! 17–39 s in the first 90 s after opening a photo on a Radeon 680M
//! (G76/WSMOKE14). Here the idle compile runs in a helper process (the app
//! started again in a headless mode, `set_idle_compile_helper`) that
//! compiles the one pipeline into a pipeline cache of its own and exits.
//! The moment a compile or an export waits for the compile slot
//! (`slot_wanted`) the helper is killed, which frees its memory at once, and
//! the job goes back to the front of its queue for the next idle time.
//! When it finishes, the app compiles the same pipeline through the cache
//! the helper left (`pipeline_cache::foreign`), which the driver answers
//! without compiling (a variant another processor compiled: 140 ms on the
//! 680M, probe round 15).
//!
//! A frame whose features the running job covers still waits for it
//! (`IdleCompiles::covering`): its own compile would be that variant
//! (`export_display_features`), so the wait is its own first compile, part
//! of it already done.
//!
//! Without a helper (tests, the CLI, a cache that cannot be opened, a
//! helper that failed once) idle compiles run in this process as before.

use super::idle_compiles::{IdleJob, IdleTarget};
use super::*;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Starts the helper process for job `job` (`IdleJob::arg`) writing its
/// pipeline cache into the folder given.
pub type IdleCompileHelper =
    Box<dyn Fn(&str, &Path) -> std::io::Result<std::process::Child> + Send + Sync>;

static HELPER: std::sync::OnceLock<IdleCompileHelper> = std::sync::OnceLock::new();
/// Set after a helper failed: idle compiles run in this process from then on.
static UNAVAILABLE: AtomicBool = AtomicBool::new(false);

/// What a helper that failed writes into its folder: why.
pub const HELPER_ERROR_FILE: &str = "error.txt";
/// How often the helper is looked at.
const POLL: Duration = Duration::from_millis(50);
/// A helper still compiling after this is stopped (the longest planned
/// variant took ~60 s on the 680M; a driver hang must not hold the slot).
const LIMIT: Duration = Duration::from_secs(600);

/// Helpers stopped and helpers that compiled their job (tests).
#[cfg(test)]
pub(super) static STOPS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
#[cfg(test)]
pub(super) static DONE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

thread_local! {
    /// The cache a finished helper left, while this thread compiles the
    /// same pipeline through it (`create_compute_pipeline`).
    pub(super) static HELPER_CACHE: std::cell::RefCell<Option<wgpu::PipelineCache>> =
        const { std::cell::RefCell::new(None) };
}

/// Sets how idle compiles start their helper process; once per process.
/// Used only under a compile limit (idle compiles run nowhere else).
pub fn set_idle_compile_helper(helper: IdleCompileHelper) {
    if HELPER.set(helper).is_err() {
        log::warn!("GPU idle compiles: the helper was already set");
    }
}

/// How an idle compile through a helper ended.
pub(super) enum Helped {
    /// The helper compiled it; compile it here through this cache.
    Compiled(wgpu::PipelineCache),
    /// Stopped for a compile or export that waits for the slot, or because
    /// the idle compiles end.
    Stopped,
    /// No helper (or it failed): compile it in this process.
    Unavailable,
}

impl IdleJob {
    /// The job as the helper's argument (`IdleJob::parse`).
    pub(super) fn arg(self) -> String {
        match self {
            IdleJob::Display(features) => format!("display:{features}"),
            IdleJob::LayerSums(features) => format!("layer-sums:{features}"),
            IdleJob::Deferred(features) => format!("deferred:{features}"),
        }
    }

    fn parse(arg: &str) -> Option<Self> {
        let (kind, features) = arg.split_once(':')?;
        let features: usize = features.parse().ok()?;
        if features >= PREVIEW_PIPELINE_COUNT {
            return None;
        }
        match kind {
            "display" => Some(IdleJob::Display(features)),
            "layer-sums" => Some(IdleJob::LayerSums(features)),
            "deferred" => Some(IdleJob::Deferred(features)),
            _ => None,
        }
    }
}

/// Runs `job` in a helper process. `stop`: the idle compiles end.
pub(super) fn compile_in_helper(
    target: &IdleTarget,
    job: IdleJob,
    stop: impl Fn() -> bool,
) -> Helped {
    if UNAVAILABLE.load(Ordering::Relaxed) {
        return Helped::Unavailable;
    }
    let Some(helper) = HELPER.get() else {
        return Helped::Unavailable;
    };
    // The helper writes a cache file of the same name (adapter, driver and
    // build); another adapter or build would write another name.
    let Some(name) = crate::pipeline_cache::file_name_of(&target.device) else {
        return Helped::Unavailable;
    };
    // A folder of its own, made with a random name that no other process
    // can have made first (private to this user on Unix); removed when it
    // drops.
    let folder = match tempfile::Builder::new()
        .prefix("rawtonic-idle-compile-")
        .tempdir()
    {
        Ok(folder) => folder,
        Err(error) => {
            log::warn!(
                "GPU idle compiles run in the app from now on: no folder for the helper ({error})"
            );
            UNAVAILABLE.store(true, Ordering::Relaxed);
            return Helped::Unavailable;
        }
    };
    let give_up = |why: String| {
        log::warn!("GPU idle compiles run in the app from now on: {why}");
        UNAVAILABLE.store(true, Ordering::Relaxed);
        Helped::Unavailable
    };
    let arg = job.arg();
    let started = Instant::now();
    let mut child = match helper(&arg, folder.path()) {
        Ok(child) => child,
        Err(error) => return give_up(format!("the helper did not start ({error})")),
    };
    log::info!(
        "GPU idle compile of {arg}: in a helper process (pid {}, +{:.2} s since start)",
        child.id(),
        since_start()
    );
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return give_up(format!("the helper could not be watched ({error})"));
            }
        }
        let wanted = slot_wanted();
        if wanted || stop() || started.elapsed() >= LIMIT {
            let _ = child.kill();
            let _ = child.wait();
            log::info!(
                "GPU idle compile of {arg}: helper stopped after {:.2} s ({})",
                started.elapsed().as_secs_f64(),
                if wanted {
                    "a compile or an export waits for the slot; it runs again in idle time"
                } else if stop() {
                    "idle compiles end"
                } else {
                    "time limit"
                }
            );
            if !wanted && !stop() {
                UNAVAILABLE.store(true, Ordering::Relaxed);
            }
            #[cfg(test)]
            STOPS.fetch_add(1, Ordering::SeqCst);
            return Helped::Stopped;
        }
        std::thread::sleep(POLL);
    };
    if !status.success() {
        let why =
            std::fs::read_to_string(folder.path().join(HELPER_ERROR_FILE)).unwrap_or_default();
        return give_up(format!(
            "the helper for {arg} ended with {status}{}{}",
            if why.is_empty() { "" } else { ": " },
            why.trim()
        ));
    }
    let cache = crate::pipeline_cache::foreign(&target.device, folder.path(), &name);
    // What the helper left instead, for the log.
    let found = std::fs::read_dir(folder.path())
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    drop(folder);
    match cache {
        Some(cache) => {
            #[cfg(test)]
            DONE.fetch_add(1, Ordering::SeqCst);
            log::info!(
                "GPU idle compile of {arg}: helper done after {:.2} s",
                started.elapsed().as_secs_f64()
            );
            Helped::Compiled(cache)
        }
        None => give_up(format!(
            "the helper for {arg} left no pipeline cache {} (found: {})",
            name.to_string_lossy(),
            if found.is_empty() { "nothing" } else { &found }
        )),
    }
}

/// The helper process's work (the app's headless mode): starts the device
/// as the app does, compiles idle job `job` (`IdleJob::arg`) the way the
/// app's idle compiles do, into a pipeline cache in `folder` named for
/// program version `build` (the one the app opens its own cache with: the
/// app looks for that name), and writes it. `Err` says why not.
pub fn compile_in_helper_process(job: &str, folder: &Path, build: &str) -> Result<(), String> {
    let job = IdleJob::parse(job).ok_or_else(|| format!("unknown job '{job}'"))?;
    let context = GpuContext::headless_with_pipeline_cache_of(Some(folder), build)?;
    let device = Arc::clone(&context.device);
    if crate::pipeline_cache::file_name_of(&device).is_none() {
        return Err("no pipeline cache on this device".into());
    }
    let processor = GpuProcessor::for_exports(context, 64, 64)?;
    let created = idle_compiles::BACKGROUND_COMPILE.with(|background| {
        background.set(true);
        let created = match job {
            IdleJob::Display(features) | IdleJob::LayerSums(features) => create_preview_pipeline(
                &device,
                &processor.preview_shader,
                Some(&processor.display_shader),
                &processor.preview_layout,
                features,
                true,
                true,
                matches!(job, IdleJob::LayerSums(_)),
                false,
            )
            .map(|_| ()),
            IdleJob::Deferred(features) => processor
                .deferred_display
                .compile(&device, features)
                .map(|_| ()),
        };
        background.set(false);
        created
    });
    created?;
    crate::pipeline_cache::save_now(&device)
        .map(|_| ())
        .ok_or_else(|| "the pipeline cache has no data".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jobs_pass_to_the_helper_and_back() {
        for job in [
            IdleJob::Display(5238847),
            IdleJob::LayerSums(7327807),
            IdleJob::Deferred(17),
        ] {
            assert_eq!(IdleJob::parse(&job.arg()), Some(job));
        }
        for bad in ["display", "display:x", "other:1", "display:999999999999"] {
            assert_eq!(IdleJob::parse(bad), None, "{bad}");
        }
    }
}
