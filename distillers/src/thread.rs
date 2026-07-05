//! Thread distiller (X / Reddit / Hacker News).
//!
//! Short inputs go straight to `distill-thread` (single Fabric call). Long
//! threads (above the shared 12K-token threshold) chunk → map → reduce via the
//! shared `chunk_transcript`/`find_boundary`: each chunk runs through
//! `distill-thread-chunk` (attribution-aware, anchorless), and a single
//! `distill-thread-reduce` call synthesizes the summary, SELECTS the final
//! claims from the pool (Phase 5 mechanics), AND re-emits `author`/`post-count`.
//! Those two fields live at the TOP of the rendered thread, so the reduce input
//! prepends a verbatim `## Thread Head` section — the mechanism that keeps
//! `KindPayload::Thread` fields alive through the long path (the single-call
//! parse that used to extract them no longer runs for long threads).
//!
//! `KindPayload::Thread` is attached from a combination of the LLM-extracted
//! `author`/`post_count` and a `platform` string inferred from
//! `inputs.source_url`. Stage 0 for threads is the same generic
//! article-fetcher chain (Jina / fabric -u / browser-UA + markitdown) - no
//! dedicated JSON fetcher yet; the rendered markdown is sufficient input.

use crate::parse::{
    PatternClaim, PatternLink, PatternYaml as ChunkYaml, approx_tokens, build_thread_reduce_input,
    input_truncation_tag, select_reduce_claims, strip_fences,
};
use crate::video::{CHUNK_TOKEN_TARGET, SINGLE_CALL_TOKEN_THRESHOLD, chunk_transcript};
use async_trait::async_trait;
use chrono::Utc;
use eyre::Result;
use futures::stream::{self, StreamExt};
use serde::{Deserialize, Serialize};
use vault::distilled::{Claim, Distilled, DistilledMeta, KindPayload, Link, ThreadPayload, ValidationMeta};

use crate::{
    DistillExtractor, DistillInputs, FabricCaller, FabricRequest, enforce_bounds, fallback_distilled, max_claims,
    validate::MAX_SUMMARY_CHARS,
};

const ID: &str = "distill-thread-v1";
const PATTERN: &str = "distill-thread";
const PATTERN_CHUNK: &str = "distill-thread-chunk";
const PATTERN_REDUCE: &str = "distill-thread-reduce";
/// Default parallelism for chunk distillation (I/O bound; mirrors video).
const DEFAULT_CHUNK_CONCURRENCY: usize = 4;
/// How much of the transcript head to carry verbatim into the thread reduce
/// input. The author handle and the first posts sit at the very top of a
/// rendered thread; this bound keeps the reduce input sane while giving the
/// reduce pattern enough context to re-emit `author`/`post-count`.
const THREAD_HEAD_CHARS: usize = 8_000;

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

/// The distilled body plus the two thread-only fields the reduce/single-call
/// step extracts. Kept together so both paths hand the same tuple to the outer
/// `distill`, which attaches `KindPayload::Thread`.
type ThreadDistilled = (Distilled, Option<String>, u32);

#[async_trait]
impl<F: FabricCaller + Clone> DistillExtractor for ThreadDistiller<F> {
    fn id(&self) -> &'static str {
        ID
    }

    async fn distill(&self, inputs: DistillInputs<'_>) -> Result<Distilled> {
        let transcript = inputs.transcript;
        let token_estimate = approx_tokens(transcript.len());
        let platform = infer_platform(inputs.source_url);
        log::debug!(
            "ThreadDistiller::distill: transcript_len={} approx_tokens={} source_url={:?} platform={platform}",
            transcript.len(),
            token_estimate,
            inputs.source_url,
        );

        let (mut distilled, author, post_count, chunk_count) = if token_estimate <= SINGLE_CALL_TOKEN_THRESHOLD {
            let (d, author, post_count) = self.distill_short(transcript, inputs.capture_note).await?;
            (d, author, post_count, 1usize)
        } else {
            let chunks = chunk_transcript(transcript, CHUNK_TOKEN_TARGET);
            let chunk_count = chunks.len().max(1);
            let (d, author, post_count) = self.distill_long(transcript, chunks).await?;
            (d, author, post_count, chunk_count)
        };

        // Loud sub-threshold truncation (single-call path only): see the
        // article distiller for the rationale. The long path chunks below
        // max_chars, so it never truncates.
        if chunk_count == 1
            && let Some(tag) = input_truncation_tag(transcript.chars().count(), self.config.max_chars)
        {
            log::warn!(
                "ThreadDistiller: sub-threshold input truncated ({tag}) source_url={:?}",
                inputs.source_url
            );
            distilled.meta.validation.bounds_truncations.push(tag);
        }

        let mut bounded = enforce_bounds(distilled, max_claims(chunk_count));
        debug_assert!(bounded.summary.chars().count() <= MAX_SUMMARY_CHARS);
        bounded.tags.iter_mut().for_each(|t| *t = t.to_lowercase());
        Ok(attach_platform(bounded, platform, author, post_count))
    }
}

