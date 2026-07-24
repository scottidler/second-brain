use super::*;
use crate::FakeFabric;
use std::sync::Arc;

fn make_distiller(fake: FakeFabric, config: SessionConfig) -> SessionDistiller<Arc<FakeFabric>> {
    SessionDistiller::new(Arc::new(fake), config)
}

fn meta() -> SessionMetadata {
    SessionMetadata {
        repo: Some("scottidler/second-brain".to_string()),
        session_ids: vec!["871f6428".to_string(), "4ae69e3a".to_string()],
        msg_count: 806,
        date_start: Some("2026-07-02T09:00:00Z".to_string()),
        date_end: Some("2026-07-02T11:00:00Z".to_string()),
        body_truncated: false,
    }
}

fn inputs<'a>(transcript: &'a str, m: Option<&'a SessionMetadata>) -> DistillInputs<'a> {
    DistillInputs {
        transcript,
        source_url: Some("clyde://871f6428"),
        title_hint: None,
        repo_metadata: None,
        video_metadata: None,
        capture_note: None,
        session_metadata: m,
    }
}

const VALID_YAML: &str = r#"
summary: "The session chose typed cross-stage contracts and rejected an isolated CARGO_TARGET_DIR because it fills the tmpfs."
claims:
  - text: "Chose typed contracts over markdown for cross-stage handoff."
    kind: position
  - text: "Rejected an isolated CARGO_TARGET_DIR; it fills the tmpfs, so reuse the project target."
    kind: recommendation
tags: [rust, ci]
links:
  - url: "https://example.com/design"
    label: "design doc"
"#;

// ---- SUCCESS CRITERION: FakeFabric emits a bounds-valid Distilled ----

#[tokio::test]
async fn fake_fabric_emits_bounds_valid_distilled_with_session_payload() {
    let fake = FakeFabric::new();
    fake.set_response(PATTERN, VALID_YAML);
    let m = meta();
    let distiller = make_distiller(fake, SessionConfig::default());
    let distilled = distiller
        .distill(inputs("USER: hi\nASSISTANT: decided X", Some(&m)))
        .await
        .expect("distill");

    assert_eq!(distilled.meta.extractor, "distill-session-v1");
    assert!(distilled.summary.starts_with("The session chose typed"));
    assert!(distilled.meta.validation.fallback_reason.is_none());
    // Bounds hold.
    assert!(distilled.summary.chars().count() <= MAX_SUMMARY_CHARS);
    assert!(distilled.claims.len() <= max_claims(1));
    assert!(distilled.tags.len() <= 7);
    // Session never carries a note transcript (staged body is the archive).
    assert!(distilled.transcript.is_none());
    // Payload attached from the deterministic Stage-0 metadata.
    let Some(vault::distilled::KindPayload::Session(p)) = distilled.kind_specific else {
        panic!("expected Session payload");
    };
    assert_eq!(p.repo.as_deref(), Some("scottidler/second-brain"));
    assert_eq!(p.session_ids, vec!["871f6428".to_string(), "4ae69e3a".to_string()]);
    assert_eq!(p.msg_count, 806);
}

#[tokio::test]
async fn truncates_excess_claims_via_enforce_bounds() {
    let fake = FakeFabric::new();
    let mut yaml = String::from("summary: \"S.\"\nclaims:\n");
    for i in 0..15 {
        yaml.push_str(&format!("  - text: \"Claim {i}\"\n"));
    }
    yaml.push_str("tags: []\nlinks: []\n");
    fake.set_response(PATTERN, yaml);
    let m = meta();
    let distiller = make_distiller(fake, SessionConfig::default());
    let distilled = distiller.distill(inputs("body", Some(&m))).await.expect("distill");
    assert_eq!(distilled.claims.len(), max_claims(1));
    assert!(
        distilled
            .meta
            .validation
            .bounds_truncations
            .iter()
            .any(|t| t.starts_with("claims:"))
    );
}

// ---- SUCCESS CRITERION: degraded fallback path sets degraded ----

