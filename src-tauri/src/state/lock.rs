//! The one lock policy for state in `state/` (README.md): a lock poisoned by a
//! panic in another thread is recovered, not propagated. The data behind these
//! locks stays consistent across a panic (each writer replaces a whole value),
//! and a crash of the whole app over one failed job would lose the user's work.

use std::panic::Location;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};

static LOGGED: AtomicBool = AtomicBool::new(false);

/// Logs the first recovery of the run as an error, later ones at debug level
/// (a poisoned lock stays poisoned, so every later access would log again).
fn note_poisoned(at: &Location<'_>) {
    if LOGGED.swap(true, Ordering::Relaxed) {
        log::debug!("Recovered a poisoned lock at {at}");
    } else {
        log::error!(
            "A lock was poisoned by a panic in another thread (first seen at {at}); continuing with its data"
        );
    }
}

#[track_caller]
pub fn lock_recover<T: ?Sized>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => {
            note_poisoned(Location::caller());
            poisoned.into_inner()
        }
    }
}

#[track_caller]
pub fn read_recover<T: ?Sized>(lock: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    match lock.read() {
        Ok(guard) => guard,
        Err(poisoned) => {
            note_poisoned(Location::caller());
            poisoned.into_inner()
        }
    }
}

#[track_caller]
pub fn write_recover<T: ?Sized>(lock: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    match lock.write() {
        Ok(guard) => guard,
        Err(poisoned) => {
            note_poisoned(Location::caller());
            poisoned.into_inner()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn a_poisoned_lock_is_recovered_with_its_data() {
        let mutex = Arc::new(Mutex::new(1));
        let rw = Arc::new(RwLock::new(2));
        let (m, r) = (mutex.clone(), rw.clone());
        let _ = std::thread::spawn(move || {
            let _m = m.lock().unwrap();
            let _r = r.write().unwrap();
            panic!("poison both");
        })
        .join();
        assert!(mutex.is_poisoned() && rw.is_poisoned());
        *lock_recover(&mutex) += 1;
        assert_eq!(*lock_recover(&mutex), 2);
        *write_recover(&rw) += 1;
        assert_eq!(*read_recover(&rw), 3);
    }
}
