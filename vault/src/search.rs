//! SQLite-backed full-text search index for vault notes
//!
//! Provides FTS5-powered search, incremental indexing by mtime, and
//! domain/tag analytics. Shared by oracle (MCP server) and cortex (daemon).

use crate::config::ScanConfig;
use crate::detail;
use crate::distilled::{Claim, ClaimKind};
use crate::note::{Note, scan_vault};
use crate::schema::{Domain, NoteType, Origin, Status};
use chrono;
use eyre::{Result, WrapErr};
use regex::Regex;
use rusqlite::{Connection, params};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::Duration;

/// Wikilink extraction regex, compiled once (was recompiled ~2.3k times per
/// reindex pass - once per note in `extract_wikilinks`).
static WIKILINK_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[\[([^\]|#]+)(?:[|#][^\]]+)?\]\]").expect("wikilink regex"));

/// English stop-word set for FTS5 term extraction, built once (was rebuilt on
/// every `extract_search_terms` call).
static STOP_WORDS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        "the", "a", "an", "and", "or", "but", "in", "on", "at", "to", "for", "of", "with", "by", "from", "is", "it",
        "this", "that", "are", "was", "were", "be", "been", "being", "have", "has", "had", "do", "does", "did", "will",
        "would", "could", "should", "may", "might", "can", "shall", "not", "no", "nor", "so", "if", "then", "than",
        "too", "very", "just", "about", "up", "out", "into", "over", "after", "before", "between", "through", "during",
        "without", "again", "further", "once", "here", "there", "when", "where", "why", "how", "all", "each", "every",
        "both", "few", "more", "most", "other", "some", "such", "only", "own", "same", "also", "as", "its", "you",
        "your", "we", "our", "they", "their", "what", "which", "who", "whom",
    ]
    .into_iter()
    .collect()
});

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

/// Map a single-row query result so a genuine "no rows" outcome becomes
/// `Ok(None)`, while every other rusqlite error (notably `SQLITE_BUSY` under
/// writer contention) propagates instead of being silently swallowed as a
/// missing row. Replaces the `.ok()` conflation that made busy/locked reads
/// look like absent notes (false missing notes, dropped edges, INSERT-on-PK).
pub(crate) fn optional_row<T>(result: rusqlite::Result<T>) -> Result<Option<T>> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e).wrap_err("sqlite single-row query failed"),
    }
}

/// Iterator adapter for `query_map` rows: yields each `Ok` row and emits a WARN
/// (instead of dropping silently) for any `Err`. Replaces
/// `filter_map(|r| r.ok())`, which hid per-row decode/IO failures.
pub(crate) fn warn_row<T>(result: rusqlite::Result<T>) -> Option<T> {
    match result {
        Ok(value) => Some(value),
        // A `SqliteFailure` here is the STATEMENT failing mid-step, not one bad
        // row: sqlite reports a malformed FTS5 MATCH on the first `next()`, so
        // every row "fails" and the caller sees an empty result set. That is how
        // a broken query (`xda-developers` unquoted) read as "no similar notes"
        // for months behind a WARN. ERROR so it cannot hide again; the FTS path
        // in `query::search` propagates instead of dropping.
        Err(e @ rusqlite::Error::SqliteFailure(..)) => {
            log::error!("sqlite statement failed mid-iteration (results are INCOMPLETE): {e}");
            None
        }
        Err(e) => {
            log::warn!("dropping unreadable sqlite row: {e}");
            None
        }
    }
}

/// Quote one literal term for an FTS5 MATCH expression.
///
/// FTS5 barewords cannot contain `-`, `:`, or `"`, and `AND`/`OR`/`NOT`/`NEAR`
/// are operators - an unquoted `xda-developers` or `dfb3bc2f-6dc0-…` aborts the
/// whole MATCH with `no such column: developers`. Wrapping each term in double
/// quotes makes it an FTS5 string (a phrase over the tokenizer's output), which
/// is exactly the "match these words in order" semantics a literal term wants.
/// Internal double quotes are escaped by doubling, per FTS5 syntax.
pub(crate) fn fts_quote(term: &str) -> String {
    format!("\"{}\"", term.replace('"', "\"\""))
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
/// Each bulleted line becomes one `Claim`. The renderer decorates a claim as
/// `- **kind** (who): text [anchor]` with an optional indented `  > "quote"`
/// continuation line; this parser strips every piece of that decoration so the
/// recovered `Claim.text` is the clean claim sentence the FTS index stores,
/// while the kind / who / quote / anchor fields are recovered for round-trip.
///
/// The `fact` kind and an absent `who` produce no prefix (the legacy shape
/// `- text [anchor]`), so pre-Phase-3 notes parse exactly as before. Returns an
/// empty Vec when no `## Claims` section is present.
pub fn parse_body_claims(body: &str) -> Vec<Claim> {
    let mut claims: Vec<Claim> = Vec::new();
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

        // A `> "..."` continuation line carries the verbatim quote for the
        // most recent claim bullet. Strip the blockquote marker and the
        // surrounding double quotes for the recovered `quote` field.
        if let Some(rest) = trimmed.strip_prefix("> ") {
            if let Some(last) = claims.last_mut() {
                let quote = rest.trim().trim_matches('"').trim();
                if !quote.is_empty() {
                    last.quote = Some(quote.to_string());
                }
            }
            continue;
        }

        let bullet = trimmed.strip_prefix("- ").or_else(|| trimmed.strip_prefix("* "));
        let Some(content) = bullet else {
            continue;
        };
        let content = content.trim();
        if content.is_empty() {
            continue;
        }
        // Peel decoration in render order (reverse): trailing [anchor], then
        // the leading `**kind**` / `(who)` prefix.
        let (rest, anchor) = split_trailing_anchor(content);
        let (kind, rest) = split_leading_kind(rest);
        let (who, text) = split_leading_who(rest);
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        claims.push(Claim {
            text: text.to_string(),
            anchor,
            kind,
            who,
            quote: None,
        });
    }
    claims
}