#[tokio::test]
async fn fabric_error_falls_back_and_marks_degraded() {
    let fake = FakeFabric::new();
    fake.set_error(PATTERN, "fabric -p distill-session failed: 1");
    let m = meta();
    let distiller = make_distiller(fake, SessionConfig::default());
    let distilled = distiller.distill(inputs("body", Some(&m))).await.expect("distill");
    assert_eq!(
        distilled.meta.validation.fallback_reason.as_deref(),
        Some("fabric-error")
    );
    // The borg degraded-quality contract: a fallback distillation is degraded.
    assert!(distilled.meta.validation.is_degraded());
    // The payload still rides so cortex/render see the session bookkeeping.
    assert!(matches!(
        distilled.kind_specific,
        Some(vault::distilled::KindPayload::Session(_))
    ));
}

#[tokio::test]
async fn fabric_timeout_falls_back_and_marks_degraded() {
    let fake = FakeFabric::new();
    fake.set_timeout(PATTERN);
    let m = meta();
    let distiller = make_distiller(fake, SessionConfig::default());
    let distilled = distiller.distill(inputs("body", Some(&m))).await.expect("distill");
    assert_eq!(
        distilled.meta.validation.fallback_reason.as_deref(),
        Some("fabric-timeout")
    );
    assert!(distilled.meta.validation.is_degraded());
}

#[tokio::test]
async fn malformed_yaml_falls_back_with_raw_output() {
    let fake = FakeFabric::new();
    fake.set_response(PATTERN, "this is not yaml: [unclosed");
    let m = meta();
    let distiller = make_distiller(fake, SessionConfig::default());
    let distilled = distiller.distill(inputs("body", Some(&m))).await.expect("distill");
    assert_eq!(
        distilled.meta.validation.fallback_reason.as_deref(),
        Some("yaml-parse-error")
    );
    assert!(distilled.meta.validation.raw_output.is_some());
}

// ---- SUCCESS CRITERION: truncated-body fixture shows the marker in the prompt ----

#[tokio::test]
async fn export_body_truncated_injects_marker_into_assembled_prompt() {
    let fake = Arc::new(FakeFabric::new());
    fake.set_response(PATTERN, VALID_YAML);
    let mut m = meta();
    m.body_truncated = true; // clyde cut the transcript tail
    let distiller = SessionDistiller::new(fake.clone(), SessionConfig::default());
    // Short body -> single-call path -> exactly one assembled prompt.
    distiller
        .distill(inputs("USER: short\nASSISTANT: done", Some(&m)))
        .await
        .expect("distill");
    let calls = fake.calls();
    assert_eq!(calls.len(), 1, "single-call path issues one fabric call");
    assert!(
        calls[0].input.contains(TRUNCATION_MARKER),
        "the assembled prompt must carry the truncation marker: {:?}",
        calls[0].input
    );
}

#[tokio::test]
async fn no_marker_when_not_truncated() {
    let fake = Arc::new(FakeFabric::new());
    fake.set_response(PATTERN, VALID_YAML);
    let m = meta(); // body_truncated = false, small body -> no windowing
    let distiller = SessionDistiller::new(fake.clone(), SessionConfig::default());
    distiller
        .distill(inputs("USER: short\nASSISTANT: done", Some(&m)))
        .await
        .expect("distill");
    let calls = fake.calls();
    assert!(
        !calls[0].input.contains(TRUNCATION_MARKER),
        "no marker when nothing was truncated"
    );
}

// ---- Token-cap validation: head+tail windowing on a large ("806-msg") thread ----

/// A body far larger than the default 12K-token cap, standing in for the
/// 806-message golden thread's concatenated transcript.
fn huge_body() -> String {
    let mut body = String::from("USER: HEAD-MARKER kick off the token-broker arc.\n");
    let filler = "ASSISTANT: worked through the pipeline refactor and the receipts migration. ";
    while body.len() < 400_000 {
        body.push_str(filler);
    }
    body.push_str("\nASSISTANT: TAIL-MARKER landed the fix and captured the gotcha.\n");
    body
}

