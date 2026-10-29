//! Quitting waits for the editor's pending saves. The editor saves recipes
//! shortly after an edit (debounced), so closing the main window, the
//! title bar's close button and Quit (Cmd+Q) first ask the webview
//! (`app-quit-requested`, payload `{requestId}`) to write what is pending;
//! it answers with `quit_saving(requestId)` at once and
//! `finish_quit(requestId, force)` when everything is on disk, or with
//! `cancel_quit(requestId, allowQuitAnyway)` when a save failed.
//!
//! No elapsed time ever ends the app (decision 2026-10-29,
//! QUIT-CONTRACT-DECISION.md): a deadline only ends its request. A webview
//! that does not answer cannot hold the quit forever, so a request the
//! editor did not take within `ACK_TIMEOUT`, and one that made no saving
//! progress for `SAVE_TIMEOUT`, is cancelled: the app stays open, keeps
//! saving what is queued and tells the editor (`app-quit-cancelled`,
//! payload `{requestId, reason}`) to show a keep-open warning. The editor
//! answers `quit_saving` again each time one of its queued photo writes
//! settles (a long batch keeps it saving, a hung write does not).
//!
//! The webview alone ends the app after a request, with `finish_quit`:
//! without `force` only while its request is the pending one (everything was
//! saved); with `force` only after its `cancel_quit(…, true)` was answered
//! with the user's "Quit Anyway" decision, which the request still holds. A
//! request that was cancelled or timed out holds no decision any more, and
//! `cancel_quit(…, false)` ends it. Every call names the request it answers;
//! a late answer to a cancelled, timed-out or replaced request changes
//! nothing, not even after a newer request started. The commands return
//! whether they accepted the call (the request was the pending one).
//!
//! All transitions run under one lock (`QuitState`), so a deadline, a finish
//! and a new request cannot interleave: a stale deadline or finish never
//! ends a newer request or the app. The deadline runs in a thread of its own
//! (`deadline`), which a new, saved or answered request wakes; the main
//! thread only takes the state's lock, so a deadline never waits for it.
//!
//! macOS terminating the app from outside its menu (Dock Quit, log out,
//! restart, shut down) asks the app delegate `applicationShouldTerminate:`,
//! which tao does not implement; `hold_system_terminate` adds it to tao's
//! delegate class (`system_terminate`). It answers "later" and returns into
//! the run-loop; the process ends when the editor finished (`finish_quit`)
//! or an explicit "Quit Anyway". A failed save cancels the termination
//! (`cancel_quit`; a log out then stops, as with any app holding unsaved
//! work), a cancelled or timed-out request too (AppKit answers "cancel";
//! macOS cannot be told "wait longer" again for one termination). When the
//! editor is done the app answers AppKit (`replyToApplicationShouldTerminate:`),
//! which replies to the quit Apple event and ends the app through
//! `RunEvent::Exit`: ending the process without that reply (`_exit` alone)
//! left the sender (AppleScript, the Dock, log out) waiting about half a
//! second for an answer that never came.
//!
//! What lib.rs wires (not part of this module): the three commands replace
//! their request-less forms; a close of the main window still calls
//! `intercept_close` (every close is held, so that a cancelled quit keeps
//! the window; the close's `ExitRequested` follows `CloseRequested`), and
//! lib.rs's own exit (clean-up, restart, `_exit`) runs only once
//! `exit_requested(code)` answered with a code (`None`: the editor saves
//! first). A webview restart calls `request_restart` instead of Tauri's
//! `request_restart` (which ends the app at once); `finish_quit` then quits
//! with `RESTART_EXIT_CODE`, so `RunEvent::ExitRequested` restarts as
//! before.
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Runtime};

/// The webview did not take the request (not loaded, hung).
const ACK_TIMEOUT: Duration = Duration::from_secs(3);
/// Saving made no progress for longer than any recipe write should take.
const SAVE_TIMEOUT: Duration = Duration::from_secs(30);

