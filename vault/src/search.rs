//! SQLite-backed full-text search index for vault notes
//!
//! Provides FTS5-powered search, incremental indexing by mtime, and
//! domain/tag analytics. Shared by oracle (MCP server) and cortex (daemon).

use crate::config::ScanConfig;
use crate::detail;
use crate::note::scan_vault;
use crate::schema::{Domain, NoteType, Origin, Status};
use eyre::{Result, WrapErr};
use rusqlite::{Connection, params};
use serde::Serialize;
use std::collections::HashMap;
use std::path::Path;

/// Manages the SQLite FTS5 index of vault notes
pub struct SearchIndex {
    conn: Connection,
}

/// Normalize an optional frontmatter string through a vault enum's FromStr.
/// Returns the canonical as_str() value on success, empty string on failure.
fn normalize_enum<T: std::str::FromStr + std::fmt::Display>(
    raw: Option<&str>,
    field_name: &str,
    note_path: &str,
) -> String {
    match raw {
        Some(s) if !s.is_empty() => match s.parse::<T>() {
            Ok(val) => val.to_string(),
            Err(_) => {
                log::warn!("Invalid {field_name} value '{s}' in note {note_path}, indexing as empty");
                String::new()
            }
        },
        _ => String::new(),
    }
}

impl SearchIndex {
    /// Open (or create) the search index at the given path
    pub fn open(db_path: &Path) -> Result<Self> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)
                .wrap_err_with(|| format!("Failed to create db directory: {}", parent.display()))?;
        }

        let conn = Connection::open(db_path)
            .wrap_err_with(|| format!("Failed to open search index: {}", db_path.display()))?;

        conn.execute_batch("PRAGMA journal_mode=WAL;")?;
        conn.execute_batch("PRAGMA foreign_keys=ON;")?;

        let index = Self { conn };
        index.ensure_schema()?;
        Ok(index)
    }

    /// Open an in-memory search index (for testing)
    #[cfg(test)]
    pub fn open_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        let index = Self { conn };
        index.ensure_schema()?;
        Ok(index)
    }

    fn ensure_schema(&self) -> Result<()> {
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS notes (
                path TEXT PRIMARY KEY,
                title TEXT,
                domain TEXT,
                note_type TEXT,
                origin TEXT,
                status TEXT,
                date TEXT,
                tags TEXT,
                source TEXT,
                creator TEXT,
                body TEXT,
                summary TEXT,
                modified_at INTEGER
            );

            CREATE INDEX IF NOT EXISTS idx_notes_domain ON notes(domain);
            CREATE INDEX IF NOT EXISTS idx_notes_note_type ON notes(note_type);
            CREATE INDEX IF NOT EXISTS idx_notes_status ON notes(status);
            CREATE INDEX IF NOT EXISTS idx_notes_date ON notes(date);

            CREATE VIRTUAL TABLE IF NOT EXISTS notes_fts USING fts5(
                title, body, tags, summary,
                content=notes, content_rowid=rowid
            );

            CREATE TRIGGER IF NOT EXISTS notes_ai AFTER INSERT ON notes BEGIN
                INSERT INTO notes_fts(rowid, title, body, tags, summary)
                VALUES (new.rowid, new.title, new.body, new.tags, new.summary);
            END;

            CREATE TRIGGER IF NOT EXISTS notes_ad AFTER DELETE ON notes BEGIN
                INSERT INTO notes_fts(notes_fts, rowid, title, body, tags, summary)
                VALUES ('delete', old.rowid, old.title, old.body, old.tags, old.summary);
            END;

            CREATE TRIGGER IF NOT EXISTS notes_au AFTER UPDATE ON notes BEGIN
                INSERT INTO notes_fts(notes_fts, rowid, title, body, tags, summary)
                VALUES ('delete', old.rowid, old.title, old.body, old.tags, old.summary);
                INSERT INTO notes_fts(rowid, title, body, tags, summary)
                VALUES (new.rowid, new.title, new.body, new.tags, new.summary);
            END;",
        )?;
        Ok(())
    }

    /// Index the vault, only updating notes whose mtime has changed.
    /// Parses frontmatter fields through vault enums for normalization.
    pub fn index_vault(&self, vault_root: &Path) -> Result<IndexStats> {
        let scan_config = ScanConfig::default();
        let notes = scan_vault(vault_root, &scan_config)?;

        let mut inserted = 0u64;
        let mut updated = 0u64;
        let mut unchanged = 0u64;

        for note in &notes {
            let abs_path = vault_root.join(&note.path);
            let mtime = std::fs::metadata(&abs_path)
                .and_then(|m| m.modified())
                .map(|t| t.duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs())
                .unwrap_or(0) as i64;

            let path_str = note.path.to_string_lossy();

            let existing_mtime: Option<i64> = self
                .conn
                .query_row(
                    "SELECT modified_at FROM notes WHERE path = ?1",
                    params![path_str.as_ref()],
                    |row| row.get(0),
                )
                .ok();

            if existing_mtime == Some(mtime) {
                unchanged += 1;
                continue;
            }

            let fm = &note.frontmatter;
            let summary = detail::extract_summary(&note.body);
            let tags_json = fm
                .tags
                .as_ref()
                .map(|t| serde_json::to_string(t).unwrap_or_default())
                .unwrap_or_default();

            // Normalize through vault enums
            let domain = normalize_enum::<Domain>(fm.domain.as_deref(), "domain", &path_str);
            let note_type = normalize_enum::<NoteType>(fm.note_type.as_deref(), "note_type", &path_str);
            let origin = normalize_enum::<Origin>(fm.origin.as_deref(), "origin", &path_str);
            let status = normalize_enum::<Status>(fm.status.as_deref(), "status", &path_str);

            self.conn.execute(
                "INSERT OR REPLACE INTO notes (path, title, domain, note_type, origin, status, date, tags, source, creator, body, summary, modified_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                params![
                    path_str.as_ref(),
                    fm.title.as_deref().unwrap_or(""),
                    domain,
                    note_type,
                    origin,
                    status,
                    fm.date.as_deref().unwrap_or(""),
                    tags_json,
                    fm.source.as_deref().unwrap_or(""),
                    fm.creator.as_deref().unwrap_or(""),
                    &note.body,
                    summary,
                    mtime,
                ],
            )?;

            if existing_mtime.is_some() {
                updated += 1;
            } else {
                inserted += 1;
            }
        }

        let all_paths: Vec<String> = notes.iter().map(|n| n.path.to_string_lossy().to_string()).collect();
        let removed = self.remove_stale_notes(&all_paths)?;

        Ok(IndexStats {
            total_scanned: notes.len() as u64,
            inserted,
            updated,
            unchanged,
            removed,
        })
    }

    fn remove_stale_notes(&self, current_paths: &[String]) -> Result<u64> {
        let mut stmt = self.conn.prepare("SELECT path FROM notes")?;
        let db_paths: Vec<String> = stmt.query_map([], |row| row.get(0))?.filter_map(|r| r.ok()).collect();

        let mut removed = 0u64;
        for path in &db_paths {
            if !current_paths.contains(path) {
                self.conn.execute("DELETE FROM notes WHERE path = ?1", params![path])?;
                removed += 1;
            }
        }
        Ok(removed)
    }

    /// Full-text search across notes
    pub fn search(
        &self,
        query: &str,
        domain: Option<&str>,
        note_type: Option<&str>,
        status: Option<&str>,
        limit: Option<u32>,
    ) -> Result<Vec<NoteRow>> {
        let limit = limit.unwrap_or(20);

        let mut sql = String::from(
            "SELECT n.path, n.title, n.domain, n.note_type, n.origin, n.status, n.date, n.tags, n.source, n.creator, n.body, n.summary
             FROM notes n
             JOIN notes_fts f ON n.rowid = f.rowid
             WHERE notes_fts MATCH ?1",
        );
        let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = vec![Box::new(query.to_string())];
        let mut param_idx = 2;

        if let Some(d) = domain {
            sql.push_str(&format!(" AND n.domain = ?{param_idx}"));
            param_values.push(Box::new(d.to_string()));
            param_idx += 1;
        }
        if let Some(t) = note_type {
            sql.push_str(&format!(" AND n.note_type = ?{param_idx}"));
            param_values.push(Box::new(t.to_string()));
            param_idx += 1;
        }
        if let Some(s) = status {
            sql.push_str(&format!(" AND n.status = ?{param_idx}"));
            param_values.push(Box::new(s.to_string()));
            param_idx += 1;
        }
        let _ = param_idx;

        sql.push_str(&format!(" ORDER BY rank LIMIT {limit}"));

        let params_refs: Vec<&dyn rusqlite::types::ToSql> = param_values.iter().map(|p| p.as_ref()).collect();
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map(params_refs.as_slice(), NoteRow::from_row)?
            .filter_map(|r| r.ok())
            .collect();

        Ok(rows)
    }

    /// Find notes most similar to the given content using FTS5 term matching
    pub fn find_similar(&self, content: &str, limit: usize) -> Result<Vec<NoteRow>> {
        // Extract significant words from content for FTS5 query
        let terms = extract_search_terms(content, 20);
        if terms.is_empty() {
            return Ok(vec![]);
        }

        // Build OR query from extracted terms
        let fts_query = terms.join(" OR ");

        self.search(&fts_query, None, None, None, Some(limit as u32))
    }

    /// List notes with optional filters (no full-text search)
    pub fn list_notes(
        &self,
        domain: Option<&str>,
        note_type: Option<&str>,
        status: Option<&str>,
        after: Option<&str>,
        before: Option<&str>,
        limit: Option<u32>,
    ) -> Result<Vec<NoteRow>> {
        let limit = limit.unwrap_or(50);
        let mut sql = String::from(
            "SELECT path, title, domain, note_type, origin, status, date, tags, source, creator, body, summary
             FROM notes WHERE 1=1",
        );
        let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = vec![];
        let mut param_idx = 1;

        if let Some(d) = domain {
            sql.push_str(&format!(" AND domain = ?{param_idx}"));
            param_values.push(Box::new(d.to_string()));
            param_idx += 1;
        }
        if let Some(t) = note_type {
            sql.push_str(&format!(" AND note_type = ?{param_idx}"));
            param_values.push(Box::new(t.to_string()));
            param_idx += 1;
        }
        if let Some(s) = status {
            sql.push_str(&format!(" AND status = ?{param_idx}"));
            param_values.push(Box::new(s.to_string()));
            param_idx += 1;
        }
        if let Some(a) = after {
            sql.push_str(&format!(" AND date >= ?{param_idx}"));
            param_values.push(Box::new(a.to_string()));
            param_idx += 1;
        }
        if let Some(b) = before {
            sql.push_str(&format!(" AND date <= ?{param_idx}"));
            param_values.push(Box::new(b.to_string()));
            param_idx += 1;
        }
        let _ = param_idx;

        sql.push_str(&format!(" ORDER BY date DESC LIMIT {limit}"));

        let params_refs: Vec<&dyn rusqlite::types::ToSql> = param_values.iter().map(|p| p.as_ref()).collect();
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map(params_refs.as_slice(), NoteRow::from_row)?
            .filter_map(|r| r.ok())
            .collect();

        Ok(rows)
    }

    /// Get a single note by path
    pub fn get_note(&self, path: &str) -> Result<Option<NoteRow>> {
        let row = self
            .conn
            .query_row(
                "SELECT path, title, domain, note_type, origin, status, date, tags, source, creator, body, summary
                 FROM notes WHERE path = ?1",
                params![path],
                NoteRow::from_row,
            )
            .ok();
        Ok(row)
    }

    /// Get vault statistics including schema gaps
    pub fn stats(&self) -> Result<VaultStats> {
        let total: u64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM notes", [], |row| row.get(0))?;

        let domain_counts = self.count_by_column("domain")?;
        let type_counts = self.count_by_column("note_type")?;
        let status_counts = self.count_by_column("status")?;

        let schema_gaps = self.compute_schema_gaps()?;

        Ok(VaultStats {
            total_notes: total,
            by_domain: domain_counts,
            by_type: type_counts,
            by_status: status_counts,
            schema_gaps,
        })
    }

    fn count_by_column(&self, column: &str) -> Result<Vec<(String, u64)>> {
        let sql = format!(
            "SELECT {column}, COUNT(*) as cnt FROM notes WHERE {column} != '' GROUP BY {column} ORDER BY cnt DESC"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?)))?
            .filter_map(|r| r.ok())
            .collect();
        Ok(rows)
    }

    fn compute_schema_gaps(&self) -> Result<Vec<(String, u64)>> {
        let fields = ["domain", "note_type", "origin", "status"];
        let mut gaps = Vec::new();
        for field in fields {
            let count: u64 =
                self.conn
                    .query_row(&format!("SELECT COUNT(*) FROM notes WHERE {field} = ''"), [], |row| {
                        row.get(0)
                    })?;
            if count > 0 {
                gaps.push((field.to_string(), count));
            }
        }
        Ok(gaps)
    }

    /// Get notes for a specific domain with stats
    pub fn domain_brief(&self, domain: &str, limit: Option<u32>) -> Result<DomainBrief> {
        let limit = limit.unwrap_or(10);

        let total: u64 =
            self.conn
                .query_row("SELECT COUNT(*) FROM notes WHERE domain = ?1", params![domain], |row| {
                    row.get(0)
                })?;

        let unread: u64 = self.conn.query_row(
            "SELECT COUNT(*) FROM notes WHERE domain = ?1 AND status = 'unread'",
            params![domain],
            |row| row.get(0),
        )?;

        let starred: u64 = self.conn.query_row(
            "SELECT COUNT(*) FROM notes WHERE domain = ?1 AND status = 'starred'",
            params![domain],
            |row| row.get(0),
        )?;

        let type_counts = {
            let mut stmt = self.conn.prepare(
                "SELECT note_type, COUNT(*) FROM notes WHERE domain = ?1 AND note_type != '' GROUP BY note_type ORDER BY COUNT(*) DESC",
            )?;
            stmt.query_map(params![domain], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?))
            })?
            .filter_map(|r| r.ok())
            .collect()
        };

        let recent_notes = self.list_notes(Some(domain), None, None, None, None, Some(limit))?;

        Ok(DomainBrief {
            domain: domain.to_string(),
            total_notes: total,
            unread,
            starred,
            by_type: type_counts,
            recent: recent_notes,
        })
    }

    /// Get domain distribution: how many notes per domain
    pub fn domain_stats(&self) -> Result<HashMap<String, u64>> {
        let counts = self.count_by_column("domain")?;
        Ok(counts.into_iter().collect())
    }

    /// Get tag-domain correlation: for each tag, which domains it appears in and how often.
    /// Returns a map of tag -> (domain -> count).
    pub fn tag_domain_map(&self) -> Result<HashMap<String, HashMap<String, u64>>> {
        let mut stmt = self
            .conn
            .prepare("SELECT tags, domain FROM notes WHERE tags != '' AND domain != ''")?;

        let mut result: HashMap<String, HashMap<String, u64>> = HashMap::new();

        let rows = stmt.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?;

        for row in rows.flatten() {
            let (tags_json, domain) = row;
            if let Ok(tags) = serde_json::from_str::<Vec<String>>(&tags_json) {
                for tag in tags {
                    let domain_counts = result.entry(tag).or_default();
                    *domain_counts.entry(domain.clone()).or_insert(0) += 1;
                }
            }
        }

        Ok(result)
    }

    /// Get exemplar notes for a domain (recent, well-classified notes)
    pub fn domain_exemplars(&self, domain: &str, limit: usize) -> Result<Vec<NoteRow>> {
        self.list_notes(Some(domain), None, None, None, None, Some(limit as u32))
    }
}

