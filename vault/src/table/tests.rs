use super::*;

const HEADER: &str = "| Date | Time | Method | Origin | Kind | Preview | Trace |";
const SEPARATOR: &str = "|------|------|--------|--------|------|---------|-------|";

fn fixture_with_rows(rows: &[&str]) -> String {
    let mut s = String::from("---\ntitle: Test\n---\n\n# Test\n\n");
    s.push_str(HEADER);
    s.push('\n');
    s.push_str(SEPARATOR);
    s.push('\n');
    for r in rows {
        s.push_str(r);
        s.push('\n');
    }
    s
}

#[test]
fn parses_rows_by_column_name() {
    let content = fixture_with_rows(&[
        "| 2026-05-11 | 19:07 | telegram | chat-1 | url | https://x | tg-aaaaaa |",
        "| 2026-05-11 | 19:09 | http | 192.168.0.42 | text | hello world | ht-bbbbbb |",
    ]);
    let parsed = parse_table(
        &content,
        &["Date", "Time", "Method", "Origin", "Kind", "Preview", "Trace"],
    )
    .expect("parse");
    assert_eq!(parsed.rows.len(), 2);
    assert_eq!(parsed.rows[0].get("Trace"), Some("tg-aaaaaa"));
    assert_eq!(parsed.rows[0].get("Kind"), Some("url"));
    assert_eq!(parsed.rows[1].get("Method"), Some("http"));
    assert_eq!(parsed.rows[1].get("Origin"), Some("192.168.0.42"));
}

#[test]
fn parse_table_is_insensitive_to_column_order() {
    // Same data, columns in DIFFERENT order from canonical. A positional
    // parser would silently misalign every cell; the name-based parser
    // returns the correct values for the requested column names.
    let permuted_header = "| Trace | Date | Kind | Method | Preview | Time | Origin |";
    let permuted_sep = "|-------|------|------|--------|---------|------|--------|";
    let row = "| tg-aaaaaa | 2026-05-11 | url | telegram | https://x | 19:07 | chat-1 |";

    let content = format!("---\ntitle: T\n---\n\n# T\n\n{permuted_header}\n{permuted_sep}\n{row}\n");
    let parsed = parse_table(&content, &["Date", "Trace", "Kind"]).expect("parse");
    assert_eq!(parsed.rows[0].get("Trace"), Some("tg-aaaaaa"));
    assert_eq!(parsed.rows[0].get("Date"), Some("2026-05-11"));
    assert_eq!(parsed.rows[0].get("Kind"), Some("url"));
    assert_eq!(parsed.rows[0].get("Method"), Some("telegram"));
}

#[test]
fn parse_table_fails_on_missing_required_column() {
    let content = fixture_with_rows(&[]);
    let err = parse_table(&content, &["Date", "DoesNotExist"]).expect_err("should error");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("DoesNotExist"),
        "expected missing-column error, got: {msg}"
    );
}

#[test]
fn parse_table_skips_separator_and_empty_lines() {
    let content = fixture_with_rows(&["| 2026-05-11 | 19:07 | telegram | chat-1 | url | https://x | tg-aaaaaa |"]);
    let parsed = parse_table(&content, &["Date"]).expect("parse");
    assert_eq!(parsed.rows.len(), 1);
}

#[test]
fn parse_table_handles_escaped_pipes() {
    let content = fixture_with_rows(&["| 2026-05-11 | 19:07 | telegram | chat-1 | text | a \\| b \\| c | tg-aaaaaa |"]);
    let parsed = parse_table(&content, &["Preview"]).expect("parse");
    assert_eq!(parsed.rows[0].get("Preview"), Some("a | b | c"));
}

#[test]
fn format_row_escapes_pipes_and_newlines() {
    let row = format_row(&[("A", "with | pipe"), ("B", "with\nnewline")]);
    assert!(row.contains("with \\| pipe"));
    assert!(row.contains("with newline"));
    assert!(!row.contains('\n'));
}

#[test]
fn insert_after_separator_places_at_top_of_data() {
    let content = fixture_with_rows(&["| 2026-05-11 | 19:07 | telegram | chat-1 | url | https://x | tg-aaaaaa |"]);
    let new_row = "| 2026-05-12 | 09:00 | http | 1.2.3.4 | text | hi | ht-cccccc |";
    let updated = insert_after_separator(&content, new_row);
    let data_lines: Vec<&str> = updated
        .lines()
        .filter(|l| l.contains("tg-") || l.contains("ht-"))
        .collect();
    assert_eq!(data_lines[0], new_row);
    assert!(data_lines[1].contains("tg-aaaaaa"));
}

#[test]
fn insert_after_separator_handles_no_separator_gracefully() {
    let updated = insert_after_separator("no table here\n", "| a | b |");
    assert!(updated.contains("| a | b |"));
}

#[test]
fn ensure_header_matches_repairs_drifted_header() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("test.md");
    let drifted_header = "| Date | Time | Method | Foo | Kind | Preview | Trace |";
    let content = format!("---\ntitle: T\n---\n\n# T\n\n{drifted_header}\n{SEPARATOR}\n");
    fs::write(&path, &content).expect("write");

    ensure_header_matches(&path, HEADER, SEPARATOR, "Test").expect("repair");

    let after = fs::read_to_string(&path).expect("read");
    assert!(after.contains(HEADER));
    assert!(!after.contains("Foo"));
}

#[test]
fn ensure_header_matches_no_op_when_canonical() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("test.md");
    let content = format!("---\ntitle: T\n---\n\n# T\n\n{HEADER}\n{SEPARATOR}\n");
    fs::write(&path, &content).expect("write");
    let before = fs::read_to_string(&path).expect("read");

    ensure_header_matches(&path, HEADER, SEPARATOR, "Test").expect("noop");

    let after = fs::read_to_string(&path).expect("read");
    assert_eq!(before, after);
}
