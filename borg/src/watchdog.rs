//! Background watchdog that promotes orphaned `received` receipts rows.
//!
//! Every `WATCHDOG_INTERVAL_SECS` it SELECTs `status='received'` rows older
//! than `pipeline.hard_timeout_secs + buffer`, filters out traces still active
//! in the pipeline (permit-queued or running), and issues a status-guarded
//! UPDATE to `crashed` for each survivor. This catches the OOM-killed /
//! panic-outside-timeout cases the per-pipeline hard timeout misses.

use crate::config::Config;
use crate::pipeline::permits;
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
/// is genuinely lost.
const WATCHDOG_BUFFER_SECS: u64 = 60;

/// One scan: promote stale `received` rows to `failed/crashed`. Returns the
/// number promoted.
///
/// `active_traces` is consulted before promotion: any trace currently inside
/// `process_content` (queued for a permit or running) is excluded even if its
/// age has crossed `deadline`. Production passes `&permits::is_trace_active`;
/// tests pass closures over a fixture set so the global `ACTIVE_TRACES` is
/// never touched.
pub fn run_once(config: &Config, active_traces: &dyn Fn(&str) -> bool) -> Result<usize> {
    log::debug!("watchdog::run_once: starting scan");
    let deadline = config.pipeline.hard_timeout_secs + WATCHDOG_BUFFER_SECS;
    let conn = receipts::open_default().context("receipts: open_default")?;
    let promoted = run_once_conn(&conn, deadline, active_traces)?;
    if promoted > 0 {
        log::info!("watchdog: promoted {promoted} receipts row(s) to crashed this pass");
    } else {
        log::debug!("watchdog: scan clean, no orphans this pass");
    }
    Ok(promoted)
}

/// Conn-injectable core of [`run_once`]:
///
/// 1. `SELECT trace_id, received_at FROM receipts WHERE status='received' AND received_at < cutoff`
/// 2. Filter survivors through `active_traces` (a permit-queued trace is
///    legitimately mid-flight and must not be promoted).
/// 3. For each survivor, issue the status-guarded `promote_single_to_crashed`.
fn run_once_conn(conn: &Connection, deadline_secs: u64, active_traces: &dyn Fn(&str) -> bool) -> Result<usize> {
    // Read the clock once so `list_stale`'s SELECT and each promotion UPDATE
    // agree on "now" - the atomic lease predicate in `promote_single_to_crashed`
    // depends on comparing against the SAME instant, not a re-read per row.
    let now = Utc::now();
    let stale = receipts::list_stale(conn, deadline_secs, now).context("receipts: list_stale")?;
    let mut promoted = 0usize;
    for (trace_id, received_at) in stale {
        if active_traces(&trace_id) {
            log::debug!(
                "watchdog: trace {trace_id} aged past deadline (received_at={received_at}) but still active; skipping"
            );
            continue;
        }
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
        let result = tokio::task::spawn_blocking(move || run_once(&cfg, &permits::is_trace_active))
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
