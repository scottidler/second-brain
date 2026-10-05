//! `sb borg wait`: block until the ingest batch in flight drains, then exit
//! with a code an agent can branch on (design:
//! `docs/design/2026-10-04-ingest-queue-status.md`, API Design).
//!
//! The first plain `GET /queue` pins `batch.id`; every later poll asks for
//! `?batch=<pinned>`, so a new click joining that batch extends it and a later,
//! separate batch can never be mistaken for it. One monotonic deadline covers
//! requests, body reads and sleeps. Exactly one snapshot reaches stdout, the
//! final one; nothing is printed per poll.

use std::time::{Duration, Instant};

use vault::queue::{ItemState, QueueSnapshot, QueueState};

/// Pause between polls (`borg::replay::POLL_INTERVAL_SECS` precedent).
pub const POLL_INTERVAL: Duration = Duration::from_secs(2);

/// Per-request ceiling; each request gets `min(this, time left)`.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Exit code for "the pinned batch drained with `failed > 0`".
pub const EXIT_FAILED: u8 = 3;
/// Exit code for "an item in the pinned batch is wedged".
pub const EXIT_WEDGED: u8 = 4;
/// Exit code for "`--timeout` reached while still draining".
pub const EXIT_TIMEOUT: u8 = 5;

/// What one snapshot means for `wait`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Exit with this code, printing the snapshot (0 is success).
    Exit(u8),
    /// Still draining and time remains: poll again.
    KeepWaiting,
}

/// The exit table, over one snapshot and whether the deadline has passed.
/// Order matters: wedged beats idle, and both beat the deadline, so an answer
/// that arrives exactly at the deadline is still reported for what it says.
pub fn decide(snapshot: &QueueSnapshot, deadline_reached: bool) -> Verdict {
    let wedged = snapshot
        .items
        .as_deref()
        .unwrap_or_default()
        .iter()
        .any(|item| item.state == ItemState::Wedged);
    if wedged {
        return Verdict::Exit(EXIT_WEDGED);
    }
    if snapshot.state == QueueState::Idle {
        let failed = snapshot.batch.as_ref().map_or(0, |b| b.failed);
        return Verdict::Exit(if failed == 0 { 0 } else { EXIT_FAILED });
    }
    if deadline_reached {
        return Verdict::Exit(EXIT_TIMEOUT);
    }
    Verdict::KeepWaiting
}

/// How `wait` ended without an error.
#[derive(Debug)]
pub struct WaitOutcome {
    /// 0, 3, 4 or 5.
    pub code: u8,
    /// The final snapshot. `None` only on exit 5 when the deadline passed
    /// before the daemon ever answered.
    pub snapshot: Option<QueueSnapshot>,
}

/// Poll the daemon until [`decide`] says exit, or the deadline passes.
///
/// A request that fails before the deadline, or a draining answer with no
/// batch to pin, is an `Err` (exit 1). A request
/// that fails because the deadline cut it short is exit 5, never 1: its
/// timeout was `min(REQUEST_TIMEOUT, time left)`, so it can only time out at
/// or after the deadline.
pub async fn run(config: &borg::config::Config, timeout: Duration) -> eyre::Result<WaitOutcome> {
    log::debug!(
        "wait::run: daemon={}:{} timeout={timeout:?}",
        config.hotkey.host,
        config.hotkey.port
    );
    let deadline = Instant::now() + timeout;
    let mut pinned: Option<String> = None;
    let mut last: Option<QueueSnapshot> = None;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            log::debug!("wait::run: deadline reached between polls, pinned={pinned:?}");
            return Ok(WaitOutcome {
                code: EXIT_TIMEOUT,
                snapshot: last,
            });
        }
        let request_timeout = left.min(REQUEST_TIMEOUT);
        let snapshot = match borg::queue::fetch(config, pinned.as_deref(), request_timeout).await {
            Ok(snapshot) => snapshot,
            Err(e) if Instant::now() >= deadline => {
                log::debug!("wait::run: request cut short by the deadline ({e}), pinned={pinned:?}");
                return Ok(WaitOutcome {
                    code: EXIT_TIMEOUT,
                    snapshot: last,
                });
            }
            Err(e) => {
                log::error!("wait::run: request failed before the deadline, pinned={pinned:?}: {e}");
                return Err(e.into());
            }
        };
        if let Verdict::Exit(code) = decide(&snapshot, Instant::now() >= deadline) {
            log::debug!(
                "wait::run: exit {code} pinned={pinned:?} state={:?} total={:?}",
                snapshot.state,
                snapshot.batch.as_ref().map(|b| b.total)
            );
            return Ok(WaitOutcome {
                code,
                snapshot: Some(snapshot),
            });
        }
        if pinned.is_none() {
            let Some(batch) = snapshot.batch.as_ref() else {
                eyre::bail!(
                    "daemon at {}:{} reported state: draining without a batch",
                    config.hotkey.host,
                    config.hotkey.port
                );
            };
            log::debug!("wait::run: pinned batch {}", batch.id);
            pinned = Some(batch.id.clone());
        }
        log::trace!(
            "wait::run: draining pinned={pinned:?} remaining={:?}",
            snapshot.batch.as_ref().map(|b| b.remaining)
        );
        last = Some(snapshot);
        tokio::time::sleep(POLL_INTERVAL.min(deadline.saturating_duration_since(Instant::now()))).await;
    }
}

#[cfg(test)]
mod tests;
