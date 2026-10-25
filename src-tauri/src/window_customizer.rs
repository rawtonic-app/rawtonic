use tauri::{Runtime, Webview, plugin::Plugin};

pub struct PinchZoomDisablePlugin;

#[cfg(target_os = "macos")]
const MACOS_WINDOW_RADIUS: f64 = 14.0;

#[cfg(target_os = "macos")]
unsafe fn apply_macos_window_rounding(ns_view: *mut objc::runtime::Object) {
    use objc::{msg_send, sel, sel_impl};

    if ns_view.is_null() {
        return;
    }

    let ns_window: *mut objc::runtime::Object = msg_send![ns_view, window];
    if ns_window.is_null() {
        return;
    }

    let () = msg_send![ns_window, setOpaque: false];
    let () = msg_send![ns_window, setHasShadow: true];

    let content_view: *mut objc::runtime::Object = msg_send![ns_window, contentView];
    if !content_view.is_null() {
        let () = msg_send![content_view, setWantsLayer: true];
        let content_layer: *mut objc::runtime::Object = msg_send![content_view, layer];
        if !content_layer.is_null() {
            let () = msg_send![content_layer, setCornerRadius: MACOS_WINDOW_RADIUS];
            let () = msg_send![content_layer, setMasksToBounds: true];
        }
    }

    let () = msg_send![ns_view, setWantsLayer: true];
    let webview_layer: *mut objc::runtime::Object = msg_send![ns_view, layer];
    if !webview_layer.is_null() {
        let () = msg_send![webview_layer, setCornerRadius: MACOS_WINDOW_RADIUS];
        let () = msg_send![webview_layer, setMasksToBounds: true];
    }

    let () = msg_send![ns_window, invalidateShadow];
}

impl Default for PinchZoomDisablePlugin {
    fn default() -> Self {
        Self
    }
}

impl<R: Runtime> Plugin<R> for PinchZoomDisablePlugin {
    fn name(&self) -> &'static str {
        "Does not matter here"
    }

    fn webview_created(&mut self, webview: Webview<R>) {
        let _ = webview.with_webview(|_webview| {
            #[cfg(target_os = "macos")]
            unsafe {
                apply_macos_window_rounding(_webview.inner().cast());
                let view: *mut objc::runtime::Object = _webview.inner().cast();
                if !view.is_null() {
                    use objc::{msg_send, sel, sel_impl};
                    let window: *mut objc::runtime::Object = msg_send![view, window];
                    if !window.is_null() {
                        full_screen::observe(window);
                    }
                }
            }

            #[cfg(target_os = "linux")]
            unsafe {
                use gtk::GestureZoom;
                use gtk::glib::ObjectExt;
                use webkit2gtk::glib::gobject_ffi;

                if let Some(data) = _webview.inner().data::<GestureZoom>("wk-view-zoom-gesture") {
                    gobject_ffi::g_signal_handlers_destroy(data.as_ptr().cast());
                }
            }
        });
    }
}

/// How long a full screen change may animate before a request is no longer
/// held for it. macOS sends no notification when a change fails to start.
#[cfg(any(target_os = "macos", test))]
const TRANSITION_LIMIT: std::time::Duration = std::time::Duration::from_secs(3);

/// Full screen requests while macOS animates a change (F, F11, ⌃⌘F or the
/// green button): a request then waits and is applied when the change is
/// over, as Tao's own full screen does; `toggleFullScreen:` during the
/// animation would be dropped. Only the last request counts.
#[cfg(any(target_os = "macos", test))]
#[derive(Debug, Default)]
struct FullScreenRequests {
    transition_since: Option<std::time::Instant>,
    pending: Option<bool>,
}

#[cfg(any(target_os = "macos", test))]
impl FullScreenRequests {
    fn in_transition(&self, now: std::time::Instant) -> bool {
        self.transition_since
            .is_some_and(|since| now.duration_since(since) < TRANSITION_LIMIT)
    }

