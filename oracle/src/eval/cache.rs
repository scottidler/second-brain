//! Judgment cache: persists `(query, note) -> score` so reruns are cheap and
//! reproducible. Keyed on everything that can change a judgment — the query
//! text, the exact note text shown to the judge, the judge model, and the
//! rubric version — so any of those changing invalidates only the affected
//! rows (Architect review findings #1/#2).

use std::path::Path;

use eyre::{Context, Result};
use rusqlite::{Connection, params};

/// Rubric version embedded in the cache key. Bump when `judge-relevance.md` or
/// the score semantics change, to invalidate all prior judgments.
pub const RUBRIC_VERSION: &str = "v1";

const FNV_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
const FNV_PRIME: u64 = 0x100000001b3;

/// Stable, process- AND toolchain-independent hash of a string, hex-encoded.
///
/// FNV-1a (64-bit), pinned by the constants above. `std`'s `DefaultHasher` was
/// previously used here, but its algorithm is explicitly NOT guaranteed stable
/// across Rust releases — a toolchain bump would silently change every hash and
/// invalidate the entire judgment cache (re-buying every LLM judgment). FNV-1a
/// is a fixed spec, so the cache survives toolchain upgrades.
pub fn stable_hash(s: &str) -> String {
    let mut h = FNV_OFFSET_BASIS;
    for byte in s.as_bytes() {
        h ^= *byte as u64;
        h = h.wrapping_mul(FNV_PRIME);
    }
    format!("{h:016x}")
}

/// The full identity of a cached judgment.
#[derive(Debug, Clone)]
pub struct CacheKey<'a> {
    pub query_id: &'a str,
    pub query_hash: &'a str,
    pub note_path: &'a str,
    pub content_hash: &'a str,
    pub judge_model: &'a str,
}

/// A cached relevance judgment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CachedJudgment {
    pub score: u8,
    pub truncated: bool,
}

/// SQLite-backed judgment cache.
pub struct JudgmentCache {
    conn: Connection,
}

impl JudgmentCache {
    /// Open (creating parent dirs and the table if needed).
    pub fn open(path: &Path) -> Result<Self> {
        tracing::debug!(path = %path.display(), "JudgmentCache::open");
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| format!("creating eval cache dir {}", parent.display()))?;
        }
        let conn = Connection::open(path).with_context(|| format!("opening eval cache {}", path.display()))?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS eval_judgments (
                query_id        TEXT NOT NULL,
                query_hash      TEXT NOT NULL,
                note_path       TEXT NOT NULL,
                content_hash    TEXT NOT NULL,
                judge_model     TEXT NOT NULL,
                rubric_version  TEXT NOT NULL,
                score           INTEGER NOT NULL,
                truncated       INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (query_id, query_hash, note_path, content_hash, judge_model, rubric_version)
            );",
        )
        .context("creating eval_judgments table")?;
        Ok(Self { conn })
    }

    /// Open an in-memory cache (tests).
    pub fn open_memory() -> Result<Self> {
        let conn = Connection::open_in_memory().context("opening in-memory eval cache")?;
        conn.execute_batch(
            "CREATE TABLE eval_judgments (
                query_id TEXT NOT NULL, query_hash TEXT NOT NULL, note_path TEXT NOT NULL,
                content_hash TEXT NOT NULL, judge_model TEXT NOT NULL, rubric_version TEXT NOT NULL,
                score INTEGER NOT NULL, truncated INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (query_id, query_hash, note_path, content_hash, judge_model, rubric_version)
            );",
        )?;
        Ok(Self { conn })
    }

    /// Fetch a cached judgment, or `None` if absent for this exact key. A
    /// genuine "no rows" outcome is a cache miss; every other rusqlite error
    /// (locked/busy, decode failure) propagates instead of being silently
    /// swallowed as a miss — which would re-buy the LLM judgment every run.
    pub fn get(&self, k: &CacheKey) -> Result<Option<CachedJudgment>> {
        let result = self.conn.query_row(
            "SELECT score, truncated FROM eval_judgments
                 WHERE query_id=?1 AND query_hash=?2 AND note_path=?3
                   AND content_hash=?4 AND judge_model=?5 AND rubric_version=?6",
            params![
                k.query_id,
                k.query_hash,
                k.note_path,
                k.content_hash,
                k.judge_model,
                RUBRIC_VERSION
            ],
            |r| {
                let score: i64 = r.get(0)?;
                let truncated: i64 = r.get(1)?;
                Ok(CachedJudgment {
                    score: score as u8,
                    truncated: truncated != 0,
                })
            },
        );
        match result {
            Ok(judgment) => Ok(Some(judgment)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e).context("judgment cache lookup failed"),
        }
    }

    /// Insert or replace a judgment.
    pub fn put(&self, k: &CacheKey, j: CachedJudgment) -> Result<()> {
        self.conn
            .execute(
                "INSERT OR REPLACE INTO eval_judgments
                 (query_id, query_hash, note_path, content_hash, judge_model, rubric_version, score, truncated)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    k.query_id,
                    k.query_hash,
                    k.note_path,
                    k.content_hash,
                    k.judge_model,
                    RUBRIC_VERSION,
                    j.score as i64,
                    j.truncated as i64,
                ],
            )
            .context("inserting eval judgment")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
