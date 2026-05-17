//! Per-kind dispatcher.
//!
//! Distillers crate exports its own `DistillKind` rather than reaching into
//! `borg::types::IngestKind`. Borg translates IngestKind -> DistillKind at
//! the call site; cortex's backfill (Phase 7) infers DistillKind from
//! frontmatter `type:` + `source:`. This keeps the distillers crate free of
//! borg/cortex deps.
//!
//! As of Phase 5 the dispatcher is generic over a `FabricCaller` so each
//! Fabric-backed distiller (Article, Repo, Video, Thread) can be tested
//! with `FakeFabric` and run in production with `FabricShell`. Phases 3-5
//! wired Article, Repo, and Video; Thread still bails with an explicit
//! "ships in Phase 6" error.

use async_trait::async_trait;
use eyre::{Result, bail};

use crate::{
    ArticleConfig, ArticleDistiller, DistillExtractor, DistillInputs, FabricCaller, IdeaDistiller,
    PassthroughDistiller, RepoConfig, RepoDistiller, VideoConfig, VideoDistiller,
};

/// Kinds the distillers crate knows how to produce a `Distilled` for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DistillKind {
    Idea,
    Image,
    VoiceNote,
    Article,
    Repo,
    Video,
    Thread,
}

impl DistillKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Idea => "idea",
            Self::Image => "image",
            Self::VoiceNote => "voice-note",
            Self::Article => "article",
            Self::Repo => "repo",
            Self::Video => "video",
            Self::Thread => "thread",
        }
    }
}

/// Phase-4 dispatcher. Routes Idea / Image / VoiceNote through the no-LLM
/// distillers, Article through `ArticleDistiller<F>`, Repo through
/// `RepoDistiller<F>`, and Video through `VideoDistiller<F>`. Thread still
/// bails so callers see the cutover boundary.
#[derive(Debug, Clone)]
pub struct Dispatcher<F: FabricCaller + Clone> {
    pub idea: IdeaDistiller,
    pub passthrough: PassthroughDistiller,
    pub article: ArticleDistiller<F>,
    pub repo: RepoDistiller<F>,
    pub video: VideoDistiller<F>,
}

impl<F: FabricCaller + Clone> Dispatcher<F> {
    /// Build a dispatcher with a real `FabricCaller`. The article, repo, and
    /// video configs share `fabric` so cloning the caller once is enough;
    /// the no-LLM distillers ignore the fabric caller entirely.
    pub fn new(fabric: F, article_config: ArticleConfig) -> Self {
        let repo_config = RepoConfig {
            model: article_config.model.clone(),
            max_chars: article_config.max_chars,
            timeout_secs: article_config.timeout_secs,
        };
        let video_config = VideoConfig {
            model: article_config.model.clone(),
            max_chars: article_config.max_chars,
            timeout_secs: article_config.timeout_secs,
            ..VideoConfig::default()
        };
        Self {
            idea: IdeaDistiller,
            passthrough: PassthroughDistiller,
            article: ArticleDistiller::new(fabric.clone(), article_config),
            repo: RepoDistiller::new(fabric.clone(), repo_config),
            video: VideoDistiller::new(fabric, video_config),
        }
    }

    /// Build a dispatcher with explicit per-kind configs. Tests that want to
    /// tune one distiller without affecting the others can use this directly.
    pub fn with_configs(
        fabric: F,
        article_config: ArticleConfig,
        repo_config: RepoConfig,
        video_config: VideoConfig,
    ) -> Self {
        Self {
            idea: IdeaDistiller,
            passthrough: PassthroughDistiller,
            article: ArticleDistiller::new(fabric.clone(), article_config),
            repo: RepoDistiller::new(fabric.clone(), repo_config),
            video: VideoDistiller::new(fabric, video_config),
        }
    }
}

#[async_trait]
impl<F: FabricCaller + Clone> Dispatch for Dispatcher<F> {
    async fn distill(&self, kind: DistillKind, inputs: DistillInputs<'_>) -> Result<vault::distilled::Distilled> {
        log::debug!(
            "Dispatcher::distill: kind={} transcript_len={} source_url={:?}",
            kind.as_str(),
            inputs.transcript.len(),
            inputs.source_url
        );
        match kind {
            DistillKind::Idea => self.idea.distill(inputs).await,
            DistillKind::Image | DistillKind::VoiceNote => self.passthrough.distill(inputs).await,
            DistillKind::Article => self.article.distill(inputs).await,
            DistillKind::Repo => self.repo.distill(inputs).await,
            DistillKind::Video => self.video.distill(inputs).await,
            DistillKind::Thread => {
                bail!("dispatcher: kind {} is not wired yet; ships in Phase 6", kind.as_str());
            }
        }
    }
}

/// Object-safe API so call sites can hold a `&dyn Dispatch` when generics
/// would be awkward (e.g., heterogeneous test setups).
#[async_trait]
pub trait Dispatch: Send + Sync {
    async fn distill(&self, kind: DistillKind, inputs: DistillInputs<'_>) -> Result<vault::distilled::Distilled>;
}

#[cfg(test)]
mod tests;
