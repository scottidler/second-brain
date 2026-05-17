//! Stage 2 distillation entry point.
//!
//! Sits next to `summarize::Summarizer` and adds the structured `Distilled`
//! contract. As of Phase 4 the stage routes Article and Repo through their
//! Fabric-backed distillers and Idea / Image / VoiceNote through the no-LLM
//! distillers; Video / Thread still bail with an explicit Phases 5-6 message.
//!
//! Borg never writes to SQLite. The output of this stage is a `Distilled`
//! value that Stage 3 (publish) renders into the vault markdown file via
//! `distillers::render`; VaultWatcher then triggers `index_vault`.

use crate::config::{FabricConfig, PipelineConfig, StagingConfig};
use crate::github::{GitHubFetcher, RepoFetch};
use crate::types::IngestKind;
use distillers::{
    ArticleConfig, Dispatch, Dispatcher, DistillInputs, DistillKind, FabricCaller, FabricShell, RepoMetadata,
    VideoMetadata,
};
use eyre::{Context, Result, bail};
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
/// `Vocabulary*` is the only kind without a counterpart - it is explicitly
/// deferred per the staged pipeline doc.
pub fn distill_kind_from_ingest(kind: IngestKind) -> Result<DistillKind> {
    match kind {
        IngestKind::ArticleUrl => Ok(DistillKind::Article),
        IngestKind::GitHubUrl => Ok(DistillKind::Repo),
        IngestKind::YoutubeUrl => Ok(DistillKind::Video),
        IngestKind::ThreadUrl => Ok(DistillKind::Thread),
        IngestKind::Image => Ok(DistillKind::Image),
        IngestKind::VoiceNote => Ok(DistillKind::VoiceNote),
        IngestKind::Idea => Ok(DistillKind::Idea),
        IngestKind::VocabularyEn | IngestKind::VocabularyEs => {
            bail!("distillation not yet supported for vocabulary kinds")
        }
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
    ) -> Result<Distilled> {
        self.distill_with_metadata(kind, transcript, source_url, title_hint, None)
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
    ) -> Result<Distilled> {
        log::debug!(
            "DistillStage::distill: kind={} transcript_len={} source_url={:?} has_repo_metadata={}",
            kind,
            transcript.len(),
            source_url,
            repo_metadata.is_some()
        );
        let distill_kind = distill_kind_from_ingest(kind)?;
        let inputs = DistillInputs {
            transcript,
            source_url,
            title_hint,
            repo_metadata,
            video_metadata: None,
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
    ) -> Result<Distilled> {
        log::debug!(
            "DistillStage::distill_with_video_metadata: kind={} transcript_len={} source_url={:?} has_video_metadata={}",
            kind,
            transcript.len(),
            source_url,
            video_metadata.is_some()
        );
        let distill_kind = distill_kind_from_ingest(kind)?;
        let inputs = DistillInputs {
            transcript,
            source_url,
            title_hint,
            repo_metadata: None,
            video_metadata,
        };
        self.dispatcher.distill(distill_kind, inputs).await
    }
}

/// Filename inside the per-trace staging directory where shadow-mode
/// (Phases 3-4) and the future Stage-2 cutover write the structured payload.
pub const DISTILLED_FILENAME: &str = "distilled.yml";

/// Shadow-mode: run the article distiller against the raw article markdown
/// in the background and persist `distilled.yml` to the staging directory.
/// Never mutates the legacy pipeline output and never propagates errors -
/// the caller fires-and-forgets via `tokio::spawn` and any failure is logged.
///
/// Phase 3's job: collect empirical telemetry on the new pattern and
/// validator against real article captures, without risking the legacy
/// publish path. The cutover that replaces `process_article_fabric`'s
/// returned summary with the rendered Distilled body lands in a later phase.
pub async fn shadow_distill_article(
    fabric: FabricConfig,
    staging: StagingConfig,
    trace_id: String,
    url: String,
    article_md: String,
) {
    if !staging.enabled {
        return;
    }
    log::debug!(
        "shadow_distill_article: trace={trace_id} url={url} transcript_len={}",
        article_md.len()
    );
    let stage = DistillStage::from_fabric_config(&fabric);
    let started = std::time::Instant::now();
    let distilled = match stage
        .distill(IngestKind::ArticleUrl, &article_md, Some(&url), None)
        .await
    {
        Ok(d) => d,
        Err(e) => {
            log::warn!(
                "[{trace_id}] shadow_distill_article: dispatch error: {e:#} (shadow mode; legacy path unaffected)"
            );
            return;
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
        "[{trace_id}] shadow_distill_article: extractor={} model={} claims={} tags={} links={} fallback={} elapsed_ms={}",
        distilled.meta.extractor,
        distilled.meta.model,
        distilled.claims.len(),
        distilled.tags.len(),
        distilled.links.len(),
        fallback,
        elapsed_ms,
    );

    if let Err(e) = write_distilled_yml(&staging, &trace_id, &distilled) {
        log::warn!(
            "[{trace_id}] shadow_distill_article: persist distilled.yml failed: {e:#} (shadow mode; legacy path unaffected)"
        );
    }
}

/// Shadow-mode: run the GitHub fetcher and `RepoDistiller` against a github
/// URL in the background and persist `distilled.yml` to the staging
/// directory. Fires-and-forgets - never blocks or affects the legacy path.
///
/// Phase 4's job: collect empirical telemetry on the new repo pattern and
/// the GitHub REST fetcher against real captures, without risking the
/// legacy publish path. The cutover that replaces the legacy github
/// summary with the rendered Distilled body lands in a later phase.
pub async fn shadow_distill_repo(fabric: FabricConfig, staging: StagingConfig, trace_id: String, url: String) {
    if !staging.enabled {
        return;
    }
    log::debug!("shadow_distill_repo: trace={trace_id} url={url}");
    let Some((owner, repo)) = crate::github::parse_repo_url(&url) else {
        log::warn!("[{trace_id}] shadow_distill_repo: url is not a github repo root: {url} (shadow mode; skipping)");
        return;
    };
    let started = std::time::Instant::now();
    let fetch_result: RepoFetch = match GitHubFetcher::new().fetch_repo(&owner, &repo).await {
        Ok(r) => r,
        Err(e) => {
            log::warn!(
                "[{trace_id}] shadow_distill_repo: github fetch failed: {e:#} (shadow mode; legacy path unaffected)"
            );
            return;
        }
    };
    let metadata = repo_metadata_from_fetch(&fetch_result.metadata);
    let stage = DistillStage::from_fabric_config(&fabric);
    let distilled = match stage
        .distill_with_metadata(
            IngestKind::GitHubUrl,
            &fetch_result.transcript,
            Some(&url),
            None,
            Some(&metadata),
        )
        .await
    {
        Ok(d) => d,
        Err(e) => {
            log::warn!("[{trace_id}] shadow_distill_repo: dispatch error: {e:#} (shadow mode; legacy path unaffected)");
            return;
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
        "[{trace_id}] shadow_distill_repo: extractor={} model={} claims={} tags={} links={} fallback={} elapsed_ms={}",
        distilled.meta.extractor,
        distilled.meta.model,
        distilled.claims.len(),
        distilled.tags.len(),
        distilled.links.len(),
        fallback,
        elapsed_ms,
    );

    if let Err(e) = write_distilled_yml(&staging, &trace_id, &distilled) {
        log::warn!(
            "[{trace_id}] shadow_distill_repo: persist distilled.yml failed: {e:#} (shadow mode; legacy path unaffected)"
        );
    }
}

/// Shadow-mode: run the YouTube distiller against a fetched timestamped
/// transcript and persist `distilled.yml` to the staging directory. Fetches
/// the raw VTT separately (cheap parallel work) so the distiller sees real
/// timestamps regardless of which path the legacy `transcript` came from.
/// Fires-and-forgets - never blocks or affects the legacy publish path.
pub async fn shadow_distill_video(
    fabric: FabricConfig,
    pipeline: PipelineConfig,
    staging: StagingConfig,
    trace_id: String,
    url: String,
) {
    if !staging.enabled {
        return;
    }
    log::debug!("shadow_distill_video: trace={trace_id} url={url}");

    // Two concurrent yt-dlp calls: metadata (json) and raw subtitles (VTT).
    // They are independent and take longest in the legacy hot path; the
    // shadow path can wait without blocking the legacy publish.
    let yt_dlp_timeout = pipeline.yt_dlp_timeout_secs;
    let metadata_future = crate::youtube::fetch_metadata(&url, yt_dlp_timeout);
    let subtitles_future = crate::youtube::fetch_subtitles_raw(&url, &pipeline);
    let (metadata_result, subs_result) = tokio::join!(metadata_future, subtitles_future);

    let metadata = match metadata_result {
        Ok(m) => m,
        Err(e) => {
            log::warn!(
                "[{trace_id}] shadow_distill_video: yt-dlp metadata failed: {e:#} (shadow mode; legacy path unaffected)"
            );
            return;
        }
    };
    let video_metadata = video_metadata_from_yt_dlp(&metadata);
    let transcript = match subs_result {
        Ok(Some(vtt)) => {
            let segments = crate::youtube::parse_vtt_segments(&vtt);
            if segments.is_empty() {
                log::warn!("[{trace_id}] shadow_distill_video: empty VTT segments; aborting (shadow mode)");
                return;
            }
            render_timestamped_transcript(&segments)
        }
        Ok(None) => {
            log::warn!("[{trace_id}] shadow_distill_video: no subtitles available; aborting (shadow mode)");
            return;
        }
        Err(e) => {
            log::warn!(
                "[{trace_id}] shadow_distill_video: subtitle fetch failed: {e:#} (shadow mode; legacy path unaffected)"
            );
            return;
        }
    };

    let stage = DistillStage::from_fabric_config(&fabric);
    let started = std::time::Instant::now();
    let distilled = match stage
        .distill_with_video_metadata(
            IngestKind::YoutubeUrl,
            &transcript,
            Some(&url),
            Some(metadata.title.as_str()),
            Some(&video_metadata),
        )
        .await
    {
        Ok(d) => d,
        Err(e) => {
            log::warn!(
                "[{trace_id}] shadow_distill_video: dispatch error: {e:#} (shadow mode; legacy path unaffected)"
            );
            return;
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
        "[{trace_id}] shadow_distill_video: extractor={} model={} claims={} tags={} links={} anchors_stripped={} fallback={} elapsed_ms={}",
        distilled.meta.extractor,
        distilled.meta.model,
        distilled.claims.len(),
        distilled.tags.len(),
        distilled.links.len(),
        distilled.meta.validation.anchors_stripped,
        fallback,
        elapsed_ms,
    );

    if let Err(e) = write_distilled_yml(&staging, &trace_id, &distilled) {
        log::warn!(
            "[{trace_id}] shadow_distill_video: persist distilled.yml failed: {e:#} (shadow mode; legacy path unaffected)"
        );
    }
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
