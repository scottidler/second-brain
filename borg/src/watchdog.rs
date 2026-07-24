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

/// One scan: promote stale `received` rows to `failed/crashed`. Returns the
/// number promoted.
///
/// Liveness is now read from the shared receipts row's lease
/// (`lease_owner_pid` + `lease_until`), not a process-local set: a row whose
/// owning process (daemon OR a separate `sb borg harvest`) is still renewing
/// its lease is excluded by the lease predicate baked into `list_stale` AND
/// `promote_single_to_crashed`, even across process boundaries.
pub fn run_once(config: &Config) -> Result<usize> {
    log::debug!("watchdog::run_once: starting scan");
    let deadline = config.pipeline.hard_timeout_secs + WATCHDOG_BUFFER_SECS;
    let conn = receipts::open_default().context("receipts: open_default")?;
    let promoted = run_once_conn(&conn, deadline)?;
    if promoted > 0 {
        log::info!("watchdog: promoted {promoted} receipts row(s) to crashed this pass");
    } else {
        log::debug!("watchdog: scan clean, no orphans this pass");
    }
    Ok(promoted)
}

/// Conn-injectable core of [`run_once`]:
///
/// 1. `list_stale` SELECTs `status='received'` rows past the deadline whose
///    lease is absent or expired.
/// 2. For each candidate, issue `promote_single_to_crashed`, whose UPDATE
///    REPEATS the lease predicate atomically - so an owner that renews between
///    the SELECT and the UPDATE (the cross-process TOCTOU) makes the UPDATE
///    match 0 rows and the live trace is NOT reaped.
fn run_once_conn(conn: &Connection, deadline_secs: u64) -> Result<usize> {
    // Read the clock once so `list_stale`'s SELECT and each promotion UPDATE
    // agree on "now" - the atomic lease predicate in `promote_single_to_crashed`
    // depends on comparing against the SAME instant, not a re-read per row.
    let now = Utc::now();
    let stale = receipts::list_stale(conn, deadline_secs, now).context("receipts: list_stale")?;
    let mut promoted = 0usize;
    for (trace_id, received_at) in stale {
        match receipts::promote_single_to_crashed(conn, &trace_id, deadline_secs, now) {
            Ok(true) => {
                log::warn!("watchdog: promoted trace={trace_id} to crashed (received_at={received_at})");
                promoted += 1;
            }
            Ok(false) => {
                log::debug!("watchdog: trace={trace_id} no longer in received state (race lost); skipping");
            }
            Err(e) => {
                log::error!("watchdog: promote_single failed for trace={trace_id}: {e:#}");
            }
        }
    }
    Ok(promoted)
}

/// Background task entry point. Runs forever, scanning every
/// `WATCHDOG_INTERVAL_SECS`. Any scan error is logged at WARN and the loop
/// continues - one bad read should not silently disable the watchdog.
pub async fn run(config: Arc<Config>) {
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
                Ok(0)
            });
        if let Err(e) = result {
            log::warn!("watchdog: scan failed: {e:#}");
        }
    }
}

#[cfg(test)]
mod tests;
