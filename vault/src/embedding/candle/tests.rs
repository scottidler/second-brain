//! Unit tests for the Candle pool path.
//!
//! Network-touching tests (real model load via hf-hub) are gated behind
//! `CANDLE_TESTS_REAL=1` so the default `otto ci` run stays offline. The
//! Phase 3 numerical-parity regression test in `vault/tests/regression/`
//! covers the on-disk load+forward path end-to-end against
//! sentence-transformers reference vectors.

#![allow(clippy::unwrap_used)]

use super::*;

#[test]
fn default_worker_count_is_within_bounds() {
    let n = default_worker_count();
    assert!(n >= 1);
    assert!(n <= MAX_WORKERS);
}

#[test]
fn l2_normalize_produces_unit_vectors() {
    let device = Device::Cpu;
    let raw = Tensor::from_vec(vec![3.0_f32, 4.0_f32, -1.0_f32, 2.0_f32], (2, 2), &device).unwrap();
    let normed = l2_normalize(&raw).unwrap();
    let host: Vec<Vec<f32>> = normed.to_vec2().unwrap();
    for row in &host {
        let n: f32 = row.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((n - 1.0).abs() < 1e-5, "row norm {n} != 1.0");
    }
}

/// Optional real-model test: load BAAI/bge-small-en-v1.5 with 4
/// replicas and confirm that batch == per-item embeddings (within fp32
/// tolerance). Skipped unless `CANDLE_TESTS_REAL=1` is set; the network
/// + ~133 MB download keeps this off the default CI path.
#[test]
fn pool_batch_matches_one_at_a_time_real_model() {
    if std::env::var("CANDLE_TESTS_REAL").unwrap_or_default() != "1" {
        eprintln!(
            "skipping pool_batch_matches_one_at_a_time_real_model; set \
             CANDLE_TESTS_REAL=1 to run (downloads ~133 MB)"
        );
        return;
    }
    let model = CandleBertModel::load_with_workers(4).expect("load");
    let inputs = ["alpha", "beta", "gamma", "delta", "epsilon", "zeta", "eta", "theta"];
    let batched = model.embed_batch(&inputs).expect("batch");
    assert_eq!(batched.len(), inputs.len());
    for (i, text) in inputs.iter().enumerate() {
        let single = model.embed_one(text).expect("single");
        let cos: f32 = batched[i].iter().zip(single.iter()).map(|(a, b)| a * b).sum();
        let dist = 1.0 - cos;
        assert!(
            dist < 1e-4,
            "pool result drifted from single-input at i={i}: cos_dist={dist:.6}"
        );
    }
}
