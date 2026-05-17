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
    fake.set_error(PATTERN_SHORT, "fabric -p distill-voicenote timed out after 60s");
    let distiller = make_distiller(fake);
    let transcript = "Just a short note about something.";
    let distilled = distiller
        .distill(DistillInputs {
            transcript,
            source_url: None,
            title_hint: None,
            repo_metadata: None,
            video_metadata: None,
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
