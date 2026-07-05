//! Stage 2 distillation entry point.
//!
//! Sits alongside the Gate-2 detector in `summarize.rs` and produces the
//! structured `Distilled` contract. As of Phase 6 the stage routes all
//! four URL kinds (Article, Repo, Video, Thread) through their Fabric-
//! backed distillers, plus Idea / Image / VoiceNote through the no-LLM
//! distillers. Only the `Vocabulary*` kinds remain outside the contract
//! (handled upstream).
//!
//! As of the post-Phase-6 cutover the `distill_for_publish_*` functions
//! are the primary path: `pipeline.rs` awaits them, renders the result
//! into the published note's body and frontmatter, and the legacy
//! `fabric::summarize` prose path is gone for URL kinds. Each function
//! also persists `distilled.yml` to the staging directory for forensics
//! and `borg replay` support.
//!
//! Borg never writes to SQLite. The output of this stage is a `Distilled`
//! value that Stage 3 (publish) renders into the vault markdown file via
//! `distillers::render`; VaultWatcher then triggers `index_vault`.

use crate::config::{FabricConfig, PipelineConfig, StagingConfig};
use crate::github::{GitHubFetcher, RepoFetch};
use crate::stages::artifact::{ArtifactStore, FsArtifactStore};
use crate::types::{IngestKind, TraceMeta};
use distillers::{
    ArticleConfig, Dispatch, Dispatcher, DistillInputs, DistillKind, FabricCaller, FabricShell, RepoMetadata,
    VideoMetadata,
};
use eyre::{Context, Result};
use vault::distilled::Distilled;

/// Translate borg's GitHub fetcher metadata into the distillers-crate
/// `RepoMetadata`. Drops the extra fields (description, default_branch)
/// the distiller doesn't use yet.
pub fn repo_metadata_from_fetch(meta: &crate::github::RepoMetadata) -> RepoMetadata {
    RepoMetadata {
        owner: meta.owner.clone(),
        repo: meta.repo.clone(),
        stars: meta.stars,
        primary_language: meta.primary_language.clone(),
        last_commit: meta.last_commit.clone(),
        topics: meta.topics.clone(),
    }
}

/// Translate borg's yt-dlp metadata into the distillers-crate `VideoMetadata`.
/// `channel` is yt-dlp's `uploader`; sentinel "Unknown" maps to None so the
/// distiller doesn't write that into frontmatter.
pub fn video_metadata_from_yt_dlp(meta: &crate::youtube::VideoMetadata) -> VideoMetadata {
    let channel = match meta.uploader.as_str() {
        "" | "Unknown" => None,
        other => Some(other.to_string()),
    };
    let duration_seconds = if meta.duration_secs > 0.0 {
        Some(meta.duration_secs.round() as u32)
    } else {
        None
    };
    VideoMetadata {
        channel,
        duration_seconds,
        published_at: None,
        // Stays empty here: this is a pure yt-dlp field mapper. The
        // description scan that populates `repos` runs at the
        // `distill_for_publish_video` seam where `metadata.description` is in
        // scope (see borg::github::extract_repo_slugs).
        repos: Vec::new(),
    }
}

/// Render parsed VTT segments as a timestamped transcript the distill-video
/// pattern expects. Each line is `[HH:MM:SS] text`.
pub fn render_timestamped_transcript(segments: &[(f64, String)]) -> String {
    let mut out = String::new();
    for (start_secs, text) in segments {
        let total = start_secs.max(0.0) as u32;
        let h = total / 3600;
        let m = (total % 3600) / 60;
        let s = total % 60;
        out.push_str(&format!("[{h:02}:{m:02}:{s:02}] "));
        out.push_str(text.trim());
        out.push('\n');
    }
    out
}

/// Convert borg's `IngestKind` to the distillers crate's `DistillKind`.
///
/// As of Phase 9c-hotfix every `IngestKind` has a counterpart - `Vocabulary*`
/// routes through `DistillKind::Vocabulary`, which the dispatcher dispatches
/// to `IdeaDistiller` (degenerate Distilled: summary = definition prose,
/// claims empty, full verbatim text preserved in `Distilled.transcript`).
pub fn distill_kind_from_ingest(kind: IngestKind) -> Result<DistillKind> {
    match kind {
        IngestKind::ArticleUrl => Ok(DistillKind::Article),
        IngestKind::GitHubUrl => Ok(DistillKind::Repo),
        IngestKind::YoutubeUrl => Ok(DistillKind::Video),
        IngestKind::ThreadUrl => Ok(DistillKind::Thread),
        IngestKind::Image => Ok(DistillKind::Image),
        IngestKind::VoiceNote => Ok(DistillKind::VoiceNote),
        IngestKind::Idea => Ok(DistillKind::Idea),
        IngestKind::VocabularyEn | IngestKind::VocabularyEs => Ok(DistillKind::Vocabulary),
    }
}

