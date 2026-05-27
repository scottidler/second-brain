use super::*;
use crate::config::Config;
use crate::fabric::FakeFabric;
use crate::gems::{InteractionTurn, Review};
use crate::narrative::Archetype;
use crate::narrative::discover::ClusterCandidate;
use chrono::{TimeZone, Utc};

fn gem(id: i64, task: &str, tags: Vec<&str>) -> Gem {
    Gem {
        id,
        workitem_id: 1,
        session_uuid: "s1".to_string(),
        task: task.to_string(),
        context_loaded: vec![],
        context_missing: vec![],
        interaction: vec![InteractionTurn {
            ai_says: "ai".to_string(),
            ai_turn_uuid: format!("ai-{id}"),
            user_says: "user reply".to_string(),
            user_turn_uuid: format!("u-{id}"),
            tags: vec![],
        }],
        review: Review::default(),
        tags: tags.into_iter().map(String::from).collect(),
        why_it_matters: format!("matters {id}"),
        extractor_model: "sonnet".to_string(),
        extracted_at: Utc.with_ymd_and_hms(2026, 5, 1, 12, 0, 0).single().expect("ts"),
    }
}

fn candidate() -> ClusterCandidate {
    ClusterCandidate {
        archetype: Archetype::Session,
        cluster_key: "session-abc".to_string(),
        gems: vec![
            gem(1, "task one", vec!["reject"]),
            gem(2, "task two", vec!["verify"]),
            gem(3, "task three", vec!["name-the-failure"]),
        ],
    }
}

#[tokio::test]
async fn accepts_well_formed_output() {
    let cfg = Config::default();
    let fabric = FakeFabric::new();
    fabric.set_response(
        "facet-narrate",
        r###"{
            "title": "The Three Rejections",
            "thesis": "Three plausible-but-wrong AI suggestions in one session.",
            "body_md": "## Setup\n\nFirst paragraph.\n\n## Complication\n\nSecond paragraph.\n\n## Resolution\n\nThird paragraph.",
            "gem_ids": [1, 2, 3],
            "chronologically_ordered": true
        }"###,
    );
    let outcome = narrate(&candidate(), &cfg, &fabric).await.expect("narrate");
    match outcome {
        NarrateOutcome::Accepted(n) => {
            assert_eq!(n.title, "The Three Rejections");
            assert_eq!(n.gem_ids, vec![1, 2, 3]);
            assert!(!n.body_md.is_empty());
        }
        NarrateOutcome::Skipped { .. } => panic!("expected accepted"),
    }
}

#[tokio::test]
async fn empty_title_fires_rejection_gate() {
    let cfg = Config::default();
    let fabric = FakeFabric::new();
    fabric.set_response(
        "facet-narrate",
        r#"{"title":"","thesis":"","body_md":"","gem_ids":[],"chronologically_ordered":true}"#,
    );
    let outcome = narrate(&candidate(), &cfg, &fabric).await.expect("narrate");
    assert!(matches!(outcome, NarrateOutcome::Skipped { .. }));
}

#[tokio::test]
async fn empty_thesis_also_fires_rejection_gate() {
    let cfg = Config::default();
    let fabric = FakeFabric::new();
    fabric.set_response(
        "facet-narrate",
        r#"{"title":"Something","thesis":"","body_md":"","gem_ids":[],"chronologically_ordered":true}"#,
    );
    let outcome = narrate(&candidate(), &cfg, &fabric).await.expect("narrate");
    assert!(matches!(outcome, NarrateOutcome::Skipped { .. }));
}

#[tokio::test]
async fn malformed_json_returns_error() {
    let cfg = Config::default();
    let fabric = FakeFabric::new();
    fabric.set_response("facet-narrate", "not json");
    let err = narrate(&candidate(), &cfg, &fabric).await.expect_err("parse fail");
    assert!(format!("{err:#}").contains("parse facet-narrate"));
}
