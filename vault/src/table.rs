//! Shared helpers for markdown-table-backed stores (intake, dlq, future).
//!
//! Every store in this crate that persists rows as a Markdown table uses the
//! same access pattern: parse the header line ONCE to build a name->index
//! map, then look up each cell by canonical column name. Hardcoded numeric
//! indices are deliberately avoided - the existing `vault::ledger` shows the
//! failure mode (an off-by-one bug appeared when a column was removed
//! upstream of position-based parsers).

use eyre::{Context, Result, bail};
use std::collections::HashMap;
use std::fs;
use std::path::Path;

/// A single parsed row: column name -> cell value. Cells are trimmed.
#[derive(Debug, Clone)]
pub struct RowMap {
    cells: HashMap<String, String>,
}

impl RowMap {
    pub fn get(&self, column: &str) -> Option<&str> {
        self.cells.get(column).map(|s| s.as_str())
    }
}

/// Result of parsing a markdown table: ordered rows and the header line.
#[derive(Debug)]
pub struct ParsedTable {
    pub rows: Vec<RowMap>,
}

/// Split a markdown table row into trimmed cells, honouring `\|` as an
/// escaped pipe inside cell content. Drops the empty leading + trailing
/// cells that the outer `|...|` pipes produce.
fn split_cells(line: &str) -> Vec<String> {
    let mut cells: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(&next) = chars.peek()
                && next == '|'
            {
                current.push('|');
                chars.next();
                continue;
            }
            current.push(c);
        } else if c == '|' {
            cells.push(current.trim().to_string());
            current = String::new();
        } else {
            current.push(c);
        }
    }
    cells.push(current.trim().to_string());

    if cells.len() < 2 {
        return Vec::new();
    }
    let start = if cells.first().is_some_and(|c| c.is_empty()) { 1 } else { 0 };
    let end = if cells.last().is_some_and(|c| c.is_empty()) {
        cells.len() - 1
    } else {
        cells.len()
    };
    cells[start..end].to_vec()
}

pub fn is_separator(line: &str) -> bool {
    line.starts_with('|') && line.contains('-') && line.chars().all(|c| c == '|' || c == '-' || c == ' ' || c == ':')
}

/// Returns true if the line looks like a markdown table row (starts with `|`
/// and is not a separator). Note that a "header row" and a "data row" share
/// the same shape; this function is true for both. Use this together with
/// position (the FIRST such row in a file is the header).
pub fn is_table_row(line: &str) -> bool {
    line.starts_with('|') && !is_separator(line)
}

/// Parse a markdown table out of the given content. `required_columns` lists
/// the canonical column names the caller expects; the parser uses the
/// header's actual column order to map names to positions. Returns a
/// row-iterator each of which maps column-name -> cell value.
///
/// Errors when:
///   - no header row is found
///   - a required column is missing from the header
pub fn parse_table(content: &str, required_columns: &[&str]) -> Result<ParsedTable> {
    let lines: Vec<&str> = content.lines().collect();
    let header_idx = lines
        .iter()
        .position(|l| is_table_row(l))
        .ok_or_else(|| eyre::eyre!("no markdown table header found"))?;

    let header_cells: Vec<String> = split_cells(lines[header_idx]);

    for required in required_columns {
        if !header_cells.iter().any(|c| c == required) {
            bail!("required column `{required}` missing from table header: {header_cells:?}");
        }
    }

    let mut rows = Vec::new();
    for (idx, line) in lines.iter().enumerate() {
        if idx <= header_idx {
            continue;
        }
        if !is_table_row(line) {
            continue;
        }
        let cells = split_cells(line);
        if cells.len() < header_cells.len() {
            log::trace!(
                "parse_table: skipping row {idx} (cells={} headers={}): {line}",
                cells.len(),
                header_cells.len()
            );
            continue;
        }
        let mut map = HashMap::new();
        for (col_idx, col_name) in header_cells.iter().enumerate() {
            if let Some(cell) = cells.get(col_idx) {
                map.insert(col_name.clone(), cell.clone());
            }
        }
        rows.push(RowMap { cells: map });
    }

    Ok(ParsedTable { rows })
}

fn escape_cell(s: &str) -> String {
    // Escape pipe characters (they would break the table layout) and
    // collapse newlines into spaces so a multi-line preview doesn't shatter
    // the row.
    s.chars()
        .map(|c| match c {
            '|' => "\\|".to_string(),
            '\n' | '\r' => " ".to_string(),
            _ => c.to_string(),
        })
        .collect()
}

/// Format a single row given a slice of (column_name, value) pairs. The order
/// of the slice IS the column order written to disk - callers control layout
/// here. Names are not written into the row; they exist solely to let the
/// caller code reference columns symbolically at the call site.
pub fn format_row(cells: &[(&str, &str)]) -> String {
    let mut row = String::from("|");
    for (_name, value) in cells {
        row.push(' ');
        row.push_str(&escape_cell(value));
        row.push_str(" |");
    }
    row
}

/// Insert `row` immediately after the table's separator line (newest first),
/// returning the new file content. Appends to the end if no separator is
/// present (which only happens on a malformed file).
pub fn insert_after_separator(content: &str, row: &str) -> String {
    let mut lines: Vec<String> = content.lines().map(String::from).collect();
    let insert_pos = lines
        .iter()
        .position(|l| is_separator(l))
        .map(|i| i + 1)
        .unwrap_or(lines.len());
    lines.insert(insert_pos, row.to_string());
    format!("{}\n", lines.join("\n"))
}

/// Verify the table header matches the canonical layout. If the header has
/// drifted (e.g. a column was removed or renamed), replace it in-place. This
/// is a forward-compatible safety net: new code that writes rows in the
/// canonical column order will continue to work even if a previous version
/// of the file used a different layout.
pub fn ensure_header_matches(
    path: &Path,
    canonical_header: &str,
    canonical_separator: &str,
    label: &str,
) -> Result<()> {
    let content = fs::read_to_string(path).with_context(|| format!("Failed to read {label}"))?;
    let lines: Vec<&str> = content.lines().collect();

    let header_idx = lines.iter().position(|l| is_table_row(l));
    let sep_idx = lines.iter().position(|l| is_separator(l));

    if let (Some(hi), Some(si)) = (header_idx, sep_idx) {
        let current_header = lines[hi].trim();
        let canonical_trim = canonical_header.trim();
        let norm = |s: &str| -> String { s.split('|').map(|c| c.trim()).collect::<Vec<_>>().join("|") };
        if norm(current_header) != norm(canonical_trim) {
            log::warn!(
                "{label} header has drifted, repairing: {:?} -> {:?}",
                current_header,
                canonical_trim
            );
            let mut new_lines: Vec<&str> = lines.clone();
            new_lines[hi] = canonical_header;
            new_lines[si] = canonical_separator;
            let new_content = format!("{}\n", new_lines.join("\n"));
            fs::write(path, new_content).with_context(|| format!("Failed to repair {label} header"))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
