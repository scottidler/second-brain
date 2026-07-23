//! Phase 9 migration tests: the `note_embeddings` table rebuild that widens
//! the `kind` CHECK constraint to permit `'claim'`. The migration is the
//! high-risk change in the phase (a deployed DB corruption is High impact),
//! so these are hard gates: row-preservation is asserted byte-identical,
//! idempotency is asserted, and the widened CHECK is proven by a `'claim'`
//! insert succeeding after (and failing before) the rebuild.

use crate::search::{EmbeddingKind, SearchIndex};
use rusqlite::{Connection, params};

/// A full snapshot of one `note_embeddings` row, including the embedding
/// BLOB, so equality is a byte-for-byte comparison (the row-preservation
/// gate). `PartialEq`-derived so a `Vec<Row>` compares element-wise.
#[derive(Debug, Clone, PartialEq)]
struct Row {
    id: i64,
    note_path: String,
    kind: String,
    chunk_index: i64,
    text: String,
    embedding: Vec<u8>,
    dim: i64,
    model_version: String,
    produced_at: i64,
    source_modified_at: i64,
}

/// The pre-Phase-9 `note_embeddings` schema: the CHECK permits only
/// `'summary'` and `'transcript-chunk'`. Deliberately hand-written (not the
/// current `ensure_vec_schema`) so this test pins the exact legacy shape the
/// migration must upgrade. A minimal `notes` parent table is created because
/// the rebuilt table carries the `FOREIGN KEY ... REFERENCES notes(path)` and
/// SQLite reparses that reference on the final `ALTER TABLE ... RENAME`.
const OLD_SCHEMA: &str = "
    CREATE TABLE notes (path TEXT PRIMARY KEY);
    INSERT INTO notes (path) VALUES ('notes/a.md'), ('notes/b.md'), ('notes/c.md');
    CREATE TABLE note_embeddings (
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
        UNIQUE (note_path, kind, chunk_index, model_version)
    );
    CREATE INDEX idx_note_embeddings_path ON note_embeddings(note_path);
    CREATE INDEX idx_note_embeddings_stale ON note_embeddings(source_modified_at);
    CREATE INDEX idx_note_embeddings_kind_model ON note_embeddings(kind, model_version);
    CREATE TABLE embedding_config (key TEXT PRIMARY KEY, value TEXT NOT NULL);
";

fn insert_old_row(conn: &Connection, note_path: &str, kind: &str, chunk_index: i64, text: &str, blob: &[u8]) {
    conn.execute(
        "INSERT INTO note_embeddings
            (note_path, kind, chunk_index, text, embedding, dim, model_version, produced_at, source_modified_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            note_path,
            kind,
            chunk_index,
            text,
            blob,
            blob.len() as i64,
            "mock-model",
            10i64,
            100i64
        ],
    )
    .expect("insert old row");
}

fn dump_rows(index: &SearchIndex) -> Vec<Row> {
    let mut stmt = index
        .conn
        .prepare(
            "SELECT id, note_path, kind, chunk_index, text, embedding, dim, model_version, produced_at, source_modified_at
             FROM note_embeddings ORDER BY id",
        )
        .expect("prepare dump");
    stmt.query_map([], |r| {
        Ok(Row {
            id: r.get(0)?,
            note_path: r.get(1)?,
            kind: r.get(2)?,
            chunk_index: r.get(3)?,
            text: r.get(4)?,
            embedding: r.get(5)?,
            dim: r.get(6)?,
            model_version: r.get(7)?,
            produced_at: r.get(8)?,
            source_modified_at: r.get(9)?,
        })
    })
    .expect("query dump")
    .map(|r| r.expect("row"))
    .collect()
}

fn index_names(index: &SearchIndex) -> Vec<String> {
    let mut stmt = index
        .conn
        .prepare("SELECT name FROM sqlite_master WHERE type='index' AND tbl_name='note_embeddings' AND name NOT LIKE 'sqlite_%' ORDER BY name")
        .expect("prepare index names");
    stmt.query_map([], |r| r.get::<_, String>(0))
        .expect("query index names")
        .map(|r| r.expect("index name"))
        .collect()
}

