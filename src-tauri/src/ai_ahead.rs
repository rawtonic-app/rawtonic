// SPDX-License-Identifier: AGPL-3.0-or-later
//! When AI work made ahead of a click may start (G74/MEM18).
//!
//! The masks computed when a photo opens in Develop (Subject, then Sky, then
//! the object masks' encoding) and the models loaded for them are a
//! convenience: a click on Subject answers at once. Running out of memory is
//! not acceptable for it. On a 14 GB Radeon 680M laptop (AI on the CPU) the
//! Subject model's session build and its first run started beside the
//! full-resolution decodes of a 50 MP RAF and three CR2s and took the app to
//! 6.0–7.0 GB at +6–8 s after the Develop click, and the system below
//! 1.2 GB available when about 7 GB were free before (G74/WG74).
//!
//! So a job made ahead starts only
//! - once the open photo's full-resolution decode and its frames have
//!   settled (`multi_view_prefetch::primary_settled`);
//! - one job at a time;
//! - and when the memory free for work that may wait
//!   (`memory_pressure::work_headroom`, the multi view's background decodes
//!   use the same) covers the job's estimated peak (`estimated_peak`), and
//!   a background decode's peak (`BACKGROUND_DECODE`) for each of the multi
//!   view and the step prefetch that has one under way (it ends beside the
//!   job; the next one waits for the job). Waiting for the background decodes
//!   instead had the ahead Subject ready 11-17 s after the photo's decode
//!   started instead of 3-5 s on a Mac with 9 GB free (MEM18).
//!
//! Loading a model ahead (`preload_ai_mask_models`) is a job of its own,
//! admitted on the load's peak alone (`estimated_load_peak`), not on the
//! model and its run: where the memory never covers the run (14 GB beside
//! large RAWs, WMEM18: 3.3–3.5 GB free of 4.7 GB needed), the model is
//! loaded and a click pays only the network (MEM19). The run made ahead
//! keeps its own rule (`estimated_peak` with the model loaded).
//!
//! It waits for that up to `ADMIT_WAIT`, then is dropped (a click computes
//! the mask), and the command says so (the webview asks again when the
//! tools open). While it runs (`AheadRun`), the background decodes wait in
//! turn (`running`): the multi view's and the step prefetch's, which were
//! counted in neither the job's verdict nor its estimate. A request the photographer makes (opening the masking tools,
//! a click on Subject) never waits here: it starts at once, and a job made
//! ahead that waits here starts with it (`ask_now`), and then runs as the
//! request (`AheadRun::asked`). With its model loaded, such a run waits only
//! for the open photo's frames its click started, and only where the memory
//! does not cover it beside them (`wait_for_photo_frames`, MEM20). Nothing
//! changes in how a mask is computed, only when.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// Work made ahead of a click.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AheadJob {
    /// The Subject mask (BiRefNet), its model loaded first when it is not.
    Subject,
    /// The Sky mask (UperNet), the same.
    Sky,
    /// The object masks' SAM 2.1 encoding of the photo, the same.
    ObjectEncoding,
}

impl AheadJob {
    pub fn name(self) -> &'static str {
        match self {
            AheadJob::Subject => "subject",
            AheadJob::Sky => "sky",
            AheadJob::ObjectEncoding => "object",
        }
    }
}

const MB: u64 = 1_000_000;

