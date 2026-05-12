//! User-facing intake / DLQ inspection commands and the orphan-audit pass.
//!
//! The orphan audit walks intake.md, ledger.md, and dlq.md, computes the
//! set of trace_ids that exist in intake but have no resolution in either
//! ledger (success) or dlq (failure), and writes
//! `system/views/borg-orphans.md` for the dashboard to consume. Because
//! dataview cannot natively join two tables, materializing orphans as a
//! third table is the practical option (per design doc open question).

use crate::config::Config;
use crate::intake as intake_helper;
use crate::ledger;
use chrono::{Local, NaiveDateTime, TimeZone};
use eyre::{Context, Result, bail};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use vault::dlq::{self, DlqStatus};
use vault::intake::{self, ParsedIntakeRow};
use vault::ledger as vault_ledger;
use vault::table;

fn expand_tilde(path: &str) -> PathBuf {
    if let Some(stripped) = path.strip_prefix("~/")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(stripped);
    }
    PathBuf::from(path)
}

fn vault_root(config: &Config) -> PathBuf {
    expand_tilde(&config.vault.root_path)
}

fn orphans_path(config: &Config) -> PathBuf {
    vault_root(config).join("system").join("views").join("borg-orphans.md")
}

fn ledger_trace_ids(ledger_path: &Path) -> Result<HashSet<String>> {
    if !ledger_path.exists() {
        return Ok(HashSet::new());
    }
    let content = std::fs::read_to_string(ledger_path).context("read ledger")?;
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
    let now = Local::now();
    Some((now - local).num_seconds())
}

/// Walk intake -> ledger / dlq and report orphans (intake rows older than
/// `bound_secs` with no matching row in either store). Writes
/// `system/views/borg-orphans.md`.
pub async fn run_orphan_audit(config: &Config, bound_secs: u64) -> Result<()> {
    log::debug!("dlq_cli::run_orphan_audit: bound_secs={bound_secs}");
    let intake_md = intake_helper::intake_path(config);
    let dlq_md = intake_helper::dlq_path(config);
    let ledger_md = ledger::ledger_path(config);

    let intake_rows = intake::parse_entries(&intake_md).context("parse intake")?;
    let ledger_traces = ledger_trace_ids(&ledger_md).context("parse ledger")?;
    let dlq_rows = dlq::parse_entries(&dlq_md).context("parse dlq")?;
    let dlq_traces: HashSet<String> = dlq_rows.iter().map(|r| r.trace_id.clone()).collect();

    log::info!(
        "audit --invariant: intake={} ledger={} dlq={}",
        intake_rows.len(),
        ledger_traces.len(),
        dlq_rows.len(),
    );

    let mut orphans: Vec<&ParsedIntakeRow> = Vec::new();
    let mut intake_only_recent = 0u64;
    let mut asymmetric_ledger = 0u64;
    let mut asymmetric_dlq = 0u64;

    let bound = bound_secs as i64;
    for row in &intake_rows {
        if ledger_traces.contains(&row.trace_id) || dlq_traces.contains(&row.trace_id) {
            continue;
        }
        match intake_age_secs(row) {
            Some(age) if age >= bound => orphans.push(row),
            Some(_) => intake_only_recent += 1,
            None => {
                log::warn!(
                    "audit --invariant: cannot parse intake timestamp for trace={} ({} {})",
                    row.trace_id,
                    row.date,
                    row.time
                );
            }
        }
    }

    let intake_traces: HashSet<String> = intake_rows.iter().map(|r| r.trace_id.clone()).collect();
    for trace in &ledger_traces {
        if !intake_traces.contains(trace) {
            asymmetric_ledger += 1;
        }
    }
    for trace in &dlq_traces {
        if !intake_traces.contains(trace) {
            asymmetric_dlq += 1;
        }
    }

    let orphans_md_path = orphans_path(config);
    write_orphans_md(&orphans_md_path, &orphans)?;

    println!(
        "audit --invariant complete:\n  intake rows scanned: {}\n  ledger resolutions: {}\n  dlq resolutions: {}\n  orphans (>{}s no resolution): {}\n  intake rows still within deadline: {}\n  ledger rows with no intake row: {}\n  dlq rows with no intake row: {}\n  wrote: {}",
        intake_rows.len(),
        ledger_traces.len(),
        dlq_traces.len(),
        bound_secs,
        orphans.len(),
        intake_only_recent,
        asymmetric_ledger,
        asymmetric_dlq,
        orphans_md_path.display(),
    );

    Ok(())
}

fn write_orphans_md(path: &Path, orphans: &[&ParsedIntakeRow]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).context("create orphans parent dir")?;
    }
    let header = "| Date | Time | Method | Origin | Kind | Preview | Trace |";
    let separator = "|------|------|--------|--------|------|---------|-------|";
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let mut body = format!(
        "---\ntitle: Borg Orphans\ndate: {today}\ntype: system\ndomain: system\norigin: authored\ntags:\n  - obsidian-borg\n  - system\n---\n\n# Borg Orphans\n\nIntake rows with no matching ledger or DLQ resolution within the deadline. Written by `borg audit --invariant`; do not edit by hand.\n\nSee also: [[borg-intake]], [[borg-ledger]], [[borg-dlq]], [[borg-dashboard]]\n\n{header}\n{separator}\n",
    );
    for row in orphans {
        let line = table::format_row(&[
            ("Date", row.date.as_str()),
            ("Time", row.time.as_str()),
            ("Method", row.method.as_str()),
            ("Origin", row.origin_ctx.as_str()),
            ("Kind", row.kind.as_str()),
            ("Preview", row.preview.as_str()),
            ("Trace", row.trace_id.as_str()),
        ]);
        body.push_str(&line);
        body.push('\n');
    }
    std::fs::write(path, body).context("write orphans.md")?;
    Ok(())
}

