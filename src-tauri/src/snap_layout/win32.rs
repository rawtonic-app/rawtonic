//! The native part of [`super`]: a transparent child window over the
//! studio toolbar's maximize button that answers `WM_NCHITTEST` with
//! `HTMAXBUTTON`. Windows 11 opens the snap-layouts flyout only for that
//! hit-test answer, and the WebView2 content (a window of another process)
//! never gives it. The window is layered with alpha 1 of 255, so the button
//! drawn by the webview below stays visible; a layered child window needs
//! the Windows 8+ compatibility entry in the app manifest (`build.rs`).
//!
//! Because the pointer then talks to this window instead of the webview, it
//! also does the button's work: a click toggles maximize/restore on the top
//! window, and hover/pressed changes are reported through [`set_listener`]
//! so the webview can draw them. Everything runs on the UI thread.

use std::cell::Cell;
use std::sync::{Once, OnceLock};

use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, TME_LEAVE, TME_NONCLIENT, TRACKMOUSEEVENT, TrackMouseEvent,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GA_ROOT, GetAncestor, GetCursorPos, GetParent,
    GetWindowRect, HTMAXBUTTON, HWND_TOP, IDC_ARROW, IsZoomed, LWA_ALPHA, LoadCursorW,
    RegisterClassW, SW_HIDE, SW_MAXIMIZE, SW_RESTORE, SWP_NOACTIVATE, SWP_SHOWWINDOW,
    SetLayeredWindowAttributes, SetWindowPos, ShowWindow, WM_CANCELMODE, WM_CAPTURECHANGED,
    WM_LBUTTONUP, WM_NCHITTEST, WM_NCLBUTTONDOWN, WM_NCLBUTTONUP, WM_NCMOUSELEAVE, WM_NCMOUSEMOVE,
    WM_NCRBUTTONDOWN, WM_NCRBUTTONUP, WNDCLASSW, WS_CHILD, WS_CLIPSIBLINGS, WS_EX_LAYERED,
    WS_EX_NOACTIVATE,
};
use windows::core::w;

/// The button's area in physical pixels of the top window's client area.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

type Listener = Box<dyn Fn(bool, bool) + Send + Sync>;
static LISTENER: OnceLock<Listener> = OnceLock::new();

/// Called with `(hovered, pressed)` whenever either changes.
pub fn set_listener(listener: impl Fn(bool, bool) + Send + Sync + 'static) {
    let _ = LISTENER.set(Box::new(listener));
}

thread_local! {
    static OVERLAY: Cell<isize> = const { Cell::new(0) };
    static HOVERED: Cell<bool> = const { Cell::new(false) };
    static PRESSED: Cell<bool> = const { Cell::new(false) };
}

const CLASS_NAME: windows::core::PCWSTR = w!("RawtonicMaximizeButton");

fn set_state(hovered: bool, pressed: bool) {
    let hover_changed = HOVERED.replace(hovered) != hovered;
    let press_changed = PRESSED.replace(pressed) != pressed;
    if (hover_changed || press_changed)
        && let Some(listener) = LISTENER.get()
    {
        listener(hovered, pressed);
    }
}

/// Places the overlay over `rect` of `parent` (the top window), or hides it
/// for `None`. Must run on the UI thread that owns `parent`.
///
/// # Safety
/// `parent` must be a live top-level window of this thread.
pub unsafe fn place(parent: isize, rect: Option<Rect>) -> windows::core::Result<()> {
    let parent = HWND(parent);
    let mut overlay = HWND(OVERLAY.get());
    if overlay.0 != 0 && unsafe { GetParent(overlay) } != parent {
        // A different (or recreated) top window: start over.
        let _ = unsafe { DestroyWindow(overlay) };
        overlay = HWND(0);
        OVERLAY.set(0);
    }
    let Some(rect) = rect.filter(|r| r.width > 0 && r.height > 0) else {
        if overlay.0 != 0 {
            let _ = unsafe { ShowWindow(overlay, SW_HIDE) };
        }
        set_state(false, false);
        return Ok(());
    };
    if overlay.0 == 0 {
        overlay = unsafe { create(parent)? };
        OVERLAY.set(overlay.0);
    }
    if crate::window_limits::trace() {
        log::info!("window trace: maximize button area {rect:?}");
    }
    // HWND_TOP also keeps it above the webview's window, which sits in the
    // same sibling list.
    unsafe {
        SetWindowPos(
            overlay,
            HWND_TOP,
            rect.x,
            rect.y,
            rect.width,
            rect.height,
            SWP_NOACTIVATE | SWP_SHOWWINDOW,
        )
    }
}

