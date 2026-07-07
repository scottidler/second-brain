//! Voice-note distiller.
//!
//! Ports the structural template from `VideoDistiller` with timestamp
//! handling stripped: Groq ASR transcripts come back as plain text with no
//! anchors, so claim `anchor` fields are always `None`.
//!
//! Short transcripts go straight to `distill-voicenote` (single Fabric call).
//! Long transcripts split at sentence boundaries into ~8K-token chunks; each
//! chunk runs through `distill-voicenote-chunk` in parallel and produces a
//! partial `Distilled`. Chunk claims are concatenated; chunk summaries are
//! reduced via a single `distill-voicenote-reduce` call into the final
//! summary.
//!
//! Phase 9c-voicenote contract: `Distilled.transcript` always carries the
//! full raw Groq output so the published vault note is a verbatim archive
//! even after the LLM-distilled summary collapses the original. This is the
//! sole structural difference vs. URL kinds (which leave transcript as None).

use crate::parse::{
    PatternYaml, ReduceYaml, approx_tokens, build_reduce_input, find_boundary, select_reduce_claims, strip_fences,
};
use async_trait::async_trait;
use chrono::Utc;
use eyre::Result;
use futures::stream::{self, StreamExt};
use vault::distilled::{Claim, Distilled, DistilledMeta, Link, ValidationMeta};

use crate::{
    DistillExtractor, DistillInputs, FabricCaller, FabricRequest, enforce_bounds, fallback_distilled, max_claims,
    validate::MAX_SUMMARY_CHARS,
};

const ID: &str = "distill-voicenote-v1";
const PATTERN_SHORT: &str = "distill-voicenote";
const PATTERN_CHUNK: &str = "distill-voicenote-chunk";
const PATTERN_REDUCE: &str = "distill-voicenote-reduce";

/// Token threshold above which we switch to the map-reduce path. Mirrors
/// `VideoDistiller::SINGLE_CALL_TOKEN_THRESHOLD` (12K tokens).
pub const SINGLE_CALL_TOKEN_THRESHOLD: usize = 12_000;
/// Target chunk size (in approximate tokens) for the map step.
pub const CHUNK_TOKEN_TARGET: usize = 8_000;
/// 4 chars per token is a common rule of thumb for English prose.
const CHARS_PER_TOKEN: usize = 4;
/// Default parallelism for chunk distillation. I/O bound (fabric subprocess).
const DEFAULT_CHUNK_CONCURRENCY: usize = 4;

/// Tunables for the voicenote distiller. Same shape as `VideoConfig`.
#[derive(Debug, Clone)]
pub struct VoiceNoteConfig {
    pub model: String,
    pub max_chars: usize,
    pub timeout_secs: u64,
    pub chunk_concurrency: usize,
}

impl Default for VoiceNoteConfig {
    fn default() -> Self {
        Self {
            model: String::new(),
            max_chars: 32_000,
            timeout_secs: 60,
            chunk_concurrency: DEFAULT_CHUNK_CONCURRENCY,
        }
    }
}

#[derive(Debug, Clone)]
pub struct VoiceNoteDistiller<F: FabricCaller + Clone> {
    fabric: F,
    config: VoiceNoteConfig,
}

impl<F: FabricCaller + Clone> VoiceNoteDistiller<F> {
    pub fn new(fabric: F, config: VoiceNoteConfig) -> Self {
        Self { fabric, config }
    }
}

