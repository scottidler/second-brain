use super::*;

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
                trace_expires TEXT DEFAULT ''
            );

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

        // FTS5 cannot ALTER, so we detect old (no-claims) schemas and rebuild.
        // Triggers attach to `notes`, not `notes_fts`, so they must be dropped
        // explicitly before the FTS table; CREATE TRIGGER would otherwise fail
        // with "trigger already exists" on the recreate path.
        self.ensure_fts5_schema()?;

        #[cfg(feature = "vec")]
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
    #[cfg(feature = "vec")]
    fn ensure_vec_schema(&self) -> Result<()> {
        // Fresh DBs get the widened CHECK immediately (`'claim'` included,
        // Phase 9). Existing DBs created before Phase 9 keep the old
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
                FOREIGN KEY (note_path) REFERENCES notes(path) ON DELETE CASCADE,
                UNIQUE (note_path, kind, chunk_index, model_version)
            );",
        )?;

        // SQLite cannot ALTER a CHECK constraint, so a pre-Phase-9 DB needs a
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

            -- Phase 3 (docs/design/2026-07-05-cortex-daemon-oscillation-loop.md):
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
        self.conn.execute(
            "INSERT OR IGNORE INTO embedding_config (key, value) VALUES ('active_model', ?1)",
            rusqlite::params![crate::embedding::ACTIVE_MODEL_VERSION],
        )?;
        Ok(())
    }

    /// Widen the `note_embeddings.kind` CHECK constraint to include
    /// `'claim'` (Phase 9 of
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
    #[cfg(feature = "vec")]
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
            // Phase 9: the operator's capture annotation, populated by the
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

    /// Add the `repo` column to existing DBs (harvest-clyde-sessions design,
    /// Phase 9: the note's canonical `<org>/<repo>` anchor, feeding the repo
    /// hub edge in Phase 10). Same idempotent `PRAGMA table_info` + single
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

    /// Add the `repos_touched` column to existing DBs (harvest-completion design,
    /// Phase 4: every repo a session touched, feeding the multi-repo-member hub
    /// edge). Same idempotent `PRAGMA table_info` + single `ALTER ADD COLUMN`
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
