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
                pinned INTEGER DEFAULT 0
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
        // Two-step setup: the static DDL runs as a batch, the
        // backend-dependent seed for `active_model` runs separately so
        // the model_version string stays a Rust const (driven by the
        // active embedding-backend feature) rather than baked into a SQL
        // string literal.
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS note_embeddings (
                id INTEGER PRIMARY KEY,
                note_path TEXT NOT NULL,
                kind TEXT NOT NULL CHECK (kind IN ('summary', 'transcript-chunk')),
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

            CREATE INDEX IF NOT EXISTS idx_note_embeddings_path
                ON note_embeddings(note_path);
            CREATE INDEX IF NOT EXISTS idx_note_embeddings_stale
                ON note_embeddings(source_modified_at);
            CREATE INDEX IF NOT EXISTS idx_note_embeddings_kind_model
                ON note_embeddings(kind, model_version);

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
