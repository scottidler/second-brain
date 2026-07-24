//! Process-wide concurrency caps for `pipeline::process_content`.
//!
//! Two permit pools shape in-flight ingest fan-out:
//!
//! - [`GENERAL_PERMITS`] - acquired at the top of `process_content` for every
//!   trace. Caps the number of concurrent pipeline executions overall so cheap
//!   work (text, vocab, code snippet, image, Jina-only articles) can still
//!   make progress even while heavy work fills the heavy pool.
//! - [`HEAVY_PERMITS`] - acquired *per-handler* by the four functions that
//!   shell out to subprocess-heavy work: `process_youtube`,
//!   `process_article_fabric`, `process_audio_inner`,
//!   `process_document_file_inner`. Acquired *after* the general permit and
//!   *immediately before* the heavy subprocess call.
//!
//! Per-handler placement (rather than a top-of-`process_content` classifier)
//! is intentional: `fabric -u` internally delegates to `yt-dlp` for media
//! URLs, so a URL classified as "article" at the top of `process_content`
//! can still fan out to ffmpeg under the article path. Acquiring the heavy
//! permit at the actual subprocess call site closes that gap.
//!
//! Heavy-permit acquire sites (keep this list in sync with `pipeline.rs`):
//!   - `pipeline::process_youtube`
//!   - `pipeline::process_article_fabric`
//!   - `pipeline::process_audio_inner`
//!   - `pipeline::process_document_file_inner`
//!
//! A [`TraceLeaseGuard`] records trace liveness as a renewable lease on the
//! shared receipts row (`lease_owner_pid` + `lease_until`) rather than a
//! process-local set. Because the lease lives in the SAME SQLite DB both the
//! daemon and a separate `sb borg harvest` process write, the daemon watchdog
//! can see a harvest-process trace as live and will not falsely reap it. The
//! watchdog excludes any row with a live lease (checked atomically in the
//! promotion UPDATE); a dead owner stops renewing and the lease expires, so a
//! genuine orphan is still reaped (fail-closed). See
//! `docs/design/2026-07-24-harvest-watchdog-cross-process-reaping.md`.

use std::sync::{Arc, OnceLock};

use chrono::Utc;
use eyre::{Context, Result};
use rusqlite::Connection;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::receipts;

/// Process-wide permit pool. Each instance wraps a `tokio::sync::Semaphore`
/// behind a `OnceLock<Arc<Semaphore>>` so the size is supplied at process
/// startup (`init`) and all callers share one bounded queue (`acquire`).
pub struct PermitPool {
    inner: OnceLock<Arc<Semaphore>>,
    name: &'static str,
}

impl PermitPool {
    pub const fn new(name: &'static str) -> Self {
        Self {
            inner: OnceLock::new(),
            name,
        }
    }

    /// Initialize with a permit count. Idempotent: a second call after the
    /// first wins is a no-op (logged at warn).
    pub fn init(&self, cap: usize) {
        log::debug!("PermitPool::init: name={} cap={}", self.name, cap);
        if self.inner.set(Arc::new(Semaphore::new(cap))).is_err() {
            log::warn!("PermitPool {}: init called twice; second call ignored", self.name);
        }
    }

    /// Acquire one owned permit. Awaits if the pool is saturated. Panics if
    /// the pool has not been initialized yet (programmer error - call
    /// `startup::init_permits` before any dispatch).
    pub async fn acquire(&self) -> OwnedSemaphorePermit {
        let sem = self
            .inner
            .get()
            .expect("PermitPool::init must be called before acquire");
        log::debug!(
            "permits[{}]: acquiring (available={})",
            self.name,
            sem.available_permits()
        );
        let permit = sem.clone().acquire_owned().await.expect("semaphore never closed");
        log::debug!(
            "permits[{}]: acquired (available={})",
            self.name,
            sem.available_permits()
        );
        permit
    }
}

/// Permit pool gating *every* `pipeline::process_content` invocation. Cap
/// configured via `pipeline.max-concurrent-traces`.
pub static GENERAL_PERMITS: PermitPool = PermitPool::new("general");

/// Permit pool gating subprocess-heavy handlers (yt-dlp, fabric -u, ffmpeg,
/// Groq, OCR). Cap configured via `pipeline.max-concurrent-heavy-traces`.
pub static HEAVY_PERMITS: PermitPool = PermitPool::new("heavy");

/// Compute the `lease_until` wall-clock expiry for a lease taken `deadline_secs`
/// from `now`, formatted so it compares lexicographically the same way the
/// receipts `lease_until` column is stored. `deadline_secs` is the SAME
/// `hard_timeout_secs + WATCHDOG_BUFFER_SECS` the watchdog uses, so "slow but
/// alive" and "dead" only diverge AFTER the handler's own hard timeout fires.
fn lease_until(now: chrono::DateTime<Utc>, deadline_secs: u64) -> String {
    (now + chrono::Duration::seconds(deadline_secs as i64))
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string()
}

