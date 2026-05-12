//! Dead Letter Queue: append-only record of every input borg failed to
//! process successfully. Pairs with `vault::intake`: every trace_id that
//! lands in the intake log must eventually appear in the ledger (success
//! path) or here (failure path). `borg audit` enforces that invariant.

use crate::schema::Method;
use crate::table::{self, RowMap};
use eyre::{Context, Result};
use fs2::FileExt;
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DlqStage {
    /// Filtered or rejected at the intake door (disallowed chat, unsupported
    /// media, bad payload).
    IntakeReject,
    /// The classifier could not figure out what to do with the input.
    ClassifyFailed,
    /// Network fetch / extractor failure (fabric + jina both failed,
    /// blocklist hit, 4xx/5xx).
    FetchFailed,
    /// Quality gate refused to publish the produced note.
    QualityBlocked,
    /// `PIPELINE_HARD_TIMEOUT_SECS` elapsed before publish.
    PipelineTimedOut,
    /// `write_atomic` failure when publishing the final note.
    PublishFailed,
    /// Background watchdog detected an intake row with no resolution within
    /// the deadline window.
    WatchdogOrphan,
}

impl DlqStage {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::IntakeReject => "intake-reject",
            Self::ClassifyFailed => "classify-failed",
            Self::FetchFailed => "fetch-failed",
            Self::QualityBlocked => "quality-blocked",
            Self::PipelineTimedOut => "pipeline-timed-out",
            Self::PublishFailed => "publish-failed",
            Self::WatchdogOrphan => "watchdog-orphan",
        }
    }
}

impl std::fmt::Display for DlqStage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl FromStr for DlqStage {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "intake-reject" => Ok(Self::IntakeReject),
            "classify-failed" => Ok(Self::ClassifyFailed),
            "fetch-failed" => Ok(Self::FetchFailed),
            "quality-blocked" => Ok(Self::QualityBlocked),
            "pipeline-timed-out" => Ok(Self::PipelineTimedOut),
            "publish-failed" => Ok(Self::PublishFailed),
            "watchdog-orphan" => Ok(Self::WatchdogOrphan),
            _ => Err(format!("unknown dlq stage: {s}")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DlqStatus {
    Pending,
    Retried,
    Abandoned,
    Resolved,
}

impl DlqStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Retried => "retried",
            Self::Abandoned => "abandoned",
            Self::Resolved => "resolved",
        }
    }
}

impl std::fmt::Display for DlqStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl FromStr for DlqStatus {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "pending" => Ok(Self::Pending),
            "retried" => Ok(Self::Retried),
            "abandoned" => Ok(Self::Abandoned),
            "resolved" => Ok(Self::Resolved),
            _ => Err(format!("unknown dlq status: {s}")),
        }
    }
}

pub struct DlqEntry {
    pub date: String,
    pub time: String,
    pub method: Method,
    pub stage: DlqStage,
    pub reason: String,
    pub preview: String,
    pub retries: u32,
    pub status: DlqStatus,
    pub trace_id: String,
    pub replay_of: Option<String>,
}

const DLQ_FRONTMATTER: &str = r#"---
title: Borg Dead Letter Queue
date: {date}
type: system
domain: system
origin: authored
tags:
  - obsidian-borg
  - system
---

# Borg Dead Letter Queue

Every input borg received but did not produce a successful ledger row for. This file is machine-maintained - do not edit the table manually. Each trace_id here must also exist in [[borg-intake]]; `borg audit` walks the invariant.

See also: [[borg-intake]], [[borg-ledger]], [[borg-dashboard]]

| Date | Time | Method | Stage | Reason | Preview | Retries | Status | Trace | Replay-Of |
|------|------|--------|-------|--------|---------|---------|--------|-------|-----------|
"#;

