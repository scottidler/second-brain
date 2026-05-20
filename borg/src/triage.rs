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

/// Compute current invariant health: how many intake rows have no ledger
/// or DLQ resolution, and how old the oldest unresolved row is. Used by
/// the `GET /health/audit` HTTP endpoint so operators can poll without
/// re-reading the markdown tables.
pub fn audit_health_stats(config: &Config) -> Result<crate::routes::AuditHealth> {
    let intake_md = intake_helper::intake_path(config);
    let dlq_md = intake_helper::dlq_path(config);
    let ledger_md = ledger::ledger_path(config);

    let intake_rows = intake::parse_entries(&intake_md).context("parse intake")?;
    let ledger_traces = ledger_trace_ids(&ledger_md).context("parse ledger")?;
    let dlq_rows = dlq::parse_entries(&dlq_md).context("parse dlq")?;
    let dlq_traces: std::collections::HashSet<String> = dlq_rows.iter().map(|r| r.trace_id.clone()).collect();

    let mut orphan_count = 0usize;
    let mut oldest_age: Option<i64> = None;
    for row in &intake_rows {
        if ledger_traces.contains(&row.trace_id) || dlq_traces.contains(&row.trace_id) {
            continue;
        }
        if let Some(age) = intake_age_secs(row) {
            orphan_count += 1;
            oldest_age = Some(oldest_age.map_or(age, |o| o.max(age)));
        }
    }
    let dlq_pending = dlq_rows.iter().filter(|r| r.status == "pending").count();

    Ok(crate::routes::AuditHealth {
        orphan_count,
        oldest_orphan_secs: oldest_age,
        intake_rows: intake_rows.len(),
        ledger_rows: ledger_traces.len(),
        dlq_rows: dlq_rows.len(),
        dlq_pending,
    })
}

/// Walk intake -> ledger / dlq and report orphans (intake rows older than
/// `bound_secs` with no matching row in either store). Writes
/// `system/views/borg-orphans.md`.
pub async fn orphan_audit(config: &Config, bound_secs: u64) -> Result<Vec<String>> {
    log::debug!("triage::orphan_audit: bound_secs={bound_secs}");
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

    Ok(vec![format!(
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
    )])
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

pub async fn intake_rows(
    config: &Config,
    method: Option<String>,
    since: Option<String>,
    limit: usize,
) -> Result<Vec<String>> {
    let intake_md = intake_helper::intake_path(config);
    let rows = intake::parse_entries(&intake_md).context("parse intake")?;
    let filtered: Vec<&ParsedIntakeRow> = rows
        .iter()
        .filter(|r| method.as_deref().is_none_or(|m| r.method == m))
        .filter(|r| since.as_deref().is_none_or(|s| r.date.as_str() >= s))
        .take(limit)
        .collect();
    if filtered.is_empty() {
        return Ok(vec!["(no intake rows match)".to_string()]);
    }
    let mut lines = vec!["Date        Time  Method    Origin        Kind      Trace      Preview".to_string()];
    for r in &filtered {
        let preview = if r.preview.len() > 60 {
            format!("{}...", &r.preview[..60])
        } else {
            r.preview.clone()
        };
        lines.push(format!(
            "{:11} {:5} {:9} {:13} {:9} {:9} {}",
            r.date, r.time, r.method, r.origin_ctx, r.kind, r.trace_id, preview
        ));
    }
    Ok(lines)
}

pub async fn intake_row(config: &Config, trace_id: &str) -> Result<Vec<String>> {
    let intake_md = intake_helper::intake_path(config);
    let Some(row) = intake::find_by_trace(&intake_md, trace_id)? else {
        bail!("trace_id {trace_id} not found in intake log");
    };
    let mut lines = vec![
        "Intake row:".to_string(),
        format!("  date: {}", row.date),
        format!("  time: {}", row.time),
        format!("  method: {}", row.method),
        format!("  origin: {}", row.origin_ctx),
        format!("  kind: {}", row.kind),
        format!("  preview: {}", row.preview),
        format!("  trace: {}", row.trace_id),
    ];

    let sidecar = intake::raw_input_path(&vault_root(config), trace_id);
    if sidecar.exists() {
        let bytes = std::fs::read(&sidecar).context("read sidecar")?;
        lines.push(format!(
            "\n--- sidecar {} ({} bytes) ---",
            sidecar.display(),
            bytes.len()
        ));
        match std::str::from_utf8(&bytes) {
            Ok(s) => lines.push(s.to_string()),
            Err(_) => lines.push(format!("[binary - {} bytes]", bytes.len())),
        }
    } else {
        lines.push(format!("\n(no sidecar at {})", sidecar.display()));
    }
    Ok(lines)
}

pub async fn dlq_rows(
    config: &Config,
    method: Option<String>,
    stage: Option<String>,
    status: Option<String>,
    limit: usize,
) -> Result<Vec<String>> {
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
        return Ok(vec!["(no dlq rows match)".to_string()]);
    }
    let mut lines = vec!["Date        Time  Method    Stage              Status     Trace      Reason".to_string()];
    for r in &filtered {
        let reason = if r.reason.len() > 60 {
            format!("{}...", &r.reason[..60])
        } else {
            r.reason.clone()
        };
        lines.push(format!(
            "{:11} {:5} {:9} {:18} {:10} {:9} {}",
            r.date, r.time, r.method, r.stage, r.status, r.trace_id, reason
        ));
    }
    Ok(lines)
}

