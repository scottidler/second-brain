use chrono::Utc;

use super::*;
use crate::config::Config;
use crate::fabric::FakeFabric;
use crate::jsonl::{ContentBlock, ParsedSlice, Role, Turn};
use crate::ledger::Ledger;
use crate::scan::FacetSession;

fn turn(uuid: &str, role: Role, text: &str) -> Turn {
    Turn {
        uuid: uuid.to_string(),
        parent_uuid: None,
        timestamp: Utc::now(),
        role,
        content: vec![ContentBlock::Text { text: text.to_string() }],
        model: None,
    }
}

fn session(repo: &str, turns: Vec<Turn>) -> FacetSession {
    FacetSession {
        session_uuid: "sess-1".to_string(),
        cwd: std::path::PathBuf::from("/home/me/r"),
        repo_slug: Some(repo.to_string()),
        parsed: ParsedSlice {
            session_uuid: "sess-1".to_string(),
            turns: turns.clone(),
            end_byte_offset: 4096,
            schema_drift_lines: 0,
            cwd: Some(std::path::PathBuf::from("/home/me/r")),
        },
        subagent_session_uuids: vec![],
    }
}

fn config_with(model: &str) -> Config {
    Config {
        llm: crate::config::LlmConfig {
            cluster_model: model.to_string(),
            ..Default::default()
        },
        ..Default::default()
    }
}

#[tokio::test]
async fn brand_new_session_brand_new_workitem() {
    let l = Ledger::open_in_memory().expect("ledger");
    let cfg = config_with("haiku");
    let fabric = FakeFabric::new();
    fabric.set_response(
        "facet-cluster",
        "assignments:\n  - first_turn_uuid: t1\n    last_turn_uuid: t2\n    kind: new\n    title: \"Loopr v5 stage eight wiring\"\n",
    );

    let s = session(
        "me/loopr",
        vec![turn("t1", Role::User, "hi"), turn("t2", Role::Assistant, "yo")],
    );
    let out = cluster_new_turns(&s, &cfg, &l, &fabric).await.expect("cluster");
    assert_eq!(out.len(), 1);
    match &out[0].kind {
        AssignmentKind::New { title } => assert_eq!(title, "Loopr v5 stage eight wiring"),
        _ => panic!("expected new"),
    }
    let w = l
        .workitem_by_slug("loopr-v5-stage-eight-wiring")
        .expect("q")
        .expect("present");
    assert_eq!(w.title, "Loopr v5 stage eight wiring");
    assert_eq!(w.repos, vec!["me/loopr".to_string()]);
    let after = l.get_session("sess-1").expect("get").expect("present");
    assert_eq!(after.last_cluster_offset, 4096);
    let pending = l.pending_cluster_assignments(10).expect("pending");
    assert_eq!(pending.len(), 1);
}

#[tokio::test]
async fn existing_session_continuing_workitem() {
    let l = Ledger::open_in_memory().expect("ledger");
    let cfg = config_with("haiku");
    let fabric = FakeFabric::new();
    // Pre-seed an existing work-item.
    let wid = l
        .insert_workitem(crate::ledger::workitems::NewWorkItem {
            slug: "loopr-v5",
            title: "Loopr v5",
            created_at: Utc::now(),
        })
        .expect("insert");
    l.link_workitem_repo(wid, "me/loopr").expect("link");

    fabric.set_response(
        "facet-cluster",
        "assignments:\n  - first_turn_uuid: t1\n    last_turn_uuid: t1\n    kind: existing\n    slug: loopr-v5\n",
    );

    let s = session("me/loopr", vec![turn("t1", Role::User, "more loopr work")]);
    let out = cluster_new_turns(&s, &cfg, &l, &fabric).await.expect("cluster");
    assert_eq!(out.len(), 1);
    let w = l.workitem_by_slug("loopr-v5").expect("q").expect("present");
    assert_eq!(w.sessions_count, 1);
    assert_eq!(w.repos, vec!["me/loopr".to_string()]);
}

#[tokio::test]
async fn cluster_llm_failure_does_not_advance_offset() {
    let l = Ledger::open_in_memory().expect("ledger");
    let cfg = config_with("haiku");
    let fabric = FakeFabric::new();
    fabric.set_error("facet-cluster", "transient: 503");

    let s = session("me/r", vec![turn("t1", Role::User, "x")]);
    let err = cluster_new_turns(&s, &cfg, &l, &fabric).await;
    assert!(err.is_err());
    assert!(l.get_session("sess-1").expect("get").is_none(), "no writes on failure");
}