/// Strip a leading `**kind**` decoration when `content` begins with a bold
/// token that is a known [`ClaimKind`] followed by ` ` / `(` / `:`. An unknown
/// bold token is left untouched (returns `Fact` and the original content) so a
/// legacy claim that happens to start with bold text is never misparsed.
fn split_leading_kind(content: &str) -> (ClaimKind, &str) {
    let Some(after_open) = content.strip_prefix("**") else {
        return (ClaimKind::Fact, content);
    };
    let Some(close) = after_open.find("**") else {
        return (ClaimKind::Fact, content);
    };
    let word = &after_open[..close];
    let Some(kind) = ClaimKind::parse_known(word) else {
        return (ClaimKind::Fact, content);
    };
    let rest = after_open[close + 2..].trim_start();
    // Drop the `: ` separator only when it directly follows the kind prefix
    // and there is no `(who)` group (which owns the separator instead).
    let rest = rest.strip_prefix(':').map(str::trim_start).unwrap_or(rest);
    (kind, rest)
}

/// Strip a leading `(who)` decoration followed by a `: ` separator. Returns the
/// attribution and the remaining claim text. Only a `(...)` group at the very
/// start immediately followed by `:` is treated as attribution, so mid-text
/// parentheses are never mistaken for a who-field.
fn split_leading_who(content: &str) -> (Option<String>, &str) {
    let Some(after_open) = content.strip_prefix('(') else {
        return (None, content);
    };
    let Some(close) = after_open.find(')') else {
        return (None, content);
    };
    let after_paren = after_open[close + 1..].trim_start();
    let Some(text) = after_paren.strip_prefix(':') else {
        // A leading `(...)` that is not a `(who):` prefix is real claim text.
        return (None, content);
    };
    let who = after_open[..close].trim();
    if who.is_empty() {
        return (None, content);
    }
    (Some(who.to_string()), text.trim_start())
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

mod cold;
mod index;
mod query;
mod rerank;
mod schema;
mod stats;

// The reranker port, test fake, and pure helpers are backend-independent.
pub use rerank::{MockReranker, Reranker, project_batch_ms, rerank_paths};
// The Candle cross-encoder is local model inference, so it lands here (like the
// embedder); gated to the Candle backend the daemon host must run.
#[cfg(feature = "vec-candle")]
pub use rerank::{CandleCrossEncoder, get_or_load_reranker, prefetch_reranker};

/// Typed errors `SearchIndex` returns as `eyre::Report` sources (the
/// `FabricError` pattern in `vault::fabric`): callers keep the `eyre::Result`
/// signature and downcast when they need to branch on the specific cause.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SearchError {
    /// The oracle DB has moved under `~/.local/share/sb/oracle/` but this host
    /// still holds the pre-move DB at `~/.local/share/oracle/`. Opening would
    /// silently create an empty index and orphan every embedding, so refuse.
    #[error(
        "legacy oracle DB at {legacy} but the current path is {new}; \
         refusing to create an empty index (runbook R1 moves it)"
    )]
    LegacyOracleDb { legacy: PathBuf, new: PathBuf },
}

impl SearchIndex {
    /// Open (or create) the search index at the given path
    pub fn open(db_path: &Path) -> Result<Self> {
        // Fail-closed guard, FIRST: this must run before `create_dir_all`
        // below, or merely *checking* would mint `~/.local/share/sb/oracle/`
        // and runbook R1's `mv -T` would then nest the legacy dir inside it.
        // Every opener (cortex daemon and one-shots, `oracle serve/index/call/
        // stats/eval`, `sb doctor`) funnels through here, so no process can
        // create an empty DB while the pre-move one still exists. No
        // auto-migration: concurrent openers have no lock, and a lost race
        // would cost a full re-embed.
        if db_path == crate::paths::oracle_db_path()
            && !db_path.exists()
            && crate::paths::legacy_oracle_dir().join("oracle.db").exists()
        {
            return Err(SearchError::LegacyOracleDb {
                legacy: crate::paths::legacy_oracle_dir(),
                new: db_path.to_path_buf(),
            }
            .into());
        }

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

    // --- Governance & Health Methods ---
}

/// Extract significant search terms from content for FTS5 similarity queries.
/// Filters out common English stop words and short tokens.
fn extract_search_terms(content: &str, max_terms: usize) -> Vec<String> {
    let stop_words = &*STOP_WORDS;

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
    let re = &*WIKILINK_RE;
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

/// Extract hostname from a URL string: strip the scheme, drop path/query, drop
/// a leading `www.`, lowercase. `None` when the input carries no `http(s)://`
/// scheme or resolves to an empty host.
///
/// `pub` so cortex's hub/graph layers derive source hubs from the SAME host
/// implementation this crate's stats use (single source of host truth); before
/// this there were two divergent copies in cortex that disagreed on schemeless
/// input.
pub fn extract_host(url: &str) -> Option<String> {
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
    /// borg staged-trace handle, or `""` when absent.
    pub trace: String,
    /// Retention-clock start (ISO-8601 or bare date), or `""` when absent.
    pub ingested: String,
    /// Absolute policy expiry (`YYYY-MM-DD`), or `""` when not yet stamped.
    pub trace_expires: String,
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
            trace: row.get(12)?,
            ingested: row.get(13)?,
            trace_expires: row.get(14)?,
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
