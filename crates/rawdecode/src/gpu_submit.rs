//! Render submissions to the GPU queue go through [`submit`]: it logs a
//! submission whose GPU work took longer than [`SLOW_SUBMISSION`], with its
//! pass and size, so a run names the passes that come near the watchdog.
//!
//! Why (DVL1/DVL2, G74/G62): Windows takes a GPU away when one submission
//! runs longer than its watchdog (`TdrDelay`, 2 s): on a Radeon 680M one
//! submission of 1.67–1.86 s was kept and one of 2.15–2.41 s lost the
//! device, however its work was split into workgroups or dispatches inside
//! it. Integrated GPUs render in short submissions
//! (`rawtonic_gpu::gpu_processing::set_short_submissions`); this log shows
//! which passes still run long.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Submissions whose GPU work takes longer than this are logged: a quarter
/// of Windows' 2 s watchdog.
pub const SLOW_SUBMISSION: Duration = Duration::from_millis(500);

/// Process start, for the times below (nanoseconds since it).
fn epoch() -> Instant {
    static EPOCH: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    *EPOCH.get_or_init(Instant::now)
}

/// When the newest submission seen done finished (ns since [`epoch`]): a
/// submission's own GPU time starts when the one before it finished, not
/// when it was queued behind it.
static LAST_DONE: AtomicU64 = AtomicU64::new(0);

fn nanos(at: Instant) -> u64 {
    at.saturating_duration_since(epoch()).as_nanos() as u64
}

/// The GPU time a submission queued at `submitted` and seen done at `done`
/// took, when the queue's work before it was done at `previous_done` (all
/// ns): from whichever came later.
fn own_time(submitted: u64, previous_done: u64, done: u64) -> Duration {
    Duration::from_nanos(done.saturating_sub(submitted.max(previous_done)))
}

/// Submits `buffers` to `queue` like `wgpu::Queue::submit`, and logs the
/// submission when its GPU work takes longer than [`SLOW_SUBMISSION`], as
/// "`label`, `pixels` px". The time is from submitting (or the end of the
/// work queued before it) until wgpu sees it done, so a submission done
/// while nobody polls the device can look longer than it was, never
/// shorter.
pub fn submit<I>(
    queue: &wgpu::Queue,
    label: &'static str,
    pixels: u64,
    buffers: I,
) -> wgpu::SubmissionIndex
where
    I: IntoIterator<Item = wgpu::CommandBuffer>,
{
    let submitted = nanos(Instant::now());
    let index = queue.submit(buffers);
    queue.on_submitted_work_done(move || {
        let done = nanos(Instant::now());
        let previous = LAST_DONE.fetch_max(done, Ordering::AcqRel);
        let took = own_time(submitted, previous, done);
        if took > SLOW_SUBMISSION {
            log::warn!(
                "Slow GPU submission: {label}, {pixels} px, {:.0} ms (watchdog budget {} ms)",
                took.as_secs_f64() * 1000.0,
                SLOW_SUBMISSION.as_millis()
            );
        }
    });
    index
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_submission_queued_behind_others_counts_from_their_end() {
        let ms = |v: u64| v * 1_000_000;
        // Alone: from its submission.
        assert_eq!(own_time(ms(10), ms(5), ms(30)), Duration::from_millis(20));
        // Queued behind work done at 25: only the last 5 ms are its own.
        assert_eq!(own_time(ms(10), ms(25), ms(30)), Duration::from_millis(5));
        // Seen done in another order: never negative.
        assert_eq!(own_time(ms(10), ms(40), ms(30)), Duration::ZERO);
    }
}
