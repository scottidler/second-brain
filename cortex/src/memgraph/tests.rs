use super::*;
use crate::vault::Note;
use vault::embedding::{EmbeddingModel, MockEmbedder};
use vault::search::{EmbeddingKind, SearchIndex};

fn graph_cfg() -> GraphConfig {
    GraphConfig {
        fact_weight: 0.5,
        bridge_min_cosine: -1.0, // admit all in bridge tests
        ..GraphConfig::default()
    }
}

fn ingested(path: &str, body: &str) -> Note {
    let fm = vault::frontmatter::Frontmatter {
        origin: Some("assisted".to_string()),
        ..Default::default()
    };
    Note {
        path: std::path::PathBuf::from(path),
        frontmatter: fm,
        body: body.to_string(),
        raw: String::new(),
    }
}

/// Returns a fixed triple set regardless of body.
struct MockTriples(Vec<Triple>);
impl TripleExtractor for MockTriples {
    fn extract(&self, _body: &str) -> Vec<Triple> {
        self.0.clone()
    }
}

#[test]
fn parse_triple_parses_and_slugifies_predicate() {
    let t = parse_triple("LangChain | built on | Neo4j").expect("triple");
    assert_eq!(t.subject, "LangChain");
    assert_eq!(t.predicate, "built-on");
    assert_eq!(t.object, "Neo4j");
    assert!(parse_triple("only two | fields").is_none());
    assert!(parse_triple("a |  | c").is_none(), "empty predicate rejected");
}

#[test]
fn extract_facts_writes_typed_edge_with_provenance() {
    let mut index = SearchIndex::open_memory().expect("open");
    // Both entity hubs must exist for the fact edge endpoints to resolve.
    index
        .insert_test_note_graph("entities/langchain.md", &[], "", "", "tech", "hub", 100)
        .unwrap();
    index
        .insert_test_note_graph("entities/neo4j.md", &[], "", "", "tech", "hub", 100)
        .unwrap();

    let extractor = MockTriples(vec![Triple {
        subject: "LangChain".into(),
        predicate: "built-on".into(),
        object: "Neo4j".into(),
    }]);
    let notes = vec![ingested("notes/a.md", "LangChain is built on Neo4j.")];

    let stats = extract_facts(&mut index, &notes, &extractor, &graph_cfg(), 50).expect("facts");
    assert_eq!(stats.facts_written, 1);

    let facts = index.fact_edges().expect("facts");
    assert_eq!(facts.len(), 1);
    assert_eq!(facts[0].src, "entities/langchain.md");
    assert_eq!(facts[0].dst, "entities/neo4j.md");
    assert_eq!(facts[0].predicate, "built-on");
    assert_eq!(facts[0].src_note, "notes/a.md", "provenance preserved");
}

#[test]
fn extract_facts_skips_when_a_hub_is_missing() {
    let mut index = SearchIndex::open_memory().expect("open");
    // Only the subject hub exists; object hub absent -> edge skipped, no abort.
    index
        .insert_test_note_graph("entities/langchain.md", &[], "", "", "tech", "hub", 100)
        .unwrap();

    let extractor = MockTriples(vec![Triple {
        subject: "LangChain".into(),
        predicate: "built-on".into(),
        object: "Neo4j".into(),
    }]);
    let notes = vec![ingested("notes/a.md", "x")];
    let stats = extract_facts(&mut index, &notes, &extractor, &graph_cfg(), 50).expect("facts");
    assert_eq!(stats.facts_written, 0);
    assert_eq!(index.count_edges(Some("fact")).expect("count"), 0);
}

fn add_fact(index: &mut SearchIndex, src: &str, dst: &str, predicate: &str) {
    index
        .insert_edges(&[Edge::fact(src, dst, predicate, 0.5, "notes/src.md")])
        .expect("insert fact");
}

