use super::*;

#[test]
fn mock_embedder_returns_requested_dim() {
    let m = MockEmbedder::new(16, "test-mock");
    let v = m.embed_one("hello").expect("embed_one");
    assert_eq!(v.len(), 16);
    assert_eq!(m.dim(), 16);
}

#[test]
fn mock_embedder_is_deterministic() {
    let m = MockEmbedder::new(32, "test-mock");
    let a = m.embed_one("the quick brown fox").expect("a");
    let b = m.embed_one("the quick brown fox").expect("b");
    assert_eq!(a, b);
}

#[test]
fn mock_embedder_differentiates_inputs() {
    let m = MockEmbedder::new(32, "test-mock");
    let a = m.embed_one("alpha").expect("a");
    let b = m.embed_one("beta").expect("b");
    assert_ne!(a, b, "different inputs must produce different vectors");
}

#[test]
fn mock_embedder_outputs_are_l2_normalized() {
    let m = MockEmbedder::default_384();
    let v = m.embed_one("temporal restate dbos durable execution").expect("embed");
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    assert!(
        (norm - 1.0).abs() < 1e-5,
        "expected L2-normalized output (norm == 1.0), got {norm}"
    );
}

#[test]
fn mock_embedder_default_384_returns_correct_dim_and_label() {
    let m = MockEmbedder::default_384();
    assert_eq!(m.dim(), BGE_SMALL_EN_V15_DIM);
    assert_eq!(m.model_version(), "mock-bge-small-en-v1.5");
}

#[test]
fn mock_embedder_batch_matches_one_at_a_time() {
    let m = MockEmbedder::new(8, "test-mock");
    let inputs = ["one", "two", "three"];
    let batch = m.embed_batch(&inputs).expect("batch");
    assert_eq!(batch.len(), 3);
    for (i, text) in inputs.iter().enumerate() {
        let single = m.embed_one(text).expect("single");
        assert_eq!(batch[i], single, "batch[{i}] must match embed_one");
    }
}

#[test]
fn mock_embedder_handles_empty_string() {
    let m = MockEmbedder::new(8, "test-mock");
    let v = m.embed_one("").expect("empty");
    assert_eq!(v.len(), 8);
    // Even for the empty string we produce a vector; cosine similarity
    // against it is undefined but the call does not panic or return None.
}

#[cfg(feature = "vec")]
#[test]
fn embed_query_rejects_unknown_model_version() {
    // Use a version string we know is not the canonical bge-small. The
    // function must error cleanly without attempting a download.
    let err = embed_query(
        "hello",
        "not-a-real-model",
        &InferenceConfig::default(),
        &crate::cpu::CpuFeatures::default(),
    )
    .expect_err("must fail");
    let msg = format!("{err}");
    assert!(msg.contains("unknown model_version"), "got: {msg}");
}

#[cfg(feature = "vec")]
#[test]
fn embed_query_onnx_pin_on_incapable_cpu_fails_before_loading() {
    // A CPU lacking AVX2+FMA with `onnx` pinned must error at resolution: no
    // download, no `ort` call, no registry entry.
    let avx_only = crate::cpu::CpuFeatures {
        avx: true,
        ..Default::default()
    };
    let inference = InferenceConfig {
        backend: BackendChoice::Onnx,
        threads: 0,
    };
    let err = embed_query("hello", BGE_SMALL_MODEL_VERSION, &inference, &avx_only)
        .expect_err("onnx pinned on a CPU lacking AVX2+FMA must fail")
        .to_string();
    assert!(err.contains("lacks avx2 and fma"), "got: {err}");
}

#[cfg(feature = "vec")]
#[test]
fn backend_support_follows_the_parity_gate() {
    assert!(is_supported_model_version(BGE_SMALL_MODEL_VERSION, Backend::Candle));
    assert!(is_supported_model_version(BGE_BASE_MODEL_VERSION, Backend::Candle));
    assert!(is_supported_model_version(BGE_SMALL_MODEL_VERSION, Backend::Onnx));
    assert!(!is_supported_model_version(BGE_BASE_MODEL_VERSION, Backend::Onnx));
    assert!(!is_supported_model_version("not-a-real-model", Backend::Candle));
}

#[cfg(feature = "vec")]
#[test]
fn load_model_version_onnx_refuses_incapable_cpu() {
    let err = match load_model_version(
        BGE_SMALL_MODEL_VERSION,
        Backend::Onnx,
        0,
        &crate::cpu::CpuFeatures::default(),
    ) {
        Ok(_) => panic!("an incapable CPU must not load ONNX"),
        Err(e) => e.to_string(),
    };
    assert!(err.contains("lacks avx2 and fma"), "got: {err}");
}
