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
use rusqlite::{Connection, Transaction, TransactionBehavior, params};
use sha2::{Digest, Sha256};

use super::{SearchIndex, optional_row};

/// Rows hashed per write transaction during the backfill. Each batch is its
/// own `BEGIN IMMEDIATE`, so the write lock is held for one batch (about 2 MB
/// of WAL at the live row size), never for the whole table: a concurrent
/// opener waits at most one batch, well inside `BUSY_TIMEOUT`.
pub(crate) const BACKFILL_BATCH: i64 = 512;

/// The partial index over rows still waiting for a hash.
const UNHASHED_INDEX: &str = "idx_note_embeddings_unhashed";

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

/// A point in [`SearchIndex::migrate_vector_hash_with`] where no transaction
/// is open, so another connection can take the write lock. The test seam
/// runs a second opener at each.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MigrationStep {
    /// The read-only plan found work; nothing is written yet.
    Planned,
    /// The column transaction committed.
    ColumnChecked,
    /// One backfill batch committed.
    Batch,
    /// The partial index transaction committed.
    Indexed,
}

/// What the read-only first look found. Only gates whether a step is tried:
/// every step re-reads the schema inside its own write transaction, because
/// another opener may have done the step since.
#[derive(Debug, Clone, Copy)]
struct MigrationPlan {
    has_embeddings: bool,
    needs_column: bool,
    has_index: bool,
    unhashed_present: bool,
    needs_edge_swap: bool,
}

impl MigrationPlan {
    fn is_noop(&self) -> bool {
        !self.needs_column && self.has_index && !self.unhashed_present && !self.needs_edge_swap
    }
}

impl SearchIndex {
    /// Bring every index to the revision-marker shape, idempotent and safe
    /// against a concurrent opener running the same migration:
    ///
    /// - add `note_embeddings.vector_hash` when missing (one short
    ///   transaction, `ALTER ADD COLUMN` rewrites no rows);
    /// - hash every row whose hash is NULL (a fresh column, or a row an older
    ///   binary wrote), [`BACKFILL_BATCH`] rows per transaction. A NULL hash
    ///   marks a pending row, so an interrupted backfill resumes on the next
    ///   open. The first pass walks rowids, then creates
    ///   `idx_note_embeddings_unhashed` (a partial index over exactly the NULL
    ///   rows, built from pages the walk just read), and later passes drain
    ///   that index, so the steady-state check on every open reads an empty
    ///   index instead of every blob;
    /// - move `edge_build_state` from `semantic_built_at` to
    ///   `semantic_vector_hash` in one short transaction: a note fresh under
    ///   the old rule (`semantic_built_at >= MAX(produced_at)` of its
    ///   active-model summary) gets that summary's current hash, every other
    ///   note stays NULL and is rebuilt on the next pass, then
    ///   `semantic_built_at` is dropped.
    ///
    /// No write lock is held between steps, and each step decides inside its
    /// own `BEGIN IMMEDIATE`, after the lock: a second opener that read the old
    /// shape finds the step done and skips it. After it returns, no
    /// `note_embeddings` row has a NULL hash.
    pub(crate) fn migrate_vector_hash(&self) -> Result<VectorHashMigration> {
        self.migrate_vector_hash_with(BACKFILL_BATCH, &|_| {})
    }

