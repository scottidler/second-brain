//! The topic map's readers: what oracle serves.
//!
//! Every reader runs inside ONE read transaction. Oracle's process mutex does
//! not exclude cortex's connection, so `replace_generation` can commit between
//! two statements of a reader; a response assembled from several statements
//! outside a transaction could describe two generations at once. Inside one
//! deferred read transaction the snapshot is fixed by the first statement (the
//! active-pin read), so the state, counts, labels and exemplars all come from
//! one generation.
//!
//! Each public reader calls a private `*_with` twin that takes a `between`
//! hook, run after the state is read and before any count is. The tests use it
//! to commit a second generation from another connection at exactly that seam.

use std::collections::HashMap;

use eyre::Result;
use rusqlite::types::Value;
use rusqlite::{Transaction, params_from_iter};

use super::{TopicMeta, TopicStatus, load_state};
use crate::search::SearchIndex;
use crate::search::filter::Filter;

/// A tag is a topic's label only at this lift or above.
const LABEL_MIN_LIFT: f64 = 2.0;
/// ... and only when at least this many of the topic's notes carry it, or
/// [`LABEL_MIN_SHARE_PERCENT`] of them, whichever is larger.
const LABEL_MIN_COUNT: usize = 5;
const LABEL_MIN_SHARE_PERCENT: usize = 20;
const LABEL_MAX_TAGS: usize = 3;
const EXEMPLARS: usize = 3;

/// The generation's identity and size, read in the same transaction as
/// whatever else a reader returns.
#[derive(Debug, Clone, PartialEq)]
pub struct TopicSnapshot {
    pub status: TopicStatus,
    /// `None` only when `status` is `Absent`.
    pub meta: Option<TopicMeta>,
    /// Rows in `note_topics`.
    pub assigned: usize,
}

/// A tag that labels a topic.
#[derive(Debug, Clone, PartialEq)]
pub struct TopicTag {
    pub tag: String,
    /// (share of the topic's notes with the tag) / (share of all assigned notes with it).
    pub lift: f64,
    /// Notes in the topic carrying the tag.
    pub notes: usize,
}

/// One topic as the readers describe it.
#[derive(Debug, Clone, PartialEq)]
pub struct TopicSummary {
    pub topic: u32,
    /// Notes counted under the reader's filter; every note of the topic when
    /// there is none.
    pub notes: usize,
    /// The topic's label. Never depends on the filter.
    pub tags: Vec<TopicTag>,
    /// Titles of the 3 notes nearest the centroid; empty when not requested.
    pub exemplars: Vec<String>,
}

/// The result of [`SearchIndex::overview`] and [`SearchIndex::in_tags`].
#[derive(Debug, Clone, PartialEq)]
pub struct TopicOverview {
    pub snapshot: TopicSnapshot,
    /// Empty unless `snapshot.status` is `Valid`.
    pub topics: Vec<TopicSummary>,
}

/// A note in a topic and how near the centroid it sits.
#[derive(Debug, Clone, PartialEq)]
pub struct TopicNote {
    pub path: String,
    pub cosine: f64,
}

/// The result of [`SearchIndex::notes`].
#[derive(Debug, Clone, PartialEq)]
pub enum TopicNotes {
    /// Absent or Stale: nothing to read.
    Unavailable(TopicSnapshot),
    /// The caller's topic ids come from another generation.
    GenerationMismatch { requested: i64, stored: i64 },
    /// `topic` is not in `0..k` for the stored generation.
    UnknownTopic { topic: u32, k: usize },
    /// Nearest the centroid first.
    Notes {
        snapshot: TopicSnapshot,
        notes: Vec<TopicNote>,
    },
}

/// The label rule: a tag labels a topic when its lift is 2.0 or more and at
/// least max(5, 20% of the topic's notes, rounded down) carry it. Top 3 by
/// lift, ties by tag name. Zero qualifying tags is a legal answer.
///
/// `in_topic` is `(tag, notes in the topic carrying it)`; `overall` is the
/// number of assigned notes carrying each tag; `assigned` is every assigned
/// note.
pub fn label_tags(
    topic_notes: usize,
    assigned: usize,
    in_topic: &[(String, usize)],
    overall: &HashMap<String, usize>,
) -> Vec<TopicTag> {
    if topic_notes == 0 || assigned == 0 {
        return Vec::new();
    }
    let min_count = LABEL_MIN_COUNT.max(topic_notes * LABEL_MIN_SHARE_PERCENT / 100);
    let mut labels: Vec<TopicTag> = in_topic
        .iter()
        .filter_map(|(tag, count)| {
            let everywhere = *overall.get(tag)?;
            if *count < min_count || everywhere == 0 {
                return None;
            }
            // One division of two exact integers: equal rationals give equal
            // floats, so the tie-break below is by name and not by rounding.
            let lift = (*count as f64 * assigned as f64) / (everywhere as f64 * topic_notes as f64);
            (lift >= LABEL_MIN_LIFT).then(|| TopicTag {
                tag: tag.clone(),
                lift,
                notes: *count,
            })
        })
        .collect();
    labels.sort_by(|a, b| b.lift.total_cmp(&a.lift).then_with(|| a.tag.cmp(&b.tag)));
    labels.truncate(LABEL_MAX_TAGS);
    labels
}