#[async_trait]
impl<F: FabricCaller + Clone> DistillExtractor for VoiceNoteDistiller<F> {
    fn id(&self) -> &'static str {
        ID
    }

    async fn distill(&self, inputs: DistillInputs<'_>) -> Result<Distilled> {
        let transcript = inputs.transcript;
        let token_estimate = approx_tokens(transcript.len());
        log::debug!(
            "VoiceNoteDistiller::distill: transcript_len={} approx_tokens={} title_hint={:?}",
            transcript.len(),
            token_estimate,
            inputs.title_hint
        );

        // The chunk count drives the size-aware claim budget (Phase 5). The
        // single-call path is chunk_count = 1 (cap 10); the map-reduce path
        // computes the chunks once here and hands them to distill_long so the
        // real count scales the budget instead of the flat max_claims(1).
        let (mut distilled, chunk_count) = if token_estimate <= SINGLE_CALL_TOKEN_THRESHOLD {
            (self.distill_short(transcript).await?, 1usize)
        } else {
            let chunks = chunk_transcript(transcript, CHUNK_TOKEN_TARGET);
            let chunk_count = chunks.len().max(1);
            (self.distill_long(transcript, chunks).await?, chunk_count)
        };

        // Verbatim preservation contract. Set AFTER distill_short / distill_long
        // so neither path needs to know about transcript; both produce a
        // Distilled with transcript = None and we override here.
        distilled.transcript = Some(transcript.to_string());

        // Phase 5: the real chunk count scales the claim budget so a long
        // voice note keeps proportionally more selected claims (single-call
        // path passes chunk_count = 1, holding the cap at 10 as before).
        let mut bounded = enforce_bounds(distilled, max_claims(chunk_count));
        debug_assert!(bounded.summary.chars().count() <= MAX_SUMMARY_CHARS);
        bounded.tags.iter_mut().for_each(|t| *t = t.to_lowercase());
        // Re-set transcript after enforce_bounds in case any future bounds
        // logic touches it. enforce_bounds today only clips summary/claims/tags
        // but defensive about future drift.
        bounded.transcript = Some(transcript.to_string());
        Ok(bounded)
    }
}

impl<F: FabricCaller + Clone> VoiceNoteDistiller<F> {
    /// Single-call path for transcripts under the threshold.
    async fn distill_short(&self, transcript: &str) -> Result<Distilled> {
        // Short-circuit an empty transcript before burning a Fabric call (the
        // long path already guards via `chunks.is_empty()`).
        if transcript.trim().is_empty() {
            return Ok(fallback_distilled(
                ID,
                "empty-transcript",
                transcript,
                None,
                &self.config.model,
            ));
        }
        let raw = match self.call_fabric(PATTERN_SHORT, transcript).await {
            Ok(r) => r,
            Err((reason, _)) => return Ok(fallback_distilled(ID, &reason, transcript, None, &self.config.model)),
        };
        match parse_voicenote_yaml(&raw) {
            Ok(parsed) => Ok(build_distilled(parsed, transcript, &raw, &self.config.model)),
            Err(_) => Ok(fallback_distilled(
                ID,
                "yaml-parse-error",
                transcript,
                Some(&raw),
                &self.config.model,
            )),
        }
    }

    /// Map-reduce path for long transcripts.
    async fn distill_long(&self, transcript: &str, chunks: Vec<String>) -> Result<Distilled> {
        log::debug!(
            "VoiceNoteDistiller::distill_long: chunks={} threshold_tokens={} target_tokens={}",
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

        let concurrency = self.config.chunk_concurrency.max(1);
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
                    let msg = format!("{err}");
                    log::warn!("VoiceNoteDistiller: chunk fabric call failed: {msg}");
                    any_chunk_failed = true;
                    continue;
                }
            };
            output_chars += raw.len();
            let parsed = match parse_voicenote_yaml(&raw) {
                Ok(p) => p,
                Err(err) => {
                    log::warn!("VoiceNoteDistiller: chunk yaml parse failed: {err}");
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
                let mut claim = c.into_claim();
                // Voice notes have no anchors at this layer regardless of what
                // the pattern produced.
                claim.anchor = None;
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
            // Union chunk tags (was dropped entirely). Deduped below;
            // enforce_bounds caps at 7.
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
            log::warn!("VoiceNoteDistiller: all chunks failed; using map-reduce fallback");
            return Ok(fallback_distilled(
                ID,
                "chunk-failures",
                transcript,
                None,
                &self.config.model,
            ));
        }

        // Reduce step (Phase 5): the reduce pattern re-synthesizes the summary
        // AND SELECTS the final claims from the pooled chunk claims, spanning
        // the whole recording. `combined_claims` is both the selection pool
        // and the chronological fallback used when selection fails — that
        // fallback silently reintroduces head-bias, so it is recorded as a
        // distinct `reduce-selection-failed` reason for the eval harness.
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
                        Some(mut selected) => {
                            // Voice notes carry no anchors at this layer regardless
                            // of what the reduce pattern produced.
                            selected.iter_mut().for_each(|c| c.anchor = None);
                            (summary, selected, false)
                        }
                        None => {
                            log::warn!(
                                "VoiceNoteDistiller: reduce selected no claims; falling back to chronological merge"
                            );
                            (summary, combined_claims.clone(), true)
                        }
                    }
                }
                Err(err) => {
                    log::warn!(
                        "VoiceNoteDistiller: reduce yaml parse failed: {err}; falling back to concat + chronological claims"
                    );
                    (joined.clone(), combined_claims.clone(), true)
                }
            },
            Err((reason, _)) => {
                log::warn!(
                    "VoiceNoteDistiller: reduce fabric call failed ({reason}); falling back to concatenated chunks + chronological claims"
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
            // Caller (`distill` above) overrides this with Some(transcript)
            // after both short and long paths return; staying None here keeps
            // the helper internals symmetric with URL kinds.
            transcript: None,
        })
    }

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
                log::warn!("VoiceNoteDistiller::call_fabric: pattern={pattern} reason={reason} err={msg}");
                Err((reason, msg))
            }
        }
    }
}

