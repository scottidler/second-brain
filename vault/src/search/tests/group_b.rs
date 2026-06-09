use super::*;

#[test]
fn bump_access_increments_and_stamps_timestamp() {
    let index = SearchIndex::open_memory().expect("open");
    let note = make_test_note("notes/bump.md", "# T\n\n## Summary\n\nBody.\n");
    index.index_one(&note, 100).expect("index_one");

    let before = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("now")
        .as_secs() as i64;

    index.bump_access("notes/bump.md").expect("first bump");
    index.bump_access("notes/bump.md").expect("second bump");

    let (hits, last, _inbound) = signal_row(&index, "notes/bump.md");
    assert_eq!(hits, 2, "two bumps -> count 2");
    let last = last.expect("last_accessed_at set after bump");
    assert!(last >= before, "stamp should be at-or-after before-bump time");
}

#[test]
fn recompute_inbound_link_counts_basic() {
    let mut index = SearchIndex::open_memory().expect("open");
    index
        .index_one(&make_test_note("notes/a.md", "Refer to [[b]]."), 100)
        .expect("a");
    index
        .index_one(&make_test_note("notes/b.md", "## Summary\n\nB.\n"), 100)
        .expect("b");
    index
        .index_one(&make_test_note("notes/c.md", "See [[b]] and [[a]]."), 100)
        .expect("c");

    let changed = index.recompute_inbound_link_counts().expect("recompute");
    assert!(changed >= 2, "at least a and b should have changed: {changed}");

    assert_eq!(signal_row(&index, "notes/a.md").2, 1, "a is linked from c");
    assert_eq!(signal_row(&index, "notes/b.md").2, 2, "b is linked from a and c");
    assert_eq!(signal_row(&index, "notes/c.md").2, 0, "c has no inbound");
}

#[test]
fn recompute_inbound_link_counts_handles_link_removal() {
    let mut index = SearchIndex::open_memory().expect("open");
    index
        .index_one(&make_test_note("notes/a.md", "Link to [[b]]."), 100)
        .expect("a");
    index
        .index_one(&make_test_note("notes/b.md", "## Summary\n\nB.\n"), 100)
        .expect("b");

    index.recompute_inbound_link_counts().expect("first");
    assert_eq!(signal_row(&index, "notes/b.md").2, 1);

    // Edit A to drop the link to B.
    index
        .index_one(&make_test_note("notes/a.md", "No link here."), 200)
        .expect("re-a");
    index.recompute_inbound_link_counts().expect("second");
    assert_eq!(signal_row(&index, "notes/b.md").2, 0, "removed link drops to 0");
}

#[test]
fn recompute_inbound_link_counts_excludes_self_links() {
    let mut index = SearchIndex::open_memory().expect("open");
    index
        .index_one(&make_test_note("notes/a.md", "I reference [[a]] - myself."), 100)
        .expect("a");

    index.recompute_inbound_link_counts().expect("recompute");
    assert_eq!(
        signal_row(&index, "notes/a.md").2,
        0,
        "self-link must not bump the note's own inbound count",
    );
}

#[test]
fn recompute_inbound_link_counts_is_case_insensitive() {
    let mut index = SearchIndex::open_memory().expect("open");
    // Target file uses lowercase-hyphenated stem; source spells it
    // with mixed case in the wikilink. Should still match.
    index
        .index_one(&make_test_note("notes/some-note.md", "## Summary\n\nT.\n"), 100)
        .expect("target");
    index
        .index_one(&make_test_note("notes/a.md", "See [[Some-Note]] for more."), 100)
        .expect("source");

    index.recompute_inbound_link_counts().expect("recompute");
    assert_eq!(
        signal_row(&index, "notes/some-note.md").2,
        1,
        "case-insensitive stem match",
    );
}

#[test]
fn recompute_inbound_link_counts_strips_path_prefix() {
    // `[[folder/note]]` should match a row whose stem is `note`.
    let mut index = SearchIndex::open_memory().expect("open");
    index
        .index_one(&make_test_note("notes/target.md", "## Summary\n\nT.\n"), 100)
        .expect("target");
    index
        .index_one(
            &make_test_note("notes/source.md", "Link [[notes/target]] with path."),
            100,
        )
        .expect("source");

    index.recompute_inbound_link_counts().expect("recompute");
    assert_eq!(
        signal_row(&index, "notes/target.md").2,
        1,
        "[[folder/note]] should match the stem",
    );
}

#[test]
fn index_one_persists_pinned_true() {
    let index = SearchIndex::open_memory().expect("open");
    let note = make_pinned_note("notes/pinned.md", Some(true), "## Summary\n\nP.\n");
    index.index_one(&note, 100).expect("index");
    assert_eq!(pinned_value(&index, "notes/pinned.md"), 1);
}

