//! Intake log: append-only record of every input borg receives.
//!
//! Mirrors `vault::ledger` (same file-locking pattern, same insert-at-row-1
//! ordering, same header-drift repair) but parses rows by column NAME via a
//! header-derived index map - never by hardcoded numeric position. The
//! intake log is the first half of the durable-capture invariant introduced
//! by the 2026-05-11 intake-log + DLQ design doc: every received input
//! lands here synchronously, before any filter / classifier / pipeline can
//! silently drop it.

use crate::schema::Method;
use crate::table::{self, RowMap};
use eyre::{Context, Result};
use fs2::FileExt;
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::str::FromStr;

/// Coarse classification of the received input, recorded on the intake row.
/// Independent of `vault::schema::NoteType` because intake happens before
/// classification and includes kinds (sticker, animation, poll) that never
/// produce a note.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntakeKind {
    Url,
    Text,
    Photo,
    Voice,
    Audio,
    Document,
    Sticker,
    Video,
    Animation,
    Poll,
    Location,
    Contact,
    Empty,
    Unknown,
}

impl IntakeKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Url => "url",
            Self::Text => "text",
            Self::Photo => "photo",
            Self::Voice => "voice",
            Self::Audio => "audio",
            Self::Document => "document",
            Self::Sticker => "sticker",
            Self::Video => "video",
            Self::Animation => "animation",
            Self::Poll => "poll",
            Self::Location => "location",
            Self::Contact => "contact",
            Self::Empty => "empty",
            Self::Unknown => "unknown",
        }
    }
}

impl std::fmt::Display for IntakeKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl FromStr for IntakeKind {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "url" => Ok(Self::Url),
            "text" => Ok(Self::Text),
            "photo" => Ok(Self::Photo),
            "voice" => Ok(Self::Voice),
            "audio" => Ok(Self::Audio),
            "document" => Ok(Self::Document),
            "sticker" => Ok(Self::Sticker),
            "video" => Ok(Self::Video),
            "animation" => Ok(Self::Animation),
            "poll" => Ok(Self::Poll),
            "location" => Ok(Self::Location),
            "contact" => Ok(Self::Contact),
            "empty" => Ok(Self::Empty),
            "unknown" => Ok(Self::Unknown),
            _ => Err(format!("unknown intake kind: {s}")),
        }
    }
}

pub struct IntakeEntry {
    pub date: String,
    pub time: String,
    pub method: Method,
    /// Free-form transport context: chat_id (telegram), remote_addr (http),
    /// topic (ntfy), etc. Recorded verbatim; renderer escapes pipes.
    pub origin_ctx: String,
    pub kind: IntakeKind,
    /// First 80 chars of text input, or a structured descriptor for binary
    /// inputs (`[image: filename.jpg, 12345 bytes, image/jpeg]`).
    pub preview: String,
    pub trace_id: String,
}

const INTAKE_FRONTMATTER: &str = r#"---
title: Borg Intake
date: {date}
type: system
domain: system
origin: authored
tags:
  - obsidian-borg
  - system
---

# Borg Intake

Every input received by obsidian-borg. This file is machine-maintained - do not edit the table manually. Each row is appended synchronously at the moment of receipt; if a row is missing here, borg never saw the input.

See also: [[borg-ledger]], [[borg-dlq]], [[borg-dashboard]]

| Date | Time | Method | Origin | Kind | Preview | Trace |
|------|------|--------|--------|------|---------|-------|
"#;

/// Canonical column names. The header parser maps these to row positions, so
/// reordering columns in the table layout never breaks parsing - only renames
/// or removals do (and those fail loudly with a missing-column error).
pub const COL_DATE: &str = "Date";
pub const COL_TIME: &str = "Time";
pub const COL_METHOD: &str = "Method";
pub const COL_ORIGIN: &str = "Origin";
pub const COL_KIND: &str = "Kind";
pub const COL_PREVIEW: &str = "Preview";
pub const COL_TRACE: &str = "Trace";

fn canonical_columns() -> &'static [&'static str] {
    &[
        COL_DATE,
        COL_TIME,
        COL_METHOD,
        COL_ORIGIN,
        COL_KIND,
        COL_PREVIEW,
        COL_TRACE,
    ]
}

const INTAKE_HEADER: &str = "| Date | Time | Method | Origin | Kind | Preview | Trace |";
const INTAKE_SEPARATOR: &str = "|------|------|--------|--------|------|---------|-------|";

/// Resolve the Borg Intake path from a vault root.
pub fn intake_path(vault_root: &Path) -> PathBuf {
    vault_root.join("system").join("views").join("borg-intake.md")
}

/// Resolve the directory where raw-input sidecar files live.
pub fn intake_raw_dir(vault_root: &Path) -> PathBuf {
    vault_root.join("system").join("intake")
}

