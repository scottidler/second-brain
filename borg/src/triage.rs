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
use crate::receipts;
use chrono::{Local, NaiveDateTime, TimeZone};
use eyre::{Context, Result, bail};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use vault::dlq::{self, DlqStatus, ParsedDlqRow};
use vault::intake::{self, ParsedIntakeRow};
use vault::ledger as vault_ledger;
use vault::receipts::{FailureStage, ReceiptStatus};
use vault::schema::Method;
use vault::table;

/// One intake-sidecar payload, either UTF-8 decoded or flagged binary.
#[derive(Debug)]
pub enum SidecarContent {
    Utf8(String),
    Binary,
}

/// Resolved sidecar associated with an intake row.
#[derive(Debug)]
pub struct IntakeSidecar {
    pub path: PathBuf,
    pub size_bytes: usize,
    pub content: SidecarContent,
}

/// Detailed view of an intake row: the parsed row plus its sidecar (or
/// the path where one was expected when missing). Used by `intake_row`
/// and by `dlq_row` (which embeds the intake detail for the originating
/// trace).
#[derive(Debug)]
pub struct IntakeRowDetail {
    pub row: ParsedIntakeRow,
    /// `Ok(payload)` when the sidecar file exists; `Err(expected_path)`
    /// when it does not (sb formats "(no sidecar at <path>)").
    pub sidecar: std::result::Result<IntakeSidecar, PathBuf>,
}

/// Detailed view of a DLQ row: the parsed row plus the intake detail
/// for the originating trace (if found) and an optional ledger source
/// hit (which can happen on the replay path when both stores carry the
/// same trace).
#[derive(Debug)]
pub struct DlqRowDetail {
    pub row: ParsedDlqRow,
    pub intake: Option<IntakeRowDetail>,
    pub ledger_has_completed_for: Option<String>,
}

/// Outcome of `borg::triage::orphan_audit`. sb prints the summary
/// (counts + path the markdown view was written to).
#[derive(Debug)]
pub struct OrphanAuditReport {
    pub intake_scanned: usize,
    pub ledger_resolutions: usize,
    pub dlq_resolutions: usize,
    pub bound_secs: u64,
    pub orphans_found: usize,
    pub intake_recent: u64,
    pub asymmetric_ledger: u64,
    pub asymmetric_dlq: u64,
    pub orphans_path: PathBuf,
}

/// Outcome of `borg::triage::dlq_archive`. The two operating modes
/// (`--resolved` bulk archive vs single-trace status update) produce
/// distinct variants so sb formats per case.
#[derive(Debug)]
pub enum DlqArchiveOutcome {
    ResolvedBatch { moved: usize, archive_path: PathBuf },
    StatusUpdated { trace_id: String, new_status: DlqStatus },
}

/// Outcome of `borg::triage::dlq_replay`. Carries the new trace, the
/// original trace, the method that was re-dispatched, and the
/// re-dispatched pipeline run's status; sb formats one line.
#[derive(Debug)]
pub struct DlqReplayOutcome {
    pub original_trace: String,
    pub new_trace: String,
    pub method: crate::types::IngestMethod,
    pub result_status: crate::types::IngestStatus,
}

fn vault_root(config: &Config) -> Result<PathBuf> {
    config.vault_root()
}

