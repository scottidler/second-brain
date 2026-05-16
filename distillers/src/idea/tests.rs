use super::*;

#[tokio::test]
async fn passthrough_summary_matches_transcript() {
    let distiller = IdeaDistiller::new();
    let inputs = DistillInputs {
        transcript: "Quick observation about distributed consensus.",
        source_url: None,
        title_hint: None,
        repo_metadata: None,
    };
    let distilled = distiller.distill(inputs).await.expect("distill");
    assert_eq!(distilled.summary, "Quick observation about distributed consensus.");
    assert!(distilled.claims.is_empty());
    assert!(distilled.tags.is_empty());
    assert!(distilled.kind_specific.is_none());
    assert_eq!(distilled.meta.extractor, "distill-idea-v1");
    assert!(distilled.meta.validation.fallback_reason.is_none());
}

#[tokio::test]
async fn truncates_long_summary_at_char_limit() {
    let long_text = "a".repeat(SUMMARY_CHAR_LIMIT + 50);
    let distiller = IdeaDistiller::new();
    let inputs = DistillInputs {
        transcript: &long_text,
        source_url: None,
        title_hint: None,
        repo_metadata: None,
    };
    let distilled = distiller.distill(inputs).await.expect("distill");
    assert_eq!(distilled.summary.chars().count(), SUMMARY_CHAR_LIMIT);
    let trunc = &distilled.meta.validation.bounds_truncations;
    assert!(
        trunc.iter().any(|t| t.starts_with("summary:")),
        "expected summary truncation tag, got {trunc:?}"
    );
}

#[tokio::test]
async fn extracts_outbound_links_from_transcript() {
    let distiller = IdeaDistiller::new();
    let inputs = DistillInputs {
        transcript: "Check out https://example.com and also http://foo.bar/path?q=1 for context.",
        source_url: None,
        title_hint: None,
        repo_metadata: None,
    };
    let distilled = distiller.distill(inputs).await.expect("distill");
    let urls: Vec<&str> = distilled.links.iter().map(|l| l.url.as_str()).collect();
    assert!(urls.contains(&"https://example.com"));
    assert!(urls.contains(&"http://foo.bar/path?q=1"));
}
