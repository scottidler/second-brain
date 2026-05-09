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
mod tests {
    use super::*;

    #[test]
    fn test_try_acquire_returns_none_when_already_held() {
        let url = "https://example.com/inflight-test-already-held";
        let g1 = InflightGuard::try_acquire(url).expect("first acquire");
        assert!(InflightGuard::try_acquire(url).is_none());
        drop(g1);
        // After drop, the URL is releaseable again.
        assert!(InflightGuard::try_acquire(url).is_some());
    }

    #[test]
    fn test_drop_releases_entry_on_panic_unwind() {
        let url = "https://example.com/inflight-test-panic";
        let result = std::panic::catch_unwind(|| {
            let _guard = InflightGuard::try_acquire(url).expect("acquire");
            panic!("simulated mid-pipeline panic");
        });
        assert!(result.is_err(), "panic should propagate to catch_unwind");
        // Drop ran during unwind; URL must be releaseable.
        assert!(
            InflightGuard::try_acquire(url).is_some(),
            "guard should release inflight entry during panic-unwind"
        );
    }

    #[test]
    fn test_drop_releases_after_timeout_drop() {
        // Simulates the path where tokio::time::timeout fires and drops the
        // future holding the InflightGuard. The future never completes, but
        // Drop runs as the future is unwound.
        let url = "https://example.com/inflight-test-timeout-drop";
        {
            let _guard = InflightGuard::try_acquire(url).expect("acquire");
            // Pretend the enclosing future is dropped here (e.g. timeout fired).
        }
        assert!(InflightGuard::try_acquire(url).is_some());
    }
}
