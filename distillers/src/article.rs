//! Article distiller.
//!
//! Short inputs go straight to `distill-article` (single Fabric call). Long
//! inputs (above the shared 12K-token threshold) split at sentence boundaries
//! into ~8K-token chunks via the shared `chunk_transcript`/`find_boundary`;
//! each chunk runs through `distill-article-chunk` in parallel, and a single
//! `distill-article-reduce` call synthesizes the final summary AND SELECTS the
//! final claims from the pooled chunk claims (Phase 5's reduce mechanics,
//! reused). Articles carry no timestamp anchors, so the claim pool is
//! anchorless and the anchor-honesty rule degrades to "accept every selected
//! claim as a synthesis" (no invention gate to trip). The pattern is the
//! prompt; the parser is here.

use crate::parse::{
    PatternYaml, ReduceYaml, approx_tokens, build_reduce_input, input_truncation_tag, select_reduce_claims,
    strip_fences,
};
use crate::video::{CHUNK_TOKEN_TARGET, SINGLE_CALL_TOKEN_THRESHOLD, chunk_transcript};
use async_trait::async_trait;
use chrono::Utc;
use eyre::Result;
use futures::stream::{self, StreamExt};
use vault::distilled::{Claim, Distilled, DistilledMeta, Link, ValidationMeta};

use crate::{
    DistillExtractor, DistillInputs, FabricCaller, FabricRequest, enforce_bounds, fallback_distilled, max_claims,
    validate::MAX_SUMMARY_CHARS,
};

const ID: &str = "distill-article-v1";
const PATTERN: &str = "distill-article";
const PATTERN_CHUNK: &str = "distill-article-chunk";
const PATTERN_REDUCE: &str = "distill-article-reduce";
/// Default parallelism for chunk distillation. The chunk path is I/O bound
/// (fabric subprocess); this mirrors the video distiller's default. Article and
/// thread do not expose it as a config field because no borg/cortex config maps
/// to it (video's own field is always the default in production).
const DEFAULT_CHUNK_CONCURRENCY: usize = 4;

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
        let transcript = inputs.transcript;
        let token_estimate = approx_tokens(transcript.len());
        log::debug!(
            "ArticleDistiller::distill: transcript_len={} approx_tokens={} source_url={:?}",
            transcript.len(),
            token_estimate,
            inputs.source_url
        );

        // Above the shared 12K-token threshold we chunk → map → reduce so the
        // whole article is covered instead of silently truncated at max_chars.
        // The single-call path is chunk_count = 1 (cap 10); the long path scales
        // the claim budget by the real chunk count.
        let (mut distilled, chunk_count) = if token_estimate <= SINGLE_CALL_TOKEN_THRESHOLD {
            (self.distill_short(transcript, inputs.capture_note).await?, 1usize)
        } else {
            let chunks = chunk_transcript(transcript, CHUNK_TOKEN_TARGET);
            let chunk_count = chunks.len().max(1);
            (self.distill_long(transcript, chunks).await?, chunk_count)
        };

        // Loud sub-threshold truncation (single-call path only): if the input
        // exceeds max_chars, `vault::fabric::truncate_input` silently cut its
        // tail. Surface it as a distinct `bounds_truncations` entry AND a WARN
        // carrying the source in scope (the trace id lives at the borg pipeline
        // layer, not here). The long path chunks below max_chars, so it never
        // truncates.
        if chunk_count == 1
            && let Some(tag) = input_truncation_tag(transcript.chars().count(), self.config.max_chars)
        {
            log::warn!(
                "ArticleDistiller: sub-threshold input truncated ({tag}) source_url={:?}",
                inputs.source_url
            );
            distilled.meta.validation.bounds_truncations.push(tag);
        }

        let mut bounded = enforce_bounds(distilled, max_claims(chunk_count));
        debug_assert!(bounded.summary.chars().count() <= MAX_SUMMARY_CHARS);
        // We do NOT canonicalise tags here; the canonical tag filter lives
        // in borg's `hygiene::sanitize_tag` and is applied at the publish
        // step (alongside autotag pipeline output). Distillers emit raw tags.
        bounded.tags.iter_mut().for_each(|t| *t = t.to_lowercase());
        Ok(bounded)
    }
}

