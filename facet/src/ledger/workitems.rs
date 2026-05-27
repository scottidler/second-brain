//! `work_items`, `work_item_repos`, `session_workitem` accessors.

use chrono::{DateTime, Utc};
use eyre::{Context, Result};
use rusqlite::OptionalExtension;
use std::str::FromStr;

use super::Ledger;
use crate::workitem::{WorkItem, WorkItemStatus};

#[derive(Debug, Clone)]
pub struct NewWorkItem<'a> {
    pub slug: &'a str,
    pub title: &'a str,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct SessionContribution<'a> {
    pub session_uuid: &'a str,
    pub workitem_id: i64,
    pub at: DateTime<Utc>,
}

impl Ledger {
    /// Insert a new work-item, returning the assigned rowid. Caller is
    /// responsible for slug uniqueness; if the slug already exists this
    /// errors via the UNIQUE constraint.
    pub fn insert_workitem(&self, n: NewWorkItem<'_>) -> Result<i64> {
        log::debug!("ledger::insert_workitem: slug={} title_len={}", n.slug, n.title.len());
        self.with_conn(|c| {
            c.execute(
                "INSERT INTO work_items(slug, title, status, created_at, updated_at) \
                 VALUES (?1, ?2, 'active', ?3, ?3)",
                rusqlite::params![n.slug, n.title, n.created_at.to_rfc3339()],
            )
            .context("insert work_item")?;
            Ok(c.last_insert_rowid())
        })
    }

    pub fn workitem_by_slug(&self, slug: &str) -> Result<Option<WorkItem>> {
        self.with_conn(|c| query_workitem(c, "slug = ?1", rusqlite::params![slug]))
    }

    pub fn workitem_by_id(&self, id: i64) -> Result<Option<WorkItem>> {
        self.with_conn(|c| query_workitem(c, "id = ?1", rusqlite::params![id]))
    }

    /// Add a repo affinity to a work-item. Idempotent on the
    /// (workitem_id, repo_slug) composite key.
    pub fn link_workitem_repo(&self, workitem_id: i64, repo_slug: &str) -> Result<()> {
        log::debug!(
            "ledger::link_workitem_repo: workitem_id={} repo_slug={}",
            workitem_id,
            repo_slug
        );
        self.with_conn(|c| {
            c.execute(
                "INSERT OR IGNORE INTO work_item_repos(workitem_id, repo_slug) VALUES (?1, ?2)",
                rusqlite::params![workitem_id, repo_slug],
            )
            .context("insert work_item_repos")?;
            Ok(())
        })
    }

    /// Record that a session contributed to a work-item. First contribution
    /// inserts; subsequent contributions advance `last_contribution_at`.
    pub fn record_contribution(&self, c: SessionContribution<'_>) -> Result<()> {
        log::debug!(
            "ledger::record_contribution: session_uuid={} workitem_id={}",
            c.session_uuid,
            c.workitem_id
        );
        self.with_conn(|conn| {
            conn.execute(
                "INSERT INTO session_workitem(session_uuid, workitem_id, first_contribution_at, last_contribution_at) \
                 VALUES (?1, ?2, ?3, ?3) \
                 ON CONFLICT(session_uuid, workitem_id) DO UPDATE SET \
                    last_contribution_at = excluded.last_contribution_at",
                rusqlite::params![c.session_uuid, c.workitem_id, c.at.to_rfc3339()],
            )
            .context("insert session_workitem")?;
            Ok(())
        })
    }

    /// Advance the per-(session, workitem) extract cursor. Called by the
    /// extract stage on success so retries pick up after the last
    /// extracted range, not from offset 0.
    pub fn set_last_extract_turn_uuid(&self, session_uuid: &str, workitem_id: i64, last_turn_uuid: &str) -> Result<()> {
        log::debug!(
            "ledger::set_last_extract_turn_uuid: session_uuid={} workitem_id={} last_turn_uuid={}",
            session_uuid,
            workitem_id,
            last_turn_uuid
        );
        self.with_conn(|c| {
            c.execute(
                "UPDATE session_workitem SET last_extract_turn_uuid = ?3 \
                 WHERE session_uuid = ?1 AND workitem_id = ?2",
                rusqlite::params![session_uuid, workitem_id, last_turn_uuid],
            )
            .context("update last_extract_turn_uuid")?;
            Ok(())
        })
    }

