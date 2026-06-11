//! Drop-safe in-memory dedup guard for concurrent ingestion attempts.
//!
//! Phase 2 of the borg-pipeline-resilience design doc. Replaces the explicit
//! `INFLIGHT.lock().await.remove(...)` cleanup at every termination site with
//! an RAII handle whose `Drop` impl releases the entry automatically. This
//! closes the leak observed during the 2026-05-08 incident: when the inner
//! ingestion future was abandoned (timeout, panic, runtime cancellation) the
//! explicit cleanup never ran and every retry of the same canonical URL
//! short-circuited as `Duplicate (inflight)` until the daemon restarted.
//!
//! The lock is now `std::sync::Mutex` rather than `tokio::sync::Mutex` so
//! `Drop` can lock synchronously without an `.await`. The lock is held for
//! microseconds (one set lookup + one set mutation) and is never held across
//! an `.await`, so a sync mutex is correct in async context.

use std::collections::HashSet;
use std::sync::{LazyLock, Mutex, MutexGuard};

/// Process-wide set of canonicalized URLs that are currently being ingested.
/// Used by `InflightGuard` only; do not access directly.
static INFLIGHT: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

/// RAII handle for an inflight-set entry. Holding the guard keeps the URL in
/// the set; dropping it (success, error, panic-unwind, or future-cancel from
/// `tokio::time::timeout`) releases the entry.
pub struct InflightGuard {
    canonical: String,
}

impl InflightGuard {
    /// Try to insert `canonical` into the inflight set. Returns `Some(guard)`
    /// if the URL was newly added, `None` if it was already present (the
    /// caller should treat this as a duplicate-inflight outcome and return
    /// without proceeding).
    pub fn try_acquire(canonical: &str) -> Option<Self> {
        let mut set = lock_inflight();
        if set.contains(canonical) {
            None
        } else {
            set.insert(canonical.to_string());
            Some(Self {
                canonical: canonical.to_string(),
            })
        }
    }
}

impl Drop for InflightGuard {
    fn drop(&mut self) {
        // Tolerate a poisoned mutex (a previous panic while holding the lock).
        // The data is a HashSet<String> with no invariants to protect, so
        // recovering the inner value is safe and we proceed to remove our
        // entry. Panicking from Drop during unwind would abort the process,
        // so this branch must not bubble.
        lock_inflight().remove(&self.canonical);
    }
}

fn lock_inflight() -> MutexGuard<'static, HashSet<String>> {
    match INFLIGHT.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    }
}

#[cfg(test)]
mod tests;
