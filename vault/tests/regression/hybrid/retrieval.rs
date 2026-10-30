//! Regression tests for the hybrid retrieval path.
//!
//! These run on every `cargo test --features vec` against a synthetic
//! 20-note vault. For each of 20 fixed queries the hybrid result list
//! must contain at least the top-3 hits of the union of BM25 and pure-
//! vector results. The 18/20 tolerance follows the design's threshold
//! for ranker noise; with the deterministic `MockEmbedder` it is unused
//! but kept so a future swap to a real model does not have to
//! renegotiate the threshold.
//!
//! Latency lives in `vault/benches/hybrid.rs` (criterion-driven) so the
//! statistical work happens where the rest of the bench suite expects
//! it. Run the bench with:
//!
//! ```text
//! cargo bench --package vault --features vec --bench hybrid
//! ```

use std::collections::HashSet;

use vault::embedding::{EmbeddingModel, MockEmbedder};
use vault::search::{BatchUpsert, EmbeddingKind, K_RRF_INPUT, KindOffsets, RRF_K, SearchIndex, reciprocal_rank_fusion};

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
            "embedder local inference",
            "embedder onnx local inference embedding",
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
        "embedder onnx inference",
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

/// Notes whose summary is orthogonal to the query but whose
/// transcript chunks carry the matching tokens. Tests max-pool
/// behavior end-to-end (chunks reachable via vector search after the
/// max-pool aggregation).
fn transcript_corpus() -> Vec<(&'static str, &'static str, &'static str, &'static str)> {
    // (path, note_type, summary, transcript)
    //
    // note_type values are the actual NoteType enum serializations
    // (see `vault::schema::NoteType`); the design doc's conceptual
    // "voice-note"/"idea"/"thread" names do not appear in the DB:
    // VoiceNote -> "audio", Idea -> "note", Thread -> "social"/"reddit".
    vec![
        (
            "notes/v1.md",
            "audio",
            "weekly review of household tasks",
            "I want to schedule a temporal workflow for the inbox triage cron job. \
             Restate could work too but temporal is more battle-tested for this kind \
             of durable execution problem.",
        ),
        (
            "notes/v2.md",
            "audio",
            "morning thoughts on parenting",
            "Discussed hnsw with the team. They prefer brute force at our scale \
             since the index would only be useful past 100k vectors.",
        ),
        (
            "notes/v3.md",
            "note",
            "snippet for the design notes",
            "RRF (reciprocal rank fusion) handles the BM25 plus vector ensemble \
             cleanly. The k=60 constant comes from Cormack 2009.",
        ),
        (
            "notes/video1.md",
            "video",
            "an intro lecture on UI patterns",
            "Around minute 45 the speaker mentions Temporal as the durable execution \
             engine they recommend over Restate and DBOS.",
        ),
        (
            "notes/thread1.md",
            "social",
            "X thread on observability",
            "Several replies reference playwright-mcp as a useful tool for the \
             browser automation layer when you want claude to use a browser.",
        ),
    ]
}

fn transcript_queries() -> Vec<&'static str> {
    vec![
        "temporal workflow durable execution engine",
        "hnsw vector index",
        "rrf reciprocal rank fusion bm25",
        "minute 45 temporal speaker",
        "playwright mcp browser",
    ]
}

/// Summary rows plus transcript-chunk rows, one index per `MockEmbedder`.
fn build_transcript_index(m: &MockEmbedder) -> SearchIndex {
    let mut index = SearchIndex::open_memory().expect("open");
    index.set_active_embedding(m.model_version(), m.dim()).expect("set");

    for (path, ntype, summary, transcript) in transcript_corpus() {
        index
            .insert_test_note_full(path, ntype, summary, summary, 100)
            .expect("note");
        let summary_vec = m.embed_one(summary).expect("summary vec");
        index
            .upsert_embeddings_batch(&[BatchUpsert {
                note_path: path,
                kind: EmbeddingKind::Summary,
                chunk_index: 0,
                text: summary,
                embedding: &summary_vec,
                model_version: m.model_version(),
                source_modified_at: 100,
            }])
            .expect("summary upsert");

        let chunks = m.chunk("", transcript).expect("chunk");
        let chunk_pairs: Vec<(String, Vec<f32>)> = chunks
            .into_iter()
            .map(|c| {
                let v = m.embed_one(&c).expect("chunk vec");
                (c, v)
            })
            .collect();
        if !chunk_pairs.is_empty() {
            index
                .swap_transcript_chunks(path, &chunk_pairs, m.model_version(), 100)
                .expect("swap chunks");
        }
    }
    index
}

