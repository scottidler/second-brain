use super::*;
use distillers::{ArticleConfig, Dispatcher, FakeFabric};
use std::sync::Arc;

fn make_stage() -> DistillStage<Arc<FakeFabric>> {
    DistillStage::with_dispatcher(Dispatcher::new(Arc::new(FakeFabric::new()), ArticleConfig::default()))
}

fn make_stage_with_fake(fake: Arc<FakeFabric>) -> DistillStage<Arc<FakeFabric>> {
    DistillStage::with_dispatcher(Dispatcher::new(fake, ArticleConfig::default()))
}

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
    let stage = make_stage();
    let distilled = stage
        .distill(IngestKind::Idea, "A small idea.", None, None)
        .await
        .expect("distill");
    assert_eq!(distilled.summary, "A small idea.");
    assert_eq!(distilled.meta.extractor, "distill-idea-v1");
}

#[tokio::test]
async fn distill_stage_handles_image_through_passthrough() {
    let stage = make_stage();
    let distilled = stage
        .distill(IngestKind::Image, "ocr'd text", None, None)
        .await
        .expect("distill");
    assert_eq!(distilled.meta.extractor, "distill-passthrough-v1");
}

#[tokio::test]
async fn distill_stage_handles_article_through_fabric() {
    let fake = Arc::new(FakeFabric::new());
    fake.set_response(
        "distill-article",
        "summary: \"An article.\"\nclaims: []\ntags: []\nlinks: []\n",
    );
    let stage = make_stage_with_fake(fake);
    let distilled = stage
        .distill(
            IngestKind::ArticleUrl,
            "Article body.",
            Some("https://example.com"),
            None,
        )
        .await
        .expect("distill");
    assert_eq!(distilled.meta.extractor, "distill-article-v1");
    assert_eq!(distilled.summary, "An article.");
}

#[tokio::test]
async fn distill_stage_bails_on_unwired_url_kinds() {
    let stage = make_stage();
    for kind in [IngestKind::YoutubeUrl, IngestKind::ThreadUrl] {
        let err = stage
            .distill(kind, "x", None, None)
            .await
            .expect_err("video/thread should still bail in Phase 4");
        let msg = format!("{err}");
        assert!(
            msg.contains("Phases 5-6"),
            "expected Phases 5-6 reference for {kind}; got {msg}"
        );
    }
}

#[tokio::test]
async fn distill_stage_handles_repo_through_fabric_with_metadata() {
    let fake = Arc::new(FakeFabric::new());
    fake.set_response(
        "distill-repo",
        "summary: \"A workspace.\"\nclaims: []\ntags: []\nlinks: []\ninstall: \"cargo install --path borg\"\n",
    );
    let stage = make_stage_with_fake(fake);
    let metadata = distillers::RepoMetadata {
        owner: "scottidler".to_string(),
        repo: "second-brain".to_string(),
        stars: Some(99),
        primary_language: Some("Rust".to_string()),
        last_commit: Some("2026-05-16T10:00:00Z".to_string()),
        topics: vec!["obsidian".to_string()],
    };
    let distilled = stage
        .distill_with_metadata(
            IngestKind::GitHubUrl,
            "README transcript",
            Some("https://github.com/scottidler/second-brain"),
            None,
            Some(&metadata),
        )
        .await
        .expect("distill");
    assert_eq!(distilled.meta.extractor, "distill-repo-v1");
    let Some(vault::distilled::KindPayload::Repo(payload)) = distilled.kind_specific else {
        panic!("expected Repo payload");
    };
    assert_eq!(payload.stars, Some(99));
    assert_eq!(payload.install.as_deref(), Some("cargo install --path borg"));
}

#[test]
fn repo_metadata_from_fetch_copies_relevant_fields() {
    let src = crate::github::RepoMetadata {
        owner: "o".to_string(),
        repo: "r".to_string(),
        stars: Some(3),
        primary_language: Some("Rust".to_string()),
        last_commit: Some("2026-05-16T00:00:00Z".to_string()),
        topics: vec!["a".to_string(), "b".to_string()],
        default_branch: Some("main".to_string()),
        description: Some("desc".to_string()),
    };
    let mapped = repo_metadata_from_fetch(&src);
    assert_eq!(mapped.owner, "o");
    assert_eq!(mapped.repo, "r");
    assert_eq!(mapped.stars, Some(3));
    assert_eq!(mapped.primary_language.as_deref(), Some("Rust"));
    assert_eq!(mapped.last_commit.as_deref(), Some("2026-05-16T00:00:00Z"));
    assert_eq!(mapped.topics, vec!["a", "b"]);
}

#[test]
fn write_distilled_yml_no_op_when_staging_disabled() {
    use crate::config::{StagingConfig, StagingLayout};
    use tempfile::TempDir;
    use vault::distilled::{Distilled, DistilledMeta, ValidationMeta};

    let tmp = TempDir::new().expect("tempdir");
    let staging = StagingConfig {
        enabled: false,
        root: tmp.path().to_path_buf(),
        layout: StagingLayout::PerTrace,
        ..StagingConfig::default()
    };
    let distilled = Distilled {
        summary: "s".to_string(),
        claims: Vec::new(),
        tags: Vec::new(),
        links: Vec::new(),
        kind_specific: None,
        meta: DistilledMeta {
            extractor: "distill-article-v1".to_string(),
            model: "test".to_string(),
            input_tokens: 0,
            output_tokens: 0,
            produced_at: "2026-05-16T14:03:22Z".to_string(),
            validation: ValidationMeta::default(),
        },
    };
    write_distilled_yml(&staging, "trace-1", &distilled).expect("no-op");
    assert!(!tmp.path().join("trace-1").exists());
}

#[test]
fn write_distilled_yml_persists_to_per_trace_dir() {
    use crate::config::{StagingConfig, StagingLayout};
    use tempfile::TempDir;
    use vault::distilled::{Distilled, DistilledMeta, ValidationMeta};

    let tmp = TempDir::new().expect("tempdir");
    let staging = StagingConfig {
        enabled: true,
        root: tmp.path().to_path_buf(),
        layout: StagingLayout::PerTrace,
        ..StagingConfig::default()
    };
    let distilled = Distilled {
        summary: "Hello.".to_string(),
        claims: Vec::new(),
        tags: Vec::new(),
        links: Vec::new(),
        kind_specific: None,
        meta: DistilledMeta {
            extractor: "distill-article-v1".to_string(),
            model: "test".to_string(),
            input_tokens: 0,
            output_tokens: 0,
            produced_at: "2026-05-16T14:03:22Z".to_string(),
            validation: ValidationMeta::default(),
        },
    };
    write_distilled_yml(&staging, "trace-1", &distilled).expect("write");
    let path = tmp.path().join("trace-1").join(DISTILLED_FILENAME);
    let bytes = std::fs::read_to_string(&path).expect("read");
    assert!(bytes.contains("summary: Hello."), "yaml mismatch: {bytes}");
    assert!(bytes.contains("distill-article-v1"));
    // tmp suffix must not linger after the atomic rename.
    assert!(!tmp.path().join("trace-1").join("distilled.yml.tmp").exists());
}
