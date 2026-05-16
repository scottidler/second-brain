use super::*;

#[tokio::test]
async fn passthrough_summary_matches_transcript() {
    let distiller = PassthroughDistiller::new();
    let inputs = DistillInputs {
        transcript: "OCR output for an image.",
        source_url: None,
        title_hint: None,
        repo_metadata: None,
    };
    let distilled = distiller.distill(inputs).await.expect("distill");
    assert_eq!(distilled.summary, "OCR output for an image.");
    assert!(distilled.claims.is_empty());
    assert!(distilled.links.is_empty());
    assert_eq!(distilled.meta.extractor, "distill-passthrough-v1");
}

#[tokio::test]
async fn truncates_long_summary() {
    let long_text = "x".repeat(SUMMARY_CHAR_LIMIT + 50);
    let distiller = PassthroughDistiller::new();
    let inputs = DistillInputs {
        transcript: &long_text,
        source_url: None,
        title_hint: None,
        repo_metadata: None,
    };
    let distilled = distiller.distill(inputs).await.expect("distill");
    assert_eq!(distilled.summary.chars().count(), SUMMARY_CHAR_LIMIT);
}
