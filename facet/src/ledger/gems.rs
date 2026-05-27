//! `gems` and `interaction_turns` accessors (facet v2).
//!
//! The schema is created by `bin/migrate-facet-v2.sh`; this module is
//! purely accessor code that assumes the tables exist. Tests apply
//! [`apply_v2_ddl`] after `Ledger::open_in_memory` to get a working
//! schema in-process.
//!
//! Idempotency contract: `UNIQUE (workitem_id, content_hash)` on
//! `gems`. Re-extracting a gem whose interaction turns span the same
//! set of UUIDs (in any order) produces the same `content_hash` and
//! upserts the existing row. `interaction_turns` for that gem are
//! wiped and reinserted on upsert so per-turn tag revisions land
//! cleanly.

use chrono::{DateTime, Utc};
use eyre::{Context, Result};
use rusqlite::Connection;

use super::Ledger;
use crate::gems::{Gem, InteractionTurn, Review};

#[cfg(test)]
mod tests;

/// The v2 DDL mirrors `bin/migrate-facet-v2.sh`. Kept here so tests
/// (and any future folding into the main migrate path) have a single
/// Rust-side source of truth. If you change this, change the bash
/// script too.
pub const V2_DDL: &str = r#"
CREATE TABLE IF NOT EXISTS gems (
    id INTEGER PRIMARY KEY,
    workitem_id INTEGER NOT NULL REFERENCES work_items(id) ON DELETE CASCADE,
    session_uuid TEXT NOT NULL,
    content_hash TEXT NOT NULL,
    first_user_turn_uuid TEXT NOT NULL,
    last_user_turn_uuid TEXT NOT NULL,
    task TEXT NOT NULL,
    context_loaded TEXT NOT NULL,
    context_missing TEXT NOT NULL,
    review_accepted TEXT,
    review_rejected TEXT,
    review_verified_manually TEXT,
    review_rewrote_by_hand TEXT,
    tags TEXT NOT NULL,
    why_it_matters TEXT NOT NULL,
    extractor_model TEXT NOT NULL,
    extracted_at TEXT NOT NULL,
    UNIQUE (workitem_id, content_hash)
);

CREATE INDEX IF NOT EXISTS idx_gems_session  ON gems(session_uuid);
CREATE INDEX IF NOT EXISTS idx_gems_workitem ON gems(workitem_id);

CREATE TABLE IF NOT EXISTS interaction_turns (
    id INTEGER PRIMARY KEY,
    gem_id INTEGER NOT NULL REFERENCES gems(id) ON DELETE CASCADE,
    seq INTEGER NOT NULL,
    ai_says TEXT NOT NULL,
    ai_turn_uuid TEXT NOT NULL,
    user_says TEXT NOT NULL,
    user_turn_uuid TEXT NOT NULL,
    tags TEXT NOT NULL,
    UNIQUE (gem_id, seq)
);

CREATE INDEX IF NOT EXISTS idx_interaction_turns_gem ON interaction_turns(gem_id);

-- One row per discovered narrative (Session Arc, Cross-Session Arc, or
-- evergreen mode rollup). `cluster_key` is the stable identity per
-- cluster (session_uuid / sha256-derived xs-... / mode-<name>) and is
-- the idempotency key; titles may drift on re-narrate. `gem_ids` is a
-- JSON array of citations into the gems table.
CREATE TABLE IF NOT EXISTS narratives (
    id INTEGER PRIMARY KEY,
    cluster_key TEXT NOT NULL UNIQUE,
    slug TEXT NOT NULL,
    title TEXT NOT NULL,
    thesis TEXT NOT NULL,
    body_md TEXT NOT NULL,
    gem_ids TEXT NOT NULL,
    archetype TEXT NOT NULL,
    synthesised_at TEXT NOT NULL,
    synthesiser_model TEXT NOT NULL,
    revision INTEGER NOT NULL DEFAULT 1
);

CREATE INDEX IF NOT EXISTS idx_narratives_slug      ON narratives(slug);
CREATE INDEX IF NOT EXISTS idx_narratives_archetype ON narratives(archetype);

