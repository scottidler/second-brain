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

/// Normalize a raw frontmatter `date:` to canonical `YYYY-MM-DD`. Returns ``
/// for anything that does not parse as a leading ISO date - absent, a bare
/// `Number(2023)` debug-string, a slash format, or a Templater literal. The
/// `notes.date` column is written exclusively through this, so downstream
/// lexical comparison is over guaranteed-canonical data (or the empty
/// sentinel, which every consumer treats as "undated").
fn normalize_date(raw: &str) -> String {
    let head = raw.get(..10).unwrap_or(raw);
    match chrono::NaiveDate::parse_from_str(head, "%Y-%m-%d") {
        Ok(d) => d.format("%Y-%m-%d").to_string(),
        Err(_) => String::new(),
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

mod rerank;

// The reranker port, test fake, and pure helpers are backend-independent.
pub use rerank::{MockReranker, Reranker, project_batch_ms, rerank_paths};
// The Candle cross-encoder is local model inference, so it lands here (like the
// embedder); gated to the Candle backend the daemon host must run.
#[cfg(feature = "vec-candle")]
pub use rerank::{CandleCrossEncoder, get_or_load_reranker, prefetch_reranker};

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
            -- The cold-note SELECT filters and orders by `date`; this index
            -- keeps it index-backed instead of a full scan.
            CREATE INDEX IF NOT EXISTS idx_notes_date ON notes(date);
            -- `modified_at` is still read by other code paths; keep its index
            -- even though the cold SELECT no longer filters on it.
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

        // `modified_at` is still read by other code paths; ensure its index
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
        // Normalize to canonical YYYY-MM-DD (or `` when unparseable) so the
        // `date` column is structurally trustworthy for lexical comparison;
        // the cold sweep's date floor rests on this.
        let date = normalize_date(fm.date.as_deref().unwrap_or(""));
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
    /// - `date < before_date`: content older than the floor. Undated notes
    ///   (`date = ''`) are excluded - age cannot be inferred, and they are
    ///   the lint/quality sweep's responsibility, not cold's.
    ///
    /// Ordered by `date ASC` so the oldest cold notes surface first.
    pub fn cold_notes(&self, q: &ColdQuery) -> Result<Vec<ColdNote>> {
        log::debug!("cold_notes: before_date={} limit={}", q.before_date, q.limit);
        let mut stmt = self.conn.prepare(
            "SELECT path, title, domain, date
             FROM notes
             WHERE search_hit_count = 0
               AND last_accessed_at IS NULL
               AND inbound_link_count = 0
               AND pinned = 0
               AND date != ''
               AND date IS NOT NULL
               AND date < ?1
             ORDER BY date ASC
             LIMIT ?2",
        )?;
        let rows = stmt
            .query_map(params![q.before_date, q.limit as i64], |row| {
                Ok(ColdNote {
                    path: row.get(0)?,
                    title: row.get::<_, String>(1).unwrap_or_default(),
                    domain: row.get::<_, String>(2).unwrap_or_default(),
                    date: row.get::<_, String>(3).unwrap_or_default(),
                })
            })?
            .filter_map(|r| r.ok())
            .collect();
        Ok(rows)
    }

    /// Count rows that would have qualified for the cold report except
    /// they are pinned. Surfaces visibility into how often the promotion
    /// floor rescues notes from the report. Uses the identical age predicate
    /// as `cold_notes` so the two numbers describe the same population.
    pub fn count_pinned_excluded(&self, before_date: &str) -> Result<u64> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM notes
             WHERE search_hit_count = 0
               AND last_accessed_at IS NULL
               AND inbound_link_count = 0
               AND pinned = 1
               AND date != ''
               AND date IS NOT NULL
               AND date < ?1",
            params![before_date],
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

    /// Cortex-assigned quality level for one note (`low` / `medium` / `high`,
    /// or `""` when unscored). `None` when the note is not in the index.
    ///
    /// This is the only stubness signal queryable from the `notes` table: the
    /// richer `cortex-quality-issues` frontmatter (which carries the
    /// `[stub-body]` marker) is not a column here. Oracle's exclude filter uses
    /// `quality = low` as its stub proxy.
    pub fn note_quality(&self, path: &str) -> Result<Option<String>> {
        let q: Option<String> = self
            .conn
            .query_row("SELECT quality FROM notes WHERE path = ?1", params![path], |row| {
                row.get(0)
            })
            .ok();
        Ok(q)
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

/// Parameters for `SearchIndex::cold_notes`. `before_date` is an exclusive
/// ISO `YYYY-MM-DD` floor: a note qualifies when its `date:` frontmatter is
/// strictly older - lexically less than `before_date`. A note dated exactly
/// on the floor day does NOT qualify. The lexical compare is correct because
/// `index_one` normalizes the `date` column to canonical ISO (or `''`).
#[derive(Debug, Clone)]
pub struct ColdQuery {
    pub before_date: String,
    pub limit: u32,
}

/// Subset of `notes` returned by `cold_notes` - just the fields the
/// review report needs. Saves a row body load. `date` is the note's
/// content date (`date:` frontmatter), canonical `YYYY-MM-DD`.
#[derive(Debug, Clone, Serialize)]
pub struct ColdNote {
    pub path: String,
    pub title: String,
    pub domain: String,
    pub date: String,
}

#[cfg(test)]
mod tests;
