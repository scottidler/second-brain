//! Idea-kind distiller. No LLM call - the user's text becomes the summary
//! verbatim, claims stay empty, links are regex-extracted. The full input
//! is preserved as `Distilled.transcript` so the published note is a
//! verbatim archive even after the global `MAX_SUMMARY_CHARS` cap in
//! `validate::enforce_bounds` clips the summary.
//!
//! As of Phase 9c-hotfix the per-distiller 280-char cap (a Rev-1 design
//! defect that silently truncated multi-paragraph idea text) has been
//! removed. The global 2000-char cap is the only schema protection now.

use async_trait::async_trait;
use chrono::Utc;
use eyre::Result;
use regex::Regex;
use std::sync::LazyLock;
use vault::distilled::{Distilled, DistilledMeta, Link, ValidationMeta};

use crate::{DistillExtractor, DistillInputs};

/// URL extraction regex, compiled once (was recompiled on every
/// `extract_links` call).
static LINK_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"https?://[^\s)\]]+").expect("link regex compiles"));

const ID: &str = "distill-idea-v2";

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
        let links = extract_links(trimmed);

        Ok(Distilled {
            summary: trimmed.to_string(),
            tldr: None,
            enumeration: None,
            key_ideas: Vec::new(),
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
                validation: ValidationMeta::default(),
            },
            transcript: Some(trimmed.to_string()),
        })
    }
}

fn extract_links(text: &str) -> Vec<Link> {
    let re = &*LINK_RE;
    re.find_iter(text)
        .map(|m| Link {
            url: m.as_str().to_string(),
            label: None,
        })
        .collect()
}

#[cfg(test)]
mod tests;