    /// [`Self::migrate_vector_hash`] with the batch size and a hook run at
    /// every [`MigrationStep`], where no transaction is open.
    pub(crate) fn migrate_vector_hash_with(
        &self,
        batch: i64,
        between: &dyn Fn(MigrationStep),
    ) -> Result<VectorHashMigration> {
        let plan = self.plan_vector_hash_migration()?;
        log::debug!("search::migrate: vector_hash plan={plan:?}");
        if plan.is_noop() {
            return Ok(VectorHashMigration::default());
        }
        between(MigrationStep::Planned);

        let mut report = VectorHashMigration::default();
        if plan.has_embeddings {
            if plan.needs_column {
                report.added_column = self.add_vector_hash_column()?;
                between(MigrationStep::ColumnChecked);
            }
            if !plan.has_index {
                report.hashed_rows += self.hash_rows_by_rowid(batch, between)?;
                self.create_unhashed_index()?;
                between(MigrationStep::Indexed);
            }
            report.hashed_rows += self.hash_indexed_unhashed_rows(batch, between)?;
        }
        if plan.needs_edge_swap
            && let Some(fresh_notes) = self.swap_edge_state_to_vector_hash()?
        {
            report.swapped_edge_state = true;
            report.fresh_notes = fresh_notes;
        }

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

    /// Read-only first look, no write lock: decides only whether to try.
    fn plan_vector_hash_migration(&self) -> Result<MigrationPlan> {
        let embedding_columns = table_columns(&self.conn, "note_embeddings")?;
        let has_embeddings = !embedding_columns.is_empty();
        let needs_column = has_embeddings && !embedding_columns.iter().any(|c| c == "vector_hash");
        let has_index = !has_embeddings || index_exists(&self.conn, UNHASHED_INDEX)?;
        // Only read through the partial index; without it this would scan
        // every blob, and a missing index already means there is work.
        let unhashed_present = has_embeddings
            && !needs_column
            && has_index
            && optional_row(self.conn.query_row(
                "SELECT 1 FROM note_embeddings WHERE vector_hash IS NULL LIMIT 1",
                [],
                |row| row.get::<_, i64>(0),
            ))?
            .is_some();
        let needs_edge_swap = table_columns(&self.conn, "edge_build_state")?
            .iter()
            .any(|c| c == "semantic_built_at");
        Ok(MigrationPlan {
            has_embeddings,
            needs_column,
            has_index,
            unhashed_present,
            needs_edge_swap,
        })
    }

    /// Add `vector_hash` if it is still missing once the lock is held.
    /// Returns whether this call added it.
    fn add_vector_hash_column(&self) -> Result<bool> {
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let columns = table_columns(&tx, "note_embeddings")?;
        let added = !columns.is_empty() && !columns.iter().any(|c| c == "vector_hash");
        if added {
            tx.execute_batch("ALTER TABLE note_embeddings ADD COLUMN vector_hash TEXT;")?;
        }
        tx.commit()?;
        Ok(added)
    }

    /// The first pass, before the partial index exists: walk every row in
    /// rowid order, `batch` rows per transaction, hashing the ones whose hash
    /// is NULL. Each batch reads at most `batch` rows whatever another opener
    /// already hashed.
    fn hash_rows_by_rowid(&self, batch: i64, between: &dyn Fn(MigrationStep)) -> Result<usize> {
        let mut hashed = 0usize;
        let mut after = i64::MIN;
        loop {
            let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
            let rows = tx
                .prepare(
                    "SELECT id, CASE WHEN vector_hash IS NULL THEN embedding END
                       FROM note_embeddings WHERE id > ?1 ORDER BY id LIMIT ?2",
                )?
                .query_map(params![after, batch], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, Option<Vec<u8>>>(1)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let Some(&(last, _)) = rows.last() else {
                tx.commit()?;
                return Ok(hashed);
            };
            hashed += set_hashes(&tx, rows.iter().filter_map(|(id, blob)| Some((*id, blob.as_deref()?))))?;
            tx.commit()?;
            after = last;
            between(MigrationStep::Batch);
        }
    }

    fn create_unhashed_index(&self) -> Result<()> {
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        tx.execute_batch(&format!(
            "CREATE INDEX IF NOT EXISTS {UNHASHED_INDEX}
                ON note_embeddings(id) WHERE vector_hash IS NULL;"
        ))?;
        tx.commit()?;
        Ok(())
    }

    /// Drain the partial index, `batch` rows per transaction: rows an older
    /// binary wrote, and any left by an interrupted or concurrent walk.
    fn hash_indexed_unhashed_rows(&self, batch: i64, between: &dyn Fn(MigrationStep)) -> Result<usize> {
        let mut hashed = 0usize;
        loop {
            let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
            let rows = tx
                .prepare("SELECT id, embedding FROM note_embeddings WHERE vector_hash IS NULL LIMIT ?1")?
                .query_map(params![batch], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            if rows.is_empty() {
                tx.commit()?;
                return Ok(hashed);
            }
            hashed += set_hashes(&tx, rows.iter().map(|(id, blob)| (*id, blob.as_slice())))?;
            tx.commit()?;
            between(MigrationStep::Batch);
        }
    }

    /// Add `semantic_vector_hash`, backfill the notes fresh under the
    /// `produced_at` rule, drop `semantic_built_at`, in one transaction that
    /// first re-checks `semantic_built_at` is still there. Returns the notes
    /// backfilled, or `None` when another opener already swapped.
    fn swap_edge_state_to_vector_hash(&self) -> Result<Option<usize>> {
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let edge_columns = table_columns(&tx, "edge_build_state")?;
        if !edge_columns.iter().any(|c| c == "semantic_built_at") {
            tx.commit()?;
            return Ok(None);
        }
        if !edge_columns.iter().any(|c| c == "semantic_vector_hash") {
            tx.execute_batch("ALTER TABLE edge_build_state ADD COLUMN semantic_vector_hash TEXT;")?;
        }
        let active_model: Option<String> = if table_columns(&tx, "note_embeddings")?.is_empty() {
            None
        } else {
            optional_row(tx.query_row(
                "SELECT value FROM embedding_config WHERE key = 'active_model'",
                [],
                |row| row.get(0),
            ))?
        };
        let fresh = match active_model {
            // A note with no active-model summary has a NULL MAX, which no
            // comparison satisfies: it stays NULL, as the old rule left it out.
            Some(model) => tx.execute(
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
        tx.execute_batch("ALTER TABLE edge_build_state DROP COLUMN semantic_built_at;")?;
        tx.commit()?;
        Ok(Some(fresh))
    }
}

/// Store `vector_hash(blob)` on each `(id, blob)`, inside the caller's
/// transaction. Returns the rows written.
fn set_hashes<'a>(conn: &Connection, rows: impl Iterator<Item = (i64, &'a [u8])>) -> Result<usize> {
    let mut update = conn.prepare("UPDATE note_embeddings SET vector_hash = ?2 WHERE id = ?1")?;
    let mut written = 0usize;
    for (id, blob) in rows {
        update.execute(params![id, vector_hash(blob)])?;
        written += 1;
    }
    Ok(written)
}

/// Whether an index named `name` exists. Reads `sqlite_schema` itself, not the
/// connection's cached schema.
fn index_exists(conn: &Connection, name: &str) -> Result<bool> {
    Ok(optional_row(conn.query_row(
        "SELECT 1 FROM sqlite_schema WHERE type = 'index' AND name = ?1",
        params![name],
        |row| row.get::<_, i64>(0),
    ))?
    .is_some())
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
