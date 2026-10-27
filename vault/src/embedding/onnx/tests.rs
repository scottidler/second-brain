//! Unit tests for the ONNX embedder.
//!
//! The CPU gate, registry and pooling tests run offline. The real-model
//! tests download `BAAI/bge-small-en-v1.5` (`onnx/model.onnx`, ~133 MB) and
//! are `#[ignore]`d; `otto ignored` runs them by exact name.

#![allow(clippy::unwrap_used)]

use super::*;
use crate::embedding::BGE_SMALL_MODEL_VERSION;

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
    let err = OnnxEmbedder::load_version(BGE_SMALL_MODEL_VERSION, 1, &cpu(false, true))
        .err()
        .unwrap()
        .to_string();
    assert!(err.contains("lacks avx2"), "{err}");
}

#[test]
fn load_refuses_a_cpu_without_fma() {
    let err = OnnxEmbedder::load_version(BGE_SMALL_MODEL_VERSION, 1, &cpu(true, false))
        .err()
        .unwrap()
        .to_string();
    assert!(err.contains("lacks fma"), "{err}");
}

#[test]
fn load_rejects_a_model_without_a_parity_proof_offline() {
    // The registry lookup runs before any download, so this asserts offline.
    for version in ["bge-base-en-v1.5", "bge-small-en-v1.5-candle", "bogus"] {
        let err = OnnxEmbedder::load_version(version, 1, &cpu(true, true))
            .err()
            .unwrap()
            .to_string();
        assert!(err.contains("no ONNX parity proof"), "{version}: {err}");
    }
}

#[test]
fn prefetch_rejects_a_model_without_a_parity_proof_offline() {
    assert!(OnnxEmbedder::prefetch_version("bge-base-en-v1.5").is_err());
}

#[test]
fn supported_model_lists_bge_small_only() {
    let small = supported_model(BGE_SMALL_MODEL_VERSION).expect("bge-small");
    assert_eq!((small.hf_repo, small.dim), ("BAAI/bge-small-en-v1.5", 384));
    // In the registry, but without a parity proof.
    assert!(crate::embedding::model_spec("bge-base-en-v1.5").is_some());
    assert_eq!(supported_model("bge-base-en-v1.5"), None);
    assert_eq!(supported_versions(), vec![BGE_SMALL_MODEL_VERSION]);
}

#[test]
fn zero_threads_resolves_to_the_bounded_default() {
    let n = resolve_threads(0);
    assert_eq!(n, default_thread_count());
    assert!(n >= 1);
    assert!(n <= DEFAULT_ONNX_THREADS);
    assert!(n <= available_parallelism());
}

#[test]
fn explicit_threads_are_capped_at_available_parallelism() {
    assert_eq!(resolve_threads(1), 1);
    assert_eq!(resolve_threads(usize::MAX), available_parallelism());
}

#[test]
fn cls_pool_takes_token_zero_and_normalizes() {
    // batch 2, seq 2, dim 2: CLS rows are [3, 4] and [0, -2].
    let hidden = [3.0, 4.0, 9.0, 9.0, 0.0, -2.0, 7.0, 7.0];
    let pooled = cls_pool_normalize(&[2, 2, 2], &hidden, 2, 2).unwrap();
    assert_eq!(pooled, vec![vec![0.6, 0.8], vec![0.0, -1.0]]);
}

#[test]
fn cls_pool_rejects_a_shape_that_does_not_match_the_model() {
    let hidden = [0.0; 8];
    assert!(cls_pool_normalize(&[2, 4], &hidden, 2, 4).is_err(), "rank 2");
    assert!(cls_pool_normalize(&[2, 2, 2], &hidden, 2, 4).is_err(), "wrong dim");
    assert!(cls_pool_normalize(&[1, 4, 2], &hidden, 2, 2).is_err(), "wrong batch");
    assert!(
        cls_pool_normalize(&[2, 2, 2], &hidden[..6], 2, 2).is_err(),
        "short data"
    );
}

/// Mixed lengths so padding and the attention mask matter, plus one text far
/// past the 512-token truncation limit.
fn mixed_texts() -> Vec<String> {
    vec![
        "alpha".to_string(),
        "Rust async runtimes schedule futures cooperatively on a work-stealing pool.".to_string(),
        "beta gamma".to_string(),
        "An Obsidian vault is a folder of markdown notes linked by wikilinks; \
         the second brain indexes it for keyword and vector search."
            .to_string(),
        "embedding parity across inference backends ".repeat(200),
        "delta".to_string(),
    ]
}

fn cosine_distance(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    (1.0 - dot).abs()
}

fn assert_unit(v: &[f32], what: &str) {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    assert!((norm - 1.0).abs() < 1e-4, "{what}: norm {norm} != 1");
}

/// Real-model test: a mixed-length batch, including a > 512-token text,
/// embeds the same as one text at a time (padding and truncation are inert).
#[test]
#[ignore = "downloads bge-small onnx (~133 MB)"]
fn pool_batch_matches_one_at_a_time_real_model() {
    let model = OnnxEmbedder::load_version(BGE_SMALL_MODEL_VERSION, 4, &CpuFeatures::detect()).expect("load");
    let texts = mixed_texts();
    let inputs: Vec<&str> = texts.iter().map(String::as_str).collect();
    let batched = model.embed_batch(&inputs).expect("batch");
    assert_eq!(batched.len(), inputs.len());
    for (i, text) in inputs.iter().enumerate() {
        let single = model.embed_one(text).expect("single");
        assert_eq!(single.len(), model.dim());
        assert_unit(&batched[i], "batched");
        let dist = cosine_distance(&batched[i], &single);
        assert!(
            dist < 1e-4,
            "batch drifted from single-input at i={i}: cos_dist={dist:.3e}"
        );
    }
}

/// Real-model test: ONNX and Candle embed the same mixed batch to the same
/// vectors. This is what lets both backends serve one model id.
#[cfg(feature = "vec-candle")]
#[test]
#[ignore = "downloads bge-small onnx + safetensors (~266 MB)"]
fn onnx_matches_candle_real_model() {
    let onnx = OnnxEmbedder::load_version(BGE_SMALL_MODEL_VERSION, 4, &CpuFeatures::detect()).expect("load onnx");
    let candle = crate::embedding::CandleBertModel::load_with_workers(2).expect("load candle");
    let texts = mixed_texts();
    let inputs: Vec<&str> = texts.iter().map(String::as_str).collect();
    let from_onnx = onnx.embed_batch(&inputs).expect("onnx batch");
    let from_candle = candle.embed_batch(&inputs).expect("candle batch");
    let mut worst = 0.0_f32;
    for (i, (o, c)) in from_onnx.iter().zip(&from_candle).enumerate() {
        assert_unit(o, "onnx");
        assert_unit(c, "candle");
        let dist = cosine_distance(o, c);
        worst = worst.max(dist);
        assert!(dist < 1e-4, "onnx vs candle at i={i}: cos_dist={dist:.3e}");
    }
    eprintln!("onnx vs candle: max cos_dist {worst:.3e} over {} texts", inputs.len());
}
