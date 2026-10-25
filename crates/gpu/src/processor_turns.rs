//! Turns at the GPU processor: renders the user watches (the Develop view)
//! before background renders (exports, thumbnails).
//!
//! Export workers take the processor one after another, each for a whole
//! photo's render. A Develop frame that queued behind them waited for
//! several, up to 1.9 s during a batch export (notes: parity-gaps/G44). Now
//! a background render does not take the processor while a foreground render
//! waits for it, nor within [`FOREGROUND_PAUSE`] after one ends, so a drag
//! keeps the processor until it stops; a background render already running
//! still finishes first.
//!
//! A background render that already runs kept the processor for the whole
//! photo: on an M4 Pro a 45 MP export render takes ~0.9 s on the GPU, and
//! the first frame of a drag waited for it. Exports therefore render on a
//! processor of their own ([`ProcessorSlot::background`]): they take no
//! turn, so foreground renders never wait for them, and they pause between
//! their tiles while foreground renders run (notes: parity-gaps/G44).

use std::ops::{Deref, DerefMut};
use std::sync::{Condvar, LockResult, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// How long background renders wait after a foreground render ends: longer
/// than the gap between two frames of a drag, so that no export render
/// starts between them.
pub(super) const FOREGROUND_PAUSE: Duration = Duration::from_millis(250);

#[derive(Default)]
struct State {
    /// A render holds its turn (it has or is about to lock the slot).
    taken: bool,
    /// The render holding the turn is a foreground render.
    foreground_runs: bool,
    /// Foreground renders waiting for a turn.
    waiting: usize,
    /// When the last foreground render ended.
    ended: Option<Instant>,
    /// Background renders blocked in `wait_for_foreground`; lets tests
    /// release a foreground render only once a waiter is really waiting.
    #[cfg(test)]
    background_waiting: usize,
}

/// A GPU processor slot and whether foreground renders use it.
pub struct ProcessorSlot<T> {
    slot: Mutex<T>,
    background_only: bool,
}

impl<T> ProcessorSlot<T> {
    /// The slot of the Develop view's renders and of the background
    /// renders that share its processor (thumbnails): they take turns.
    pub const fn new(value: T) -> Self {
        Self {
            slot: Mutex::new(value),
            background_only: false,
        }
    }

    /// A slot only background renders use (exports): a render
    /// on it takes no turn and pauses between its tiles for foreground
    /// renders, which it cannot hold up.
    pub const fn background(value: T) -> Self {
        Self {
            slot: Mutex::new(value),
            background_only: true,
        }
    }
}

impl<T> ProcessorSlot<T> {
    /// Whether only background renders (exports) use this slot.
    pub fn background_only(&self) -> bool {
        self.background_only
    }
}

impl<T> ProcessorSlot<Option<T>> {
    /// Empties the slot unless a render holds it; what it held.
    pub fn take_if_idle(&self) -> Option<T> {
        self.slot.try_lock().ok()?.take()
    }
}

impl<T> Deref for ProcessorSlot<T> {
    type Target = Mutex<T>;

    fn deref(&self) -> &Mutex<T> {
        &self.slot
    }
}

pub(super) struct ProcessorTurns {
    state: Mutex<State>,
    changed: Condvar,
}

/// The app's Develop processor slot takes its turns here.
pub(super) static TURNS: ProcessorTurns = ProcessorTurns::new();

impl ProcessorTurns {
    pub(super) const fn new() -> Self {
        Self {
            state: Mutex::new(State {
                taken: false,
                foreground_runs: false,
                waiting: 0,
                ended: None,
                #[cfg(test)]
                background_waiting: 0,
            }),
            changed: Condvar::new(),
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Locks `slot` in turn: a foreground render once no render holds a
    /// turn, a background render once, besides, no foreground render waits
    /// or has ended within [`FOREGROUND_PAUSE`]. Every waiter checks again
    /// when a turn ends, so one queued earlier gets no precedence. A render
    /// on a background-only slot takes no turn: it waits only for the
    /// foreground renders, and for the slot.
    pub(super) fn lock<'a, T>(
        &'a self,
        slot: &'a ProcessorSlot<T>,
        foreground: bool,
    ) -> LockResult<TurnGuard<'a, T>> {
        debug_assert!(
            !(foreground && slot.background_only),
            "foreground renders do not use a background-only processor"
        );
        let mut state = self.state();
        if foreground {
            state.waiting += 1;
            while state.taken {
                state = self
                    .changed
                    .wait(state)
                    .unwrap_or_else(PoisonError::into_inner);
            }
            state.waiting -= 1;
        } else {
            state = self.wait_for_foreground(state, !slot.background_only).0;
        }
        let takes_turn = !slot.background_only;
        state.taken |= takes_turn;
        state.foreground_runs |= foreground;
        drop(state);
        // Uncontended but for callers outside the turns (the app's
        // pipeline prewarm) and, on a background-only slot, other
        // background renders.
        let turn = |guard| TurnGuard {
            guard,
            turns: self,
            foreground,
            takes_turn,
        };
        match slot.lock() {
            Ok(guard) => Ok(turn(guard)),
            Err(poisoned) => Err(PoisonError::new(turn(poisoned.into_inner()))),
        }
    }

    /// Waits until no foreground render runs, waits or has ended within
    /// [`FOREGROUND_PAUSE`] (and, with `for_turn`, no render holds a turn);
    /// true if it had to.
    fn wait_for_foreground<'a>(
        &self,
        mut state: MutexGuard<'a, State>,
        for_turn: bool,
    ) -> (MutexGuard<'a, State>, bool) {
        let mut waited = false;
        loop {
            let pause = state.ended.map_or(Duration::ZERO, |ended| {
                FOREGROUND_PAUSE.saturating_sub(ended.elapsed())
            });
            let taken = state.foreground_runs || (for_turn && state.taken);
            if !taken && state.waiting == 0 && pause.is_zero() {
                return (state, waited);
            }
            waited = true;
            #[cfg(test)]
            {
                state.background_waiting += 1;
            }
            state = if taken || state.waiting > 0 {
                self.changed
                    .wait(state)
                    .unwrap_or_else(PoisonError::into_inner)
            } else {
                self.changed
                    .wait_timeout(state, pause)
                    .unwrap_or_else(PoisonError::into_inner)
                    .0
            };
            #[cfg(test)]
            {
                state.background_waiting -= 1;
            }
        }
    }
}