#[test]
fn index_one_pinned_defaults_to_zero() {
    let index = SearchIndex::open_memory().expect("open");
    let note = make_pinned_note("notes/unpinned.md", None, "## Summary\n\nU.\n");
    index.index_one(&note, 100).expect("index");
    assert_eq!(pinned_value(&index, "notes/unpinned.md"), 0);
}

#[test]
fn index_one_pinned_survives_reindex_without_frontmatter_change() {
    let index = SearchIndex::open_memory().expect("open");
    let note = make_pinned_note("notes/keep.md", Some(true), "## Summary\n\nFirst.\n");
    index.index_one(&note, 100).expect("first");
    assert_eq!(pinned_value(&index, "notes/keep.md"), 1);

    // Reindex with new body but same pinned: true.
    let updated = make_pinned_note("notes/keep.md", Some(true), "## Summary\n\nSecond.\n");
    index.index_one(&updated, 200).expect("reindex");
    assert_eq!(
        pinned_value(&index, "notes/keep.md"),
        1,
        "pinned survives content reindex when frontmatter unchanged",
    );
}

#[test]
fn index_one_pinned_clears_when_frontmatter_drops_field() {
    let index = SearchIndex::open_memory().expect("open");
    let note = make_pinned_note("notes/flip.md", Some(true), "## Summary\n\nP.\n");
    index.index_one(&note, 100).expect("first");
    assert_eq!(pinned_value(&index, "notes/flip.md"), 1);

    // User removes pinned: true from frontmatter; reindex.
    let updated = make_pinned_note("notes/flip.md", None, "## Summary\n\nU.\n");
    index.index_one(&updated, 200).expect("reindex");
    assert_eq!(
        pinned_value(&index, "notes/flip.md"),
        0,
        "removing pinned: true must clear the column on reindex",
    );
}

#[test]
fn cold_notes_excludes_daily_and_journal_notes() {
    let index = SearchIndex::open_memory().expect("open");
    // Old, zero-signal knowledge note in notes/ -> surfaces.
    index
        .index_one(
            &make_dated_note("notes/knowledge.md", "2023-01-01", "## Summary\n\nK.\n"),
            1_000,
        )
        .expect("k");
    // type: daily, even outside journal/ -> excluded by type.
    index
        .index_one(
            &make_typed_dated_note("notes/some-daily.md", "2023-01-01", "daily", "## Summary\n\nD.\n"),
            1_000,
        )
        .expect("d");
    // journal/ subtree -> excluded by path even without an explicit daily type.
    index
        .index_one(
            &make_dated_note("journal/2023/01/2023-01-02.md", "2023-01-01", "## Summary\n\nJ.\n"),
            1_000,
        )
        .expect("j");

    let rows = index.cold_notes(&cold_query("2025-01-01")).expect("cold");
    let paths: Vec<&str> = rows.iter().map(|r| r.path.as_str()).collect();
    assert_eq!(paths, vec!["notes/knowledge.md"]);
}

#[test]
fn cold_notes_returns_only_floor_satisfying_rows() {
    let index = SearchIndex::open_memory().expect("open");
    // Cold candidate: zero signals, old content date, not pinned.
    index
        .index_one(
            &make_dated_note("notes/cold.md", "2023-01-01", "## Summary\n\nC.\n"),
            1_000,
        )
        .expect("cold");
    // Recent content date: shouldn't surface.
    index
        .index_one(
            &make_dated_note("notes/recent.md", "2026-01-01", "## Summary\n\nR.\n"),
            1_000,
        )
        .expect("recent");
    // Pinned: shouldn't surface even though old.
    index
        .index_one(
            &make_dated_pinned_note("notes/pin.md", "2023-01-01", Some(true), "## Summary\n\nP.\n"),
            1_000,
        )
        .expect("pin");
    // Undated: shouldn't surface - age cannot be inferred.
    index
        .index_one(&make_test_note("notes/undated.md", "## Summary\n\nU.\n"), 1_000)
        .expect("undated");
    // Has inbound (seed signal directly to avoid running recompute):
    index
        .index_one(
            &make_dated_note("notes/linked.md", "2023-01-01", "## Summary\n\nL.\n"),
            1_000,
        )
        .expect("linked");
    index
        .conn
        .execute(
            "UPDATE notes SET inbound_link_count = 2 WHERE path = 'notes/linked.md'",
            [],
        )
        .expect("seed inbound");

    let rows = index.cold_notes(&cold_query("2024-01-01")).expect("cold");
    let paths: Vec<&str> = rows.iter().map(|r| r.path.as_str()).collect();
    assert_eq!(paths, vec!["notes/cold.md"]);
}

