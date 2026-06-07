use super::*;

/// The mock scores by lexical overlap, so a doc that contains the query's terms
/// reranks above one that does not - and `rerank_paths` returns paths in that
/// order. This is the host-independent stand-in for "rerank reorders a known-
/// relevant doc to the top on a fixture."
#[test]
fn rerank_paths_reorders_by_relevance() {
    let reranker = MockReranker::new();
    // Input order is deliberately "wrong": the irrelevant doc is first.
    let items = vec![
        ("notes/off-topic.md".to_string(), "cooking pasta recipes".to_string()),
        ("notes/on-topic.md".to_string(), "rust async runtime tokio".to_string()),
    ];
    let ranked = rerank_paths(&reranker, "rust async tokio", &items).expect("rerank");
    assert_eq!(
        ranked,
        vec!["notes/on-topic.md".to_string(), "notes/off-topic.md".to_string()],
        "the relevant doc must be reranked to the top"
    );
}

#[test]
fn rerank_paths_empty_input_is_empty() {
    let reranker = MockReranker::new();
    let ranked = rerank_paths(&reranker, "anything", &[]).expect("rerank");
    assert!(ranked.is_empty());
}

#[test]
fn rerank_paths_ties_break_by_path() {
    let reranker = MockReranker::new();
    // Both docs have the same (zero) overlap with the query, so they tie on
    // score; the stable tiebreaker orders by path ascending.
    let items = vec![
        ("notes/b.md".to_string(), "unrelated".to_string()),
        ("notes/a.md".to_string(), "unrelated".to_string()),
    ];
    let ranked = rerank_paths(&reranker, "zzz", &items).expect("rerank");
    assert_eq!(ranked, vec!["notes/a.md".to_string(), "notes/b.md".to_string()]);
}

/// The latency-budget projection: pairs run in `ceil(n/threads)` waves, so the
/// projected cost scales with waves, not raw count. This is what oracle's
/// warmup probe compares against `latency-budget-ms` to decide fail-open.
#[test]
fn project_batch_ms_accounts_for_parallel_waves() {
    // 50 pairs, 200 ms/pair, 32 threads => 2 waves => ~400 ms (well under a
    // 1500 ms budget): the stage would run.
    let ms = project_batch_ms(200.0, 50, 32);
    assert!((ms - 400.0).abs() < 1e-6, "got {ms}");

    // Single-threaded, the same batch is 50 waves => 10_000 ms: the probe trips
    // and the stage fails open.
    let serial = project_batch_ms(200.0, 50, 1);
    assert!((serial - 10_000.0).abs() < 1e-6, "got {serial}");

    // Zero candidates cost nothing.
    assert_eq!(project_batch_ms(200.0, 0, 8), 0.0);
    // Thread count of 0 is treated as 1 (no divide-by-zero).
    assert_eq!(project_batch_ms(10.0, 4, 0), 40.0);
}
