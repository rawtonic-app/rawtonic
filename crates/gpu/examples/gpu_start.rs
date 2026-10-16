//! Times the GPU start-up the app goes through before its first Develop frame:
//! device, `GpuProcessor::new` and the display variants compiled behind it.
//! Each shader or pipeline compile over 250 ms is logged with its time.
//!
//! cargo run --release -p rawtonic-gpu --example gpu_start [-- WAIT_SECONDS]
//!
//! For a cold driver cache on NVIDIA/Linux point `__GL_SHADER_DISK_CACHE_PATH`
//! at a fresh empty folder (Mesa: `MESA_SHADER_CACHE_DIR`).

use std::time::{Duration, Instant};

use rawtonic_gpu::gpu_processing::{GpuContext, GpuProcessor};

struct Logger(Instant);

impl log::Log for Logger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= log::Level::Info && metadata.target().starts_with("rawtonic")
    }
    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) {
            eprintln!(
                "{:7.2} s  {}",
                self.0.elapsed().as_secs_f64(),
                record.args()
            );
        }
    }
    fn flush(&self) {}
}

fn main() -> Result<(), String> {
    let start = Instant::now();
    log::set_logger(Box::leak(Box::new(Logger(start)))).map_err(|e| e.to_string())?;
    log::set_max_level(log::LevelFilter::Info);
    let wait: u64 = std::env::args()
        .nth(1)
        .map(|s| s.parse().map_err(|_| format!("not seconds: {s}")))
        .transpose()?
        .unwrap_or(0);

    let context = GpuContext::headless()?;
    eprintln!("{:7.2} s  device ready", start.elapsed().as_secs_f64());
    let max = context.limits.max_texture_dimension_2d;
    let _processor = GpuProcessor::new(context, max, max)?;
    eprintln!("{:7.2} s  processor ready", start.elapsed().as_secs_f64());
    // Display variants compile on their own threads; their log lines arrive
    // while this waits.
    std::thread::sleep(Duration::from_secs(wait));
    Ok(())
}
