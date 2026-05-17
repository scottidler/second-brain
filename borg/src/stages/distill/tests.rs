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
fn vocabulary_kinds_map_to_vocabulary_distill_kind() {
    // Phase 9c-hotfix: Vocabulary is now wired (routes through IdeaDistiller).
    assert_eq!(
        distill_kind_from_ingest(IngestKind::VocabularyEn).expect("map vocab-en"),
        DistillKind::Vocabulary
    );
    assert_eq!(
        distill_kind_from_ingest(IngestKind::VocabularyEs).expect("map vocab-es"),
        DistillKind::Vocabulary
    );
}

#[tokio::test]
async fn distill_stage_handles_idea_through_dispatcher() {
    let stage = make_stage();
    let distilled = stage
        .distill(IngestKind::Idea, "A small idea.", None, None)
        .await
        .expect("distill");
    assert_eq!(distilled.summary, "A small idea.");
    // Phase 9c-hotfix: IdeaDistiller ID bumped to v2 after 280-cap deletion.
    assert_eq!(distilled.meta.extractor, "distill-idea-v2");
    assert_eq!(distilled.transcript.as_deref(), Some("A small idea."));
}

#[tokio::test]
async fn distill_stage_handles_vocabulary_through_idea_distiller() {
    let stage = make_stage();
    let distilled = stage
        .distill(
            IngestKind::VocabularyEn,
            "definir: a Spanish-style infinitive",
            None,
            None,
        )
        .await
        .expect("distill");
    // Both EN and ES route through IdeaDistiller in the degenerate cutover.
    assert_eq!(distilled.meta.extractor, "distill-idea-v2");
    assert_eq!(
        distilled.transcript.as_deref(),
        Some("definir: a Spanish-style infinitive")
    );
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
async fn distill_stage_handles_thread_through_fabric() {
    let fake = Arc::new(FakeFabric::new());
    fake.set_response(
        "distill-thread",
        "summary: \"Thread.\"\nclaims: []\ntags: []\nlinks: []\nauthor: \"u/spez\"\npost-count: 4\n",
    );
    let stage = make_stage_with_fake(fake);
    let distilled = stage
        .distill(
            IngestKind::ThreadUrl,
            "Thread body.",
            Some("https://www.reddit.com/r/rust/comments/abc/x/"),
            None,
        )
        .await
        .expect("distill");
    assert_eq!(distilled.meta.extractor, "distill-thread-v1");
    let Some(vault::distilled::KindPayload::Thread(payload)) = distilled.kind_specific else {
        panic!("expected Thread payload");
    };
    assert_eq!(payload.platform, "reddit");
    assert_eq!(payload.post_count, 4);
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
fn video_metadata_from_yt_dlp_maps_uploader_and_duration() {
    let src = crate::youtube::VideoMetadata {
        title: "Talk title".to_string(),
        uploader: "Some Channel".to_string(),
        duration_secs: 612.4,
        description: "x".to_string(),
        tags: Vec::new(),
    };
    let mapped = video_metadata_from_yt_dlp(&src);
    assert_eq!(mapped.channel.as_deref(), Some("Some Channel"));
    assert_eq!(mapped.duration_seconds, Some(612));
    assert!(mapped.published_at.is_none());
}

#[test]
fn video_metadata_from_yt_dlp_drops_sentinel_uploader() {
    let src = crate::youtube::VideoMetadata {
        title: String::new(),
        uploader: "Unknown".to_string(),
        duration_secs: 0.0,
        description: String::new(),
        tags: Vec::new(),
    };
    let mapped = video_metadata_from_yt_dlp(&src);
    assert!(mapped.channel.is_none());
    assert!(mapped.duration_seconds.is_none());
}

#[test]
fn render_timestamped_transcript_formats_segments() {
    let segments = vec![
        (0.0, "Welcome to the talk.".to_string()),
        (12.5, "Today we will discuss consensus.".to_string()),
        (3725.0, "And finally, in conclusion.".to_string()),
    ];
    let rendered = render_timestamped_transcript(&segments);
    let expected = "\
[00:00:00] Welcome to the talk.
[00:00:12] Today we will discuss consensus.
[01:02:05] And finally, in conclusion.
";
    assert_eq!(rendered, expected);
}

#[tokio::test]
async fn distill_stage_handles_video_through_fabric_with_metadata() {
    let fake = Arc::new(FakeFabric::new());
    fake.set_response(
        "distill-video",
        "summary: \"A short talk.\"\nclaims:\n  - text: \"Claim one.\"\n    anchor: \"00:00:30\"\ntags: []\nlinks: []\n",
    );
    let stage = make_stage_with_fake(fake);
    let metadata = distillers::VideoMetadata {
        channel: Some("Channel".to_string()),
        duration_seconds: Some(60),
        published_at: None,
    };
    let distilled = stage
        .distill_with_video_metadata(
            IngestKind::YoutubeUrl,
            "[00:00:00] Welcome.\n[00:00:30] Main claim here.",
            Some("https://youtu.be/abc"),
            Some("Title"),
            Some(&metadata),
        )
        .await
        .expect("distill");
    assert_eq!(distilled.meta.extractor, "distill-video-v1");
    let Some(vault::distilled::KindPayload::Video(payload)) = distilled.kind_specific else {
        panic!("expected Video payload");
    };
    assert_eq!(payload.duration_seconds, Some(60));
    assert_eq!(distilled.claims[0].anchor.as_deref(), Some("00:00:30"));
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
        transcript: None,
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
        transcript: None,
    };
    write_distilled_yml(&staging, "trace-1", &distilled).expect("write");
    let path = tmp.path().join("trace-1").join(DISTILLED_FILENAME);
    let bytes = std::fs::read_to_string(&path).expect("read");
    assert!(bytes.contains("summary: Hello."), "yaml mismatch: {bytes}");
    assert!(bytes.contains("distill-article-v1"));
    // tmp suffix must not linger after the atomic rename.
    assert!(!tmp.path().join("trace-1").join("distilled.yml.tmp").exists());
}

#[test]
fn persist_thread_transcript_no_op_when_staging_disabled() {
    use crate::config::{StagingConfig, StagingLayout};
    use tempfile::TempDir;

    let tmp = TempDir::new().expect("tempdir");
    let staging = StagingConfig {
        enabled: false,
        root: tmp.path().to_path_buf(),
        layout: StagingLayout::PerTrace,
        ..StagingConfig::default()
    };
    persist_thread_transcript_if_staging(&staging, "trace-1", "# thread body").expect("no-op");
    assert!(!tmp.path().join("trace-1").exists());
}

#[test]
fn persist_github_stage_0_1_writes_fetched_and_transcript() {
    use crate::config::{StagingConfig, StagingLayout};
    use crate::github::{RepoFetch, RepoMetadata};
    use tempfile::TempDir;

    let tmp = TempDir::new().expect("tempdir");
    let staging = StagingConfig {
        enabled: true,
        root: tmp.path().to_path_buf(),
        layout: StagingLayout::PerTrace,
        ..StagingConfig::default()
    };
    let fetch_result = RepoFetch {
        transcript: "# Repository Metadata\n- repo: o/r\n# README\n\nbody".to_string(),
        metadata: RepoMetadata {
            owner: "o".to_string(),
            repo: "r".to_string(),
            ..Default::default()
        },
        raw_json: br#"{"repo":{"name":"r"},"readme":{"content":"Ym9keQ==","encoding":"base64"}}"#.to_vec(),
    };
    persist_github_stage_0_1_if_staging(&staging, "trace-9b", "https://github.com/o/r", &fetch_result).expect("write");

    let fetched_path = tmp.path().join("trace-9b").join("fetched.html");
    let fetched_yml = tmp.path().join("trace-9b").join("fetched.yml");
    let transcript_md = tmp.path().join("trace-9b").join("transcript.md");
    let transcript_yml = tmp.path().join("trace-9b").join("transcript.yml");

    let raw = std::fs::read(&fetched_path).expect("read fetched.html");
    assert_eq!(
        raw, fetch_result.raw_json,
        "fetched.html should be the raw JSON envelope"
    );

    let fetched_meta_text = std::fs::read_to_string(&fetched_yml).expect("read fetched.yml");
    assert!(
        fetched_meta_text.contains("extractor: github-api"),
        "missing extractor in fetched.yml: {fetched_meta_text}"
    );
    assert!(
        fetched_meta_text.contains("content-type: application/json"),
        "missing content-type in fetched.yml: {fetched_meta_text}"
    );

    let transcript = std::fs::read_to_string(&transcript_md).expect("read transcript.md");
    assert_eq!(transcript, fetch_result.transcript);
    let transcript_meta = std::fs::read_to_string(&transcript_yml).expect("read transcript.yml");
    assert!(
        transcript_meta.contains("extractor: github-render"),
        "missing extractor in transcript.yml: {transcript_meta}"
    );
}

#[test]
fn persist_github_stage_0_1_no_op_when_staging_disabled() {
    use crate::config::{StagingConfig, StagingLayout};
    use crate::github::{RepoFetch, RepoMetadata};
    use tempfile::TempDir;

    let tmp = TempDir::new().expect("tempdir");
    let staging = StagingConfig {
        enabled: false,
        root: tmp.path().to_path_buf(),
        layout: StagingLayout::PerTrace,
        ..StagingConfig::default()
    };
    let fetch_result = RepoFetch {
        transcript: "x".to_string(),
        metadata: RepoMetadata::default(),
        raw_json: b"{}".to_vec(),
    };
    persist_github_stage_0_1_if_staging(&staging, "trace-9b-off", "https://github.com/o/r", &fetch_result)
        .expect("no-op");
    assert!(!tmp.path().join("trace-9b-off").exists());
}

#[test]
fn persist_thread_transcript_writes_md_and_yml() {
    use crate::config::{StagingConfig, StagingLayout};
    use tempfile::TempDir;

    let tmp = TempDir::new().expect("tempdir");
    let staging = StagingConfig {
        enabled: true,
        root: tmp.path().to_path_buf(),
        layout: StagingLayout::PerTrace,
        ..StagingConfig::default()
    };
    let thread_md = "# Some thread\n\nFirst post.\nSecond post.\n";
    persist_thread_transcript_if_staging(&staging, "trace-9a", thread_md).expect("write");
    let md_path = tmp.path().join("trace-9a").join("transcript.md");
    let yml_path = tmp.path().join("trace-9a").join("transcript.yml");
    let md = std::fs::read_to_string(&md_path).expect("read md");
    assert_eq!(md, thread_md);
    let yml = std::fs::read_to_string(&yml_path).expect("read yml");
    assert!(
        yml.contains("extractor: thread-markdown-shim"),
        "expected extractor in yml: {yml}"
    );
}
