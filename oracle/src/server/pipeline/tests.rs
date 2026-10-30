use super::*;
use crate::config::GraphMethod;
use crate::config::{Bm25Method, Config, MethodsConfig, RerankConfig, RetrievalConfig, VectorMethod};
use crate::tools::SearchMode;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use vault::embedding::{EmbeddingModel, MockEmbedder};
use vault::frontmatter::Frontmatter;
use vault::note::Note;
use vault::search::{BatchUpsert, Edge, EmbeddingKind, MockReranker, SearchIndex};

fn seed(db: &SearchIndex, path: &str, body: &str) {
    seed_tagged(db, path, body, &[]);
}

fn seed_tagged(db: &SearchIndex, path: &str, body: &str, tags: &[&str]) {
    let fm = Frontmatter {
        title: Some(path.to_string()),
        note_type: Some("article".to_string()),
        origin: Some("assisted".to_string()),
        tags: (!tags.is_empty()).then(|| tags.iter().map(|t| t.to_string()).collect()),
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

/// Rerank is pinned off: it is on by default, and whether it reorders
/// depends on the host loading the cross-encoder within budget.
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
        rerank: rerank_off(),
        ..Default::default()
    }
}

fn rerank_off() -> RerankConfig {
    RerankConfig {
        enabled: false,
        ..RerankConfig::default()
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
        .run_configured_pipeline(&guard, "transformer", None, false, None, None, 10)
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
            false,
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

/// `tags` narrows the configured (bm25-only, hermetic) pipeline, threaded
/// through `run_pipeline` -> `bm25_paths` -> `SearchIndex::search`'s
/// `Filter::and_tags`.
#[test]
fn run_configured_pipeline_filters_by_tags() {
    let db = SearchIndex::open_memory().expect("db");
    seed_tagged(&db, "notes/hit.md", "transformer attention mechanism", &["privacy"]);
    seed_tagged(&db, "notes/miss.md", "transformer attention mechanism", &["cooking"]);
    let cfg = Config {
        retrieval: bm25_only_retrieval(0.3),
        ..Default::default()
    };
    let server = OracleMcpServer::new(cfg, db);
    let handle = server.db_handle();
    let guard = handle.lock().expect("lock");
    let tags = vec!["privacy".to_string()];
    let rows = server
        .run_configured_pipeline(&guard, "transformer", Some(&tags), false, None, None, 10)
        .expect("configured pipeline");
    let paths: Vec<String> = rows.iter().map(|r| r.path.clone()).collect();
    assert_eq!(
        paths,
        vec!["notes/hit.md".to_string()],
        "tags filter must exclude notes/miss.md: {paths:?}"
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
        .expand_to_graph_paths(&guard, &seeds, None, false, None, None, 2, None, 0.0, 0.5)
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

/// The BFS regression fixture: three ranked seeds, six neighbors, mixed edge
/// kinds and weights, one edge between seeds, a neighbor reached from two
/// seeds, a reverse-written edge, and 2-hop paths.
fn bfs_regression_server() -> (OracleMcpServer, Vec<String>) {
    let db = SearchIndex::open_memory().expect("db");
    for p in [
        "notes/s1.md",
        "notes/s2.md",
        "notes/s3.md",
        "notes/a.md",
        "notes/b.md",
        "notes/c.md",
        "notes/d.md",
        "notes/e.md",
        "notes/f.md",
    ] {
        seed(&db, p, "body");
    }
    let server = OracleMcpServer::new(Config::default(), db);
    {
        let handle = server.db_handle();
        let mut guard = handle.lock().expect("lock");
        let e = |s: &str, d: &str, k: &str, w: f32| Edge::deterministic(s, d, k, w);
        guard
            .insert_edges(&[
                e("notes/s1.md", "notes/a.md", "wikilink", 1.0),
                e("notes/b.md", "notes/s1.md", "wikilink", 0.8),
                e("notes/s1.md", "notes/s2.md", "wikilink", 1.0),
                e("notes/s2.md", "notes/a.md", "semantic", 0.7),
                e("notes/s2.md", "notes/c.md", "wikilink", 0.6),
                e("notes/a.md", "notes/d.md", "wikilink", 0.9),
                e("notes/c.md", "notes/d.md", "shared-tag", 0.35),
                e("notes/s3.md", "notes/e.md", "wikilink", 0.45),
                e("notes/e.md", "notes/f.md", "semantic", 0.3),
                e("notes/b.md", "notes/f.md", "wikilink", 1.0),
            ])
            .expect("insert edges");
    }
    let seeds = vec![
        "notes/s1.md".to_string(),
        "notes/s2.md".to_string(),
        "notes/s3.md".to_string(),
    ];
    (server, seeds)
}

/// BFS scores on the regression fixture are bit-identical to the scorer as
/// it stood before PPR was added: the f32 bit patterns below were captured
/// from that code (2 hops, all kinds, min weight 0, hop decay 0.5).
#[test]
fn bfs_scores_unchanged() {
    let (server, seeds) = bfs_regression_server();
    let handle = server.db_handle();
    let guard = handle.lock().expect("lock");
    let scored = OracleMcpServer::bfs_scores(&guard, &seeds, 2, None, 0.0, 0.5).expect("bfs");
    let bits: Vec<(&str, u32)> = scored.iter().map(|(p, s)| (p.as_str(), s.to_bits())).collect();
    assert_eq!(
        bits,
        vec![
            ("notes/a.md", 1068289229), // 1.35
            ("notes/b.md", 1061997773), // 0.8
            ("notes/d.md", 1057006551), // 0.5025
            ("notes/f.md", 1054364140), // 0.4225
            ("notes/c.md", 1050253722), // 0.3
            ("notes/e.md", 1041865114), // 0.15
        ]
    );
    let paths = server
        .expand_to_graph_paths(&guard, &seeds, None, false, None, None, 2, None, 0.0, 0.5)
        .expect("expand");
    assert_eq!(
        paths,
        scored.iter().map(|(p, _)| p.clone()).collect::<Vec<_>>(),
        "the graph list is the scored order"
    );
}

/// PPR on the same fixture: seeds never appear, every non-seed neighbor is
/// ranked, and the edge-kind filter is honored.
#[test]
fn ppr_graph_paths_ranks_non_seed_neighbors() {
    let (server, seeds) = bfs_regression_server();
    let handle = server.db_handle();
    let guard = handle.lock().expect("lock");
    let params = GraphMethod::default().ppr_params();
    let all = server
        .ppr_graph_paths(&guard, &seeds, None, false, None, None, None, 0.0, &params)
        .expect("ppr");
    let mut sorted = all.clone();
    sorted.sort();
    assert_eq!(
        sorted,
        [
            "notes/a.md",
            "notes/b.md",
            "notes/c.md",
            "notes/d.md",
            "notes/e.md",
            "notes/f.md"
        ]
        .map(String::from)
        .to_vec()
    );
    // a hangs off both top seeds; it leads.
    assert_eq!(all[0], "notes/a.md", "{all:?}");

    let wikilink = vec!["wikilink".to_string()];
    let links_only = server
        .ppr_graph_paths(&guard, &seeds, None, false, None, None, Some(&wikilink), 0.0, &params)
        .expect("ppr");
    // f is reachable over wikilinks through b; nothing reaches anything through
    // the semantic or shared-tag edges.
    assert!(links_only.contains(&"notes/f.md".to_string()), "{links_only:?}");
    assert!(!links_only.iter().any(|p| p.starts_with("notes/s")), "{links_only:?}");
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

/// Sleeps on every call so the probe's measured per-pair cost is reliably
/// positive, making `projected > 0` deterministic against a zero budget.
struct SlowMock;
impl vault::search::Reranker for SlowMock {
    fn model_id(&self) -> &str {
        "slow-mock"
    }
    fn score(&self, _query: &str, docs: &[&str]) -> eyre::Result<Vec<f32>> {
        std::thread::sleep(std::time::Duration::from_millis(3));
        Ok(vec![0.0; docs.len()])
    }
}

/// A reranker whose probe always errors.
struct ErrMock;
impl vault::search::Reranker for ErrMock {
    fn model_id(&self) -> &str {
        "err-mock"
    }
    fn score(&self, _query: &str, _docs: &[&str]) -> eyre::Result<Vec<f32>> {
        eyre::bail!("probe exploded")
    }
}

/// Two seeded notes and their fused order, for the budget/streak tests.
fn two_note_db() -> (SearchIndex, Vec<String>) {
    let db = SearchIndex::open_memory().expect("db");
    seed(&db, "notes/p0.md", "alpha");
    seed(&db, "notes/p1.md", "query");
    (db, vec!["notes/p0.md".to_string(), "notes/p1.md".to_string()])
}

/// One query through the pure core plus the streak/latch settle step, against
/// caller-owned atomics: over budget (`SlowMock`, 0 ms budget) or within
/// budget (`MockReranker`, unbounded budget).
fn settle_one(over_budget: bool, streak: &AtomicU32, disabled: &AtomicBool) -> (Vec<String>, RerankRun) {
    let (db, fused) = two_note_db();
    let outcome = if over_budget {
        OracleMcpServer::rerank_within_budget(&db, &rerank_cfg(2, 0), "query", fused, &SlowMock)
    } else {
        OracleMcpServer::rerank_within_budget(&db, &rerank_cfg(2, u64::MAX), "query", fused, &MockReranker::new())
    }
    .expect("rerank");
    settle_rerank_outcome(outcome, streak, disabled)
}

/// A projected batch over the budget keeps the fused order and reports
/// `OverBudget` (one query), never `Disable` (the whole process).
#[test]
fn rerank_over_budget_reports_over_budget_not_disable() {
    let (db, fused) = two_note_db();
    let cfg = rerank_cfg(2, 0);

    let outcome = OracleMcpServer::rerank_within_budget(&db, &cfg, "query", fused.clone(), &SlowMock).expect("rerank");
    match outcome {
        RerankOutcome::OverBudget(paths) => assert_eq!(paths, fused),
        _ => panic!("expected OverBudget"),
    }
}

/// A probe scoring error is not a latency blip: the core says `Disable` and the
/// settle step latches the process off on the first occurrence.
#[test]
fn rerank_probe_error_latches_immediately() {
    let (db, fused) = two_note_db();
    let outcome =
        OracleMcpServer::rerank_within_budget(&db, &rerank_cfg(2, u64::MAX), "query", fused.clone(), &ErrMock)
            .expect("rerank");
    let streak = AtomicU32::new(0);
    let disabled = AtomicBool::new(false);
    let (paths, run) = settle_rerank_outcome(outcome, &streak, &disabled);
    assert_eq!(paths, fused);
    assert_eq!(run, RerankRun::Disabled);
    assert!(disabled.load(Ordering::Relaxed));
}

/// One over-budget query fails open for that query only; the next
/// within-budget query reranks and resets the streak.
#[test]
fn rerank_one_over_budget_then_within_resets_streak_without_latch() {
    let streak = AtomicU32::new(0);
    let disabled = AtomicBool::new(false);
    let (_, run) = settle_one(true, &streak, &disabled);
    assert_eq!(run, RerankRun::FailedOpen);
    assert_eq!(streak.load(Ordering::Relaxed), 1);
    let (_, run) = settle_one(false, &streak, &disabled);
    assert_eq!(run, RerankRun::Ran);
    assert_eq!(streak.load(Ordering::Relaxed), 0);
    assert!(!disabled.load(Ordering::Relaxed));
}

/// Two over-budget queries in a row stay below the latch and each returns the
/// fused order.
#[test]
fn rerank_two_over_budget_in_a_row_do_not_latch() {
    let (_, fused) = two_note_db();
    let streak = AtomicU32::new(0);
    let disabled = AtomicBool::new(false);
    for _ in 0..2 {
        let (paths, run) = settle_one(true, &streak, &disabled);
        assert_eq!(paths, fused, "over-budget query returns the fused order");
        assert_eq!(run, RerankRun::FailedOpen);
    }
    assert_eq!(streak.load(Ordering::Relaxed), 2);
    assert!(!disabled.load(Ordering::Relaxed));
}

/// The `RERANK_OVER_BUDGET_LATCH_AFTER`th consecutive over-budget query
/// latches rerank off.
#[test]
fn rerank_three_over_budget_in_a_row_latch() {
    let streak = AtomicU32::new(0);
    let disabled = AtomicBool::new(false);
    let runs: Vec<RerankRun> = (0..3).map(|_| settle_one(true, &streak, &disabled).1).collect();
    assert_eq!(
        runs,
        vec![RerankRun::FailedOpen, RerankRun::FailedOpen, RerankRun::Disabled]
    );
    assert!(disabled.load(Ordering::Relaxed));
}

/// A within-budget query in the middle breaks the streak: over, over, within,
/// over, over never latches.
#[test]
fn rerank_over_over_within_over_over_does_not_latch() {
    let streak = AtomicU32::new(0);
    let disabled = AtomicBool::new(false);
    for over in [true, true, false, true, true] {
        let (_, run) = settle_one(over, &streak, &disabled);
        assert_ne!(run, RerankRun::Disabled);
    }
    assert_eq!(streak.load(Ordering::Relaxed), 2);
    assert!(!disabled.load(Ordering::Relaxed));
}

/// The pure streak decision: within budget resets, the threshold latches.
#[test]
fn next_over_budget_streak_resets_and_latches_at_threshold() {
    assert_eq!(next_over_budget_streak(false, 2), (0, false));
    assert_eq!(next_over_budget_streak(true, 0), (1, false));
    assert_eq!(
        next_over_budget_streak(true, RERANK_OVER_BUDGET_LATCH_AFTER - 1),
        (RERANK_OVER_BUDGET_LATCH_AFTER, true)
    );
}

const MATCHING_NOTES: usize = 60;
const MOCK_MODEL: &str = "mock-limit-v1";

fn mock_embedder() -> Arc<MockEmbedder> {
    Arc::new(MockEmbedder::new(64, MOCK_MODEL))
}

fn matching_path(i: usize) -> String {
    format!("notes/m{i:02}.md")
}

fn neighbor_path(i: usize) -> String {
    format!("notes/n{i:02}.md")
}

/// `MATCHING_NOTES` notes that all match the query `zebra`, with a varying term
/// frequency so bm25 order is non-trivial, each embedded by the mock embedder.
/// With `neighbors`, every match also links to one unembedded note that does
/// not match, so the graph arm has exactly `MATCHING_NOTES` neighbors to reach.
fn limit_index(neighbors: bool) -> SearchIndex {
    let mut db = SearchIndex::open_memory().expect("db");
    let m = mock_embedder();
    db.set_active_embedding(m.model_version(), m.dim()).expect("active");
    for i in 0..MATCHING_NOTES {
        let path = matching_path(i);
        let body = format!("{} filler{i}", "zebra ".repeat(i % 7 + 1));
        db.insert_test_note_full(&path, "article", &body, &body, 100)
            .expect("note");
        let v = m.embed_one(&body).expect("embed");
        db.upsert_embeddings_batch(&[BatchUpsert {
            note_path: &path,
            kind: EmbeddingKind::Summary,
            chunk_index: 0,
            text: &body,
            embedding: &v,
            model_version: m.model_version(),
            source_modified_at: 100,
        }])
        .expect("upsert");
    }
    if neighbors {
        let mut edges = Vec::new();
        for i in 0..MATCHING_NOTES {
            db.insert_test_note_full(&neighbor_path(i), "article", "unrelated", "unrelated", 100)
                .expect("neighbor");
            let mut edge = link_edge(&matching_path(i), &neighbor_path(i));
            edge.kind = "wikilink".to_string();
            edges.push(edge);
        }
        db.insert_edges(&edges).expect("edges");
    }
    db
}

fn limit_server(db: SearchIndex, retrieval: RetrievalConfig) -> OracleMcpServer {
    let cfg = Config {
        retrieval,
        ..Default::default()
    };
    OracleMcpServer::new(cfg, db).with_query_embedder(mock_embedder())
}

fn retrieval_with(vector: bool, bm25: bool, graph: bool) -> RetrievalConfig {
    RetrievalConfig {
        methods: MethodsConfig {
            vector: VectorMethod {
                enabled: vector,
                ..Default::default()
            },
            bm25: Bm25Method {
                enabled: bm25,
                weight: 1.0,
                ..Default::default()
            },
            graph: GraphMethod {
                enabled: graph,
                weight: 1.0,
                ..Default::default()
            },
        },
        rerank: rerank_off(),
        ..Default::default()
    }
}

fn configured_paths(server: &OracleMcpServer, limit: u32) -> Vec<String> {
    let handle = server.db_handle();
    let guard = handle.lock().expect("lock");
    server
        .run_configured_pipeline(&guard, "zebra", None, false, None, None, limit)
        .expect("configured pipeline")
        .into_iter()
        .map(|r| r.path)
        .collect()
}

fn legacy_paths(server: &OracleMcpServer, mode: SearchMode, limit: u32) -> Vec<String> {
    let handle = server.db_handle();
    let guard = handle.lock().expect("lock");
    server
        .run_search_mode(&guard, mode, "zebra", None, false, None, None, limit, 1, None, 0.0)
        .expect("run_search_mode")
        .into_iter()
        .map(|r| r.path)
        .collect()
}

#[test]
fn bm25_only_retrieval_returns_all_sixty_matches_at_limit_100() {
    let server = limit_server(limit_index(false), bm25_only_retrieval(1.0));
    assert_eq!(configured_paths(&server, 100).len(), MATCHING_NOTES);
}

#[test]
fn vector_only_retrieval_returns_all_sixty_matches_at_limit_100() {
    let server = limit_server(limit_index(false), retrieval_with(true, false, false));
    assert_eq!(configured_paths(&server, 100).len(), MATCHING_NOTES);
}

#[test]
fn configured_hybrid_returns_all_sixty_matches_at_limit_100() {
    let server = limit_server(limit_index(false), retrieval_with(true, true, false));
    assert_eq!(configured_paths(&server, 100).len(), MATCHING_NOTES);
}

/// Graph is the only enabled method, so every result is a neighbor reached from
/// a seed: the seed depth and the graph `top-k` both bound the count.
#[test]
fn configured_graph_returns_all_sixty_neighbors_at_limit_100() {
    let server = limit_server(limit_index(true), retrieval_with(false, false, true));
    let paths = configured_paths(&server, 100);
    assert_eq!(paths.len(), MATCHING_NOTES, "{paths:?}");
    assert!(paths.iter().all(|p| p.starts_with("notes/n")), "{paths:?}");
}

/// The configured graph retriever with `algorithm: ppr` returns the same
/// sixty neighbors: PPR is wired into the pipeline and seeded from the same
/// list as BFS.
#[test]
fn configured_graph_ppr_returns_all_sixty_neighbors_at_limit_100() {
    let mut retrieval = retrieval_with(false, false, true);
    retrieval.methods.graph.algorithm = crate::config::GraphAlgorithm::Ppr;
    let server = limit_server(limit_index(true), retrieval);
    let paths = configured_paths(&server, 100);
    assert_eq!(paths.len(), MATCHING_NOTES, "{paths:?}");
    assert!(paths.iter().all(|p| p.starts_with("notes/n")), "{paths:?}");
}

#[test]
fn legacy_hybrid_returns_all_sixty_matches_at_limit_100() {
    let server = limit_server(limit_index(false), Config::default().retrieval);
    assert_eq!(legacy_paths(&server, SearchMode::Hybrid, 100).len(), MATCHING_NOTES);
}

#[test]
fn legacy_graph_returns_all_sixty_matches_at_limit_100() {
    let server = limit_server(limit_index(false), Config::default().retrieval);
    assert_eq!(legacy_paths(&server, SearchMode::Graph, 100).len(), MATCHING_NOTES);
}

#[test]
fn legacy_graph_hybrid_returns_all_sixty_matches_at_limit_100() {
    let server = limit_server(limit_index(false), Config::default().retrieval);
    assert_eq!(
        legacy_paths(&server, SearchMode::GraphHybrid, 100).len(),
        MATCHING_NOTES
    );
}

/// A limit at or below `top-k` retrieves at the same depth as before, so the
/// order is unchanged. The literals were captured on the code before the
/// `max(top-k, limit)` depth change.
#[test]
fn limit_10_order_is_unchanged_by_the_depth_change() {
    let bm25 = limit_server(limit_index(false), bm25_only_retrieval(1.0));
    let hybrid = limit_server(limit_index(false), retrieval_with(true, true, false));
    let legacy = limit_server(limit_index(false), Config::default().retrieval);
    let strs = |paths: Vec<String>| {
        paths
            .iter()
            .map(|p| p.trim_start_matches("notes/").to_string())
            .collect::<Vec<_>>()
    };
    let expect = |names: &[&str]| names.iter().map(|n| n.to_string()).collect::<Vec<_>>();
    assert_eq!(
        strs(configured_paths(&bm25, 10)),
        expect(&[
            "m06.md", "m13.md", "m20.md", "m27.md", "m34.md", "m41.md", "m48.md", "m55.md", "m05.md", "m12.md"
        ]),
        "configured bm25"
    );
    let fused = expect(&[
        "m13.md", "m55.md", "m26.md", "m54.md", "m06.md", "m04.md", "m18.md", "m11.md", "m09.md", "m53.md",
    ]);
    assert_eq!(strs(configured_paths(&hybrid, 10)), fused, "configured hybrid");
    assert_eq!(
        strs(legacy_paths(&legacy, SearchMode::Hybrid, 10)),
        fused,
        "legacy hybrid"
    );
    assert_eq!(
        strs(legacy_paths(&legacy, SearchMode::Graph, 10)),
        fused,
        "legacy graph"
    );
}

/// BM25 receives natural language, not FTS5 syntax: `24/7` and embedded
/// quotes once aborted `sb oracle eval` with an fts5 syntax error. Covers
/// both oracle call sites (`run_search_mode`'s BM25 arm and `bm25_paths` via
/// hybrid's configured bm25-only pipeline).
#[test]
fn bm25_quotes_natural_language_queries() {
    let db = SearchIndex::open_memory().expect("db");
    seed(
        &db,
        "notes/agent.md",
        "how to run an AI coding agent 24/7 securely with the \"best\" setup",
    );
    let cfg = Config {
        retrieval: bm25_only_retrieval(0.3),
        ..Default::default()
    };
    let server = OracleMcpServer::new(cfg, db);
    let handle = server.db_handle();
    let guard = handle.lock().expect("lock");

    for query in ["how to run an AI coding agent 24/7 securely", "the \"best\" setup"] {
        let rows = server
            .run_search_mode(
                &guard,
                SearchMode::Bm25,
                query,
                None,
                false,
                None,
                None,
                10,
                1,
                None,
                0.0,
            )
            .unwrap_or_else(|e| panic!("bm25 mode, query {query:?}: {e:?}"));
        assert_eq!(rows.len(), 1, "bm25 mode, query {query:?}");

        let rows = server
            .run_configured_pipeline(&guard, query, None, false, None, None, 10)
            .unwrap_or_else(|e| panic!("configured pipeline, query {query:?}: {e:?}"));
        assert_eq!(rows.len(), 1, "configured pipeline, query {query:?}");
    }
}

/// With rerank off in config, the traced pipeline reports the stage `Off`.
#[test]
fn traced_pipeline_reports_rerank_off_when_disabled_in_config() {
    let db = SearchIndex::open_memory().expect("db");
    seed(&db, "notes/hit.md", "transformer attention mechanism");
    let mut retrieval = bm25_only_retrieval(1.0);
    retrieval.rerank.enabled = false;
    let cfg = Config {
        retrieval,
        ..Default::default()
    };
    let server = OracleMcpServer::new(cfg, db);
    let handle = server.db_handle();
    let guard = handle.lock().expect("lock");
    let run = server
        .run_configured_pipeline_traced(&guard, "transformer", None, false, None, None, 10)
        .expect("configured pipeline");
    assert_eq!(run.rerank, RerankRun::Off);
    assert_eq!(run.rows.len(), 1);
}

/// A rerank model that cannot load latches the stage off and returns the
/// fused order; the traced run says `Disabled`, never `Ran`. The unknown id
/// fails at the registry lookup, before any download.
#[test]
fn traced_pipeline_reports_rerank_disabled_when_the_model_cannot_load() {
    let db = SearchIndex::open_memory().expect("db");
    seed(&db, "notes/a.md", "transformer attention mechanism");
    seed(&db, "notes/b.md", "transformer encoder stack");
    let mut retrieval = bm25_only_retrieval(1.0);
    retrieval.rerank = RerankConfig {
        enabled: true,
        model: "not-a-cross-encoder".to_string(),
        ..rerank_cfg(5, u64::MAX)
    };
    let cfg = Config {
        retrieval,
        ..Default::default()
    };
    let server = OracleMcpServer::new(cfg, db);
    let handle = server.db_handle();
    let guard = handle.lock().expect("lock");
    let run = server
        .run_configured_pipeline_traced(&guard, "transformer", None, false, None, None, 10)
        .expect("configured pipeline");
    assert_eq!(run.rerank, RerankRun::Disabled);
    assert_eq!(run.rows.len(), 2, "fused order still returned");
}