/// Extract significant search terms from content for FTS5 similarity queries.
/// Filters out common English stop words and short tokens.
fn extract_search_terms(content: &str, max_terms: usize) -> Vec<String> {
    let stop_words: std::collections::HashSet<&str> = [
        "the", "a", "an", "and", "or", "but", "in", "on", "at", "to", "for", "of", "with", "by", "from", "is", "it",
        "this", "that", "are", "was", "were", "be", "been", "being", "have", "has", "had", "do", "does", "did", "will",
        "would", "could", "should", "may", "might", "can", "shall", "not", "no", "nor", "so", "if", "then", "than",
        "too", "very", "just", "about", "up", "out", "into", "over", "after", "before", "between", "through", "during",
        "without", "again", "further", "once", "here", "there", "when", "where", "why", "how", "all", "each", "every",
        "both", "few", "more", "most", "other", "some", "such", "only", "own", "same", "also", "as", "its", "you",
        "your", "we", "our", "they", "their", "what", "which", "who", "whom",
    ]
    .into_iter()
    .collect();

    let mut word_counts: HashMap<String, usize> = HashMap::new();

    for word in content.split(|c: char| !c.is_alphanumeric() && c != '-') {
        let lower = word.to_lowercase();
        if lower.len() >= 3 && !stop_words.contains(lower.as_str()) {
            *word_counts.entry(lower).or_insert(0) += 1;
        }
    }

    // Sort by frequency (descending), take top N
    let mut terms: Vec<(String, usize)> = word_counts.into_iter().collect();
    terms.sort_by(|a, b| b.1.cmp(&a.1));

    terms.into_iter().take(max_terms).map(|(word, _)| word).collect()
}

