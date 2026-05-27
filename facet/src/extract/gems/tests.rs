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
    .expect("upsert session");
    let wid = l
        .insert_workitem(NewWorkItem {
            slug: "rename-portrait",
            title: "Rename portrait to spectrum",
            created_at: now,
        })
        .expect("insert workitem");
    l.record_contribution(SessionContribution {
        session_uuid: "sess-1",
        workitem_id: wid,
        at: now,
    })
    .expect("contribution");
    let _ = l
        .insert_cluster_assignment(NewClusterAssignment {
            session_uuid: "sess-1",
            workitem_id: wid,
            first_turn_uuid: "ai-1",
            last_turn_uuid: "u-2",
            clustered_at: now,
            cluster_model: "haiku",
        })
        .expect("cluster assignment");
    let pending = l.pending_cluster_assignments(10).expect("pending");
    (pending.into_iter().next().expect("one"), "rename-portrait".to_string())
}

fn fabric_response_2_gems() -> &'static str {
    r#"{
        "gems": [
            {
                "task": "rename portrait to spectrum",
                "context_loaded": ["facet/src/extract/portrait.rs"],
                "context_missing": [],
                "interaction": [
                    {
                        "ai_turn_uuid": "ai-1",
                        "ai_says": "I'll rename portrait.rs to spectrum.rs",
                        "user_turn_uuid": "u-1",
                        "user_says": "no, also update the daemon",
                        "tags": ["reject", "name-the-failure"]
                    },
                    {
                        "ai_turn_uuid": "ai-2",
                        "ai_says": "Updating daemon imports too",
                        "user_turn_uuid": "u-2",
                        "user_says": "good",
                        "tags": ["verify"]
                    }
                ],
                "review": {
                    "accepted": "rename + daemon update landed",
                    "rejected": null,
                    "verified_manually": "cargo check passed",
                    "rewrote_by_hand": null
                },
                "tags": ["reject", "verify"],
                "why_it_matters": "rename has to cascade through every importer"
            },
            {
                "task": "wire spectrum into CLI",
                "context_loaded": [],
                "context_missing": ["the CLI subcommand registration"],
                "interaction": [
                    {
                        "ai_turn_uuid": "ai-3",
                        "ai_says": "Adding sb facet spectrum subcommand",
                        "user_turn_uuid": "u-3",
                        "user_says": "ack",
                        "tags": ["frame"]
                    },
                    {
                        "ai_turn_uuid": "ai-4",
                        "ai_says": "Done",
                        "user_turn_uuid": "u-4",
                        "user_says": "verified with sb facet --help",
                        "tags": ["verify"]
                    }
                ],
                "review": {
                    "accepted": "subcommand registered",
                    "rejected": null,
                    "verified_manually": "sb facet --help shows spectrum",
                    "rewrote_by_hand": null
                },
                "tags": ["frame"],
                "why_it_matters": "CLI surface follows the rename"
            }
        ]
    }"#
}

#[tokio::test]
async fn mine_gems_persists_and_marks_extracted() {
    let l = Ledger::open_in_memory().expect("ledger");
    let (assignment, slug) = seed_ledger_with_assignment(&l);
    let cfg = Config::default();
    let fabric = FakeFabric::new();
    fabric.set_response("facet-extract", fabric_response_2_gems());

    let turns = vec![
        turn("ai-1", Role::Assistant, "I'll rename portrait.rs"),
        turn("u-1", Role::User, "no, also update the daemon"),
    ];
    let out = mine_gems(
        &assignment,
        &turns,
        &slug,
        "Rename portrait",
        Some("me/r"),
        &cfg,
        &l,
        &fabric,
    )
    .await
    .expect("mine gems");

    assert_eq!(out.len(), 2);
    assert_eq!(out[0].interaction.len(), 2);
    assert_eq!(out[0].tags, vec!["reject".to_string(), "verify".to_string()]);

    // Cluster assignment should now be marked extracted=1.
    let pending = l.pending_cluster_assignments(10).expect("pending");
    assert!(pending.is_empty());
}

