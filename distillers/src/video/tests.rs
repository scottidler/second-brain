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
    fake.set_error(PATTERN_SHORT, "fabric -p distill-video timed out after 60s");
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

    assert_eq!(distilled.summary, "Reduced full-video summary.");
    assert!(!distilled.claims.is_empty(), "claims should be merged from chunks");
    assert!(distilled.meta.validation.fallback_reason.is_none());
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
