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

use std::path::Path;

use super::{optional_row, warn_row};
use eyre::Result;
use rusqlite::{TransactionBehavior, params};

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

/// Discriminates summary, transcript-chunk, and claim rows in
/// `note_embeddings`. Maps 1:1 to the `kind TEXT CHECK (...)` column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmbeddingKind {
    Summary,
    TranscriptChunk,
    /// One embedding per group of a note's extracted claims (Phase 9 of
    /// `docs/design/2026-07-05-distillation-knowledge-extraction.md`).
    /// Added so the default vector-only retrieval pipeline reaches claim
    /// text, which is otherwise only FTS-indexed. `search_vector` scans
    /// this kind automatically (no kind filter); it must only *add
    /// recall*, never displace summary precision - see the max-pool
    /// contingency note in `search_vector`.
    Claim,
}

impl EmbeddingKind {
    /// SQL value for this kind. Must match the `CHECK (kind IN (...))`
    /// constraint in `ensure_vec_schema`.
    pub fn as_str(&self) -> &'static str {
        match self {
            EmbeddingKind::Summary => "summary",
            EmbeddingKind::TranscriptChunk => "transcript-chunk",
            EmbeddingKind::Claim => "claim",
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
    /// Snapshot of the text-carrying column at query time. For Summary
    /// embeddings this is `notes.summary`; cortex uses it directly as the
    /// input text (no file I/O on hot path; no skip-without-write loop on
    /// notes missing a `## Summary` heading). For Claim embeddings this is
    /// `notes.claims` (the newline-joined claim text, also no file I/O);
    /// cortex groups it into token-window-sized chunks. For TranscriptChunk
    /// this is always an empty string; cortex reads the transcript from the
    /// staged `distilled.yml` (via `trace`) for Video/Article kinds and from
    /// the in-note `## Transcript` section for the verbatim kinds.
    pub summary: String,
    /// Snapshot of `notes.title` at query time. Cortex prepends it to the
    /// summary before embedding (the title carries strong topical signal);
    /// may be empty.
    pub title: String,
    /// Snapshot of `notes.capture_note` at query time (the operator's
    /// "why I captured this" annotation, Phase 8/9). For Summary embeddings
    /// cortex splices it between the title and summary so the annotation is
    /// semantically searchable; empty for notes without one, in which case
    /// the assembled embed text is byte-identical to the pre-Phase-9 form
    /// (title + summary), so staleness detection re-embeds nothing
    /// retroactively. Empty for the TranscriptChunk and Claim arms.
    pub capture_note: String,
    /// Snapshot of `notes.trace` at query time — the borg staged-source handle
    /// (the per-trace directory name under the staging root). For the
    /// TranscriptChunk arm, cortex resolves Video/Article transcripts from
    /// `<staging-root>/<trace>/distilled.yml` rather than the note body
    /// (2026-07-07-distillation-output-restore Phase 5). Empty for notes
    /// ingested before the trace column existed, and for the Summary/Claim arms
    /// (which carry their text in `summary`).
    pub trace: String,
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
    for (i, chunk) in stored.as_chunks::<4>().0.iter().enumerate() {
        let v = f32::from_le_bytes(*chunk);
        dot += query_vec[i] * v;
    }
    dot
}

impl SearchIndex {
    /// Brute-force cosine-similarity search over `note_embeddings`.
    ///
    /// Reads every row (`summary`, `transcript-chunk`, and `claim`) for
    /// the active model and aggregates by note via max-pool: a note's
    /// score is `min(distances across all rows for that note)` - the
    /// single best-matching representation wins. In cosine-distance
    /// space smaller is closer, so the minimum over the rows is the
    /// max-pool similarity.
    ///
    /// NAMED CONTINGENCY (Phase 9, retrieval invariant "claim rows add
    /// recall, never displace precision"): the max-pool below is
    /// kind-agnostic, so a note's up-to-24 narrow `claim` vectors get the
    /// same weight as its `summary` vector and can let it win on a
    /// tangential sentence. If the Phase 9 operator eval shows a per-query
    /// nDCG regression on the calibration set, the fix is **kind-weighted
    /// pooling here**: pull `e.kind` in the SELECT and apply a per-kind
    /// distance penalty (e.g. add a small epsilon to claim-row distances)
    /// so claims can only rescue a note the summary missed, never outrank
    /// a note whose summary answered the query. Not implemented until the
    /// eval demands it - see the Phase 9 measurement step.
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
        // Distance asc, then note_path asc as a stable tiebreaker. Without it,
        // notes with bitwise-equal cosine distance fall back to the random
        // HashMap iteration order of `best` above, making both the ranking and
        // (via the truncate) the membership at the limit boundary non-deterministic
        // across runs. (v0.8.56 fixed the same class in RRF + graph_dispatch but
        // missed this raw vector path.)
        hits.sort_by(|a, b| {
            a.distance
                .partial_cmp(&b.distance)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.note_path.cmp(&b.note_path))
        });
        hits.truncate(limit as usize);
        Ok(hits)
    }

    /// Top-`k` semantic neighbors of a note by cosine similarity over the
    /// `summary` `note_embeddings` rows, restricted to similarity
    /// `>= min_cosine`.
    ///
    /// This is the per-note reader the graph pass needs but `search_vector`
    /// does not provide: `search_vector` takes an *external* query vector,
    /// and the per-row BLOB decoder is private. Here we read `note_path`'s own
    /// stored summary vector, then reuse the same zero-allocation
    /// dot-product loop against every *other* note's summary vector. Both
    /// vectors are L2-normalized, so the dot product is cosine similarity in
    /// `[-1, 1]`; larger is closer. Returns `(neighbor_path, cosine)` pairs
    /// sorted by descending similarity, capped at `k`. Returns an empty Vec
    /// when the note has no summary embedding yet (the graph pass skips it).
    pub fn semantic_neighbors(&self, note_path: &str, k: usize, min_cosine: f32) -> Result<Vec<(String, f32)>> {
        let active_model = self.active_embedding_model()?;

        // Read the source note's own summary vector.
        let own: Option<Vec<u8>> = optional_row(self.conn.query_row(
            "SELECT embedding FROM note_embeddings
                 WHERE note_path = ?1 AND kind = ?2 AND model_version = ?3
                 ORDER BY chunk_index LIMIT 1",
            params![note_path, EmbeddingKind::Summary.as_str(), active_model],
            |row| row.get(0),
        ))?;
        let Some(own_bytes) = own else {
            return Ok(vec![]);
        };
        if own_bytes.len() % 4 != 0 {
            eyre::bail!("note {note_path} summary embedding BLOB length not a multiple of 4");
        }
        let query_vec: Vec<f32> = own_bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| f32::from_le_bytes(*c))
            .collect();

        // Scan every other note's summary vector for the active model.
        let mut stmt = self.conn.prepare(
            "SELECT note_path, embedding FROM note_embeddings
             WHERE kind = ?1 AND model_version = ?2 AND note_path != ?3",
        )?;
        let mut hits: Vec<(String, f32)> = Vec::new();
        let mut rows = stmt.query(params![EmbeddingKind::Summary.as_str(), active_model, note_path])?;
        while let Some(row) = rows.next()? {
            let neighbor: String = row.get(0)?;
            let bytes: Vec<u8> = row.get(1)?;
            if bytes.len() != query_vec.len() * 4 {
                log::warn!("semantic_neighbors: dim mismatch for {neighbor}, skipping");
                continue;
            }
            let cosine = dot_product_from_bytes(&query_vec, &bytes);
            if cosine >= min_cosine {
                hits.push((neighbor, cosine));
            }
        }

        hits.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        hits.truncate(k);
        Ok(hits)
    }

    /// Exact pairwise cosine similarity between two notes' stored `summary`
    /// embeddings (active model only). `None` when either note lacks a
    /// summary embedding for the active model.
    ///
    /// Deviates from the design's literal `-> Option<f32>` signature (same
    /// effect, correct seam): reading `note_embeddings` is a fallible SQLite
    /// call like every other reader in this module, so the DB error path is
    /// `Result`-wrapped and only embedding-presence collapses to `Option`.
    ///
    /// Distinct from [`semantic_neighbors`](Self::semantic_neighbors), which
    /// is global-top-k: a genuinely-similar note can be crowded out of the
    /// top-k by unrelated high-similarity notes elsewhere in the vault,
    /// silently misrouting a caller that needs THIS pair's exact similarity
    /// (e.g. `cortex::association`'s same-slug merge-vs-cross-link decision).
    /// This reads exactly the two named rows and dot-products them directly -
    /// no top-k, no other note in the vault can affect the result.
    pub fn cosine_between(&self, note_a: &Path, note_b: &Path) -> Result<Option<f32>> {
        let a = note_a.to_string_lossy();
        let b = note_b.to_string_lossy();
        log::debug!("search::cosine_between: note_a={a} note_b={b}");
        let active_model = self.active_embedding_model()?;

        let bytes_a = self.read_summary_embedding_bytes(&a, &active_model)?;
        let bytes_b = self.read_summary_embedding_bytes(&b, &active_model)?;
        let (Some(bytes_a), Some(bytes_b)) = (bytes_a, bytes_b) else {
            log::debug!("search::cosine_between: uncomputable (missing summary embedding)");
            return Ok(None);
        };
        if bytes_a.len() != bytes_b.len() {
            log::warn!("search::cosine_between: dim mismatch note_a={a} note_b={b}, treating as uncomputable");
            return Ok(None);
        }

        let vec_a: Vec<f32> = bytes_a
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| f32::from_le_bytes(*c))
            .collect();
        let cosine = dot_product_from_bytes(&vec_a, &bytes_b);
        Ok(Some(cosine))
    }

    /// Read one note's own `kind=summary` embedding BLOB for the given
    /// model, or `None` if it has none. Shared by [`cosine_between`](Self::cosine_between).
    fn read_summary_embedding_bytes(&self, note_path: &str, active_model: &str) -> Result<Option<Vec<u8>>> {
        optional_row(self.conn.query_row(
            "SELECT embedding FROM note_embeddings
                 WHERE note_path = ?1 AND kind = ?2 AND model_version = ?3
                 ORDER BY chunk_index LIMIT 1",
            params![note_path, EmbeddingKind::Summary.as_str(), active_model],
            |row| row.get(0),
        ))
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
        // BEGIN IMMEDIATE: acquire the write lock at transaction start, not at
        // first write. transaction_with_behavior issues the IMMEDIATE itself
        // and propagates any failure, unlike the old deferred transaction +
        // swallowed `execute_batch("BEGIN IMMEDIATE").ok()` (which never took
        // the lock up front and hid a SQL error).
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
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
    /// chunk set. Thin wrapper over [`SearchIndex::swap_kind_chunks`].
    pub fn swap_transcript_chunks(
        &mut self,
        note_path: &str,
        chunks: &[(String, Vec<f32>)],
        model_version: &str,
        source_modified_at: i64,
    ) -> Result<()> {
        self.swap_kind_chunks(
            note_path,
            EmbeddingKind::TranscriptChunk,
            chunks,
            model_version,
            source_modified_at,
        )
    }

    /// Atomic swap of every row of one `kind` for a single note, inside
    /// one short write transaction (`BEGIN IMMEDIATE` → DELETE → INSERTs
    /// → `COMMIT`). Generalizes the transcript-chunk swap to any
    /// multi-row kind whose per-chunk identity is not stable across
    /// re-embeds - the Phase 9 `claim` kind reuses it: when a note's
    /// claims change the group boundaries shift, so wipe-and-rewrite in
    /// one transaction keeps hybrid search from seeing a half-replaced
    /// chunk set.
    pub fn swap_kind_chunks(
        &mut self,
        note_path: &str,
        kind: EmbeddingKind,
        chunks: &[(String, Vec<f32>)],
        model_version: &str,
        source_modified_at: i64,
    ) -> Result<()> {
        // BEGIN IMMEDIATE: take the write lock at transaction start. See
        // upsert_embeddings_batch for why the deferred + swallowed-BEGIN form
        // this replaced was wrong.
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "DELETE FROM note_embeddings
             WHERE note_path = ?1 AND kind = ?2",
            params![note_path, kind.as_str()],
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
                    kind.as_str(),
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

    /// Delete every embedding row of a single `kind`, returning the number
    /// of rows removed. This is the first-class rollback verb behind
    /// `sb cortex embed --drop-kind claim` (Phase 9): reverting cortex code
    /// does NOT stop oracle reading claim rows, because `search_vector`
    /// scans all kinds, so removing the rows is the only real rollback.
    pub fn delete_embeddings_of_kind(&self, kind: EmbeddingKind) -> Result<usize> {
        let n = self
            .conn
            .execute("DELETE FROM note_embeddings WHERE kind = ?1", params![kind.as_str()])?;
        Ok(n)
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
    /// Summary rows: every note with a non-empty `notes.summary` column
    /// is a candidate. The indexer fills that column via
    /// `parse_body_summary` with `detail::extract_summary` as a fallback,
    /// so the only notes excluded here are ones whose entire body the
    /// indexer judged unworth summarising. Without this filter, notes
    /// with no summary text would never get an `note_embeddings` row
    /// written and would re-appear in every batch forever (the cortex
    /// embed loop has no skip-sentinel mechanism).
    ///
    /// Transcript-chunk rows: filtered to the kinds listed in
    /// `NoteType::transcript_eligible()`. Without that filter, every
    /// Article and Repo in the vault matches `e.id IS NULL` permanently
    /// and the cortex daemon spins. Driving the filter from the schema
    /// enum (rather than a hand-typed SQL string list) means a future
    /// `NoteType` variant rename cannot silently re-break this path.
    ///
    /// **Examined sentinel (Phase 3,
    /// `docs/design/2026-07-05-cortex-daemon-oscillation-loop.md`).** A
    /// transcript-eligible note with no `## Transcript` section is scanned,
    /// found unembeddable, and writes no `note_embeddings` row - so `e.id`
    /// stays NULL and it re-qualifies every tick forever (~127 notes in the
    /// live vault). Cortex records such a note in `embedding_examined` with
    /// the note's indexed `modified_at`; every arm here LEFT JOINs that side
    /// table and excludes a note whose `examined_at >= n.modified_at`. The
    /// note re-qualifies the moment its indexed `modified_at` advances past
    /// the recorded watermark (keyed on the indexed value, NOT raw filesystem
    /// mtime), exactly like the `note_embeddings.source_modified_at` staleness
    /// watermark above it.
    ///
    /// Claim rows (Phase 9): every note with non-empty `notes.claims` is a
    /// candidate. The claim text is carried in the `summary` field of the
    /// returned `StaleTarget` (no file I/O); cortex groups it into
    /// token-window-sized chunks before embedding.
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
            // Summary: the text carrier is `notes.summary`; `notes.capture_note`
            // is threaded through so the embed text becomes title + capture-note
            // + summary (Phase 9). Notes without a capture note carry '' and the
            // assembled text stays byte-identical to the pre-Phase-9 form.
            EmbeddingKind::Summary => {
                "SELECT n.path, n.note_type, n.modified_at, n.summary, n.title, n.capture_note, n.trace
                 FROM notes n
                 LEFT JOIN note_embeddings e
                   ON e.note_path = n.path
                  AND e.kind = ?1
                  AND e.model_version = ?2
                 LEFT JOIN embedding_examined x
                   ON x.note_path = n.path
                  AND x.kind = ?1
                  AND x.model_version = ?2
                 WHERE (n.summary IS NOT NULL AND n.summary != '')
                   AND (n.superseded_by IS NULL OR n.superseded_by = '')
                   AND (e.id IS NULL
                        OR e.source_modified_at < n.modified_at)
                   AND (x.note_path IS NULL
                        OR x.examined_at < n.modified_at)
                 ORDER BY n.modified_at DESC
                 LIMIT ?3"
                    .to_string()
            }
            EmbeddingKind::TranscriptChunk => format!(
                "SELECT n.path, n.note_type, n.modified_at, '', n.title, '', n.trace
                 FROM notes n
                 LEFT JOIN note_embeddings e
                   ON e.note_path = n.path
                  AND e.kind = ?1
                  AND e.model_version = ?2
                 LEFT JOIN embedding_examined x
                   ON x.note_path = n.path
                  AND x.kind = ?1
                  AND x.model_version = ?2
                 WHERE n.note_type IN ({transcript_eligible_in_clause})
                   AND (n.superseded_by IS NULL OR n.superseded_by = '')
                   AND (e.id IS NULL
                        OR e.source_modified_at < n.modified_at)
                   AND (x.note_path IS NULL
                        OR x.examined_at < n.modified_at)
                 ORDER BY n.modified_at DESC
                 LIMIT ?3"
            ),
            // Claim: the text carrier is `notes.claims` (already populated by
            // the indexer from the `## Claims` body section, no file I/O). Any
            // note with non-empty claims text is a candidate. Cortex groups the
            // newline-joined claims into token-window-sized chunks before
            // embedding so a note's tail claims are never dropped by silent
            // model-side truncation (the Phase 9 defect).
            EmbeddingKind::Claim => "SELECT n.path, n.note_type, n.modified_at, n.claims, n.title, '', n.trace
                 FROM notes n
                 LEFT JOIN note_embeddings e
                   ON e.note_path = n.path
                  AND e.kind = ?1
                  AND e.model_version = ?2
                 LEFT JOIN embedding_examined x
                   ON x.note_path = n.path
                  AND x.kind = ?1
                  AND x.model_version = ?2
                 WHERE (n.claims IS NOT NULL AND n.claims != '')
                   AND (n.superseded_by IS NULL OR n.superseded_by = '')
                   AND (e.id IS NULL
                        OR e.source_modified_at < n.modified_at)
                   AND (x.note_path IS NULL
                        OR x.examined_at < n.modified_at)
                 ORDER BY n.modified_at DESC
                 LIMIT ?3"
                .to_string(),
        };

        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map(params![kind.as_str(), model_version, limit as i64], |row| {
                Ok(StaleTarget {
                    note_path: row.get(0)?,
                    note_type: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                    modified_at: row.get::<_, Option<i64>>(2)?.unwrap_or(0),
                    summary: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
                    title: row.get::<_, Option<String>>(4)?.unwrap_or_default(),
                    capture_note: row.get::<_, Option<String>>(5)?.unwrap_or_default(),
                    trace: row.get::<_, Option<String>>(6)?.unwrap_or_default(),
                })
            })?
            .filter_map(warn_row)
            .collect();
        Ok(rows)
    }

    /// Record that a batch of notes were examined for `kind` embedding and
    /// found to have nothing to embed (e.g. a transcript-eligible note with no
    /// `## Transcript` section). Each item is `(note_path, examined_at)` where
    /// `examined_at` is the note's indexed `notes.modified_at` at examine time;
    /// [`stale_embedding_targets`](Self::stale_embedding_targets) then excludes
    /// the note until its indexed `modified_at` advances past that value.
    ///
    /// The write is ONE short `BEGIN IMMEDIATE` transaction (upsert per row),
    /// so it stays well under the 200 ms budget even for the full ~127-note
    /// skip set and never holds the write lock across CPU work - mirroring
    /// [`upsert_embeddings_batch`](Self::upsert_embeddings_batch). Cortex is the
    /// only writer; oracle only reads the exclusion in `stale_embedding_targets`
    /// (which oracle does not call). An `ON CONFLICT` upsert keeps re-examining
    /// the same note idempotent (the watermark advances to the latest value).
    pub fn mark_embedding_examined_batch(
        &mut self,
        kind: EmbeddingKind,
        model_version: &str,
        items: &[(String, i64)],
    ) -> Result<()> {
        log::debug!(
            "search::mark_embedding_examined_batch: kind={:?} model_version={} count={}",
            kind,
            model_version,
            items.len(),
        );
        if items.is_empty() {
            return Ok(());
        }
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        for (note_path, examined_at) in items {
            tx.execute(
                "INSERT INTO embedding_examined (note_path, kind, model_version, examined_at)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(note_path, kind, model_version)
                 DO UPDATE SET examined_at = excluded.examined_at",
                params![note_path, kind.as_str(), model_version, examined_at],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// The recorded `examined_at` watermark for one note/kind/model, or `None`
    /// when the note has never been marked examined. The note re-qualifies for
    /// a fresh embed attempt once its indexed `notes.modified_at` exceeds this
    /// value. Exposed for tests in other crates and for diagnostics.
    pub fn examined_watermark(&self, note_path: &str, kind: EmbeddingKind, model_version: &str) -> Result<Option<i64>> {
        optional_row(self.conn.query_row(
            "SELECT examined_at FROM embedding_examined
             WHERE note_path = ?1 AND kind = ?2 AND model_version = ?3",
            params![note_path, kind.as_str(), model_version],
            |row| row.get(0),
        ))
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

    /// Count the TranscriptChunk embedding rows for a single note. Lets tests in
    /// other crates assert that the staged-source re-point (2026-07-07
    /// distillation-output-restore) produced chunks for the intended note and
    /// zero for a note it must skip, without reaching into the private `conn`.
    pub fn transcript_chunk_count(&self, note_path: &str) -> Result<i64> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM note_embeddings WHERE note_path = ?1 AND kind = ?2",
            params![note_path, EmbeddingKind::TranscriptChunk.as_str()],
            |row| row.get(0),
        )?;
        Ok(count)
    }

    /// The `text` actually embedded for a note's `kind` row (chunk 0). `None`
    /// when no such row exists. Lets tests in other crates assert what was fed
    /// to the model (e.g. cortex's title+summary prefix) without reaching into
    /// the private `conn`.
    pub fn embedding_text(&self, note_path: &str, kind: EmbeddingKind) -> Result<Option<String>> {
        optional_row(self.conn.query_row(
            "SELECT text FROM note_embeddings WHERE note_path = ?1 AND kind = ?2 AND chunk_index = 0",
            params![note_path, kind.as_str()],
            |row| row.get(0),
        ))
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

    /// Set the `notes.capture_note` column for a test row (Phase 9). Lets
    /// tests in other crates exercise the title + capture-note + summary
    /// embed-text assembly without reaching into the private `conn`.
    pub fn set_test_capture_note(&self, path: &str, capture_note: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE notes SET capture_note = ?2 WHERE path = ?1",
            params![path, capture_note],
        )?;
        Ok(())
    }

    /// Set the `notes.claims` column for a test row (Phase 9). Claims are
    /// stored as newline-joined text (the shape the indexer writes); lets
    /// tests in other crates drive the claim-embedding arm without reaching
    /// into the private `conn`.
    pub fn set_test_claims(&self, path: &str, claims: &str) -> Result<()> {
        self.conn
            .execute("UPDATE notes SET claims = ?2 WHERE path = ?1", params![path, claims])?;
        Ok(())
    }

    /// Set the `notes.trace` column for a test row (2026-07-07 distillation
    /// output restore). The trace is the per-trace staging directory name; lets
    /// tests in other crates drive the staged-transcript embedding arm without
    /// reaching into the private `conn`.
    pub fn set_test_trace(&self, path: &str, trace: &str) -> Result<()> {
        self.conn
            .execute("UPDATE notes SET trace = ?2 WHERE path = ?1", params![path, trace])?;
        Ok(())
    }

    /// Note paths whose newest `summary` embedding (for the active model) was
    /// produced after their semantic edges were last built —
    /// `note_embeddings.produced_at > edge_build_state.semantic_built_at`
    /// (no `edge_build_state` row defaults to 0). This is the **semantic-edge
    /// incremental trigger**: it keys on `produced_at`, NOT
    /// `notes.modified_at`, because `cortex embed` bumps `produced_at` when a
    /// vector lands but never touches `modified_at` — so a note whose
    /// embedding arrives after it was skipped is picked up here (no stranding).
    pub fn semantic_edge_targets(&self) -> Result<Vec<String>> {
        let active_model = self.active_embedding_model()?;
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT e.note_path FROM note_embeddings e
             LEFT JOIN edge_build_state s ON s.note_path = e.note_path
             WHERE e.kind = ?1 AND e.model_version = ?2
               AND e.produced_at > COALESCE(s.semantic_built_at, 0)",
        )?;
        let rows = stmt
            .query_map(params![EmbeddingKind::Summary.as_str(), active_model], |row| {
                row.get::<_, String>(0)
            })?
            .filter_map(warn_row)
            .collect();
        Ok(rows)
    }

    /// Newest `summary`-embedding `produced_at` for one note (0 when it has no
    /// summary embedding for the active model). Persisted as the note's
    /// `semantic_built_at` after its edges are rebuilt.
    pub fn note_summary_produced_at(&self, note_path: &str) -> Result<i64> {
        let active_model = self.active_embedding_model()?;
        let v: i64 = self.conn.query_row(
            "SELECT COALESCE(MAX(produced_at), 0) FROM note_embeddings
             WHERE note_path = ?1 AND kind = ?2 AND model_version = ?3",
            params![note_path, EmbeddingKind::Summary.as_str(), active_model],
            |row| row.get(0),
        )?;
        Ok(v)
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

/// Weighted Reciprocal Rank Fusion over any number of ranked lists.
///
/// Each input is a `(ranked paths, weight)` pair. A note's fused score is
/// the weighted sum across all lists of `weight * 1 / (k + rank)` (rank
/// position 0 = top). A weight of `0.0` makes a list contribute nothing
/// (it is then equivalent to not passing it), so a retriever can be
/// "enabled but demoted out of the result." Notes present in only some
/// lists still contribute (absence adds zero). The result is sorted by
/// descending score and truncated to `limit`.
///
/// `k` is the smoothing constant; the literature's default of 60 (see
/// [`RRF_K`]) keeps the contribution of low-rank hits from dominating.
///
/// The unweighted [`reciprocal_rank_fusion`] is a thin wrapper that passes
/// every list a weight of `1.0`, so it produces bit-identical output to the
/// historical implementation.
pub fn reciprocal_rank_fusion_weighted(lists: &[(&[String], f32)], k: usize, limit: usize) -> Vec<FusedHit> {
    use std::collections::HashMap;
    let mut scores: HashMap<String, f32> = HashMap::new();
    for (list, weight) in lists {
        // A non-positive weight is a true no-op: the list contributes nothing
        // and a note reachable ONLY through it never enters the result. This is
        // how a retriever stays "enabled but demoted out of the fused result"
        // (graph weight 0.0). Skipping (rather than adding a 0.0 score) keeps
        // such notes out instead of parking them at the bottom by tiebreaker.
        if *weight <= 0.0 {
            continue;
        }
        for (rank, path) in list.iter().enumerate() {
            let contrib = weight * (1.0_f32 / (k as f32 + (rank + 1) as f32));
            *scores.entry(path.clone()).or_insert(0.0) += contrib;
        }
    }
    let mut fused: Vec<FusedHit> = scores
        .into_iter()
        .map(|(note_path, score)| FusedHit { note_path, score })
        .collect();
    // Deterministic order: sort by score desc, then by note_path asc as a stable
    // tiebreaker. Without the tiebreaker, tied scores fall back to the random
    // HashMap iteration order above, which makes both the ranking AND (because of
    // the truncate below) the membership at the limit boundary non-deterministic
    // across process runs.
    fused.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.note_path.cmp(&b.note_path))
    });
    fused.truncate(limit);
    fused
}

/// Reciprocal Rank Fusion (Cormack 2009) over any number of ranked lists.
///
/// Each input list is treated as a ranking (position 0 = top). A note's
/// fused score is the sum across all lists of `1 / (k + rank)`. Notes
/// present in only some lists still contribute (absence from a list adds
/// zero). The result is sorted by descending score and truncated to
/// `limit`. Generalized from the original two-list form so `graph-hybrid`
/// can fuse bm25 ⊕ vector ⊕ graph in one pass; the two-list hybrid caller
/// passes `&[&bm25, &vec]` and gets identical output.
///
/// `k` is the smoothing constant; the literature's default of 60 keeps
/// the contribution of low-rank hits from dominating. This is the
/// uniform-weight special case of [`reciprocal_rank_fusion_weighted`].
pub fn reciprocal_rank_fusion(lists: &[&[String]], k: usize, limit: usize) -> Vec<FusedHit> {
    let weighted: Vec<(&[String], f32)> = lists.iter().map(|l| (*l, 1.0_f32)).collect();
    reciprocal_rank_fusion_weighted(&weighted, k, limit)
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
