use super::*;

#[tokio::test]
async fn passthrough_summary_matches_transcript() {
    let distiller = IdeaDistiller::new();
    let inputs = DistillInputs {
        transcript: "Quick observation about distributed consensus.",
        source_url: None,
        title_hint: None,
        repo_metadata: None,
        video_metadata: None,
        capture_note: None,
    };
    let distilled = distiller.distill(inputs).await.expect("distill");
    assert_eq!(distilled.summary, "Quick observation about distributed consensus.");
    assert!(distilled.claims.is_empty());
    assert!(distilled.tags.is_empty());
    assert!(distilled.kind_specific.is_none());
    assert_eq!(distilled.meta.extractor, "distill-idea-v2");
    assert!(distilled.meta.validation.fallback_reason.is_none());
    // Phase 9c-hotfix: the full input lands in `transcript` so the published
    // note is a verbatim archive.
    assert_eq!(
        distilled.transcript.as_deref(),
        Some("Quick observation about distributed consensus.")
    );
}

#[tokio::test]
async fn preserves_long_input_verbatim_in_transcript() {
    // Phase 9c-hotfix: the per-distiller 280-char cap was deleted; the global
    // 2000-char cap in `validate::enforce_bounds` is the only schema protection.
    // IdeaDistiller itself returns the full trimmed input as summary; the
    // `transcript` field is the verbatim archive regardless.
    let long_text = "a".repeat(5000);
    let distiller = IdeaDistiller::new();
    let inputs = DistillInputs {
        transcript: &long_text,
        source_url: None,
        title_hint: None,
        repo_metadata: None,
        video_metadata: None,
        capture_note: None,
    };
    let distilled = distiller.distill(inputs).await.expect("distill");
    // Distiller does not truncate; only enforce_bounds (called by URL kinds
    // via wrapper helpers) would trim summary. For idea/vocab the call path
    // goes through DistillStage which does not invoke enforce_bounds, so the
    // full summary is preserved here too. Transcript is always verbatim.
    assert_eq!(distilled.summary.chars().count(), 5000);
    assert_eq!(distilled.transcript.as_deref().map(|s| s.chars().count()), Some(5000));
    assert!(distilled.meta.validation.bounds_truncations.is_empty());
}

#[tokio::test]
async fn extracts_outbound_links_from_transcript() {
    let distiller = IdeaDistiller::new();
    let inputs = DistillInputs {
        transcript: "Check out https://example.com and also http://foo.bar/path?q=1 for context.",
        source_url: None,
        title_hint: None,
        repo_metadata: None,
        video_metadata: None,
        capture_note: None,
    };
    let distilled = distiller.distill(inputs).await.expect("distill");
    let urls: Vec<&str> = distilled.links.iter().map(|l| l.url.as_str()).collect();
    assert!(urls.contains(&"https://example.com"));
    assert!(urls.contains(&"http://foo.bar/path?q=1"));
}