unsafe fn create(parent: HWND) -> windows::core::Result<HWND> {
    let instance = HINSTANCE(unsafe { GetModuleHandleW(None)? }.0);
    static REGISTER: Once = Once::new();
    let mut registered = Ok(());
    REGISTER.call_once(|| {
        let class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            lpszClassName: CLASS_NAME,
            hCursor: unsafe { LoadCursorW(None, IDC_ARROW) }.unwrap_or_default(),
            ..Default::default()
        };
        if unsafe { RegisterClassW(&class) } == 0 {
            registered = Err(windows::core::Error::from_win32());
        }
    });
    registered?;
    let overlay = unsafe {
        CreateWindowExW(
            WS_EX_LAYERED | WS_EX_NOACTIVATE,
            CLASS_NAME,
            None,
            WS_CHILD | WS_CLIPSIBLINGS,
            0,
            0,
            0,
            0,
            parent,
            None,
            instance,
            None,
        )
    };
    if overlay.0 == 0 {
        return Err(windows::core::Error::from_win32());
    }
    // Alpha 1, not 0: a fully transparent layered window lets the pointer through.
    if let Err(error) = unsafe { SetLayeredWindowAttributes(overlay, COLORREF(0), 1, LWA_ALPHA) } {
        let _ = unsafe { DestroyWindow(overlay) };
        return Err(error);
    }
    Ok(overlay)
}

fn toggle_maximize(overlay: HWND) {
    unsafe {
        let top = GetAncestor(overlay, GA_ROOT);
        let zoomed = IsZoomed(top).as_bool();
        let _ = ShowWindow(top, if zoomed { SW_RESTORE } else { SW_MAXIMIZE });
        if crate::window_limits::trace() {
            log::info!(
                "window trace: maximize button toggled, zoomed {zoomed} -> {}",
                IsZoomed(top).as_bool()
            );
        }
    }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_NCHITTEST => return LRESULT(HTMAXBUTTON as isize),
        WM_NCMOUSEMOVE => {
            if !HOVERED.get() {
                let mut track = TRACKMOUSEEVENT {
                    cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE | TME_NONCLIENT,
                    hwndTrack: hwnd,
                    dwHoverTime: 0,
                };
                let _ = unsafe { TrackMouseEvent(&mut track) };
                set_state(true, PRESSED.get());
            }
        }
        // The shell flyout can send LEAVE between a button down and up.
        // Preserve the click until its release; cancellation is explicit.
        WM_NCMOUSELEAVE => set_state(false, PRESSED.get()),
        WM_CANCELMODE | WM_CAPTURECHANGED => {
            if PRESSED.get() && crate::window_limits::trace() {
                log::info!("window trace: maximize button press cancelled by {msg:#x}");
            }
            set_state(false, false);
        }
        // Handled here: DefWindowProc would run its own caption-button loop
        // and send the maximize command to this child window.
        WM_NCLBUTTONDOWN => {
            if crate::window_limits::trace() {
                log::info!("window trace: maximize button press");
            }
            set_state(true, true);
            let _ = unsafe { SetCapture(hwnd) };
            return LRESULT(0);
        }
        WM_NCLBUTTONUP | WM_LBUTTONUP => {
            let was_pressed = PRESSED.get();
            let mut point = POINT::default();
            let mut rect = RECT::default();
            let inside = unsafe { GetCursorPos(&mut point) }.is_ok()
                && unsafe { GetWindowRect(hwnd, &mut rect) }.is_ok()
                && point.x >= rect.left
                && point.x < rect.right
                && point.y >= rect.top
                && point.y < rect.bottom;
            set_state(inside, false);
            let _ = unsafe { ReleaseCapture() };
            if crate::window_limits::trace() {
                log::info!(
                    "window trace: maximize button release {msg:#x}, pressed {was_pressed}, inside {inside}, pointer {},{} button {},{}..{},{}",
                    point.x,
                    point.y,
                    rect.left,
                    rect.top,
                    rect.right,
                    rect.bottom
                );
            }
            if was_pressed && inside {
                toggle_maximize(hwnd);
            }
            return LRESULT(0);
        }
        WM_NCRBUTTONDOWN | WM_NCRBUTTONUP => return LRESULT(0),
        _ => {}
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}
