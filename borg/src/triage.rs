//! Receipts-backed inspection for `sb borg log` and the health endpoint.
//!
//! `sb borg log` / `sb borg log --trace` query the receipts SQLite DB
//! (`receipts_log` / `receipts_show`). `audit_health_stats` powers
//! `GET /health/audit` and the `sb doctor` borg check. The legacy markdown
//! intake/DLQ inspection commands and the orphan audit were removed when the
//! markdown bookkeeping was excised (see
//! docs/design/2026-06-03-receipts-log-legacy-markdown-excision.md).

use crate::receipts;
use eyre::{Context, Result};
use rusqlite::Connection;
use vault::receipts::{FailureStage, ReceiptStatus};
use vault::schema::Method;

/// Live receipts health for `GET /health/audit` and the `sb doctor` borg
/// check: lifetime status counts + the `crashed` subset, plus the last-24h
/// failed / crashed counts. `crashed_24h` is the actionable silent-drop signal
/// (a state the watchdog has definitively ruled on); lifetime `crashed` is
/// informational only (monotonic).
pub fn audit_health_stats() -> Result<crate::routes::AuditHealth> {
    let conn = receipts::open_default().context("open receipts DB")?;
    audit_health_stats_conn(&conn)
}

/// Conn-injectable core of [`audit_health_stats`].
fn audit_health_stats_conn(conn: &Connection) -> Result<crate::routes::AuditHealth> {
    let (received, succeeded, failed) = receipts::count_by_status(conn)?;
    let crashed = receipts::count_failed_by_stage(conn)?
        .into_iter()
        .find(|(stage, _)| *stage == FailureStage::Crashed)
        .map_or(0, |(_, count)| count);
    let since = receipts::hours_ago_iso(24);
    let failed_24h = receipts::count_failed_since(conn, &since)?;
    let crashed_24h = receipts::count_crashed_since(conn, &since)?;
    Ok(crate::routes::AuditHealth {
        received: received as usize,
        succeeded: succeeded as usize,
        failed: failed as usize,
        crashed: crashed as usize,
        failed_24h: failed_24h as usize,
        crashed_24h: crashed_24h as usize,
    })
}

/// Filter args for `sb borg log`. All fields are optional except `limit`.
pub struct ReceiptLogFilter {
    pub status: Option<String>,
    pub method: Option<String>,
    pub stage: Option<String>,
    pub since: Option<String>,
    pub source: Option<String>,
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

#[cfg(test)]
mod tests;
