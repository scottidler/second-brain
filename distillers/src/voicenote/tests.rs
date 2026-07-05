use super::*;
use crate::FakeFabric;
use std::sync::Arc;

fn make_distiller(fake: FakeFabric) -> VoiceNoteDistiller<Arc<FakeFabric>> {
    VoiceNoteDistiller::new(Arc::new(fake), VoiceNoteConfig::default())
}

fn short_config_distiller(fake: FakeFabric) -> VoiceNoteDistiller<Arc<FakeFabric>> {
    VoiceNoteDistiller::new(
        Arc::new(fake),
        VoiceNoteConfig {
            chunk_concurrency: 2,
            ..VoiceNoteConfig::default()
        },
    )
}

#[tokio::test]
async fn short_path_parses_yaml_and_preserves_transcript() {
    let fake = FakeFabric::new();
    fake.set_response(
        PATTERN_SHORT,
        r#"
summary: "A quick voice note about an idea for caching layers."
claims:
  - text: "Cache invalidation drives most production bugs."
    anchor: null
  - text: "Decide whether to use ARC or LRU before next sprint."
    anchor: null
tags: []
links: []
"#,
    );
    let transcript = "So I was thinking about the cache layer, and the real problem is invalidation. We should decide on ARC versus LRU.";
    let distiller = make_distiller(fake);
    let distilled = distiller
        .distill(DistillInputs {
            transcript,
            source_url: None,
            title_hint: Some("cache thoughts"),
            repo_metadata: None,
            video_metadata: None,
            capture_note: None,
        })
        .await
        .expect("distill");

    assert_eq!(distilled.meta.extractor, "distill-voicenote-v1");
    assert!(distilled.summary.starts_with("A quick voice note"));
    assert_eq!(distilled.claims.len(), 2);
    // Voicenote claims never have anchors at this layer.
    assert!(distilled.claims.iter().all(|c| c.anchor.is_none()));
    assert_eq!(distilled.transcript.as_deref(), Some(transcript));
}

#[tokio::test]
async fn short_path_fabric_timeout_falls_back_and_preserves_transcript() {
    let fake = FakeFabric::new();
    fake.set_timeout(PATTERN_SHORT);
    let distiller = make_distiller(fake);
    let transcript = "Just a short note about something.";
    let distilled = distiller
        .distill(DistillInputs {
            transcript,
            source_url: None,
            title_hint: None,
            repo_metadata: None,
            video_metadata: None,
            capture_note: None,
        })
        .await
        .expect("distill");
    assert_eq!(distilled.meta.extractor, "distill-voicenote-v1");
    assert_eq!(
        distilled.meta.validation.fallback_reason.as_deref(),
        Some("fabric-timeout")
    );
    assert_eq!(distilled.transcript.as_deref(), Some(transcript));
}

#[tokio::test]
async fn short_path_malformed_yaml_falls_back_and_preserves_transcript() {
    let fake = FakeFabric::new();
    fake.set_response(PATTERN_SHORT, "this is : : not yaml\nrandom text");
    let distiller = make_distiller(fake);
    let transcript = "Another quick note.";
    let distilled = distiller
        .distill(DistillInputs {
            transcript,
            source_url: None,
            title_hint: None,
            repo_metadata: None,
            video_metadata: None,
            capture_note: None,
        })
        .await
        .expect("distill");
    assert_eq!(
        distilled.meta.validation.fallback_reason.as_deref(),
        Some("yaml-parse-error")
    );
    assert_eq!(distilled.transcript.as_deref(), Some(transcript));
}

#[tokio::test]
async fn long_path_dispatches_chunks_and_reduces_summary() {
    // Construct a transcript that crosses the 12K-token threshold.
    // ~50K chars => ~12.5K tokens at 4 chars/token => triggers long path.
    let long_transcript = "This is a single sentence about an idea. ".repeat(1500);
    assert!(approx_tokens(long_transcript.len()) > SINGLE_CALL_TOKEN_THRESHOLD);

    let fake = FakeFabric::new();
    // Same canned response for every chunk; the chunker may produce multiple
    // chunks but all hit the same pattern call.
    fake.set_response(
        PATTERN_CHUNK,
        r#"
summary: "Chunk discusses an idea."
claims:
  - text: "An idea is worth pursuing."
    anchor: null
tags: []
links: []
"#,
    );
    fake.set_response(
        PATTERN_REDUCE,
        r#"
summary: "Overall the speaker outlined a single idea over a long recording."
claims:
  - text: "The speaker committed to a single idea."
    anchor: null
"#,
    );
    let distiller = short_config_distiller(fake);
    let distilled = distiller
        .distill(DistillInputs {
            transcript: &long_transcript,
            source_url: None,
            title_hint: None,
            repo_metadata: None,
            video_metadata: None,
            capture_note: None,
        })
        .await
        .expect("distill");

    assert_eq!(distilled.meta.extractor, "distill-voicenote-v1");
    assert!(distilled.summary.starts_with("Overall the speaker"));
    // Every chunk contributed at least one claim; multi-chunk transcripts
    // produce multiple combined claims.
    assert!(!distilled.claims.is_empty());
    // Verbatim preservation: the full input survives even on the long path.
    assert_eq!(
        distilled.transcript.as_deref().map(|s| s.len()),
        Some(long_transcript.len())
    );
}

