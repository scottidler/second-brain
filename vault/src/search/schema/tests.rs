//! Migration tests. Two migrations live here: the backend-free model-id
//! rename (`migrate_backend_free_model_ids`, at the end of this file) and,
//! first, the `note_embeddings` table rebuild that widens
//! the `kind` CHECK constraint to permit `'claim'`. The migration is the
//! high-risk change in the claim-kind work (a deployed DB corruption is High impact),
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

/// The older `note_embeddings` schema: the CHECK permits only
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

/// The full claim-kind migration contract in one test: build a DB that predates it
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
            "INSERT INTO notes (path, title, note_type, origin, status, date, tags, source, creator, body, summary, modified_at)
             VALUES ('notes/z.md', 'T', 'article', 'assisted', '', '2026-07-05', '[]', '', '', 'b', 's', 100)",
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

/// (docs/design/2026-07-05-cortex-daemon-oscillation-loop.md): the
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

/// (harvest-completion): the `repos_touched` column migration. A DB
/// whose `notes` table predates the column gains it via `ensure_schema` without
/// losing rows, existing rows read back as SQL NULL (== `None` == "touched set
/// unknowable", NOT `'[]'`), and re-ensuring is a no-op. Mirrors the crate's
/// established no-`user_version` idempotent `ALTER ADD COLUMN` discipline.
#[test]
fn repos_touched_column_migration_adds_column_preserves_rows_and_is_idempotent() {
    let conn = Connection::open_in_memory().expect("open");
    // A older `notes` table: no `repos_touched` column, one existing row.
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

// --- backend-free model ids -------------------------------------------------

const LEGACY_SMALL: &str = "bge-small-en-v1.5-candle";
const SMALL: &str = "bge-small-en-v1.5";
const LEGACY_BASE: &str = "bge-base-en-v1.5-candle";
const BASE: &str = "bge-base-en-v1.5";

/// One `note_embeddings` row with an explicit `produced_at`, so the dedup's
/// newer-wins rule is under the test's control.
fn insert_embedding(
    index: &SearchIndex,
    note_path: &str,
    kind: &str,
    chunk_index: i64,
    text: &str,
    model_version: &str,
    produced_at: i64,
) {
    index
        .conn
        .execute(
            "INSERT INTO note_embeddings
                (note_path, kind, chunk_index, text, embedding, dim, model_version, produced_at, source_modified_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6, ?7, 100)",
            params![
                note_path,
                kind,
                chunk_index,
                text,
                vec![0u8; 4],
                model_version,
                produced_at
            ],
        )
        .expect("insert embedding");
}

fn insert_examined(index: &SearchIndex, note_path: &str, kind: &str, model_version: &str, examined_at: i64) {
    index
        .conn
        .execute(
            "INSERT INTO embedding_examined (note_path, kind, model_version, examined_at) VALUES (?1, ?2, ?3, ?4)",
            params![note_path, kind, model_version, examined_at],
        )
        .expect("insert examined");
}

/// A note with a non-empty summary and claims, last modified at 100 (the
/// `source_modified_at` every seeded embedding carries, so a row under the
/// active id is current, not stale).
fn insert_embeddable_note(index: &SearchIndex, path: &str, note_type: &str) {
    index
        .conn
        .execute(
            "INSERT INTO notes (path, title, note_type, summary, claims, modified_at) VALUES (?1, 'T', ?2, 'a summary', 'a claim', 100)",
            params![path, note_type],
        )
        .expect("insert note");
}

fn set_pin(index: &SearchIndex, model_version: &str) {
    index
        .conn
        .execute(
            "UPDATE embedding_config SET value = ?1 WHERE key = 'active_model'",
            params![model_version],
        )
        .expect("set pin");
}

fn pin(index: &SearchIndex) -> String {
    index.active_embedding_model().expect("active_model")
}

/// `(note_path, kind, model_version, examined_at)`, ordered.
fn dump_examined(index: &SearchIndex) -> Vec<(String, String, String, i64)> {
    let mut stmt = index
        .conn
        .prepare("SELECT note_path, kind, model_version, examined_at FROM embedding_examined ORDER BY 1, 2, 3")
        .expect("prepare examined");
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .expect("query examined")
        .map(|r| r.expect("examined row"))
        .collect()
}

fn model_versions_in_use(index: &SearchIndex) -> Vec<String> {
    let mut stmt = index
        .conn
        .prepare("SELECT DISTINCT model_version FROM note_embeddings ORDER BY 1")
        .expect("prepare versions");
    stmt.query_map([], |r| r.get::<_, String>(0))
        .expect("query versions")
        .map(|r| r.expect("version"))
        .collect()
}

/// A DB written by a pre-rename binary: `-candle` pin, `-candle` rows for
/// every embedding kind, and `-candle` examined sentinels. Reopening renames
/// all of it in place: the backend-free pin, the same row count, every row
/// current (no stale target for any kind), so nothing re-embeds.
#[test]
fn backend_free_migration_renames_the_pin_and_rows_without_a_reembed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("oracle.db");
    {
        let index = SearchIndex::open(&db).expect("seed open");
        set_pin(&index, LEGACY_SMALL);
        for (i, path) in ["notes/a.md", "notes/b.md", "notes/c.md"].iter().enumerate() {
            insert_embeddable_note(&index, path, "youtube");
            insert_embedding(&index, path, "summary", 0, "s", LEGACY_SMALL, 10 + i as i64);
            insert_embedding(&index, path, "claim", 0, "c", LEGACY_SMALL, 10);
            insert_embedding(&index, path, "transcript-chunk", 0, "t0", LEGACY_SMALL, 10);
            insert_embedding(&index, path, "transcript-chunk", 1, "t1", LEGACY_SMALL, 10);
        }
        // An examined sentinel for a note with no transcript rows of its own.
        insert_embeddable_note(&index, "notes/silent.md", "youtube");
        insert_embedding(&index, "notes/silent.md", "summary", 0, "s", LEGACY_SMALL, 10);
        insert_embedding(&index, "notes/silent.md", "claim", 0, "c", LEGACY_SMALL, 10);
        insert_examined(&index, "notes/silent.md", "transcript-chunk", LEGACY_SMALL, 100);
        // The A/B candidate's legacy id migrates too.
        insert_embedding(&index, "notes/a.md", "summary", 0, "s", LEGACY_BASE, 10);
    }
    // Read the seed through a raw connection: a `SearchIndex::open` here
    // would already run the migration under test.
    let seeded: i64 = Connection::open(&db)
        .expect("raw open")
        .query_row("SELECT COUNT(*) FROM note_embeddings", [], |r| r.get(0))
        .expect("count");
    assert_eq!(seeded, 15, "seeded row count");

    let index = SearchIndex::open(&db).expect("migrating open");

    assert_eq!(pin(&index), SMALL, "the pin is backend-free");
    let rows = dump_rows(&index);
    assert_eq!(rows.len(), 15, "a rename, not a delete or a re-embed");
    assert_eq!(model_versions_in_use(&index), vec![BASE.to_string(), SMALL.to_string()]);
    assert_eq!(
        dump_examined(&index),
        vec![(
            "notes/silent.md".to_string(),
            "transcript-chunk".to_string(),
            SMALL.to_string(),
            100
        )]
    );
    for kind in [
        EmbeddingKind::Summary,
        EmbeddingKind::TranscriptChunk,
        EmbeddingKind::Claim,
    ] {
        let stale = index.stale_embedding_targets(kind, SMALL, 100).expect("stale targets");
        assert!(
            stale.is_empty(),
            "{kind:?}: every note is current under the backend-free id, got {:?}",
            stale.iter().map(|t| &t.note_path).collect::<Vec<_>>()
        );
    }
}

