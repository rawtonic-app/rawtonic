//! Turns between an idle compile, an export and an AI network run on the
//! CPU (G76/COLD2).
//!
//! Under a compile limit (AMD's Windows Vulkan driver) one display variant's
//! compile commits about 4.4 GB in its helper process, and a Subject mask on
//! the CPU about 2.2–2.5 GB above its model in the app. On an integrated
//! GPU both come out of the same 14–16 GB: together with the app they
//! reached 8.25–8.5 GB on a Radeon 680M laptop (G76/WSMOKE20), each alone
//! stays within 6.6 GB. So the two take turns there (`enable`):
//!
//! - a run the photographer asked for (a click on Subject) goes before an
//!   idle compile nobody waits for: the helper stops and compiles again
//!   afterwards;
//! - an idle compile goes before a run made ahead (the masks computed when
//!   a photo opens, a warm-up): the run waits until the compile is done;
//! - a compile a frame or an export waits for and a run asked for are both
//!   what the photographer waits for: whichever came later waits;
//! - a compile a frame or an export waits for does not wait for a run made
//!   ahead that is in progress: it starts beside it, and the runs made
//!   ahead are told to stop (`on_awaited_compile`). WMV8/DVL5: after a lost
//!   GPU device the shown photograph's display variant had to compile again
//!   while the Subject mask computed ahead held its turn with a model load
//!   that did not end, and the multi view stayed soft for 35 s and more;
//! - an export and a run do not overlap either (the app alone reached
//!   7.6 GB with a mask computed ahead beside a JPEG export, WSMOKE20): the
//!   export waits for the run in progress (the app stops one made ahead),
//!   and runs wait for the export.
//!
//! Exports and compiles keep out of each other's way elsewhere
//! (`gpu_processing::wait_for_running_compiles`, the app's memory guard).
//!
//! Nothing is held where `enable` was not called (every other computer).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

/// Who asked for an AI network run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AiRun {
    /// The photographer, and they wait for it.
    Asked,
    /// The app, ahead of a click.
    Ahead,
}

/// How long a compile that found a run in its way counts as waiting: runs
/// made ahead do not start in that time (the idle worker asks again every
/// 250 ms).
const COMPILE_WAITS: Duration = Duration::from_secs(1);

#[derive(Default)]
struct State {
    /// AI runs in progress.
    runs: usize,
    /// Of `runs`, those made ahead (`AiRun::Ahead`).
    ahead_runs: usize,
    /// Asked runs waiting for a compile to end.
    asked_waiting: usize,
    /// An idle compile is in progress.
    compiling: bool,
    /// When a compile last found a run in its way.
    compile_waited: Option<Instant>,
    /// Exports in progress.
    exports: usize,
    /// Exports waiting for the runs in progress to end.
    exports_waiting: usize,
}

/// The turns of one process (`TURNS`); tests make their own.
#[derive(Default)]
struct Turns {
    state: Mutex<State>,
    changed: Condvar,
}

static TURNS: Turns = Turns {
    state: Mutex::new(State {
        runs: 0,
        ahead_runs: 0,
        asked_waiting: 0,
        compiling: false,
        compile_waited: None,
        exports: 0,
        exports_waiting: 0,
    }),
    changed: Condvar::new(),
};
static ENABLED: AtomicBool = AtomicBool::new(false);

/// From now on idle compiles and AI network runs take turns in this
/// process: for an integrated GPU under a compile limit whose AI models run
/// on the CPU.
pub fn enable() {
    if !ENABLED.swap(true, Ordering::SeqCst) {
        log::info!("GPU idle compiles and AI runs on the CPU take turns");
    }
}

fn enabled() -> bool {
    ENABLED.load(Ordering::SeqCst)
}

/// Whether `enable` was called: `ai_run` may wait.
pub fn takes_turns() -> bool {
    enabled()
}