#[test]
fn window_head_tail_keeps_head_and_tail_within_budget() {
    let body = huge_body();
    let token_cap = 12_000usize;
    let (windowed, truncated) = window_head_tail(&body, token_cap);
    assert!(truncated, "a 400K-char body must window under a 12K-token cap");
    // Windowed body stays within the char budget (token_cap * 4 chars/token).
    assert!(
        windowed.chars().count() <= token_cap * 4,
        "windowed body ({} chars) must fit the {}-char budget",
        windowed.chars().count(),
        token_cap * 4
    );
    assert!(windowed.contains(TRUNCATION_MARKER), "marker separates head from tail");
    assert!(windowed.contains("HEAD-MARKER"), "the head is preserved");
    assert!(windowed.contains("TAIL-MARKER"), "the tail is preserved");
}

#[test]
fn window_head_tail_passes_small_bodies_through_unchanged() {
    let body = "USER: tiny\nASSISTANT: done";
    let (windowed, truncated) = window_head_tail(body, 12_000);
    assert!(!truncated);
    assert_eq!(windowed, body);
}

#[tokio::test]
async fn large_thread_windows_to_single_call_under_default_cap() {
    // SUCCESS CRITERION (token-cap validation): with the default 12K cap a very
    // large thread windows down to the single-call path, and the assembled
    // prompt stays within the cap and carries the marker.
    let fake = Arc::new(FakeFabric::new());
    fake.set_response(PATTERN, VALID_YAML);
    let m = meta();
    let body = huge_body();
    let distiller = SessionDistiller::new(fake.clone(), SessionConfig::default());
    let distilled = distiller.distill(inputs(&body, Some(&m))).await.expect("distill");
    assert!(distilled.meta.validation.fallback_reason.is_none());
    let calls = fake.calls();
    assert_eq!(
        calls.len(),
        1,
        "windowed body routes to the single-call path at the default cap"
    );
    assert_eq!(calls[0].pattern, PATTERN);
    assert!(
        calls[0].input.contains(TRUNCATION_MARKER),
        "windowing marker rides the prompt"
    );
    // Prompt input is bounded by the cap (+ small capture framing, which is
    // None here, so it equals the windowed body).
    assert!(approx_tokens(calls[0].input.len()) <= SessionConfig::default().token_cap);
}

// ---- Map-reduce path (live only when token_cap is raised above the threshold) ----

#[tokio::test]
async fn raised_token_cap_routes_to_map_reduce() {
    let fake = Arc::new(FakeFabric::new());
    fake.set_response(
        PATTERN_CHUNK,
        "summary: \"Chunk covered the migration.\"\nclaims:\n  - text: \"A chunk decision.\"\ntags: [ci]\nlinks: []\n",
    );
    fake.set_response(
        PATTERN_REDUCE,
        "summary: \"Whole-session synthesis of the token-broker arc.\"\nclaims:\n  - text: \"A chunk decision.\"\n",
    );
    let m = meta();
    let body = huge_body();
    // token_cap above SINGLE_CALL_TOKEN_THRESHOLD so the windowed body still
    // exceeds the single-call threshold and map-reduce fires.
    let config = SessionConfig {
        token_cap: 30_000,
        ..SessionConfig::default()
    };
    let distiller = SessionDistiller::new(fake.clone(), config);
    let distilled = distiller.distill(inputs(&body, Some(&m))).await.expect("distill");
    assert!(distilled.summary.contains("Whole-session synthesis"));
    assert!(distilled.meta.validation.fallback_reason.is_none());
    let patterns: Vec<String> = fake.calls().into_iter().map(|c| c.pattern).collect();
    assert!(patterns.iter().any(|p| p == PATTERN_CHUNK), "chunk pattern invoked");
    assert!(patterns.iter().any(|p| p == PATTERN_REDUCE), "reduce pattern invoked");
    assert!(
        !patterns.iter().any(|p| p == PATTERN),
        "single-call pattern NOT invoked on the map-reduce path"
    );
}

