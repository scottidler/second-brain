//! Receipts-backed inspection for `sb borg log` and the health endpoint.
//!
//! `sb borg log` / `sb borg log --trace` query the receipts SQLite DB
//! (`receipts_log` / `receipts_show`). `audit_health_stats` powers
//! `GET /health/audit` and the `sb doctor` borg check. The legacy markdown
//! intake/DLQ inspection commands and the orphan audit were removed when the
//! markdown bookkeeping was excised (see
//! docs/design/2026-06-03-receipts-log-legacy-markdown-excision.md).

use std::path::Path;

use crate::harvest::watermark::WatermarkState;
use crate::receipts;
use eyre::{Context, Result};
use rusqlite::Connection;
use vault::receipts::{FailureStage, ReceiptKind, ReceiptStatus};
use vault::schema::Method;

/// Live receipts health for `GET /health/audit` and the `sb doctor` borg
/// check: lifetime status counts + the `crashed` subset, plus the last-24h
/// failed / crashed / degraded counts. `crashed_24h` is the actionable
/// silent-drop signal (a state the watchdog has definitively ruled on);
/// `degraded_24h` is the actionable silent-quality signal (notes that landed but
/// via a distill fallback - they read as `succeeded`, so they hide in the
/// status counts); lifetime `crashed` is informational only (monotonic).
pub fn audit_health_stats() -> Result<crate::routes::AuditHealth> {
    let conn = receipts::open_default().context("open receipts DB")?;
    audit_health_stats_conn(&conn)
}

/// Conn-injectable core of [`audit_health_stats`].
fn audit_health_stats_conn(conn: &Connection) -> Result<crate::routes::AuditHealth> {
    let (received, succeeded, failed, _) = receipts::count_by_status(conn)?;
    let crashed = receipts::count_failed_by_stage(conn)?
        .into_iter()
        .find(|(stage, _)| *stage == FailureStage::Crashed)
        .map_or(0, |(_, count)| count);
    let since = receipts::hours_ago_iso(24);
    let failed_24h = receipts::count_failed_since(conn, &since)?;
    let crashed_24h = receipts::count_crashed_since(conn, &since)?;
    let degraded_24h = receipts::count_degraded_since(conn, &since)?;
    Ok(crate::routes::AuditHealth {
        received: received as usize,
        succeeded: succeeded as usize,
        failed: failed as usize,
        crashed: crashed as usize,
        failed_24h: failed_24h as usize,
        crashed_24h: crashed_24h as usize,
        degraded_24h: degraded_24h as usize,
    })
}

/// Filter args for `sb borg log`. All fields are optional except `limit`.
pub struct ReceiptLogFilter {
    pub status: Option<String>,
    pub method: Option<String>,
    pub stage: Option<String>,
    pub since: Option<String>,
    pub source: Option<String>,
    /// When true, restrict to degraded (distill-fallback) publishes.
    pub degraded: bool,
    pub limit: usize,
}

/// Query the receipts DB for `sb borg log`. Returns rows newest-first.
pub fn receipts_log(filter: ReceiptLogFilter) -> Result<Vec<crate::receipts::Receipt>> {
    let conn = receipts::open_default().context("open receipts DB")?;
    let status = filter
        .status
        .as_deref()
        .map(|s| s.parse::<ReceiptStatus>().map_err(|e| eyre::eyre!(e)))
        .transpose()
        .context("parse --status")?;
    let method = filter
        .method
        .as_deref()
        .map(|m| m.parse::<Method>().map_err(|e| eyre::eyre!(e)))
        .transpose()
        .context("parse --method")?;
    let stage = filter
        .stage
        .as_deref()
        .map(|s| s.parse::<FailureStage>().map_err(|e| eyre::eyre!(e)))
        .transpose()
        .context("parse --stage")?;
    let since = filter
        .since
        .as_deref()
        .map(|s| receipts::parse_since(s, chrono::Utc::now()))
        .transpose()
        .context("parse --since")?;
    let receipts_filter = receipts::Filter {
        status,
        method,
        stage,
        since,
        source_like: filter.source,
        degraded: filter.degraded.then_some(true),
        limit: Some(filter.limit),
    };
    receipts::query(&conn, &receipts_filter)
}

