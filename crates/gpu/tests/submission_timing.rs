//! DVL2/WDVL1: render submissions are timed on the GPU with timestamps
//! where the device has them (`gpu_submit::time_on_gpu`), so the watchdog's
//! log names a submission's own GPU time, not its time in the queue.

use std::time::{Duration, Instant};

use rawtonic_rawdecode::gpu_submit;

#[test]
#[ignore = "requires a native GPU"]
fn a_submission_is_timed_on_the_gpu() {
    let context = rawtonic_gpu::gpu_processing::GpuContext::headless().expect("a GPU");
    let device = &context.device;
    if !device.features().contains(gpu_submit::TIMING_FEATURES) {
        eprintln!("SKIP: no timestamp queries on this device");
        return;
    }
    gpu_submit::time_on_gpu(device);
    let size = 256u64 << 20;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("timing test"),
        size,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    for _ in 0..8 {
        encoder.clear_buffer(&buffer, 0, None);
    }
    let queued = Instant::now();
    gpu_submit::submit(
        &context.device,
        &context.queue,
        "timing test",
        0,
        Some(encoder.finish()),
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while gpu_submit::last_gpu_time() == Duration::ZERO {
        assert!(Instant::now() < deadline, "no GPU time came back");
        device.poll(wgpu::PollType::Poll).unwrap();
        std::thread::sleep(Duration::from_millis(1));
    }
    let gpu = gpu_submit::last_gpu_time();
    let wall = queued.elapsed();
    eprintln!("2 GB cleared: {gpu:?} on the GPU, {wall:?} until read back");
    assert!(gpu > Duration::from_micros(10), "{gpu:?}");
    assert!(gpu <= wall, "GPU time {gpu:?} above the wall time {wall:?}");
}
