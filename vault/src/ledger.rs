use crate::schema::Method;
use eyre::{Context, Result};
use fs2::FileExt;
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LedgerStatus {
    Completed,
    Failed,
    Skipped,
    Replaced,
}

impl std::fmt::Display for LedgerStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Completed => write!(f, "\u{2705}"),
            Self::Failed => write!(f, "\u{274c}"),
            Self::Skipped => write!(f, "\u{23ed}\u{fe0f}"),
            Self::Replaced => write!(f, "\u{01f504}"),
        }
    }
}

pub struct LedgerEntry {
    pub date: String,
    pub time: String,
    pub method: Method,
    pub status: LedgerStatus,
    pub title: Option<String>,
    pub filename: Option<String>,
    pub source: String,
    pub domain: Option<String>,
    pub trace_id: Option<String>,
}

const LEDGER_FRONTMATTER: &str = r#"---
title: Borg Ledger
date: {date}
type: system
domain: system
origin: authored
tags:
  - obsidian-borg
  - system
---

# Borg Ledger

All URLs ingested by obsidian-borg. This file is machine-maintained - do not edit the table manually.

See also: [[borg-dashboard]]

| Date | Time | Method | Status | Title | Filename | Source | Domain | Trace |
|------|------|--------|--------|-------|----------|--------|--------|-------|
"#;

/// The canonical table header and separator - single source of truth for column
/// names and order. Any code that reads or writes ledger rows must match this.
const LEDGER_HEADER: &str = "| Date | Time | Method | Status | Title | Filename | Source | Domain | Trace |";
const LEDGER_SEPARATOR: &str = "|------|------|--------|--------|-------|----------|--------|--------|-------|";

/// Resolve the Borg Ledger path from a vault root.
pub fn ledger_path(vault_root: &Path) -> PathBuf {
    vault_root.join("system").join("views").join("borg-ledger.md")
}

/// Verify the ledger header matches the canonical column layout. If the header
/// has drifted (e.g. a column was removed or renamed), replace it in-place.
/// This prevents the off-by-one column bugs that occur when the header and
/// data rows disagree on column count or order.
fn ensure_header_matches(ledger_path: &Path) -> Result<()> {
    let content = fs::read_to_string(ledger_path).context("Failed to read Borg Ledger")?;
    let lines: Vec<&str> = content.lines().collect();

    // Find the header line (starts with "| Date")
    let header_idx = lines.iter().position(|l| {
        let trimmed = l.trim();
        trimmed.starts_with("| Date") || trimmed.starts_with("|Date")
    });
    let sep_idx = lines.iter().position(|l| {
        l.starts_with('|')
            && l.contains('-')
            && l.chars().all(|c| c == '|' || c == '-' || c == ' ' || c == ':')
    });

    if let (Some(hi), Some(si)) = (header_idx, sep_idx) {
        let current_header = lines[hi].trim();
        let canonical_header = LEDGER_HEADER.trim();
        // Normalize: collapse whitespace for comparison
        let norm = |s: &str| -> String {
            s.split('|').map(|c| c.trim()).collect::<Vec<_>>().join("|")
        };
        if norm(current_header) != norm(canonical_header) {
            log::warn!(
                "Borg Ledger header has drifted, repairing: {:?} -> {:?}",
                current_header,
                canonical_header
            );
            let mut new_lines: Vec<&str> = lines.clone();
            new_lines[hi] = LEDGER_HEADER;
            new_lines[si] = LEDGER_SEPARATOR;
            let new_content = format!("{}\n", new_lines.join("\n"));
            fs::write(ledger_path, new_content).context("Failed to repair Borg Ledger header")?;
        }
    }

    Ok(())
}

/// Create the Borg Ledger file with frontmatter and header if it doesn't exist.
pub fn ensure_ledger_exists(ledger_path: &Path) -> Result<()> {
    if ledger_path.exists() {
        return Ok(());
    }
    if let Some(parent) = ledger_path.parent() {
        fs::create_dir_all(parent).context("Failed to create Borg Ledger directory")?;
    }
    let date = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let content = LEDGER_FRONTMATTER.replace("{date}", &date);
    fs::write(ledger_path, content).context("Failed to create Borg Ledger")?;
    log::info!("Created Borg Ledger at {}", ledger_path.display());
    Ok(())
}

