// SPDX-License-Identifier: AGPL-3.0-or-later
//! The session a model runs on, and runs on a GPU that never wait for a
//! hung GPU (gap G76: AI Denoise stalled for over 13 minutes on a Radeon
//! 680M under Windows, no progress, no CPU use).
//!
//! What is known about the session (its provider, runs, the slowest run)
//! is kept apart from the session's own lock, so asking for the provider
//! never waits for a run; beside the session is when the run that holds it
//! began. A watched run on a GPU
//! goes to a thread of its own; the caller waits for it and looks every
//! moment whether the run that holds the session has taken longer than its
//! limit. Then that run is left behind with its session (it may never
//! return; its memory stays taken) and the model moves to a new CPU session
//! for good, which every later and every waiting caller uses. Generic over
//! the session so the tests can hang one on purpose.

use std::sync::mpsc::RecvTimeoutError;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use crate::Provider;

/// How long a GPU run may take before it is left: a first run may compile
/// shaders (BiRefNet's first WebGPU run on a Radeon 680M: 77 s), later ones
/// get ten times the slowest so far, at least a minute (AI Denoise's tiles
/// there: 8.4 s).
pub(crate) fn watch_limit(runs: u32, slowest: Duration) -> Duration {
    if runs == 0 {
        Duration::from_secs(300)
    } else {
        (slowest * 10).max(Duration::from_secs(60))
    }
}

/// How often a waiting caller looks whether the run it waits behind hangs.
const LOOK: Duration = Duration::from_millis(250);

/// A session and when the run that holds it began.
pub(crate) struct Held<S> {
    session: Mutex<S>,
    since: Mutex<Option<Instant>>,
}

impl<S> Held<S> {
    fn new(session: S) -> Arc<Self> {
        Arc::new(Self {
            session: Mutex::new(session),
            since: Mutex::new(None),
        })
    }

    /// Runs `f` holding the session, its start noted for the waiters.
    pub(crate) fn with<T>(&self, f: impl FnOnce(&mut S) -> T) -> T {
        let mut session = self.session.lock().unwrap();
        *self.since.lock().unwrap() = Some(Instant::now());
        let value = f(&mut session);
        *self.since.lock().unwrap() = None;
        value
    }

    /// True when the run that holds the session began more than `limit` ago.
    fn hung(&self, limit: Duration) -> bool {
        self.since
            .lock()
            .unwrap()
            .is_some_and(|since| since.elapsed() > limit)
    }
}

struct Current<S> {
    session: Arc<Held<S>>,
    provider: Provider,
    runs: u32,
    slowest: Duration,
}

pub(crate) struct Slot<S> {
    name: &'static str,
    current: Mutex<Current<S>>,
    limit: fn(u32, Duration) -> Duration,
}

