use crate::schema::Method;
use eyre::{Context, Result};
use fs2::FileExt;
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};

/// On-disk marker for a successful ledger row. The receipts log carries
/// every failure now; the ledger only records successes. Kept as a const
/// so the writer and any drift-repair logic agree on the literal.
const SUCCESS_GLYPH: &str = "\u{2705}";

pub struct LedgerEntry {
    pub date: String,
    pub time: String,
    pub method: Method,
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

See also: [[borg-ledger]]

| Date | Time | Method | Status | Note | Source | Domain | Trace |
|------|------|--------|--------|------|--------|--------|-------|
"#;

/// The canonical table header and separator - single source of truth for column
/// names and order. Any code that reads or writes ledger rows must match this.
/// `Note` holds a single `[[slug]]` wikilink whose target is the filename stem;
/// it replaces the older two-column Title + Filename layout.
const LEDGER_HEADER: &str = "| Date | Time | Method | Status | Note | Source | Domain | Trace |";
const LEDGER_SEPARATOR: &str = "|------|------|--------|--------|------|--------|--------|-------|";

/// Resolve the Borg Ledger path. The ledger is a machine-maintained dedup
/// datastore (not a human note), so it lives alongside `receipts.db` in the
/// borg data dir - NOT inside the vault, where Obsidian would try (and hang)
/// rendering its thousands of rows. The human-facing view is `borg-ledger.base`.
pub fn ledger_path() -> Result<PathBuf> {
    Ok(crate::receipts::receipts_dir()?.join("borg-ledger.md"))
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
        l.starts_with('|') && l.contains('-') && l.chars().all(|c| c == '|' || c == '-' || c == ' ' || c == ':')
    });

    if let (Some(hi), Some(si)) = (header_idx, sep_idx) {
        let current_header = lines[hi].trim();
        let canonical_header = LEDGER_HEADER.trim();
        // Normalize: collapse whitespace for comparison
        let norm = |s: &str| -> String { s.split('|').map(|c| c.trim()).collect::<Vec<_>>().join("|") };
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

/// Indices into a `split('|')` of one ledger row, for either layout.
///
/// Current layout (8 fields, `cols.len() == 10`):
///   `Date | Time | Method | Status | Note | Source | Domain | Trace`
/// Legacy layout (9 fields, `cols.len() == 11`):
///   `Date | Time | Method | Status | Title | Filename | Source | Domain | Trace`
struct ColIdx {
    note: usize,
    filename: Option<usize>,
    source: usize,
    domain: usize,
}

fn col_idx(col_count: usize) -> Option<ColIdx> {
    match col_count {
        // New 8-field format: collapsed Note column.
        10 => Some(ColIdx {
            note: 5,
            filename: None,
            source: 6,
            domain: 7,
        }),
        // Legacy 9-field format: separate Title + Filename columns.
        n if n >= 11 => Some(ColIdx {
            note: 5,
            filename: Some(6),
            source: 7,
            domain: 8,
        }),
        _ => None,
    }
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
        let Some(idx) = col_idx(cols.len()) else {
            continue;
        };
        let status = cols[4].trim();
        let source = cols[idx.source].trim();
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

/// Extract the wikilink target (the slug) from a `Note` cell. Accepts `[[stem]]`
/// or `[[stem|alias]]` and returns the stem; returns the raw cell otherwise so
/// pre-migration rows still surface something useful.
fn parse_note_slug(cell: &str) -> String {
    let inner = cell
        .strip_prefix("[[")
        .and_then(|s| s.strip_suffix("]]"))
        .unwrap_or(cell);
    inner
        .split_once('|')
        .map(|(stem, _)| stem)
        .unwrap_or(inner)
        .trim()
        .to_string()
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
        let Some(idx) = col_idx(cols.len()) else {
            continue;
        };
        let status = cols[4].trim();
        let source = cols[idx.source].trim();
        // Legacy rows have a dedicated filename column; new rows derive the
        // filename from the [[slug]] in the Note column.
        let filename = match idx.filename {
            Some(i) => cols[i].trim().to_string(),
            None => {
                let slug = parse_note_slug(cols[idx.note].trim());
                if slug.is_empty() || slug == "-" {
                    "-".to_string()
                } else {
                    format!("{slug}.md")
                }
            }
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
    pub slug: String,
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
        let Some(idx) = col_idx(cols.len()) else {
            continue;
        };
        let status = cols[4].trim();
        if status != "\u{2705}" {
            continue;
        }

        let date = cols[1].trim().to_string();
        let method = cols[3].trim().to_string();
        let slug = parse_note_slug(cols[idx.note].trim());
        // Filename: legacy rows store it as a separate column; new rows
        // derive it from the [[slug]] target.
        let filename = match idx.filename {
            Some(i) => cols[i].trim().to_string(),
            None if slug.is_empty() || slug == "-" => "-".to_string(),
            None => format!("{slug}.md"),
        };
        let source = cols[idx.source].trim().to_string();
        let domain = cols[idx.domain].trim().to_string();

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
            slug,
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
    /// Filename stem from the `[[slug]]` link in the Note column. Pre-migration
    /// rows surface their separate Filename cell here (with `.md` stripped) so
    /// audit code can use this as the unique key for the note.
    pub slug: String,
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
        let Some(idx) = col_idx(cols.len()) else {
            continue;
        };
        let status = cols[4].trim().to_string();
        if status != "\u{2705}" {
            continue;
        }
        // Prefer the explicit Filename column on legacy rows; fall back to the
        // [[slug]] target on migrated rows.
        let slug = match idx.filename {
            Some(i) => {
                let raw = cols[i].trim();
                let bare = raw.rsplit('/').next().unwrap_or(raw);
                bare.strip_suffix(".md").unwrap_or(bare).to_string()
            }
            None => parse_note_slug(cols[idx.note].trim()),
        };
        let source = cols[idx.source].trim();
        entries.push(ParsedLedgerRow {
            date: cols[1].trim().to_string(),
            status,
            slug,
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

    let note_display = entry
        .filename
        .as_deref()
        .map(|p| p.rsplit('/').next().unwrap_or(p))
        .map(|name| name.strip_suffix(".md").unwrap_or(name))
        .map(|stem| format!("[[{stem}]]"))
        .unwrap_or_else(|| "-".to_string());
    let domain_display = entry.domain.as_deref().unwrap_or("-");
    let trace_display = entry.trace_id.as_deref().unwrap_or("-");

    let row = format!(
        "| {} | {} | {} | {} | {} | {} | {} | {} |",
        entry.date, entry.time, entry.method, SUCCESS_GLYPH, note_display, entry.source, domain_display, trace_display,
    );

    let content = fs::read_to_string(ledger_path).context("Failed to read Borg Ledger")?;
    let mut lines: Vec<&str> = content.lines().collect();

    // Match the markdown table separator row. The separator contains only
    // pipes, dashes, spaces, and colons (for alignment). We check that the
    // line starts with '|' and every non-pipe character is one of [- :].
    let insert_pos = lines
        .iter()
        .position(|l| {
            l.starts_with('|') && l.contains('-') && l.chars().all(|c| c == '|' || c == '-' || c == ' ' || c == ':')
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
            source: "https://example.com/article".to_string(),
            domain: Some("ai".to_string()),
            filename: Some("test-article.md".to_string()),
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
    fn test_find_completed_returns_path() {
        let path = temp_ledger_path().with_file_name("test-vault-find-completed.md");
        cleanup(&path);

        let entry = LedgerEntry {
            date: "2026-03-18".to_string(),
            time: "10:00".to_string(),
            method: Method::Cli,
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
            data_lines[0].contains("[[new-note]]"),
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
            filename: Some("test.md".to_string()),
            source: "https://example.com".to_string(),
            domain: Some("ai".to_string()),
            trace_id: None,
        };
        append_entry(&path, &entry).expect("append");

        let result = fs::read_to_string(&path).expect("read");
        assert!(
            result.contains("| Note |"),
            "header should be repaired to canonical 8-column layout with Note column"
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
            filename: Some("inbox/should-strip-this.md".to_string()),
            source: "https://example.com/strip".to_string(),
            domain: Some("ai".to_string()),
            trace_id: None,
        };
        append_entry(&path, &entry).expect("append");

        let result = fs::read_to_string(&path).expect("read");
        let data_line = result
            .lines()
            .find(|l| l.contains("should-strip-this"))
            .expect("should find data row");

        assert!(
            !data_line.contains("inbox/"),
            "path prefix should be stripped, got: {data_line}"
        );
        assert!(
            data_line.contains("[[should-strip-this]]"),
            "Note cell should be the bare slug as a wikilink, got: {data_line}"
        );
        assert!(
            !data_line.contains("should-strip-this.md"),
            ".md extension should not appear in the Note cell, got: {data_line}"
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
                    filename: Some(format!("{}.md", title.to_lowercase())),
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
        assert!(
            data_lines[0].contains("[[third]]"),
            "newest should be first, got: {}",
            data_lines[0]
        );
        assert!(
            data_lines[1].contains("[[second]]"),
            "middle should be second, got: {}",
            data_lines[1]
        );
        assert!(
            data_lines[2].contains("[[first]]"),
            "oldest should be last, got: {}",
            data_lines[2]
        );

        cleanup(&path);
    }
}