/// Check if canonical URL exists in log with a completed status. Returns the date if found.
pub fn check_duplicate(ledger_path: &Path, canonical_url: &str) -> Result<Option<String>> {
    if !ledger_path.exists() {
        return Ok(None);
    }

    let file = OpenOptions::new()
        .read(true)
        .open(ledger_path)
        .context("Failed to open Borg Ledger for reading")?;
    file.lock_shared()
        .context("Failed to acquire shared lock on Borg Ledger")?;

    let content = fs::read_to_string(ledger_path).context("Failed to read Borg Ledger")?;
    file.unlock().ok();

    for line in content.lines() {
        if !line.starts_with('|') || line.starts_with("| Date") || line.starts_with("|--") {
            continue;
        }
        let cols: Vec<&str> = line.split('|').collect();
        if cols.len() < 8 {
            continue;
        }
        let status = cols[4].trim();
        // Source is at index 7 (new format, 10+ cols) or index 6 (old format)
        let source = if cols.len() >= 11 { cols[7].trim() } else { cols[6].trim() };
        if status == "\u{2705}" && source == canonical_url {
            return Ok(Some(cols[1].trim().to_string()));
        }
    }

    Ok(None)
}

/// Result from finding a completed entry for a content key.
#[derive(Debug)]
pub struct CompletedEntry {
    pub date: String,
    pub filename: String,
    pub line_number: usize,
}

/// Find the most recent completed entry for a content key (canonical URL or normalized text).
pub fn find_completed(ledger_path: &Path, content_key: &str) -> Result<Option<CompletedEntry>> {
    if !ledger_path.exists() {
        return Ok(None);
    }

    let file = OpenOptions::new()
        .read(true)
        .open(ledger_path)
        .context("Failed to open Borg Ledger for reading")?;
    file.lock_shared()
        .context("Failed to acquire shared lock on Borg Ledger")?;

    let content = fs::read_to_string(ledger_path).context("Failed to read Borg Ledger")?;
    file.unlock().ok();

    let mut last_match: Option<CompletedEntry> = None;

    for (line_number, line) in content.lines().enumerate() {
        if !line.starts_with('|') || line.starts_with("| Date") || line.starts_with("|--") {
            continue;
        }
        let cols: Vec<&str> = line.split('|').collect();
        if cols.len() < 8 {
            continue;
        }
        let status = cols[4].trim();
        let (source, filename) = if cols.len() >= 11 {
            (cols[7].trim(), cols[6].trim().to_string())
        } else {
            (cols[6].trim(), "-".to_string())
        };
        if status == "\u{2705}" && source == content_key {
            last_match = Some(CompletedEntry {
                date: cols[1].trim().to_string(),
                filename,
                line_number,
            });
        }
    }

    Ok(last_match)
}

/// Mark an existing ledger row as replaced.
pub fn mark_replaced(ledger_path: &Path, line_number: usize) -> Result<()> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(ledger_path)
        .context("Failed to open Borg Ledger for update")?;
    file.lock_exclusive()
        .context("Failed to acquire exclusive lock on Borg Ledger")?;

    let content = fs::read_to_string(ledger_path).context("Failed to read Borg Ledger")?;
    let mut lines: Vec<String> = content.lines().map(|l| l.to_string()).collect();

    if line_number < lines.len() {
        lines[line_number] = lines[line_number].replacen("\u{2705}", "\u{01f504}", 1);
    }

    let new_content = lines.join("\n");
    let final_content = if content.ends_with('\n') { format!("{new_content}\n") } else { new_content };

    fs::write(ledger_path, final_content).context("Failed to write updated Borg Ledger")?;
    file.unlock().ok();

    Ok(())
}

/// Filter criteria for querying ledger entries.
#[derive(Debug, Default)]
pub struct EntryFilter {
    pub source: Option<String>,
    pub domain: Option<String>,
    pub before: Option<String>,
    pub after: Option<String>,
}

/// Extended completed entry with all fields for reingest.
#[derive(Debug)]
pub struct QueriedEntry {
    pub date: String,
    pub method: String,
    pub title: String,
    pub filename: String,
    pub source: String,
    pub domain: String,
    pub line_number: usize,
}

