use super::*;

fn temp_ledger_path() -> PathBuf {
    let dir = std::env::temp_dir().join("vault-test-ledger");
    fs::create_dir_all(&dir).ok();
    dir.join("borg-ledger.md")
}

fn cleanup(path: &Path) {
    let _ = fs::remove_file(path);
}

#[test]
fn test_ensure_ledger_exists_creates_file() {
    let path = temp_ledger_path().with_file_name("test-vault-create.md");
    cleanup(&path);
    ensure_ledger_exists(&path).expect("should create");
    assert!(path.exists());
    let content = fs::read_to_string(&path).expect("read");
    assert!(content.contains("# Borg Ledger"));
    assert!(content.contains("| Date |"));
    cleanup(&path);
}

#[test]
fn test_ensure_ledger_exists_idempotent() {
    let path = temp_ledger_path().with_file_name("test-vault-idempotent.md");
    cleanup(&path);
    ensure_ledger_exists(&path).expect("first");
    let content1 = fs::read_to_string(&path).expect("read");
    ensure_ledger_exists(&path).expect("second");
    let content2 = fs::read_to_string(&path).expect("read");
    assert_eq!(content1, content2);
    cleanup(&path);
}

#[test]
fn test_check_duplicate_empty_log() {
    let path = temp_ledger_path().with_file_name("test-vault-dedup-empty.md");
    cleanup(&path);
    ensure_ledger_exists(&path).expect("create");
    let result = check_duplicate(&path, "https://example.com").expect("check");
    assert!(result.is_none());
    cleanup(&path);
}

#[test]
fn test_append_and_check_duplicate() {
    let path = temp_ledger_path().with_file_name("test-vault-append-dedup.md");
    cleanup(&path);

    let entry = LedgerEntry {
        date: "2026-03-07".to_string(),
        time: "14:30".to_string(),
        method: Method::Cli,
        source: "https://example.com/article".to_string(),
        domain: Some("ai".to_string()),
        filename: Some("test-article.md".to_string()),
        trace_id: None,
    };
    append_entry(&path, &entry).expect("append");

    let result = check_duplicate(&path, "https://example.com/article").expect("check");
    assert_eq!(result, Some("2026-03-07".to_string()));

    let result = check_duplicate(&path, "https://example.com/other").expect("check");
    assert!(result.is_none());

    cleanup(&path);
}

#[test]
fn test_find_completed_returns_path() {
    let path = temp_ledger_path().with_file_name("test-vault-find-completed.md");
    cleanup(&path);

    let entry = LedgerEntry {
        date: "2026-03-18".to_string(),
        time: "10:00".to_string(),
        method: Method::Cli,
        filename: Some("test-note.md".to_string()),
        source: "https://example.com/article".to_string(),
        domain: Some("ai".to_string()),
        trace_id: None,
    };
    append_entry(&path, &entry).expect("append");

    let result = find_completed(&path, "https://example.com/article").expect("find");
    assert!(result.is_some());
    let found = result.expect("should have entry");
    assert_eq!(found.date, "2026-03-18");
    assert_eq!(found.filename, "test-note.md");

    cleanup(&path);
}

#[test]
fn test_separator_detection_with_spaces() {
    // The bug: separator "| --- | --- |" (with spaces) wasn't matched by
    // starts_with("|--"), causing new rows to append at the bottom.
    let path = temp_ledger_path().with_file_name("test-vault-sep-spaces.md");
    cleanup(&path);

    // Write a ledger with Obsidian-style spaced separators
    let content = format!(
        "---\ntitle: Borg Ledger\ndate: 2026-03-23\ntype: system\ndomain: system\norigin: authored\ntags: []\n---\n\n\
             # Borg Ledger\n\n\
             | Date | Time | Method | Status | Title | Filename | Source | Domain | Trace |\n\
             | ---------- | ----- | --------- | ------ | ----- | -------- | ------ | ------ | ----- |\n\
             | 2026-03-20 | 10:00 | http | {} | [[Old Note]] | old.md | https://example.com/old | ai | tr-000001 |\n",
        "\u{2705}"
    );
    fs::write(&path, content).expect("write");

    let entry = LedgerEntry {
        date: "2026-03-23".to_string(),
        time: "14:00".to_string(),
        method: Method::Http,
        filename: Some("new-note.md".to_string()),
        source: "https://example.com/new".to_string(),
        domain: Some("ai".to_string()),
        trace_id: Some("tr-000002".to_string()),
    };
    append_entry(&path, &entry).expect("append");

    let result = fs::read_to_string(&path).expect("read");
    let data_lines: Vec<&str> = result
        .lines()
        .filter(|l| l.starts_with('|') && l.contains("example.com"))
        .collect();

    // New entry should be FIRST (top of table), not last
    assert!(
        data_lines[0].contains("[[new-note]]"),
        "newest entry should be at top, got: {}",
        data_lines[0]
    );
    assert!(
        data_lines[1].contains("Old Note"),
        "older entry should be below, got: {}",
        data_lines[1]
    );

    cleanup(&path);
}

