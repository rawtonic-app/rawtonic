// SPDX-License-Identifier: AGPL-3.0-or-later
//! When AI work made ahead of a click may start (G74/MEM18, BRK1).
//!
//! The masks computed when a photo opens in Develop (Subject, then Sky, then
//! the object masks' encoding), the models loaded for them and the low-peak
//! Subject model's fetch are a convenience: a click on Subject answers at
//! once. Running out of memory is not acceptable for it. On a 14 GB Radeon
//! 680M laptop (AI on the CPU) the Subject model's session build and its
//! first run started beside the full-resolution decodes of a 50 MP RAF and
//! three CR2s and took the app to 6.0–7.0 GB at +6–8 s after the Develop
//! click (G74/WG74).
//!
//! Such a job is speculative work of the app's memory budget
//! (`memory_broker`, `Class::Speculative`): it reserves its measured peak
//! (`estimated_peak`) and starts only once the open photo's decode and
//! frames have settled, while no request of the photographer waits or runs,
//! one at a time (Subject, Sky, object encoding; the fetch beside them), and
//! when the room beside every reservation held covers its peak and the
//! click reserve (`memory_broker::click_reserve`; not for Subject's own
//! jobs, which are the click's work made ahead). The background decodes
//! reserve theirs in the same budget, so neither waits for the other blindly.
//!
//! It waits for that up to `ADMIT_WAIT`, then is dropped (a click computes
//! the mask), and the command says so (the webview asks again when the
//! tools open). A request the photographer makes (opening the masking tools,
//! a click on Subject) is `Class::Asked`: it never waits for this, and a job
//! made ahead that waits here becomes one when asked for (`ask_now`).
//! Nothing changes in how a mask is computed, only when.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use rawtonic_runtime::memory_budget::{Class, Request};

/// Work made ahead of a click.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AheadJob {
    /// The Subject mask (BiRefNet), its model loaded first when it is not.
    Subject,
    /// The Sky mask (UperNet), the same.
    Sky,
    /// The object masks' SAM 2.1 encoding of the photo, the same.
    ObjectEncoding,
    /// The one-time background fetch of the low-peak Subject model
    /// (`ai_processing::fetch_low_peak_subject_in_background`).
    ModelFetch,
}

