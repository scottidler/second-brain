use super::*;
use crate::config::{Bm25Method, Config, MethodsConfig, RerankConfig, RetrievalConfig, VectorMethod};
use std::path::PathBuf;
use vault::frontmatter::Frontmatter;
use vault::note::Note;
use vault::search::{Edge, MockReranker, SearchIndex};

fn seed(db: &SearchIndex, path: &str, body: &str) {
    let fm = Frontmatter {
        title: Some(path.to_string()),
        note_type: Some("article".to_string()),
        origin: Some("assisted".to_string()),
        domain: Some("ai".to_string()),
        ..Frontmatter::default()
    };
    let note = Note {
        path: PathBuf::from(path),
        frontmatter: fm,
        body: body.to_string(),
        raw: format!("---\n---\n{body}"),
    };
    db.index_one(&note, 100).expect("seed note");
}

fn rerank_cfg(input_k: u32, latency_budget_ms: u64) -> RerankConfig {
    RerankConfig {
        enabled: true,
        input_k,
        latency_budget_ms,
        ..RerankConfig::default()
    }
}

/// The head (top `input_k`) is reranked by the injected scorer; the tail keeps
/// its fused order and is appended untouched.
#[test]
fn rerank_reorders_head_preserves_tail() {
    let db = SearchIndex::open_memory().expect("db");
    seed(&db, "notes/p0.md", "alpha content");
    seed(&db, "notes/p1.md", "query content"); // contains the query token
    seed(&db, "notes/p2.md", "gamma content");
    seed(&db, "notes/p3.md", "delta content");

    let fused = vec![
        "notes/p0.md".to_string(),
        "notes/p1.md".to_string(),
        "notes/p2.md".to_string(),
        "notes/p3.md".to_string(),
    ];
    // Generous budget so the probe never trips.
    let cfg = rerank_cfg(2, u64::MAX);
    let reranker = MockReranker::new();

    let outcome = OracleMcpServer::rerank_within_budget(&db, &cfg, "query", fused, &reranker).expect("rerank");
    match outcome {
        RerankOutcome::Reordered(paths) => {
            // Head [p0,p1] reranked: p1 (overlaps "query") outranks p0.
            // Tail [p2,p3] preserved in fused order.
            assert_eq!(
                paths,
                vec![
                    "notes/p1.md".to_string(),
                    "notes/p0.md".to_string(),
                    "notes/p2.md".to_string(),
                    "notes/p3.md".to_string(),
                ]
            );
        }
        _ => panic!("expected Reordered"),
    }
}

fn bm25_only_retrieval(weight: f32) -> RetrievalConfig {
    RetrievalConfig {
        methods: MethodsConfig {
            vector: VectorMethod {
                enabled: false,
                ..Default::default()
            },
            bm25: Bm25Method {
                enabled: true,
                weight,
                ..Default::default()
            },
            ..Default::default()
        },
        ..Default::default()
    }
}

/// The path `eval::retrieve` records under CONFIGURED_LABEL: query transform
/// followed by `run_pipeline` over the operator config. A bm25-only config
/// (vector off) keeps it hermetic - no embedding model. bm25 sits at the
/// shipped demoted weight 0.3; a non-zero weight must still contribute its
/// full order.
#[test]
fn run_configured_pipeline_runs_the_configured_bm25_retriever() {
    let db = SearchIndex::open_memory().expect("db");
    seed(&db, "notes/hit.md", "transformer attention mechanism");
    seed(&db, "notes/miss.md", "unrelated kitchen recipe content");
    let cfg = Config {
        retrieval: bm25_only_retrieval(0.3),
        ..Default::default()
    };
    let server = OracleMcpServer::new(cfg, db);
    let handle = server.db_handle();
    let guard = handle.lock().expect("lock");
    let rows = server
        .run_configured_pipeline(&guard, "transformer", None, None, None, 10)
        .expect("configured pipeline");
    let paths: Vec<String> = rows.iter().map(|r| r.path.clone()).collect();
    assert!(
        paths.contains(&"notes/hit.md".to_string()),
        "bm25 at the demoted 0.3 weight must still contribute its match: {paths:?}"
    );
    assert!(
        !paths.contains(&"notes/miss.md".to_string()),
        "a non-matching note must not appear: {paths:?}"
    );
}

/// Weight 0.0 is the strongest demotion: a 0.0-weighted list contributes
/// nothing to weighted RRF, so a sole bm25 retriever at weight 0.0 yields no
/// results. This pins the run_pipeline -> weighted-fusion WIRING (the doc's
/// "demoted retriever stays out") through the real pipeline, complementing the
/// vault primitive test that pins the fusion arithmetic.
#[test]
fn run_pipeline_drops_a_fully_demoted_zero_weight_method() {
    let db = SearchIndex::open_memory().expect("db");
    seed(&db, "notes/hit.md", "transformer attention mechanism");
    let cfg = bm25_only_retrieval(0.0);
    let server = OracleMcpServer::new(Config::default(), db);
    let handle = server.db_handle();
    let guard = handle.lock().expect("lock");
    let rows = server
        .run_pipeline(
            &guard,
            &cfg,
            "transformer",
            &["transformer".to_string()],
            None,
            None,
            None,
            10,
        )
        .expect("run_pipeline");
    assert!(
        rows.is_empty(),
        "a fully-demoted (0.0-weight) sole method must yield no results: {rows:?}"
    );
}

