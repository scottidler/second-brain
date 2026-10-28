//! Background watchdog that promotes orphaned `received` receipts rows.
//!
//! Every `WATCHDOG_INTERVAL_SECS` it SELECTs `status='received'` rows older
//! than `pipeline.hard_timeout_secs + buffer` whose shared trace lease is
//! absent or expired, and issues a status-and-lease-guarded UPDATE to
//! `crashed` for each survivor. Reading liveness from the shared receipts-row
//! lease (rather than a process-local set) is what lets the daemon watchdog
//! avoid falsely reaping a trace a SEPARATE `sb borg harvest` process is still
//! working. This catches the OOM-killed / panic-outside-timeout cases the
//! per-pipeline hard timeout misses.

use crate::batch::{self, BatchSummary, SummarySender};
use crate::config::Config;
use crate::receipts;
use chrono::Utc;
use eyre::{Context, Result};
use rusqlite::Connection;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::interval;

/// How often the watchdog wakes up to scan. Configurable later if needed.
const WATCHDOG_INTERVAL_SECS: u64 = 60;

/// Buffer added on top of `pipeline.hard_timeout_secs` before a trace is
/// considered orphaned. Anything still `received` past (hard_timeout + buffer)
/// with no live lease is genuinely lost. Public so the lease-writing
/// `TraceLeaseGuard` pins `lease_until` to the SAME deadline the watchdog uses.
pub const WATCHDOG_BUFFER_SECS: u64 = 60;

/// What one watchdog pass did: rows promoted to crashed, and the bulk
/// summaries it won (a promotion that drained a sealed batch, or the
/// seal-missing backstop). The caller puts the summaries on the notifier
/// channel; the pass itself touches no sink.
#[derive(Debug, Default)]
pub struct Pass {
    pub promoted: usize,
    pub summaries: Vec<BatchSummary>,
}

/// One scan: promote stale `received` rows to `failed/crashed`, then run the
/// bulk backstop.
///
/// Liveness is now read from the shared receipts row's lease
/// (`lease_owner_pid` + `lease_until`), not a process-local set: a row whose
/// owning process (daemon OR a separate `sb borg harvest`) is still renewing
/// its lease is excluded by the lease predicate baked into `list_stale` AND
/// `promote_single_to_crashed`, even across process boundaries.
pub fn run_once(config: &Config) -> Result<Pass> {
    log::debug!("watchdog::run_once: starting scan");
    let deadline = config.pipeline.hard_timeout_secs + WATCHDOG_BUFFER_SECS;
    let conn = receipts::open_default().context("receipts: open_default")?;
    let pass = run_once_conn(&conn, deadline)?;
    if pass.promoted > 0 {
        log::info!(
            "watchdog: promoted {} receipts row(s) to crashed this pass",
            pass.promoted
        );
    } else {
        log::debug!("watchdog: scan clean, no orphans this pass");
    }
    Ok(pass)
}

/// Conn-injectable core of [`run_once`]:
///
/// 1. `list_stale` SELECTs `status='received'` rows past the deadline whose
///    lease is absent or expired.
/// 2. For each candidate, issue `promote_single_to_crashed`, whose UPDATE
///    REPEATS the lease predicate atomically - so an owner that renews between
///    the SELECT and the UPDATE (the cross-process TOCTOU) makes the UPDATE
///    match 0 rows and the live trace is NOT reaped. A promoted batched trace
///    runs its batch's drain check.
/// 3. The bulk backstop (`batch::backstop`, seal-missing only) on the same
///    deadline. A batch error is logged and never stops the crash promotion.
fn run_once_conn(conn: &Connection, deadline_secs: u64) -> Result<Pass> {
    // Read the clock once so `list_stale`'s SELECT and each promotion UPDATE
    // agree on "now" - the atomic lease predicate in `promote_single_to_crashed`
    // depends on comparing against the SAME instant, not a re-read per row.
    let now = Utc::now();
    let stale = receipts::list_stale(conn, deadline_secs, now).context("receipts: list_stale")?;
    let mut pass = Pass::default();
    for (trace_id, received_at) in stale {
        match receipts::promote_single_to_crashed(conn, &trace_id, deadline_secs, now) {
            Ok(true) => {
                log::warn!("watchdog: promoted trace={trace_id} to crashed (received_at={received_at})");
                pass.promoted += 1;
                match batch::after_terminal(conn, &trace_id) {
                    Ok(Some(summary)) => pass.summaries.push(summary),
                    Ok(None) => {}
                    Err(e) => log::error!("watchdog: batch drain check failed for trace={trace_id}: {e:#}"),
                }
            }
            Ok(false) => {
                log::debug!("watchdog: trace={trace_id} no longer in received state (race lost); skipping");
            }
            Err(e) => {
                log::error!("watchdog: promote_single failed for trace={trace_id}: {e:#}");
            }
        }
    }
    match batch::backstop(conn, deadline_secs, now) {
        Ok(summaries) => pass.summaries.extend(summaries),
        Err(e) => log::error!("watchdog: batch backstop failed: {e:#}"),
    }
    Ok(pass)
}

/// Background task entry point. Runs forever, scanning every
/// `WATCHDOG_INTERVAL_SECS`. Any scan error is logged at WARN and the loop
/// continues - one bad read should not silently disable the watchdog. Won
/// bulk summaries go on `summaries`, the notifier channel.
pub async fn run(config: Arc<Config>, summaries: SummarySender) {
    log::info!(
        "watchdog: starting (interval={}s, deadline=hard_timeout+{}s)",
        WATCHDOG_INTERVAL_SECS,
        WATCHDOG_BUFFER_SECS
    );
    let mut ticker = interval(Duration::from_secs(WATCHDOG_INTERVAL_SECS));
    loop {
        ticker.tick().await;
        let cfg = config.clone();
        // The receipts scan is a short SQLite read + targeted UPDATEs; run it
        // on a blocking-safe task so the tokio runtime is never starved.
        let result = tokio::task::spawn_blocking(move || run_once(&cfg))
            .await
            .unwrap_or_else(|join_err| {
                log::error!("watchdog: join error: {join_err}");
                Ok(Pass::default())
            });
        match result {
            Ok(pass) => {
                for summary in pass.summaries {
                    batch::send(&summaries, summary);
                }
            }
            Err(e) => log::warn!("watchdog: scan failed: {e:#}"),
        }
    }
}

#[cfg(test)]
mod tests;