/// The deploy race: an old cortex wrote `-candle` rows and re-pinned
/// `-candle` after a new binary had already renamed the DB. Both ids then
/// hold the same chunks, which would abort a bare rename on the UNIQUE
/// constraint. The open succeeds, and per `(note_path, kind)` only the set
/// with the newer `produced_at` survives, renamed.
#[test]
fn backend_free_migration_keeps_the_newer_row_when_both_ids_exist() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("oracle.db");
    {
        let index = SearchIndex::open(&db).expect("seed open");
        insert_embeddable_note(&index, "notes/old-wins.md", "youtube");
        insert_embeddable_note(&index, "notes/new-wins.md", "youtube");
        insert_embeddable_note(&index, "notes/tie.md", "youtube");
        insert_embeddable_note(&index, "notes/chunks.md", "youtube");

        // The `-candle` row is newer: it wins and is renamed.
        insert_embedding(&index, "notes/old-wins.md", "summary", 0, "free-older", SMALL, 200);
        insert_embedding(
            &index,
            "notes/old-wins.md",
            "summary",
            0,
            "legacy-newer",
            LEGACY_SMALL,
            300,
        );
        // The backend-free row is newer: it wins, the `-candle` row goes.
        insert_embedding(&index, "notes/new-wins.md", "summary", 0, "free-newer", SMALL, 300);
        insert_embedding(
            &index,
            "notes/new-wins.md",
            "summary",
            0,
            "legacy-older",
            LEGACY_SMALL,
            200,
        );
        // A tie keeps the backend-free row.
        insert_embedding(&index, "notes/tie.md", "summary", 0, "free-tie", SMALL, 250);
        insert_embedding(&index, "notes/tie.md", "summary", 0, "legacy-tie", LEGACY_SMALL, 250);
        // Multi-chunk: the newer `-candle` set has 2 chunks, the older
        // backend-free set 3. The newer set wins whole; the older set's
        // chunk 2 does not survive beside it.
        for i in 0..3 {
            insert_embedding(
                &index,
                "notes/chunks.md",
                "transcript-chunk",
                i,
                "free-old-chunk",
                SMALL,
                200,
            );
        }
        for i in 0..2 {
            insert_embedding(
                &index,
                "notes/chunks.md",
                "transcript-chunk",
                i,
                "legacy-new-chunk",
                LEGACY_SMALL,
                300,
            );
        }
        // Examined sentinels: the newer `examined_at` wins either way.
        insert_examined(&index, "notes/old-wins.md", "transcript-chunk", SMALL, 100);
        insert_examined(&index, "notes/old-wins.md", "transcript-chunk", LEGACY_SMALL, 150);
        insert_examined(&index, "notes/new-wins.md", "transcript-chunk", SMALL, 150);
        insert_examined(&index, "notes/new-wins.md", "transcript-chunk", LEGACY_SMALL, 100);
        // The old cortex re-pins its id on every tick.
        set_pin(&index, LEGACY_SMALL);
    }

    let index = SearchIndex::open(&db).expect("the open succeeds despite the collisions");

    assert_eq!(pin(&index), SMALL);
    assert_eq!(model_versions_in_use(&index), vec![SMALL.to_string()]);
    let survivors: Vec<(String, String, i64, String, i64)> = dump_rows(&index)
        .into_iter()
        .map(|r| (r.note_path, r.kind, r.chunk_index, r.text, r.produced_at))
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    let expect = |p: &str, k: &str, i: i64, t: &str, at: i64| (p.to_string(), k.to_string(), i, t.to_string(), at);
    assert_eq!(
        survivors,
        vec![
            expect("notes/chunks.md", "transcript-chunk", 0, "legacy-new-chunk", 300),
            expect("notes/chunks.md", "transcript-chunk", 1, "legacy-new-chunk", 300),
            expect("notes/new-wins.md", "summary", 0, "free-newer", 300),
            expect("notes/old-wins.md", "summary", 0, "legacy-newer", 300),
            expect("notes/tie.md", "summary", 0, "free-tie", 250),
        ]
    );
    assert_eq!(
        dump_examined(&index),
        vec![
            (
                "notes/new-wins.md".to_string(),
                "transcript-chunk".to_string(),
                SMALL.to_string(),
                150
            ),
            (
                "notes/old-wins.md".to_string(),
                "transcript-chunk".to_string(),
                SMALL.to_string(),
                150
            ),
        ]
    );
}

