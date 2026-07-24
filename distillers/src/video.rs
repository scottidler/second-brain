//! YouTube video distiller.
//!
//! Short transcripts go straight to `distill-video` (single Fabric call).
//! Long transcripts split at sentence boundaries into ~8K-token chunks;
//! each chunk runs through `distill-video-chunk` in parallel and produces
//! a partial `Distilled`. Chunk claims are merged structurally (no LLM
//! reduce on the claim list - they are already structured). A single
//! `distill-video-reduce` call combines the chunk summaries into the
//! final coherent summary.
//!
//! Anchor validation: timestamps must match `HH:MM:SS` or `MM:SS` and
//! fall within `video_metadata.duration_seconds` (when supplied). Out
//! of range strips the anchor (claim text retained, anchor cleared)
//! and increments `meta.validation.anchors_stripped`.

use crate::parse::{
    EnumCandidate, PatternYaml, ReduceYaml, approx_tokens, build_reduce_input, find_boundary,
    resolve_reduce_enumeration, select_reduce_claims, strip_fences,
};
use async_trait::async_trait;
use chrono::Utc;
use eyre::Result;
use futures::stream::{self, StreamExt};
use vault::distilled::{Claim, Distilled, DistilledMeta, KindPayload, Link, ValidationMeta, VideoPayload};

use crate::{
    DistillExtractor, DistillInputs, FabricCaller, FabricRequest, enforce_bounds, fallback_distilled,
    mark_enumeration_shortfall, max_claims, validate::MAX_SUMMARY_CHARS,
};

const ID: &str = "distill-video-v1";
const PATTERN_SHORT: &str = "distill-video";
const PATTERN_CHUNK: &str = "distill-video-chunk";
const PATTERN_REDUCE: &str = "distill-video-reduce";

/// Token threshold above which we switch to the map-reduce path. Calibrated
/// against the design doc's "<12K tokens => single call" guidance.
pub const SINGLE_CALL_TOKEN_THRESHOLD: usize = 12_000;
/// Target chunk size (in approximate tokens) for the map step. Chunks are
/// cut at sentence boundaries within this budget.
pub const CHUNK_TOKEN_TARGET: usize = 8_000;
/// 4 chars per token is a common rule of thumb for English prose.
const CHARS_PER_TOKEN: usize = 4;
/// Default parallelism for chunk distillation. The chunk path is I/O bound
/// (fabric subprocess), so we don't need to match cpu count.
const DEFAULT_CHUNK_CONCURRENCY: usize = 4;

/// Video metadata frozen at ingest. Mirrors `vault::distilled::VideoPayload`;
/// the duplication keeps distillers free of yt-dlp / HTTP concerns.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VideoMetadata {
    pub channel: Option<String>,
    pub duration_seconds: Option<u32>,
    /// ISO 8601 date, e.g. "2026-05-16". Optional because yt-dlp may not
    /// always surface it.
    pub published_at: Option<String>,
    /// `owner/repo` slugs harvested from the video description (see
    /// `borg::github::extract_repo_slugs`). Empty when none were found.
    pub repos: Vec<String>,
}

/// Tunables for the video distiller.
#[derive(Debug, Clone)]
pub struct VideoConfig {
    pub model: String,
    pub max_chars: usize,
    pub timeout_secs: u64,
    /// Max chunks distilled in parallel during the map step.
    pub chunk_concurrency: usize,
}

impl Default for VideoConfig {
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
pub struct VideoDistiller<F: FabricCaller + Clone> {
    fabric: F,
    config: VideoConfig,
}

impl<F: FabricCaller + Clone> VideoDistiller<F> {
    pub fn new(fabric: F, config: VideoConfig) -> Self {
        Self { fabric, config }
    }
}

#[async_trait]
impl<F: FabricCaller + Clone> DistillExtractor for VideoDistiller<F> {
    fn id(&self) -> &'static str {
        ID
    }