#[test]
fn cold_notes_excludes_undated_rows() {
    let index = SearchIndex::open_memory().expect("open");
    // No `date:` frontmatter at all - normalizes to '' in the column.
    index
        .index_one(&make_test_note("notes/undated.md", "## Summary\n\nU.\n"), 1_000)
        .expect("undated");

    let rows = index.cold_notes(&cold_query("2024-01-01")).expect("cold");
    assert!(rows.is_empty(), "undated note must not surface: got {rows:?}");
}

#[test]
fn cold_notes_orders_oldest_first() {
    let index = SearchIndex::open_memory().expect("open");
    index
        .index_one(
            &make_dated_note("notes/middle.md", "2023-06-01", "## Summary\n\nM.\n"),
            1_000,
        )
        .expect("m");
    index
        .index_one(
            &make_dated_note("notes/oldest.md", "2023-01-01", "## Summary\n\nO.\n"),
            1_000,
        )
        .expect("o");
    index
        .index_one(
            &make_dated_note("notes/newer.md", "2023-12-01", "## Summary\n\nN.\n"),
            1_000,
        )
        .expect("n");

    let rows = index.cold_notes(&cold_query("2024-01-01")).expect("cold");
    let paths: Vec<&str> = rows.iter().map(|r| r.path.as_str()).collect();
    assert_eq!(paths, vec!["notes/oldest.md", "notes/middle.md", "notes/newer.md"]);
}

#[test]
fn cold_notes_excludes_once_read_notes() {
    let index = SearchIndex::open_memory().expect("open");
    index
        .index_one(
            &make_dated_note("notes/once.md", "2023-01-01", "## Summary\n\nO.\n"),
            1_000,
        )
        .expect("once");
    // One bump suffices to mark the note permanently warm under the
    // binary decay rule.
    index.bump_access("notes/once.md").expect("bump");

    let rows = index.cold_notes(&cold_query("2024-01-01")).expect("cold");
    assert!(rows.is_empty(), "any prior read disqualifies: got {rows:?}");
}

#[test]
fn count_pinned_excluded_counts_only_pinned_floor_satisfiers() {
    let index = SearchIndex::open_memory().expect("open");
    index
        .index_one(
            &make_dated_pinned_note("notes/p.md", "2023-01-01", Some(true), "## Summary\n\nP.\n"),
            1_000,
        )
        .expect("pinned");
    // Recent pinned: would not have qualified for the cold report
    // because its content date is not old enough, so should NOT count
    // toward pinned_excluded.
    index
        .index_one(
            &make_dated_pinned_note("notes/p-recent.md", "2026-01-01", Some(true), "## Summary\n\nN.\n"),
            1_000,
        )
        .expect("pinned-recent");
    index
        .index_one(
            &make_dated_note("notes/not-pinned.md", "2023-01-01", "## Summary\n\nU.\n"),
            1_000,
        )
        .expect("unpinned");

    let count = index.count_pinned_excluded("2024-01-01").expect("count");
    assert_eq!(count, 1, "only old, otherwise-cold, pinned rows count");
}

#[test]
fn normalize_date_canonical_iso_passes_through() {
    assert_eq!(normalize_date("2023-01-13"), "2023-01-13");
}

#[test]
fn normalize_date_keeps_date_from_iso_with_time_suffix() {
    assert_eq!(normalize_date("2023-01-13T09:30:00Z"), "2023-01-13");
}

#[test]
fn normalize_date_rejects_debug_string_and_non_iso() {
    // The `"Number(2023)"` garbage path, slash format, a Templater literal,
    // and an empty string all normalize to '' (undated).
    assert_eq!(normalize_date("Number(2023)"), "");
    assert_eq!(normalize_date("05/12/2026"), "");
    assert_eq!(normalize_date("{{date}}"), "");
    assert_eq!(normalize_date("2023"), "");
    assert_eq!(normalize_date(""), "");
}

#[test]
fn index_one_writes_empty_date_for_non_iso_frontmatter() {
    let index = SearchIndex::open_memory().expect("open");
    index
        .index_one(
            &make_dated_note("notes/slash.md", "05/12/2026", "## Summary\n\nS.\n"),
            1_000,
        )
        .expect("index");
    let stored: String = index
        .conn
        .query_row("SELECT date FROM notes WHERE path = 'notes/slash.md'", [], |row| {
            row.get(0)
        })
        .expect("read date");
    assert_eq!(
        stored, "",
        "non-ISO date must land as '' so it is excluded, not mis-aged"
    );
}

#[test]
fn bump_access_is_noop_on_missing_path() {
    // The note may have been deleted between knowledge_search and note_read;
    // the bump is best-effort and must not error.
    let index = SearchIndex::open_memory().expect("open");
    let result = index.bump_access("nonexistent/path.md");
    assert!(result.is_ok(), "missing path is silent: {result:?}");
}

