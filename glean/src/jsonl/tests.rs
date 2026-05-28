#![allow(clippy::unwrap_used)]
use super::*;

fn fixture_jsonl() -> String {
    [
        r#"{"type":"user","uuid":"u1","parentUuid":null,"sessionId":"sess-a","cwd":"/home/saidler/repos/scottidler/second-brain","timestamp":"2026-05-27T10:00:00Z","message":{"role":"user","content":"hello"}}"#,
        r#"{"type":"assistant","uuid":"a1","parentUuid":"u1","sessionId":"sess-a","cwd":"/home/saidler/repos/scottidler/second-brain","timestamp":"2026-05-27T10:00:01Z","message":{"role":"assistant","model":"claude-opus-4-7","content":[{"type":"text","text":"hi"}]}}"#,
        r#"{"type":"last-prompt","data":"ignored"}"#,
        r#"{"type":"user","uuid":"u2","parentUuid":"a1","sessionId":"sess-a","cwd":"/home/saidler/repos/scottidler/second-brain","timestamp":"2026-05-27T10:00:02Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"tool-1","content":"ok","is_error":false}]}}"#,
    ]
    .join("\n")
}

fn parse_str(s: &str) -> ParsedSession {
    let path = PathBuf::from("/tmp/fake.jsonl");
    let reader = BufReader::new(s.as_bytes());
    parse_session_reader(reader, path, "sha-stub".to_string()).expect("parse")
}

#[test]
fn parses_user_and_assistant_turns_in_order() {
    let p = parse_str(&fixture_jsonl());
    assert_eq!(p.session_uuid, "sess-a");
    assert_eq!(p.turns.len(), 3);
    assert!(matches!(p.turns[0].role, Role::User));
    assert!(matches!(p.turns[1].role, Role::Assistant));
    assert!(matches!(p.turns[2].role, Role::User));
    assert_eq!(
        p.cwd,
        Some(PathBuf::from("/home/saidler/repos/scottidler/second-brain"))
    );
}

#[test]
fn skips_known_non_turn_types() {
    let p = parse_str(&fixture_jsonl());
    assert_eq!(p.schema_drift_lines, 0);
}

#[test]
fn counts_unknown_line_types_as_drift() {
    let s = [
        r#"{"type":"strange-line"}"#,
        r#"{"type":"user","uuid":"u1","sessionId":"s1","timestamp":"2026-05-27T10:00:00Z","message":{"content":"x"}}"#,
    ]
    .join("\n");
    let p = parse_str(&s);
    assert_eq!(p.schema_drift_lines, 1);
    assert_eq!(p.turns.len(), 1);
}

#[test]
fn handles_text_string_content() {
    let s = r#"{"type":"user","uuid":"u","sessionId":"s","timestamp":"2026-05-27T10:00:00Z","message":{"content":"plain text"}}"#;
    let p = parse_str(s);
    assert_eq!(p.turns.len(), 1);
    assert!(matches!(p.turns[0].content[0], ContentBlock::Text { .. }));
}

#[test]
fn malformed_json_lines_become_schema_drift() {
    let s = "{not valid json\n";
    let p = parse_str(s);
    assert_eq!(p.schema_drift_lines, 1);
    // No turns means session_uuid would fall back to path stem.
    assert_eq!(p.session_uuid, "fake");
}

#[test]
fn file_sha256_is_deterministic_for_real_file() {
    let tmp = tempfile::NamedTempFile::new().expect("tempfile");
    std::fs::write(tmp.path(), b"abc\n").expect("write");
    let h1 = file_sha256(tmp.path()).expect("hash");
    let h2 = file_sha256(tmp.path()).expect("hash");
    assert_eq!(h1, h2);
    assert_eq!(h1.len(), 64);
}