#[tokio::test]
async fn map_reduce_falls_back_when_reduce_fails() {
    let fake = Arc::new(FakeFabric::new());
    fake.set_response(
        PATTERN_CHUNK,
        "summary: \"Chunk summary.\"\nclaims:\n  - text: \"A chronological claim.\"\ntags: []\nlinks: []\n",
    );
    fake.set_error(PATTERN_REDUCE, "reduce boom");
    let m = meta();
    let body = huge_body();
    let config = SessionConfig {
        token_cap: 30_000,
        ..SessionConfig::default()
    };
    let distiller = SessionDistiller::new(fake.clone(), config);
    let distilled = distiller.distill(inputs(&body, Some(&m))).await.expect("distill");
    assert_eq!(
        distilled.meta.validation.fallback_reason.as_deref(),
        Some("reduce-selection-failed")
    );
    assert!(
        !distilled.claims.is_empty(),
        "chronological merge claims survive a failed reduce"
    );
    assert!(distilled.meta.validation.is_degraded());
}

#[tokio::test]
async fn empty_body_falls_back() {
    let fake = FakeFabric::new();
    fake.set_response(PATTERN, VALID_YAML);
    let m = meta();
    let distiller = make_distiller(fake, SessionConfig::default());
    let distilled = distiller.distill(inputs("   ", Some(&m))).await.expect("distill");
    assert_eq!(
        distilled.meta.validation.fallback_reason.as_deref(),
        Some("empty-transcript")
    );
}

// ---- SUCCESS CRITERION: content-derived slug (harvest-content-slug-naming) ----

const SLUG_YAML: &str = r#"
summary: "Chose typed cross-stage contracts."
slug: "  Typed-CrossStage-Contract-Handoff  "
claims:
  - text: "Chose typed contracts over markdown."
    kind: position
tags: [rust]
links: []
"#;

#[tokio::test]
async fn single_call_extracts_and_normalizes_content_slug() {
    // Positive: the distiller carries the pattern's slug through, trimmed and
    // lowercased (filename-safety sanitization is the publish path's job).
    let fake = FakeFabric::new();
    fake.set_response(PATTERN, SLUG_YAML);
    let m = meta();
    let distiller = make_distiller(fake, SessionConfig::default());
    let distilled = distiller
        .distill(inputs("USER: hi\nASSISTANT: ok", Some(&m)))
        .await
        .expect("distill");
    assert_eq!(distilled.slug.as_deref(), Some("typed-crossstage-contract-handoff"));
}

#[tokio::test]
async fn single_call_slug_is_none_when_pattern_omits_it() {
    // Negative: VALID_YAML carries no `slug:` key, so the distiller emits None -
    // the signal the publish path uses to fall back to the title-slug.
    let fake = FakeFabric::new();
    fake.set_response(PATTERN, VALID_YAML);
    let m = meta();
    let distiller = make_distiller(fake, SessionConfig::default());
    let distilled = distiller
        .distill(inputs("USER: hi\nASSISTANT: ok", Some(&m)))
        .await
        .expect("distill");
    assert_eq!(distilled.slug, None);
}

#[tokio::test]
async fn map_reduce_carries_reduce_slug() {
    // The reduce pass names the whole session; its slug rides through.
    let fake = Arc::new(FakeFabric::new());
    fake.set_response(
        PATTERN_CHUNK,
        "summary: \"Chunk.\"\nclaims:\n  - text: \"A decision.\"\ntags: []\nlinks: []\n",
    );
    fake.set_response(
        PATTERN_REDUCE,
        "summary: \"Whole-session synthesis.\"\nslug: \"token-broker-arc-refactor\"\nclaims:\n  - text: \"A decision.\"\n",
    );
    let m = meta();
    let body = huge_body();
    let config = SessionConfig {
        token_cap: 30_000,
        ..SessionConfig::default()
    };
    let distiller = SessionDistiller::new(fake.clone(), config);
    let distilled = distiller.distill(inputs(&body, Some(&m))).await.expect("distill");
    assert_eq!(distilled.slug.as_deref(), Some("token-broker-arc-refactor"));
}

#[test]
fn clean_slug_trims_lowercases_and_drops_empty() {
    assert_eq!(clean_slug(Some("  Foo-Bar-Baz  ")).as_deref(), Some("foo-bar-baz"));
    assert_eq!(clean_slug(Some("   ")), None);
    assert_eq!(clean_slug(Some("")), None);
    assert_eq!(clean_slug(None), None);
}

