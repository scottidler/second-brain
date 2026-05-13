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
//! A process-wide [`ActiveTraceGuard`] tracks every trace currently inside
//! `process_content` (queued for a permit OR running). The watchdog consults
//! that set via [`is_trace_active`] before declaring an intake row an orphan,
//! preserving the `ledger XOR DLQ` invariant from the intake-log design.

use std::collections::HashSet;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

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

/// Trace IDs currently inside `process_content` (waiting on a permit or
/// running). Watchdog reads via [`is_trace_active`]; never lock across an
/// `.await`.
static ACTIVE_TRACES: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

fn active_traces() -> &'static Mutex<HashSet<String>> {
    ACTIVE_TRACES.get_or_init(|| Mutex::new(HashSet::new()))
}

/// RAII guard: insert a trace ID into the active-traces set on construction,
/// remove on Drop. Mirrors `InflightGuard` so panic-unwind, future-cancel,
/// and normal exit all release the entry.
///
/// The lifetime parameter allows tests to bind a local `Mutex<HashSet>` via
/// [`ActiveTraceGuard::acquire_in`] instead of touching the process-wide
/// `ACTIVE_TRACES` static. Production code uses [`ActiveTraceGuard::acquire`],
/// which always binds `'static`.
pub struct ActiveTraceGuard<'a> {
    trace_id: String,
    set: &'a Mutex<HashSet<String>>,
}

impl ActiveTraceGuard<'static> {
    /// Acquire the active-trace entry for `trace_id` against the process-wide
    /// `ACTIVE_TRACES` set. Returns even if `trace_id` is already present
    /// (production paths only construct one guard per `process_content`).
    pub fn acquire(trace_id: &str) -> Self {
        Self::acquire_in(active_traces(), trace_id)
    }
}

impl<'a> ActiveTraceGuard<'a> {
    /// Acquire the active-trace entry against a caller-supplied set. The
    /// `'static` `acquire` is the production constructor; tests build a
    /// local `Mutex<HashSet>` and pass it here for isolation.
    pub fn acquire_in(set: &'a Mutex<HashSet<String>>, trace_id: &str) -> Self {
        lock(set).insert(trace_id.to_string());
        Self {
            trace_id: trace_id.to_string(),
            set,
        }
    }
}

impl<'a> Drop for ActiveTraceGuard<'a> {
    fn drop(&mut self) {
        lock(self.set).remove(&self.trace_id);
    }
}

fn lock(set: &Mutex<HashSet<String>>) -> MutexGuard<'_, HashSet<String>> {
    match set.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// True if `trace_id` is currently inside `process_content` (queued or
/// running). Used by `watchdog::run_once` to suppress false orphan DLQ rows.
pub fn is_trace_active(trace_id: &str) -> bool {
    lock(active_traces()).contains(trace_id)
}

#[cfg(test)]
mod tests;
