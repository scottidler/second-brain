use super::*;
use crate::search::SearchIndex;

/// Build an in-memory index with `n` minimal note rows at paths
/// `notes/{0..n}.md`.
fn index_with_notes(n: usize) -> SearchIndex {
    let index = SearchIndex::open_memory().expect("open");
    for i in 0..n {
        index
            .insert_test_note_row(&format!("notes/{i}.md"), "article", 1_000 + i as i64)
            .expect("insert note");
    }
    index
}

fn det(src: &str, dst: &str, kind: &str, weight: f32) -> Edge {
    Edge::deterministic(src, dst, kind, weight)
}

#[test]
fn note_path_exists_reports_presence() {
    let index = index_with_notes(2);
    assert!(index.note_path_exists("notes/0.md").expect("exists"));
    assert!(!index.note_path_exists("notes/absent.md").expect("absent"));
}

#[test]
fn graph_state_round_trips() {
    let index = index_with_notes(0);
    assert_eq!(index.graph_state_get("k").expect("get"), None);
    index.graph_state_set("k", "v1").expect("set");
    assert_eq!(index.graph_state_get("k").expect("get"), Some("v1".to_string()));
    // upsert overwrites
    index.graph_state_set("k", "v2").expect("set");
    assert_eq!(index.graph_state_get("k").expect("get"), Some("v2".to_string()));
}

#[test]
fn insert_edges_skips_absent_dst_without_aborting_batch() {
    let mut index = index_with_notes(2);
    let edges = vec![
        det("notes/0.md", "notes/1.md", "semantic", 0.9),      // valid
        det("notes/0.md", "notes/absent.md", "wikilink", 1.0), // dangling dst -> skip
        det("notes/1.md", "notes/0.md", "semantic", 0.8),      // valid
    ];
    let (inserted, skipped) = index.insert_edges(&edges).expect("insert");
    assert_eq!(inserted, 2, "two valid edges inserted");
    assert_eq!(skipped, 1, "one dangling-dst edge skipped");
    assert_eq!(index.count_edges(None).expect("count"), 2);
    // The batch did NOT abort: the valid edges are present.
    assert_eq!(index.count_edges(Some("semantic")).expect("count"), 2);
    assert_eq!(index.count_edges(Some("wikilink")).expect("count"), 0);
}

#[test]
fn insert_edges_skips_self_edges() {
    let mut index = index_with_notes(1);
    let (inserted, skipped) = index
        .insert_edges(&[det("notes/0.md", "notes/0.md", "semantic", 1.0)])
        .expect("insert");
    assert_eq!(inserted, 0);
    assert_eq!(skipped, 1);
}

#[test]
fn delete_edges_by_src_removes_only_that_src() {
    let mut index = index_with_notes(3);
    index
        .insert_edges(&[
            det("notes/0.md", "notes/1.md", "semantic", 0.9),
            det("notes/0.md", "notes/2.md", "semantic", 0.8),
            det("notes/1.md", "notes/2.md", "semantic", 0.7),
        ])
        .expect("insert");
    assert_eq!(index.count_edges(None).expect("count"), 3);
    let removed = index.delete_edges_by_src("notes/0.md").expect("delete");
    assert_eq!(removed, 2);
    assert_eq!(index.count_edges(None).expect("count"), 1);
}

#[test]
fn expand_graph_one_hop_returns_neighbors_both_directions() {
    let mut index = index_with_notes(3);
    // Only one directed row written per pair; expansion must traverse both
    // src->dst and dst->src.
    index
        .insert_edges(&[
            det("notes/0.md", "notes/1.md", "semantic", 0.9), // 0 owns -> 1
            det("notes/2.md", "notes/0.md", "semantic", 0.8), // 2 owns -> 0
        ])
        .expect("insert");

    let reaches = index
        .expand_graph(&["notes/0.md".to_string()], 1, None, 0.0)
        .expect("expand");
    let paths: Vec<&str> = reaches.iter().map(|r| r.path.as_str()).collect();
    assert!(paths.contains(&"notes/1.md"), "forward neighbor reached");
    assert!(paths.contains(&"notes/2.md"), "reverse neighbor reached");
    assert!(reaches.iter().all(|r| r.hop == 1));
}