#[test]
fn index_one_persists_distilled_metadata_from_frontmatter() {
    use crate::frontmatter::Frontmatter;
    use std::path::PathBuf;
    let mut extra = HashMap::new();
    extra.insert("cortex-repo-stars".to_string(), serde_yaml::Value::Number(1432.into()));
    extra.insert(
        "cortex-repo-primary-language".to_string(),
        serde_yaml::Value::String("Rust".to_string()),
    );
    let fm = Frontmatter {
        title: Some("Repo Note".to_string()),
        note_type: Some("article".to_string()),
        origin: Some("assisted".to_string()),
        extra,
        ..Frontmatter::default()
    };

    let note = Note {
        path: PathBuf::from("notes/repo.md"),
        frontmatter: fm,
        body: "## Summary\n\nA repo.\n\n## Claims\n- It builds.\n".to_string(),
        raw: String::new(),
    };

    let index = SearchIndex::open_memory().expect("open");
    index.index_one(&note, 100).expect("index_one");

    let (stars, lang): (Option<i64>, Option<String>) = index
        .conn
        .query_row(
            "SELECT cortex_repo_stars, cortex_repo_primary_language
                 FROM notes WHERE path = 'notes/repo.md'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("row");
    assert_eq!(stars, Some(1432));
    assert_eq!(lang.as_deref(), Some("Rust"));

    // Claims column should hold the flattened claim text.
    let claims_flat: String = index
        .conn
        .query_row("SELECT claims FROM notes WHERE path = 'notes/repo.md'", [], |row| {
            row.get(0)
        })
        .expect("claims");
    assert_eq!(claims_flat, "It builds.");
}

#[test]
fn fts5_schema_migrates_from_old_schema() {
    // Build a connection at the OLD schema, then run ensure_schema and
    // verify the claims column is in place and FTS5 search over claims works.
    let conn = Connection::open_in_memory().expect("open");
    conn.execute_batch(
            "CREATE TABLE notes (
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
                modified_at INTEGER
            );
            CREATE VIRTUAL TABLE notes_fts USING fts5(
                title, body, tags, summary,
                content=notes, content_rowid=rowid
            );
            CREATE TRIGGER notes_ai AFTER INSERT ON notes BEGIN
                INSERT INTO notes_fts(rowid, title, body, tags, summary)
                VALUES (new.rowid, new.title, new.body, new.tags, new.summary);
            END;
            INSERT INTO notes (path, title, domain, note_type, origin, status, date, tags, source, creator, body, summary, modified_at)
            VALUES ('notes/legacy.md', 'Legacy', 'tech', 'article', 'assisted', '', '2026-03-21', '[]', '', '', 'legacy body', 'legacy summary', 0);",
        )
        .expect("seed old schema");

    let index = SearchIndex { conn };
    index.ensure_schema().expect("migrate");

    // The new columns must exist on notes.
    let mut stmt = index.conn.prepare("PRAGMA table_info(notes)").expect("table_info");
    let cols: Vec<String> = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .expect("query")
        .filter_map(|r| r.ok())
        .collect();
    assert!(cols.iter().any(|c| c == "claims"), "claims col missing");
    assert!(
        cols.iter().any(|c| c == "cortex_repo_stars"),
        "cortex_repo_stars col missing"
    );
    assert!(
        cols.iter().any(|c| c == "search_hit_count"),
        "search_hit_count col missing"
    );

    // FTS5 must carry claims now.
    assert!(index.fts_has_claims_column().expect("fts cols"));

    // Existing data should survive the migration.
    let title: String = index
        .conn
        .query_row("SELECT title FROM notes WHERE path = 'notes/legacy.md'", [], |row| {
            row.get(0)
        })
        .expect("legacy row");
    assert_eq!(title, "Legacy");

    // FTS5 was rebuilt: the legacy body should still be searchable.
    let hits = index.search("legacy", None, None, None, None).expect("search");
    assert!(
        hits.iter().any(|n| n.path == "notes/legacy.md"),
        "post-migration FTS5 must still surface legacy rows: got {hits:?}"
    );
}

#[test]
fn fts5_search_hits_claims_column() {
    let index = SearchIndex::open_memory().expect("open");
    let note = make_test_note(
        "notes/distinctclaim.md",
        "# T\n\n## Summary\n\nSome summary.\n\n## Claims\n- xenomorphism is the unique signal.\n",
    );
    index.index_one(&note, 100).expect("index_one");

    // FTS5 query for the claim-only term should find this note.
    let hits = index.search("xenomorphism", None, None, None, None).expect("search");
    assert!(
        hits.iter().any(|n| n.path == "notes/distinctclaim.md"),
        "expected FTS5 to index claims column; got {hits:?}"
    );
}

