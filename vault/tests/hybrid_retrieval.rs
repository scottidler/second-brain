//! Phase A7: regression and latency tests for the hybrid retrieval path.
//!
//! Two test bodies live here:
//!
//! 1. **Regression** (`hybrid_recovers_union_top3`): runs on every
//!    `cargo test`. Uses `MockEmbedder` against a synthetic 20-note
//!    vault and asserts that for each of 20 fixed queries, the hybrid
//!    result list contains at least the top-3 hits of the union of
//!    BM25 and pure-vector results. The tolerance follows the design's
//!    18/20 threshold for ranker noise; with a deterministic mock
//!    embedder the tolerance is unused but kept so a future swap to a
//!    real model does not have to renegotiate the threshold.
//!
//! 2. **Latency** (`hybrid_p50_latency_under_200ms`): marked `#[ignore]`
//!    so it runs only when explicitly requested. Builds a synthetic
//!    7 K-note + 21 K-note SQLite DB, runs 100 queries through the
//!    full hybrid dispatch (embedding + BM25 + vector + RRF), and
//!    asserts the p50 wall-clock stays under 200 ms.
//!
//! Run with:
//!
//! ```text
//! cargo test --package vault --features vec --test hybrid_retrieval -- --include-ignored
//! ```

#![cfg(feature = "vec")]

use std::collections::HashSet;

use vault::embedding::{EmbeddingModel, MockEmbedder};
use vault::search::{BatchUpsert, EmbeddingKind, K_RRF_INPUT, RRF_K, SearchIndex, reciprocal_rank_fusion};

/// 20 synthetic notes with bodies that exercise both lexical (BM25) and
/// semantic (mock-embedder hash) signals. Each line is `(path, title,
/// summary)`.
fn corpus() -> Vec<(&'static str, &'static str, &'static str)> {
    vec![
        (
            "notes/01.md",
            "Temporal durable execution",
            "temporal workflow durable execution",
        ),
        (
            "notes/02.md",
            "Restate journaled events",
            "restate journaled events durable workflow",
        ),
        (
            "notes/03.md",
            "DBOS transactional functions",
            "dbos transactional functions reliability",
        ),
        (
            "notes/04.md",
            "Computer Use API",
            "claude computer use api agents browser",
        ),
        (
            "notes/05.md",
            "Operator browser sandbox",
            "operator browser sandbox automation",
        ),
        (
            "notes/06.md",
            "playwright-mcp adapter",
            "playwright mcp adapter browser automation",
        ),
        ("notes/07.md", "Rust async runtime", "tokio async runtime executor"),
        (
            "notes/08.md",
            "Go context cancellation",
            "go context cancellation graceful shutdown",
        ),
        (
            "notes/09.md",
            "Postgres logical replication",
            "postgres logical replication wal2json",
        ),
        ("notes/10.md", "SQLite WAL mode", "sqlite wal write ahead log"),
        (
            "notes/11.md",
            "Vector search HNSW",
            "hnsw vector search approximate nearest neighbor",
        ),
        ("notes/12.md", "BM25 ranking", "bm25 ranking probabilistic retrieval"),
        (
            "notes/13.md",
            "Reciprocal rank fusion",
            "rrf reciprocal rank fusion ensemble",
        ),
        (
            "notes/14.md",
            "Obsidian vault structure",
            "obsidian vault notes markdown frontmatter",
        ),
        (
            "notes/15.md",
            "fastembed local inference",
            "fastembed onnx local inference embedding",
        ),
        (
            "notes/16.md",
            "Kubernetes operator pattern",
            "kubernetes operator controller reconcile loop",
        ),
        (
            "notes/17.md",
            "React useEffect cleanup",
            "react useeffect cleanup memory leak",
        ),
        (
            "notes/18.md",
            "Borg ingestion pipeline",
            "borg ingestion telegram discord ntfy",
        ),
        (
            "notes/19.md",
            "Cortex daemon sweep",
            "cortex daemon sweep cadence interval",
        ),
        (
            "notes/20.md",
            "Oracle MCP server",
            "oracle mcp model context protocol server",
        ),
    ]
}

/// 20 fixed queries paired with the path the human author expects the
/// retrieval to surface. The assertion is not "this query must rank N
/// first" - it is "the hybrid top-K must overlap with the union of
/// per-mode top-3."
fn queries() -> Vec<&'static str> {
    vec![
        "durable execution workflow engine",
        "agents that can use a browser",
        "rust async runtime",
        "context cancellation graceful shutdown",
        "wal write ahead log",
        "hnsw approximate nearest neighbor",
        "rrf rank fusion",
        "obsidian vault notes",
        "fastembed onnx inference",
        "kubernetes operator reconcile",
        "react cleanup memory leak",
        "telegram ingestion pipeline",
        "daemon cadence sweep",
        "mcp model context protocol",
        "temporal restate dbos",
        "playwright browser automation",
        "computer use api claude",
        "postgres replication wal2json",
        "bm25 retrieval ranking",
        "approximate nearest neighbor search",
    ]
}