#[tokio::test]
async fn no_metadata_yields_no_payload() {
    let fake = FakeFabric::new();
    fake.set_response(PATTERN, VALID_YAML);
    let distiller = make_distiller(fake, SessionConfig::default());
    let distilled = distiller.distill(inputs("body", None)).await.expect("distill");
    assert!(distilled.kind_specific.is_none(), "no metadata -> no session payload");
}

// ---- SUCCESS CRITERION (Phase 3): bounded per-chunk retry -----------------
//
// These exercise `distill_long` directly with hand-built chunk vectors so the
// retry behavior is deterministic (the normal `distill()` entry cannot produce
// a single chunk on the map-reduce path under the fixed chunk-size constants).

const CHUNK_YAML_VALID: &str =
    "summary: \"Chunk covered the migration.\"\nclaims:\n  - text: \"A chunk decision.\"\ntags: [ci]\nlinks: []\n";
const REDUCE_YAML_VALID: &str = "summary: \"Whole-session synthesis.\"\nclaims:\n  - text: \"A chunk decision.\"\n";
/// Transient, NON-repair-shape malformed YAML: it is neither a duplicate-key
/// nor a prose-preamble shape, so `parse_pattern_yaml`'s structural repair does
/// NOT cover it — only a RETRY can rescue the chunk.
const CHUNK_YAML_TRANSIENT_GARBAGE: &str = "this is not yaml: [unclosed";

#[tokio::test]
async fn chunk_retry_recovers_transient_malformed_then_valid() {
    // Attempt 1 returns transient garbage YAML; attempt 2 returns valid YAML.
    // With the default chunk_retries=1 the retry rescues the chunk, so the
    // result is full and NOT flagged partial-chunk-failure.
    let fake = FakeFabric::new();
    fake.set_response_sequence(
        PATTERN_CHUNK,
        vec![
            Ok(CHUNK_YAML_TRANSIENT_GARBAGE.to_string()),
            Ok(CHUNK_YAML_VALID.to_string()),
        ],
    );
    fake.set_response(PATTERN_REDUCE, REDUCE_YAML_VALID);
    let distiller = make_distiller(fake, SessionConfig::default());
    let built = distiller
        .distill_long("body text", vec!["USER: chunk one\nASSISTANT: ok".to_string()])
        .await
        .expect("distill_long");
    let distilled = built.take();
    assert!(
        distilled.meta.validation.fallback_reason.is_none(),
        "retry rescued the chunk; expected fallback=none, got {:?}",
        distilled.meta.validation.fallback_reason
    );
    assert!(!distilled.claims.is_empty(), "the recovered chunk contributes claims");
}

#[tokio::test]
async fn chunk_retries_zero_disables_retry_and_degrades() {
    // Same transient-then-valid sequence, but chunk_retries=0 → exactly ONE
    // attempt, which fails to parse. The single chunk fails → chunk-failures
    // fallback. Proves the config knob is load-bearing (contrast with the
    // default-1 test above, which recovers the identical input).
    let fake = FakeFabric::new();
    fake.set_response_sequence(
        PATTERN_CHUNK,
        vec![
            Ok(CHUNK_YAML_TRANSIENT_GARBAGE.to_string()),
            Ok(CHUNK_YAML_VALID.to_string()),
        ],
    );
    fake.set_response(PATTERN_REDUCE, REDUCE_YAML_VALID);
    let config = SessionConfig {
        chunk_retries: 0,
        ..SessionConfig::default()
    };
    let distiller = make_distiller(fake, config);
    let built = distiller
        .distill_long("body text", vec!["USER: chunk one\nASSISTANT: ok".to_string()])
        .await
        .expect("distill_long");
    let distilled = built.take();
    assert_eq!(
        distilled.meta.validation.fallback_reason.as_deref(),
        Some("chunk-failures"),
        "chunk_retries=0 issues one attempt only; the failed chunk is not rescued"
    );
}

