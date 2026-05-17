//! Vector search and reciprocal rank fusion over the `note_embeddings`
//! BLOB column.
//!
//! Phase A3 of the hybrid retrieval design
//! (`docs/design/2026-05-16-hybrid-retrieval-fts5-vector-rrf.md`).
//!
//! Storage is a regular SQLite table with `embedding BLOB NOT NULL` (one
//! row per (note, kind, chunk_index)). The brute-force cosine scan
//! decodes each BLOB inline into a dot product with the query vector
//! without ever allocating a `Vec<f32>` per row - allocating 21 K
//! short-lived vectors per query would blow the latency budget at the
//! three-year scale envelope.
//!
//! Phase A reads only `kind = 'summary'` rows. Phase B3 will add
//! max-pool aggregation across summary + transcript-chunk rows; the
//! storage and API shapes here do not need to change for that work.

use eyre::Result;
use rusqlite::params;

use super::SearchIndex;
use crate::schema::NoteType;

/// One row of the `note_embeddings` scan. Phase A returns these directly;
/// Phase A6's RRF dispatch fuses them with BM25 hits.
#[derive(Debug, Clone)]
pub struct VectorHit {
    pub note_path: String,
    /// Cosine distance in `[0.0, 2.0]`. Both query and stored vectors are
    /// L2-normalized (bge-small outputs unit vectors), so
    /// `distance = 1.0 - dot_product`. Smaller is closer.
    pub distance: f32,
}

/// Discriminates summary and transcript-chunk rows in `note_embeddings`.
/// Maps 1:1 to the `kind TEXT CHECK (...)` column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmbeddingKind {
    Summary,
    TranscriptChunk,
}

impl EmbeddingKind {
    /// SQL value for this kind. Must match the `CHECK (kind IN (...))`
    /// constraint in `ensure_vec_schema`.
    pub fn as_str(&self) -> &'static str {
        match self {
            EmbeddingKind::Summary => "summary",
            EmbeddingKind::TranscriptChunk => "transcript-chunk",
        }
    }
}

/// One row of input to [`SearchIndex::upsert_embeddings_batch`].
///
/// Lifetime is bounded to the call - the caller owns the underlying
/// `text` and `embedding` storage so we can avoid copying every row's
/// vector twice (once for the inputs Vec, once for the encoded bytes).
pub struct BatchUpsert<'a> {
    pub note_path: &'a str,
    pub kind: EmbeddingKind,
    pub chunk_index: u32,
    pub text: &'a str,
    pub embedding: &'a [f32],
    pub model_version: &'a str,
    pub source_modified_at: i64,
}

/// One row identifying a note whose `kind` embedding is missing or stale
/// relative to `notes.modified_at`. Cortex's re-embed loop drives this
/// list (see Phase A5).
#[derive(Debug, Clone)]
pub struct StaleTarget {
    pub note_path: String,
    pub note_type: String,
    /// Snapshot of `notes.modified_at` at the time of the query. Cortex
    /// writes this verbatim into `note_embeddings.source_modified_at` so
    /// the next scan sees a consistent watermark even if the note is
    /// re-modified between read and write.
    pub modified_at: i64,
}

/// Validate that a stored embedding BLOB matches its declared `dim`.
///
/// `bytes.len()` must equal `dim * 4` (little-endian f32). Returns a
/// clean error on mismatch so callers can surface the path and row id;
/// never panics. The brute-force scan calls this once per row before
/// the inner dot-product loop so a corrupt BLOB cannot read past the
/// slice.
pub(crate) fn validate_embedding_bytes(bytes: &[u8], dim: usize) -> Result<()> {
    if bytes.len() != dim * 4 {
        eyre::bail!(
            "embedding BLOB length mismatch: got {} bytes, expected dim={} ({} bytes)",
            bytes.len(),
            dim,
            dim * 4,
        );
    }
    Ok(())
}