/// Build a Fabric-backed dispatcher from borg's FabricConfig. Production
/// callers go through `DistillStage::from_fabric_config`; tests build a
/// `DistillStage::with_dispatcher` to inject a `FakeFabric`-driven one.
pub fn dispatcher_from_fabric_config(config: &FabricConfig) -> Dispatcher<FabricShell> {
    let fabric = FabricShell::new(config.binary.clone());
    let article_config = ArticleConfig {
        model: config.model.clone(),
        max_chars: config.max_content_chars,
        timeout_secs: config.timeout_secs,
    };
    Dispatcher::new(fabric, article_config)
}

/// Stage-2 entry point. Wraps the dispatcher with the IngestKind translation
/// borg's pipeline.rs needs. Generic over the FabricCaller so tests can
/// inject `FakeFabric` while production uses `FabricShell`.
#[derive(Debug, Clone)]
pub struct DistillStage<F: FabricCaller + Clone> {
    dispatcher: Dispatcher<F>,
}

impl DistillStage<FabricShell> {
    pub fn from_fabric_config(config: &FabricConfig) -> Self {
        Self {
            dispatcher: dispatcher_from_fabric_config(config),
        }
    }
}

impl<F: FabricCaller + Clone> DistillStage<F> {
    pub fn with_dispatcher(dispatcher: Dispatcher<F>) -> Self {
        Self { dispatcher }
    }

    pub async fn distill(
        &self,
        kind: IngestKind,
        transcript: &str,
        source_url: Option<&str>,
        title_hint: Option<&str>,
        capture_note: Option<&str>,
    ) -> Result<Distilled> {
        self.distill_with_metadata(kind, transcript, source_url, title_hint, None, capture_note)
            .await
    }

    /// Repo-aware variant. Phase 4's shadow path passes `repo_metadata` from
    /// the GitHub REST API; non-repo dispatches leave it `None`.
    pub async fn distill_with_metadata(
        &self,
        kind: IngestKind,
        transcript: &str,
        source_url: Option<&str>,
        title_hint: Option<&str>,
        repo_metadata: Option<&RepoMetadata>,
        capture_note: Option<&str>,
    ) -> Result<Distilled> {
        log::debug!(
            "DistillStage::distill: kind={} transcript_len={} source_url={:?} has_repo_metadata={} has_capture_note={}",
            kind,
            transcript.len(),
            source_url,
            repo_metadata.is_some(),
            capture_note.is_some()
        );
        let distill_kind = distill_kind_from_ingest(kind)?;
        let inputs = DistillInputs {
            transcript,
            source_url,
            title_hint,
            repo_metadata,
            video_metadata: None,
            capture_note,
        };
        self.dispatcher.distill(distill_kind, inputs).await
    }

    /// Video-aware variant. Phase 5's shadow path passes `video_metadata`
    /// alongside the transcript so the distiller can validate anchors
    /// against `duration_seconds` and attach `KindPayload::Video`.
    pub async fn distill_with_video_metadata(
        &self,
        kind: IngestKind,
        transcript: &str,
        source_url: Option<&str>,
        title_hint: Option<&str>,
        video_metadata: Option<&distillers::VideoMetadata>,
        capture_note: Option<&str>,
    ) -> Result<Distilled> {
        log::debug!(
            "DistillStage::distill_with_video_metadata: kind={} transcript_len={} source_url={:?} has_video_metadata={} has_capture_note={}",
            kind,
            transcript.len(),
            source_url,
            video_metadata.is_some(),
            capture_note.is_some()
        );
        let distill_kind = distill_kind_from_ingest(kind)?;
        let inputs = DistillInputs {
            transcript,
            source_url,
            title_hint,
            repo_metadata: None,
            video_metadata,
            capture_note,
        };
        self.dispatcher.distill(distill_kind, inputs).await
    }
}