impl AheadJob {
    /// The memory budget's label.
    pub fn label(self) -> &'static str {
        match self {
            AheadJob::Subject => "AI subject ahead",
            AheadJob::Sky => "AI sky ahead",
            AheadJob::ObjectEncoding => "AI object encoding ahead",
            AheadJob::ModelFetch => "AI subject model fetch",
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
/// - The low-peak Subject model's fetch: the download is held in memory whole
///   and written from it (`fetch_model`): its 470 MB plus half → 700 MB.
pub fn estimated_peak(job: AheadJob, on_cpu: bool, loaded: bool) -> u64 {
    let (model, run) = match (job, on_cpu) {
        (AheadJob::Subject, true) => (1000, 3700),
        (AheadJob::Subject, false) => (500, 4200),
        (AheadJob::Sky, _) => (400, 800),
        (AheadJob::ObjectEncoding, _) => (400, 600),
        (AheadJob::ModelFetch, _) => (0, 700),
    };
    if loaded { run * MB } else { (model + run) * MB }
}

/// How long a job made ahead waits for its start at most.
pub const ADMIT_WAIT: Duration = Duration::from_secs(60);
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

/// A job made ahead runs while this lives (`admit`): its reservation in
/// the memory budget.
pub type AheadRun = rawtonic_runtime::memory_budget::Reservation;

/// Why a job made ahead was not started.
pub(crate) use crate::memory_broker::NotStarted;

/// The memory budget's request for `job` of estimated peak `need`: one job
/// at a time beside each other (the fetch beside them), leaving the click
/// reserve unless it is Subject's own work (`reserve`).
pub fn request(job: AheadJob, need: u64, reserve: u64) -> Request {
    let request = Request::new(Class::Speculative, job.label(), need);
    let request = match job {
        AheadJob::ModelFetch => request,
        _ => request.exclusive(),
    };
    match job {
        AheadJob::Subject => request,
        _ => request.reserve(reserve),
    }
}

/// Waits until `job` (estimated peak `need`) may start (`request`): `Ok`
/// with the reservation to hold while it runs; at once (as a request of the
/// photographer, `Class::Asked`) when `asked` says the photographer asked
/// for it (`asked_since`); checks `give_up` before each attempt; dropped
/// after `ADMIT_WAIT`. The broker logs when it waits and starts, with the
/// memory, so a run's log shows where the memory went.
pub async fn admit(
    app: &tauri::AppHandle,
    job: AheadJob,
    asked: impl Fn() -> bool,
    need: u64,
    give_up: impl FnMut() -> Option<&'static str>,
) -> Result<AheadRun, NotStarted> {
    admit_cancellable(app, job, asked, need, give_up, None).await
}

/// `admit`, with `cancel` stopping the job when a request of the
/// photographer needs its room (`memory_budget`: speculative work yields).
pub async fn admit_cancellable(
    app: &tauri::AppHandle,
    job: AheadJob,
    asked: impl Fn() -> bool,
    need: u64,
    give_up: impl FnMut() -> Option<&'static str>,
    cancel: Option<std::sync::Arc<dyn Fn() + Send + Sync>>,
) -> Result<AheadRun, NotStarted> {
    let request = request(job, need, crate::memory_broker::click_reserve(app));
    crate::memory_broker::reserve(
        request,
        || {
            if asked() {
                Class::Asked
            } else {
                Class::Speculative
            }
        },
        give_up,
        cancel,
        Some(ADMIT_WAIT),
    )
    .await
    .map(|granted| granted.reservation)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rawtonic_runtime::memory_budget::{Decision, GB, Held, Reading, Situation, Wait, decide};

    /// The Lenovo of WMEM21 (15859 MB) with `available` and the app at `app`.
    fn lenovo(available: u64, app: u64) -> Reading {
        Reading {
            total: 15_859 * MB,
            available,
            app: Some(app),
        }
    }

    fn situation(held: &[Held], reading: Reading) -> Situation<'_> {
        Situation {
            held,
            reading,
            visible_busy: false,
            waited: Duration::ZERO,
            asked_waiting: false,
            reclaimable: 0,
        }
    }

    #[test]
    fn a_job_made_ahead_waits_for_the_photo_then_for_memory() {
        let need = estimated_peak(AheadJob::Subject, true, false);
        assert_eq!(need, 4700 * MB);
        let subject = request(AheadJob::Subject, need, 3 * GB);
        // Subject's own job leaves no click reserve: it is the click's work.
        assert_eq!(subject.reserve, 0);
        let none: [Held; 0] = [];
        // The photo's decode or frame under way: held whatever the memory.
        let mut s = situation(&none, lenovo(20 * GB, 3 * GB));
        s.visible_busy = true;
        assert_eq!(decide(&subject, &s), Decision::Wait(Wait::Visible));
        // Settled, too little room (WMEM18: 3.5–4.0 GB free beside the
        // decodes): held, as MEM18 measured on the Lenovo.
        assert!(matches!(
            decide(&subject, &situation(&none, lenovo(5 * GB, 3 * GB))),
            Decision::Wait(Wait::Memory { .. })
        ));
        // Settled with room for its peak.
        assert!(matches!(
            decide(&subject, &situation(&none, lenovo(7 * GB, 2 * GB))),
            Decision::Grant { .. }
        ));
    }

    #[test]
    fn a_background_decode_under_way_counts_its_reservation() {
        let need = estimated_peak(AheadJob::Sky, true, false);
        let sky = request(AheadJob::Sky, need, 0);
        // A neighbour's decode reserved 2.44 GB (61 MP) and took nothing
        // yet: Sky (1.2 GB) waits with 4 GB available, starts with 6 GB.
        let decode = [Held {
            id: 1,
            class: Class::Speculative,
            estimate: 2440 * MB,
            app_at_start: Some(2 * GB),
            exclusive: false,
            cancellable: true,
            cancelled_for: None,
        }];
        assert!(matches!(
            decide(&sky, &situation(&decode, lenovo(4 * GB, 2 * GB))),
            Decision::Wait(Wait::Memory { .. })
        ));
        assert!(matches!(
            decide(&sky, &situation(&decode, lenovo(6 * GB, 2 * GB))),
            Decision::Grant { .. }
        ));
    }

    #[test]
    fn the_model_fetch_waits_for_memory_and_a_mask_run_and_leaves_the_click_reserve() {
        let need = estimated_peak(AheadJob::ModelFetch, true, false);
        assert_eq!(need, 700 * MB);
        let fetch = request(AheadJob::ModelFetch, need, 2 * GB);
        assert!(!fetch.exclusive, "it runs beside a job made ahead");
        let none: [Held; 0] = [];
        // WREV1's Lenovo with the model loaded, 4.3 GB available: 2.7 GB
        // room covers the fetch and the 2 GB click reserve; 3.5 GB does not.
        assert!(matches!(
            decide(&fetch, &situation(&none, lenovo(4300 * MB, 3 * GB))),
            Decision::Grant { .. }
        ));
        assert!(matches!(
            decide(&fetch, &situation(&none, lenovo(3500 * MB, 3 * GB))),
            Decision::Wait(Wait::Memory { .. })
        ));
        // A Subject click's run holds it, whatever the memory.
        let click = [Held {
            id: 1,
            class: Class::Asked,
            estimate: 2 * GB,
            app_at_start: Some(3 * GB),
            exclusive: true,
            cancellable: false,
            cancelled_for: None,
        }];
        assert_eq!(
            decide(&fetch, &situation(&click, lenovo(20 * GB, 3 * GB))),
            Decision::Wait(Wait::Asked)
        );
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
        // Never room here (u64::MAX needed); the ask makes it a request of
        // the photographer, which is never refused.
        let asked_at = asked_count();
        let asking = tokio::spawn(async {
            tokio::time::sleep(Duration::from_millis(50)).await;
            ask_now(None);
        });
        let job = request(AheadJob::Sky, u64::MAX, 0);
        let result = tokio::time::timeout(
            Duration::from_secs(10),
            crate::memory_broker::reserve(
                job,
                || {
                    if asked_since(asked_at, None) {
                        Class::Asked
                    } else {
                        Class::Speculative
                    }
                },
                || None,
                None,
                Some(ADMIT_WAIT),
            ),
        )
        .await
        .expect("started before the timeout");
        asking.await.unwrap();
        let granted = result.expect("started");
        assert_eq!(granted.reservation.class(), Class::Asked);
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
        // Checked before each attempt, so before a start too.
        let result = crate::memory_broker::reserve(
            request(AheadJob::Subject, 0, 0),
            || Class::Speculative,
            || Some("the photo has changed"),
            None,
            Some(ADMIT_WAIT),
        )
        .await;
        assert_eq!(
            result.map(|_| ()),
            Err(NotStarted::GaveUp("the photo has changed"))
        );
    }
}
