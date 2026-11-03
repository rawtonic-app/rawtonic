//! Generation-based cancellation shared by decode callers.
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

/// A request remains current while its counter equals the captured generation.
#[derive(Clone, Debug)]
pub struct CancelToken {
    tracker: Arc<AtomicUsize>,
    generation: usize,
}

impl CancelToken {
    pub fn new(tracker: Arc<AtomicUsize>, generation: usize) -> Self {
        Self {
            tracker,
            generation,
        }
    }

    #[inline]
    pub fn is_cancelled(&self) -> bool {
        self.tracker.load(Ordering::SeqCst) != self.generation
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_the_current_generation_including_after_clone() {
        let tracker = Arc::new(AtomicUsize::new(7));
        let token = CancelToken::new(Arc::clone(&tracker), 7);
        let cloned = token.clone();
        assert!(!token.is_cancelled());
        tracker.store(8, Ordering::SeqCst);
        assert!(token.is_cancelled());
        assert!(cloned.is_cancelled());
        tracker.store(7, Ordering::SeqCst);
        assert!(!token.is_cancelled());
    }
}