impl SearchIndex {
    /// The generation's status, identity and assigned count, in one
    /// transaction.
    pub fn topic_snapshot(&self) -> Result<TopicSnapshot> {
        let tx = self.conn.unchecked_transaction()?;
        let snapshot = read_snapshot(&tx)?;
        tx.commit()?;
        Ok(snapshot)
    }

    /// Every topic with at least one note, biggest first (ties by id), each
    /// with its label and 3 exemplar titles. With `tags`, `notes` counts only
    /// the notes carrying them (`tags_all`: every one, else at least one) and
    /// topics left with none are dropped; labels and exemplars do not change.
    pub fn overview(&self, tags: Option<&[String]>, tags_all: bool) -> Result<TopicOverview> {
        self.overview_with(tags, tags_all, None, true, &|| {})
    }

    /// The topics holding `tag`, most such notes first, at most `limit`, each
    /// with its label. `notes` counts the notes carrying the tag.
    pub fn in_tags(&self, tag: &str, limit: usize) -> Result<TopicOverview> {
        self.overview_with(Some(&[tag.to_string()]), false, Some(limit), false, &|| {})
    }

    /// The notes of one topic, nearest the centroid first, at most `limit`.
    /// `generation` is the generation the caller's topic id came from; a
    /// mismatch with the stored one is reported, never answered. With `tags`,
    /// only notes carrying them.
    pub fn notes(
        &self,
        topic: u32,
        generation: Option<i64>,
        tags: Option<&[String]>,
        tags_all: bool,
        limit: usize,
    ) -> Result<TopicNotes> {
        self.notes_with(topic, generation, tags, tags_all, limit, &|| {})
    }