/// A second open of a migrated DB changes nothing: same rows byte for byte,
/// same sentinels, same pin, and the migration reports no work.
#[test]
fn backend_free_migration_is_idempotent() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("oracle.db");
    {
        let index = SearchIndex::open(&db).expect("seed open");
        set_pin(&index, LEGACY_SMALL);
        insert_embeddable_note(&index, "notes/a.md", "youtube");
        insert_embedding(&index, "notes/a.md", "summary", 0, "s", LEGACY_SMALL, 10);
        insert_embedding(&index, "notes/a.md", "summary", 0, "s-free", SMALL, 20);
        insert_examined(&index, "notes/a.md", "transcript-chunk", LEGACY_SMALL, 100);
    }
    let (rows, examined, first_pin) = {
        let index = SearchIndex::open(&db).expect("first open migrates");
        (dump_rows(&index), dump_examined(&index), pin(&index))
    };
    assert_eq!(first_pin, SMALL);
    assert_eq!(rows.len(), 1);

    let index = SearchIndex::open(&db).expect("second open");
    assert_eq!(dump_rows(&index), rows, "rows unchanged byte for byte");
    assert_eq!(dump_examined(&index), examined);
    assert_eq!(pin(&index), first_pin);
    assert!(
        !index.migrate_backend_free_model_ids().expect("third run"),
        "a migrated DB has nothing to rename"
    );
}

