//! Background watchdog that enforces the intake invariant.
//!
//! Periodically scans `borg-intake.md`, looks up each trace_id in the
//! ledger and DLQ, and any trace_id older than
//! `pipeline.hard_timeout_secs + 60s` that has not produced a resolution
//! row in either store gets a `watchdog-orphan` DLQ entry. This catches
//! the OOM-killed / panic-outside-timeout cases that the per-pipeline
//! Phase-1 hard timeout misses.

use crate::config::Config;
use crate::intake as intake_helper;
use crate::ledger;
use crate::pipeline::permits;
use chrono::{Local, NaiveDateTime, TimeZone};
use eyre::{Context, Result};
use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::interval;
use vault::dlq::{self, DlqEntry, DlqStage, DlqStatus};
use vault::intake::{self, ParsedIntakeRow};
use vault::table;

/// How often the watchdog wakes up to scan. Configurable later if needed.
const WATCHDOG_INTERVAL_SECS: u64 = 60;

/// Buffer added on top of `pipeline.hard_timeout_secs` before a trace is
/// considered orphaned. Anything that has not produced a ledger or DLQ row
/// within (hard_timeout + buffer) is genuinely lost.
const WATCHDOG_BUFFER_SECS: u64 = 60;

fn ledger_trace_ids(path: &Path) -> Result<HashSet<String>> {
    if !path.exists() {
        return Ok(HashSet::new());
    }
    let content = std::fs::read_to_string(path).context("read ledger")?;
    let parsed = table::parse_table(&content, &["Trace"])?;
    let mut out = HashSet::new();
    for row in &parsed.rows {
        if let Some(t) = row.get("Trace") {
            let trimmed = t.trim();
            if !trimmed.is_empty() && trimmed != "-" {
                out.insert(trimmed.to_string());
            }
        }
    }
    Ok(out)
}

fn intake_age_secs(row: &ParsedIntakeRow) -> Option<i64> {
    let dt_str = format!("{} {}", row.date, row.time);
    let parsed = NaiveDateTime::parse_from_str(&dt_str, "%Y-%m-%d %H:%M").ok()?;
    let local = Local.from_local_datetime(&parsed).single()?;
    Some((Local::now() - local).num_seconds())
}

/// One scan: find orphans, append watchdog-orphan DLQ rows for each. Returns
/// the number of orphans recorded.
///
/// `active_traces` is a predicate the watchdog consults *after* the
/// ledger/DLQ check: any trace ID that is currently inside `process_content`
/// (queued for a permit or running) is excluded from orphan detection, even
/// if its intake age has crossed `deadline`. Production passes
/// `&permits::is_trace_active`; tests pass closures over a fixture set so
/// the global `ACTIVE_TRACES` is never touched.
pub fn run_once(config: &Config, active_traces: &dyn Fn(&str) -> bool) -> Result<usize> {
    log::debug!("watchdog::run_once: starting scan");
    let intake_md = intake_helper::intake_path(config)?;
    let dlq_md = intake_helper::dlq_path(config)?;
    let ledger_md = ledger::ledger_path(config)?;

    let intake_rows = intake::parse_entries(&intake_md).context("parse intake")?;
    let ledger_traces = ledger_trace_ids(&ledger_md).context("parse ledger")?;
    let dlq_rows = dlq::parse_entries(&dlq_md).context("parse dlq")?;
    let dlq_traces: HashSet<String> = dlq_rows.iter().map(|r| r.trace_id.clone()).collect();

    let deadline = (config.pipeline.hard_timeout_secs + WATCHDOG_BUFFER_SECS) as i64;
    let tz: chrono_tz::Tz = config
        .frontmatter
        .timezone
        .parse()
        .unwrap_or(chrono_tz::America::Los_Angeles);
    let now = chrono::Utc::now().with_timezone(&tz);
    let date = now.format("%Y-%m-%d").to_string();
    let time = now.format("%H:%M").to_string();

    let mut new_orphans = 0usize;
    for row in &intake_rows {
        if ledger_traces.contains(&row.trace_id) || dlq_traces.contains(&row.trace_id) {
            continue;
        }
        let Some(age) = intake_age_secs(row) else {
            continue;
        };
        if age < deadline {
            continue;
        }
        if active_traces(&row.trace_id) {
            log::debug!(
                "watchdog: trace {} aged {}s but still active in pipeline; skipping",
                row.trace_id,
                age
            );
            continue;
        }
        let method: vault::schema::Method = row.method.parse().unwrap_or(vault::schema::Method::Manual);
        let entry = DlqEntry {
            date: date.clone(),
            time: time.clone(),
            method,
            stage: DlqStage::WatchdogOrphan,
            reason: format!("no ledger or dlq row produced within {deadline}s (intake age {age}s)"),
            preview: row.preview.clone(),
            retries: 0,
            status: DlqStatus::Pending,
            trace_id: row.trace_id.clone(),
            replay_of: None,
        };
        if let Err(e) = dlq::append_entry(&dlq_md, &entry) {
            log::error!(
                "watchdog: failed to append orphan DLQ row for trace={}: {e:#}",
                row.trace_id
            );
            continue;
        }
        log::warn!(
            "watchdog: orphan detected trace={} age={}s method={} preview={}",
            row.trace_id,
            age,
            row.method,
            row.preview
        );
        new_orphans += 1;
    }
    if new_orphans > 0 {
        log::info!("watchdog: recorded {new_orphans} new orphan(s) this pass");
    } else {
        log::debug!("watchdog: scan clean, no orphans this pass");
    }
    Ok(new_orphans)
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
        // Filesystem walk + parse runs in a blocking-safe context: it's
        // fast (no I/O proportional to size beyond reading three files) so
        // we run it on the current task.
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