#[test]
fn expand_graph_respects_min_weight() {
    let mut index = index_with_notes(3);
    index
        .insert_edges(&[
            det("notes/0.md", "notes/1.md", "semantic", 0.9),
            det("notes/0.md", "notes/2.md", "shared-tag", 0.1),
        ])
        .expect("insert");
    let reaches = index
        .expand_graph(&["notes/0.md".to_string()], 1, None, 0.5)
        .expect("expand");
    let paths: Vec<&str> = reaches.iter().map(|r| r.path.as_str()).collect();
    assert!(paths.contains(&"notes/1.md"));
    assert!(!paths.contains(&"notes/2.md"), "below min_weight filtered out");
}

#[test]
fn expand_graph_respects_edge_kinds() {
    let mut index = index_with_notes(3);
    index
        .insert_edges(&[
            det("notes/0.md", "notes/1.md", "semantic", 0.9),
            det("notes/0.md", "notes/2.md", "wikilink", 1.0),
        ])
        .expect("insert");
    let reaches = index
        .expand_graph(&["notes/0.md".to_string()], 1, Some(&["wikilink".to_string()]), 0.0)
        .expect("expand");
    let paths: Vec<&str> = reaches.iter().map(|r| r.path.as_str()).collect();
    assert_eq!(paths, vec!["notes/2.md"]);
}

#[test]
fn expand_graph_edge_kinds_also_matches_predicate() {
    let mut index = index_with_notes(3);
    // A typed fact edge (kind="fact", predicate="uses") and a semantic edge.
    index
        .insert_edges(&[
            Edge::fact("notes/0.md", "notes/1.md", "uses", 0.5, "notes/src.md"),
            det("notes/0.md", "notes/2.md", "semantic", 0.9),
        ])
        .expect("insert");
    // Filtering by the predicate "uses" matches the fact edge (whose kind is
    // "fact"), not the semantic one.
    let reaches = index
        .expand_graph(&["notes/0.md".to_string()], 1, Some(&["uses".to_string()]), 0.0)
        .expect("expand");
    let paths: Vec<&str> = reaches.iter().map(|r| r.path.as_str()).collect();
    assert_eq!(paths, vec!["notes/1.md"], "predicate filter selected the fact edge");
}

#[test]
fn expand_graph_two_hops_accumulates_weight_and_origin() {
    let mut index = index_with_notes(3);
    // 0 -> 1 -> 2
    index
        .insert_edges(&[
            det("notes/0.md", "notes/1.md", "semantic", 0.5),
            det("notes/1.md", "notes/2.md", "semantic", 0.4),
        ])
        .expect("insert");
    let reaches = index
        .expand_graph(&["notes/0.md".to_string()], 2, None, 0.0)
        .expect("expand");
    let two_hop = reaches
        .iter()
        .find(|r| r.path == "notes/2.md")
        .expect("two-hop neighbor present");
    assert_eq!(two_hop.hop, 2);
    assert_eq!(two_hop.origin_seed, "notes/0.md");
    assert!((two_hop.weight - 0.2).abs() < 1e-6, "weight is product 0.5*0.4");
}

#[test]
fn expand_graph_never_returns_seeds() {
    let mut index = index_with_notes(2);
    index
        .insert_edges(&[det("notes/0.md", "notes/1.md", "semantic", 0.9)])
        .expect("insert");
    let reaches = index
        .expand_graph(&["notes/0.md".to_string(), "notes/1.md".to_string()], 1, None, 0.0)
        .expect("expand");
    assert!(
        reaches.iter().all(|r| r.path != "notes/0.md" && r.path != "notes/1.md"),
        "seeds are never returned as neighbors"
    );
}

#[test]
fn cascade_clears_edges_when_note_deleted() {
    let mut index = index_with_notes(2);
    index
        .insert_edges(&[det("notes/0.md", "notes/1.md", "semantic", 0.9)])
        .expect("insert");
    assert_eq!(index.count_edges(None).expect("count"), 1);
    // Deleting the dst note must cascade-delete the incident edge. The test
    // is a descendant module of `search`, so the private `conn` is reachable.
    index
        .conn
        .execute("DELETE FROM notes WHERE path = ?1", rusqlite::params!["notes/1.md"])
        .expect("delete note");
    assert_eq!(
        index.count_edges(None).expect("count"),
        0,
        "ON DELETE CASCADE cleared the incident edge"
    );
}
