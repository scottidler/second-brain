//! Per-kind dispatcher.
//!
//! Distillers crate exports its own `DistillKind` rather than reaching into
//! `borg::types::IngestKind`. Borg translates IngestKind -> DistillKind at
//! the call site; cortex's backfill (Phase 7) infers DistillKind from
//! frontmatter `type:` + `source:`. This keeps the distillers crate free of
//! borg/cortex deps.
//!
//! Phase 2 wires the no-LLM kinds (Idea / Image / VoiceNote). Phases 3-6
//! extend the dispatcher with Fabric-backed kinds; the `Dispatcher` is
//! intentionally a concrete struct so subsequent phases add fields without
//! reshaping the trait surface.

use async_trait::async_trait;
use eyre::{Result, bail};

use crate::{DistillExtractor, DistillInputs, IdeaDistiller, PassthroughDistiller};

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

/// Phase 2 dispatcher. Only the no-LLM distillers are wired; Fabric-backed
/// kinds bail out so callers see an explicit error instead of a silent
/// passthrough fallback. Phases 3-6 will extend this to dispatch every kind.
#[derive(Debug, Default, Clone)]
pub struct Dispatcher {
    pub idea: IdeaDistiller,
    pub passthrough: PassthroughDistiller,
}

impl Dispatcher {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl Dispatch for Dispatcher {
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
            DistillKind::Article | DistillKind::Repo | DistillKind::Video | DistillKind::Thread => {
                bail!(
                    "dispatcher: kind {} is not wired in Phase 2; per-kind distillers ship in Phases 3-6",
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