pub const COL_DATE: &str = "Date";
pub const COL_TIME: &str = "Time";
pub const COL_METHOD: &str = "Method";
pub const COL_STAGE: &str = "Stage";
pub const COL_REASON: &str = "Reason";
pub const COL_PREVIEW: &str = "Preview";
pub const COL_RETRIES: &str = "Retries";
pub const COL_STATUS: &str = "Status";
pub const COL_TRACE: &str = "Trace";
pub const COL_REPLAY_OF: &str = "Replay-Of";

fn canonical_columns() -> &'static [&'static str] {
    &[
        COL_DATE,
        COL_TIME,
        COL_METHOD,
        COL_STAGE,
        COL_REASON,
        COL_PREVIEW,
        COL_RETRIES,
        COL_STATUS,
        COL_TRACE,
        COL_REPLAY_OF,
    ]
}

const DLQ_HEADER: &str = "| Date | Time | Method | Stage | Reason | Preview | Retries | Status | Trace | Replay-Of |";
const DLQ_SEPARATOR: &str =
    "|------|------|--------|-------|--------|---------|---------|--------|-------|-----------|";

pub fn dlq_path(vault_root: &Path) -> PathBuf {
    vault_root.join("system").join("views").join("borg-dlq.md")
}

pub fn dlq_archive_path(vault_root: &Path) -> PathBuf {
    vault_root.join("system").join("views").join("borg-dlq-archive.md")
}

pub fn ensure_dlq_exists(dlq_path: &Path) -> Result<()> {
    if dlq_path.exists() {
        return Ok(());
    }
    if let Some(parent) = dlq_path.parent() {
        fs::create_dir_all(parent).context("Failed to create Borg DLQ directory")?;
    }
    let date = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let content = DLQ_FRONTMATTER.replace("{date}", &date);
    fs::write(dlq_path, content).context("Failed to create Borg DLQ")?;
    log::info!("Created Borg DLQ at {}", dlq_path.display());
    Ok(())
}

pub fn append_entry(dlq_path: &Path, entry: &DlqEntry) -> Result<()> {
    log::debug!(
        "dlq::append_entry: trace={} method={} stage={} reason={} status={} replay_of={:?}",
        entry.trace_id,
        entry.method,
        entry.stage,
        entry.reason,
        entry.status,
        entry.replay_of,
    );
    ensure_dlq_exists(dlq_path)?;
    table::ensure_header_matches(dlq_path, DLQ_HEADER, DLQ_SEPARATOR, "Borg DLQ")?;

    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(dlq_path)
        .context("Failed to open Borg DLQ for writing")?;
    file.lock_exclusive()
        .context("Failed to acquire exclusive lock on Borg DLQ")?;

    let retries_str = entry.retries.to_string();
    let replay_str = entry.replay_of.as_deref().unwrap_or("-");
    let row = table::format_row(&[
        (COL_DATE, &entry.date),
        (COL_TIME, &entry.time),
        (COL_METHOD, entry.method.as_str()),
        (COL_STAGE, entry.stage.as_str()),
        (COL_REASON, &entry.reason),
        (COL_PREVIEW, &entry.preview),
        (COL_RETRIES, &retries_str),
        (COL_STATUS, entry.status.as_str()),
        (COL_TRACE, &entry.trace_id),
        (COL_REPLAY_OF, replay_str),
    ]);

    let content = fs::read_to_string(dlq_path).context("Failed to read Borg DLQ")?;
    let new_content = table::insert_after_separator(&content, &row);
    fs::write(dlq_path, new_content).context("Failed to write Borg DLQ")?;
    file.unlock().ok();

    Ok(())
}

#[derive(Debug, Clone)]
pub struct ParsedDlqRow {
    pub date: String,
    pub time: String,
    pub method: String,
    pub stage: String,
    pub reason: String,
    pub preview: String,
    pub retries: u32,
    pub status: String,
    pub trace_id: String,
    pub replay_of: Option<String>,
}

