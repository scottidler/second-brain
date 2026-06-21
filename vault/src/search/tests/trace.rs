//! Phase-1 tests for the borg staged-source (`trace`) columns: schema
//! migration, `index_one` write/read-back, and the forced-reindex repopulation
//! path that backfills the additive columns past the mtime gate.

use super::*;
use crate::frontmatter::Frontmatter;
use crate::note::Note;
use std::path::PathBuf;

/// Build a Note carrying the three promoted borg join keys. Pass `None` for
/// any key the note should omit (manual / legacy notes).
fn trace_note(path: &str, trace: Option<&str>, ingested: Option<&str>, trace_expires: Option<&str>) -> Note {
    let fm = Frontmatter {
        title: Some(format!("title for {path}")),
        note_type: Some("article".to_string()),
        origin: Some("assisted".to_string()),
        trace: trace.map(str::to_string),
        ingested: ingested.map(str::to_string),
        trace_expires: trace_expires.map(str::to_string),
        ..Frontmatter::default()
    };
    Note {
        path: PathBuf::from(path),
        frontmatter: fm,
        body: "body".to_string(),
        raw: "---\n---\nbody".to_string(),
    }
}

/// The `notes` column names currently present in the DB.
fn note_columns(index: &SearchIndex) -> Vec<String> {
    let mut stmt = index.conn.prepare("PRAGMA table_info(notes)").expect("pragma");
    stmt.query_map([], |row| row.get::<_, String>(1))
        .expect("query")
        .map(|r| r.expect("col name"))
        .collect()
}

/// Compute a file's mtime the same way `index_vault` does (epoch seconds).
fn file_mtime_secs(path: &std::path::Path) -> i64 {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .map(|t| t.duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs())
        .unwrap_or(0) as i64
}

#[test]
fn trace_columns_migrate_onto_preexisting_db_idempotently() {
    // Build a pre-trace `notes` table (the schema before this change: every
    // column EXCEPT trace/ingested/trace_expires) to exercise the real
    // `ALTER TABLE ADD COLUMN` migration path, then prove `ensure_schema` is
    // idempotent on a DB that already has the columns (no "duplicate column
    // name" on the second run).
    let conn = rusqlite::Connection::open_in_memory().expect("open");
    conn.execute_batch(
        "CREATE TABLE notes (
            path TEXT PRIMARY KEY, title TEXT, domain TEXT, note_type TEXT,
            origin TEXT, status TEXT, date TEXT, tags TEXT, source TEXT,
            creator TEXT, body TEXT, summary TEXT, claims TEXT DEFAULT '',
            modified_at INTEGER, quality TEXT DEFAULT '',
            classified INTEGER DEFAULT 0, classified_by TEXT DEFAULT '',
            confidence TEXT DEFAULT '', needs_review INTEGER DEFAULT 0,
            duplicate_group TEXT DEFAULT '', search_hit_count INTEGER DEFAULT 0,
            last_accessed_at INTEGER, inbound_link_count INTEGER DEFAULT 0,
            pinned INTEGER DEFAULT 0
        );",
    )
    .expect("create old schema");
    let index = SearchIndex { conn };

    assert!(!note_columns(&index).contains(&"trace".to_string()));

    index.ensure_schema().expect("first migration adds trace columns");
    index.ensure_schema().expect("second migration idempotent");

    let cols = note_columns(&index);
    assert!(cols.contains(&"trace".to_string()));
    assert!(cols.contains(&"ingested".to_string()));
    assert!(cols.contains(&"trace_expires".to_string()));
}

#[test]
fn index_one_persists_and_reads_back_trace_block() {
    let index = SearchIndex::open_memory().expect("open");
    let note = trace_note(
        "notes/x.md",
        Some("ht-95aa4e"),
        Some("2026-06-20T20:40:27-07:00"),
        Some("2026-08-19"),
    );
    index.index_one(&note, 1).expect("index");

    let row = index.get_note("notes/x.md").expect("query").expect("row");
    assert_eq!(row.trace, "ht-95aa4e");
    assert_eq!(row.ingested, "2026-06-20T20:40:27-07:00");
    assert_eq!(row.trace_expires, "2026-08-19");
}

#[test]
fn index_one_trace_only_leaves_expires_empty() {
    let index = SearchIndex::open_memory().expect("open");
    let note = trace_note("notes/legacy.md", Some("ht-legacy"), Some("2026-01-01"), None);
    index.index_one(&note, 1).expect("index");

    let row = index.get_note("notes/legacy.md").expect("query").expect("row");
    assert_eq!(row.trace, "ht-legacy");
    assert_eq!(row.ingested, "2026-01-01");
    assert_eq!(row.trace_expires, "");
}

#[test]
fn index_one_no_trace_reads_back_empty() {
    let index = SearchIndex::open_memory().expect("open");
    let note = trace_note("notes/manual.md", None, None, None);
    index.index_one(&note, 1).expect("index");

    let row = index.get_note("notes/manual.md").expect("query").expect("row");
    assert_eq!(row.trace, "");
    assert_eq!(row.ingested, "");
    assert_eq!(row.trace_expires, "");
}

#[test]
fn index_one_update_path_writes_trace() {
    // Insert then re-index the same path so the UPDATE branch (not INSERT) runs
    // and carries the trace columns through ?31/?32/?33.
    let index = SearchIndex::open_memory().expect("open");
    let before = trace_note("notes/u.md", None, None, None);
    index.index_one(&before, 1).expect("insert");

    let after = trace_note("notes/u.md", Some("ht-update"), Some("2026-06-20"), Some("2026-08-19"));
    let action = index.index_one(&after, 2).expect("update");
    assert_eq!(action, IndexAction::Updated);

    let row = index.get_note("notes/u.md").expect("query").expect("row");
    assert_eq!(row.trace, "ht-update");
    assert_eq!(row.trace_expires, "2026-08-19");
}

#[test]
fn forced_reindex_repopulates_when_mtime_unchanged() {
    let dir = tempfile::tempdir().expect("tempdir");
    let vault = dir.path();
    let note_path = vault.join("a.md");

    // First version of the note has no trace.
    std::fs::write(&note_path, "---\ntitle: A\n---\nbody").expect("write");
    let index = SearchIndex::open_memory().expect("open");
    index.index_vault(vault).expect("index");
    assert_eq!(index.get_note("a.md").expect("q").expect("row").trace, "");

    // The note gains a trace. We pin the stored mtime to the file's CURRENT
    // mtime so the non-force gate treats the note as unchanged (mirrors the
    // deploy scenario: additive columns added, but mtime hasn't moved).
    std::fs::write(&note_path, "---\ntitle: A\ntrace: ht-zzz\n---\nbody").expect("rewrite");
    let m = file_mtime_secs(&note_path);
    index
        .conn
        .execute("UPDATE notes SET modified_at = ?1 WHERE path = 'a.md'", params![m])
        .expect("pin mtime");

    // Non-force pass: mtime gate skips it, trace stays empty.
    index.index_vault(vault).expect("reindex");
    assert_eq!(
        index.get_note("a.md").expect("q").expect("row").trace,
        "",
        "non-force reindex must not pick up the change behind the mtime gate"
    );

    // Forced pass: gate bypassed, the trace is repopulated.
    index.index_vault_force(vault, true).expect("force reindex");
    assert_eq!(
        index.get_note("a.md").expect("q").expect("row").trace,
        "ht-zzz",
        "forced reindex must repopulate the trace column"
    );
}