/// Query all completed entries from the ledger, applying optional filters.
pub fn query_entries(ledger_path: &Path, filter: &EntryFilter) -> Result<Vec<QueriedEntry>> {
    if !ledger_path.exists() {
        return Ok(Vec::new());
    }

    let file = OpenOptions::new()
        .read(true)
        .open(ledger_path)
        .context("Failed to open Borg Ledger for reading")?;
    file.lock_shared()
        .context("Failed to acquire shared lock on Borg Ledger")?;

    let content = fs::read_to_string(ledger_path).context("Failed to read Borg Ledger")?;
    file.unlock().ok();

    let mut entries = Vec::new();

    for (line_number, line) in content.lines().enumerate() {
        if !line.starts_with('|') || line.starts_with("| Date") || line.starts_with("|--") {
            continue;
        }
        let cols: Vec<&str> = line.split('|').collect();
        if cols.len() < 8 {
            continue;
        }
        let status = cols[4].trim();
        if status != "\u{2705}" {
            continue;
        }

        let date = cols[1].trim().to_string();
        let method = cols[3].trim().to_string();
        let title_raw = cols[5].trim();
        let title = title_raw
            .strip_prefix("[[")
            .and_then(|s| s.strip_suffix("]]"))
            .unwrap_or(title_raw)
            .to_string();

        let (filename, source, domain) = if cols.len() >= 11 {
            (
                cols[6].trim().to_string(),
                cols[7].trim().to_string(),
                cols[8].trim().to_string(),
            )
        } else {
            ("-".to_string(), cols[6].trim().to_string(), cols[7].trim().to_string())
        };

        if let Some(ref f_source) = filter.source
            && source != *f_source
        {
            continue;
        }
        if let Some(ref f_domain) = filter.domain
            && domain != *f_domain
        {
            continue;
        }
        if let Some(ref f_before) = filter.before
            && date.as_str() >= f_before.as_str()
        {
            continue;
        }
        if let Some(ref f_after) = filter.after
            && date.as_str() <= f_after.as_str()
        {
            continue;
        }

        entries.push(QueriedEntry {
            date,
            method,
            title,
            filename,
            source,
            domain,
            line_number,
        });
    }

    Ok(entries)
}

/// Parsed row from the ledger for audit purposes.
#[derive(Debug)]
pub struct ParsedLedgerRow {
    pub date: String,
    pub status: String,
    pub title: String,
    pub source: String,
}

/// Parse all completed entries from the ledger for auditing.
pub fn parse_completed_entries(ledger_path: &Path) -> Result<Vec<ParsedLedgerRow>> {
    if !ledger_path.exists() {
        return Ok(Vec::new());
    }
    let content = fs::read_to_string(ledger_path).context("Failed to read Borg Ledger")?;
    let mut entries = Vec::new();
    for line in content.lines() {
        if !line.starts_with('|') || line.starts_with("| Date") || line.starts_with("|--") {
            continue;
        }
        let cols: Vec<&str> = line.split('|').collect();
        if cols.len() < 8 {
            continue;
        }
        let status = cols[4].trim().to_string();
        if status != "\u{2705}" {
            continue;
        }
        let title_raw = cols[5].trim();
        let title = title_raw
            .strip_prefix("[[")
            .and_then(|s| s.strip_suffix("]]"))
            .unwrap_or(title_raw)
            .to_string();
        let source = if cols.len() >= 11 { cols[7].trim() } else { cols[6].trim() };
        entries.push(ParsedLedgerRow {
            date: cols[1].trim().to_string(),
            status,
            title,
            source: source.to_string(),
        });
    }
    Ok(entries)
}

