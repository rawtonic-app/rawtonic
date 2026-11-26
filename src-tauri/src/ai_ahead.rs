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
//! It waits for that up to `ADMIT_WAIT`, then is dropped (a click computes
//! the mask), and the command says so (the webview asks again when the
//! tools open). While it runs (`AheadRun`), the background decodes wait in
//! turn (`running`): the multi view's and the step prefetch's, which were
//! counted in neither the job's verdict nor its estimate. A request the photographer makes (opening the masking tools,
//! a click on Subject) never waits here: it starts at once, and a job made
//! ahead that waits here starts with it (`ask_now`). Nothing changes in how a
//! mask is computed, only when.

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
pub struct AheadRun(());

impl AheadRun {
    /// Starts a job: `asked` at once, else only while no other job made
    /// ahead runs (None: one started meanwhile). Two jobs that found the way
    /// free in the same instant (Subject's model load and its run, asked for
    /// together as Develop opens) start one after the other.
    fn start(asked: bool) -> Option<Self> {
        if asked {
            RUNNING.fetch_add(1, Ordering::SeqCst);
            return Some(AheadRun(()));
        }
        RUNNING
            .compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst)
            .ok()
            .map(|_| AheadRun(()))
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

/// Waits until `job` (estimated peak `need`) may start: `Ok` with the guard
/// to hold while it runs, at once when `asked` says the photographer asked
/// for it (`asked_since`); checks `give_up` before each verdict. Logs a line
/// when it is held and when it starts, with the memory, so a run's log
/// shows where the memory went.
pub async fn admit(
    job: AheadJob,
    asked: impl Fn() -> bool,
    need: u64,
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
                u64::MAX,
                || None,
            ),
        )
        .await
        .expect("started before the timeout");
        asking.await.unwrap();
        let run = result.expect("started");
        assert!(running(), "the background decodes wait for it");
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

    #[tokio::test]
    async fn a_held_job_gives_up_when_its_photo_changes() {
        // Checked before each verdict, so before a start too.
        let result = admit(
            AheadJob::Subject,
            || true,
            0,
            || Some("the photo has changed"),
        )
        .await;
        assert_eq!(
            result.map(|_| ()),
            Err(NotStarted::GaveUp("the photo has changed"))
        );
    }
}