    /// A request for `on` with the window now `current`: true to toggle now.
    fn request(&mut self, now: std::time::Instant, current: bool, on: bool) -> bool {
        if self.in_transition(now) {
            self.pending = Some(on);
            return false;
        }
        self.pending = None;
        if current == on {
            return false;
        }
        self.transition_since = Some(now);
        true
    }

    /// The window began entering or leaving full screen.
    fn started(&mut self, now: std::time::Instant) {
        self.transition_since = Some(now);
    }

    /// The change is over with the window `current`: true to toggle again
    /// for a request made meanwhile.
    fn finished(&mut self, now: std::time::Instant, current: bool) -> bool {
        self.transition_since = None;
        match self.pending.take() {
            Some(on) if on != current => {
                self.transition_since = Some(now);
                true
            }
            _ => false,
        }
    }
}

#[cfg(target_os = "macos")]
static FULL_SCREEN_REQUESTS: std::sync::Mutex<FullScreenRequests> =
    std::sync::Mutex::new(FullScreenRequests {
        transition_since: None,
        pending: None,
    });

#[cfg(target_os = "macos")]
mod full_screen {
    use super::FULL_SCREEN_REQUESTS;
    use objc::declare::ClassDecl;
    use objc::runtime::{Class, Object, Sel};
    use objc::{class, msg_send, sel, sel_impl};
    use std::sync::Once;
    use std::time::Instant;

    /// NSWindowStyleMaskFullScreen.
    const FULL_SCREEN: usize = 1 << 14;

    /// Whether `window` is in full screen. Main thread, live NSWindow.
    pub unsafe fn is_full_screen(window: *mut Object) -> bool {
        let mask: usize = unsafe { msg_send![window, styleMask] };
        mask & FULL_SCREEN != 0
    }

    /// `toggleFullScreen:`, as ⌃⌘F. Main thread, live NSWindow.
    pub unsafe fn toggle(window: *mut Object) {
        let () = unsafe { msg_send![window, toggleFullScreen: std::ptr::null_mut::<Object>()] };
    }

    extern "C" fn will_change(_: &Object, _: Sel, _: *mut Object) {
        let mut requests = FULL_SCREEN_REQUESTS
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        requests.started(Instant::now());
    }

    extern "C" fn did_change(_: &Object, _: Sel, notification: *mut Object) {
        // SAFETY: AppKit posts these on the main thread with the window.
        unsafe {
            let window: *mut Object = msg_send![notification, object];
            if window.is_null() {
                return;
            }
            let again = FULL_SCREEN_REQUESTS
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .finished(Instant::now(), is_full_screen(window));
            if again {
                toggle(window);
            }
        }
    }

    /// Follows the window's full screen changes, whoever started them.
    /// Main thread, live NSWindow; the observer lives as long as the app.
    pub unsafe fn observe(window: *mut Object) {
        static REGISTER: Once = Once::new();
        REGISTER.call_once(|| {
            if let Some(mut decl) = ClassDecl::new("RawtonicFullScreenRequests", class!(NSObject)) {
                // SAFETY: both take one object argument, as the selectors say.
                unsafe {
                    decl.add_method(
                        sel!(willChange:),
                        will_change as extern "C" fn(&Object, Sel, *mut Object),
                    );
                    decl.add_method(
                        sel!(didChange:),
                        did_change as extern "C" fn(&Object, Sel, *mut Object),
                    );
                }
                decl.register();
            }
        });
        let Some(class) = Class::get("RawtonicFullScreenRequests") else {
            return;
        };
        unsafe {
            let observer: *mut Object = msg_send![class, new];
            let center: *mut Object = msg_send![class!(NSNotificationCenter), defaultCenter];
            for (name, selector) in [
                ("NSWindowWillEnterFullScreenNotification", sel!(willChange:)),
                ("NSWindowWillExitFullScreenNotification", sel!(willChange:)),
                ("NSWindowDidEnterFullScreenNotification", sel!(didChange:)),
                ("NSWindowDidExitFullScreenNotification", sel!(didChange:)),
            ] {
                let name = objc2_foundation::NSString::from_str(name);
                let name = objc2::rc::Retained::as_ptr(&name)
                    .cast_mut()
                    .cast::<Object>();
                let () = msg_send![center, addObserver: observer
                                             selector: selector
                                                 name: name
                                               object: window];
            }
        }
    }
}