/// The memory a job made ahead adds to the app at its peak: its model (when
/// `loaded` is false: its weights and the load's transient) and its run's
/// transient, on the CPU (`on_cpu`, ONNX Runtime's CPU provider, where the
/// app holds it all) or on a GPU provider (Core ML, DirectML).
///
/// The largest rise measured, model and run together (notes:
/// benchmark-results/mem18-2026-11-25, phys_footprint at 0.5 s from the
/// job's start to its mask, 45 MP NEF in a 4-up, M5 Pro; and
/// benchmark-results/aimem-cpu-mask-2026-11-14):
/// - Subject on the CPU: +1.4–4.6 GB on the Mac (arm64, KleidiAI keeps a
///   1.6 GB buffer there, AIMEM); 1.0 GB model with a 0.67 GB load
///   transient and a 2.2–2.55 GB run on x86-like settings (AIMEM) →
///   4700 MB cold, 3700 MB with the model loaded.
/// - Subject on Core ML: +3.2–4.7 GB, the model about 0.5 GB of it
///   (`ai_lifecycle`) → 4700 MB cold, 4200 MB loaded.
/// - Sky: +0.26–0.64 GB (Core ML) and +0.59–1.08 GB (CPU) → 1200 MB cold,
///   800 MB loaded.
/// - Object encoding (SAM 2.1 encoder, ONNX Runtime on the CPU): +0.16–0.76
///   GB → 1000 MB cold, 600 MB loaded.
pub fn estimated_peak(job: AheadJob, on_cpu: bool, loaded: bool) -> u64 {
    let (model, run) = match (job, on_cpu) {
        (AheadJob::Subject, true) => (1000, 3700),
        (AheadJob::Subject, false) => (500, 4200),
        (AheadJob::Sky, _) => (400, 800),
        (AheadJob::ObjectEncoding, _) => (400, 600),
    };
    if loaded { run * MB } else { (model + run) * MB }
}

/// The memory loading a job's model alone adds to the app at its peak (the
/// weights and the load's transient, no run): what `preload_ai_mask_models`
/// needs, which loads the model ahead without running it, so a click (or
/// the run made ahead, `estimated_peak` with the model loaded, when the
/// memory allows it later) pays only the network (G74/MEM19).
///
/// The CPU's figure whatever the provider setting: a model loads on the
/// CPU also when no GPU provider works, its check is pending or failed
/// (Windows, Linux) or a Core ML file is missing, and the CPU load is the
/// largest measured. The largest rise from the load's start to its end
/// (notes: benchmark-results/mem19-2026-11-26, phys_footprint at 0.25 s,
/// 45 MP NEF in a 4-up, M5 Pro; and benchmark-results/aimem-cpu-mask-2026-11-14):
/// - Subject on the CPU: 1.0 GB settled with a 0.67 GB load transient on
///   x86-like settings (AIMEM); +0.42–1.35 GB on the Mac (5 loads from the
///   saved optimized graph, 0.6–4.9 s) → 1700 MB. On Core ML +0.28–0.60 GB
///   (4 loads, 0.2–0.75 s).
/// - Sky: 0.2–0.5 GB beside Subject (AIMEM; Core ML +0.01 GB, MEM19) →
///   500 MB.
/// - Object encoding: not loaded ahead on its own; the model's share of
///   `estimated_peak` (400 MB).
pub fn estimated_load_peak(job: AheadJob) -> u64 {
    let load = match job {
        AheadJob::Subject => 1700,
        AheadJob::Sky => 500,
        AheadJob::ObjectEncoding => 400,
    };
    load * MB
}

/// Whether a job made ahead may start now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Start,
    /// Another job made ahead runs (`running`): one at a time, so their
    /// peaks do not add up (Sky's model load had started beside Subject's
    /// run).
    WaitForAheadRun,
    /// The open photo's full-resolution decode or a frame of it is under way.
    WaitForDecodes,
    /// The memory free for such work (`room`) is below the job's `need`.
    WaitForMemory {
        room: u64,
        need: u64,
    },
}

/// A background decode's peak: 61 MP at 40 B per pixel, as the multi view
/// counts one whose size is not known (`multi_view::DECODE_BYTES_PER_PIXEL`,
/// `UNKNOWN_PIXELS`).
pub const BACKGROUND_DECODE: u64 =
    crate::multi_view::UNKNOWN_PIXELS * crate::multi_view::DECODE_BYTES_PER_PIXEL;