fn link_edge(src: &str, dst: &str) -> Edge {
    Edge {
        src: src.to_string(),
        dst: dst.to_string(),
        kind: "link".to_string(),
        weight: 1.0,
        predicate: String::new(),
        src_note: String::new(),
    }
}

/// Graph expansion scores each reached neighbor by
/// `w_seed(origin) * edge_weight * hop_decay^(hop-1)`, where the better-ranked
/// seed (rank 0) contributes `1/(rank+1)`. This pins BOTH levers - seed
/// weighting and hop decay - at the oracle layer, hermetically (no embeddings).
#[test]
fn expand_to_graph_paths_applies_seed_weight_and_hop_decay() {
    let db = SearchIndex::open_memory().expect("db");
    // Seed every endpoint first: insert_edges skips edges with absent endpoints.
    for p in [
        "notes/s1.md",
        "notes/s2.md",
        "notes/aa.md",
        "notes/mid.md",
        "notes/bb.md",
        "notes/cc.md",
    ] {
        seed(&db, p, "body");
    }
    let server = OracleMcpServer::new(Config::default(), db);
    {
        let handle = server.db_handle();
        let mut guard = handle.lock().expect("lock");
        // s1 (seed rank 0) -> aa, mid (both 1 hop). mid -> cc (2 hops from s1).
        // s2 (seed rank 1) -> bb (1 hop). All edge weights 1.0.
        guard
            .insert_edges(&[
                link_edge("notes/s1.md", "notes/aa.md"),
                link_edge("notes/s1.md", "notes/mid.md"),
                link_edge("notes/mid.md", "notes/cc.md"),
                link_edge("notes/s2.md", "notes/bb.md"),
            ])
            .expect("insert edges");
    }

    let seeds = vec!["notes/s1.md".to_string(), "notes/s2.md".to_string()];
    let handle = server.db_handle();
    let guard = handle.lock().expect("lock");
    let paths = server
        .expand_to_graph_paths(&guard, &seeds, None, None, None, 2, None, 0.0, 0.5)
        .expect("expand");

    // Scores: aa = 1.0*1.0*1.0 = 1.0; mid = 1.0; bb = 0.5(seed rank 1)*1.0*1.0 = 0.5;
    // cc = 1.0(seed rank 0)*1.0(edge product)*0.5(hop_decay^1) = 0.5.
    // Order is score desc, path asc on ties:
    //   - aa, mid (1.0) before bb (0.5)  => seed weighting (bb's rank-1 seed loses)
    //   - cc (0.5, hop 2) sits with bb   => hop decay pulled the rank-0 2-hop node
    //     down to the rank-1 1-hop node's score.
    assert_eq!(
        paths,
        vec![
            "notes/aa.md".to_string(),
            "notes/mid.md".to_string(),
            "notes/bb.md".to_string(),
            "notes/cc.md".to_string(),
        ]
    );
}

/// `input_k = 0` short-circuits to fail-open (no reorder, no disable).
#[test]
fn rerank_input_k_zero_fails_open() {
    let db = SearchIndex::open_memory().expect("db");
    seed(&db, "notes/p0.md", "alpha");
    seed(&db, "notes/p1.md", "query");
    let fused = vec!["notes/p0.md".to_string(), "notes/p1.md".to_string()];
    let cfg = rerank_cfg(0, u64::MAX);
    let reranker = MockReranker::new();

    let outcome = OracleMcpServer::rerank_within_budget(&db, &cfg, "query", fused.clone(), &reranker).expect("rerank");
    match outcome {
        RerankOutcome::FailOpen(paths) => assert_eq!(paths, fused),
        _ => panic!("expected FailOpen"),
    }
}

/// A single candidate short-circuits to fail-open.
#[test]
fn rerank_single_candidate_fails_open() {
    let db = SearchIndex::open_memory().expect("db");
    seed(&db, "notes/p0.md", "alpha");
    let fused = vec!["notes/p0.md".to_string()];
    let cfg = rerank_cfg(5, u64::MAX);
    let reranker = MockReranker::new();

    let outcome = OracleMcpServer::rerank_within_budget(&db, &cfg, "query", fused.clone(), &reranker).expect("rerank");
    match outcome {
        RerankOutcome::FailOpen(paths) => assert_eq!(paths, fused),
        _ => panic!("expected FailOpen"),
    }
}

/// A reranker slow enough that the projected batch cost exceeds a zero budget
/// trips the budget branch: fused order returned, caller told to disable.
#[test]
fn rerank_over_budget_disables() {
    struct SlowMock;
    impl vault::search::Reranker for SlowMock {
        fn model_id(&self) -> &str {
            "slow-mock"
        }
        fn score(&self, _query: &str, docs: &[&str]) -> eyre::Result<Vec<f32>> {
            // Sleep so the probe's measured per-pair cost is reliably positive,
            // making `projected > 0` deterministic against a zero budget.
            std::thread::sleep(std::time::Duration::from_millis(3));
            Ok(vec![0.0; docs.len()])
        }
    }

    let db = SearchIndex::open_memory().expect("db");
    seed(&db, "notes/p0.md", "alpha");
    seed(&db, "notes/p1.md", "beta");
    let fused = vec!["notes/p0.md".to_string(), "notes/p1.md".to_string()];
    let cfg = rerank_cfg(2, 0);

    let outcome = OracleMcpServer::rerank_within_budget(&db, &cfg, "query", fused.clone(), &SlowMock).expect("rerank");
    match outcome {
        RerankOutcome::Disable(paths) => assert_eq!(paths, fused),
        _ => panic!("expected Disable (over budget)"),
    }
}
