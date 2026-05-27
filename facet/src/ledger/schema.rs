//! Ledger schema. Single DDL applied idempotently on every open.
//!
//! Every statement is `CREATE TABLE IF NOT EXISTS` / `CREATE INDEX IF
//! NOT EXISTS`, so re-applying against an existing database is a no-op.

use eyre::{Context, Result};

use super::Ledger;

/// Full schema. All tables the daemon writes to or reads from. Index
/// statements follow each table for locality.
const DDL: &str = r#"
CREATE TABLE IF NOT EXISTS sessions (
    session_uuid TEXT PRIMARY KEY,
    cwd TEXT NOT NULL,
    repo_slug TEXT,
    first_seen_at TEXT NOT NULL,
    last_seen_at TEXT NOT NULL,
    last_cluster_offset INTEGER NOT NULL DEFAULT 0,
    last_cluster_turn_uuid TEXT,
    failure_count INTEGER NOT NULL DEFAULT 0,
    last_failure_reason TEXT,
    last_failure_stage TEXT
);

CREATE INDEX IF NOT EXISTS idx_sessions_repo ON sessions(repo_slug);

CREATE TABLE IF NOT EXISTS work_items (
    id INTEGER PRIMARY KEY,
    slug TEXT NOT NULL UNIQUE,
    title TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('active','dormant','archived')),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    dormant_since TEXT
);

CREATE TABLE IF NOT EXISTS work_item_repos (
    workitem_id INTEGER NOT NULL REFERENCES work_items(id) ON DELETE CASCADE,
    repo_slug TEXT NOT NULL,
    PRIMARY KEY (workitem_id, repo_slug)
);

CREATE TABLE IF NOT EXISTS session_workitem (
    session_uuid TEXT NOT NULL REFERENCES sessions(session_uuid) ON DELETE CASCADE,
    workitem_id INTEGER NOT NULL REFERENCES work_items(id) ON DELETE CASCADE,
    first_contribution_at TEXT NOT NULL,
    last_contribution_at TEXT NOT NULL,
    last_extract_turn_uuid TEXT,
    PRIMARY KEY (session_uuid, workitem_id)
);

CREATE TABLE IF NOT EXISTS cluster_assignments (
    id INTEGER PRIMARY KEY,
    session_uuid TEXT NOT NULL REFERENCES sessions(session_uuid) ON DELETE CASCADE,
    workitem_id INTEGER NOT NULL REFERENCES work_items(id) ON DELETE CASCADE,
    first_turn_uuid TEXT NOT NULL,
    last_turn_uuid TEXT NOT NULL,
    clustered_at TEXT NOT NULL,
    cluster_model TEXT NOT NULL,
    extracted INTEGER NOT NULL DEFAULT 0 CHECK (extracted IN (0, 1)),
    UNIQUE (session_uuid, first_turn_uuid, last_turn_uuid)
);

CREATE INDEX IF NOT EXISTS idx_cluster_pending
    ON cluster_assignments(extracted) WHERE extracted = 0;

CREATE TABLE IF NOT EXISTS ledger_meta (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

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

/// Apply the schema idempotently. Safe to call on every open.
pub fn apply(ledger: &Ledger) -> Result<()> {
    ledger.with_conn(|c| {
        log::debug!("schema::apply");
        c.execute_batch(DDL).context("apply schema DDL")?;
        Ok(())
    })
}