fn parse_row(row: &RowMap) -> Option<ParsedDlqRow> {
    let retries_raw = row.get(COL_RETRIES)?;
    let retries: u32 = retries_raw.parse().unwrap_or(0);
    let replay_raw = row.get(COL_REPLAY_OF).unwrap_or("-");
    let replay_of = if replay_raw == "-" || replay_raw.is_empty() {
        None
    } else {
        Some(replay_raw.to_string())
    };
    Some(ParsedDlqRow {
        date: row.get(COL_DATE)?.to_string(),
        time: row.get(COL_TIME)?.to_string(),
        method: row.get(COL_METHOD)?.to_string(),
        stage: row.get(COL_STAGE)?.to_string(),
        reason: row.get(COL_REASON)?.to_string(),
        preview: row.get(COL_PREVIEW)?.to_string(),
        retries,
        status: row.get(COL_STATUS)?.to_string(),
        trace_id: row.get(COL_TRACE)?.to_string(),
        replay_of,
    })
}

pub fn parse_entries(dlq_path: &Path) -> Result<Vec<ParsedDlqRow>> {
    if !dlq_path.exists() {
        return Ok(Vec::new());
    }
    let content = fs::read_to_string(dlq_path).context("Failed to read Borg DLQ")?;
    let table = table::parse_table(&content, canonical_columns())?;
    Ok(table.rows.iter().filter_map(parse_row).collect())
}

pub fn find_by_trace(dlq_path: &Path, trace_id: &str) -> Result<Option<ParsedDlqRow>> {
    let rows = parse_entries(dlq_path)?;
    Ok(rows.into_iter().find(|r| r.trace_id == trace_id))
}