/// Read one receipts row by trace_id (for `sb borg log --trace ...`).
pub fn receipts_show(trace_id: &str) -> Result<crate::receipts::Receipt> {
    let conn = receipts::open_default().context("open receipts DB")?;
    let row = receipts::get(&conn, trace_id)
        .context("lookup receipt")?
        .ok_or_else(|| eyre::eyre!("trace_id {trace_id} not found in receipts DB"))?;
    Ok(row)
}

/// The `sb doctor` harvest-drift window, in days (harvest-completion Phase 6,
/// Opus SE K2 finding). Wide enough to absorb a nightly timer's normal jitter
/// (a missed/delayed run) while still catching a contract drift that has gone
/// silent for multiple cycles.
pub const HARVEST_DRIFT_WINDOW_DAYS: i64 = 3;

/// Harvest drift guard stats (harvest-completion Phase 6): the durable
/// structural guard against a FUTURE clyde contract drift that the frozen CI
/// fixtures cannot see. Mirrors the `degraded_24h` pattern - a silent-quality
/// signal that never shows up in the failed/crashed counts above, because a
/// total-abort drift (a brand new unanticipated type mismatch that dies before
/// `write_rejections` ever runs) leaves NO receipts at all, not even a failed
/// one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HarvestDriftStats {
    /// The harvest timer has run at least once: the watermark cursor is set.
    /// Every LIVE run persists a cursor via `apply_plan_to_state`/`state.save`,
    /// even a run that selected/published nothing, so this is independent of
    /// the receipts table, unlike `session_receipts_in_window` below. A state
    /// file that has never been written (`cursor: None`) means harvest has
    /// never run live yet, which is expected pre-soak and never a warning.
    pub timer_has_run: bool,
    /// Count of `session`-kind receipts (any status - received, succeeded,
    /// rejected, failed) received within [`HARVEST_DRIFT_WINDOW_DAYS`].
    pub session_receipts_in_window: usize,
}

impl HarvestDriftStats {
    /// The guard fires only when the timer has proven it can run (a prior
    /// cursor exists) AND the recent window produced literally nothing - the
    /// exact shape a future contract-parse abort would leave behind, since an
    /// abort dies before any receipt (even a rejected one) gets written.
    pub fn should_warn(&self) -> bool {
        self.timer_has_run && self.session_receipts_in_window == 0
    }
}

/// Compute the harvest drift guard against the real state file + receipts DB.
pub fn harvest_drift_stats() -> Result<HarvestDriftStats> {
    let state_path = vault::paths::borg_harvest_state();
    let conn = receipts::open_default().context("open receipts DB")?;
    harvest_drift_stats_at(&state_path, &conn, HARVEST_DRIFT_WINDOW_DAYS)
}

/// Path/conn-injectable core of [`harvest_drift_stats`].
fn harvest_drift_stats_at(state_path: &Path, conn: &Connection, window_days: i64) -> Result<HarvestDriftStats> {
    log::debug!(
        "triage::harvest_drift_stats_at: state_path={} window_days={window_days}",
        state_path.display()
    );
    let state = WatermarkState::load(state_path).context("load harvest watermark state")?;
    let timer_has_run = state.cursor.is_some();
    let since = receipts::hours_ago_iso(window_days * 24);
    let session_receipts_in_window = receipts::count_kind_since(conn, ReceiptKind::Session, &since)? as usize;
    log::debug!(
        "triage::harvest_drift_stats_at: timer_has_run={timer_has_run} session_receipts_in_window={session_receipts_in_window}"
    );
    Ok(HarvestDriftStats {
        timer_has_run,
        session_receipts_in_window,
    })
}

#[cfg(test)]
mod tests;