/// Why a request ended without the app quitting (`app-quit-cancelled`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CancelReason {
    /// The editor did not take the request in time.
    NotAcknowledged,
    /// Saving made no progress in time.
    SavingStalled,
}

impl CancelReason {
    /// The webview's `reason`, which it shows as a keep-open warning.
    fn as_str(self) -> &'static str {
        match self {
            CancelReason::NotAcknowledged => "notAcknowledged",
            CancelReason::SavingStalled => "savingStalled",
        }
    }
}

/// `app-quit-cancelled`: the request `request_id` ended without a quit, so
/// the editor drops that request's guard and says the app stays open.
#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct QuitCancelled {
    request_id: u64,
    reason: &'static str,
}

/// What a transition decides to do outside the lock.
#[derive(Debug, Default, PartialEq)]
struct Effects {
    /// Emit `app-quit-requested` for this request.
    ask: Option<u64>,
    /// Emit `app-quit-cancelled` for this request.
    tell: Option<(u64, CancelReason)>,
    /// The app quits (the request was decided for it).
    quit: bool,
}

impl Effects {
    fn is_noop(&self) -> bool {
        *self == Effects::default()
    }
}

/// The quit request that is open (see the module).
#[derive(Debug, Clone, Copy, PartialEq)]
struct Request {
    id: u64,
    /// The editor took the request (answered `quit_saving`): from here the
    /// save deadline applies instead of the acknowledgement deadline.
    acknowledged: bool,
    /// When it was sent or last made saving progress.
    last: Instant,
    /// A request of its own holds its exit: the restart's (see
    /// `QuitState::request`).
    held: bool,
    /// The user decided "Quit Anyway" for a failed save: only
    /// `finish_quit(id, force = true)` quits for it, and no deadline ends it.
    quit_anyway: bool,
}

impl Request {
    /// When its current deadline runs out.
    fn deadline(&self) -> Instant {
        self.last
            + if self.acknowledged {
                SAVE_TIMEOUT
            } else {
                ACK_TIMEOUT
            }
    }

    fn expired(&self, now: Instant) -> bool {
        !self.held && !self.quit_anyway && now >= self.deadline()
    }
}

/// The one quit state, changed only under `QUIT.lock()` (see the module).
#[derive(Debug, Default)]
struct QuitState {
    /// The request the editor may answer; the id of `app-quit-requested`.
    pending: Option<Request>,
    /// The app is on its way out (started here or by a restart): requests
    /// and cancellations do nothing, only `finish_quit` ends it.
    quitting: bool,
    /// Whether the app's last request was a restart's, which ends the app
    /// with the new instance.
    restart: bool,
}

impl QuitState {
    /// A new quit request, unless one is pending. A restart's ask nothing
    /// (whatever AppKit does, the app ends anyway) and end the app with
    /// `restart`'s code; every request holds the run-loop's exit that began
    /// it (`exit_requested`).
    fn request(&mut self, id: u64, now: Instant, restart: bool) -> Effects {
        if self.pending.is_some() {
            return Effects::default();
        }
        self.restart = restart;
        self.pending = Some(Request {
            id,
            acknowledged: false,
            last: now,
            held: restart,
            quit_anyway: false,
        });
        if restart {
            return Effects::default();
        }
        Effects {
            ask: Some(id),
            ..Effects::default()
        }
    }

    /// The editor took the request (`quit_saving`) or a photo write settled:
    /// progress from `now`.
    fn saving(&mut self, id: u64, now: Instant) -> Effects {
        if let Some(request) = self.pending.as_mut().filter(|request| request.id == id) {
            request.acknowledged = true;
            request.last = now;
        }
        Effects::default()
    }

    /// The editor is done (`finish_quit`, `force`: after "Quit Anyway"):
    /// the app quits for its request.
    fn finish(&mut self, id: u64, force: bool) -> Effects {
        let own = match self.pending {
            Some(request) if request.id == id => !force || request.quit_anyway,
            _ => false,
        };
        if !own {
            return Effects::default();
        }
        self.pending = None;
        self.quitting = true;
        Effects {
            quit: true,
            ..Effects::default()
        }
    }