#[cfg(feature = "vec")]
#[test]
fn vec_schema_creates_note_embeddings_table() {
    let index = SearchIndex::open_memory().expect("open");
    let count: i64 = index
        .conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master \
                 WHERE type = 'table' AND name = 'note_embeddings'",
            [],
            |row| row.get(0),
        )
        .expect("query master");
    assert_eq!(count, 1, "note_embeddings table should be created");

    let mut stmt = index
        .conn
        .prepare("PRAGMA table_info(note_embeddings)")
        .expect("table_info");
    let cols: Vec<String> = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .expect("query")
        .filter_map(|r| r.ok())
        .collect();
    for expected in [
        "id",
        "note_path",
        "kind",
        "chunk_index",
        "text",
        "embedding",
        "dim",
        "model_version",
        "produced_at",
        "source_modified_at",
    ] {
        assert!(
            cols.iter().any(|c| c == expected),
            "note_embeddings missing column {expected}; got {cols:?}"
        );
    }
}

#[cfg(feature = "vec")]
#[test]
fn vec_schema_seeds_active_model_config() {
    let index = SearchIndex::open_memory().expect("open");
    let model: String = index
        .conn
        .query_row(
            "SELECT value FROM embedding_config WHERE key = 'active_model'",
            [],
            |row| row.get(0),
        )
        .expect("active_model row");
    assert_eq!(model, "bge-small-en-v1.5-candle");

    let dim: String = index
        .conn
        .query_row(
            "SELECT value FROM embedding_config WHERE key = 'active_dim'",
            [],
            |row| row.get(0),
        )
        .expect("active_dim row");
    assert_eq!(dim, "384");
}

#[cfg(feature = "vec")]
#[test]
fn vec_schema_is_idempotent() {
    // Two consecutive ensure_schema calls must not error and must not
    // double-insert the embedding_config seed rows.
    let index = SearchIndex::open_memory().expect("open");
    index.ensure_schema().expect("re-ensure");
    let count: i64 = index
        .conn
        .query_row(
            "SELECT COUNT(*) FROM embedding_config WHERE key = 'active_model'",
            [],
            |row| row.get(0),
        )
        .expect("count");
    assert_eq!(count, 1, "active_model must remain a single row across re-ensure");
}

#[cfg(feature = "vec")]
#[test]
fn vec_schema_migrates_old_db_without_note_embeddings() {
    // Build an old DB with notes only (no note_embeddings) and confirm a
    // fresh open creates the new tables idempotently and preserves the
    // existing notes row.
    let conn = Connection::open_in_memory().expect("open");
    conn.execute_batch("PRAGMA foreign_keys=ON;").expect("fk on");
    conn.execute_batch(
            "CREATE TABLE notes (
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
                modified_at INTEGER
            );
            INSERT INTO notes (path, title, domain, note_type, origin, status, date, tags, source, creator, body, summary, modified_at)
            VALUES ('notes/old.md', 'Old', 'tech', 'article', 'assisted', '', '2026-03-21', '[]', '', '', 'old body', 'old summary', 0);",
        )
        .expect("seed old schema");

    let index = SearchIndex { conn };
    index.ensure_schema().expect("migrate");

    // note_embeddings table was created
    let count: i64 = index
        .conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master \
                 WHERE type = 'table' AND name = 'note_embeddings'",
            [],
            |row| row.get(0),
        )
        .expect("query master");
    assert_eq!(count, 1);

    // Pre-existing notes row preserved
    let title: String = index
        .conn
        .query_row("SELECT title FROM notes WHERE path = 'notes/old.md'", [], |row| {
            row.get(0)
        })
        .expect("legacy row");
    assert_eq!(title, "Old");
}

#[cfg(feature = "vec")]
#[test]
fn vec_schema_fk_cascade_deletes_embeddings_with_note() {
    // Insert a note + embedding row, delete the note, confirm the
    // embedding row vanishes via the native FK CASCADE. No trigger.
    let index = SearchIndex::open_memory().expect("open");
    index
            .conn
            .execute(
                "INSERT INTO notes (path, title, domain, note_type, origin, status, date, tags, source, creator, body, summary, modified_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                params![
                    "notes/cascade.md",
                    "T", "tech", "article", "assisted", "", "2026-03-21",
                    "[]", "", "", "body", "summary", 0_i64,
                ],
            )
            .expect("insert note");
    let bytes = encode_le_f32(&[0.1_f32, 0.2, 0.3, 0.4]);
    index
        .conn
        .execute(
            "INSERT INTO note_embeddings (
                    note_path, kind, chunk_index, text, embedding, dim,
                    model_version, produced_at, source_modified_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                "notes/cascade.md",
                "summary",
                0_i64,
                "summary text",
                bytes,
                4_i64,
                "test-model",
                0_i64,
                0_i64,
            ],
        )
        .expect("insert embedding");

    let before: i64 = index
        .conn
        .query_row(
            "SELECT COUNT(*) FROM note_embeddings WHERE note_path = ?1",
            params!["notes/cascade.md"],
            |row| row.get(0),
        )
        .expect("count before");
    assert_eq!(before, 1);

    index
        .conn
        .execute("DELETE FROM notes WHERE path = ?1", params!["notes/cascade.md"])
        .expect("delete note");

    let after: i64 = index
        .conn
        .query_row(
            "SELECT COUNT(*) FROM note_embeddings WHERE note_path = ?1",
            params!["notes/cascade.md"],
            |row| row.get(0),
        )
        .expect("count after");
    assert_eq!(after, 0, "FK CASCADE must remove embeddings when note is deleted");
}

