use super::*;

mod group_a;
mod group_b;
mod legacy_oracle_guard;
mod tags_facet;
mod trace;

/// Helper: insert a test note directly into the DB
fn insert_test_note(index: &SearchIndex, path: &str, title: &str, tags: &[&str], body: &str) {
    let tags_json = serde_json::to_string(&tags).expect("tags json");
    index
            .conn
            .execute(
                "INSERT INTO notes (path, title, note_type, origin, status, date, tags, source, creator, body, summary, modified_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![path, title, "article", "assisted", "", "2026-03-21", tags_json, "", "", body, "", 0],
            )
            .expect("insert test note");
}

fn make_test_note(path: &str, body: &str) -> Note {
    use crate::frontmatter::Frontmatter;
    use std::path::PathBuf;
    let fm = Frontmatter {
        title: Some(format!("title for {path}")),
        note_type: Some("article".to_string()),
        origin: Some("assisted".to_string()),
        tags: Some(vec!["rust".to_string()]),
        ..Frontmatter::default()
    };
    Note {
        path: PathBuf::from(path),
        frontmatter: fm,
        body: body.to_string(),
        raw: format!("---\n---\n{body}"),
    }
}

fn signal_row(index: &SearchIndex, path: &str) -> (i64, Option<i64>, i64) {
    index
        .conn
        .query_row(
            "SELECT search_hit_count, last_accessed_at, inbound_link_count
                 FROM notes WHERE path = ?1",
            params![path],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, Option<i64>>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )
        .expect("signal row")
}

fn pinned_value(index: &SearchIndex, path: &str) -> i64 {
    index
        .conn
        .query_row("SELECT pinned FROM notes WHERE path = ?1", params![path], |row| {
            row.get::<_, i64>(0)
        })
        .expect("pinned row")
}

fn make_pinned_note(path: &str, pinned: Option<bool>, body: &str) -> Note {
    use crate::frontmatter::Frontmatter;
    use std::path::PathBuf;
    let fm = Frontmatter {
        title: Some(format!("title for {path}")),
        note_type: Some("article".to_string()),
        origin: Some("assisted".to_string()),
        pinned,
        ..Frontmatter::default()
    };
    Note {
        path: PathBuf::from(path),
        frontmatter: fm,
        body: body.to_string(),
        raw: format!("---\n---\n{body}"),
    }
}

fn cold_query(before_date: &str) -> ColdQuery {
    ColdQuery {
        before_date: before_date.to_string(),
        limit: 100,
    }
}

/// Like `make_test_note` but with an explicit content `date:`. Cold is now
/// measured by `date:` frontmatter, so the cold tests must seed it.
fn make_dated_note(path: &str, date: &str, body: &str) -> Note {
    use crate::frontmatter::Frontmatter;
    use std::path::PathBuf;
    let fm = Frontmatter {
        title: Some(format!("title for {path}")),
        date: Some(date.to_string()),
        note_type: Some("article".to_string()),
        origin: Some("assisted".to_string()),
        tags: Some(vec!["rust".to_string()]),
        ..Frontmatter::default()
    };
    Note {
        path: PathBuf::from(path),
        frontmatter: fm,
        body: body.to_string(),
        raw: format!("---\n---\n{body}"),
    }
}

/// A pinned note carrying an explicit content `date:`.
fn make_dated_pinned_note(path: &str, date: &str, pinned: Option<bool>, body: &str) -> Note {
    use crate::frontmatter::Frontmatter;
    use std::path::PathBuf;
    let fm = Frontmatter {
        title: Some(format!("title for {path}")),
        date: Some(date.to_string()),
        note_type: Some("article".to_string()),
        origin: Some("assisted".to_string()),
        pinned,
        ..Frontmatter::default()
    };
    Note {
        path: PathBuf::from(path),
        frontmatter: fm,
        body: body.to_string(),
        raw: format!("---\n---\n{body}"),
    }
}