impl<F: FabricCaller + Clone> ArticleDistiller<F> {
    /// Single-call path for inputs under the threshold. Returns the parsed (or
    /// fallback) `Distilled`; the outer `distill` applies bounds + tag
    /// lowercasing so both paths share one exit.
    async fn distill_short(&self, transcript: &str, capture_note: Option<&str>) -> Result<Distilled> {
        let request = FabricRequest {
            pattern: PATTERN.to_string(),
            input: crate::parse::compose_capture_input(transcript, capture_note),
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
                log::warn!("ArticleDistiller: fabric call failed: {msg}; using {reason} fallback");
                return Ok(fallback_distilled(ID, reason, transcript, None, &self.config.model));
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
                    transcript,
                    Some(yaml_body),
                    &self.config.model,
                ));
            }
        };

        let summary = parsed.summary.unwrap_or_default().trim().to_string();
        if summary.is_empty() {
            log::warn!("ArticleDistiller: empty summary; using missing-summary fallback");
            return Ok(fallback_distilled(
                ID,
                "missing-summary",
                transcript,
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

        // Empty-claims canary: a transcript over ~500 words should produce
        // at least one claim. Log a warning but don't reject; pattern drift
        // is operational signal, not a publication blocker.
        let word_count = transcript.split_whitespace().count();
        if claims.is_empty() && word_count > 500 {
            log::warn!(
                "ArticleDistiller: empty claims for transcript with {word_count} words (possible pattern drift)"
            );
        }

        // Token counts for `meta`. Fabric's output doesn't surface these
        // directly; we report char-based approximations rather than lie.
        let input_tokens = approx_tokens(transcript.len()) as u32;
        let output_tokens = approx_tokens(raw.len()) as u32;

        Ok(Distilled {
            summary,
            tldr: None,
            enumeration: None,
            key_ideas: Vec::new(),
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
            // Phase 7: articles are the lossiest kind past the 60-day staging
            // retention (an 8K-word essay collapsing to a ≤2000-char summary +
            // a URL that may rot). Keep the fetched markdown verbatim in-note
            // under `## Transcript`, mirroring video/voicenote/thread; this
            // restores FTS + (with the `transcript_eligible()` amendment)
            // embedding reach for the whole article class.
            transcript: article_transcript(transcript),
        })
    }

    /// Map-reduce path for long inputs. Chunks are distilled in parallel
    /// (bounded by `DEFAULT_CHUNK_CONCURRENCY`); chunk claims are pooled and a
    /// single `distill-article-reduce` call synthesizes the summary and SELECTS
    /// the final claims from the pool (spanning the whole article, not its
    /// head). Articles carry no anchors, so `select_reduce_claims` accepts every
    /// selected claim as an anchorless synthesis. Selection failure / empty
    /// selection reverts to the chronological chunk-claim merge and records the
    /// distinct `reduce-selection-failed` reason, mirroring the video path.
    async fn distill_long(&self, transcript: &str, chunks: Vec<String>) -> Result<Distilled> {
        log::debug!(
            "ArticleDistiller::distill_long: chunks={} threshold_tokens={} target_tokens={}",
            chunks.len(),
            SINGLE_CALL_TOKEN_THRESHOLD,
            CHUNK_TOKEN_TARGET
        );
        if chunks.is_empty() {
            return Ok(fallback_distilled(
                ID,
                "empty-transcript",
                transcript,
                None,
                &self.config.model,
            ));
        }

        let concurrency = DEFAULT_CHUNK_CONCURRENCY.max(1);
        let chunk_results: Vec<(usize, Result<String>)> = stream::iter(chunks.iter().cloned().enumerate())
            .map(|(idx, chunk)| {
                let fabric = self.fabric.clone();
                let model = self.config.model.clone();
                let max_chars = self.config.max_chars;
                let timeout_secs = self.config.timeout_secs;
                async move {
                    let request = FabricRequest {
                        pattern: PATTERN_CHUNK.to_string(),
                        input: chunk,
                        model,
                        max_chars,
                        timeout_secs,
                    };
                    let result = fabric.call(request).await;
                    (idx, result)
                }
            })
            .buffer_unordered(concurrency)
            .collect()
            .await;
        let mut chunk_results = chunk_results;
        chunk_results.sort_by_key(|(idx, _)| *idx);

        let mut chunk_summaries: Vec<String> = Vec::with_capacity(chunk_results.len());
        let mut combined_claims: Vec<Claim> = Vec::new();
        let mut combined_links: Vec<Link> = Vec::new();
        let mut combined_tags: Vec<String> = Vec::new();
        let mut any_chunk_failed = false;
        let mut output_chars: usize = 0;

        for (_, result) in chunk_results {
            let raw = match result {
                Ok(r) => r,
                Err(err) => {
                    log::warn!("ArticleDistiller: chunk fabric call failed: {err}");
                    any_chunk_failed = true;
                    continue;
                }
            };
            output_chars += raw.len();
            let parsed = match parse_article_yaml(&raw) {
                Ok(p) => p,
                Err(err) => {
                    log::warn!("ArticleDistiller: chunk yaml parse failed: {err}");
                    any_chunk_failed = true;
                    continue;
                }
            };
            if let Some(s) = parsed.summary.clone()
                && !s.trim().is_empty()
            {
                chunk_summaries.push(s.trim().to_string());
            }
            combined_claims.extend(parsed.claims.unwrap_or_default().into_iter().filter_map(|c| {
                let claim = c.into_claim();
                (!claim.text.is_empty()).then_some(claim)
            }));
            combined_links.extend(parsed.links.unwrap_or_default().into_iter().filter_map(|l| {
                let url = l.url.trim().to_string();
                if url.is_empty() {
                    return None;
                }
                Some(Link {
                    url,
                    label: l.label.filter(|s| !s.is_empty()),
                })
            }));
            combined_tags.extend(
                parsed
                    .tags
                    .unwrap_or_default()
                    .into_iter()
                    .map(|t| t.trim().to_string())
                    .filter(|t| !t.is_empty()),
            );
        }

        // Dedup tags, first-seen order.
        let mut seen_tags = std::collections::HashSet::new();
        combined_tags.retain(|t| seen_tags.insert(t.clone()));

        if chunk_summaries.is_empty() {
            log::warn!("ArticleDistiller: all chunks failed; using chunk-failures fallback");
            return Ok(fallback_distilled(
                ID,
                "chunk-failures",
                transcript,
                None,
                &self.config.model,
            ));
        }

        let joined = chunk_summaries.join("\n\n");
        let reduce_input = build_reduce_input(&chunk_summaries, &combined_claims);
        let mut anchors_stripped: u32 = 0;
        let (summary, claims, reduce_selection_failed) = match self.call_fabric(PATTERN_REDUCE, &reduce_input).await {
            Ok(raw) => match parse_reduce_yaml(&raw) {
                Ok(parsed) => {
                    let summary = parsed
                        .summary
                        .as_ref()
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| joined.clone());
                    match parsed
                        .claims
                        .and_then(|c| select_reduce_claims(c, &combined_claims, &mut anchors_stripped))
                    {
                        Some(selected) => (summary, selected, false),
                        None => {
                            log::warn!(
                                "ArticleDistiller: reduce selected no claims; falling back to chronological merge"
                            );
                            (summary, combined_claims.clone(), true)
                        }
                    }
                }
                Err(err) => {
                    log::warn!(
                        "ArticleDistiller: reduce yaml parse failed: {err}; falling back to concat + chronological claims"
                    );
                    (joined.clone(), combined_claims.clone(), true)
                }
            },
            Err((reason, _)) => {
                log::warn!(
                    "ArticleDistiller: reduce fabric call failed ({reason}); falling back to concatenated chunks + chronological claims"
                );
                (joined.clone(), combined_claims.clone(), true)
            }
        };

        let mut validation = ValidationMeta::default();
        // reduce-selection-failed takes precedence over partial-chunk-failure:
        // reintroduced head-bias is the signal this phase exists to surface.
        if reduce_selection_failed {
            validation.fallback_reason = Some("reduce-selection-failed".to_string());
        } else if any_chunk_failed {
            validation.fallback_reason = Some("partial-chunk-failure".to_string());
        }
        validation.anchors_stripped = anchors_stripped;
        let input_tokens = approx_tokens(transcript.len()) as u32;
        let output_tokens = approx_tokens(output_chars) as u32;
        Ok(Distilled {
            summary,
            tldr: None,
            enumeration: None,
            key_ideas: Vec::new(),
            claims,
            tags: combined_tags,
            links: combined_links,
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
                validation,
            },
            // Phase 7: long articles keep their full fetched markdown in-note
            // too (both paths share this behavior), so a chunked essay is just
            // as durable as a short one past staging retention.
            transcript: article_transcript(transcript),
        })
    }

    /// Wrapper around `FabricCaller::call` that maps timeout vs error strings
    /// into stable fallback reasons (mirrors the video distiller).
    async fn call_fabric(&self, pattern: &str, input: &str) -> std::result::Result<String, (String, String)> {
        let request = FabricRequest {
            pattern: pattern.to_string(),
            input: input.to_string(),
            model: self.config.model.clone(),
            max_chars: self.config.max_chars,
            timeout_secs: self.config.timeout_secs,
        };
        match self.fabric.call(request).await {
            Ok(text) => Ok(text),
            Err(err) => {
                let msg = format!("{err}");
                let reason = if vault::fabric::FabricError::is_timeout(&err) {
                    "fabric-timeout".to_string()
                } else {
                    "fabric-error".to_string()
                };
                log::warn!("ArticleDistiller::call_fabric: pattern={pattern} reason={reason} err={msg}");
                Err((reason, msg))
            }
        }
    }
}

/// The verbatim fetched article markdown to persist in-note under
/// `## Transcript` (Phase 7). Mirrors `thread::thread_transcript`: `None` for
/// an empty/whitespace-only input so the renderer emits no empty section;
/// otherwise the full fetched markdown. `render::push_transcript` demotes any
/// embedded headings so nav junk / H1s in the fetched markdown stay
/// subordinate to the note's section structure.
fn article_transcript(transcript: &str) -> Option<String> {
    if transcript.trim().is_empty() { None } else { Some(transcript.to_string()) }
}

fn parse_article_yaml(raw: &str) -> Result<PatternYaml> {
    let yaml_body = strip_fences(raw);
    let parsed: PatternYaml = serde_yaml::from_str(yaml_body)?;
    Ok(parsed)
}

fn parse_reduce_yaml(raw: &str) -> Result<ReduceYaml> {
    let yaml_body = strip_fences(raw);
    let parsed: ReduceYaml = serde_yaml::from_str(yaml_body)?;
    Ok(parsed)
}

#[cfg(test)]
mod tests;