pub async fn dlq_row(config: &Config, trace_id: &str) -> Result<Vec<String>> {
    let dlq_md = intake_helper::dlq_path(config);
    let Some(dlq_row) = dlq::find_by_trace(&dlq_md, trace_id)? else {
        bail!("trace_id {trace_id} not found in DLQ");
    };
    let mut lines = vec![
        "DLQ row:".to_string(),
        format!("  date: {} {}", dlq_row.date, dlq_row.time),
        format!("  method: {}", dlq_row.method),
        format!("  stage: {}", dlq_row.stage),
        format!("  status: {}", dlq_row.status),
        format!("  retries: {}", dlq_row.retries),
        format!("  trace: {}", dlq_row.trace_id),
    ];
    if let Some(r) = &dlq_row.replay_of {
        lines.push(format!("  replay-of: {r}"));
    }
    lines.push(format!("  reason: {}", dlq_row.reason));
    lines.push(format!("  preview: {}", dlq_row.preview));

    // Intake + sidecar
    lines.push(String::new());
    lines.extend(intake_row(config, trace_id).await?);

    // Ledger (likely empty - if there was a ledger row we wouldn't have a
    // pending DLQ entry - but the replay path can leave both)
    if let Ok(Some(ledger)) = vault_ledger::find_completed(&ledger::ledger_path(config), &dlq_row.preview)
        .map(|o| o.map(|_| dlq_row.preview.clone()))
    {
        lines.push(format!("\n(ledger has a completed row for this source: {ledger})"));
    }

    Ok(lines)
}

pub async fn dlq_archive(
    config: &Config,
    trace_id: Option<String>,
    status: &str,
    resolved_mode: bool,
) -> Result<Vec<String>> {
    let dlq_md = intake_helper::dlq_path(config);
    if resolved_mode {
        let archive_md = vault_root(config)
            .join("system")
            .join("views")
            .join("borg-dlq-archive.md");
        let moved = dlq::archive_resolved(&dlq_md, &archive_md).context("archive resolved rows")?;
        return Ok(vec![format!(
            "archive --resolved: moved {moved} resolved/abandoned row(s) to {}",
            archive_md.display()
        )]);
    }
    let Some(trace_id) = trace_id else {
        bail!("archive: provide a trace_id or use --resolved");
    };
    let new_status = DlqStatus::from_str(status).map_err(|e| eyre::eyre!(e))?;
    let changed = dlq::update_status(&dlq_md, &trace_id, new_status)?;
    if changed {
        Ok(vec![format!("updated dlq trace={trace_id} status={new_status}")])
    } else {
        bail!("trace_id {trace_id} not found in DLQ");
    }
}