-- Sidecar of narrative metadata describing what holds the cluster
-- together. One row per narrative.
CREATE TABLE IF NOT EXISTS narrative_axes (
    narrative_id INTEGER PRIMARY KEY REFERENCES narratives(id) ON DELETE CASCADE,
    semantic_cluster_id INTEGER,
    mode_mix TEXT NOT NULL,
    time_window_start TEXT,
    time_window_end TEXT,
    repos TEXT NOT NULL,
    workitem_ids TEXT NOT NULL
);
"#;

/// Apply the v2 DDL idempotently. Used by tests and as the inner
/// helper if the schema-management path eventually moves to Rust.
pub fn apply_v2_ddl(conn: &mut Connection) -> Result<()> {
    conn.execute_batch(V2_DDL).context("apply v2 ddl")
}

#[derive(Debug, Clone)]
pub struct NewGem<'a> {
    pub workitem_id: i64,
    pub session_uuid: &'a str,
    pub task: &'a str,
    pub context_loaded: &'a [String],
    pub context_missing: &'a [String],
    pub interaction: &'a [InteractionTurn],
    pub review: &'a Review,
    pub tags: &'a [String],
    pub why_it_matters: &'a str,
    pub extractor_model: &'a str,
    pub extracted_at: DateTime<Utc>,
}

impl Ledger {
    /// Apply the v2 schema (gems + interaction_turns). Idempotent.
    /// Production installs invoke `bin/migrate-facet-v2.sh` instead;
    /// this method exists for in-process callers (tests, future
    /// schema-version bump).
    pub fn apply_facet_v2_schema(&self) -> Result<()> {
        log::debug!("ledger::apply_facet_v2_schema");
        self.with_conn(apply_v2_ddl)
    }