    pub(super) fn overview_with(
        &self,
        tags: Option<&[String]>,
        tags_all: bool,
        limit: Option<usize>,
        exemplars: bool,
        between: &dyn Fn(),
    ) -> Result<TopicOverview> {
        let tx = self.conn.unchecked_transaction()?;
        let status_and_meta = read_status_and_meta(&tx)?;
        between();
        let snapshot = finish_snapshot(&tx, status_and_meta)?;
        if snapshot.status != TopicStatus::Valid {
            tx.commit()?;
            return Ok(TopicOverview {
                snapshot,
                topics: Vec::new(),
            });
        }

        let filter = Filter::new(
            "SELECT nt.topic, COUNT(*) FROM note_topics nt JOIN notes n ON n.path = nt.note_path WHERE 1 = 1",
            Vec::new(),
        )
        .and_tags("n", tags, tags_all)
        .then(" GROUP BY nt.topic");
        let mut counted: Vec<(u32, usize)> = tx
            .prepare(&filter.sql)?
            .query_map(params_from_iter(&filter.params), |row| {
                Ok((row.get::<_, i64>(0)? as u32, row.get::<_, i64>(1)? as usize))
            })?
            .collect::<rusqlite::Result<_>>()?;
        counted.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        if let Some(limit) = limit {
            counted.truncate(limit);
        }

        let sizes: HashMap<u32, usize> = tx
            .prepare("SELECT topic, COUNT(*) FROM note_topics GROUP BY topic")?
            .query_map([], |row| {
                Ok((row.get::<_, i64>(0)? as u32, row.get::<_, i64>(1)? as usize))
            })?
            .collect::<rusqlite::Result<_>>()?;
        let mut per_topic: HashMap<u32, Vec<(String, usize)>> = HashMap::new();
        for row in tx
            .prepare(
                "SELECT nt.topic, t.tag, COUNT(*) FROM note_topics nt
                   JOIN note_tags t ON t.path = nt.note_path
                  GROUP BY nt.topic, t.tag",
            )?
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)? as u32,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)? as usize,
                ))
            })?
        {
            let (topic, tag, count) = row?;
            per_topic.entry(topic).or_default().push((tag, count));
        }
        let overall: HashMap<String, usize> = tx
            .prepare(
                "SELECT t.tag, COUNT(*) FROM note_tags t
                   JOIN note_topics nt ON nt.note_path = t.path
                  GROUP BY t.tag",
            )?
            .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as usize)))?
            .collect::<rusqlite::Result<_>>()?;

        let mut titles: HashMap<u32, Vec<String>> = HashMap::new();
        if exemplars {
            for row in tx
                .prepare(
                    "SELECT topic, title FROM (
                         SELECT nt.topic AS topic, n.title AS title,
                                ROW_NUMBER() OVER (PARTITION BY nt.topic
                                                   ORDER BY nt.cosine DESC, nt.note_path) AS rank
                           FROM note_topics nt JOIN notes n ON n.path = nt.note_path)
                      WHERE rank <= ?1
                      ORDER BY topic, rank",
                )?
                .query_map([EXEMPLARS as i64], |row| {
                    Ok((row.get::<_, i64>(0)? as u32, row.get::<_, String>(1)?))
                })?
            {
                let (topic, title) = row?;
                titles.entry(topic).or_default().push(title);
            }
        }
        tx.commit()?;

        let topics = counted
            .into_iter()
            .map(|(topic, notes)| {
                let tags = label_tags(
                    sizes.get(&topic).copied().unwrap_or(0),
                    snapshot.assigned,
                    per_topic.get(&topic).map(Vec::as_slice).unwrap_or(&[]),
                    &overall,
                );
                TopicSummary {
                    topic,
                    notes,
                    tags,
                    exemplars: titles.remove(&topic).unwrap_or_default(),
                }
            })
            .collect();
        Ok(TopicOverview { snapshot, topics })
    }

    pub(super) fn notes_with(
        &self,
        topic: u32,
        generation: Option<i64>,
        tags: Option<&[String]>,
        tags_all: bool,
        limit: usize,
        between: &dyn Fn(),
    ) -> Result<TopicNotes> {
        let tx = self.conn.unchecked_transaction()?;
        let status_and_meta = read_status_and_meta(&tx)?;
        between();
        let snapshot = finish_snapshot(&tx, status_and_meta)?;
        let meta = match (&snapshot.status, &snapshot.meta) {
            (TopicStatus::Valid, Some(meta)) => meta,
            _ => {
                tx.commit()?;
                return Ok(TopicNotes::Unavailable(snapshot));
            }
        };
        if let Some(requested) = generation.filter(|g| *g != meta.generation) {
            let stored = meta.generation;
            tx.commit()?;
            return Ok(TopicNotes::GenerationMismatch { requested, stored });
        }
        if topic as usize >= meta.k {
            let k = meta.k;
            tx.commit()?;
            return Ok(TopicNotes::UnknownTopic { topic, k });
        }

        let filter = Filter::new(
            "SELECT nt.note_path, nt.cosine FROM note_topics nt JOIN notes n ON n.path = nt.note_path WHERE nt.topic = ?",
            vec![Value::Integer(topic as i64)],
        )
        .and_tags("n", tags, tags_all)
        .then(&format!(" ORDER BY nt.cosine DESC, nt.note_path LIMIT {limit}"));
        let notes = tx
            .prepare(&filter.sql)?
            .query_map(params_from_iter(&filter.params), |row| {
                Ok(TopicNote {
                    path: row.get(0)?,
                    cosine: row.get(1)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        tx.commit()?;
        Ok(TopicNotes::Notes { snapshot, notes })
    }
}

/// The first statements of a reader: the active pin, then the topic state.
/// Reading the pin first is what fixes the transaction's snapshot.
fn read_status_and_meta(tx: &Transaction<'_>) -> Result<(TopicStatus, Option<TopicMeta>)> {
    let pin = |key: &str| -> Result<Option<String>> {
        match tx.query_row("SELECT value FROM embedding_config WHERE key = ?1", [key], |row| {
            row.get::<_, String>(0)
        }) {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(eyre::eyre!("read embedding_config.{key}: {e}")),
        }
    };
    let model = pin("active_model")?;
    let dim = match pin("active_dim")? {
        Some(raw) => raw
            .parse::<usize>()
            .map_err(|e| eyre::eyre!("embedding_config.active_dim is not an integer: {raw} ({e})"))?,
        None => 0,
    };
    let state = load_state(tx)?;
    // No pin means nothing was ever embedded, so no generation can be valid
    // against it: an unpinned index with a generation reads Stale.
    let status = state.status(model.as_deref().unwrap_or(""), dim)?;
    Ok((status, state.meta()?))
}

fn finish_snapshot(tx: &Transaction<'_>, (status, meta): (TopicStatus, Option<TopicMeta>)) -> Result<TopicSnapshot> {
    let assigned: i64 = tx.query_row("SELECT COUNT(*) FROM note_topics", [], |row| row.get(0))?;
    Ok(TopicSnapshot {
        status,
        meta,
        assigned: assigned as usize,
    })
}

fn read_snapshot(tx: &Transaction<'_>) -> Result<TopicSnapshot> {
    finish_snapshot(tx, read_status_and_meta(tx)?)
}

#[cfg(test)]
mod tests;
