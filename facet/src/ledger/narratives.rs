//! `narratives` and `narrative_axes` accessors.
//!
//! Idempotency contract: `UNIQUE (cluster_key)` on `narratives`.
//! Re-narrating the same cluster upserts the existing row and bumps
//! `revision`. The slug may drift on title revision; the cluster_key
//! is the stable identity.

use chrono::{DateTime, Utc};
use eyre::{Context, Result};

use super::Ledger;
use crate::narrative::{Archetype, Narrative, NarrativeAxes};

#[cfg(test)]
mod tests;

#[derive(Debug, Clone)]
pub struct NewNarrative<'a> {
    pub cluster_key: &'a str,
    pub archetype: Archetype,
    pub slug: &'a str,
    pub title: &'a str,
    pub thesis: &'a str,
    pub body_md: &'a str,
    pub gem_ids: &'a [i64],
    pub axes: &'a NarrativeAxes,
    pub synthesised_at: DateTime<Utc>,
    pub synthesiser_model: &'a str,
}

impl Ledger {
    /// Upsert a narrative + its axes sidecar. Returns the narrative id.
    /// On conflict (cluster_key): updates payload + bumps revision.
    pub fn upsert_narrative(&self, new: NewNarrative<'_>) -> Result<i64> {
        log::debug!(
            "ledger::upsert_narrative: cluster_key={} archetype={} title={:?}",
            new.cluster_key,
            new.archetype.as_str(),
            new.title,
        );
        let gem_ids_json = serde_json::to_string(new.gem_ids).context("encode gem_ids")?;
        let mode_mix_json = serde_json::to_string(&new.axes.mode_mix).context("encode mode_mix")?;
        let repos_json = serde_json::to_string(&new.axes.repos).context("encode repos")?;
        let workitem_ids_json = serde_json::to_string(&new.axes.workitem_ids).context("encode workitem_ids")?;
        let (time_start, time_end) = match new.axes.time_window {
            Some((s, e)) => (Some(s.to_rfc3339()), Some(e.to_rfc3339())),
            None => (None, None),
        };

        self.with_conn(|c| {
            let tx = c.transaction().context("begin upsert_narrative tx")?;
            tx.execute(
                "INSERT INTO narratives \
                    (cluster_key, slug, title, thesis, body_md, gem_ids, archetype, synthesised_at, synthesiser_model, revision) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 1) \
                 ON CONFLICT (cluster_key) DO UPDATE SET \
                    slug = excluded.slug, \
                    title = excluded.title, \
                    thesis = excluded.thesis, \
                    body_md = excluded.body_md, \
                    gem_ids = excluded.gem_ids, \
                    archetype = excluded.archetype, \
                    synthesised_at = excluded.synthesised_at, \
                    synthesiser_model = excluded.synthesiser_model, \
                    revision = narratives.revision + 1",
                rusqlite::params![
                    new.cluster_key,
                    new.slug,
                    new.title,
                    new.thesis,
                    new.body_md,
                    gem_ids_json,
                    new.archetype.as_str(),
                    new.synthesised_at.to_rfc3339(),
                    new.synthesiser_model,
                ],
            )
            .context("upsert narrative row")?;

            let narrative_id: i64 = tx
                .query_row(
                    "SELECT id FROM narratives WHERE cluster_key = ?1",
                    rusqlite::params![new.cluster_key],
                    |r| r.get(0),
                )
                .context("look up upserted narrative id")?;

            tx.execute(
                "INSERT INTO narrative_axes \
                    (narrative_id, semantic_cluster_id, mode_mix, time_window_start, time_window_end, repos, workitem_ids) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
                 ON CONFLICT (narrative_id) DO UPDATE SET \
                    semantic_cluster_id = excluded.semantic_cluster_id, \
                    mode_mix = excluded.mode_mix, \
                    time_window_start = excluded.time_window_start, \
                    time_window_end = excluded.time_window_end, \
                    repos = excluded.repos, \
                    workitem_ids = excluded.workitem_ids",
                rusqlite::params![
                    narrative_id,
                    new.axes.semantic_cluster_id,
                    mode_mix_json,
                    time_start,
                    time_end,
                    repos_json,
                    workitem_ids_json,
                ],
            )
            .context("upsert narrative_axes row")?;

            tx.commit().context("commit upsert_narrative")?;
            Ok(narrative_id)
        })
    }

