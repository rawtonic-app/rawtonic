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

/// Share of the physical memory the app's own memory stays within while it
/// does work that may wait or be made again (the multi view's background
/// decodes, held decodes, zoomed regions beyond the first): 6.3 GB of a
/// 14 GB laptop with a Radeon 680M, whose graphics memory counts in the
/// app's private bytes, where the multi view with seven 39–61 MP RAWs at
/// 100 % reached 10.4 GB and blank cells for up to 23 s (G76/WG74).
const WORK_SHARE: f64 = 0.45;

/// Bytes of memory still free for work that may wait (`WORK_SHARE`): what
/// the app's own memory (`app_memory`) may grow by below that share of the
/// physical memory, and at most what the system has available beyond twice
/// `low_threshold`, so that neither the app nor the system is pushed into
/// memory pressure by it. Everything the app holds counts, an export
/// running in it too. 0 when there is no room; None when the memory cannot
/// be read.
pub fn work_headroom() -> Option<u64> {
    static SYSTEM: std::sync::Mutex<Option<sysinfo::System>> = std::sync::Mutex::new(None);
    let mut guard = SYSTEM.lock().unwrap_or_else(|e| e.into_inner());
    let system = guard.get_or_insert_with(sysinfo::System::new);
    system.refresh_memory();
    let total = system.total_memory();
    if total == 0 {
        return None;
    }
    let own = app_memory(system);
    Some(headroom(total, system.available_memory(), own))
}

/// `work_headroom` from the physical memory `total`, the memory the system
/// has `available` and the app's own (`own`, None when unknown).
fn headroom(total: u64, available: u64, own: Option<u64>) -> u64 {
    let system_room = available.saturating_sub(2 * low_threshold(total));
    let own_room = own.map_or(u64::MAX, |own| {
        ((total as f64 * WORK_SHARE) as u64).saturating_sub(own)
    });
    system_room.min(own_room)
}

/// The app's own memory: its physical footprint on macOS (graphics memory
/// included), Private Bytes on Windows (the commit charge; an integrated
/// GPU's shared memory counts there), resident memory on Linux.
fn app_memory(system: &mut sysinfo::System) -> Option<u64> {
    #[cfg(target_os = "macos")]
    {
        let _ = system;
        // SAFETY: `rusage_info_v4` is plain data, filled by the kernel for
        // this process when the flavour matches the struct.
        let mut info: libc::rusage_info_v4 = unsafe { std::mem::zeroed() };
        let result = unsafe {
            libc::proc_pid_rusage(
                std::process::id() as libc::c_int,
                libc::RUSAGE_INFO_V4,
                (&mut info as *mut libc::rusage_info_v4).cast(),
            )
        };
        (result == 0).then_some(info.ri_phys_footprint)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let pid = sysinfo::get_current_pid().ok()?;
        process_memory(system, pid)
    }
}

/// Private memory a helper process of an idle shader compile reaches when
/// it compiles cold, the most measured on a Radeon 680M for the planned
/// variants: 4379 MB (WCC11 r19), 4.44 GB (WCOLD2), 4024–4692 MB
/// (WCOLD2B/C); a helper the pipeline cache answers stays near 0.3 GB. The
/// once-per-computer Glow + Halation helper (5113 MB, WCOLD2B) still leaves
/// more than `low_threshold` free inside the margin of
/// `models_beside_helper` (G76).
pub const COLD_HELPER_PEAK: u64 = 4_500_000_000;

/// Memory now, for whether the loaded AI models may stay beside a helper
/// process's idle compile (`models_beside_helper`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BesideHelper {
    /// Physical memory.
    pub total: u64,
    /// What the system has available.
    pub available: u64,
    /// The app's own memory (`app_memory`, the loaded models included).
    pub own: Option<u64>,
    /// What the helper may still commit: `COLD_HELPER_PEAK` less what it
    /// holds already.
    pub helper_more: u64,
    /// The system's memory pressure.
    pub level: Option<Level>,
}

impl BesideHelper {
    /// Reads the memory now; `helper`: the running helper's process id
    /// (None: the compile has not started, it may still commit all of
    /// `COLD_HELPER_PEAK`). None when the memory cannot be read.
    pub fn now(helper: Option<u32>) -> Option<Self> {
        let level = level_now();
        let mut system = sysinfo::System::new();
        system.refresh_memory();
        let total = system.total_memory();
        if total == 0 {
            return None;
        }
        let own = app_memory(&mut system);
        let held = helper.and_then(|pid| process_memory(&mut system, sysinfo::Pid::from_u32(pid)));
        Some(Self {
            total,
            available: system.available_memory(),
            own,
            helper_more: helper_more(held),
            level,
        })
    }
}

