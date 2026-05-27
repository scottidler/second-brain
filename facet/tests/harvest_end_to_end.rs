//! End-to-end integration test for `facet::daemon::harvest::run_with_fabric`.
//!
//! Lays down a synthetic `~/.claude/projects/<encoded-cwd>/<sid>.jsonl`
//! tree under a tempdir, runs harvest once with `FakeFabric` canned
//! responses, asserts vault notes appear with the expected fencepost
//! structure, and asserts re-run is idempotent (no new fabric calls,
//! same vault bytes).

use chrono::Utc;
use std::fs;
use std::io::Write;
use std::path::Path;

use facet::Ledger;
use facet::config::{Config, LlmConfig};
use facet::fabric::FakeFabric;

fn write_jsonl(path: &Path, body: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("mkdir");
    }
    let mut f = fs::File::create(path).expect("create");
    f.write_all(body.as_bytes()).expect("write");
}

fn turn(uuid: &str, parent: Option<&str>, role: &str, ts: &str, text: &str, session: &str, cwd: &str) -> String {
    let parent_field = match parent {
        Some(p) => format!("\"parentUuid\":\"{p}\","),
        None => "\"parentUuid\":null,".to_string(),
    };
    let role_field = if role == "assistant" {
        "\"role\":\"assistant\",\"model\":\"sonnet\""
    } else {
        "\"role\":\"user\""
    };
    format!(
        "{{\"type\":\"{role}\",\"uuid\":\"{uuid}\",{parent_field}\"timestamp\":\"{ts}\",\"cwd\":\"{cwd}\",\"sessionId\":\"{session}\",\
         \"message\":{{{role_field},\"content\":[{{\"type\":\"text\",\"text\":\"{text}\"}}]}}}}\n"
    )
}

#[tokio::test]
async fn harvest_e2e_produces_workitem_note_then_is_idempotent() {
    let dir = tempfile::tempdir().expect("tempdir");
    let projects = dir.path().join("projects");
    let vault_root = dir.path().join("vault");
    fs::create_dir_all(&vault_root).expect("mk vault");

    let cwd = "/home/me/scottidler/loopr";
    let proj = projects.join("-home-me-scottidler-loopr");
    fs::create_dir_all(&proj).expect("mk proj");
    let session_id = "11111111-aaaa-4222-8333-cccccccccccc";
    let jsonl = proj.join(format!("{session_id}.jsonl"));
    let now = Utc::now().to_rfc3339();
    let mut body = String::new();
    body.push_str(&turn("t1", None, "user", &now, "actually no, do this", session_id, cwd));
    body.push_str(&turn("t2", Some("t1"), "assistant", &now, "ok i will", session_id, cwd));
    write_jsonl(&jsonl, &body);

    let cfg = Config {
        claude_projects_root: projects.clone(),
        include_cwds: vec![],
        exclude_cwds: vec![],
        llm: LlmConfig {
            cluster_model: "haiku".to_string(),
            extract_model: "sonnet".to_string(),
            spectra_model: "opus".to_string(),
            ..Default::default()
        },
        ..Default::default()
    };
    let ledger = Ledger::open(dir.path().join("ledger.db")).expect("ledger");
    let fabric = FakeFabric::new();
    fabric.set_response(
        "facet-cluster",
        "assignments:\n  - first_turn_uuid: t1\n    last_turn_uuid: t2\n    kind: new\n    title: \"Loopr stage eight\"\n",
    );
    fabric.set_response(
        "facet-extract",
        r#"{"moments": [
  {"turn_uuid": "t1", "mode": "reject", "ai_move": "AI proposed something", "scott_move": "rejected and redirected", "quote_excerpt": "actually no, do this", "why_it_matters": "redirecting the AI mid-task"}
]}"#,
    );

    let report = facet::daemon::harvest::run_with_fabric(&cfg, &ledger, &vault_root, &fabric)
        .await
        .expect("harvest");
    assert_eq!(report.sessions_seen, 1);
    assert_eq!(report.cluster_assignments_created, 1);
    assert_eq!(report.moments_extracted, 1);
    assert_eq!(report.workitems_rendered, 1);
    assert_eq!(report.failures, 0);

    let note_path = vault_root
        .join("notes")
        .join("facet")
        .join("prisms")
        .join("loopr-stage-eight.md");
    let body = fs::read_to_string(&note_path).expect("read note");
    assert!(body.contains("<!-- facet:auto:begin frontmatter -->"));
    assert!(body.contains("type: facet-workitem"));
    assert!(body.contains("facet-slug: loopr-stage-eight"));
    assert!(body.contains("## Reject"));
    assert!(body.contains("actually no, do this"));
    assert!(body.contains("redirecting the AI mid-task"));

    // Second tick: no new turns, no new LLM calls, vault note byte-identical.
    let calls_before = fabric.calls().len();
    let report2 = facet::daemon::harvest::run_with_fabric(&cfg, &ledger, &vault_root, &fabric)
        .await
        .expect("second harvest");
    assert_eq!(report2.sessions_seen, 0, "no new turns -> no sessions");
    assert_eq!(report2.cluster_assignments_created, 0);
    assert_eq!(report2.moments_extracted, 0);
    let calls_after = fabric.calls().len();
    assert_eq!(calls_before, calls_after, "second tick must not call the LLM");
    let body2 = fs::read_to_string(&note_path).expect("read note");
    assert_eq!(body, body2);
}
