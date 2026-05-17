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

// --- Phase B1: chunker ------------------------------------------------

#[test]
fn chunk_transcript_returns_empty_for_empty_input() {
    assert!(chunk_transcript("", 100, 10).is_empty());
    assert!(chunk_transcript("   \n\t  ", 100, 10).is_empty());
}

#[test]
fn chunk_transcript_short_input_returns_single_chunk() {
    let text = "alpha beta gamma";
    let chunks = chunk_transcript(text, 100, 10);
    assert_eq!(chunks, vec!["alpha beta gamma".to_string()]);
}

#[test]
fn chunk_transcript_exact_max_tokens_returns_single_chunk() {
    let text = "one two three four five";
    let chunks = chunk_transcript(text, 5, 1);
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0], "one two three four five");
}

#[test]
fn chunk_transcript_long_input_splits_with_overlap() {
    let words: Vec<String> = (0..1000).map(|i| format!("w{i}")).collect();
    let text = words.join(" ");
    let chunks = chunk_transcript(&text, 400, 50);

    // Stride is 350; 1000 words yields chunks starting at 0, 350, 700.
    // Chunk 3 starts at 700 and ends at 1000 (300 words), terminating.
    assert_eq!(
        chunks.len(),
        3,
        "got {} chunks: {:?}",
        chunks.len(),
        chunks.iter().map(|c| c.split_whitespace().count()).collect::<Vec<_>>()
    );

    // Each non-final chunk is exactly max_tokens long.
    for chunk in chunks.iter().take(2) {
        assert_eq!(chunk.split_whitespace().count(), 400);
    }

    // The first 50 words of chunk[1] must equal the last 50 of chunk[0]
    // (the overlap window).
    let last_of_first: Vec<&str> = chunks[0].split_whitespace().rev().take(50).collect();
    let first_of_second: Vec<&str> = chunks[1].split_whitespace().take(50).collect();
    let last_of_first_in_order: Vec<&str> = last_of_first.into_iter().rev().collect();
    assert_eq!(last_of_first_in_order, first_of_second);
}

#[test]
fn chunk_transcript_overlap_larger_than_max_is_clamped() {
    let words: Vec<String> = (0..100).map(|i| format!("w{i}")).collect();
    let text = words.join(" ");
    // overlap_tokens >= max_tokens would deadlock the loop without
    // the saturating_sub clamp.
    let chunks = chunk_transcript(&text, 10, 100);
    assert!(!chunks.is_empty());
    assert!(chunks.len() > 1, "must make forward progress when overlap is huge");
}

#[test]
fn chunk_transcript_single_long_word_returns_one_chunk() {
    // One pathological "word" with no spaces: split_whitespace yields
    // one item, len() <= max_tokens, so it goes back as one chunk.
    let long = "x".repeat(10_000);
    let chunks = chunk_transcript(&long, 50, 5);
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0], long);
}

#[test]
fn chunk_transcript_zero_max_tokens_returns_empty() {
    // Pathological caller input.
    let chunks = chunk_transcript("alpha beta gamma", 0, 0);
    assert!(chunks.is_empty());
}
