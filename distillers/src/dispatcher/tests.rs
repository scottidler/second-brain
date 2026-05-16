use super::*;
use crate::FakeFabric;
use std::sync::Arc;

fn make_dispatcher() -> Dispatcher<Arc<FakeFabric>> {
    Dispatcher::new(Arc::new(FakeFabric::new()), ArticleConfig::default())
}

#[tokio::test]
async fn dispatches_idea_to_idea_distiller() {
    let dispatcher = make_dispatcher();
    let inputs = DistillInputs {
        transcript: "An idea about caches.",
        source_url: None,
        title_hint: None,
    };
    let distilled = dispatcher.distill(DistillKind::Idea, inputs).await.expect("distill");
    assert_eq!(distilled.meta.extractor, "distill-idea-v1");
}

#[tokio::test]
async fn dispatches_image_to_passthrough() {
    let dispatcher = make_dispatcher();
    let inputs = DistillInputs {
        transcript: "ocr text",
        source_url: None,
        title_hint: None,
    };
    let distilled = dispatcher.distill(DistillKind::Image, inputs).await.expect("distill");
    assert_eq!(distilled.meta.extractor, "distill-passthrough-v1");
}

#[tokio::test]
async fn dispatches_voice_note_to_passthrough() {
    let dispatcher = make_dispatcher();
    let inputs = DistillInputs {
        transcript: "transcribed audio",
        source_url: None,
        title_hint: None,
    };
    let distilled = dispatcher
        .distill(DistillKind::VoiceNote, inputs)
        .await
        .expect("distill");
    assert_eq!(distilled.meta.extractor, "distill-passthrough-v1");
}

#[tokio::test]
async fn dispatches_article_to_fabric_backed_distiller() {
    let fake = Arc::new(FakeFabric::new());
    fake.set_response(
        "distill-article",
        "summary: \"An article.\"\nclaims: []\ntags: []\nlinks: []\n",
    );
    let dispatcher = Dispatcher::new(fake, ArticleConfig::default());
    let inputs = DistillInputs {
        transcript: "Article body.",
        source_url: Some("https://example.com"),
        title_hint: None,
    };
    let distilled = dispatcher.distill(DistillKind::Article, inputs).await.expect("distill");
    assert_eq!(distilled.meta.extractor, "distill-article-v1");
    assert_eq!(distilled.summary, "An article.");
}

#[tokio::test]
async fn unwired_fabric_kinds_bail_until_phase_4_plus() {
    let dispatcher = make_dispatcher();
    for kind in [DistillKind::Repo, DistillKind::Video, DistillKind::Thread] {
        let inputs = DistillInputs {
            transcript: "x",
            source_url: None,
            title_hint: None,
        };
        let err = dispatcher
            .distill(kind, inputs)
            .await
            .expect_err("phase-3 dispatcher must not handle Repo/Video/Thread yet");
        let msg = format!("{err}");
        assert!(
            msg.contains("Phases 4-6"),
            "error should reference Phases 4-6 for {kind:?}; got {msg}"
        );
    }
}