    /// The editor does not quit (`cancel_quit`): `allow_quit_anyway` waits
    /// for the user with this request (its `finish_quit(id, true)` quits);
    /// otherwise the request ends.
    fn cancel(&mut self, id: u64, allow_quit_anyway: bool) -> Effects {
        let Some(request) = self.pending.as_mut().filter(|request| request.id == id) else {
            return Effects::default();
        };
        if allow_quit_anyway {
            request.quit_anyway = true;
            return Effects::default();
        }
        self.pending = None;
        Effects::default()
    }

    /// Ends the pending request without the app quitting (the editor cannot
    /// answer it).
    fn end(&mut self) -> Effects {
        self.pending = None;
        Effects::default()
    }

    /// The request's deadline ran out at `now`: it ends unless something
    /// changed it since (a late `quit_saving` moved the deadline, a finish
    /// or cancel dropped it, a new request took its place). Never quits.
    fn expired(&mut self, now: Instant) -> Effects {
        let Some(request) = self.pending.filter(|request| request.expired(now)) else {
            return Effects::default();
        };
        self.pending = None;
        self.restart = false;
        Effects {
            tell: Some((
                request.id,
                if request.acknowledged {
                    CancelReason::SavingStalled
                } else {
                    CancelReason::NotAcknowledged
                },
            )),
            ..Effects::default()
        }
    }

    /// The run-loop asks to leave (lib.rs: a close, a restart). It leaves
    /// when no quit request waits. A restart's own code ends the app with
    /// the new instance, requested after `finish_quit`; any other exit is a
    /// close that the request made before it holds (else the close goes on,
    /// e.g. a restart the editor cancelled). Returns the exit code, or
    /// `None` while a request waits.
    fn exit_requested(&mut self, code: Option<i32>, held: bool) -> Option<i32> {
        if self.quitting {
            self.pending = None;
            return Some(code.unwrap_or(0));
        }
        if self.restart && code.is_some() && !held {
            self.pending = None;
            return code;
        }
        if held && self.pending.is_some() {
            return None;
        }
        self.pending = None;
        self.restart = false;
        Some(0)
    }
}

fn lock() -> MutexGuard<'static, QuitState> {
    QUIT.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The quit state (see the module).
static QUIT: Mutex<QuitState> = Mutex::new(QuitState {
    pending: None,
    quitting: false,
    restart: false,
});
/// Id of the latest request.
static NEXT_REQUEST: AtomicU64 = AtomicU64::new(0);
/// Whether the run-loop's exit began with a request (window close,
/// restart): it then waits for the editor.
static EXIT_HELD: AtomicBool = AtomicBool::new(false);

/// A request ended without a quit: the close it was made for stays open
/// (the run-loop's exit is the one that close kept, and must not end the app
/// once it comes).
fn take_back_exit() {
    EXIT_HELD.store(false, Ordering::SeqCst);
}

/// A close of the main window: returns whether it must be held while the
/// webview saves (the close then becomes a quit request).
pub fn intercept_close<R: Runtime>(app: &AppHandle<R>) -> bool {
    request_quit(app);
    // The window stays open (`prevent_close`) until the app quits.
    true
}

/// Asks the webview to save what is pending and quit (see the module): the
/// Quit menu item, Cmd+Q, and on platforms without `exit_requested` closes.
/// The request holds the run-loop's next exit (`exit_requested`).
pub fn request_quit<R: Runtime>(app: &AppHandle<R>) {
    start_request(app, false);
}

/// A restart: the editor is asked to save, and the app restarts once it
/// answers (`finish_quit`); its answer has no deadline.
pub fn request_restart<R: Runtime>(app: &AppHandle<R>) {
    start_request(app, true);
}

/// Asks the editor; it holds the run-loop's next exit (`exit_requested`).
fn start_request<R: Runtime>(app: &AppHandle<R>, restart: bool) {
    // The close or restart this request is made for asks to leave: its exit
    // waits for the request (`exit_requested`).
    EXIT_HELD.store(true, Ordering::SeqCst);
    let id = NEXT_REQUEST.fetch_add(1, Ordering::SeqCst) + 1;
    let effects = lock().request(id, Instant::now(), restart);
    wake(app);
    run(app, effects);
}

