//! Criterion bench for the hybrid retrieval dispatch path.
//!
//! Replaces the `#[ignore]`'d latency test from `vault/tests/regression/`
//! so the p50 / p95 numbers come from criterion's statistical sampler
//! rather than a hand-rolled `Instant::now()` loop. The corpus is the
//! same as the original test: 7 K synthetic notes seeded once into an
//! in-memory SQLite index, then queried 100 times with the full
//! embedding + BM25 + vector + RRF stack.
//!
//! Run with:
//!
//! ```text
//! cargo bench --package vault --features vec --bench hybrid
//! ```
//!
//! The design's p50 budget is 200 ms wall-clock (see
//! `docs/design/2026-05-16-hybrid-retrieval-fts5-vector-rrf.md`). The
//! bench does not assert that budget; it reports the distribution so
//! the operator can spot regressions in the criterion report. The 200
//! ms budget remains the design contract.

use criterion::{Criterion, criterion_group, criterion_main};

use vault::embedding::{EmbeddingModel, MockEmbedder};
use vault::search::{BatchUpsert, EmbeddingKind, K_RRF_INPUT, RRF_K, SearchIndex, reciprocal_rank_fusion};

const N: usize = 7_000;

fn build_index() -> (SearchIndex, MockEmbedder) {
    let mut index = SearchIndex::open_memory().expect("open");
    let m = MockEmbedder::new(384, "mock-bench-v1");
    index.set_active_embedding(m.model_version(), m.dim()).expect("set");

    for i in 0..N {
        let path = format!("notes/synth-{i:05}.md");
        let text = format!("synthetic note {i} body content");
        index.insert_test_note_row(&path, "article", 100).expect("insert");
        let v = m.embed_one(&text).expect("embed");
        index
            .upsert_embeddings_batch(&[BatchUpsert {
                note_path: &path,
                kind: EmbeddingKind::Summary,
                chunk_index: 0,
                text: &text,
                embedding: &v,
                model_version: m.model_version(),
                source_modified_at: 100,
            }])
            .expect("upsert");
    }
    (index, m)
}

fn hybrid_dispatch(c: &mut Criterion) {
    let (index, m) = build_index();
    let mut group = c.benchmark_group("hybrid_retrieval");
    // Hybrid dispatch is the bottleneck path that the 200 ms budget
    // gates; sample size 100 mirrors the original loop count and gives
    // criterion enough data to surface tail-latency drift.
    group.sample_size(100);
    group.bench_function("dispatch_n7000", |b| {
        let mut i: usize = 0;
        b.iter(|| {
            let q = format!("query {i} synthetic body");
            i = i.wrapping_add(1);
            let q_vec = m.embed_one(&q).expect("q vec");
            let bm25 = index.search(&q, None, None, None, Some(K_RRF_INPUT)).expect("bm25");
            let vec_hits = index.search_vector(&q_vec, K_RRF_INPUT, None, None, None).expect("vec");
            let bm25_paths: Vec<String> = bm25.iter().map(|n| n.path.clone()).collect();
            let vec_paths: Vec<String> = vec_hits.iter().map(|h| h.note_path.clone()).collect();
            let _fused = reciprocal_rank_fusion(&[&bm25_paths, &vec_paths], RRF_K, 10);
        });
    });
    group.finish();
}

criterion_group!(benches, hybrid_dispatch);
criterion_main!(benches);