/// Puts the window into macOS full screen or takes it out, as ⌃⌘F and the
/// green button do (`toggleFullScreen:`). Leaving through Tauri's
/// `setFullscreen(false)` left the page without keyboard focus — the next F
/// went nowhere until a click (measured 2026-10-25 in a QA build:
/// `document.hasFocus()` false after leaving that way, true after
/// `toggleFullScreen:`). A request during the animation waits for its end
/// (`FullScreenRequests`). Elsewhere the window's own full screen is used.
#[tauri::command]
pub fn set_window_full_screen(window: tauri::WebviewWindow, on: bool) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let target = window.clone();
        window
            .run_on_main_thread(move || {
                use objc::runtime::Object;
                let Ok(ns_window) = target.ns_window() else {
                    return;
                };
                let ns_window = ns_window.cast::<Object>();
                if ns_window.is_null() {
                    return;
                }
                // SAFETY: on the main thread with the live NSWindow.
                unsafe {
                    let now = full_screen::is_full_screen(ns_window);
                    let toggle = FULL_SCREEN_REQUESTS
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .request(std::time::Instant::now(), now, on);
                    if toggle {
                        full_screen::toggle(ns_window);
                    }
                }
            })
            .map_err(|e| e.to_string())
    }
    #[cfg(not(target_os = "macos"))]
    window.set_fullscreen(on).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::{FullScreenRequests, TRANSITION_LIMIT};
    use std::time::{Duration, Instant};

    #[test]
    fn a_request_during_the_animation_waits_for_its_end() {
        let t = Instant::now();
        let mut requests = FullScreenRequests::default();
        // F: into full screen, animated.
        assert!(requests.request(t, false, true));
        // F again 50 ms later, while the window already reports full screen.
        assert!(!requests.request(t + Duration::from_millis(50), true, false));
        // The entry ends: the waiting request takes the window out again.
        assert!(requests.finished(t + Duration::from_millis(700), true));
        // That exit ends with nothing left to do.
        assert!(!requests.finished(t + Duration::from_millis(1400), false));
        assert!(!requests.request(t + Duration::from_millis(1500), false, false));
    }

    #[test]
    fn only_the_last_request_during_an_animation_counts() {
        let t = Instant::now();
        let mut requests = FullScreenRequests::default();
        assert!(requests.request(t, false, true));
        assert!(!requests.request(t + Duration::from_millis(50), true, false));
        assert!(!requests.request(t + Duration::from_millis(80), true, true));
        // In full screen as last asked: no second change.
        assert!(!requests.finished(t + Duration::from_millis(700), true));
    }

    #[test]
    fn a_change_started_elsewhere_also_holds_requests() {
        let t = Instant::now();
        let mut requests = FullScreenRequests::default();
        // ⌃⌘F or the green button: only the notifications are seen.
        requests.started(t);
        assert!(!requests.request(t + Duration::from_millis(100), true, false));
        assert!(requests.finished(t + Duration::from_millis(700), true));
    }

    #[test]
    fn a_change_that_never_ends_stops_holding_requests() {
        let t = Instant::now();
        let mut requests = FullScreenRequests::default();
        assert!(requests.request(t, false, true));
        // No notification came (the change failed to start).
        assert!(requests.request(t + TRANSITION_LIMIT, false, true));
    }

    #[test]
    fn a_request_for_the_current_state_does_nothing() {
        let mut requests = FullScreenRequests::default();
        assert!(!requests.request(Instant::now(), true, true));
    }
}