#[tokio::test]
async fn two_workitems_emerge_from_one_session() {
    let l = Ledger::open_in_memory().expect("ledger");
    let cfg = config_with("haiku");
    let fabric = FakeFabric::new();
    fabric.set_response(
        "facet-cluster",
        "assignments:\n  - first_turn_uuid: t1\n    last_turn_uuid: t2\n    kind: new\n    title: \"first thing\"\n  - first_turn_uuid: t3\n    last_turn_uuid: t4\n    kind: new\n    title: \"second thing\"\n",
    );

    let s = session(
        "me/r",
        vec![
            turn("t1", Role::User, "a"),
            turn("t2", Role::Assistant, "b"),
            turn("t3", Role::User, "c"),
            turn("t4", Role::Assistant, "d"),
        ],
    );
    let out = cluster_new_turns(&s, &cfg, &l, &fabric).await.expect("cluster");
    assert_eq!(out.len(), 2);
    let first = l.workitem_by_slug("first-thing").expect("q").expect("present");
    let second = l.workitem_by_slug("second-thing").expect("q").expect("present");
    assert_ne!(first.id, second.id);
}

#[tokio::test]
async fn duplicate_slug_auto_suffixes() {
    let l = Ledger::open_in_memory().expect("ledger");
    let cfg = config_with("haiku");
    let fabric = FakeFabric::new();
    // Pre-seed a work-item with the slug the LLM is about to coin.
    l.insert_workitem(crate::ledger::workitems::NewWorkItem {
        slug: "shared",
        title: "Existing shared",
        created_at: Utc::now(),
    })
    .expect("seed");

    fabric.set_response(
        "facet-cluster",
        "assignments:\n  - first_turn_uuid: t1\n    last_turn_uuid: t1\n    kind: new\n    title: \"shared\"\n",
    );

    let s = session("me/r", vec![turn("t1", Role::User, "x")]);
    let _ = cluster_new_turns(&s, &cfg, &l, &fabric).await.expect("cluster");
    // The new one must have been suffixed.
    assert!(l.workitem_by_slug("shared-2").expect("q").is_some());
}

#[tokio::test]
async fn cluster_persist_is_one_transaction() {
    // Architect round-1 finding: the per-session persist must be
    // atomic. If the LLM returns garbage that causes a downstream
    // violation, none of the prior writes should survive.
    let l = Ledger::open_in_memory().expect("ledger");
    let cfg = config_with("haiku");
    let fabric = FakeFabric::new();
    // Two assignments; the second one references the same
    // (first_turn_uuid, last_turn_uuid) as a pre-seeded `cluster_assignments`
    // row but for a DIFFERENT workitem. The UNIQUE (session_uuid,
    // first_turn_uuid, last_turn_uuid) constraint is on session+range,
    // not workitem, so this would not cause a violation. Instead we
    // simulate failure by passing duplicate slugs that the LLM should
    // not produce — auto-suffix handles it. Cleaner test:
    // Force a failure by exhausting the slug suffix budget.
    // Generate 60 assignments all asking for slug "x" — past the 50 retry cap.
    let mut yaml = String::from("assignments:\n");
    for i in 0..60 {
        yaml.push_str(&format!(
            "  - first_turn_uuid: t{i}\n    last_turn_uuid: t{i}\n    kind: new\n    title: \"x\"\n"
        ));
    }
    fabric.set_response("facet-cluster", yaml);
    let turns: Vec<crate::jsonl::Turn> = (0..60).map(|i| turn(&format!("t{i}"), Role::User, "x")).collect();
    let s = session("me/r", turns);
    let res = cluster_new_turns(&s, &cfg, &l, &fabric).await;
    assert!(res.is_err(), "expected slug exhaustion failure");
    // The whole transaction should have rolled back: no work-items, no
    // cluster_assignments, no session_workitem rows. The session row may
    // still be absent too because tx_upsert_session is inside the same tx.
    let pending = l.pending_cluster_assignments(100).expect("pending");
    assert!(pending.is_empty(), "no cluster_assignments rows after rollback");
    let s_row = l.get_session("sess-1").expect("get");
    assert!(s_row.is_none(), "no sessions row after rollback");
    let wi = l.workitem_by_slug("x").expect("query");
    assert!(wi.is_none(), "no work_items row after rollback");
}

#[tokio::test]
async fn retry_after_transient_failure_succeeds() {
    let l = Ledger::open_in_memory().expect("ledger");
    let cfg = config_with("haiku");
    let fabric = FakeFabric::new();
    fabric.set_error("facet-cluster", "503 transient");

    let s = session("me/r", vec![turn("t1", Role::User, "x")]);
    assert!(cluster_new_turns(&s, &cfg, &l, &fabric).await.is_err());
    fabric.set_response(
        "facet-cluster",
        "assignments:\n  - first_turn_uuid: t1\n    last_turn_uuid: t1\n    kind: new\n    title: x\n",
    );
    let out = cluster_new_turns(&s, &cfg, &l, &fabric).await.expect("cluster");
    assert_eq!(out.len(), 1);
    // First retry created one work-item; nothing from the failed run.
    let pending = l.pending_cluster_assignments(10).expect("pending");
    assert_eq!(pending.len(), 1);
}
