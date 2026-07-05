use super::*;
use crate::FakeFabric;
use std::sync::Arc;

fn make_distiller(fake: FakeFabric) -> ThreadDistiller<Arc<FakeFabric>> {
    ThreadDistiller::new(Arc::new(fake), ThreadConfig::default())
}

#[tokio::test]
async fn happy_path_parses_yaml_and_attaches_platform() {
    let fake = FakeFabric::new();
    fake.set_response(
        PATTERN,
        r#"
summary: "Thread arguing for typed cross-stage contracts."
claims:
  - text: "Markdown is for humans; cross-stage contracts should be typed."
    anchor: null
  - text: "Reply pushed back on YAML as a contract format."
    anchor: null
tags: []
links:
  - url: "https://example.com/paper"
    label: null
author: "@simonw"
post-count: 7
"#,
    );
    let distiller = make_distiller(fake);
    let distilled = distiller
        .distill(DistillInputs {
            transcript: "Original post plus replies.",
            source_url: Some("https://x.com/simonw/status/12345"),
            title_hint: None,
            repo_metadata: None,
            video_metadata: None,
            capture_note: None,
        })
        .await
        .expect("distill");

    assert_eq!(distilled.meta.extractor, "distill-thread-v1");
    assert!(distilled.summary.starts_with("Thread arguing"));
    assert_eq!(distilled.claims.len(), 2);
    assert_eq!(distilled.links.len(), 1);
    assert!(distilled.meta.validation.fallback_reason.is_none());

    let Some(vault::distilled::KindPayload::Thread(payload)) = distilled.kind_specific else {
        panic!("expected Thread payload");
    };
    assert_eq!(payload.author.as_deref(), Some("@simonw"));
    assert_eq!(payload.post_count, 7);
    assert_eq!(payload.platform, "x");
}

#[tokio::test]
async fn infers_reddit_platform_from_source_url() {
    let fake = FakeFabric::new();
    fake.set_response(
        PATTERN,
        "summary: \"S.\"\nclaims: []\ntags: []\nlinks: []\nauthor: \"u/spez\"\npost-count: 12\n",
    );
    let distiller = make_distiller(fake);
    let distilled = distiller
        .distill(DistillInputs {
            transcript: "Reddit thread body.",
            source_url: Some("https://www.reddit.com/r/rust/comments/abc/def/"),
            title_hint: None,
            repo_metadata: None,
            video_metadata: None,
            capture_note: None,
        })
        .await
        .expect("distill");
    let Some(vault::distilled::KindPayload::Thread(payload)) = distilled.kind_specific else {
        panic!("expected Thread payload");
    };
    assert_eq!(payload.platform, "reddit");
    assert_eq!(payload.post_count, 12);
}

#[tokio::test]
async fn infers_hn_platform_from_source_url() {
    let fake = FakeFabric::new();
    fake.set_response(
        PATTERN,
        "summary: \"S.\"\nclaims: []\ntags: []\nlinks: []\nauthor: \"pg\"\npost-count: 30\n",
    );
    let distiller = make_distiller(fake);
    let distilled = distiller
        .distill(DistillInputs {
            transcript: "HN thread body.",
            source_url: Some("https://news.ycombinator.com/item?id=12345"),
            title_hint: None,
            repo_metadata: None,
            video_metadata: None,
            capture_note: None,
        })
        .await
        .expect("distill");
    let Some(vault::distilled::KindPayload::Thread(payload)) = distilled.kind_specific else {
        panic!("expected Thread payload");
    };
    assert_eq!(payload.platform, "hn");
}

#[tokio::test]
async fn infers_twitter_legacy_host() {
    let fake = FakeFabric::new();
    fake.set_response(
        PATTERN,
        "summary: \"S.\"\nclaims: []\ntags: []\nlinks: []\nauthor: null\npost-count: 1\n",
    );
    let distiller = make_distiller(fake);
    let distilled = distiller
        .distill(DistillInputs {
            transcript: "x",
            source_url: Some("https://twitter.com/user/status/1"),
            title_hint: None,
            repo_metadata: None,
            video_metadata: None,
            capture_note: None,
        })
        .await
        .expect("distill");
    let Some(vault::distilled::KindPayload::Thread(payload)) = distilled.kind_specific else {
        panic!("expected Thread payload");
    };
    assert_eq!(payload.platform, "x");
}