/// What a helper holding `held` (None: not started) may still commit.
fn helper_more(held: Option<u64>) -> u64 {
    COLD_HELPER_PEAK.saturating_sub(held.unwrap_or(0))
}

/// Whether the loaded AI models may stay loaded beside a helper process's
/// idle shader compile: `Ok` when the app, models included, is within its
/// share of the physical memory (`WORK_SHARE`) and the system keeps twice
/// `low_threshold` available after the helper reaches its peak; `Err`
/// (unload) otherwise and under memory pressure. The text gives the
/// numbers either way, for the log line.
///
/// A mask the photographer clicks should not pay a model reload (7–9 s on
/// a Radeon 680M, a Subject mask 21 s instead of 12 s) because the app
/// compiles in the background (coordinator decision AIW3, G76/WAIW1). The
/// models are already counted in the app's and the system's memory; a
/// network does not run beside the helper (`ai_lifecycle::turn`), so
/// keeping them costs only their resident memory (Subject on the CPU about
/// 1 GB). The helper is another process: its peak comes off the system's
/// available memory, not the app's share.
pub fn models_beside_helper(memory: &BesideHelper) -> Result<String, String> {
    const GB: f64 = 1e9;
    let margin = 2 * low_threshold(memory.total);
    let share = (memory.total as f64 * WORK_SHARE) as u64;
    let own = memory.own.map_or_else(
        || "unknown".to_string(),
        |own| format!("{:.1} GB", own as f64 / GB),
    );
    let numbers = format!(
        "{:.1} GB available of {:.1} GB, the helper may commit {:.1} GB more, {:.1} GB kept free; app {own} (share {:.1} GB)",
        memory.available as f64 / GB,
        memory.total as f64 / GB,
        memory.helper_more as f64 / GB,
        margin as f64 / GB,
        share as f64 / GB,
    );
    if let Some(level) = memory.level {
        return Err(format!("memory pressure ({level:?}); {numbers}"));
    }
    if memory.own.is_some_and(|own| own > share) {
        return Err(format!("the app is above its share; {numbers}"));
    }
    if memory.available < memory.helper_more.saturating_add(margin) {
        return Err(format!("too little available beside the helper; {numbers}"));
    }
    Ok(numbers)
}

