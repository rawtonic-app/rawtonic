//! Thumbnail generation's state (plan step 3.3): the viewport queue and its
//! workers' bookkeeping, the progress shown in the Library, the cancel flag
//! and the bases thumbnails are made from. See `README.md` here.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Condvar, Mutex};

use image::DynamicImage;

use super::lock::lock_recover;

pub struct ThumbnailProgressTracker {
    pub total: usize,
    pub completed: usize,
}

pub struct ThumbnailManager {
    pub queue: Mutex<VecDeque<String>>,
    pub cvar: Condvar,
    pub processing_now: Mutex<HashSet<String>>,
    pub rotational_disk: AtomicBool,
    pub io_gate: Mutex<()>,
}

impl ThumbnailManager {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            queue: Mutex::new(VecDeque::new()),
            cvar: Condvar::new(),
            processing_now: Mutex::new(HashSet::new()),
            rotational_disk: AtomicBool::new(false),
            io_gate: Mutex::new(()),
        })
    }
}

/// A thumbnail's base: recipe hash, image, scale, mask source, and the
/// photo's size when the base was made from its full decode.
pub type ThumbnailGeometryEntry = (
    u64,
    Arc<DynamicImage>,
    f32,
    Option<crate::mask_generation::ParametricMaskSource>,
    Option<(u32, u32)>,
);

/// Thumbnail generation's managed state. Locks: `README.md` here.
pub struct ThumbnailState {
    /// The viewport queue and its workers (shared with the worker threads).
    pub manager: Arc<ThumbnailManager>,
    /// Set by `cancel_thumbnail_generation`; the workers then drop the rest
    /// of the queue at the next photo, and `update_thumbnail_queue` clears it
    /// when a new generation starts.
    pub cancelled: AtomicBool,
    /// The Library's progress counter.
    pub progress: Mutex<ThumbnailProgressTracker>,
    /// Thumbnail bases by photo path, at most eight.
    pub geometry_cache: Mutex<HashMap<String, ThumbnailGeometryEntry>>,
    /// Admits one restoration of a missing cache entry at a time
    /// (`restore_thumbnail`), apart from the viewport queue.
    pub restoration_slot: Arc<tokio::sync::Semaphore>,
}

impl Default for ThumbnailState {
    fn default() -> Self {
        Self {
            manager: ThumbnailManager::new(),
            cancelled: AtomicBool::new(false),
            progress: Mutex::new(ThumbnailProgressTracker {
                total: 0,
                completed: 0,
            }),
            geometry_cache: Mutex::new(HashMap::new()),
            restoration_slot: Arc::new(tokio::sync::Semaphore::new(1)),
        }
    }
}

impl ThumbnailState {
    /// Zeroes the progress counter.
    pub fn reset_progress(&self) {
        let mut tracker = lock_recover(&self.progress);
        tracker.total = 0;
        tracker.completed = 0;
    }

    /// Adds `count` photos to the total; returns (current, total).
    pub fn add_to_total(&self, count: usize) -> (usize, usize) {
        let mut tracker = lock_recover(&self.progress);
        tracker.total += count;
        (tracker.completed, tracker.total)
    }

    /// Sets the total to what is done plus `queued`; returns (current, total).
    pub fn set_queued(&self, queued: usize) -> (usize, usize) {
        let mut tracker = lock_recover(&self.progress);
        tracker.total = tracker.completed + queued;
        (tracker.completed, tracker.total)
    }

    /// Counts one finished photo; returns (current, total), or `None` once
    /// the generation is complete (the counter is then zeroed).
    pub fn complete_one(&self) -> Option<(usize, usize)> {
        let mut tracker = lock_recover(&self.progress);
        tracker.completed += 1;
        if tracker.completed >= tracker.total {
            tracker.total = 0;
            tracker.completed = 0;
            None
        } else {
            Some((tracker.completed, tracker.total))
        }
    }
}