/// The verdict for a job of estimated peak `need` (`estimated_peak`), with
/// another job made ahead running or not (`ahead_running`), the open
/// photo's decode and frames settled or not (`primary_settled`), the
/// background workers with a decode under way (`background`) and `room` bytes free
/// for work that may wait (`memory_pressure::work_headroom`; None: the
/// memory cannot be read, as for the multi view's background decodes). A
/// request the photographer made (`asked`) always starts.
pub fn verdict(
    asked: bool,
    ahead_running: bool,
    primary_settled: bool,
    background: u64,
    room: Option<u64>,
    need: u64,
) -> Verdict {
    if asked {
        return Verdict::Start;
    }
    if ahead_running {
        return Verdict::WaitForAheadRun;
    }
    if !primary_settled {
        return Verdict::WaitForDecodes;
    }
    let need = need.saturating_add(background.saturating_mul(BACKGROUND_DECODE));
    match room {
        Some(room) if room < need => Verdict::WaitForMemory { room, need },
        _ => Verdict::Start,
    }
}

/// How long a job made ahead waits for its verdict at most.
pub const ADMIT_WAIT: Duration = Duration::from_secs(60);
/// How often the verdict is asked again while it waits.
const POLL: Duration = Duration::from_millis(250);
/// How long the open photo's decode and frames must have been quiet.
pub const QUIET: Duration = Duration::from_millis(750);

/// Counts the photographer's requests for the masks (`ask_now`).
static ASKED: AtomicU64 = AtomicU64::new(0);
/// The tasks the photographer asked for (`ask_now`), newest last: a task
/// asked for before its command reached `admit` (it waits for the photo's
/// decode first) still starts at once.
static ASKED_TASKS: std::sync::Mutex<std::collections::VecDeque<String>> =
    std::sync::Mutex::new(std::collections::VecDeque::new());
/// Asked tasks remembered at most.
const ASKED_TASKS_KEPT: usize = 32;

/// The photographer asked for the masks (opened the masking tools): jobs
/// made ahead that wait for their verdict start now, and `task_id` (the
/// photo's run made ahead) whenever it gets here.
pub fn ask_now(task_id: Option<String>) {
    if let Some(task_id) = task_id {
        let mut tasks = ASKED_TASKS.lock().unwrap_or_else(|e| e.into_inner());
        if tasks.len() >= ASKED_TASKS_KEPT {
            tasks.pop_front();
        }
        tasks.push_back(task_id);
    }
    ASKED.fetch_add(1, Ordering::SeqCst);
}

/// The photographer's requests so far (`ask_now`): read when a command
/// arrives, before it waits for anything, so a request made meanwhile is
/// seen (`asked_since`).
pub fn asked_count() -> u64 {
    ASKED.load(Ordering::SeqCst)
}

/// Whether the photographer asked for the masks since `count`
/// (`asked_count`) or asked for `task_id` itself.
pub fn asked_since(count: u64, task_id: Option<&str>) -> bool {
    ASKED.load(Ordering::SeqCst) != count
        || task_id.is_some_and(|id| {
            ASKED_TASKS
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .iter()
                .any(|asked| asked == id)
        })
}

/// Jobs made ahead that run (`AheadRun`).
static RUNNING: AtomicUsize = AtomicUsize::new(0);

/// Whether a job made ahead runs: the background decodes wait for it.
pub fn running() -> bool {
    RUNNING.load(Ordering::SeqCst) > 0
}

/// A job made ahead runs while this lives (`admit`).
#[must_use = "the job counts as running while this lives"]
#[derive(Debug)]
pub struct AheadRun {
    asked: bool,
}

impl AheadRun {
    /// Starts a job: `asked` at once, else only while no other job made
    /// ahead runs (None: one started meanwhile). Two jobs that found the way
    /// free in the same instant (Subject's model load and its run, asked for
    /// together as Develop opens) start one after the other.
    fn start(asked: bool) -> Option<Self> {
        if asked {
            RUNNING.fetch_add(1, Ordering::SeqCst);
            return Some(AheadRun { asked });
        }
        RUNNING
            .compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst)
            .ok()
            .map(|_| AheadRun { asked })
    }

    /// Whether it started because the photographer asked for it (`ask_now`):
    /// they wait for it now, so it runs as their click would (G74/MEM20).
    pub fn asked(&self) -> bool {
        self.asked
    }
}