    /// Upsert a gem with its interaction turns. Returns the gem id.
    ///
    /// On conflict (workitem_id, content_hash):
    /// - the gem row is updated with the latest payload
    /// - the existing interaction_turns rows for that gem are deleted
    ///   and reinserted from `new.interaction`.
    pub fn upsert_gem(&self, new: NewGem<'_>) -> Result<i64> {
        log::debug!(
            "ledger::upsert_gem: workitem_id={} session_uuid={} turns={} tags={:?}",
            new.workitem_id,
            new.session_uuid,
            new.interaction.len(),
            new.tags,
        );

        let content_hash = compute_content_hash(new.interaction);
        let (first_user_uuid, last_user_uuid) = boundary_user_uuids(new.interaction)?;
        let ctx_loaded_json = serde_json::to_string(new.context_loaded).context("encode context_loaded")?;
        let ctx_missing_json = serde_json::to_string(new.context_missing).context("encode context_missing")?;
        let tags_json = serde_json::to_string(new.tags).context("encode tags")?;

        self.with_conn(|c| {
            let tx = c.transaction().context("begin upsert_gem tx")?;

            tx.execute(
                "INSERT INTO gems \
                    (workitem_id, session_uuid, content_hash, first_user_turn_uuid, last_user_turn_uuid, \
                     task, context_loaded, context_missing, \
                     review_accepted, review_rejected, review_verified_manually, review_rewrote_by_hand, \
                     tags, why_it_matters, extractor_model, extracted_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16) \
                 ON CONFLICT (workitem_id, content_hash) DO UPDATE SET \
                    session_uuid = excluded.session_uuid, \
                    first_user_turn_uuid = excluded.first_user_turn_uuid, \
                    last_user_turn_uuid = excluded.last_user_turn_uuid, \
                    task = excluded.task, \
                    context_loaded = excluded.context_loaded, \
                    context_missing = excluded.context_missing, \
                    review_accepted = excluded.review_accepted, \
                    review_rejected = excluded.review_rejected, \
                    review_verified_manually = excluded.review_verified_manually, \
                    review_rewrote_by_hand = excluded.review_rewrote_by_hand, \
                    tags = excluded.tags, \
                    why_it_matters = excluded.why_it_matters, \
                    extractor_model = excluded.extractor_model, \
                    extracted_at = excluded.extracted_at",
                rusqlite::params![
                    new.workitem_id,
                    new.session_uuid,
                    content_hash,
                    first_user_uuid,
                    last_user_uuid,
                    new.task,
                    ctx_loaded_json,
                    ctx_missing_json,
                    new.review.accepted,
                    new.review.rejected,
                    new.review.verified_manually,
                    new.review.rewrote_by_hand,
                    tags_json,
                    new.why_it_matters,
                    new.extractor_model,
                    new.extracted_at.to_rfc3339(),
                ],
            )
            .context("upsert gem row")?;

            let gem_id: i64 = tx
                .query_row(
                    "SELECT id FROM gems WHERE workitem_id = ?1 AND content_hash = ?2",
                    rusqlite::params![new.workitem_id, content_hash],
                    |r| r.get(0),
                )
                .context("look up upserted gem id")?;

            tx.execute(
                "DELETE FROM interaction_turns WHERE gem_id = ?1",
                rusqlite::params![gem_id],
            )
            .context("clear stale interaction_turns")?;

            for (seq, turn) in new.interaction.iter().enumerate() {
                let turn_tags_json = serde_json::to_string(&turn.tags).context("encode interaction turn tags")?;
                tx.execute(
                    "INSERT INTO interaction_turns \
                        (gem_id, seq, ai_says, ai_turn_uuid, user_says, user_turn_uuid, tags) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    rusqlite::params![
                        gem_id,
                        seq as i64,
                        turn.ai_says,
                        turn.ai_turn_uuid,
                        turn.user_says,
                        turn.user_turn_uuid,
                        turn_tags_json,
                    ],
                )
                .context("insert interaction_turn")?;
            }

            tx.commit().context("commit upsert_gem")?;
            Ok(gem_id)
        })
    }

    /// Read a single gem (and its interaction turns) by id. Returns
    /// `Ok(None)` if no such gem exists.
    pub fn gem_by_id(&self, id: i64) -> Result<Option<Gem>> {
        log::debug!("ledger::gem_by_id: id={id}");
        self.with_conn(|c| {
            let row: Option<GemRow> = c
                .query_row(
                    "SELECT id, workitem_id, session_uuid, content_hash, \
                            first_user_turn_uuid, last_user_turn_uuid, \
                            task, context_loaded, context_missing, \
                            review_accepted, review_rejected, review_verified_manually, review_rewrote_by_hand, \
                            tags, why_it_matters, extractor_model, extracted_at \
                     FROM gems WHERE id = ?1",
                    rusqlite::params![id],
                    GemRow::from_row,
                )
                .ok();
            match row {
                None => Ok(None),
                Some(r) => {
                    let interaction = load_interaction_turns(c, r.id)?;
                    Ok(Some(r.into_gem(interaction)))
                }
            }
        })
    }

    /// Read a gem by its idempotency key. Used by callers that want
    /// to short-circuit a re-extract before invoking the LLM.
    pub fn gem_by_content_hash(&self, workitem_id: i64, content_hash: &str) -> Result<Option<Gem>> {
        log::debug!("ledger::gem_by_content_hash: workitem_id={workitem_id} content_hash={content_hash}");
        self.with_conn(|c| {
            let id: Option<i64> = c
                .query_row(
                    "SELECT id FROM gems WHERE workitem_id = ?1 AND content_hash = ?2",
                    rusqlite::params![workitem_id, content_hash],
                    |r| r.get(0),
                )
                .ok();
            match id {
                None => Ok(None),
                Some(gem_id) => {
                    let row: GemRow = c
                        .query_row(
                            "SELECT id, workitem_id, session_uuid, content_hash, \
                                    first_user_turn_uuid, last_user_turn_uuid, \
                                    task, context_loaded, context_missing, \
                                    review_accepted, review_rejected, review_verified_manually, review_rewrote_by_hand, \
                                    tags, why_it_matters, extractor_model, extracted_at \
                             FROM gems WHERE id = ?1",
                            rusqlite::params![gem_id],
                            GemRow::from_row,
                        )
                        .context("read gem row")?;
                    let interaction = load_interaction_turns(c, gem_id)?;
                    Ok(Some(row.into_gem(interaction)))
                }
            }
        })
    }

    /// Distinct workitem ids that have at least one gem. Used by the
    /// stale-render sweep so a deleted prism note can be re-rendered
    /// from canonical even when the current tick produced no new gems.
    pub fn workitem_ids_with_gems(&self) -> Result<Vec<i64>> {
        log::debug!("ledger::workitem_ids_with_gems");
        self.with_conn(|c| {
            let mut stmt = c
                .prepare_cached("SELECT DISTINCT workitem_id FROM gems ORDER BY workitem_id ASC")
                .context("prep workitem_ids_with_gems")?;
            let rows = stmt
                .query_map([], |r| r.get::<_, i64>(0))
                .context("query workitem_ids_with_gems")?;
            let mut out = Vec::new();
            for r in rows {
                out.push(r.context("workitem_id row")?);
            }
            Ok(out)
        })
    }

    /// All gems for a workitem, ordered by extracted_at (then id).
    pub fn gems_for_workitem(&self, workitem_id: i64) -> Result<Vec<Gem>> {
        log::debug!("ledger::gems_for_workitem: workitem_id={workitem_id}");
        self.with_conn(|c| {
            // Collect gem rows in an inner scope so the CachedStatement
            // releases its borrow on the connection before we re-borrow
            // mutably to load each gem's interaction turns.
            let gem_rows: Vec<GemRow> = {
                let mut stmt = c
                    .prepare_cached(
                        "SELECT id, workitem_id, session_uuid, content_hash, \
                                first_user_turn_uuid, last_user_turn_uuid, \
                                task, context_loaded, context_missing, \
                                review_accepted, review_rejected, review_verified_manually, review_rewrote_by_hand, \
                                tags, why_it_matters, extractor_model, extracted_at \
                         FROM gems WHERE workitem_id = ?1 \
                         ORDER BY extracted_at ASC, id ASC",
                    )
                    .context("prep gems_for_workitem")?;
                let mapped = stmt
                    .query_map(rusqlite::params![workitem_id], GemRow::from_row)
                    .context("query gems")?;
                let mut collected = Vec::new();
                for r in mapped {
                    collected.push(r.context("gem row")?);
                }
                collected
            };
            let mut out = Vec::with_capacity(gem_rows.len());
            for row in gem_rows {
                let interaction = load_interaction_turns(c, row.id)?;
                out.push(row.into_gem(interaction));
            }
            Ok(out)
        })
    }
}

