//! The preview worker's input: one replaceable pending job. A request that
//! arrives while the worker renders replaces the waiting one at once, so the
//! next render always starts from the newest value received and a superseded
//! request is answered (rejected) right away instead of waiting in a queue
//! until the worker is free. With a FIFO channel drained only between
//! renders, the webview's four requests in flight sat behind a slow render
//! and the next frame started from the oldest of them: on a 100 ms render
//! with 16 ms slider moves each frame began ~4 moves behind the newest one
//! and the release frame waited for one extra stale render.

use std::sync::{Condvar, Mutex};
use std::time::Duration;

/// Whether a job received after another should replace it.
pub trait Supersedes {
    fn supersedes(&self, current: &Self) -> bool;
}

pub struct LatestSlot<T> {
    pending: Mutex<Option<T>>,
    ready: Condvar,
}

impl<T> Default for LatestSlot<T> {
    fn default() -> Self {
        Self {
            pending: Mutex::new(None),
            ready: Condvar::new(),
        }
    }
}

impl<T: Supersedes> LatestSlot<T> {
    /// Keeps `job` as the next one unless the waiting job is newer; the
    /// loser is dropped (outside the lock), which rejects its invoke.
    /// Returns whether `job` was kept.
    pub fn submit(&self, job: T) -> bool {
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        let (dropped, kept) = match pending.take() {
            Some(current) if !job.supersedes(&current) => {
                *pending = Some(current);
                (Some(job), false)
            }
            current => {
                *pending = Some(job);
                (current, true)
            }
        };
        drop(pending);
        self.ready.notify_one();
        drop(dropped);
        kept
    }
}

impl<T> LatestSlot<T> {
    /// Waits for a job.
    pub fn take(&self) -> T {
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if let Some(job) = pending.take() {
                return job;
            }
            pending = self.ready.wait(pending).unwrap_or_else(|e| e.into_inner());
        }
    }

    /// Waits up to `timeout` for a job.
    pub fn take_timeout(&self, timeout: Duration) -> Option<T> {
        let pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        let (mut pending, _) = self
            .ready
            .wait_timeout_while(pending, timeout, |pending| pending.is_none())
            .unwrap_or_else(|e| e.into_inner());
        pending.take()
    }

    /// Waits for a job, or until `other_work` holds (checked under the lock
    /// on every wake, so a [`wake`](Self::wake) after it became true is
    /// never lost): then None.
    pub fn take_unless(&self, other_work: impl Fn() -> bool) -> Option<T> {
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if let Some(job) = pending.take() {
                return Some(job);
            }
            if other_work() {
                return None;
            }
            pending = self.ready.wait(pending).unwrap_or_else(|e| e.into_inner());
        }
    }

    /// Wakes a worker waiting in [`take_unless`](Self::take_unless) to check
    /// its other work again.
    pub fn wake(&self) {
        let _pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        self.ready.notify_all();
    }

    pub fn try_take(&self) -> Option<T> {
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
    }
}

/// How long the preview worker waits for the primary's next frame before
/// developing another multi view cell live.
pub const LIVE_IDLE: Duration = Duration::from_millis(25);

/// Edit all selected between the primary's frames: the preview worker
/// develops the other cells live (`multi_view::live_step`, `step` here: true
/// when it developed one) while no frame of the primary is asked for. During
/// a drag one cell follows each frame; once no frame is asked for within
/// [`LIVE_IDLE`], or after a final frame (a typed value, a drag's release),
/// they catch up back to back, with a look for a request of the primary
/// between any two, so it waits for one cell at most.
#[derive(Default)]
pub struct LiveCells {
    /// The others catch up back to back.
    idle: bool,
}

impl LiveCells {
    /// The primary's next job; until it comes, cells while `pending`.
    pub fn next_job<T>(
        &mut self,
        jobs: &LatestSlot<T>,
        pending: impl Fn() -> bool,
        mut step: impl FnMut() -> bool,
    ) -> T {
        loop {
            if pending() {
                let next = if self.idle {
                    jobs.try_take()
                } else {
                    jobs.take_timeout(LIVE_IDLE)
                };
                match next {
                    Some(job) => {
                        self.idle = false;
                        return job;
                    }
                    // Nothing developed (no cell ready, no GPU): wait again
                    // rather than spin.
                    None => self.idle = step(),
                }
            } else if let Some(job) = jobs.take_unless(&pending) {
                // Else woken by a live change (`wake`): a typed value asks
                // for one frame of the primary only.
                self.idle = false;
                return job;
            }
        }
    }

