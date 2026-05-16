//! Stage 2 distillation entry point.
//!
//! Sits next to `summarize::Summarizer` and adds the structured `Distilled`
//! contract. Phase 2 routes only the no-LLM kinds (Idea / Image / VoiceNote)
//! through the new dispatcher; URL-bearing kinds continue to flow through
//! the legacy `Summarizer` path until Phases 3-6 ship their per-kind
//! distillers.
//!
//! Borg never writes to SQLite. The output of this stage is a `Distilled`
//! value that Stage 3 (publish) renders into the vault markdown file via
//! `distillers::render`; VaultWatcher then triggers `index_vault`.

use crate::types::IngestKind;
use distillers::{Dispatch, Dispatcher, DistillInputs, DistillKind};
use eyre::{Result, bail};
use vault::distilled::Distilled;

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

/// Phase-2 entry point. Wraps the dispatcher with the IngestKind translation
/// borg's pipeline.rs needs. Production callers construct a single
/// `DistillStage` once and reuse it; tests can build one cheaply per-test
/// since it owns no state besides the dispatcher.
#[derive(Debug, Default, Clone)]
pub struct DistillStage {
    dispatcher: Dispatcher,
}

impl DistillStage {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn distill(
        &self,
        kind: IngestKind,
        transcript: &str,
        source_url: Option<&str>,
        title_hint: Option<&str>,
    ) -> Result<Distilled> {
        log::debug!(
            "DistillStage::distill: kind={} transcript_len={} source_url={:?}",
            kind,
            transcript.len(),
            source_url
        );
        let distill_kind = distill_kind_from_ingest(kind)?;
        let inputs = DistillInputs {
            transcript,
            source_url,
            title_hint,
        };
        self.dispatcher.distill(distill_kind, inputs).await
    }
}

#[cfg(test)]
mod tests;
