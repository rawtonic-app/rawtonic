//! The merge dialog's pending result and the locks ordering its jobs.
use crate::merges::MergeOutcome;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Default)]
pub struct MergeState {
    /// A finished merge waiting for `save_merge`.
    pub result: Arc<Mutex<Option<MergeOutcome>>>,
    /// Last run started, or zero after the dialog closes.
    pub current_job: Arc<AtomicU64>,
    /// One merge at a time: each holds all its frames and cannot be interrupted.
    pub running: tokio::sync::Mutex<()>,
}

impl MergeState {
    pub fn is_current(&self, job: u64) -> bool {
        self.current_job.load(Ordering::SeqCst) == job
    }
}