/// The full Phase-9 migration contract in one test: build a pre-Phase-9 DB
/// with existing summary + transcript rows, then assert every panel
/// condition.
#[test]
fn migration_preserves_rows_widens_check_and_is_idempotent() {
    let conn = Connection::open_in_memory().expect("open");
    conn.execute_batch(OLD_SCHEMA).expect("old schema");
    conn.execute(
        "INSERT INTO embedding_config (key, value) VALUES ('active_model', 'mock-model')",
        [],
    )
    .expect("seed model");
    conn.execute(
        "INSERT INTO embedding_config (key, value) VALUES ('active_dim', '4')",
        [],
    )
    .expect("seed dim");

    // Seed existing rows of both legacy kinds with distinct blobs so a
    // byte-level comparison is meaningful.
    insert_old_row(&conn, "notes/a.md", "summary", 0, "alpha summary", &[1u8, 2, 3, 4]);
    insert_old_row(&conn, "notes/b.md", "summary", 0, "beta summary", &[5u8, 6, 7, 8]);
    insert_old_row(
        &conn,
        "notes/c.md",
        "transcript-chunk",
        0,
        "chunk zero",
        &[9u8, 10, 11, 12],
    );
    insert_old_row(
        &conn,
        "notes/c.md",
        "transcript-chunk",
        1,
        "chunk one",
        &[13u8, 14, 15, 16],
    );

    let index = SearchIndex { conn };

    // (pre) The old CHECK must REJECT a 'claim' insert. A rejected insert
    // leaves the table unchanged, so the `before` snapshot taken next is
    // still the pristine legacy set.
    let claim_before = index.conn.execute(
        "INSERT INTO note_embeddings
            (note_path, kind, chunk_index, text, embedding, dim, model_version, produced_at, source_modified_at)
         VALUES ('notes/a.md', 'claim', 0, 'x', ?1, 4, 'mock-model', 10, 100)",
        params![&[1u8, 2, 3, 4][..]],
    );
    assert!(claim_before.is_err(), "old CHECK must reject a 'claim' insert");

    let before = dump_rows(&index);
    assert_eq!(before.len(), 4, "four seeded rows");

    // (a) Run the migration: it rebuilds.
    let rebuilt = index.migrate_note_embeddings_add_claim_kind().expect("migrate");
    assert!(rebuilt, "first run must rebuild the table");

    // (b) All pre-existing rows survive byte-identical (row-count + full
    // per-row comparison including the embedding BLOB and the id).
    let after = dump_rows(&index);
    assert_eq!(before, after, "existing rows must survive the rebuild byte-identical");

    // (c) The CHECK now permits 'claim'.
    index
        .conn
        .execute(
            "INSERT INTO note_embeddings
                (note_path, kind, chunk_index, text, embedding, dim, model_version, produced_at, source_modified_at)
             VALUES ('notes/a.md', 'claim', 0, 'a claim', ?1, 4, 'mock-model', 10, 100)",
            params![&[1u8, 2, 3, 4][..]],
        )
        .expect("claim insert must succeed after migration");

    // (c') Indexes preserved (recreated inside the migration).
    let idx = index_names(&index);
    assert!(
        idx.contains(&"idx_note_embeddings_path".to_string())
            && idx.contains(&"idx_note_embeddings_stale".to_string())
            && idx.contains(&"idx_note_embeddings_kind_model".to_string()),
        "all three named indexes must survive; got {idx:?}"
    );

    // (c'') embedding_config preserved (a separate table the rebuild never
    // touches).
    let active_model: String = index
        .conn
        .query_row(
            "SELECT value FROM embedding_config WHERE key = 'active_model'",
            [],
            |r| r.get(0),
        )
        .expect("active_model preserved");
    assert_eq!(active_model, "mock-model");

    // (d) Running the migration AGAIN is a no-op (idempotent via the
    // sqlite_master CHECK-literal detection).
    let rebuilt_again = index
        .migrate_note_embeddings_add_claim_kind()
        .expect("migrate idempotent");
    assert!(!rebuilt_again, "second run must be a no-op");
}