fn boundary_user_uuids(interaction: &[InteractionTurn]) -> Result<(&str, &str)> {
    let first = interaction
        .first()
        .ok_or_else(|| eyre::eyre!("upsert_gem: empty interaction is invalid"))?;
    let last = interaction
        .last()
        .ok_or_else(|| eyre::eyre!("upsert_gem: empty interaction is invalid"))?;
    Ok((first.user_turn_uuid.as_str(), last.user_turn_uuid.as_str()))
}

fn compute_content_hash(interaction: &[InteractionTurn]) -> String {
    use sha2::{Digest, Sha256};
    let mut uuids: Vec<&str> = Vec::with_capacity(interaction.len() * 2);
    for turn in interaction {
        uuids.push(turn.ai_turn_uuid.as_str());
        uuids.push(turn.user_turn_uuid.as_str());
    }
    uuids.sort_unstable();
    let mut hasher = Sha256::new();
    for (idx, uuid) in uuids.iter().enumerate() {
        if idx > 0 {
            hasher.update(b"|");
        }
        hasher.update(uuid.as_bytes());
    }
    hex::encode(hasher.finalize())
}

struct GemRow {
    id: i64,
    workitem_id: i64,
    session_uuid: String,
    #[allow(dead_code)]
    content_hash: String,
    #[allow(dead_code)]
    first_user_turn_uuid: String,
    #[allow(dead_code)]
    last_user_turn_uuid: String,
    task: String,
    context_loaded: Vec<String>,
    context_missing: Vec<String>,
    review: Review,
    tags: Vec<String>,
    why_it_matters: String,
    extractor_model: String,
    extracted_at: DateTime<Utc>,
}

