//! Passthrough distiller for trivial-input kinds. As of Phase 9c-image and
//! 9c-voicenote both Image and VoiceNote route to their own Fabric-backed
//! distillers; this struct remains in the crate as a stub for any future
//! kind whose published note is the verbatim input without LLM synthesis.
//!
//! The 280-char Rev-1 summary cap was a data-loss defect — multi-paragraph
//! Vision+OCR text or Groq transcripts routed through here would have been
//! silently truncated. As of Phase 9c-hotfix the cap is gone; the global
//! 2000-char `MAX_SUMMARY_CHARS` in `validate::enforce_bounds` is the only
//! schema protection, and the full input is preserved verbatim in
//! `Distilled.transcript`.

use async_trait::async_trait;
use chrono::Utc;
use eyre::Result;
use vault::distilled::{Distilled, DistilledMeta, ValidationMeta};

use crate::{DistillExtractor, DistillInputs};

const ID: &str = "distill-passthrough-v1";

#[derive(Debug, Default, Clone)]
pub struct PassthroughDistiller;

impl PassthroughDistiller {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl DistillExtractor for PassthroughDistiller {
    fn id(&self) -> &'static str {
        ID
    }

    async fn distill(&self, inputs: DistillInputs<'_>) -> Result<Distilled> {
        log::debug!(
            "PassthroughDistiller::distill: transcript_len={} source_url={:?}",
            inputs.transcript.len(),
            inputs.source_url
        );

        let trimmed = inputs.transcript.trim();

        Ok(Distilled {
            summary: trimmed.to_string(),
            tldr: None,
            enumeration: None,
            key_ideas: Vec::new(),
            claims: Vec::new(),
            tags: Vec::new(),
            links: Vec::new(),
            kind_specific: None,
            meta: DistilledMeta {
                extractor: ID.to_string(),
                model: "passthrough".to_string(),
                input_tokens: 0,
                output_tokens: 0,
                produced_at: Utc::now().to_rfc3339(),
                validation: ValidationMeta::default(),
            },
            transcript: Some(trimmed.to_string()),
        })
    }
}

#[cfg(test)]
mod tests;