/// Filename inside the per-trace staging directory where shadow-mode
/// (Phases 3-4) and the future Stage-2 cutover write the structured payload.
pub const DISTILLED_FILENAME: &str = "distilled.yml";

/// Post-Phase-6 cutover: run the article distiller against the raw article
/// markdown and return the `Distilled`. Persists `distilled.yml` to the
/// staging directory on success for forensics and `borg replay` support.
/// On any error (dispatch failure, etc.) returns a `fallback_distilled`
/// with the appropriate reason tag so the caller always gets a usable
/// payload - publish never blocks on distillation.
/// Shared core for the simple single-call publish distillers (article, image,
/// voicenote, idea, vocab): dispatch the kind, fall back on error, log the
/// outcome, persist `distilled.yml`. The per-kind wrappers below supply the
/// label / kind / fallback-id. The map-reduce / payload-building distillers
/// (video, repo, thread) keep bespoke bodies because they do more than this
/// core.
#[allow(clippy::too_many_arguments)]
async fn run_distiller(
    fabric: &FabricConfig,
    staging: &StagingConfig,
    trace_id: &str,
    label: &str,
    kind: IngestKind,
    fallback_id: &str,
    transcript: &str,
    source_url: Option<&str>,
    title: Option<&str>,
    capture_note: Option<&str>,
) -> Distilled {
    log::debug!(
        "{label}: trace={trace_id} kind={kind} transcript_len={} title_hint={title:?} has_capture_note={}",
        transcript.len(),
        capture_note.is_some()
    );
    let stage = DistillStage::from_fabric_config(fabric);
    let started = std::time::Instant::now();
    let distilled = match stage.distill(kind, transcript, source_url, title, capture_note).await {
        Ok(d) => d,
        Err(e) => {
            log::warn!("[{trace_id}] {label}: dispatch error: {e:#}; using fallback");
            // fallback_distilled already preserves the full transcript when
            // non-empty, so the transcript-bearing kinds need no re-assert.
            distillers::fallback_distilled(fallback_id, "dispatch-error", transcript, None, &fabric.model)
        }
    };
    let elapsed_ms = started.elapsed().as_millis();
    let fallback = distilled
        .meta
        .validation
        .fallback_reason
        .clone()
        .unwrap_or_else(|| "none".to_string());
    log::info!(
        "[{trace_id}] {label}: kind={kind} extractor={} model={} claims={} tags={} links={} fallback={} elapsed_ms={}",
        distilled.meta.extractor,
        distilled.meta.model,
        distilled.claims.len(),
        distilled.tags.len(),
        distilled.links.len(),
        fallback,
        elapsed_ms,
    );
    if let Err(e) = write_distilled_yml(staging, trace_id, &distilled) {
        log::warn!("[{trace_id}] {label}: persist distilled.yml failed: {e:#}");
    }
    distilled
}

pub async fn distill_for_publish_article(
    fabric: &FabricConfig,
    staging: &StagingConfig,
    trace_id: &str,
    url: &str,
    article_md: &str,
    capture_note: Option<&str>,
) -> Distilled {
    run_distiller(
        fabric,
        staging,
        trace_id,
        "distill_for_publish_article",
        IngestKind::ArticleUrl,
        "distill-article-v1",
        article_md,
        Some(url),
        None,
        capture_note,
    )
    .await
}

/// Phase 9c-voicenote cutover: run the VoiceNote distiller against a Groq ASR
/// transcript. Short transcripts dispatch a single Fabric call; long
/// transcripts (>12K tokens) go through map-reduce. The raw Groq output is
/// always preserved in `Distilled.transcript` so the published note is a
/// verbatim archive of what the speaker actually said.
pub async fn distill_for_publish_voicenote(
    fabric: &FabricConfig,
    staging: &StagingConfig,
    trace_id: &str,
    transcript: &str,
    title_hint: Option<&str>,
) -> Distilled {
    run_distiller(
        fabric,
        staging,
        trace_id,
        "distill_for_publish_voicenote",
        IngestKind::VoiceNote,
        "distill-voicenote-v1",
        transcript,
        None,
        title_hint,
        None,
    )
    .await
}

