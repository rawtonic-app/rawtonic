//! While the user edits in Develop, a batch export runs on a few CPUs.
//!
//! Export threads run below the editor's priority (`export_processing`), yet
//! during a 200-photo export on a 6-core laptop (Radeon 680M, RADV) drags
//! drew 27 previews against 59 idle, and 23 with the export threads on
//! SCHED_IDLE: the editor lacked not CPU time but what busy cores share
//! (memory bandwidth, caches, package power). Its webview took 50–75 ms per
//! preview instead of 10. With the export on 2 of the 12 CPUs, drags drew 49
//! (4 CPUs: 45; notes: parity-gaps/G44). So while the Develop view renders,
//! and for [`EDITING_HOLD`] after its last frame, export threads run on
//! [`editing_cpus`] only; then on all again. Linux only: macOS has no thread
//! affinity, and its exports did not starve the editor (G44, Mac mini).

use std::sync::Arc;
#[cfg(target_os = "linux")]
use std::sync::{Mutex, Weak};
#[cfg(target_os = "linux")]
use std::time::Duration;

/// How long after the Develop view's last frame the export keeps to few
/// CPUs: longer than the pauses within one edit.
#[cfg(target_os = "linux")]
const EDITING_HOLD: Duration = Duration::from_secs(1);

/// How often the export checks whether the Develop view renders.
#[cfg(target_os = "linux")]
const POLL: Duration = Duration::from_millis(20);

/// The CPUs an export runs on while the user edits: the last sixth of the
/// CPUs it may use, at least one.
#[cfg(any(target_os = "linux", test))]
fn editing_cpus(all: &[usize]) -> &[usize] {
    &all[all.len() - (all.len() / 6).max(1).min(all.len())..]
}

/// The threads of one export run.
pub(crate) struct ExportCpus {
    #[cfg(target_os = "linux")]
    all: Vec<usize>,
    #[cfg(target_os = "linux")]
    threads: Mutex<Threads>,
}

#[cfg(target_os = "linux")]
#[derive(Default)]
struct Threads {
    ids: Vec<libc::pid_t>,
    confined: bool,
}

#[cfg(target_os = "linux")]
impl ExportCpus {
    /// The CPUs the calling thread may use, which export threads inherit.
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            all: allowed_cpus(),
            threads: Mutex::default(),
        })
    }

    /// Adds the calling thread, from an export pool's start handler.
    pub(crate) fn add_current_thread(&self) {
        // SAFETY: no arguments; returns the calling thread's id.
        let id = unsafe { libc::gettid() };
        let mut threads = self.threads.lock().unwrap_or_else(|e| e.into_inner());
        if threads.confined {
            set_cpus(id, editing_cpus(&self.all));
        }
        threads.ids.push(id);
    }

    fn confine(&self, editing: bool) {
        let mut threads = self.threads.lock().unwrap_or_else(|e| e.into_inner());
        if threads.confined == editing {
            return;
        }
        threads.confined = editing;
        let cpus = if editing {
            editing_cpus(&self.all)
        } else {
            &self.all
        };
        for &id in &threads.ids {
            set_cpus(id, cpus);
        }
    }

    /// Confines the threads while `editing` holds, for as long as the run
    /// keeps `self`.
    fn follow(self: &Arc<Self>, editing: impl Fn() -> bool + Send + 'static) {
        let run = Arc::downgrade(self);
        let follow = move || {
            let run: Weak<Self> = run;
            loop {
                std::thread::sleep(POLL);
                let Some(cpus) = run.upgrade() else { break };
                cpus.confine(editing());
            }
        };
        if let Err(error) = std::thread::Builder::new()
            .name("export-cpus".into())
            .spawn(follow)
        {
            log::warn!("Export keeps all CPUs while editing: {error}");
        }
    }

    /// Follows the Develop view's renders.
    pub(crate) fn follow_editing(self: &Arc<Self>) {
        self.follow(|| crate::gpu_processing::foreground_rendered_within(EDITING_HOLD));
    }
}

#[cfg(target_os = "linux")]
fn allowed_cpus() -> Vec<usize> {
    // SAFETY: a zeroed cpu_set_t is the empty set; the kernel fills it for
    // the calling thread and the CPU_ISSET reads stay within it.
    unsafe {
        let mut set: libc::cpu_set_t = std::mem::zeroed();
        if libc::sched_getaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &mut set) != 0 {
            return Vec::new();
        }
        (0..libc::CPU_SETSIZE as usize)
            .filter(|&cpu| libc::CPU_ISSET(cpu, &set))
            .collect()
    }
}

