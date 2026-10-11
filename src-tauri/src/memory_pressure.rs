// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tells the app when the system runs short of memory (`ai_lifecycle` then
//! drops the AI models).
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
