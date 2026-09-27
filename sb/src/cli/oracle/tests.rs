#![allow(clippy::unwrap_used)]

use super::*;
use rmcp::model::{CallToolResult, Content};
use serde_json::json;

#[test]
fn outcome_is_failure_protocol_error() {
    let result = CallToolResult::error(vec![Content::text("Provide either 'content' or 'path'")]);
    assert!(outcome_is_failure(&result));
}

#[test]
fn outcome_is_failure_application_not_found() {
    let result = CallToolResult::success(vec![
        Content::json(json!({
            "found": false,
            "kind": "note",
            "path": "notes/missing.md",
            "message": "Note not found",
        }))
        .unwrap(),
    ]);
    assert!(outcome_is_failure(&result));
}

#[test]
fn outcome_is_failure_plain_success_is_not_failure() {
    let result = CallToolResult::success(vec![
        Content::json(json!({
            "count": 3,
            "results": [{"path": "a.md"}, {"path": "b.md"}, {"path": "c.md"}],
        }))
        .unwrap(),
    ]);
    assert!(!outcome_is_failure(&result));
}

#[test]
fn outcome_is_failure_found_true_is_not_failure() {
    let result = CallToolResult::success(vec![
        Content::json(json!({
            "found": true,
            "path": "notes/exists.md",
        }))
        .unwrap(),
    ]);
    assert!(!outcome_is_failure(&result));
}

#[test]
fn outcome_is_failure_text_content_without_json_is_not_failure() {
    let result = CallToolResult::success(vec![Content::text("just some prose with no found key")]);
    assert!(!outcome_is_failure(&result));
}

#[test]
fn outcome_is_failure_empty_content_is_not_failure() {
    let result = CallToolResult::success(vec![]);
    assert!(!outcome_is_failure(&result));
}

#[test]
fn wrap_breaks_on_word_boundaries_within_width() {
    let lines = wrap("the quick brown fox jumps", 10);
    assert_eq!(lines, vec!["the quick", "brown fox", "jumps"]);
    assert!(lines.iter().all(|l| l.len() <= 10));
}

#[test]
fn wrap_overflows_word_longer_than_width_onto_its_own_line() {
    let lines = wrap("a supercalifragilistic b", 8);
    assert_eq!(lines, vec!["a", "supercalifragilistic", "b"]);
}

#[test]
fn wrap_empty_text_yields_no_lines() {
    assert!(wrap("", 40).is_empty());
}

#[test]
fn pad_display_pads_by_display_width_not_char_count() {
    // "日本" is 2 chars but 4 terminal columns.
    assert_eq!(pad_display("日本", 6), "日本  ");
    // "e" + combining acute is 2 chars but 1 column.
    assert_eq!(pad_display("e\u{301}", 3), "e\u{301}  ");
    assert_eq!(pad_display("ai", 5), "ai   ");
}

#[test]
fn pad_display_never_truncates_a_name_wider_than_the_column() {
    assert_eq!(pad_display("software-engineering", 15), "software-engineering");
}

#[test]
fn col_width_is_the_longest_name_when_it_exceeds_the_floor() {
    let names = vec![
        ("ai".to_string(), 1169),
        ("software-engineering".to_string(), 205),
        ("rust".to_string(), 250),
    ];
    assert_eq!(col_width(&names, 15), "software-engineering".len());
}

#[test]
fn col_width_falls_back_to_the_floor_for_short_or_empty_sections() {
    let names = vec![("ai".to_string(), 1), ("llm".to_string(), 2)];
    assert_eq!(col_width(&names, 15), 15);
    assert_eq!(col_width(&[], 10), 10);
}

#[test]
fn col_width_counts_display_columns_for_wide_names() {
    let names = vec![("日本語タグ".to_string(), 1)];
    assert_eq!(col_width(&names, 0), 10);
}
