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
    /// Phase A returns rows with `kind = 'summary'` only. The note-side
    /// filters (`domain`, `note_type`, `status`) are pushed into SQL so
    /// the scan only visits rows that pass the filter; the dot-product
    /// loop then ranks the survivors.
    ///
    /// Performance contract: at 21 K vectors / 384 dims this runs in
    /// well under 20 ms single-threaded. Phase A7's benchmark enforces
    /// the budget.
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
             WHERE e.kind = ?1
               AND e.model_version = ?2",
        );
        let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = vec![
            Box::new(EmbeddingKind::Summary.as_str().to_string()),
            Box::new(active_model),
        ];
        let mut param_idx = 3;
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

        // Greater (negated) distance is sorted higher in a BinaryHeap, but
        // we want the smallest distances. Collect and sort: at 21 K rows
        // a single sort is ~100 us, far cheaper than a heap's per-push
        // overhead at this size.
        let mut hits: Vec<VectorHit> = Vec::new();
        let mut rows = stmt.query(params_refs.as_slice())?;
        while let Some(row) = rows.next()? {
            let note_path: String = row.get(0)?;
            let bytes: Vec<u8> = row.get(1)?;
            let dim: i64 = row.get(2)?;
            validate_embedding_bytes(&bytes, dim as usize)?;
            if dim as usize != query_vec.len() {
                // dim mismatch on a row whose model_version matched is a
                // schema bug; refuse to score it.
                eyre::bail!(
                    "row dim ({}) does not match query dim ({}) for note {note_path}",
                    dim,
                    query_vec.len(),
                );
            }
            let dot = dot_product_from_bytes(query_vec, &bytes);
            let distance = 1.0_f32 - dot;
            hits.push(VectorHit { note_path, distance });
        }

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
    /// Transcript-chunk rows: filtered to transcript-eligible kinds
    /// (Image, VoiceNote, Idea, Vocabulary, Video, Thread). Without
    /// that filter, every Article and Repo in the vault matches
    /// `e.id IS NULL` permanently and the cortex daemon spins.
    pub fn stale_embedding_targets(
        &self,
        kind: EmbeddingKind,
        model_version: &str,
        limit: u32,
    ) -> Result<Vec<StaleTarget>> {
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
            }
            EmbeddingKind::TranscriptChunk => {
                "SELECT n.path, n.note_type, n.modified_at
                 FROM notes n
                 LEFT JOIN note_embeddings e
                   ON e.note_path = n.path
                  AND e.kind = ?1
                  AND e.model_version = ?2
                 WHERE n.note_type IN (
                         'image', 'voice-note', 'idea', 'vocabulary',
                         'video', 'thread'
                       )
                   AND (e.id IS NULL
                        OR e.source_modified_at < n.modified_at)
                 ORDER BY n.modified_at DESC
                 LIMIT ?3"
            }
        };

        let mut stmt = self.conn.prepare(sql)?;
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