/// Phase 9c-image cutover: run the Image distiller against the Vision+OCR
/// transcript. The full input is preserved as `Distilled.transcript` so the
/// published note carries verbatim extracted text below the LLM-distilled
/// `## Summary` / `## Claims`.
pub async fn distill_for_publish_image(
    fabric: &FabricConfig,
    staging: &StagingConfig,
    trace_id: &str,
    transcript: &str,
    title_hint: Option<&str>,
) -> Distilled {
    run_distiller(
        fabric,
        staging,
        trace_id,
        "distill_for_publish_image",
        IngestKind::Image,
        "distill-image-v1",
        transcript,
        None,
        title_hint,
        None,
    )
    .await
}

/// Phase 9c-hotfix cutover: run the Idea distiller against a free-form text
/// note. No Fabric call (synthesis-only); the full input is preserved as
/// `Distilled.transcript` so the published note is a verbatim archive even
/// after the global `MAX_SUMMARY_CHARS` cap clips the summary. Persists
/// `distilled.yml` on success. On any error returns a `fallback_distilled`.
pub async fn distill_for_publish_idea(
    fabric: &FabricConfig,
    staging: &StagingConfig,
    trace_id: &str,
    transcript: &str,
    title_hint: Option<&str>,
) -> Distilled {
    // IdeaDistiller emits distill-idea-v2 on success after the 9c-hotfix cap
    // deletion; the fallback path mirrors that ID and preserves the transcript.
    run_distiller(
        fabric,
        staging,
        trace_id,
        "distill_for_publish_idea",
        IngestKind::Idea,
        "distill-idea-v2",
        transcript,
        None,
        title_hint,
        None,
    )
    .await
}

/// Phase 9c-hotfix cutover: run the Vocabulary kind through the distiller
/// dispatcher (which routes to `IdeaDistiller` as a degenerate path - the
/// vocab definition is preserved verbatim in `Distilled.transcript`). Takes
/// the `IngestKind` so EN vs ES can flow through to translation; both map
/// to `DistillKind::Vocabulary` inside `DistillStage`.
pub async fn distill_for_publish_vocab(
    fabric: &FabricConfig,
    staging: &StagingConfig,
    trace_id: &str,
    kind: IngestKind,
    transcript: &str,
    title_hint: Option<&str>,
) -> Distilled {
    // Vocab routes to IdeaDistiller (distill-idea-v2); `kind` carries EN vs ES
    // through to translation. The shared core logs `kind=` so it stays visible.
    run_distiller(
        fabric,
        staging,
        trace_id,
        "distill_for_publish_vocab",
        kind,
        "distill-idea-v2",
        transcript,
        None,
        title_hint,
        None,
    )
    .await
}

/// Post-Phase-6 cutover: fetch the github repo's README + metadata via the
/// REST API and distill into a `Distilled`. Persists `distilled.yml` on
/// success. On any error (URL not a repo root, REST fetch failed, dispatch
/// failure) returns a `fallback_distilled` so publish always has a payload
/// to render - degraded distillation never blocks the note from landing.
pub async fn distill_for_publish_repo(
    fabric: &FabricConfig,
    staging: &StagingConfig,
    trace_id: &str,
    url: &str,
    article_md_fallback: &str,
    capture_note: Option<&str>,
) -> Distilled {
    log::debug!("distill_for_publish_repo: trace={trace_id} url={url}");
    let Some((owner, repo)) = crate::github::parse_repo_url(url) else {
        log::warn!("[{trace_id}] distill_for_publish_repo: url is not a github repo root: {url}; using fallback");
        return distillers::fallback_distilled(
            "distill-repo-v1",
            "not-a-repo-root",
            article_md_fallback,
            None,
            &fabric.model,
        );
    };
    let started = std::time::Instant::now();
    let fetch_result: RepoFetch = match GitHubFetcher::new().fetch_repo(&owner, &repo).await {
        Ok(r) => r,
        Err(e) => {
            log::warn!(
                "[{trace_id}] distill_for_publish_repo: github fetch failed: {e:#}; falling back to article_md path"
            );
            return distillers::fallback_distilled(
                "distill-repo-v1",
                "github-fetch-error",
                article_md_fallback,
                None,
                &fabric.model,
            );
        }
    };
    if let Err(e) = persist_github_stage_0_1_if_staging(staging, trace_id, url, &fetch_result) {
        log::warn!("[{trace_id}] distill_for_publish_repo: persist github artifacts failed: {e:#}");
    }
    let metadata = repo_metadata_from_fetch(&fetch_result.metadata);
    let stage = DistillStage::from_fabric_config(fabric);
    let distilled = match stage
        .distill_with_metadata(
            IngestKind::GitHubUrl,
            &fetch_result.transcript,
            Some(url),
            None,
            Some(&metadata),
            capture_note,
        )
        .await
    {
        Ok(d) => d,
        Err(e) => {
            log::warn!("[{trace_id}] distill_for_publish_repo: dispatch error: {e:#}; using fallback");
            distillers::fallback_distilled(
                "distill-repo-v1",
                "dispatch-error",
                &fetch_result.transcript,
                None,
                &fabric.model,
            )
        }
    };
    let elapsed_ms = started.elapsed().as_millis();
    let fallback = distilled
        .meta
        .validation
        .fallback_reason
        .clone()
        .unwrap_or_else(|| "none".to_string());
    log::info!(
        "[{trace_id}] distill_for_publish_repo: extractor={} model={} claims={} tags={} links={} fallback={} elapsed_ms={}",
        distilled.meta.extractor,
        distilled.meta.model,
        distilled.claims.len(),
        distilled.tags.len(),
        distilled.links.len(),
        fallback,
        elapsed_ms,
    );

    if let Err(e) = write_distilled_yml(staging, trace_id, &distilled) {
        log::warn!("[{trace_id}] distill_for_publish_repo: persist distilled.yml failed: {e:#}");
    }
    distilled
}

