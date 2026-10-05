//! The shared SQL filter builder, per public query: exact SQL and params with
//! every filter set, and a database-level positive and negative with the same
//! filters. A filter dropped from a builder fails the exact-SQL test and the
//! negative test for that query.

use super::*;
use crate::search::query::{list_notes_filter, search_filter};
use crate::search::stats::{
    classify_filter, notes_by_creator_filter, notes_by_source_domain_filter, tag_search_filter,
};
use rusqlite::types::Value;

fn text(s: &str) -> Value {
    Value::Text(s.to_string())
}

fn two_tags() -> Vec<String> {
    vec!["rust".to_string(), "sqlite".to_string()]
}

const OR_TAGS: &str = " AND EXISTS (SELECT 1 FROM note_tags t WHERE t.path = notes.path AND t.tag IN (?, ?))";
const NOTE_COLS: &str = "SELECT path, title, note_type, origin, status, date, tags, source, creator, body, summary, trace, ingested, trace_expires";

/// The statement must prepare and its placeholder count must equal the params
/// count: a numbered or missing `?` shows up here.
fn assert_binds(index: &SearchIndex, f: &crate::search::filter::Filter) {
    let stmt = index.conn.prepare(&f.sql).expect("prepare");
    assert_eq!(stmt.parameter_count(), f.params.len(), "{}", f.sql);
}

fn seed(index: &SearchIndex) {
    index
        .conn
        .execute(
            "INSERT INTO notes (path, title, note_type, origin, status, date, tags, source, creator, body, summary, modified_at, classified, classified_by, confidence, needs_review)
             VALUES ('n/hit.md', 'hit', 'article', 'assisted', 'active', '2026-03-10', '[\"rust\",\"sqlite\"]', 'https://example.com/x', 'alice', 'zebra body', '', 0, 1, 'llm', 'high', 1)",
            [],
        )
        .expect("insert hit");
    index
        .conn
        .execute(
            "INSERT INTO notes (path, title, note_type, origin, status, date, tags, source, creator, body, summary, modified_at, classified, classified_by, confidence, needs_review)
             VALUES ('n/miss.md', 'miss', 'video', 'assisted', 'draft', '2025-01-01', '[\"other\"]', 'https://other.org/y', 'bob', 'zebra body', '', 0, 0, '', '', 0)",
            [],
        )
        .expect("insert miss");
    for (path, tag) in [("n/hit.md", "rust"), ("n/hit.md", "sqlite"), ("n/miss.md", "other")] {
        index
            .conn
            .execute("INSERT INTO note_tags (path, tag) VALUES (?1, ?2)", params![path, tag])
            .expect("insert tag");
    }
}

fn paths(rows: Vec<NoteRow>) -> Vec<String> {
    rows.into_iter().map(|r| r.path).collect()
}

#[test]
fn search_filter_all_set_has_exact_sql_and_params() {
    let tags = two_tags();
    let f = search_filter("zebra", Some(&tags), true, Some("article"), Some("active"));
    assert_eq!(
        f.sql,
        "SELECT n.path, n.title, n.note_type, n.origin, n.status, n.date, n.tags, n.source, n.creator, n.body, n.summary, n.trace, n.ingested, n.trace_expires
             FROM notes n
             JOIN notes_fts f ON n.rowid = f.rowid
             WHERE notes_fts MATCH ? AND (SELECT count(DISTINCT t.tag) FROM note_tags t WHERE t.path = n.path AND t.tag IN (?, ?)) = 2 AND n.note_type = ? AND n.status = ?"
    );
    assert_eq!(
        f.params,
        vec![
            text("zebra"),
            text("rust"),
            text("sqlite"),
            text("article"),
            text("active")
        ]
    );
}

#[test]
fn list_notes_filter_all_set_has_exact_sql_and_params() {
    let tags = two_tags();
    let f = list_notes_filter(
        Some(&tags),
        false,
        Some("article"),
        Some("active"),
        Some("2026-01-01"),
        Some("2026-12-31"),
    );
    assert_eq!(
        f.sql,
        format!(
            "{NOTE_COLS}\n             FROM notes WHERE 1=1{} AND note_type = ? AND status = ? AND date >= ? AND date <= ?",
            OR_TAGS
        )
    );
    assert_eq!(
        f.params,
        vec![
            text("rust"),
            text("sqlite"),
            text("article"),
            text("active"),
            text("2026-01-01"),
            text("2026-12-31")
        ]
    );
}

