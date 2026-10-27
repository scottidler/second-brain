//! Unit tests for the ONNX cross-encoder.
//!
//! The CPU gate, registry and output-shape tests run offline. The ordering
//! parity test downloads `cross-encoder/ms-marco-MiniLM-L6-v2` (both the
//! ONNX export and the safetensors, ~90 MB each) and is `#[ignore]`d;
//! `otto ignored` runs it by exact name.

#![allow(clippy::unwrap_used)]

use super::*;
use crate::search::RERANK_MODEL_ID;

fn cpu(avx2: bool, fma: bool) -> CpuFeatures {
    CpuFeatures {
        avx: true,
        avx2,
        fma,
        ..CpuFeatures::default()
    }
}

#[test]
fn load_refuses_a_cpu_without_avx2() {
    let err = OnnxCrossEncoder::load(RERANK_MODEL_ID, 1, &cpu(false, true))
        .err()
        .unwrap()
        .to_string();
    assert!(err.contains("lacks avx2"), "{err}");
}

#[test]
fn load_refuses_a_cpu_without_fma() {
    let err = OnnxCrossEncoder::load(RERANK_MODEL_ID, 1, &cpu(true, false))
        .err()
        .unwrap()
        .to_string();
    assert!(err.contains("lacks fma"), "{err}");
}

#[test]
fn load_rejects_an_unknown_model_offline() {
    // The registry lookup runs before any download.
    let err = OnnxCrossEncoder::load("ms-marco-MiniLM-L-12-v2", 1, &cpu(true, true))
        .err()
        .unwrap()
        .to_string();
    assert!(err.contains("unknown rerank model"), "{err}");
    assert!(OnnxCrossEncoder::prefetch("bogus").is_err());
}

#[test]
fn single_logits_takes_one_logit_per_pair() {
    assert_eq!(
        single_logits(&[3, 1], &[0.5, -2.0, 7.25], 3).unwrap(),
        vec![0.5, -2.0, 7.25]
    );
}

#[test]
fn single_logits_rejects_a_shape_that_is_not_one_logit_per_pair() {
    assert!(single_logits(&[3], &[0.0; 3], 3).is_err(), "rank 1");
    assert!(single_logits(&[3, 2], &[0.0; 6], 3).is_err(), "two labels");
    assert!(single_logits(&[2, 1], &[0.0; 2], 3).is_err(), "wrong batch");
    assert!(single_logits(&[3, 1], &[0.0; 2], 3).is_err(), "short data");
}

/// One query and docs of clearly different relevance and mixed lengths, so
/// the rank order is well separated and padding matters. The last doc runs
/// far past the 512-token pair limit, so truncation is exercised too.
fn ordering_fixture() -> (&'static str, Vec<String>) {
    let query = "how does tokio schedule async tasks in rust";
    let docs = vec![
        "Tokio is a Rust async runtime. Its multi-threaded scheduler runs tasks on a \
         work-stealing pool: each worker has a local queue and idle workers steal from busy ones."
            .to_string(),
        "Async functions in Rust compile to futures; an executor such as tokio polls them \
         when their wakers fire."
            .to_string(),
        "The Obsidian vault is a folder of markdown notes linked by wikilinks.".to_string(),
        "A sourdough starter needs flour, water, and a warm kitchen for about a week.".to_string(),
        "Rust's borrow checker enforces aliasing rules at compile time.".to_string(),
        "tokio".to_string(),
        "Kubernetes schedules pods onto nodes based on resource requests and affinity rules.".to_string(),
        "Tokio spawns tasks onto its scheduler; blocking work belongs on spawn_blocking. ".repeat(80),
    ];
    (query, docs)
}

/// Ranks `scores` descending; ties (none expected) break by index.
fn rank_order(scores: &[f32]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..scores.len()).collect();
    order.sort_by(|&a, &b| scores[b].partial_cmp(&scores[a]).unwrap().then(a.cmp(&b)));
    order
}

/// The `onnx_parity` proof for the cross-encoder registry: ONNX and Candle
/// score the same fixed pairs in the same rank order, every score within
/// 1e-3. Without it `CROSS_ENCODERS` may not set `onnx_parity`.
#[cfg(feature = "vec-candle")]
#[test]
#[ignore = "downloads ms-marco-MiniLM-L6-v2 onnx + safetensors (~90 MB each)"]
fn onnx_ranks_like_candle_real_model() {
    let onnx = OnnxCrossEncoder::load(RERANK_MODEL_ID, 4, &CpuFeatures::detect()).expect("onnx load");
    let candle = super::super::CandleCrossEncoder::load(RERANK_MODEL_ID).expect("candle load");
    let (query, docs) = ordering_fixture();
    let docs: Vec<&str> = docs.iter().map(String::as_str).collect();

    let onnx_scores = onnx.score(query, &docs).expect("onnx score");
    let candle_scores = candle.score(query, &docs).expect("candle score");
    assert_eq!(onnx_scores.len(), docs.len());
    assert_eq!(candle_scores.len(), docs.len());

    let max_delta = onnx_scores
        .iter()
        .zip(&candle_scores)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0_f32, f32::max);
    eprintln!("onnx={onnx_scores:?}\ncandle={candle_scores:?}\nmax_delta={max_delta:.3e}");
    assert!(max_delta < 1e-3, "max score delta {max_delta:.3e} >= 1e-3");
    assert_eq!(
        rank_order(&onnx_scores),
        rank_order(&candle_scores),
        "ONNX and Candle rank the pairs differently"
    );
    // The fixture must discriminate: the on-topic doc beats the sourdough one.
    assert!(onnx_scores[0] > onnx_scores[3], "fixture does not separate relevance");

    // A batch and one pair at a time agree, so padding is inert.
    for (i, doc) in docs.iter().enumerate() {
        let single = onnx.score(query, &[doc]).expect("single")[0];
        assert!(
            (single - onnx_scores[i]).abs() < 1e-3,
            "batch drifted from single at i={i}: {single} vs {}",
            onnx_scores[i]
        );
    }
}
