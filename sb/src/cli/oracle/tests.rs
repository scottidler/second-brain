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
fn outcome_is_failure_domain_not_found() {
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
