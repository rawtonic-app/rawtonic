//! The main window's `WM_GETMINMAXINFO` answer on Windows: the maximized
//! window is the monitor's work area, and the tracking minimum (interface
//! size × 1000×600, `useScaledMinimum`) never exceeds it.
//!
//! A minimum larger than the work area cannot be honoured by a maximized
//! window; it is cut to the work area here, so the interface degrades
//! (the top bar compacts) instead of the window leaving the screen.
//!
//! Measured on Windows 11 (2560×1600 at 200 %, work area 2560×1504,
//! G76/WUX11): the maximized frameless window's rectangle is still
//! -13,-13 2586×1530, the work area plus the invisible resize border on
//! every side, as for any resizable window; its client area (tao's
//! `WM_NCCALCSIZE`) is exactly the work area 0,0 2560×1504 and nothing is
//! cut off. So the maximum answered here does not move the window's outer
//! rectangle there; what a user sees is the work area either way.
//!
//! The same subclass tells Windows where the maximize button drawn in the
//! toolbar is ([`hits_maximize_button`], `WM_GETTITLEBARINFOEX`).
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

    fn contains(self, (x, y): (i32, i32)) -> bool {
        x >= self.left && x < self.right && y >= self.top && y < self.bottom
    }
}

/// The screen point in a `WM_NCHITTEST` lParam: two signed 16-bit halves
/// (a monitor left of or above the primary has negative coordinates).
pub(crate) fn screen_point(lparam: isize) -> (i32, i32) {
    let half = |value: isize| i32::from(value as u16 as i16);
    (half(lparam & 0xffff), half((lparam >> 16) & 0xffff))
}

/// Whether the main window itself answers a hit test at `point` as its
/// maximize button: inside the button the toolbar draws (`button`, screen
/// pixels; `None` while it is not shown). The pointer's own hit tests go to
/// the snap-layout overlay over the button (`snap_layout`); Windows 11's
/// snap-layouts flyout asks the top window where its maximize button is.
/// Without an answer it placed the flyout below a system-height title bar
/// (54 px under the window's top at 200 %), over our taller button from
/// interface size 125 %: the flyout then took the click meant for the
/// button and covered the Preferences button beside it (G76/WUX11).
pub(crate) fn hits_maximize_button(button: Option<Bounds>, point: (i32, i32)) -> bool {
    button.is_some_and(|button| button.contains(point))
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
        HTMAXBUTTON, IsZoomed, MINMAXINFO, TITLEBARINFOEX, WINDOWPOS, WM_ERASEBKGND,
        WM_GETMINMAXINFO, WM_GETTITLEBARINFOEX, WM_MOUSEMOVE, WM_NCHITTEST, WM_NCMOUSEMOVE,
        WM_PAINT, WM_SETCURSOR, WM_SIZE, WM_SYSCOMMAND, WM_TIMER, WM_WINDOWPOSCHANGED,
        WM_WINDOWPOSCHANGING,
    };

    /// `TITLEBARINFOEX::rgrect`/`rgstate` index of the maximize button.
    const MAXIMIZE_BUTTON: usize = 3;

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
        if (msg == WM_NCHITTEST || msg == WM_GETTITLEBARINFOEX)
            && let Some(button) = crate::snap_layout::maximize_button_on_screen()
        {
            if msg == WM_NCHITTEST {
                let point = super::screen_point(lparam.0);
                if super::hits_maximize_button(Some(button), point) {
                    if trace {
                        log::info!(
                            "window trace: top window hit test at {point:?} answered as the maximize button (was {})",
                            result.0
                        );
                    }
                    return LRESULT(HTMAXBUTTON as isize);
                }
            } else if lparam.0 != 0 {
                // SAFETY: WM_GETTITLEBARINFOEX's lParam points at the caller's
                // TITLEBARINFOEX; its size is checked before the write.
                let info = unsafe { &mut *(lparam.0 as *mut TITLEBARINFOEX) };
                if info.cbSize as usize >= std::mem::size_of::<TITLEBARINFOEX>() {
                    info.rgrect[MAXIMIZE_BUTTON] = RECT {
                        left: button.left,
                        top: button.top,
                        right: button.right,
                        bottom: button.bottom,
                    };
                    // Shown and enabled (no STATE_SYSTEM_* flag).
                    info.rgstate[MAXIMIZE_BUTTON] = 0;
                    if trace {
                        log::info!(
                            "window trace: WM_GETTITLEBARINFOEX answered with the maximize button {button:?}, title bar {:?}",
                            bounds(info.rcTitleBar)
                        );
                    }
                }
            }
        }
        if trace
            && crate::snap_layout::maximize_button_hovered()
            && !matches!(
                msg,
                WM_NCHITTEST
                    | WM_SETCURSOR
                    | WM_MOUSEMOVE
                    | WM_NCMOUSEMOVE
                    | WM_PAINT
                    | WM_ERASEBKGND
                    | WM_TIMER
            )
        {
            // What Windows asks the top window while the pointer rests on
            // the maximize button (the snap-layouts flyout's questions).
            log::info!(
                "window trace: top window message {msg:#x} ({:#x}, {:#x}) while the maximize button is hovered",
                wparam.0,
                lparam.0
            );
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

    /// G76/WUX11 on the Lenovo (200 %): the button drawn at interface size
    /// 125 % in the minimum window is 2357,17..2437,97 on screen; the click
    /// went to 2398,58, the Preferences button's centre is 2085,58.
    #[test]
    fn the_top_window_answers_hit_tests_inside_the_drawn_maximize_button() {
        let lparam = |x: i32, y: i32| ((y as u16 as isize) << 16) | (x as u16 as isize);
        let button = Some(Bounds {
            left: 2357,
            top: 17,
            right: 2437,
            bottom: 97,
        });
        assert_eq!(screen_point(lparam(2398, 58)), (2398, 58));
        assert!(hits_maximize_button(button, screen_point(lparam(2398, 58))));
        // The whole drawn button, also below a system-height title bar (55).
        assert!(hits_maximize_button(button, (2357, 17)));
        assert!(hits_maximize_button(button, (2436, 96)));
        for outside in [(2085, 58), (2356, 58), (2437, 58), (2398, 16), (2398, 97)] {
            assert!(!hits_maximize_button(button, outside));
        }
        // No button shown (full screen, a modal dialog open).
        assert!(!hits_maximize_button(None, (2398, 58)));
        // A monitor left of and above the primary: negative coordinates.
        assert_eq!(screen_point(lparam(-1500, -40)), (-1500, -40));
        let left = Some(Bounds {
            left: -1540,
            top: -60,
            right: -1460,
            bottom: 20,
        });
        assert!(hits_maximize_button(left, screen_point(lparam(-1500, -40))));
        // 64-bit lParams may carry set upper bits.
        assert_eq!(screen_point(lparam(-1500, -40) | (-1 << 32)), (-1500, -40));
    }

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