#[test]
fn list_notes_filter_with_nothing_set_is_the_bare_statement() {
    let f = list_notes_filter(None, false, None, None, None, None);
    assert_eq!(f.sql, format!("{NOTE_COLS}\n             FROM notes WHERE 1=1"));
    assert!(f.params.is_empty());
}

#[test]
fn tag_search_filter_has_exact_sql_and_params() {
    let tags = two_tags();
    let f = tag_search_filter(Some(&tags), false);
    assert_eq!(
        f.sql,
        format!("{NOTE_COLS}\n             FROM notes WHERE tags != ''{OR_TAGS}")
    );
    assert_eq!(f.params, vec![text("rust"), text("sqlite")]);
}

#[test]
fn creator_and_source_filters_have_exact_sql_and_params() {
    let tags = two_tags();
    let c = notes_by_creator_filter("Alice", Some(&tags), false);
    assert_eq!(
        c.sql,
        format!("{NOTE_COLS}\n             FROM notes WHERE LOWER(creator) LIKE ?{OR_TAGS}")
    );
    assert_eq!(c.params, vec![text("%alice%"), text("rust"), text("sqlite")]);
    let s = notes_by_source_domain_filter("Example.com", Some(&tags), false);
    assert_eq!(
        s.sql,
        format!("{NOTE_COLS}\n             FROM notes WHERE source LIKE ?{OR_TAGS}")
    );
    assert_eq!(s.params, vec![text("%example.com%"), text("rust"), text("sqlite")]);
}

#[test]
fn classify_filter_has_exact_sql_with_and_without_group_by() {
    let tags = two_tags();
    let f = classify_filter(
        "confidence, COUNT(*)",
        "classified = 1",
        Some(&tags),
        true,
        Some("confidence"),
    );
    assert_eq!(
        f.sql,
        "SELECT confidence, COUNT(*) FROM notes WHERE classified = 1 AND (SELECT count(DISTINCT t.tag) FROM note_tags t WHERE t.path = notes.path AND t.tag IN (?, ?)) = 2 GROUP BY confidence ORDER BY COUNT(*) DESC"
    );
    assert_eq!(f.params, vec![text("rust"), text("sqlite")]);
    let bare = classify_filter("COUNT(*)", "needs_review = 1", None, false, None);
    assert_eq!(bare.sql, "SELECT COUNT(*) FROM notes WHERE needs_review = 1");
}

#[cfg(feature = "vec")]
#[test]
fn vector_filter_all_set_has_exact_sql_and_params() {
    let tags = two_tags();
    let f = crate::search::vector::vector_filter("m1".to_string(), Some(&tags), false, Some("article"), Some("active"));
    assert_eq!(
        f.sql,
        "SELECT e.note_path, e.embedding, e.dim
             FROM note_embeddings e
             JOIN notes n ON n.path = e.note_path
             WHERE e.model_version = ? AND EXISTS (SELECT 1 FROM note_tags t WHERE t.path = n.path AND t.tag IN (?, ?)) AND n.note_type = ? AND n.status = ?"
    );
    assert_eq!(
        f.params,
        vec![
            text("m1"),
            text("rust"),
            text("sqlite"),
            text("article"),
            text("active")
        ]
    );
}

#[test]
fn every_builder_binds_exactly_its_params() {
    let index = SearchIndex::open_memory().expect("open");
    let tags = two_tags();
    assert_binds(
        &index,
        &search_filter("zebra", Some(&tags), true, Some("article"), Some("active")),
    );
    assert_binds(
        &index,
        &list_notes_filter(Some(&tags), true, Some("a"), Some("b"), Some("c"), Some("d")),
    );
    assert_binds(&index, &tag_search_filter(Some(&tags), true));
    assert_binds(&index, &notes_by_creator_filter("a", Some(&tags), true));
    assert_binds(&index, &notes_by_source_domain_filter("a", Some(&tags), true));
    assert_binds(
        &index,
        &classify_filter("COUNT(*)", "classified = 1", Some(&tags), true, None),
    );
    #[cfg(feature = "vec")]
    assert_binds(
        &index,
        &crate::search::vector::vector_filter("m".to_string(), Some(&tags), true, Some("a"), Some("b")),
    );
}

