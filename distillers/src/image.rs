//! Image-kind distiller.
//!
//! Calls Fabric with `distill-image.md` against the concatenation of
//! `## Description` (Vision API output) and `## Extracted Text` (OCR), parses
//! the YAML body into a `Distilled`, and preserves the raw concatenation as
//! `Distilled.transcript` so the published note is a verbatim archive even
//! after the LLM collapses the original.
//!
//! Single Fabric call only - image transcripts (Vision + OCR concat) are
//! bounded by the upstream extractor pipeline (`process_image_inner` in borg)
//! and don't need the chunk/reduce map step that long-audio and long-video
//! transcripts require.

use crate::parse::{PatternYaml, approx_tokens, strip_fences};
use async_trait::async_trait;
use chrono::Utc;
use eyre::Result;
use vault::distilled::{Claim, Distilled, DistilledMeta, Link, ValidationMeta};

use crate::{
    DistillExtractor, DistillInputs, FabricCaller, FabricRequest, enforce_bounds, fallback_distilled, max_claims,
    validate::MAX_SUMMARY_CHARS,
};

const ID: &str = "distill-image-v1";
const PATTERN: &str = "distill-image";

/// Tunables for the image distiller. Same shape as `ArticleConfig` for
/// homogeneity at the dispatcher; image transcripts are typically small, so
/// the `max_chars` cap rarely bites.
#[derive(Debug, Clone)]
pub struct ImageConfig {
    pub model: String,
    pub max_chars: usize,
    pub timeout_secs: u64,
}

impl Default for ImageConfig {
    fn default() -> Self {
        Self {
            model: String::new(),
            max_chars: 32_000,
            timeout_secs: 60,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ImageDistiller<F: FabricCaller + Clone> {
    fabric: F,
    config: ImageConfig,
}

impl<F: FabricCaller + Clone> ImageDistiller<F> {
    pub fn new(fabric: F, config: ImageConfig) -> Self {
        Self { fabric, config }
    }
}

#[async_trait]
impl<F: FabricCaller + Clone> DistillExtractor for ImageDistiller<F> {
    fn id(&self) -> &'static str {
        ID
    }

    async fn distill(&self, inputs: DistillInputs<'_>) -> Result<Distilled> {
        log::debug!(
            "ImageDistiller::distill: transcript_len={} title_hint={:?}",
            inputs.transcript.len(),
            inputs.title_hint
        );

        let request = FabricRequest {
            pattern: PATTERN.to_string(),
            input: inputs.transcript.to_string(),
            model: self.config.model.clone(),
            max_chars: self.config.max_chars,
            timeout_secs: self.config.timeout_secs,
        };

        let raw = match self.fabric.call(request).await {
            Ok(text) => text,
            Err(err) => {
                let msg = format!("{err}");
                let reason = if vault::fabric::FabricError::is_timeout(&err) {
                    "fabric-timeout"
                } else {
                    "fabric-error"
                };
                log::warn!("ImageDistiller: fabric call failed: {msg}; using {reason} fallback");
                // fallback_distilled already preserves the full transcript
                // (it sets `transcript = Some(snippet)` when non-empty); no
                // post-fallback re-set needed.
                return Ok(fallback_distilled(
                    ID,
                    reason,
                    inputs.transcript,
                    None,
                    &self.config.model,
                ));
            }
        };

        let yaml_body = strip_fences(&raw);
        let parsed: PatternYaml = match serde_yaml::from_str(yaml_body) {
            Ok(p) => p,
            Err(err) => {
                log::warn!("ImageDistiller: yaml parse failed: {err}; using fallback");
                return Ok(fallback_distilled(
                    ID,
                    "yaml-parse-error",
                    inputs.transcript,
                    Some(yaml_body),
                    &self.config.model,
                ));
            }
        };

        let summary = parsed.summary.unwrap_or_default().trim().to_string();
        if summary.is_empty() {
            log::warn!("ImageDistiller: empty summary; using missing-summary fallback");
            return Ok(fallback_distilled(
                ID,
                "missing-summary",
                inputs.transcript,
                Some(yaml_body),
                &self.config.model,
            ));
        }

        let claims: Vec<Claim> = parsed
            .claims
            .unwrap_or_default()
            .into_iter()
            .map(|c| c.into_claim())
            .filter(|c| !c.text.is_empty())
            .collect();
        let tags: Vec<String> = parsed
            .tags
            .unwrap_or_default()
            .into_iter()
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .collect();
        let links: Vec<Link> = parsed
            .links
            .unwrap_or_default()
            .into_iter()
            .map(|l| Link {
                url: l.url.trim().to_string(),
                label: l.label.filter(|s| !s.is_empty()),
            })
            .filter(|l| !l.url.is_empty())
            .collect();

        let word_count = inputs.transcript.split_whitespace().count();
        if claims.is_empty() && word_count > 200 {
            log::warn!("ImageDistiller: empty claims for transcript with {word_count} words (possible pattern drift)");
        }

        let input_tokens = approx_tokens(inputs.transcript.len()) as u32;
        let output_tokens = approx_tokens(raw.len()) as u32;

        let distilled = Distilled {
            summary,
            claims,
            tags,
            links,
            kind_specific: None,
            meta: DistilledMeta {
                extractor: ID.to_string(),
                model: if self.config.model.is_empty() {
                    "default".to_string()
                } else {
                    self.config.model.clone()
                },
                input_tokens,
                output_tokens,
                produced_at: Utc::now().to_rfc3339(),
                validation: ValidationMeta::default(),
            },
            // Non-URL kind: published note is the only persistent source, so
            // preserve the raw Vision+OCR concat verbatim. Stays uncapped at
            // the distiller level; the global summary cap only clips summary.
            transcript: Some(inputs.transcript.to_string()),
        };

        // Single-call kind: chunk_count = 1, so the cap stays 10.
        let mut bounded = enforce_bounds(distilled, max_claims(1));
        debug_assert!(bounded.summary.chars().count() <= MAX_SUMMARY_CHARS);
        bounded.tags.iter_mut().for_each(|t| *t = t.to_lowercase());
        Ok(bounded)
    }
}

#[cfg(test)]
mod tests;
