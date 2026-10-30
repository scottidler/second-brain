use super::*;

/// Backend-suffixed model ids a shipped binary wrote, each paired with the
/// backend-free id it migrates to. `-fastembed` ids are absent on purpose: no
/// shipped binary wrote them, so any such rows stay stale and re-embed.
#[cfg(any(feature = "vec-candle", feature = "vec-onnx"))]
pub(crate) const LEGACY_MODEL_IDS: &[(&str, &str)] = &[
    ("bge-small-en-v1.5-candle", crate::embedding::BGE_SMALL_MODEL_VERSION),
    ("bge-base-en-v1.5-candle", crate::embedding::BGE_BASE_MODEL_VERSION),
];

/// The backend-free id a legacy pin migrates to on the next open, or `None`
/// for an id the migration does not own.
#[cfg(any(feature = "vec-candle", feature = "vec-onnx"))]
pub fn migrated_model_id(legacy: &str) -> Option<&'static str> {
    LEGACY_MODEL_IDS
        .iter()
        .find(|(old, _)| *old == legacy)
        .map(|(_, current)| *current)
}

impl super::SearchIndex {
    pub(crate) fn ensure_schema(&self) -> Result<()> {
        // Fresh-DB path: create the content table with every column. Existing
        // DBs land in the migration helpers below.
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS notes (
                path TEXT PRIMARY KEY,
                title TEXT,
                domain TEXT,
                note_type TEXT,
                origin TEXT,
                status TEXT,
                date TEXT,
                tags TEXT,
                source TEXT,
                creator TEXT,
                body TEXT,
                summary TEXT,
                claims TEXT DEFAULT '',
                capture_note TEXT DEFAULT '',
                modified_at INTEGER,
                quality TEXT DEFAULT '',
                classified INTEGER DEFAULT 0,
                classified_by TEXT DEFAULT '',
                confidence TEXT DEFAULT '',
                needs_review INTEGER DEFAULT 0,
                duplicate_group TEXT DEFAULT '',
                cortex_repo_stars INTEGER,
                cortex_repo_last_commit TEXT,
                cortex_repo_primary_language TEXT,
                cortex_video_duration_seconds INTEGER,
                cortex_video_channel TEXT,
                cortex_video_published_at TEXT,
                cortex_thread_platform TEXT,
                cortex_thread_post_count INTEGER,
                cortex_thread_author TEXT,
                search_hit_count INTEGER DEFAULT 0,
                last_accessed_at INTEGER,
                inbound_link_count INTEGER DEFAULT 0,
                pinned INTEGER DEFAULT 0,
                trace TEXT DEFAULT '',
                ingested TEXT DEFAULT '',
                trace_expires TEXT DEFAULT '',
                superseded_by TEXT DEFAULT ''
            );

            -- The tags facet. `notes.tags` keeps its JSON array for FTS and
            -- for `NoteRow`; this table is what makes a tag filter an index
            -- lookup instead of a full scan plus JSON parse, which is what
            -- `idx_notes_domain` gave the column it replaces.
            --
            -- No foreign key, by choice: the connection does run with
            -- `PRAGMA foreign_keys=ON` (`SearchIndex::open` / `open_memory`),
            -- and `note_embeddings` relies on it for its CASCADE (see
            -- `vec_schema_fk_pragma_must_be_on_for_cascade`). This table is
            -- instead maintained explicitly: `index_one` rewrites a note's
            -- rows inside the per-note SAVEPOINT, and `remove_note` deletes
            -- them with the note in both stale-removal paths. Keeping the
            -- maintenance in code means the facet is rebuilt from the same
            -- deduped list as `notes.tags`, which a CASCADE alone would not
            -- guarantee.
            CREATE TABLE IF NOT EXISTS note_tags (
                path TEXT NOT NULL,
                tag  TEXT NOT NULL,
                PRIMARY KEY (path, tag)
            );
            CREATE INDEX IF NOT EXISTS idx_note_tags_tag ON note_tags(tag);

            CREATE INDEX IF NOT EXISTS idx_notes_domain ON notes(domain);
            CREATE INDEX IF NOT EXISTS idx_notes_note_type ON notes(note_type);
            CREATE INDEX IF NOT EXISTS idx_notes_status ON notes(status);
            -- The cold-note SELECT filters and orders by `date`; this index
            -- keeps it index-backed instead of a full scan.
            CREATE INDEX IF NOT EXISTS idx_notes_date ON notes(date);
            -- `modified_at` is still read by other code paths; keep its index
            -- even though the cold SELECT no longer filters on it.
            CREATE INDEX IF NOT EXISTS idx_notes_modified_at ON notes(modified_at);",
        )?;