/// A process's own memory, as `app_memory` counts it (Private Bytes on
/// Windows, resident memory elsewhere).
fn process_memory(system: &mut sysinfo::System, pid: sysinfo::Pid) -> Option<u64> {
    system.refresh_processes_specifics(
        sysinfo::ProcessesToUpdate::Some(&[pid]),
        false,
        sysinfo::ProcessRefreshKind::nothing().with_memory(),
    );
    system.process(pid).map(|process| {
        if cfg!(windows) {
            process.virtual_memory()
        } else {
            process.memory()
        }
    })
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
/// in the private memory. Asked with the compile slot held, right before a
/// compile: masking models unused for a while are unloaded first
/// (`ai_lifecycle::before_idle_compile`), and when the compile may start
/// the caches that rebuild on demand go (`DevelopSession::
/// release_rebuildable`; G76/WCC11). Idle compiles run only under a compile
/// limit (AMD's Windows Vulkan driver), so elsewhere this is never asked.
pub fn guard_idle_compiles(app_handle: &tauri::AppHandle) {
    use tauri::Manager as _;
    let app = app_handle.clone();
    let state = std::sync::Mutex::new((sysinfo::System::new(), None::<std::time::Instant>));
    rawtonic_gpu::gpu_processing::set_idle_compile_memory_guard(Box::new(move || {
        let exports = app.state::<crate::state::export::ExportState>();
        if exports.running() {
            return Err("an export runs".into());
        }
        let since_export =
            crate::state::lock::lock_recover(&exports.last_run_end).map(|ended| ended.elapsed());
        // Only where an idle compile cannot be stopped: one in a helper
        // process ends the moment an export needs the slot. Held there, the
        // variant a 16-bit export renders with could not compile between a
        // JPEG export and the TIFF export after it, which then compiled it
        // itself (35.9 s, 7.2 GB on a Radeon 680M; G76/WSMOKE19).
        if !rawtonic_gpu::gpu_processing::idle_compiles_in_helpers() {
            export_hold(since_export)?;
        }
        // Masking models nobody uses now go before the compile, not beside it.
        crate::ai_lifecycle::before_idle_compile(&app);
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
        let verdict = idle_compile_verdict(
            private,
            system.total_memory(),
            level,
            last_pressure.map(|at| now.duration_since(at)),
        );
        if verdict.is_ok() {
            // The compile may start: what rebuilds on demand goes first, and
            // the pages the allocator keeps committed per thread after
            // frees (mimalloc) go back to the system on every worker thread
            // (without waiting for busy ones) and this one.
            release_rebuildable_memory(&app);
        }
        verdict
    }));
    // The same before the compile an export of the shown photo waits for
    // (`export_of_shown_photo_starts`), which the guard above is not asked
    // for: the app's memory beside that helper's (G76/WCOLD2: 2.45 GB
    // beside 4.44 GB on a Radeon 680M).
    let app = app_handle.clone();
    rawtonic_gpu::gpu_processing::set_before_export_compile(Box::new(move || {
        crate::ai_lifecycle::before_export_compile(&app);
        release_rebuildable_memory(&app);
    }));
}

/// Drops the caches that rebuild on demand (`DevelopSession::
/// release_rebuildable`) and returns the pages the allocator keeps committed
/// per thread after frees (mimalloc) on every worker thread (without waiting
/// for busy ones) and this one.
pub fn release_rebuildable_memory(app: &tauri::AppHandle) {
    release_rebuildable_memory_keeping(app, false);
}

/// `release_rebuildable_memory`; `keep_warp` keeps the warped mask image
/// (`DevelopSession::release_rebuildable`).
pub fn release_rebuildable_memory_keeping(app: &tauri::AppHandle, keep_warp: bool) {
    use tauri::Manager as _;
    app.state::<crate::state::develop::DevelopSession>()
        .release_rebuildable(keep_warp);
    rayon::spawn_broadcast(|_| rawtonic_runtime::allocator::release_freed_memory());
    rawtonic_runtime::allocator::release_freed_memory();
}

/// How long after an export run idle compiles that run in the app (no
/// helper process) stay held: on a Radeon 680M
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

    /// The Radeon 680M laptop (14 GB) of G76/WAIW1 Run A: app 1.9 GB + the
    /// Subject model on the CPU 1.0 GB, a cold helper up to 4.5 GB. The
    /// models stay when the system keeps 2 GiB free beside the helper's
    /// peak, and go when it would not, the app is above its share or the
    /// system is short of memory.
    #[test]
    fn loaded_models_stay_beside_a_helper_when_the_memory_covers_it() {
        let total = 14 * GB;
        let app = Some(29 * GB / 10);
        let memory = |available, held, level| BesideHelper {
            total,
            available,
            own: app,
            helper_more: helper_more(held),
            level,
        };
        // Before the compile (the helper may commit all 4.5 GB): 8 GiB
        // available covers it with 2 GiB to spare, 6 GiB does not.
        let kept = models_beside_helper(&memory(8 * GB, None, None));
        assert!(kept.unwrap().contains("4.5 GB more"));
        let short = models_beside_helper(&memory(6 * GB, None, None));
        assert!(short.unwrap_err().starts_with("too little available"));
        // A helper holding 1.1 GB already may commit 3.4 GB more: 6 GiB
        // available covers that; 5 GiB does not.
        let held = Some(1_100_000_000);
        assert!(models_beside_helper(&memory(6 * GB, held, None)).is_ok());
        assert!(models_beside_helper(&memory(5 * GB, held, None)).is_err());
        // At or past its peak it commits nothing more.
        assert_eq!(helper_more(Some(COLD_HELPER_PEAK + 1)), 0);
        // Memory pressure unloads however much is available.
        for level in [Level::Warn, Level::Critical] {
            let pressure = models_beside_helper(&memory(10 * GB, None, Some(level)));
            assert!(pressure.unwrap_err().starts_with("memory pressure"));
        }
        // The app above its share (45 % of 14 GiB = 6.3 GiB) unloads.
        let large = BesideHelper {
            own: Some(65 * GB / 10),
            ..memory(10 * GB, None, None)
        };
        assert!(
            models_beside_helper(&large)
                .unwrap_err()
                .starts_with("the app is above")
        );
        // An unknown app memory leaves the system's check.
        let unknown = BesideHelper {
            own: None,
            ..memory(8 * GB, None, None)
        };
        assert!(models_beside_helper(&unknown).is_ok());
    }

    #[test]
    fn work_stays_within_the_apps_share_and_the_memory_available() {
        let total = 14 * GB;
        // 45 % of 14 GB = 6.3 GB for the app: 2.3 GB beside its 4 GB.
        assert_eq!(headroom(total, 8 * GB, Some(4 * GB)), 63 * GB / 10 - 4 * GB);
        // The system has less to spare (twice the 1 GiB threshold kept).
        assert_eq!(
            headroom(total, 3 * GB, Some(2 * GB)),
            3 * GB - 2 * (1 << 30)
        );
        // Above its share (an export running in it): none.
        assert_eq!(headroom(total, 6 * GB, Some(7 * GB)), 0);
        // The app's memory not readable: what the system has to spare.
        assert_eq!(headroom(total, 6 * GB, None), 6 * GB - 2 * (1 << 30));
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
