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

use std::ops::{Deref, DerefMut};
use std::sync::{Condvar, LockResult, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// How long background renders wait after a foreground render ends: longer
/// than the gap between two frames of a drag, so that no export render
/// starts between them.
pub(super) const FOREGROUND_PAUSE: Duration = Duration::from_millis(250);

#[derive(Default)]
struct State {
    /// Foreground renders waiting for the processor.
    waiting: usize,
    /// When the last foreground render ended.
    ended: Option<Instant>,
}

pub(super) struct ProcessorTurns {
    state: Mutex<State>,
    changed: Condvar,
}

/// The app's one GPU processor slot (the CLI's too) takes its turns here.
pub(super) static TURNS: ProcessorTurns = ProcessorTurns::new();

impl ProcessorTurns {
    pub(super) const fn new() -> Self {
        Self {
            state: Mutex::new(State {
                waiting: 0,
                ended: None,
            }),
            changed: Condvar::new(),
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Locks `slot` in turn: a foreground render at once, a background
    /// render once no foreground render waits or has ended within
    /// [`FOREGROUND_PAUSE`].
    pub(super) fn lock<'a, T>(
        &'a self,
        slot: &'a Mutex<T>,
        foreground: bool,
    ) -> LockResult<TurnGuard<'a, T>> {
        if foreground {
            self.state().waiting += 1;
            let locked = slot.lock();
            self.state().waiting -= 1;
            return wrap(locked, |guard| TurnGuard {
                guard,
                turns: Some(self),
            });
        }
        let mut state = self.state();
        loop {
            let pause = state.ended.map_or(Duration::ZERO, |ended| {
                FOREGROUND_PAUSE.saturating_sub(ended.elapsed())
            });
            if state.waiting == 0 && pause.is_zero() {
                break;
            }
            state = if state.waiting > 0 {
                self.changed
                    .wait(state)
                    .unwrap_or_else(PoisonError::into_inner)
            } else {
                self.changed
                    .wait_timeout(state, pause)
                    .unwrap_or_else(PoisonError::into_inner)
                    .0
            };
        }
        drop(state);
        wrap(slot.lock(), |guard| TurnGuard { guard, turns: None })
    }
}

fn wrap<'a, T>(
    locked: LockResult<MutexGuard<'a, T>>,
    turn: impl FnOnce(MutexGuard<'a, T>) -> TurnGuard<'a, T>,
) -> LockResult<TurnGuard<'a, T>> {
    match locked {
        Ok(guard) => Ok(turn(guard)),
        Err(poisoned) => Err(PoisonError::new(turn(poisoned.into_inner()))),
    }
}

/// The locked slot; a foreground render's records when it ended.
pub(super) struct TurnGuard<'a, T> {
    guard: MutexGuard<'a, T>,
    turns: Option<&'a ProcessorTurns>,
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
        if let Some(turns) = self.turns {
            turns.state().ended = Some(Instant::now());
            turns.changed.notify_all();
        }
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
        let slot: &'static Mutex<Vec<&str>> = Box::leak(Box::new(Mutex::new(Vec::new())));
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

    /// A drag's next frame arrives after its previous one ended: background
    /// renders wait the pause after a foreground render, then go on.
    #[test]
    fn background_renders_wait_out_the_pause_after_a_foreground_render() {
        let turns = turns();
        let slot = Arc::new(Mutex::new(()));
        drop(turns.lock(&slot, true).unwrap());
        let ended = Instant::now();
        drop(turns.lock(&slot, false).unwrap());
        assert!(ended.elapsed() >= FOREGROUND_PAUSE);
        // No foreground render since: at once.
        let start = Instant::now();
        drop(turns.lock(&slot, false).unwrap());
        assert!(start.elapsed() < FOREGROUND_PAUSE);
    }

    /// A poisoned slot is handed on (the caller resets it), and a
    /// foreground render that panicked still counts as ended.
    #[test]
    fn a_poisoned_slot_is_handed_on() {
        let turns = turns();
        let slot = Arc::new(Mutex::new(1));
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