#[test]
fn detect_contradictions_flags_functional_predicate_with_two_objects() {
    let mut index = SearchIndex::open_memory().expect("open");
    for h in [
        "entities/x.md",
        "entities/y.md",
        "entities/z.md",
        "entities/u.md",
        "entities/v.md",
    ] {
        index
            .insert_test_note_graph(h, &[], "", "", "tech", "hub", 100)
            .unwrap();
    }
    // Functional predicate `released-on` with two distinct objects -> conflict.
    add_fact(&mut index, "entities/x.md", "entities/y.md", "released-on");
    add_fact(&mut index, "entities/x.md", "entities/z.md", "released-on");
    // Multi-valued predicate `uses` with two objects -> NOT a conflict.
    add_fact(&mut index, "entities/x.md", "entities/u.md", "uses");
    add_fact(&mut index, "entities/x.md", "entities/v.md", "uses");

    let functional: std::collections::HashSet<String> = ["released-on".to_string()].into_iter().collect();
    let conflicts = detect_contradictions(&index, &functional).expect("detect");
    assert_eq!(conflicts.len(), 1, "only the functional predicate conflicts");
    assert_eq!(conflicts[0].predicate, "released-on");
    assert_eq!(conflicts[0].subject, "entities/x.md");
    assert_eq!(conflicts[0].objects.len(), 2);
    // Flag-only: the conflicting edges are NOT removed.
    assert_eq!(index.count_edges(Some("fact")).expect("count"), 4);
}

#[test]
fn remove_noise_drops_only_noise_predicates() {
    let mut index = SearchIndex::open_memory().expect("open");
    for h in ["entities/x.md", "entities/y.md", "entities/z.md"] {
        index
            .insert_test_note_graph(h, &[], "", "", "tech", "hub", 100)
            .unwrap();
    }
    add_fact(&mut index, "entities/x.md", "entities/y.md", "is"); // noise
    add_fact(&mut index, "entities/x.md", "entities/z.md", "built-on"); // keep

    let noise: std::collections::HashSet<String> = ["is".to_string()].into_iter().collect();
    let removed = remove_noise(&index, &noise).expect("noise");
    assert_eq!(removed, 1);
    let facts = index.fact_edges().expect("facts");
    assert_eq!(facts.len(), 1);
    assert_eq!(facts[0].predicate, "built-on");
}

#[test]
fn bridge_clusters_connects_an_isolated_note_to_its_nearest_neighbor() {
    let mut index = SearchIndex::open_memory().expect("open");
    let m = MockEmbedder::new(16, "mock-bridge-v1");
    index.set_active_embedding(m.model_version(), m.dim()).unwrap();
    // Two isolated notes with identical embeddings (cosine 1.0), no edges.
    for p in ["notes/iso1.md", "notes/iso2.md"] {
        index
            .insert_test_note_graph(p, &[], "", "", "tech", "island", 100)
            .unwrap();
        let v = m.embed_one("shared island topic").expect("embed");
        index
            .upsert_embedding(
                p,
                EmbeddingKind::Summary,
                0,
                "shared island topic",
                &v,
                m.model_version(),
                100,
            )
            .unwrap();
    }
    assert_eq!(index.count_edges(None).expect("count"), 0, "both notes start isolated");

    let bridges = bridge_clusters(&mut index, &graph_cfg()).expect("bridge");
    assert!(bridges >= 1, "at least one bridge added");
    assert!(index.count_edges(Some("bridge")).expect("count") >= 1);
    // No note remains fully isolated.
    assert!(index.notes_without_edges().expect("iso").is_empty());
}

#[test]
fn consolidate_runs_all_three_agents() {
    let mut index = SearchIndex::open_memory().expect("open");
    let m = MockEmbedder::new(16, "mock-consolidate-v1");
    index.set_active_embedding(m.model_version(), m.dim()).unwrap();
    for h in ["entities/x.md", "entities/y.md", "entities/z.md"] {
        index
            .insert_test_note_graph(h, &[], "", "", "tech", "hub", 100)
            .unwrap();
    }
    // A noise fact + two functional-conflict facts.
    add_fact(&mut index, "entities/x.md", "entities/y.md", "is");
    add_fact(&mut index, "entities/x.md", "entities/y.md", "released-on");
    add_fact(&mut index, "entities/x.md", "entities/z.md", "released-on");

    let report = consolidate(&mut index, &graph_cfg()).expect("consolidate");
    assert_eq!(report.noise_removed, 1, "the `is` fact removed");
    assert_eq!(report.contradictions.len(), 1, "released-on conflict flagged");
}