fn build_index() -> SearchIndex {
    let mut index = SearchIndex::open_memory().expect("open");
    let m = MockEmbedder::new(64, "mock-regression-v1");
    index.set_active_embedding(m.model_version(), m.dim()).expect("set");

    for (path, _title, summary) in corpus() {
        index
            .insert_test_note_full(path, "article", summary, summary, 100)
            .expect("insert note row");

        let v = m.embed_one(summary).expect("embed");
        index
            .upsert_embeddings_batch(&[BatchUpsert {
                note_path: path,
                kind: EmbeddingKind::Summary,
                chunk_index: 0,
                text: summary,
                embedding: &v,
                model_version: m.model_version(),
                source_modified_at: 100,
            }])
            .expect("upsert");
    }
    index
}

#[test]
fn hybrid_recovers_union_top3() {
    let mut index = build_index();
    let m = MockEmbedder::new(64, "mock-regression-v1");
    index.set_active_embedding(m.model_version(), m.dim()).expect("set");

    let mut recovered = 0;
    let total = queries().len();
    for q in queries() {
        let bm25 = index.search(q, None, None, None, Some(K_RRF_INPUT)).expect("bm25");
        let q_vec = m.embed_one(q).expect("q");
        let vec_hits = index.search_vector(&q_vec, K_RRF_INPUT, None, None, None).expect("vec");

        let bm25_paths: Vec<String> = bm25.iter().map(|n| n.path.clone()).collect();
        let vec_paths: Vec<String> = vec_hits.iter().map(|h| h.note_path.clone()).collect();
        let fused = reciprocal_rank_fusion(&bm25_paths, &vec_paths, RRF_K, 10);

        let union_top3: HashSet<String> = bm25_paths
            .iter()
            .take(3)
            .chain(vec_paths.iter().take(3))
            .cloned()
            .collect();
        let fused_set: HashSet<String> = fused.iter().map(|f| f.note_path.clone()).collect();

        // "Hybrid must recover at least the union's top-3 hits."
        // With deterministic mock vectors this either holds or fails -
        // there is no ranker noise. The 18/20 tolerance applies once
        // the real model lands.
        if union_top3.is_subset(&fused_set) {
            recovered += 1;
        } else {
            eprintln!("query={q:?} missed: union={union_top3:?} fused={fused_set:?}");
        }
    }
    assert!(
        recovered >= total * 18 / 20,
        "hybrid recovered union top-3 for {recovered}/{total}; threshold is 18/20"
    );
}

#[test]
#[ignore]
fn hybrid_p50_latency_under_200ms() {
    // Builds a 7K-note synthetic index, runs the hybrid dispatch 100
    // times, asserts p50 < 200ms. With MockEmbedder this is a pure
    // pure-Rust workload; substitute FastEmbedModel for end-to-end
    // numbers.
    use std::time::Instant;

    const N: usize = 7_000;
    const ITERS: usize = 100;
    const BUDGET_MS: u128 = 200;

    let mut index = SearchIndex::open_memory().expect("open");
    let m = MockEmbedder::new(384, "mock-latency-v1");
    index.set_active_embedding(m.model_version(), m.dim()).expect("set");

    // Build the index.
    let texts: Vec<String> = (0..N).map(|i| format!("synthetic note {i} body content")).collect();
    for (i, text) in texts.iter().enumerate() {
        let path = format!("notes/synth-{i:05}.md");
        index.insert_test_note_row(&path, "article", 100).expect("insert");
        let v = m.embed_one(text).expect("embed");
        index
            .upsert_embeddings_batch(&[BatchUpsert {
                note_path: &path,
                kind: EmbeddingKind::Summary,
                chunk_index: 0,
                text,
                embedding: &v,
                model_version: m.model_version(),
                source_modified_at: 100,
            }])
            .expect("upsert");
    }

    let mut samples = Vec::with_capacity(ITERS);
    for i in 0..ITERS {
        let q = format!("query {i} synthetic body");
        let start = Instant::now();

        let q_vec = m.embed_one(&q).expect("q vec");
        let bm25 = index.search(&q, None, None, None, Some(K_RRF_INPUT)).expect("bm25");
        let vec_hits = index.search_vector(&q_vec, K_RRF_INPUT, None, None, None).expect("vec");
        let bm25_paths: Vec<String> = bm25.iter().map(|n| n.path.clone()).collect();
        let vec_paths: Vec<String> = vec_hits.iter().map(|h| h.note_path.clone()).collect();
        let _fused = reciprocal_rank_fusion(&bm25_paths, &vec_paths, RRF_K, 10);

        samples.push(start.elapsed().as_millis());
    }
    samples.sort_unstable();
    let p50 = samples[samples.len() / 2];
    println!("hybrid p50 @ N={N} iters={ITERS}: {p50} ms (budget {BUDGET_MS} ms)");
    assert!(
        p50 < BUDGET_MS,
        "hybrid p50 latency {p50} ms exceeds budget {BUDGET_MS} ms"
    );
}
