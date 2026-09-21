//! Review-fold regression tests for the `note_tags` facet: the distinct-tag
//! count, duplicate handling in both the filter builder and `index_one`, and
//! atomic note removal through `index_changed`.

use super::*;
use crate::frontmatter::Frontmatter;
use std::path::PathBuf;

fn tagged_note(path: &str, tags: &[&str]) -> Note {
    Note {
        path: PathBuf::from(path),
        frontmatter: Frontmatter {
            title: Some(path.to_string()),
            note_type: Some("article".to_string()),
            origin: Some("assisted".to_string()),
            tags: Some(tags.iter().map(|t| t.to_string()).collect()),
            ..Frontmatter::default()
        },
        body: "body".to_string(),
        raw: "---\n---\nbody".to_string(),
    }
}

fn facet_rows(index: &SearchIndex, path: &str) -> u64 {
    index
        .conn
        .query_row("SELECT COUNT(*) FROM note_tags WHERE path = ?1", params![path], |row| {
            row.get(0)
        })
        .expect("facet count")
}

fn notes_rows(index: &SearchIndex, path: &str) -> u64 {
    index
        .conn
        .query_row("SELECT COUNT(*) FROM notes WHERE path = ?1", params![path], |row| {
            row.get(0)
        })
        .expect("notes count")
}

/// C1: `by_tag` is `top_tags(20)`, so a vault with more than 20 tags used to
/// report exactly 20 through `by_tag.len()`. `distinct_tags` is the count.
#[test]
fn stats_distinct_tags_is_not_capped_at_the_top_20() {
    let index = SearchIndex::open_memory().expect("open");
    for i in 0..25 {
        let tag = format!("tag-{i:02}");
        index
            .index_one(&tagged_note(&format!("notes/{i}.md"), &[&tag]), 1)
            .expect("index");
    }
    let stats = index.stats().expect("stats");
    assert_eq!(stats.by_tag.len(), 20, "by_tag stays the top-20 list");
    assert_eq!(stats.distinct_tags, 25, "distinct_tags counts every tag");
}

/// C2: a duplicated input with `tags_all = true` compared `count(DISTINCT
/// t.tag)` (at most 1 here) against the raw list length (2), so it could
/// never match. Dedup happens before the placeholder list and the count.
#[test]
fn tags_all_filter_matches_when_the_input_list_repeats_a_tag() {
    let index = SearchIndex::open_memory().expect("open");
    index
        .index_one(&tagged_note("notes/a.md", &["rust", "ai"]), 1)
        .expect("a");
    index.index_one(&tagged_note("notes/b.md", &["ai"]), 1).expect("b");

    let repeated = vec!["rust".to_string(), "rust".to_string()];
    let rows = index
        .list_notes(Some(&repeated), true, None, None, None, None, None)
        .expect("list");
    assert_eq!(rows.len(), 1, "a repeated tag still matches in AND mode");
    assert_eq!(rows[0].path, "notes/a.md");

    // The deduped list still binds the right number of parameters in OR mode.
    let rows = index
        .list_notes(Some(&repeated), false, None, None, None, None, None)
        .expect("list or");
    assert_eq!(rows.len(), 1);

    // And a genuine two-tag AND still requires both.
    let both = vec!["rust".to_string(), "ai".to_string()];
    let rows = index
        .list_notes(Some(&both), true, None, None, None, None, None)
        .expect("list both");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].path, "notes/a.md");
}

/// C3: a note whose frontmatter repeats a tag used to store the duplicate in
/// `notes.tags` while `INSERT OR IGNORE` collapsed it in `note_tags`, breaking
/// the AC4 `json_each` == `note_tags` equality. Both are written deduped.
#[test]
fn index_one_dedups_a_repeated_frontmatter_tag_in_both_stores() {
    let index = SearchIndex::open_memory().expect("open");
    index
        .index_one(&tagged_note("notes/dup.md", &["rust", "rust", "ai"]), 1)
        .expect("index");

    let json_count: u64 = index
        .conn
        .query_row(
            "SELECT COUNT(*) FROM notes, json_each(notes.tags) WHERE json_valid(notes.tags)",
            [],
            |row| row.get(0),
        )
        .expect("json_each count");
    let facet_count: u64 = index
        .conn
        .query_row("SELECT COUNT(*) FROM note_tags", [], |row| row.get(0))
        .expect("facet count");
    assert_eq!(json_count, 2, "the JSON column is deduped");
    assert_eq!(facet_count, json_count, "AC4: facet rows equal json_each pairs");

    let row = index.get_note("notes/dup.md").expect("get").expect("present");
    let tags: Vec<String> = serde_json::from_str(&row.tags).expect("tags json");
    assert_eq!(tags, vec!["rust".to_string(), "ai".to_string()], "order is preserved");
}

/// C4: removing a deleted file through `index_changed` deletes the note and
/// its facet rows together, so no orphan `note_tags` row survives.
#[test]
fn index_changed_removal_leaves_no_facet_rows() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let vault_root = tmp.path();
    let index = SearchIndex::open_memory().expect("open");
    index
        .index_one(&tagged_note("notes/gone.md", &["rust", "ai"]), 1)
        .expect("index");
    assert_eq!(facet_rows(&index, "notes/gone.md"), 2);

    let missing = vault_root.join("notes/gone.md");
    let stats = index.index_changed(vault_root, &[missing]).expect("index_changed");
    assert_eq!(stats.removed, 1);
    assert_eq!(notes_rows(&index, "notes/gone.md"), 0);
    assert_eq!(facet_rows(&index, "notes/gone.md"), 0, "no orphan facet rows");
}

/// C4, the failure half: when the facet delete fails, the `notes` delete rolls
/// back with it instead of leaving a facet row pointing at a vanished note.
/// The trigger makes the second statement of the pair fail on demand; before
/// the SAVEPOINT the `notes` row was already gone by then.
#[test]
fn index_changed_removal_rolls_back_the_note_when_the_facet_delete_fails() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let vault_root = tmp.path();
    let index = SearchIndex::open_memory().expect("open");
    index
        .index_one(&tagged_note("notes/gone.md", &["rust"]), 1)
        .expect("index");
    index
        .conn
        .execute_batch(
            "CREATE TRIGGER block_facet_delete BEFORE DELETE ON note_tags
             BEGIN SELECT RAISE(ABORT, 'facet delete blocked'); END;",
        )
        .expect("trigger");

    let missing = vault_root.join("notes/gone.md");
    let err = index
        .index_changed(vault_root, &[missing])
        .expect_err("the blocked facet delete must surface");
    assert!(format!("{err:#}").contains("facet delete blocked"), "{err:#}");
    assert_eq!(notes_rows(&index, "notes/gone.md"), 1, "the notes delete rolled back");
    assert_eq!(facet_rows(&index, "notes/gone.md"), 1);
    assert!(index.conn.is_autocommit(), "no savepoint left open on the connection");
}
