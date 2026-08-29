//! One heavy job at a time.
//!
//! The agent has three threads with their own SQLite connections (scheduler,
//! watcher, RPC). Each writes in short transactions, but a full scan, a
//! classification pass and an analysis rebuild all contend for the write
//! lock, and a waiter that sits at the busy timeout fails with "database is
//! locked". Serialising the heavy jobs in-process means they queue instead.

use std::sync::{Mutex, MutexGuard, TryLockError};

static HEAVY: Mutex<()> = Mutex::new(());

/// Block until no other heavy job is running.
pub fn heavy() -> MutexGuard<'static, ()> {
    match HEAVY.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    }
}

/// Take the heavy-job slot only if it is free right now.
pub fn try_heavy() -> Option<MutexGuard<'static, ()>> {
    match HEAVY.try_lock() {
        Ok(g) => Some(g),
        Err(TryLockError::Poisoned(p)) => Some(p.into_inner()),
        Err(TryLockError::WouldBlock) => None,
    }
}
