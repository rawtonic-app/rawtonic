//! Quitting waits for the editor's pending saves. The editor saves recipes
//! shortly after an edit (debounced), so closing the main window, the
//! title bar's close button and Quit (Cmd+Q) first ask the webview
//! (`app-quit-requested`, payload `{requestId}`) to write what is pending;
//! it answers with `quit_saving(requestId)` at once and
//! `finish_quit(requestId, false)` when everything is on disk, or with
//! `cancel_quit(requestId, allowQuitAnyway)` when a save failed.
//!
//! No elapsed time ends the app (decision 2026-10-29,
//! QUIT-CONTRACT-DECISION.md). A request the editor did not take within
//! `ACK_TIMEOUT`, or that made no saving progress for `SAVE_TIMEOUT`, is
//! cancelled instead: the app stays open, keeps saving what is queued, and
//! the editor is told (`app-quit-cancelled`, payload
//! `{requestId, reason}`) to show a keep-open warning. The editor answers
//! `quit_saving` again each time one of its queued photo writes settles (a
//! long batch keeps it saving, a hung write does not).
//!
//! The app quits only when the editor says so for its own request: a normal
//! `finish_quit` while it is saving, or a forced one after the user chose
//! "Quit Anyway". All transitions run under one lock (`QuitState`), which
//! also owns the request ids, so an answer, a deadline or a new request
//! cannot interleave. Every call names its request; an answer to a
//! cancelled or replaced request changes nothing, not even after a newer
//! request started. The commands return whether they accepted the call.
//!
//! macOS terminating the app from outside its menu (Dock Quit, log out,
//! restart, shut down) asks the app delegate `applicationShouldTerminate:`,
//! which tao does not implement; `hold_system_terminate` adds it to tao's
//! delegate class (`system_terminate`). It answers "later" and runs the same
//! request; the process ends when the editor finished. A cancellation, also
//! one with `allowQuitAnyway` (the user then decides as after Cmd+Q),
//! answers AppKit "cancel": a log out stops, as with any app holding
//! unsaved work. When the editor is done the app answers AppKit
//! (`replyToApplicationShouldTerminate:`), which replies to the quit Apple
//! event and ends the app through `RunEvent::Exit`: ending the process
//! without that reply (`_exit` alone) left the sender (AppleScript, the
//! Dock, log out) waiting about half a second for an answer that never came.
use std::sync::atomic::Ordering;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager, Runtime};

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

/// `app-quit-cancelled`: request `request_id` ended without a quit; the
/// editor drops that request's guard (only if it is still the active one)
/// and shows that the app stays open.
#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct QuitCancelled {
    request_id: u64,
    reason: &'static str,
}

/// Where the one quit request is (see the module). Every phase but `Idle`
/// is a live request; only `Quitting` ends the app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// No request.
    Idle,
    /// Asked (`app-quit-requested`); the editor has not answered yet. The
    /// acknowledgement deadline runs from `at`.
    WaitingAck { id: u64, at: Instant },
    /// The editor saves; its last `quit_saving` (or the request) was `last`.
    Saving { id: u64, last: Instant },
    /// A save failed and `cancel_quit(id, true)` armed the user's "Quit
    /// Anyway" decision: only `finish_quit(id, true)` ends the app, and no
    /// deadline ends the request.
    Decision { id: u64 },
    /// The app is on its way out.
    Quitting,
}

impl Phase {
    /// The id of the request this phase is (not `Quitting`, which has
    /// none).
    fn live_id(self) -> Option<u64> {
        match self {
            Phase::Idle | Phase::Quitting => None,
            Phase::WaitingAck { id, .. } | Phase::Saving { id, .. } | Phase::Decision { id } => {
                Some(id)
            }
        }
    }

    /// Saves or waits for the acknowledgement: a normal finish may quit.
    fn is_saving(self, id: u64) -> bool {
        match self {
            Phase::WaitingAck { id: other, .. } | Phase::Saving { id: other, .. } => other == id,
            _ => false,
        }
    }

    /// When the deadline running from now ends the request: the
    /// acknowledgement deadline until the editor answered, the save
    /// deadline after (a "Quit Anyway" decision has none).
    fn deadline(self) -> Option<Instant> {
        match self {
            Phase::WaitingAck { at, .. } => Some(at + ACK_TIMEOUT),
            Phase::Saving { last, .. } => Some(last + SAVE_TIMEOUT),
            Phase::Idle | Phase::Decision { .. } | Phase::Quitting => None,
        }
    }
}