#[tokio::test]
async fn unknown_platform_when_source_url_is_none() {
    let fake = FakeFabric::new();
    fake.set_response(
        PATTERN,
        "summary: \"S.\"\nclaims: []\ntags: []\nlinks: []\nauthor: null\npost-count: 0\n",
    );
    let distiller = make_distiller(fake);
    let distilled = distiller
        .distill(DistillInputs {
            transcript: "x",
            source_url: None,
            title_hint: None,
            repo_metadata: None,
            video_metadata: None,
            capture_note: None,
        })
        .await
        .expect("distill");
    let Some(vault::distilled::KindPayload::Thread(payload)) = distilled.kind_specific else {
        panic!("expected Thread payload");
    };
    assert_eq!(payload.platform, "unknown");
}

#[tokio::test]
async fn fabric_timeout_falls_back_with_platform_attached() {
    let fake = FakeFabric::new();
    fake.set_timeout(PATTERN);
    let distiller = make_distiller(fake);
    let distilled = distiller
        .distill(DistillInputs {
            transcript: "Thread body.",
            source_url: Some("https://x.com/u/status/1"),
            title_hint: None,
            repo_metadata: None,
            video_metadata: None,
            capture_note: None,
        })
        .await
        .expect("distill");

    assert_eq!(
        distilled.meta.validation.fallback_reason.as_deref(),
        Some("fabric-timeout")
    );
    let Some(vault::distilled::KindPayload::Thread(payload)) = distilled.kind_specific else {
        panic!("fallback should still attach a Thread payload so cortex sees the platform");
    };
    assert_eq!(payload.platform, "x");
    assert_eq!(payload.post_count, 0);
}

#[tokio::test]
async fn fabric_error_falls_back_with_error_reason() {
    let fake = FakeFabric::new();
    fake.set_error(PATTERN, "fabric -p distill-thread failed: 1");
    let distiller = make_distiller(fake);
    let distilled = distiller
        .distill(DistillInputs {
            transcript: "x",
            source_url: Some("https://www.reddit.com/r/x/comments/y/z/"),
            title_hint: None,
            repo_metadata: None,
            video_metadata: None,
            capture_note: None,
        })
        .await
        .expect("distill");
    assert_eq!(
        distilled.meta.validation.fallback_reason.as_deref(),
        Some("fabric-error")
    );
}