/// The locked slot; gives the turn back when dropped (a foreground render's
/// also records when it ended).
pub(super) struct TurnGuard<'a, T> {
    guard: MutexGuard<'a, T>,
    turns: &'a ProcessorTurns,
    foreground: bool,
    /// Off on a background-only slot.
    takes_turn: bool,
}

impl<T> TurnGuard<'_, T> {
    /// Whether the render may pause in its course for foreground renders:
    /// it holds no turn they wait for.
    pub(super) fn pauses_for_foreground(&self) -> bool {
        !self.takes_turn
    }
}

impl ProcessorTurns {
    /// A background render on a background-only slot, between two of its
    /// tiles: waits while foreground renders run; true if it did.
    pub(super) fn pause_for_foreground(&self) -> bool {
        self.wait_for_foreground(self.state(), false).1
    }
}

impl<T> Deref for TurnGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.guard
    }
}

impl<T> DerefMut for TurnGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.guard
    }
}

impl<T> Drop for TurnGuard<'_, T> {
    fn drop(&mut self) {
        if !self.takes_turn {
            return;
        }
        let mut state = self.turns.state();
        state.taken = false;
        state.foreground_runs = false;
        if self.foreground {
            state.ended = Some(Instant::now());
        }
        drop(state);
        self.turns.changed.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::mpsc::channel;

    fn turns() -> &'static ProcessorTurns {
        Box::leak(Box::new(ProcessorTurns::new()))
    }

    /// Waits until `turns` counts `n` waiting foreground renders.
    fn until_waiting(turns: &ProcessorTurns, n: usize) {
        while turns.state().waiting != n {
            std::thread::yield_now();
        }
    }

    /// The Develop view's frame goes before an export that asked first:
    /// with a plain mutex either could win.
    #[test]
    fn a_waiting_foreground_render_goes_before_background_renders() {
        let turns = turns();
        let slot: &'static ProcessorSlot<Vec<&str>> =
            Box::leak(Box::new(ProcessorSlot::new(Vec::new())));
        let running = turns.lock(slot, false).unwrap();
        let (sent, order) = channel();
        let foreground = std::thread::spawn({
            let sent = sent.clone();
            move || {
                turns.lock(slot, true).unwrap().push("foreground");
                sent.send(()).unwrap();
            }
        });
        until_waiting(turns, 1);
        let backgrounds: Vec<_> = (0..3)
            .map(|_| {
                let sent = sent.clone();
                std::thread::spawn(move || {
                    turns.lock(slot, false).unwrap().push("background");
                    sent.send(()).unwrap();
                })
            })
            .collect();
        // The background renders reach their wait.
        std::thread::sleep(Duration::from_millis(50));
        drop(running);
        foreground.join().unwrap();
        for background in backgrounds {
            background.join().unwrap();
        }
        assert_eq!(order.try_iter().count(), 4);
        let order = slot.lock().unwrap();
        assert_eq!(order[0], "foreground");
        assert_eq!(order.len(), 4);
    }

    /// Export renders that queued while another render ran do not get the
    /// processor before a Develop frame that asks after them.
    #[test]
    fn background_renders_queued_earlier_do_not_go_before_a_foreground_render() {
        // A plain mutex hands itself to any of its waiters: a few rounds.
        for _ in 0..5 {
            let turns = turns();
            let slot: &'static ProcessorSlot<Vec<&str>> =
                Box::leak(Box::new(ProcessorSlot::new(Vec::new())));
            let running = turns.lock(slot, false).unwrap();
            let backgrounds: Vec<_> = (0..6)
                .map(|_| {
                    std::thread::spawn(move || {
                        turns.lock(slot, false).unwrap().push("background");
                    })
                })
                .collect();
            // The background renders reach their wait.
            std::thread::sleep(Duration::from_millis(30));
            let foreground = std::thread::spawn(move || {
                turns.lock(slot, true).unwrap().push("foreground");
            });
            until_waiting(turns, 1);
            // The foreground render reaches its wait.
            std::thread::sleep(Duration::from_millis(10));
            drop(running);
            foreground.join().unwrap();
            for background in backgrounds {
                background.join().unwrap();
            }
            let order = slot.lock().unwrap();
            assert_eq!(order[0], "foreground");
            assert_eq!(order.len(), 7);
        }
    }

    /// An export render that queued during a Develop frame still waits out
    /// the pause after it.
    #[test]
    fn a_background_render_queued_during_a_foreground_render_waits_out_the_pause() {
        let turns = turns();
        let slot: &'static ProcessorSlot<()> = Box::leak(Box::new(ProcessorSlot::new(())));
        let frame = turns.lock(slot, true).unwrap();
        let (sent, taken) = channel();
        let background = std::thread::spawn(move || {
            let _turn = turns.lock(slot, false).unwrap();
            sent.send(Instant::now()).unwrap();
        });
        // The background render reaches its wait.
        std::thread::sleep(Duration::from_millis(50));
        drop(frame);
        let ended = Instant::now();
        let taken = taken.recv().unwrap();
        background.join().unwrap();
        assert!(taken.duration_since(ended) >= FOREGROUND_PAUSE - Duration::from_millis(5));
    }

    /// A drag's next frame arrives after its previous one ended: background
    /// renders wait the pause after a foreground render, then go on.
    #[test]
    fn background_renders_wait_out_the_pause_after_a_foreground_render() {
        let turns = turns();
        let slot = Arc::new(ProcessorSlot::new(()));
        drop(turns.lock(&slot, true).unwrap());
        let ended = Instant::now();
        drop(turns.lock(&slot, false).unwrap());
        assert!(ended.elapsed() >= FOREGROUND_PAUSE);
        // No foreground render since: at once.
        let start = Instant::now();
        drop(turns.lock(&slot, false).unwrap());
        assert!(start.elapsed() < FOREGROUND_PAUSE);
    }

    /// An export render on its own processor holds up no Develop frame:
    /// before, both took the one processor and the frame waited for the
    /// whole export render.
    #[test]
    fn a_foreground_render_does_not_wait_for_a_render_on_a_background_only_slot() {
        let turns = turns();
        let shared: &'static ProcessorSlot<()> = Box::leak(Box::new(ProcessorSlot::new(())));
        let exports: &'static ProcessorSlot<()> =
            Box::leak(Box::new(ProcessorSlot::background(())));
        let export = turns.lock(exports, false).unwrap();
        assert!(export.pauses_for_foreground());
        let (sent, done) = channel();
        let frame = std::thread::spawn(move || {
            drop(turns.lock(shared, true).unwrap());
            sent.send(()).unwrap();
        });
        done.recv_timeout(Duration::from_secs(5))
            .expect("the frame renders while the export render runs");
        frame.join().unwrap();
        drop(export);
        // Renders on the shared slot hold a turn and never pause in their course.
        assert!(!turns.lock(shared, false).unwrap().pauses_for_foreground());
    }

    /// Between two tiles, an export render on its own processor waits
    /// while a Develop frame renders and for the pause after it.
    #[test]
    fn a_background_only_render_pauses_between_tiles_for_foreground_renders() {
        let turns = turns();
        let shared: &'static ProcessorSlot<()> = Box::leak(Box::new(ProcessorSlot::new(())));
        let exports: &'static ProcessorSlot<()> =
            Box::leak(Box::new(ProcessorSlot::background(())));
        let frame = turns.lock(shared, true).unwrap();
        let (sent, resumed) = channel();
        let export = std::thread::spawn(move || {
            let _render = turns.lock(exports, false).unwrap();
            sent.send(Instant::now()).unwrap();
        });
        // The export waits to start while the frame renders.
        assert!(resumed.recv_timeout(Duration::from_millis(50)).is_err());
        drop(frame);
        let ended = Instant::now();
        let started = resumed.recv().unwrap();
        export.join().unwrap();
        assert!(started.duration_since(ended) >= FOREGROUND_PAUSE - Duration::from_millis(5));

        // Started, it pauses at its next tile for a frame begun since.
        let render = turns.lock(exports, false).unwrap();
        let frame = turns.lock(shared, true).unwrap();
        let (sent, resumed) = channel();
        let tiles = std::thread::spawn(move || {
            assert!(turns.pause_for_foreground());
            sent.send(Instant::now()).unwrap();
        });
        // Release the frame only once the tile thread waits for it: on a
        // loaded machine the thread can start later than the pause after it.
        while turns.state().background_waiting == 0 {
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(resumed.recv_timeout(Duration::from_millis(50)).is_err());
        drop(frame);
        let ended = Instant::now();
        let resumed = resumed.recv().unwrap();
        tiles.join().unwrap();
        assert!(resumed.duration_since(ended) >= FOREGROUND_PAUSE - Duration::from_millis(5));
        drop(render);
        // No frame since: it goes on at once.
        std::thread::sleep(FOREGROUND_PAUSE);
        assert!(!turns.pause_for_foreground());
    }

    /// Memory pressure frees an idle processor, never one a render holds.
    #[test]
    fn only_an_idle_slot_is_emptied() {
        let slot = ProcessorSlot::background(Some(1));
        let render = slot.lock().unwrap();
        assert_eq!(slot.take_if_idle(), None);
        drop(render);
        assert_eq!(slot.take_if_idle(), Some(1));
        assert_eq!(*slot.lock().unwrap(), None);
    }

    /// A poisoned slot is handed on (the caller resets it), and a
    /// foreground render that panicked still counts as ended.
    #[test]
    fn a_poisoned_slot_is_handed_on() {
        let turns = turns();
        let slot = Arc::new(ProcessorSlot::new(1));
        let poisoner = Arc::clone(&slot);
        let _ = std::thread::spawn(move || {
            let _guard = turns.lock(&poisoner, true);
            panic!("render failed");
        })
        .join();
        assert_eq!(turns.state().waiting, 0);
        let Err(poisoned) = turns.lock(&slot, false) else {
            panic!("the slot is poisoned");
        };
        assert_eq!(**poisoned.get_ref(), 1);
    }
}