impl<F: FabricCaller + Clone> ThreadDistiller<F> {
    /// Single-call path for inputs under the threshold. Returns the parsed (or
    /// fallback) `Distilled` plus the extracted `author`/`post_count`; the
    /// outer `distill` applies bounds, lowercases tags, and attaches the
    /// platform payload so both paths share one exit.
    async fn distill_short(&self, transcript: &str, capture_note: Option<&str>) -> Result<ThreadDistilled> {
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
                log::warn!("ThreadDistiller: fabric call failed: {msg}; using {reason} fallback");
                return Ok((
                    fallback_distilled(ID, reason, transcript, None, &self.config.model),
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
                return Ok((
                    fallback_distilled(ID, "yaml-parse-error", transcript, Some(yaml_body), &self.config.model),
                    None,
                    0,
                ));
            }
        };

        let summary = parsed.summary.unwrap_or_default().trim().to_string();
        if summary.is_empty() {
            log::warn!("ThreadDistiller: empty summary; using missing-summary fallback");
            return Ok((
                fallback_distilled(ID, "missing-summary", transcript, Some(yaml_body), &self.config.model),
                None,
                0,
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

        let author = parsed.author.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        let post_count = parsed.post_count.unwrap_or(0);

        let word_count = transcript.split_whitespace().count();
        if claims.is_empty() && word_count > 200 {
            log::warn!("ThreadDistiller: empty claims for transcript with {word_count} words (possible pattern drift)");
        }

        let input_tokens = approx_tokens(transcript.len()) as u32;
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
            transcript: thread_transcript(transcript),
        };

        Ok((distilled, author, post_count))
    }

    /// Map-reduce path for long threads. Chunks are distilled in parallel with
    /// `distill-thread-chunk` (attribution-aware, anchorless); the reduce step
    /// synthesizes the summary, SELECTS the final claims from the pool, and
    /// re-emits `author`/`post-count` read from the verbatim `## Thread Head`
    /// section of the reduce input. Selection failure reverts claims to the
    /// chronological chunk merge (`reduce-selection-failed`); author/post-count
    /// still come from whatever the reduce parse yielded (both default when the
    /// reduce fails entirely, exactly as a fabric-failed single call would).
    async fn distill_long(&self, transcript: &str, chunks: Vec<String>) -> Result<ThreadDistilled> {
        log::debug!(
            "ThreadDistiller::distill_long: chunks={} threshold_tokens={} target_tokens={}",
            chunks.len(),
            SINGLE_CALL_TOKEN_THRESHOLD,
            CHUNK_TOKEN_TARGET
        );
        if chunks.is_empty() {
            return Ok((
                fallback_distilled(ID, "empty-transcript", transcript, None, &self.config.model),
                None,
                0,
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
                    log::warn!("ThreadDistiller: chunk fabric call failed: {err}");
                    any_chunk_failed = true;
                    continue;
                }
            };
            output_chars += raw.len();
            let parsed = match parse_chunk_yaml(&raw) {
                Ok(p) => p,
                Err(err) => {
                    log::warn!("ThreadDistiller: chunk yaml parse failed: {err}");
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

        let mut seen_tags = std::collections::HashSet::new();
        combined_tags.retain(|t| seen_tags.insert(t.clone()));

        if chunk_summaries.is_empty() {
            log::warn!("ThreadDistiller: all chunks failed; using chunk-failures fallback");
            return Ok((
                fallback_distilled(ID, "chunk-failures", transcript, None, &self.config.model),
                None,
                0,
            ));
        }

        // The reduce input carries the verbatim thread head so `author` /
        // `post-count` survive the long path.
        let head: String = transcript.chars().take(THREAD_HEAD_CHARS).collect();
        let joined = chunk_summaries.join("\n\n");
        let reduce_input = build_thread_reduce_input(&head, &chunk_summaries, &combined_claims);
        let mut anchors_stripped: u32 = 0;
        let (summary, claims, author, post_count, reduce_selection_failed) = match self
            .call_fabric(PATTERN_REDUCE, &reduce_input)
            .await
        {
            Ok(raw) => match parse_thread_reduce_yaml(&raw) {
                Ok(parsed) => {
                    let summary = parsed
                        .summary
                        .as_ref()
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| joined.clone());
                    let author = parsed.author.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
                    let post_count = parsed.post_count.unwrap_or(0);
                    match parsed
                        .claims
                        .and_then(|c| select_reduce_claims(c, &combined_claims, &mut anchors_stripped))
                    {
                        Some(selected) => (summary, selected, author, post_count, false),
                        None => {
                            log::warn!(
                                "ThreadDistiller: reduce selected no claims; falling back to chronological merge"
                            );
                            (summary, combined_claims.clone(), author, post_count, true)
                        }
                    }
                }
                Err(err) => {
                    log::warn!(
                        "ThreadDistiller: reduce yaml parse failed: {err}; falling back to concat + chronological claims"
                    );
                    (joined.clone(), combined_claims.clone(), None, 0, true)
                }
            },
            Err((reason, _)) => {
                log::warn!(
                    "ThreadDistiller: reduce fabric call failed ({reason}); falling back to concatenated chunks + chronological claims"
                );
                (joined.clone(), combined_claims.clone(), None, 0, true)
            }
        };

        let mut validation = ValidationMeta::default();
        if reduce_selection_failed {
            validation.fallback_reason = Some("reduce-selection-failed".to_string());
        } else if any_chunk_failed {
            validation.fallback_reason = Some("partial-chunk-failure".to_string());
        }
        validation.anchors_stripped = anchors_stripped;
        let input_tokens = approx_tokens(transcript.len()) as u32;
        let output_tokens = approx_tokens(output_chars) as u32;
        let distilled = Distilled {
            summary,
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
            // Preserve the concatenated post bodies so cortex can chunk-embed a
            // long thread (mirrors the single-call path).
            transcript: thread_transcript(transcript),
        };

        Ok((distilled, author, post_count))
    }

    /// Wrapper around `FabricCaller::call` mapping timeout vs error into stable
    /// fallback reasons (mirrors the video / article distillers).
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
                log::warn!("ThreadDistiller::call_fabric: pattern={pattern} reason={reason} err={msg}");
                Err((reason, msg))
            }
        }
    }
}

/// The full concatenated thread body preserved in `Distilled.transcript`, or
/// `None` when the transcript is blank.
fn thread_transcript(transcript: &str) -> Option<String> {
    if transcript.trim().is_empty() { None } else { Some(transcript.to_string()) }
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

/// Single-call thread YAML: the common distiller shape plus the two thread-only
/// fields.
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

/// Thread reduce YAML: the shared `ReduceYaml` shape (summary + selected
/// claims) plus the thread-only `author`/`post-count` the reduce re-emits from
/// the `## Thread Head` section. Serde-defaulted so a reduce that omits them
/// still parses (author → None, post_count → 0).
#[derive(Debug, Deserialize, Serialize)]
struct ThreadReduceYaml {
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    claims: Option<Vec<PatternClaim>>,
    #[serde(default)]
    author: Option<String>,
    #[serde(default, rename = "post-count")]
    post_count: Option<u32>,
}

fn parse_chunk_yaml(raw: &str) -> Result<ChunkYaml> {
    let yaml_body = strip_fences(raw);
    let parsed: ChunkYaml = serde_yaml::from_str(yaml_body)?;
    Ok(parsed)
}

fn parse_thread_reduce_yaml(raw: &str) -> Result<ThreadReduceYaml> {
    let yaml_body = strip_fences(raw);
    let parsed: ThreadReduceYaml = serde_yaml::from_str(yaml_body)?;
    Ok(parsed)
}

#[cfg(test)]
mod tests;
