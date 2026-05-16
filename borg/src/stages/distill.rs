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

use crate::config::{FabricConfig, StagingConfig};
use crate::github::{GitHubFetcher, RepoFetch};
use crate::types::IngestKind;
use distillers::{
    ArticleConfig, Dispatch, Dispatcher, DistillInputs, DistillKind, FabricCaller, FabricShell, RepoMetadata,
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
