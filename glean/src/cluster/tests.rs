#![allow(clippy::unwrap_used)]
use super::*;
use chrono::{TimeZone, Utc};
use std::path::PathBuf;

fn session(uuid: &str, focus: Option<&str>, tags: &[&str], is_orphan: bool) -> SessionRecord {
    SessionRecord {
        session_uuid: uuid.to_string(),
        jsonl_path: PathBuf::from(format!("/tmp/{uuid}.jsonl")),
        jsonl_sha256: "h".to_string(),
        repo_slug: Some("scottidler/second-brain".to_string()),
        repo_path: None,
        cwd: None,
        started_at: Utc.timestamp_opt(1_700_000_000, 0).unwrap(),
        ended_at: Utc.timestamp_opt(1_700_000_100, 0).unwrap(),
        design_doc_files: vec![],
        skill_invocations: vec![],
        interaction_normalized: String::new(),
        summary_one_line: format!("session {uuid}"),
        theme_tags: tags.iter().map(|s| s.to_string()).collect(),
        design_doc_focus: focus.map(PathBuf::from),
        is_orphan,
        classified_at: Utc.timestamp_opt(1_700_000_200, 0).unwrap(),
        classifier_model: "claude-sonnet-4-6".to_string(),
    }
}

#[test]
fn content_hash_is_stable_across_member_reordering() {
    let a = compute_content_hash(&["uuid-1".to_string(), "uuid-2".to_string(), "uuid-3".to_string()]);
    let b = compute_content_hash(&["uuid-3".to_string(), "uuid-1".to_string(), "uuid-2".to_string()]);
    assert_eq!(a, b);
}

#[test]
fn content_hash_differs_when_membership_set_changes() {
    let a = compute_content_hash(&["x".to_string(), "y".to_string()]);
    let b = compute_content_hash(&["x".to_string(), "y".to_string(), "z".to_string()]);
    assert_ne!(a, b);
}

#[test]
fn linkage_parse_round_trip() {
    for s in &["complete-link", "average-link", "single-link"] {
        let l = Linkage::parse(s).expect("parse");
        assert_eq!(l.as_str(), *s);
    }
    assert!(Linkage::parse("nonsense").is_none());
}

#[test]
fn hard_cluster_groups_sessions_with_same_focus() {
    let s1 = session("s1", Some("docs/design/foo.md"), &["a"], false);
    let s2 = session("s2", Some("docs/design/foo.md"), &["b"], false);
    let s3 = session("s3", Some("docs/design/bar.md"), &["c"], false);
    let items = cluster_sessions(&[s1, s2, s3], Linkage::CompleteLink, 0.78).expect("cluster");
    let dd: Vec<&WorkItem> = items.iter().filter(|w| w.key_type == WorkItemKey::DesignDoc).collect();
    assert_eq!(dd.len(), 2);
    let foo = dd
        .iter()
        .find(|w| w.key_value == "docs/design/foo.md")
        .expect("foo bucket");
    assert_eq!(foo.session_uuids.len(), 2);
}

#[test]
fn orphans_are_singletons_even_when_they_share_focus() {
    let s1 = session("s1", Some("docs/design/foo.md"), &[], true);
    let s2 = session("s2", Some("docs/design/foo.md"), &[], false);
    let items = cluster_sessions(&[s1, s2], Linkage::CompleteLink, 0.78).expect("cluster");
    let singletons: Vec<&WorkItem> = items.iter().filter(|w| w.key_type == WorkItemKey::Singleton).collect();
    assert!(singletons.iter().any(|w| w.session_uuids == vec!["s1"]));
}

#[test]
fn agglomerative_complete_link_does_not_chain() {
    // Three pseudo-clusters of two 384-dim points each; A and B share
    // a weak similarity, B and C share a weak similarity, but A and C
    // do NOT. complete-link must keep them as three clusters; single-
    // link will merge them.
    let dim = 4;
    let mut emb: Vec<Vec<f32>> = Vec::new();
    let a1 = vec![1.0, 0.0, 0.0, 0.0];
    let a2 = vec![0.95, 0.0, 0.0, 0.0];
    let b1 = vec![0.6, 0.6, 0.0, 0.0];
    let b2 = vec![0.6, 0.6, 0.0, 0.0];
    let c1 = vec![0.0, 1.0, 0.0, 0.0];
    let c2 = vec![0.0, 0.95, 0.0, 0.0];
    for v in [&a1, &a2, &b1, &b2, &c1, &c2] {
        let norm = (v.iter().map(|x| x * x).sum::<f32>()).sqrt();
        emb.push(v.iter().map(|x| x / norm).collect());
    }
    assert_eq!(dim, emb[0].len());

    let complete = agglomerative_cluster(&emb, Linkage::CompleteLink, 0.85);
    // complete-link must NOT collapse all 6 into one (the "chain" case).
    assert!(complete.iter().all(|c| c.len() <= 4));
    let single = agglomerative_cluster(&emb, Linkage::SingleLink, 0.85);
    let max_single = single.iter().map(|c| c.len()).max().unwrap_or(0);
    assert!(max_single >= complete.iter().map(|c| c.len()).max().unwrap_or(0));
}