#[tokio::test]
async fn mine_gems_is_idempotent_on_replay() {
    let l = Ledger::open_in_memory().expect("ledger");
    let (assignment, slug) = seed_ledger_with_assignment(&l);
    let cfg = Config::default();
    let fabric = FakeFabric::new();
    fabric.set_response("facet-extract", fabric_response_2_gems());

    let turns = vec![
        turn("ai-1", Role::Assistant, "I'll rename"),
        turn("u-1", Role::User, "no"),
    ];

    // First run.
    let out1 = mine_gems(&assignment, &turns, &slug, "Rename", Some("me/r"), &cfg, &l, &fabric)
        .await
        .expect("mine 1");

    // Reset cluster assignment to pending so we can drive a second run
    // (production retries do not happen on extracted=1 rows).
    l.with_conn(|c| {
        c.execute(
            "UPDATE cluster_assignments SET extracted = 0 WHERE id = ?1",
            rusqlite::params![assignment.id],
        )
        .expect("reset extracted");
        Ok(())
    })
    .expect("with_conn");
    let pending = l.pending_cluster_assignments(10).expect("pending");
    let assignment2 = pending.into_iter().next().expect("one pending");

    fabric.set_response("facet-extract", fabric_response_2_gems());
    let out2 = mine_gems(&assignment2, &turns, &slug, "Rename", Some("me/r"), &cfg, &l, &fabric)
        .await
        .expect("mine 2");

    assert_eq!(out1.len(), out2.len());
    // The same content_hash -> same gem ids on second insert.
    let ids1: Vec<i64> = out1.iter().map(|g| g.id).collect();
    let ids2: Vec<i64> = out2.iter().map(|g| g.id).collect();
    assert_eq!(ids1, ids2);
    let stored = l.gems_for_workitem(assignment.workitem_id).expect("list");
    assert_eq!(stored.len(), 2);
}

#[tokio::test]
async fn mine_gems_handles_empty_gem_list() {
    let l = Ledger::open_in_memory().expect("ledger");
    let (assignment, slug) = seed_ledger_with_assignment(&l);
    let cfg = Config::default();
    let fabric = FakeFabric::new();
    fabric.set_response("facet-extract", r#"{"gems": []}"#);

    let turns = vec![
        turn("ai-1", Role::Assistant, "boilerplate"),
        turn("u-1", Role::User, "ok"),
    ];
    let out = mine_gems(&assignment, &turns, &slug, "Boring", Some("me/r"), &cfg, &l, &fabric)
        .await
        .expect("mine empty");

    assert!(out.is_empty());
    // Even with no gems, the assignment is still marked extracted=1
    // (the LLM said "nothing here," not "we failed").
    let pending = l.pending_cluster_assignments(10).expect("pending");
    assert!(pending.is_empty());
}

#[tokio::test]
async fn mine_gems_errors_on_malformed_llm_output() {
    let l = Ledger::open_in_memory().expect("ledger");
    let (assignment, slug) = seed_ledger_with_assignment(&l);
    let cfg = Config::default();
    let fabric = FakeFabric::new();
    fabric.set_response("facet-extract", "this is not json");

    let turns = vec![turn("ai-1", Role::Assistant, "x"), turn("u-1", Role::User, "y")];
    let err = mine_gems(&assignment, &turns, &slug, "Bad", Some("me/r"), &cfg, &l, &fabric)
        .await
        .expect_err("expected parse error");
    let msg = format!("{err:#}");
    assert!(msg.contains("parse extract"));
    // Cluster_assignment stays pending so the next tick retries.
    let pending = l.pending_cluster_assignments(10).expect("pending");
    assert_eq!(pending.len(), 1);
}

#[tokio::test]
async fn mine_gems_skips_extracted_gems_with_empty_interaction() {
    // Defensive: if the LLM emits a gem object with an empty interaction
    // (a malformed output that nonetheless parses), we WARN and skip it
    // rather than failing the whole chunk. This preserves the
    // remaining good gems.
    let l = Ledger::open_in_memory().expect("ledger");
    let (assignment, slug) = seed_ledger_with_assignment(&l);
    let cfg = Config::default();
    let fabric = FakeFabric::new();
    fabric.set_response(
        "facet-extract",
        r#"{"gems": [
            {
                "task": "empty",
                "interaction": [],
                "why_it_matters": "this should be skipped"
            },
            {
                "task": "good",
                "interaction": [
                    {
                        "ai_turn_uuid": "ai-1",
                        "ai_says": "hi",
                        "user_turn_uuid": "u-1",
                        "user_says": "ack",
                        "tags": ["frame"]
                    },
                    {
                        "ai_turn_uuid": "ai-2",
                        "ai_says": "bye",
                        "user_turn_uuid": "u-2",
                        "user_says": "ok",
                        "tags": ["verify"]
                    }
                ],
                "why_it_matters": "non-empty"
            }
        ]}"#,
    );

    let turns = vec![turn("ai-1", Role::Assistant, "hi"), turn("u-1", Role::User, "ack")];
    let out = mine_gems(&assignment, &turns, &slug, "Mixed", Some("me/r"), &cfg, &l, &fabric)
        .await
        .expect("mine mixed");

    assert_eq!(out.len(), 1);
    assert_eq!(out[0].task, "good");
}
