//! Topic map storage: one topic per in-scope note, frozen centroids, and the
//! generation they belong to (`docs/plan/2026-11-14-topic-map.md`).
//!
//! Three plain tables beside `note_embeddings`: `topic_state` (a key/value
//! store, the `graph_state` shape), `topic_centroids`, and `note_topics`.
//! Cortex is the only writer; this module holds the storage primitives and
//! no clustering (that is `vault::kmeans`).
//!
//! Every row in `note_topics` belongs to `topic_state.generation`:
//! [`SearchIndex::replace_generation`] swaps all three tables in one
//! `BEGIN IMMEDIATE`, so a per-row generation copy could only diverge.
//!
//! Validity is one rule, [`TopicState::status`]. Corrupt state is an error at
//! every caller, never `Absent` and never an empty list.

use std::collections::BTreeMap;

use eyre::{Result, bail, eyre};
use rusqlite::{TransactionBehavior, params, params_from_iter};

use super::SearchIndex;
use super::vector::{EmbeddingKind, encode_embedding_bytes, validate_embedding_bytes};
use crate::schema::NoteType;

const KEY_GENERATION: &str = "generation";
const KEY_TRAINED_AT: &str = "trained_at";
const KEY_K: &str = "k";
const KEY_SEED: &str = "seed";
const KEY_EPOCHS: &str = "epochs";
const KEY_E_MEANS_VERSION: &str = "e_means_version";
const KEY_MODEL_VERSION: &str = "model_version";
const KEY_DIM: &str = "dim";
const KEY_TRAINED_ROWS: &str = "trained_rows";
const KEY_TRAIN_MEAN_COSINE: &str = "train_mean_cosine";
const KEY_EXCLUDE_TYPES: &str = "exclude_types";

/// Every `topic_state` key a trained generation carries, besides `generation`
/// (which survives a clear).
const TRAINED_KEYS: [&str; 10] = [
    KEY_TRAINED_AT,
    KEY_K,
    KEY_SEED,
    KEY_EPOCHS,
    KEY_E_MEANS_VERSION,
    KEY_MODEL_VERSION,
    KEY_DIM,
    KEY_TRAINED_ROWS,
    KEY_TRAIN_MEAN_COSINE,
    KEY_EXCLUDE_TYPES,
];

/// One note's topic, as written by [`SearchIndex::assign_batch`] and
/// [`SearchIndex::replace_generation`].
#[derive(Debug, Clone, PartialEq)]
pub struct Assignment {
    pub note_path: String,
    pub topic: u32,
    /// Dot product with the centroid.
    pub cosine: f32,
    /// The `note_embeddings.id` the topic was assigned from.
    pub embedding_id: i64,
}

/// Everything one training run hands to [`SearchIndex::replace_generation`].
#[derive(Debug, Clone)]
pub struct NewGeneration<'a> {
    /// `k * dim` f32, topic-major, unit norm.
    pub centroids: &'a [f32],
    /// Members per topic at train time; its length is `k`.
    pub trained_rows: &'a [i64],
    pub dim: usize,
    pub model_version: &'a str,
    pub assignments: &'a [Assignment],
    /// Unix seconds; also the `assigned_at` of every train-time assignment.
    pub trained_at: i64,
    pub seed: u64,
    pub epochs: f64,
    pub e_means_version: &'a str,
    pub train_mean_cosine: f64,
    pub exclude_types: &'a [NoteType],
}

/// An in-scope note's active-model summary vector, as read by
/// [`SearchIndex::unassigned`] and [`SearchIndex::in_scope_summaries`].
#[derive(Debug, Clone, PartialEq)]
pub struct SummaryVector {
    pub note_path: String,
    pub embedding_id: i64,
    pub embedding: Vec<f32>,
}

/// The typed view of a trained generation's `topic_state` keys.
#[derive(Debug, Clone, PartialEq)]
pub struct TopicMeta {
    pub generation: i64,
    pub trained_at: i64,
    pub k: usize,
    pub seed: u64,
    pub epochs: f64,
    pub e_means_version: String,
    pub model_version: String,
    pub dim: usize,
    pub trained_rows: i64,
    pub train_mean_cosine: f64,
    pub exclude_types: Vec<String>,
}

