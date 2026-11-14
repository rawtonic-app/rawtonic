//! The main window's `WM_GETMINMAXINFO` answer on Windows: the maximized
//! window is the monitor's work area, and the tracking minimum (interface
//! size × 1000×600, `useScaledMinimum`) never exceeds it.
//!
//! A minimum larger than the work area cannot be honoured by a maximized
//! window; it is cut to the work area here, so the interface degrades
//! (the top bar compacts) instead of the window leaving the screen.
//!
//! Installed as a window subclass in front of tao's own: tao fills the
//! structure first (`DefSubclassProc`), then [`min_max_info`] adjusts it.
#![cfg_attr(not(windows), allow(dead_code))] // the computation is tested everywhere

/// A Win32 rectangle in physical pixels (left, top, right, bottom).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Bounds {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Bounds {
    fn size(self) -> (i32, i32) {
        (self.right - self.left, self.bottom - self.top)
    }
}

/// The three fields of `MINMAXINFO` this module answers, in physical pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MinMax {
    /// Relative to the monitor's top-left corner, as `ptMaxPosition` is.
    pub max_position: (i32, i32),
    pub max_size: (i32, i32),
    pub min_track: (i32, i32),
}

/// `given` is what tao and Windows filled in. A frameless window (our
/// default chrome) maximizes onto the work area exactly; with the system
/// frame (Preferences) Windows' own overhanging maximum stays. When the
/// work area is the whole monitor (auto-hidden or no taskbar) the system
/// maximum stays too, so an auto-hidden taskbar can still be revealed.
/// In every case the tracking minimum is cut to the work area and to the
/// maximum size, so a maximized window is never bigger than its maximum.
pub(crate) fn min_max_info(
    monitor: Bounds,
    work: Bounds,
    given: MinMax,
    frameless: bool,
) -> MinMax {
    let work_size = work.size();
    let (max_position, max_size) = if frameless && work != monitor {
        (
            (work.left - monitor.left, work.top - monitor.top),
            work_size,
        )
    } else {
        (given.max_position, given.max_size)
    };
    let cap = (work_size.0.min(max_size.0), work_size.1.min(max_size.1));
    MinMax {
        max_position,
        max_size,
        min_track: (
            given.min_track.0.min(cap.0).max(0),
            given.min_track.1.min(cap.1).max(0),
        ),
    }
}

/// Whether `RAWTONIC_WINDOW_TRACE` asks for the window's size and state
/// messages in app.log (the maximize button's clicks, the limits answered
/// here, minimum changes): for finding out on someone's machine why a
/// window does not maximize or lands off the screen.
pub(crate) fn trace() -> bool {
    static TRACE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *TRACE.get_or_init(|| std::env::var_os("RAWTONIC_WINDOW_TRACE").is_some_and(|v| !v.is_empty()))
}

/// Sets the main window's minimum client size (logical pixels) only while it
/// is in its normal state; returns whether it was set. tao's
/// `set_min_inner_size` re-applies the current inner size, which takes a
/// maximized window out of maximize at its maximized size (G76/WQG). The
/// check and the change run together on the UI thread, so a maximize (the
/// start restoring a maximized window, a click) cannot come between them.
#[tauri::command]
pub(crate) fn set_window_minimum(
    window: tauri::WebviewWindow,
    width: f64,
    height: f64,
) -> Result<bool, String> {
    if !(width.is_finite() && height.is_finite() && width > 0.0 && height > 0.0) {
        return Err(format!("Invalid window minimum {width}×{height}"));
    }
    let (sender, receiver) = std::sync::mpsc::channel();
    let target = window.clone();
    window
        .run_on_main_thread(move || {
            let normal = !target.is_maximized().unwrap_or(true)
                && !target.is_minimized().unwrap_or(true)
                && !target.is_fullscreen().unwrap_or(true);
            if trace() {
                log::info!(
                    "window trace: minimum {width}x{height} asked, normal state {normal}, inner {:?}, outer {:?}",
                    target.inner_size().ok(),
                    target.outer_size().ok()
                );
            }
            let result = if normal {
                target
                    .set_min_size(Some(tauri::LogicalSize::new(width, height)))
                    .map(|()| true)
                    .map_err(|error| error.to_string())
            } else {
                Ok(false)
            };
            let _ = sender.send(result);
        })
        .map_err(|error| error.to_string())?;
    receiver.recv().map_err(|error| error.to_string())?
}

