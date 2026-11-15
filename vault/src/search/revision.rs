//! The embedding revision marker: `note_embeddings.vector_hash`, the
//! lowercase hex SHA-256 of the exact blob bytes a row stores.
//!
//! `note_embeddings.id` is not a marker (`INTEGER PRIMARY KEY` without
//! `AUTOINCREMENT`, and the summary swap deletes before it inserts, so the
//! note holding the max rowid gets its id back), and `produced_at` is whole
//! seconds. The hash changes exactly when the stored vector does, so two
//! consumers key on it: topic reassignment (`note_topics.vector_hash`) and
//! semantic-edge freshness (`edge_build_state.semantic_vector_hash`).
//! Design: `docs/plan/2026-11-14-topic-map.md`, "Embedding revision marker".
//!
//! Not feature gated: the migration runs from `ensure_schema` in every build,
//! because `edge_build_state` exists in every build and its backfill reads the
//! hashes.

use eyre::Result;
use rusqlite::{Connection, params};
use sha2::{Digest, Sha256};

use super::{SearchIndex, optional_row};

/// Rows hashed per statement round during the backfill. Bounds the blob bytes
/// held in memory at once; the whole backfill is still one transaction.
const BACKFILL_CHUNK: i64 = 2048;

/// Lowercase hex SHA-256 of a stored vector blob (the exact little-endian f32
/// bytes in `note_embeddings.embedding`). The one function every
/// `note_embeddings` writer and the migration call.
pub fn vector_hash(blob: &[u8]) -> String {
    Sha256::digest(blob).iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The error every marker reader returns for a `note_embeddings` row with a
/// NULL `vector_hash`. `migrate_vector_hash` leaves none on open and every
/// writer sets one, so a NULL here is a writer that bypassed both.
#[cfg(any(feature = "vec-candle", feature = "vec-onnx"))]
pub(crate) fn unhashed_row(note_path: &str) -> eyre::Report {
    eyre::eyre!(
        "{note_path}: note_embeddings row has no vector_hash; every writer sets it and \
         reopening the index backfills it"
    )
}

/// Column names of `table`, empty when the table does not exist.
fn table_columns(conn: &Connection, table: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let columns = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(columns)
}

/// What [`SearchIndex::migrate_vector_hash`] changed, for its log line and tests.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct VectorHashMigration {
    /// `note_embeddings.vector_hash` was added by this run.
    pub added_column: bool,
    /// `note_embeddings` rows whose NULL hash this run filled.
    pub hashed_rows: usize,
    /// `edge_build_state` moved from `semantic_built_at` to `semantic_vector_hash`.
    pub swapped_edge_state: bool,
    /// Notes fresh under the old rule, given their current summary hash.
    pub fresh_notes: usize,
}

impl VectorHashMigration {
    fn changed(&self) -> bool {
        self.added_column || self.hashed_rows > 0 || self.swapped_edge_state
    }
}

impl SearchIndex {
    /// Bring every index to the revision-marker shape, in one
    /// `BEGIN IMMEDIATE` transaction, idempotent:
    ///
    /// - add `note_embeddings.vector_hash` when missing, and hash every row
    ///   whose hash is NULL (a fresh column, or a row an older binary
    ///   wrote). `idx_note_embeddings_unhashed` is a partial index over exactly
    ///   those rows, so the steady-state check on every open reads an empty
    ///   index instead of every blob;
    /// - move `edge_build_state` from `semantic_built_at` to
    ///   `semantic_vector_hash`: a note fresh under the old rule
    ///   (`semantic_built_at >= MAX(produced_at)` of its active-model summary)
    ///   gets that summary's current hash, every other note stays NULL and is
    ///   rebuilt on the next pass, then `semantic_built_at` is dropped.
    ///
    /// After it returns, no `note_embeddings` row has a NULL hash.
    pub(crate) fn migrate_vector_hash(&self) -> Result<VectorHashMigration> {
        let embedding_columns = table_columns(&self.conn, "note_embeddings")?;
        let has_embeddings = !embedding_columns.is_empty();
        let needs_column = has_embeddings && !embedding_columns.iter().any(|c| c == "vector_hash");
        let edge_columns = table_columns(&self.conn, "edge_build_state")?;
        let needs_edge_swap = edge_columns.iter().any(|c| c == "semantic_built_at");

        let unhashed_present = if has_embeddings && !needs_column {
            self.conn.execute_batch(
                "CREATE INDEX IF NOT EXISTS idx_note_embeddings_unhashed
                    ON note_embeddings(id) WHERE vector_hash IS NULL;",
            )?;
            optional_row(self.conn.query_row(
                "SELECT 1 FROM note_embeddings WHERE vector_hash IS NULL LIMIT 1",
                [],
                |row| row.get::<_, i64>(0),
            ))?
            .is_some()
        } else {
            false
        };
        if !needs_column && !unhashed_present && !needs_edge_swap {
            return Ok(VectorHashMigration::default());
        }

        self.conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| -> Result<VectorHashMigration> {
            let mut report = VectorHashMigration::default();
            if has_embeddings {
                if needs_column {
                    self.conn
                        .execute_batch("ALTER TABLE note_embeddings ADD COLUMN vector_hash TEXT;")?;
                    report.added_column = true;
                }
                self.conn.execute_batch(
                    "CREATE INDEX IF NOT EXISTS idx_note_embeddings_unhashed
                        ON note_embeddings(id) WHERE vector_hash IS NULL;",
                )?;
                report.hashed_rows = self.hash_unhashed_rows()?;
            }
            if needs_edge_swap {
                report.fresh_notes = self.swap_edge_state_to_vector_hash(has_embeddings)?;
                report.swapped_edge_state = true;
            }
            Ok(report)
        })();

