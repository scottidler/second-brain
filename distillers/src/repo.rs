//! GitHub repository distiller.
//!
//! Calls Fabric with `distill-repo.md`, parses the YAML body into a
//! `Distilled`, and attaches the structured `KindPayload::Repo` payload from
//! the Stage-0 `RepoMetadata` the caller provides. The metadata fields
//! (stars, primary language, last commit, topics) come from the GitHub REST
//! API at fetch time; the LLM only contributes `summary`, `claims`, and the
//! `install` hint.

use crate::parse::{PatternClaim, PatternLink, approx_tokens, strip_fences};
use async_trait::async_trait;
use chrono::Utc;
use eyre::Result;
use serde::{Deserialize, Serialize};
use vault::distilled::{Claim, Distilled, DistilledMeta, KindPayload, Link, RepoPayload, ValidationMeta};

use crate::{
    DistillExtractor, DistillInputs, FabricCaller, FabricRequest, enforce_bounds, fallback_distilled,
    validate::MAX_SUMMARY_CHARS,
};

const ID: &str = "distill-repo-v1";
const PATTERN: &str = "distill-repo";
/// Cap from the design doc: `install` survives into frontmatter only if it
/// fits under 500 chars; longer install strings are dropped (not truncated)
/// so the distilled artifact does not lie about completeness.
const MAX_INSTALL_CHARS: usize = 500;

/// Stage-0 metadata frozen at ingest. Mirrors `borg::github::RepoMetadata`
/// in field names; the duplication keeps distillers free of HTTP/JSON deps.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepoMetadata {
    pub owner: String,
    pub repo: String,
    pub stars: Option<u32>,
    pub primary_language: Option<String>,
    /// ISO 8601 UTC, e.g. "2026-05-16T14:03:22Z".
    pub last_commit: Option<String>,
    pub topics: Vec<String>,
}

/// Tunables for the repo distiller. Same shape as `ArticleConfig`.
#[derive(Debug, Clone)]
pub struct RepoConfig {
    pub model: String,
    pub max_chars: usize,
    pub timeout_secs: u64,
}

impl Default for RepoConfig {
    fn default() -> Self {
        Self {
            model: String::new(),
            max_chars: 32_000,
            timeout_secs: 60,
        }
    }
}

#[derive(Debug, Clone)]
pub struct RepoDistiller<F: FabricCaller + Clone> {
    fabric: F,
    config: RepoConfig,
}

impl<F: FabricCaller + Clone> RepoDistiller<F> {
    pub fn new(fabric: F, config: RepoConfig) -> Self {
        Self { fabric, config }
    }
}

#[async_trait]
impl<F: FabricCaller + Clone> DistillExtractor for RepoDistiller<F> {
    fn id(&self) -> &'static str {
        ID
    }

    async fn distill(&self, inputs: DistillInputs<'_>) -> Result<Distilled> {
        log::debug!(
            "RepoDistiller::distill: transcript_len={} source_url={:?} has_metadata={}",
            inputs.transcript.len(),
            inputs.source_url,
            inputs.repo_metadata.is_some()
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
                log::warn!("RepoDistiller: fabric call failed: {msg}; using {reason} fallback");
                return Ok(attach_metadata(
                    fallback_distilled(ID, reason, inputs.transcript, None),
                    inputs.repo_metadata,
                    None,
                ));
            }
        };

        let yaml_body = strip_fences(&raw);
        let parsed: PatternYaml = match serde_yaml::from_str(yaml_body) {
            Ok(p) => p,
            Err(err) => {
                log::warn!("RepoDistiller: yaml parse failed: {err}; using fallback");
                return Ok(attach_metadata(
                    fallback_distilled(ID, "yaml-parse-error", inputs.transcript, Some(yaml_body)),
                    inputs.repo_metadata,
                    None,
                ));
            }
        };

        let summary = parsed.summary.unwrap_or_default().trim().to_string();
        if summary.is_empty() {
            log::warn!("RepoDistiller: empty summary; using missing-summary fallback");
            return Ok(attach_metadata(
                fallback_distilled(ID, "missing-summary", inputs.transcript, Some(yaml_body)),
                inputs.repo_metadata,
                None,
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

        let install = parsed
            .install
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .filter(|s| s.chars().count() <= MAX_INSTALL_CHARS);

        let word_count = inputs.transcript.split_whitespace().count();
        if claims.is_empty() && word_count > 500 {
            log::warn!("RepoDistiller: empty claims for transcript with {word_count} words (possible pattern drift)");
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
            // URL kind: origin URL is the recoverable archive.
            transcript: None,
        };

        let mut bounded = enforce_bounds(distilled);
        debug_assert!(bounded.summary.chars().count() <= MAX_SUMMARY_CHARS);
        bounded.tags.iter_mut().for_each(|t| *t = t.to_lowercase());
        Ok(attach_metadata(bounded, inputs.repo_metadata, install))
    }
}

/// Build the `KindPayload::Repo` from Stage-0 metadata plus the pattern's
/// extracted `install` hint. When metadata is absent (cortex backfill) the
/// payload still attaches if there's any install string the LLM extracted.
fn attach_metadata(mut distilled: Distilled, metadata: Option<&RepoMetadata>, install: Option<String>) -> Distilled {
    let payload = match (metadata, install) {
        (None, None) => return distilled,
        (None, Some(install)) => RepoPayload {
            install: Some(install),
            ..Default::default()
        },
        (Some(m), install) => RepoPayload {
            stars: m.stars,
            primary_language: m.primary_language.clone(),
            last_commit: m.last_commit.clone(),
            topics: m.topics.clone(),
            install,
        },
    };
    distilled.kind_specific = Some(KindPayload::Repo(payload));
    distilled
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
    #[serde(default)]
    install: Option<String>,
}
#[cfg(test)]
mod tests;
