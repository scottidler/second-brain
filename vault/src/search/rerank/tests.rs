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

/// The latency-budget projection is LINEAR in `n` (no thread division): the
/// candle cross-encoder runs one batched forward over all `n` docs and that
/// single pass already saturates every core, so the one-doc probe also used
/// all cores. This is what oracle's warmup probe compares against
/// `latency-budget-ms` to decide fail-open.
#[test]
fn project_batch_ms_is_linear_in_count() {
    // 50 pairs, 200 ms/pair => 10_000 ms (over a 1500 ms budget): the probe
    // trips and the stage fails open. The old ceil(n/threads) model would have
    // under-projected this to ~400 ms on a 32-thread box and run it anyway.
    let ms = project_batch_ms(200.0, 50);
    assert!((ms - 10_000.0).abs() < 1e-6, "got {ms}");

    // Zero candidates cost nothing.
    assert_eq!(project_batch_ms(200.0, 0), 0.0);
    // A small batch within a generous budget.
    assert_eq!(project_batch_ms(10.0, 4), 40.0);
}

#[test]
fn registry_serves_the_oracle_default_with_an_onnx_parity_proof() {
    let spec = cross_encoder_spec(RERANK_MODEL_ID).expect("default model in registry");
    assert_eq!(spec.hf_repo, "cross-encoder/ms-marco-MiniLM-L6-v2");
    assert!(spec.onnx_parity);
    assert_eq!(cross_encoder_spec("ms-marco-MiniLM-L-12-v2"), None);
}

#[cfg(any(feature = "vec-candle", feature = "vec-onnx"))]
mod resolution {
    use super::super::*;
    use crate::embedding::BackendChoice;

    fn cpu(capable: bool) -> CpuFeatures {
        CpuFeatures {
            avx: true,
            avx2: capable,
            fma: capable,
            ..CpuFeatures::default()
        }
    }

    /// The rerank model resolves through the embedding table with the
    /// cross-encoder registry's parity bit.
    #[test]
    fn rerank_backend_follows_the_resolution_table() {
        let rows = [
            (BackendChoice::Auto, true, Some(Backend::Onnx)),
            (BackendChoice::Auto, false, Some(Backend::Candle)),
            (BackendChoice::Candle, true, Some(Backend::Candle)),
            (BackendChoice::Onnx, true, Some(Backend::Onnx)),
            (BackendChoice::Onnx, false, None),
        ];
        for (requested, capable, expected) in rows {
            let got = resolve_rerank_backend(requested, &cpu(capable), RERANK_MODEL_ID);
            match expected {
                Some(backend) => assert_eq!(got.expect("resolves"), backend, "{requested} capable={capable}"),
                None => assert!(
                    got.expect_err("must fail").to_string().contains("lacks avx2 and fma"),
                    "{requested} capable={capable}"
                ),
            }
        }
    }

    #[test]
    fn unknown_rerank_model_fails_under_every_backend_choice() {
        for requested in [BackendChoice::Auto, BackendChoice::Candle, BackendChoice::Onnx] {
            let err = resolve_rerank_backend(requested, &cpu(true), "bogus")
                .expect_err("unknown model")
                .to_string();
            assert!(err.contains("unknown rerank model"), "{requested}: {err}");
        }
    }

    #[test]
    fn load_and_prefetch_refuse_an_unknown_model_offline() {
        for backend in [Backend::Candle, Backend::Onnx] {
            assert!(load_reranker("bogus", backend, 1, &cpu(true)).is_err(), "{backend}");
            assert!(prefetch_reranker("bogus", backend).is_err(), "{backend}");
        }
    }

    #[test]
    fn get_or_load_refuses_an_incapable_cpu_pinned_to_onnx() {
        let inference = InferenceConfig {
            backend: BackendChoice::Onnx,
            threads: 1,
        };
        let err = get_or_load_reranker(RERANK_MODEL_ID, &inference, &cpu(false))
            .err()
            .expect("no ONNX on an AVX-only CPU")
            .to_string();
        assert!(err.contains("lacks avx2 and fma"), "{err}");
    }
}