/// A row from the notes table
#[derive(Debug, Clone, Serialize)]
pub struct NoteRow {
    pub path: String,
    pub title: String,
    pub domain: String,
    pub note_type: String,
    pub origin: String,
    pub status: String,
    pub date: String,
    pub tags: String,
    pub source: String,
    pub creator: String,
    pub body: String,
    pub summary: String,
}

impl NoteRow {
    pub(crate) fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            path: row.get(0)?,
            title: row.get(1)?,
            domain: row.get(2)?,
            note_type: row.get(3)?,
            origin: row.get(4)?,
            status: row.get(5)?,
            date: row.get(6)?,
            tags: row.get(7)?,
            source: row.get(8)?,
            creator: row.get(9)?,
            body: row.get(10)?,
            summary: row.get(11)?,
        })
    }
}

/// Vault-wide statistics
#[derive(Debug, Serialize)]
pub struct VaultStats {
    pub total_notes: u64,
    pub by_domain: Vec<(String, u64)>,
    pub by_type: Vec<(String, u64)>,
    pub by_status: Vec<(String, u64)>,
    pub schema_gaps: Vec<(String, u64)>,
}

/// Statistics and recent notes for a single domain
#[derive(Debug, Serialize)]
pub struct DomainBrief {
    pub domain: String,
    pub total_notes: u64,
    pub unread: u64,
    pub starred: u64,
    pub by_type: Vec<(String, u64)>,
    pub recent: Vec<NoteRow>,
}