#[test]
fn search_all_filters_positive_and_each_filter_negative() {
    let index = SearchIndex::open_memory().expect("open");
    seed(&index);
    let tags = two_tags();
    let hit = |t: Option<&[String]>, all, nt: Option<&str>, st: Option<&str>| {
        paths(index.search("zebra", t, all, nt, st, None).expect("search"))
    };
    assert_eq!(
        hit(Some(&tags), true, Some("article"), Some("active")),
        vec!["n/hit.md"]
    );
    let other = vec!["other".to_string()];
    assert!(hit(Some(&other), true, Some("article"), Some("active")).is_empty());
    assert!(hit(Some(&tags), true, Some("video"), Some("active")).is_empty());
    assert!(hit(Some(&tags), true, Some("article"), Some("draft")).is_empty());
}

#[test]
fn list_notes_all_filters_positive_and_each_filter_negative() {
    let index = SearchIndex::open_memory().expect("open");
    seed(&index);
    let tags = two_tags();
    let run = |t: &[String], nt: &str, st: &str, after: &str, before: &str| {
        paths(
            index
                .list_notes(Some(t), true, Some(nt), Some(st), Some(after), Some(before), None)
                .expect("list"),
        )
    };
    assert_eq!(
        run(&tags, "article", "active", "2026-01-01", "2026-12-31"),
        vec!["n/hit.md"]
    );
    let other = vec!["other".to_string()];
    assert!(run(&other, "article", "active", "2026-01-01", "2026-12-31").is_empty());
    assert!(run(&tags, "video", "active", "2026-01-01", "2026-12-31").is_empty());
    assert!(run(&tags, "article", "draft", "2026-01-01", "2026-12-31").is_empty());
    assert!(run(&tags, "article", "active", "2026-06-01", "2026-12-31").is_empty());
    assert!(run(&tags, "article", "active", "2026-01-01", "2026-02-01").is_empty());
}

#[test]
fn creator_source_and_tag_search_positive_and_negative() {
    let index = SearchIndex::open_memory().expect("open");
    seed(&index);
    let tags = two_tags();
    let other = vec!["other".to_string()];
    assert_eq!(
        paths(
            index
                .notes_by_creator("alice", Some(&tags), true, None)
                .expect("creator")
        ),
        vec!["n/hit.md"]
    );
    assert!(
        index
            .notes_by_creator("alice", Some(&other), true, None)
            .expect("creator")
            .is_empty()
    );
    assert_eq!(
        paths(
            index
                .notes_by_source_domain("example.com", Some(&tags), true, None)
                .expect("source")
        ),
        vec!["n/hit.md"]
    );
    assert!(
        index
            .notes_by_source_domain("example.com", Some(&other), true, None)
            .expect("source")
            .is_empty()
    );
    assert_eq!(
        paths(index.tag_search("rust", Some(&tags), true, None).expect("tag_search")),
        vec!["n/hit.md"]
    );
    assert!(
        index
            .tag_search("rust", Some(&other), true, None)
            .expect("tag_search")
            .is_empty()
    );
}

#[test]
fn classify_stats_tag_filter_narrows_every_filtered_count() {
    let index = SearchIndex::open_memory().expect("open");
    seed(&index);
    let tags = two_tags();
    let other = vec!["other".to_string()];
    let narrowed = index.classify_stats(Some(&tags), true).expect("stats");
    assert_eq!(narrowed.total_classified, 1);
    assert_eq!(narrowed.pending_review, 1);
    assert_eq!(narrowed.by_method, vec![("llm".to_string(), 1)]);
    let none = index.classify_stats(Some(&other), true).expect("stats");
    assert_eq!(none.total_classified, 0);
    assert_eq!(none.pending_review, 0);
    assert!(none.by_method.is_empty());
}