#[cfg(feature = "vec")]
#[test]
fn vec_schema_fk_pragma_must_be_on_for_cascade() {
    // Defensive: if a future change disables PRAGMA foreign_keys=ON, the
    // FK CASCADE silently no-ops and orphans accumulate. Mimic the broken
    // case here and assert the orphan-detection signal so the regression
    // is loud.
    let conn = Connection::open_in_memory().expect("open");
    conn.execute_batch("PRAGMA foreign_keys=OFF;").expect("fk off");
    let index = SearchIndex { conn };
    index.ensure_schema().expect("schema");

    index
            .conn
            .execute(
                "INSERT INTO notes (path, title, domain, note_type, origin, status, date, tags, source, creator, body, summary, modified_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                params![
                    "notes/orphan.md",
                    "T", "tech", "article", "assisted", "", "2026-03-21",
                    "[]", "", "", "body", "summary", 0_i64,
                ],
            )
            .expect("insert note");
    let bytes = encode_le_f32(&[1.0_f32, 0.0]);
    index
        .conn
        .execute(
            "INSERT INTO note_embeddings (
                    note_path, kind, chunk_index, text, embedding, dim,
                    model_version, produced_at, source_modified_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                "notes/orphan.md",
                "summary",
                0_i64,
                "t",
                bytes,
                2_i64,
                "m",
                0_i64,
                0_i64,
            ],
        )
        .expect("insert embedding");

    index
        .conn
        .execute("DELETE FROM notes WHERE path = ?1", params!["notes/orphan.md"])
        .expect("delete");

    let orphan_count: i64 = index
        .conn
        .query_row(
            "SELECT COUNT(*) FROM note_embeddings WHERE note_path = ?1",
            params!["notes/orphan.md"],
            |row| row.get(0),
        )
        .expect("orphan count");
    // With FK enforcement OFF this must produce an orphan. If a future
    // refactor accidentally re-enables FK enforcement at the connection
    // level (or moves CASCADE into a trigger), this assertion fails
    // loudly and the maintainer is forced to re-think the regression.
    assert_eq!(
        orphan_count, 1,
        "with foreign_keys=OFF an orphan row must remain; \
             if FK enforcement is bolted on somewhere else, this test is the canary"
    );
}

#[cfg(feature = "vec")]
#[test]
fn validate_le_f32_len_rejects_mismatched_length() {
    // Length not divisible by 4 -> error.
    let bytes = vec![0u8; 7];
    let err = validate_le_f32_len(&bytes, 4).expect_err("expected error");
    let msg = format!("{err}");
    assert!(msg.contains("length mismatch"), "got: {msg}");

    // Length divisible by 4 but != dim*4 -> error.
    let bytes = vec![0u8; 12]; // 3 floats
    let err = validate_le_f32_len(&bytes, 4).expect_err("expected error");
    let msg = format!("{err}");
    assert!(msg.contains("length mismatch"), "got: {msg}");
}

#[cfg(feature = "vec")]
#[test]
fn validate_le_f32_len_accepts_exact_length() {
    let v = [1.5_f32, -0.25, 0.0, 7.5];
    let bytes = encode_le_f32(&v);
    validate_le_f32_len(&bytes, v.len()).expect("valid length");
}

#[cfg(feature = "vec")]
#[test]
fn vec_schema_kind_check_constraint_rejects_unknown_kind() {
    // The CHECK (kind IN ('summary', 'transcript-chunk')) constraint must
    // reject anything else. This protects the staleness queries from
    // ever seeing rows with a typo'd kind value.
    let index = SearchIndex::open_memory().expect("open");
    index
            .conn
            .execute(
                "INSERT INTO notes (path, title, domain, note_type, origin, status, date, tags, source, creator, body, summary, modified_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                params![
                    "notes/x.md",
                    "T", "tech", "article", "assisted", "", "2026-03-21",
                    "[]", "", "", "body", "summary", 0_i64,
                ],
            )
            .expect("insert note");

    let bytes = encode_le_f32(&[0.0_f32]);
    let result = index.conn.execute(
        "INSERT INTO note_embeddings (
                note_path, kind, chunk_index, text, embedding, dim,
                model_version, produced_at, source_modified_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            "notes/x.md",
            "garbage-kind",
            0_i64,
            "t",
            bytes,
            1_i64,
            "m",
            0_i64,
            0_i64,
        ],
    );
    assert!(result.is_err(), "CHECK constraint must reject unknown kinds");
}