/// Post-Phase-6 cutover: run the YouTube distiller. Fetches yt-dlp metadata
/// and raw VTT subtitles in parallel so the distiller sees real timestamps,
/// rather than the legacy transcript that strips them. Persists
/// `distilled.yml` on success. On any error returns a `fallback_distilled`
/// using the supplied `transcript_fallback` so publish always has a payload
/// even when yt-dlp / VTT parsing fails.
pub async fn distill_for_publish_video(
    fabric: &FabricConfig,
    pipeline: &PipelineConfig,
    staging: &StagingConfig,
    trace_id: &str,
    url: &str,
    transcript_fallback: &str,
    title_hint: Option<&str>,
    capture_note: Option<&str>,
) -> Distilled {
    log::debug!("distill_for_publish_video: trace={trace_id} url={url}");

    let yt_dlp_timeout = pipeline.yt_dlp_timeout_secs;
    let metadata_future = crate::youtube::fetch_metadata(url, yt_dlp_timeout);
    let subtitles_future = crate::youtube::fetch_subtitles_raw(url, pipeline);
    let (metadata_result, subs_result) = tokio::join!(metadata_future, subtitles_future);

    let metadata = match metadata_result {
        Ok(m) => m,
        Err(e) => {
            log::warn!("[{trace_id}] distill_for_publish_video: yt-dlp metadata failed: {e:#}; using fallback");
            return distillers::fallback_distilled(
                "distill-video-v1",
                "yt-dlp-metadata-error",
                transcript_fallback,
                None,
                &fabric.model,
            );
        }
    };
    let mut video_metadata = video_metadata_from_yt_dlp(&metadata);
    // Harvest owner/repo slugs from the description here, at the seam where
    // `metadata.description` is in scope - keeping video_metadata_from_yt_dlp a
    // pure yt-dlp field mapper (see borg::github::extract_repo_slugs).
    video_metadata.repos = crate::github::extract_repo_slugs(&metadata.description);
    let transcript = match subs_result {
        Ok(Some(vtt)) => {
            let segments = crate::youtube::parse_vtt_segments(&vtt);
            if segments.is_empty() {
                log::warn!("[{trace_id}] distill_for_publish_video: empty VTT segments; using transcript_fallback");
                transcript_fallback.to_string()
            } else {
                render_timestamped_transcript(&segments)
            }
        }
        Ok(None) => {
            log::warn!("[{trace_id}] distill_for_publish_video: no subtitles available; using transcript_fallback");
            transcript_fallback.to_string()
        }
        Err(e) => {
            log::warn!(
                "[{trace_id}] distill_for_publish_video: subtitle fetch failed: {e:#}; using transcript_fallback"
            );
            transcript_fallback.to_string()
        }
    };

    let stage = DistillStage::from_fabric_config(fabric);
    let started = std::time::Instant::now();
    let resolved_title = title_hint
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| metadata.title.clone());
    let distilled = match stage
        .distill_with_video_metadata(
            IngestKind::YoutubeUrl,
            &transcript,
            Some(url),
            Some(resolved_title.as_str()),
            Some(&video_metadata),
            capture_note,
        )
        .await
    {
        Ok(d) => d,
        Err(e) => {
            log::warn!("[{trace_id}] distill_for_publish_video: dispatch error: {e:#}; using fallback");
            distillers::fallback_distilled("distill-video-v1", "dispatch-error", &transcript, None, &fabric.model)
        }
    };
    let elapsed_ms = started.elapsed().as_millis();
    let fallback = distilled
        .meta
        .validation
        .fallback_reason
        .clone()
        .unwrap_or_else(|| "none".to_string());
    log::info!(
        "[{trace_id}] distill_for_publish_video: extractor={} model={} claims={} tags={} links={} anchors_stripped={} fallback={} elapsed_ms={}",
        distilled.meta.extractor,
        distilled.meta.model,
        distilled.claims.len(),
        distilled.tags.len(),
        distilled.links.len(),
        distilled.meta.validation.anchors_stripped,
        fallback,
        elapsed_ms,
    );

    if let Err(e) = write_distilled_yml(staging, trace_id, &distilled) {
        log::warn!("[{trace_id}] distill_for_publish_video: persist distilled.yml failed: {e:#}");
    }
    distilled
}

