//! Config-load tests for the `retrieval:` pipeline section.
//!
//! Covers the three Phase 1 cases from
//! `docs/design/2026-06-06-configurable-retrieval-pipeline.md`:
//! an empty config loads to the eval-best default; a fully specified
//! `retrieval:` block round-trips; unknown keys error.

use super::*;

/// An `oracle.yml` with no `retrieval:` block loads to the eval-best
/// built-in default: vector-only, bm25 demoted (weight 0.3) and off,
/// graph weight 0.0 and off, rrf k=60, no rerank, no transform,
/// stub-exclude on.
#[test]
fn empty_config_loads_eval_best_default() {
    let cfg: Config = serde_yaml::from_str("").expect("empty yaml loads");
    let r = &cfg.retrieval;

    assert!(r.methods.vector.enabled, "vector on by default");
    assert_eq!(r.methods.vector.top_k, 50);

    assert!(!r.methods.bm25.enabled, "bm25 off by default");
    assert_eq!(r.methods.bm25.weight, 0.3);

    assert!(!r.methods.graph.enabled, "graph off by default");
    assert_eq!(r.methods.graph.weight, 0.0, "graph demoted out of fusion");
    assert_eq!(r.methods.graph.hops, 2);
    assert_eq!(r.methods.graph.edge_kinds, vec!["wikilink".to_string()]);

    assert_eq!(r.fusion.method, FusionMethod::Rrf);
    assert_eq!(r.fusion.k, vault::search::RRF_K);

    assert!(!r.rerank.enabled, "rerank off by default");
    assert_eq!(r.rerank.method, RerankMethod::CrossEncoder);
    assert_eq!(r.rerank.input_k, 50);
    assert_eq!(r.rerank.latency_budget_ms, 1500);

    assert!(!r.query_transform.enabled, "transform off by default");
    assert_eq!(r.query_transform.method, TransformMethod::Hyde);
    assert_eq!(r.query_transform.variants, 3);

    assert!(r.exclude.stub, "stub-exclude on by default");
    assert_eq!(r.exclude.min_body_chars, 0);
}

/// A config that omits `retrieval:` entirely but sets other fields still
/// gets the full default pipeline (the field is `#[serde(default)]`).
#[test]
fn config_without_retrieval_block_defaults_pipeline() {
    let yaml = "inbound-recompute-interval-secs: 600\n";
    let cfg: Config = serde_yaml::from_str(yaml).expect("loads");
    assert!(cfg.retrieval.methods.vector.enabled);
    assert!(!cfg.retrieval.methods.bm25.enabled);
}

/// A fully specified `retrieval:` block round-trips every field.
#[test]
fn full_retrieval_block_round_trips() {
    let yaml = r#"
retrieval:
  methods:
    vector:
      enabled: true
      top-k: 75
    bm25:
      enabled: true
      top-k: 40
      weight: 0.5
    graph:
      enabled: true
      top-k: 30
      weight: 0.2
      hops: 1
      hop-decay: 0.25
      min-edge-weight: 0.1
      edge-kinds: [wikilink, semantic]
  fusion:
    method: rrf
    k: 42
  rerank:
    enabled: true
    method: cross-encoder
    model: ms-marco-MiniLM-L6-v2
    input-k: 25
    latency-budget-ms: 800
  query-transform:
    enabled: true
    method: multi-query
    pattern: hyde
    model: llama3
    variants: 5
  exclude:
    stub: false
    min-body-chars: 120
"#;
    let cfg: Config = serde_yaml::from_str(yaml).expect("full block loads");
    let r = &cfg.retrieval;

    assert!(r.methods.vector.enabled);
    assert_eq!(r.methods.vector.top_k, 75);
    assert!(r.methods.bm25.enabled);
    assert_eq!(r.methods.bm25.top_k, 40);
    assert_eq!(r.methods.bm25.weight, 0.5);
    assert!(r.methods.graph.enabled);
    assert_eq!(r.methods.graph.top_k, 30);
    assert_eq!(r.methods.graph.weight, 0.2);
    assert_eq!(r.methods.graph.hops, 1);
    assert_eq!(r.methods.graph.hop_decay, 0.25);
    assert_eq!(r.methods.graph.min_edge_weight, 0.1);
    assert_eq!(
        r.methods.graph.edge_kinds,
        vec!["wikilink".to_string(), "semantic".to_string()]
    );

    assert_eq!(r.fusion.k, 42);

    assert!(r.rerank.enabled);
    assert_eq!(r.rerank.model, "ms-marco-MiniLM-L6-v2");
    assert_eq!(r.rerank.input_k, 25);
    assert_eq!(r.rerank.latency_budget_ms, 800);

    assert!(r.query_transform.enabled);
    assert_eq!(r.query_transform.method, TransformMethod::MultiQuery);
    assert_eq!(r.query_transform.pattern, "hyde");
    assert_eq!(r.query_transform.model, "llama3");
    assert_eq!(r.query_transform.variants, 5);

    assert!(!r.exclude.stub);
    assert_eq!(r.exclude.min_body_chars, 120);
}

/// A partial block fills the rest from defaults: only `bm25.enabled` is
/// set, everything else (including vector) keeps its default.
#[test]
fn partial_retrieval_block_fills_defaults() {
    let yaml = r#"
retrieval:
  methods:
    bm25:
      enabled: true
"#;
    let cfg: Config = serde_yaml::from_str(yaml).expect("partial block loads");
    assert!(cfg.retrieval.methods.bm25.enabled, "explicit field honored");
    assert_eq!(cfg.retrieval.methods.bm25.weight, 0.3, "unspecified field defaulted");
    assert!(cfg.retrieval.methods.vector.enabled, "untouched method defaulted on");
}

/// An unknown key under `retrieval:` is a typo and must error, not be
/// silently ignored (the section is hand-edited operator config).
#[test]
fn unknown_retrieval_key_errors() {
    let yaml = r#"
retrieval:
  bogus-key: true
"#;
    let err = serde_yaml::from_str::<Config>(yaml);
    assert!(err.is_err(), "unknown retrieval key must error: {err:?}");
}

/// An unknown key inside a nested method block must also error.
#[test]
fn unknown_nested_key_errors() {
    let yaml = r#"
retrieval:
  methods:
    vector:
      enabledd: true
"#;
    let err = serde_yaml::from_str::<Config>(yaml);
    assert!(err.is_err(), "unknown nested key must error: {err:?}");
}