    /// Mark work-items dormant whose `last_contribution_at` is older than
    /// `now - inactive_days * 86400`. Returns the number of rows flipped.
    pub fn mark_dormant(&self, now: DateTime<Utc>, inactive_days: u32) -> Result<u32> {
        log::debug!("ledger::mark_dormant: now={now} inactive_days={inactive_days}");
        let cutoff = now - chrono::Duration::days(inactive_days as i64);
        self.with_conn(|c| {
            let updated = c
                .execute(
                    "UPDATE work_items \
                 SET status = 'dormant', dormant_since = ?2 \
                 WHERE status = 'active' \
                   AND id IN ( \
                     SELECT w.id FROM work_items w \
                     LEFT JOIN session_workitem sw ON sw.workitem_id = w.id \
                     GROUP BY w.id \
                     HAVING COALESCE(MAX(sw.last_contribution_at), w.updated_at) < ?1 \
                   )",
                    rusqlite::params![cutoff.to_rfc3339(), now.to_rfc3339()],
                )
                .context("mark_dormant update")?;
            Ok(updated as u32)
        })
    }
}

fn query_workitem(
    conn: &rusqlite::Connection,
    where_clause: &str,
    params: &[&dyn rusqlite::ToSql],
) -> Result<Option<WorkItem>> {
    let sql = format!(
        "SELECT id, slug, title, status, created_at, updated_at, dormant_since \
         FROM work_items WHERE {where_clause}"
    );
    let base = conn
        .query_row(&sql, params, row_to_partial_workitem)
        .optional()
        .context("query work_item")?;
    let Some(partial) = base else { return Ok(None) };
    let repos = workitem_repos(conn, partial.id)?;
    let sessions_count = sessions_for_workitem(conn, partial.id)?;
    let modes_present = modes_for_workitem(conn, partial.id)?;
    Ok(Some(WorkItem {
        id: partial.id,
        slug: partial.slug,
        title: partial.title,
        repos,
        status: partial.status,
        created_at: partial.created_at,
        updated_at: partial.updated_at,
        dormant_since: partial.dormant_since,
        sessions_count,
        modes_present,
    }))
}

struct PartialWorkItem {
    id: i64,
    slug: String,
    title: String,
    status: WorkItemStatus,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    dormant_since: Option<DateTime<Utc>>,
}

fn row_to_partial_workitem(r: &rusqlite::Row<'_>) -> rusqlite::Result<PartialWorkItem> {
    let status_str: String = r.get(3)?;
    let created_at: String = r.get(4)?;
    let updated_at: String = r.get(5)?;
    let dormant_since: Option<String> = r.get(6)?;
    Ok(PartialWorkItem {
        id: r.get(0)?,
        slug: r.get(1)?,
        title: r.get(2)?,
        status: WorkItemStatus::from_str(&status_str)
            .map_err(|e| rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, e.into()))?,
        created_at: parse_dt(&created_at)?,
        updated_at: parse_dt(&updated_at)?,
        dormant_since: dormant_since.as_deref().map(parse_dt).transpose()?,
    })
}

fn parse_dt(s: &str) -> rusqlite::Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e)))
}

fn workitem_repos(conn: &rusqlite::Connection, id: i64) -> Result<Vec<String>> {
    let mut stmt = conn
        .prepare_cached("SELECT repo_slug FROM work_item_repos WHERE workitem_id = ?1 ORDER BY repo_slug")
        .context("prep workitem_repos")?;
    let rows = stmt
        .query_map(rusqlite::params![id], |r| r.get::<_, String>(0))
        .context("query workitem_repos")?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r.context("workitem_repos row")?);
    }
    Ok(out)
}

fn sessions_for_workitem(conn: &rusqlite::Connection, id: i64) -> Result<u32> {
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM session_workitem WHERE workitem_id = ?1",
            rusqlite::params![id],
            |r| r.get(0),
        )
        .context("count session_workitem")?;
    Ok(n as u32)
}

fn modes_for_workitem(conn: &rusqlite::Connection, id: i64) -> Result<Vec<String>> {
    let mut stmt = conn
        .prepare_cached("SELECT tags FROM gems WHERE workitem_id = ?1")
        .context("prep modes")?;
    let rows = stmt
        .query_map(rusqlite::params![id], |r| r.get::<_, String>(0))
        .context("query modes")?;
    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for r in rows {
        let tags_json = r.context("modes row")?;
        let tags: Vec<String> = serde_json::from_str(&tags_json).unwrap_or_default();
        for t in tags {
            seen.insert(t);
        }
    }
    Ok(seen.into_iter().collect())
}