/// Post-Phase-6 cutover: run the thread distiller against the markdown rendered
/// by the standard Stage-0 fetcher chain (Jina / fabric -u / browser-UA +
/// markitdown) for X/Reddit/HN URLs. The returned `Distilled` is the source of
/// truth for the published note's body and `cortex-thread-*` frontmatter.
///
/// Persists two artifacts in the per-trace staging directory:
/// - `transcript.md` + `transcript.yml` (the rendered markdown the distiller
///   saw, with `extractor: thread-markdown-shim`). The Phase-6 audit verified
///   the rendered markdown is sufficient input for `distill-thread`; a
///   dedicated X/Reddit/HN JSON fetcher remains tracked as a potential future
///   enhancement but is not required.
/// - `distilled.yml` (the structured contract for replay).
pub async fn distill_for_publish_thread(
    fabric: &FabricConfig,
    staging: &StagingConfig,
    trace_id: &str,
    url: &str,
    thread_md: &str,
    capture_note: Option<&str>,
) -> Distilled {
    log::debug!(
        "distill_for_publish_thread: trace={trace_id} url={url} transcript_len={}",
        thread_md.len()
    );
    if let Err(e) = persist_thread_transcript_if_staging(staging, trace_id, thread_md) {
        log::warn!("[{trace_id}] distill_for_publish_thread: persist transcript.md failed: {e:#}");
    }
    let stage = DistillStage::from_fabric_config(fabric);
    let started = std::time::Instant::now();
    let distilled = match stage
        .distill(IngestKind::ThreadUrl, thread_md, Some(url), None, capture_note)
        .await
    {
        Ok(d) => d,
        Err(e) => {
            log::warn!("[{trace_id}] distill_for_publish_thread: dispatch error: {e:#}; using fallback");
            distillers::fallback_distilled("distill-thread-v1", "dispatch-error", thread_md, None, &fabric.model)
        }
    };
    let elapsed_ms = started.elapsed().as_millis();
    let fallback = distilled
        .meta
        .validation
        .fallback_reason
        .clone()
        .unwrap_or_else(|| "none".to_string());
    let platform = match &distilled.kind_specific {
        Some(vault::distilled::KindPayload::Thread(p)) => p.platform.as_str(),
        _ => "unknown",
    };
    log::info!(
        "[{trace_id}] distill_for_publish_thread: extractor={} model={} platform={} claims={} tags={} links={} fallback={} elapsed_ms={}",
        distilled.meta.extractor,
        distilled.meta.model,
        platform,
        distilled.claims.len(),
        distilled.tags.len(),
        distilled.links.len(),
        fallback,
        elapsed_ms,
    );

    if let Err(e) = write_distilled_yml(staging, trace_id, &distilled) {
        log::warn!("[{trace_id}] distill_for_publish_thread: persist distilled.yml failed: {e:#}");
    }
    distilled
}