/// The one quit state: the generation and the phase, changed only under
/// `QUIT.lock()` (see the module).
#[derive(Debug)]
struct QuitState {
    /// The id of the latest request; every transition that ends or replaces
    /// a request takes a new one (an older `app-quit-requested` payload
    /// stays unique). `0` before the first request.
    generation: u64,
    phase: Phase,
}

impl QuitState {
    const fn new() -> QuitState {
        QuitState {
            generation: 0,
            phase: Phase::Idle,
        }
    }

    /// The id of the next request.
    fn next_id(&mut self) -> u64 {
        self.generation += 1;
        self.generation
    }

    /// A new request from the menu, Cmd+Q or a close. It replaces an idle
    /// request or a "Quit Anyway" decision (its authorization is gone, the
    /// new id invalidates its answers); requests that are being asked or
    /// are saving, and quitting, keep theirs. Returns the requested id if
    /// the editor must be asked.
    fn begin(&mut self, now: Instant) -> Option<u64> {
        if !matches!(self.phase, Phase::Idle | Phase::Decision { .. }) {
            return None;
        }
        let id = self.next_id();
        self.phase = Phase::WaitingAck { id, at: now };
        Some(id)
    }

    /// `quit_saving`: the editor took the request, or a photo write
    /// settled. Only its own request that is waiting or saving makes
    /// progress; returns whether the call was accepted.
    fn saving(&mut self, id: u64, now: Instant) -> bool {
        if !self.phase.is_saving(id) {
            return false;
        }
        self.phase = Phase::Saving { id, last: now };
        true
    }

    /// `finish_quit`: everything is saved (`force` false: only a request
    /// that is waiting or saving; force true: only the "Quit Anyway"
    /// decision of this request). True: the app quits.
    fn finish(&mut self, id: u64, force: bool) -> bool {
        let allowed = if force {
            self.phase == Phase::Decision { id }
        } else {
            self.phase.is_saving(id)
        };
        if !allowed {
            return false;
        }
        self.next_id();
        self.phase = Phase::Quitting;
        true
    }

    /// `cancel_quit`, after a failed save: with `allow_quit_anyway` this
    /// request waits for the user's "Quit Anyway" decision, without it the
    /// request ends. Only a live request of this id; returns whether the
    /// call was accepted.
    fn cancel(&mut self, id: u64, allow_quit_anyway: bool) -> bool {
        if self.phase.live_id() != Some(id) {
            return false;
        }
        self.next_id();
        self.phase = if allow_quit_anyway {
            Phase::Decision { id }
        } else {
            Phase::Idle
        };
        true
    }

    /// The deadline of request `id` ran out at `now` (evaluated only while
    /// `id` is the waiting or saving request, see `expire`): it ends, and
    /// the app stays open. `None`: nothing to tell the editor.
    fn timed_out(&mut self, id: u64, now: Instant) -> Option<CancelReason> {
        let reason = match self.phase {
            Phase::WaitingAck { id: other, .. } if other == id => CancelReason::NotAcknowledged,
            Phase::Saving { id: other, .. } if other == id => CancelReason::SavingStalled,
            _ => return None,
        };
        if self.phase.deadline().is_none_or(|deadline| now < deadline) {
            return None;
        }
        self.next_id();
        self.phase = Phase::Idle;
        Some(reason)
    }

    /// The editor could not be asked (the event did not go out): its
    /// request ends, and nothing quits.
    fn abort(&mut self, id: u64) -> bool {
        if !matches!(self.phase, Phase::WaitingAck { id: other, .. } if other == id) {
            return false;
        }
        self.next_id();
        self.phase = Phase::Idle;
        true
    }
}

fn lock() -> MutexGuard<'static, QuitState> {
    QUIT.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The quit state (see the module).
static QUIT: Mutex<QuitState> = Mutex::new(QuitState::new());

/// Whether the app is on its way out (`app_quit::is_quitting`; a
/// `Quitting` can never be undone, so this only reads the truth).
pub fn is_quitting() -> bool {
    lock().phase == Phase::Quitting
}

/// The custom Quit menu item (macOS) that replaces AppKit's predefined one.
#[cfg(target_os = "macos")]
const QUIT_MENU_ID: &str = "rawtonic-quit";

/// "Report a Problem…" in the Help menu (macOS); the webview opens the
/// report (src/components/contribute/ContributionHost.tsx).
#[cfg(target_os = "macos")]
const REPORT_MENU_ID: &str = "rawtonic-report-problem";
#[cfg(target_os = "macos")]
const REPORT_EVENT: &str = "report-problem-requested";

