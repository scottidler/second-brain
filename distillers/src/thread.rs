//! Thread distiller (X / Reddit / Hacker News).
//!
//! Calls Fabric with `distill-thread.md`, parses the YAML body into a
//! `Distilled`, and attaches `KindPayload::Thread` from a combination of
//! LLM-extracted fields (`author`, `post_count`) and a `platform` string
//! inferred from `inputs.source_url`. Stage 0 for threads is the same
//! generic article-fetcher chain (Jina / fabric -u / browser-UA +
//! markitdown) - no dedicated JSON fetcher yet; the rendered markdown
//! is sufficient input for this distiller in shadow mode.

use async_trait::async_trait;
use chrono::Utc;
use eyre::Result;
use serde::{Deserialize, Serialize};
use vault::distilled::{Claim, Distilled, DistilledMeta, KindPayload, Link, ThreadPayload, ValidationMeta};

use crate::{
    DistillExtractor, DistillInputs, FabricCaller, FabricRequest, enforce_bounds, fallback_distilled,
    validate::MAX_SUMMARY_CHARS,
};

const ID: &str = "distill-thread-v1";
const PATTERN: &str = "distill-thread";

/// Tunables for the thread distiller. Same shape as `ArticleConfig`.
#[derive(Debug, Clone)]
pub struct ThreadConfig {
    pub model: String,
    pub max_chars: usize,
    pub timeout_secs: u64,
}

impl Default for ThreadConfig {
    fn default() -> Self {
        Self {
            model: String::new(),
            max_chars: 32_000,
            timeout_secs: 60,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ThreadDistiller<F: FabricCaller + Clone> {
    fabric: F,
    config: ThreadConfig,
}

impl<F: FabricCaller + Clone> ThreadDistiller<F> {
    pub fn new(fabric: F, config: ThreadConfig) -> Self {
        Self { fabric, config }
    }
}

#[async_trait]
impl<F: FabricCaller + Clone> DistillExtractor for ThreadDistiller<F> {
    fn id(&self) -> &'static str {
        ID
    }

    async fn distill(&self, inputs: DistillInputs<'_>) -> Result<Distilled> {
        log::debug!(
            "ThreadDistiller::distill: transcript_len={} source_url={:?}",
            inputs.transcript.len(),
            inputs.source_url
        );

        let platform = infer_platform(inputs.source_url);

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
                log::warn!("ThreadDistiller: fabric call failed: {msg}; using {reason} fallback");
                return Ok(attach_platform(
                    fallback_distilled(ID, reason, inputs.transcript, None),
                    platform,
                    None,
                    0,
                ));
            }
        };

        let yaml_body = strip_fences(&raw);
        let parsed: PatternYaml = match serde_yaml::from_str(yaml_body) {
            Ok(p) => p,
            Err(err) => {
                log::warn!("ThreadDistiller: yaml parse failed: {err}; using fallback");
                return Ok(attach_platform(
                    fallback_distilled(ID, "yaml-parse-error", inputs.transcript, Some(yaml_body)),
                    platform,
                    None,
                    0,
                ));
            }
        };

        let summary = parsed.summary.unwrap_or_default().trim().to_string();
        if summary.is_empty() {
            log::warn!("ThreadDistiller: empty summary; using missing-summary fallback");
            return Ok(attach_platform(
                fallback_distilled(ID, "missing-summary", inputs.transcript, Some(yaml_body)),
                platform,
                None,
                0,
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

        let author = parsed.author.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        let post_count = parsed.post_count.unwrap_or(0);

        let word_count = inputs.transcript.split_whitespace().count();
        if claims.is_empty() && word_count > 200 {
            log::warn!("ThreadDistiller: empty claims for transcript with {word_count} words (possible pattern drift)");
        }

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
            // Phase B2: preserve the concatenated post bodies so cortex
            // can produce chunked transcript embeddings. A long X or
            // Reddit thread distilled to a 4-sentence summary cannot
            // represent a token that appears in a single mid-thread
            // post; the transcript chunks make that token reachable
            // via semantic query.
            transcript: if inputs.transcript.trim().is_empty() {
                None
            } else {
                Some(inputs.transcript.to_string())
            },
        };

        let mut bounded = enforce_bounds(distilled);
        debug_assert!(bounded.summary.chars().count() <= MAX_SUMMARY_CHARS);
        bounded.tags.iter_mut().for_each(|t| *t = t.to_lowercase());
        Ok(attach_platform(bounded, platform, author, post_count))
    }
}

/// Build the `KindPayload::Thread` from the inferred platform plus the
/// LLM-extracted author and post count. Attaches even when `platform` is
/// `"unknown"` so the payload's presence is itself a Phase 6 signal.
fn attach_platform(mut distilled: Distilled, platform: String, author: Option<String>, post_count: u32) -> Distilled {
    distilled.kind_specific = Some(KindPayload::Thread(ThreadPayload {
        author,
        post_count,
        platform,
    }));
    distilled
}

/// Map a thread URL's host to a short platform identifier. Matches the host
/// list in `borg::stages::raw::classify_url`. Returns `"unknown"` (rather
/// than `None`) so the payload always carries a value - cortex backfill on
/// a thread note without a clear platform still gets a well-formed payload.
pub fn infer_platform(source_url: Option<&str>) -> String {
    let Some(url) = source_url else {
        return "unknown".to_string();
    };
    let host = url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(|h| h.to_ascii_lowercase()))
        .unwrap_or_default();
    if host == "x.com" || host.ends_with(".x.com") || host == "twitter.com" || host.ends_with(".twitter.com") {
        "x".to_string()
    } else if host.ends_with("reddit.com") {
        "reddit".to_string()
    } else if host.ends_with("news.ycombinator.com") {
        "hn".to_string()
    } else {
        "unknown".to_string()
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
    #[serde(default)]
    author: Option<String>,
    #[serde(default, rename = "post-count")]
    post_count: Option<u32>,
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

fn approx_tokens(chars: usize) -> u32 {
    (chars / 4) as u32
}

#[cfg(test)]
mod tests;
