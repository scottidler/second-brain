//! `sessions` table accessors.
//!
//! Tracks per-JSONL-session metadata plus the byte-offset cursor that
//! the cluster stage advances on success.

use chrono::{DateTime, Utc};
use eyre::{Context, Result};
use rusqlite::OptionalExtension;

use super::Ledger;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRow {
    pub session_uuid: String,
    pub cwd: String,
    pub repo_slug: Option<String>,
    pub first_seen_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    pub last_cluster_offset: u64,
    pub last_cluster_turn_uuid: Option<String>,
    pub failure_count: u32,
    pub last_failure_reason: Option<String>,
    pub last_failure_stage: Option<String>,
}

#[derive(Debug, Clone)]
pub struct UpsertSession<'a> {
    pub session_uuid: &'a str,
    pub cwd: &'a str,
    pub repo_slug: Option<&'a str>,
    pub seen_at: DateTime<Utc>,
}

impl Ledger {
    /// Insert a new session row or update its `last_seen_at`. Preserves
    /// `last_cluster_offset`, `last_cluster_turn_uuid`, and failure
    /// columns across re-upserts (a session is "seen" before every tick;
    /// the cursor advances only on cluster success).
    pub fn upsert_session(&self, u: UpsertSession<'_>) -> Result<()> {
        log::debug!(
            "ledger::upsert_session: session_uuid={} cwd={} repo_slug={:?}",
            u.session_uuid,
            u.cwd,
            u.repo_slug
        );
        self.with_conn(|c| {
            c.execute(
                "INSERT INTO sessions(session_uuid, cwd, repo_slug, first_seen_at, last_seen_at) \
                 VALUES (?1, ?2, ?3, ?4, ?4) \
                 ON CONFLICT(session_uuid) DO UPDATE SET \
                    cwd = excluded.cwd, \
                    repo_slug = excluded.repo_slug, \
                    last_seen_at = excluded.last_seen_at",
                rusqlite::params![u.session_uuid, u.cwd, u.repo_slug, u.seen_at.to_rfc3339(),],
            )
            .context("upsert sessions row")?;
            Ok(())
        })
    }

    pub fn get_session(&self, session_uuid: &str) -> Result<Option<SessionRow>> {
        self.with_conn(|c| {
            c.query_row(
                "SELECT session_uuid, cwd, repo_slug, first_seen_at, last_seen_at, \
                        last_cluster_offset, last_cluster_turn_uuid, failure_count, \
                        last_failure_reason, last_failure_stage \
                 FROM sessions WHERE session_uuid = ?1",
                rusqlite::params![session_uuid],
                row_to_session,
            )
            .optional()
            .context("query sessions row")
        })
    }

    /// Advance the cluster cursor on success. Sets the byte offset and
    /// the last-turn-uuid pointer.
    pub fn set_cluster_offset(&self, session_uuid: &str, offset: u64, last_turn_uuid: Option<&str>) -> Result<()> {
        log::debug!(
            "ledger::set_cluster_offset: session_uuid={} offset={} last_turn_uuid={:?}",
            session_uuid,
            offset,
            last_turn_uuid
        );
        self.with_conn(|c| {
            c.execute(
                "UPDATE sessions SET last_cluster_offset = ?2, last_cluster_turn_uuid = ?3 \
                 WHERE session_uuid = ?1",
                rusqlite::params![session_uuid, offset as i64, last_turn_uuid],
            )
            .context("update last_cluster_offset")?;
            Ok(())
        })
    }

    /// Record a stage failure on the session. Bumps the count and
    /// stores the most recent reason + stage.
    pub fn record_session_failure(&self, session_uuid: &str, stage: &str, reason: &str) -> Result<()> {
        log::warn!(
            "ledger::record_session_failure: session_uuid={} stage={} reason={}",
            session_uuid,
            stage,
            reason
        );
        self.with_conn(|c| {
            c.execute(
                "UPDATE sessions \
                 SET failure_count = failure_count + 1, \
                     last_failure_stage = ?2, \
                     last_failure_reason = ?3 \
                 WHERE session_uuid = ?1",
                rusqlite::params![session_uuid, stage, reason],
            )
            .context("update sessions failure")?;
            Ok(())
        })
    }
}

fn row_to_session(r: &rusqlite::Row<'_>) -> rusqlite::Result<SessionRow> {
    let first_seen_at: String = r.get(3)?;
    let last_seen_at: String = r.get(4)?;
    let offset: i64 = r.get(5)?;
    Ok(SessionRow {
        session_uuid: r.get(0)?,
        cwd: r.get(1)?,
        repo_slug: r.get(2)?,
        first_seen_at: DateTime::parse_from_rfc3339(&first_seen_at)
            .map_err(|e| rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e)))?
            .with_timezone(&Utc),
        last_seen_at: DateTime::parse_from_rfc3339(&last_seen_at)
            .map_err(|e| rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e)))?
            .with_timezone(&Utc),
        last_cluster_offset: offset as u64,
        last_cluster_turn_uuid: r.get(6)?,
        failure_count: r.get::<_, i64>(7)? as u32,
        last_failure_reason: r.get(8)?,
        last_failure_stage: r.get(9)?,
    })
}
