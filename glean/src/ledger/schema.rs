//! Ledger DDL. Applied idempotently on every open.
//!
//! Three tables, no foreign keys between them: `sessions` is upsert-
//! by-(session_uuid), `quarantine` is append-only, `work_items` is
//! truncate-and-rebuild on every cluster pass.

use eyre::{Context, Result};

use super::Ledger;

const DDL: &str = r#"
CREATE TABLE IF NOT EXISTS sessions (
    session_uuid              TEXT PRIMARY KEY,
    jsonl_path                TEXT NOT NULL,
    jsonl_sha256              TEXT NOT NULL,
    repo_slug                 TEXT,
    repo_path                 TEXT,
    cwd                       TEXT,
    started_at                TEXT NOT NULL,
    ended_at                  TEXT NOT NULL,
    design_doc_files          TEXT NOT NULL,
    skill_invocations         TEXT NOT NULL,
    interaction_normalized    TEXT NOT NULL,
    summary_one_line          TEXT NOT NULL,
    theme_tags                TEXT NOT NULL,
    design_doc_focus          TEXT,
    is_orphan                 INTEGER NOT NULL CHECK (is_orphan IN (0, 1)),
    classified_at             TEXT NOT NULL,
    classifier_model          TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_sessions_repo  ON sessions(repo_slug);
CREATE INDEX IF NOT EXISTS idx_sessions_focus ON sessions(design_doc_focus);

CREATE TABLE IF NOT EXISTS quarantine (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    session_uuid    TEXT NOT NULL,
    jsonl_path      TEXT NOT NULL,
    reason          TEXT NOT NULL,
    quarantined_at  TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_quarantine_session ON quarantine(session_uuid);

CREATE TABLE IF NOT EXISTS work_items (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    key_type          TEXT NOT NULL CHECK (key_type IN ('design-doc','theme','singleton')),
    key_value         TEXT NOT NULL,
    repo_slug         TEXT,
    content_hash      TEXT NOT NULL,
    session_uuids     TEXT NOT NULL,
    time_start        TEXT NOT NULL,
    time_end          TEXT NOT NULL,
    aggregated_tags   TEXT NOT NULL,
    materialized_at   TEXT NOT NULL,
    UNIQUE (content_hash)
);
CREATE INDEX IF NOT EXISTS idx_work_items_key_type ON work_items(key_type);
CREATE INDEX IF NOT EXISTS idx_work_items_key_value ON work_items(key_value);

CREATE TABLE IF NOT EXISTS ledger_meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
"#;

pub fn apply(ledger: &Ledger) -> Result<()> {
    ledger.with_conn(|c| {
        log::debug!("schema::apply");
        c.execute_batch(DDL).context("apply schema DDL")?;
        Ok(())
    })
}
