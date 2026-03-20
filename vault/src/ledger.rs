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
    pub path: Option<String>,
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

| Date | Time | Method | Status | Title | Path | Source | Domain | Trace |
|------|------|--------|--------|-------|------|--------|--------|-------|
"#;

/// Resolve the Borg Ledger path from a vault root.
pub fn ledger_path(vault_root: &Path) -> PathBuf {
    vault_root.join("system").join("borg-ledger.md")
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
    pub path: String,
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
        let (source, path) = if cols.len() >= 11 {
            (cols[7].trim(), cols[6].trim().to_string())
        } else {
            (cols[6].trim(), "-".to_string())
        };
        if status == "\u{2705}" && source == content_key {
            last_match = Some(CompletedEntry {
                date: cols[1].trim().to_string(),
                path,
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
    pub path: String,
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

        let (path, source, domain) = if cols.len() >= 11 {
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
            path,
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
    let path_display = entry.path.as_deref().unwrap_or("-");
    let domain_display = entry.domain.as_deref().unwrap_or("-");
    let trace_display = entry.trace_id.as_deref().unwrap_or("-");

    let row = format!(
        "| {} | {} | {} | {} | {} | {} | {} | {} | {} |",
        entry.date,
        entry.time,
        entry.method,
        entry.status,
        title_display,
        path_display,
        entry.source,
        domain_display,
        trace_display,
    );

    let content = fs::read_to_string(ledger_path).context("Failed to read Borg Ledger")?;
    let mut lines: Vec<&str> = content.lines().collect();

    let insert_pos = lines
        .iter()
        .position(|l| l.starts_with("|--"))
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
            path: None,
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
            path: None,
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
            path: Some("notes/test-note.md".to_string()),
            source: "https://example.com/article".to_string(),
            domain: Some("ai".to_string()),
            trace_id: None,
        };
        append_entry(&path, &entry).expect("append");

        let result = find_completed(&path, "https://example.com/article").expect("find");
        assert!(result.is_some());
        let found = result.expect("should have entry");
        assert_eq!(found.date, "2026-03-18");
        assert_eq!(found.path, "notes/test-note.md");

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
            path: Some("notes/test-note.md".to_string()),
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
}
