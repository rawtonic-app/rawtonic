// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tells the app when the system runs short of memory (`ai_lifecycle` then
//! drops the AI models), and holds the GPU's idle compiles while memory is
//! short (`guard_idle_compiles`).
//!
//! macOS: the kernel's memory pressure notifications (a libdispatch
//! memory-pressure source, the mechanism behind `NSProcessInfo`'s and
//! UIKit's low-memory warnings), at the warning and critical levels.
//! Windows and Linux, and macOS if the source cannot be made: the available
//! physical memory, read every few seconds (`sysinfo`: `MemAvailable` on
//! Linux, `ullAvailPhys` on Windows), below `low_threshold`.

use std::sync::Arc;
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Warn,
    Critical,
}

pub type Handler = Arc<dyn Fn(Level) + Send + Sync>;

/// How often the available memory is read where there are no notifications.
const POLL_EVERY: Duration = Duration::from_secs(5);

/// Calls `on_pressure` (on a background thread) whenever the system enters
/// memory pressure. For the life of the app.
pub fn watch(on_pressure: Handler) {
    #[cfg(target_os = "macos")]
    if dispatch::watch(on_pressure.clone()) {
        return;
    }
    poll(on_pressure);
}

/// Whether the system is short of memory now: macOS's pressure level
/// (`kern.memorystatus_vm_pressure_level`, warning or critical), else the
/// available memory below `low_threshold`. For work that may wait (a model
/// warm-up), not instead of `watch`.
pub fn under_pressure() -> bool {
    #[cfg(target_os = "macos")]
    if let Some(level) = dispatch::current_level() {
        return level.is_some();
    }
    let mut system = sysinfo::System::new();
    system.refresh_memory();
    let total = system.total_memory();
    total > 0 && system.available_memory() < low_threshold(total)
}

/// How short of memory the system is now (None: not): macOS's pressure
/// level, else the available memory below `low_threshold` (Warn) or half
/// of it (Critical). For background work that should yield only when memory
/// is really scarce: a Mac with several large apps open sits at the warning
/// level for long stretches.
pub fn level_now() -> Option<Level> {
    #[cfg(target_os = "macos")]
    if let Some(level) = dispatch::current_level() {
        return level;
    }
    let mut system = sysinfo::System::new();
    system.refresh_memory();
    let total = system.total_memory();
    let available = system.available_memory();
    let threshold = low_threshold(total);
    if total == 0 || available >= threshold {
        None
    } else if available < threshold / 2 {
        Some(Level::Critical)
    } else {
        Some(Level::Warn)
    }
}

/// Share of the physical memory above which the app's private memory holds
/// the GPU's idle compiles: one display variant's compile committed up to
/// 5–6 GB on AMD's Windows Vulkan driver, and with the AI models loaded a
/// 15 GB laptop reached 13.8 GB and repeated memory pressure (G76/WGT14).
const IDLE_COMPILE_PRIVATE_SHARE: f64 = 0.6;
/// How long after the last memory pressure seen idle compiles stay held.
const IDLE_COMPILE_PRESSURE_HOLD: Duration = Duration::from_secs(60);

