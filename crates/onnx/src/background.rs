// SPDX-License-Identifier: AGPL-3.0-or-later
//! Work that waits until the app is idle and gives way the moment it is
//! not: the GPU checks (`check::queue`).
//!
//! A model without a verdict for a GPU is loaded on the CPU at once and its
//! check is queued here; the photographer's first AI click never waits for
//! a measurement (gap G76: AI Subject's first click took 110 s on a Radeon
//! 680M, 49 s of them the check). The queue runs one job at a time on a
//! thread of its own, and only when no AI model runs (background tagging
//! included), no export holds the checks (`hold`) and nothing was noted for
//! `idle_after` (`activity`: an edit's render, a thumbnail, a model load).
//! Any of those stops the running job (`Stop`: its ONNX runs are
//! terminated) and puts it back at the front of the queue, so it never
//! competes with the photographer's work and its timings are not taken
//! beside other work (a CPU timed too slow would wrongly favour the GPU).
//!
//! A job writes its result only through `Stop::commit`, which refuses once
//! the job was stopped: after `hold` returns (an export starts) or `shut`
//! (the app quits), no verdict changes, so an export uses one provider from
//! its first photo to its last.

use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// Why a job's run ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Ran {
    /// Done, with or without a result: not run again.
    Done,
    /// Stopped by activity, a hold or the quit: run again later.
    Stopped,
}

type Callback = Box<dyn FnOnce() + Send>;

/// Stops one run of a job.
#[derive(Default)]
pub(crate) struct Stop {
    state: Mutex<StopState>,
}

#[derive(Default)]
struct StopState {
    stopped: bool,
    on_stop: Vec<Callback>,
}

impl Stop {
    pub(crate) fn new() -> Arc<Self> {
        Arc::default()
    }

    /// Stops the run: calls what was registered with `on_stop` and refuses
    /// every later `commit`.
    pub(crate) fn stop(&self) {
        let callbacks = {
            let mut state = self.state.lock().unwrap();
            if state.stopped {
                return;
            }
            state.stopped = true;
            std::mem::take(&mut state.on_stop)
        };
        for callback in callbacks {
            callback();
        }
    }

    pub(crate) fn stopped(&self) -> bool {
        self.state.lock().unwrap().stopped
    }

    /// Calls `f` when the run is stopped (now, if it is already).
    pub(crate) fn on_stop(&self, f: impl FnOnce() + Send + 'static) {
        let mut state = self.state.lock().unwrap();
        if state.stopped {
            drop(state);
            f();
        } else {
            state.on_stop.push(Box::new(f));
        }
    }

    /// Runs `f` (the job's result) unless the run was stopped; `stop` waits
    /// for a `f` that runs, so nothing is written after `stop` returns.
    pub(crate) fn commit(&self, f: impl FnOnce()) -> bool {
        let state = self.state.lock().unwrap();
        if state.stopped {
            return false;
        }
        f();
        drop(state);
        true
    }
}

struct State<J> {
    queue: VecDeque<(String, Arc<J>)>,
    /// Keys queued or running.
    keys: HashSet<String>,
    running: Option<Arc<Stop>>,
    /// Exports (or other batches) under way: nothing runs.
    holds: usize,
    /// Model runs under way: nothing runs.
    busy: usize,
    last_activity: Instant,
    worker: bool,
    shut: bool,
}

/// A queue of jobs keyed by name, each run by `run` while the app is idle.
pub(crate) struct Scheduler<J> {
    state: Mutex<State<J>>,
    wake: Condvar,
    idle_after: Duration,
    run: fn(&J, &Arc<Stop>) -> Ran,
}

