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
//! What it measures: on a device that can time its work
//! ([`time_on_gpu`]: timestamp queries, which the app's processing device
//! asks for where the adapter has them), the submission's own GPU time,
//! from a timestamp written before its first command buffer to one after
//! its last (WDVL1: the queuing time below named a display draw at 2.6 s,
//! a single full-window quad, queued behind other work or seen late).
//! Elsewhere the time from queuing it until wgpu reports it done (a
//! completion callback, seen when the device is next polled): that
//! includes the work queued before it and the wait for the poll, never
//! shorter than its own GPU time and often much longer.

use std::time::{Duration, Instant};

/// Submissions that take longer than this are logged: a quarter of
/// Windows' 2 s watchdog.
pub const SLOW_SUBMISSION: Duration = Duration::from_millis(500);

/// The newest submission's GPU time measured with timestamps, in ns (tests).
static LAST_GPU_NS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The newest timed submission's own GPU time (tests and tools).
#[doc(hidden)]
pub fn last_gpu_time() -> Duration {
    Duration::from_nanos(LAST_GPU_NS.load(std::sync::atomic::Ordering::Acquire))
}

/// The features a device needs for [`time_on_gpu`].
pub const TIMING_FEATURES: wgpu::Features =
    wgpu::Features::TIMESTAMP_QUERY.union(wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS);

/// Whether submissions are timed on the GPU ([`time_on_gpu`]).
static TIMED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Times submissions on the GPU from now on, on devices with
/// [`TIMING_FEATURES`] (others log the queuing time instead).
pub fn time_on_gpu(device: &wgpu::Device) {
    let on = device.features().contains(TIMING_FEATURES);
    log::info!("GPU submissions timed on the GPU: {on}");
    TIMED.store(on, std::sync::atomic::Ordering::Release);
}

/// QA builds (`performance-profiling`) with `RAWTONIC_QA_GPU_LOG` set: every
/// timed submission's GPU time is logged (`[gpu-time] submission ...`), and
/// the device times its submissions also where it would not
/// (`rawtonic_gpu::device_start`). Measurement only; other builds never.
#[cfg(feature = "performance-profiling")]
pub fn qa_log_requested() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("RAWTONIC_QA_GPU_LOG").is_some())
}

/// Never outside QA builds (see the profiling build's version).
#[cfg(not(feature = "performance-profiling"))]
pub fn qa_log_requested() -> bool {
    false
}

/// Times no submission on the GPU (a device started without short
/// submissions).
pub fn stop_timing() {
    TIMED.store(false, std::sync::atomic::Ordering::Release);
}

/// Submits `buffers` to `queue` (of `device`) like `wgpu::Queue::submit`,
/// and logs the submission as "`label`, `pixels` px" when it took longer
/// than [`SLOW_SUBMISSION`] (see the module docs).
pub fn submit<I>(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    label: &'static str,
    pixels: u64,
    buffers: I,
) -> wgpu::SubmissionIndex
where
    I: IntoIterator<Item = wgpu::CommandBuffer>,
{
    if TIMED.load(std::sync::atomic::Ordering::Acquire)
        && device.features().contains(TIMING_FEATURES)
    {
        let period = queue.get_timestamp_period();
        return submit_timed(device, queue, period, label, pixels, buffers);
    }
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

/// `submit` with a timestamp before the first and after the last of
/// `buffers`, read back once the work is done.
fn submit_timed<I>(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    period: f32,
    label: &'static str,
    pixels: u64,
    buffers: I,
) -> wgpu::SubmissionIndex
where
    I: IntoIterator<Item = wgpu::CommandBuffer>,
{
    let queries = device.create_query_set(&wgpu::QuerySetDescriptor {
        label: Some("Submission timestamps"),
        ty: wgpu::QueryType::Timestamp,
        count: 2,
    });
    let buffer = |label, usage| {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size: 16,
            usage,
            mapped_at_creation: false,
        })
    };
    let resolved = buffer(
        "Submission timestamps resolved",
        wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
    );
    let read = buffer(
        "Submission timestamps read",
        wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
    );
    let mut first = device.create_command_encoder(&Default::default());
    first.write_timestamp(&queries, 0);
    let mut last = device.create_command_encoder(&Default::default());
    last.write_timestamp(&queries, 1);
    last.resolve_query_set(&queries, 0..2, &resolved, 0);
    last.copy_buffer_to_buffer(&resolved, 0, &read, 0, 16);
    let index = queue.submit(
        std::iter::once(first.finish())
            .chain(buffers)
            .chain(std::iter::once(last.finish())),
    );
    let mapped = read.clone();
    read.slice(..)
        .map_async(wgpu::MapMode::Read, move |result| {
            if result.is_err() {
                return;
            }
            let ticks = mapped.slice(..).get_mapped_range().ok().map(|view| {
                let word =
                    |at: usize| u64::from_le_bytes(view[at..at + 8].try_into().unwrap_or_default());
                [word(0), word(8)]
            });
            mapped.unmap();
            let Some(ticks) = ticks else { return };
            let took = Duration::from_nanos(
                (ticks[1].saturating_sub(ticks[0]) as f64 * f64::from(period)) as u64,
            );
            LAST_GPU_NS.store(took.as_nanos() as u64, std::sync::atomic::Ordering::Release);
            if qa_log_requested() {
                log::info!(
                    "[gpu-time] submission {label} {pixels} px {:.3} ms",
                    took.as_secs_f64() * 1000.0
                );
            }
            if let Some(took) = slow(took) {
                log::warn!(
                    "Slow GPU submission: {label}, {pixels} px, {:.0} ms on the GPU (budget {} ms)",
                    took.as_secs_f64() * 1000.0,
                    SLOW_SUBMISSION.as_millis()
                );
            }
        });
    drop(queries);
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