    /// After a frame of the primary: its next job when already asked for,
    /// else one other cell while `pending`.
    pub fn after_frame<T>(
        &mut self,
        jobs: &LatestSlot<T>,
        interactive: bool,
        pending: impl Fn() -> bool,
        mut step: impl FnMut() -> bool,
    ) -> Option<T> {
        if !pending() {
            return None;
        }
        let next = jobs.try_take();
        if next.is_none() {
            self.idle = step() && !interactive;
        }
        next
    }
}

/// Artificial render delay for measuring superseding on a fast GPU:
/// `RAWTONIC_PREVIEW_DELAY_MS` (profiling builds only, off unless set).
#[cfg(feature = "performance-profiling")]
pub fn debug_render_delay() -> Option<Duration> {
    static DELAY: std::sync::OnceLock<Option<Duration>> = std::sync::OnceLock::new();
    *DELAY.get_or_init(|| {
        std::env::var("RAWTONIC_PREVIEW_DELAY_MS")
            .ok()
            .and_then(|ms| ms.parse::<u64>().ok())
            .filter(|&ms| ms > 0)
            .map(Duration::from_millis)
    })
}

#[cfg(test)]
mod tests {
    use super::{LatestSlot, Supersedes};
    use crate::app_state::preview_job_is_stale;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant};

    /// A request with its invoke's answer, like `PreviewJob`.
    struct Job {
        sequence: Option<u64>,
        responder: tokio::sync::oneshot::Sender<u64>,
    }

    impl Supersedes for Job {
        fn supersedes(&self, current: &Self) -> bool {
            crate::app_state::supersedes(self.sequence, current.sequence)
        }
    }

    fn job(sequence: Option<u64>) -> (Job, tokio::sync::oneshot::Receiver<u64>) {
        let (responder, answer) = tokio::sync::oneshot::channel();
        (
            Job {
                sequence,
                responder,
            },
            answer,
        )
    }

    fn rejected(answer: &mut tokio::sync::oneshot::Receiver<u64>) -> bool {
        matches!(
            answer.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Closed)
        )
    }

    #[test]
    fn a_newer_request_rejects_the_waiting_one_at_once() {
        let slot = LatestSlot::default();
        let (first, mut first_answer) = job(Some(1));
        let (second, mut second_answer) = job(Some(2));
        assert!(slot.submit(first));
        assert!(slot.submit(second));
        // Rejected on arrival of the newer request, not when the worker is free.
        assert!(rejected(&mut first_answer));
        assert!(!rejected(&mut second_answer));
        assert_eq!(slot.try_take().unwrap().sequence, Some(2));
        assert!(slot.try_take().is_none());
    }

    #[test]
    fn a_late_older_request_never_replaces_a_newer_one() {
        let slot = LatestSlot::default();
        let (newer, _newer_answer) = job(Some(8));
        let (older, mut older_answer) = job(Some(7));
        assert!(slot.submit(newer));
        assert!(!slot.submit(older));
        assert!(rejected(&mut older_answer));
        assert_eq!(slot.try_take().unwrap().sequence, Some(8));
        // Unsequenced jobs (photo activation) keep arrival order.
        let (sequenced, _a) = job(Some(9));
        let (activation, _b) = job(None);
        slot.submit(sequenced);
        slot.submit(activation);
        assert_eq!(slot.try_take().unwrap().sequence, None);
    }

    #[test]
    fn take_timeout_returns_none_when_idle_and_wakes_for_a_job() {
        let slot = Arc::new(LatestSlot::<Job>::default());
        assert!(slot.take_timeout(Duration::from_millis(5)).is_none());
        let sender = Arc::clone(&slot);
        let submitter = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            sender.submit(job(Some(3)).0);
        });
        let started = Instant::now();
        let taken = slot.take_timeout(Duration::from_secs(5));
        assert_eq!(taken.map(|job| job.sequence), Some(Some(3)));
        assert!(started.elapsed() < Duration::from_secs(5));
        submitter.join().unwrap();
    }

    /// A slider drag (a move every 2 ms) against a renderer taking 25 ms, the
    /// worker loop as in `start_preview_worker`: every render starts from the
    /// newest move received, frames never go back, the release value renders
    /// last, and superseded moves are answered while the render still runs.
    #[test]
    fn slow_renders_always_start_from_the_newest_move_and_end_on_the_last() {
        const MOVES: u64 = 150;
        const RENDER: Duration = Duration::from_millis(25);
        let slot = Arc::new(LatestSlot::<Job>::default());
        let newest_submitted = Arc::new(AtomicU64::new(0));

        let worker_slot = Arc::clone(&slot);
        let worker_newest = Arc::clone(&newest_submitted);
        let worker = std::thread::spawn(move || {
            let mut newest_sequence = None;
            let mut rendered = Vec::new();
            loop {
                let seen = worker_newest.load(Ordering::SeqCst);
                let job = worker_slot.take();
                let sequence = job.sequence.unwrap();
                // Never older than what had been submitted before the take.
                assert!(sequence >= seen, "rendered {sequence}, {seen} was waiting");
                if preview_job_is_stale(job.sequence, newest_sequence) {
                    continue;
                }
                newest_sequence = job.sequence;
                std::thread::sleep(RENDER);
                rendered.push(sequence);
                let _ = job.responder.send(sequence);
                if sequence == MOVES {
                    return rendered;
                }
            }
        });

        let mut answers = Vec::new();
        for sequence in 1..=MOVES {
            let (next, answer) = job(Some(sequence));
            slot.submit(next);
            newest_submitted.store(sequence, Ordering::SeqCst);
            answers.push(answer);
            std::thread::sleep(Duration::from_millis(2));
        }
        // Looked at before the last render ends: answered with a frame
        // (Some(true)), rejected (Some(false)) or still open (None).
        let outcome = |answer: &mut tokio::sync::oneshot::Receiver<u64>| match answer.try_recv() {
            Ok(_) => Some(true),
            Err(tokio::sync::oneshot::error::TryRecvError::Closed) => Some(false),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty) => None,
        };
        let mut outcomes: Vec<_> = answers.iter_mut().map(outcome).collect();
        let rejected_during_drag = outcomes.iter().filter(|&&o| o == Some(false)).count();
        let rendered = worker.join().unwrap();
        for (answer, seen) in answers.iter_mut().zip(outcomes.iter_mut()) {
            if seen.is_none() {
                *seen = outcome(answer);
            }
        }

        assert_eq!(
            rendered.last(),
            Some(&MOVES),
            "the release value renders last"
        );
        assert!(rendered.windows(2).all(|w| w[0] < w[1]), "{rendered:?}");
        // ~300 ms of moves over 25 ms renders: most moves are skipped, and
        // their invokes were answered during the drag, not queued behind the
        // renders (bounds loose for loaded test machines).
        assert!(rendered.len() < MOVES as usize / 2, "{}", rendered.len());
        assert!(
            rejected_during_drag >= MOVES as usize / 4,
            "{rejected_during_drag}"
        );
        // Every move was answered: a frame for the rendered ones, a
        // rejection for the rest.
        assert!(outcomes.iter().all(Option::is_some));
        let answered = outcomes.iter().filter(|&&o| o == Some(true)).count();
        assert_eq!(answered, rendered.len());
    }

    /// A plain job for the Edit-all scheduling tests.
    struct Frame(u32);

    impl Supersedes for Frame {
        fn supersedes(&self, _current: &Self) -> bool {
            true
        }
    }

    /// Cells waiting for the live recipe, as `multi_view::live_step` sees
    /// them: `step` develops the next one (true) or finds none (false, and
    /// nothing is pending any more).
    struct Cells {
        behind: std::sync::atomic::AtomicUsize,
        pending: std::sync::atomic::AtomicBool,
        steps: AtomicU64,
    }

    impl Cells {
        fn new(behind: usize) -> Arc<Self> {
            Arc::new(Cells {
                behind: behind.into(),
                pending: (behind > 0).into(),
                steps: 0.into(),
            })
        }
        fn pending(&self) -> bool {
            self.pending.load(Ordering::SeqCst)
        }
        fn step(&self) -> bool {
            self.steps.fetch_add(1, Ordering::SeqCst);
            let developed = self
                .behind
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
                .is_ok();
            if self.behind.load(Ordering::SeqCst) == 0 {
                self.pending.store(false, Ordering::SeqCst);
            }
            developed
        }
        /// A new live change: every cell is behind again.
        fn change(&self, cells: usize) {
            self.behind.store(cells, Ordering::SeqCst);
            self.pending.store(true, Ordering::SeqCst);
        }
    }

    #[test]
    fn idle_cells_catch_up_back_to_back() {
        let slot = Arc::new(LatestSlot::<Frame>::default());
        let cells = Cells::new(11);
        let mut live = super::LiveCells::default();
        let start = Instant::now();
        let (worker_slot, worker_cells) = (slot.clone(), cells.clone());
        let release = std::thread::spawn(move || {
            // Once every cell is developed, the primary asks for a frame.
            while worker_cells.pending() {
                std::thread::sleep(Duration::from_millis(1));
            }
            worker_slot.submit(Frame(1));
        });
        let job = live.next_job(&slot, || cells.pending(), || cells.step());
        release.join().unwrap();
        assert_eq!(job.0, 1);
        assert_eq!(cells.behind.load(Ordering::SeqCst), 0);
        // One wait for the primary's frame, not one per cell (11 × 25 ms).
        assert!(
            start.elapsed() < super::LIVE_IDLE * 4,
            "{:?}",
            start.elapsed()
        );
    }

    #[test]
    fn a_frame_of_the_primary_goes_before_the_next_cell() {
        let slot = LatestSlot::<Frame>::default();
        let cells = Cells::new(11);
        let mut live = super::LiveCells::default();
        // The primary asks for a frame while the third cell is developed.
        let job = live.next_job(
            &slot,
            || cells.pending(),
            || {
                if cells.steps.load(Ordering::SeqCst) == 2 {
                    slot.submit(Frame(7));
                }
                cells.step()
            },
        );
        assert_eq!(job.0, 7);
        assert_eq!(cells.steps.load(Ordering::SeqCst), 3);
        // After a frame during a drag: one cell, then the primary's wait.
        assert!(
            live.after_frame(&slot, true, || cells.pending(), || cells.step())
                .is_none()
        );
        assert_eq!(cells.steps.load(Ordering::SeqCst), 4);
        assert!(!live.idle);
        // After a final frame the rest catch up back to back.
        assert!(
            live.after_frame(&slot, false, || cells.pending(), || cells.step())
                .is_none()
        );
        assert!(live.idle);
        // A frame already asked for goes first.
        slot.submit(Frame(8));
        let next = live.after_frame(&slot, false, || cells.pending(), || cells.step());
        assert_eq!(next.map(|f| f.0), Some(8));
        assert_eq!(cells.steps.load(Ordering::SeqCst), 5);
    }

    #[test]
    fn a_new_live_change_during_catch_up_brings_every_cell_to_it() {
        let slot = LatestSlot::<Frame>::default();
        let cells = Cells::new(11);
        let mut live = super::LiveCells::default();
        let job = live.next_job(
            &slot,
            || cells.pending(),
            || {
                // The patch is replaced after five cells: all 11 again.
                if cells.steps.load(Ordering::SeqCst) == 5 {
                    cells.change(11);
                }
                let developed = cells.step();
                if !cells.pending() {
                    slot.submit(Frame(2));
                }
                developed
            },
        );
        assert_eq!(job.0, 2);
        assert_eq!(cells.behind.load(Ordering::SeqCst), 0);
        assert_eq!(cells.steps.load(Ordering::SeqCst), 5 + 11);
    }

    #[test]
    fn a_waiting_worker_wakes_for_a_live_change_and_stops_once_it_is_written() {
        let slot = Arc::new(LatestSlot::<Frame>::default());
        let cells = Cells::new(0);
        let (other_slot, other_cells) = (slot.clone(), cells.clone());
        let changes = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            // A typed value: a live change and no further frame.
            other_cells.change(11);
            other_slot.wake();
            // The cells catch up; the recipes are then written and the
            // live change cleared while cells are still behind.
            while other_cells.steps.load(Ordering::SeqCst) < 6 {
                std::thread::sleep(Duration::from_millis(1));
            }
            other_cells.pending.store(false, Ordering::SeqCst);
            let cleared_at = other_cells.steps.load(Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(60));
            other_slot.submit(Frame(3));
            cleared_at
        });
        let mut live = super::LiveCells::default();
        let job = live.next_job(
            &slot,
            || cells.pending(),
            || {
                std::thread::sleep(Duration::from_millis(2));
                cells.step()
            },
        );
        let cleared_at = changes.join().unwrap();
        assert_eq!(job.0, 3);
        // Woken without a frame of the primary, and no cell after the clear
        // but the one under way.
        let steps = cells.steps.load(Ordering::SeqCst);
        assert!(
            (6..=cleared_at + 1).contains(&steps),
            "{steps} after {cleared_at}"
        );
    }
}