pub async fn run_intake_list(
    config: &Config,
    method: Option<String>,
    since: Option<String>,
    limit: usize,
) -> Result<()> {
    let intake_md = intake_helper::intake_path(config);
    let rows = intake::parse_entries(&intake_md).context("parse intake")?;
    let filtered: Vec<&ParsedIntakeRow> = rows
        .iter()
        .filter(|r| method.as_deref().is_none_or(|m| r.method == m))
        .filter(|r| since.as_deref().is_none_or(|s| r.date.as_str() >= s))
        .take(limit)
        .collect();
    if filtered.is_empty() {
        println!("(no intake rows match)");
        return Ok(());
    }
    println!("Date        Time  Method    Origin        Kind      Trace      Preview");
    for r in &filtered {
        let preview = if r.preview.len() > 60 {
            format!("{}...", &r.preview[..60])
        } else {
            r.preview.clone()
        };
        println!(
            "{:11} {:5} {:9} {:13} {:9} {:9} {}",
            r.date, r.time, r.method, r.origin_ctx, r.kind, r.trace_id, preview
        );
    }
    Ok(())
}

pub async fn run_intake_show(config: &Config, trace_id: &str) -> Result<()> {
    let intake_md = intake_helper::intake_path(config);
    let Some(row) = intake::find_by_trace(&intake_md, trace_id)? else {
        bail!("trace_id {trace_id} not found in intake log");
    };
    println!("Intake row:");
    println!("  date: {}", row.date);
    println!("  time: {}", row.time);
    println!("  method: {}", row.method);
    println!("  origin: {}", row.origin_ctx);
    println!("  kind: {}", row.kind);
    println!("  preview: {}", row.preview);
    println!("  trace: {}", row.trace_id);

    let sidecar = intake::raw_input_path(&vault_root(config), trace_id);
    if sidecar.exists() {
        let bytes = std::fs::read(&sidecar).context("read sidecar")?;
        println!("\n--- sidecar {} ({} bytes) ---", sidecar.display(), bytes.len());
        match std::str::from_utf8(&bytes) {
            Ok(s) => println!("{s}"),
            Err(_) => println!("[binary - {} bytes]", bytes.len()),
        }
    } else {
        println!("\n(no sidecar at {})", sidecar.display());
    }
    Ok(())
}

pub async fn run_dlq_list(
    config: &Config,
    method: Option<String>,
    stage: Option<String>,
    status: Option<String>,
    limit: usize,
) -> Result<()> {
    let dlq_md = intake_helper::dlq_path(config);
    let rows = dlq::parse_entries(&dlq_md).context("parse dlq")?;
    let filtered: Vec<_> = rows
        .iter()
        .filter(|r| method.as_deref().is_none_or(|m| r.method == m))
        .filter(|r| stage.as_deref().is_none_or(|s| r.stage == s))
        .filter(|r| status.as_deref().is_none_or(|s| r.status == s))
        .take(limit)
        .collect();
    if filtered.is_empty() {
        println!("(no dlq rows match)");
        return Ok(());
    }
    println!("Date        Time  Method    Stage              Status     Trace      Reason");
    for r in &filtered {
        let reason = if r.reason.len() > 60 {
            format!("{}...", &r.reason[..60])
        } else {
            r.reason.clone()
        };
        println!(
            "{:11} {:5} {:9} {:18} {:10} {:9} {}",
            r.date, r.time, r.method, r.stage, r.status, r.trace_id, reason
        );
    }
    Ok(())
}

pub async fn run_dlq_show(config: &Config, trace_id: &str) -> Result<()> {
    let dlq_md = intake_helper::dlq_path(config);
    let Some(dlq_row) = dlq::find_by_trace(&dlq_md, trace_id)? else {
        bail!("trace_id {trace_id} not found in DLQ");
    };
    println!("DLQ row:");
    println!("  date: {} {}", dlq_row.date, dlq_row.time);
    println!("  method: {}", dlq_row.method);
    println!("  stage: {}", dlq_row.stage);
    println!("  status: {}", dlq_row.status);
    println!("  retries: {}", dlq_row.retries);
    println!("  trace: {}", dlq_row.trace_id);
    if let Some(r) = &dlq_row.replay_of {
        println!("  replay-of: {r}");
    }
    println!("  reason: {}", dlq_row.reason);
    println!("  preview: {}", dlq_row.preview);

    // Intake + sidecar
    println!();
    run_intake_show(config, trace_id).await?;

    // Ledger (likely empty - if there was a ledger row we wouldn't have a
    // pending DLQ entry - but the replay path can leave both)
    if let Ok(Some(ledger)) = vault_ledger::find_completed(&ledger::ledger_path(config), &dlq_row.preview)
        .map(|o| o.map(|_| dlq_row.preview.clone()))
    {
        println!("\n(ledger has a completed row for this source: {ledger})");
    }

    Ok(())
}

pub async fn run_dlq_archive(config: &Config, trace_id: &str, status: &str) -> Result<()> {
    let new_status = DlqStatus::from_str(status).map_err(|e| eyre::eyre!(e))?;
    let dlq_md = intake_helper::dlq_path(config);
    let changed = dlq::update_status(&dlq_md, trace_id, new_status)?;
    if changed {
        println!("updated dlq trace={trace_id} status={new_status}");
    } else {
        bail!("trace_id {trace_id} not found in DLQ");
    }
    Ok(())
}

#[cfg(test)]
mod tests;