    async fn distill(&self, inputs: DistillInputs<'_>) -> Result<Distilled> {
        let transcript = inputs.transcript;
        let token_estimate = approx_tokens(transcript.len());
        let has_metadata = inputs.video_metadata.is_some();
        log::debug!(
            "VideoDistiller::distill: transcript_len={} approx_tokens={} source_url={:?} has_metadata={}",
            transcript.len(),
            token_estimate,
            inputs.source_url,
            has_metadata
        );

        // The chunk count drives the size-aware claim budget (Phase 5). The
        // single-call path is chunk_count = 1 (cap 10); the map-reduce path
        // computes the chunks once here and hands them to distill_long so the
        // real count scales the budget instead of the flat max_claims(1).
        let (mut distilled, chunk_count) = if token_estimate <= SINGLE_CALL_TOKEN_THRESHOLD {
            (self.distill_short(transcript, inputs.capture_note).await?, 1usize)
        } else {
            let chunks = chunk_transcript(transcript, CHUNK_TOKEN_TARGET);
            let chunk_count = chunks.len().max(1);
            (self.distill_long(transcript, chunks).await?, chunk_count)
        };

        validate_anchors(&mut distilled, inputs.video_metadata);
        // Phase B2: populate transcript for chunked semantic recall AFTER
        // distill_short / distill_long so neither path needs to know about
        // it (mirrors `voicenote.rs`). build_distilled, fallback_distilled,
        // and the map-reduce return in distill_long all default to None;
        // we override here so chunking works for all videos, including
        // long ones routed through the map-reduce path.
        let transcript_owned = if transcript.trim().is_empty() { None } else { Some(transcript.to_string()) };
        distilled.transcript = transcript_owned.clone();
        // Phase 5: the real chunk count scales the claim budget so a long
        // video keeps proportionally more selected claims (single-call path
        // passes chunk_count = 1, holding the cap at 10 as before).
        let mut bounded = enforce_bounds(distilled, max_claims(chunk_count));
        debug_assert!(bounded.summary.chars().count() <= MAX_SUMMARY_CHARS);
        // Enumeration shortfall (Resolved Decision 2026-07-07): flag AFTER
        // enforce_bounds so the item-count cap (which only trims counts ABOVE
        // declared_count) can never manufacture a false shortfall. Publishes
        // degraded, never blocks.
        mark_enumeration_shortfall(&mut bounded);
        bounded.tags.iter_mut().for_each(|t| *t = t.to_lowercase());
        attach_payload(&mut bounded, inputs.video_metadata);
        // Defensive re-set after enforce_bounds and attach_payload in case
        // any future bounds logic touches the transcript field.
        bounded.transcript = transcript_owned;
        Ok(bounded)
    }
}