#[cfg(feature = "vec")]
#[test]
fn vec_schema_unique_constraint_replaces_on_upsert_intent() {
    // The UNIQUE (note_path, kind, chunk_index, model_version) is the
    // upsert key used by Phase A5's re-embed loop. Direct INSERT must
    // fail on the second attempt, and INSERT OR REPLACE must succeed.
    let index = SearchIndex::open_memory().expect("open");
    index
            .conn
            .execute(
                "INSERT INTO notes (path, title, domain, note_type, origin, status, date, tags, source, creator, body, summary, modified_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                params![
                    "notes/up.md",
                    "T", "tech", "article", "assisted", "", "2026-03-21",
                    "[]", "", "", "body", "summary", 0_i64,
                ],
            )
            .expect("insert note");

    let bytes_a = encode_le_f32(&[1.0_f32, 0.0]);
    let bytes_b = encode_le_f32(&[0.0_f32, 1.0]);
    index
        .conn
        .execute(
            "INSERT INTO note_embeddings (
                    note_path, kind, chunk_index, text, embedding, dim,
                    model_version, produced_at, source_modified_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                "notes/up.md",
                "summary",
                0_i64,
                "a",
                bytes_a,
                2_i64,
                "bge-small-en-v1.5",
                0_i64,
                0_i64,
            ],
        )
        .expect("first insert");

    // Re-insert same (path, kind, chunk_index, model_version) must fail.
    let dup = index.conn.execute(
        "INSERT INTO note_embeddings (
                note_path, kind, chunk_index, text, embedding, dim,
                model_version, produced_at, source_modified_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            "notes/up.md",
            "summary",
            0_i64,
            "b",
            bytes_b.clone(),
            2_i64,
            "bge-small-en-v1.5",
            0_i64,
            0_i64,
        ],
    );
    assert!(dup.is_err(), "duplicate (path,kind,chunk,model) must be rejected");

    // INSERT OR REPLACE must replace cleanly.
    index
        .conn
        .execute(
            "INSERT OR REPLACE INTO note_embeddings (
                    note_path, kind, chunk_index, text, embedding, dim,
                    model_version, produced_at, source_modified_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                "notes/up.md",
                "summary",
                0_i64,
                "b",
                bytes_b,
                2_i64,
                "bge-small-en-v1.5",
                1_i64,
                1_i64,
            ],
        )
        .expect("replace");

    let (text, produced): (String, i64) = index
        .conn
        .query_row(
            "SELECT text, produced_at FROM note_embeddings \
                 WHERE note_path = ?1 AND kind = ?2 AND chunk_index = ?3 \
                   AND model_version = ?4",
            params!["notes/up.md", "summary", 0_i64, "bge-small-en-v1.5"],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("read replaced");
    assert_eq!(text, "b");
    assert_eq!(produced, 1);
}

#[cfg(feature = "vec")]
#[test]
fn index_one_update_preserves_existing_embedding_rows() {
    let index = SearchIndex::open_memory().expect("open");
    let note = make_test_note("inbox/stale.md", "# T\n\n## Summary\n\nv1.\n");
    index.index_one(&note, 100).expect("first index");

    // Seed an embedding row that snapshotted notes.modified_at = 100.
    let bytes = encode_le_f32(&[0.1_f32, 0.2, 0.3]);
    index
        .conn
        .execute(
            "INSERT INTO note_embeddings (
                    note_path, kind, chunk_index, text, embedding, dim,
                    model_version, produced_at, source_modified_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                "inbox/stale.md",
                "summary",
                0_i64,
                "v1.",
                bytes,
                3_i64,
                "bge-small-en-v1.5",
                100_i64,
                100_i64,
            ],
        )
        .expect("seed embedding");

    // Reindex with new body + new mtime. This is the moment the
    // contract is load-bearing: if the UPDATE branch deletes the
    // embedding, hybrid search loses this note until cortex's next
    // re-embed tick (up to 10 minutes of blackout).
    let updated = make_test_note("inbox/stale.md", "# T\n\n## Summary\n\nv2.\n");
    index.index_one(&updated, 300).expect("reindex");

    let count: i64 = index
        .conn
        .query_row(
            "SELECT COUNT(*) FROM note_embeddings WHERE note_path = 'inbox/stale.md'",
            [],
            |row| row.get(0),
        )
        .expect("count");
    assert_eq!(
        count, 1,
        "index_one must not delete embeddings on UPDATE (blackout-free contract)"
    );

    // The row's source_modified_at is still 100, but notes.modified_at
    // is now 300. stale_embedding_targets must flag it.
    let targets = index
        .stale_embedding_targets(EmbeddingKind::Summary, "bge-small-en-v1.5", 100)
        .expect("targets");
    let paths: Vec<&str> = targets.iter().map(|t| t.note_path.as_str()).collect();
    assert!(
        paths.contains(&"inbox/stale.md"),
        "stale-target scan must flag the modified note: {paths:?}"
    );
}