/// A fresh DB is seeded with the backend-free pin directly, and the
/// migration finds nothing to do.
#[test]
fn backend_free_migration_is_a_no_op_on_a_fresh_db() {
    let index = SearchIndex::open_memory().expect("open");
    assert_eq!(pin(&index), SMALL);
    assert!(!index.migrate_backend_free_model_ids().expect("run"));
}

/// Ids this migration does not own are left alone: `-fastembed` rows were
/// never written by a shipped binary and stay stale for the re-embed path,
/// and an operator's custom pin is untouched.
#[test]
fn backend_free_migration_leaves_unowned_ids_alone() {
    let index = SearchIndex::open_memory().expect("open");
    insert_embeddable_note(&index, "notes/a.md", "youtube");
    insert_embedding(
        &index,
        "notes/a.md",
        "summary",
        0,
        "s",
        "bge-small-en-v1.5-fastembed",
        10,
    );
    set_pin(&index, "mock-model");
    assert!(!index.migrate_backend_free_model_ids().expect("run"));
    assert_eq!(
        model_versions_in_use(&index),
        vec!["bge-small-en-v1.5-fastembed".to_string()]
    );
    assert_eq!(pin(&index), "mock-model");
}

fn table_has_column(index: &SearchIndex, table: &str, col: &str) -> bool {
    let mut stmt = index
        .conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .expect("table_info");
    stmt.query_map([], |r| r.get::<_, String>(1))
        .expect("query")
        .map(|r| r.expect("col"))
        .any(|c| c == col)
}

/// Recipe versioning (`2026-10-29-retrieval-quality-from-rag-scour`):
/// a DB that predates both the claim kind and the recipe column gains
/// `recipe` on both tables, every existing row and sentinel reads recipe 1,
/// the stale-join index exists, and a second run changes nothing.
#[test]
fn recipe_migration_adds_columns_reading_one_and_is_idempotent() {
    let conn = Connection::open_in_memory().expect("open");
    conn.execute_batch(OLD_SCHEMA).expect("old schema");
    conn.execute_batch(
        "CREATE TABLE embedding_examined (
            note_path TEXT NOT NULL, kind TEXT NOT NULL, model_version TEXT NOT NULL,
            examined_at INTEGER NOT NULL, PRIMARY KEY (note_path, kind, model_version));",
    )
    .expect("old examined");
    insert_old_row(&conn, "notes/a.md", "summary", 0, "alpha", &[1u8, 2, 3, 4]);
    insert_old_row(&conn, "notes/c.md", "transcript-chunk", 0, "chunk", &[5u8, 6, 7, 8]);
    let index = SearchIndex { conn };
    insert_examined(&index, "notes/b.md", "transcript-chunk", "mock-model", 100);

    index.migrate_note_embeddings_add_claim_kind().expect("claim migrate");
    assert!(!table_has_column(&index, "note_embeddings", "recipe"));
    let before = dump_rows(&index);

    index.ensure_recipe_columns().expect("recipe migrate");
    for table in ["note_embeddings", "embedding_examined"] {
        assert!(table_has_column(&index, table, "recipe"), "{table}.recipe added");
        let recipes: Vec<i64> = {
            let mut stmt = index
                .conn
                .prepare(&format!("SELECT recipe FROM {table}"))
                .expect("prepare");
            stmt.query_map([], |r| r.get(0))
                .expect("query")
                .map(|r| r.expect("recipe"))
                .collect()
        };
        assert!(
            !recipes.is_empty() && recipes.iter().all(|&r| r == 1),
            "{table}: {recipes:?}"
        );
    }
    assert_eq!(before, dump_rows(&index), "existing rows otherwise untouched");
    assert!(index_names(&index).contains(&"idx_note_embeddings_note_kind_model".to_string()));

    index.ensure_recipe_columns().expect("second run");
    assert_eq!(before, dump_rows(&index), "idempotent");
}