        self.ensure_graph_schema()?;

        // Migrate older DBs: add columns the CREATE TABLE above lists but
        // existing schemas may be missing.
        self.ensure_governance_columns()?;
        self.ensure_distilled_columns()?;
        self.ensure_trace_columns()?;
        self.ensure_repo_columns()?;
        self.ensure_repos_touched_column()?;
        self.ensure_superseded_by_column()?;
        self.ensure_channel_handle_column()?;

        // FTS5 cannot ALTER, so we detect old (no-claims) schemas and rebuild.
        // Triggers attach to `notes`, not `notes_fts`, so they must be dropped
        // explicitly before the FTS table; CREATE TRIGGER would otherwise fail
        // with "trigger already exists" on the recreate path.
        self.ensure_fts5_schema()?;

        #[cfg(any(feature = "vec-candle", feature = "vec-onnx"))]
        self.ensure_vec_schema()?;

        Ok(())
    }

    /// Create the embedding tables and the `embedding_config` key/value row
    /// used as the single source of truth for the active embedding model.
    ///
    /// Storage shape is deliberately boring: one regular table with a `BLOB`
    /// column for the f32 vector and an explicit `dim` column for length
    /// validation. No virtual table, no SQLite extension, no triggers; FK
    /// CASCADE works natively. The hybrid retrieval path runs FTS5 and
    /// vector as two separate queries and fuses them with RRF in Rust, so
    /// there is no single-query SQL composition that would benefit from
    /// a `vec0` virtual table.
    ///
    /// Idempotent: every statement uses `IF NOT EXISTS` (or `INSERT OR
    /// IGNORE` for the config row) so existing DBs upgrade without data
    /// loss.
    #[cfg(any(feature = "vec-candle", feature = "vec-onnx"))]
    fn ensure_vec_schema(&self) -> Result<()> {
        // Fresh DBs get the widened CHECK immediately (`'claim'` included).
        // Existing DBs created before the claim kind keep the old
        // two-value CHECK here (CREATE TABLE IF NOT EXISTS is a no-op) and
        // are widened by `migrate_note_embeddings_add_claim_kind` below.
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS note_embeddings (
                id INTEGER PRIMARY KEY,
                note_path TEXT NOT NULL,
                kind TEXT NOT NULL CHECK (kind IN ('summary', 'transcript-chunk', 'claim')),
                chunk_index INTEGER NOT NULL DEFAULT 0,
                text TEXT NOT NULL,
                embedding BLOB NOT NULL,
                dim INTEGER NOT NULL,
                model_version TEXT NOT NULL,
                produced_at INTEGER NOT NULL,
                source_modified_at INTEGER NOT NULL,
                recipe INTEGER NOT NULL DEFAULT 1,
                FOREIGN KEY (note_path) REFERENCES notes(path) ON DELETE CASCADE,
                UNIQUE (note_path, kind, chunk_index, model_version)
            );",
        )?;

        // SQLite cannot ALTER a CHECK constraint, so a DB created before the claim kind needs a
        // table rebuild to permit the `'claim'` kind. Run it before the index
        // creation below so the rebuilt table's indexes are (re)created here.
        self.migrate_note_embeddings_add_claim_kind()?;

        // Indexes are idempotent (`IF NOT EXISTS`) and are also recreated
        // inside the migration's transaction; this covers the fresh-DB path
        // and the already-migrated path.
        self.conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_note_embeddings_path
                ON note_embeddings(note_path);
            CREATE INDEX IF NOT EXISTS idx_note_embeddings_stale
                ON note_embeddings(source_modified_at);
            CREATE INDEX IF NOT EXISTS idx_note_embeddings_kind_model
                ON note_embeddings(kind, model_version);

            -- See docs/design/2026-07-05-cortex-daemon-oscillation-loop.md:
            -- the 'examined, nothing to embed' sentinel. A transcript-eligible
            -- note with no `## Transcript` section is scanned by cortex's embed
            -- loop, produces no `note_embeddings` row, and would therefore be
            -- re-selected as stale on every tick forever. This side table (NOT
            -- a `note_embeddings` row - that table's NOT NULL embedding/dim
            -- columns and the sentinel-blind `search_vector` scan mean a
            -- tombstone there would poison cosine similarity) records the note's
            -- indexed `notes.modified_at` at examine time. `stale_embedding_targets`
            -- excludes the note until `notes.modified_at` advances past
            -- `examined_at`, mirroring the `note_embeddings.source_modified_at`
            -- staleness watermark and the `edge_build_state` incremental pattern.
            -- FK CASCADE drops the row natively when the note leaves `notes`.
            CREATE TABLE IF NOT EXISTS embedding_examined (
                note_path     TEXT NOT NULL,
                kind          TEXT NOT NULL,
                model_version TEXT NOT NULL,
                examined_at   INTEGER NOT NULL,
                recipe        INTEGER NOT NULL DEFAULT 1,
                PRIMARY KEY (note_path, kind, model_version),
                FOREIGN KEY (note_path) REFERENCES notes(path) ON DELETE CASCADE
            );

            CREATE TABLE IF NOT EXISTS embedding_config (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            INSERT OR IGNORE INTO embedding_config (key, value)
                VALUES ('active_dim', '384');",
        )?;
        self.ensure_recipe_columns()?;
        // Before the seed below: a pre-existing `-candle` pin is renamed in
        // place, and a fresh DB has no pin for the seed to collide with.
        self.migrate_backend_free_model_ids()?;
        self.conn.execute(
            "INSERT OR IGNORE INTO embedding_config (key, value) VALUES ('active_model', ?1)",
            rusqlite::params![crate::embedding::ACTIVE_MODEL_VERSION],
        )?;
        Ok(())
    }

    /// Add `recipe` to `note_embeddings` and `embedding_examined` on DBs
    /// created before embed-recipe versioning
    /// (`docs/design/2026-10-29-retrieval-quality-from-rag-scour.md`),
    /// and the `(note_path, kind, model_version)` index the stale query joins on.
    ///
    /// Existing rows read as recipe 1, the recipe every kind had before the
    /// column existed. Same idempotent `PRAGMA table_info` probe + single
    /// `ALTER ADD COLUMN` pattern as `ensure_trace_columns`. Runs after
    /// `migrate_note_embeddings_add_claim_kind`, whose rebuild copies an
    /// explicit pre-recipe column list.
    ///
    /// The index is load-bearing for latency, not correctness: without it the
    /// planner joins `note_embeddings` per note through
    /// `idx_note_embeddings_kind_model`, whose `(kind, model_version)` prefix
    /// matches every chunk row, and one stale read on the live DB took 18-24 s.
    #[cfg(any(feature = "vec-candle", feature = "vec-onnx"))]
    pub(crate) fn ensure_recipe_columns(&self) -> Result<()> {
        for table in ["note_embeddings", "embedding_examined"] {
            let mut stmt = self.conn.prepare(&format!("PRAGMA table_info({table})"))?;
            let columns: Vec<String> = stmt
                .query_map([], |row| row.get::<_, String>(1))?
                .filter_map(warn_row)
                .collect();
            if !columns.iter().any(|c| c == "recipe") {
                log::info!("search::migrate: adding {table}.recipe (existing rows read as recipe 1)");
                self.conn.execute_batch(&format!(
                    "ALTER TABLE {table} ADD COLUMN recipe INTEGER NOT NULL DEFAULT 1;"
                ))?;
            }
        }
        self.conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_note_embeddings_note_kind_model
                ON note_embeddings(note_path, kind, model_version);",
        )?;
        Ok(())
    }

    /// Widen the `note_embeddings.kind` CHECK constraint to include
    /// `'claim'` (see
    /// `docs/design/2026-07-05-distillation-knowledge-extraction.md`).
    ///
    /// SQLite cannot `ALTER` a CHECK, so this rebuilds the table preserving
    /// every existing row. Returns `true` if it rebuilt, `false` if the
    /// table was already current (or absent). Guarantees, per the design's
    /// migration spec:
    ///
    /// - **One transaction.** `BEGIN IMMEDIATE` → build → swap → `COMMIT`;
    ///   any error rolls the whole thing back.
    /// - **Idempotent.** Inspects the stored DDL in `sqlite_master.sql` for
    ///   the `'claim'` literal in the CHECK; if already present, no-op.
    /// - **Crash-safe.** The new table is built under a temp name and the
    ///   old table is dropped/renamed LAST, so a crash mid-migration leaves
    ///   the original `note_embeddings` intact (and the wrapping transaction
    ///   makes the whole rebuild atomic regardless).
    /// - **Row-preserving.** `INSERT ... SELECT` copies every column
    ///   (including `id`) verbatim, so existing summary/transcript rows
    ///   survive byte-identical.
    /// - **Indexes recreated; `embedding_config` untouched** (a separate
    ///   table the rebuild never drops).
    #[cfg(any(feature = "vec-candle", feature = "vec-onnx"))]
    pub(crate) fn migrate_note_embeddings_add_claim_kind(&self) -> Result<bool> {
        let stored_sql: Option<String> = super::optional_row(self.conn.query_row(
            "SELECT sql FROM sqlite_master WHERE type='table' AND name='note_embeddings'",
            [],
            |row| row.get(0),
        ))?;

        let Some(stored_sql) = stored_sql else {
            // No table yet - the fresh-DB CREATE above already wrote the
            // widened CHECK, so there is nothing to migrate.
            return Ok(false);
        };
        if stored_sql.contains("'claim'") {
            // CHECK already permits 'claim'; idempotent no-op.
            return Ok(false);
        }

        log::info!("search::migrate: rebuilding note_embeddings to widen kind CHECK for 'claim'");

        self.conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| -> Result<()> {
            self.conn.execute_batch(
                "CREATE TABLE note_embeddings_new (
                    id INTEGER PRIMARY KEY,
                    note_path TEXT NOT NULL,
                    kind TEXT NOT NULL CHECK (kind IN ('summary', 'transcript-chunk', 'claim')),
                    chunk_index INTEGER NOT NULL DEFAULT 0,
                    text TEXT NOT NULL,
                    embedding BLOB NOT NULL,
                    dim INTEGER NOT NULL,
                    model_version TEXT NOT NULL,
                    produced_at INTEGER NOT NULL,
                    source_modified_at INTEGER NOT NULL,
                    FOREIGN KEY (note_path) REFERENCES notes(path) ON DELETE CASCADE,
                    UNIQUE (note_path, kind, chunk_index, model_version)
                );

                INSERT INTO note_embeddings_new (
                    id, note_path, kind, chunk_index, text, embedding, dim,
                    model_version, produced_at, source_modified_at
                )
                SELECT
                    id, note_path, kind, chunk_index, text, embedding, dim,
                    model_version, produced_at, source_modified_at
                FROM note_embeddings;

                DROP TABLE note_embeddings;
                ALTER TABLE note_embeddings_new RENAME TO note_embeddings;

                CREATE INDEX IF NOT EXISTS idx_note_embeddings_path
                    ON note_embeddings(note_path);
                CREATE INDEX IF NOT EXISTS idx_note_embeddings_stale
                    ON note_embeddings(source_modified_at);
                CREATE INDEX IF NOT EXISTS idx_note_embeddings_kind_model
                    ON note_embeddings(kind, model_version);",
            )?;
            Ok(())
        })();

        match result {
            Ok(()) => {
                self.conn.execute_batch("COMMIT")?;
                Ok(true)
            }
            Err(e) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                Err(e)
            }
        }
    }

    /// Rename the backend-suffixed model ids in [`LEGACY_MODEL_IDS`] to their
    /// backend-free ids in every place a model id is stored:
    /// `embedding_config.active_model`, `note_embeddings.model_version` and
    /// `embedding_examined.model_version` (the full set in this schema;
    /// `docs/design/2026-10-27-cpu-tier-inference-backend.md`).
    ///
    /// Returns `true` if it changed anything. Guarantees:
    ///
    /// - **Idempotent and cheap when done.** A read-only probe runs first;
    ///   with no legacy id left anywhere it returns without taking the write
    ///   lock, so every later open pays only that read.
    /// - **Dedup before rename.** An old cortex still running across a deploy
    ///   can write `-candle` rows after a new binary renamed the DB, so both
    ///   ids can hold the same `(note_path, kind)`. The rename would then hit
    ///   `UNIQUE (note_path, kind, chunk_index, model_version)` (and the
    ///   `embedding_examined` primary key). For each colliding
    ///   `(note_path, kind)` the whole set with the newer `MAX(produced_at)`
    ///   wins (ties keep the backend-free set) and the other set is deleted;
    ///   `embedding_examined` keeps the newer `examined_at` the same way. The
    ///   next open converges after any such race.
    /// - **One transaction.** `BEGIN IMMEDIATE` → dedup → rename → `COMMIT`;
    ///   any error rolls the whole thing back.
    #[cfg(any(feature = "vec-candle", feature = "vec-onnx"))]
    pub(crate) fn migrate_backend_free_model_ids(&self) -> Result<bool> {
        if !self.legacy_model_ids_present()? {
            return Ok(false);
        }
        log::info!("search::migrate: renaming backend-suffixed embedding model ids to backend-free ids");

        self.conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| -> Result<()> {
            for (legacy, current) in LEGACY_MODEL_IDS {
                self.rename_model_id(legacy, current)?;
            }
            Ok(())
        })();

        match result {
            Ok(()) => {
                self.conn.execute_batch("COMMIT")?;
                Ok(true)
            }
            Err(e) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                Err(e)
            }
        }
    }

    /// Whether any legacy id remains in the pin or either embedding table.
    #[cfg(any(feature = "vec-candle", feature = "vec-onnx"))]
    fn legacy_model_ids_present(&self) -> Result<bool> {
        for (legacy, _) in LEGACY_MODEL_IDS {
            let present: bool = self.conn.query_row(
                "SELECT EXISTS (SELECT 1 FROM embedding_config WHERE key = 'active_model' AND value = ?1)
                     OR EXISTS (SELECT 1 FROM note_embeddings WHERE model_version = ?1)
                     OR EXISTS (SELECT 1 FROM embedding_examined WHERE model_version = ?1)",
                rusqlite::params![legacy],
                |row| row.get(0),
            )?;
            if present {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Dedup then rename one `legacy` id to `current`. Runs inside the
    /// caller's transaction.
    #[cfg(any(feature = "vec-candle", feature = "vec-onnx"))]
    fn rename_model_id(&self, legacy: &str, current: &str) -> Result<()> {
        let params = rusqlite::params![legacy, current];

        // A (note_path, kind) whose legacy set is strictly newer drops its
        // backend-free set. After this, every (note_path, kind) still holding
        // both ids has the backend-free set newest (or tied).
        let embeddings_superseded = self.conn.execute(
            "DELETE FROM note_embeddings
              WHERE model_version = ?2
                AND (SELECT MAX(l.produced_at) FROM note_embeddings l
                      WHERE l.model_version = ?1
                        AND l.note_path = note_embeddings.note_path
                        AND l.kind = note_embeddings.kind)
                  > (SELECT MAX(c.produced_at) FROM note_embeddings c
                      WHERE c.model_version = ?2
                        AND c.note_path = note_embeddings.note_path
                        AND c.kind = note_embeddings.kind)",
            params,
        )?;
        let embeddings_dropped = self.conn.execute(
            "DELETE FROM note_embeddings
              WHERE model_version = ?1
                AND EXISTS (SELECT 1 FROM note_embeddings c
                             WHERE c.model_version = ?2
                               AND c.note_path = note_embeddings.note_path
                               AND c.kind = note_embeddings.kind)",
            params,
        )?;
        let embeddings_renamed = self.conn.execute(
            "UPDATE note_embeddings SET model_version = ?2 WHERE model_version = ?1",
            params,
        )?;

        let examined_superseded = self.conn.execute(
            "DELETE FROM embedding_examined
              WHERE model_version = ?2
                AND EXISTS (SELECT 1 FROM embedding_examined l
                             WHERE l.model_version = ?1
                               AND l.note_path = embedding_examined.note_path
                               AND l.kind = embedding_examined.kind
                               AND l.examined_at > embedding_examined.examined_at)",
            params,
        )?;
        let examined_dropped = self.conn.execute(
            "DELETE FROM embedding_examined
              WHERE model_version = ?1
                AND EXISTS (SELECT 1 FROM embedding_examined c
                             WHERE c.model_version = ?2
                               AND c.note_path = embedding_examined.note_path
                               AND c.kind = embedding_examined.kind)",
            params,
        )?;
        let examined_renamed = self.conn.execute(
            "UPDATE embedding_examined SET model_version = ?2 WHERE model_version = ?1",
            params,
        )?;

        let pin_renamed = self.conn.execute(
            "UPDATE embedding_config SET value = ?2 WHERE key = 'active_model' AND value = ?1",
            params,
        )?;

        log::info!(
            "search::migrate: {legacy} -> {current}: note_embeddings renamed={embeddings_renamed} \
             dropped-older={} embedding_examined renamed={examined_renamed} dropped-older={} pin={pin_renamed}",
            embeddings_superseded + embeddings_dropped,
            examined_superseded + examined_dropped,
        );
        Ok(())
    }

    /// Add cortex governance columns if they don't exist yet (handles schema migration)
    fn ensure_governance_columns(&self) -> Result<()> {
        let mut stmt = self.conn.prepare("PRAGMA table_info(notes)")?;
        let existing_columns: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(1))?
            .filter_map(warn_row)
            .collect();

        let governance_columns = [
            ("quality", "TEXT DEFAULT ''"),
            ("classified", "INTEGER DEFAULT 0"),
            ("classified_by", "TEXT DEFAULT ''"),
            ("confidence", "TEXT DEFAULT ''"),
            ("needs_review", "INTEGER DEFAULT 0"),
            ("duplicate_group", "TEXT DEFAULT ''"),
        ];

        for (col, col_type) in governance_columns {
            if !existing_columns.contains(&col.to_string()) {
                self.conn
                    .execute_batch(&format!("ALTER TABLE notes ADD COLUMN {col} {col_type};"))?;
            }
        }

        Ok(())
    }

    /// Add distilled-contract columns to existing DBs. New columns are the
    /// `claims` FTS-indexed text, the per-kind `cortex_*` metadata, and the
    /// signal columns Doc 3 will own.
    fn ensure_distilled_columns(&self) -> Result<()> {
        let mut stmt = self.conn.prepare("PRAGMA table_info(notes)")?;
        let existing_columns: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(1))?
            .filter_map(warn_row)
            .collect();

        let distilled_columns = [
            ("claims", "TEXT DEFAULT ''"),
            // The operator's capture annotation, populated by the
            // indexer from the `capture-note:` frontmatter, spliced into the
            // summary embedding text.
            ("capture_note", "TEXT DEFAULT ''"),
            ("cortex_repo_stars", "INTEGER"),
            ("cortex_repo_last_commit", "TEXT"),
            ("cortex_repo_primary_language", "TEXT"),
            ("cortex_video_duration_seconds", "INTEGER"),
            ("cortex_video_channel", "TEXT"),
            ("cortex_video_published_at", "TEXT"),
            ("cortex_thread_platform", "TEXT"),
            ("cortex_thread_post_count", "INTEGER"),
            ("cortex_thread_author", "TEXT"),
            ("search_hit_count", "INTEGER DEFAULT 0"),
            ("last_accessed_at", "INTEGER"),
            ("inbound_link_count", "INTEGER DEFAULT 0"),
            ("pinned", "INTEGER DEFAULT 0"),
        ];

        for (col, col_type) in distilled_columns {
            if !existing_columns.contains(&col.to_string()) {
                self.conn
                    .execute_batch(&format!("ALTER TABLE notes ADD COLUMN {col} {col_type};"))?;
            }
        }

        // `modified_at` is still read by other code paths; ensure its index
        // exists on already-deployed DBs whose CREATE TABLE predates the
        // index addition.
        self.conn
            .execute_batch("CREATE INDEX IF NOT EXISTS idx_notes_modified_at ON notes(modified_at);")?;

        Ok(())
    }

    /// Add the borg staged-source columns to existing DBs. These carry the
    /// frontmatter join keys (`trace`, `ingested`, `trace-expires`) so oracle
    /// can advertise that a verbatim staged source still exists.
    ///
    /// Matches the established `ensure_distilled_columns` pattern verbatim:
    /// PRAGMA `table_info` probe then individual `ALTER TABLE ADD COLUMN`, with
    /// NO wrapping transaction and NO `set_version`. A single idempotent
    /// `ALTER ADD COLUMN` cannot half-apply, so the Rust DDL-transaction rule
    /// does not bite, and there is no version infra in `vault/src/search/` to
    /// hang a version on.
    fn ensure_trace_columns(&self) -> Result<()> {
        let mut stmt = self.conn.prepare("PRAGMA table_info(notes)")?;
        let existing_columns: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(1))?
            .filter_map(warn_row)
            .collect();

        let trace_columns = [
            ("trace", "TEXT DEFAULT ''"),
            ("ingested", "TEXT DEFAULT ''"),
            ("trace_expires", "TEXT DEFAULT ''"),
        ];

        for (col, col_type) in trace_columns {
            if !existing_columns.contains(&col.to_string()) {
                self.conn
                    .execute_batch(&format!("ALTER TABLE notes ADD COLUMN {col} {col_type};"))?;
            }
        }

        Ok(())
    }

    /// Add the `repo` column to existing DBs (harvest-clyde-sessions design):
    /// the note's canonical `<org>/<repo>` anchor, feeding the repo
    /// hub edge. Same idempotent `PRAGMA table_info` + single
    /// `ALTER ADD COLUMN` pattern as `ensure_trace_columns`.
    fn ensure_repo_columns(&self) -> Result<()> {
        let mut stmt = self.conn.prepare("PRAGMA table_info(notes)")?;
        let existing_columns: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(1))?
            .filter_map(warn_row)
            .collect();

        if !existing_columns.contains(&"repo".to_string()) {
            self.conn
                .execute_batch("ALTER TABLE notes ADD COLUMN repo TEXT DEFAULT '';")?;
        }

        Ok(())
    }

    /// Add the `channel_handle` column to existing DBs (YouTube channel search
    /// design): the `channel-handle:` frontmatter value, `''` when absent.
    /// Same idempotent `PRAGMA table_info` + single `ALTER ADD COLUMN` pattern
    /// as `ensure_repos_touched_column`; existing rows read `''` until the
    /// next reindex of each note picks the frontmatter up.
    fn ensure_channel_handle_column(&self) -> Result<()> {
        let mut stmt = self.conn.prepare("PRAGMA table_info(notes)")?;
        let existing_columns: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(1))?
            .filter_map(warn_row)
            .collect();

        if !existing_columns.contains(&"channel_handle".to_string()) {
            self.conn
                .execute_batch("ALTER TABLE notes ADD COLUMN channel_handle TEXT DEFAULT '';")?;
        }

        Ok(())
    }

    /// Add the `repos_touched` column to existing DBs (harvest-completion design):
    /// every repo a session touched, feeding the multi-repo-member hub
    /// edge. Same idempotent `PRAGMA table_info` + single `ALTER ADD COLUMN`
    /// pattern as `ensure_repo_columns`.
    ///
    /// Deliberately NULLABLE with NO `DEFAULT`, unlike sibling `repo` (which
    /// defaults `''`): the frontmatter field is THREE-STATE and the distinction
    /// is load-bearing (`vault::frontmatter::Frontmatter::repos_touched`). A
    /// pre-existing row (or an omitted key) reads back as SQL `NULL` == `None`
    /// == "touched set unknowable", never `'[]'` == `Some(vec![])` ==
    /// "definitively touched nothing". A `DEFAULT ''`/`'[]'` would collapse the
    /// unknowable state into the empty state and lose the distinction. Stored
    /// form: `NULL` for `None`, a JSON array (including `[]`) for `Some(..)`.
    ///
    /// No `user_version`/`schema_version` bump: this crate's `notes`-table
    /// migrations carry no version infra (see `ensure_trace_columns`), and a
    /// single idempotent `ALTER ADD COLUMN` cannot half-apply, so the Rust
    /// DDL-transaction rule does not bite.
    fn ensure_repos_touched_column(&self) -> Result<()> {
        let mut stmt = self.conn.prepare("PRAGMA table_info(notes)")?;
        let existing_columns: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(1))?
            .filter_map(warn_row)
            .collect();

        if !existing_columns.contains(&"repos_touched".to_string()) {
            self.conn
                .execute_batch("ALTER TABLE notes ADD COLUMN repos_touched TEXT;")?;
        }

        Ok(())
    }

    /// Add the `superseded_by` column to existing DBs (cortex association-sweep
    /// design: the survivor-stem a soft-retired tombstone points at).
    /// A non-empty value marks the row as a merge tombstone, which
    /// [`stale_embedding_targets`](super::SearchIndex::stale_embedding_targets)
    /// excludes from every embedding kind so a tombstone never lands an
    /// embedding row (the exclusion is a required part of the feature, not an
    /// afterthought). Same idempotent `PRAGMA table_info` + single
    /// `ALTER ADD COLUMN` pattern as `ensure_repo_columns`.
    ///
    /// `DEFAULT ''` (not NULL) so a non-tombstone row reads back as the empty
    /// string, matching what `index_one` writes for every ordinary note; the
    /// exclusion predicate then treats both `''` and legacy `NULL` as
    /// "not superseded".
    fn ensure_superseded_by_column(&self) -> Result<()> {
        let mut stmt = self.conn.prepare("PRAGMA table_info(notes)")?;
        let existing_columns: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(1))?
            .filter_map(warn_row)
            .collect();

        if !existing_columns.contains(&"superseded_by".to_string()) {
            self.conn
                .execute_batch("ALTER TABLE notes ADD COLUMN superseded_by TEXT DEFAULT '';")?;
        }

        Ok(())
    }

    /// Ensure the FTS5 virtual table includes the `claims` column and that the
    /// trio of triggers populating it from `notes` is in place. Rebuilds the
    /// FTS5 table when the existing schema lacks `claims`.
    /// NEVER `VACUUM` this database. The FTS5 table is `content=notes,
    /// content_rowid=rowid`: it links to the `notes` content table by the
    /// implicit SQLite `rowid`. `VACUUM` may RENUMBER rowids, silently
    /// dissociating every FTS row from its note (search returns wrong/zero
    /// results with no error). If reclaiming space ever becomes necessary,
    /// rebuild the index from the vault (`index_vault`) instead of VACUUMing,
    /// or migrate the schema to an explicit stable key first.
    fn ensure_fts5_schema(&self) -> Result<()> {
        if !self.fts_has_claims_column()? {
            // Migration path. Triggers attach to the `notes` content table -
            // dropping the FTS table does NOT cascade to them, and a later
            // CREATE TRIGGER would fail with "already exists." Drop them
            // explicitly before recreating.
            self.conn.execute_batch(
                "DROP TRIGGER IF EXISTS notes_ai;
                 DROP TRIGGER IF EXISTS notes_ad;
                 DROP TRIGGER IF EXISTS notes_au;
                 DROP TABLE IF EXISTS notes_fts;
                 CREATE VIRTUAL TABLE notes_fts USING fts5(
                     title, body, tags, summary, claims,
                     content=notes, content_rowid=rowid
                 );
                 INSERT INTO notes_fts(notes_fts) VALUES('rebuild');",
            )?;
        }
        self.create_fts_triggers()?;
        Ok(())
    }

    pub(crate) fn fts_has_claims_column(&self) -> Result<bool> {
        let exists: bool = self
            .conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name='notes_fts'",
                [],
                |_| Ok::<_, rusqlite::Error>(()),
            )
            .is_ok();
        if !exists {
            return Ok(false);
        }

        let mut stmt = self.conn.prepare("PRAGMA table_info(notes_fts)")?;
        let columns: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(1))?
            .filter_map(warn_row)
            .collect();
        Ok(columns.iter().any(|c| c == "claims"))
    }

    fn create_fts_triggers(&self) -> Result<()> {
        self.conn.execute_batch(
            "CREATE TRIGGER IF NOT EXISTS notes_ai AFTER INSERT ON notes BEGIN
                INSERT INTO notes_fts(rowid, title, body, tags, summary, claims)
                VALUES (new.rowid, new.title, new.body, new.tags, new.summary, new.claims);
            END;

            CREATE TRIGGER IF NOT EXISTS notes_ad AFTER DELETE ON notes BEGIN
                INSERT INTO notes_fts(notes_fts, rowid, title, body, tags, summary, claims)
                VALUES ('delete', old.rowid, old.title, old.body, old.tags, old.summary, old.claims);
            END;

            CREATE TRIGGER IF NOT EXISTS notes_au AFTER UPDATE ON notes BEGIN
                INSERT INTO notes_fts(notes_fts, rowid, title, body, tags, summary, claims)
                VALUES ('delete', old.rowid, old.title, old.body, old.tags, old.summary, old.claims);
                INSERT INTO notes_fts(rowid, title, body, tags, summary, claims)
                VALUES (new.rowid, new.title, new.body, new.tags, new.summary, new.claims);
            END;",
        )?;
        Ok(())
    }
}

#[cfg(all(test, feature = "vec"))]
mod tests;