#[tokio::test]
async fn long_path_all_chunks_fail_falls_back_with_transcript() {
    let long_transcript = "An idea. ".repeat(8000);
    assert!(approx_tokens(long_transcript.len()) > SINGLE_CALL_TOKEN_THRESHOLD);

    let fake = FakeFabric::new();
    fake.set_error(PATTERN_CHUNK, "fabric chunk call failed");
    let distiller = short_config_distiller(fake);
    let distilled = distiller
        .distill(DistillInputs {
            transcript: &long_transcript,
            source_url: None,
            title_hint: None,
            repo_metadata: None,
            video_metadata: None,
            capture_note: None,
        })
        .await
        .expect("distill");
    assert_eq!(
        distilled.meta.validation.fallback_reason.as_deref(),
        Some("chunk-failures")
    );
    assert_eq!(
        distilled.transcript.as_deref().map(|s| s.len()),
        Some(long_transcript.len())
    );
}

#[tokio::test]
async fn long_path_partial_chunk_failure_keeps_surviving_claims() {
    // One chunk call fails (sequence), the rest distill cleanly (steady). The
    // result keeps the surviving claims and flags `partial-chunk-failure`.
    let long_transcript = "This is a single sentence about an idea. ".repeat(1500);
    assert!(approx_tokens(long_transcript.len()) > SINGLE_CALL_TOKEN_THRESHOLD);

    let fake = FakeFabric::new();
    fake.set_response(
        PATTERN_CHUNK,
        "summary: \"Chunk discusses an idea.\"\nclaims:\n  - text: \"An idea is worth pursuing.\"\n    anchor: null\ntags: []\nlinks: []\n",
    );
    // First chunk call (whichever lands first) fails; the rest fall through to
    // the steady Ok above.
    fake.set_response_sequence(PATTERN_CHUNK, vec![Err("chunk boom".to_string())]);
    // The reduce selects a claim cleanly, so the surviving fallback_reason is
    // the partial-chunk-failure (reduce-selection did NOT fall back).
    fake.set_response(
        PATTERN_REDUCE,
        "summary: \"Overall the speaker outlined an idea.\"\nclaims:\n  - text: \"The speaker committed to an idea.\"\n    anchor: null\n",
    );
    let distiller = short_config_distiller(fake);
    let distilled = distiller
        .distill(DistillInputs {
            transcript: &long_transcript,
            source_url: None,
            title_hint: None,
            repo_metadata: None,
            video_metadata: None,
            capture_note: None,
        })
        .await
        .expect("distill");

    assert_eq!(
        distilled.meta.validation.fallback_reason.as_deref(),
        Some("partial-chunk-failure")
    );
    // Surviving chunks still contributed claims; the failure didn't nuke them.
    assert!(!distilled.claims.is_empty());
    assert!(distilled.summary.starts_with("Overall the speaker"));
    assert_eq!(
        distilled.transcript.as_deref().map(|s| s.len()),
        Some(long_transcript.len())
    );
}

#[tokio::test]
async fn long_path_reduce_failure_falls_back_to_concatenated_summaries() {
    // All chunks succeed but the reduce call fails; the final summary is the
    // concatenation of per-chunk summaries. Phase 5: a failed reduce call also
    // means claim SELECTION never ran, so the claims revert to the chronological
    // chunk merge — recorded as the distinct `reduce-selection-failed` reason
    // (was `None` pre-Phase-5, when the reduce only touched the summary).
    let long_transcript = "This is a single sentence about an idea. ".repeat(1500);
    assert!(approx_tokens(long_transcript.len()) > SINGLE_CALL_TOKEN_THRESHOLD);

    let fake = FakeFabric::new();
    fake.set_response(
        PATTERN_CHUNK,
        "summary: \"Chunk summary text.\"\nclaims:\n  - text: \"Chunk claim one.\"\n    anchor: null\ntags: []\nlinks: []\n",
    );
    fake.set_error(PATTERN_REDUCE, "reduce boom");
    let distiller = short_config_distiller(fake);
    let distilled = distiller
        .distill(DistillInputs {
            transcript: &long_transcript,
            source_url: None,
            title_hint: None,
            repo_metadata: None,
            video_metadata: None,
            capture_note: None,
        })
        .await
        .expect("distill");

    assert_eq!(
        distilled.meta.validation.fallback_reason.as_deref(),
        Some("reduce-selection-failed"),
        "a failed reduce call must record the distinct reduce-selection-failed reason"
    );
    assert!(
        distilled.summary.contains("Chunk summary text."),
        "summary should fall back to concatenated chunk summaries: {:?}",
        distilled.summary
    );
    assert!(!distilled.claims.is_empty(), "chronological fallback claims survive");
}