/// Replay a DLQ entry. Reads the intake row + sidecar for the original
/// trace, generates a NEW trace_id (so the replay attempt is itself
/// recorded), writes a new intake row with `replay_of = <original>`
/// implicit context (preview prefixed), and re-injects the input through
/// the same method's pipeline. The new attempt's DLQ entry (if it fails
/// again) carries `replay_of: <original>`. Currently supports URL and
/// text payloads; binary replay requires the sidecar to contain bytes,
/// which today is a descriptor only - so binary inputs are rejected with
/// a clear error rather than silently producing a wrong-bytes ingest.
pub async fn dlq_replay(config: &Config, original_trace: &str) -> Result<Vec<String>> {
    log::debug!("triage::dlq_replay: original_trace={original_trace}");
    let intake_md = intake_helper::intake_path(config);
    let Some(orig) = intake::find_by_trace(&intake_md, original_trace)? else {
        bail!("trace_id {original_trace} not found in intake log");
    };

    let method: crate::types::IngestMethod = match orig.method.as_str() {
        "telegram" => crate::types::IngestMethod::Telegram,
        "discord" => crate::types::IngestMethod::Discord,
        "http" => crate::types::IngestMethod::Http,
        "clipboard" => crate::types::IngestMethod::Clipboard,
        "cli" => crate::types::IngestMethod::Cli,
        "ntfy" => crate::types::IngestMethod::Ntfy,
        other => bail!("replay: unknown method `{other}` on original intake row"),
    };

    let new_trace = crate::trace::generate(method);
    let sidecar = intake::raw_input_path(&vault_root(config), original_trace);
    let sidecar_bytes = if sidecar.exists() {
        std::fs::read(&sidecar).context("read original sidecar")?
    } else {
        Vec::new()
    };

    let preview = format!("replay-of:{original_trace} | {}", orig.preview);

    // Write the new intake row tying replay back to the original.
    crate::intake::record_intake_with_sidecar(
        config,
        method,
        &orig.origin_ctx,
        intake::IntakeKind::from_str(&orig.kind).unwrap_or(intake::IntakeKind::Unknown),
        &preview,
        if sidecar_bytes.is_empty() { orig.preview.as_bytes() } else { &sidecar_bytes },
        &new_trace,
    )
    .context("failed to record replay intake")?;

    // Dispatch by kind. Only URL and Text are losslessly replayable from
    // the sidecar; for everything else we record an immediate DLQ entry
    // with the replay_of pointer so the operator knows the replay was
    // attempted.
    match orig.kind.as_str() {
        "url" => {
            let url = orig.preview.clone();
            log::info!("replay: dispatching URL {url} new_trace={new_trace} original={original_trace}");
            let result = crate::pipeline::process_content(
                crate::types::ContentKind::Url(url.clone()),
                vec![],
                method,
                /* force */ true,
                config,
                Some(new_trace.clone()),
            )
            .await;
            Ok(vec![format!(
                "replay: trace={new_trace} replay_of={original_trace} result={:?}",
                result.status
            )])
        }
        "text" => {
            let text = if !sidecar_bytes.is_empty() {
                String::from_utf8_lossy(&sidecar_bytes).into_owned()
            } else {
                orig.preview.clone()
            };
            log::info!("replay: dispatching text new_trace={new_trace} original={original_trace}");
            let result = crate::pipeline::process_content(
                crate::types::ContentKind::Text(text),
                vec![],
                method,
                /* force */ true,
                config,
                Some(new_trace.clone()),
            )
            .await;
            Ok(vec![format!(
                "replay: trace={new_trace} replay_of={original_trace} result={:?}",
                result.status
            )])
        }
        other => {
            let reason = format!("replay: binary kind `{other}` cannot be replayed from text sidecar");
            crate::intake::record_dlq(
                config,
                method,
                crate::intake::Stage::IntakeReject,
                &reason,
                &preview,
                &new_trace,
                Some(original_trace),
            );
            bail!("{reason}");
        }
    }
}

#[cfg(test)]
mod tests;