#[tokio::test]
async fn malformed_yaml_falls_back_with_raw_output() {
    let fake = FakeFabric::new();
    fake.set_response(PATTERN, "this is not yaml: [unclosed");
    let distiller = make_distiller(fake);
    let distilled = distiller
        .distill(DistillInputs {
            transcript: "x",
            source_url: Some("https://news.ycombinator.com/item?id=1"),
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
    assert!(distilled.meta.validation.raw_output.is_some());
}

#[tokio::test]
async fn empty_summary_falls_back() {
    let fake = FakeFabric::new();
    fake.set_response(
        PATTERN,
        "summary: \"\"\nclaims: []\ntags: []\nlinks: []\nauthor: null\npost-count: 0\n",
    );
    let distiller = make_distiller(fake);
    let distilled = distiller
        .distill(DistillInputs {
            transcript: "x",
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
        Some("missing-summary")
    );
}

#[tokio::test]
async fn strips_yaml_code_fence() {
    let fake = FakeFabric::new();
    fake.set_response(
        PATTERN,
        "```yaml\nsummary: \"Fenced.\"\nclaims: []\ntags: []\nlinks: []\nauthor: null\npost-count: 0\n```\n",
    );
    let distiller = make_distiller(fake);
    let distilled = distiller
        .distill(DistillInputs {
            transcript: "x",
            source_url: None,
            title_hint: None,
            repo_metadata: None,
            video_metadata: None,
            capture_note: None,
        })
        .await
        .expect("distill");
    assert_eq!(distilled.summary, "Fenced.");
}

#[tokio::test]
async fn truncates_excess_claims_via_enforce_bounds() {
    let fake = FakeFabric::new();
    let mut yaml = String::from("summary: \"S.\"\nclaims:\n");
    for i in 0..15 {
        yaml.push_str(&format!("  - text: \"Claim {i}\"\n    anchor: null\n"));
    }
    yaml.push_str("tags: []\nlinks: []\nauthor: null\npost-count: 0\n");
    fake.set_response(PATTERN, yaml);

    let distiller = make_distiller(fake);
    let distilled = distiller
        .distill(DistillInputs {
            transcript: "x",
            source_url: None,
            title_hint: None,
            repo_metadata: None,
            video_metadata: None,
            capture_note: None,
        })
        .await
        .expect("distill");

    assert_eq!(distilled.claims.len(), crate::validate::max_claims(1));
    assert!(
        distilled
            .meta
            .validation
            .bounds_truncations
            .iter()
            .any(|t| t.starts_with("claims:"))
    );
}

#[tokio::test]
async fn drops_empty_author_and_zero_post_count() {
    let fake = FakeFabric::new();
    fake.set_response(
        PATTERN,
        "summary: \"S.\"\nclaims: []\ntags: []\nlinks: []\nauthor: \"   \"\npost-count: 0\n",
    );
    let distiller = make_distiller(fake);
    let distilled = distiller
        .distill(DistillInputs {
            transcript: "x",
            source_url: Some("https://x.com/u/status/1"),
            title_hint: None,
            repo_metadata: None,
            video_metadata: None,
            capture_note: None,
        })
        .await
        .expect("distill");
    let Some(vault::distilled::KindPayload::Thread(payload)) = distilled.kind_specific else {
        panic!("expected Thread payload");
    };
    assert!(payload.author.is_none());
    assert_eq!(payload.post_count, 0);
}

#[tokio::test]
async fn records_request_pattern_in_fake_history() {
    let fake = Arc::new(FakeFabric::new());
    fake.set_response(
        PATTERN,
        "summary: \"S.\"\nclaims: []\ntags: []\nlinks: []\nauthor: null\npost-count: 1\n",
    );
    let distiller = ThreadDistiller::new(fake.clone(), ThreadConfig::default());
    distiller
        .distill(DistillInputs {
            transcript: "x",
            source_url: Some("https://x.com/u/status/1"),
            title_hint: None,
            repo_metadata: None,
            video_metadata: None,
            capture_note: None,
        })
        .await
        .expect("distill");
    let calls = fake.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].pattern, PATTERN);
}

// ---- Phase 6: map-reduce long path ----

/// A thread transcript above the long-path threshold (>48K chars) whose author
/// handle sits at the very top (the thread head).
fn long_thread_with_head_author() -> String {
    let mut transcript = String::from("@simonw: Cross-stage contracts should be typed, not markdown.\n\n");
    let filler = "Reply post arguing about YAML versus typed contracts in ingestion pipelines. ";
    while transcript.len() < 60_000 {
        transcript.push_str(filler);
    }
    transcript
}

#[tokio::test]
async fn short_thread_stays_on_single_call_path() {
    let transcript = "A short thread body.";
    assert!(approx_tokens(transcript.len()) <= SINGLE_CALL_TOKEN_THRESHOLD);
    let fake = Arc::new(FakeFabric::new());
    fake.set_response(
        PATTERN,
        "summary: \"Short thread summary.\"\nclaims: []\ntags: []\nlinks: []\nauthor: \"@simonw\"\npost-count: 3\n",
    );
    let distiller = ThreadDistiller::new(fake.clone(), ThreadConfig::default());
    let distilled = distiller
        .distill(DistillInputs {
            transcript,
            source_url: Some("https://x.com/simonw/status/1"),
            title_hint: None,
            repo_metadata: None,
            video_metadata: None,
            capture_note: None,
        })
        .await
        .expect("distill");
    let calls = fake.calls();
    assert_eq!(calls.len(), 1, "single-call path issues exactly one call");
    assert_eq!(calls[0].pattern, PATTERN);
    let Some(vault::distilled::KindPayload::Thread(payload)) = distilled.kind_specific else {
        panic!("expected Thread payload");
    };
    assert_eq!(payload.author.as_deref(), Some("@simonw"));
    assert_eq!(payload.post_count, 3);
}

#[tokio::test]
async fn long_thread_preserves_author_and_post_count_through_long_path() {
    // SUCCESS CRITERION: a >32K thread publishes with author/post-count intact
    // through the map-reduce path. The reduce reads them from the `## Thread
    // Head` section and re-emits them; the outer distill attaches them to
    // KindPayload::Thread alongside the inferred platform.
    let transcript = long_thread_with_head_author();
    assert!(
        approx_tokens(transcript.len()) > SINGLE_CALL_TOKEN_THRESHOLD,
        "fixture must route to the long path"
    );

    let fake = Arc::new(FakeFabric::new());
    fake.set_response(
        PATTERN_CHUNK,
        "summary: \"Chunk summary.\"\nclaims:\n  - text: \"A chunk claim.\"\n    anchor: null\n    kind: position\n    who: \"@simonw\"\ntags: []\nlinks: []\n",
    );
    fake.set_response(
        PATTERN_REDUCE,
        "summary: \"Whole-thread synthesis.\"\nclaims:\n  - text: \"Typed contracts beat markdown for cross-stage handoff.\"\n    anchor: null\n    kind: position\n    who: \"@simonw\"\nauthor: \"@simonw\"\npost-count: 42\n",
    );
    let distiller = ThreadDistiller::new(fake.clone(), ThreadConfig::default());
    let distilled = distiller
        .distill(DistillInputs {
            transcript: &transcript,
            source_url: Some("https://x.com/simonw/status/12345"),
            title_hint: None,
            repo_metadata: None,
            video_metadata: None,
            capture_note: None,
        })
        .await
        .expect("distill");

    let Some(vault::distilled::KindPayload::Thread(payload)) = distilled.kind_specific else {
        panic!("long path must still attach a Thread payload");
    };
    assert_eq!(
        payload.author.as_deref(),
        Some("@simonw"),
        "author survives the long path"
    );
    assert_eq!(payload.post_count, 42, "post-count survives the long path");
    assert_eq!(payload.platform, "x", "platform still inferred from the source URL");
    assert!(distilled.meta.validation.fallback_reason.is_none());
    // Zero truncation on the long path.
    assert!(
        !distilled
            .meta
            .validation
            .bounds_truncations
            .iter()
            .any(|t| t.starts_with("input:"))
    );
    // The full thread body is preserved for chunk embeddings.
    assert_eq!(distilled.transcript.as_deref(), Some(transcript.as_str()));
}

#[tokio::test]
async fn long_thread_reduce_input_carries_thread_head() {
    let transcript = long_thread_with_head_author();
    let fake = Arc::new(FakeFabric::new());
    fake.set_response(
        PATTERN_CHUNK,
        "summary: \"Chunk summary.\"\nclaims:\n  - text: \"A chunk claim.\"\n    anchor: null\ntags: []\nlinks: []\n",
    );
    fake.set_response(
        PATTERN_REDUCE,
        "summary: \"Reduced.\"\nclaims:\n  - text: \"A chunk claim.\"\n    anchor: null\nauthor: \"@simonw\"\npost-count: 9\n",
    );
    let distiller = ThreadDistiller::new(fake.clone(), ThreadConfig::default());
    distiller
        .distill(DistillInputs {
            transcript: &transcript,
            source_url: Some("https://x.com/simonw/status/1"),
            title_hint: None,
            repo_metadata: None,
            video_metadata: None,
            capture_note: None,
        })
        .await
        .expect("distill");

    let reduce_call = fake
        .calls()
        .into_iter()
        .find(|c| c.pattern == PATTERN_REDUCE)
        .expect("reduce call recorded");
    assert!(
        reduce_call.input.contains("## Thread Head"),
        "reduce input has the head section"
    );
    assert!(
        reduce_call.input.contains("@simonw"),
        "the head carries the author line where thread metadata lives"
    );
    assert!(reduce_call.input.contains("## Chunk Summaries"));
    assert!(reduce_call.input.contains("## Claim Pool"));
    eprintln!(
        "PHASE6-MEASURE thread reduce-input: {} chars (~{} tokens), {} chunks",
        reduce_call.input.chars().count(),
        approx_tokens(reduce_call.input.len()),
        chunk_transcript(&transcript, CHUNK_TOKEN_TARGET).len(),
    );
}

#[tokio::test]
async fn long_thread_reduce_failure_falls_back_and_keeps_platform() {
    let transcript = long_thread_with_head_author();
    let fake = Arc::new(FakeFabric::new());
    fake.set_response(
        PATTERN_CHUNK,
        "summary: \"Chunk summary text.\"\nclaims:\n  - text: \"A chronological claim.\"\n    anchor: null\ntags: []\nlinks: []\n",
    );
    fake.set_error(PATTERN_REDUCE, "reduce boom");
    let distiller = ThreadDistiller::new(fake.clone(), ThreadConfig::default());
    let distilled = distiller
        .distill(DistillInputs {
            transcript: &transcript,
            source_url: Some("https://www.reddit.com/r/rust/comments/a/b/"),
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
    assert!(!distilled.claims.is_empty(), "chronological merge claims survive");
    let Some(vault::distilled::KindPayload::Thread(payload)) = distilled.kind_specific else {
        panic!("fallback still attaches a Thread payload");
    };
    assert_eq!(payload.platform, "reddit");
    // A failed reduce cannot recover author/post-count; they default (same
    // class as a fabric-failed single call).
    assert!(payload.author.is_none());
    assert_eq!(payload.post_count, 0);
}

#[tokio::test]
async fn sub_threshold_oversize_thread_records_loud_truncation() {
    let filler = "Reply post arguing about typed contracts in ingestion pipelines. ";
    let mut transcript = String::from("@simonw: Original post.\n\n");
    while transcript.len() < 40_000 {
        transcript.push_str(filler);
    }
    assert!(approx_tokens(transcript.len()) <= SINGLE_CALL_TOKEN_THRESHOLD);
    assert!(transcript.chars().count() > ThreadConfig::default().max_chars);

    let fake = Arc::new(FakeFabric::new());
    fake.set_response(
        PATTERN,
        "summary: \"Summary of an oversize single-call thread.\"\nclaims: []\ntags: []\nlinks: []\nauthor: \"@simonw\"\npost-count: 5\n",
    );
    let distiller = ThreadDistiller::new(fake.clone(), ThreadConfig::default());
    let distilled = distiller
        .distill(DistillInputs {
            transcript: &transcript,
            source_url: Some("https://x.com/simonw/status/1"),
            title_hint: None,
            repo_metadata: None,
            video_metadata: None,
            capture_note: None,
        })
        .await
        .expect("distill");

    let char_count = transcript.chars().count();
    let expected_tag = format!("input:{char_count}>{}", ThreadConfig::default().max_chars);
    assert!(
        distilled.meta.validation.bounds_truncations.contains(&expected_tag),
        "loud truncation entry expected, got {:?}",
        distilled.meta.validation.bounds_truncations
    );
    assert!(distilled.meta.validation.fallback_reason.is_none());
}

#[test]
fn infer_platform_handles_subdomain_hosts() {
    assert_eq!(infer_platform(Some("https://mobile.twitter.com/u/status/1")), "x");
    assert_eq!(
        infer_platform(Some("https://old.reddit.com/r/x/comments/y/z/")),
        "reddit"
    );
    assert_eq!(infer_platform(Some("https://www.reddit.com/r/x/")), "reddit");
    assert_eq!(infer_platform(Some("https://news.ycombinator.com/item?id=1")), "hn");
    assert_eq!(infer_platform(Some("https://example.com/anything")), "unknown");
    assert_eq!(infer_platform(None), "unknown");
}