/// Encode an f32 slice into a little-endian byte sequence suitable for
/// the `note_embeddings.embedding` column. The reverse of the inline
/// decoder in `search_vector`; cortex's upsert path calls this.
pub(crate) fn encode_embedding_bytes(vector: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(vector.len() * 4);
    for v in vector {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

/// Zero-allocation dot product between a query vector (already in
/// memory) and a stored BLOB (borrowed from the rusqlite row, not
/// copied). Folds the byte-iteration and the multiply-add into one
/// pass.
fn dot_product_from_bytes(query_vec: &[f32], stored: &[u8]) -> f32 {
    debug_assert_eq!(stored.len(), query_vec.len() * 4);
    let mut dot = 0.0_f32;
    for (i, chunk) in stored.chunks_exact(4).enumerate() {
        let v = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        dot += query_vec[i] * v;
    }
    dot
}

impl SearchIndex {
    /// Brute-force cosine-similarity search over `note_embeddings`.
    ///
    /// Reads every row (`summary` and `transcript-chunk`) for the
    /// active model and aggregates by note via max-pool: a note's
    /// score is `min(distances across all rows for that note)` - the
    /// single best-matching representation wins. In cosine-distance
    /// space smaller is closer, so the minimum over the rows is the
    /// max-pool similarity.
    ///
    /// The note-side filters (`domain`, `note_type`, `status`) are
    /// pushed into SQL so the scan only visits rows that pass them;
    /// the dot-product loop then ranks the survivors.
    ///
    /// Performance contract: at ~25 K total rows (21 K summary + a
    /// handful of chunks for transcript-eligible notes at the three-
    /// year horizon) the scan runs in well under 20 ms single-
    /// threaded. Phase A7's benchmark enforces the budget.
    pub fn search_vector(
        &self,
        query_vec: &[f32],
        limit: u32,
        domain: Option<&str>,
        note_type: Option<&str>,
        status: Option<&str>,
    ) -> Result<Vec<VectorHit>> {
        let active_model = self.active_embedding_model()?;
        let active_dim = self.active_embedding_dim()?;
        if query_vec.len() != active_dim {
            eyre::bail!(
                "query vector dim ({}) does not match active model dim ({})",
                query_vec.len(),
                active_dim,
            );
        }

        let mut sql = String::from(
            "SELECT e.note_path, e.embedding, e.dim
             FROM note_embeddings e
             JOIN notes n ON n.path = e.note_path
             WHERE e.model_version = ?1",
        );
        let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = vec![Box::new(active_model)];
        let mut param_idx = 2;
        if let Some(d) = domain {
            sql.push_str(&format!(" AND n.domain = ?{param_idx}"));
            param_values.push(Box::new(d.to_string()));
            param_idx += 1;
        }
        if let Some(t) = note_type {
            sql.push_str(&format!(" AND n.note_type = ?{param_idx}"));
            param_values.push(Box::new(t.to_string()));
            param_idx += 1;
        }
        if let Some(s) = status {
            sql.push_str(&format!(" AND n.status = ?{param_idx}"));
            param_values.push(Box::new(s.to_string()));
            param_idx += 1;
        }
        let _ = param_idx;

        let mut stmt = self.conn.prepare(&sql)?;
        let params_refs: Vec<&dyn rusqlite::types::ToSql> = param_values.iter().map(|p| p.as_ref()).collect();

        // Walk every row, compute distance, and reduce-by-note via min.
        // A HashMap is faster than a Vec<(path,best)> linear scan once
        // candidate counts cross ~500.
        use std::collections::HashMap;
        let mut best: HashMap<String, f32> = HashMap::new();
        let mut rows = stmt.query(params_refs.as_slice())?;
        while let Some(row) = rows.next()? {
            let note_path: String = row.get(0)?;
            let bytes: Vec<u8> = row.get(1)?;
            let dim: i64 = row.get(2)?;
            validate_embedding_bytes(&bytes, dim as usize)?;
            if dim as usize != query_vec.len() {
                eyre::bail!(
                    "row dim ({}) does not match query dim ({}) for note {note_path}",
                    dim,
                    query_vec.len(),
                );
            }
            let dot = dot_product_from_bytes(query_vec, &bytes);
            let distance = 1.0_f32 - dot;
            best.entry(note_path)
                .and_modify(|d| {
                    if distance < *d {
                        *d = distance;
                    }
                })
                .or_insert(distance);
        }

        let mut hits: Vec<VectorHit> = best
            .into_iter()
            .map(|(note_path, distance)| VectorHit { note_path, distance })
            .collect();
        hits.sort_by(|a, b| a.distance.partial_cmp(&b.distance).unwrap_or(std::cmp::Ordering::Equal));
        hits.truncate(limit as usize);
        Ok(hits)
    }

    /// Insert (or replace) a single embedding row.
    ///
    /// The UNIQUE `(note_path, kind, chunk_index, model_version)` constraint
    /// makes this idempotent under the staleness contract: cortex re-embeds
    /// by computing the new vector and calling this; the prior row for the
    /// same key is replaced atomically.
    pub fn upsert_embedding(
        &self,
        note_path: &str,
        kind: EmbeddingKind,
        chunk_index: u32,
        text: &str,
        embedding: &[f32],
        model_version: &str,
        source_modified_at: i64,
    ) -> Result<()> {
        let bytes = encode_embedding_bytes(embedding);
        let dim = embedding.len() as i64;
        let now = chrono::Utc::now().timestamp();
        self.conn.execute(
            "INSERT OR REPLACE INTO note_embeddings (
                note_path, kind, chunk_index, text, embedding, dim,
                model_version, produced_at, source_modified_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                note_path,
                kind.as_str(),
                chunk_index as i64,
                text,
                bytes,
                dim,
                model_version,
                now,
                source_modified_at,
            ],
        )?;
        Ok(())
    }

    /// Upsert a batch of embedding rows inside a single short write
    /// transaction (`BEGIN IMMEDIATE` → upserts → `COMMIT`).
    ///
    /// **DO NOT** call this from inside a context that holds the
    /// embedding model: the transaction discipline mandates that
    /// inference runs in auto-commit, then this short transaction
    /// flushes the results. Per row work is just an INSERT OR REPLACE,
    /// so a 64-row batch comfortably runs under 200 ms even on slow
    /// disks. Phase A5's regression test asserts the budget.
    pub fn upsert_embeddings_batch(&mut self, items: &[BatchUpsert<'_>]) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute_batch("BEGIN IMMEDIATE;").ok();
        for item in items {
            let bytes = encode_embedding_bytes(item.embedding);
            tx.execute(
                "INSERT OR REPLACE INTO note_embeddings (
                    note_path, kind, chunk_index, text, embedding, dim,
                    model_version, produced_at, source_modified_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    item.note_path,
                    item.kind.as_str(),
                    item.chunk_index as i64,
                    item.text,
                    bytes,
                    item.embedding.len() as i64,
                    item.model_version,
                    chrono::Utc::now().timestamp(),
                    item.source_modified_at,
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Atomic swap of every `transcript-chunk` row for a single note,
    /// inside one short write transaction (`BEGIN IMMEDIATE` → DELETE
    /// → INSERTs → `COMMIT`).
    ///
    /// Phase B2's re-embed loop: when a transcript's text changes, the
    /// chunk boundaries shift, so there is no stable per-chunk identity
    /// to preserve. Wiping the existing chunks and writing the new ones
    /// in one transaction means hybrid search never sees a half-replaced
    /// chunk set.
    pub fn swap_transcript_chunks(
        &mut self,
        note_path: &str,
        chunks: &[(String, Vec<f32>)],
        model_version: &str,
        source_modified_at: i64,
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute_batch("BEGIN IMMEDIATE;").ok();
        tx.execute(
            "DELETE FROM note_embeddings
             WHERE note_path = ?1 AND kind = ?2",
            params![note_path, EmbeddingKind::TranscriptChunk.as_str()],
        )?;
        let now = chrono::Utc::now().timestamp();
        for (idx, (text, embedding)) in chunks.iter().enumerate() {
            let bytes = encode_embedding_bytes(embedding);
            tx.execute(
                "INSERT INTO note_embeddings (
                    note_path, kind, chunk_index, text, embedding, dim,
                    model_version, produced_at, source_modified_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    note_path,
                    EmbeddingKind::TranscriptChunk.as_str(),
                    idx as i64,
                    text,
                    bytes,
                    embedding.len() as i64,
                    model_version,
                    now,
                    source_modified_at,
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Update `embedding_config.active_model` and `active_dim` inside a
    /// single short transaction. Both rows must move together so oracle
    /// never sees a half-rolled-over config.
    pub fn set_active_embedding(&mut self, model_version: &str, dim: usize) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "UPDATE embedding_config SET value = ?1 WHERE key = 'active_model'",
            params![model_version],
        )?;
        tx.execute(
            "UPDATE embedding_config SET value = ?1 WHERE key = 'active_dim'",
            params![dim.to_string()],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Delete every embedding row for a note. Normally unnecessary
    /// because `ON DELETE CASCADE` runs automatically when the
    /// matching `notes` row is removed; cortex calls this explicitly
    /// when it needs to wipe transcript-chunk rows ahead of a re-chunk
    /// (Phase B2's atomic swap).
    pub fn delete_embeddings_for_note(&self, note_path: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM note_embeddings WHERE note_path = ?1", params![note_path])?;
        Ok(())
    }

    /// List notes whose `kind` embedding is missing or stale relative
    /// to `notes.modified_at`. Cortex's re-embed loop drives this list.
    ///
    /// Summary rows: every note is a candidate.
    /// Transcript-chunk rows: filtered to the kinds listed in
    /// `NoteType::transcript_eligible()`. Without that filter, every
    /// Article and Repo in the vault matches `e.id IS NULL` permanently
    /// and the cortex daemon spins. Driving the filter from the schema
    /// enum (rather than a hand-typed SQL string list) means a future
    /// `NoteType` variant rename cannot silently re-break this path.
    pub fn stale_embedding_targets(
        &self,
        kind: EmbeddingKind,
        model_version: &str,
        limit: u32,
    ) -> Result<Vec<StaleTarget>> {
        let transcript_eligible_in_clause = NoteType::transcript_eligible()
            .iter()
            .map(|t| format!("'{}'", t.as_str()))
            .collect::<Vec<_>>()
            .join(", ");

        let sql = match kind {
            EmbeddingKind::Summary => {
                "SELECT n.path, n.note_type, n.modified_at
                 FROM notes n
                 LEFT JOIN note_embeddings e
                   ON e.note_path = n.path
                  AND e.kind = ?1
                  AND e.model_version = ?2
                 WHERE e.id IS NULL
                    OR e.source_modified_at < n.modified_at
                 ORDER BY n.modified_at DESC
                 LIMIT ?3"
                    .to_string()
            }
            EmbeddingKind::TranscriptChunk => format!(
                "SELECT n.path, n.note_type, n.modified_at
                 FROM notes n
                 LEFT JOIN note_embeddings e
                   ON e.note_path = n.path
                  AND e.kind = ?1
                  AND e.model_version = ?2
                 WHERE n.note_type IN ({transcript_eligible_in_clause})
                   AND (e.id IS NULL
                        OR e.source_modified_at < n.modified_at)
                 ORDER BY n.modified_at DESC
                 LIMIT ?3"
            ),
        };

        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map(params![kind.as_str(), model_version, limit as i64], |row| {
                Ok(StaleTarget {
                    note_path: row.get(0)?,
                    note_type: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                    modified_at: row.get::<_, Option<i64>>(2)?.unwrap_or(0),
                })
            })?
            .filter_map(|r| r.ok())
            .collect();
        Ok(rows)
    }

    /// Count embedding rows matching an optional kind. Used by tests
    /// outside the vault crate (e.g. cortex's embed integration tests)
    /// to assert the write phase produced the expected number of rows
    /// without reaching into the private `conn` field.
    pub fn count_embeddings(&self, kind: Option<EmbeddingKind>) -> Result<i64> {
        let count: i64 = match kind {
            Some(k) => self.conn.query_row(
                "SELECT COUNT(*) FROM note_embeddings WHERE kind = ?1",
                params![k.as_str()],
                |row| row.get(0),
            )?,
            None => self
                .conn
                .query_row("SELECT COUNT(*) FROM note_embeddings", [], |row| row.get(0))?,
        };
        Ok(count)
    }

    /// Insert a minimal `notes` row for tests in other crates. Only
    /// the columns required by the vector search path are populated;
    /// the rest get sensible defaults. The body and summary are also
    /// set so the FTS5 triggers index searchable content.
    pub fn insert_test_note_row(&self, path: &str, note_type: &str, modified_at: i64) -> Result<()> {
        self.insert_test_note_full(path, note_type, "body", "summary", modified_at)
    }

    /// Same as [`insert_test_note_row`] but with explicit body and
    /// summary so the FTS5 path can be exercised by tests.
    pub fn insert_test_note_full(
        &self,
        path: &str,
        note_type: &str,
        body: &str,
        summary: &str,
        modified_at: i64,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO notes (path, title, domain, note_type, origin, status, date, tags, source, creator, body, summary, modified_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                path, "T", "tech", note_type, "assisted", "", "2026-05-16",
                "[]", "", "", body, summary, modified_at,
            ],
        )?;
        Ok(())
    }

    /// Read the active embedding model identifier from `embedding_config`.
    /// Both oracle and cortex read this on every dispatch so they never
    /// drift onto different models.
    pub fn active_embedding_model(&self) -> Result<String> {
        let v: String = self.conn.query_row(
            "SELECT value FROM embedding_config WHERE key = 'active_model'",
            [],
            |row| row.get(0),
        )?;
        Ok(v)
    }

    /// Read the active embedding dimension from `embedding_config`. Used
    /// by `search_vector` to validate the query vector matches the rows
    /// it is about to score.
    pub fn active_embedding_dim(&self) -> Result<usize> {
        let v: String = self.conn.query_row(
            "SELECT value FROM embedding_config WHERE key = 'active_dim'",
            [],
            |row| row.get(0),
        )?;
        v.parse::<usize>()
            .map_err(|e| eyre::eyre!("embedding_config.active_dim is not an integer: {v} ({e})"))
    }
}

/// One fused result from reciprocal rank fusion. The caller maps
/// `note_path` back to a `NoteRow` if needed.
#[derive(Debug, Clone)]
pub struct FusedHit {
    pub note_path: String,
    pub score: f32,
}

/// Reciprocal Rank Fusion (Cormack 2009).
///
/// Each input list is treated as a ranking (position 0 = top). A note's
/// fused score is the sum across both lists of `1 / (k + rank)`. Notes
/// present in only one list still contribute (their absence from the
/// other list adds zero). The result is sorted by descending score and
/// truncated to `limit`.
///
/// `k` is the smoothing constant; the literature's default of 60 keeps
/// the contribution of low-rank hits from dominating.
pub fn reciprocal_rank_fusion(bm25_paths: &[String], vec_paths: &[String], k: usize, limit: usize) -> Vec<FusedHit> {
    use std::collections::HashMap;
    let mut scores: HashMap<String, f32> = HashMap::new();
    for (rank, path) in bm25_paths.iter().enumerate() {
        let contrib = 1.0_f32 / (k as f32 + (rank + 1) as f32);
        *scores.entry(path.clone()).or_insert(0.0) += contrib;
    }
    for (rank, path) in vec_paths.iter().enumerate() {
        let contrib = 1.0_f32 / (k as f32 + (rank + 1) as f32);
        *scores.entry(path.clone()).or_insert(0.0) += contrib;
    }
    let mut fused: Vec<FusedHit> = scores
        .into_iter()
        .map(|(note_path, score)| FusedHit { note_path, score })
        .collect();
    fused.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    fused.truncate(limit);
    fused
}

/// Smoothing constant for [`reciprocal_rank_fusion`]; the literature
/// default. Exposed as a `pub const` so oracle's dispatch reads the same
/// value.
pub const RRF_K: usize = 60;

/// Number of candidates pulled from each list before fusion. Over-
/// pulling 50 from each is cheap and improves recall vs. pulling
/// exactly `limit`. Phase A6's dispatch uses this constant.
pub const K_RRF_INPUT: u32 = 50;

#[cfg(test)]
mod tests;
