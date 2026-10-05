//! A note whose file metadata cannot be read is skipped for the pass: its
//! existing row survives, it is counted in `IndexStats::skipped`, and it is not
//! reported as removed or re-indexed with a fabricated mtime.

use super::*;

fn failing_stat_for(name: &'static str) -> impl Fn(&std::path::Path) -> std::io::Result<i64> {
    move |path| {
        if path.file_name().is_some_and(|f| f == name) {
            Err(std::io::Error::other("injected metadata failure"))
        } else {
            Ok(1_000)
        }
    }
}

#[test]
fn metadata_error_keeps_existing_row_and_counts_skipped() {
    let dir = tempfile::tempdir().expect("tempdir");
    let vault = dir.path();
    std::fs::write(vault.join("a.md"), "---\ntitle: A\n---\nbody").expect("write a");
    std::fs::write(vault.join("b.md"), "---\ntitle: B\n---\nbody").expect("write b");

    let index = SearchIndex::open_memory().expect("open");
    let first = index
        .index_vault_with_stat(vault, false, |_| Ok(1_000))
        .expect("first pass");
    assert_eq!((first.inserted, first.skipped), (2, 0));

    // b.md is edited, but its metadata read fails: the stale row must stay.
    std::fs::write(vault.join("b.md"), "---\ntitle: B2\n---\nbody").expect("rewrite b");
    let stats = index
        .index_vault_with_stat(vault, false, failing_stat_for("b.md"))
        .expect("second pass");

    assert_eq!(stats.skipped, 1, "the unreadable note is counted: {stats:?}");
    assert_eq!(stats.removed, 0, "a skipped note is not a stale note: {stats:?}");
    assert_eq!(stats.updated, 0, "a skipped note is not re-indexed: {stats:?}");
    assert_eq!(stats.unchanged, 1, "a.md is unchanged: {stats:?}");

    let row = index.get_note("b.md").expect("query").expect("b.md row survives");
    assert_eq!(row.title, "B", "existing row untouched, not upserted");
    let mtime: i64 = index
        .conn
        .query_row("SELECT modified_at FROM notes WHERE path = 'b.md'", [], |r| r.get(0))
        .expect("mtime");
    assert_eq!(mtime, 1_000, "mtime was not overwritten with 0");
}

#[test]
fn metadata_error_on_a_new_note_inserts_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let vault = dir.path();
    std::fs::write(vault.join("a.md"), "---\ntitle: A\n---\nbody").expect("write a");

    let index = SearchIndex::open_memory().expect("open");
    let stats = index
        .index_vault_with_stat(vault, false, failing_stat_for("a.md"))
        .expect("pass");
    assert_eq!((stats.inserted, stats.skipped), (0, 1));
    assert!(index.get_note("a.md").expect("query").is_none());
}

#[test]
fn real_pass_reports_zero_skipped() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("a.md"), "---\ntitle: A\n---\nbody").expect("write");
    let index = SearchIndex::open_memory().expect("open");
    let stats = index.index_vault(dir.path()).expect("pass");
    assert_eq!((stats.inserted, stats.skipped), (1, 0));
}
