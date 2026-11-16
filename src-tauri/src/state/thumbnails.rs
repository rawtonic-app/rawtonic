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

/// The viewport queue. A new viewport's photos are first looked up in the
/// disk cache all together, by a thread of its own (never behind decodes of
/// rows a fast scroll passed), so a screen of cached thumbnails reaches the
/// webview as one batch event (two events per photo queued behind every
/// row a fast scroll passed made the visible tiles wait, G53 DKT1); only
/// the misses are then decoded, first visible first.
#[derive(Default)]
pub struct ThumbnailQueue {
    lookup: Vec<String>,
    decode: VecDeque<String>,
    generation: u64,
}

/// The two kinds of thumbnail worker.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ThumbnailWork {
    Lookup,
    Decode,
}

/// What a thumbnail worker does next.
#[derive(Debug, PartialEq)]
pub enum ThumbnailJob {
    /// Look up a viewport's photos in the disk cache.
    Lookup { paths: Vec<String>, generation: u64 },
    /// Make the thumbnails of one photo the cache did not have.
    Decode(String),
}

impl ThumbnailQueue {
    /// Replaces the queue with a viewport's photos, in visible order.
    pub fn replace(&mut self, paths: Vec<String>) {
        self.generation += 1;
        self.lookup = paths;
        self.decode.clear();
    }

    pub fn clear(&mut self) {
        self.replace(Vec::new());
    }

    pub fn is_empty(&self) -> bool {
        self.lookup.is_empty() && self.decode.is_empty()
    }

    pub fn len(&self) -> usize {
        self.lookup.len() + self.decode.len()
    }

    /// Whether a worker of this kind has a job.
    pub fn has(&self, work: ThumbnailWork) -> bool {
        match work {
            ThumbnailWork::Lookup => !self.lookup.is_empty(),
            ThumbnailWork::Decode => !self.decode.is_empty(),
        }
    }

    /// The next job of this kind: a whole viewport's lookup, or one decode.
    pub fn take(&mut self, work: ThumbnailWork) -> Option<ThumbnailJob> {
        match work {
            ThumbnailWork::Lookup => (!self.lookup.is_empty()).then(|| ThumbnailJob::Lookup {
                paths: std::mem::take(&mut self.lookup),
                generation: self.generation,
            }),
            ThumbnailWork::Decode => self.decode.pop_front().map(ThumbnailJob::Decode),
        }
    }

    /// Queues a lookup's misses for decoding, unless a newer viewport has
    /// replaced its own meanwhile; returns whether its viewport is current.
    pub fn queue_misses(&mut self, generation: u64, misses: Vec<String>) -> bool {
        if generation != self.generation {
            return false;
        }
        self.decode.extend(misses);
        true
    }
}

pub struct ThumbnailManager {
    pub queue: Mutex<ThumbnailQueue>,
    pub cvar: Condvar,
    pub processing_now: Mutex<HashSet<String>>,
    pub rotational_disk: AtomicBool,
    pub io_gate: Mutex<()>,
}

impl ThumbnailManager {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            queue: Mutex::new(ThumbnailQueue::default()),
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

    /// Counts `count` finished photos; returns (current, total), or `None`
    /// once the generation is complete (the counter is then zeroed).
    pub fn complete(&self, count: usize) -> Option<(usize, usize)> {
        let mut tracker = lock_recover(&self.progress);
        tracker.completed += count;
        if tracker.completed >= tracker.total {
            tracker.total = 0;
            tracker.completed = 0;
            None
        } else {
            Some((tracker.completed, tracker.total))
        }
    }
}