/// Holds the GPU's idle compiles (`rawtonic_gpu::gpu_processing::
/// set_idle_compile_memory_guard`) while the app's private memory (Private
/// Bytes on Windows, resident memory elsewhere) is above
/// `IDLE_COMPILE_PRIVATE_SHARE` of the physical memory, while the system is
/// short of memory (`level_now`) and for `IDLE_COMPILE_PRESSURE_HOLD`
/// after; and while an export runs or the export queue works through its
/// jobs: a compile cannot be stopped midway, so one started in idle time
/// made an export wait for the compile slot (G76/WQI: 20 s on a Radeon
/// 680M). The AI models are loaded in this process, so their memory counts
/// in the private memory. Idle compiles run only under a compile limit
/// (AMD's Windows Vulkan driver), so elsewhere this is never asked.
pub fn guard_idle_compiles(app: &tauri::AppHandle) {
    use tauri::Manager as _;
    let app = app.clone();
    let state = std::sync::Mutex::new((sysinfo::System::new(), None::<std::time::Instant>));
    rawtonic_gpu::gpu_processing::set_idle_compile_memory_guard(Box::new(move || {
        let exports = app.state::<crate::state::export::ExportState>();
        if exports
            .runner_active
            .load(std::sync::atomic::Ordering::SeqCst)
            || exports
                .task_token
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_some()
        {
            return Err("an export runs".into());
        }
        let since_export =
            crate::state::lock::lock_recover(&exports.last_run_end).map(|ended| ended.elapsed());
        export_hold(since_export)?;
        let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
        let (system, last_pressure) = &mut *state;
        let level = level_now();
        let now = std::time::Instant::now();
        if level.is_some() {
            *last_pressure = Some(now);
        }
        system.refresh_memory();
        let private = sysinfo::get_current_pid().ok().and_then(|pid| {
            system.refresh_processes_specifics(
                sysinfo::ProcessesToUpdate::Some(&[pid]),
                false,
                sysinfo::ProcessRefreshKind::nothing().with_memory(),
            );
            system.process(pid).map(|process| {
                if cfg!(windows) {
                    // PrivateUsage: the commit charge (Private Bytes).
                    process.virtual_memory()
                } else {
                    process.memory()
                }
            })
        });
        idle_compile_verdict(
            private,
            system.total_memory(),
            level,
            last_pressure.map(|at| now.duration_since(at)),
        )
    }));
}

/// How long after an export run idle compiles stay held: on a Radeon 680M
/// an idle compile (here the brush layer's layer-sum variant, 41.6 s at
/// 5.7 GB) started 3 s after an export, and the next export waited 35 s
/// for it at 6.24 GB (G76/WGT23). A photographer exporting again soon
/// after must not meet one.
const IDLE_COMPILE_EXPORT_HOLD: Duration = Duration::from_secs(60);

/// `Err` while the last export run ended less than
/// `IDLE_COMPILE_EXPORT_HOLD` ago.
fn export_hold(since_export: Option<Duration>) -> Result<(), String> {
    match since_export {
        Some(since) if since < IDLE_COMPILE_EXPORT_HOLD => {
            Err(format!("an export ended {:.0} s ago", since.as_secs_f64()))
        }
        _ => Ok(()),
    }
}

/// Whether an idle compile may start: `Err` names why not.
fn idle_compile_verdict(
    private: Option<u64>,
    total: u64,
    level: Option<Level>,
    since_pressure: Option<Duration>,
) -> Result<(), String> {
    const GB: f64 = 1e9;
    if let Some(level) = level {
        return Err(format!("memory pressure ({level:?})"));
    }
    if let Some(since) = since_pressure.filter(|since| *since < IDLE_COMPILE_PRESSURE_HOLD) {
        return Err(format!("memory pressure {:.0} s ago", since.as_secs_f64()));
    }
    if let Some(private) = private
        && total > 0
        && private as f64 > IDLE_COMPILE_PRIVATE_SHARE * total as f64
    {
        return Err(format!(
            "private memory {:.1} GB above {:.0} % of {:.1} GB",
            private as f64 / GB,
            IDLE_COMPILE_PRIVATE_SHARE * 100.0,
            total as f64 / GB
        ));
    }
    Ok(())
}

/// Available memory below which the system counts as short of memory: a
/// tenth of the physical memory, at most 1 GiB.
fn low_threshold(total: u64) -> u64 {
    (total / 10).min(1 << 30)
}

/// Pressure from readings of the available memory: reported once on
/// entering the low range, again only after it was left (twice the
/// threshold available).
#[derive(Default)]
struct LowMemory {
    low: bool,
}

