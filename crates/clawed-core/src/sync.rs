//! Synchronization helpers for graceful poison recovery.
//!
//! When a thread panics while holding a `std::sync::Mutex` or `RwLock`, the
//! lock is "poisoned". Calling `.lock().unwrap()` (or `.read().unwrap()`,
//! `.write().unwrap()`) will panic on subsequent access.
//!
//! The helpers in this module recover from poison by returning the inner data
//! via `poisoned.into_inner()`, allowing the application to continue rather
//! than crashing.

use std::sync::{Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};

/// Lock a [`Mutex`], recovering from poison if necessary.
pub fn lock_or_recover<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Read-lock an [`RwLock`], recovering from poison if necessary.
pub fn read_or_recover<T>(lock: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    lock.read().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Write-lock an [`RwLock`], recovering from poison if necessary.
pub fn write_or_recover<T>(lock: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    lock.write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}