/// RAII guard that owns the shared trace lease on a receipts row. Writes the
/// lease on construction, [`renew`](Self::renew)s it once when the general
/// permit is granted, and on Drop clears the lease UNLESS it was
/// [`cancel`](Self::cancel)led first. Mirrors the old `ActiveTraceGuard`'s RAII
/// shape so panic-unwind and future-cancel still release liveness - but now the
/// release is a cross-process-visible `lease_until=NULL` write, not an
/// in-memory set removal.
///
/// The happy path calls [`cancel`](Self::cancel) after the terminal
/// `mark_succeeded`/`mark_failed` UPDATE (which already NULLed the lease in the
/// same statement), so Drop performs NO further I/O on the common path - no
/// blocking SQLite UPDATE on a Tokio worker. Drop only writes when the guard
/// was never cancelled, i.e. the owning future panicked or was cancelled before
/// the terminal write, making that genuinely-dead trace immediately
/// reap-eligible.
pub struct TraceLeaseGuard {
    trace_id: String,
    conn: Connection,
    deadline_secs: u64,
    cancelled: bool,
}

impl TraceLeaseGuard {
    /// Production constructor: open the default receipts DB and write the
    /// initial lease. **Fails CLOSED** - if the open or the initial
    /// `write_lease` errors, the caller must abort the trace to a terminal
    /// failure rather than continue with a NULL lease that is instantly
    /// reap-eligible (see `pipeline::process_content`). Each guard opens its
    /// own connection (harvest has no pool; `process_content` already opens
    /// per-call at its terminal write), so the guard owns the handle for the
    /// life of the trace and uses it for renew/clear.
    pub fn acquire(trace_id: &str, deadline_secs: u64) -> Result<Self> {
        let conn = receipts::open_default().context("receipts: open_default for trace lease")?;
        Self::acquire_with_conn(conn, trace_id, deadline_secs)
    }

    /// Conn-injectable constructor. The production [`acquire`](Self::acquire)
    /// delegates here after opening the default DB; tests pass a connection
    /// over a shared on-disk DB so the guard and a concurrent watchdog scan
    /// see the same lease. Writes the initial lease immediately; a write
    /// failure propagates so the caller can fail closed.
    pub fn acquire_with_conn(conn: Connection, trace_id: &str, deadline_secs: u64) -> Result<Self> {
        let pid = std::process::id();
        let until = lease_until(Utc::now(), deadline_secs);
        log::debug!(
            "TraceLeaseGuard::acquire: trace={trace_id} pid={pid} deadline_secs={deadline_secs} lease_until={until}"
        );
        receipts::write_lease(&conn, trace_id, pid, &until)
            .with_context(|| format!("write initial trace lease for {trace_id}"))?;
        Ok(Self {
            trace_id: trace_id.to_string(),
            conn,
            deadline_secs,
            cancelled: false,
        })
    }

    /// Re-stamp `lease_until` (renew at permit grant, so the processing window
    /// is measured from when work truly starts). WARN-not-fail: a renew error
    /// is logged but does not abort the trace - the lease still expires on its
    /// own and the watchdog reaps only after that (fail-closed).
    pub fn renew(&self) {
        let until = lease_until(Utc::now(), self.deadline_secs);
        log::debug!("TraceLeaseGuard::renew: trace={} lease_until={until}", self.trace_id);
        if let Err(e) = receipts::renew_lease(&self.conn, &self.trace_id, &until) {
            log::warn!(
                "TraceLeaseGuard::renew: trace={} renew_lease failed: {e:#}",
                self.trace_id
            );
        }
    }

    /// Disarm Drop. Called on the happy path AFTER the terminal write already
    /// NULLed the lease in its own UPDATE, so Drop does nothing (no redundant
    /// blocking I/O on a Tokio worker). Consumes the guard.
    pub fn cancel(mut self) {
        log::debug!("TraceLeaseGuard::cancel: trace={} (Drop disarmed)", self.trace_id);
        self.cancelled = true;
    }
}

impl Drop for TraceLeaseGuard {
    fn drop(&mut self) {
        if self.cancelled {
            return;
        }
        // Reached only on panic-unwind / future-cancel: the terminal write
        // never ran, so the lease is still live. Clear it here to make a
        // genuinely dead trace immediately reap-eligible. WARN-not-panic - if
        // the clear fails the lease still expires on its own and the next
        // watchdog scan reaps it.
        log::debug!(
            "TraceLeaseGuard::drop: trace={} not cancelled, clearing lease",
            self.trace_id
        );
        if let Err(e) = receipts::clear_lease(&self.conn, &self.trace_id) {
            log::warn!(
                "TraceLeaseGuard::drop: trace={} clear_lease failed: {e:#}",
                self.trace_id
            );
        }
    }
}

#[cfg(test)]
mod tests;
