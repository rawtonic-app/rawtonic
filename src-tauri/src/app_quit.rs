//! Quitting waits for the editor's pending saves. The editor saves recipes
//! shortly after an edit (debounced), so closing the main window, the
//! title bar's close button and Quit (Cmd+Q) first ask the webview
//! (`app-quit-requested`) to write what is pending; it answers with
//! `quit_saving` at once and `finish_quit` when everything is on disk, or
//! with `cancel_quit` when a save failed and the user keeps the app open. A
//! webview that does not answer cannot keep the app open: it quits after
//! `ACK_TIMEOUT` without an answer and after `SAVE_TIMEOUT` in any case.
//!
//! macOS terminating the app from outside its menu (Dock Quit, log out,
//! restart, shut down) asks the app delegate `applicationShouldTerminate:`,
//! which tao does not implement; `hold_system_terminate` adds it to tao's
//! delegate class (`system_terminate`). It answers "later", runs the same
//! request, and the process ends when the editor finished (or the timeouts
//! ran out); a save that failed cancels the termination (`cancel_quit`; a
//! log out then stops, as with any app holding unsaved work) and the user
//! decides as after Cmd+Q. When the editor is done the app answers AppKit
//! (`replyToApplicationShouldTerminate:`), which replies to the quit Apple
//! event and ends the app through `RunEvent::Exit`: ending the process
//! without that reply (`_exit` alone) left the sender (AppleScript, the
//! Dock, log out) waiting about half a second for an answer that never came.
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, Runtime};

/// The webview did not take the request (not loaded, hung).
const ACK_TIMEOUT: Duration = Duration::from_secs(3);
/// Saving took longer than any recipe write should.
const SAVE_TIMEOUT: Duration = Duration::from_secs(30);

/// The custom Quit menu item (macOS) that replaces AppKit's predefined one.
#[cfg(target_os = "macos")]
const QUIT_MENU_ID: &str = "rawtonic-quit";

/// The quit was decided: the window may close and the app exit.
static QUITTING: AtomicBool = AtomicBool::new(false);
/// A request waits for the webview; further requests are ignored meanwhile.
static PENDING: AtomicBool = AtomicBool::new(false);
static ACKNOWLEDGED: AtomicBool = AtomicBool::new(false);
/// Identifies the request a timeout belongs to (`cancel_quit` ends it).
static GENERATION: AtomicU64 = AtomicU64::new(0);

/// A close of the main window: returns whether it must be held while the
/// webview saves (the close then becomes a quit request).
pub fn intercept_close<R: Runtime>(app: &AppHandle<R>) -> bool {
    if QUITTING.load(Ordering::SeqCst) {
        return false;
    }
    request_quit(app);
    true
}

/// Asks the webview to save what is pending and quit (see the module).
pub fn request_quit<R: Runtime>(app: &AppHandle<R>) {
    if QUITTING.load(Ordering::SeqCst) || PENDING.swap(true, Ordering::SeqCst) {
        return;
    }
    ACKNOWLEDGED.store(false, Ordering::SeqCst);
    let generation = GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    if let Err(e) = app.emit_to("main", "app-quit-requested", ()) {
        log::warn!("Could not ask the editor to save before quitting: {e}");
        exit(app);
        return;
    }
    let app = app.clone();
    std::thread::spawn(move || {
        let waiting =
            || PENDING.load(Ordering::SeqCst) && GENERATION.load(Ordering::SeqCst) == generation;
        std::thread::sleep(ACK_TIMEOUT);
        if waiting() && !ACKNOWLEDGED.load(Ordering::SeqCst) {
            log::warn!("The editor did not answer the quit request; quitting without it.");
            exit(&app);
            return;
        }
        std::thread::sleep(SAVE_TIMEOUT - ACK_TIMEOUT);
        if waiting() {
            log::warn!("The editor did not finish saving in time; quitting.");
            exit(&app);
        }
    });
}

fn exit<R: Runtime>(app: &AppHandle<R>) {
    QUITTING.store(true, Ordering::SeqCst);
    // AppKit waits for its termination: let it go on (system_terminate).
    #[cfg(target_os = "macos")]
    if system_terminate::WAITING.load(Ordering::SeqCst) {
        system_terminate::finish(app);
        return;
    }
    // ExitRequested with a code: lib.rs ends the process at once.
    app.exit(0);
}

/// The webview took the quit request and is saving.
#[tauri::command]
pub(crate) fn quit_saving() {
    ACKNOWLEDGED.store(true, Ordering::SeqCst);
}

/// Everything is saved (or the user quits anyway): quit now.
#[tauri::command]
pub(crate) fn finish_quit(app_handle: AppHandle) {
    exit(&app_handle);
}