/// A dated note with an explicit `type:` (for the daily/journal exclusion).
fn make_typed_dated_note(path: &str, date: &str, note_type: &str, body: &str) -> Note {
    use crate::frontmatter::Frontmatter;
    use std::path::PathBuf;
    let fm = Frontmatter {
        title: Some(format!("title for {path}")),
        date: Some(date.to_string()),
        note_type: Some(note_type.to_string()),
        origin: Some("authored".to_string()),
        ..Frontmatter::default()
    };
    Note {
        path: PathBuf::from(path),
        frontmatter: fm,
        body: body.to_string(),
        raw: format!("---\n---\n{body}"),
    }
}

/// Test-local encoder. The production encoder/decoder land in Phase A3
/// alongside `search_vector` which calls them on every row.
#[cfg(feature = "vec")]
fn encode_le_f32(vector: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(vector.len() * 4);
    for v in vector {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

/// Test-local validator. Phase A3's `search_vector` will run the same
/// check (length == dim * 4) before its inner dot-product loop.
#[cfg(feature = "vec")]
fn validate_le_f32_len(bytes: &[u8], dim: usize) -> eyre::Result<()> {
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

#[test]
fn repo_round_trips_through_index_to_graph_note_row() {
    // Phase 9 end-to-end: repo: frontmatter -> upsert bind -> notes.repo
    // column -> GraphNoteRow.repo, verbatim.
    use crate::frontmatter::Frontmatter;
    use std::path::PathBuf;
    let index = SearchIndex::open_memory().expect("open");
    let note = Note {
        path: PathBuf::from("inbox/session.md"),
        frontmatter: Frontmatter {
            title: Some("a session".to_string()),
            note_type: Some("session".to_string()),
            origin: Some("generated".to_string()),
            repo: Some("scottidler/loopr".to_string()),
            ..Frontmatter::default()
        },
        body: "body".to_string(),
        raw: "---\n---\nbody".to_string(),
    };
    index.index_one(&note, 1).expect("index");
    let rows = index.graph_note_rows().expect("rows");
    let row = rows.iter().find(|r| r.path == "inbox/session.md").expect("row present");
    assert_eq!(
        row.repo, "scottidler/loopr",
        "repo threads verbatim from frontmatter through the upsert to GraphNoteRow"
    );
}

/// Phase 4: `repos-touched` frontmatter -> upsert bind -> `notes.repos_touched`
/// column -> `GraphNoteRow.repos_touched`, exercising the THREE-STATE
/// distinction byte-for-byte. The DB column stores `None` as SQL NULL,
/// `Some(vec![])` as `'[]'`, and `Some(xs)` as the JSON array; the edge-facing
/// `GraphNoteRow.repos_touched` flattens NULL and `'[]'` to an empty Vec (both
/// mean "no bridge") and carries the populated set verbatim.
#[test]
fn repos_touched_round_trips_through_index_to_graph_note_row_three_state() {
    use crate::frontmatter::Frontmatter;
    use std::path::PathBuf;

    fn index_with(index: &SearchIndex, path: &str, repos_touched: Option<Vec<String>>) {
        let note = Note {
            path: PathBuf::from(path),
            frontmatter: Frontmatter {
                title: Some("a session".to_string()),
                note_type: Some("session".to_string()),
                origin: Some("generated".to_string()),
                repos_touched,
                ..Frontmatter::default()
            },
            body: "body".to_string(),
            raw: "---\n---\nbody".to_string(),
        };
        index.index_one(&note, 1).expect("index");
    }

    let index = SearchIndex::open_memory().expect("open");
    index_with(&index, "inbox/none.md", None);
    index_with(&index, "inbox/empty.md", Some(vec![]));
    index_with(
        &index,
        "inbox/multi.md",
        Some(vec!["scottidler/loopr".to_string(), "tatari-tv/marquee".to_string()]),
    );

    // Byte-for-byte three-state in the stored column: NULL vs '[]' vs JSON.
    let raw = |path: &str| -> Option<String> {
        index
            .conn
            .query_row("SELECT repos_touched FROM notes WHERE path = ?1", params![path], |r| {
                r.get::<_, Option<String>>(0)
            })
            .expect("query")
    };
    assert_eq!(raw("inbox/none.md"), None, "None -> SQL NULL (touched set unknowable)");
    assert_eq!(
        raw("inbox/empty.md").as_deref(),
        Some("[]"),
        "Some(vec![]) -> '[]' (definitively touched nothing), NEVER NULL"
    );
    assert_eq!(
        raw("inbox/multi.md").as_deref(),
        Some(r#"["scottidler/loopr","tatari-tv/marquee"]"#),
        "Some(xs) -> the JSON array verbatim"
    );

    // GraphNoteRow flattens NULL and '[]' to empty, carries the populated set.
    let rows = index.graph_note_rows().expect("rows");
    let get = |path: &str| rows.iter().find(|r| r.path == path).expect("row present");
    assert!(
        get("inbox/none.md").repos_touched.is_empty(),
        "None -> empty touched set"
    );
    assert!(
        get("inbox/empty.md").repos_touched.is_empty(),
        "Some(vec![]) -> empty touched set at the edge seam"
    );
    assert_eq!(
        get("inbox/multi.md").repos_touched,
        vec!["scottidler/loopr".to_string(), "tatari-tv/marquee".to_string()],
        "Some(xs) -> the touched set threads verbatim to GraphNoteRow"
    );
}

// ---- Phase 5: the tags facet ----

fn tagged_note(path: &str, tags: &[&str]) -> Note {
    use crate::frontmatter::Frontmatter;
    use std::path::PathBuf;
    Note {
        path: PathBuf::from(path),
        frontmatter: Frontmatter {
            title: Some(path.to_string()),
            note_type: Some("note".to_string()),
            origin: Some("authored".to_string()),
            tags: Some(tags.iter().map(|t| t.to_string()).collect()),
            ..Frontmatter::default()
        },
        body: "body".to_string(),
        raw: "---\n---\nbody".to_string(),
    }
}

#[test]
fn note_tags_rows_track_the_frontmatter_list() {
    let index = SearchIndex::open_memory().expect("open");
    index
        .index_one(&tagged_note("notes/a.md", &["rust", "ai"]), 1)
        .expect("index");

    let mut tags: Vec<String> = index
        .conn
        .prepare("SELECT tag FROM note_tags WHERE path = ?1")
        .expect("prepare")
        .query_map(["notes/a.md"], |r| r.get(0))
        .expect("query")
        .filter_map(Result::ok)
        .collect();
    tags.sort();
    assert_eq!(tags, vec!["ai".to_string(), "rust".to_string()]);

    // Re-indexing with a different list REPLACES the rows: a tag removed from
    // frontmatter must not linger in the facet.
    index
        .index_one(&tagged_note("notes/a.md", &["rust"]), 2)
        .expect("reindex");
    let after: Vec<String> = index
        .conn
        .prepare("SELECT tag FROM note_tags WHERE path = ?1")
        .expect("prepare")
        .query_map(["notes/a.md"], |r| r.get(0))
        .expect("query")
        .filter_map(Result::ok)
        .collect();
    assert_eq!(after, vec!["rust".to_string()]);
}

#[test]
fn note_with_no_tags_indexes_as_empty_json_not_empty_string() {
    // 303 rows in the live index held `tags = ''`, which is not valid JSON, so
    // `json_each` errored on them and every tag query had to special-case it.
    use crate::frontmatter::Frontmatter;
    use std::path::PathBuf;
    let index = SearchIndex::open_memory().expect("open");
    let note = Note {
        path: PathBuf::from("notes/untagged.md"),
        frontmatter: Frontmatter {
            title: Some("untagged".to_string()),
            ..Frontmatter::default()
        },
        body: "body".to_string(),
        raw: "---\n---\nbody".to_string(),
    };
    index.index_one(&note, 1).expect("index");

    let stored: String = index
        .conn
        .query_row("SELECT tags FROM notes WHERE path = ?1", ["notes/untagged.md"], |r| {
            r.get(0)
        })
        .expect("row");
    assert_eq!(stored, "[]");

    let valid: i64 = index
        .conn
        .query_row(
            "SELECT json_valid(tags) FROM notes WHERE path = ?1",
            ["notes/untagged.md"],
            |r| r.get(0),
        )
        .expect("json_valid");
    assert_eq!(valid, 1, "every indexed row must be valid JSON");
}

#[test]
fn note_tags_and_notes_tags_agree() {
    // AC4's standing invariant, in miniature: the facet row count equals the
    // json_each count over the same rows.
    let index = SearchIndex::open_memory().expect("open");
    index
        .index_one(&tagged_note("notes/a.md", &["rust", "ai"]), 1)
        .expect("a");
    index.index_one(&tagged_note("notes/b.md", &["rust"]), 1).expect("b");
    index.index_one(&tagged_note("notes/c.md", &[]), 1).expect("c");

    let facet: i64 = index
        .conn
        .query_row("SELECT count(*) FROM note_tags", [], |r| r.get(0))
        .expect("facet count");
    let json: i64 = index
        .conn
        .query_row(
            "SELECT count(*) FROM notes, json_each(notes.tags) WHERE json_valid(notes.tags)",
            [],
            |r| r.get(0),
        )
        .expect("json count");
    assert_eq!(facet, json, "facet and JSON column disagree");
    assert_eq!(facet, 3);
}

#[test]
fn removing_a_note_removes_its_facet_rows() {
    let index = SearchIndex::open_memory().expect("open");
    index.index_one(&tagged_note("notes/a.md", &["rust"]), 1).expect("a");
    index.index_one(&tagged_note("notes/b.md", &["ai"]), 1).expect("b");

    // `a` is no longer in the vault.
    let removed = index
        .remove_stale_notes(&["notes/b.md".to_string()])
        .expect("remove stale");
    assert_eq!(removed, 1);

    let orphans: i64 = index
        .conn
        .query_row(
            "SELECT count(*) FROM note_tags WHERE path NOT IN (SELECT path FROM notes)",
            [],
            |r| r.get(0),
        )
        .expect("orphan count");
    assert_eq!(orphans, 0, "facet rows outlived their note");
}

#[test]
fn index_is_atomic_per_note() {
    // A failed facet write must roll the `notes` row back with it, so the two
    // tables can never disagree. Forced by dropping `note_tags` so the insert
    // errors while the `notes` upsert would otherwise have succeeded.
    let index = SearchIndex::open_memory().expect("open");
    index
        .conn
        .execute_batch("DROP TABLE note_tags")
        .expect("drop the facet table");

    let err = index.index_one(&tagged_note("notes/doomed.md", &["rust"]), 1);
    assert!(err.is_err(), "a failed facet write must fail the index call");

    let rows: i64 = index
        .conn
        .query_row("SELECT count(*) FROM notes WHERE path = ?1", ["notes/doomed.md"], |r| {
            r.get(0)
        })
        .expect("count");
    assert_eq!(rows, 0, "the notes row survived a failed facet write");
}

#[test]
fn list_notes_filters_by_tag_or_and_and() {
    let index = SearchIndex::open_memory().expect("open");
    index
        .index_one(&tagged_note("notes/a.md", &["rust", "ai"]), 1)
        .expect("a");
    index.index_one(&tagged_note("notes/b.md", &["rust"]), 1).expect("b");
    index.index_one(&tagged_note("notes/c.md", &["cooking"]), 1).expect("c");

    let paths = |rows: Vec<NoteRow>| {
        let mut p: Vec<String> = rows.into_iter().map(|r| r.path).collect();
        p.sort();
        p
    };

    // OR (the default): either tag.
    let rust_or_cooking = vec!["rust".to_string(), "cooking".to_string()];
    let rows = index
        .list_notes(Some(&rust_or_cooking), false, None, None, None, None, None)
        .expect("or");
    assert_eq!(
        paths(rows),
        vec![
            "notes/a.md".to_string(),
            "notes/b.md".to_string(),
            "notes/c.md".to_string()
        ]
    );

    // AND: every tag.
    let rust_and_ai = vec!["rust".to_string(), "ai".to_string()];
    let rows = index
        .list_notes(Some(&rust_and_ai), true, None, None, None, None, None)
        .expect("and");
    assert_eq!(paths(rows), vec!["notes/a.md".to_string()]);

    // An empty list is NOT a filter that matches nothing.
    let rows = index
        .list_notes(Some(&[]), false, None, None, None, None, None)
        .expect("empty");
    assert_eq!(rows.len(), 3, "an empty tag list must not filter everything out");

    // No filter at all.
    let rows = index
        .list_notes(None, false, None, None, None, None, None)
        .expect("none");
    assert_eq!(rows.len(), 3);
}