#[test]
fn hybrid_recovers_union_top3_with_transcript_chunks() {
    // Build a small vault that mixes summary-only notes with notes
    // whose semantic signal lives in transcript chunks. The
    // max-pool aggregation must let the chunk-only matches surface
    // via the vector path; RRF then keeps them in the fused result.
    let m = MockEmbedder::new(64, "mock-b4-v1");
    let index = build_transcript_index(&m);

    let mut recovered = 0;
    let total = transcript_queries().len();
    for q in transcript_queries() {
        let bm25 = index
            .search(q, None, false, None, None, Some(K_RRF_INPUT))
            .expect("bm25");
        let q_vec = m.embed_one(q).expect("q vec");
        let vec_hits = index
            .search_vector(&q_vec, K_RRF_INPUT, None, false, None, None, &KindOffsets::default())
            .expect("vec");

        let bm25_paths: Vec<String> = bm25.iter().map(|n| n.path.clone()).collect();
        let vec_paths: Vec<String> = vec_hits.iter().map(|h| h.note_path.clone()).collect();
        let fused = reciprocal_rank_fusion(&[&bm25_paths, &vec_paths], RRF_K, 10);

        let union_top3: HashSet<String> = bm25_paths
            .iter()
            .take(3)
            .chain(vec_paths.iter().take(3))
            .cloned()
            .collect();
        let fused_set: HashSet<String> = fused.iter().map(|f| f.note_path.clone()).collect();

        if union_top3.is_subset(&fused_set) {
            recovered += 1;
        } else {
            eprintln!("transcript query={q:?} missed: union={union_top3:?} fused={fused_set:?}");
        }
    }
    assert!(
        recovered >= total * 4 / 5,
        "transcript hybrid recovered {recovered}/{total}; threshold is 4/5"
    );
}

#[test]
fn hybrid_recovers_union_top3() {
    let mut index = build_index();
    let m = MockEmbedder::new(64, "mock-regression-v1");
    index.set_active_embedding(m.model_version(), m.dim()).expect("set");

    let mut recovered = 0;
    let total = queries().len();
    for q in queries() {
        let bm25 = index
            .search(q, None, false, None, None, Some(K_RRF_INPUT))
            .expect("bm25");
        let q_vec = m.embed_one(q).expect("q");
        let vec_hits = index
            .search_vector(&q_vec, K_RRF_INPUT, None, false, None, None, &KindOffsets::default())
            .expect("vec");

        let bm25_paths: Vec<String> = bm25.iter().map(|n| n.path.clone()).collect();
        let vec_paths: Vec<String> = vec_hits.iter().map(|h| h.note_path.clone()).collect();
        let fused = reciprocal_rank_fusion(&[&bm25_paths, &vec_paths], RRF_K, 10);

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

/// The plain per-note min-reduce, written out from the fixture's own texts and
/// independent of `search_vector`: every note's best `1 - cosine` over its
/// summary and chunk vectors, ordered by distance then path.
fn reference_ranking(m: &MockEmbedder, query: &str) -> Vec<(String, f32)> {
    let q = m.embed_one(query).expect("q");
    let dist = |text: &str| -> f32 {
        let v = m.embed_one(text).expect("row vec");
        1.0 - q.iter().zip(&v).map(|(a, b)| a * b).sum::<f32>()
    };
    let mut ranked: Vec<(String, f32)> = transcript_corpus()
        .into_iter()
        .map(|(path, _, summary, transcript)| {
            let best = std::iter::once(dist(summary))
                .chain(m.chunk("", transcript).expect("chunk").iter().map(|c| dist(c)))
                .fold(f32::INFINITY, f32::min);
            (path.to_string(), best)
        })
        .collect();
    ranked.sort_by(|a, b| a.1.partial_cmp(&b.1).expect("finite").then_with(|| a.0.cmp(&b.0)));
    ranked
}

#[test]
fn kind_offsets_zero_is_identity() {
    let m = MockEmbedder::new(64, "mock-b4-v1");
    let index = build_transcript_index(&m);
    let zero = KindOffsets {
        summary: 0.0,
        transcript_chunk: 0.0,
        claim: 0.0,
    };

    let mut chunk_won = 0;
    for q in transcript_queries() {
        let q_vec = m.embed_one(q).expect("q vec");
        let hits = index
            .search_vector(&q_vec, K_RRF_INPUT, None, false, None, None, &zero)
            .expect("vec");
        let default_hits = index
            .search_vector(&q_vec, K_RRF_INPUT, None, false, None, None, &KindOffsets::default())
            .expect("vec");
        let reference = reference_ranking(&m, q);

        let got: Vec<&str> = hits.iter().map(|h| h.note_path.as_str()).collect();
        let want: Vec<&str> = reference.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(got, want, "all-zero offsets must keep today's ranking for {q:?}");
        for (h, (path, d)) in hits.iter().zip(&reference) {
            assert!(
                (h.distance - d).abs() < 1e-5,
                "{path} distance {} drifted from the plain min-reduce {d} for {q:?}",
                h.distance
            );
        }
        assert_eq!(hits.len(), default_hits.len());
        for (a, b) in hits.iter().zip(&default_hits) {
            assert_eq!((&a.note_path, a.distance), (&b.note_path, b.distance));
        }
        // The fixture is only a test of the min-reduce if chunks actually win.
        let summary_only = transcript_corpus()
            .into_iter()
            .map(|(_, _, summary, _)| summary)
            .map(|s| m.embed_one(s).expect("summary vec"))
            .map(|v| 1.0 - q_vec.iter().zip(&v).map(|(a, b)| a * b).sum::<f32>())
            .fold(f32::INFINITY, f32::min);
        if reference[0].1 < summary_only - 1e-6 {
            chunk_won += 1;
        }
    }
    assert!(chunk_won > 0, "the fixture must have a query a chunk row wins");
}
