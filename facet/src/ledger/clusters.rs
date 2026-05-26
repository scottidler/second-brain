//! `cluster_assignments` accessors. Each row records one cluster-LLM
//! decision: a contiguous turn range in one session is assigned to one
//! work-item. The row stays around through extract; on success the
//! `extracted` flag flips to 1.

use chrono::{DateTime, Utc};
use eyre::{Context, Result};

use super::Ledger;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClusterAssignmentRow {
    pub id: i64,
    pub session_uuid: String,
    pub workitem_id: i64,
    pub first_turn_uuid: String,
    pub last_turn_uuid: String,
    pub clustered_at: DateTime<Utc>,
    pub cluster_model: String,
    pub extracted: bool,
}

#[derive(Debug, Clone)]
pub struct NewClusterAssignment<'a> {
    pub session_uuid: &'a str,
    pub workitem_id: i64,
    pub first_turn_uuid: &'a str,
    pub last_turn_uuid: &'a str,
    pub clustered_at: DateTime<Utc>,
    pub cluster_model: &'a str,
}

impl Ledger {
    /// Insert a new cluster assignment, returning its rowid. The UNIQUE
    /// (session_uuid, first_turn_uuid, last_turn_uuid) constraint makes
    /// re-clusters of the same range idempotent.
    pub fn insert_cluster_assignment(&self, n: NewClusterAssignment<'_>) -> Result<i64> {
        log::debug!(
            "ledger::insert_cluster_assignment: session_uuid={} workitem_id={} first={} last={}",
            n.session_uuid,
            n.workitem_id,
            n.first_turn_uuid,
            n.last_turn_uuid
        );
        self.with_conn(|c| {
            c.execute(
                "INSERT OR IGNORE INTO cluster_assignments \
                    (session_uuid, workitem_id, first_turn_uuid, last_turn_uuid, clustered_at, cluster_model, extracted) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0)",
                rusqlite::params![
                    n.session_uuid,
                    n.workitem_id,
                    n.first_turn_uuid,
                    n.last_turn_uuid,
                    n.clustered_at.to_rfc3339(),
                    n.cluster_model,
                ],
            )
            .context("insert cluster_assignment")?;
            let id: i64 = c
                .query_row(
                    "SELECT id FROM cluster_assignments \
                     WHERE session_uuid = ?1 AND first_turn_uuid = ?2 AND last_turn_uuid = ?3",
                    rusqlite::params![n.session_uuid, n.first_turn_uuid, n.last_turn_uuid],
                    |r| r.get(0),
                )
                .context("lookup cluster_assignment id")?;
            Ok(id)
        })
    }

    pub fn pending_cluster_assignments(&self, limit: u32) -> Result<Vec<ClusterAssignmentRow>> {
        log::debug!("ledger::pending_cluster_assignments: limit={limit}");
        self.with_conn(|c| {
            let mut stmt = c
                .prepare(
                    "SELECT id, session_uuid, workitem_id, first_turn_uuid, last_turn_uuid, \
                            clustered_at, cluster_model, extracted \
                     FROM cluster_assignments \
                     WHERE extracted = 0 \
                     ORDER BY clustered_at ASC \
                     LIMIT ?1",
                )
                .context("prep pending")?;
            let rows = stmt
                .query_map(rusqlite::params![limit as i64], row_to_cluster_assignment)
                .context("query pending")?;
            let mut out = Vec::new();
            for r in rows {
                out.push(r.context("pending row")?);
            }
            Ok(out)
        })
    }

    pub fn mark_extracted(&self, cluster_assignment_id: i64) -> Result<()> {
        log::debug!("ledger::mark_extracted: id={cluster_assignment_id}");
        self.with_conn(|c| {
            c.execute(
                "UPDATE cluster_assignments SET extracted = 1 WHERE id = ?1",
                rusqlite::params![cluster_assignment_id],
            )
            .context("mark_extracted update")?;
            Ok(())
        })
    }
}

fn row_to_cluster_assignment(r: &rusqlite::Row<'_>) -> rusqlite::Result<ClusterAssignmentRow> {
    let clustered_at: String = r.get(5)?;
    let extracted: i64 = r.get(7)?;
    Ok(ClusterAssignmentRow {
        id: r.get(0)?,
        session_uuid: r.get(1)?,
        workitem_id: r.get(2)?,
        first_turn_uuid: r.get(3)?,
        last_turn_uuid: r.get(4)?,
        clustered_at: DateTime::parse_from_rfc3339(&clustered_at)
            .map_err(|e| rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e)))?
            .with_timezone(&Utc),
        cluster_model: r.get(6)?,
        extracted: extracted != 0,
    })
}