        match result {
            Ok(report) => {
                self.conn.execute_batch("COMMIT")?;
                if report.changed() {
                    log::info!(
                        "search::migrate: vector_hash added_column={} hashed_rows={} \
                         edge_state_swapped={} fresh_notes={}",
                        report.added_column,
                        report.hashed_rows,
                        report.swapped_edge_state,
                        report.fresh_notes
                    );
                }
                Ok(report)
            }
            Err(e) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                Err(e)
            }
        }
    }

    /// Hash every `note_embeddings` row whose hash is NULL, in chunks. Runs
    /// inside the caller's transaction. Each UPDATE drops the row out of the
    /// partial index, so the next chunk's SELECT starts on the rows still left.
    fn hash_unhashed_rows(&self) -> Result<usize> {
        let mut select = self
            .conn
            .prepare("SELECT id, embedding FROM note_embeddings WHERE vector_hash IS NULL LIMIT ?1")?;
        let mut update = self
            .conn
            .prepare("UPDATE note_embeddings SET vector_hash = ?2 WHERE id = ?1")?;
        let mut hashed = 0usize;
        loop {
            let chunk = select
                .query_map(params![BACKFILL_CHUNK], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            if chunk.is_empty() {
                return Ok(hashed);
            }
            for (id, blob) in &chunk {
                update.execute(params![id, vector_hash(blob)])?;
            }
            hashed += chunk.len();
        }
    }

    /// Add `semantic_vector_hash`, backfill the notes fresh under the
    /// `produced_at` rule, drop `semantic_built_at`. Runs inside the caller's
    /// transaction. Returns the notes backfilled.
    fn swap_edge_state_to_vector_hash(&self, has_embeddings: bool) -> Result<usize> {
        let edge_columns = table_columns(&self.conn, "edge_build_state")?;
        if !edge_columns.iter().any(|c| c == "semantic_vector_hash") {
            self.conn
                .execute_batch("ALTER TABLE edge_build_state ADD COLUMN semantic_vector_hash TEXT;")?;
        }
        let active_model: Option<String> = if has_embeddings {
            optional_row(self.conn.query_row(
                "SELECT value FROM embedding_config WHERE key = 'active_model'",
                [],
                |row| row.get(0),
            ))?
        } else {
            None
        };
        let fresh = match active_model {
            // A note with no active-model summary has a NULL MAX, which no
            // comparison satisfies: it stays NULL, as the old rule left it out.
            Some(model) => self.conn.execute(
                "UPDATE edge_build_state
                    SET semantic_vector_hash = (
                        SELECT e.vector_hash FROM note_embeddings e
                         WHERE e.note_path = edge_build_state.note_path
                           AND e.kind = 'summary' AND e.model_version = ?1
                         ORDER BY e.chunk_index LIMIT 1)
                  WHERE semantic_built_at >= (
                        SELECT MAX(e.produced_at) FROM note_embeddings e
                         WHERE e.note_path = edge_build_state.note_path
                           AND e.kind = 'summary' AND e.model_version = ?1)",
                params![model],
            )?,
            None => 0,
        };
        self.conn
            .execute_batch("ALTER TABLE edge_build_state DROP COLUMN semantic_built_at;")?;
        Ok(fresh)
    }
}

#[cfg(any(test, feature = "test-util"))]
impl SearchIndex {
    /// Every `note_embeddings` row as `(note_path, id)`, ordered by id, so a
    /// test can find the note holding the max rowid and prove a re-embed
    /// gave its id back.
    pub fn test_embedding_ids(&self) -> Result<Vec<(String, i64)>> {
        let rows = self
            .conn
            .prepare("SELECT note_path, id FROM note_embeddings ORDER BY id")?
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
}

#[cfg(test)]
mod tests;
