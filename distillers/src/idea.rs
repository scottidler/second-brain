//! Idea-kind distiller. No LLM call - the user's text becomes the summary
//! verbatim, claims stay empty, links are regex-extracted.

use async_trait::async_trait;
use chrono::Utc;
use eyre::Result;
use regex::Regex;
use vault::distilled::{Distilled, DistilledMeta, Link, ValidationMeta};

use crate::{DistillExtractor, DistillInputs};

const ID: &str = "distill-idea-v1";
const SUMMARY_CHAR_LIMIT: usize = 280;

#[derive(Debug, Default, Clone)]
pub struct IdeaDistiller;

impl IdeaDistiller {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl DistillExtractor for IdeaDistiller {
    fn id(&self) -> &'static str {
        ID
    }

    async fn distill(&self, inputs: DistillInputs<'_>) -> Result<Distilled> {
        log::debug!(
            "IdeaDistiller::distill: transcript_len={} title_hint={:?}",
            inputs.transcript.len(),
            inputs.title_hint
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

        let links = extract_links(trimmed);

        Ok(Distilled {
            summary,
            claims: Vec::new(),
            tags: Vec::new(),
            links,
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

fn extract_links(text: &str) -> Vec<Link> {
    let re = Regex::new(r"https?://[^\s)\]]+").expect("link regex compiles");
    re.find_iter(text)
        .map(|m| Link {
            url: m.as_str().to_string(),
            label: None,
        })
        .collect()
}

#[cfg(test)]
mod tests;
