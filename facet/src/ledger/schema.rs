//! Ledger schema + migrations.
//!
//! Schema version is recorded in `ledger_meta.value WHERE key='schema-version'`.
//! New migrations are appended to the [`MIGRATIONS`] slice; never reorder.
//!
//! Tables (per design doc 2026-05-26-facet-judgment-harvester.md):
//!
//! - `sessions`            - per JSONL session; tracks cluster offset cursor
//! - `work_items`          - cross-session work-item identity
//! - `work_item_repos`     - many-to-many: a work-item can span multiple repos
//! - `session_workitem`    - which sessions contributed to which work-items;
//!   per-(session, workitem) extract cursor
//! - `cluster_assignments` - persisted cluster output so extract retries
//!   never re-call the cluster LLM
//! - `judgment_moments`    - the success surface: one row per extracted moment
//! - `ledger_meta`         - key/value store for schema version, last-tick
//!   timestamps, current-budget-tick-usd

use eyre::{Context, Result};
use rusqlite::Connection;

use super::Ledger;

pub const CURRENT_VERSION: u32 = 1;

/// Idempotent CREATE TABLE statements for schema v1. Run on every open
/// via `CREATE TABLE IF NOT EXISTS`. Future migrations land in
/// [`MIGRATIONS`] as ALTER TABLE / new-table diffs.
const V1_DDL: &str = r#"
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

CREATE TABLE IF NOT EXISTS judgment_moments (
    id INTEGER PRIMARY KEY,
    workitem_id INTEGER NOT NULL REFERENCES work_items(id) ON DELETE CASCADE,
    session_uuid TEXT NOT NULL,
    turn_uuid TEXT NOT NULL,
    mode TEXT NOT NULL,
    ai_move TEXT NOT NULL,
    scott_move TEXT NOT NULL,
    quote_excerpt TEXT NOT NULL,
    why_it_matters TEXT NOT NULL,
    extractor_model TEXT NOT NULL,
    extracted_at TEXT NOT NULL,
    UNIQUE (workitem_id, turn_uuid, mode)
);

CREATE INDEX IF NOT EXISTS idx_moments_mode ON judgment_moments(mode);
CREATE INDEX IF NOT EXISTS idx_moments_workitem ON judgment_moments(workitem_id);
CREATE INDEX IF NOT EXISTS idx_sessions_repo ON sessions(repo_slug);

CREATE TABLE IF NOT EXISTS ledger_meta (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
"#;

/// Sequence of migration steps. Index 0 = v1, etc. Append-only.
const MIGRATIONS: &[&str] = &[V1_DDL];

pub fn migrate(ledger: &Ledger) -> Result<()> {
    ledger.with_conn(|c| {
        let current = current_version(c)?;
        log::debug!("schema::migrate: current_version={current} target={CURRENT_VERSION}");
        if current > CURRENT_VERSION {
            eyre::bail!(
                "facet ledger schema is at v{current} but binary expects v{CURRENT_VERSION}. \
                 Downgrade detected; aborting."
            );
        }
        for v in (current + 1)..=CURRENT_VERSION {
            let stmt = MIGRATIONS
                .get((v - 1) as usize)
                .ok_or_else(|| eyre::eyre!("missing migration for v{v}"))?;
            log::info!("facet ledger: applying schema v{v}");
            let tx = c.transaction().context("begin migration tx")?;
            tx.execute_batch(stmt).context("execute migration")?;
            tx.execute(
                "INSERT OR REPLACE INTO ledger_meta(key, value) VALUES ('schema-version', ?1)",
                rusqlite::params![v.to_string()],
            )
            .context("write schema-version")?;
            tx.commit().context("commit migration")?;
        }
        Ok(())
    })
}

/// Read the current schema version from `ledger_meta`. Returns 0 if the
/// table does not yet exist or the row is missing.
pub fn current_version(conn: &Connection) -> Result<u32> {
    let table_exists: u32 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='ledger_meta'",
            [],
            |r| r.get(0),
        )
        .context("check ledger_meta exists")?;
    if table_exists == 0 {
        return Ok(0);
    }
    let v: Option<String> = conn
        .query_row("SELECT value FROM ledger_meta WHERE key='schema-version'", [], |r| {
            r.get(0)
        })
        .ok()
        .flatten();
    match v {
        Some(s) => s.parse::<u32>().context("parse schema-version"),
        None => Ok(0),
    }
}