/// Result of an indexing operation
#[derive(Debug, Serialize)]
pub struct IndexStats {
    pub total_scanned: u64,
    pub inserted: u64,
    pub updated: u64,
    pub unchanged: u64,
    pub removed: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_search_terms() {
        let content = "Rust programming language for building CLI tools with great performance";
        let terms = extract_search_terms(content, 10);
        assert!(!terms.is_empty());
        // Content words should be included
        assert!(terms.contains(&"rust".to_string()));
        assert!(terms.contains(&"programming".to_string()));
        assert!(terms.contains(&"building".to_string()));
        // Stop words should be excluded
        assert!(!terms.contains(&"for".to_string()));
        assert!(!terms.contains(&"with".to_string()));
    }

    #[test]
    fn test_extract_search_terms_empty_input() {
        let terms = extract_search_terms("", 5);
        assert!(terms.is_empty());
    }

    #[test]
    fn test_extract_search_terms_respects_limit() {
        let content = "one two three four five six seven eight nine ten eleven twelve";
        let terms = extract_search_terms(content, 3);
        assert!(terms.len() <= 3);
    }

    #[test]
    fn test_open_memory_index() {
        let index = SearchIndex::open_memory().expect("Failed to open in-memory index");
        let stats = index.stats().expect("Failed to get stats");
        assert_eq!(stats.total_notes, 0);
    }

    #[test]
    fn test_domain_stats_empty() {
        let index = SearchIndex::open_memory().expect("Failed to open in-memory index");
        let stats = index.domain_stats().expect("Failed to get domain stats");
        assert!(stats.is_empty());
    }

    #[test]
    fn test_tag_domain_map_empty() {
        let index = SearchIndex::open_memory().expect("Failed to open in-memory index");
        let map = index.tag_domain_map().expect("Failed to get tag domain map");
        assert!(map.is_empty());
    }

    #[test]
    fn test_find_similar_empty_content() {
        let index = SearchIndex::open_memory().expect("Failed to open in-memory index");
        let results = index.find_similar("", 5).expect("Failed find_similar");
        assert!(results.is_empty());
    }
}