/// Persist the GitHub-API JSON envelope (Stage 0) and rendered repo transcript
/// (Stage 1) into the per-trace staging directory so `borg replay --from-stage 1`
/// can re-run distillation against the same input without re-hitting the
/// GitHub API. The article-fetch chain does not run for github root URLs (the
/// repo path is dispatched directly in `pipeline.rs`), so without this helper
/// neither `fetched.html` nor `transcript.md` lands for github traces.
///
/// Stage-0 artifact: `fetched.html` carries the raw `{"repo": ..., "readme": ...}`
/// JSON envelope; `fetched.yml` carries `extractor: github-api` /
/// `content_type: application/json`.
///
/// Stage-1 artifact: `transcript.md` carries the rendered transcript the
/// distiller saw; `transcript.yml` carries `extractor: github-render` to
/// distinguish from upstream article-fetch transcripts.
///
/// No-op when `staging.enabled = false`. Failures of either write WARN-log
/// individually rather than propagating; the distillation downstream is the
/// authoritative path.
pub fn persist_github_stage_0_1_if_staging(
    staging: &StagingConfig,
    trace_id: &str,
    url: &str,
    fetch_result: &RepoFetch,
) -> Result<()> {
    if !staging.enabled {
        return Ok(());
    }
    let store = FsArtifactStore::from_config(staging);
    let fetched_meta = crate::types::FetchMeta {
        source: url.to_string(),
        extractor: "github-api".to_string(),
        status: 200,
        content_type: Some("application/json".to_string()),
        bytes: fetch_result.raw_json.len() as u64,
        sha256: crate::stages::artifact::sha256_hex(&fetch_result.raw_json),
        fallbacks_attempted: Vec::new(),
        author: None,
    };
    if let Err(e) = store.write_fetched(trace_id, &fetch_result.raw_json, &fetched_meta) {
        log::warn!("[{trace_id}] persist_github_stage_0_1: fetched.html write failed: {e:#}");
    }
    let trace_meta = TraceMeta {
        extractor: "github-render".to_string(),
        ..TraceMeta::default()
    };
    if let Err(e) = store.write_transcript(trace_id, &fetch_result.transcript, &trace_meta) {
        log::warn!("[{trace_id}] persist_github_stage_0_1: transcript.md write failed: {e:#}");
    }
    Ok(())
}

/// Persist the rendered thread markdown (the input the thread distiller saw)
/// as Stage-1 `transcript.md` + `transcript.yml`. Mirrors the `fetched.html`
/// persistence the article-fetch chain already performs for thread URLs, so a
/// future `borg replay --from-stage 2` has both Stage-0 (fetched bytes) and
/// Stage-1 (rendered markdown) artifacts available without re-fetching.
///
/// `extractor: "thread-markdown-shim"` distinguishes this transcript from the
/// upstream article-fetch transcripts. No-op when `staging.enabled = false`.
pub fn persist_thread_transcript_if_staging(staging: &StagingConfig, trace_id: &str, thread_md: &str) -> Result<()> {
    if !staging.enabled {
        return Ok(());
    }
    let store = FsArtifactStore::from_config(staging);
    let meta = TraceMeta {
        extractor: "thread-markdown-shim".to_string(),
        ..TraceMeta::default()
    };
    store.write_transcript(trace_id, thread_md, &meta)?;
    Ok(())
}

/// Persist `distilled.yml` into the per-trace staging directory. Atomic
/// (temp-then-rename) and idempotent across replays. Only enabled when
/// `staging.enabled = true`.
pub fn write_distilled_yml(staging: &StagingConfig, trace_id: &str, distilled: &Distilled) -> Result<()> {
    if !staging.enabled {
        return Ok(());
    }
    let yaml = serde_yaml::to_string(distilled).context("serialize Distilled to yaml")?;
    let trace_dir = std::path::PathBuf::from(&staging.root).join(trace_id);
    std::fs::create_dir_all(&trace_dir).with_context(|| format!("create trace dir {}", trace_dir.display()))?;
    let final_path = trace_dir.join(DISTILLED_FILENAME);
    let tmp_path = final_path.with_extension("yml.tmp");
    std::fs::write(&tmp_path, yaml.as_bytes()).with_context(|| format!("write {}", tmp_path.display()))?;
    std::fs::rename(&tmp_path, &final_path)
        .with_context(|| format!("rename {} -> {}", tmp_path.display(), final_path.display()))?;
    log::debug!(
        "write_distilled_yml: trace={trace_id} path={} bytes={}",
        final_path.display(),
        yaml.len()
    );
    Ok(())
}

#[cfg(test)]
mod tests;
