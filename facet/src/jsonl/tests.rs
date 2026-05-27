use std::io::Write;

use super::*;

fn write(path: &std::path::Path, body: &str) {
    let mut f = std::fs::File::create(path).expect("create");
    f.write_all(body.as_bytes()).expect("write");
}

fn turn_line(uuid: &str, parent: Option<&str>, role: &str, ts: &str, text: &str) -> String {
    let parent_field = match parent {
        Some(p) => format!("\"parentUuid\":\"{p}\","),
        None => "\"parentUuid\":null,".to_string(),
    };
    let role_field = if role == "assistant" {
        "\"role\":\"assistant\",\"model\":\"claude-sonnet-4-6\""
    } else {
        "\"role\":\"user\""
    };
    format!(
        "{{\"type\":\"{role}\",\"uuid\":\"{uuid}\",{parent_field}\"timestamp\":\"{ts}\",\"sessionId\":\"sess-1\",\
         \"message\":{{{role_field},\"content\":[{{\"type\":\"text\",\"text\":\"{text}\"}}]}}}}\n"
    )
}

#[test]
fn empty_file_returns_empty_slice() {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = dir.path().join("sess-1.jsonl");
    write(&p, "");
    let slice = parse_session_file(&p, 0).expect("parse");
    assert!(slice.turns.is_empty());
    assert_eq!(slice.end_byte_offset, 0);
    assert_eq!(slice.schema_drift_lines, 0);
    assert_eq!(slice.session_uuid, "sess-1");
}

#[test]
fn single_user_turn_parsed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = dir.path().join("sess-2.jsonl");
    write(&p, &turn_line("u1", None, "user", "2026-05-26T12:00:00Z", "hello"));
    let slice = parse_session_file(&p, 0).expect("parse");
    assert_eq!(slice.turns.len(), 1);
    let t = &slice.turns[0];
    assert_eq!(t.uuid, "u1");
    assert!(matches!(t.role, Role::User));
    assert_eq!(t.content.len(), 1);
    match &t.content[0] {
        ContentBlock::Text { text } => assert_eq!(text, "hello"),
        other => panic!("wrong block: {other:?}"),
    }
    assert!(slice.end_byte_offset > 0);
}

#[test]
fn skips_non_turn_lines() {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = dir.path().join("sess-3.jsonl");
    let mut body = String::new();
    body.push_str("{\"type\":\"last-prompt\",\"leafUuid\":\"x\"}\n");
    body.push_str("{\"type\":\"permission-mode\",\"permissionMode\":\"default\"}\n");
    body.push_str("{\"type\":\"attachment\",\"uuid\":\"a1\"}\n");
    body.push_str("{\"type\":\"file-history-snapshot\"}\n");
    body.push_str("{\"type\":\"ai-title\"}\n");
    body.push_str("{\"type\":\"system\"}\n");
    body.push_str(&turn_line("u1", None, "user", "2026-05-26T12:00:00Z", "hi"));
    write(&p, &body);
    let slice = parse_session_file(&p, 0).expect("parse");
    assert_eq!(slice.turns.len(), 1);
    assert_eq!(slice.schema_drift_lines, 0);
}

#[test]
fn unknown_line_type_counted_as_drift() {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = dir.path().join("sess-4.jsonl");
    let mut body = String::new();
    body.push_str("{\"type\":\"some-new-shape\",\"x\":1}\n");
    body.push_str(&turn_line("u1", None, "user", "2026-05-26T12:00:00Z", "hi"));
    write(&p, &body);
    let slice = parse_session_file(&p, 0).expect("parse");
    assert_eq!(slice.turns.len(), 1);
    assert_eq!(slice.schema_drift_lines, 1);
}

#[test]
fn malformed_json_counted_as_drift() {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = dir.path().join("sess-5.jsonl");
    let mut body = String::new();
    body.push_str("{not valid json\n");
    body.push_str(&turn_line("u1", None, "user", "2026-05-26T12:00:00Z", "hi"));
    write(&p, &body);
    let slice = parse_session_file(&p, 0).expect("parse");
    assert_eq!(slice.turns.len(), 1);
    assert_eq!(slice.schema_drift_lines, 1);
}

#[test]
fn partial_trailing_line_deferred() {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = dir.path().join("sess-6.jsonl");
    let mut body = String::new();
    body.push_str(&turn_line("u1", None, "user", "2026-05-26T12:00:00Z", "first"));
    let after_first = body.len() as u64;
    // partial line - no terminating newline
    body.push_str("{\"type\":\"user\",\"uuid\":\"u2\"");
    write(&p, &body);
    let slice = parse_session_file(&p, 0).expect("parse");
    assert_eq!(slice.turns.len(), 1);
    assert_eq!(
        slice.end_byte_offset, after_first,
        "end offset must stop at the complete line"
    );
}

