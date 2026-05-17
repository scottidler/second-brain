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

use async_trait::async_trait;
use chrono::Utc;
use eyre::Result;
use futures::stream::{self, StreamExt};
use serde::{Deserialize, Serialize};
use vault::distilled::{Claim, Distilled, DistilledMeta, KindPayload, Link, ValidationMeta, VideoPayload};

use crate::{
    DistillExtractor, DistillInputs, FabricCaller, FabricRequest, enforce_bounds, fallback_distilled,
    validate::MAX_SUMMARY_CHARS,
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

        let distilled = if token_estimate <= SINGLE_CALL_TOKEN_THRESHOLD {
            self.distill_short(transcript).await
        } else {
            self.distill_long(transcript).await
        };

        let mut distilled = distilled?;
        validate_anchors(&mut distilled, inputs.video_metadata);
        // Phase B2: populate transcript for chunked semantic recall AFTER
        // distill_short / distill_long so neither path needs to know about
        // it (mirrors `voicenote.rs`). build_distilled, fallback_distilled,
        // and the map-reduce return in distill_long all default to None;
        // we override here so chunking works for all videos, including
        // long ones routed through the map-reduce path.
        let transcript_owned = if transcript.trim().is_empty() { None } else { Some(transcript.to_string()) };
        distilled.transcript = transcript_owned.clone();
        let mut bounded = enforce_bounds(distilled);
        debug_assert!(bounded.summary.chars().count() <= MAX_SUMMARY_CHARS);
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
    async fn distill_short(&self, transcript: &str) -> Result<Distilled> {
        let raw = match self.call_fabric(PATTERN_SHORT, transcript).await {
            Ok(r) => r,
            Err((reason, _)) => return Ok(fallback_distilled(ID, &reason, transcript, None)),
        };
        match parse_video_yaml(&raw) {
            Ok(parsed) => Ok(build_distilled(parsed, transcript, &raw, &self.config.model)),
            Err(_) => Ok(fallback_distilled(ID, "yaml-parse-error", transcript, Some(&raw))),
        }
    }

    /// Map-reduce path for long transcripts. Chunks are distilled in parallel
    /// (bounded by `chunk_concurrency`); chunk claims are concatenated and
    /// chunk summaries are reduced via a final Fabric call.
    async fn distill_long(&self, transcript: &str) -> Result<Distilled> {
        let chunks = chunk_transcript(transcript, CHUNK_TOKEN_TARGET);
        log::debug!(
            "VideoDistiller::distill_long: chunks={} threshold_tokens={} target_tokens={}",
            chunks.len(),
            SINGLE_CALL_TOKEN_THRESHOLD,
            CHUNK_TOKEN_TARGET
        );
        if chunks.is_empty() {
            return Ok(fallback_distilled(ID, "empty-transcript", transcript, None));
        }

        // Map step: parallelise chunk distillation, bounded by chunk_concurrency.
        let concurrency = self.config.chunk_concurrency.max(1);
        let chunk_results: Vec<(usize, String, Result<String>)> = stream::iter(chunks.iter().cloned().enumerate())
            .map(|(idx, chunk)| {
                let fabric = self.fabric.clone();
                let model = self.config.model.clone();
                let max_chars = self.config.max_chars;
                let timeout_secs = self.config.timeout_secs;
                async move {
                    let request = FabricRequest {
                        pattern: PATTERN_CHUNK.to_string(),
                        input: chunk.clone(),
                        model,
                        max_chars,
                        timeout_secs,
                    };
                    let result = fabric.call(request).await;
                    (idx, chunk, result)
                }
            })
            .buffer_unordered(concurrency)
            .collect()
            .await;
        let mut chunk_results = chunk_results;
        chunk_results.sort_by_key(|(idx, _, _)| *idx);

        let mut chunk_summaries: Vec<String> = Vec::with_capacity(chunk_results.len());
        let mut combined_claims: Vec<Claim> = Vec::new();
        let mut combined_links: Vec<Link> = Vec::new();
        let mut any_chunk_failed = false;
        let mut output_chars: usize = 0;

        for (_, chunk_text, result) in chunk_results {
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
                let text = c.text.trim().to_string();
                if text.is_empty() {
                    return None;
                }
                Some(Claim {
                    text,
                    anchor: c.anchor.filter(|s| !s.is_empty()),
                })
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
            let _ = chunk_text;
        }

        if chunk_summaries.is_empty() {
            log::warn!("VideoDistiller: all chunks failed; using map-reduce fallback");
            return Ok(fallback_distilled(ID, "chunk-failures", transcript, None));
        }

        // Reduce step: combine chunk summaries via Fabric.
        let joined = chunk_summaries.join("\n\n");
        let summary = match self.call_fabric(PATTERN_REDUCE, &joined).await {
            Ok(raw) => match parse_reduce_yaml(&raw) {
                Ok(parsed) => parsed
                    .summary
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| joined.clone()),
                Err(err) => {
                    log::warn!("VideoDistiller: reduce yaml parse failed: {err}; falling back to concat");
                    joined.clone()
                }
            },
            Err((reason, _)) => {
                log::warn!("VideoDistiller: reduce fabric call failed ({reason}); falling back to concatenated chunks");
                joined.clone()
            }
        };

        let mut validation = ValidationMeta::default();
        if any_chunk_failed {
            validation.fallback_reason = Some("partial-chunk-failure".to_string());
        }
        let input_tokens = approx_tokens(transcript.len()) as u32;
        let output_tokens = approx_tokens(output_chars) as u32;
        Ok(Distilled {
            summary,
            claims: combined_claims,
            tags: Vec::new(),
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
                let reason = if msg.contains("timed out") {
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
        return fallback_distilled(ID, "missing-summary", transcript, Some(raw));
    }
    let claims: Vec<Claim> = parsed
        .claims
        .unwrap_or_default()
        .into_iter()
        .filter_map(|c| {
            let text = c.text.trim().to_string();
            if text.is_empty() {
                return None;
            }
            Some(Claim {
                text,
                anchor: c.anchor.filter(|s| !s.is_empty()),
            })
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

    Distilled {
        summary,
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
        let Some(anchor) = claim.anchor.clone() else {
            continue;
        };
        match parse_anchor_seconds(&anchor) {
            Some(secs) => match duration_cap {
                Some(cap) if secs > cap => {
                    claim.anchor = None;
                    stripped += 1;
                }
                _ => {}
            },
            None => {
                claim.anchor = None;
                stripped += 1;
            }
        }
    }
    distilled.meta.validation.anchors_stripped = distilled.meta.validation.anchors_stripped.saturating_add(stripped);
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
    if m.channel.is_none() && m.duration_seconds.is_none() && m.published_at.is_none() {
        return;
    }
    distilled.kind_specific = Some(KindPayload::Video(VideoPayload {
        channel: m.channel.clone(),
        duration_seconds: m.duration_seconds,
        published_at: m.published_at.clone(),
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
    let bytes = transcript.as_bytes();
    let len = bytes.len();
    let mut chunks: Vec<String> = Vec::new();
    let mut start: usize = 0;
    while start < len {
        let mut end = (start + target_chars).min(len);
        if end < len {
            end = find_boundary(transcript, start, end);
        }
        if end <= start {
            end = (start + target_chars).min(len);
        }
        chunks.push(transcript[start..end].to_string());
        start = end;
    }
    chunks
}

/// Find a sentence boundary at or before `end`, walking backwards from `end`
/// until we hit `.`, `!`, `?`, or `\n` followed by whitespace. Falls back
/// to `end` (hard cut) when no boundary is found within the lookback.
fn find_boundary(transcript: &str, start: usize, end: usize) -> usize {
    let bytes = transcript.as_bytes();
    let lookback = end.saturating_sub(start).min(2048);
    let floor = end.saturating_sub(lookback);
    let mut i = end;
    while i > floor {
        i -= 1;
        let b = bytes[i];
        if b == b'\n' {
            return i + 1;
        }
        if (b == b'.' || b == b'!' || b == b'?') && i + 1 < bytes.len() && bytes[i + 1].is_ascii_whitespace() {
            return i + 1;
        }
    }
    end
}

/// Approximate token count from char count (4 chars/token rule of thumb).
pub fn approx_tokens(chars: usize) -> usize {
    chars / CHARS_PER_TOKEN
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

#[derive(Debug, Deserialize, Serialize)]
struct ReduceYaml {
    #[serde(default)]
    summary: Option<String>,
}

#[cfg(test)]
mod tests;
