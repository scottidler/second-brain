//! Passthrough distiller for kinds whose dedicated pattern hasn't shipped yet
//! (Image, VoiceNote). Mirrors `IdeaDistiller` semantically but records a
//! distinct extractor id so the source of the passthrough is auditable.

use async_trait::async_trait;
use chrono::Utc;
use eyre::Result;
use vault::distilled::{Distilled, DistilledMeta, ValidationMeta};

use crate::{DistillExtractor, DistillInputs};

const ID: &str = "distill-passthrough-v1";
const SUMMARY_CHAR_LIMIT: usize = 280;

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
        let (summary, truncations) = if trimmed.chars().count() > SUMMARY_CHAR_LIMIT {
            let summary: String = trimmed.chars().take(SUMMARY_CHAR_LIMIT).collect();
            (
                summary,
                vec![format!("summary:{}>{SUMMARY_CHAR_LIMIT}", trimmed.chars().count())],
            )
        } else {
            (trimmed.to_string(), Vec::new())
        };

        Ok(Distilled {
            summary,
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
                validation: ValidationMeta {
                    fallback_reason: None,
                    bounds_truncations: truncations,
                    anchors_stripped: 0,
                    raw_output: None,
                },
            },
        })
    }
}

#[cfg(test)]
mod tests;
