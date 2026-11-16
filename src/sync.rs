//! Small concurrency helpers.

use std::sync::{Mutex, MutexGuard};

/// Lock, ignoring poisoning: every mutex in moth guards plain data that stays consistent even if
/// a holder panicked, and a panic elsewhere shouldn't take playback or IPC down with it.
pub fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}
