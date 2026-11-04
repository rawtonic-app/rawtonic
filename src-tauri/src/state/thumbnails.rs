//! Thumbnail generation's state (plan step 3.3): the viewport queue and its
//! workers' bookkeeping, the progress shown in the Library, the cancel flag
//! and the bases thumbnails are made from. See `README.md` here.

use std::collections::{HashSet, VecDeque};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Condvar, Mutex};

use image::DynamicImage;

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