    /// Look up a narrative by cluster_key.
    pub fn narrative_by_cluster_key(&self, cluster_key: &str) -> Result<Option<Narrative>> {
        log::debug!("ledger::narrative_by_cluster_key: cluster_key={cluster_key}");
        self.with_conn(|c| {
            let row: Option<NarrativeRow> = c
                .query_row(
                    "SELECT id, cluster_key, slug, title, thesis, body_md, gem_ids, \
                            archetype, synthesised_at, synthesiser_model, revision \
                     FROM narratives WHERE cluster_key = ?1",
                    rusqlite::params![cluster_key],
                    NarrativeRow::from_row,
                )
                .ok();
            match row {
                None => Ok(None),
                Some(r) => {
                    let axes = load_axes(c, r.id)?;
                    Ok(Some(r.into_narrative(axes)))
                }
            }
        })
    }
}

struct NarrativeRow {
    id: i64,
    #[allow(dead_code)]
    cluster_key: String,
    slug: String,
    title: String,
    thesis: String,
    body_md: String,
    gem_ids: Vec<i64>,
    #[allow(dead_code)]
    archetype: String,
    synthesised_at: DateTime<Utc>,
    synthesiser_model: String,
    revision: u32,
}

impl NarrativeRow {
    fn from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        let gem_ids_json: String = r.get(6)?;
        let gem_ids: Vec<i64> = serde_json::from_str(&gem_ids_json)
            .map_err(|e| rusqlite::Error::FromSqlConversionFailure(6, rusqlite::types::Type::Text, Box::new(e)))?;
        let synthesised_at_str: String = r.get(8)?;
        let synthesised_at = DateTime::parse_from_rfc3339(&synthesised_at_str)
            .map_err(|e| rusqlite::Error::FromSqlConversionFailure(8, rusqlite::types::Type::Text, Box::new(e)))?
            .with_timezone(&Utc);
        Ok(NarrativeRow {
            id: r.get(0)?,
            cluster_key: r.get(1)?,
            slug: r.get(2)?,
            title: r.get(3)?,
            thesis: r.get(4)?,
            body_md: r.get(5)?,
            gem_ids,
            archetype: r.get(7)?,
            synthesised_at,
            synthesiser_model: r.get(9)?,
            revision: r.get::<_, i64>(10)? as u32,
        })
    }

    fn into_narrative(self, axes: NarrativeAxes) -> Narrative {
        Narrative {
            id: self.id,
            slug: self.slug,
            title: self.title,
            thesis: self.thesis,
            body_md: self.body_md,
            gem_ids: self.gem_ids,
            axes,
            synthesised_at: self.synthesised_at,
            synthesiser_model: self.synthesiser_model,
            revision: self.revision,
        }
    }
}

type AxesRow = (Option<i64>, String, Option<String>, Option<String>, String, String);

fn load_axes(conn: &rusqlite::Connection, narrative_id: i64) -> Result<NarrativeAxes> {
    let row: Option<AxesRow> = conn
        .query_row(
            "SELECT semantic_cluster_id, mode_mix, time_window_start, time_window_end, repos, workitem_ids \
             FROM narrative_axes WHERE narrative_id = ?1",
            rusqlite::params![narrative_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
        )
        .ok();
    match row {
        None => Ok(NarrativeAxes::default()),
        Some((scid, mode_mix_json, tws, twe, repos_json, wids_json)) => {
            let mode_mix: Vec<(String, u32)> = serde_json::from_str(&mode_mix_json).unwrap_or_default();
            let repos: Vec<String> = serde_json::from_str(&repos_json).unwrap_or_default();
            let workitem_ids: Vec<i64> = serde_json::from_str(&wids_json).unwrap_or_default();
            let time_window = match (tws, twe) {
                (Some(s), Some(e)) => match (DateTime::parse_from_rfc3339(&s), DateTime::parse_from_rfc3339(&e)) {
                    (Ok(s), Ok(e)) => Some((s.with_timezone(&Utc), e.with_timezone(&Utc))),
                    _ => None,
                },
                _ => None,
            };
            Ok(NarrativeAxes {
                semantic_cluster_id: scid,
                mode_mix,
                time_window,
                repos,
                workitem_ids,
            })
        }
    }
}