fn build_distilled(parsed: PatternYaml, transcript: &str, raw: &str, model: &str) -> Distilled {
    let summary = parsed.summary.unwrap_or_default().trim().to_string();
    if summary.is_empty() {
        return fallback_distilled(ID, "missing-summary", transcript, Some(raw), model);
    }
    let claims: Vec<Claim> = parsed
        .claims
        .unwrap_or_default()
        .into_iter()
        .filter_map(|c| {
            let mut claim = c.into_claim();
            claim.anchor = None;
            (!claim.text.is_empty()).then_some(claim)
        })
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
        .filter_map(|l| {
            let url = l.url.trim().to_string();
            if url.is_empty() {
                return None;
            }
            Some(Link {
                url,
                label: l.label.filter(|s| !s.is_empty()),
            })
        })
        .collect();

    let word_count = transcript.split_whitespace().count();
    if claims.is_empty() && word_count > 200 {
        log::warn!("VoiceNoteDistiller: empty claims for transcript with {word_count} words (possible pattern drift)");
    }

    Distilled {
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
            model: if model.is_empty() { "default".to_string() } else { model.to_string() },
            input_tokens: approx_tokens(transcript.len()) as u32,
            output_tokens: approx_tokens(raw.len()) as u32,
            produced_at: Utc::now().to_rfc3339(),
            validation: ValidationMeta::default(),
        },
        transcript: None,
    }
}

/// Split a transcript into chunks at sentence boundaries within the target
/// token budget. Mirrors the video chunker; sentences are defined as text
/// terminated by `.`, `!`, or `?` followed by whitespace.
pub fn chunk_transcript(transcript: &str, target_tokens: usize) -> Vec<String> {
    let target_chars = target_tokens.saturating_mul(CHARS_PER_TOKEN);
    if target_chars == 0 || transcript.is_empty() {
        return Vec::new();
    }
    let len = transcript.len();
    let mut chunks: Vec<String> = Vec::new();
    let mut start: usize = 0;
    while start < len {
        let raw_end = (start + target_chars).min(len);
        let found = if raw_end < len { find_boundary(transcript, start, raw_end) } else { raw_end };
        // Snap to a char boundary: find_boundary's ASCII matches are safe, but
        // its fallback returns the raw byte index, which can split a codepoint
        // (the old `transcript[start..end]` byte slice then panicked).
        let mut end = transcript.floor_char_boundary(found);
        if end <= start {
            end = transcript.ceil_char_boundary((start + 1).min(len));
        }
        chunks.push(transcript[start..end].to_string());
        start = end;
    }
    chunks
}

fn parse_voicenote_yaml(raw: &str) -> Result<PatternYaml> {
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