/// A save failed and the user decides: the app stays open meanwhile (and a
/// termination AppKit waits for is cancelled).
#[tauri::command]
pub(crate) fn cancel_quit(app_handle: AppHandle) {
    GENERATION.fetch_add(1, Ordering::SeqCst);
    PENDING.store(false, Ordering::SeqCst);
    #[cfg(target_os = "macos")]
    system_terminate::cancel(&app_handle);
    #[cfg(not(target_os = "macos"))]
    let _ = app_handle;
}

/// Whether an exit request asks for a restart (`relaunch()` in the webview,
/// Tauri's `request_restart`).
pub fn is_restart(code: Option<i32>) -> bool {
    code == Some(tauri::RESTART_EXIT_CODE)
}

/// Starts the new instance of a restart. Tauri would start it after
/// `RunEvent::Exit`, which the app never reaches: lib.rs ends the process in
/// `ExitRequested`. The single-instance name is released first, else the new
/// process would find this one, hand its arguments over and quit.
pub fn start_new_instance<R: Runtime>(app: &AppHandle<R>) {
    #[cfg(any(target_os = "windows", target_os = "macos", target_os = "linux"))]
    tauri_plugin_single_instance::destroy(app);
    let env = app.env();
    let started = tauri::process::current_binary(&env).and_then(|binary| {
        std::process::Command::new(binary)
            .args(env.args_os.iter().skip(1))
            .spawn()
    });
    if let Err(e) = started {
        log::error!("Could not restart the app: {e}");
    }
}

#[cfg(target_os = "macos")]
pub use system_terminate::hold_system_terminate;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_tauris_restart_code_restarts() {
        assert!(is_restart(Some(tauri::RESTART_EXIT_CODE)));
        assert!(!is_restart(Some(0)));
        assert!(!is_restart(Some(1)));
        assert!(!is_restart(None));
    }
}

/// macOS: the delegate method AppKit asks before it terminates the app on
/// its own (Dock Quit, log out, restart, shut down).
#[cfg(target_os = "macos")]
mod system_terminate {
    use super::{QUITTING, request_quit};
    use objc2::runtime::{AnyClass, AnyObject, Bool, Imp, Sel};
    use objc2::{class, msg_send, sel};
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;
    use tauri::{AppHandle, Runtime};

    /// AppKit was told to terminate but the process still runs: end it.
    const FINISH_TIMEOUT: Duration = Duration::from_secs(2);

    // NSApplicationTerminateReply
    const TERMINATE_NOW: usize = 1;
    const TERMINATE_LATER: usize = 2;

    /// Starts the quit request (`request_quit` with the app's handle).
    static REQUEST: OnceLock<Box<dyn Fn() + Send + Sync>> = OnceLock::new();
    /// AppKit waits for `replyToApplicationShouldTerminate:`.
    pub(super) static WAITING: AtomicBool = AtomicBool::new(false);
    /// `should_terminate` runs: a quit decided meanwhile is its answer.
    static DECIDING: AtomicBool = AtomicBool::new(false);

    extern "C-unwind" fn should_terminate(
        _this: &AnyObject,
        _cmd: Sel,
        _sender: *mut AnyObject,
    ) -> usize {
        let Some(request) = REQUEST.get() else {
            return TERMINATE_NOW;
        };
        if QUITTING.load(Ordering::SeqCst) {
            return TERMINATE_NOW;
        }
        WAITING.store(true, Ordering::SeqCst);
        log::info!("macOS asked to quit; saving pending edits first.");
        // Answers AppKit when done (`finish`) or cancels (`cancel`).
        DECIDING.store(true, Ordering::SeqCst);
        request();
        DECIDING.store(false, Ordering::SeqCst);
        // The editor could not be asked: quit at once.
        if QUITTING.load(Ordering::SeqCst) {
            WAITING.store(false, Ordering::SeqCst);
            return TERMINATE_NOW;
        }
        TERMINATE_LATER
    }

    /// Adds `applicationShouldTerminate:` to the app delegate's class
    /// (tao's, which does not implement it); leaves a delegate alone that
    /// already decides. Once, on the main thread, after launch (setup).
    pub fn hold_system_terminate<R: Runtime>(app: &AppHandle<R>) {
        let handle = app.clone();
        if REQUEST
            .set(Box::new(move || request_quit(&handle)))
            .is_err()
        {
            return;
        }
        unsafe {
            let ns_app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
            let delegate: *mut AnyObject = msg_send![ns_app, delegate];
            let Some(delegate) = delegate.as_ref() else {
                log::warn!("No app delegate: macOS quits (Dock, log out) do not wait for saves.");
                return;
            };
            if let Err(e) = add_should_terminate(delegate.class()) {
                log::warn!("{e}; macOS quits (Dock, log out) do not wait for saves.");
            }
        }
    }

