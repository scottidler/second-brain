use super::*;

/// One transcript line carrying a `knowledge_search` tool call for `query`.
fn search_line(query: &str) -> String {
    serde_json::json!({
        "type": "assistant",
        "message": {"role": "assistant", "content": [
            {"type": "text", "text": "let me search"},
            {"type": "tool_use", "id": "t1", "name": "mcp__oracle__knowledge_search",
             "input": {"query": query, "detail": "metadata"}}
        ]}
    })
    .to_string()
}

/// Stage a live transcript for `id` under `projects/` holding one search for
/// `query`, and return its export record with `scope`.
fn session(root: &Path, id: &str, scope: Option<&str>, query: &str) -> ExportSession {
    let project = root.join("projects");
    std::fs::create_dir_all(&project).expect("mkdir");
    let transcript = project.join(format!("{id}.jsonl"));
    std::fs::write(&transcript, format!("{}\n", search_line(query))).expect("write transcript");
    ExportSession {
        session_id: id.to_string(),
        scope: scope.map(str::to_string),
        transcript_path: transcript,
        project_dir: project,
        staged_path: None,
    }
}

#[test]
fn a_work_session_and_a_session_with_no_scope_contribute_no_queries() {
    let tmp = tempfile::tempdir().expect("tmp");
    let envelope = ExportEnvelope {
        sessions: vec![
            session(tmp.path(), "p1", Some("personal"), "personal question"),
            session(tmp.path(), "w1", Some("work"), "work question"),
            session(tmp.path(), "n1", None, "unscoped question"),
            session(tmp.path(), "u1", Some("home"), "unknown-scope question"),
        ],
    };
    let mined = mine(&envelope).expect("mine");
    assert_eq!(mined.queries, vec!["personal question".to_string()]);
    assert_eq!(mined.sessions_total, 4);
    assert_eq!(mined.sessions_personal, 1);
    assert_eq!(mined.sessions_work, 1);
    assert_eq!(mined.sessions_unknown_scope, 2);
    assert_eq!(mined.calls, 1);
}

#[test]
fn the_scope_key_absent_from_the_export_json_drops_the_session() {
    let tmp = tempfile::tempdir().expect("tmp");
    let s = session(tmp.path(), "n1", None, "q");
    let json = format!(
        r#"{{"schema-version":2,"sessions":[{{"session-id":"n1","transcript-path":"{}","project-dir":"{}","staged-path":null}}]}}"#,
        s.transcript_path.display(),
        s.project_dir.display()
    );
    let envelope: ExportEnvelope = serde_json::from_str(&json).expect("parse");
    assert_eq!(envelope.sessions[0].scope_verdict(), ScopeVerdict::Unknown);
    assert!(mine(&envelope).expect("mine").queries.is_empty());
}

#[test]
fn queries_dedup_case_and_whitespace_insensitively_keeping_the_first_seen() {
    let tmp = tempfile::tempdir().expect("tmp");
    let a = session(tmp.path(), "a", Some("personal"), "Rust  Async");
    let b = session(tmp.path(), "b", Some("personal"), "rust async");
    let c = session(tmp.path(), "c", Some("personal"), "24/7 agent \"securely\"");
    let mined = mine(&ExportEnvelope {
        sessions: vec![a, b, c],
    })
    .expect("mine");
    assert_eq!(
        mined.queries,
        vec!["Rust  Async".to_string(), "24/7 agent \"securely\"".to_string()]
    );
    assert_eq!(mined.calls, 3);
}

#[test]
fn subagent_transcripts_are_mined_and_a_staged_copy_stands_in_for_a_reaped_live_one() {
    let tmp = tempfile::tempdir().expect("tmp");
    let mut s = session(tmp.path(), "s1", Some("personal"), "live parent");
    let subs = s.project_dir.join("s1").join("subagents");
    std::fs::create_dir_all(&subs).expect("mkdir");
    std::fs::write(subs.join("agent-1.jsonl"), search_line("from a subagent")).expect("write");
    let mined = mine(&ExportEnvelope {
        sessions: vec![s.clone()],
    })
    .expect("mine");
    assert_eq!(
        mined.queries,
        vec!["live parent".to_string(), "from a subagent".to_string()]
    );

    let staged = tmp.path().join("staged");
    std::fs::create_dir_all(&staged).expect("mkdir");
    std::fs::write(staged.join("s1.jsonl"), search_line("from the staged copy")).expect("write");
    std::fs::remove_file(&s.transcript_path).expect("reap live");
    s.staged_path = Some(staged);
    let mined = mine(&ExportEnvelope {
        sessions: vec![s.clone()],
    })
    .expect("mine");
    assert_eq!(mined.queries, vec!["from the staged copy".to_string()]);

    s.staged_path = None;
    let mined = mine(&ExportEnvelope { sessions: vec![s] }).expect("mine");
    assert!(mined.queries.is_empty());
    assert_eq!(mined.sessions_without_transcript, 1);
}

#[test]
fn only_knowledge_search_tool_calls_with_a_query_are_read() {
    let other_tool = serde_json::json!({"message": {"content": [
        {"type": "tool_use", "name": "mcp__oracle__note_read", "input": {"query": "not a search"}}
    ]}})
    .to_string();
    let text_mention = serde_json::json!({"message": {"content": [
        {"type": "text", "text": "knowledge_search with query x"}
    ]}})
    .to_string();
    let empty = serde_json::json!({"message": {"content": [
        {"type": "tool_use", "name": "mcp__oracle__knowledge_search", "input": {"query": "  "}}
    ]}})
    .to_string();
    let text = format!(
        "{other_tool}\n{text_mention}\n{empty}\nnot json knowledge_search\n{}\n",
        search_line("  real one  ")
    );
    assert_eq!(queries_in_transcript(&text), vec!["real one".to_string()]);
}

#[test]
fn the_rendered_set_loads_as_an_eval_query_set_with_stable_ids() {
    let mined = Mined {
        queries: vec!["Alpha beta".into(), "how to run an AI coding agent 24/7".into()],
        sessions_personal: 2,
        calls: 3,
        ..Mined::default()
    };
    let yaml = render_yaml(&mined).expect("render");
    let tmp = tempfile::tempdir().expect("tmp");
    let path = tmp.path().join("q.yml");
    std::fs::write(&path, &yaml).expect("write");
    let loaded = crate::eval::Queries::load(&path).expect("load");
    assert_eq!(loaded.queries.len(), 2);
    assert_eq!(loaded.queries[1].query, "how to run an AI coding agent 24/7");
    assert_eq!(loaded.queries[0].id, format!("mined-{}", stable_hash("alpha beta")));
    assert!(
        loaded
            .queries
            .iter()
            .all(|q| q.tags.is_none() && q.calibration.is_none())
    );
}

#[test]
fn a_failing_clyde_is_an_error_naming_the_command() {
    let err = export_from_clyde("false").expect_err("non-zero exit");
    assert!(format!("{err:#}").contains("session export"), "{err:#}");
    let err = export_from_clyde("/nonexistent/clyde-binary").expect_err("missing binary");
    assert!(format!("{err:#}").contains("is clyde installed"), "{err:#}");
}
