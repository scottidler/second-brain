use super::*;

#[tokio::test]
async fn passthrough_summary_matches_transcript() {
    let distiller = PassthroughDistiller::new();
    let inputs = DistillInputs {
        transcript: "OCR output for an image.",
        source_url: None,
        title_hint: None,
        repo_metadata: None,
        video_metadata: None,
        capture_note: None,
    };
    let distilled = distiller.distill(inputs).await.expect("distill");
    assert_eq!(distilled.summary, "OCR output for an image.");
    assert!(distilled.claims.is_empty());
    assert!(distilled.links.is_empty());
    assert_eq!(distilled.meta.extractor, "distill-passthrough-v1");
    // Phase 9c-hotfix: verbatim archive preservation.
    assert_eq!(distilled.transcript.as_deref(), Some("OCR output for an image."));
}

#[tokio::test]
async fn preserves_long_input_verbatim_in_transcript() {
    // Phase 9c-hotfix: the 280-char per-distiller cap was deleted to fix a
    // data-loss regression for Vision+OCR and Groq-transcript inputs. The
    // global 2000-char cap in `validate::enforce_bounds` is the only schema
    // protection on summary; transcript is uncapped at the distiller level.
    let long_text = "x".repeat(5000);
    let distiller = PassthroughDistiller::new();
    let inputs = DistillInputs {
        transcript: &long_text,
        source_url: None,
        title_hint: None,
        repo_metadata: None,
        video_metadata: None,
        capture_note: None,
    };
    let distilled = distiller.distill(inputs).await.expect("distill");
    assert_eq!(distilled.summary.chars().count(), 5000);
    assert_eq!(distilled.transcript.as_deref().map(|s| s.chars().count()), Some(5000));
    assert!(distilled.meta.validation.bounds_truncations.is_empty());
}
