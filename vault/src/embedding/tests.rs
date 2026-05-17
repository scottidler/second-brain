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
    let err = embed_query("hello", "not-a-real-model").expect_err("must fail");
    let msg = format!("{err}");
    assert!(msg.contains("unknown embedding model_version"), "got: {msg}");
}
