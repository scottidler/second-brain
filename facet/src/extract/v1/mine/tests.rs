use chrono::Utc;

use super::*;
use crate::config::Config;
use crate::fabric::FakeFabric;
use crate::jsonl::{ContentBlock, Role, Turn};
use crate::ledger::Ledger;
use crate::ledger::clusters::{ClusterAssignmentRow, NewClusterAssignment};
use crate::ledger::sessions::UpsertSession;
use crate::ledger::workitems::{NewWorkItem, SessionContribution};

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

fn seed_ledger_with_assignment(l: &Ledger) -> (ClusterAssignmentRow, String) {
    let now = Utc::now();
    l.upsert_session(UpsertSession {
        session_uuid: "sess-1",
        cwd: "/home/me/r",
        repo_slug: Some("me/r"),
        seen_at: now,
    })
    .expect("upsert");
    let wid = l
        .insert_workitem(NewWorkItem {
            slug: "the-thing",
            title: "The thing",
            created_at: now,
        })
        .expect("insert");
    l.record_contribution(SessionContribution {
        session_uuid: "sess-1",
        workitem_id: wid,
        at: now,
    })
    .expect("contrib");
    let _cid = l
        .insert_cluster_assignment(NewClusterAssignment {
            session_uuid: "sess-1",
            workitem_id: wid,
            first_turn_uuid: "t1",
            last_turn_uuid: "t3",
            clustered_at: now,
            cluster_model: "haiku",
        })
        .expect("ca");
    let pending = l.pending_cluster_assignments(10).expect("pending");
    (pending.into_iter().next().expect("one"), "the-thing".to_string())
}

#[tokio::test]
async fn each_scaffolding_mode_persists() {
    let l = Ledger::open_in_memory().expect("ledger");
    let (assignment, slug) = seed_ledger_with_assignment(&l);
    let cfg = Config::default();
    let fabric = FakeFabric::new();
    fabric.set_response(
        "facet-extract",
        r#"{"moments": [
  {"turn_uuid": "t1", "mode": "frame", "ai_move": "AI proposed X", "scott_move": "reframed to Y", "quote_excerpt": "actually it's not X, it's Y", "why_it_matters": "naming the framing matters"},
  {"turn_uuid": "t2", "mode": "reject", "ai_move": "AI suggested a bad name", "scott_move": "rejected and renamed", "quote_excerpt": "no - call it foo", "why_it_matters": "naming sets taste"}
]}"#,
    );
    let turns = vec![
        turn("t1", Role::User, "actually it's not X, it's Y"),
        turn("t2", Role::User, "no - call it foo"),
        turn("t3", Role::Assistant, "ok"),
    ];
    let out = mine_moments(&assignment, &turns, &slug, "The thing", Some("me/r"), &cfg, &l, &fabric)
        .await
        .expect("mine");
    assert_eq!(out.len(), 2);
    let stored = l.moments_for_workitem(assignment.workitem_id).expect("query");
    assert_eq!(stored.len(), 2);
    let modes: Vec<&str> = stored.iter().map(|m| m.mode.as_str()).collect();
    assert!(modes.contains(&"frame"));
    assert!(modes.contains(&"reject"));
    let pending = l.pending_cluster_assignments(10).expect("pending");
    assert!(pending.is_empty(), "row should be extracted=1");
}

#[tokio::test]
async fn open_vocabulary_mode_accepted() {
    let l = Ledger::open_in_memory().expect("ledger");
    let (assignment, slug) = seed_ledger_with_assignment(&l);
    let cfg = Config::default();
    let fabric = FakeFabric::new();
    fabric.set_response(
        "facet-extract",
        r#"{"moments": [
  {"turn_uuid": "t1", "mode": "re-scope", "ai_move": "AI offered the wrong slice", "scott_move": "narrowed the scope", "quote_excerpt": "just do the first half", "why_it_matters": "scope control"}
]}"#,
    );
    let turns = vec![turn("t1", Role::User, "just do the first half")];
    let out = mine_moments(&assignment, &turns, &slug, "The thing", Some("me/r"), &cfg, &l, &fabric)
        .await
        .expect("mine");
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].mode, "re-scope");
}

#[tokio::test]
async fn extract_failure_leaves_ledger_clean() {
    let l = Ledger::open_in_memory().expect("ledger");
    let (assignment, slug) = seed_ledger_with_assignment(&l);
    let cfg = Config::default();
    let fabric = FakeFabric::new();
    fabric.set_error("facet-extract", "transient: 503");
    let turns = vec![turn("t1", Role::User, "x")];
    let err = mine_moments(&assignment, &turns, &slug, "The thing", Some("me/r"), &cfg, &l, &fabric).await;
    assert!(err.is_err());
    let pending = l.pending_cluster_assignments(10).expect("pending");
    assert_eq!(pending.len(), 1, "still pending after failure");
    let stored = l.moments_for_workitem(assignment.workitem_id).expect("query");
    assert!(stored.is_empty());
}

