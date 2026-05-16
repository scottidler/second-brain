//! Per-kind dispatcher.
//!
//! Distillers crate exports its own `DistillKind` rather than reaching into
//! `borg::types::IngestKind`. Borg translates IngestKind -> DistillKind at
//! the call site; cortex's backfill (Phase 7) infers DistillKind from
//! frontmatter `type:` + `source:`. This keeps the distillers crate free of
//! borg/cortex deps.
//!
//! As of Phase 4 the dispatcher is generic over a `FabricCaller` so each
//! Fabric-backed distiller (Article, Repo, Video, Thread) can be tested
//! with `FakeFabric` and run in production with `FabricShell`. Phase 3
//! wired Article; Phase 4 wires Repo; Video / Thread still bail with an
//! explicit "ships in Phases 5-6" error.

use async_trait::async_trait;
use eyre::{Result, bail};

use crate::{
    ArticleConfig, ArticleDistiller, DistillExtractor, DistillInputs, FabricCaller, IdeaDistiller,
    PassthroughDistiller, RepoConfig, RepoDistiller,
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
/// distillers, Article through the Fabric-backed `ArticleDistiller<F>`, and
/// Repo through `RepoDistiller<F>`. Video / Thread still bail so callers
/// see the cutover boundary.
#[derive(Debug, Clone)]
pub struct Dispatcher<F: FabricCaller + Clone> {
    pub idea: IdeaDistiller,
    pub passthrough: PassthroughDistiller,
    pub article: ArticleDistiller<F>,
    pub repo: RepoDistiller<F>,
}

impl<F: FabricCaller + Clone> Dispatcher<F> {
    /// Build a dispatcher with a real `FabricCaller`. The article and repo
    /// configs share `fabric` so cloning the caller once is enough; the
    /// no-LLM distillers ignore the fabric caller entirely.
    pub fn new(fabric: F, article_config: ArticleConfig) -> Self {
        let repo_config = RepoConfig {
            model: article_config.model.clone(),
            max_chars: article_config.max_chars,
            timeout_secs: article_config.timeout_secs,
        };
        Self {
            idea: IdeaDistiller,
            passthrough: PassthroughDistiller,
            article: ArticleDistiller::new(fabric.clone(), article_config),
            repo: RepoDistiller::new(fabric, repo_config),
        }
    }

    /// Build a dispatcher with explicit per-kind configs. Tests that want to
    /// tune one distiller without affecting the other can use this directly.
    pub fn with_configs(fabric: F, article_config: ArticleConfig, repo_config: RepoConfig) -> Self {
        Self {
            idea: IdeaDistiller,
            passthrough: PassthroughDistiller,
            article: ArticleDistiller::new(fabric.clone(), article_config),
            repo: RepoDistiller::new(fabric, repo_config),
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
            DistillKind::Video | DistillKind::Thread => {
                bail!(
                    "dispatcher: kind {} is not wired yet; ships in Phases 5-6",
                    kind.as_str()
                );
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
