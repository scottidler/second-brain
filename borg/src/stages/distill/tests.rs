use super::*;

#[test]
fn ingest_kind_maps_to_distill_kind() {
    assert_eq!(
        distill_kind_from_ingest(IngestKind::ArticleUrl).expect("map ingest kind"),
        DistillKind::Article
    );
    assert_eq!(
        distill_kind_from_ingest(IngestKind::GitHubUrl).expect("map ingest kind"),
        DistillKind::Repo
    );
    assert_eq!(
        distill_kind_from_ingest(IngestKind::YoutubeUrl).expect("map ingest kind"),
        DistillKind::Video
    );
    assert_eq!(
        distill_kind_from_ingest(IngestKind::ThreadUrl).expect("map ingest kind"),
        DistillKind::Thread
    );
    assert_eq!(
        distill_kind_from_ingest(IngestKind::Image).expect("map ingest kind"),
        DistillKind::Image
    );
    assert_eq!(
        distill_kind_from_ingest(IngestKind::VoiceNote).expect("map ingest kind"),
        DistillKind::VoiceNote
    );
    assert_eq!(
        distill_kind_from_ingest(IngestKind::Idea).expect("map ingest kind"),
        DistillKind::Idea
    );
}

#[test]
fn vocabulary_kinds_bail() {
    assert!(distill_kind_from_ingest(IngestKind::VocabularyEn).is_err());
    assert!(distill_kind_from_ingest(IngestKind::VocabularyEs).is_err());
}

#[tokio::test]
async fn distill_stage_handles_idea_through_dispatcher() {
    let stage = DistillStage::new();
    let distilled = stage
        .distill(IngestKind::Idea, "A small idea.", None, None)
        .await
        .expect("distill");
    assert_eq!(distilled.summary, "A small idea.");
    assert_eq!(distilled.meta.extractor, "distill-idea-v1");
}

#[tokio::test]
async fn distill_stage_handles_image_through_passthrough() {
    let stage = DistillStage::new();
    let distilled = stage
        .distill(IngestKind::Image, "ocr'd text", None, None)
        .await
        .expect("distill");
    assert_eq!(distilled.meta.extractor, "distill-passthrough-v1");
}

#[tokio::test]
async fn distill_stage_bails_on_url_kinds_until_phase_3_plus() {
    let stage = DistillStage::new();
    let err = stage
        .distill(IngestKind::ArticleUrl, "x", None, None)
        .await
        .expect_err("article should not dispatch in Phase 2");
    let msg = format!("{err}");
    assert!(msg.contains("Phases 3-6"), "expected Phases 3-6 reference; got {msg}");
}