/// A thread that has ended keeps its id in `ids`; the call then fails, and
/// changes nothing (a new thread of another export run has its own set).
#[cfg(target_os = "linux")]
fn set_cpus(id: libc::pid_t, cpus: &[usize]) {
    if cpus.is_empty() {
        return;
    }
    // SAFETY: as in `allowed_cpus`; CPU numbers come from sched_getaffinity,
    // so below CPU_SETSIZE.
    unsafe {
        let mut set: libc::cpu_set_t = std::mem::zeroed();
        for &cpu in cpus {
            libc::CPU_SET(cpu, &mut set);
        }
        libc::sched_setaffinity(id, std::mem::size_of::<libc::cpu_set_t>(), &set);
    }
}

#[cfg(not(target_os = "linux"))]
impl ExportCpus {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {})
    }

    pub(crate) fn add_current_thread(&self) {}

    pub(crate) fn follow_editing(self: &Arc<Self>) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editing_takes_the_last_sixth_of_the_cpus() {
        let cpus: Vec<usize> = (0..12).collect();
        assert_eq!(editing_cpus(&cpus), &[10, 11]);
        assert_eq!(editing_cpus(&cpus[..4]), &[3]);
        assert_eq!(editing_cpus(&[5]), &[5]);
        assert_eq!(editing_cpus(&[]), &[] as &[usize]);
        let many: Vec<usize> = (0..32).collect();
        assert_eq!(editing_cpus(&many), &[27, 28, 29, 30, 31]);
    }

    #[cfg(target_os = "linux")]
    fn cpus_of(id: libc::pid_t) -> Vec<usize> {
        // SAFETY: as in `allowed_cpus`, for another thread of this process.
        unsafe {
            let mut set: libc::cpu_set_t = std::mem::zeroed();
            assert_eq!(
                libc::sched_getaffinity(id, std::mem::size_of::<libc::cpu_set_t>(), &mut set),
                0
            );
            (0..libc::CPU_SETSIZE as usize)
                .filter(|&cpu| libc::CPU_ISSET(cpu, &set))
                .collect()
        }
    }

    /// Export threads, also those that start meanwhile, keep to the editing
    /// CPUs while the user edits, and get all back afterwards.
    #[cfg(target_os = "linux")]
    #[test]
    fn export_threads_keep_to_few_cpus_while_editing() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::mpsc::channel;

        let run = ExportCpus::new();
        if run.all.len() < 2 {
            return; // nothing to confine on one CPU
        }
        let editing_set = editing_cpus(&run.all).to_vec();
        let thread = |run: &Arc<ExportCpus>| {
            let (started, id) = channel();
            let (stop, stopped) = channel::<()>();
            let run = Arc::clone(run);
            let handle = std::thread::spawn(move || {
                run.add_current_thread();
                // SAFETY: as in `add_current_thread`.
                started.send(unsafe { libc::gettid() }).unwrap();
                let _ = stopped.recv();
            });
            (id.recv().unwrap(), stop, handle)
        };
        let until = |id, cpus: &[usize]| {
            let start = std::time::Instant::now();
            while cpus_of(id) != cpus {
                assert!(
                    start.elapsed() < Duration::from_secs(5),
                    "{:?} instead of {cpus:?}",
                    cpus_of(id)
                );
                std::thread::sleep(Duration::from_millis(5));
            }
        };

        let (first, stop_first, first_handle) = thread(&run);
        assert_eq!(cpus_of(first), run.all);
        let editing = Arc::new(AtomicBool::new(false));
        run.follow({
            let editing = Arc::clone(&editing);
            move || editing.load(Ordering::SeqCst)
        });
        std::thread::sleep(POLL * 3);
        assert_eq!(cpus_of(first), run.all, "not while nobody edits");

        editing.store(true, Ordering::SeqCst);
        until(first, &editing_set);
        let (second, stop_second, second_handle) = thread(&run);
        assert_eq!(cpus_of(second), editing_set, "started while editing");

        editing.store(false, Ordering::SeqCst);
        until(first, &run.all);
        until(second, &run.all);
        for (stop, handle) in [(stop_first, first_handle), (stop_second, second_handle)] {
            stop.send(()).unwrap();
            handle.join().unwrap();
        }
    }
}
