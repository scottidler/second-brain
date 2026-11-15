//! The embedding revision marker: the hash function, every writer, the
//! pre-marker schema migration, the NULL-is-an-error rule, and the graph freshness
//! cases `produced_at` could not see.

use super::vector_hash;

#[test]
fn vector_hash_is_lowercase_hex_sha256_of_the_exact_bytes() {
    assert_eq!(
        vector_hash(b""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        vector_hash(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_ne!(
        vector_hash(&[0, 0, 0, 0]),
        vector_hash(&[0, 0, 0, 1]),
        "one bit moves it"
    );
}

#[cfg(any(feature = "vec-candle", feature = "vec-onnx"))]
mod vec {
    use std::path::Path;

    use rusqlite::{Connection, params};

    use super::vector_hash;
    use crate::search::{BatchUpsert, EmbeddingKind, SearchIndex};

    const MODEL: &str = "revision-model";
    const DIM: usize = 4;

    fn index_with_notes(path: Option<&Path>, notes: &[&str]) -> SearchIndex {
        let mut index = match path {
            Some(path) => SearchIndex::open(path).expect("open"),
            None => SearchIndex::open_memory().expect("open"),
        };
        index.set_active_embedding(MODEL, DIM).expect("pin");
        for note in notes {
            index.insert_test_note_row(note, "article", 100).expect("note");
        }
        index
    }

    fn embed(index: &SearchIndex, path: &str, v: [f32; DIM]) {
        index
            .upsert_embedding(path, EmbeddingKind::Summary, 0, "s", &v, MODEL, 100)
            .expect("embed");
    }

    fn swap(index: &mut SearchIndex, path: &str, v: [f32; DIM]) {
        index
            .swap_embeddings_batch(&[BatchUpsert {
                note_path: path,
                kind: EmbeddingKind::Summary,
                chunk_index: 0,
                text: "s",
                embedding: &v,
                model_version: MODEL,
                source_modified_at: 100,
            }])
            .expect("swap");
    }

    /// `(note_path, kind, chunk_index, embedding, vector_hash)` for every row.
    type HashRow = (String, String, i64, Vec<u8>, Option<String>);

    fn hash_rows(conn: &Connection) -> Vec<HashRow> {
        conn.prepare(
            "SELECT note_path, kind, chunk_index, embedding, vector_hash
               FROM note_embeddings ORDER BY note_path, kind, chunk_index",
        )
        .expect("prepare")
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))
        .expect("query")
        .collect::<rusqlite::Result<_>>()
        .expect("rows")
    }

    fn assert_every_row_hashes_its_blob(conn: &Connection) {
        let rows = hash_rows(conn);
        assert!(!rows.is_empty());
        for (path, kind, chunk, blob, hash) in rows {
            assert_eq!(
                hash.as_deref(),
                Some(vector_hash(&blob).as_str()),
                "{path} {kind} {chunk}"
            );
        }
    }

    fn columns(conn: &Connection, table: &str) -> Vec<String> {
        conn.prepare(&format!("PRAGMA table_info({table})"))
            .expect("prepare")
            .query_map([], |r| r.get::<_, String>(1))
            .expect("query")
            .collect::<rusqlite::Result<_>>()
            .expect("cols")
    }

    #[test]
    fn every_note_embeddings_writer_stores_the_hash_of_its_blob() {
        let mut index = index_with_notes(None, &["a.md", "b.md", "c.md", "d.md"]);
        embed(&index, "a.md", [1.0, 0.0, 0.0, 0.0]);
        index
            .upsert_embeddings_batch(&[BatchUpsert {
                note_path: "b.md",
                kind: EmbeddingKind::Summary,
                chunk_index: 0,
                text: "s",
                embedding: &[0.0, 1.0, 0.0, 0.0],
                model_version: MODEL,
                source_modified_at: 100,
            }])
            .expect("batch");
        swap(&mut index, "c.md", [0.0, 0.0, 1.0, 0.0]);
        index
            .swap_kind_chunks(
                "d.md",
                EmbeddingKind::TranscriptChunk,
                &[
                    ("one".to_string(), vec![0.5, 0.5, 0.5, 0.5]),
                    ("two".to_string(), vec![0.0, 0.0, 0.0, 1.0]),
                ],
                MODEL,
                100,
            )
            .expect("chunks");
        assert_eq!(hash_rows(&index.conn).len(), 5);
        assert_every_row_hashes_its_blob(&index.conn);
    }

    /// The pre-marker `edge_build_state`, verbatim from the schema it replaces.
    const OLD_EDGE_BUILD_STATE: &str = "
        CREATE TABLE edge_build_state (
            note_path        TEXT PRIMARY KEY,
            content_built_at INTEGER NOT NULL DEFAULT 0,
            semantic_built_at INTEGER NOT NULL DEFAULT 0,
            FOREIGN KEY (note_path) REFERENCES notes(path) ON DELETE CASCADE
        );";

    /// Take a current index file back to the shape before the revision marker: no
    /// `vector_hash` column, no partial index, and `edge_build_state` keyed on
    /// `semantic_built_at`. `old_state` is `(note_path, semantic_built_at)`.
    fn downgrade_to_pre_marker(path: &Path, produced_at: &[(&str, i64)], old_state: &[(&str, i64)]) {
        let conn = Connection::open(path).expect("open");
        conn.execute_batch(
            "DROP INDEX idx_note_embeddings_unhashed;
             ALTER TABLE note_embeddings DROP COLUMN vector_hash;
             DROP TABLE edge_build_state;",
        )
        .expect("drop the revision-marker shape");
        conn.execute_batch(OLD_EDGE_BUILD_STATE).expect("old edge state");
        for (note, at) in produced_at {
            conn.execute(
                "UPDATE note_embeddings SET produced_at = ?2 WHERE note_path = ?1",
                params![note, at],
            )
            .expect("produced_at");
        }
        for (note, built) in old_state {
            conn.execute(
                "INSERT INTO edge_build_state (note_path, content_built_at, semantic_built_at)
                 VALUES (?1, 100, ?2)",
                params![note, built],
            )
            .expect("old state");
        }
        assert!(!columns(&conn, "note_embeddings").contains(&"vector_hash".to_string()));
    }

    /// Everything the migration could touch, for the idempotency check.
    /// `(note_path, content_built_at, semantic_vector_hash)`.
    type EdgeStateRow = (String, i64, Option<String>);

    fn snapshot(conn: &Connection) -> (Vec<HashRow>, Vec<EdgeStateRow>, Vec<String>) {
        let state = conn
            .prepare(
                "SELECT note_path, content_built_at, semantic_vector_hash
                   FROM edge_build_state ORDER BY note_path",
            )
            .expect("prepare")
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .expect("query")
            .collect::<rusqlite::Result<_>>()
            .expect("state");
        let schema = conn
            .prepare("SELECT COALESCE(sql, name) FROM sqlite_master ORDER BY type, name")
            .expect("prepare")
            .query_map([], |r| r.get(0))
            .expect("query")
            .collect::<rusqlite::Result<_>>()
            .expect("schema");
        (hash_rows(conn), state, schema)
    }

    #[test]
    fn migration_hashes_every_row_carries_fresh_notes_over_and_drops_semantic_built_at() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("index.db");
        let mut index = index_with_notes(Some(&db), &["fresh.md", "stale.md", "never.md", "bare.md"]);
        embed(&index, "fresh.md", [1.0, 0.0, 0.0, 0.0]);
        embed(&index, "stale.md", [0.0, 1.0, 0.0, 0.0]);
        embed(&index, "never.md", [0.0, 0.0, 1.0, 0.0]);
        index
            .swap_kind_chunks(
                "fresh.md",
                EmbeddingKind::TranscriptChunk,
                &[("chunk".to_string(), vec![0.5, 0.5, 0.5, 0.5])],
                MODEL,
                100,
            )
            .expect("chunk");
        drop(index);
        // fresh: built at its produced_at (the old rule's `>` says unchanged);
        // stale: embedded after its build; never: no state row; bare: a state
        // row and no summary at all.
        downgrade_to_pre_marker(
            &db,
            &[("fresh.md", 1_000), ("stale.md", 2_000), ("never.md", 1_000)],
            &[("fresh.md", 1_000), ("stale.md", 1_500), ("bare.md", 1_500)],
        );

        let index = SearchIndex::open(&db).expect("migrating open");
        assert_every_row_hashes_its_blob(&index.conn);
        let edge_columns = columns(&index.conn, "edge_build_state");
        assert!(
            !edge_columns.contains(&"semantic_built_at".to_string()),
            "{edge_columns:?}"
        );
        assert!(edge_columns.contains(&"semantic_vector_hash".to_string()));

        assert_eq!(
            index.semantic_edge_targets().expect("targets"),
            ["never.md", "stale.md"],
            "fresh under the old rule is not a target; stale and never-built are"
        );
        let fresh_hash = index.note_summary_vector_hash("fresh.md").expect("hash");
        let (_, state, _) = snapshot(&index.conn);
        assert_eq!(
            state,
            [
                ("bare.md".to_string(), 100, None),
                ("fresh.md".to_string(), 100, fresh_hash),
                ("stale.md".to_string(), 100, None),
            ]
        );

        let before = snapshot(&index.conn);
        assert_eq!(
            index.migrate_vector_hash().expect("again"),
            Default::default(),
            "a second run has nothing to do"
        );
        drop(index);
        let index = SearchIndex::open(&db).expect("reopen");
        assert_eq!(snapshot(&index.conn), before, "ensure_schema again changes nothing");
    }

    #[test]
    fn a_row_an_older_binary_writes_after_the_migration_is_hashed_on_the_next_open() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("index.db");
        let index = index_with_notes(Some(&db), &["a.md"]);
        drop(index);
        Connection::open(&db)
            .expect("open")
            .execute(
                "INSERT INTO note_embeddings
                    (note_path, kind, chunk_index, text, embedding, dim, model_version, produced_at, source_modified_at)
                 VALUES ('a.md', 'summary', 0, 's', ?1, 1, ?2, 1, 1)",
                params![&[1u8, 2, 3, 4][..], MODEL],
            )
            .expect("an old writer leaves vector_hash NULL");
        let index = SearchIndex::open(&db).expect("reopen");
        assert_every_row_hashes_its_blob(&index.conn);
    }

    #[test]
    fn a_null_vector_hash_reached_by_a_reader_is_an_error() {
        let index = index_with_notes(None, &["a.md"]);
        embed(&index, "a.md", [1.0, 0.0, 0.0, 0.0]);
        index
            .conn
            .execute("UPDATE note_embeddings SET vector_hash = NULL", [])
            .expect("null it");

        for err in [
            index.semantic_edge_targets().expect_err("targets").to_string(),
            index.note_summary_vector_hash("a.md").expect_err("hash").to_string(),
            index.unassigned(&[], 10).expect_err("unassigned").to_string(),
            index.in_scope_summaries(&[]).expect_err("in scope").to_string(),
        ] {
            assert!(err.contains("a.md") && err.contains("vector_hash"), "{err}");
        }
    }

    /// A note's semantic edges are recorded as built from its current summary.
    fn record_built(index: &SearchIndex, path: &str) {
        let hash = index.note_summary_vector_hash(path).expect("hash");
        index.record_edge_build(path, 100, hash.as_deref()).expect("record");
    }

    fn pin_produced_at(index: &SearchIndex, path: &str, at: i64) {
        index
            .conn
            .execute(
                "UPDATE note_embeddings SET produced_at = ?2 WHERE note_path = ?1",
                params![path, at],
            )
            .expect("pin produced_at");
    }

    /// Two re-embeds inside one second: `produced_at` is whole seconds, so the
    /// old `produced_at > semantic_built_at` rule saw the second one as
    /// unchanged. The note also holds the max rowid, which the swap gives back.
    #[test]
    fn a_second_re_embed_inside_one_second_makes_the_note_a_semantic_target_again() {
        let mut index = index_with_notes(None, &["other.md", "a.md"]);
        swap(&mut index, "other.md", [0.0, 1.0, 0.0, 0.0]);
        swap(&mut index, "a.md", [1.0, 0.0, 0.0, 0.0]);
        pin_produced_at(&index, "a.md", 5_000);
        record_built(&index, "other.md");
        record_built(&index, "a.md");
        assert!(index.semantic_edge_targets().expect("targets").is_empty());
        let max_id = index.test_embedding_ids().expect("ids").last().cloned();

        swap(&mut index, "a.md", [0.0, 0.0, 1.0, 0.0]);
        pin_produced_at(&index, "a.md", 5_000);
        assert_eq!(
            index.semantic_edge_targets().expect("targets"),
            ["a.md"],
            "first re-embed"
        );
        record_built(&index, "a.md");
        assert!(index.semantic_edge_targets().expect("targets").is_empty());

        swap(&mut index, "a.md", [0.0, 0.0, 0.0, 1.0]);
        pin_produced_at(&index, "a.md", 5_000);
        assert_eq!(
            index.test_embedding_ids().expect("ids").last().cloned(),
            max_id,
            "same id and same produced_at second: neither old marker moved"
        );
        assert_eq!(
            index.semantic_edge_targets().expect("targets"),
            ["a.md"],
            "the second re-embed in the same second is a target again"
        );
    }

    #[test]
    fn a_note_built_while_unembedded_becomes_a_target_when_its_summary_lands() {
        let mut index = index_with_notes(None, &["a.md"]);
        record_built(&index, "a.md");
        assert_eq!(index.note_summary_vector_hash("a.md").expect("hash"), None);
        assert!(index.semantic_edge_targets().expect("targets").is_empty());
        swap(&mut index, "a.md", [1.0, 0.0, 0.0, 0.0]);
        assert_eq!(index.semantic_edge_targets().expect("targets"), ["a.md"]);
    }
}