impl GemRow {
    fn from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        let context_loaded_json: String = r.get(7)?;
        let context_missing_json: String = r.get(8)?;
        let tags_json: String = r.get(13)?;
        let extracted_at_str: String = r.get(16)?;
        let context_loaded: Vec<String> = serde_json::from_str(&context_loaded_json)
            .map_err(|e| rusqlite::Error::FromSqlConversionFailure(7, rusqlite::types::Type::Text, Box::new(e)))?;
        let context_missing: Vec<String> = serde_json::from_str(&context_missing_json)
            .map_err(|e| rusqlite::Error::FromSqlConversionFailure(8, rusqlite::types::Type::Text, Box::new(e)))?;
        let tags: Vec<String> = serde_json::from_str(&tags_json)
            .map_err(|e| rusqlite::Error::FromSqlConversionFailure(13, rusqlite::types::Type::Text, Box::new(e)))?;
        let extracted_at = DateTime::parse_from_rfc3339(&extracted_at_str)
            .map_err(|e| rusqlite::Error::FromSqlConversionFailure(16, rusqlite::types::Type::Text, Box::new(e)))?
            .with_timezone(&Utc);
        Ok(GemRow {
            id: r.get(0)?,
            workitem_id: r.get(1)?,
            session_uuid: r.get(2)?,
            content_hash: r.get(3)?,
            first_user_turn_uuid: r.get(4)?,
            last_user_turn_uuid: r.get(5)?,
            task: r.get(6)?,
            context_loaded,
            context_missing,
            review: Review {
                accepted: r.get(9)?,
                rejected: r.get(10)?,
                verified_manually: r.get(11)?,
                rewrote_by_hand: r.get(12)?,
            },
            tags,
            why_it_matters: r.get(14)?,
            extractor_model: r.get(15)?,
            extracted_at,
        })
    }

    fn into_gem(self, interaction: Vec<InteractionTurn>) -> Gem {
        Gem {
            id: self.id,
            workitem_id: self.workitem_id,
            session_uuid: self.session_uuid,
            task: self.task,
            context_loaded: self.context_loaded,
            context_missing: self.context_missing,
            interaction,
            review: self.review,
            tags: self.tags,
            why_it_matters: self.why_it_matters,
            extractor_model: self.extractor_model,
            extracted_at: self.extracted_at,
        }
    }
}

fn load_interaction_turns(conn: &mut Connection, gem_id: i64) -> Result<Vec<InteractionTurn>> {
    let mut stmt = conn
        .prepare_cached(
            "SELECT ai_says, ai_turn_uuid, user_says, user_turn_uuid, tags \
             FROM interaction_turns WHERE gem_id = ?1 ORDER BY seq ASC",
        )
        .context("prep load_interaction_turns")?;
    let rows = stmt
        .query_map(rusqlite::params![gem_id], |r| {
            let ai_says: String = r.get(0)?;
            let ai_turn_uuid: String = r.get(1)?;
            let user_says: String = r.get(2)?;
            let user_turn_uuid: String = r.get(3)?;
            let tags_json: String = r.get(4)?;
            let tags: Vec<String> = serde_json::from_str(&tags_json)
                .map_err(|e| rusqlite::Error::FromSqlConversionFailure(4, rusqlite::types::Type::Text, Box::new(e)))?;
            Ok(InteractionTurn {
                ai_says,
                ai_turn_uuid,
                user_says,
                user_turn_uuid,
                tags,
            })
        })
        .context("query interaction_turns")?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r.context("interaction_turn row")?);
    }
    Ok(out)
}
