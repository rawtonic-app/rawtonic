//! The export queue's managed state (plan step 3.3): the persistent queue,
//! the jobs the runner is working on, whether it runs and whether the queue
//! is held, the Edit With runs a quit cut short, and the running export
//! task's cancel token. See `README.md` here.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use rawtonic_export::queue::ExportQueue;

use super::lock::lock_recover;
use crate::export_queue::{EditWithRun, RunningJob};

/// The export queue's state. Locks: `README.md` here.
#[derive(Default)]
pub struct ExportState {
    /// The queue (`export-queue.sqlite`), opened at start-up.
    pub(crate) queue: Mutex<Option<ExportQueue>>,
    /// The jobs of the running export. The runner sets them before their
    /// export task starts and clears them after it ended, both under this
    /// lock, so while it names a job the active export task is that job's.
    pub(crate) running_jobs: Mutex<Vec<RunningJob>>,
    /// The runner is working through the queue (`kick`).
    pub(crate) runner_active: AtomicBool,
    /// Waiting jobs do not start (Hold queue, Cancel, a restart with jobs left).
    pub(crate) held: AtomicBool,
    /// The Edit With runs the last quit or crash cut short, offered once.
    pub(crate) interrupted_edits: Mutex<Vec<EditWithRun>>,
    /// The cancel flag of the export task that runs now, if one does.
    pub task_token: Arc<Mutex<Option<Arc<AtomicBool>>>>,
}

impl ExportState {
    pub fn is_held(&self) -> bool {
        self.held.load(Ordering::SeqCst)
    }

    pub fn set_held(&self, held: bool) {
        self.held.store(held, Ordering::SeqCst);
    }

    /// The interrupted Edit With runs, each handed out once.
    pub(crate) fn take_interrupted_edits(&self) -> Vec<EditWithRun> {
        std::mem::take(&mut *lock_recover(&self.interrupted_edits))
    }
}