    /// Adds `should_terminate` as `applicationShouldTerminate:` to `class`,
    /// unless it (or a superclass) has one.
    ///
    /// # Safety
    /// `class` is an app delegate class (or a test's own class).
    unsafe fn add_should_terminate(class: &AnyClass) -> Result<(), &'static str> {
        let selector = sel!(applicationShouldTerminate:);
        if class.instance_method(selector).is_some() {
            return Err("The app delegate already answers applicationShouldTerminate:");
        }
        let method: extern "C-unwind" fn(&AnyObject, Sel, *mut AnyObject) -> usize =
            should_terminate;
        // SAFETY: an IMP is called with the signature the type encoding
        // gives: NSApplicationTerminateReply (NSUInteger) for (id, SEL, id).
        let added = unsafe {
            let imp = std::mem::transmute::<
                extern "C-unwind" fn(&AnyObject, Sel, *mut AnyObject) -> usize,
                Imp,
            >(method);
            objc2::ffi::class_addMethod(
                class as *const AnyClass as *mut AnyClass,
                selector,
                imp,
                c"Q@:@".as_ptr(),
            )
        };
        if added.as_bool() {
            Ok(())
        } else {
            Err("Could not add applicationShouldTerminate:")
        }
    }

    /// The editor is done: AppKit's waiting termination goes on. AppKit
    /// replies to the quit Apple event and ends the app through tao's
    /// `applicationWillTerminate:` (`RunEvent::Exit` in lib.rs); the process
    /// ends here should that not happen.
    pub(super) fn finish<R: Runtime>(app: &AppHandle<R>) {
        if DECIDING.load(Ordering::SeqCst) {
            return;
        }
        let end = || {
            crate::window_state::flush();
            crate::startup::flush_logs();
            unsafe { libc::_exit(0) };
        };
        std::thread::spawn(move || {
            std::thread::sleep(FINISH_TIMEOUT);
            log::warn!("AppKit did not end the app after the quit; ending it.");
            end();
        });
        let reply = || unsafe {
            let ns_app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
            let () = msg_send![ns_app, replyToApplicationShouldTerminate: Bool::YES];
        };
        if let Err(e) = app.run_on_main_thread(reply) {
            log::warn!("Could not let macOS finish the quit: {e}");
            end();
        }
    }

    /// The quit was cancelled: AppKit's waiting termination is too.
    pub(super) fn cancel<R: Runtime>(app: &AppHandle<R>) {
        if !WAITING.swap(false, Ordering::SeqCst) {
            return;
        }
        let reply = || unsafe {
            let ns_app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
            let () = msg_send![ns_app, replyToApplicationShouldTerminate: Bool::NO];
        };
        if let Err(e) = app.run_on_main_thread(reply) {
            log::warn!("Could not cancel the macOS quit: {e}");
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use objc2::runtime::ClassBuilder;

        /// The added method answers through the Objective-C runtime with the
        /// reply AppKit reads; without a request (no app) it is "now". A
        /// class that already answers is left alone.
        #[test]
        fn adds_should_terminate_to_a_delegate_class_once() {
            let class = ClassBuilder::new(c"RawtonicQuitTestDelegate", class!(NSObject))
                .unwrap()
                .register();
            unsafe {
                add_should_terminate(class).unwrap();
                assert!(add_should_terminate(class).is_err());
                let delegate: *mut AnyObject = msg_send![class, new];
                let responds: Bool =
                    msg_send![delegate, respondsToSelector: sel!(applicationShouldTerminate:)];
                assert!(responds.as_bool());
                let reply: usize = msg_send![delegate, applicationShouldTerminate: std::ptr::null_mut::<AnyObject>()];
                assert_eq!(reply, TERMINATE_NOW);
                assert!(!WAITING.load(Ordering::SeqCst));
                let () = msg_send![delegate, release];
            }
        }
    }
}

/// macOS: the default menu with a Quit that asks the editor to save first
/// (AppKit's predefined Quit terminates without a request to hold).
#[cfg(target_os = "macos")]
pub fn menu<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<tauri::menu::Menu<R>> {
    use tauri::menu::{Menu, MenuItem, MenuItemKind};
    let menu = Menu::default(app)?;
    if let Some(MenuItemKind::Submenu(app_menu)) = menu.items()?.first() {
        let items = app_menu.items()?;
        if let Some(index) = items.iter().rposition(|item| {
            matches!(item, MenuItemKind::Predefined(quit)
                if quit.text().is_ok_and(|text| text.starts_with("Quit")))
        }) {
            app_menu.remove_at(index)?;
            let quit = MenuItem::with_id(
                app,
                QUIT_MENU_ID,
                format!("Quit {}", app.package_info().name),
                true,
                Some("CmdOrCtrl+Q"),
            )?;
            app_menu.insert(&quit, index)?;
        }
    }
    Ok(menu)
}

#[cfg(target_os = "macos")]
pub fn on_menu_event<R: Runtime>(app: &AppHandle<R>, event: tauri::menu::MenuEvent) {
    if event.id() == QUIT_MENU_ID {
        request_quit(app);
    }
}
