use super::*;
use crate::FakeFabric;

fn make_distiller(fake: FakeFabric) -> ImageDistiller<std::sync::Arc<FakeFabric>> {
    ImageDistiller::new(std::sync::Arc::new(fake), ImageConfig::default())
}

#[tokio::test]
async fn happy_path_parses_distilled_yaml_and_preserves_transcript() {
    let fake = FakeFabric::new();
    fake.set_response(
        PATTERN,
        r#"
summary: "A screenshot of a tweet criticizing centralized cloud platforms."
claims:
  - text: "Centralized cloud platforms create single points of failure."
    anchor: null
  - text: "Cost of egress traffic punishes multi-cloud deployments."
    anchor: null
tags: []
links:
  - url: "https://example.com/post/1"
    label: null
"#,
    );
    let transcript = "## Description\n\nA tweet on a white background, text in black.\n\n## Extracted Text\n\nCloud lock-in is the new vendor lock-in. Egress fees are the toll booth. https://example.com/post/1";
    let distiller = make_distiller(fake);
    let distilled = distiller
        .distill(DistillInputs {
            transcript,
            source_url: None,
            title_hint: None,
            repo_metadata: None,
            video_metadata: None,
            capture_note: None,
            session_metadata: None,
        })
        .await
        .expect("distill");

    assert_eq!(distilled.meta.extractor, "distill-image-v1");
    assert!(distilled.summary.starts_with("A screenshot"));
    assert_eq!(distilled.claims.len(), 2);
    assert_eq!(distilled.links.len(), 1);
    assert_eq!(distilled.links[0].url, "https://example.com/post/1");

    // Phase 9c-image: the raw Vision+OCR concat must round-trip verbatim into
    // `transcript` so the published note is a searchable archive.
    assert_eq!(distilled.transcript.as_deref(), Some(transcript));
}

#[tokio::test]
async fn fenced_yaml_response_is_stripped_and_parsed() {
    // Consumer-level fence-strip regression: the LLM wraps its YAML in a
    // ```yaml fence despite the prompt. The shared `strip_fences` (Phase 9
    // consolidation) must run inside the image distiller so the body still
    // parses cleanly rather than landing in a yaml-parse-error fallback.
    let fake = FakeFabric::new();
    fake.set_response(
        PATTERN,
        "```yaml\nsummary: \"A fenced screenshot summary.\"\nclaims: []\ntags: []\nlinks: []\n```",
    );
    let distiller = make_distiller(fake);
    let distilled = distiller
        .distill(DistillInputs {
            transcript: "## Extracted Text\n\nSome text.",
            source_url: None,
            title_hint: None,
            repo_metadata: None,
            video_metadata: None,
            capture_note: None,
            session_metadata: None,
        })
        .await
        .expect("distill");
    assert!(distilled.meta.validation.fallback_reason.is_none());
    assert_eq!(distilled.summary, "A fenced screenshot summary.");
}

#[tokio::test]
async fn fabric_timeout_falls_back_and_preserves_transcript() {
    let fake = FakeFabric::new();
    fake.set_timeout(PATTERN);
    let distiller = make_distiller(fake);
    let transcript = "## Extracted Text\n\nA blurry receipt for groceries.";
    let distilled = distiller
        .distill(DistillInputs {
            transcript,
            source_url: None,
            title_hint: None,
            repo_metadata: None,
            video_metadata: None,
            capture_note: None,
            session_metadata: None,
        })
        .await
        .expect("distill");
    assert_eq!(distilled.meta.extractor, "distill-image-v1");
    assert_eq!(
        distilled.meta.validation.fallback_reason.as_deref(),
        Some("fabric-timeout")
    );
    assert_eq!(distilled.transcript.as_deref(), Some(transcript));
}

#[tokio::test]
async fn malformed_yaml_falls_back_and_preserves_transcript() {
    let fake = FakeFabric::new();
    fake.set_response(PATTERN, "this is not yaml at all : : :\nrandom prose follows");
    let distiller = make_distiller(fake);
    let transcript = "## Extracted Text\n\nLooks like a meme.";
    let distilled = distiller
        .distill(DistillInputs {
            transcript,
            source_url: None,
            title_hint: None,
            repo_metadata: None,
            video_metadata: None,
            capture_note: None,
            session_metadata: None,
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
async fn long_transcript_is_preserved_in_full() {
    // 9c-image must not silently truncate Vision+OCR text. Phase 9c-hotfix's
    // contract: the transcript field is uncapped at the distiller level
    // even when the summary gets clipped by the global 2000-char cap.
    let fake = FakeFabric::new();
    fake.set_response(
        PATTERN,
        r#"
summary: "Multi-paragraph screenshot of an article on consensus algorithms."
claims: []
tags: []
links: []
"#,
    );
    let long_transcript = "## Extracted Text\n\n".to_string() + &"Long paragraph text. ".repeat(200);
    let distiller = make_distiller(fake);
    let distilled = distiller
        .distill(DistillInputs {
            transcript: &long_transcript,
            source_url: None,
            title_hint: None,
            repo_metadata: None,
            video_metadata: None,
            capture_note: None,
            session_metadata: None,
        })
        .await
        .expect("distill");
    assert_eq!(distilled.transcript.as_deref(), Some(long_transcript.as_str()));
    assert_eq!(
        distilled.transcript.as_deref().map(|s| s.chars().count()),
        Some(long_transcript.chars().count())
    );
}