#[cfg(feature = "vec")]
#[test]
fn note_deletion_via_remove_stale_cascades_to_embeddings() {
    // Simulate the index_vault end-of-pass cleanup: a note's path
    // disappears from the scan list, remove_stale_notes deletes the
    // notes row, and the FK CASCADE removes every matching
    // note_embeddings row. No trigger involved.
    let index = SearchIndex::open_memory().expect("open");
    let note = make_test_note("inbox/gone.md", "# T\n\n## Summary\n\nGoing.\n");
    index.index_one(&note, 100).expect("index");
    let bytes = encode_le_f32(&[0.5_f32, 0.5]);
    index
        .conn
        .execute(
            "INSERT INTO note_embeddings (
                    note_path, kind, chunk_index, text, embedding, dim,
                    model_version, produced_at, source_modified_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                "inbox/gone.md",
                "summary",
                0_i64,
                "Going.",
                bytes,
                2_i64,
                "bge-small-en-v1.5",
                100_i64,
                100_i64,
            ],
        )
        .expect("seed");

    // Drive remove_stale_notes with an empty current-paths list: every
    // existing row is stale.
    let removed = index.remove_stale_notes(&[]).expect("remove_stale");
    assert!(removed >= 1);

    let leftover: i64 = index
        .conn
        .query_row(
            "SELECT COUNT(*) FROM note_embeddings WHERE note_path = 'inbox/gone.md'",
            [],
            |row| row.get(0),
        )
        .expect("count");
    assert_eq!(leftover, 0, "FK CASCADE must remove all embeddings for a deleted note");
}

#[cfg(feature = "vec")]
#[test]
fn cortex_upsert_after_stale_flag_replaces_old_row_atomically() {
    // End-to-end of the staleness loop: a stale row exists, cortex
    // computes a new embedding and upserts (INSERT OR REPLACE keyed
    // by UNIQUE constraint). Exactly one row must remain afterward,
    // with the new source_modified_at.
    let index = SearchIndex::open_memory().expect("open");
    let note = make_test_note("inbox/loop.md", "# T\n\n## Summary\n\nv1.\n");
    index.index_one(&note, 100).expect("index v1");

    let stale = encode_le_f32(&[0.1_f32, 0.2]);
    index
        .conn
        .execute(
            "INSERT INTO note_embeddings (
                    note_path, kind, chunk_index, text, embedding, dim,
                    model_version, produced_at, source_modified_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                "inbox/loop.md",
                "summary",
                0_i64,
                "v1",
                stale,
                2_i64,
                "bge-small-en-v1.5",
                100_i64,
                100_i64,
            ],
        )
        .expect("seed stale");

    // Reindex with bumped mtime so the row is stale.
    let updated = make_test_note("inbox/loop.md", "# T\n\n## Summary\n\nv2 latest.\n");
    index.index_one(&updated, 300).expect("reindex");

    // Cortex catches up: produce a new vector and upsert.
    let fresh = [0.7_f32, 0.3];
    index
        .upsert_embedding(
            "inbox/loop.md",
            EmbeddingKind::Summary,
            0,
            "v2 latest",
            &fresh,
            "bge-small-en-v1.5",
            300,
        )
        .expect("cortex upsert");

    let (count, source_mod, text): (i64, i64, String) = index
        .conn
        .query_row(
            "SELECT COUNT(*), MAX(source_modified_at), MAX(text)
                 FROM note_embeddings
                 WHERE note_path = 'inbox/loop.md' AND kind = 'summary' AND chunk_index = 0",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("post-upsert");
    assert_eq!(count, 1, "UNIQUE must keep exactly one row");
    assert_eq!(source_mod, 300, "source_modified_at must advance");
    assert_eq!(text, "v2 latest");

    // And the note is no longer flagged stale.
    let targets = index
        .stale_embedding_targets(EmbeddingKind::Summary, "bge-small-en-v1.5", 100)
        .expect("targets");
    let paths: Vec<&str> = targets.iter().map(|t| t.note_path.as_str()).collect();
    assert!(
        !paths.contains(&"inbox/loop.md"),
        "post-upsert the row must not appear as stale: {paths:?}"
    );
}