impl<F: FabricCaller + Clone> VideoDistiller<F> {
    /// Single-call path for transcripts under the threshold.
    async fn distill_short(&self, transcript: &str, capture_note: Option<&str>) -> Result<Distilled> {
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
        let input = crate::parse::compose_capture_input(transcript, capture_note);
        let raw = match self.call_fabric(PATTERN_SHORT, &input).await {
            Ok(r) => r,
            Err((reason, _)) => return Ok(fallback_distilled(ID, &reason, transcript, None, &self.config.model)),
        };
        match parse_video_yaml(&raw) {
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

    /// Map-reduce path for long transcripts. Chunks are distilled in parallel
    /// (bounded by `chunk_concurrency`); chunk claims are concatenated and
    /// chunk summaries are reduced via a final Fabric call.
    async fn distill_long(&self, transcript: &str, chunks: Vec<String>) -> Result<Distilled> {
        log::debug!(
            "VideoDistiller::distill_long: chunks={} threshold_tokens={} target_tokens={}",
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

        // Map step: parallelise chunk distillation, bounded by chunk_concurrency.
        // Carry only (idx, result) out of the stream - the chunk text was moved
        // into the request, so cloning it through the tuple just to `let _`-drop
        // it later wasted ~32 KB per chunk (mirrors the voicenote twin).
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
        // Phase 4: pool enumeration candidates across chunks for the reduce step,
        // and carry the first declared count any chunk saw (stated once, in the
        // intro chunk).
        let mut combined_candidates: Vec<EnumCandidate> = Vec::new();
        let mut declared_count: Option<u32> = None;
        let mut any_chunk_failed = false;
        let mut output_chars: usize = 0;

        for (_, result) in chunk_results {
            let raw = match result {
                Ok(r) => r,
                Err(err) => {
                    let msg = format!("{err}");
                    log::warn!("VideoDistiller: chunk fabric call failed: {msg}");
                    any_chunk_failed = true;
                    continue;
                }
            };
            output_chars += raw.len();
            let parsed = match parse_video_yaml(&raw) {
                Ok(p) => p,
                Err(err) => {
                    log::warn!("VideoDistiller: chunk yaml parse failed: {err}");
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
            // Union chunk tags (was dropped entirely - long videos lost ALL
            // distiller tags). Deduped below; enforce_bounds caps at 7.
            combined_tags.extend(
                parsed
                    .tags
                    .unwrap_or_default()
                    .into_iter()
                    .map(|t| t.trim().to_string())
                    .filter(|t| !t.is_empty()),
            );
            // Phase 4: pool this chunk's enumeration candidates and adopt the
            // first declared count seen (the intro states it once).
            if declared_count.is_none() {
                declared_count = parsed.declared_count;
            }
            combined_candidates.extend(
                parsed
                    .enumeration_candidates
                    .unwrap_or_default()
                    .into_iter()
                    .map(|c| c.into_candidate())
                    .filter(|c| !c.name.is_empty()),
            );
        }

        // Dedup tags, first-seen order.
        let mut seen_tags = std::collections::HashSet::new();
        combined_tags.retain(|t| seen_tags.insert(t.clone()));

        if chunk_summaries.is_empty() {
            log::warn!("VideoDistiller: all chunks failed; using map-reduce fallback");
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
        // the whole timeline. `combined_claims` is both the selection pool
        // (rendered into the reduce input) and the chronological fallback used
        // when selection fails — that fallback silently reintroduces the
        // head-bias this phase removes, so it is recorded as a distinct
        // `reduce-selection-failed` reason (never folded into
        // bounds_truncations) for the eval harness to watch.
        let joined = chunk_summaries.join("\n\n");
        let reduce_input = build_reduce_input(&chunk_summaries, &combined_claims, &combined_candidates, declared_count);
        let mut anchors_stripped: u32 = 0;
        // tldr / enumeration / key_ideas are reduce-only outputs; the fallback
        // arms (parse/call failure) leave them empty because those arms never
        // saw the reduce pattern's structured output.
        let mut tldr: Option<String> = None;
        let mut enumeration: Option<vault::distilled::Enumeration> = None;
        let mut key_ideas: Vec<String> = Vec::new();
        let (summary, claims, reduce_selection_failed) = match self.call_fabric(PATTERN_REDUCE, &reduce_input).await {
            Ok(raw) => match parse_reduce_yaml(&raw) {
                Ok(parsed) => {
                    let summary = parsed
                        .summary
                        .as_ref()
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| joined.clone());
                    tldr = parsed.tldr.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
                    // Restore the enumeration from the pooled candidates, applying
                    // the anchor-honesty rule against the candidate anchors.
                    enumeration = parsed
                        .enumeration
                        .and_then(|e| resolve_reduce_enumeration(e, &combined_candidates, &mut anchors_stripped));
                    key_ideas = parsed
                        .key_ideas
                        .unwrap_or_default()
                        .into_iter()
                        .map(|k| k.trim().to_string())
                        .filter(|k| !k.is_empty())
                        .collect();
                    match parsed
                        .claims
                        .and_then(|c| select_reduce_claims(c, &combined_claims, &mut anchors_stripped))
                    {
                        Some(selected) => (summary, selected, false),
                        None => {
                            log::warn!(
                                "VideoDistiller: reduce selected no claims; falling back to chronological merge"
                            );
                            (summary, combined_claims.clone(), true)
                        }
                    }
                }
                Err(err) => {
                    log::warn!(
                        "VideoDistiller: reduce yaml parse failed: {err}; falling back to concat + chronological claims"
                    );
                    (joined.clone(), combined_claims.clone(), true)
                }
            },
            Err((reason, _)) => {
                log::warn!(
                    "VideoDistiller: reduce fabric call failed ({reason}); falling back to concatenated chunks + chronological claims"
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
            slug: None,
            summary,
            tldr,
            enumeration,
            key_ideas,
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
            // transcript is populated by the outer `distill()` after this
            // returns; the map-reduce path doesn't need to track it.
            transcript: None,
        })
    }

    /// Wrapper around `FabricCaller::call` that maps timeout vs error
    /// strings into stable fallback reasons.
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
                log::warn!("VideoDistiller::call_fabric: pattern={pattern} reason={reason} err={msg}");
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
            let claim = c.into_claim();
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
    if claims.is_empty() && word_count > 500 {
        log::warn!("VideoDistiller: empty claims for transcript with {word_count} words (possible pattern drift)");
    }

    // Phase 4: single-call enumeration/tldr/key-ideas straight off the parsed
    // pattern output. `into_enumeration` returns None for an empty `items:`
    // list so a stray `enumeration:` header never renders an empty section.
    let tldr = parsed.tldr.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let enumeration = parsed.enumeration.and_then(|e| e.into_enumeration());
    let key_ideas: Vec<String> = parsed
        .key_ideas
        .unwrap_or_default()
        .into_iter()
        .map(|k| k.trim().to_string())
        .filter(|k| !k.is_empty())
        .collect();

    Distilled {
        slug: None,
        summary,
        tldr,
        enumeration,
        key_ideas,
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
        // transcript is populated by the outer `distill()` after both the
        // short and map-reduce paths return; leaving it None here keeps
        // the assignment in one place. See `VideoDistiller::distill`.
        transcript: None,
    }
}

/// Per-kind anchor validation. Parses each claim's anchor as `HH:MM:SS`
/// or `MM:SS`; out-of-range or malformed strips the anchor and bumps
/// `validation.anchors_stripped`.
pub fn validate_anchors(distilled: &mut Distilled, metadata: Option<&VideoMetadata>) {
    let duration_cap = metadata.and_then(|m| m.duration_seconds);
    let mut stripped: u32 = 0;
    for claim in &mut distilled.claims {
        if strip_dishonest_anchor(&mut claim.anchor, duration_cap) {
            stripped += 1;
        }
    }
    // Enumeration item anchors ride the same anchor-honesty rule (Phase 4): a
    // malformed timestamp, or one past the video duration, is not a real
    // transcript position, so strip it (item text retained). Reduce-path items
    // already passed the candidate-pool gate; this catches the single-call path
    // (no pool) where the model may lift an in-format but out-of-range anchor
    // from the description's chapter list.
    if let Some(enumeration) = distilled.enumeration.as_mut() {
        for item in &mut enumeration.items {
            if strip_dishonest_anchor(&mut item.anchor, duration_cap) {
                stripped += 1;
            }
        }
    }
    distilled.meta.validation.anchors_stripped = distilled.meta.validation.anchors_stripped.saturating_add(stripped);
}

/// Strip an anchor that fails the video anchor-honesty rule: unparseable as
/// `HH:MM:SS`/`MM:SS`, or (when a duration cap is known) past the video's end.
/// Returns `true` when it stripped an anchor. Shared by claim and enumeration
/// anchor validation so both apply exactly the same rule.
fn strip_dishonest_anchor(anchor: &mut Option<String>, duration_cap: Option<u32>) -> bool {
    let Some(value) = anchor.as_deref() else {
        return false;
    };
    let dishonest = match parse_anchor_seconds(value) {
        Some(secs) => matches!(duration_cap, Some(cap) if secs > cap),
        None => true,
    };
    if dishonest {
        *anchor = None;
    }
    dishonest
}

/// Parse `HH:MM:SS` or `MM:SS` into seconds. Returns None on unparseable
/// input. Whitespace is trimmed; surrounding brackets are tolerated.
pub fn parse_anchor_seconds(anchor: &str) -> Option<u32> {
    let trimmed = anchor.trim().trim_start_matches('[').trim_end_matches(']');
    let parts: Vec<&str> = trimmed.split(':').collect();
    let (h, m, s) = match parts.len() {
        3 => (
            parts[0].parse::<u32>().ok()?,
            parts[1].parse::<u32>().ok()?,
            parts[2].parse::<u32>().ok()?,
        ),
        2 => (0u32, parts[0].parse::<u32>().ok()?, parts[1].parse::<u32>().ok()?),
        _ => return None,
    };
    if m >= 60 || s >= 60 {
        return None;
    }
    Some(h * 3600 + m * 60 + s)
}

/// Attach the `KindPayload::Video` if any metadata field is populated.
fn attach_payload(distilled: &mut Distilled, metadata: Option<&VideoMetadata>) {
    let Some(m) = metadata else {
        return;
    };
    if m.channel.is_none() && m.duration_seconds.is_none() && m.published_at.is_none() && m.repos.is_empty() {
        return;
    }
    distilled.kind_specific = Some(KindPayload::Video(VideoPayload {
        channel: m.channel.clone(),
        duration_seconds: m.duration_seconds,
        published_at: m.published_at.clone(),
        repos: m.repos.clone(),
    }));
}

/// Split a transcript into chunks at sentence boundaries within the target
/// token budget. Sentences are defined as text terminated by `.`, `!`, or
/// `?` followed by whitespace; lines without sentence terminators end the
/// chunk at the line break instead.
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

fn parse_video_yaml(raw: &str) -> Result<PatternYaml> {
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