/// Insert a row at the top of the Borg Ledger table (newest first).
pub fn append_entry(ledger_path: &Path, entry: &LedgerEntry) -> Result<()> {
    ensure_ledger_exists(ledger_path)?;
    ensure_header_matches(ledger_path)?;

    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(ledger_path)
        .context("Failed to open Borg Ledger for writing")?;
    file.lock_exclusive()
        .context("Failed to acquire exclusive lock on Borg Ledger")?;

    let title_display = entry
        .title
        .as_ref()
        .map(|t| format!("[[{}]]", t.replace('|', "-")))
        .unwrap_or_else(|| "-".to_string());
    let filename_display = entry
        .filename
        .as_deref()
        .map(|p| p.rsplit('/').next().unwrap_or(p))
        .unwrap_or("-");
    let domain_display = entry.domain.as_deref().unwrap_or("-");
    let trace_display = entry.trace_id.as_deref().unwrap_or("-");

    let row = format!(
        "| {} | {} | {} | {} | {} | {} | {} | {} | {} |",
        entry.date,
        entry.time,
        entry.method,
        entry.status,
        title_display,
        filename_display,
        entry.source,
        domain_display,
        trace_display,
    );

    let content = fs::read_to_string(ledger_path).context("Failed to read Borg Ledger")?;
    let mut lines: Vec<&str> = content.lines().collect();

    // Match the markdown table separator row. The separator contains only
    // pipes, dashes, spaces, and colons (for alignment). We check that the
    // line starts with '|' and every non-pipe character is one of [- :].
    let insert_pos = lines
        .iter()
        .position(|l| {
            l.starts_with('|')
                && l.contains('-')
                && l.chars()
                    .all(|c| c == '|' || c == '-' || c == ' ' || c == ':')
        })
        .map(|i| i + 1)
        .unwrap_or(lines.len());

    lines.insert(insert_pos, &row);

    let new_content = format!("{}\n", lines.join("\n"));
    fs::write(ledger_path, new_content).context("Failed to write Borg Ledger")?;
    file.unlock().ok();

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_ledger_path() -> PathBuf {
        let dir = std::env::temp_dir().join("vault-test-ledger");
        fs::create_dir_all(&dir).ok();
        dir.join("borg-ledger.md")
    }

    fn cleanup(path: &Path) {
        let _ = fs::remove_file(path);
    }

    #[test]
    fn test_ensure_ledger_exists_creates_file() {
        let path = temp_ledger_path().with_file_name("test-vault-create.md");
        cleanup(&path);
        ensure_ledger_exists(&path).expect("should create");
        assert!(path.exists());
        let content = fs::read_to_string(&path).expect("read");
        assert!(content.contains("# Borg Ledger"));
        assert!(content.contains("| Date |"));
        cleanup(&path);
    }

    #[test]
    fn test_ensure_ledger_exists_idempotent() {
        let path = temp_ledger_path().with_file_name("test-vault-idempotent.md");
        cleanup(&path);
        ensure_ledger_exists(&path).expect("first");
        let content1 = fs::read_to_string(&path).expect("read");
        ensure_ledger_exists(&path).expect("second");
        let content2 = fs::read_to_string(&path).expect("read");
        assert_eq!(content1, content2);
        cleanup(&path);
    }

    #[test]
    fn test_check_duplicate_empty_log() {
        let path = temp_ledger_path().with_file_name("test-vault-dedup-empty.md");
        cleanup(&path);
        ensure_ledger_exists(&path).expect("create");
        let result = check_duplicate(&path, "https://example.com").expect("check");
        assert!(result.is_none());
        cleanup(&path);
    }

    #[test]
    fn test_append_and_check_duplicate() {
        let path = temp_ledger_path().with_file_name("test-vault-append-dedup.md");
        cleanup(&path);

        let entry = LedgerEntry {
            date: "2026-03-07".to_string(),
            time: "14:30".to_string(),
            method: Method::Cli,
            status: LedgerStatus::Completed,
            title: Some("Test Article".to_string()),
            source: "https://example.com/article".to_string(),
            domain: Some("ai".to_string()),
            filename: None,
            trace_id: None,
        };
        append_entry(&path, &entry).expect("append");

        let result = check_duplicate(&path, "https://example.com/article").expect("check");
        assert_eq!(result, Some("2026-03-07".to_string()));

        let result = check_duplicate(&path, "https://example.com/other").expect("check");
        assert!(result.is_none());

        cleanup(&path);
    }

    #[test]
    fn test_failed_entry_not_duplicate() {
        let path = temp_ledger_path().with_file_name("test-vault-failed-not-dup.md");
        cleanup(&path);

        let entry = LedgerEntry {
            date: "2026-03-07".to_string(),
            time: "14:30".to_string(),
            method: Method::Telegram,
            status: LedgerStatus::Failed,
            title: None,
            filename: None,
            source: "https://example.com/broken".to_string(),
            domain: None,
            trace_id: None,
        };
        append_entry(&path, &entry).expect("append");

        let result = check_duplicate(&path, "https://example.com/broken").expect("check");
        assert!(result.is_none());

        cleanup(&path);
    }

    #[test]
    fn test_ledger_status_display() {
        assert_eq!(format!("{}", LedgerStatus::Completed), "\u{2705}");
        assert_eq!(format!("{}", LedgerStatus::Failed), "\u{274c}");
        assert_eq!(format!("{}", LedgerStatus::Replaced), "\u{01f504}");
    }

    #[test]
    fn test_find_completed_returns_path() {
        let path = temp_ledger_path().with_file_name("test-vault-find-completed.md");
        cleanup(&path);

        let entry = LedgerEntry {
            date: "2026-03-18".to_string(),
            time: "10:00".to_string(),
            method: Method::Cli,
            status: LedgerStatus::Completed,
            title: Some("Test Note".to_string()),
            filename: Some("test-note.md".to_string()),
            source: "https://example.com/article".to_string(),
            domain: Some("ai".to_string()),
            trace_id: None,
        };
        append_entry(&path, &entry).expect("append");

        let result = find_completed(&path, "https://example.com/article").expect("find");
        assert!(result.is_some());
        let found = result.expect("should have entry");
        assert_eq!(found.date, "2026-03-18");
        assert_eq!(found.filename, "test-note.md");

        cleanup(&path);
    }

    #[test]
    fn test_mark_replaced_changes_status() {
        let path = temp_ledger_path().with_file_name("test-vault-mark-replaced.md");
        cleanup(&path);

        let entry = LedgerEntry {
            date: "2026-03-18".to_string(),
            time: "10:00".to_string(),
            method: Method::Cli,
            status: LedgerStatus::Completed,
            title: Some("Test Note".to_string()),
            filename: Some("test-note.md".to_string()),
            source: "https://example.com/article".to_string(),
            domain: Some("ai".to_string()),
            trace_id: None,
        };
        append_entry(&path, &entry).expect("append");

        let existing = find_completed(&path, "https://example.com/article")
            .expect("find")
            .expect("should exist");
        mark_replaced(&path, existing.line_number).expect("mark");

        let result = check_duplicate(&path, "https://example.com/article").expect("check");
        assert!(result.is_none(), "replaced entry should not count as duplicate");

        cleanup(&path);
    }

    #[test]
    fn test_separator_detection_with_spaces() {
        // The bug: separator "| --- | --- |" (with spaces) wasn't matched by
        // starts_with("|--"), causing new rows to append at the bottom.
        let path = temp_ledger_path().with_file_name("test-vault-sep-spaces.md");
        cleanup(&path);

        // Write a ledger with Obsidian-style spaced separators
        let content = format!(
            "---\ntitle: Borg Ledger\ndate: 2026-03-23\ntype: system\ndomain: system\norigin: authored\ntags: []\n---\n\n\
             # Borg Ledger\n\n\
             | Date | Time | Method | Status | Title | Filename | Source | Domain | Trace |\n\
             | ---------- | ----- | --------- | ------ | ----- | -------- | ------ | ------ | ----- |\n\
             | 2026-03-20 | 10:00 | http | {} | [[Old Note]] | old.md | https://example.com/old | ai | tr-000001 |\n",
            "\u{2705}"
        );
        fs::write(&path, content).expect("write");

        let entry = LedgerEntry {
            date: "2026-03-23".to_string(),
            time: "14:00".to_string(),
            method: Method::Http,
            status: LedgerStatus::Completed,
            title: Some("New Note".to_string()),
            filename: Some("new-note.md".to_string()),
            source: "https://example.com/new".to_string(),
            domain: Some("ai".to_string()),
            trace_id: Some("tr-000002".to_string()),
        };
        append_entry(&path, &entry).expect("append");

        let result = fs::read_to_string(&path).expect("read");
        let data_lines: Vec<&str> = result
            .lines()
            .filter(|l| l.starts_with('|') && l.contains("example.com"))
            .collect();

        // New entry should be FIRST (top of table), not last
        assert!(
            data_lines[0].contains("New Note"),
            "newest entry should be at top, got: {}",
            data_lines[0]
        );
        assert!(
            data_lines[1].contains("Old Note"),
            "older entry should be below, got: {}",
            data_lines[1]
        );

        cleanup(&path);
    }

    #[test]
    fn test_header_drift_repair() {
        // The bug: a previous agent removed the "Filename" column from the header,
        // causing an off-by-one where Path data showed under Source.
        let path = temp_ledger_path().with_file_name("test-vault-header-drift.md");
        cleanup(&path);

        // Write a ledger with a broken header (missing Filename column)
        let content = "\
            ---\ntitle: Borg Ledger\ndate: 2026-03-23\ntype: system\ndomain: system\norigin: authored\ntags: []\n---\n\n\
            # Borg Ledger\n\n\
            | Date | Time | Method | Status | Title | Source | Domain |   |   |\n\
            |------|------|--------|--------|-------|--------|--------|---|---|\n";
        fs::write(&path, content).expect("write");

        // Appending should trigger header repair
        let entry = LedgerEntry {
            date: "2026-03-23".to_string(),
            time: "14:00".to_string(),
            method: Method::Http,
            status: LedgerStatus::Completed,
            title: Some("Test".to_string()),
            filename: Some("test.md".to_string()),
            source: "https://example.com".to_string(),
            domain: Some("ai".to_string()),
            trace_id: None,
        };
        append_entry(&path, &entry).expect("append");

        let result = fs::read_to_string(&path).expect("read");
        assert!(
            result.contains("| Filename |"),
            "header should be repaired to include Filename column"
        );
        assert!(
            !result.contains("| Source | Domain |   |"),
            "broken header should be gone"
        );

        cleanup(&path);
    }

    #[test]
    fn test_filename_stripping_in_append() {
        // Verify that even if LedgerEntry.filename contains a path prefix,
        // the written row only has the bare filename.
        let path = temp_ledger_path().with_file_name("test-vault-filename-strip.md");
        cleanup(&path);
        ensure_ledger_exists(&path).expect("create");

        let entry = LedgerEntry {
            date: "2026-03-23".to_string(),
            time: "14:00".to_string(),
            method: Method::Http,
            status: LedgerStatus::Completed,
            title: Some("Strip Test".to_string()),
            filename: Some("inbox/should-strip-this.md".to_string()),
            source: "https://example.com/strip".to_string(),
            domain: Some("ai".to_string()),
            trace_id: None,
        };
        append_entry(&path, &entry).expect("append");

        let result = fs::read_to_string(&path).expect("read");
        let data_line = result
            .lines()
            .find(|l| l.contains("Strip Test"))
            .expect("should find data row");

        assert!(
            !data_line.contains("inbox/"),
            "path prefix should be stripped, got: {data_line}"
        );
        assert!(
            data_line.contains("should-strip-this.md"),
            "bare filename should be present, got: {data_line}"
        );

        cleanup(&path);
    }

    #[test]
    fn test_append_newest_first_multiple_entries() {
        // End-to-end: append 3 entries in chronological order,
        // verify they appear newest-first in the file.
        let path = temp_ledger_path().with_file_name("test-vault-ordering.md");
        cleanup(&path);
        ensure_ledger_exists(&path).expect("create");

        let dates = [
            ("2026-03-20", "08:00", "First"),
            ("2026-03-21", "09:00", "Second"),
            ("2026-03-22", "10:00", "Third"),
        ];

        for (date, time, title) in &dates {
            append_entry(
                &path,
                &LedgerEntry {
                    date: date.to_string(),
                    time: time.to_string(),
                    method: Method::Http,
                    status: LedgerStatus::Completed,
                    title: Some(title.to_string()),
                    filename: None,
                    source: format!("https://example.com/{}", title.to_lowercase()),
                    domain: Some("ai".to_string()),
                    trace_id: None,
                },
            )
            .expect("append");
        }

        let result = fs::read_to_string(&path).expect("read");
        let data_lines: Vec<&str> = result
            .lines()
            .filter(|l| l.starts_with('|') && l.contains("example.com"))
            .collect();

        assert_eq!(data_lines.len(), 3);
        assert!(data_lines[0].contains("Third"), "newest should be first");
        assert!(data_lines[1].contains("Second"), "middle should be second");
        assert!(data_lines[2].contains("First"), "oldest should be last");

        cleanup(&path);
    }
}