/// An AI network run's turn, held while it lives.
#[must_use = "the run's turn ends when this drops"]
pub struct AiRunTurn(Option<(&'static Turns, AiRun)>);

impl Drop for AiRunTurn {
    fn drop(&mut self) {
        if let Some((turns, kind)) = self.0 {
            turns.run_ended(kind);
        }
    }
}

/// An idle compile's turn, held while it lives.
#[must_use = "the compile's turn ends when this drops"]
pub(crate) struct CompileTurn(Option<&'static Turns>);

impl Drop for CompileTurn {
    fn drop(&mut self) {
        if let Some(turns) = self.0 {
            turns.compile_ended();
        }
    }
}

/// An export's turn, held while it lives.
#[must_use = "the export's turn ends when this drops"]
pub struct ExportTurn(Option<&'static Turns>);

impl Drop for ExportTurn {
    fn drop(&mut self) {
        if let Some(turns) = self.0 {
            turns.export_ended();
        }
    }
}

/// Waits until no AI network run is in progress and takes an export's
/// turn: runs wait while it lives. Stop the runs made ahead first (they
/// can be stopped). Returns at once where turns are not enabled.
pub fn export() -> ExportTurn {
    if !enabled() {
        return ExportTurn(None);
    }
    let started = Instant::now();
    TURNS.export();
    let waited = started.elapsed();
    if waited >= Duration::from_millis(250) {
        log::info!("Export waited {:.1} s for an AI run", waited.as_secs_f64());
    }
    ExportTurn(Some(&TURNS))
}

/// Waits for an AI network run's turn (see the module) and takes it. Call
/// on the thread that runs the network, right before the run. Returns at
/// once where turns are not enabled.
pub fn ai_run(kind: AiRun) -> AiRunTurn {
    if !enabled() {
        return AiRunTurn(None);
    }
    let started = Instant::now();
    TURNS.run(kind);
    let waited = started.elapsed();
    if waited >= Duration::from_millis(250) {
        log::info!(
            "AI run ({kind:?}) waited {:.1} s for an idle shader compile or an export",
            waited.as_secs_f64()
        );
    }
    AiRunTurn(Some((&TURNS, kind)))
}

/// Who is told that a compile a frame or an export waits for starts beside
/// runs made ahead (`on_awaited_compile`).
static AWAITED_COMPILE: std::sync::OnceLock<Box<dyn Fn() + Send + Sync>> =
    std::sync::OnceLock::new();

/// Calls `stop_ahead_runs` when a compile a frame or an export waits for
/// starts beside AI runs made ahead (see the module): the app stops them,
/// as it does for an export. Set once.
pub fn on_awaited_compile(stop_ahead_runs: impl Fn() + Send + Sync + 'static) {
    let _ = AWAITED_COMPILE.set(Box::new(stop_ahead_runs));
}

/// An idle compile's turn if no AI run is in progress, else `None` (the
/// compile asks again; runs made ahead wait for it meanwhile). `awaited`:
/// a frame or an export waits for the compile, which then is refused only
/// by runs asked for (in progress or waiting), not by runs made ahead.
/// Always a turn where turns are not enabled.
pub(crate) fn compile(awaited: bool) -> Option<CompileTurn> {
    if !enabled() {
        return Some(CompileTurn(None));
    }
    let (granted, beside_ahead) = TURNS.compile(awaited);
    if beside_ahead > 0 {
        log::warn!(
            "GPU idle compile a frame or an export waits for starts beside {beside_ahead} AI run(s) made ahead; they are told to stop"
        );
        if let Some(stop) = AWAITED_COMPILE.get() {
            stop();
        }
    }
    granted.then_some(CompileTurn(Some(&TURNS)))
}

/// Whether a run the photographer asked for waits for the compile in
/// progress: an idle compile nobody waits for stops for it.
pub(crate) fn asked_run_waits() -> bool {
    enabled() && TURNS.lock().asked_waiting > 0
}

impl Turns {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn run(&self, kind: AiRun) {
        let mut state = self.lock();
        if kind == AiRun::Asked {
            state.asked_waiting += 1;
        }
        loop {
            let compile_waits = state
                .compile_waited
                .is_some_and(|at| at.elapsed() < COMPILE_WAITS);
            // Beside a run in progress the compile cannot start anyway, and
            // a computation that takes a turn inside another's (a mask that
            // needs a second network) must not wait for itself.
            // Likewise an export that waits goes before every run that
            // comes after it.
            let wait = state.compiling
                || state.exports > 0
                || (state.runs == 0
                    && (state.exports_waiting > 0 || (kind == AiRun::Ahead && compile_waits)));
            if !wait {
                break;
            }
            // `compile_waited` runs out without a signal.
            state = self
                .changed
                .wait_timeout(state, Duration::from_millis(100))
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        if kind == AiRun::Asked {
            state.asked_waiting -= 1;
        }
        state.runs += 1;
        if kind == AiRun::Ahead {
            state.ahead_runs += 1;
        }
    }

    fn run_ended(&self, kind: AiRun) {
        let mut state = self.lock();
        state.runs -= 1;
        if kind == AiRun::Ahead {
            state.ahead_runs -= 1;
        }
        drop(state);
        self.changed.notify_all();
    }

    fn export(&self) {
        let mut state = self.lock();
        state.exports_waiting += 1;
        while state.runs > 0 {
            state = self.changed.wait(state).unwrap_or_else(|e| e.into_inner());
        }
        state.exports_waiting -= 1;
        state.exports += 1;
    }

    fn export_ended(&self) {
        let mut state = self.lock();
        state.exports -= 1;
        drop(state);
        self.changed.notify_all();
    }

    /// Whether the compile has its turn, and beside how many runs made
    /// ahead it starts (only an `awaited` one does).
    fn compile(&self, awaited: bool) -> (bool, usize) {
        let mut state = self.lock();
        // An asked run that waits goes first: the compile that stopped for
        // it must not start again before it took its turn. Runs made ahead
        // in progress refuse only a compile nobody waits for.
        let asked_runs = state.runs - state.ahead_runs;
        let refused = if awaited {
            asked_runs > 0
        } else {
            state.runs > 0
        };
        if refused || state.asked_waiting > 0 {
            state.compile_waited = Some(Instant::now());
            return (false, 0);
        }
        state.compiling = true;
        state.compile_waited = None;
        (true, state.ahead_runs)
    }

    fn compile_ended(&self) {
        let mut state = self.lock();
        state.compiling = false;
        drop(state);
        self.changed.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn turns() -> Arc<Turns> {
        Arc::new(Turns::default())
    }

    /// Runs `kind` on a thread; `true` is sent when it has its turn.
    fn start_run(turns: &Arc<Turns>, kind: AiRun) -> std::sync::mpsc::Receiver<bool> {
        let (sender, receiver) = std::sync::mpsc::channel();
        let turns = Arc::clone(turns);
        std::thread::spawn(move || {
            turns.run(kind);
            let _ = sender.send(true);
        });
        receiver
    }

    const SOON: Duration = Duration::from_secs(5);
    const NOT_YET: Duration = Duration::from_millis(300);

    /// A compile and a run never have their turns at the same time: the
    /// compile is refused while a run is in progress, and a run waits for
    /// the compile to end.
    #[test]
    fn a_compile_and_a_run_take_turns() {
        let turns = turns();
        turns.run(AiRun::Ahead);
        assert!(!turns.compile(false).0, "a run is in progress");
        turns.run_ended(AiRun::Ahead);
        assert!(turns.compile(false).0);
        let run = start_run(&turns, AiRun::Ahead);
        assert!(run.recv_timeout(NOT_YET).is_err(), "the compile runs");
        turns.compile_ended();
        assert_eq!(run.recv_timeout(SOON), Ok(true));
    }

    /// A run the photographer asked for is seen by the compile in progress
    /// (which stops when nobody waits for it), and that compile does not
    /// start again before the run had its turn.
    #[test]
    fn an_asked_run_goes_before_an_idle_compile() {
        let turns = turns();
        assert!(turns.compile(false).0);
        assert_eq!(turns.lock().asked_waiting, 0);
        let run = start_run(&turns, AiRun::Asked);
        let seen = Instant::now();
        while turns.lock().asked_waiting == 0 {
            assert!(seen.elapsed() < SOON);
            std::thread::sleep(Duration::from_millis(5));
        }
        // A run made ahead is not: the compile keeps its turn.
        let ahead = start_run(&turns, AiRun::Ahead);
        assert!(ahead.recv_timeout(NOT_YET).is_err());
        assert_eq!(turns.lock().asked_waiting, 1);
        // The helper stopped. Until the run took its turn the compile is
        // refused.
        turns.compile_ended();
        assert_eq!(run.recv_timeout(SOON), Ok(true));
        assert!(!turns.compile(false).0, "the asked run is in progress");
        turns.run_ended(AiRun::Asked);
        // The run made ahead got in with it (no compile was in progress).
        assert_eq!(ahead.recv_timeout(SOON), Ok(true));
        turns.run_ended(AiRun::Ahead);
        assert!(turns.compile(false).0);
    }

    /// An export waits for the run in progress, and runs of either kind
    /// wait for the export, also those that come while it still waits.
    #[test]
    fn an_export_and_a_run_take_turns() {
        let turns = turns();
        turns.run(AiRun::Asked);
        let (sender, export) = std::sync::mpsc::channel();
        let exporting = Arc::clone(&turns);
        std::thread::spawn(move || {
            exporting.export();
            let _ = sender.send(true);
        });
        assert!(
            export.recv_timeout(NOT_YET).is_err(),
            "a run is in progress"
        );
        turns.run_ended(AiRun::Asked);
        assert_eq!(export.recv_timeout(SOON), Ok(true));
        let asked = start_run(&turns, AiRun::Asked);
        let ahead = start_run(&turns, AiRun::Ahead);
        assert!(asked.recv_timeout(NOT_YET).is_err(), "the export runs");
        assert!(ahead.recv_timeout(NOT_YET).is_err(), "the export runs");
        turns.export_ended();
        assert_eq!(asked.recv_timeout(SOON), Ok(true));
        assert_eq!(ahead.recv_timeout(SOON), Ok(true));
    }

    /// A compile that found a run in its way goes before the runs made
    /// ahead that come after: they wait until it had its turn. A run asked
    /// for does not wait for a compile that has not started.
    #[test]
    fn a_waiting_compile_goes_before_runs_made_ahead() {
        let turns = turns();
        turns.run(AiRun::Ahead);
        assert!(!turns.compile(false).0);
        // A turn taken inside the run's own is not held.
        let inner = start_run(&turns, AiRun::Ahead);
        assert_eq!(inner.recv_timeout(SOON), Ok(true));
        turns.run_ended(AiRun::Ahead);
        turns.run_ended(AiRun::Ahead);
        let ahead = start_run(&turns, AiRun::Ahead);
        assert!(ahead.recv_timeout(NOT_YET).is_err(), "the compile waits");
        assert!(turns.compile(false).0);
        assert!(ahead.recv_timeout(NOT_YET).is_err(), "the compile runs");
        turns.compile_ended();
        assert_eq!(ahead.recv_timeout(SOON), Ok(true));
        // The compile asks again while that run is in progress; a run
        // asked for starts all the same.
        assert!(!turns.compile(false).0);
        let asked = start_run(&turns, AiRun::Asked);
        assert_eq!(asked.recv_timeout(SOON), Ok(true));
        // A compile that stopped asking no longer holds runs made ahead.
        turns.run_ended(AiRun::Asked);
        turns.run_ended(AiRun::Ahead);
        std::thread::sleep(COMPILE_WAITS + Duration::from_millis(100));
        let later = start_run(&turns, AiRun::Ahead);
        assert_eq!(later.recv_timeout(SOON), Ok(true));
    }

    /// A compile a frame or an export waits for starts beside a run made
    /// ahead in progress (and the app is told to stop that run), never
    /// beside a run asked for, in progress or waiting; one nobody waits for
    /// is refused by both (WMV8/DVL5: the shown photograph's variant waited
    /// for a Subject mask computed ahead whose model load did not end).
    #[test]
    fn an_awaited_compile_does_not_wait_for_a_run_made_ahead() {
        let turns = turns();
        turns.run(AiRun::Ahead);
        assert_eq!(turns.compile(false), (false, 0), "nobody waits for it");
        assert_eq!(turns.compile(true), (true, 1), "a frame waits for it");
        // Runs that come now wait for the compile, as beside any other.
        let ahead = start_run(&turns, AiRun::Ahead);
        assert!(ahead.recv_timeout(NOT_YET).is_err(), "the compile runs");
        turns.compile_ended();
        assert_eq!(ahead.recv_timeout(SOON), Ok(true));
        turns.run_ended(AiRun::Ahead);
        turns.run_ended(AiRun::Ahead);
        // A run asked for is what the photographer waits for too.
        turns.run(AiRun::Asked);
        assert_eq!(turns.compile(true), (false, 0), "an asked run is in progress");
        turns.run(AiRun::Ahead);
        assert_eq!(turns.compile(true), (false, 0), "an asked run is in progress");
        turns.run_ended(AiRun::Asked);
        assert_eq!(turns.compile(true), (true, 1));
        turns.compile_ended();
        turns.run_ended(AiRun::Ahead);
        turns.lock().asked_waiting = 1;
        assert_eq!(turns.compile(true), (false, 0), "an asked run waits");
    }
}
