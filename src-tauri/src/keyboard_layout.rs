//! The characters the active keyboard layout types (macOS), so menus,
//! tooltips and Preferences name the key that works on a Swiss or German
//! keyboard (UXR4M-06): the web view matches punctuation keys by their place
//! (`KeyboardEvent.code`), and WebKit has no keyboard-map API. Windows reads
//! the same in the web view (`navigator.keyboard.getLayoutMap`); Linux keeps
//! the US names. `keyboard-layout-changed` tells the web view when the user
//! switches layouts.
use std::collections::HashMap;

/// The event sent to the main web view when the input source changes.
#[cfg(target_os = "macos")]
const CHANGED_EVENT: &str = "keyboard-layout-changed";

/// The keys a shortcut can name by place, with their macOS virtual key codes
/// (Carbon's kVK_* values).
#[cfg(target_os = "macos")]
const KEYS: [(&str, u16); 48] = [
    ("Backquote", 0x32),
    ("Minus", 0x1B),
    ("Equal", 0x18),
    ("BracketLeft", 0x21),
    ("BracketRight", 0x1E),
    ("Backslash", 0x2A),
    ("Semicolon", 0x29),
    ("Quote", 0x27),
    ("Comma", 0x2B),
    ("Period", 0x2F),
    ("Slash", 0x2C),
    ("IntlBackslash", 0x0A),
    ("Digit0", 0x1D),
    ("Digit1", 0x12),
    ("Digit2", 0x13),
    ("Digit3", 0x14),
    ("Digit4", 0x15),
    ("Digit5", 0x17),
    ("Digit6", 0x16),
    ("Digit7", 0x1A),
    ("Digit8", 0x1C),
    ("Digit9", 0x19),
    ("KeyA", 0x00),
    ("KeyB", 0x0B),
    ("KeyC", 0x08),
    ("KeyD", 0x02),
    ("KeyE", 0x0E),
    ("KeyF", 0x03),
    ("KeyG", 0x05),
    ("KeyH", 0x04),
    ("KeyI", 0x22),
    ("KeyJ", 0x26),
    ("KeyK", 0x28),
    ("KeyL", 0x25),
    ("KeyM", 0x2E),
    ("KeyN", 0x2D),
    ("KeyO", 0x1F),
    ("KeyP", 0x23),
    ("KeyQ", 0x0C),
    ("KeyR", 0x0F),
    ("KeyS", 0x01),
    ("KeyT", 0x11),
    ("KeyU", 0x20),
    ("KeyV", 0x09),
    ("KeyW", 0x0D),
    ("KeyX", 0x07),
    ("KeyY", 0x10),
    ("KeyZ", 0x06),
];

#[cfg(target_os = "macos")]
mod carbon {
    use std::ffi::c_void;

    #[link(name = "Carbon", kind = "framework")]
    unsafe extern "C" {
        pub fn TISCopyCurrentKeyboardLayoutInputSource() -> *mut c_void;
        pub fn TISGetInputSourceProperty(source: *mut c_void, key: *const c_void) -> *const c_void;
        pub static kTISPropertyUnicodeKeyLayoutData: *const c_void;
        pub fn LMGetKbdType() -> u8;
        pub fn UCKeyTranslate(
            layout: *const c_void,
            virtual_key_code: u16,
            key_action: u16,
            modifier_key_state: u32,
            keyboard_type: u32,
            key_translate_options: u32,
            dead_key_state: *mut u32,
            max_string_length: usize,
            actual_string_length: *mut usize,
            unicode_string: *mut u16,
        ) -> i32;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        pub fn CFDataGetBytePtr(data: *const c_void) -> *const u8;
        pub fn CFRelease(cf: *const c_void);
    }

    pub const KEY_ACTION_DISPLAY: u16 = 3;
    pub const NO_DEAD_KEYS: u32 = 1;
}