impl LowMemory {
    fn update(&mut self, available: u64, total: u64) -> Option<Level> {
        let threshold = low_threshold(total);
        if total == 0 {
            return None;
        }
        if self.low {
            if available > threshold * 2 {
                self.low = false;
            }
            return None;
        }
        if available >= threshold {
            return None;
        }
        self.low = true;
        Some(if available < threshold / 2 {
            Level::Critical
        } else {
            Level::Warn
        })
    }
}

fn poll(on_pressure: Handler) {
    let spawned = std::thread::Builder::new()
        .name("memory-pressure".into())
        .spawn(move || {
            let mut system = sysinfo::System::new();
            let mut low = LowMemory::default();
            loop {
                system.refresh_memory();
                if let Some(level) = low.update(system.available_memory(), system.total_memory()) {
                    on_pressure(level);
                }
                std::thread::sleep(POLL_EVERY);
            }
        });
    if let Err(e) = spawned {
        log::warn!("Memory pressure: not watched ({e})");
    }
}

#[cfg(target_os = "macos")]
mod dispatch {
    use super::{Handler, Level};
    use std::ffi::c_void;

    #[repr(C)]
    struct SourceType {
        _private: [u8; 0],
    }

    // libdispatch (part of libSystem).
    unsafe extern "C" {
        static _dispatch_source_type_memorypressure: SourceType;
        fn dispatch_get_global_queue(identifier: isize, flags: usize) -> *mut c_void;
        fn dispatch_source_create(
            kind: *const SourceType,
            handle: usize,
            mask: usize,
            queue: *mut c_void,
        ) -> *mut c_void;
        fn dispatch_set_context(object: *mut c_void, context: *mut c_void);
        fn dispatch_source_set_event_handler_f(
            source: *mut c_void,
            handler: extern "C" fn(*mut c_void),
        );
        fn dispatch_source_get_data(source: *mut c_void) -> usize;
        fn dispatch_resume(object: *mut c_void);
    }

    const MEMORYPRESSURE_WARN: usize = 0x2;
    const MEMORYPRESSURE_CRITICAL: usize = 0x4;
    const QOS_CLASS_UTILITY: isize = 0x11;

    /// The level a source event reports (`None`: back to normal).
    pub(super) fn level(data: usize) -> Option<Level> {
        if data & MEMORYPRESSURE_CRITICAL != 0 {
            Some(Level::Critical)
        } else if data & MEMORYPRESSURE_WARN != 0 {
            Some(Level::Warn)
        } else {
            None
        }
    }

