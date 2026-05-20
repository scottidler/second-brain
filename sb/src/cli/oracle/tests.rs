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