/// Counts over the topic scope, read in one transaction by
/// [`SearchIndex::topic_scope`].
#[derive(Debug, Clone, PartialEq)]
pub struct TopicScope {
    /// Notes whose type is not excluded, whose quality is not `low`, and that
    /// are not merge tombstones.
    pub eligible: usize,
    /// Eligible notes holding an active-model `summary` row.
    pub in_scope: usize,
    /// Rows in `note_topics`.
    pub assigned: usize,
    /// Mean `note_topics.cosine`, `None` when there are no rows.
    pub mean_cosine: Option<f64>,
    /// Eligible notes with no active-model `summary` row, ordered by path.
    /// Which of them are pending is the embed loop's call
    /// (`stale_embedding_targets`), not this module's.
    pub eligible_unembedded: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TopicStatus {
    /// No generation: never trained, or cleared.
    Absent,
    Valid,
    /// The generation was trained under a model or dim the index no longer pins.
    Stale,
}

/// A raw snapshot of the three tables' metadata, read in one transaction.
/// [`status`](Self::status) is the only judge of it.
#[derive(Debug, Clone)]
pub struct TopicState {
    values: BTreeMap<String, String>,
    /// `(topic, centroid blob length)` ordered by topic.
    centroids: Vec<(i64, usize)>,
}

impl TopicState {
    /// `Absent`, `Valid` or `Stale` for the active pin, or an error naming what
    /// is corrupt: an unparseable value, a missing trained key, a centroid
    /// count that differs from `k`, topic ids that are not `0..k`, a centroid
    /// blob whose length is not `dim * 4`.
    pub fn status(&self, active_model: &str, active_dim: usize) -> Result<TopicStatus> {
        let Some(meta) = self.meta()? else {
            return Ok(TopicStatus::Absent);
        };
        if meta.model_version != active_model || meta.dim != active_dim {
            return Ok(TopicStatus::Stale);
        }
        Ok(TopicStatus::Valid)
    }

    /// The parsed generation metadata, `None` when there is no generation.
    /// Runs the same corruption checks as [`status`](Self::status).
    pub fn meta(&self) -> Result<Option<TopicMeta>> {
        if !self.values.contains_key(KEY_TRAINED_AT) {
            if let Some(stray) = TRAINED_KEYS.iter().find(|key| self.values.contains_key(**key)) {
                bail!("topic_state has `{stray}` but no `{KEY_TRAINED_AT}`: corrupt topic state");
            }
            if !self.centroids.is_empty() {
                bail!(
                    "topic_centroids holds {} rows but topic_state has no `{KEY_TRAINED_AT}`: corrupt topic state",
                    self.centroids.len()
                );
            }
            return Ok(None);
        }
        let meta = TopicMeta {
            generation: self.parsed(KEY_GENERATION)?,
            trained_at: self.parsed(KEY_TRAINED_AT)?,
            k: self.parsed(KEY_K)?,
            seed: self.parsed(KEY_SEED)?,
            epochs: self.parsed(KEY_EPOCHS)?,
            e_means_version: self.text(KEY_E_MEANS_VERSION)?.to_string(),
            model_version: self.text(KEY_MODEL_VERSION)?.to_string(),
            dim: self.parsed(KEY_DIM)?,
            trained_rows: self.parsed(KEY_TRAINED_ROWS)?,
            train_mean_cosine: self.parsed(KEY_TRAIN_MEAN_COSINE)?,
            exclude_types: self
                .text(KEY_EXCLUDE_TYPES)?
                .split(',')
                .filter(|part| !part.is_empty())
                .map(str::to_string)
                .collect(),
        };
        if self.centroids.len() != meta.k {
            bail!(
                "topic_state says k={} but topic_centroids holds {} rows: corrupt topic state",
                meta.k,
                self.centroids.len()
            );
        }
        for (expected, (topic, len)) in self.centroids.iter().enumerate() {
            if *topic != expected as i64 {
                bail!(
                    "topic_centroids ids are not 0..{}: found {topic} where {expected} belongs",
                    meta.k
                );
            }
            if *len != meta.dim * 4 {
                bail!(
                    "topic_centroids topic {topic} blob is {len} bytes, expected dim={} ({} bytes): corrupt topic state",
                    meta.dim,
                    meta.dim * 4
                );
            }
        }
        Ok(Some(meta))
    }

    fn text(&self, key: &str) -> Result<&str> {
        self.values
            .get(key)
            .map(String::as_str)
            .ok_or_else(|| eyre!("topic_state is missing `{key}`: corrupt topic state"))
    }

