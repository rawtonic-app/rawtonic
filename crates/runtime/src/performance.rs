//! Opt-in native stage timing. Never compiled into normal application builds.
use std::time::Instant;

static APPLICATION_START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();

pub fn mark_application_start() {
    let _ = APPLICATION_START.set(Instant::now());
}

pub fn application_elapsed_ms() -> Option<f64> {
    APPLICATION_START
        .get()
        .map(|start| start.elapsed().as_secs_f64() * 1000.0)
}

/// Wall-clock milliseconds, comparable with the webview's
/// `performance.timeOrigin + performance.now()` to split IPC round trips.
pub fn epoch_ms() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |elapsed| elapsed.as_secs_f64() * 1000.0)
}

pub struct PerformanceStage {
    stage: &'static str,
    started: Instant,
}

impl PerformanceStage {
    pub fn new(stage: &'static str) -> Self {
        Self {
            stage,
            started: Instant::now(),
        }
    }
}

/// Current and lifetime-peak physical footprint in bytes (the figure Activity
/// Monitor and `footprint` report). A stage that raises the peak shows it.
#[cfg(target_os = "macos")]
pub fn footprint_bytes() -> Option<(u64, u64)> {
    // SAFETY: `rusage_info_v4` is plain data, and the kernel fills it for
    // this process when the flavour matches the struct.
    let mut info: libc::rusage_info_v4 = unsafe { std::mem::zeroed() };
    let result = unsafe {
        libc::proc_pid_rusage(
            std::process::id() as libc::c_int,
            libc::RUSAGE_INFO_V4,
            (&mut info as *mut libc::rusage_info_v4).cast(),
        )
    };
    (result == 0).then_some((info.ri_phys_footprint, info.ri_lifetime_max_phys_footprint))
}

#[cfg(not(target_os = "macos"))]
pub fn footprint_bytes() -> Option<(u64, u64)> {
    None
}

/// A `[PERF_STAGE]` record with counters beside the time (for example dabs
/// and rows of a brush stroke), stamped with `epoch_ms` at its end so it can
/// be joined with the webview's `pointer_session`. No footprint: these run
/// per pointer move, and `proc_pid_rusage` would add to what they time.
pub fn record_stage(stage: &'static str, started: Instant, fields: serde_json::Value) {
    let mut record = serde_json::json!({
        "stage": stage,
        "ms": started.elapsed().as_secs_f64() * 1000.0,
        "epoch_ms": epoch_ms(),
    });
    if let (Some(record), serde_json::Value::Object(fields)) = (record.as_object_mut(), fields) {
        record.extend(fields);
    }
    emit(&record);
}

/// To the log when a logger is installed (the app's writes it to stderr as
/// well, on a thread of its own), else to stderr (CLI, tests). Writing to
/// stderr here too blocked the preview worker for ~1 s while the Linux PC's
/// btrfs flushed, with stderr redirected to a file (notes:
/// parity-gaps/G55-performance.md).
fn emit(record: &serde_json::Value) {
    if log::max_level() >= log::LevelFilter::Info {
        log::info!("[PERF_STAGE] {record}");
    } else {
        eprintln!("[PERF_STAGE] {record}");
    }
}

impl Drop for PerformanceStage {
    fn drop(&mut self) {
        let footprint = footprint_bytes();
        let record = serde_json::json!({
            "stage": self.stage,
            "ms": self.started.elapsed().as_secs_f64() * 1000.0,
            "footprint_mb": footprint.map(|(current, _)| current as f64 / 1e6),
            "peak_mb": footprint.map(|(_, peak)| peak as f64 / 1e6),
        });
        emit(&record);
    }
}
