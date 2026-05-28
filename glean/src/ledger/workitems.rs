//! `work_items` table CRUD. Truncate-and-rebuild on every cluster pass.

use chrono::{DateTime, Utc};
use eyre::{Context, Result};
use rusqlite::OptionalExtension;

use super::Ledger;
use crate::types::{WorkItem, WorkItemKey};

impl Ledger {
    /// Replace the entire `work_items` table with `items`. Used by
    /// the cluster stage as part of one transaction so a partial
    /// failure leaves the prior state intact.
    pub fn replace_work_items(&self, items: &[WorkItem]) -> Result<()> {
        log::info!("ledger::replace_work_items: n={}", items.len());
        self.with_tx(|tx| {
            tx.execute("DELETE FROM work_items", [])
                .context("truncate work_items")?;
            for item in items {
                let session_uuids = serde_json::to_string(&item.session_uuids).context("encode session_uuids")?;
                let aggregated_tags = serde_json::to_string(&item.aggregated_tags).context("encode aggregated_tags")?;
                tx.execute(
                    "INSERT INTO work_items(\
                        key_type, key_value, repo_slug, content_hash, session_uuids, \
                        time_start, time_end, aggregated_tags, materialized_at\
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                    rusqlite::params![
                        item.key_type.as_str(),
                        item.key_value,
                        item.repo_slug,
                        item.content_hash,
                        session_uuids,
                        item.time_start.to_rfc3339(),
                        item.time_end.to_rfc3339(),
                        aggregated_tags,
                        item.materialized_at.to_rfc3339(),
                    ],
                )
                .context("insert work_item")?;
            }
            Ok(())
        })
    }

    pub fn all_work_items(&self) -> Result<Vec<WorkItem>> {
        log::debug!("ledger::all_work_items");
        self.with_conn(|c| {
            let mut stmt = c
                .prepare(
                    "SELECT id, key_type, key_value, repo_slug, content_hash, session_uuids, \
                            time_start, time_end, aggregated_tags, materialized_at \
                     FROM work_items ORDER BY time_start",
                )
                .context("prep all_work_items")?;
            let rows = stmt.query_map([], row_to_work_item).context("query all_work_items")?;
            let mut out = Vec::new();
            for r in rows {
                out.push(r.context("row all_work_items")?);
            }
            Ok(out)
        })
    }

    pub fn get_work_item_by_content_hash(&self, content_hash: &str) -> Result<Option<WorkItem>> {
        log::debug!(
            "ledger::get_work_item_by_content_hash: content_hash={}",
            &content_hash[..8.min(content_hash.len())]
        );
        self.with_conn(|c| {
            c.query_row(
                "SELECT id, key_type, key_value, repo_slug, content_hash, session_uuids, \
                        time_start, time_end, aggregated_tags, materialized_at \
                 FROM work_items WHERE content_hash = ?1",
                rusqlite::params![content_hash],
                row_to_work_item,
            )
            .optional()
            .context("get_work_item_by_content_hash")
        })
    }
}

fn row_to_work_item(r: &rusqlite::Row<'_>) -> rusqlite::Result<WorkItem> {
    let key_type: String = r.get(1)?;
    let key_type = WorkItemKey::parse(&key_type).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            1,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::other(format!("unknown work_item key_type: {key_type}"))),
        )
    })?;
    let session_uuids_raw: String = r.get(5)?;
    let aggregated_tags_raw: String = r.get(8)?;
    let session_uuids: Vec<String> = serde_json::from_str(&session_uuids_raw)
        .map_err(|e| rusqlite::Error::FromSqlConversionFailure(5, rusqlite::types::Type::Text, Box::new(e)))?;
    let aggregated_tags: Vec<String> = serde_json::from_str(&aggregated_tags_raw)
        .map_err(|e| rusqlite::Error::FromSqlConversionFailure(8, rusqlite::types::Type::Text, Box::new(e)))?;
    let time_start: String = r.get(6)?;
    let time_end: String = r.get(7)?;
    let materialized_at: String = r.get(9)?;
    Ok(WorkItem {
        id: r.get(0)?,
        key_type,
        key_value: r.get(2)?,
        repo_slug: r.get(3)?,
        content_hash: r.get(4)?,
        session_uuids,
        time_start: parse_rfc3339(&time_start, 6)?,
        time_end: parse_rfc3339(&time_end, 7)?,
        aggregated_tags,
        materialized_at: parse_rfc3339(&materialized_at, 9)?,
    })
}

fn parse_rfc3339(s: &str, col: usize) -> rusqlite::Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .map(|d| d.with_timezone(&Utc))
        .map_err(|e| rusqlite::Error::FromSqlConversionFailure(col, rusqlite::types::Type::Text, Box::new(e)))
}