#[tokio::test]
async fn long_path_reduce_empty_selection_falls_back_to_chronological() {
    // Reduce output parses but carries NO claims (empty selection). Claims fall
    // back to the chronological chunk merge, recorded as the distinct
    // reduce-selection-failed reason — NOT folded into bounds_truncations.
    // This is the voicenote fallback-path success-criterion test.
    let long_transcript = "This is a single sentence about an idea. ".repeat(1500);
    assert!(approx_tokens(long_transcript.len()) > SINGLE_CALL_TOKEN_THRESHOLD);

    let fake = FakeFabric::new();
    fake.set_response(
        PATTERN_CHUNK,
        "summary: \"Chunk summary text.\"\nclaims:\n  - text: \"Chronological voicenote claim.\"\n    anchor: null\ntags: []\nlinks: []\n",
    );
    fake.set_response(PATTERN_REDUCE, "summary: \"Overall the speaker outlined an idea.\"\n");
    let distiller = short_config_distiller(fake);
    let distilled = distiller
        .distill(DistillInputs {
            transcript: &long_transcript,
            source_url: None,
            title_hint: None,
            repo_metadata: None,
            video_metadata: None,
            capture_note: None,
        })
        .await
        .expect("distill");

    assert_eq!(
        distilled.meta.validation.fallback_reason.as_deref(),
        Some("reduce-selection-failed")
    );
    assert!(
        !distilled
            .meta
            .validation
            .bounds_truncations
            .iter()
            .any(|t| t.contains("reduce-selection")),
        "the fallback reason must be distinct from bounds_truncations"
    );
    assert!(!distilled.claims.is_empty(), "chronological merge claims survive");
}

#[tokio::test]
async fn long_path_reduce_selects_claims_with_no_anchors() {
    // The reduce selects claims; voice-note claims are forced anchorless
    // regardless of what the pattern emitted, and the reduce input carries the
    // two labeled sections.
    let long_transcript = "This is a single sentence about an idea. ".repeat(1500);
    assert!(approx_tokens(long_transcript.len()) > SINGLE_CALL_TOKEN_THRESHOLD);

    let fake = std::sync::Arc::new(FakeFabric::new());
    fake.set_response(
        PATTERN_CHUNK,
        "summary: \"Chunk summary.\"\nclaims:\n  - text: \"Pooled voicenote claim.\"\n    anchor: null\ntags: []\nlinks: []\n",
    );
    fake.set_response(
        PATTERN_REDUCE,
        "summary: \"Overall summary.\"\nclaims:\n  - text: \"A selected synthesis claim.\"\n    anchor: null\n",
    );
    let distiller = VoiceNoteDistiller::new(
        fake.clone(),
        VoiceNoteConfig {
            chunk_concurrency: 2,
            ..VoiceNoteConfig::default()
        },
    );
    let distilled = distiller
        .distill(DistillInputs {
            transcript: &long_transcript,
            source_url: None,
            title_hint: None,
            repo_metadata: None,
            video_metadata: None,
            capture_note: None,
        })
        .await
        .expect("distill");

    assert!(distilled.meta.validation.fallback_reason.is_none());
    assert!(!distilled.claims.is_empty());
    assert!(
        distilled.claims.iter().all(|c| c.anchor.is_none()),
        "voice-note reduce claims carry no anchors"
    );
    let reduce_call = fake
        .calls()
        .into_iter()
        .find(|c| c.pattern == PATTERN_REDUCE)
        .expect("reduce call recorded");
    assert!(reduce_call.input.contains("## Chunk Summaries"));
    assert!(reduce_call.input.contains("## Claim Pool"));
    assert!(
        reduce_call.input.contains("Pooled voicenote claim."),
        "pool lists the chunk claims: {:?}",
        reduce_call.input
    );
}

#[test]
fn chunk_transcript_handles_multibyte_without_panic() {
    // Regression: find_boundary's fallback returns a raw byte index that can
    // split a multi-byte codepoint; chunk_transcript floors to a char boundary
    // before slicing. Each Japanese char is 3 bytes, so cuts land mid-codepoint
    // without the fix.
    let transcript = "日本語のテストです。これは文です。".repeat(5000);
    let chunks = chunk_transcript(&transcript, CHUNK_TOKEN_TARGET);
    assert!(chunks.len() > 1, "expected multiple chunks for long multibyte input");
    assert_eq!(chunks.join("").len(), transcript.len());
}

#[test]
fn chunk_transcript_splits_on_sentence_boundary() {
    let transcript = "First sentence. Second sentence. Third sentence. ".repeat(2000);
    let chunks = chunk_transcript(&transcript, CHUNK_TOKEN_TARGET);
    assert!(chunks.len() > 1, "expected multiple chunks for long transcript");
    // Concatenating chunks reconstitutes the original (modulo whitespace
    // at boundaries that find_boundary cut at).
    let recombined: String = chunks.join("");
    assert_eq!(recombined.len(), transcript.len());
}

#[test]
fn approx_tokens_uses_four_char_rule() {
    assert_eq!(approx_tokens(4), 1);
    assert_eq!(approx_tokens(40), 10);
    assert_eq!(approx_tokens(0), 0);
}