    /// The kernel's pressure level now (`None` inside: normal), or `None`
    /// when it cannot be read.
    pub(super) fn current_level() -> Option<Option<Level>> {
        let mut value: libc::c_int = 0;
        let mut size = std::mem::size_of::<libc::c_int>();
        // SAFETY: reads one int into `value`, whose size is passed.
        let result = unsafe {
            libc::sysctlbyname(
                c"kern.memorystatus_vm_pressure_level".as_ptr(),
                (&mut value as *mut libc::c_int).cast(),
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        // The level is one of the source's flags (1 normal, 2 warn, 4 critical).
        (result == 0).then(|| level(value as usize))
    }

    struct Context {
        source: *mut c_void,
        on_pressure: Handler,
    }

    extern "C" fn event(context: *mut c_void) {
        // SAFETY: the context is the leaked `Context` set in `watch`, alive
        // for the life of the process, and only read here.
        let context = unsafe { &*(context as *const Context) };
        let data = unsafe { dispatch_source_get_data(context.source) };
        if let Some(level) = level(data) {
            (context.on_pressure)(level);
        }
    }

    /// Registers the source; `false` if it could not be made.
    pub(super) fn watch(on_pressure: Handler) -> bool {
        // SAFETY: plain libdispatch calls; the source and its context are
        // kept for the life of the process (never cancelled or released).
        unsafe {
            let queue = dispatch_get_global_queue(QOS_CLASS_UTILITY, 0);
            let source = dispatch_source_create(
                &_dispatch_source_type_memorypressure,
                0,
                MEMORYPRESSURE_WARN | MEMORYPRESSURE_CRITICAL,
                queue,
            );
            if source.is_null() {
                return false;
            }
            let context = Box::into_raw(Box::new(Context {
                source,
                on_pressure,
            }));
            dispatch_set_context(source, context.cast());
            dispatch_source_set_event_handler_f(source, event);
            dispatch_resume(source);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GB: u64 = 1 << 30;

    /// Idle compiles wait a minute after an export run, so the next export
    /// does not wait for one (G76/WGT23).
    #[test]
    fn idle_compiles_stay_held_a_minute_after_an_export() {
        assert!(export_hold(None).is_ok());
        assert!(export_hold(Some(Duration::from_secs(3))).is_err());
        assert!(export_hold(Some(Duration::from_secs(59))).is_err());
        assert!(export_hold(Some(IDLE_COMPILE_EXPORT_HOLD)).is_ok());
    }

    #[test]
    fn low_available_memory_is_reported_once_until_it_recovers() {
        let mut low = LowMemory::default();
        let total = 16 * GB;
        assert_eq!(low.update(6 * GB, total), None);
        assert_eq!(low.update(GB - 1, total), Some(Level::Warn));
        // Still low, or barely better: not reported again.
        assert_eq!(low.update(GB / 4, total), None);
        assert_eq!(low.update(GB + GB / 2, total), None);
        // Recovered, then low again.
        assert_eq!(low.update(3 * GB, total), None);
        assert_eq!(low.update(GB / 4, total), Some(Level::Critical));
    }

    #[test]
    fn idle_compiles_wait_for_private_memory_and_pressure() {
        let total = 15 * GB;
        let ok = idle_compile_verdict(Some(8 * GB), total, None, None);
        assert_eq!(ok, Ok(()));
        // Above 60 % of the physical memory.
        let high = idle_compile_verdict(Some(10 * GB), total, None, None);
        assert!(high.unwrap_err().starts_with("private memory"));
        // Under pressure, and for a minute after it.
        assert!(idle_compile_verdict(Some(GB), total, Some(Level::Warn), None).is_err());
        let after = Some(Duration::from_secs(20));
        assert!(idle_compile_verdict(Some(GB), total, None, after).is_err());
        let later = Some(IDLE_COMPILE_PRESSURE_HOLD);
        assert_eq!(idle_compile_verdict(Some(GB), total, None, later), Ok(()));
        // Unknown readings do not hold.
        assert_eq!(idle_compile_verdict(None, 0, None, None), Ok(()));
    }

    #[test]
    fn small_machines_count_a_tenth_of_their_memory() {
        assert_eq!(low_threshold(64 * GB), GB);
        assert_eq!(low_threshold(4 * GB), 4 * GB / 10);
        let mut low = LowMemory::default();
        assert_eq!(low.update(500 << 20, 4 * GB), None);
        assert_eq!(low.update(300 << 20, 4 * GB), Some(Level::Warn));
        // An unknown total never counts as pressure.
        assert_eq!(LowMemory::default().update(0, 0), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn source_events_map_to_levels() {
        assert_eq!(dispatch::level(0x1), None);
        assert_eq!(dispatch::level(0x2), Some(Level::Warn));
        assert_eq!(dispatch::level(0x4), Some(Level::Critical));
        assert_eq!(dispatch::level(0x6), Some(Level::Critical));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_pressure_level_can_be_read() {
        assert!(dispatch::current_level().is_some());
        assert_eq!(
            under_pressure(),
            dispatch::current_level().unwrap().is_some()
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_memory_pressure_source_can_be_made() {
        assert!(dispatch::watch(Arc::new(|_| {})));
    }
}
