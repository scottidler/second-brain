//! `judgment_moments` accessors. The success surface.

use chrono::{DateTime, Utc};
use eyre::{Context, Result};

use super::Ledger;
use crate::extract::JudgmentMoment;

#[derive(Debug, Clone)]
pub struct NewJudgmentMoment<'a> {
    pub workitem_id: i64,
    pub session_uuid: &'a str,
    pub turn_uuid: &'a str,
    pub mode: &'a str,
    pub ai_move: &'a str,
    pub scott_move: &'a str,
    pub quote_excerpt: &'a str,
    pub why_it_matters: &'a str,
    pub extractor_model: &'a str,
    pub extracted_at: DateTime<Utc>,
}

impl Ledger {
    /// Insert a judgment moment. The UNIQUE (workitem_id, turn_uuid, mode)
    /// constraint makes re-extraction of the same moment a no-op.
    pub fn insert_moment(&self, m: NewJudgmentMoment<'_>) -> Result<()> {
        log::debug!(
            "ledger::insert_moment: workitem_id={} turn_uuid={} mode={}",
            m.workitem_id,
            m.turn_uuid,
            m.mode
        );
        self.with_conn(|c| {
            c.execute(
                "INSERT OR IGNORE INTO judgment_moments \
                    (workitem_id, session_uuid, turn_uuid, mode, ai_move, scott_move, \
                     quote_excerpt, why_it_matters, extractor_model, extracted_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                rusqlite::params![
                    m.workitem_id,
                    m.session_uuid,
                    m.turn_uuid,
                    m.mode,
                    m.ai_move,
                    m.scott_move,
                    m.quote_excerpt,
                    m.why_it_matters,
                    m.extractor_model,
                    m.extracted_at.to_rfc3339(),
                ],
            )
            .context("insert judgment_moment")?;
            Ok(())
        })
    }

    pub fn moments_for_workitem(&self, workitem_id: i64) -> Result<Vec<JudgmentMoment>> {
        log::debug!("ledger::moments_for_workitem: workitem_id={workitem_id}");
        self.with_conn(|c| {
            let mut stmt = c
                .prepare_cached(
                    "SELECT id, workitem_id, session_uuid, turn_uuid, mode, ai_move, scott_move, \
                            quote_excerpt, why_it_matters, extracted_at, extractor_model \
                     FROM judgment_moments \
                     WHERE workitem_id = ?1 \
                     ORDER BY extracted_at ASC, id ASC",
                )
                .context("prep moments_for_workitem")?;
            let rows = stmt
                .query_map(rusqlite::params![workitem_id], row_to_moment)
                .context("query moments")?;
            let mut out = Vec::new();
            for r in rows {
                out.push(r.context("moment row")?);
            }
            Ok(out)
        })
    }

    pub fn moments_by_mode(&self, mode: &str, window_days: u32, limit: u32) -> Result<Vec<JudgmentMoment>> {
        log::debug!("ledger::moments_by_mode: mode={mode} window_days={window_days} limit={limit}");
        let cutoff = Utc::now() - chrono::Duration::days(window_days as i64);
        self.with_conn(|c| {
            let mut stmt = c
                .prepare_cached(
                    "SELECT id, workitem_id, session_uuid, turn_uuid, mode, ai_move, scott_move, \
                            quote_excerpt, why_it_matters, extracted_at, extractor_model \
                     FROM judgment_moments \
                     WHERE mode = ?1 AND extracted_at >= ?2 \
                     ORDER BY extracted_at DESC \
                     LIMIT ?3",
                )
                .context("prep moments_by_mode")?;
            let rows = stmt
                .query_map(
                    rusqlite::params![mode, cutoff.to_rfc3339(), limit as i64],
                    row_to_moment,
                )
                .context("query moments_by_mode")?;
            let mut out = Vec::new();
            for r in rows {
                out.push(r.context("by_mode row")?);
            }
            Ok(out)
        })
    }
}

fn row_to_moment(r: &rusqlite::Row<'_>) -> rusqlite::Result<JudgmentMoment> {
    let extracted_at: String = r.get(9)?;
    Ok(JudgmentMoment {
        id: r.get(0)?,
        workitem_id: r.get(1)?,
        session_uuid: r.get(2)?,
        turn_uuid: r.get(3)?,
        mode: r.get(4)?,
        ai_move: r.get(5)?,
        scott_move: r.get(6)?,
        quote_excerpt: r.get(7)?,
        why_it_matters: r.get(8)?,
        extracted_at: DateTime::parse_from_rfc3339(&extracted_at)
            .map_err(|e| rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e)))?
            .with_timezone(&Utc),
        extractor_model: r.get(10)?,
    })
}
