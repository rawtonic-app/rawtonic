//! The GPU's managed state (plan step 3.3): the processing context and the
//! adapter it was made from, the display and export processors, the
//! scopes' upload cache, the crash flag of a device start, why the GPU did
//! not start, and the window's newest size. See `README.md` here.

use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex, MutexGuard};

use rawtonic_gpu::gpu_processing::{GpuContext, GpuImageCache, GpuProcessorSlot};

use super::lock::lock_recover;

/// The GPU's handles. Locks: `README.md` here.
pub struct GpuState {
    /// The processing context, created on first use
    /// (`gpu_processing::get_or_init_gpu_context`).
    context: Mutex<Option<GpuContext>>,
    /// The instance and adapter `context` was made from: the window's
    /// surface is created on them once the window exists
    /// (`gpu_processing::attach_display`).
    pub(crate) adapter: Mutex<Option<(wgpu::Instance, wgpu::Adapter)>>,
    /// The processor of the preview, thumbnails and the other interactive
    /// renders.
    pub processor: GpuProcessorSlot,
    /// The export's own processor (created in the background, dropped when
    /// the export queue is idle).
    pub export_processor: GpuProcessorSlot,
    /// The flag file written while a device starts (a crash there leaves it
    /// for the next start); set once at start-up.
    pub(crate) crash_flag_path: Mutex<Option<PathBuf>>,
    /// The scopes' GPU upload of their source (`scope_analytics`).
    pub scope_cache: Mutex<Option<GpuImageCache>>,
    /// Why the GPU did not start, for the window (`gpu_start_error`).
    pub(crate) start_error: Mutex<Option<String>>,
    /// Backends left out of device starts after a crash, and what this
    /// start tells about it (`gpu_start_guard`); set once at start-up.
    pub(crate) avoid_backends: Mutex<wgpu::Backends>,
    pub(crate) start_notice: Mutex<Option<crate::gpu_start_guard::Notice>>,
    /// The window's newest size in device pixels (width << 32 | height), for
    /// a resize that waits for the display (`gpu_processing::resize_display`;
    /// an `Arc` so the waiting worker can own it).
    pub(crate) window_size: Arc<AtomicU64>,
}

impl Default for GpuState {
    fn default() -> Self {
        Self {
            context: Mutex::new(None),
            adapter: Mutex::new(None),
            processor: GpuProcessorSlot::new(None),
            export_processor: GpuProcessorSlot::background(None),
            crash_flag_path: Mutex::new(None),
            scope_cache: Mutex::new(None),
            start_error: Mutex::new(None),
            avoid_backends: Mutex::new(wgpu::Backends::empty()),
            start_notice: Mutex::new(None),
            window_size: Arc::new(AtomicU64::new(0)),
        }
    }
}

impl GpuState {
    /// The processing context if one exists (never creates one; that is
    /// `gpu_processing::get_or_init_gpu_context`).
    pub fn context(&self) -> Option<GpuContext> {
        lock_recover(&self.context).clone()
    }

    /// The context slot, for creating or replacing the context under its
    /// lock. A lock poisoned by a panic during a device start is not
    /// recovered with its data but emptied, so the next use starts a new
    /// device (the one exception to `lock_recover` here, kept from before
    /// the split).
    pub(crate) fn context_slot(&self) -> MutexGuard<'_, Option<GpuContext>> {
        match self.context.lock() {
            Ok(guard) => guard,
            Err(poisoned) => {
                log::warn!(
                    "GPU context lock was poisoned (previous crash). Wiping state to self-heal."
                );
                let mut guard = poisoned.into_inner();
                *guard = None;
                guard
            }
        }
    }

    /// The context slot without the reset of [`Self::context_slot`]: for
    /// using the context under its lock (its display) or replacing it
    /// (`gpu_processing::attach_display`).
    pub(crate) fn context_lock(&self) -> MutexGuard<'_, Option<GpuContext>> {
        lock_recover(&self.context)
    }

    pub fn set_crash_flag_path(&self, path: PathBuf) {
        *lock_recover(&self.crash_flag_path) = Some(path);
    }

    pub(crate) fn crash_flag_path(&self) -> Option<PathBuf> {
        lock_recover(&self.crash_flag_path).clone()
    }

    pub(crate) fn avoid_backends(&self) -> wgpu::Backends {
        *lock_recover(&self.avoid_backends)
    }

    pub(crate) fn set_start_guard(
        &self,
        avoid: wgpu::Backends,
        notice: Option<crate::gpu_start_guard::Notice>,
    ) {
        *lock_recover(&self.avoid_backends) = avoid;
        *lock_recover(&self.start_notice) = notice;
    }

    pub(crate) fn start_notice(&self) -> Option<crate::gpu_start_guard::Notice> {
        lock_recover(&self.start_notice).clone()
    }

    pub(crate) fn set_start_error(&self, error: String) {
        *lock_recover(&self.start_error) = Some(error);
    }

    pub fn start_error(&self) -> Option<String> {
        lock_recover(&self.start_error).clone()
    }
}