/// Installs the subclass on the main window (UI thread, from `setup`).
#[cfg(windows)]
pub(crate) fn install(window: &tauri::WebviewWindow, frameless: bool) {
    match window.hwnd() {
        Ok(hwnd) => {
            // SAFETY: `hwnd` is the live main window, created on this (the UI) thread.
            if let Err(error) = unsafe { win32::install(hwnd.0 as isize, frameless) } {
                log::warn!("Could not install the window size limits: {error}");
            }
        }
        Err(error) => log::warn!("Could not install the window size limits: {error}"),
    }
}

#[cfg(windows)]
mod win32 {
    use super::{Bounds, MinMax, min_max_info};
    use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
    use windows::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
    };
    use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
    use windows::Win32::UI::WindowsAndMessaging::{
        IsZoomed, MINMAXINFO, WINDOWPOS, WM_GETMINMAXINFO, WM_SIZE, WM_SYSCOMMAND,
        WM_WINDOWPOSCHANGED, WM_WINDOWPOSCHANGING,
    };

    const SUBCLASS_ID: usize = 0x5257_4d4d;

    fn bounds(rect: RECT) -> Bounds {
        Bounds {
            left: rect.left,
            top: rect.top,
            right: rect.right,
            bottom: rect.bottom,
        }
    }

    /// # Safety
    /// `hwnd` must be a live window owned by the calling thread.
    pub(super) unsafe fn install(hwnd: isize, frameless: bool) -> windows::core::Result<()> {
        if unsafe {
            SetWindowSubclass(
                HWND(hwnd),
                Some(window_proc),
                SUBCLASS_ID,
                frameless as usize,
            )
        }
        .as_bool()
        {
            Ok(())
        } else {
            Err(windows::core::Error::from_win32())
        }
    }

    unsafe extern "system" fn window_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
        _id: usize,
        frameless: usize,
    ) -> LRESULT {
        let trace = super::trace();
        if trace && msg == WM_WINDOWPOSCHANGING && lparam.0 != 0 {
            // SAFETY: the message's lParam points at its WINDOWPOS.
            let pos = unsafe { &*(lparam.0 as *const WINDOWPOS) };
            log::info!(
                "window trace: WM_WINDOWPOSCHANGING asked {},{} {}x{} flags {:#x}",
                pos.x,
                pos.y,
                pos.cx,
                pos.cy,
                pos.flags.0
            );
        }
        let result = unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) };
        if trace {
            let zoomed = unsafe { IsZoomed(hwnd) }.as_bool();
            match msg {
                WM_SYSCOMMAND => {
                    log::info!(
                        "window trace: WM_SYSCOMMAND {:#x}, zoomed {zoomed}",
                        wparam.0
                    );
                }
                WM_SIZE => log::info!(
                    "window trace: WM_SIZE kind {} client {}x{}, zoomed {zoomed}",
                    wparam.0,
                    lparam.0 & 0xffff,
                    (lparam.0 >> 16) & 0xffff
                ),
                WM_WINDOWPOSCHANGING | WM_WINDOWPOSCHANGED if lparam.0 != 0 => {
                    // SAFETY: the message's lParam points at its WINDOWPOS.
                    let pos = unsafe { &*(lparam.0 as *const WINDOWPOS) };
                    log::info!(
                        "window trace: {} {},{} {}x{} flags {:#x}, zoomed {zoomed}",
                        if msg == WM_WINDOWPOSCHANGING {
                            "WM_WINDOWPOSCHANGING answered"
                        } else {
                            "WM_WINDOWPOSCHANGED"
                        },
                        pos.x,
                        pos.y,
                        pos.cx,
                        pos.cy,
                        pos.flags.0
                    );
                }
                _ => {}
            }
        }
        if msg == WM_GETMINMAXINFO && lparam.0 != 0 {
            // SAFETY: WM_GETMINMAXINFO's lParam points at the caller's MINMAXINFO.
            let info = unsafe { &mut *(lparam.0 as *mut MINMAXINFO) };
            let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
            let mut monitor_info = MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            if unsafe { GetMonitorInfoW(monitor, &mut monitor_info) }.as_bool() {
                let given = MinMax {
                    max_position: (info.ptMaxPosition.x, info.ptMaxPosition.y),
                    max_size: (info.ptMaxSize.x, info.ptMaxSize.y),
                    min_track: (info.ptMinTrackSize.x, info.ptMinTrackSize.y),
                };
                let limits = min_max_info(
                    bounds(monitor_info.rcMonitor),
                    bounds(monitor_info.rcWork),
                    given,
                    frameless != 0,
                );
                if trace {
                    log::info!(
                        "window trace: WM_GETMINMAXINFO given {given:?} max track {},{} answered {limits:?} (frameless {}, monitor {:?}, work {:?})",
                        info.ptMaxTrackSize.x,
                        info.ptMaxTrackSize.y,
                        frameless != 0,
                        bounds(monitor_info.rcMonitor),
                        bounds(monitor_info.rcWork)
                    );
                }
                let point = |(x, y): (i32, i32)| POINT { x, y };
                info.ptMaxPosition = point(limits.max_position);
                info.ptMaxSize = point(limits.max_size);
                info.ptMinTrackSize = point(limits.min_track);
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(left: i32, top: i32, width: i32, height: i32) -> Bounds {
        Bounds {
            left,
            top,
            right: left + width,
            bottom: top + height,
        }
    }

    /// The Lenovo of G76/WQG: 2560×1600 at 200 %, taskbar 96 px (work area
    /// 1280×752 logical). The 125 % minimum is 1250×750 logical, i.e.
    /// 2500×1500 client plus the 26 px frameless shadow frame.
    #[test]
    fn the_125_percent_minimum_on_the_lenovo_stays_inside_the_work_area() {
        let monitor = rect(0, 0, 2560, 1600);
        let work = rect(0, 0, 2560, 1504);
        let system = MinMax {
            max_position: (-16, -16),
            max_size: (2592, 1632),
            min_track: (2526, 1526),
        };
        let limits = min_max_info(monitor, work, system, true);
        assert_eq!(limits.max_position, (0, 0));
        assert_eq!(limits.max_size, (2560, 1504));
        assert_eq!(limits.min_track, (2526, 1504));
        assert!(limits.min_track.0 <= limits.max_size.0 && limits.min_track.1 <= limits.max_size.1);

        // 100 % and 112 % fit unchanged.
        for min_track in [(2026, 1226), (2266, 1370)] {
            let limits = min_max_info(
                monitor,
                work,
                MinMax {
                    min_track,
                    ..system
                },
                true,
            );
            assert_eq!(limits.min_track, min_track);
            assert_eq!(limits.max_size, (2560, 1504));
        }
    }

    #[test]
    fn the_maximum_is_relative_to_a_monitor_left_of_the_primary_with_a_top_taskbar() {
        let limits = min_max_info(
            rect(-1920, 0, 1920, 1080),
            rect(-1920, 40, 1920, 1040),
            MinMax {
                max_position: (-8, -8),
                max_size: (1936, 1096),
                min_track: (1266, 789),
            },
            true,
        );
        assert_eq!(limits.max_position, (0, 40));
        assert_eq!(limits.max_size, (1920, 1040));
        assert_eq!(limits.min_track, (1266, 789));
    }

    #[test]
    fn the_system_frame_and_an_auto_hidden_taskbar_keep_the_system_maximum() {
        let system = MinMax {
            max_position: (-16, -16),
            max_size: (2592, 1632),
            min_track: (2600, 1700),
        };
        let framed = min_max_info(
            rect(0, 0, 2560, 1600),
            rect(0, 0, 2560, 1504),
            system,
            false,
        );
        assert_eq!(
            (framed.max_position, framed.max_size),
            (system.max_position, system.max_size)
        );
        assert_eq!(framed.min_track, (2560, 1504));
        let auto_hidden =
            min_max_info(rect(0, 0, 2560, 1600), rect(0, 0, 2560, 1600), system, true);
        assert_eq!(
            (auto_hidden.max_position, auto_hidden.max_size),
            (system.max_position, system.max_size)
        );
        assert_eq!(auto_hidden.min_track, (2560, 1600));
    }
}