#[test]
fn test_header_drift_repair() {
    // The bug: a previous agent removed the "Filename" column from the header,
    // causing an off-by-one where Path data showed under Source.
    let path = temp_ledger_path().with_file_name("test-vault-header-drift.md");
    cleanup(&path);

    // Write a ledger with a broken header (missing Filename column)
    let content = "\
            ---\ntitle: Borg Ledger\ndate: 2026-03-23\ntype: system\ndomain: system\norigin: authored\ntags: []\n---\n\n\
            # Borg Ledger\n\n\
            | Date | Time | Method | Status | Title | Source | Domain |   |   |\n\
            |------|------|--------|--------|-------|--------|--------|---|---|\n";
    fs::write(&path, content).expect("write");

    // Appending should trigger header repair
    let entry = LedgerEntry {
        date: "2026-03-23".to_string(),
        time: "14:00".to_string(),
        method: Method::Http,
        filename: Some("test.md".to_string()),
        source: "https://example.com".to_string(),
        domain: Some("ai".to_string()),
        trace_id: None,
    };
    append_entry(&path, &entry).expect("append");

    let result = fs::read_to_string(&path).expect("read");
    assert!(
        result.contains("| Note |"),
        "header should be repaired to canonical 8-column layout with Note column"
    );
    assert!(
        !result.contains("| Source | Domain |   |"),
        "broken header should be gone"
    );

    cleanup(&path);
}

#[test]
fn test_filename_stripping_in_append() {
    // Verify that even if LedgerEntry.filename contains a path prefix,
    // the written row only has the bare filename.
    let path = temp_ledger_path().with_file_name("test-vault-filename-strip.md");
    cleanup(&path);
    ensure_ledger_exists(&path).expect("create");

    let entry = LedgerEntry {
        date: "2026-03-23".to_string(),
        time: "14:00".to_string(),
        method: Method::Http,
        filename: Some("inbox/should-strip-this.md".to_string()),
        source: "https://example.com/strip".to_string(),
        domain: Some("ai".to_string()),
        trace_id: None,
    };
    append_entry(&path, &entry).expect("append");

    let result = fs::read_to_string(&path).expect("read");
    let data_line = result
        .lines()
        .find(|l| l.contains("should-strip-this"))
        .expect("should find data row");

    assert!(
        !data_line.contains("inbox/"),
        "path prefix should be stripped, got: {data_line}"
    );
    assert!(
        data_line.contains("[[should-strip-this]]"),
        "Note cell should be the bare slug as a wikilink, got: {data_line}"
    );
    assert!(
        !data_line.contains("should-strip-this.md"),
        ".md extension should not appear in the Note cell, got: {data_line}"
    );

    cleanup(&path);
}

#[test]
fn test_append_newest_first_multiple_entries() {
    // End-to-end: append 3 entries in chronological order,
    // verify they appear newest-first in the file.
    let path = temp_ledger_path().with_file_name("test-vault-ordering.md");
    cleanup(&path);
    ensure_ledger_exists(&path).expect("create");

    let dates = [
        ("2026-03-20", "08:00", "First"),
        ("2026-03-21", "09:00", "Second"),
        ("2026-03-22", "10:00", "Third"),
    ];

    for (date, time, title) in &dates {
        append_entry(
            &path,
            &LedgerEntry {
                date: date.to_string(),
                time: time.to_string(),
                method: Method::Http,
                filename: Some(format!("{}.md", title.to_lowercase())),
                source: format!("https://example.com/{}", title.to_lowercase()),
                domain: Some("ai".to_string()),
                trace_id: None,
            },
        )
        .expect("append");
    }

    let result = fs::read_to_string(&path).expect("read");
    let data_lines: Vec<&str> = result
        .lines()
        .filter(|l| l.starts_with('|') && l.contains("example.com"))
        .collect();

    assert_eq!(data_lines.len(), 3);
    assert!(
        data_lines[0].contains("[[third]]"),
        "newest should be first, got: {}",
        data_lines[0]
    );
    assert!(
        data_lines[1].contains("[[second]]"),
        "middle should be second, got: {}",
        data_lines[1]
    );
    assert!(
        data_lines[2].contains("[[first]]"),
        "oldest should be last, got: {}",
        data_lines[2]
    );

    cleanup(&path);
}