/// A close of the main window: returns whether it must be held while the
/// webview saves (the close then becomes a quit request). Once the app
/// quits, it closes.
pub fn intercept_close<R: Runtime>(app: &AppHandle<R>) -> bool {
    if is_quitting() {
        return false;
    }
    request_quit(app);
    true
}

/// Asks the webview to save what is pending and quit (see the module).
pub fn request_quit<R: Runtime>(app: &AppHandle<R>) {
    let Some(id) = lock().begin(Instant::now()) else {
        return;
    };
    let asked = app.emit_to(
        "main",
        "app-quit-requested",
        serde_json::json!({ "requestId": id }),
    );
    if let Err(e) = asked {
        log::warn!("Could not ask the editor to save before quitting: {e}");
        // Nobody can answer it: the request ends, and the app stays open.
        if lock().abort(id) {
            run_deadline(app, id);
        }
    } else {
        run_deadline(app, id);
    }
}

/// Watches request `id`'s deadline in the existing sleep loop (see
/// `expire`). It ends only its own request.
fn run_deadline<R: Runtime>(app: &AppHandle<R>, id: u64) {
    let app = app.clone();
    std::thread::spawn(move || expire(&app, id));
}

/// Sleeps request `id`'s deadline and cancels it when it runs out: the app
/// stays open and the editor is told why. It never exits, and never touches
/// another request.
fn expire<R: Runtime>(app: &AppHandle<R>, id: u64) {
    // Sleeps in slices: each one is capped to the time left, so a clock
    // step never turns into a spin, and each wake-up looks at the state
    // again (a `quit_saving` moved the save deadline).
    let slice = Duration::from_millis(20);
    loop {
        let phase = lock().phase;
        let Some(deadline) = phase.deadline().filter(|_| phase.live_id() == Some(id)) else {
            return;
        };
        let now = Instant::now();
        if now >= deadline {
            if let Some(reason) = lock().timed_out(id, now) {
                log::warn!(
                    "The quit request ended without quitting: {}",
                    reason.as_str()
                );
                emit_cancelled(app, id, reason);
            }
            return;
        }
        std::thread::sleep(deadline.saturating_duration_since(now).min(slice));
    }
}