/// Path for a single trace's raw-input sidecar file.
pub fn raw_input_path(vault_root: &Path, trace_id: &str) -> PathBuf {
    intake_raw_dir(vault_root).join(format!("{trace_id}.txt"))
}

/// Create the Borg Intake file with frontmatter and header if it doesn't exist.
pub fn ensure_intake_exists(intake_path: &Path) -> Result<()> {
    if intake_path.exists() {
        return Ok(());
    }
    if let Some(parent) = intake_path.parent() {
        fs::create_dir_all(parent).context("Failed to create Borg Intake directory")?;
    }
    let date = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let content = INTAKE_FRONTMATTER.replace("{date}", &date);
    fs::write(intake_path, content).context("Failed to create Borg Intake")?;
    log::info!("Created Borg Intake at {}", intake_path.display());
    Ok(())
}

/// Insert an intake row at the top of the table (newest first).
pub fn append_entry(intake_path: &Path, entry: &IntakeEntry) -> Result<()> {
    log::debug!(
        "intake::append_entry: trace={} method={} kind={} origin={}",
        entry.trace_id,
        entry.method,
        entry.kind,
        entry.origin_ctx
    );
    ensure_intake_exists(intake_path)?;
    table::ensure_header_matches(intake_path, INTAKE_HEADER, INTAKE_SEPARATOR, "Borg Intake")?;

    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(intake_path)
        .context("Failed to open Borg Intake for writing")?;
    file.lock_exclusive()
        .context("Failed to acquire exclusive lock on Borg Intake")?;

    let row = table::format_row(&[
        (COL_DATE, &entry.date),
        (COL_TIME, &entry.time),
        (COL_METHOD, entry.method.as_str()),
        (COL_ORIGIN, &entry.origin_ctx),
        (COL_KIND, entry.kind.as_str()),
        (COL_PREVIEW, &entry.preview),
        (COL_TRACE, &entry.trace_id),
    ]);

    let content = fs::read_to_string(intake_path).context("Failed to read Borg Intake")?;
    let new_content = table::insert_after_separator(&content, &row);
    fs::write(intake_path, new_content).context("Failed to write Borg Intake")?;
    file.unlock().ok();

    Ok(())
}

/// Write the raw-input sidecar for a trace. Bytes are written verbatim; for
/// large binary inputs the caller is expected to pass a short descriptor
/// (e.g. `[image: foo.jpg, 12345 bytes, image/jpeg]`) rather than the raw
/// payload, to keep `system/intake/` small.
pub fn write_raw_input(vault_root: &Path, trace_id: &str, bytes: &[u8]) -> Result<()> {
    log::debug!("intake::write_raw_input: trace={trace_id} bytes={}", bytes.len());
    let dir = intake_raw_dir(vault_root);
    fs::create_dir_all(&dir).context("Failed to create intake raw-input directory")?;
    let path = raw_input_path(vault_root, trace_id);
    fs::write(&path, bytes).with_context(|| format!("Failed to write raw-input sidecar {}", path.display()))?;
    Ok(())
}

/// Parsed row used by `borg audit` and DLQ tooling. Fields are looked up by
/// canonical column name, so the parser tolerates added or reordered columns.
#[derive(Debug, Clone)]
pub struct ParsedIntakeRow {
    pub date: String,
    pub time: String,
    pub method: String,
    pub origin_ctx: String,
    pub kind: String,
    pub preview: String,
    pub trace_id: String,
}

fn parse_row(row: &RowMap) -> Option<ParsedIntakeRow> {
    Some(ParsedIntakeRow {
        date: row.get(COL_DATE)?.to_string(),
        time: row.get(COL_TIME)?.to_string(),
        method: row.get(COL_METHOD)?.to_string(),
        origin_ctx: row.get(COL_ORIGIN)?.to_string(),
        kind: row.get(COL_KIND)?.to_string(),
        preview: row.get(COL_PREVIEW)?.to_string(),
        trace_id: row.get(COL_TRACE)?.to_string(),
    })
}

/// Parse every intake row by column name.
pub fn parse_entries(intake_path: &Path) -> Result<Vec<ParsedIntakeRow>> {
    if !intake_path.exists() {
        return Ok(Vec::new());
    }
    let content = fs::read_to_string(intake_path).context("Failed to read Borg Intake")?;
    let table = table::parse_table(&content, canonical_columns())?;
    Ok(table.rows.iter().filter_map(parse_row).collect())
}

/// Look up an intake row by trace_id.
pub fn find_by_trace(intake_path: &Path, trace_id: &str) -> Result<Option<ParsedIntakeRow>> {
    let rows = parse_entries(intake_path)?;
    Ok(rows.into_iter().find(|r| r.trace_id == trace_id))
}

#[cfg(test)]
mod tests;
