#![allow(clippy::unwrap_used)]
use super::*;
use crate::jsonl::{ContentBlock, ParsedSession, Role, Turn};
use chrono::TimeZone;
use std::path::PathBuf;

fn turn(role: Role, blocks: Vec<ContentBlock>, seq: i64) -> Turn {
    Turn {
        uuid: format!("u{seq}"),
        parent_uuid: None,
        timestamp: chrono::Utc.timestamp_opt(1_700_000_000 + seq, 0).unwrap(),
        role,
        content: blocks,
        model: None,
    }
}

fn session(turns: Vec<Turn>) -> ParsedSession {
    ParsedSession {
        session_uuid: "sess-x".to_string(),
        jsonl_path: PathBuf::from("/tmp/x.jsonl"),
        jsonl_sha256: "abc".to_string(),
        turns,
        schema_drift_lines: 0,
        cwd: Some(PathBuf::from("/home/saidler/repos/scottidler/second-brain")),
    }
}

#[test]
fn extracts_design_doc_paths() {
    let s = session(vec![turn(
        Role::User,
        vec![ContentBlock::Text {
            text: "see docs/design/2026-05-27-glean.md and docs/design/some-other-doc.md".to_string(),
        }],
        0,
    )]);
    let docs = extract_design_doc_paths(&s, None);
    let names = docs
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
        .collect::<Vec<_>>();
    assert!(names.contains(&"2026-05-27-glean.md".to_string()));
    assert!(names.contains(&"some-other-doc.md".to_string()));
}

#[test]
fn extracts_skill_invocations() {
    let s = session(vec![
        turn(
            Role::User,
            vec![ContentBlock::Text {
                text: "/architect please".to_string(),
            }],
            0,
        ),
        turn(
            Role::User,
            vec![ContentBlock::Text {
                text: "/create-design-doc next".to_string(),
            }],
            1,
        ),
        turn(
            Role::Assistant,
            vec![ContentBlock::Text {
                text: "/should-not-count".to_string(),
            }],
            2,
        ),
    ]);
    let skills = extract_skill_invocations(&s);
    assert!(skills.contains(&"architect".to_string()));
    assert!(skills.contains(&"create-design-doc".to_string()));
    assert!(!skills.iter().any(|s| s.contains("not-count")));
}

#[test]
fn normalize_interaction_truncates_long_tool_results() {
    let big = "x".repeat(2000);
    let s = session(vec![turn(
        Role::Assistant,
        vec![ContentBlock::ToolResult {
            tool_use_id: "t".to_string(),
            content: big,
            is_error: false,
        }],
        0,
    )]);
    let norm = normalize_interaction(&s, 800);
    assert!(norm.contains("tool-result:"));
    assert!(norm.len() < 2000);
}

#[test]
fn empty_interaction_quarantines() {
    let s = session(vec![]);
    let cfg = Config::default();
    let outcome = classify(&s, &cfg).expect("classify");
    match outcome {
        ClassifyOutcome::Quarantined { reason } => {
            assert_eq!(reason, quarantine_reason::EMPTY_INTERACTION);
        }
        _ => panic!("expected quarantine"),
    }
}
