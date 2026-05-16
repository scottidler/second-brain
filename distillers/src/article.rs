//! Article distiller.
//!
//! Calls Fabric with `distill-article.md`, parses the YAML body into a
//! `Distilled`, enforces bounds, and falls back on timeout / non-zero exit /
//! parse failure. The pattern is the prompt; the parser is here.

use async_trait::async_trait;
use chrono::Utc;
use eyre::Result;
use serde::{Deserialize, Serialize};
use vault::distilled::{Claim, Distilled, DistilledMeta, Link, ValidationMeta};

use crate::{
    DistillExtractor, DistillInputs, FabricCaller, FabricRequest, enforce_bounds, fallback_distilled,
    validate::MAX_SUMMARY_CHARS,
};

const ID: &str = "distill-article-v1";
const PATTERN: &str = "distill-article";

/// Tunables for the article distiller. Mirrors the relevant subset of
/// borg's FabricConfig so the distiller stays decoupled from borg.
#[derive(Debug, Clone)]
pub struct ArticleConfig {
    pub model: String,
    pub max_chars: usize,
    pub timeout_secs: u64,
}

impl Default for ArticleConfig {
    fn default() -> Self {
        Self {
            model: String::new(),
            max_chars: 32_000,
            timeout_secs: 60,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ArticleDistiller<F: FabricCaller + Clone> {
    fabric: F,
    config: ArticleConfig,
}

impl<F: FabricCaller + Clone> ArticleDistiller<F> {
    pub fn new(fabric: F, config: ArticleConfig) -> Self {
        Self { fabric, config }
    }
}

#[async_trait]
impl<F: FabricCaller + Clone> DistillExtractor for ArticleDistiller<F> {
    fn id(&self) -> &'static str {
        ID
    }

    async fn distill(&self, inputs: DistillInputs<'_>) -> Result<Distilled> {
        log::debug!(
            "ArticleDistiller::distill: transcript_len={} source_url={:?}",
            inputs.transcript.len(),
            inputs.source_url
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
                let reason = if msg.contains("timed out") { "fabric-timeout" } else { "fabric-error" };
                log::warn!("ArticleDistiller: fabric call failed: {msg}; using {reason} fallback");
                return Ok(fallback_distilled(ID, reason, inputs.transcript, None));
            }
        };

        let yaml_body = strip_fences(&raw);
        let parsed: PatternYaml = match serde_yaml::from_str(yaml_body) {
            Ok(p) => p,
            Err(err) => {
                log::warn!("ArticleDistiller: yaml parse failed: {err}; using fallback");
                return Ok(fallback_distilled(
                    ID,
                    "yaml-parse-error",
                    inputs.transcript,
                    Some(yaml_body),
                ));
            }
        };

        let summary = parsed.summary.unwrap_or_default().trim().to_string();
        if summary.is_empty() {
            log::warn!("ArticleDistiller: empty summary; using missing-summary fallback");
            return Ok(fallback_distilled(
                ID,
                "missing-summary",
                inputs.transcript,
                Some(yaml_body),
            ));
        }

        let claims: Vec<Claim> = parsed
            .claims
            .unwrap_or_default()
            .into_iter()
            .map(|c| Claim {
                text: c.text.trim().to_string(),
                anchor: c.anchor.filter(|s| !s.is_empty()),
            })
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

        // Empty-claims canary: a transcript over ~500 words should produce
        // at least one claim. Log a warning but don't reject; pattern drift
        // is operational signal, not a publication blocker.
        let word_count = inputs.transcript.split_whitespace().count();
        if claims.is_empty() && word_count > 500 {
            log::warn!(
                "ArticleDistiller: empty claims for transcript with {word_count} words (possible pattern drift)"
            );
        }

        // Token counts for `meta`. Fabric's output doesn't surface these
        // directly; we report char-based approximations rather than lie.
        let input_tokens = approx_tokens(inputs.transcript.len());
        let output_tokens = approx_tokens(raw.len());

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
        };

        let mut bounded = enforce_bounds(distilled);
        // After bounds enforcement the summary may have lost its trailing
        // punctuation if the original was huge. Validation already records
        // the truncation tag; nothing more to do here. Keep the post-bounds
        // summary length cap explicit by re-checking the documented limit.
        debug_assert!(bounded.summary.chars().count() <= MAX_SUMMARY_CHARS);
        // We do NOT canonicalise tags here; the canonical tag filter lives
        // in borg's `hygiene::sanitize_tag` and is applied at the publish
        // step (alongside autotag pipeline output). Distillers emit raw tags.
        bounded.tags.iter_mut().for_each(|t| *t = t.to_lowercase());
        Ok(bounded)
    }
}

#[derive(Debug, Deserialize, Serialize)]
struct PatternYaml {
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    claims: Option<Vec<PatternClaim>>,
    #[serde(default)]
    tags: Option<Vec<String>>,
    #[serde(default)]
    links: Option<Vec<PatternLink>>,
}

#[derive(Debug, Deserialize, Serialize)]
struct PatternClaim {
    text: String,
    #[serde(default)]
    anchor: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
struct PatternLink {
    url: String,
    #[serde(default)]
    label: Option<String>,
}

/// Strip a leading ```yaml ... ``` (or bare ``` ... ```) fence if the LLM
/// added one despite the prompt asking it not to. We don't try to repair
/// otherwise-malformed YAML.
fn strip_fences(raw: &str) -> &str {
    let trimmed = raw.trim();
    let without_open = trimmed
        .strip_prefix("```yaml")
        .or_else(|| trimmed.strip_prefix("```"))
        .unwrap_or(trimmed);
    let stripped = without_open.trim_start_matches('\n');
    if let Some(close) = stripped.rfind("```") {
        stripped[..close].trim_end()
    } else {
        stripped
    }
}

/// Rough character-to-token approximation. ~4 chars per token is a common
/// rule of thumb for English prose; good enough for budget reporting.
fn approx_tokens(chars: usize) -> u32 {
    (chars / 4).min(u32::MAX as usize) as u32
}

#[cfg(test)]
mod tests;
