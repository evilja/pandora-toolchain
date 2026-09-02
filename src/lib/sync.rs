use std::sync::{Mutex, MutexGuard};

// `Mutex::lock().unwrap()` turns one panic into every later panic. A thread that dies while
// holding a lock poisons it, and from then on every caller — including ones that had nothing to do
// with the original fault — panics on acquisition instead of doing its work.
//
// For the state this crate keeps behind a mutex that is the wrong trade. The link board, the
// node's pending-report buffer and the asset hash cache are all plain maps: a panic mid-update
// leaves one entry in whatever state it reached, which the next reader either finds or does not.
// Losing the whole coordinator's cluster state, or a node's ability to report anything for the
// life of the process, is strictly worse — and it is invisible, because the second panic names the
// lock rather than the fault that poisoned it.
//
// So poisoning is recovered from rather than propagated. The first panic still reaches the log
// where it happened; nothing after it is turned into a second, less informative failure.
pub fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    // The property that matters: a panic inside one critical section must not take every later
    // caller with it. Without this, one bad payload on one link route stops a coordinator from
    // scheduling anything at all, and the panic that says so names the mutex and not the cause.
    #[test]
    fn a_poisoned_lock_is_still_usable() {
        let value = Arc::new(Mutex::new(1u32));
        let poisoner = Arc::clone(&value);
        std::thread::spawn(move || {
            let _guard = poisoner.lock().unwrap();
            panic!("holding the lock");
        })
        .join()
        .expect_err("the thread was supposed to panic");

        assert!(value.lock().is_err(), "the lock should now be poisoned");
        *lock(&value) += 1;
        assert_eq!(*lock(&value), 2);
    }
}