    fn parsed<T: std::str::FromStr>(&self, key: &str) -> Result<T>
    where
        T::Err: std::fmt::Display,
    {
        let raw = self.text(key)?;
        raw.parse::<T>()
            .map_err(|e| eyre!("topic_state `{key}` is not parseable: {raw:?} ({e})"))
    }
}

impl SearchIndex {
    /// Create the topic-map tables. Called from `ensure_vec_schema`, beside
    /// `note_embeddings`. `note_topics.embedding_id` is a recorded value, not a
    /// foreign key: the embedding row it names is deleted on every re-embed.
    pub(super) fn ensure_topic_schema(&self) -> Result<()> {
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS topic_state (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS topic_centroids (
                topic        INTEGER PRIMARY KEY,
                centroid     BLOB NOT NULL,
                trained_rows INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS note_topics (
                note_path    TEXT PRIMARY KEY REFERENCES notes(path) ON DELETE CASCADE,
                topic        INTEGER NOT NULL REFERENCES topic_centroids(topic),
                cosine       REAL NOT NULL,
                embedding_id INTEGER NOT NULL,
                assigned_at  INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_note_topics_topic ON note_topics(topic);",
        )?;
        Ok(())
    }

    /// Snapshot the topic metadata in one read transaction.
    pub fn topic_state(&self) -> Result<TopicState> {
        let tx = self.conn.unchecked_transaction()?;
        let state = load_state(&tx)?;
        tx.commit()?;
        Ok(state)
    }

    /// The `k * dim` f32 centroids, topic-major, after checking the state is
    /// `Valid` for the active pin. Stale, absent or corrupt state is an error:
    /// the caller asked for centroids it must not assign against.
    pub fn topic_centroids(&self) -> Result<(TopicMeta, Vec<f32>)> {
        let active_model = self.active_embedding_model()?;
        let active_dim = self.active_embedding_dim()?;
        let tx = self.conn.unchecked_transaction()?;
        let state = load_state(&tx)?;
        let status = state.status(&active_model, active_dim)?;
        let meta = match (status, state.meta()?) {
            (TopicStatus::Valid, Some(meta)) => meta,
            (status, _) => bail!("topic centroids are unavailable: status is {status:?}"),
        };
        let blobs = tx
            .prepare("SELECT centroid FROM topic_centroids ORDER BY topic")?
            .query_map([], |row| row.get::<_, Vec<u8>>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        tx.commit()?;
        let mut flat = Vec::with_capacity(meta.k * meta.dim);
        for blob in &blobs {
            validate_embedding_bytes(blob, meta.dim)?;
            flat.extend(decode_f32(blob));
        }
        Ok((meta, flat))
    }

    /// Swap all three tables to a new generation in one `BEGIN IMMEDIATE`.
    /// Returns the new generation: the stored counter plus one (1 when the
    /// counter has never been written). An error anywhere rolls every table
    /// back, so a failed train leaves the previous generation intact.
    ///
    /// Refuses a generation whose `model_version` or `dim` differs from the
    /// active pin: that would be stale at birth.
    pub fn replace_generation(&mut self, new: &NewGeneration<'_>) -> Result<i64> {
        let k = new.trained_rows.len();
        log::debug!(
            "search::replace_generation: k={k} dim={} assignments={}",
            new.dim,
            new.assignments.len()
        );
        if k == 0 || new.dim == 0 {
            bail!("a generation needs k >= 1 and dim >= 1 (k={k}, dim={})", new.dim);
        }
        if new.centroids.len() != k * new.dim {
            bail!(
                "centroids hold {} floats, expected k*dim = {}*{} = {}",
                new.centroids.len(),
                k,
                new.dim,
                k * new.dim
            );
        }
        if let Some(bad) = new.assignments.iter().find(|a| a.topic as usize >= k) {
            bail!("assignment of {} names topic {} but k={k}", bad.note_path, bad.topic);
        }

        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let pinned_model: String = tx.query_row(
            "SELECT value FROM embedding_config WHERE key = 'active_model'",
            [],
            |row| row.get(0),
        )?;
        let pinned_dim: String = tx.query_row(
            "SELECT value FROM embedding_config WHERE key = 'active_dim'",
            [],
            |row| row.get(0),
        )?;
        if pinned_model != new.model_version || pinned_dim != new.dim.to_string() {
            bail!(
                "generation trained for {} dim {} but the index pins {pinned_model} dim {pinned_dim}",
                new.model_version,
                new.dim
            );
        }

        let previous: Option<String> = tx
            .query_row(
                "SELECT value FROM topic_state WHERE key = ?1",
                params![KEY_GENERATION],
                |row| row.get(0),
            )
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })?;
        let generation = match previous {
            None => 1,
            Some(raw) => {
                raw.parse::<i64>()
                    .map_err(|e| eyre!("topic_state `{KEY_GENERATION}` is not parseable: {raw:?} ({e})"))?
                    + 1
            }
        };

        tx.execute("DELETE FROM note_topics", [])?;
        tx.execute("DELETE FROM topic_centroids", [])?;
        tx.execute("DELETE FROM topic_state", [])?;

        {
            let mut insert =
                tx.prepare("INSERT INTO topic_centroids (topic, centroid, trained_rows) VALUES (?1, ?2, ?3)")?;
            for (topic, (row, members)) in new.centroids.chunks_exact(new.dim).zip(new.trained_rows).enumerate() {
                insert.execute(params![topic as i64, encode_embedding_bytes(row), members])?;
            }
        }
        insert_assignments(&tx, new.assignments, new.trained_at)?;

        let exclude_types = new
            .exclude_types
            .iter()
            .map(NoteType::as_str)
            .collect::<Vec<_>>()
            .join(",");
        let trained_total: i64 = new.trained_rows.iter().sum();
        let state: [(&str, String); 11] = [
            (KEY_GENERATION, generation.to_string()),
            (KEY_TRAINED_AT, new.trained_at.to_string()),
            (KEY_K, k.to_string()),
            (KEY_SEED, new.seed.to_string()),
            (KEY_EPOCHS, new.epochs.to_string()),
            (KEY_E_MEANS_VERSION, new.e_means_version.to_string()),
            (KEY_MODEL_VERSION, new.model_version.to_string()),
            (KEY_DIM, new.dim.to_string()),
            (KEY_TRAINED_ROWS, trained_total.to_string()),
            (KEY_TRAIN_MEAN_COSINE, new.train_mean_cosine.to_string()),
            (KEY_EXCLUDE_TYPES, exclude_types),
        ];
        for (key, value) in &state {
            tx.execute(
                "INSERT INTO topic_state (key, value) VALUES (?1, ?2)",
                params![key, value],
            )?;
        }
        tx.commit()?;
        Ok(generation)
    }

    /// Write assignments against the frozen centroids in one `BEGIN IMMEDIATE`
    /// (insert, or overwrite the note's existing row). `assigned_at` is unix
    /// seconds, supplied by the caller. An unknown topic id fails on the
    /// `topic_centroids` foreign key and rolls the whole batch back.
    pub fn assign_batch(&mut self, assignments: &[Assignment], assigned_at: i64) -> Result<usize> {
        log::debug!("search::assign_batch: rows={}", assignments.len());
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        insert_assignments(&tx, assignments, assigned_at)?;
        tx.commit()?;
        Ok(assignments.len())
    }

    /// In-scope notes with no `note_topics` row, or a row assigned from a
    /// different `note_embeddings.id` than the one now stored, with the vector
    /// to assign. Eligibility is the caller's: `exclude_types`, plus the fixed
    /// rules that `quality` is not `low` and the note is not a merge tombstone.
    /// In scope also means holding an active-model `summary` row. Ordered by
    /// `note_path`, at most `limit`.
    pub fn unassigned(&self, exclude_types: &[NoteType], limit: usize) -> Result<Vec<SummaryVector>> {
        log::debug!(
            "search::unassigned: exclude_types={} limit={limit}",
            exclude_types.len()
        );
        self.summary_vectors(exclude_types, true, Some(limit))
    }

    /// Every in-scope note's active-model summary vector, ordered by
    /// `note_path`: the training set for a new generation. Same scope rule as
    /// [`unassigned`](Self::unassigned).
    pub fn in_scope_summaries(&self, exclude_types: &[NoteType]) -> Result<Vec<SummaryVector>> {
        log::debug!("search::in_scope_summaries: exclude_types={}", exclude_types.len());
        self.summary_vectors(exclude_types, false, None)
    }

    /// Delete the `note_topics` rows of notes that left scope: a type now
    /// excluded, quality now `low`, a merge tombstone, or no active-model
    /// `summary` row any more. One `BEGIN IMMEDIATE`; returns the rows removed.
    pub fn remove_out_of_scope_topics(&mut self, exclude_types: &[NoteType]) -> Result<usize> {
        let (eligible, mut args) = eligible_filter(exclude_types);
        let sql = format!(
            "DELETE FROM note_topics
              WHERE note_path NOT IN (
                    SELECT n.path
                      FROM notes n
                      JOIN note_embeddings e
                        ON e.note_path = n.path AND e.kind = ? AND e.model_version = ?
                     WHERE {eligible})"
        );
        let active_model = self.active_embedding_model()?;
        let mut all: Vec<rusqlite::types::Value> =
            vec![EmbeddingKind::Summary.as_str().to_string().into(), active_model.into()];
        all.append(&mut args);
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let removed = tx.execute(&sql, params_from_iter(all))?;
        tx.commit()?;
        log::debug!("search::remove_out_of_scope_topics: removed={removed}");
        Ok(removed)
    }

    /// Eligible, in-scope and assigned counts, the mean assigned cosine, and
    /// the eligible notes with no active-model summary row, from one read
    /// transaction.
    pub fn topic_scope(&self, exclude_types: &[NoteType]) -> Result<TopicScope> {
        let active_model = self.active_embedding_model()?;
        let (eligible_sql, args) = eligible_filter(exclude_types);
        let summary_row = "SELECT 1 FROM note_embeddings e
                            WHERE e.note_path = n.path AND e.kind = ? AND e.model_version = ?";
        let with_summary_args = || {
            let mut all: Vec<rusqlite::types::Value> = vec![
                EmbeddingKind::Summary.as_str().to_string().into(),
                active_model.clone().into(),
            ];
            all.extend(args.iter().cloned());
            all
        };

        let tx = self.conn.unchecked_transaction()?;
        let eligible: i64 = tx.query_row(
            &format!("SELECT COUNT(*) FROM notes n WHERE {eligible_sql}"),
            params_from_iter(args.iter().cloned()),
            |row| row.get(0),
        )?;
        let in_scope: i64 = tx.query_row(
            &format!("SELECT COUNT(*) FROM notes n WHERE EXISTS ({summary_row}) AND {eligible_sql}"),
            params_from_iter(with_summary_args()),
            |row| row.get(0),
        )?;
        let eligible_unembedded = tx
            .prepare(&format!(
                "SELECT n.path FROM notes n WHERE NOT EXISTS ({summary_row}) AND {eligible_sql} ORDER BY n.path"
            ))?
            .query_map(params_from_iter(with_summary_args()), |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let (assigned, mean_cosine): (i64, Option<f64>) =
            tx.query_row("SELECT COUNT(*), AVG(cosine) FROM note_topics", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })?;
        tx.commit()?;
        Ok(TopicScope {
            eligible: eligible as usize,
            in_scope: in_scope as usize,
            assigned: assigned as usize,
            mean_cosine,
            eligible_unembedded,
        })
    }

    fn summary_vectors(
        &self,
        exclude_types: &[NoteType],
        only_unassigned: bool,
        limit: Option<usize>,
    ) -> Result<Vec<SummaryVector>> {
        let active_model = self.active_embedding_model()?;
        let dim = self.active_embedding_dim()?;
        let (eligible, eligible_args) = eligible_filter(exclude_types);
        let unassigned_clause = if only_unassigned {
            "AND (t.note_path IS NULL OR t.embedding_id != e.id)"
        } else {
            ""
        };
        let limit_clause = if limit.is_some() { "LIMIT ?" } else { "" };
        let sql = format!(
            "SELECT n.path, e.id, e.embedding
               FROM notes n
               JOIN note_embeddings e
                 ON e.note_path = n.path AND e.kind = ? AND e.model_version = ?
               LEFT JOIN note_topics t ON t.note_path = n.path
              WHERE {eligible}
                {unassigned_clause}
              ORDER BY n.path
              {limit_clause}"
        );
        let mut args: Vec<rusqlite::types::Value> =
            vec![EmbeddingKind::Summary.as_str().to_string().into(), active_model.into()];
        args.extend(eligible_args);
        if let Some(limit) = limit {
            args.push((limit as i64).into());
        }

        let mut stmt = self.conn.prepare(&sql)?;
        let raw = stmt
            .query_map(params_from_iter(args), |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Vec<u8>>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        raw.into_iter()
            .map(|(note_path, embedding_id, blob)| {
                validate_embedding_bytes(&blob, dim).map_err(|e| eyre!("{note_path}: {e}"))?;
                Ok(SummaryVector {
                    note_path,
                    embedding_id,
                    embedding: decode_f32(&blob),
                })
            })
            .collect()
    }

    /// Empty `note_topics`, `topic_centroids` and every `topic_state` key
    /// except `generation`, in one transaction. The counter survives so a later
    /// train never reuses a generation number a client has seen. Returns the
    /// number of `note_topics` rows removed.
    pub fn clear_topics(&mut self) -> Result<usize> {
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let removed = tx.execute("DELETE FROM note_topics", [])?;
        tx.execute("DELETE FROM topic_centroids", [])?;
        tx.execute("DELETE FROM topic_state WHERE key != ?1", params![KEY_GENERATION])?;
        tx.commit()?;
        Ok(removed)
    }
}

/// Raw contents of the three topic tables, for tests in other crates:
/// `topic_state` key/value pairs, `(topic, centroid, trained_rows)`, and
/// `(note_path, topic, embedding_id, assigned_at)`, each ordered by key.
#[cfg(any(test, feature = "test-util"))]
pub type TopicTables = (
    Vec<(String, String)>,
    Vec<(i64, Vec<u8>, i64)>,
    Vec<(String, i64, i64, i64)>,
);

#[cfg(any(test, feature = "test-util"))]
impl SearchIndex {
    /// Dump the three topic tables, so a test can prove a failed write left
    /// them byte-identical.
    pub fn test_topic_tables(&self) -> Result<TopicTables> {
        let state = self
            .conn
            .prepare("SELECT key, value FROM topic_state ORDER BY key")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let centroids = self
            .conn
            .prepare("SELECT topic, centroid, trained_rows FROM topic_centroids ORDER BY topic")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let notes = self
            .conn
            .prepare("SELECT note_path, topic, embedding_id, assigned_at FROM note_topics ORDER BY note_path")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
            .collect::<rusqlite::Result<_>>()?;
        Ok((state, centroids, notes))
    }

    /// Set a note's `quality` column (`cortex-quality` in frontmatter).
    pub fn set_test_quality(&self, path: &str, quality: &str) -> Result<()> {
        self.conn
            .execute("UPDATE notes SET quality = ?2 WHERE path = ?1", params![path, quality])?;
        Ok(())
    }
}

/// The eligibility rule over `notes n`, defined without reference to
/// embeddings: type not in `exclude_types` (a note with no type is not in the
/// list), `quality` is not `low`, and not a merge tombstone. Returns the SQL
/// fragment and its bound values, in order.
fn eligible_filter(exclude_types: &[NoteType]) -> (String, Vec<rusqlite::types::Value>) {
    let mut sql = "COALESCE(n.quality, '') != 'low' AND COALESCE(n.superseded_by, '') = ''".to_string();
    if !exclude_types.is_empty() {
        let placeholders = vec!["?"; exclude_types.len()].join(",");
        sql.push_str(&format!(" AND COALESCE(n.note_type, '') NOT IN ({placeholders})"));
    }
    let args = exclude_types
        .iter()
        .map(|t| rusqlite::types::Value::from(t.as_str().to_string()))
        .collect();
    (sql, args)
}

fn load_state(tx: &rusqlite::Transaction<'_>) -> Result<TopicState> {
    let values = tx
        .prepare("SELECT key, value FROM topic_state")?
        .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?
        .collect::<rusqlite::Result<BTreeMap<_, _>>>()?;
    let centroids = tx
        .prepare("SELECT topic, length(centroid) FROM topic_centroids ORDER BY topic")?
        .query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)? as usize)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(TopicState { values, centroids })
}

fn insert_assignments(tx: &rusqlite::Transaction<'_>, assignments: &[Assignment], assigned_at: i64) -> Result<()> {
    let mut insert = tx.prepare(
        "INSERT OR REPLACE INTO note_topics (note_path, topic, cosine, embedding_id, assigned_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
    )?;
    for a in assignments {
        insert.execute(params![
            a.note_path,
            a.topic as i64,
            a.cosine as f64,
            a.embedding_id,
            assigned_at
        ])?;
    }
    Ok(())
}

fn decode_f32(bytes: &[u8]) -> Vec<f32> {
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|chunk| f32::from_le_bytes(*chunk))
        .collect()
}

#[cfg(test)]
mod tests;
