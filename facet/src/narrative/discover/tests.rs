use super::*;
use crate::gems::{InteractionTurn, Review};
use chrono::{TimeZone, Utc};

fn ts(year: i32, month: u32, day: u32, hour: u32) -> chrono::DateTime<chrono::Utc> {
    Utc.with_ymd_and_hms(year, month, day, hour, 0, 0)
        .single()
        .expect("valid ts")
}

fn make_gem(id: i64, session: &str, tags: Vec<&str>, extracted_at: chrono::DateTime<chrono::Utc>) -> Gem {
    Gem {
        id,
        workitem_id: 1,
        session_uuid: session.to_string(),
        task: format!("task {id}"),
        context_loaded: vec![],
        context_missing: vec![],
        interaction: vec![InteractionTurn {
            ai_says: "ai".to_string(),
            ai_turn_uuid: format!("ai-{id}"),
            user_says: format!("user reply {id}"),
            user_turn_uuid: format!("u-{id}"),
            tags: vec![],
        }],
        review: Review::default(),
        tags: tags.into_iter().map(String::from).collect(),
        why_it_matters: format!("matters {id}"),
        extractor_model: "sonnet".to_string(),
        extracted_at,
    }
}

#[test]
fn session_arc_requires_min_size_and_obstacle_tag() {
    let too_short = vec![
        make_gem(1, "s1", vec!["frame"], ts(2026, 5, 1, 10)),
        make_gem(2, "s1", vec!["reject"], ts(2026, 5, 1, 11)),
    ];
    assert!(discover_session_arcs(&too_short).is_empty());

    let no_obstacle = vec![
        make_gem(1, "s1", vec!["frame"], ts(2026, 5, 1, 10)),
        make_gem(2, "s1", vec!["verify"], ts(2026, 5, 1, 11)),
        make_gem(3, "s1", vec!["iterate"], ts(2026, 5, 1, 12)),
    ];
    assert!(discover_session_arcs(&no_obstacle).is_empty());

    let with_obstacle = vec![
        make_gem(1, "s1", vec!["frame"], ts(2026, 5, 1, 10)),
        make_gem(2, "s1", vec!["reject"], ts(2026, 5, 1, 11)),
        make_gem(3, "s1", vec!["verify"], ts(2026, 5, 1, 12)),
    ];
    let arcs = discover_session_arcs(&with_obstacle);
    assert_eq!(arcs.len(), 1);
    assert_eq!(arcs[0].archetype, Archetype::Session);
    assert_eq!(arcs[0].cluster_key, "s1");
    assert_eq!(arcs[0].gems.len(), 3);
}

#[test]
fn session_arc_orders_chronologically() {
    // Pass gems in REVERSE chronological order; expect them sorted.
    let gems = vec![
        make_gem(3, "s1", vec!["verify"], ts(2026, 5, 1, 12)),
        make_gem(1, "s1", vec!["name-the-failure"], ts(2026, 5, 1, 10)),
        make_gem(2, "s1", vec!["iterate"], ts(2026, 5, 1, 11)),
    ];
    let arcs = discover_session_arcs(&gems);
    assert_eq!(arcs.len(), 1);
    let ids: Vec<i64> = arcs[0].gems.iter().map(|g| g.id).collect();
    assert_eq!(ids, vec![1, 2, 3]);
}

#[test]
fn evergreen_clusters_emitted_for_scaffold_modes_with_enough_gems() {
    let gems = vec![
        make_gem(1, "s1", vec!["frame"], ts(2026, 5, 1, 10)),
        make_gem(2, "s1", vec!["frame"], ts(2026, 5, 1, 11)),
        make_gem(3, "s2", vec!["frame"], ts(2026, 5, 2, 10)),
        make_gem(4, "s2", vec!["reject"], ts(2026, 5, 2, 11)),
    ];
    let evergreens = discover_evergreen_clusters(&gems);
    // frame has 3 gems -> emitted; reject has 1 -> filtered.
    assert_eq!(evergreens.len(), 1);
    assert_eq!(evergreens[0].cluster_key, "mode-frame");
    assert_eq!(evergreens[0].gems.len(), 3);
}

#[test]
fn cross_session_clustering_groups_by_similarity_and_orders_chronologically() {
    // Two pairs of similar vectors -> two clusters of 2; total cluster count
    // = 0 (below MIN_CLUSTER_SIZE). Bump to 6 gems in two groups of 3 each.
    let gems = vec![
        make_gem(1, "s1", vec!["reject"], ts(2026, 5, 1, 10)),
        make_gem(2, "s2", vec!["reject"], ts(2026, 5, 2, 10)),
        make_gem(3, "s3", vec!["reject"], ts(2026, 5, 3, 10)),
        make_gem(4, "s4", vec!["frame"], ts(2026, 5, 1, 11)),
        make_gem(5, "s5", vec!["frame"], ts(2026, 5, 2, 11)),
        make_gem(6, "s6", vec!["frame"], ts(2026, 5, 3, 11)),
    ];
    // Embedding stub: gems with tag "reject" -> vector [1.0, 0.0, 0.0];
    // tag "frame" -> [0.0, 1.0, 0.0]. Single-link cosine collapses each
    // group above the threshold.
    let embed = |g: &Gem| -> eyre::Result<Vec<f32>> {
        if g.tags.iter().any(|t| t == "reject") {
            Ok(vec![1.0, 0.0, 0.0])
        } else {
            Ok(vec![0.0, 1.0, 0.0])
        }
    };
    let clusters = discover_cross_session_arcs(&gems, embed).expect("cluster");
    assert_eq!(clusters.len(), 2);
    for c in &clusters {
        assert_eq!(c.archetype, Archetype::CrossSession);
        assert_eq!(c.gems.len(), 3);
        // Chronological ordering within the cluster.
        let mut last = c.gems[0].extracted_at;
        for g in &c.gems[1..] {
            assert!(g.extracted_at >= last);
            last = g.extracted_at;
        }
    }
}

#[test]
fn cross_session_no_clusters_when_corpus_too_small() {
    let gems = vec![make_gem(1, "s1", vec!["reject"], ts(2026, 5, 1, 10))];
    let embed = |_: &Gem| -> eyre::Result<Vec<f32>> { Ok(vec![1.0]) };
    let clusters = discover_cross_session_arcs(&gems, embed).expect("cluster");
    assert!(clusters.is_empty());
}

#[test]
fn embedding_text_includes_task_why_and_first_user_truncated() {
    let mut g = make_gem(1, "s1", vec!["reject"], ts(2026, 5, 1, 10));
    g.interaction[0].user_says = "a".repeat(1000);
    let text = embedding_text(&g);
    assert!(text.contains("task 1"));
    assert!(text.contains("matters 1"));
    // The user_says portion is capped at 500 chars in the embedding text.
    let user_portion = text.split('\n').next_back().unwrap_or("");
    assert_eq!(user_portion.chars().count(), 500);
}
