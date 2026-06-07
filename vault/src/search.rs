//! SQLite-backed full-text search index for vault notes
//!
//! Provides FTS5-powered search, incremental indexing by mtime, and
//! domain/tag analytics. Shared by oracle (MCP server) and cortex (daemon).

use crate::config::ScanConfig;
use crate::detail;
use crate::distilled::Claim;
use crate::note::{Note, scan_vault};
use crate::schema::{Domain, NoteType, Origin, Status};
use chrono;
use eyre::{Result, WrapErr};
use regex::Regex;
use rusqlite::{Connection, params};
use serde::Serialize;
use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

/// Busy-timeout for every SQLite connection opened through `SearchIndex`.
///
/// Two writers can briefly contend for the WAL writer lock (cortex's embed
/// loop and oracle's `index_vault` updates). Five seconds comfortably
/// covers the worst-case write transaction (one embedding upsert batch,
/// under 200ms) without masking real deadlocks.
const BUSY_TIMEOUT: Duration = Duration::from_millis(5_000);

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

/// Extract an optional string from frontmatter extras. Returns None when the
/// key is absent or the value is not a string.
fn extract_cortex_optional_string(extra: &HashMap<String, serde_yaml::Value>, key: &str) -> Option<String> {
    extra.get(key).and_then(|v| match v {
        serde_yaml::Value::String(s) if !s.is_empty() => Some(s.clone()),
        _ => None,
    })
}

/// Extract an optional integer from frontmatter extras. Returns None when the
/// key is absent or the value is not a number.
fn extract_cortex_optional_i64(extra: &HashMap<String, serde_yaml::Value>, key: &str) -> Option<i64> {
    extra.get(key).and_then(|v| match v {
        serde_yaml::Value::Number(n) => n.as_i64(),
        _ => None,
    })
}

/// Parse the `## Summary` section out of a published note body.
///
/// Returns `None` when the section is absent so callers can fall back to the
/// legacy `detail::extract_summary`. The anchor is exact (case-sensitive, no
/// fuzzy match) per the design's "managed sections" contract.
pub fn parse_body_summary(body: &str) -> Option<String> {
    let mut lines = body.lines();
    while let Some(line) = lines.next() {
        if line.trim() == "## Summary" {
            let mut collected = String::new();
            for next in lines.by_ref() {
                if next.starts_with("## ") {
                    break;
                }
                collected.push_str(next);
                collected.push('\n');
            }
            let trimmed = collected.trim();
            if trimmed.is_empty() {
                return None;
            }
            return Some(trimmed.to_string());
        }
    }
    None
}

/// Parse the `## Claims` section out of a published note body.
///
/// Each bulleted line becomes one `Claim`; a trailing `[anchor]` marker is
/// extracted into `Claim.anchor`. Returns an empty Vec when no `## Claims`
/// section is present.
pub fn parse_body_claims(body: &str) -> Vec<Claim> {
    let mut claims = Vec::new();
    let mut in_claims = false;
    for line in body.lines() {
        if line.trim() == "## Claims" {
            in_claims = true;
            continue;
        }
        if !in_claims {
            continue;
        }
        if line.starts_with("## ") {
            break;
        }
        let trimmed = line.trim_start();
        let bullet = trimmed.strip_prefix("- ").or_else(|| trimmed.strip_prefix("* "));
        let Some(content) = bullet else {
            continue;
        };
        let content = content.trim();
        if content.is_empty() {
            continue;
        }
        let (text, anchor) = split_trailing_anchor(content);
        claims.push(Claim {
            text: text.to_string(),
            anchor,
        });
    }
    claims
}

/// Split a bulleted claim line into its text and an optional trailing
/// `[anchor]` marker. Only a `[...]` group at the very end of the line is
/// treated as an anchor; mid-text brackets are left in the claim text.
fn split_trailing_anchor(content: &str) -> (&str, Option<String>) {
    let trimmed = content.trim_end();
    if !trimmed.ends_with(']') {
        return (trimmed, None);
    }
    let Some(open_idx) = trimmed.rfind('[') else {
        return (trimmed, None);
    };
    let inner = &trimmed[open_idx + 1..trimmed.len() - 1];
    if inner.is_empty() {
        return (trimmed, None);
    }
    let text = trimmed[..open_idx].trim_end();
    (text, Some(inner.to_string()))
}

/// Result of indexing one note. Distinguishes inserts (new path) from
/// updates (existing path) for the caller's progress reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexAction {
    Inserted,
    Updated,
}

#[cfg(feature = "vec")]
mod vector;

#[cfg(feature = "vec")]
pub use vector::{
    BatchUpsert, EmbeddingKind, FusedHit, K_RRF_INPUT, RRF_K, StaleTarget, VectorHit, reciprocal_rank_fusion,
    reciprocal_rank_fusion_weighted,
};

mod graph;