/// Emits the request too (only one that nothing overtook meanwhile).
fn emit_requested<R: Runtime>(app: &AppHandle<R>, id: u64) {
    let state = lock().clone();
    if state.quitting || state.pending.is_some_and(|request| request.id != id) {
        return;
    }
    let asked = app.emit_to(
        "main",
        "app-quit-requested",
        serde_json::json!({ "requestId": id }),
    );
    if let Err(e) = asked {
        // Nobody can answer a request the editor did not get: it ends, and
        // the close it was made for stays open (the app never quits by
        // itself).
        log::warn!("Could not ask the editor to save before quitting: {e}");
        let effects = lock().end();
        take_back_exit();
        run(app, effects);
    }
}

/// Tells the editor that request `id` ended without a quit (and why); a
/// termination AppKit waits for is cancelled with it, and the close taken
/// back.
fn emit_ended<R: Runtime>(app: &AppHandle<R>, id: u64, reason: CancelReason) {
    take_back_exit();
    #[cfg(target_os = "macos")]
    system_terminate::cancel(app);
    let told = app.emit_to(
        "main",
        "app-quit-cancelled",
        QuitCancelled {
            request_id: id,
            reason: reason.as_str(),
        },
    );
    if let Err(e) = told {
        log::warn!("Could not tell the editor that the quit was cancelled: {e}");
    }
}

/// Does what a transition decided, outside the lock.
fn run<R: Runtime>(app: &AppHandle<R>, effects: Effects) {
    if let Some(id) = effects.ask {
        emit_requested(app, id);
    }
    if let Some((id, reason)) = effects.tell {
        emit_ended(app, id, reason);
    }
    if effects.quit {
        exit(app);
    }
}

/// Ends the app now that the request is decided for it.
fn exit<R: Runtime>(app: &AppHandle<R>) {
    // AppKit waits for its termination: let it go on (system_terminate).
    #[cfg(target_os = "macos")]
    if system_terminate::WAITING.load(Ordering::SeqCst) {
        system_terminate::finish(app);
        return;
    }
    // ExitRequested with a code: lib.rs ends the process at once.
    let code = if lock().restart {
        tauri::RESTART_EXIT_CODE
    } else {
        0
    };
    app.exit(code);
}

/// The webview took the quit request and is saving (again: a write settled):
/// returns whether the request was the pending one.
#[tauri::command]
pub(crate) fn quit_saving(app_handle: AppHandle, request_id: u64) -> bool {
    let now = Instant::now();
    let mut state = lock();
    let accepted = state
        .pending
        .is_some_and(|request| request.id == request_id);
    let effects = state.saving(request_id, now);
    drop(state);
    wake(&app_handle);
    run(&app_handle, effects);
    accepted
}

/// Everything is saved (or the user quits anyway): quit now. Returns
/// whether the app quits.
#[tauri::command]
pub(crate) fn finish_quit(app_handle: AppHandle, request_id: u64, force: bool) -> bool {
    let effects = lock().finish(request_id, force);
    let accepted = effects.quit;
    wake(&app_handle);
    run(&app_handle, effects);
    accepted
}

/// The editor does not quit (a save failed): the app stays open, and with
/// `allow_quit_anyway` the request waits for the user's decision. Returns
/// whether the request was the pending one.
#[tauri::command]
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub(crate) fn cancel_quit(app_handle: AppHandle, request_id: u64, allow_quit_anyway: bool) -> bool {
    let mut state = lock();
    let accepted = state
        .pending
        .is_some_and(|request| request.id == request_id);
    let effects = if accepted {
        state.cancel(request_id, allow_quit_anyway)
    } else {
        Effects::default()
    };
    let restart = state.restart;
    drop(state);
    if accepted && !allow_quit_anyway {
        // A restart the editor cancelled does not happen.
        if restart {
            take_back_exit();
        }
        #[cfg(target_os = "macos")]
        system_terminate::cancel(&app_handle);
    }
    wake(&app_handle);
    run(&app_handle, effects);
    accepted
}