/// Code → the character its key types without modifiers. Main thread (the
/// Text Input Sources API asserts it on current macOS).
#[cfg(target_os = "macos")]
fn read_layout() -> HashMap<String, String> {
    use carbon::*;
    let mut out = HashMap::new();
    // SAFETY: Carbon calls on the main thread; the source is released, the
    // layout data is borrowed from it while it lives.
    unsafe {
        let source = TISCopyCurrentKeyboardLayoutInputSource();
        if source.is_null() {
            return out;
        }
        let data = TISGetInputSourceProperty(source, kTISPropertyUnicodeKeyLayoutData);
        if !data.is_null() {
            let layout = CFDataGetBytePtr(data).cast::<std::ffi::c_void>();
            let keyboard = u32::from(LMGetKbdType());
            for (code, virtual_key) in KEYS {
                let mut dead = 0u32;
                let mut length = 0usize;
                let mut chars = [0u16; 4];
                let status = UCKeyTranslate(
                    layout,
                    virtual_key,
                    KEY_ACTION_DISPLAY,
                    0,
                    keyboard,
                    NO_DEAD_KEYS,
                    &mut dead,
                    chars.len(),
                    &mut length,
                    chars.as_mut_ptr(),
                );
                if status == 0 && length > 0 {
                    let text = String::from_utf16_lossy(&chars[..length.min(chars.len())]);
                    if text.chars().count() == 1 && !text.chars().all(char::is_control) {
                        out.insert(code.to_string(), text);
                    }
                }
            }
        }
        CFRelease(source.cast_const());
    }
    out
}

/// Code → the character the active layout types on that key (macOS; empty
/// elsewhere, where the web view reads it itself or keeps US names).
#[tauri::command]
pub async fn keyboard_layout(app: tauri::AppHandle) -> Result<HashMap<String, String>, String> {
    #[cfg(target_os = "macos")]
    {
        let (send, receive) = std::sync::mpsc::channel();
        app.run_on_main_thread(move || {
            let _ = send.send(read_layout());
        })
        .map_err(|e| e.to_string())?;
        receive.recv().map_err(|e| e.to_string())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = app;
        Ok(HashMap::new())
    }
}

/// Tells the main web view when the user switches keyboard layouts (macOS).
#[cfg(target_os = "macos")]
pub fn observe_changes(app: &tauri::AppHandle) {
    use objc::runtime::{Class, Object, Sel};
    use objc::{class, msg_send, sel, sel_impl};
    use std::sync::{Once, OnceLock};

    static APP: OnceLock<tauri::AppHandle> = OnceLock::new();
    static REGISTER: Once = Once::new();
    if APP.set(app.clone()).is_err() {
        return;
    }
    extern "C" fn changed(_: &Object, _: Sel, _: *mut Object) {
        use tauri::Emitter;
        if let Some(app) = APP.get() {
            let _ = app.emit_to("main", CHANGED_EVENT, ());
        }
    }
    REGISTER.call_once(|| {
        if let Some(mut decl) = objc::declare::ClassDecl::new("RawtonicKeyboardLayout", class!(NSObject)) {
            // SAFETY: the selector takes one object (the notification).
            unsafe {
                decl.add_method(
                    sel!(layoutChanged:),
                    changed as extern "C" fn(&Object, Sel, *mut Object),
                );
            }
            decl.register();
        }
    });
    let Some(class) = Class::get("RawtonicKeyboardLayout") else {
        return;
    };
    let _ = app.run_on_main_thread(move || {
        // SAFETY: main thread; the observer lives as long as the app.
        unsafe {
            let observer: *mut Object = msg_send![class, new];
            let nil: *mut Object = std::ptr::null_mut();
            // The text input system's own note, and Carbon's distributed one
            // (sent whichever way the layout was switched).
            for (center, name) in [
                (
                    msg_send![class!(NSNotificationCenter), defaultCenter],
                    "NSTextInputContextKeyboardSelectionDidChangeNotification",
                ),
                (
                    msg_send![class!(NSDistributedNotificationCenter), defaultCenter],
                    "com.apple.Carbon.TISNotifySelectedKeyboardInputSourceChanged",
                ),
            ] as [(*mut Object, &str); 2]
            {
                let name = objc2_foundation::NSString::from_str(name);
                let name = objc2::rc::Retained::as_ptr(&name)
                    .cast_mut()
                    .cast::<Object>();
                let () = msg_send![center, addObserver: observer
                                             selector: sel!(layoutChanged:)
                                                 name: name
                                               object: nil];
            }
        }
    });
}