/// Tells the editor that request `id` ended without a quit; a termination
/// AppKit waits for is cancelled with it.
fn emit_cancelled<R: Runtime>(app: &AppHandle<R>, id: u64, reason: CancelReason) {
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

/// The webview took the quit request and is saving (again: a write
/// settled): returns whether the request was the live one.
#[tauri::command]
pub(crate) fn quit_saving(request_id: u64) -> bool {
    lock().saving(request_id, Instant::now())
}

/// Everything is saved (or the user quits anyway): quit now. Returns
/// whether the app quits.
#[tauri::command]
pub(crate) fn finish_quit(app_handle: AppHandle, request_id: u64, force: bool) -> bool {
    if !lock().finish(request_id, force) {
        return false;
    }
    exit(&app_handle);
    true
}

/// The editor does not quit (a save failed): the app stays open, and with
/// `allow_quit_anyway` only this request's "Quit Anyway" may end it. A
/// termination AppKit waits for is cancelled either way. Returns whether
/// the request was the live one.
#[tauri::command]
pub(crate) fn cancel_quit(app_handle: AppHandle, request_id: u64, allow_quit_anyway: bool) -> bool {
    let accepted = lock().cancel(request_id, allow_quit_anyway);
    if accepted {
        #[cfg(target_os = "macos")]
        system_terminate::cancel(&app_handle);
        #[cfg(not(target_os = "macos"))]
        let _ = &app_handle;
    }
    accepted
}

/// Ends the app now; its phase is `Quitting` already.
fn exit<R: Runtime>(app: &AppHandle<R>) {
    // AppKit waits for its termination: let it go on (system_terminate).
    #[cfg(target_os = "macos")]
    if system_terminate::WAITING.load(Ordering::SeqCst) {
        system_terminate::finish(app);
        return;
    }
    // ExitRequested with a code: lib.rs ends the process at once.
    app.exit(0);
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

    /// One fixed start for every `t(…)` of a test: nothing else yaws.
    fn t(seconds: u64) -> Instant {
        static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
        *START.get_or_init(Instant::now) + Duration::from_secs(seconds)
    }

    /// A request made at `t(0)`; `saving`: the editor answered it.
    fn requested(saving: bool) -> QuitState {
        let mut state = QuitState::new();
        assert_eq!(state.begin(t(0)), Some(1));
        if saving {
            assert!(state.saving(1, t(0)));
        }
        state
    }

    /// A request whose save failed and who waits for "Quit Anyway".
    fn decided() -> QuitState {
        let mut state = requested(true);
        assert!(state.cancel(1, true));
        state
    }

    #[test]
    fn a_new_request_is_not_started_while_one_is_asked_or_saving_or_quitting() {
        let mut state = requested(false);
        assert_eq!(state.begin(t(1)), None);
        assert_eq!(state.phase, Phase::WaitingAck { id: 1, at: t(0) });
        let mut state = requested(true);
        assert_eq!(state.begin(t(1)), None);
        assert_eq!(state.phase, Phase::Saving { id: 1, last: t(0) });
        assert!(state.finish(1, false));
        assert_eq!(state.begin(t(1)), None);
        assert_eq!(state.phase, Phase::Quitting);
    }

    /// The rejected implementation kept the decision and ignored every
    /// newer request; here the new request replaces it: asked, and the old
    /// request's forced finish does not quit.
    #[test]
    fn a_new_request_replaces_a_quit_anyway_decision() {
        let mut state = decided();
        let new = state.begin(t(5));
        assert!(new.is_some(), "a newer request is asked");
        let new = new.unwrap();
        assert_ne!(new, 1, "the new request has its own id");
        assert_eq!(state.phase, Phase::WaitingAck { id: new, at: t(5) });
        // The replaced authorization: no forced quit, and the new request's
        // answers do not start it either.
        assert!(!state.finish(1, true));
        assert!(!state.finish(1, false));
        assert!(!state.cancel(1, false));
        assert!(!state.finish(new, true), "force needs a decision");
        assert_ne!(state.phase, Phase::Quitting);
        assert_eq!(state.phase, Phase::WaitingAck { id: new, at: t(5) });
    }

    /// The rejected implementation's `finish(id, false)` also accepted a
    /// decision (`!force` was always true); here only a forced finish ends
    /// it, and a request without a decision is never forced.
    #[test]
    fn a_normal_finish_never_accepts_a_decision_and_only_a_decision_is_forced() {
        let mut state = decided();
        assert!(!state.finish(1, false), "a normal finish is not a decision");
        assert_eq!(state.phase, Phase::Decision { id: 1 });
        // The user's answer to the prompt: the forced finish quits, once.
        assert!(state.finish(1, true));
        assert_eq!(state.phase, Phase::Quitting);
        assert!(!state.finish(1, true));
        let mut state = requested(true);
        assert!(!state.finish(1, true), "no decision: no forced quit");
        assert_eq!(state.phase, Phase::Saving { id: 1, last: t(0) });
        assert!(state.finish(1, false), "the request that saves quits");
        assert_eq!(state.phase, Phase::Quitting);
        assert!(!state.cancel(1, false), "decided once");
        assert!(!state.finish(1, true), "force has no decision to accept");
    }

    #[test]
    fn only_the_matching_waiting_or_saving_request_is_accepted() {
        let mut state = requested(false);
        assert!(!state.saving(2, t(1)));
        assert!(!state.finish(2, false));
        assert!(!state.cancel(2, false));
        assert_eq!(state.phase, Phase::WaitingAck { id: 1, at: t(0) });
        assert!(state.saving(1, t(1)));
        assert_eq!(state.phase, Phase::Saving { id: 1, last: t(1) });
        assert!(state.finish(1, false));
        // Done: late answers of anyone change nothing.
        assert!(!state.saving(1, t(2)));
        assert!(!state.finish(1, false));
        assert!(!state.finish(1, true));
        assert!(!state.cancel(1, true));
        assert_eq!(state.phase, Phase::Quitting);
    }

    #[test]
    fn a_cancel_of_a_live_request_ends_it_or_arms_its_decision() {
        let mut state = requested(true);
        assert!(!state.cancel(9, true));
        // A save failed: the prompt waits for the user, so does this request
        // (it takes an id of its own: the decision belongs to this one).
        assert!(state.cancel(1, true));
        assert_eq!(state.phase, Phase::Decision { id: 1 });
        assert!(state.cancel(1, true), "asked again while the prompt shows");
        assert_eq!(state.phase, Phase::Decision { id: 1 });
        assert!(!state.finish(1, false));
        // Refused (user keeps the app open): the request ends and quits
        // nothing (AppKit's termination too, see `cancel_quit`).
        assert!(state.cancel(1, false));
        assert_eq!(state.phase, Phase::Idle);
        assert!(!state.finish(1, true));
        assert!(!state.saving(1, t(1)));
        assert!(!state.cancel(1, false), "ended once");
        // A decision ends the same way, true or false.
        let mut state = decided();
        assert!(state.cancel(1, false));
        assert_eq!(state.phase, Phase::Idle);
        assert!(state.begin(t(9)).is_some());
        let mut state = decided();
        assert!(state.cancel(1, true));
        assert_eq!(state.phase, Phase::Decision { id: 1 });
    }

    #[test]
    fn the_acknowledgement_deadline_ends_a_waiting_request_without_quitting() {
        let mut state = requested(false);
        assert_eq!(state.timed_out(7, t(ACK_TIMEOUT.as_secs() + 1)), None);
        assert_eq!(state.timed_out(1, t(ACK_TIMEOUT.as_secs() - 1)), None);
        assert_eq!(
            state.timed_out(1, t(ACK_TIMEOUT.as_secs())),
            Some(CancelReason::NotAcknowledged)
        );
        assert_eq!(state.phase, Phase::Idle);
        // Late answers change nothing.
        assert!(!state.saving(1, t(4)));
        assert!(!state.finish(1, false));
        assert!(!state.cancel(1, true));
        assert_eq!(state.phase, Phase::Idle);
    }

    #[test]
    fn each_settled_photo_write_extends_the_save_deadline() {
        let mut state = requested(true);
        assert_eq!(state.timed_out(1, t(29)), None);
        assert!(state.saving(1, t(29)));
        assert_eq!(state.timed_out(1, t(40)), None);
        assert_eq!(
            state.timed_out(1, t(29 + SAVE_TIMEOUT.as_secs())),
            Some(CancelReason::SavingStalled)
        );
        assert_eq!(state.phase, Phase::Idle);
    }

    #[test]
    fn a_decision_has_no_deadline() {
        let mut state = decided();
        assert_eq!(state.timed_out(1, t(10_000)), None);
        assert_eq!(state.phase, Phase::Decision { id: 1 });
    }

    #[test]
    fn a_stale_deadline_cannot_end_a_newer_request() {
        let mut state = requested(true);
        assert!(state.timed_out(1, t(SAVE_TIMEOUT.as_secs())).is_some());
        let next = state.begin(t(31)).unwrap();
        assert!(state.saving(next, t(31)));
        // Request 1's deadline and answers arriving late.
        assert_eq!(state.timed_out(1, t(31 + SAVE_TIMEOUT.as_secs())), None);
        assert!(!state.saving(1, t(32)));
        assert!(!state.finish(1, false));
        assert!(!state.finish(1, true));
        assert!(!state.cancel(1, true));
        assert_eq!(
            state.phase,
            Phase::Saving {
                id: next,
                last: t(31)
            }
        );
        // Its own deadline still applies.
        assert_eq!(
            state.timed_out(next, t(31 + SAVE_TIMEOUT.as_secs())),
            Some(CancelReason::SavingStalled)
        );
    }

    #[test]
    fn ids_never_repeat_so_an_old_answer_cannot_match_a_new_request() {
        let mut state = requested(true);
        assert!(state.cancel(1, false));
        let next = state.begin(t(2)).unwrap();
        assert!(state.saving(next, t(3)));
        assert!(state.finish(next, false));
        assert_eq!(state.generation, 4);
        assert_eq!(state.phase, Phase::Quitting);
    }

    #[test]
    fn a_request_a_failed_emit_could_not_ask_ends_without_quitting() {
        let mut state = requested(false);
        assert!(!state.abort(2));
        assert!(state.abort(1));
        assert_eq!(state.phase, Phase::Idle);
        assert!(!state.abort(1));
        // Not once the editor answered it.
        let mut state = requested(true);
        assert!(!state.abort(1));
        assert_eq!(state.phase, Phase::Saving { id: 1, last: t(0) });
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
    use super::{is_quitting, request_quit};
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
        if is_quitting() {
            return TERMINATE_NOW;
        }
        WAITING.store(true, Ordering::SeqCst);
        log::info!("macOS asked to quit; saving pending edits first.");
        // Answers AppKit when done (`finish`) or cancels (`cancel`).
        DECIDING.store(true, Ordering::SeqCst);
        request();
        DECIDING.store(false, Ordering::SeqCst);
        // The editor could not be asked: quit at once.
        if is_quitting() {
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
