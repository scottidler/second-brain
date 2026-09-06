use super::*;

mod group_a;
mod group_b;
mod legacy_oracle_guard;
mod trace;

/// Helper: insert a test note directly into the DB
fn insert_test_note(index: &SearchIndex, path: &str, title: &str, domain: &str, tags: &[&str], body: &str) {
    let tags_json = serde_json::to_string(&tags).expect("tags json");
    index
            .conn
            .execute(
                "INSERT INTO notes (path, title, domain, note_type, origin, status, date, tags, source, creator, body, summary, modified_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                params![path, title, domain, "article", "assisted", "", "2026-03-21", tags_json, "", "", body, "", 0],
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