/// Update the Status column for an existing DLQ row by trace_id. Rewrites the
/// entire file under an exclusive lock; no-op when the trace is absent.
pub fn update_status(dlq_path: &Path, trace_id: &str, new_status: DlqStatus) -> Result<bool> {
    log::debug!("dlq::update_status: trace={trace_id} new_status={new_status}");
    if !dlq_path.exists() {
        return Ok(false);
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(dlq_path)
        .context("Failed to open Borg DLQ for update")?;
    file.lock_exclusive()
        .context("Failed to acquire exclusive lock on Borg DLQ")?;

    let content = fs::read_to_string(dlq_path).context("Failed to read Borg DLQ")?;
    let parsed = table::parse_table(&content, canonical_columns())?;

    // Find which header column index Status / Trace map to by re-parsing the
    // header line. We rebuild the row in place so non-Status columns survive
    // verbatim.
    let lines: Vec<&str> = content.lines().collect();
    let mut header_idx = None;
    for (i, line) in lines.iter().enumerate() {
        if line.starts_with("| Date") || line.trim_start().starts_with("| Date") {
            header_idx = Some(i);
            break;
        }
    }
    let Some(hi) = header_idx else {
        return Ok(false);
    };
    let header_cells: Vec<&str> = lines[hi].split('|').map(|c| c.trim()).collect();
    let status_pos = header_cells.iter().position(|c| *c == COL_STATUS);
    let trace_pos = header_cells.iter().position(|c| *c == COL_TRACE);
    let (Some(status_pos), Some(trace_pos)) = (status_pos, trace_pos) else {
        return Ok(false);
    };

    let mut found = false;
    let mut new_lines: Vec<String> = Vec::with_capacity(lines.len());
    for line in &lines {
        if line.starts_with('|') && !line.starts_with("| Date") && !line.starts_with("|--") {
            let cells: Vec<&str> = line.split('|').collect();
            if cells.len() >= header_cells.len() && cells.get(trace_pos).map(|c| c.trim()) == Some(trace_id) {
                let mut new_cells: Vec<String> = cells.iter().map(|c| c.to_string()).collect();
                if let Some(slot) = new_cells.get_mut(status_pos) {
                    *slot = format!(" {} ", new_status.as_str());
                }
                new_lines.push(new_cells.join("|"));
                found = true;
                continue;
            }
        }
        new_lines.push((*line).to_string());
    }

    let trailing = if content.ends_with('\n') { "\n" } else { "" };
    fs::write(dlq_path, format!("{}{trailing}", new_lines.join("\n"))).context("Failed to write Borg DLQ")?;
    file.unlock().ok();

    // Sanity: the parsed view should have included the trace (warn only).
    if !found && parsed.rows.iter().any(|r| r.get(COL_TRACE) == Some(trace_id)) {
        log::warn!("dlq::update_status: parser saw {trace_id} but in-place rewrite did not");
    }

    Ok(found)
}

/// Move every DLQ row whose status is `Resolved` or `Abandoned` from
/// `borg-dlq.md` to `borg-dlq-archive.md` (created with the same header on
/// first use). Returns the number of rows moved.
pub fn archive_resolved(dlq_path: &Path, archive_path: &Path) -> Result<usize> {
    log::debug!(
        "dlq::archive_resolved: active={} archive={}",
        dlq_path.display(),
        archive_path.display()
    );
    if !dlq_path.exists() {
        return Ok(0);
    }

    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(dlq_path)
        .context("Failed to open Borg DLQ for archive")?;
    file.lock_exclusive()
        .context("Failed to acquire exclusive lock on Borg DLQ")?;

    let content = fs::read_to_string(dlq_path).context("Failed to read Borg DLQ")?;
    let lines: Vec<&str> = content.lines().collect();

    // Find header for cell-index lookup.
    let header_idx = lines.iter().position(|l| l.starts_with("| Date"));
    let Some(hi) = header_idx else {
        return Ok(0);
    };
    let header_cells: Vec<&str> = lines[hi].split('|').map(|c| c.trim()).collect();
    let status_pos = header_cells.iter().position(|c| *c == COL_STATUS);
    let Some(status_pos) = status_pos else {
        return Ok(0);
    };

    let mut kept: Vec<String> = Vec::new();
    let mut to_archive: Vec<String> = Vec::new();
    for line in &lines {
        if line.starts_with('|') && !line.starts_with("| Date") && !line.starts_with("|--") {
            let cells: Vec<&str> = line.split('|').collect();
            let status = cells.get(status_pos).map(|c| c.trim()).unwrap_or("");
            if status == "resolved" || status == "abandoned" {
                to_archive.push((*line).to_string());
                continue;
            }
        }
        kept.push((*line).to_string());
    }

    if to_archive.is_empty() {
        return Ok(0);
    }

    // Append archived rows to the archive file.
    ensure_dlq_archive_exists(archive_path)?;
    table::ensure_header_matches(archive_path, DLQ_HEADER, DLQ_SEPARATOR, "Borg DLQ Archive")?;
    let archive_content = fs::read_to_string(archive_path).context("Failed to read Borg DLQ Archive")?;
    let mut archive_updated = archive_content;
    for row in &to_archive {
        archive_updated = table::insert_after_separator(&archive_updated, row);
    }
    fs::write(archive_path, archive_updated).context("Failed to write Borg DLQ Archive")?;

    // Rewrite the active DLQ without the archived rows.
    let trailing = if content.ends_with('\n') { "\n" } else { "" };
    fs::write(dlq_path, format!("{}{trailing}", kept.join("\n"))).context("Failed to write Borg DLQ")?;
    file.unlock().ok();

    Ok(to_archive.len())
}

fn ensure_dlq_archive_exists(archive_path: &Path) -> Result<()> {
    if archive_path.exists() {
        return Ok(());
    }
    if let Some(parent) = archive_path.parent() {
        fs::create_dir_all(parent).context("Failed to create Borg DLQ Archive directory")?;
    }
    let date = chrono::Utc::now().format("%Y-%m-%d").to_string();
    // Reuse the active DLQ frontmatter but mark the title differently.
    let content = DLQ_FRONTMATTER
        .replace("{date}", &date)
        .replace("Borg Dead Letter Queue", "Borg DLQ Archive");
    fs::write(archive_path, content).context("Failed to create Borg DLQ Archive")?;
    log::info!("Created Borg DLQ Archive at {}", archive_path.display());
    Ok(())
}

#[cfg(test)]
mod tests;