/// A DB that is already on the widened schema (the fresh-DB path) must
/// report no rebuild - the idempotency detection reads the stored DDL, not a
/// version counter.
#[test]
fn migration_no_ops_on_fresh_schema() {
    let index = SearchIndex::open_memory().expect("open");
    // open_memory already ran ensure_vec_schema, which writes the widened
    // CHECK on a fresh DB. So the migration should detect 'claim' present.
    let rebuilt = index.migrate_note_embeddings_add_claim_kind().expect("migrate");
    assert!(!rebuilt, "fresh DB is already widened; migration must no-op");

    // And a claim insert works on a fresh DB.
    index
        .conn
        .execute(
            "INSERT INTO notes (path, title, domain, note_type, origin, status, date, tags, source, creator, body, summary, modified_at)
             VALUES ('notes/z.md', 'T', 'tech', 'article', 'assisted', '', '2026-07-05', '[]', '', '', 'b', 's', 100)",
            [],
        )
        .expect("insert note for FK");
    index
        .conn
        .execute(
            "INSERT INTO note_embeddings
                (note_path, kind, chunk_index, text, embedding, dim, model_version, produced_at, source_modified_at)
             VALUES ('notes/z.md', ?2, 0, 'a claim', ?1, 4, 'mock-model', 10, 100)",
            params![&[1u8, 2, 3, 4][..], EmbeddingKind::Claim.as_str()],
        )
        .expect("claim insert on fresh DB");
    assert_eq!(index.count_embeddings(Some(EmbeddingKind::Claim)).expect("count"), 1);
}

/// Phase 3 (docs/design/2026-07-05-cortex-daemon-oscillation-loop.md): the
/// `embedding_examined` sentinel side table is created by `ensure_vec_schema`
/// on a fresh DB, and re-running the idempotent schema-ensure path over an
/// existing DB is a no-op (the `CREATE TABLE IF NOT EXISTS` cannot half-apply,
/// matching the crate's established no-`user_version` migration discipline).
#[test]
fn embedding_examined_table_created_and_schema_ensure_is_idempotent() {
    let index = SearchIndex::open_memory().expect("open");
    let exists: i64 = index
        .conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='embedding_examined'",
            [],
            |r| r.get(0),
        )
        .expect("query sqlite_master");
    assert_eq!(exists, 1, "ensure_vec_schema must create embedding_examined");

    // Re-ensuring the schema over the already-created table must not error.
    index.ensure_schema().expect("re-ensure schema idempotent");
    let still: i64 = index
        .conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='embedding_examined'",
            [],
            |r| r.get(0),
        )
        .expect("query sqlite_master again");
    assert_eq!(still, 1, "re-ensure must leave exactly one embedding_examined table");
}

/// Phase 4 (harvest-completion): the `repos_touched` column migration. A DB
/// whose `notes` table predates the column gains it via `ensure_schema` without
/// losing rows, existing rows read back as SQL NULL (== `None` == "touched set
/// unknowable", NOT `'[]'`), and re-ensuring is a no-op. Mirrors the crate's
/// established no-`user_version` idempotent `ALTER ADD COLUMN` discipline.
#[test]
fn repos_touched_column_migration_adds_column_preserves_rows_and_is_idempotent() {
    let conn = Connection::open_in_memory().expect("open");
    // A pre-Phase-4 `notes` table: no `repos_touched` column, one existing row.
    conn.execute_batch(
        "CREATE TABLE notes (path TEXT PRIMARY KEY, title TEXT, repo TEXT DEFAULT '');
         INSERT INTO notes (path, title, repo) VALUES ('notes/old.md', 'legacy', 'scottidler/loopr');",
    )
    .expect("old notes schema");
    let index = SearchIndex { conn };

    // (pre) The column does not exist yet.
    assert!(
        !notes_has_column(&index, "repos_touched"),
        "pre-migration DB lacks the repos_touched column"
    );

    index.ensure_repos_touched_column().expect("migrate");

    // (post) Column present; the legacy row survived and reads back NULL.
    assert!(
        notes_has_column(&index, "repos_touched"),
        "the migration added the repos_touched column"
    );
    let (title, repos_touched): (String, Option<String>) = index
        .conn
        .query_row(
            "SELECT title, repos_touched FROM notes WHERE path = 'notes/old.md'",
            [],
            |r| Ok((r.get(0)?, r.get::<_, Option<String>>(1)?)),
        )
        .expect("legacy row survived");
    assert_eq!(title, "legacy", "the legacy row is preserved");
    assert_eq!(
        repos_touched, None,
        "a pre-existing row reads back NULL (None == unknowable), never '[]'"
    );

    // Idempotent: a second run over the migrated DB does not error or dup.
    index.ensure_repos_touched_column().expect("re-run is idempotent");
    assert!(notes_has_column(&index, "repos_touched"));
}

/// True when the `notes` table has a column named `col`.
fn notes_has_column(index: &SearchIndex, col: &str) -> bool {
    let mut stmt = index.conn.prepare("PRAGMA table_info(notes)").expect("pragma");
    let cols: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(1))
        .expect("query")
        .map(|r| r.expect("col name"))
        .collect();
    cols.iter().any(|c| c == col)
}