impl<J: Send + Sync + 'static> Scheduler<J> {
    pub(crate) fn new(idle_after: Duration, run: fn(&J, &Arc<Stop>) -> Ran) -> Self {
        Self {
            state: Mutex::new(State {
                queue: VecDeque::new(),
                keys: HashSet::new(),
                running: None,
                holds: 0,
                busy: 0,
                last_activity: Instant::now(),
                worker: false,
                shut: false,
            }),
            wake: Condvar::new(),
            idle_after,
            run,
        }
    }

    fn lock(&self) -> MutexGuard<'_, State<J>> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Something that must not wait behind a job, or be timed beside one,
    /// happens now: the running job stops and the idle time starts again.
    pub(crate) fn activity(&self) {
        let mut state = self.lock();
        state.last_activity = Instant::now();
        stop_running(&mut state);
    }

    /// Queues `job` under `key` unless a job of that key is queued or
    /// running; `false` when it was not queued (also after `shut`).
    pub(crate) fn enqueue(self: &Arc<Self>, key: String, job: J) -> bool {
        let mut state = self.lock();
        if state.shut || !state.keys.insert(key.clone()) {
            return false;
        }
        state.queue.push_back((key, Arc::new(job)));
        if !state.worker {
            let scheduler = self.clone();
            let started = std::thread::Builder::new()
                .name("ai-gpu-checks".to_string())
                .spawn(move || scheduler.work());
            match started {
                Ok(_) => state.worker = true,
                Err(e) => log::warn!("AI: GPU checks not started: {e}"),
            }
        }
        self.wake.notify_all();
        true
    }

    /// A model run starts; nothing runs until the guard is dropped and the
    /// app was idle for `idle_after` after it.
    pub(crate) fn busy(self: &Arc<Self>) -> Busy<J> {
        let mut state = self.lock();
        state.busy += 1;
        state.last_activity = Instant::now();
        stop_running(&mut state);
        Busy {
            scheduler: self.clone(),
            hold: false,
        }
    }

    /// Like `busy`, for a batch (an export): when this returns, no job
    /// commits a result until the guard is dropped.
    pub(crate) fn hold(self: &Arc<Self>) -> Busy<J> {
        let mut state = self.lock();
        state.holds += 1;
        state.last_activity = Instant::now();
        stop_running(&mut state);
        Busy {
            scheduler: self.clone(),
            hold: true,
        }
    }

    /// The app quits: the running job stops, nothing runs or commits again.
    pub(crate) fn shut(&self) {
        let mut state = self.lock();
        state.shut = true;
        let queued: Vec<_> = state.queue.drain(..).collect();
        for (key, _) in queued {
            state.keys.remove(&key);
        }
        stop_running(&mut state);
        self.wake.notify_all();
    }

    /// Keys queued or running.
    #[cfg(test)]
    fn pending(&self) -> usize {
        self.lock().keys.len()
    }

    fn work(&self) {
        loop {
            let (key, job, stop) = {
                let mut state = self.lock();
                loop {
                    if state.shut {
                        return;
                    }
                    if state.queue.is_empty() || state.holds > 0 || state.busy > 0 {
                        state = self.wake.wait(state).unwrap_or_else(|e| e.into_inner());
                        continue;
                    }
                    let idle = state.last_activity.elapsed();
                    if idle < self.idle_after {
                        state = self
                            .wake
                            .wait_timeout(state, self.idle_after - idle)
                            .unwrap_or_else(|e| e.into_inner())
                            .0;
                        continue;
                    }
                    break;
                }
                let (key, job) = state.queue.pop_front().unwrap();
                let stop = Stop::new();
                state.running = Some(stop.clone());
                (key, job, stop)
            };
            let ran = (self.run)(&job, &stop);
            let mut state = self.lock();
            state.running = None;
            if ran == Ran::Stopped && !state.shut {
                state.queue.push_front((key, job));
            } else {
                state.keys.remove(&key);
            }
        }
    }
}

fn stop_running<J>(state: &mut State<J>) {
    if let Some(stop) = state.running.take() {
        stop.stop();
    }
}

/// Holds the queue while it lives (`Scheduler::busy`, `Scheduler::hold`).
#[must_use = "the queue is held only while the guard lives"]
pub struct Busy<J: Send + Sync + 'static> {
    scheduler: Arc<Scheduler<J>>,
    hold: bool,
}