#[tokio::test]
async fn chunk_retry_exhausted_degrades_to_partial_chunk_failure() {
    // Two chunks: chunk A (FAIL-MARKER) errors on EVERY attempt and exhausts its
    // retries; chunk B (OK-MARKER) parses. Deterministic by chunk BODY, not by
    // buffer_unordered completion order. The exhausted chunk degrades the reduce
    // to partial-chunk-failure without panicking.
    let fake = FakeFabric::new();
    fake.set_error_for_input("FAIL-MARKER", "boom");
    fake.set_response_for_input("OK-MARKER", CHUNK_YAML_VALID);
    fake.set_response(PATTERN_REDUCE, REDUCE_YAML_VALID);
    let distiller = make_distiller(fake, SessionConfig::default());
    let chunks = vec![
        "USER: FAIL-MARKER chunk\nASSISTANT: x".to_string(),
        "USER: OK-MARKER chunk\nASSISTANT: y".to_string(),
    ];
    let built = distiller.distill_long("body text", chunks).await.expect("distill_long");
    let distilled = built.take();
    assert_eq!(
        distilled.meta.validation.fallback_reason.as_deref(),
        Some("partial-chunk-failure"),
        "an exhausted chunk plus a surviving sibling degrades to partial-chunk-failure"
    );
    assert!(!distilled.claims.is_empty(), "the surviving chunk's claims are kept");
}

/// A `FabricCaller` that records the peak concurrent in-flight call count for
/// `distill-session-chunk`. Each chunk call opens a real (sleep-widened)
/// in-flight window; if a retry ever OVERLAPPED the prior call, `max_inflight`
/// would read 2. Attempt 1 returns garbage (forcing a retry), attempt 2 valid.
#[derive(Clone)]
struct InFlightProbe {
    inner: Arc<ProbeInner>,
}

struct ProbeInner {
    inflight: std::sync::atomic::AtomicUsize,
    max_inflight: std::sync::atomic::AtomicUsize,
    chunk_calls: std::sync::atomic::AtomicUsize,
}

impl InFlightProbe {
    fn new() -> Self {
        Self {
            inner: Arc::new(ProbeInner {
                inflight: std::sync::atomic::AtomicUsize::new(0),
                max_inflight: std::sync::atomic::AtomicUsize::new(0),
                chunk_calls: std::sync::atomic::AtomicUsize::new(0),
            }),
        }
    }
}

#[async_trait::async_trait]
impl FabricCaller for InFlightProbe {
    async fn call(&self, request: FabricRequest) -> eyre::Result<String> {
        use std::sync::atomic::Ordering::SeqCst;
        if request.pattern != PATTERN_CHUNK {
            return Ok(REDUCE_YAML_VALID.to_string());
        }
        let attempt = self.inner.chunk_calls.fetch_add(1, SeqCst);
        let cur = self.inner.inflight.fetch_add(1, SeqCst) + 1;
        self.inner.max_inflight.fetch_max(cur, SeqCst);
        // Hold the in-flight window open so an erroneous overlapping retry would
        // be observed as inflight == 2.
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        self.inner.inflight.fetch_sub(1, SeqCst);
        // First attempt fails to parse (forces a retry); the second succeeds.
        if attempt == 0 {
            Ok(CHUNK_YAML_TRANSIENT_GARBAGE.to_string())
        } else {
            Ok(CHUNK_YAML_VALID.to_string())
        }
    }
}

#[tokio::test]
async fn chunk_retry_never_overlaps_a_running_call() {
    use std::sync::atomic::Ordering::SeqCst;
    let probe = InFlightProbe::new();
    let distiller = SessionDistiller::new(probe.clone(), SessionConfig::default());
    let built = distiller
        .distill_long("body text", vec!["USER: chunk one\nASSISTANT: ok".to_string()])
        .await
        .expect("distill_long");
    let distilled = built.take();
    assert_eq!(
        probe.inner.chunk_calls.load(SeqCst),
        2,
        "one retry means exactly two chunk attempts (proves a retry actually happened)"
    );
    assert_eq!(
        probe.inner.max_inflight.load(SeqCst),
        1,
        "the retry re-issued only AFTER the prior call returned; never overlapping"
    );
    assert!(
        distilled.meta.validation.fallback_reason.is_none(),
        "the second attempt succeeded, so the chunk is not degraded"
    );
}
