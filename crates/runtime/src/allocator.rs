//! Explicit memory release for the mimalloc global allocator (see `lib.rs`).
//!
//! mimalloc keeps freed pages committed for about a second before purging
//! them. Full-resolution RAW work frees hundreds of megabytes at a time, and
//! the GPU upload and render that follow allocate Metal memory outside the
//! allocator, so they would otherwise stack on top of the retained pages.

#[cfg(not(all(target_os = "windows", target_arch = "aarch64")))]
use libmimalloc_sys::{mi_collect, mi_option_get, mi_option_set, mi_option_t};
use std::sync::{Mutex, PoisonError};

/// `mi_option_purge_delay` in mimalloc v3 (not exported by the bindings).
#[cfg(not(all(target_os = "windows", target_arch = "aarch64")))]
const MI_OPTION_PURGE_DELAY: mi_option_t = 15;

/// Returns pages the allocator has freed to the operating system now. Not for
/// interactive loops: the next large allocations fault in fresh pages.
pub fn release_freed_memory() {
    #[cfg(not(all(target_os = "windows", target_arch = "aarch64")))]
    // SAFETY: `mi_collect` has no preconditions.
    unsafe {
        mi_collect(true)
    };
}

/// Active scopes and the purge delay to restore when the last one ends.
static IMMEDIATE_PURGE: Mutex<(usize, i64)> = Mutex::new((0, 0));

/// While alive, freed pages return to the operating system at once.
///
/// The RAW library's develop step frees and reallocates its intermediate
/// copies within a fraction of a second; with the default delay the retained
/// pages add about 0.3 GB to a 50 MP develop peak, for about 8 ms of page
/// faults. The option is process-wide, so scopes count across threads.
pub struct ImmediatePurge(());

impl ImmediatePurge {
    pub fn begin() -> Self {
        let mut scopes = IMMEDIATE_PURGE
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if scopes.0 == 0 {
            #[cfg(not(all(target_os = "windows", target_arch = "aarch64")))]
            // SAFETY: reading and setting a mimalloc option has no preconditions.
            unsafe {
                scopes.1 = mi_option_get(MI_OPTION_PURGE_DELAY) as i64;
                mi_option_set(MI_OPTION_PURGE_DELAY, 0);
            }
        }
        scopes.0 += 1;
        Self(())
    }
}

impl Drop for ImmediatePurge {
    fn drop(&mut self) {
        let mut scopes = IMMEDIATE_PURGE
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        scopes.0 -= 1;
        if scopes.0 == 0 {
            #[cfg(not(all(target_os = "windows", target_arch = "aarch64")))]
            // SAFETY: setting a mimalloc option has no preconditions.
            unsafe {
                mi_option_set(MI_OPTION_PURGE_DELAY, scopes.1 as _)
            };
        }
    }
}

#[cfg(all(test, not(all(target_os = "windows", target_arch = "aarch64"))))]
mod tests {
    use super::*;

    fn purge_delay() -> i64 {
        unsafe { mi_option_get(MI_OPTION_PURGE_DELAY) as i64 }
    }

    #[test]
    fn nested_scopes_restore_the_purge_delay() {
        // mimalloc v3 defaults to a 1000 ms purge delay; this also confirms
        // that the option index is the purge delay.
        if std::env::var_os("MIMALLOC_PURGE_DELAY").is_none() {
            assert_eq!(purge_delay(), 1000);
        }
        let initial = purge_delay();
        let outer = ImmediatePurge::begin();
        let inner = std::thread::spawn(|| {
            let scope = ImmediatePurge::begin();
            let during = purge_delay();
            drop(scope);
            during
        })
        .join()
        .unwrap();
        assert_eq!(inner, 0);
        assert_eq!(purge_delay(), 0, "the outer scope is still active");
        drop(outer);
        assert_eq!(purge_delay(), initial);
    }
}
