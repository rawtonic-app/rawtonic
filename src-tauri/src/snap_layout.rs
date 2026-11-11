//! Windows 11 snap layouts on the studio toolbar's own maximize button
//! (the window has no system frame, `decorations: false`). The frontend
//! reports where the button is; on Windows a native overlay over that area
//! answers the hit test as a maximize button (`win32.rs`). Elsewhere the
//! command does nothing.

use serde::Deserialize;

/// Event with the overlay's pointer state, so the webview draws the
/// button's hover and pressed looks it no longer sees itself.
#[cfg(windows)]
pub const BUTTON_STATE_EVENT: &str = "maximize-button-state";

/// The maximize button in physical pixels of the window's client area.
#[derive(Clone, Copy, Debug, Deserialize)]
#[cfg_attr(not(windows), allow(dead_code))] // read only on Windows
pub struct ButtonRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[cfg(windows)]
#[derive(Clone, Copy, Debug, serde::Serialize)]
pub struct ButtonState {
    pub hovered: bool,
    pub pressed: bool,
}

/// Places the snap-layouts area over the maximize button, or removes it
/// (`None`: no button shown, e.g. full screen).
#[tauri::command]
pub fn set_maximize_button_rect(
    window: tauri::WebviewWindow,
    rect: Option<ButtonRect>,
) -> Result<(), String> {
    #[cfg(windows)]
    {
        use tauri::{Emitter, Manager};
        let parent = window.hwnd().map_err(|e| e.to_string())?.0 as isize;
        let app = window.app_handle().clone();
        win32::set_listener(move |hovered, pressed| {
            let _ = app.emit(BUTTON_STATE_EVENT, ButtonState { hovered, pressed });
        });
        // Outward rounding: the whole button answers, never less.
        let rect = rect.map(|r| {
            let (left, top) = (r.x.floor(), r.y.floor());
            win32::Rect {
                x: left as i32,
                y: top as i32,
                width: ((r.x + r.width).ceil() - left) as i32,
                height: ((r.y + r.height).ceil() - top) as i32,
            }
        });
        let label = window.label().to_owned();
        window
            .run_on_main_thread(move || {
                // SAFETY: on the UI thread, with the live top window of this webview.
                if let Err(error) = unsafe { win32::place(parent, rect) } {
                    log::warn!("snap layouts: maximize-button overlay for {label} failed: {error}");
                }
            })
            .map_err(|e| e.to_string())
    }
    #[cfg(not(windows))]
    {
        let _ = (window, rect);
        Ok(())
    }
}

#[cfg(windows)]
mod win32;