fn orphans_path(config: &Config) -> Result<PathBuf> {
    Ok(vault_root(config)?.join("system").join("views").join("borg-orphans.md"))
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
    let intake_md = intake_helper::intake_path(config)?;
    let dlq_md = intake_helper::dlq_path(config)?;
    let ledger_md = ledger::ledger_path(config)?;

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
/// `system/views/borg-orphans.md` as a side effect; sb formats the
/// summary.
pub async fn orphan_audit(config: &Config, bound_secs: u64) -> Result<OrphanAuditReport> {
    log::debug!("triage::orphan_audit: bound_secs={bound_secs}");
    let intake_md = intake_helper::intake_path(config)?;
    let dlq_md = intake_helper::dlq_path(config)?;
    let ledger_md = ledger::ledger_path(config)?;

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

    let orphans_md_path = orphans_path(config)?;
    write_orphans_md(&orphans_md_path, &orphans)?;

    Ok(OrphanAuditReport {
        intake_scanned: intake_rows.len(),
        ledger_resolutions: ledger_traces.len(),
        dlq_resolutions: dlq_traces.len(),
        bound_secs,
        orphans_found: orphans.len(),
        intake_recent: intake_only_recent,
        asymmetric_ledger,
        asymmetric_dlq,
        orphans_path: orphans_md_path,
    })
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

/// Filter intake rows by method/since/limit; sb formats the table.
pub async fn intake_rows(
    config: &Config,
    method: Option<String>,
    since: Option<String>,
    limit: usize,
) -> Result<Vec<ParsedIntakeRow>> {
    let intake_md = intake_helper::intake_path(config)?;
    let rows = intake::parse_entries(&intake_md).context("parse intake")?;
    Ok(rows
        .into_iter()
        .filter(|r| method.as_deref().is_none_or(|m| r.method == m))
        .filter(|r| since.as_deref().is_none_or(|s| r.date.as_str() >= s))
        .take(limit)
        .collect())
}

/// Look up one intake row by trace id and pair it with the sidecar
/// payload (or the expected path when absent).
pub async fn intake_row(config: &Config, trace_id: &str) -> Result<IntakeRowDetail> {
    let intake_md = intake_helper::intake_path(config)?;
    let Some(row) = intake::find_by_trace(&intake_md, trace_id)? else {
        bail!("trace_id {trace_id} not found in intake log");
    };

    let sidecar_path = intake::raw_input_path(&vault_root(config)?, trace_id);
    let sidecar = if sidecar_path.exists() {
        let bytes = std::fs::read(&sidecar_path).context("read sidecar")?;
        let content = match std::str::from_utf8(&bytes) {
            Ok(s) => SidecarContent::Utf8(s.to_string()),
            Err(_) => SidecarContent::Binary,
        };
        Ok(IntakeSidecar {
            size_bytes: bytes.len(),
            path: sidecar_path,
            content,
        })
    } else {
        Err(sidecar_path)
    };

    Ok(IntakeRowDetail { row, sidecar })
}

/// Filter DLQ rows by method/stage/status/limit; sb formats the table.
pub async fn dlq_rows(
    config: &Config,
    method: Option<String>,
    stage: Option<String>,
    status: Option<String>,
    limit: usize,
) -> Result<Vec<ParsedDlqRow>> {
    let dlq_md = intake_helper::dlq_path(config)?;
    let rows = dlq::parse_entries(&dlq_md).context("parse dlq")?;
    Ok(rows
        .into_iter()
        .filter(|r| method.as_deref().is_none_or(|m| r.method == m))
        .filter(|r| stage.as_deref().is_none_or(|s| r.stage == s))
        .filter(|r| status.as_deref().is_none_or(|s| r.status == s))
        .take(limit)
        .collect())
}

/// Look up one DLQ row by trace id; carry along the intake detail (if
/// any) and an optional ledger-completed flag so sb can render the
/// composite view.
pub async fn dlq_row(config: &Config, trace_id: &str) -> Result<DlqRowDetail> {
    let dlq_md = intake_helper::dlq_path(config)?;
    let Some(row) = dlq::find_by_trace(&dlq_md, trace_id)? else {
        bail!("trace_id {trace_id} not found in DLQ");
    };

    // Intake + sidecar (best-effort; the replay path can leave a DLQ row
    // whose original trace is gone, so a missing intake row is not an error).
    let intake = intake_row(config, trace_id).await.ok();

    // Ledger lookup: only matters on the replay path (when both stores
    // carry the same source). We return the source string when there's
    // a completed row, so sb can mention it.
    let ledger_has_completed_for = match vault_ledger::find_completed(&ledger::ledger_path(config)?, &row.preview) {
        Ok(Some(_)) => Some(row.preview.clone()),
        _ => None,
    };

    Ok(DlqRowDetail {
        row,
        intake,
        ledger_has_completed_for,
    })
}

/// Bulk-archive resolved DLQ rows, or update a single trace's status.
/// sb formats per variant.
pub async fn dlq_archive(
    config: &Config,
    trace_id: Option<String>,
    status: &str,
    resolved_mode: bool,
) -> Result<DlqArchiveOutcome> {
    let dlq_md = intake_helper::dlq_path(config)?;
    if resolved_mode {
        let archive_path = vault_root(config)?
            .join("system")
            .join("views")
            .join("borg-dlq-archive.md");
        let moved = dlq::archive_resolved(&dlq_md, &archive_path).context("archive resolved rows")?;
        return Ok(DlqArchiveOutcome::ResolvedBatch { moved, archive_path });
    }
    let Some(trace_id) = trace_id else {
        bail!("archive: provide a trace_id or use --resolved");
    };
    let new_status = DlqStatus::from_str(status).map_err(|e| eyre::eyre!(e))?;
    let changed = dlq::update_status(&dlq_md, &trace_id, new_status)?;
    if changed {
        Ok(DlqArchiveOutcome::StatusUpdated { trace_id, new_status })
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
pub async fn dlq_replay(config: &Config, original_trace: &str) -> Result<DlqReplayOutcome> {
    log::debug!("triage::dlq_replay: original_trace={original_trace}");
    let intake_md = intake_helper::intake_path(config)?;
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
    let sidecar = intake::raw_input_path(&vault_root(config)?, original_trace);
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
    let result = match orig.kind.as_str() {
        "url" => {
            let url = orig.preview.clone();
            log::info!("replay: dispatching URL {url} new_trace={new_trace} original={original_trace}");
            crate::pipeline::process_content(
                crate::types::ContentKind::Url(url),
                vec![],
                method,
                /* force */ true,
                config,
                Some(new_trace.clone()),
            )
            .await
        }
        "text" => {
            let text = if !sidecar_bytes.is_empty() {
                String::from_utf8_lossy(&sidecar_bytes).into_owned()
            } else {
                orig.preview.clone()
            };
            log::info!("replay: dispatching text new_trace={new_trace} original={original_trace}");
            crate::pipeline::process_content(
                crate::types::ContentKind::Text(text),
                vec![],
                method,
                /* force */ true,
                config,
                Some(new_trace.clone()),
            )
            .await
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
    };

    Ok(DlqReplayOutcome {
        original_trace: original_trace.to_string(),
        new_trace,
        method,
        result_status: result.status,
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