impl<J: Send + Sync + 'static> Drop for Busy<J> {
    fn drop(&mut self) {
        let mut state = self.scheduler.lock();
        if self.hold {
            state.holds -= 1;
        } else {
            state.busy -= 1;
        }
        state.last_activity = Instant::now();
        self.scheduler.wake.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A job that records its runs and waits `ms` or until stopped.
    struct Job {
        ms: u64,
        runs: AtomicU32,
        results: Mutex<Vec<&'static str>>,
    }

    impl Job {
        fn new(ms: u64) -> Self {
            Self {
                ms,
                runs: AtomicU32::new(0),
                results: Mutex::new(Vec::new()),
            }
        }
    }

    fn run(job: &Job, stop: &Arc<Stop>) -> Ran {
        job.runs.fetch_add(1, Ordering::SeqCst);
        let started = Instant::now();
        while started.elapsed() < Duration::from_millis(job.ms) {
            if stop.stopped() {
                return Ran::Stopped;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        stop.commit(|| job.results.lock().unwrap().push("verdict"));
        Ran::Done
    }

    const IDLE: Duration = Duration::from_millis(60);

    fn scheduler() -> Arc<Scheduler<Job>> {
        Arc::new(Scheduler::new(IDLE, run))
    }

    fn wait_until(what: impl Fn() -> bool) -> bool {
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(5) {
            if what() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        false
    }

    /// Queued while a model runs (the first click): runs only after the
    /// run ended and the app was idle, then commits its result once.
    #[test]
    fn a_job_waits_for_the_running_model_and_the_idle_time() {
        let checks = scheduler();
        let click = checks.busy();
        assert!(checks.enqueue("subject".into(), Job::new(1)));
        std::thread::sleep(IDLE * 3);
        let queued = checks.lock().queue.front().unwrap().1.clone();
        assert_eq!(
            queued.runs.load(Ordering::SeqCst),
            0,
            "ran beside the click"
        );
        let released = Instant::now();
        drop(click);
        assert!(wait_until(|| checks.pending() == 0));
        assert!(released.elapsed() >= IDLE);
        assert_eq!(queued.runs.load(Ordering::SeqCst), 1);
        assert_eq!(*queued.results.lock().unwrap(), ["verdict"]);
    }

    #[test]
    fn a_key_is_queued_once() {
        let checks = scheduler();
        let _busy = checks.busy();
        assert!(checks.enqueue("subject".into(), Job::new(1)));
        assert!(!checks.enqueue("subject".into(), Job::new(1)));
        assert!(checks.enqueue("sky".into(), Job::new(1)));
        assert_eq!(checks.pending(), 2);
    }

    /// Activity (an edit, a click) stops the running job without a result;
    /// it runs again, whole, once the app is idle again.
    #[test]
    fn activity_stops_a_running_job_and_it_runs_again_later() {
        let checks = scheduler();
        assert!(checks.enqueue("subject".into(), Job::new(400)));
        let job = {
            assert!(wait_until(|| checks.lock().running.is_some()));
            // Running: it is out of the queue; find it again after the stop.
            checks.activity();
            assert!(wait_until(|| !checks.lock().queue.is_empty()));
            checks.lock().queue.front().unwrap().1.clone()
        };
        assert_eq!(job.runs.load(Ordering::SeqCst), 1);
        assert!(job.results.lock().unwrap().is_empty());
        assert!(wait_until(|| checks.pending() == 0));
        assert_eq!(job.runs.load(Ordering::SeqCst), 2);
        assert_eq!(*job.results.lock().unwrap(), ["verdict"]);
    }

    /// An export holds the queue: nothing runs and nothing commits while it
    /// lives, so a verdict cannot change between its photos.
    #[test]
    fn a_hold_stops_the_job_and_blocks_its_result_until_released() {
        let checks = scheduler();
        assert!(checks.enqueue("denoise".into(), Job::new(300)));
        assert!(wait_until(|| checks.lock().running.is_some()));
        let export = checks.hold();
        assert!(wait_until(|| !checks.lock().queue.is_empty()));
        let job = checks.lock().queue.front().unwrap().1.clone();
        std::thread::sleep(IDLE * 4);
        assert!(job.results.lock().unwrap().is_empty());
        assert_eq!(job.runs.load(Ordering::SeqCst), 1);
        drop(export);
        assert!(wait_until(|| checks.pending() == 0));
        assert_eq!(*job.results.lock().unwrap(), ["verdict"]);
    }

    /// A commit already under way finishes before `hold` returns; none
    /// starts after it.
    #[test]
    fn nothing_commits_after_a_stop() {
        let stop = Stop::new();
        assert!(stop.commit(|| {}));
        stop.stop();
        let mut wrote = false;
        assert!(!stop.commit(|| wrote = true));
        assert!(!wrote);
        // A callback registered after the stop runs at once.
        let called = Arc::new(AtomicU32::new(0));
        let counted = called.clone();
        stop.on_stop(move || {
            counted.fetch_add(1, Ordering::SeqCst);
        });
        assert_eq!(called.load(Ordering::SeqCst), 1);
    }

    /// Quitting stops the running job, drops the queue and writes nothing.
    #[test]
    fn quitting_cancels_the_running_and_the_queued_jobs() {
        let checks = scheduler();
        assert!(checks.enqueue("subject".into(), Job::new(400)));
        assert!(checks.enqueue("sky".into(), Job::new(1)));
        assert!(wait_until(|| checks.lock().running.is_some()));
        let running = checks.lock().running.clone().unwrap();
        checks.shut();
        assert!(running.stopped());
        assert!(wait_until(|| checks.pending() == 0));
        assert!(checks.lock().queue.is_empty());
        assert!(!checks.enqueue("depth".into(), Job::new(1)));
    }
}
