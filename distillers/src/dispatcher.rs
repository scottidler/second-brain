//! Per-kind dispatcher.
//!
//! Distillers crate exports its own `DistillKind` rather than reaching into
//! `borg::types::IngestKind`. Borg translates IngestKind -> DistillKind at
//! the call site; cortex's backfill (Phase 7) infers DistillKind from
//! frontmatter `type:` + `source:`. This keeps the distillers crate free of
//! borg/cortex deps.
//!
//! As of Phase 3 the dispatcher is generic over a `FabricCaller` so each
//! Fabric-backed distiller (Article, Repo, Video, Thread) can be tested
//! with `FakeFabric` and run in production with `FabricShell`. Phase 3
//! wires Article; Repo / Video / Thread still bail with an explicit
//! "ships in Phases 4-6" error.

use async_trait::async_trait;
use eyre::{Result, bail};

use crate::{
    ArticleConfig, ArticleDistiller, DistillExtractor, DistillInputs, FabricCaller, IdeaDistiller, PassthroughDistiller,
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

/// Phase-3 dispatcher. Routes Idea / Image / VoiceNote through the no-LLM
/// distillers and Article through the Fabric-backed `ArticleDistiller<F>`.
/// Repo / Video / Thread still bail so callers see the cutover boundary.
#[derive(Debug, Clone)]
pub struct Dispatcher<F: FabricCaller + Clone> {
    pub idea: IdeaDistiller,
    pub passthrough: PassthroughDistiller,
    pub article: ArticleDistiller<F>,
}

impl<F: FabricCaller + Clone> Dispatcher<F> {
    /// Build a dispatcher with a real `FabricCaller`. The article config is
    /// per-pattern; the no-LLM distillers ignore the fabric caller entirely.
    pub fn new(fabric: F, article_config: ArticleConfig) -> Self {
        Self {
            idea: IdeaDistiller,
            passthrough: PassthroughDistiller,
            article: ArticleDistiller::new(fabric, article_config),
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
            DistillKind::Repo | DistillKind::Video | DistillKind::Thread => {
                bail!(
                    "dispatcher: kind {} is not wired yet; ships in Phases 4-6",
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
