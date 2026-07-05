//! Judgment cache: persists `(fixture, content) -> axis scores` so reruns are
//! cheap and reproducible. Keyed on everything that can change a judgment — the
//! fixture id, the exact text shown to the judge (content hash), the judge
//! model, and the rubric version — so any of those changing invalidates only
//! the affected rows. A cache-hit re-run makes zero judge calls (a Phase 1
//! success criterion), so a genuine SQLite error must never masquerade as a
//! miss and re-buy the LLM judgment.

use std::path::Path;

use eyre::{Context, Result};
use rusqlite::{Connection, params};

use crate::eval::judge::AxisScores;

/// Rubric version embedded in the cache key. Bump when `judge-distillation.md`
/// or the score semantics change, to invalidate all prior judgments.
pub const RUBRIC_VERSION: &str = "v1";

const FNV_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
const FNV_PRIME: u64 = 0x100000001b3;

/// Stable, process- AND toolchain-independent hash of a string, hex-encoded.
///
/// FNV-1a (64-bit), pinned by the constants above — `std`'s `DefaultHasher` is
/// explicitly NOT stable across Rust releases, so a toolchain bump would
/// silently invalidate the whole judgment cache (re-buying every LLM judgment).
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
    pub fixture_id: &'a str,
    pub content_hash: &'a str,
    pub judge_model: &'a str,
}

/// A cached three-axis judgment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CachedJudgment {
    pub scores: AxisScores,
    pub truncated: bool,
}

/// SQLite-backed judgment cache.
pub struct JudgmentCache {
    conn: Connection,
}

const CREATE_TABLE: &str = "CREATE TABLE IF NOT EXISTS distill_judgments (
    fixture_id            TEXT NOT NULL,
    content_hash          TEXT NOT NULL,
    judge_model           TEXT NOT NULL,
    rubric_version        TEXT NOT NULL,
    claim_coverage        INTEGER NOT NULL,
    anchor_validity       INTEGER NOT NULL,
    summary_faithfulness  INTEGER NOT NULL,
    truncated             INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (fixture_id, content_hash, judge_model, rubric_version)
);";

impl JudgmentCache {
    /// Open (creating parent dirs and the table if needed).
    pub fn open(path: &Path) -> Result<Self> {
        log::debug!("JudgmentCache::open: path={}", path.display());
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| format!("creating eval cache dir {}", parent.display()))?;
        }
        let conn = Connection::open(path).with_context(|| format!("opening eval cache {}", path.display()))?;
        conn.execute_batch(CREATE_TABLE)
            .context("creating distill_judgments table")?;
        Ok(Self { conn })
    }

    /// Open an in-memory cache (tests).
    pub fn open_memory() -> Result<Self> {
        let conn = Connection::open_in_memory().context("opening in-memory eval cache")?;
        conn.execute_batch(CREATE_TABLE)
            .context("creating distill_judgments table")?;
        Ok(Self { conn })
    }

    /// Fetch a cached judgment, or `None` if absent for this exact key. A
    /// genuine "no rows" outcome is a cache miss; every other rusqlite error
    /// propagates instead of being silently swallowed as a miss.
    pub fn get(&self, k: &CacheKey) -> Result<Option<CachedJudgment>> {
        let result = self.conn.query_row(
            "SELECT claim_coverage, anchor_validity, summary_faithfulness, truncated
                 FROM distill_judgments
                 WHERE fixture_id=?1 AND content_hash=?2 AND judge_model=?3 AND rubric_version=?4",
            params![k.fixture_id, k.content_hash, k.judge_model, RUBRIC_VERSION],
            |r| {
                let cc: i64 = r.get(0)?;
                let av: i64 = r.get(1)?;
                let sf: i64 = r.get(2)?;
                let truncated: i64 = r.get(3)?;
                Ok(CachedJudgment {
                    scores: AxisScores {
                        claim_coverage: cc as u8,
                        anchor_validity: av as u8,
                        summary_faithfulness: sf as u8,
                    },
                    truncated: truncated != 0,
                })
            },
        );
        match result {
            Ok(j) => Ok(Some(j)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e).context("judgment cache lookup failed"),
        }
    }

    /// Insert or replace a judgment.
    pub fn put(&self, k: &CacheKey, j: CachedJudgment) -> Result<()> {
        self.conn
            .execute(
                "INSERT OR REPLACE INTO distill_judgments
                 (fixture_id, content_hash, judge_model, rubric_version,
                  claim_coverage, anchor_validity, summary_faithfulness, truncated)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    k.fixture_id,
                    k.content_hash,
                    k.judge_model,
                    RUBRIC_VERSION,
                    j.scores.claim_coverage as i64,
                    j.scores.anchor_validity as i64,
                    j.scores.summary_faithfulness as i64,
                    j.truncated as i64,
                ],
            )
            .context("inserting distill judgment")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
