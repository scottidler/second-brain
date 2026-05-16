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
        repo_metadata: None,
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
        repo_metadata: None,
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
        repo_metadata: None,
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
        repo_metadata: None,
    };
    let distilled = dispatcher.distill(DistillKind::Article, inputs).await.expect("distill");
    assert_eq!(distilled.meta.extractor, "distill-article-v1");
    assert_eq!(distilled.summary, "An article.");
}

#[tokio::test]
async fn dispatches_repo_to_fabric_backed_distiller() {
    let fake = Arc::new(FakeFabric::new());
    fake.set_response(
        "distill-repo",
        "summary: \"A Rust workspace.\"\nclaims: []\ntags: []\nlinks: []\n",
    );
    let dispatcher = Dispatcher::new(fake, ArticleConfig::default());
    let metadata = crate::RepoMetadata {
        owner: "scottidler".to_string(),
        repo: "second-brain".to_string(),
        stars: Some(7),
        primary_language: Some("Rust".to_string()),
        last_commit: None,
        topics: Vec::new(),
    };
    let inputs = DistillInputs {
        transcript: "README content",
        source_url: Some("https://github.com/scottidler/second-brain"),
        title_hint: None,
        repo_metadata: Some(&metadata),
    };
    let distilled = dispatcher.distill(DistillKind::Repo, inputs).await.expect("distill");
    assert_eq!(distilled.meta.extractor, "distill-repo-v1");
    let Some(vault::distilled::KindPayload::Repo(payload)) = distilled.kind_specific else {
        panic!("expected Repo payload from dispatcher");
    };
    assert_eq!(payload.stars, Some(7));
}

#[tokio::test]
async fn unwired_fabric_kinds_bail_until_phase_5_plus() {
    let dispatcher = make_dispatcher();
    for kind in [DistillKind::Video, DistillKind::Thread] {
        let inputs = DistillInputs {
            transcript: "x",
            source_url: None,
            title_hint: None,
            repo_metadata: None,
        };
        let err = dispatcher
            .distill(kind, inputs)
            .await
            .expect_err("phase-4 dispatcher must not handle Video/Thread yet");
        let msg = format!("{err}");
        assert!(
            msg.contains("Phases 5-6"),
            "error should reference Phases 5-6 for {kind:?}; got {msg}"
        );
    }
}