#[test]
fn mid_stream_parse_resumes_at_offset() {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = dir.path().join("sess-7.jsonl");
    let mut body = String::new();
    body.push_str(&turn_line("u1", None, "user", "2026-05-26T12:00:00Z", "first"));
    let after_first = body.len() as u64;
    body.push_str(&turn_line(
        "u2",
        Some("u1"),
        "assistant",
        "2026-05-26T12:00:01Z",
        "second",
    ));
    write(&p, &body);
    let slice = parse_session_file(&p, after_first).expect("parse");
    assert_eq!(slice.turns.len(), 1, "only the second turn should be in the slice");
    assert_eq!(slice.turns[0].uuid, "u2");
    assert!(matches!(slice.turns[0].role, Role::Assistant));
    assert_eq!(slice.turns[0].model.as_deref(), Some("claude-sonnet-4-6"));
}

#[test]
fn user_content_string_form_supported() {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = dir.path().join("sess-8.jsonl");
    let body = "{\"type\":\"user\",\"uuid\":\"u1\",\"parentUuid\":null,\"timestamp\":\"2026-05-26T12:00:00Z\",\"sessionId\":\"sess-8\",\"message\":{\"role\":\"user\",\"content\":\"plain string content\"}}\n";
    write(&p, body);
    let slice = parse_session_file(&p, 0).expect("parse");
    assert_eq!(slice.turns.len(), 1);
    match &slice.turns[0].content[0] {
        ContentBlock::Text { text } => assert_eq!(text, "plain string content"),
        other => panic!("expected Text, got {other:?}"),
    }
}

#[test]
fn tool_use_and_tool_result_blocks_parsed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = dir.path().join("sess-9.jsonl");
    let body = concat!(
        "{\"type\":\"assistant\",\"uuid\":\"a1\",\"parentUuid\":null,\"timestamp\":\"2026-05-26T12:00:00Z\",\"sessionId\":\"sess-9\",",
        "\"message\":{\"role\":\"assistant\",\"model\":\"x\",\"content\":[{\"type\":\"tool_use\",\"id\":\"t1\",\"name\":\"Read\",\"input\":{\"path\":\"a\"}}]}}\n",
        "{\"type\":\"user\",\"uuid\":\"u1\",\"parentUuid\":\"a1\",\"timestamp\":\"2026-05-26T12:00:01Z\",\"sessionId\":\"sess-9\",",
        "\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"tool_result\",\"tool_use_id\":\"t1\",\"is_error\":true,\"content\":\"oops\"}]}}\n",
    );
    write(&p, body);
    let slice = parse_session_file(&p, 0).expect("parse");
    assert_eq!(slice.turns.len(), 2);
    match &slice.turns[0].content[0] {
        ContentBlock::ToolUse { name, .. } => assert_eq!(name, "Read"),
        other => panic!("expected ToolUse, got {other:?}"),
    }
    match &slice.turns[1].content[0] {
        ContentBlock::ToolResult { content, is_error, .. } => {
            assert!(*is_error);
            assert_eq!(content, "oops");
        }
        other => panic!("expected ToolResult, got {other:?}"),
    }
}

#[test]
fn thinking_and_image_blocks_parsed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = dir.path().join("sess-10.jsonl");
    let body = concat!(
        "{\"type\":\"assistant\",\"uuid\":\"a1\",\"parentUuid\":null,\"timestamp\":\"2026-05-26T12:00:00Z\",\"sessionId\":\"sess-10\",",
        "\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"thinking\",\"thinking\":\"hmm\"}]}}\n",
        "{\"type\":\"user\",\"uuid\":\"u1\",\"parentUuid\":\"a1\",\"timestamp\":\"2026-05-26T12:00:01Z\",\"sessionId\":\"sess-10\",",
        "\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"image\",\"source\":{}}]}}\n",
    );
    write(&p, body);
    let slice = parse_session_file(&p, 0).expect("parse");
    assert_eq!(slice.turns.len(), 2);
    match &slice.turns[0].content[0] {
        ContentBlock::Thinking { text } => assert_eq!(text, "hmm"),
        other => panic!("expected Thinking, got {other:?}"),
    }
    match &slice.turns[1].content[0] {
        ContentBlock::Image { .. } => {}
        other => panic!("expected Image, got {other:?}"),
    }
}

#[test]
fn missing_timestamp_marked_drift() {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = dir.path().join("sess-11.jsonl");
    let body =
        "{\"type\":\"user\",\"uuid\":\"u1\",\"parentUuid\":null,\"message\":{\"role\":\"user\",\"content\":\"x\"}}\n";
    write(&p, body);
    let slice = parse_session_file(&p, 0).expect("parse");
    assert!(slice.turns.is_empty());
    assert_eq!(slice.schema_drift_lines, 1);
}
