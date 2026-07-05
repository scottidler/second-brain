use super::*;
use crate::{FakeFabric, VideoMetadata};
use vault::distilled::KindPayload;

fn make_distiller(fake: FakeFabric) -> VideoDistiller<std::sync::Arc<FakeFabric>> {
    VideoDistiller::new(std::sync::Arc::new(fake), VideoConfig::default())
}

fn timestamped(lines: &[(&str, &str)]) -> String {
    lines
        .iter()
        .map(|(ts, txt)| format!("[{ts}] {txt}"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn attach_payload_attaches_for_repos_only_metadata() {
    // A video whose only metadata is a description repo link still gets a
    // Video payload (the guard now checks repos).
    let mut distilled = crate::fallback_distilled(ID, "test", "transcript", None, "test-model");
    assert!(distilled.kind_specific.is_none());
    let metadata = VideoMetadata {
        channel: None,
        duration_seconds: None,
        published_at: None,
        repos: vec!["owner/repo".to_string()],
    };
    attach_payload(&mut distilled, Some(&metadata));
    let Some(KindPayload::Video(payload)) = distilled.kind_specific else {
        panic!("expected Video payload for repos-only metadata");
    };
    assert_eq!(payload.repos, vec!["owner/repo".to_string()]);
    assert!(payload.channel.is_none());
}

#[test]
fn attach_payload_skips_when_all_fields_empty() {
    let mut distilled = crate::fallback_distilled(ID, "test", "transcript", None, "test-model");
    let metadata = VideoMetadata::default();
    attach_payload(&mut distilled, Some(&metadata));
    assert!(distilled.kind_specific.is_none());
}

#[test]
fn parse_anchor_seconds_handles_hhmmss() {
    assert_eq!(parse_anchor_seconds("00:00:05"), Some(5));
    assert_eq!(parse_anchor_seconds("01:02:03"), Some(3723));
}

#[test]
fn parse_anchor_seconds_handles_mmss() {
    assert_eq!(parse_anchor_seconds("12:34"), Some(754));
}

#[test]
fn parse_anchor_seconds_tolerates_brackets_and_whitespace() {
    assert_eq!(parse_anchor_seconds("  [00:01:00]  "), Some(60));
}

#[test]
fn parse_anchor_seconds_rejects_malformed() {
    assert_eq!(parse_anchor_seconds("not-a-timestamp"), None);
    assert_eq!(parse_anchor_seconds("01:02:03:04"), None);
    assert_eq!(parse_anchor_seconds("99:99"), None);
}

#[test]
fn chunk_transcript_splits_long_text_at_sentence_boundaries() {
    // Target = 4 tokens => 16 char chunks. A sentence boundary is `. `.
    let text = "Hello there. Second sentence here. Third sentence.";
    let chunks = chunk_transcript(text, 4);
    assert!(chunks.len() >= 2, "expected multiple chunks, got: {chunks:?}");
    for (i, c) in chunks.iter().enumerate() {
        assert!(!c.is_empty(), "chunk {i} unexpectedly empty");
    }
    assert_eq!(chunks.concat(), text);
}

#[test]
fn chunk_transcript_handles_short_text_as_single_chunk() {
    let text = "Just one sentence.";
    let chunks = chunk_transcript(text, 1000);
    assert_eq!(chunks, vec![text.to_string()]);
}

#[test]
fn chunk_transcript_falls_back_to_hard_cut_when_no_boundary() {
    let text: String = "x".repeat(40);
    let chunks = chunk_transcript(&text, 1);
    assert!(chunks.len() >= 2);
    assert_eq!(chunks.concat(), text);
}

#[tokio::test]
async fn short_transcript_uses_single_call_path() {
    let fake = FakeFabric::new();
    fake.set_response(
        PATTERN_SHORT,
        r#"
summary: "A talk on consensus."
claims:
  - text: "Raft was introduced as a more teachable alternative to Paxos."
    anchor: "00:00:05"
tags: []
links: []
"#,
    );
    let distiller = make_distiller(fake);
    let transcript = timestamped(&[("00:00:00", "Welcome."), ("00:00:05", "Today we talk about consensus.")]);
    let metadata = VideoMetadata {
        channel: Some("Raft Talks".to_string()),
        duration_seconds: Some(3600),
        published_at: None,
        repos: vec![],
    };
    let distilled = distiller
        .distill(DistillInputs {
            transcript: &transcript,
            source_url: Some("https://youtu.be/abc"),
            title_hint: None,
            repo_metadata: None,
            video_metadata: Some(&metadata),
        })
        .await
        .expect("distill");

    assert_eq!(distilled.meta.extractor, "distill-video-v1");
    assert_eq!(distilled.summary, "A talk on consensus.");
    assert_eq!(distilled.claims.len(), 1);
    assert_eq!(distilled.claims[0].anchor.as_deref(), Some("00:00:05"));
    let Some(KindPayload::Video(payload)) = distilled.kind_specific else {
        panic!("expected Video payload");
    };
    assert_eq!(payload.channel.as_deref(), Some("Raft Talks"));
    assert_eq!(payload.duration_seconds, Some(3600));
}

#[tokio::test]
async fn out_of_range_anchor_is_stripped() {
    let fake = FakeFabric::new();
    fake.set_response(
        PATTERN_SHORT,
        r#"
summary: "A short talk."
claims:
  - text: "In-range claim."
    anchor: "00:00:30"
  - text: "Out-of-range claim that the LLM hallucinated past the runtime."
    anchor: "10:00:00"
  - text: "Garbage anchor claim."
    anchor: "this-is-not-a-timestamp"
tags: []
links: []
"#,
    );
    let distiller = make_distiller(fake);
    let metadata = VideoMetadata {
        channel: None,
        duration_seconds: Some(60),
        published_at: None,
        repos: vec![],
    };
    let distilled = distiller
        .distill(DistillInputs {
            transcript: "short",
            source_url: None,
            title_hint: None,
            repo_metadata: None,
            video_metadata: Some(&metadata),
        })
        .await
        .expect("distill");

    let anchors: Vec<Option<String>> = distilled.claims.iter().map(|c| c.anchor.clone()).collect();
    assert_eq!(
        anchors,
        vec![Some("00:00:30".to_string()), None, None],
        "anchors mismatch"
    );
    assert_eq!(distilled.meta.validation.anchors_stripped, 2);
    assert_eq!(distilled.claims.len(), 3, "claim texts must be retained");
}

#[tokio::test]
async fn fabric_timeout_falls_back() {
    let fake = FakeFabric::new();
    fake.set_timeout(PATTERN_SHORT);
    let distiller = make_distiller(fake);
    let distilled = distiller
        .distill(DistillInputs {
            transcript: "short transcript",
            source_url: None,
            title_hint: None,
            repo_metadata: None,
            video_metadata: None,
        })
        .await
        .expect("distill");

    assert_eq!(
        distilled.meta.validation.fallback_reason.as_deref(),
        Some("fabric-timeout")
    );
}

#[tokio::test]
async fn malformed_yaml_falls_back_with_raw_output() {
    let fake = FakeFabric::new();
    fake.set_response(PATTERN_SHORT, "this is not yaml: [unclosed");
    let distiller = make_distiller(fake);
    let distilled = distiller
        .distill(DistillInputs {
            transcript: "short transcript",
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
    assert!(distilled.meta.validation.raw_output.is_some());
}

#[tokio::test]
async fn long_transcript_takes_map_reduce_path() {
    // Build a transcript that exceeds SINGLE_CALL_TOKEN_THRESHOLD.
    // 60K chars / 4 = 15K tokens, well over the 12K threshold.
    let sentence = "This is a long sentence about consensus protocols and distributed systems. ";
    let transcript = sentence.repeat(800);
    assert!(approx_tokens(transcript.len()) > SINGLE_CALL_TOKEN_THRESHOLD);

    let fake = FakeFabric::new();
    fake.set_response(
        PATTERN_CHUNK,
        r#"
summary: "Chunk summary."
claims:
  - text: "Chunk-level claim."
    anchor: null
tags: []
links: []
"#,
    );
    fake.set_response(
        PATTERN_REDUCE,
        "summary: \"Reduced full-video summary.\"\nclaims:\n  - text: \"A selected synthesis claim.\"\n    anchor: null\n",
    );

    let distiller = make_distiller(fake);
    let distilled = distiller
        .distill(DistillInputs {
            transcript: &transcript,
            source_url: None,
            title_hint: None,
            repo_metadata: None,
            video_metadata: None,
        })
        .await
        .expect("distill");

    assert_eq!(distilled.summary, "Reduced full-video summary.");
    assert!(!distilled.claims.is_empty(), "claims should come from reduce selection");
    assert!(distilled.meta.validation.fallback_reason.is_none());
    // Regression: the map-reduce (long) path must also populate the
    // transcript field so Phase B2 chunk-embedding has a source to work
    // from. Earlier wiring left this `None` and silently dropped every
    // long video from the chunked-embedding pass.
    assert_eq!(
        distilled.transcript.as_deref(),
        Some(transcript.as_str()),
        "distill_long must populate transcript for chunked semantic recall"
    );
}

#[tokio::test]
async fn long_transcript_partial_chunk_failure_keeps_surviving_claims() {
    // One chunk call fails (sequence), the rest distill cleanly (steady); the
    // result keeps surviving claims and flags `partial-chunk-failure`.
    let sentence = "This is a long sentence about consensus protocols and distributed systems. ";
    let transcript = sentence.repeat(800);
    assert!(approx_tokens(transcript.len()) > SINGLE_CALL_TOKEN_THRESHOLD);

    let fake = FakeFabric::new();
    fake.set_response(
        PATTERN_CHUNK,
        "summary: \"Chunk summary.\"\nclaims:\n  - text: \"Chunk-level claim.\"\n    anchor: null\ntags: []\nlinks: []\n",
    );
    fake.set_response_sequence(PATTERN_CHUNK, vec![Err("chunk boom".to_string())]);
    // The reduce selects claims cleanly, so the surviving fallback_reason is
    // the partial-chunk-failure (reduce-selection did NOT fall back).
    fake.set_response(
        PATTERN_REDUCE,
        "summary: \"Reduced full-video summary.\"\nclaims:\n  - text: \"A selected synthesis claim.\"\n    anchor: null\n",
    );

    let distiller = make_distiller(fake);
    let distilled = distiller
        .distill(DistillInputs {
            transcript: &transcript,
            source_url: None,
            title_hint: None,
            repo_metadata: None,
            video_metadata: None,
        })
        .await
        .expect("distill");

    assert_eq!(
        distilled.meta.validation.fallback_reason.as_deref(),
        Some("partial-chunk-failure")
    );
    assert!(!distilled.claims.is_empty());
    assert_eq!(distilled.summary, "Reduced full-video summary.");
}

#[tokio::test]
async fn long_transcript_reduce_failure_falls_back_to_concatenated_summaries() {
    // All chunks succeed but the reduce call fails; the final summary is the
    // concatenation of per-chunk summaries. Phase 5: a failed reduce call also
    // means claim SELECTION never ran, so the claims revert to the chronological
    // chunk merge — head-bias reintroduced — recorded as the distinct
    // `reduce-selection-failed` reason (was `None` pre-Phase-5, when the reduce
    // only touched the summary).
    let sentence = "This is a long sentence about consensus protocols and distributed systems. ";
    let transcript = sentence.repeat(800);
    assert!(approx_tokens(transcript.len()) > SINGLE_CALL_TOKEN_THRESHOLD);

    let fake = FakeFabric::new();
    fake.set_response(
        PATTERN_CHUNK,
        "summary: \"Chunk summary text.\"\nclaims:\n  - text: \"Chunk claim one.\"\n    anchor: null\ntags: []\nlinks: []\n",
    );
    fake.set_error(PATTERN_REDUCE, "reduce boom");

    let distiller = make_distiller(fake);
    let distilled = distiller
        .distill(DistillInputs {
            transcript: &transcript,
            source_url: None,
            title_hint: None,
            repo_metadata: None,
            video_metadata: None,
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
    // The claims fell back to the chronological chunk merge (not empty, not
    // reduce-selected).
    assert!(!distilled.claims.is_empty(), "chronological fallback claims survive");
}

#[tokio::test]
async fn long_transcript_reduce_selects_late_anchor_from_pool() {
    // The pool carries an early AND a late anchor; the reduce SELECTS the late
    // one, and its anchor survives the honesty check because it matches a pool
    // anchor. This is the unit-level proxy for "published claims land in the
    // final third" (a live-model integration criterion).
    let sentence = "This is a long sentence about consensus protocols and distributed systems. ";
    let transcript = sentence.repeat(800);
    assert!(approx_tokens(transcript.len()) > SINGLE_CALL_TOKEN_THRESHOLD);

    let fake = FakeFabric::new();
    fake.set_response(
        PATTERN_CHUNK,
        "summary: \"Chunk summary.\"\nclaims:\n  - text: \"Early claim.\"\n    anchor: \"00:00:05\"\n  - text: \"Late claim near the end.\"\n    anchor: \"00:25:00\"\ntags: []\nlinks: []\n",
    );
    fake.set_response(
        PATTERN_REDUCE,
        "summary: \"Reduced.\"\nclaims:\n  - text: \"Late claim near the end.\"\n    anchor: \"00:25:00\"\n",
    );

    let distiller = make_distiller(fake);
    let distilled = distiller
        .distill(DistillInputs {
            transcript: &transcript,
            source_url: None,
            title_hint: None,
            repo_metadata: None,
            video_metadata: None,
        })
        .await
        .expect("distill");

    assert!(distilled.meta.validation.fallback_reason.is_none());
    assert_eq!(distilled.claims.len(), 1, "only the selected claim survives");
    assert_eq!(distilled.claims[0].anchor.as_deref(), Some("00:25:00"));
    assert_eq!(
        distilled.meta.validation.anchors_stripped, 0,
        "a pool anchor is not stripped"
    );
}

#[tokio::test]
async fn long_transcript_reduce_invented_anchor_stripped_and_counted() {
    // A selected claim whose anchor is NOT in the pool has the anchor stripped
    // to None and counted; the claim TEXT is retained (never dropped as
    // "invented").
    let sentence = "This is a long sentence about consensus protocols and distributed systems. ";
    let transcript = sentence.repeat(800);
    assert!(approx_tokens(transcript.len()) > SINGLE_CALL_TOKEN_THRESHOLD);

    let fake = FakeFabric::new();
    fake.set_response(
        PATTERN_CHUNK,
        "summary: \"Chunk summary.\"\nclaims:\n  - text: \"Pooled claim.\"\n    anchor: \"00:00:05\"\ntags: []\nlinks: []\n",
    );
    fake.set_response(
        PATTERN_REDUCE,
        "summary: \"Reduced.\"\nclaims:\n  - text: \"Reworded claim with a fabricated timestamp.\"\n    anchor: \"09:09:09\"\n",
    );

    let distiller = make_distiller(fake);
    let distilled = distiller
        .distill(DistillInputs {
            transcript: &transcript,
            source_url: None,
            title_hint: None,
            repo_metadata: None,
            video_metadata: None,
        })
        .await
        .expect("distill");

    assert!(distilled.meta.validation.fallback_reason.is_none());
    assert_eq!(distilled.claims.len(), 1, "claim text retained after anchor strip");
    assert!(distilled.claims[0].anchor.is_none(), "invented anchor stripped");
    assert!(
        distilled.claims[0].text.contains("Reworded claim"),
        "claim text preserved verbatim"
    );
    assert_eq!(
        distilled.meta.validation.anchors_stripped, 1,
        "the invented anchor is counted"
    );
}

#[tokio::test]
async fn long_transcript_reduce_empty_selection_falls_back_to_chronological() {
    // Reduce output parses but carries NO claims (empty selection). The claims
    // fall back to the chronological chunk merge, recorded as the distinct
    // reduce-selection-failed reason — NOT folded into bounds_truncations.
    let sentence = "This is a long sentence about consensus protocols and distributed systems. ";
    let transcript = sentence.repeat(800);
    assert!(approx_tokens(transcript.len()) > SINGLE_CALL_TOKEN_THRESHOLD);

    let fake = FakeFabric::new();
    fake.set_response(
        PATTERN_CHUNK,
        "summary: \"Chunk summary.\"\nclaims:\n  - text: \"Chronological claim one.\"\n    anchor: \"00:00:05\"\ntags: []\nlinks: []\n",
    );
    // Summary only — no claims selected.
    fake.set_response(PATTERN_REDUCE, "summary: \"Reduced full-video summary.\"\n");

    let distiller = make_distiller(fake);
    let distilled = distiller
        .distill(DistillInputs {
            transcript: &transcript,
            source_url: None,
            title_hint: None,
            repo_metadata: None,
            video_metadata: None,
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
    assert_eq!(
        distilled.summary, "Reduced full-video summary.",
        "summary still uses the reduce output"
    );
}

#[tokio::test]
async fn long_transcript_reduce_malformed_output_falls_back_to_chronological() {
    // Malformed (unparseable) reduce output → chronological merge, summary
    // falls back to the concatenated chunk summaries, reduce-selection-failed
    // recorded. This is the fallback-path success-criterion test.
    let sentence = "This is a long sentence about consensus protocols and distributed systems. ";
    let transcript = sentence.repeat(800);
    assert!(approx_tokens(transcript.len()) > SINGLE_CALL_TOKEN_THRESHOLD);

    let fake = FakeFabric::new();
    fake.set_response(
        PATTERN_CHUNK,
        "summary: \"Chunk summary text.\"\nclaims:\n  - text: \"Chronological claim.\"\n    anchor: \"00:00:05\"\ntags: []\nlinks: []\n",
    );
    fake.set_response(PATTERN_REDUCE, "this is not yaml: [unclosed");

    let distiller = make_distiller(fake);
    let distilled = distiller
        .distill(DistillInputs {
            transcript: &transcript,
            source_url: None,
            title_hint: None,
            repo_metadata: None,
            video_metadata: None,
        })
        .await
        .expect("distill");

    assert_eq!(
        distilled.meta.validation.fallback_reason.as_deref(),
        Some("reduce-selection-failed")
    );
    assert!(!distilled.claims.is_empty(), "chronological merge claims survive");
    assert!(
        distilled.summary.contains("Chunk summary text."),
        "summary falls back to concatenated chunk summaries: {:?}",
        distilled.summary
    );
}

#[tokio::test]
async fn long_transcript_builds_two_section_reduce_input_with_anchor_pool() {
    // The reduce call receives the two labeled sections; the Claim Pool lists
    // each pooled chunk claim, anchor-prefixed.
    let sentence = "This is a long sentence about consensus protocols and distributed systems. ";
    let transcript = sentence.repeat(800);
    assert!(approx_tokens(transcript.len()) > SINGLE_CALL_TOKEN_THRESHOLD);

    let fake = std::sync::Arc::new(FakeFabric::new());
    fake.set_response(
        PATTERN_CHUNK,
        "summary: \"Chunk summary.\"\nclaims:\n  - text: \"Anchored pool claim.\"\n    anchor: \"00:00:05\"\ntags: []\nlinks: []\n",
    );
    fake.set_response(
        PATTERN_REDUCE,
        "summary: \"Reduced.\"\nclaims:\n  - text: \"Anchored pool claim.\"\n    anchor: \"00:00:05\"\n",
    );

    let distiller = VideoDistiller::new(fake.clone(), VideoConfig::default());
    distiller
        .distill(DistillInputs {
            transcript: &transcript,
            source_url: None,
            title_hint: None,
            repo_metadata: None,
            video_metadata: None,
        })
        .await
        .expect("distill");

    let reduce_call = fake
        .calls()
        .into_iter()
        .find(|c| c.pattern == PATTERN_REDUCE)
        .expect("reduce call recorded");
    assert!(
        reduce_call.input.contains("## Chunk Summaries"),
        "reduce input has summaries section"
    );
    assert!(
        reduce_call.input.contains("## Claim Pool"),
        "reduce input has claim pool section"
    );
    assert!(
        reduce_call.input.contains("[00:00:05] Anchored pool claim."),
        "pool lines are anchor-prefixed: {:?}",
        reduce_call.input
    );
}

#[test]
fn chunk_transcript_handles_multibyte_without_panic() {
    // Regression: find_boundary's fallback returns a raw byte index that can
    // split a multi-byte codepoint; chunk_transcript floors to a char boundary
    // before slicing.
    let transcript = "日本語のテストです。これは文です。".repeat(5000);
    let chunks = chunk_transcript(&transcript, CHUNK_TOKEN_TARGET);
    assert!(chunks.len() > 1, "expected multiple chunks for long multibyte input");
    assert_eq!(chunks.join("").len(), transcript.len());
}

#[tokio::test]
async fn short_transcript_populates_transcript_field() {
    // Sibling regression: assert the short path also lands in transcript.
    let fake = FakeFabric::new();
    fake.set_response(
        PATTERN_SHORT,
        "summary: \"Short video summary.\"\nclaims: []\ntags: []\nlinks: []\n",
    );
    let distiller = make_distiller(fake);
    let transcript = "Short transcript body for the embed pipeline.";
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

    assert_eq!(distilled.transcript.as_deref(), Some(transcript));
}

#[tokio::test]
async fn long_transcript_with_all_chunks_failing_falls_back() {
    let sentence = "Long sentence here padding out the transcript. ";
    let transcript = sentence.repeat(1500);
    assert!(approx_tokens(transcript.len()) > SINGLE_CALL_TOKEN_THRESHOLD);

    let fake = FakeFabric::new();
    fake.set_error(PATTERN_CHUNK, "fabric chunk crashed");
    let distiller = make_distiller(fake);
    let distilled = distiller
        .distill(DistillInputs {
            transcript: &transcript,
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
}

#[tokio::test]
async fn empty_summary_falls_back() {
    let fake = FakeFabric::new();
    fake.set_response(PATTERN_SHORT, "summary: \"\"\nclaims: []\ntags: []\nlinks: []\n");
    let distiller = make_distiller(fake);
    let distilled = distiller
        .distill(DistillInputs {
            transcript: "short",
            source_url: None,
            title_hint: None,
            repo_metadata: None,
            video_metadata: None,
        })
        .await
        .expect("distill");

    assert_eq!(
        distilled.meta.validation.fallback_reason.as_deref(),
        Some("missing-summary")
    );
}

#[tokio::test]
async fn no_metadata_leaves_kind_specific_unset() {
    let fake = FakeFabric::new();
    fake.set_response(PATTERN_SHORT, "summary: \"S\"\nclaims: []\ntags: []\nlinks: []\n");
    let distiller = make_distiller(fake);
    let distilled = distiller
        .distill(DistillInputs {
            transcript: "short",
            source_url: None,
            title_hint: None,
            repo_metadata: None,
            video_metadata: None,
        })
        .await
        .expect("distill");

    assert!(distilled.kind_specific.is_none());
}