#[tokio::test]
async fn empty_moments_list_is_valid_outcome() {
    let l = Ledger::open_in_memory().expect("ledger");
    let (assignment, slug) = seed_ledger_with_assignment(&l);
    let cfg = Config::default();
    let fabric = FakeFabric::new();
    fabric.set_response("facet-extract", r#"{"moments": []}"#);
    let turns = vec![turn("t1", Role::User, "thanks")];
    let out = mine_moments(&assignment, &turns, &slug, "The thing", Some("me/r"), &cfg, &l, &fabric)
        .await
        .expect("mine");
    assert!(out.is_empty());
    let pending = l.pending_cluster_assignments(10).expect("pending");
    assert!(pending.is_empty(), "row should still flip to extracted=1");
}

#[tokio::test]
async fn oversize_range_splits_into_multiple_extract_calls() {
    // Architect round-1 finding: `extract.max_input_tokens` was config
    // theatre — the field existed but the splitter did not. Verify
    // that a low cap forces multiple LLM calls and idempotent merge
    // of the resulting moments.
    let l = Ledger::open_in_memory().expect("ledger");
    let (assignment, slug) = seed_ledger_with_assignment(&l);
    let cfg = Config {
        extract: crate::config::ExtractConfig {
            quote_max_chars: 800,
            // ~2_000-char budget after the .max() floor — small enough to
            // split a slice of 4 turns into multiple chunks.
            max_input_tokens: 200,
        },
        ..Default::default()
    };
    let fabric = FakeFabric::new();
    // Same response for every call; idempotency on
    // (workitem_id, turn_uuid, mode) collapses duplicates.
    fabric.set_response(
        "facet-extract",
        r#"{"moments": [
  {"turn_uuid": "t1", "mode": "frame", "ai_move": "x", "scott_move": "y", "quote_excerpt": "some quote", "why_it_matters": "z"}
]}"#,
    );

    // Build a slice large enough that even the .max(2_000) floor splits it.
    // Each turn carries ~600 chars of text so 8 turns blow past the cap.
    let long_text = "x".repeat(600);
    let turns: Vec<crate::jsonl::Turn> = (0..8)
        .map(|i| {
            let mut t = turn(&format!("t{i}"), Role::User, &long_text);
            t.uuid = format!("t{i}");
            t
        })
        .collect();
    mine_moments(&assignment, &turns, &slug, "The thing", Some("me/r"), &cfg, &l, &fabric)
        .await
        .expect("mine");
    let calls = fabric.calls();
    assert!(
        calls.len() >= 2,
        "expected splitter to issue >= 2 extract calls, got {}",
        calls.len()
    );
    // Idempotency: only one row in judgment_moments even though every chunk
    // returned the same turn_uuid+mode pair.
    let stored = l.moments_for_workitem(assignment.workitem_id).expect("query");
    assert_eq!(stored.len(), 1);
}

#[tokio::test]
async fn quote_excerpt_capped_at_config_chars() {
    let l = Ledger::open_in_memory().expect("ledger");
    let (assignment, slug) = seed_ledger_with_assignment(&l);
    let cfg = Config {
        extract: crate::config::ExtractConfig {
            quote_max_chars: 20,
            max_input_tokens: 60_000,
        },
        ..Default::default()
    };
    let long = "a".repeat(200);
    let fabric = FakeFabric::new();
    fabric.set_response(
        "facet-extract",
        format!(
            r#"{{"moments": [
  {{"turn_uuid": "t1", "mode": "frame", "ai_move": "x", "scott_move": "y", "quote_excerpt": "{long}", "why_it_matters": "z"}}
]}}"#
        ),
    );
    let turns = vec![turn("t1", Role::User, &long)];
    mine_moments(&assignment, &turns, &slug, "The thing", Some("me/r"), &cfg, &l, &fabric)
        .await
        .expect("mine");
    let stored = l.moments_for_workitem(assignment.workitem_id).expect("query");
    assert_eq!(stored.len(), 1);
    assert!(
        stored[0].quote_excerpt.ends_with('…'),
        "expected truncation marker: {}",
        stored[0].quote_excerpt
    );
    assert!(
        stored[0].quote_excerpt.chars().count() <= 21,
        "got {}",
        stored[0].quote_excerpt
    );
}
