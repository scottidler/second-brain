//! SQLite-backed full-text search index for vault notes
//!
//! Provides FTS5-powered search, incremental indexing by mtime, and
//! domain/tag analytics. Shared by oracle (MCP server) and cortex (daemon).

use crate::config::ScanConfig;
use crate::detail;
use crate::note::scan_vault;
use crate::schema::{Domain, NoteType, Origin, Status};
use chrono;
use eyre::{Result, WrapErr};
use regex::Regex;
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

/// Extract a string value from the frontmatter extra map (for cortex-* fields)
fn extract_cortex_string(extra: &HashMap<String, serde_yaml::Value>, key: &str) -> String {
    extra
        .get(key)
        .and_then(|v| match v {
            serde_yaml::Value::String(s) => Some(s.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

/// Extract a boolean value from the frontmatter extra map as i64 (0/1)
fn extract_cortex_bool(extra: &HashMap<String, serde_yaml::Value>, key: &str) -> i64 {
    extra
        .get(key)
        .map(|v| match v {
            serde_yaml::Value::Bool(b) => i64::from(*b),
            _ => 0,
        })
        .unwrap_or(0)
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
                modified_at INTEGER,
                quality TEXT DEFAULT '',
                classified INTEGER DEFAULT 0,
                classified_by TEXT DEFAULT '',
                confidence TEXT DEFAULT '',
                needs_review INTEGER DEFAULT 0,
                duplicate_group TEXT DEFAULT ''
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

        // Add governance columns to existing tables (safe to call multiple times)
        self.ensure_governance_columns()?;

        Ok(())
    }

    /// Add cortex governance columns if they don't exist yet (handles schema migration)
    fn ensure_governance_columns(&self) -> Result<()> {
        let mut stmt = self.conn.prepare("PRAGMA table_info(notes)")?;
        let existing_columns: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(1))?
            .filter_map(|r| r.ok())
            .collect();

        let governance_columns = [
            ("quality", "TEXT DEFAULT ''"),
            ("classified", "INTEGER DEFAULT 0"),
            ("classified_by", "TEXT DEFAULT ''"),
            ("confidence", "TEXT DEFAULT ''"),
            ("needs_review", "INTEGER DEFAULT 0"),
            ("duplicate_group", "TEXT DEFAULT ''"),
        ];

        for (col, col_type) in governance_columns {
            if !existing_columns.contains(&col.to_string()) {
                self.conn
                    .execute_batch(&format!("ALTER TABLE notes ADD COLUMN {col} {col_type};"))?;
            }
        }

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

            // Extract cortex governance fields from frontmatter.extra
            let quality = extract_cortex_string(&fm.extra, "cortex-quality");
            let classified = extract_cortex_bool(&fm.extra, "cortex-classified");
            let classified_by = extract_cortex_string(&fm.extra, "cortex-classified-by");
            let confidence = extract_cortex_string(&fm.extra, "cortex-confidence");
            let needs_review = extract_cortex_bool(&fm.extra, "cortex-needs-review");
            let duplicate_group = extract_cortex_string(&fm.extra, "cortex-duplicate-group");

            self.conn.execute(
                "INSERT OR REPLACE INTO notes (path, title, domain, note_type, origin, status, date, tags, source, creator, body, summary, modified_at, quality, classified, classified_by, confidence, needs_review, duplicate_group)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)",
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
                    quality,
                    classified,
                    classified_by,
                    confidence,
                    needs_review,
                    duplicate_group,
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

    /// Find notes matching a specific tag, optionally filtered by domain
    pub fn tag_search(&self, tag: &str, domain: Option<&str>, limit: Option<u32>) -> Result<Vec<NoteRow>> {
        let limit = limit.unwrap_or(20);

        // Tags are stored as JSON arrays, use Rust-side filtering
        let mut sql = String::from(
            "SELECT path, title, domain, note_type, origin, status, date, tags, source, creator, body, summary
             FROM notes WHERE tags != ''",
        );
        let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = vec![];
        let mut param_idx = 1;

        if let Some(d) = domain {
            sql.push_str(&format!(" AND domain = ?{param_idx}"));
            param_values.push(Box::new(d.to_string()));
            param_idx += 1;
        }
        let _ = param_idx;

        sql.push_str(" ORDER BY date DESC");

        let params_refs: Vec<&dyn rusqlite::types::ToSql> = param_values.iter().map(|p| p.as_ref()).collect();
        let mut stmt = self.conn.prepare(&sql)?;

        let tag_lower = tag.to_lowercase();
        let is_prefix = tag_lower.ends_with('*');
        let prefix = if is_prefix { &tag_lower[..tag_lower.len() - 1] } else { &tag_lower };

        let rows: Vec<NoteRow> = stmt
            .query_map(params_refs.as_slice(), NoteRow::from_row)?
            .filter_map(|r| r.ok())
            .filter(|note| {
                if let Ok(tags) = serde_json::from_str::<Vec<String>>(&note.tags) {
                    tags.iter().any(|t| {
                        let t_lower = t.to_lowercase();
                        if is_prefix { t_lower.starts_with(prefix) } else { t_lower == *prefix }
                    })
                } else {
                    false
                }
            })
            .take(limit as usize)
            .collect();

        Ok(rows)
    }

    /// Get all tags with their counts and domain distribution
    pub fn tag_stats(&self) -> Result<Vec<TagStat>> {
        let mut stmt = self.conn.prepare("SELECT tags, domain FROM notes WHERE tags != ''")?;

        let mut tag_info: HashMap<String, (u64, HashMap<String, u64>)> = HashMap::new();

        let rows = stmt.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?;

        for row in rows.flatten() {
            let (tags_json, domain) = row;
            if let Ok(tags) = serde_json::from_str::<Vec<String>>(&tags_json) {
                for tag in tags {
                    let entry = tag_info.entry(tag).or_insert_with(|| (0, HashMap::new()));
                    entry.0 += 1;
                    if !domain.is_empty() {
                        *entry.1.entry(domain.clone()).or_insert(0) += 1;
                    }
                }
            }
        }

        let mut stats: Vec<TagStat> = tag_info
            .into_iter()
            .map(|(tag, (count, domains))| {
                let domain_list: Vec<String> = domains.keys().cloned().collect();
                TagStat {
                    tag,
                    count,
                    domains: domain_list,
                }
            })
            .collect();

        stats.sort_by(|a, b| b.count.cmp(&a.count));
        Ok(stats)
    }

    /// Find tags that co-occur with the given tag, sorted by frequency
    pub fn tag_cooccurrence(&self, tag: &str) -> Result<Vec<(String, u64)>> {
        let mut stmt = self.conn.prepare("SELECT tags FROM notes WHERE tags != ''")?;

        let tag_lower = tag.to_lowercase();
        let mut cooccur: HashMap<String, u64> = HashMap::new();

        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;

        for row in rows.flatten() {
            if let Ok(tags) = serde_json::from_str::<Vec<String>>(&row) {
                let has_target = tags.iter().any(|t| t.to_lowercase() == tag_lower);
                if has_target {
                    for t in &tags {
                        let t_lower = t.to_lowercase();
                        if t_lower != tag_lower {
                            *cooccur.entry(t_lower).or_insert(0) += 1;
                        }
                    }
                }
            }
        }

        let mut result: Vec<(String, u64)> = cooccur.into_iter().collect();
        result.sort_by(|a, b| b.1.cmp(&a.1));
        Ok(result)
    }

    /// Get recent notes across the vault, optionally filtered by domain and/or note type
    pub fn recent_notes(
        &self,
        days: Option<u32>,
        domain: Option<&str>,
        note_type: Option<&str>,
        limit: Option<u32>,
    ) -> Result<Vec<NoteRow>> {
        let days = days.unwrap_or(7);
        let limit = limit.unwrap_or(20);

        let cutoff = chrono::Local::now()
            .date_naive()
            .checked_sub_days(chrono::Days::new(u64::from(days)))
            .map(|d| d.format("%Y-%m-%d").to_string())
            .unwrap_or_default();

        self.list_notes(domain, note_type, None, Some(&cutoff), None, Some(limit))
    }

    /// Find outbound wikilinks from a note's body
    pub fn find_outbound_links(&self, path: &str) -> Result<Vec<OutboundLink>> {
        let note = self.get_note(path)?;
        let body = match note {
            Some(n) => n.body,
            None => return Ok(vec![]),
        };

        let targets = extract_wikilinks(&body);
        let mut links = Vec::new();

        for target in targets {
            // Try to resolve the target to an actual note path
            let resolved = self.resolve_wikilink(&target)?;
            links.push(OutboundLink {
                target: target.clone(),
                resolved_path: resolved.clone(),
                exists: resolved.is_some(),
            });
        }

        Ok(links)
    }

    /// Find notes that link TO the given note (inbound links)
    pub fn find_inbound_links(&self, path: &str) -> Result<Vec<NoteRow>> {
        // Extract the stem from the path (filename without extension)
        let stem = Path::new(path).file_stem().and_then(|s| s.to_str()).unwrap_or(path);

        let mut stmt = self.conn.prepare(
            "SELECT path, title, domain, note_type, origin, status, date, tags, source, creator, body, summary
             FROM notes WHERE body LIKE ?1",
        )?;

        let pattern = format!("%[[{stem}%");
        let rows: Vec<NoteRow> = stmt
            .query_map(params![pattern], NoteRow::from_row)?
            .filter_map(|r| r.ok())
            .filter(|note| {
                // Verify with exact wikilink parsing
                let links = extract_wikilinks(&note.body);
                links.iter().any(|l| l.eq_ignore_ascii_case(stem))
            })
            .collect();

        Ok(rows)
    }

    /// Find notes with no inbound links (orphans)
    pub fn orphan_notes(&self, limit: Option<u32>) -> Result<Vec<NoteRow>> {
        let limit = limit.unwrap_or(50);

        // Get all notes
        let mut stmt = self.conn.prepare(
            "SELECT path, title, domain, note_type, origin, status, date, tags, source, creator, body, summary
             FROM notes ORDER BY date DESC",
        )?;
        let all_notes: Vec<NoteRow> = stmt.query_map([], NoteRow::from_row)?.filter_map(|r| r.ok()).collect();

        // Collect all wikilink targets across the vault
        let mut linked_stems: std::collections::HashSet<String> = std::collections::HashSet::new();
        for note in &all_notes {
            for link in extract_wikilinks(&note.body) {
                linked_stems.insert(link.to_lowercase());
            }
        }

        // Notes whose stem is never referenced
        let orphans: Vec<NoteRow> = all_notes
            .into_iter()
            .filter(|note| {
                let stem = Path::new(&note.path)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_lowercase();
                !linked_stems.contains(&stem)
            })
            .take(limit as usize)
            .collect();

        Ok(orphans)
    }

    /// Try to resolve a wikilink target to an actual note path in the index
    fn resolve_wikilink(&self, target: &str) -> Result<Option<String>> {
        // Try exact path match first
        let row: Option<String> = self
            .conn
            .query_row("SELECT path FROM notes WHERE path = ?1", params![target], |row| {
                row.get(0)
            })
            .ok();
        if row.is_some() {
            return Ok(row);
        }

        // Try matching by stem (filename without extension)
        let target_lower = target.to_lowercase();
        let row: Option<String> = self
            .conn
            .query_row(
                "SELECT path FROM notes WHERE LOWER(path) LIKE ?1 LIMIT 1",
                params![format!("%/{target_lower}.md")],
                |row| row.get(0),
            )
            .ok();
        if row.is_some() {
            return Ok(row);
        }

        // Try matching just the stem anywhere
        let row: Option<String> = self
            .conn
            .query_row(
                "SELECT path FROM notes WHERE LOWER(path) LIKE ?1 LIMIT 1",
                params![format!("%{target_lower}%")],
                |row| row.get(0),
            )
            .ok();
        Ok(row)
    }

    /// Get creator statistics (name -> count), sorted by count
    pub fn creator_stats(&self) -> Result<Vec<(String, u64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT creator, COUNT(*) as cnt FROM notes WHERE creator != '' GROUP BY creator ORDER BY cnt DESC",
        )?;
        let rows = stmt
            .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?)))?
            .filter_map(|r| r.ok())
            .collect();
        Ok(rows)
    }

    /// Get notes by a specific creator
    pub fn notes_by_creator(&self, creator: &str, domain: Option<&str>, limit: Option<u32>) -> Result<Vec<NoteRow>> {
        let limit = limit.unwrap_or(20);
        let mut sql = String::from(
            "SELECT path, title, domain, note_type, origin, status, date, tags, source, creator, body, summary
             FROM notes WHERE LOWER(creator) LIKE ?1",
        );
        let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> =
            vec![Box::new(format!("%{}%", creator.to_lowercase()))];
        let mut param_idx = 2;

        if let Some(d) = domain {
            sql.push_str(&format!(" AND domain = ?{param_idx}"));
            param_values.push(Box::new(d.to_string()));
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

    /// Get source domain statistics (host -> count), sorted by count
    pub fn source_domain_stats(&self) -> Result<Vec<(String, u64)>> {
        let mut stmt = self.conn.prepare("SELECT source FROM notes WHERE source != ''")?;

        let mut host_counts: HashMap<String, u64> = HashMap::new();
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;

        for row in rows.flatten() {
            if let Some(host) = extract_host(&row) {
                *host_counts.entry(host).or_insert(0) += 1;
            }
        }

        let mut result: Vec<(String, u64)> = host_counts.into_iter().collect();
        result.sort_by(|a, b| b.1.cmp(&a.1));
        Ok(result)
    }

    /// Get notes from a specific source domain
    pub fn notes_by_source_domain(&self, host: &str, domain: Option<&str>, limit: Option<u32>) -> Result<Vec<NoteRow>> {
        let limit = limit.unwrap_or(20);
        let mut sql = String::from(
            "SELECT path, title, domain, note_type, origin, status, date, tags, source, creator, body, summary
             FROM notes WHERE source LIKE ?1",
        );
        let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> =
            vec![Box::new(format!("%{}%", host.to_lowercase()))];
        let mut param_idx = 2;

        if let Some(d) = domain {
            sql.push_str(&format!(" AND domain = ?{param_idx}"));
            param_values.push(Box::new(d.to_string()));
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

/// Extract wikilink targets from note body, skipping fenced code blocks.
/// Handles [[simple]], [[with|alias]], [[with#heading]], [[path/to/note]].
fn extract_wikilinks(body: &str) -> Vec<String> {
    let re = Regex::new(r"\[\[([^\]|#]+)(?:[|#][^\]]+)?\]\]").expect("wikilink regex");
    let mut targets = Vec::new();
    let mut in_code_block = false;

    for line in body.lines() {
        if line.trim_start().starts_with("```") {
            in_code_block = !in_code_block;
            continue;
        }
        if in_code_block {
            continue;
        }
        for cap in re.captures_iter(line) {
            if let Some(m) = cap.get(1) {
                let target = m.as_str().trim();
                if !target.is_empty() {
                    targets.push(target.to_string());
                }
            }
        }
    }

    targets
}

/// Extract hostname from a URL string
fn extract_host(url: &str) -> Option<String> {
    // Simple extraction without pulling in the url crate
    let stripped = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://"))?;
    let host = stripped.split('/').next()?;
    let host = host.split('?').next()?;
    // Remove www. prefix
    let host = host.strip_prefix("www.").unwrap_or(host);
    if host.is_empty() { None } else { Some(host.to_lowercase()) }
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

/// Tag with count and domain distribution
#[derive(Debug, Serialize)]
pub struct TagStat {
    pub tag: String,
    pub count: u64,
    pub domains: Vec<String>,
}

/// An outbound wikilink from a note
#[derive(Debug, Serialize)]
pub struct OutboundLink {
    pub target: String,
    pub resolved_path: Option<String>,
    pub exists: bool,
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

    /// Helper: insert a test note directly into the DB
    fn insert_test_note(index: &SearchIndex, path: &str, title: &str, domain: &str, tags: &[&str], body: &str) {
        let tags_json = serde_json::to_string(&tags).expect("tags json");
        index
            .conn
            .execute(
                "INSERT INTO notes (path, title, domain, note_type, origin, status, date, tags, source, creator, body, summary, modified_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                params![path, title, domain, "article", "assisted", "", "2026-03-21", tags_json, "", "", body, "", 0],
            )
            .expect("insert test note");
    }

    #[test]
    fn test_tag_search_exact() {
        let index = SearchIndex::open_memory().expect("open");
        insert_test_note(&index, "notes/a.md", "Rust CLI", "tech", &["rust", "cli"], "body");
        insert_test_note(&index, "notes/b.md", "Rust Web", "tech", &["rust", "web"], "body");
        insert_test_note(&index, "notes/c.md", "Python ML", "ai", &["python", "ml"], "body");

        let results = index.tag_search("rust", None, None).expect("tag_search");
        assert_eq!(results.len(), 2);

        let results = index.tag_search("python", None, None).expect("tag_search");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].path, "notes/c.md");
    }

    #[test]
    fn test_tag_search_prefix() {
        let index = SearchIndex::open_memory().expect("open");
        insert_test_note(&index, "notes/a.md", "Rust CLI", "tech", &["rust", "rust-cli"], "body");
        insert_test_note(&index, "notes/b.md", "Ruby", "tech", &["ruby"], "body");

        let results = index.tag_search("rust*", None, None).expect("tag_search prefix");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].path, "notes/a.md");
    }

    #[test]
    fn test_tag_search_with_domain_filter() {
        let index = SearchIndex::open_memory().expect("open");
        insert_test_note(&index, "notes/a.md", "AI Rust", "ai", &["rust"], "body");
        insert_test_note(&index, "notes/b.md", "Tech Rust", "tech", &["rust"], "body");

        let results = index.tag_search("rust", Some("ai"), None).expect("tag_search domain");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].path, "notes/a.md");
    }

    #[test]
    fn test_tag_stats() {
        let index = SearchIndex::open_memory().expect("open");
        insert_test_note(&index, "notes/a.md", "A", "tech", &["rust", "cli"], "body");
        insert_test_note(&index, "notes/b.md", "B", "tech", &["rust", "web"], "body");
        insert_test_note(&index, "notes/c.md", "C", "ai", &["rust", "ml"], "body");

        let stats = index.tag_stats().expect("tag_stats");
        let rust_stat = stats.iter().find(|s| s.tag == "rust").expect("rust tag");
        assert_eq!(rust_stat.count, 3);
        assert!(rust_stat.domains.contains(&"tech".to_string()));
        assert!(rust_stat.domains.contains(&"ai".to_string()));

        let cli_stat = stats.iter().find(|s| s.tag == "cli").expect("cli tag");
        assert_eq!(cli_stat.count, 1);
    }

    #[test]
    fn test_tag_cooccurrence() {
        let index = SearchIndex::open_memory().expect("open");
        insert_test_note(&index, "notes/a.md", "A", "tech", &["rust", "cli", "linux"], "body");
        insert_test_note(&index, "notes/b.md", "B", "tech", &["rust", "web"], "body");
        insert_test_note(&index, "notes/c.md", "C", "ai", &["python", "ml"], "body");

        let cooccur = index.tag_cooccurrence("rust").expect("cooccurrence");
        // cli, linux, web all co-occur with rust
        assert_eq!(cooccur.len(), 3);
        assert!(cooccur.iter().any(|(t, c)| t == "cli" && *c == 1));
        assert!(cooccur.iter().any(|(t, c)| t == "web" && *c == 1));
        assert!(cooccur.iter().any(|(t, c)| t == "linux" && *c == 1));
    }

    #[test]
    fn test_extract_wikilinks_simple() {
        let body = "See [[some-note]] and [[another-note]] for details.";
        let links = extract_wikilinks(body);
        assert_eq!(links, vec!["some-note", "another-note"]);
    }

    #[test]
    fn test_extract_wikilinks_with_alias() {
        let body = "Check [[some-note|display text]] here.";
        let links = extract_wikilinks(body);
        assert_eq!(links, vec!["some-note"]);
    }

    #[test]
    fn test_extract_wikilinks_with_heading() {
        let body = "See [[some-note#heading]] for the section.";
        let links = extract_wikilinks(body);
        assert_eq!(links, vec!["some-note"]);
    }

    #[test]
    fn test_extract_wikilinks_skips_code_blocks() {
        let body = "Before\n```\n[[code-link]]\n```\nAfter [[real-link]]";
        let links = extract_wikilinks(body);
        assert_eq!(links, vec!["real-link"]);
    }

    #[test]
    fn test_extract_host() {
        assert_eq!(
            extract_host("https://www.youtube.com/watch?v=abc"),
            Some("youtube.com".to_string())
        );
        assert_eq!(
            extract_host("https://github.com/user/repo"),
            Some("github.com".to_string())
        );
        assert_eq!(extract_host("http://example.com"), Some("example.com".to_string()));
        assert_eq!(extract_host("not-a-url"), None);
    }

    #[test]
    fn test_creator_stats() {
        let index = SearchIndex::open_memory().expect("open");
        index.conn.execute(
            "INSERT INTO notes (path, title, domain, note_type, origin, status, date, tags, source, creator, body, summary, modified_at)
             VALUES ('a.md', 'A', 'tech', 'youtube', 'assisted', '', '2026-03-21', '[]', '', 'Alice', '', '', 0)",
            [],
        ).expect("insert");
        index.conn.execute(
            "INSERT INTO notes (path, title, domain, note_type, origin, status, date, tags, source, creator, body, summary, modified_at)
             VALUES ('b.md', 'B', 'ai', 'youtube', 'assisted', '', '2026-03-21', '[]', '', 'Alice', '', '', 0)",
            [],
        ).expect("insert");
        index.conn.execute(
            "INSERT INTO notes (path, title, domain, note_type, origin, status, date, tags, source, creator, body, summary, modified_at)
             VALUES ('c.md', 'C', 'tech', 'article', 'assisted', '', '2026-03-21', '[]', '', 'Bob', '', '', 0)",
            [],
        ).expect("insert");

        let stats = index.creator_stats().expect("creator_stats");
        assert_eq!(stats[0], ("Alice".to_string(), 2));
        assert_eq!(stats[1], ("Bob".to_string(), 1));
    }

    #[test]
    fn test_source_domain_stats() {
        let index = SearchIndex::open_memory().expect("open");
        index.conn.execute(
            "INSERT INTO notes (path, title, domain, note_type, origin, status, date, tags, source, creator, body, summary, modified_at)
             VALUES ('a.md', 'A', 'tech', 'youtube', 'assisted', '', '2026-03-21', '[]', 'https://www.youtube.com/watch?v=abc', '', '', '', 0)",
            [],
        ).expect("insert");
        index.conn.execute(
            "INSERT INTO notes (path, title, domain, note_type, origin, status, date, tags, source, creator, body, summary, modified_at)
             VALUES ('b.md', 'B', 'tech', 'youtube', 'assisted', '', '2026-03-21', '[]', 'https://youtube.com/watch?v=def', '', '', '', 0)",
            [],
        ).expect("insert");
        index.conn.execute(
            "INSERT INTO notes (path, title, domain, note_type, origin, status, date, tags, source, creator, body, summary, modified_at)
             VALUES ('c.md', 'C', 'tech', 'article', 'assisted', '', '2026-03-21', '[]', 'https://github.com/user/repo', '', '', '', 0)",
            [],
        ).expect("insert");

        let stats = index.source_domain_stats().expect("source_domain_stats");
        assert_eq!(stats[0], ("youtube.com".to_string(), 2));
        assert_eq!(stats[1], ("github.com".to_string(), 1));
    }

    #[test]
    fn test_find_outbound_links() {
        let index = SearchIndex::open_memory().expect("open");
        insert_test_note(&index, "notes/a.md", "A", "tech", &[], "See [[b]] and [[c|see C]].");
        insert_test_note(&index, "notes/b.md", "B", "tech", &[], "Just body.");

        let links = index.find_outbound_links("notes/a.md").expect("outbound");
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].target, "b");
        assert_eq!(links[1].target, "c");
    }

    #[test]
    fn test_find_inbound_links() {
        let index = SearchIndex::open_memory().expect("open");
        insert_test_note(&index, "notes/a.md", "A", "tech", &[], "Links to [[b]].");
        insert_test_note(&index, "notes/b.md", "B", "tech", &[], "No links.");
        insert_test_note(&index, "notes/c.md", "C", "tech", &[], "Also links to [[b]].");

        let inbound = index.find_inbound_links("notes/b.md").expect("inbound");
        assert_eq!(inbound.len(), 2);
        let paths: Vec<&str> = inbound.iter().map(|n| n.path.as_str()).collect();
        assert!(paths.contains(&"notes/a.md"));
        assert!(paths.contains(&"notes/c.md"));
    }

    #[test]
    fn test_governance_columns_exist() {
        let index = SearchIndex::open_memory().expect("open");
        // Insert a note with governance fields via direct SQL
        index
            .conn
            .execute(
                "INSERT INTO notes (path, title, domain, note_type, origin, status, date, tags, source, creator, body, summary, modified_at, quality, classified, classified_by, confidence, needs_review, duplicate_group)
                 VALUES ('test.md', 'Test', 'tech', 'article', 'assisted', '', '2026-03-21', '[]', '', '', '', '', 0, 'high', 1, 'deterministic', 'high', 0, '')",
                [],
            )
            .expect("insert with governance columns");

        let quality: String = index
            .conn
            .query_row("SELECT quality FROM notes WHERE path = 'test.md'", [], |row| row.get(0))
            .expect("query quality");
        assert_eq!(quality, "high");

        let classified: i64 = index
            .conn
            .query_row("SELECT classified FROM notes WHERE path = 'test.md'", [], |row| {
                row.get(0)
            })
            .expect("query classified");
        assert_eq!(classified, 1);
    }
}