impl<S: Send + 'static> Slot<S> {
    pub(crate) fn new(name: &'static str, session: S, provider: Provider) -> Self {
        Self::with_limit(name, session, provider, watch_limit)
    }

    pub(crate) fn with_limit(
        name: &'static str,
        session: S,
        provider: Provider,
        limit: fn(u32, Duration) -> Duration,
    ) -> Self {
        Self {
            name,
            current: Mutex::new(Current {
                session: Held::new(session),
                provider,
                runs: 0,
                slowest: Duration::ZERO,
            }),
            limit,
        }
    }

    pub(crate) fn provider(&self) -> Provider {
        self.current.lock().unwrap().provider
    }

    fn snapshot(&self) -> (Arc<Held<S>>, Provider, Duration) {
        let current = self.current.lock().unwrap();
        (
            current.session.clone(),
            current.provider,
            (self.limit)(current.runs, current.slowest),
        )
    }

    /// Takes `session` on `provider` (a GPU check found it faster) if the
    /// model is on the CPU now; else gives it back.
    pub(crate) fn adopt(&self, session: S, provider: Provider) -> Result<(), S> {
        let mut current = self.current.lock().unwrap();
        if current.provider != Provider::Cpu {
            return Err(session);
        }
        log::info!(
            "AI {}: on {provider} from now on (its check found it faster than the CPU)",
            self.name
        );
        *current = Current {
            session: Held::new(session),
            provider,
            runs: 0,
            slowest: Duration::ZERO,
        };
        Ok(())
    }

    /// The session now, without leaving a GPU on failure (benchmarks).
    pub(crate) fn session(&self) -> Arc<Held<S>> {
        self.snapshot().0
    }

    /// Notes a finished run of `session` (if it is still the current one).
    fn finished(&self, session: &Arc<Held<S>>, provider: Provider, took: Duration) {
        let mut current = self.current.lock().unwrap();
        if !Arc::ptr_eq(&current.session, session) {
            return;
        }
        current.runs += 1;
        current.slowest = current.slowest.max(took);
        if current.runs == 1 {
            log::info!(
                "AI {}: first run on {provider} in {} ms",
                self.name,
                took.as_millis()
            );
        }
    }

    /// Moves to the CPU, unless another caller did already (`old` is no
    /// longer current): the session to use from now on.
    fn leave(
        &self,
        old: &Arc<Held<S>>,
        why: &str,
        cpu: impl FnOnce() -> Result<S>,
    ) -> Result<Arc<Held<S>>> {
        let mut current = self.current.lock().unwrap();
        if Arc::ptr_eq(&current.session, old) {
            log::warn!("AI {}: {why}; using the CPU from now on", self.name);
            *current = Current {
                session: Held::new(cpu()?),
                provider: Provider::Cpu,
                runs: 0,
                slowest: Duration::ZERO,
            };
        }
        Ok(current.session.clone())
    }

    /// Runs `f` on the CPU session `session` (after a move), counting it.
    fn run_on_cpu<T>(
        &self,
        session: &Arc<Held<S>>,
        f: impl FnOnce(&mut S) -> Result<T>,
    ) -> Result<T> {
        let started = Instant::now();
        let value = session.with(f)?;
        self.finished(session, Provider::Cpu, started.elapsed());
        Ok(value)
    }

    /// Runs `f` on the session. When it fails on a GPU (other than being
    /// stopped on purpose, `stopped`), the model moves to the CPU (`cpu`
    /// makes its session) and `f` runs again there.
    pub(crate) fn run<T>(
        &self,
        mut f: impl FnMut(&mut S) -> Result<T>,
        stopped: impl Fn(&anyhow::Error) -> bool,
        cpu: impl FnOnce() -> Result<S>,
    ) -> Result<T> {
        let (session, provider, _) = self.snapshot();
        let started = Instant::now();
        let result = session.with(&mut f);
        match result {
            Ok(value) => {
                self.finished(&session, provider, started.elapsed());
                Ok(value)
            }
            Err(e) if provider != Provider::Cpu && !stopped(&e) => {
                let why = format!("run on {provider} failed ({e:#})");
                let next = self.leave(&session, &why, cpu)?;
                self.run_on_cpu(&next, f)
            }
            Err(e) => Err(e),
        }
    }

    /// `run` that never waits for a hung GPU (module comment).
    pub(crate) fn run_watched<T, F>(
        &self,
        f: F,
        stopped: impl Fn(&anyhow::Error) -> bool,
        cpu: impl FnOnce() -> Result<S>,
    ) -> Result<T>
    where
        T: Send + 'static,
        F: Fn(&mut S) -> Result<T> + Send + Sync + 'static,
    {
        let (session, provider, limit) = self.snapshot();
        if provider == Provider::Cpu {
            return self.run(|s| f(s), stopped, cpu);
        }
        let f = Arc::new(f);
        let (done, result) = std::sync::mpsc::channel();
        {
            let session = session.clone();
            let f = f.clone();
            std::thread::Builder::new()
                .name(format!("ai-{}", self.name))
                .spawn(move || {
                    let (value, took) = session.with(|s| {
                        let started = Instant::now();
                        (f(s), started.elapsed())
                    });
                    let _ = done.send((value, took));
                })
                .context("starting the AI run thread")?;
        }
        let why = loop {
            match result.recv_timeout(LOOK) {
                Ok((Ok(value), took)) => {
                    self.finished(&session, provider, took);
                    return Ok(value);
                }
                Ok((Err(e), _)) if stopped(&e) => return Err(e),
                Ok((Err(e), _)) => break format!("run on {provider} failed ({e:#})"),
                Err(RecvTimeoutError::Disconnected) => {
                    break format!("run on {provider} ended without a result");
                }
                Err(RecvTimeoutError::Timeout) => {}
            }
            let current = self.current.lock().unwrap();
            if !Arc::ptr_eq(&current.session, &session) {
                // Another caller left this session; it may be stuck behind
                // the hung run, so use the new one.
                break String::new();
            }
            drop(current);
            // Whichever run holds the session now, this caller's or one it
            // waits behind.
            if session.hung(limit) {
                break format!(
                    "run on {provider} did not finish in {} s and is left behind",
                    limit.as_secs()
                );
            }
        };
        let next = self.leave(&session, &why, cpu)?;
        self.run_on_cpu(&next, |s| f(s))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::anyhow;
    use std::sync::mpsc;

    /// A session that is GPU or CPU by name, and whose runs can be told to
    /// hang.
    struct Fake(&'static str);

    fn short(runs: u32, _: Duration) -> Duration {
        if runs == 0 {
            Duration::from_millis(600)
        } else {
            Duration::from_millis(400)
        }
    }

    fn never_stopped(_: &anyhow::Error) -> bool {
        false
    }

    #[test]
    fn a_gpu_run_gets_time_for_its_compile_then_ten_times_its_slowest() {
        let s = Duration::from_secs;
        assert_eq!(watch_limit(0, Duration::ZERO), s(300));
        // AI Denoise's 8.4 s tiles on the 680M (gap G76).
        assert_eq!(watch_limit(1, Duration::from_millis(8_400)), s(84));
        assert_eq!(watch_limit(5, Duration::from_millis(30)), s(60));
    }

    #[test]
    fn a_hung_gpu_run_moves_every_caller_to_the_cpu() {
        let slot = Arc::new(Slot::with_limit(
            "test",
            Fake("gpu"),
            Provider::WebGpu,
            short,
        ));
        // The first run never returns on the GPU.
        let (_never, hang) = mpsc::channel::<()>();
        let hang = Arc::new(Mutex::new(hang));
        let started = Instant::now();
        let first = {
            let slot = slot.clone();
            let hang = hang.clone();
            std::thread::spawn(move || {
                slot.run_watched(
                    move |s: &mut Fake| {
                        if s.0 == "gpu" {
                            let _ = hang.lock().unwrap().recv();
                        }
                        Ok(s.0)
                    },
                    never_stopped,
                    || Ok(Fake("cpu")),
                )
            })
        };
        std::thread::sleep(Duration::from_millis(100));
        // Two more callers queue behind the hung run.
        let waiting: Vec<_> = (0..2)
            .map(|_| {
                let slot = slot.clone();
                std::thread::spawn(move || {
                    slot.run_watched(|s: &mut Fake| Ok(s.0), never_stopped, || Ok(Fake("cpu")))
                })
            })
            .collect();
        // The provider is readable while the session is held.
        assert_eq!(slot.provider(), Provider::WebGpu);
        assert_eq!(first.join().unwrap().unwrap(), "cpu");
        for w in waiting {
            assert_eq!(w.join().unwrap().unwrap(), "cpu");
        }
        assert_eq!(slot.provider(), Provider::Cpu);
        assert!(started.elapsed() < Duration::from_secs(5));
        // Later runs go straight to the CPU.
        let later = slot.run_watched(
            |s: &mut Fake| Ok(s.0),
            never_stopped,
            || Err(anyhow!("no second CPU session")),
        );
        assert_eq!(later.unwrap(), "cpu");
    }

    #[test]
    fn callers_queued_behind_healthy_runs_stay_on_the_gpu() {
        let slot = Arc::new(Slot::with_limit(
            "test",
            Fake("gpu"),
            Provider::WebGpu,
            short,
        ));
        // Each run takes 150 ms, under the limit; six at once wait up to
        // 900 ms in the queue, longer than any limit.
        let runs: Vec<_> = (0..6)
            .map(|_| {
                let slot = slot.clone();
                std::thread::spawn(move || {
                    slot.run_watched(
                        |s: &mut Fake| {
                            std::thread::sleep(Duration::from_millis(150));
                            Ok(s.0)
                        },
                        never_stopped,
                        || Err(anyhow!("must not leave the GPU")),
                    )
                })
            })
            .collect();
        for r in runs {
            assert_eq!(r.join().unwrap().unwrap(), "gpu");
        }
        assert_eq!(slot.provider(), Provider::WebGpu);
    }

    #[test]
    fn a_failed_gpu_run_repeats_on_the_cpu_and_a_stop_does_not() {
        let slot = Slot::with_limit("test", Fake("gpu"), Provider::WebGpu, short);
        let stop = |e: &anyhow::Error| e.to_string() == "stopped";
        let stopped = slot.run_watched(
            |_: &mut Fake| Err::<&str, _>(anyhow!("stopped")),
            stop,
            || Ok(Fake("cpu")),
        );
        assert!(stopped.is_err());
        assert_eq!(slot.provider(), Provider::WebGpu);
        let value = slot.run_watched(
            |s: &mut Fake| {
                if s.0 == "gpu" {
                    Err(anyhow!("device lost"))
                } else {
                    Ok(s.0)
                }
            },
            stop,
            || Ok(Fake("cpu")),
        );
        assert_eq!(value.unwrap(), "cpu");
        assert_eq!(slot.provider(), Provider::Cpu);
    }
}
