//! Render submissions to the GPU queue go through [`submit`]: it logs a
//! submission that was not done [`SLOW_SUBMISSION`] after it was queued,
//! with its pass and size, so a run names the passes that may come near
//! the watchdog.
//!
//! Why (DVL1/DVL2, G74/G62): Windows takes a GPU away when one submission
//! runs longer than its watchdog (`TdrDelay`, 2 s): on a Radeon 680M one
//! submission of 1.67–1.86 s was kept and one of 2.15–2.41 s lost the
//! device, however its work was split into workgroups or dispatches inside
//! it. Integrated GPUs render in short submissions
//! (`rawtonic_gpu::gpu_processing::set_short_submissions`); this log points
//! at what may still run long.
//!
//! What it measures: from queuing the submission until wgpu reports it done
//! (a completion callback, seen when the device is next polled). That
//! includes the work queued before it and the wait for the poll, so it is
//! never shorter than the submission's own GPU time, and can be much
//! longer: a logged pass is a candidate, timed on its own to confirm.

use std::time::{Duration, Instant};

/// Submissions not done this long after they were queued are logged: a
/// quarter of Windows' 2 s watchdog.
pub const SLOW_SUBMISSION: Duration = Duration::from_millis(500);

/// Submits `buffers` to `queue` like `wgpu::Queue::submit`, and logs the
/// submission as "`label`, `pixels` px" when it was not done
/// [`SLOW_SUBMISSION`] after it was queued (see the module docs).
pub fn submit<I>(
    queue: &wgpu::Queue,
    label: &'static str,
    pixels: u64,
    buffers: I,
) -> wgpu::SubmissionIndex
where
    I: IntoIterator<Item = wgpu::CommandBuffer>,
{
    let submitted = Instant::now();
    let index = queue.submit(buffers);
    queue.on_submitted_work_done(move || {
        if let Some(took) = slow(submitted.elapsed()) {
            log::warn!(
                "Slow GPU submission: {label}, {pixels} px, done {:.0} ms after queuing (incl. earlier queued work; budget {} ms)",
                took.as_secs_f64() * 1000.0,
                SLOW_SUBMISSION.as_millis()
            );
        }
    });
    index
}

/// `took` when it is over [`SLOW_SUBMISSION`].
fn slow(took: Duration) -> Option<Duration> {
    (took > SLOW_SUBMISSION).then_some(took)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_submissions_over_the_budget_are_logged() {
        assert_eq!(slow(Duration::from_millis(500)), None);
        assert_eq!(
            slow(Duration::from_millis(501)),
            Some(Duration::from_millis(501))
        );
    }
}
