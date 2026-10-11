//! Opt-in native stage timing. Never compiled into normal application builds.
//! The stage timers live in `rawtonic-runtime`; the display probe needs the
//! window and stays here.
pub(crate) use rawtonic_runtime::performance::*;

/// Logs the refresh rate of the screen that holds the window whenever it
/// changes (startup, moving to another display). The native display presents
/// with FIFO, so this is the rate a frame can reach the screen at; the
/// webview's own animation frames may be capped lower.
#[cfg(target_os = "macos")]
pub(crate) fn log_display_refresh(ns_window: *mut std::ffi::c_void) {
    use objc::{msg_send, sel, sel_impl};
    static LAST: std::sync::Mutex<Option<(i64, i64)>> = std::sync::Mutex::new(None);
    if ns_window.is_null() {
        return;
    }
    // SAFETY: called on the main thread with the live NSWindow of a Tauri
    // window; NSScreen's properties are plain values.
    let (max_fps, backing_scale) = unsafe {
        let window = ns_window.cast::<objc::runtime::Object>();
        let screen: *mut objc::runtime::Object = msg_send![window, screen];
        if screen.is_null() {
            return;
        }
        let max_fps: isize = msg_send![screen, maximumFramesPerSecond];
        let backing_scale: f64 = msg_send![screen, backingScaleFactor];
        (max_fps as i64, backing_scale)
    };
    let key = (max_fps, (backing_scale * 100.0) as i64);
    let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
    if *last == Some(key) {
        return;
    }
    *last = Some(key);
    log::info!(
        "[PERF] {}",
        serde_json::json!({
            "stage": "display_refresh",
            "max_fps": max_fps,
            "backing_scale": backing_scale,
        })
    );
}