pub use graph::{Edge, EntityRow, FactEdge, GraphNoteRow, GraphReach};

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
        conn.busy_timeout(BUSY_TIMEOUT)?;

        let index = Self { conn };
        index.ensure_schema()?;
        Ok(index)
    }

    /// Open an in-memory search index. Used by unit and integration
    /// tests across the workspace; cortex's embed tests rely on this.
    pub fn open_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch("PRAGMA foreign_keys=ON;")?;
        conn.busy_timeout(BUSY_TIMEOUT)?;
        let index = Self { conn };
        index.ensure_schema()?;
        Ok(index)
    }

    fn ensure_schema(&self) -> Result<()> {
        // Fresh-DB path: create the content table with every column. Existing
        // DBs land in the migration helpers below.
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
                claims TEXT DEFAULT '',
                modified_at INTEGER,
                quality TEXT DEFAULT '',
                classified INTEGER DEFAULT 0,
                classified_by TEXT DEFAULT '',
                confidence TEXT DEFAULT '',
                needs_review INTEGER DEFAULT 0,
                duplicate_group TEXT DEFAULT '',
                cortex_repo_stars INTEGER,
                cortex_repo_last_commit TEXT,
                cortex_repo_primary_language TEXT,
                cortex_video_duration_seconds INTEGER,
                cortex_video_channel TEXT,
                cortex_video_published_at TEXT,
                cortex_thread_platform TEXT,
                cortex_thread_post_count INTEGER,
                cortex_thread_author TEXT,
                search_hit_count INTEGER DEFAULT 0,
                last_accessed_at INTEGER,
                inbound_link_count INTEGER DEFAULT 0,
                pinned INTEGER DEFAULT 0
            );

            CREATE INDEX IF NOT EXISTS idx_notes_domain ON notes(domain);
            CREATE INDEX IF NOT EXISTS idx_notes_note_type ON notes(note_type);
            CREATE INDEX IF NOT EXISTS idx_notes_status ON notes(status);
            CREATE INDEX IF NOT EXISTS idx_notes_date ON notes(date);
            -- Doc 3 cold-note SELECT filters on `modified_at < ?`. Without
            -- this index the query is a sequential scan over every note.
            CREATE INDEX IF NOT EXISTS idx_notes_modified_at ON notes(modified_at);",
        )?;

        self.ensure_graph_schema()?;

        // Migrate older DBs: add columns the CREATE TABLE above lists but
        // existing schemas may be missing.
        self.ensure_governance_columns()?;
        self.ensure_distilled_columns()?;

        // FTS5 cannot ALTER, so we detect old (no-claims) schemas and rebuild.
        // Triggers attach to `notes`, not `notes_fts`, so they must be dropped
        // explicitly before the FTS table; CREATE TRIGGER would otherwise fail
        // with "trigger already exists" on the recreate path.
        self.ensure_fts5_schema()?;

        #[cfg(feature = "vec")]
        self.ensure_vec_schema()?;

        Ok(())
    }

    /// Create the embedding tables and the `embedding_config` key/value row
    /// used as the single source of truth for the active embedding model.
    ///
    /// Storage shape is deliberately boring: one regular table with a `BLOB`
    /// column for the f32 vector and an explicit `dim` column for length
    /// validation. No virtual table, no SQLite extension, no triggers; FK
    /// CASCADE works natively. The hybrid retrieval path runs FTS5 and
    /// vector as two separate queries and fuses them with RRF in Rust, so
    /// there is no single-query SQL composition that would benefit from
    /// a `vec0` virtual table.
    ///
    /// Idempotent: every statement uses `IF NOT EXISTS` (or `INSERT OR
    /// IGNORE` for the config row) so existing DBs upgrade without data
    /// loss.
    #[cfg(feature = "vec")]
    fn ensure_vec_schema(&self) -> Result<()> {
        // Two-step setup: the static DDL runs as a batch, the
        // backend-dependent seed for `active_model` runs separately so
        // the model_version string stays a Rust const (driven by the
        // active embedding-backend feature) rather than baked into a SQL
        // string literal.
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS note_embeddings (
                id INTEGER PRIMARY KEY,
                note_path TEXT NOT NULL,
                kind TEXT NOT NULL CHECK (kind IN ('summary', 'transcript-chunk')),
                chunk_index INTEGER NOT NULL DEFAULT 0,
                text TEXT NOT NULL,
                embedding BLOB NOT NULL,
                dim INTEGER NOT NULL,
                model_version TEXT NOT NULL,
                produced_at INTEGER NOT NULL,
                source_modified_at INTEGER NOT NULL,
                FOREIGN KEY (note_path) REFERENCES notes(path) ON DELETE CASCADE,
                UNIQUE (note_path, kind, chunk_index, model_version)
            );

            CREATE INDEX IF NOT EXISTS idx_note_embeddings_path
                ON note_embeddings(note_path);
            CREATE INDEX IF NOT EXISTS idx_note_embeddings_stale
                ON note_embeddings(source_modified_at);
            CREATE INDEX IF NOT EXISTS idx_note_embeddings_kind_model
                ON note_embeddings(kind, model_version);

            CREATE TABLE IF NOT EXISTS embedding_config (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            INSERT OR IGNORE INTO embedding_config (key, value)
                VALUES ('active_dim', '384');",
        )?;
        self.conn.execute(
            "INSERT OR IGNORE INTO embedding_config (key, value) VALUES ('active_model', ?1)",
            rusqlite::params![crate::embedding::ACTIVE_MODEL_VERSION],
        )?;
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

    /// Add distilled-contract columns to existing DBs. New columns are the
    /// `claims` FTS-indexed text, the per-kind `cortex_*` metadata, and the
    /// signal columns Doc 3 will own.
    fn ensure_distilled_columns(&self) -> Result<()> {
        let mut stmt = self.conn.prepare("PRAGMA table_info(notes)")?;
        let existing_columns: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(1))?
            .filter_map(|r| r.ok())
            .collect();

        let distilled_columns = [
            ("claims", "TEXT DEFAULT ''"),
            ("cortex_repo_stars", "INTEGER"),
            ("cortex_repo_last_commit", "TEXT"),
            ("cortex_repo_primary_language", "TEXT"),
            ("cortex_video_duration_seconds", "INTEGER"),
            ("cortex_video_channel", "TEXT"),
            ("cortex_video_published_at", "TEXT"),
            ("cortex_thread_platform", "TEXT"),
            ("cortex_thread_post_count", "INTEGER"),
            ("cortex_thread_author", "TEXT"),
            ("search_hit_count", "INTEGER DEFAULT 0"),
            ("last_accessed_at", "INTEGER"),
            ("inbound_link_count", "INTEGER DEFAULT 0"),
            ("pinned", "INTEGER DEFAULT 0"),
        ];

        for (col, col_type) in distilled_columns {
            if !existing_columns.contains(&col.to_string()) {
                self.conn
                    .execute_batch(&format!("ALTER TABLE notes ADD COLUMN {col} {col_type};"))?;
            }
        }

        // Doc 3 cold-note query filters on `modified_at`; ensure the index
        // exists on already-deployed DBs whose CREATE TABLE predates the
        // index addition.
        self.conn
            .execute_batch("CREATE INDEX IF NOT EXISTS idx_notes_modified_at ON notes(modified_at);")?;

        Ok(())
    }

    /// Ensure the FTS5 virtual table includes the `claims` column and that the
    /// trio of triggers populating it from `notes` is in place. Rebuilds the
    /// FTS5 table when the existing schema lacks `claims`.
    fn ensure_fts5_schema(&self) -> Result<()> {
        if !self.fts_has_claims_column()? {
            // Migration path. Triggers attach to the `notes` content table -
            // dropping the FTS table does NOT cascade to them, and a later
            // CREATE TRIGGER would fail with "already exists." Drop them
            // explicitly before recreating.
            self.conn.execute_batch(
                "DROP TRIGGER IF EXISTS notes_ai;
                 DROP TRIGGER IF EXISTS notes_ad;
                 DROP TRIGGER IF EXISTS notes_au;
                 DROP TABLE IF EXISTS notes_fts;
                 CREATE VIRTUAL TABLE notes_fts USING fts5(
                     title, body, tags, summary, claims,
                     content=notes, content_rowid=rowid
                 );
                 INSERT INTO notes_fts(notes_fts) VALUES('rebuild');",
            )?;
        }
        self.create_fts_triggers()?;
        Ok(())
    }

    fn fts_has_claims_column(&self) -> Result<bool> {
        let exists: bool = self
            .conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name='notes_fts'",
                [],
                |_| Ok::<_, rusqlite::Error>(()),
            )
            .is_ok();
        if !exists {
            return Ok(false);
        }

        let mut stmt = self.conn.prepare("PRAGMA table_info(notes_fts)")?;
        let columns: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(1))?
            .filter_map(|r| r.ok())
            .collect();
        Ok(columns.iter().any(|c| c == "claims"))
    }

    fn create_fts_triggers(&self) -> Result<()> {
        self.conn.execute_batch(
            "CREATE TRIGGER IF NOT EXISTS notes_ai AFTER INSERT ON notes BEGIN
                INSERT INTO notes_fts(rowid, title, body, tags, summary, claims)
                VALUES (new.rowid, new.title, new.body, new.tags, new.summary, new.claims);
            END;

            CREATE TRIGGER IF NOT EXISTS notes_ad AFTER DELETE ON notes BEGIN
                INSERT INTO notes_fts(notes_fts, rowid, title, body, tags, summary, claims)
                VALUES ('delete', old.rowid, old.title, old.body, old.tags, old.summary, old.claims);
            END;

            CREATE TRIGGER IF NOT EXISTS notes_au AFTER UPDATE ON notes BEGIN
                INSERT INTO notes_fts(notes_fts, rowid, title, body, tags, summary, claims)
                VALUES ('delete', old.rowid, old.title, old.body, old.tags, old.summary, old.claims);
                INSERT INTO notes_fts(rowid, title, body, tags, summary, claims)
                VALUES (new.rowid, new.title, new.body, new.tags, new.summary, new.claims);
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

            match self.index_one(note, mtime)? {
                IndexAction::Inserted => inserted += 1,
                IndexAction::Updated => updated += 1,
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

    /// Index a single note from its vault file. Single SQLite writer for the
    /// `notes` table: VaultWatcher mtime updates and full-walk reindex both
    /// flow through here. Existing rows are UPDATEd in place (vault-derived
    /// columns only; signal columns stay untouched); new rows are INSERTed
    /// with signal columns zeroed.
    pub fn index_one(&self, note: &Note, mtime: i64) -> Result<IndexAction> {
        let fm = &note.frontmatter;
        let path_str = note.path.to_string_lossy();
        log::debug!(
            "search::index_one: path={} mtime={} title={:?}",
            path_str,
            mtime,
            fm.title
        );

        let summary = parse_body_summary(&note.body).unwrap_or_else(|| detail::extract_summary(&note.body));
        let claims_flat = parse_body_claims(&note.body)
            .into_iter()
            .map(|c| c.text)
            .collect::<Vec<_>>()
            .join("\n");

        let tags_json = fm
            .tags
            .as_ref()
            .map(|t| serde_json::to_string(t).unwrap_or_default())
            .unwrap_or_default();

        let domain = normalize_enum::<Domain>(fm.domain.as_deref(), "domain", &path_str);
        let note_type = normalize_enum::<NoteType>(fm.note_type.as_deref(), "note_type", &path_str);
        let origin = normalize_enum::<Origin>(fm.origin.as_deref(), "origin", &path_str);
        let status = normalize_enum::<Status>(fm.status.as_deref(), "status", &path_str);

        let quality = extract_cortex_string(&fm.extra, "cortex-quality");
        let classified = extract_cortex_bool(&fm.extra, "cortex-classified");
        let classified_by = extract_cortex_string(&fm.extra, "cortex-classified-by");
        let confidence = extract_cortex_string(&fm.extra, "cortex-confidence");
        let needs_review = extract_cortex_bool(&fm.extra, "cortex-needs-review");
        let duplicate_group = extract_cortex_string(&fm.extra, "cortex-duplicate-group");

        let repo_stars = extract_cortex_optional_i64(&fm.extra, "cortex-repo-stars");
        let repo_last_commit = extract_cortex_optional_string(&fm.extra, "cortex-repo-last-commit");
        let repo_primary_language = extract_cortex_optional_string(&fm.extra, "cortex-repo-primary-language");
        let video_duration_seconds = extract_cortex_optional_i64(&fm.extra, "cortex-video-duration-seconds");
        let video_channel = extract_cortex_optional_string(&fm.extra, "cortex-video-channel");
        let video_published_at = extract_cortex_optional_string(&fm.extra, "cortex-video-published-at");
        let thread_platform = extract_cortex_optional_string(&fm.extra, "cortex-thread-platform");
        let thread_post_count = extract_cortex_optional_i64(&fm.extra, "cortex-thread-post-count");
        let thread_author = extract_cortex_optional_string(&fm.extra, "cortex-thread-author");

        let exists: bool = self
            .conn
            .query_row(
                "SELECT 1 FROM notes WHERE path = ?1",
                params![path_str.as_ref()],
                |_| Ok::<_, rusqlite::Error>(()),
            )
            .is_ok();

        let title = fm.title.as_deref().unwrap_or("");
        let date = fm.date.as_deref().unwrap_or("");
        let source = fm.source.as_deref().unwrap_or("");
        let creator = fm.creator.as_deref().unwrap_or("");

        // `pinned` is vault-derived: the user edits `pinned: true` in their
        // note's frontmatter. None or false -> 0; true -> 1. The flip-test
        // in `index_one_pinned_clears_when_frontmatter_drops_field` locks
        // the UPDATE path's responsibility for clearing the flag.
        let pinned_value: i64 = fm.pinned.unwrap_or(false) as i64;

        if exists {
            // UPDATE only vault-derived columns. Signal columns
            // (search_hit_count, last_accessed_at, inbound_link_count) are
            // intentionally excluded so reindex never clobbers Doc 3 state.
            // `pinned` IS vault-derived so it IS updated.
            self.conn.execute(
                "UPDATE notes SET
                    title = ?2, domain = ?3, note_type = ?4, origin = ?5, status = ?6,
                    date = ?7, tags = ?8, source = ?9, creator = ?10, body = ?11,
                    summary = ?12, claims = ?13, modified_at = ?14,
                    quality = ?15, classified = ?16, classified_by = ?17,
                    confidence = ?18, needs_review = ?19, duplicate_group = ?20,
                    cortex_repo_stars = ?21, cortex_repo_last_commit = ?22,
                    cortex_repo_primary_language = ?23,
                    cortex_video_duration_seconds = ?24, cortex_video_channel = ?25,
                    cortex_video_published_at = ?26,
                    cortex_thread_platform = ?27, cortex_thread_post_count = ?28,
                    cortex_thread_author = ?29,
                    pinned = ?30
                 WHERE path = ?1",
                params![
                    path_str.as_ref(),
                    title,
                    domain,
                    note_type,
                    origin,
                    status,
                    date,
                    tags_json,
                    source,
                    creator,
                    &note.body,
                    summary,
                    claims_flat,
                    mtime,
                    quality,
                    classified,
                    classified_by,
                    confidence,
                    needs_review,
                    duplicate_group,
                    repo_stars,
                    repo_last_commit,
                    repo_primary_language,
                    video_duration_seconds,
                    video_channel,
                    video_published_at,
                    thread_platform,
                    thread_post_count,
                    thread_author,
                    pinned_value,
                ],
            )?;
            Ok(IndexAction::Updated)
        } else {
            self.conn.execute(
                "INSERT INTO notes (
                    path, title, domain, note_type, origin, status, date, tags,
                    source, creator, body, summary, claims, modified_at,
                    quality, classified, classified_by, confidence, needs_review,
                    duplicate_group,
                    cortex_repo_stars, cortex_repo_last_commit,
                    cortex_repo_primary_language,
                    cortex_video_duration_seconds, cortex_video_channel,
                    cortex_video_published_at,
                    cortex_thread_platform, cortex_thread_post_count,
                    cortex_thread_author,
                    search_hit_count, last_accessed_at, inbound_link_count,
                    pinned
                ) VALUES (
                    ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                    ?15, ?16, ?17, ?18, ?19, ?20,
                    ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29,
                    0, NULL, 0,
                    ?30
                )",
                params![
                    path_str.as_ref(),
                    title,
                    domain,
                    note_type,
                    origin,
                    status,
                    date,
                    tags_json,
                    source,
                    creator,
                    &note.body,
                    summary,
                    claims_flat,
                    mtime,
                    quality,
                    classified,
                    classified_by,
                    confidence,
                    needs_review,
                    duplicate_group,
                    repo_stars,
                    repo_last_commit,
                    repo_primary_language,
                    video_duration_seconds,
                    video_channel,
                    video_published_at,
                    thread_platform,
                    thread_post_count,
                    thread_author,
                    pinned_value,
                ],
            )?;
            Ok(IndexAction::Inserted)
        }
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

    /// Read the Doc 3 signal triple for `path`: `(search_hit_count,
    /// last_accessed_at, inbound_link_count)`. Returns `None` if the path
    /// is not in the index. Used by callers that need to observe signal
    /// state without joining on the full row (e.g. tests, future
    /// signal-aware tooling).
    pub fn note_signals(&self, path: &str) -> Result<Option<(i64, Option<i64>, i64)>> {
        let row = self
            .conn
            .query_row(
                "SELECT search_hit_count, last_accessed_at, inbound_link_count
                 FROM notes WHERE path = ?1",
                params![path],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, Option<i64>>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                },
            )
            .ok();
        Ok(row)
    }

    /// Select notes that satisfy every cold-rule floor. A note that scores
    /// anywhere on any axis is excluded:
    ///
    /// - `search_hit_count = 0` AND `last_accessed_at IS NULL`: never read
    ///   via oracle.
    /// - `inbound_link_count = 0`: nothing else in the vault links here.
    /// - `pinned = 0`: not promoted.
    /// - `modified_at < older_than`: old enough.
    ///
    /// Ordered by `modified_at ASC` so the oldest cold notes surface first.
    pub fn cold_notes(&self, q: &ColdQuery) -> Result<Vec<ColdNote>> {
        log::debug!("cold_notes: older_than={} limit={}", q.older_than, q.limit);
        let mut stmt = self.conn.prepare(
            "SELECT path, title, domain, modified_at
             FROM notes
             WHERE search_hit_count = 0
               AND last_accessed_at IS NULL
               AND inbound_link_count = 0
               AND pinned = 0
               AND modified_at < ?1
             ORDER BY modified_at ASC
             LIMIT ?2",
        )?;
        let rows = stmt
            .query_map(params![q.older_than, q.limit as i64], |row| {
                Ok(ColdNote {
                    path: row.get(0)?,
                    title: row.get::<_, String>(1).unwrap_or_default(),
                    domain: row.get::<_, String>(2).unwrap_or_default(),
                    modified_at: row.get::<_, i64>(3).unwrap_or(0),
                })
            })?
            .filter_map(|r| r.ok())
            .collect();
        Ok(rows)
    }

    /// Count rows that would have qualified for the cold report except
    /// they are pinned. Surfaces visibility into how often the promotion
    /// floor rescues notes from the report.
    pub fn count_pinned_excluded(&self, older_than: i64) -> Result<u64> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM notes
             WHERE search_hit_count = 0
               AND last_accessed_at IS NULL
               AND inbound_link_count = 0
               AND pinned = 1
               AND modified_at < ?1",
            params![older_than],
            |row| row.get(0),
        )?;
        Ok(count as u64)
    }

    /// Total number of rows in the `notes` table; cheap to fetch
    /// alongside the cold report so callers can publish "scanned N"
    /// stats without a second prepare.
    pub fn count_notes(&self) -> Result<u64> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM notes", [], |row| row.get(0))?;
        Ok(count as u64)
    }

    /// Walk every note's body, count wikilink targets, materialize the
    /// `inbound_link_count` column for every row. Idempotent, bounded by
    /// vault size, single pass.
    ///
    /// **Key normalization is symmetric**: HashMap keys are
    /// `target.to_ascii_lowercase()` (taking the last `/`-segment so that
    /// `[[folder/note]]` matches a row whose path stem is `note`); the
    /// per-row lookup key is `file_stem(path).to_ascii_lowercase()`. Both
    /// sides are lowercased before the lookup, so any case parity is
    /// automatic. Anything that compares stems without lowercasing first
    /// is a bug.
    ///
    /// Self-links are NOT counted: a note whose body contains `[[self]]`
    /// gets no structural credit for it.
    ///
    /// **Sole intended caller: oracle's 10-minute periodic background
    /// task.** Must NOT be called from `index_vault` / the watcher path:
    /// the watcher fires sub-second on every Obsidian auto-save, and at
    /// three-year scale a full-table wikilink scan holding the SearchIndex
    /// mutex would block every concurrent `note_read` / `knowledge_search`.
    ///
    /// Returns the number of rows whose stored count changed.
    pub fn recompute_inbound_link_counts(&mut self) -> Result<usize> {
        log::debug!("recompute_inbound_link_counts: scanning vault");

        let rows: Vec<(String, String, i64)> = {
            let mut stmt = self.conn.prepare("SELECT path, body, inbound_link_count FROM notes")?;
            let mapped = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })?;
            mapped.filter_map(|r| r.ok()).collect()
        };

        let mut counts: HashMap<String, u64> = HashMap::new();
        for (path, body, _stored) in &rows {
            let source_stem = Path::new(path)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            for raw_target in extract_wikilinks(body) {
                // `[[folder/note]]` -> "note"; everything is lowercased so
                // the per-row lookup key matches symmetrically.
                let target_stem = raw_target.rsplit('/').next().unwrap_or("").to_ascii_lowercase();
                if target_stem.is_empty() {
                    continue;
                }
                if target_stem == source_stem {
                    // Self-link: no structural signal.
                    continue;
                }
                *counts.entry(target_stem).or_insert(0) += 1;
            }
        }

        let tx = self.conn.transaction()?;
        let mut changed: usize = 0;
        {
            let mut stmt = tx.prepare("UPDATE notes SET inbound_link_count = ?1 WHERE path = ?2")?;
            for (path, _body, stored) in &rows {
                let row_stem = Path::new(path)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_ascii_lowercase();
                let new_count = *counts.get(&row_stem).unwrap_or(&0) as i64;
                if new_count != *stored {
                    stmt.execute(params![new_count, path])?;
                    changed += 1;
                }
            }
        }
        tx.commit()?;

        log::debug!(
            "recompute_inbound_link_counts: scanned={} changed={}",
            rows.len(),
            changed
        );
        Ok(changed)
    }

    /// Increment `search_hit_count` and stamp `last_accessed_at = now` for `path`.
    ///
    /// **Sole intended caller: `oracle::note_read`.** Counting `knowledge_search`
    /// matches as access would create a positive-feedback loop where high-BM25-
    /// scoring notes become immortal and the entire decay premise collapses
    /// (parent roadmap, decay-signals section). The
    /// `knowledge_search_does_not_bump_access` oracle test is the load-bearing
    /// regression guard for this rule.
    ///
    /// Best-effort signal: a missing row (the note was deleted between read
    /// and bump) results in `rows_affected = 0` and `Ok(())`; not surfaced.
    pub fn bump_access(&self, path: &str) -> Result<()> {
        log::debug!("bump_access: path={path}");
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        let updated = self.conn.execute(
            "UPDATE notes
                SET search_hit_count = search_hit_count + 1,
                    last_accessed_at = ?2
              WHERE path = ?1",
            params![path, now],
        )?;
        if updated == 0 {
            log::trace!("bump_access: path={path} not present in index, ignored");
        }
        Ok(())
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

    /// Coverage of the `note_embeddings` table relative to `notes`. Used by
    /// `sb status` / `sb doctor` to surface how many notes have been embedded.
    pub fn embedding_coverage(&self) -> Result<EmbeddingCoverage> {
        let total_notes: u64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM notes", [], |row| row.get(0))?;
        let embedded_notes: u64 =
            self.conn
                .query_row("SELECT COUNT(DISTINCT note_path) FROM note_embeddings", [], |row| {
                    row.get(0)
                })?;
        Ok(EmbeddingCoverage {
            total_notes,
            embedded_notes,
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

        stats.sort_by_key(|b| std::cmp::Reverse(b.count));
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
        result.sort_by_key(|b| std::cmp::Reverse(b.1));
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
        result.sort_by_key(|b| std::cmp::Reverse(b.1));
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

    // --- Governance & Health Methods ---

    /// Get notes currently in the inbox
    pub fn inbox_notes(&self, limit: Option<u32>) -> Result<Vec<NoteRow>> {
        let limit = limit.unwrap_or(50);
        let mut stmt = self.conn.prepare(&format!(
            "SELECT path, title, domain, note_type, origin, status, date, tags, source, creator, body, summary
                 FROM notes WHERE path LIKE 'inbox/%' ORDER BY date DESC LIMIT {limit}"
        ))?;
        let rows = stmt.query_map([], NoteRow::from_row)?.filter_map(|r| r.ok()).collect();
        Ok(rows)
    }

    /// Get notes that need review (cortex-needs-review = true)
    pub fn notes_needing_review(&self, limit: Option<u32>) -> Result<Vec<NoteRow>> {
        let limit = limit.unwrap_or(50);
        let mut stmt = self.conn.prepare(&format!(
            "SELECT path, title, domain, note_type, origin, status, date, tags, source, creator, body, summary
                 FROM notes WHERE needs_review = 1 ORDER BY date DESC LIMIT {limit}"
        ))?;
        let rows = stmt.query_map([], NoteRow::from_row)?.filter_map(|r| r.ok()).collect();
        Ok(rows)
    }

    /// Get quality score distribution and notes filtered by quality level
    pub fn quality_distribution(&self) -> Result<Vec<(String, u64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT quality, COUNT(*) as cnt FROM notes WHERE quality != '' GROUP BY quality ORDER BY cnt DESC",
        )?;
        let rows = stmt
            .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?)))?
            .filter_map(|r| r.ok())
            .collect();
        Ok(rows)
    }

    /// Get notes at a specific quality level
    pub fn notes_by_quality(&self, quality: &str, limit: Option<u32>) -> Result<Vec<NoteRow>> {
        let limit = limit.unwrap_or(20);
        let mut stmt = self.conn.prepare(&format!(
            "SELECT path, title, domain, note_type, origin, status, date, tags, source, creator, body, summary
                 FROM notes WHERE LOWER(quality) = ?1 ORDER BY date DESC LIMIT {limit}"
        ))?;
        let rows = stmt
            .query_map(params![quality.to_lowercase()], NoteRow::from_row)?
            .filter_map(|r| r.ok())
            .collect();
        Ok(rows)
    }

    /// Get duplicate note groups
    pub fn duplicate_groups(&self) -> Result<Vec<DuplicateGroup>> {
        let mut stmt = self.conn.prepare(
            "SELECT duplicate_group, path, title FROM notes WHERE duplicate_group != '' ORDER BY duplicate_group, path",
        )?;

        let mut groups: HashMap<String, Vec<(String, String)>> = HashMap::new();
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;

        for row in rows.flatten() {
            let (group_id, path, title) = row;
            groups.entry(group_id).or_default().push((path, title));
        }

        let mut result: Vec<DuplicateGroup> = groups
            .into_iter()
            .filter(|(_, notes)| notes.len() > 1)
            .map(|(group_id, notes)| DuplicateGroup {
                group_id,
                note_count: notes.len() as u64,
                notes: notes
                    .into_iter()
                    .map(|(path, title)| DuplicateNote { path, title })
                    .collect(),
            })
            .collect();

        result.sort_by_key(|b| std::cmp::Reverse(b.note_count));
        Ok(result)
    }

    /// Get classification pipeline statistics
    pub fn classify_stats(&self, domain: Option<&str>) -> Result<ClassifyStats> {
        let domain_filter = domain.map(|d| format!(" AND domain = '{d}'")).unwrap_or_default();

        let total_classified: u64 = self.conn.query_row(
            &format!("SELECT COUNT(*) FROM notes WHERE classified = 1{domain_filter}"),
            [],
            |row| row.get(0),
        )?;

        let by_method = {
            let mut stmt = self.conn.prepare(&format!(
                "SELECT classified_by, COUNT(*) FROM notes WHERE classified = 1 AND classified_by != ''{domain_filter} GROUP BY classified_by ORDER BY COUNT(*) DESC"
            ))?;
            stmt.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?)))?
                .filter_map(|r| r.ok())
                .collect()
        };

        let by_confidence = {
            let mut stmt = self.conn.prepare(&format!(
                "SELECT confidence, COUNT(*) FROM notes WHERE classified = 1 AND confidence != ''{domain_filter} GROUP BY confidence ORDER BY COUNT(*) DESC"
            ))?;
            stmt.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?)))?
                .filter_map(|r| r.ok())
                .collect()
        };

        let by_domain = {
            let mut stmt = self.conn.prepare(
                "SELECT domain, COUNT(*) FROM notes WHERE classified = 1 AND domain != '' GROUP BY domain ORDER BY COUNT(*) DESC",
            )?;
            stmt.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?)))?
                .filter_map(|r| r.ok())
                .collect()
        };

        let pending_review: u64 = self.conn.query_row(
            &format!("SELECT COUNT(*) FROM notes WHERE needs_review = 1{domain_filter}"),
            [],
            |row| row.get(0),
        )?;

        let inbox_count: u64 =
            self.conn
                .query_row("SELECT COUNT(*) FROM notes WHERE path LIKE 'inbox/%'", [], |row| {
                    row.get(0)
                })?;

        let unclassified: u64 = self.conn.query_row(
            "SELECT COUNT(*) FROM notes WHERE domain = '' AND note_type NOT IN ('daily', 'system')",
            [],
            |row| row.get(0),
        )?;

        Ok(ClassifyStats {
            total_classified,
            by_method,
            by_confidence,
            by_domain,
            pending_review,
            inbox_count,
            unclassified,
        })
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
    terms.sort_by_key(|b| std::cmp::Reverse(b.1));

    terms.into_iter().take(max_terms).map(|(word, _)| word).collect()
}

