//! `quarantine` table CRUD. Append-only.

use chrono::{DateTime, Utc};
use eyre::{Context, Result};
use rusqlite::OptionalExtension;
use std::path::{Path, PathBuf};

use super::Ledger;
use crate::types::QuarantineRecord;

impl Ledger {
    /// Append one quarantine row. Same session can appear multiple
    /// times if the failure reason changed across runs.
    pub fn insert_quarantine(&self, session_uuid: &str, jsonl_path: &Path, reason: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        log::warn!("ledger::insert_quarantine: session_uuid={session_uuid} reason={reason}");
        self.with_conn(|c| {
            c.execute(
                "INSERT INTO quarantine(session_uuid, jsonl_path, reason, quarantined_at) \
                 VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![session_uuid, jsonl_path.to_string_lossy(), reason, now],
            )
            .context("insert quarantine row")?;
            Ok(())
        })
    }

    pub fn list_quarantine(&self) -> Result<Vec<QuarantineRecord>> {
        log::debug!("ledger::list_quarantine");
        self.with_conn(|c| {
            let mut stmt = c
                .prepare(
                    "SELECT id, session_uuid, jsonl_path, reason, quarantined_at \
                     FROM quarantine ORDER BY id",
                )
                .context("prep list_quarantine")?;
            let rows = stmt.query_map([], row_to_quarantine).context("query list_quarantine")?;
            let mut out = Vec::new();
            for r in rows {
                out.push(r.context("row list_quarantine")?);
            }
            Ok(out)
        })
    }

    pub fn drop_quarantine(&self, session_uuid: &str) -> Result<usize> {
        log::info!("ledger::drop_quarantine: session_uuid={session_uuid}");
        self.with_conn(|c| {
            let n = c
                .execute(
                    "DELETE FROM quarantine WHERE session_uuid = ?1",
                    rusqlite::params![session_uuid],
                )
                .context("delete quarantine")?;
            Ok(n)
        })
    }

    pub fn get_quarantine_for(&self, session_uuid: &str) -> Result<Option<QuarantineRecord>> {
        self.with_conn(|c| {
            c.query_row(
                "SELECT id, session_uuid, jsonl_path, reason, quarantined_at \
                 FROM quarantine WHERE session_uuid = ?1 ORDER BY id DESC LIMIT 1",
                rusqlite::params![session_uuid],
                row_to_quarantine,
            )
            .optional()
            .context("get_quarantine_for")
        })
    }
}

fn row_to_quarantine(r: &rusqlite::Row<'_>) -> rusqlite::Result<QuarantineRecord> {
    let jsonl_path: String = r.get(2)?;
    let quarantined_at: String = r.get(4)?;
    Ok(QuarantineRecord {
        id: r.get(0)?,
        session_uuid: r.get(1)?,
        jsonl_path: PathBuf::from(jsonl_path),
        reason: r.get(3)?,
        quarantined_at: DateTime::parse_from_rfc3339(&quarantined_at)
            .map(|d| d.with_timezone(&Utc))
            .map_err(|e| rusqlite::Error::FromSqlConversionFailure(4, rusqlite::types::Type::Text, Box::new(e)))?,
    })
}