impl Drop for AheadRun {
    fn drop(&mut self) {
        RUNNING.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Why a job made ahead was not started.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotStarted {
    /// `give_up` said so (the photo changed, the task was cancelled).
    GaveUp(&'static str),
    /// Its verdict was not `Start` within `ADMIT_WAIT`.
    TimedOut(Verdict),
}

/// The app's memory for the log: "app X MB, Y MB available of Z MB, W MB
/// free for work that may wait".
pub fn memory_line() -> String {
    crate::memory_pressure::memory_line()
}

/// Waits until `job` (estimated peak `need`, read again at each poll: a
/// model loaded ahead meanwhile leaves only its run, MEM19) may start: `Ok` with the guard
/// to hold while it runs, at once when `asked` says the photographer asked
/// for it (`asked_since`); checks `give_up` before each verdict. Logs a line
/// when it is held and when it starts, with the memory, so a run's log
/// shows where the memory went.
pub async fn admit(
    job: AheadJob,
    asked: impl Fn() -> bool,
    need: impl Fn() -> u64,
    mut give_up: impl FnMut() -> Option<&'static str>,
) -> Result<AheadRun, NotStarted> {
    let start = Instant::now();
    let mut held: Option<Verdict> = None;
    loop {
        if let Some(reason) = give_up() {
            return Err(NotStarted::GaveUp(reason));
        }
        let asked = asked();
        let settled = crate::multi_view_prefetch::primary_settled(QUIET);
        let background = crate::multi_view_prefetch::background_under_way();
        let room = crate::memory_pressure::work_headroom();
        let need = need();
        let mut now = verdict(asked, running(), settled, background, room, need);
        let started = if now == Verdict::Start {
            AheadRun::start(asked)
        } else {
            None
        };
        if now == Verdict::Start && started.is_none() {
            now = Verdict::WaitForAheadRun;
        }
        if let Some(started) = started {
            log::info!(
                "AI {} ahead: started after {} ms{} (estimate {} MB); {}",
                job.name(),
                start.elapsed().as_millis(),
                if asked { ", asked for" } else { "" },
                need / MB,
                memory_line()
            );
            return Ok(started);
        }
        // One line per change of reason, not per poll.
        if held.as_ref().map(std::mem::discriminant) != Some(std::mem::discriminant(&now)) {
            log::info!(
                "AI {} ahead: held, {} (estimate {} MB); {}",
                job.name(),
                describe(now),
                need / MB,
                memory_line()
            );
        }
        held = Some(now);
        if start.elapsed() >= ADMIT_WAIT {
            log::info!(
                "AI {} ahead: not started, still {} after {} s; {}",
                job.name(),
                describe(now),
                ADMIT_WAIT.as_secs(),
                memory_line()
            );
            return Err(NotStarted::TimedOut(now));
        }
        tokio::time::sleep(POLL).await;
    }
}

/// How long a run the photographer asked for waits at most for the open
/// photo's frames (`wait_for_photo_frames`).
pub const FRAMES_WAIT: Duration = Duration::from_secs(5);
/// How often it looks again meanwhile.
const FRAMES_POLL: Duration = Duration::from_millis(50);

/// Whether a run the photographer asked for (peak `need`) waits for the open
/// photo's frames: while they are under way or ended less than `QUIET` ago,
/// or the request came less than `QUIET` ago (`frames_settled` false: its
/// decode, a frame, the saved thumbnail of an edit,
/// `multi_view_prefetch::primary_settled`), and the memory free for work that
/// may wait (`room`, None: not readable) does not cover the run beside them.
/// Once they settled, or the memory covers it, it starts (it is asked for);
/// after `FRAMES_WAIT` it starts beside frames still under way.
pub fn asked_run_waits(frames_settled: bool, room: Option<u64>, need: u64) -> bool {
    !frames_settled && room.is_some_and(|room| room < need)
}

/// A run the photographer asked for, with its model loaded, waits here up to
/// `FRAMES_WAIT` while `asked_run_waits` says so, and logs how long. G74/MEM20:
/// with the Subject model preloaded (MEM19) a click on New subject layer ran
/// the network (2.2–2.5 GB on the CPU) at once, beside the new layer's
/// full-resolution frame and its saved thumbnail, and a 14 GB Radeon 680M
/// laptop went from 4.2 to 10.2 GB private within 4 s (WMEM19, two of three
/// clicks stopped at 1.1–1.3 GB available); while the model loaded (WMEM18,
/// 9.9 s) that work had ended before the run, at 6.2 GB. `cancelled` ends
/// the wait.
pub async fn wait_for_photo_frames(job: AheadJob, need: u64, cancelled: impl Fn() -> bool) {
    let start = Instant::now();
    let mut waited = false;
    let why = loop {
        // Quiet since the request too: the frame its click asked for may not
        // have reached the preview worker yet, and the edit's save (300 ms
        // after it) has not started its thumbnail (MEM20 review r1).
        let settled =
            crate::multi_view_prefetch::primary_settled(QUIET) && start.elapsed() >= QUIET;
        let room = crate::memory_pressure::work_headroom();
        if cancelled() {
            break "cancelled";
        }
        if settled {
            break "after the photo's frames";
        }
        if !asked_run_waits(settled, room, need) {
            break "with memory for it";
        }
        if !waited {
            waited = true;
            log::info!(
                "AI {} mask: asked for, waits for the photo's frames ({} MB free for it of {} MB needed); {}",
                job.name(),
                room.unwrap_or(0) / MB,
                need / MB,
                memory_line()
            );
        }
        if start.elapsed() >= FRAMES_WAIT {
            break "beside frames still under way";
        }
        tokio::time::sleep(FRAMES_POLL).await;
    };
    if waited {
        log::info!(
            "AI {} mask: asked for, wait ended {why}, {} ms; {}",
            job.name(),
            start.elapsed().as_millis(),
            memory_line()
        );
    }
}

/// Runs the photographer asked for that started short of memory
/// (`AskedRunShort`).
static ASKED_RUNS_SHORT: AtomicUsize = AtomicUsize::new(0);

/// A run the photographer asked for that started where the memory free for
/// work that may wait did not cover it (`work_headroom` below its `need`)
/// runs while this lives: the open photo's saved thumbnail waits for it
/// (`wait_for_asked_runs`). An edit's save can come after the frames wait
/// ended (queued behind other photo writes in the webview, review r2), and
/// its full-resolution render beside the network is what MEM20 keeps apart.
#[must_use = "the run counts while this lives"]
pub struct AskedRunShort(());

impl AskedRunShort {
    /// Counts the run when `room` (None: not readable) is below `need`.
    pub fn start_if_short(room: Option<u64>, need: u64) -> Option<Self> {
        room.is_some_and(|room| room < need).then(|| {
            ASKED_RUNS_SHORT.fetch_add(1, Ordering::SeqCst);
            AskedRunShort(())
        })
    }
}

impl Drop for AskedRunShort {
    fn drop(&mut self) {
        ASKED_RUNS_SHORT.fetch_sub(1, Ordering::SeqCst);
    }
}

/// How long an edit's saved thumbnail waits at most for runs asked for
/// short of memory (a Subject run on the CPU takes about 16 s on a Radeon
/// 680M laptop); then it renders beside them.
pub const THUMBNAIL_WAIT: Duration = Duration::from_secs(60);

/// Whether a run asked for short of memory runs (`AskedRunShort`).
pub fn asked_runs_short() -> bool {
    ASKED_RUNS_SHORT.load(Ordering::SeqCst) > 0
}

/// The photos whose saved thumbnail waits for such a run (`WaitingThumbnail`).
static THUMBNAILS_WAITING: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
/// Thumbnails that waited render one at a time once the run ended.
static WAITED_RENDER: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// An edit's saved thumbnail of `path` that waits for a run asked for short
/// of memory while this lives. One per photo (review r3): the edits saved
/// meanwhile are in the photo's recipe, which the waiting one renders, so a
/// later save's thumbnail is left to it instead of a second full-resolution
/// render once the run ends.
#[must_use = "the thumbnail counts as waiting while this lives"]
pub struct WaitingThumbnail(String);

impl WaitingThumbnail {
    /// None: a thumbnail of `path` waits already (it renders this edit).
    pub fn join(path: &str) -> Option<Self> {
        let mut waiting = THUMBNAILS_WAITING.lock().unwrap_or_else(|e| e.into_inner());
        if waiting.iter().any(|p| p == path) {
            return None;
        }
        waiting.push(path.to_string());
        Some(WaitingThumbnail(path.to_string()))
    }

    /// Waits up to `THUMBNAIL_WAIT` for the runs (`wait_for_asked_runs`),
    /// then returns how long and the turn to render in (one waited thumbnail
    /// at a time); the photo no longer counts as waiting.
    pub fn wait(self) -> (Duration, std::sync::MutexGuard<'static, ()>) {
        let waited = wait_for_asked_runs(THUMBNAIL_WAIT);
        drop(self);
        (
            waited,
            WAITED_RENDER.lock().unwrap_or_else(|e| e.into_inner()),
        )
    }
}

impl Drop for WaitingThumbnail {
    fn drop(&mut self) {
        THUMBNAILS_WAITING
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|p| *p != self.0);
    }
}

/// Blocks while a run asked for short of memory runs (`AskedRunShort`), up
/// to `max`; returns how long it waited (zero: none ran).
pub fn wait_for_asked_runs(max: Duration) -> Duration {
    let start = Instant::now();
    while ASKED_RUNS_SHORT.load(Ordering::SeqCst) > 0 && start.elapsed() < max {
        std::thread::sleep(Duration::from_millis(100));
    }
    start.elapsed()
}

fn describe(verdict: Verdict) -> String {
    match verdict {
        Verdict::Start => "free to start".into(),
        Verdict::WaitForAheadRun => "another job made ahead runs".into(),
        Verdict::WaitForDecodes => "the photo's decode or a frame of it is under way".into(),
        Verdict::WaitForMemory { room, need } => {
            format!("{} MB free for it of {} MB needed", room / MB, need / MB)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GB: u64 = 1000 * MB;

    #[test]
    fn a_job_made_ahead_waits_for_the_photo_then_for_memory() {
        let need = estimated_peak(AheadJob::Subject, true, false);
        assert_eq!(need, 4700 * MB);
        // The photo's decode or frame under way: held whatever the memory.
        assert_eq!(
            verdict(false, false, false, 0, Some(20 * GB), need),
            Verdict::WaitForDecodes
        );
        // Settled, too little room (WG74: about 1.4 GB beside four large decodes).
        assert_eq!(
            verdict(false, false, true, 0, Some(1400 * MB), need),
            Verdict::WaitForMemory {
                room: 1400 * MB,
                need
            }
        );
        // Settled with room for its peak.
        assert_eq!(
            verdict(false, false, true, 0, Some(need), need),
            Verdict::Start
        );
        // Memory not readable: as the multi view's background decodes.
        assert_eq!(verdict(false, false, true, 0, None, need), Verdict::Start);
        // Another job made ahead runs: one at a time.
        assert_eq!(
            verdict(false, true, true, 0, Some(20 * GB), need),
            Verdict::WaitForAheadRun
        );
    }

    #[test]
    fn a_background_decode_under_way_counts_its_peak() {
        let need = estimated_peak(AheadJob::Subject, false, false);
        // Room for the job alone is not enough beside a background decode.
        assert_eq!(
            verdict(false, false, true, 1, Some(need), need),
            Verdict::WaitForMemory {
                room: need,
                need: need + BACKGROUND_DECODE
            }
        );
        // Room for both (a Mac with 9 GB free): it starts, no waiting for
        // the decode.
        assert_eq!(
            verdict(false, false, true, 1, Some(9 * GB), need),
            Verdict::Start
        );
        // The multi view and the step prefetch both decoding: each counts.
        assert_eq!(
            verdict(false, false, true, 2, Some(9 * GB), need),
            Verdict::WaitForMemory {
                room: 9 * GB,
                need: need + 2 * BACKGROUND_DECODE
            }
        );
    }

    #[test]
    fn a_request_the_photographer_made_is_never_held() {
        let need = estimated_peak(AheadJob::Subject, true, false);
        assert_eq!(verdict(true, true, false, 1, Some(0), need), Verdict::Start);
        assert_eq!(verdict(true, false, true, 1, Some(0), need), Verdict::Start);
        assert_eq!(verdict(true, true, false, 0, None, need), Verdict::Start);
    }

    #[test]
    fn a_loaded_model_needs_only_its_run() {
        for job in [AheadJob::Subject, AheadJob::Sky, AheadJob::ObjectEncoding] {
            for on_cpu in [true, false] {
                assert!(estimated_peak(job, on_cpu, true) < estimated_peak(job, on_cpu, false));
            }
        }
    }

    #[test]
    fn the_model_is_loaded_ahead_where_its_run_is_held() {
        // WMEM18 (Radeon 680M, 14 GB, four large RAWs): 3.3–3.5 GB free for
        // work that may wait once the decodes settled.
        let room = Some(3300 * MB);
        let load = estimated_load_peak(AheadJob::Subject);
        assert_eq!(verdict(false, false, true, 0, room, load), Verdict::Start);
        // The run stays held, cold and with the model loaded beside it
        // (the load's memory then no longer counts as free).
        let after_load = Some(3300 * MB - load);
        for (room, loaded) in [(room, false), (after_load, true)] {
            let need = estimated_peak(AheadJob::Subject, true, loaded);
            assert_eq!(
                verdict(false, false, true, 0, room, need),
                Verdict::WaitForMemory {
                    room: room.unwrap(),
                    need
                }
            );
        }
        // Still not beside the decodes, nor beside another job made ahead.
        assert_eq!(
            verdict(false, false, false, 0, Some(20 * GB), load),
            Verdict::WaitForDecodes
        );
        assert_eq!(
            verdict(false, true, true, 0, Some(20 * GB), load),
            Verdict::WaitForAheadRun
        );
        // A click is never held, preload or not.
        assert_eq!(
            verdict(
                true,
                true,
                false,
                1,
                Some(0),
                estimated_peak(AheadJob::Subject, true, true)
            ),
            Verdict::Start
        );
    }

    #[test]
    fn a_load_alone_needs_less_than_the_model_and_its_run() {
        for job in [AheadJob::Subject, AheadJob::Sky, AheadJob::ObjectEncoding] {
            for on_cpu in [true, false] {
                let load = estimated_load_peak(job);
                assert!(load > 0);
                assert!(load < estimated_peak(job, on_cpu, false));
            }
        }
    }

    #[tokio::test]
    async fn the_run_reads_its_estimate_again_once_the_model_is_loaded() {
        // Held on an estimate no memory covers until the model is
        // "loaded" (here: the second poll), then it needs nothing.
        let polls = std::sync::atomic::AtomicU32::new(0);
        let need = || {
            if polls.fetch_add(1, Ordering::SeqCst) == 0 {
                u64::MAX
            } else {
                0
            }
        };
        // Up to 10 s: another test's run may hold the way (one at a time).
        let run = tokio::time::timeout(
            Duration::from_secs(10),
            admit(AheadJob::Subject, || false, need, || None),
        )
        .await
        .expect("started before the timeout")
        .expect("started");
        // Memory that cannot be read starts it on the first poll.
        if crate::memory_pressure::work_headroom().is_some() {
            assert!(polls.load(Ordering::SeqCst) >= 2);
        }
        assert!(!run.asked());
        drop(run);
    }

    #[tokio::test]
    async fn the_photographer_asking_starts_a_held_job() {
        // Never room here (u64::MAX needed); the ask lets it start.
        let asked_at = asked_count();
        let asking = tokio::spawn(async {
            tokio::time::sleep(Duration::from_millis(50)).await;
            ask_now(None);
        });
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            admit(
                AheadJob::Sky,
                || asked_since(asked_at, None),
                || u64::MAX,
                || None,
            ),
        )
        .await
        .expect("started before the timeout");
        asking.await.unwrap();
        let run = result.expect("started");
        assert!(running(), "the background decodes wait for it");
        assert!(run.asked(), "it runs as the photographer's request (MEM20)");
        drop(run);
    }

    #[test]
    fn a_task_asked_for_before_its_command_arrives_is_asked() {
        // Opening the tools while the photo still decoded: the run made
        // ahead reaches `admit` later and starts at once.
        ask_now(Some("subject-precompute-early".into()));
        let arrived = asked_count();
        assert!(asked_since(arrived, Some("subject-precompute-early")));
    }

    #[test]
    fn a_run_asked_for_waits_only_for_the_photos_frames_and_only_short_of_memory() {
        let need = estimated_peak(AheadJob::Subject, true, true);
        // WMEM19 click: 1.4 GB free beside the new layer's frame: waits.
        assert!(asked_run_waits(false, Some(1400 * MB), need));
        // The frames settled: starts, however little memory (asked for).
        assert!(!asked_run_waits(true, Some(0), need));
        // Memory for it beside the frames (a Mac with 9 GB free): at once.
        assert!(!asked_run_waits(false, Some(9 * GB), need));
        // Memory not readable: at once, as the jobs made ahead.
        assert!(!asked_run_waits(false, None, need));
    }

    #[tokio::test]
    async fn a_run_asked_for_short_of_memory_waits_for_the_frame_its_click_asked_for() {
        // Frames settled long ago, never room (u64::MAX needed): it still
        // waits `QUIET` from the request, for the frame and the save its
        // click started (review r1), then starts.
        if crate::memory_pressure::work_headroom().is_none() {
            return;
        }
        let start = Instant::now();
        wait_for_photo_frames(AheadJob::Subject, u64::MAX, || false).await;
        assert!(start.elapsed() >= QUIET);
        assert!(start.elapsed() <= FRAMES_WAIT + Duration::from_secs(1));
    }

    #[test]
    fn a_thumbnail_saved_during_a_run_asked_for_short_of_memory_waits_for_it() {
        // Review r2: the edit's save comes after the frames wait ended
        // (queued behind rating writes); its thumbnail must not render
        // beside the network.
        assert!(AskedRunShort::start_if_short(Some(9 * GB), 3700 * MB).is_none());
        assert!(AskedRunShort::start_if_short(None, 3700 * MB).is_none());
        let run = AskedRunShort::start_if_short(Some(1400 * MB), 3700 * MB).expect("short");
        let thumbnail = std::thread::spawn(|| wait_for_asked_runs(Duration::from_secs(10)));
        std::thread::sleep(Duration::from_millis(400));
        assert!(!thumbnail.is_finished(), "waits while the run runs");
        drop(run);
        let waited = thumbnail.join().unwrap();
        assert!(waited >= Duration::from_millis(400));
        assert!(waited < Duration::from_secs(10));
    }

    #[test]
    fn one_thumbnail_per_photo_waits_for_the_run() {
        // Review r3: edits saved during the run leave their thumbnail to the
        // photo's waiting one (it renders the latest recipe); another photo
        // (stepped to meanwhile) waits on its own.
        let first = WaitingThumbnail::join("mem20-a.NEF").expect("first waits");
        assert!(WaitingThumbnail::join("mem20-a.NEF").is_none());
        let other = WaitingThumbnail::join("mem20-b.NEF").expect("another photo waits too");
        drop(first);
        assert!(WaitingThumbnail::join("mem20-a.NEF").is_some());
        drop(other);
    }

    #[tokio::test]
    async fn a_wait_for_the_frames_ends_when_cancelled() {
        // Never room (u64::MAX needed); cancelled: back at once.
        let start = Instant::now();
        wait_for_photo_frames(AheadJob::Subject, u64::MAX, || true).await;
        assert!(start.elapsed() < FRAMES_WAIT);
    }

    #[tokio::test]
    async fn a_held_job_gives_up_when_its_photo_changes() {
        // Checked before each verdict, so before a start too.
        let result = admit(
            AheadJob::Subject,
            || true,
            || 0,
            || Some("the photo has changed"),
        )
        .await;
        assert_eq!(
            result.map(|_| ()),
            Err(NotStarted::GaveUp("the photo has changed"))
        );
    }
}
