//! The desktop's window menu (Keep above, Move to desktop/screen, More
//! actions…) on a right-click of the app's own title bar (UXR4L-07). Linux
//! only: GTK asks the compositor for it (`xdg_toplevel.show_window_menu` on
//! Wayland, `_GTK_SHOW_WINDOW_MENU` on X11). Windows and macOS do nothing.

/// Shows the window menu at `x`, `y` (CSS pixels from the window's top left
/// corner, which are GTK's window coordinates). Async, so it never runs on
/// the main thread it hands the work to; whether the compositor shows a menu
/// is its own decision (logged when GTK could not ask).
#[tauri::command]
pub async fn show_window_menu(window: tauri::WebviewWindow, x: f64, y: f64) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        let target = window.clone();
        window
            .run_on_main_thread(move || match linux::show(&target, x, y) {
                Ok(true) => {}
                Ok(false) => log::info!("The desktop offers no window menu for this window."),
                Err(e) => log::warn!("Window menu: {e}"),
            })
            .map_err(|e| e.to_string())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (window, x, y);
        Ok(())
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use gtk::gdk;
    use gtk::glib::translate::{ToGlibPtr, ToGlibPtrMut};
    use gtk::prelude::*;

    /// GTK needs a button event for the menu: the compositor checks it
    /// against the last press of the seat's pointer (the right-click that is
    /// still held, as WebKitGTK reports `contextmenu` on the press).
    pub fn show(window: &tauri::WebviewWindow, x: f64, y: f64) -> Result<bool, String> {
        let gtk_window = window.gtk_window().map_err(|e| e.to_string())?;
        let gdk_window = gtk_window.window().ok_or("The window is not shown.")?;
        let pointer = gdk_window
            .display()
            .default_seat()
            .and_then(|seat| seat.pointer())
            .ok_or("No pointer on this seat.")?;
        let mut event = gdk::Event::new(gdk::EventType::ButtonPress);
        event.set_device(Some(&pointer));
        // SAFETY: a fresh button event owned by `event`; it holds a reference
        // to the window, which gdk_event_free (on drop) gives back.
        unsafe {
            let raw: *mut gdk::ffi::GdkEvent = event.to_glib_none_mut().0;
            let button = &mut (*raw).button;
            button.window = gdk_window.to_glib_full();
            button.send_event = 1;
            button.time = gdk::ffi::GDK_CURRENT_TIME as u32;
            button.x = x;
            button.y = y;
            button.button = 3;
        }
        Ok(gdk_window.show_window_menu(&mut event))
    }
}