/// Ends the pending request when its deadline ran out: the app stays open
/// (see the module). Returns whether one ended.
fn expire<R: Runtime>(app: &AppHandle<R>, now: Instant) -> bool {
    let effects = lock().expired(now);
    let ended = effects.tell.is_some();
    run(app, effects);
    ended
}

/// The run-loop asks to leave (lib.rs `RunEvent::ExitRequested`): returns
/// the exit code to end with, or `None` to wait for the editor. A close or
/// restart request holds it until it was decided; any other exit goes on.
pub fn exit_requested(code: Option<i32>) -> Option<i32> {
    let held = EXIT_HELD.swap(false, Ordering::SeqCst);
    lock().exit_requested(code, held)
}

/// Wakes the deadline (a request was made, saved or answered: work).
fn wake<R: Runtime>(app: &AppHandle<R>) {
    deadline::wake(app);
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

/// The deadline of a waiting request: it ends the request when it runs out
/// (`expire`) and never quits. `wake` starts it and ends its wait; a
/// request that is saved or answered wakes it again.
#[cfg_attr(target_os = "macos", allow(dead_code))]
mod deadline {
    //! One thread runs the deadline: while a request waits, it ends the
    //! request when that runs out (`expire`); after the last one it lingers
    //! `LINGER`, since the webview's `finish_quit` may still be on its way
    //! (a close whose exit the request held). The main thread never waits on
    //! it: `quit_saving`, `finish_quit` and `cancel_quit` only take the lock
    //! (see the module).
    use super::{expire, lock};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
    use std::time::{Duration, Instant};
    use tauri::{AppHandle, Runtime};

    /// A thread's wait before it looks at the state again.
    const SLICE: Duration = Duration::from_millis(25);
    /// How long the thread stays after the last request ended.
    const LINGER: Duration = Duration::from_secs(1);

    /// Woken: the state changed (a request was made, saved or answered).
    static WAKE: Condvar = Condvar::new();
    static WOKEN: Mutex<bool> = Mutex::new(false);
    /// A thread runs.
    static RUNNING: AtomicBool = AtomicBool::new(false);
    /// The thread's app (set by its first `wake`, on the main thread).
    static APP: Mutex<Option<AppHandle>> = Mutex::new(None);

    fn woken() -> MutexGuard<'static, bool> {
        WOKEN.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Ends the thread's wait and starts it when it does not run.
    pub(super) fn wake<R: Runtime>(app: &AppHandle<R>) {
        {
            let mut recorded = APP.lock().unwrap_or_else(PoisonError::into_inner);
            if recorded.is_none() {
                *recorded = Some(app.clone());
            }
        }
        *woken() = true;
        WAKE.notify_all();
        if RUNNING.swap(true, Ordering::SeqCst) {
            return;
        }
        std::thread::spawn(move || {
            run();
            RUNNING.store(false, Ordering::SeqCst);
        });
    }

    /// Ends the pending request at `now` if its deadline ran out.
    fn check(now: Instant) {
        let app = APP.lock().unwrap_or_else(PoisonError::into_inner).clone();
        if let Some(app) = app {
            let _ = expire(&app, now);
        }
    }

    /// Sleeps up to `time`, or until woken.
    fn pause(time: Duration) {
        let mut woken = woken();
        if !*woken {
            (woken, _) = WAKE
                .wait_timeout(woken, time)
                .unwrap_or_else(PoisonError::into_inner);
        }
        *woken = false;
    }

    /// `expire` at each deadline; `LINGER` after the last request.
    fn run() {
        let mut idle_since = None;
        loop {
            let start = Instant::now();
            match lock().pending {
                Some(request) if request.expired(start) => check(start),
                Some(_) => {
                    idle_since = None;
                    pause(SLICE);
                }
                None => {
                    let idle = *idle_since.get_or_insert(start);
                    if start.duration_since(idle) >= LINGER {
                        return;
                    }
                    pause(SLICE);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(seconds: u64) -> Instant {
        Instant::now() + Duration::from_secs(seconds)
    }

    /// A state after request `id` was asked and the editor is saving.
    fn saving(id: u64) -> QuitState {
        let mut state = QuitState::default();
        assert_eq!(state.request(id, t(0), false).ask, Some(id));
        state.saving(id, t(0));
        state
    }

    fn told(id: u64, reason: CancelReason) -> Effects {
        Effects {
            tell: Some((id, reason)),
            ..Effects::default()
        }
    }

    fn quits() -> Effects {
        Effects {
            quit: true,
            ..Effects::default()
        }
    }

    #[test]
    fn a_request_is_asked_once_and_further_requests_are_ignored() {
        let mut state = QuitState::default();
        assert_eq!(state.request(1, t(0), false).ask, Some(1));
        assert!(state.request(2, t(1), false).is_noop());
        assert_eq!(state.pending.map(|request| request.id), Some(1));
        // A restart's request is not asked (the app ends for it anyway).
        let mut state = QuitState::default();
        assert!(state.request(1, t(0), true).is_noop());
    }

    #[test]
    fn the_acknowledgement_deadline_does_not_wait_for_saving_progress() {
        let mut state = QuitState::default();
        state.request(1, t(0), false);
        // Saving progress of another request is no answer.
        assert!(state.saving(7, t(2)).is_noop());
        assert!(
            state
                .expired(t(0) + ACK_TIMEOUT - Duration::from_millis(1))
                .is_noop()
        );
        // The save deadline does not apply before the answer.
        assert_eq!(
            state.expired(t(0) + ACK_TIMEOUT),
            told(1, CancelReason::NotAcknowledged)
        );
        assert!(state.pending.is_none() && !state.quitting);
    }

    #[test]
    fn the_save_deadline_replaces_the_acknowledgement_deadline() {
        let mut state = saving(1);
        assert!(state.expired(t(0) + ACK_TIMEOUT).is_noop());
        assert!(
            state
                .expired(t(0) + SAVE_TIMEOUT - Duration::from_millis(1))
                .is_noop()
        );
        assert_eq!(
            state.expired(t(0) + SAVE_TIMEOUT),
            told(1, CancelReason::SavingStalled)
        );
        assert!(state.pending.is_none() && !state.quitting);
    }

    #[test]
    fn each_settled_photo_write_extends_the_save_deadline() {
        let mut state = saving(1);
        // One photo from 0 to 70 s takes longer than the deadline, but each
        // settled write moves it.
        for at in [20, 45, 70] {
            assert!(state.expired(t(at)).is_noop());
            assert!(state.saving(1, t(at)).is_noop());
            assert!(
                state
                    .expired(t(at) + SAVE_TIMEOUT - Duration::from_secs(1))
                    .is_noop()
            );
        }
        assert_eq!(
            state.expired(t(70) + SAVE_TIMEOUT),
            told(1, CancelReason::SavingStalled)
        );
    }

    #[test]
    fn a_timed_out_request_never_quits_and_late_answers_change_nothing() {
        let mut state = saving(1);
        assert_eq!(
            state.expired(t(0) + SAVE_TIMEOUT),
            told(1, CancelReason::SavingStalled)
        );
        // A stalled write settles later: no new request, no quit.
        assert!(state.saving(1, t(31)).is_noop());
        assert!(state.expired(t(100)).is_noop());
        assert!(state.finish(1, false).is_noop());
        assert!(state.finish(1, true).is_noop());
        assert!(state.cancel(1, true).is_noop());
        assert!(!state.quitting);
    }

    #[test]
    fn a_stale_deadline_cannot_end_a_newer_request() {
        let mut state = saving(1);
        // Request 1 times out; request 2 starts and is being saved.
        assert!(state.expired(t(30)).tell.is_some());
        assert_eq!(state.request(2, t(31), false).ask, Some(2));
        state.saving(2, t(31));
        // Request 1's answers and deadlines arriving late end nothing.
        assert!(state.expired(t(30) + SAVE_TIMEOUT).is_noop());
        assert!(state.saving(1, t(32)).is_noop());
        assert!(state.finish(1, false).is_noop());
        assert!(state.finish(1, true).is_noop());
        assert!(state.cancel(1, true).is_noop());
        assert!(!state.quitting);
        assert_eq!(state.pending.map(|request| request.id), Some(2));
        // Request 2's own deadline still applies.
        assert_eq!(
            state.expired(t(31) + SAVE_TIMEOUT),
            told(2, CancelReason::SavingStalled)
        );
    }

    #[test]
    fn a_quit_saving_that_moved_the_deadline_keeps_the_request_alive() {
        let mut state = saving(1);
        // The deadline fired at 30 s, but a write settled at 29 s (the
        // wake-up saw the old deadline): progress wins.
        state.saving(1, t(29));
        assert!(state.expired(t(30)).is_noop());
        assert_eq!(
            state.expired(t(29) + SAVE_TIMEOUT),
            told(1, CancelReason::SavingStalled)
        );
    }

    #[test]
    fn a_normal_finish_quits_only_for_the_pending_request() {
        let mut state = saving(1);
        assert!(state.finish(2, false).is_noop());
        assert!(state.finish(2, true).is_noop());
        assert!(state.finish(1, true).is_noop(), "force needs a decision");
        assert_eq!(state.finish(1, false), quits());
        assert!(state.quitting);
        assert!(state.finish(1, false).is_noop(), "once");
        assert!(state.request(2, t(1), false).is_noop());
        assert!(!state.quitting, "a request cannot undo it");
    }

    #[test]
    fn quit_anyway_authorizes_this_request_only() {
        let mut state = saving(1);
        assert!(state.cancel(1, true).is_noop());
        assert_eq!(state.pending.map(|request| request.id), Some(1));
        // The user's "Quit Anyway" ends the app; no deadline ends it first.
        assert!(state.expired(t(10_000)).is_noop());
        assert_eq!(state.finish(1, true), quits());

        // The decision does not outlive its request.
        let mut state = saving(1);
        state.cancel(1, true);
        assert!(state.end().is_noop());
        assert!(state.pending.is_none());
        assert_eq!(state.request(2, t(1), false).ask, Some(2));
        assert!(!state.finish(1, true).quit);

        // Nor does one request hold another's decision.
        let mut state = saving(3);
        state.cancel(3, true);
        assert!(state.finish(4, true).is_noop());
        assert!(!state.quitting);
        assert!(state.finish(3, true).quit);
    }

    #[test]
    fn cancel_without_quit_anyway_ends_the_request() {
        let mut state = saving(1);
        assert!(state.cancel(1, false).is_noop());
        assert!(state.pending.is_none());
        assert!(state.finish(1, false).is_noop());
        assert!(state.finish(1, true).is_noop());
        assert!(state.expired(t(10_000)).is_noop());
        // The user may try again.
        assert_eq!(state.request(2, t(1), false).ask, Some(2));
    }

    #[test]
    fn keep_open_after_a_failed_save_quits_nothing() {
        let mut state = saving(1);
        // Failed: the prompt waits for the user, who keeps the app open.
        state.cancel(1, true);
        state.cancel(1, false);
        assert!(state.finish(1, true).is_noop());
        assert!(!state.quitting);
    }

    #[test]
    fn a_restart_s_request_waits_without_a_deadline() {
        let mut state = QuitState::default();
        assert!(state.request(1, t(0), true).is_noop(), "not asked");
        assert!(state.expired(t(10_000)).is_noop());
        assert!(state.cancel(1, true).is_noop());
        assert!(state.finish(1, true).is_noop());
        assert_eq!(state.finish(1, false), quits());
        assert!(state.restart);
    }

    #[test]
    fn a_close_holds_its_exit_only_while_its_request_waits() {
        let mut state = saving(1);
        state.exit_requested(None, false);
        assert_eq!(state.exit_requested(None, true), None);
        assert_eq!(state.finish(1, false), quits());
        assert_eq!(state.exit_requested(None, false), Some(0));

        // A request that ended without a quit: its close goes on.
        let mut state = saving(1);
        state.exit_requested(None, false);
        assert!(state.cancel(1, false).is_noop());
        assert_eq!(state.exit_requested(None, true), Some(0));
        assert!(state.pending.is_none());
    }

    #[test]
    fn a_restart_ends_with_the_new_instance_once_the_editor_saved() {
        let mut state = QuitState::default();
        state.request(1, t(0), true);
        state.exit_requested(Some(tauri::RESTART_EXIT_CODE), false);
        assert_eq!(
            state.exit_requested(Some(tauri::RESTART_EXIT_CODE), true),
            None
        );
        assert!(state.finish(1, false).quit);
        assert_eq!(
            state.exit_requested(Some(tauri::RESTART_EXIT_CODE), false),
            Some(tauri::RESTART_EXIT_CODE)
        );
        // A restart the editor cancelled does not happen.
        let mut state = QuitState::default();
        state.request(1, t(0), true);
        state.cancel(1, false);
        state.exit_requested(Some(tauri::RESTART_EXIT_CODE), false);
        assert_eq!(
            state.exit_requested(Some(tauri::RESTART_EXIT_CODE), true),
            Some(0)
        );
    }

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
    use super::{lock, request_quit};
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
    const TERMINATE_CANCEL: usize = 0;

    /// Starts the quit request (`request_quit` with the app's handle).
    static REQUEST: OnceLock<Box<dyn Fn() + Send + Sync>> = OnceLock::new();
    /// AppKit waits for `replyToApplicationShouldTerminate:`.
    pub(super) static WAITING: AtomicBool = AtomicBool::new(false);
    /// A request was made for the termination AppKit asked about.
    static PREPARED: AtomicBool = AtomicBool::new(false);

    extern "C-unwind" fn should_terminate(
        _this: &AnyObject,
        _cmd: Sel,
        _sender: *mut AnyObject,
    ) -> usize {
        let Some(request) = REQUEST.get() else {
            return TERMINATE_NOW;
        };
        let prepared = PREPARED.swap(false, Ordering::SeqCst);
        if lock().quitting {
            return TERMINATE_NOW;
        }
        WAITING.store(true, Ordering::SeqCst);
        log::info!("macOS asked to quit; saving pending edits first.");
        // A close's quit request (`request_quit`) already waits.
        if !prepared {
            request();
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
        if !WAITING.swap(false, Ordering::SeqCst) {
            return;
        }
        let end = || {
            crate::contribute::clean_exit();
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

    /// The quit was cancelled (a failed save, a request that ended): AppKit's
    /// waiting termination is cancelled too.
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
            let quit = quit_item(app, &format!("Quit {}", app.package_info().name))?;
            app_menu.insert(&quit, index)?;
        }
    }
    if let Some(MenuItemKind::Submenu(help)) = menu.get(tauri::menu::HELP_SUBMENU_ID) {
        let report =
            MenuItem::with_id(app, REPORT_MENU_ID, "Report a Problem…", true, None::<&str>)?;
        help.append(&report)?;
    }
    Ok(menu)
}

/// The Quit item that asks the editor to save first (also in `app_menu`).
#[cfg(target_os = "macos")]
pub fn quit_item<R: Runtime>(
    app: &AppHandle<R>,
    label: &str,
) -> tauri::Result<tauri::menu::MenuItem<R>> {
    tauri::menu::MenuItem::with_id(app, QUIT_MENU_ID, label, true, Some("CmdOrCtrl+Q"))
}

#[cfg(target_os = "macos")]
pub fn on_menu_event<R: Runtime>(app: &AppHandle<R>, event: tauri::menu::MenuEvent) {
    if event.id() == QUIT_MENU_ID {
        request_quit(app);
    } else if event.id() == REPORT_MENU_ID {
        let _ = app.emit_to("main", REPORT_EVENT, ());
    }
}