/// Extract wikilink targets from note body, skipping fenced code blocks.
/// Handles [[simple]], [[with|alias]], [[with#heading]], [[path/to/note]].
///
/// `pub` so the cortex graph pass can derive `wikilink` edges from the same
/// parser oracle's link tools use (single source of wikilink-extraction
/// truth).
pub fn extract_wikilinks(body: &str) -> Vec<String> {
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

/// Embedding coverage snapshot: total notes in the index vs how many have at least one embedding row.
#[derive(Debug, Serialize)]
pub struct EmbeddingCoverage {
    pub total_notes: u64,
    pub embedded_notes: u64,
}

impl EmbeddingCoverage {
    pub fn percent(&self) -> f64 {
        if self.total_notes == 0 {
            0.0
        } else {
            (self.embedded_notes as f64 / self.total_notes as f64) * 100.0
        }
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

/// A group of duplicate notes
#[derive(Debug, Serialize)]
pub struct DuplicateGroup {
    pub group_id: String,
    pub note_count: u64,
    pub notes: Vec<DuplicateNote>,
}

/// A note within a duplicate group
#[derive(Debug, Serialize)]
pub struct DuplicateNote {
    pub path: String,
    pub title: String,
}

/// Classification pipeline statistics
#[derive(Debug, Serialize)]
pub struct ClassifyStats {
    pub total_classified: u64,
    pub by_method: Vec<(String, u64)>,
    pub by_confidence: Vec<(String, u64)>,
    pub by_domain: Vec<(String, u64)>,
    pub pending_review: u64,
    pub inbox_count: u64,
    pub unclassified: u64,
}

/// Parameters for `SearchIndex::cold_notes`. `older_than` is a Unix-seconds
/// boundary; the cold query keeps everything in integer seconds so the
/// `idx_notes_modified_at` index stays usable.
#[derive(Debug, Clone, Copy)]
pub struct ColdQuery {
    pub older_than: i64,
    pub limit: u32,
}

/// Subset of `notes` returned by `cold_notes` - just the fields the
/// review report needs. Saves a row body load.
#[derive(Debug, Clone, Serialize)]
pub struct ColdNote {
    pub path: String,
    pub title: String,
    pub domain: String,
    pub modified_at: i64,
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

    #[test]
    fn test_inbox_notes() {
        let index = SearchIndex::open_memory().expect("open");
        insert_test_note(&index, "inbox/a.md", "Inbox A", "tech", &[], "body");
        insert_test_note(&index, "inbox/b.md", "Inbox B", "", &[], "body");
        insert_test_note(&index, "notes/c.md", "Not inbox", "tech", &[], "body");

        let inbox = index.inbox_notes(None).expect("inbox");
        assert_eq!(inbox.len(), 2);
    }

    #[test]
    fn test_quality_distribution() {
        let index = SearchIndex::open_memory().expect("open");
        index
            .conn
            .execute(
                "INSERT INTO notes (path, title, domain, note_type, origin, status, date, tags, source, creator, body, summary, modified_at, quality)
                 VALUES ('a.md', 'A', 'tech', 'article', '', '', '2026-03-21', '[]', '', '', '', '', 0, 'high')",
                [],
            )
            .expect("insert");
        index
            .conn
            .execute(
                "INSERT INTO notes (path, title, domain, note_type, origin, status, date, tags, source, creator, body, summary, modified_at, quality)
                 VALUES ('b.md', 'B', 'tech', 'article', '', '', '2026-03-21', '[]', '', '', '', '', 0, 'low')",
                [],
            )
            .expect("insert");
        index
            .conn
            .execute(
                "INSERT INTO notes (path, title, domain, note_type, origin, status, date, tags, source, creator, body, summary, modified_at, quality)
                 VALUES ('c.md', 'C', 'tech', 'article', '', '', '2026-03-21', '[]', '', '', '', '', 0, 'high')",
                [],
            )
            .expect("insert");

        let dist = index.quality_distribution().expect("distribution");
        assert_eq!(dist.len(), 2);
        assert!(dist.iter().any(|(q, c)| q == "high" && *c == 2));
        assert!(dist.iter().any(|(q, c)| q == "low" && *c == 1));
    }

    #[test]
    fn test_classify_stats() {
        let index = SearchIndex::open_memory().expect("open");
        index
            .conn
            .execute(
                "INSERT INTO notes (path, title, domain, note_type, origin, status, date, tags, source, creator, body, summary, modified_at, classified, classified_by, confidence, needs_review)
                 VALUES ('notes/a.md', 'A', 'tech', 'article', '', '', '2026-03-21', '[]', '', '', '', '', 0, 1, 'deterministic', 'high', 0)",
                [],
            )
            .expect("insert");
        index
            .conn
            .execute(
                "INSERT INTO notes (path, title, domain, note_type, origin, status, date, tags, source, creator, body, summary, modified_at, classified, classified_by, confidence, needs_review)
                 VALUES ('inbox/b.md', 'B', '', 'article', '', '', '2026-03-21', '[]', '', '', '', '', 0, 0, '', '', 1)",
                [],
            )
            .expect("insert");

        let stats = index.classify_stats(None).expect("classify_stats");
        assert_eq!(stats.total_classified, 1);
        assert_eq!(stats.pending_review, 1);
        assert_eq!(stats.inbox_count, 1);
        assert_eq!(stats.unclassified, 1);
    }

    #[test]
    fn test_duplicate_groups() {
        let index = SearchIndex::open_memory().expect("open");
        index
            .conn
            .execute(
                "INSERT INTO notes (path, title, domain, note_type, origin, status, date, tags, source, creator, body, summary, modified_at, duplicate_group)
                 VALUES ('a.md', 'Article A', 'tech', 'article', '', '', '2026-03-21', '[]', '', '', '', '', 0, 'group-1')",
                [],
            )
            .expect("insert");
        index
            .conn
            .execute(
                "INSERT INTO notes (path, title, domain, note_type, origin, status, date, tags, source, creator, body, summary, modified_at, duplicate_group)
                 VALUES ('b.md', 'Article A Copy', 'tech', 'article', '', '', '2026-03-21', '[]', '', '', '', '', 0, 'group-1')",
                [],
            )
            .expect("insert");
        index
            .conn
            .execute(
                "INSERT INTO notes (path, title, domain, note_type, origin, status, date, tags, source, creator, body, summary, modified_at, duplicate_group)
                 VALUES ('c.md', 'Solo', 'tech', 'article', '', '', '2026-03-21', '[]', '', '', '', '', 0, 'group-solo')",
                [],
            )
            .expect("insert");

        let groups = index.duplicate_groups().expect("duplicate_groups");
        // Only group-1 has more than 1 note
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].group_id, "group-1");
        assert_eq!(groups[0].note_count, 2);
    }

    // -------- Body section parsers (distilled contract) --------

    #[test]
    fn parse_body_summary_extracts_section() {
        let body = "# Title\n\n## Summary\n\nA two-sentence summary. With a follow-up.\n\n## Claims\n- one\n";
        let summary = parse_body_summary(body).expect("summary present");
        assert_eq!(summary, "A two-sentence summary. With a follow-up.");
    }

    #[test]
    fn parse_body_summary_returns_none_when_section_missing() {
        let body = "# Title\n\n## Notes\n\nNo summary here.\n";
        assert!(parse_body_summary(body).is_none());
    }

    #[test]
    fn parse_body_summary_returns_none_when_section_empty() {
        let body = "# Title\n\n## Summary\n\n## Claims\n";
        assert!(parse_body_summary(body).is_none());
    }

    #[test]
    fn parse_body_summary_handles_trailing_section() {
        let body = "# Title\n\n## Summary\n\nLast section in the document.\n";
        let summary = parse_body_summary(body).expect("summary present");
        assert_eq!(summary, "Last section in the document.");
    }

    #[test]
    fn parse_body_claims_extracts_bullets_and_anchors() {
        let body = "## Summary\n\nx\n\n## Claims\n- First claim. [12:34]\n- Second claim with no anchor\n- Third claim. [section-three]\n\n## Links\n";
        let claims = parse_body_claims(body);
        assert_eq!(claims.len(), 3);
        assert_eq!(claims[0].text, "First claim.");
        assert_eq!(claims[0].anchor.as_deref(), Some("12:34"));
        assert_eq!(claims[1].text, "Second claim with no anchor");
        assert!(claims[1].anchor.is_none());
        assert_eq!(claims[2].text, "Third claim.");
        assert_eq!(claims[2].anchor.as_deref(), Some("section-three"));
    }

    #[test]
    fn parse_body_claims_returns_empty_when_section_missing() {
        let body = "# Title\n\nNo claims section.\n";
        assert!(parse_body_claims(body).is_empty());
    }

    #[test]
    fn parse_body_claims_ignores_similar_headings() {
        // A user heading like "## My Notes" must NOT be parsed as claims.
        let body = "## My Notes\n- looks like a claim but isn't\n\n## Claims\n- real claim\n";
        let claims = parse_body_claims(body);
        assert_eq!(claims.len(), 1);
        assert_eq!(claims[0].text, "real claim");
    }

    #[test]
    fn parse_body_claims_skips_blank_bullets() {
        let body = "## Claims\n- \n- A real claim\n-\n";
        let claims = parse_body_claims(body);
        assert_eq!(claims.len(), 1);
        assert_eq!(claims[0].text, "A real claim");
    }

    #[test]
    fn parse_body_claims_does_not_extract_anchor_when_brackets_are_inline() {
        // Brackets in the middle of the text stay in the text. Only a [...]
        // group at the very end of the line is an anchor.
        let body = "## Claims\n- See [docs] for context.\n";
        let claims = parse_body_claims(body);
        assert_eq!(claims.len(), 1);
        assert_eq!(claims[0].text, "See [docs] for context.");
        assert!(claims[0].anchor.is_none());
    }

    // -------- index_one signal-column preservation --------

    fn make_test_note(path: &str, body: &str) -> Note {
        use crate::frontmatter::Frontmatter;
        use std::path::PathBuf;
        let fm = Frontmatter {
            title: Some(format!("title for {path}")),
            note_type: Some("article".to_string()),
            origin: Some("assisted".to_string()),
            tags: Some(vec!["rust".to_string()]),
            ..Frontmatter::default()
        };
        Note {
            path: PathBuf::from(path),
            frontmatter: fm,
            body: body.to_string(),
            raw: format!("---\n---\n{body}"),
        }
    }

    fn signal_row(index: &SearchIndex, path: &str) -> (i64, Option<i64>, i64) {
        index
            .conn
            .query_row(
                "SELECT search_hit_count, last_accessed_at, inbound_link_count
                 FROM notes WHERE path = ?1",
                params![path],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, Option<i64>>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                },
            )
            .expect("signal row")
    }

    #[test]
    fn index_one_insert_zeroes_signal_columns() {
        let index = SearchIndex::open_memory().expect("open");
        let note = make_test_note("inbox/new.md", "# T\n\n## Summary\n\nHello.\n");
        let action = index.index_one(&note, 100).expect("index_one");
        assert_eq!(action, IndexAction::Inserted);

        let (hits, last, inbound) = signal_row(&index, "inbox/new.md");
        assert_eq!(hits, 0);
        assert!(last.is_none());
        assert_eq!(inbound, 0);
    }

    #[test]
    fn index_one_update_preserves_signal_columns() {
        let index = SearchIndex::open_memory().expect("open");
        let note = make_test_note("inbox/keep.md", "# T\n\n## Summary\n\nFirst pass.\n");
        index.index_one(&note, 100).expect("first index");

        // Pretend Doc 3 (or anyone) wrote signal values out-of-band.
        index
            .conn
            .execute(
                "UPDATE notes SET search_hit_count = ?1, last_accessed_at = ?2,
                                  inbound_link_count = ?3
                 WHERE path = ?4",
                params![17_i64, 999_999_i64, 3_i64, "inbox/keep.md"],
            )
            .expect("seed signals");

        // Reindex with new content + new mtime.
        let updated = make_test_note("inbox/keep.md", "# T\n\n## Summary\n\nRevised body.\n");
        let action = index.index_one(&updated, 200).expect("reindex");
        assert_eq!(action, IndexAction::Updated);

        let (hits, last, inbound) = signal_row(&index, "inbox/keep.md");
        assert_eq!(hits, 17, "search_hit_count must survive reindex");
        assert_eq!(last, Some(999_999), "last_accessed_at must survive reindex");
        assert_eq!(inbound, 3, "inbound_link_count must survive reindex");

        // And the vault-derived columns DID get updated.
        let summary: String = index
            .conn
            .query_row("SELECT summary FROM notes WHERE path = 'inbox/keep.md'", [], |row| {
                row.get(0)
            })
            .expect("summary");
        assert_eq!(summary, "Revised body.");
    }

    #[test]
    fn bump_access_increments_and_stamps_timestamp() {
        let index = SearchIndex::open_memory().expect("open");
        let note = make_test_note("notes/bump.md", "# T\n\n## Summary\n\nBody.\n");
        index.index_one(&note, 100).expect("index_one");

        let before = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("now")
            .as_secs() as i64;

        index.bump_access("notes/bump.md").expect("first bump");
        index.bump_access("notes/bump.md").expect("second bump");

        let (hits, last, _inbound) = signal_row(&index, "notes/bump.md");
        assert_eq!(hits, 2, "two bumps -> count 2");
        let last = last.expect("last_accessed_at set after bump");
        assert!(last >= before, "stamp should be at-or-after before-bump time");
    }

    #[test]
    fn recompute_inbound_link_counts_basic() {
        let mut index = SearchIndex::open_memory().expect("open");
        index
            .index_one(&make_test_note("notes/a.md", "Refer to [[b]]."), 100)
            .expect("a");
        index
            .index_one(&make_test_note("notes/b.md", "## Summary\n\nB.\n"), 100)
            .expect("b");
        index
            .index_one(&make_test_note("notes/c.md", "See [[b]] and [[a]]."), 100)
            .expect("c");

        let changed = index.recompute_inbound_link_counts().expect("recompute");
        assert!(changed >= 2, "at least a and b should have changed: {changed}");

        assert_eq!(signal_row(&index, "notes/a.md").2, 1, "a is linked from c");
        assert_eq!(signal_row(&index, "notes/b.md").2, 2, "b is linked from a and c");
        assert_eq!(signal_row(&index, "notes/c.md").2, 0, "c has no inbound");
    }

    #[test]
    fn recompute_inbound_link_counts_handles_link_removal() {
        let mut index = SearchIndex::open_memory().expect("open");
        index
            .index_one(&make_test_note("notes/a.md", "Link to [[b]]."), 100)
            .expect("a");
        index
            .index_one(&make_test_note("notes/b.md", "## Summary\n\nB.\n"), 100)
            .expect("b");

        index.recompute_inbound_link_counts().expect("first");
        assert_eq!(signal_row(&index, "notes/b.md").2, 1);

        // Edit A to drop the link to B.
        index
            .index_one(&make_test_note("notes/a.md", "No link here."), 200)
            .expect("re-a");
        index.recompute_inbound_link_counts().expect("second");
        assert_eq!(signal_row(&index, "notes/b.md").2, 0, "removed link drops to 0");
    }

    #[test]
    fn recompute_inbound_link_counts_excludes_self_links() {
        let mut index = SearchIndex::open_memory().expect("open");
        index
            .index_one(&make_test_note("notes/a.md", "I reference [[a]] - myself."), 100)
            .expect("a");

        index.recompute_inbound_link_counts().expect("recompute");
        assert_eq!(
            signal_row(&index, "notes/a.md").2,
            0,
            "self-link must not bump the note's own inbound count",
        );
    }

    #[test]
    fn recompute_inbound_link_counts_is_case_insensitive() {
        let mut index = SearchIndex::open_memory().expect("open");
        // Target file uses lowercase-hyphenated stem; source spells it
        // with mixed case in the wikilink. Should still match.
        index
            .index_one(&make_test_note("notes/some-note.md", "## Summary\n\nT.\n"), 100)
            .expect("target");
        index
            .index_one(&make_test_note("notes/a.md", "See [[Some-Note]] for more."), 100)
            .expect("source");

        index.recompute_inbound_link_counts().expect("recompute");
        assert_eq!(
            signal_row(&index, "notes/some-note.md").2,
            1,
            "case-insensitive stem match",
        );
    }

    #[test]
    fn recompute_inbound_link_counts_strips_path_prefix() {
        // `[[folder/note]]` should match a row whose stem is `note`.
        let mut index = SearchIndex::open_memory().expect("open");
        index
            .index_one(&make_test_note("notes/target.md", "## Summary\n\nT.\n"), 100)
            .expect("target");
        index
            .index_one(
                &make_test_note("notes/source.md", "Link [[notes/target]] with path."),
                100,
            )
            .expect("source");

        index.recompute_inbound_link_counts().expect("recompute");
        assert_eq!(
            signal_row(&index, "notes/target.md").2,
            1,
            "[[folder/note]] should match the stem",
        );
    }

    fn pinned_value(index: &SearchIndex, path: &str) -> i64 {
        index
            .conn
            .query_row("SELECT pinned FROM notes WHERE path = ?1", params![path], |row| {
                row.get::<_, i64>(0)
            })
            .expect("pinned row")
    }

    fn make_pinned_note(path: &str, pinned: Option<bool>, body: &str) -> Note {
        use crate::frontmatter::Frontmatter;
        use std::path::PathBuf;
        let fm = Frontmatter {
            title: Some(format!("title for {path}")),
            note_type: Some("article".to_string()),
            origin: Some("assisted".to_string()),
            pinned,
            ..Frontmatter::default()
        };
        Note {
            path: PathBuf::from(path),
            frontmatter: fm,
            body: body.to_string(),
            raw: format!("---\n---\n{body}"),
        }
    }

    #[test]
    fn index_one_persists_pinned_true() {
        let index = SearchIndex::open_memory().expect("open");
        let note = make_pinned_note("notes/pinned.md", Some(true), "## Summary\n\nP.\n");
        index.index_one(&note, 100).expect("index");
        assert_eq!(pinned_value(&index, "notes/pinned.md"), 1);
    }

    #[test]
    fn index_one_pinned_defaults_to_zero() {
        let index = SearchIndex::open_memory().expect("open");
        let note = make_pinned_note("notes/unpinned.md", None, "## Summary\n\nU.\n");
        index.index_one(&note, 100).expect("index");
        assert_eq!(pinned_value(&index, "notes/unpinned.md"), 0);
    }

    #[test]
    fn index_one_pinned_survives_reindex_without_frontmatter_change() {
        let index = SearchIndex::open_memory().expect("open");
        let note = make_pinned_note("notes/keep.md", Some(true), "## Summary\n\nFirst.\n");
        index.index_one(&note, 100).expect("first");
        assert_eq!(pinned_value(&index, "notes/keep.md"), 1);

        // Reindex with new body but same pinned: true.
        let updated = make_pinned_note("notes/keep.md", Some(true), "## Summary\n\nSecond.\n");
        index.index_one(&updated, 200).expect("reindex");
        assert_eq!(
            pinned_value(&index, "notes/keep.md"),
            1,
            "pinned survives content reindex when frontmatter unchanged",
        );
    }

    #[test]
    fn index_one_pinned_clears_when_frontmatter_drops_field() {
        let index = SearchIndex::open_memory().expect("open");
        let note = make_pinned_note("notes/flip.md", Some(true), "## Summary\n\nP.\n");
        index.index_one(&note, 100).expect("first");
        assert_eq!(pinned_value(&index, "notes/flip.md"), 1);

        // User removes pinned: true from frontmatter; reindex.
        let updated = make_pinned_note("notes/flip.md", None, "## Summary\n\nU.\n");
        index.index_one(&updated, 200).expect("reindex");
        assert_eq!(
            pinned_value(&index, "notes/flip.md"),
            0,
            "removing pinned: true must clear the column on reindex",
        );
    }

    fn cold_query(older_than: i64) -> ColdQuery {
        ColdQuery { older_than, limit: 100 }
    }

    #[test]
    fn cold_notes_returns_only_floor_satisfying_rows() {
        let index = SearchIndex::open_memory().expect("open");
        // Cold candidate: zero signals, old, not pinned.
        index
            .index_one(&make_test_note("notes/cold.md", "## Summary\n\nC.\n"), 1_000)
            .expect("cold");
        // Recent: shouldn't surface.
        index
            .index_one(&make_test_note("notes/recent.md", "## Summary\n\nR.\n"), 9_000)
            .expect("recent");
        // Pinned: shouldn't surface.
        index
            .index_one(
                &make_pinned_note("notes/pin.md", Some(true), "## Summary\n\nP.\n"),
                1_000,
            )
            .expect("pin");
        // Has inbound (seed signal directly to avoid running recompute):
        index
            .index_one(&make_test_note("notes/linked.md", "## Summary\n\nL.\n"), 1_000)
            .expect("linked");
        index
            .conn
            .execute(
                "UPDATE notes SET inbound_link_count = 2 WHERE path = 'notes/linked.md'",
                [],
            )
            .expect("seed inbound");

        let rows = index.cold_notes(&cold_query(5_000)).expect("cold");
        let paths: Vec<&str> = rows.iter().map(|r| r.path.as_str()).collect();
        assert_eq!(paths, vec!["notes/cold.md"]);
    }

    #[test]
    fn cold_notes_orders_oldest_first() {
        let index = SearchIndex::open_memory().expect("open");
        index
            .index_one(&make_test_note("notes/middle.md", "## Summary\n\nM.\n"), 3_000)
            .expect("m");
        index
            .index_one(&make_test_note("notes/oldest.md", "## Summary\n\nO.\n"), 1_000)
            .expect("o");
        index
            .index_one(&make_test_note("notes/newer.md", "## Summary\n\nN.\n"), 4_000)
            .expect("n");

        let rows = index.cold_notes(&cold_query(5_000)).expect("cold");
        let paths: Vec<&str> = rows.iter().map(|r| r.path.as_str()).collect();
        assert_eq!(paths, vec!["notes/oldest.md", "notes/middle.md", "notes/newer.md"]);
    }

    #[test]
    fn cold_notes_excludes_once_read_notes() {
        let index = SearchIndex::open_memory().expect("open");
        index
            .index_one(&make_test_note("notes/once.md", "## Summary\n\nO.\n"), 1_000)
            .expect("once");
        // One bump suffices to mark the note permanently warm under the
        // binary decay rule.
        index.bump_access("notes/once.md").expect("bump");

        let rows = index.cold_notes(&cold_query(5_000)).expect("cold");
        assert!(rows.is_empty(), "any prior read disqualifies: got {rows:?}");
    }

    #[test]
    fn count_pinned_excluded_counts_only_pinned_floor_satisfiers() {
        let index = SearchIndex::open_memory().expect("open");
        index
            .index_one(&make_pinned_note("notes/p.md", Some(true), "## Summary\n\nP.\n"), 1_000)
            .expect("pinned");
        // Recent pinned: would not have qualified for the cold report
        // because it's not old enough, so should NOT count toward
        // pinned_excluded.
        index
            .index_one(
                &make_pinned_note("notes/p-recent.md", Some(true), "## Summary\n\nN.\n"),
                9_000,
            )
            .expect("pinned-recent");
        index
            .index_one(&make_test_note("notes/not-pinned.md", "## Summary\n\nU.\n"), 1_000)
            .expect("unpinned");

        let count = index.count_pinned_excluded(5_000).expect("count");
        assert_eq!(count, 1, "only old, otherwise-cold, pinned rows count");
    }

    #[test]
    fn bump_access_is_noop_on_missing_path() {
        // The note may have been deleted between knowledge_search and note_read;
        // the bump is best-effort and must not error.
        let index = SearchIndex::open_memory().expect("open");
        let result = index.bump_access("nonexistent/path.md");
        assert!(result.is_ok(), "missing path is silent: {result:?}");
    }

    #[test]
    fn index_one_persists_distilled_metadata_from_frontmatter() {
        use crate::frontmatter::Frontmatter;
        use std::path::PathBuf;
        let mut extra = HashMap::new();
        extra.insert("cortex-repo-stars".to_string(), serde_yaml::Value::Number(1432.into()));
        extra.insert(
            "cortex-repo-primary-language".to_string(),
            serde_yaml::Value::String("Rust".to_string()),
        );
        let fm = Frontmatter {
            title: Some("Repo Note".to_string()),
            note_type: Some("article".to_string()),
            origin: Some("assisted".to_string()),
            extra,
            ..Frontmatter::default()
        };

        let note = Note {
            path: PathBuf::from("notes/repo.md"),
            frontmatter: fm,
            body: "## Summary\n\nA repo.\n\n## Claims\n- It builds.\n".to_string(),
            raw: String::new(),
        };

        let index = SearchIndex::open_memory().expect("open");
        index.index_one(&note, 100).expect("index_one");

        let (stars, lang): (Option<i64>, Option<String>) = index
            .conn
            .query_row(
                "SELECT cortex_repo_stars, cortex_repo_primary_language
                 FROM notes WHERE path = 'notes/repo.md'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("row");
        assert_eq!(stars, Some(1432));
        assert_eq!(lang.as_deref(), Some("Rust"));

        // Claims column should hold the flattened claim text.
        let claims_flat: String = index
            .conn
            .query_row("SELECT claims FROM notes WHERE path = 'notes/repo.md'", [], |row| {
                row.get(0)
            })
            .expect("claims");
        assert_eq!(claims_flat, "It builds.");
    }

    #[test]
    fn fts5_schema_migrates_from_old_schema() {
        // Build a connection at the OLD schema, then run ensure_schema and
        // verify the claims column is in place and FTS5 search over claims works.
        let conn = Connection::open_in_memory().expect("open");
        conn.execute_batch(
            "CREATE TABLE notes (
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
            CREATE VIRTUAL TABLE notes_fts USING fts5(
                title, body, tags, summary,
                content=notes, content_rowid=rowid
            );
            CREATE TRIGGER notes_ai AFTER INSERT ON notes BEGIN
                INSERT INTO notes_fts(rowid, title, body, tags, summary)
                VALUES (new.rowid, new.title, new.body, new.tags, new.summary);
            END;
            INSERT INTO notes (path, title, domain, note_type, origin, status, date, tags, source, creator, body, summary, modified_at)
            VALUES ('notes/legacy.md', 'Legacy', 'tech', 'article', 'assisted', '', '2026-03-21', '[]', '', '', 'legacy body', 'legacy summary', 0);",
        )
        .expect("seed old schema");

        let index = SearchIndex { conn };
        index.ensure_schema().expect("migrate");

        // The new columns must exist on notes.
        let mut stmt = index.conn.prepare("PRAGMA table_info(notes)").expect("table_info");
        let cols: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(1))
            .expect("query")
            .filter_map(|r| r.ok())
            .collect();
        assert!(cols.iter().any(|c| c == "claims"), "claims col missing");
        assert!(
            cols.iter().any(|c| c == "cortex_repo_stars"),
            "cortex_repo_stars col missing"
        );
        assert!(
            cols.iter().any(|c| c == "search_hit_count"),
            "search_hit_count col missing"
        );

        // FTS5 must carry claims now.
        assert!(index.fts_has_claims_column().expect("fts cols"));

        // Existing data should survive the migration.
        let title: String = index
            .conn
            .query_row("SELECT title FROM notes WHERE path = 'notes/legacy.md'", [], |row| {
                row.get(0)
            })
            .expect("legacy row");
        assert_eq!(title, "Legacy");

        // FTS5 was rebuilt: the legacy body should still be searchable.
        let hits = index.search("legacy", None, None, None, None).expect("search");
        assert!(
            hits.iter().any(|n| n.path == "notes/legacy.md"),
            "post-migration FTS5 must still surface legacy rows: got {hits:?}"
        );
    }

    #[test]
    fn fts5_search_hits_claims_column() {
        let index = SearchIndex::open_memory().expect("open");
        let note = make_test_note(
            "notes/distinctclaim.md",
            "# T\n\n## Summary\n\nSome summary.\n\n## Claims\n- xenomorphism is the unique signal.\n",
        );
        index.index_one(&note, 100).expect("index_one");

        // FTS5 query for the claim-only term should find this note.
        let hits = index.search("xenomorphism", None, None, None, None).expect("search");
        assert!(
            hits.iter().any(|n| n.path == "notes/distinctclaim.md"),
            "expected FTS5 to index claims column; got {hits:?}"
        );
    }

    // --- Phase A1: vec feature schema ------------------------------------

    /// Test-local encoder. The production encoder/decoder land in Phase A3
    /// alongside `search_vector` which calls them on every row.
    #[cfg(feature = "vec")]
    fn encode_le_f32(vector: &[f32]) -> Vec<u8> {
        let mut out = Vec::with_capacity(vector.len() * 4);
        for v in vector {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out
    }

    /// Test-local validator. Phase A3's `search_vector` will run the same
    /// check (length == dim * 4) before its inner dot-product loop.
    #[cfg(feature = "vec")]
    fn validate_le_f32_len(bytes: &[u8], dim: usize) -> eyre::Result<()> {
        if bytes.len() != dim * 4 {
            eyre::bail!(
                "embedding BLOB length mismatch: got {} bytes, expected dim={} ({} bytes)",
                bytes.len(),
                dim,
                dim * 4,
            );
        }
        Ok(())
    }

    #[cfg(feature = "vec")]
    #[test]
    fn vec_schema_creates_note_embeddings_table() {
        let index = SearchIndex::open_memory().expect("open");
        let count: i64 = index
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master \
                 WHERE type = 'table' AND name = 'note_embeddings'",
                [],
                |row| row.get(0),
            )
            .expect("query master");
        assert_eq!(count, 1, "note_embeddings table should be created");

        let mut stmt = index
            .conn
            .prepare("PRAGMA table_info(note_embeddings)")
            .expect("table_info");
        let cols: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(1))
            .expect("query")
            .filter_map(|r| r.ok())
            .collect();
        for expected in [
            "id",
            "note_path",
            "kind",
            "chunk_index",
            "text",
            "embedding",
            "dim",
            "model_version",
            "produced_at",
            "source_modified_at",
        ] {
            assert!(
                cols.iter().any(|c| c == expected),
                "note_embeddings missing column {expected}; got {cols:?}"
            );
        }
    }

    #[cfg(feature = "vec")]
    #[test]
    fn vec_schema_seeds_active_model_config() {
        let index = SearchIndex::open_memory().expect("open");
        let model: String = index
            .conn
            .query_row(
                "SELECT value FROM embedding_config WHERE key = 'active_model'",
                [],
                |row| row.get(0),
            )
            .expect("active_model row");
        assert_eq!(model, "bge-small-en-v1.5-candle");

        let dim: String = index
            .conn
            .query_row(
                "SELECT value FROM embedding_config WHERE key = 'active_dim'",
                [],
                |row| row.get(0),
            )
            .expect("active_dim row");
        assert_eq!(dim, "384");
    }

    #[cfg(feature = "vec")]
    #[test]
    fn vec_schema_is_idempotent() {
        // Two consecutive ensure_schema calls must not error and must not
        // double-insert the embedding_config seed rows.
        let index = SearchIndex::open_memory().expect("open");
        index.ensure_schema().expect("re-ensure");
        let count: i64 = index
            .conn
            .query_row(
                "SELECT COUNT(*) FROM embedding_config WHERE key = 'active_model'",
                [],
                |row| row.get(0),
            )
            .expect("count");
        assert_eq!(count, 1, "active_model must remain a single row across re-ensure");
    }

    #[cfg(feature = "vec")]
    #[test]
    fn vec_schema_migrates_old_db_without_note_embeddings() {
        // Build an old DB with notes only (no note_embeddings) and confirm a
        // fresh open creates the new tables idempotently and preserves the
        // existing notes row.
        let conn = Connection::open_in_memory().expect("open");
        conn.execute_batch("PRAGMA foreign_keys=ON;").expect("fk on");
        conn.execute_batch(
            "CREATE TABLE notes (
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
            INSERT INTO notes (path, title, domain, note_type, origin, status, date, tags, source, creator, body, summary, modified_at)
            VALUES ('notes/old.md', 'Old', 'tech', 'article', 'assisted', '', '2026-03-21', '[]', '', '', 'old body', 'old summary', 0);",
        )
        .expect("seed old schema");

        let index = SearchIndex { conn };
        index.ensure_schema().expect("migrate");

        // note_embeddings table was created
        let count: i64 = index
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master \
                 WHERE type = 'table' AND name = 'note_embeddings'",
                [],
                |row| row.get(0),
            )
            .expect("query master");
        assert_eq!(count, 1);

        // Pre-existing notes row preserved
        let title: String = index
            .conn
            .query_row("SELECT title FROM notes WHERE path = 'notes/old.md'", [], |row| {
                row.get(0)
            })
            .expect("legacy row");
        assert_eq!(title, "Old");
    }

    #[cfg(feature = "vec")]
    #[test]
    fn vec_schema_fk_cascade_deletes_embeddings_with_note() {
        // Insert a note + embedding row, delete the note, confirm the
        // embedding row vanishes via the native FK CASCADE. No trigger.
        let index = SearchIndex::open_memory().expect("open");
        index
            .conn
            .execute(
                "INSERT INTO notes (path, title, domain, note_type, origin, status, date, tags, source, creator, body, summary, modified_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                params![
                    "notes/cascade.md",
                    "T", "tech", "article", "assisted", "", "2026-03-21",
                    "[]", "", "", "body", "summary", 0_i64,
                ],
            )
            .expect("insert note");
        let bytes = encode_le_f32(&[0.1_f32, 0.2, 0.3, 0.4]);
        index
            .conn
            .execute(
                "INSERT INTO note_embeddings (
                    note_path, kind, chunk_index, text, embedding, dim,
                    model_version, produced_at, source_modified_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    "notes/cascade.md",
                    "summary",
                    0_i64,
                    "summary text",
                    bytes,
                    4_i64,
                    "test-model",
                    0_i64,
                    0_i64,
                ],
            )
            .expect("insert embedding");

        let before: i64 = index
            .conn
            .query_row(
                "SELECT COUNT(*) FROM note_embeddings WHERE note_path = ?1",
                params!["notes/cascade.md"],
                |row| row.get(0),
            )
            .expect("count before");
        assert_eq!(before, 1);

        index
            .conn
            .execute("DELETE FROM notes WHERE path = ?1", params!["notes/cascade.md"])
            .expect("delete note");

        let after: i64 = index
            .conn
            .query_row(
                "SELECT COUNT(*) FROM note_embeddings WHERE note_path = ?1",
                params!["notes/cascade.md"],
                |row| row.get(0),
            )
            .expect("count after");
        assert_eq!(after, 0, "FK CASCADE must remove embeddings when note is deleted");
    }

    #[cfg(feature = "vec")]
    #[test]
    fn vec_schema_fk_pragma_must_be_on_for_cascade() {
        // Defensive: if a future change disables PRAGMA foreign_keys=ON, the
        // FK CASCADE silently no-ops and orphans accumulate. Mimic the broken
        // case here and assert the orphan-detection signal so the regression
        // is loud.
        let conn = Connection::open_in_memory().expect("open");
        conn.execute_batch("PRAGMA foreign_keys=OFF;").expect("fk off");
        let index = SearchIndex { conn };
        index.ensure_schema().expect("schema");

        index
            .conn
            .execute(
                "INSERT INTO notes (path, title, domain, note_type, origin, status, date, tags, source, creator, body, summary, modified_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                params![
                    "notes/orphan.md",
                    "T", "tech", "article", "assisted", "", "2026-03-21",
                    "[]", "", "", "body", "summary", 0_i64,
                ],
            )
            .expect("insert note");
        let bytes = encode_le_f32(&[1.0_f32, 0.0]);
        index
            .conn
            .execute(
                "INSERT INTO note_embeddings (
                    note_path, kind, chunk_index, text, embedding, dim,
                    model_version, produced_at, source_modified_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    "notes/orphan.md",
                    "summary",
                    0_i64,
                    "t",
                    bytes,
                    2_i64,
                    "m",
                    0_i64,
                    0_i64,
                ],
            )
            .expect("insert embedding");

        index
            .conn
            .execute("DELETE FROM notes WHERE path = ?1", params!["notes/orphan.md"])
            .expect("delete");

        let orphan_count: i64 = index
            .conn
            .query_row(
                "SELECT COUNT(*) FROM note_embeddings WHERE note_path = ?1",
                params!["notes/orphan.md"],
                |row| row.get(0),
            )
            .expect("orphan count");
        // With FK enforcement OFF this must produce an orphan. If a future
        // refactor accidentally re-enables FK enforcement at the connection
        // level (or moves CASCADE into a trigger), this assertion fails
        // loudly and the maintainer is forced to re-think the regression.
        assert_eq!(
            orphan_count, 1,
            "with foreign_keys=OFF an orphan row must remain; \
             if FK enforcement is bolted on somewhere else, this test is the canary"
        );
    }

    #[cfg(feature = "vec")]
    #[test]
    fn validate_le_f32_len_rejects_mismatched_length() {
        // Length not divisible by 4 -> error.
        let bytes = vec![0u8; 7];
        let err = validate_le_f32_len(&bytes, 4).expect_err("expected error");
        let msg = format!("{err}");
        assert!(msg.contains("length mismatch"), "got: {msg}");

        // Length divisible by 4 but != dim*4 -> error.
        let bytes = vec![0u8; 12]; // 3 floats
        let err = validate_le_f32_len(&bytes, 4).expect_err("expected error");
        let msg = format!("{err}");
        assert!(msg.contains("length mismatch"), "got: {msg}");
    }

    #[cfg(feature = "vec")]
    #[test]
    fn validate_le_f32_len_accepts_exact_length() {
        let v = [1.5_f32, -0.25, 0.0, 7.5];
        let bytes = encode_le_f32(&v);
        validate_le_f32_len(&bytes, v.len()).expect("valid length");
    }

    #[cfg(feature = "vec")]
    #[test]
    fn vec_schema_kind_check_constraint_rejects_unknown_kind() {
        // The CHECK (kind IN ('summary', 'transcript-chunk')) constraint must
        // reject anything else. This protects the staleness queries from
        // ever seeing rows with a typo'd kind value.
        let index = SearchIndex::open_memory().expect("open");
        index
            .conn
            .execute(
                "INSERT INTO notes (path, title, domain, note_type, origin, status, date, tags, source, creator, body, summary, modified_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                params![
                    "notes/x.md",
                    "T", "tech", "article", "assisted", "", "2026-03-21",
                    "[]", "", "", "body", "summary", 0_i64,
                ],
            )
            .expect("insert note");

        let bytes = encode_le_f32(&[0.0_f32]);
        let result = index.conn.execute(
            "INSERT INTO note_embeddings (
                note_path, kind, chunk_index, text, embedding, dim,
                model_version, produced_at, source_modified_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                "notes/x.md",
                "garbage-kind",
                0_i64,
                "t",
                bytes,
                1_i64,
                "m",
                0_i64,
                0_i64,
            ],
        );
        assert!(result.is_err(), "CHECK constraint must reject unknown kinds");
    }

    #[cfg(feature = "vec")]
    #[test]
    fn vec_schema_unique_constraint_replaces_on_upsert_intent() {
        // The UNIQUE (note_path, kind, chunk_index, model_version) is the
        // upsert key used by Phase A5's re-embed loop. Direct INSERT must
        // fail on the second attempt, and INSERT OR REPLACE must succeed.
        let index = SearchIndex::open_memory().expect("open");
        index
            .conn
            .execute(
                "INSERT INTO notes (path, title, domain, note_type, origin, status, date, tags, source, creator, body, summary, modified_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                params![
                    "notes/up.md",
                    "T", "tech", "article", "assisted", "", "2026-03-21",
                    "[]", "", "", "body", "summary", 0_i64,
                ],
            )
            .expect("insert note");

        let bytes_a = encode_le_f32(&[1.0_f32, 0.0]);
        let bytes_b = encode_le_f32(&[0.0_f32, 1.0]);
        index
            .conn
            .execute(
                "INSERT INTO note_embeddings (
                    note_path, kind, chunk_index, text, embedding, dim,
                    model_version, produced_at, source_modified_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    "notes/up.md",
                    "summary",
                    0_i64,
                    "a",
                    bytes_a,
                    2_i64,
                    "bge-small-en-v1.5",
                    0_i64,
                    0_i64,
                ],
            )
            .expect("first insert");

        // Re-insert same (path, kind, chunk_index, model_version) must fail.
        let dup = index.conn.execute(
            "INSERT INTO note_embeddings (
                note_path, kind, chunk_index, text, embedding, dim,
                model_version, produced_at, source_modified_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                "notes/up.md",
                "summary",
                0_i64,
                "b",
                bytes_b.clone(),
                2_i64,
                "bge-small-en-v1.5",
                0_i64,
                0_i64,
            ],
        );
        assert!(dup.is_err(), "duplicate (path,kind,chunk,model) must be rejected");

        // INSERT OR REPLACE must replace cleanly.
        index
            .conn
            .execute(
                "INSERT OR REPLACE INTO note_embeddings (
                    note_path, kind, chunk_index, text, embedding, dim,
                    model_version, produced_at, source_modified_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    "notes/up.md",
                    "summary",
                    0_i64,
                    "b",
                    bytes_b,
                    2_i64,
                    "bge-small-en-v1.5",
                    1_i64,
                    1_i64,
                ],
            )
            .expect("replace");

        let (text, produced): (String, i64) = index
            .conn
            .query_row(
                "SELECT text, produced_at FROM note_embeddings \
                 WHERE note_path = ?1 AND kind = ?2 AND chunk_index = ?3 \
                   AND model_version = ?4",
                params!["notes/up.md", "summary", 0_i64, "bge-small-en-v1.5"],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("read replaced");
        assert_eq!(text, "b");
        assert_eq!(produced, 1);
    }

    // --- Phase A4: index_vault staleness contract ----------------------
    //
    // The contract: index_one's UPDATE branch does NOT touch
    // note_embeddings. Old embedding rows survive across reindex so
    // hybrid search never goes through a search-blackout window. Cortex's
    // stale_embedding_targets surfaces the row on its next tick and
    // upserts the new vector via the UNIQUE constraint.
    //
    // For remove_stale_notes (path absent from the next scan), the FK
    // CASCADE deletes matching note_embeddings rows automatically. No
    // trigger; foreign_keys=ON does the work.

    #[cfg(feature = "vec")]
    #[test]
    fn index_one_update_preserves_existing_embedding_rows() {
        let index = SearchIndex::open_memory().expect("open");
        let note = make_test_note("inbox/stale.md", "# T\n\n## Summary\n\nv1.\n");
        index.index_one(&note, 100).expect("first index");

        // Seed an embedding row that snapshotted notes.modified_at = 100.
        let bytes = encode_le_f32(&[0.1_f32, 0.2, 0.3]);
        index
            .conn
            .execute(
                "INSERT INTO note_embeddings (
                    note_path, kind, chunk_index, text, embedding, dim,
                    model_version, produced_at, source_modified_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    "inbox/stale.md",
                    "summary",
                    0_i64,
                    "v1.",
                    bytes,
                    3_i64,
                    "bge-small-en-v1.5",
                    100_i64,
                    100_i64,
                ],
            )
            .expect("seed embedding");

        // Reindex with new body + new mtime. This is the moment the
        // contract is load-bearing: if the UPDATE branch deletes the
        // embedding, hybrid search loses this note until cortex's next
        // re-embed tick (up to 10 minutes of blackout).
        let updated = make_test_note("inbox/stale.md", "# T\n\n## Summary\n\nv2.\n");
        index.index_one(&updated, 300).expect("reindex");

        let count: i64 = index
            .conn
            .query_row(
                "SELECT COUNT(*) FROM note_embeddings WHERE note_path = 'inbox/stale.md'",
                [],
                |row| row.get(0),
            )
            .expect("count");
        assert_eq!(
            count, 1,
            "index_one must not delete embeddings on UPDATE (blackout-free contract)"
        );

        // The row's source_modified_at is still 100, but notes.modified_at
        // is now 300. stale_embedding_targets must flag it.
        let targets = index
            .stale_embedding_targets(EmbeddingKind::Summary, "bge-small-en-v1.5", 100)
            .expect("targets");
        let paths: Vec<&str> = targets.iter().map(|t| t.note_path.as_str()).collect();
        assert!(
            paths.contains(&"inbox/stale.md"),
            "stale-target scan must flag the modified note: {paths:?}"
        );
    }

    #[cfg(feature = "vec")]
    #[test]
    fn note_deletion_via_remove_stale_cascades_to_embeddings() {
        // Simulate the index_vault end-of-pass cleanup: a note's path
        // disappears from the scan list, remove_stale_notes deletes the
        // notes row, and the FK CASCADE removes every matching
        // note_embeddings row. No trigger involved.
        let index = SearchIndex::open_memory().expect("open");
        let note = make_test_note("inbox/gone.md", "# T\n\n## Summary\n\nGoing.\n");
        index.index_one(&note, 100).expect("index");
        let bytes = encode_le_f32(&[0.5_f32, 0.5]);
        index
            .conn
            .execute(
                "INSERT INTO note_embeddings (
                    note_path, kind, chunk_index, text, embedding, dim,
                    model_version, produced_at, source_modified_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    "inbox/gone.md",
                    "summary",
                    0_i64,
                    "Going.",
                    bytes,
                    2_i64,
                    "bge-small-en-v1.5",
                    100_i64,
                    100_i64,
                ],
            )
            .expect("seed");

        // Drive remove_stale_notes with an empty current-paths list: every
        // existing row is stale.
        let removed = index.remove_stale_notes(&[]).expect("remove_stale");
        assert!(removed >= 1);

        let leftover: i64 = index
            .conn
            .query_row(
                "SELECT COUNT(*) FROM note_embeddings WHERE note_path = 'inbox/gone.md'",
                [],
                |row| row.get(0),
            )
            .expect("count");
        assert_eq!(leftover, 0, "FK CASCADE must remove all embeddings for a deleted note");
    }

    #[cfg(feature = "vec")]
    #[test]
    fn cortex_upsert_after_stale_flag_replaces_old_row_atomically() {
        // End-to-end of the staleness loop: a stale row exists, cortex
        // computes a new embedding and upserts (INSERT OR REPLACE keyed
        // by UNIQUE constraint). Exactly one row must remain afterward,
        // with the new source_modified_at.
        let index = SearchIndex::open_memory().expect("open");
        let note = make_test_note("inbox/loop.md", "# T\n\n## Summary\n\nv1.\n");
        index.index_one(&note, 100).expect("index v1");

        let stale = encode_le_f32(&[0.1_f32, 0.2]);
        index
            .conn
            .execute(
                "INSERT INTO note_embeddings (
                    note_path, kind, chunk_index, text, embedding, dim,
                    model_version, produced_at, source_modified_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    "inbox/loop.md",
                    "summary",
                    0_i64,
                    "v1",
                    stale,
                    2_i64,
                    "bge-small-en-v1.5",
                    100_i64,
                    100_i64,
                ],
            )
            .expect("seed stale");

        // Reindex with bumped mtime so the row is stale.
        let updated = make_test_note("inbox/loop.md", "# T\n\n## Summary\n\nv2 latest.\n");
        index.index_one(&updated, 300).expect("reindex");

        // Cortex catches up: produce a new vector and upsert.
        let fresh = [0.7_f32, 0.3];
        index
            .upsert_embedding(
                "inbox/loop.md",
                EmbeddingKind::Summary,
                0,
                "v2 latest",
                &fresh,
                "bge-small-en-v1.5",
                300,
            )
            .expect("cortex upsert");

        let (count, source_mod, text): (i64, i64, String) = index
            .conn
            .query_row(
                "SELECT COUNT(*), MAX(source_modified_at), MAX(text)
                 FROM note_embeddings
                 WHERE note_path = 'inbox/loop.md' AND kind = 'summary' AND chunk_index = 0",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("post-upsert");
        assert_eq!(count, 1, "UNIQUE must keep exactly one row");
        assert_eq!(source_mod, 300, "source_modified_at must advance");
        assert_eq!(text, "v2 latest");

        // And the note is no longer flagged stale.
        let targets = index
            .stale_embedding_targets(EmbeddingKind::Summary, "bge-small-en-v1.5", 100)
            .expect("targets");
        let paths: Vec<&str> = targets.iter().map(|t| t.note_path.as_str()).collect();
        assert!(
            !paths.contains(&"inbox/loop.md"),
            "post-upsert the row must not appear as stale: {paths:?}"
        );
    }
}
