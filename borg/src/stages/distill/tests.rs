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
fn session_kind_maps_to_session_distill_kind() {
    // Harvest-clyde-sessions Phase 4: IngestKind::Session routes to
    // DistillKind::Session (the SessionDistiller).
    assert_eq!(
        distill_kind_from_ingest(IngestKind::Session).expect("map session"),
        DistillKind::Session
    );
}

#[tokio::test]
async fn session_distillation_is_subject_to_gate_2() {
    // SUCCESS CRITERION: Gate-2 (the paraphrase-of-a-block-page backstop)
    // applies to session distillation. A session whose distiller summary
    // matches a Gate-2 signature is caught exactly like every other kind's
    // summary - the gate runs on the produced summary regardless of kind.
    let fake = Arc::new(FakeFabric::new());
    fake.set_response(
        "distill-session",
        "summary: \"The provided input contains an error message indicating access was denied.\"\nclaims: []\ntags: []\nlinks: []\n",
    );
    let stage = make_stage_with_fake(fake);
    let metadata = distillers::SessionMetadata {
        repo: Some("scottidler/second-brain".to_string()),
        session_ids: vec!["871f6428".to_string()],
        msg_count: 486,
        date_start: None,
        date_end: None,
        body_truncated: false,
    };
    let distilled = stage
        .distill_with_session_metadata("USER: x\nASSISTANT: y", Some("clyde://871f6428"), Some(&metadata))
        .await
        .expect("distill");
    let reason = crate::stages::detect_paraphrased_block(&distilled.summary);
    assert!(
        reason.is_some(),
        "Gate-2 must flag a session summary that paraphrases a block page: {:?}",
        distilled.summary
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
        .distill(IngestKind::Idea, "A small idea.", None, None, None)
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
async fn distill_stage_handles_image_through_image_distiller() {
    // Phase 9c-image: Image now routes to ImageDistiller (Fabric-backed),
    // not PassthroughDistiller. With a stub FakeFabric (no canned response)
    // the call falls back; the fallback path mirrors the live extractor id.
    let stage = make_stage();
    let distilled = stage
        .distill(IngestKind::Image, "ocr'd text", None, None, None)
        .await
        .expect("distill");
    assert_eq!(distilled.meta.extractor, "distill-image-v1");
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
            None,
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
        repos: vec![],
    };
    let distilled = stage
        .distill_with_video_metadata(
            IngestKind::YoutubeUrl,
            "[00:00:00] Welcome.\n[00:00:30] Main claim here.",
            Some("https://youtu.be/abc"),
            Some("Title"),
            Some(&metadata),
            None,
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
        tldr: None,
        enumeration: None,
        key_ideas: Vec::new(),
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
        tldr: None,
        enumeration: None,
        key_ideas: Vec::new(),
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

// ---------------------------------------------------------------------------
// Article source/quality gate. Moved here from pipeline/tests.rs (2026-07-07
// distillation-output-restore, Phase 3): the gate now lives in this module
// and runs BEFORE `write_distilled_yml`, not just before render, so a
// non-clean or chrome-heavy transcript never reaches staging or embeddings.
// The pre-Phase-3 `enabled` toggle (`distill.article-transcript`) is gone
// entirely, so these tests exercise only `clean_source`.
// ---------------------------------------------------------------------------

fn distilled_with(transcript: &str) -> vault::distilled::Distilled {
    vault::distilled::Distilled {
        transcript: Some(transcript.to_string()),
        ..Default::default()
    }
}

/// A clean article body (long prose lines) passes the coarse quality gate.
#[test]
fn transcript_quality_keeps_clean_article() {
    let clean = "Anthropic today announced its most agentic Sonnet model yet, with \
        substantial gains on real-world coding and agentic tool-use benchmarks.\n\n\
        The model is available immediately across the API and Claude Code, and the \
        company published evaluations covering software engineering and retrieval.\n\n\
        Early testers report meaningfully fewer wrong-tool calls on long tasks.";
    assert!(transcript_quality_ok(clean), "clean prose must pass");
}

/// Criterion (d): a link-heavy-but-legit article (HN-style roundup: real titles
/// + editorial context per line) PASSES - the false-positive guard.
#[test]
fn transcript_quality_keeps_link_heavy_legit() {
    let roundup = (0..12)
        .map(|i| {
            format!(
                "- [A Substantial Article Title Number {i} About Systems](https://example.com/{i}) - one line of real editorial context"
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        transcript_quality_ok(&roundup),
        "link-dense-but-legit article must pass (chrome guard)"
    );
}

/// A legitimately short article (real prose, above the min) passes.
#[test]
fn transcript_quality_keeps_short_legit() {
    let short = "This is a short but legitimate article. It has a couple of real \
        sentences of prose that comfortably clear the minimum length, and its lines \
        are long enough to read as article body rather than navigation chrome.";
    assert!(transcript_quality_ok(short), "short-but-legit prose must pass");
}

/// A prose-like page is NOT dropped by the coarse gate - bot-walls are Phase-3's
/// `detect_block_page` job; this gate must not overreach onto prose.
#[test]
fn transcript_quality_keeps_prose_like_content() {
    let prose = "Please verify you are a human to continue reading this article. \
        We use a short verification step to protect the site from automated abuse, \
        and normal readers are let through within a few seconds without any action.";
    assert!(
        transcript_quality_ok(prose),
        "prose-like content must not be dropped by the coarse gate"
    );
}

/// Criteria (a)/(b): a chrome-heavy transcript (country-dropdown / nav wall of
/// short lines) FAILS the coarse gate.
#[test]
fn transcript_quality_drops_chrome_heavy() {
    let chrome = [
        "Afghanistan",
        "Albania",
        "Algeria",
        "Andorra",
        "Angola",
        "Argentina",
        "Armenia",
        "Australia",
        "Austria",
        "Azerbaijan",
        "Bahamas",
        "Bahrain",
        "Bangladesh",
        "Barbados",
        "Belarus",
        "Belgium",
        "Belize",
        "Benin",
        "Bhutan",
        "Bolivia",
        "Botswana",
        "Brazil",
        "Subscribe",
        "Sign in",
        "Menu",
        "Home",
        "About",
        "Contact",
        "Newsletter",
        "Follow us",
    ]
    .join("\n");
    assert!(
        chrome.chars().count() >= 200,
        "fixture must clear the min-length check first"
    );
    assert!(
        !transcript_quality_ok(&chrome),
        "a wall of short chrome lines must fail"
    );
}

/// Too-thin content fails on the length floor.
#[test]
fn transcript_quality_drops_too_short() {
    assert!(
        !transcript_quality_ok("# Hi\n\nnot enough"),
        "below the min-length floor must fail"
    );
}

/// gate_article_transcript, clean source: a clean transcript is KEPT
/// (criterion c), a chrome-heavy transcript is CLEARED (criteria a/b - one
/// borg-layer gate on the final Distilled covers both the success and
/// fallback paths).
#[test]
fn gate_keeps_clean_and_clears_chrome_heavy() {
    let clean = "This is a genuine article body with several sentences of real prose \
        that clearly exceeds the minimum length and reads as content, not chrome, so \
        the coarse quality gate leaves the transcript in place for publication.";
    let kept = gate_article_transcript(distilled_with(clean), true);
    assert!(
        kept.transcript.is_some(),
        "clean transcript must be kept for a clean source"
    );

    let chrome = vec!["Afghanistan"; 40].join("\n");
    let cleared = gate_article_transcript(distilled_with(&chrome), true);
    assert!(
        cleared.transcript.is_none(),
        "chrome-heavy transcript must be cleared even for a clean source"
    );
}

/// Source gate (finding #1 fix): a clean-LOOKING transcript from a NON-clean fetch
/// source (fallthrough to fabric-u/Jina/browser-UA) is dropped even though it would
/// pass the coarse quality gate - only cleanly-extracted output is ever stored.
/// This is what makes "chrome is never published on the fallback path" true; the
/// coarse ratio alone keeps a 0.61-short-line trainwreck.
#[test]
fn gate_clears_transcript_from_non_clean_source() {
    let clean = "A perfectly clean-looking article body with several real sentences \
        of prose that easily clears the coarse quality gate, but it came from a \
        non-readable fallback fetch, so it must not be stored as a transcript.";
    let out = gate_article_transcript(distilled_with(clean), false);
    assert!(
        out.transcript.is_none(),
        "a non-clean fetch source must drop the transcript even when the text looks clean"
    );
}

/// Ordering regression (2026-07-07 distillation-output-restore, Phase 3): the
/// source gate must clear a non-clean transcript BEFORE `distilled.yml` is
/// persisted, not after - otherwise chrome junk from a non-clean fetch would
/// land in staging and the transcript-chunk embedding source even though the
/// rendered note looks clean. Exercises the exact seam
/// `distill_for_publish_article` calls (`gate_and_persist_article`) without
/// invoking the live Fabric dispatch.
#[test]
fn gate_and_persist_article_clears_before_staged_write() {
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
    let dirty = Distilled {
        summary: "s".to_string(),
        transcript: Some("Raw fallback-fetch chrome that must never reach staging.".to_string()),
        meta: DistilledMeta {
            extractor: "distill-article-v1".to_string(),
            model: "test".to_string(),
            produced_at: "2026-07-07T00:00:00Z".to_string(),
            validation: ValidationMeta::default(),
            ..DistilledMeta::default()
        },
        ..Default::default()
    };

    let gated = gate_and_persist_article(&staging, "trace-gate-order", dirty, false);
    assert!(
        gated.transcript.is_none(),
        "returned Distilled must have the transcript cleared"
    );

    let staged_path = tmp.path().join("trace-gate-order").join(DISTILLED_FILENAME);
    let staged = std::fs::read_to_string(&staged_path).expect("staged distilled.yml must exist");
    assert!(
        !staged.contains("transcript:"),
        "staged distilled.yml must not carry a transcript when the source gate cleared it: {staged}"
    );
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
