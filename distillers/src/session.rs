//! Claude Code session distiller (harvest-clyde-sessions design, Phase 4).
//!
//! Input is the concatenated, role-labeled transcript of a harvested thread
//! (1+ clyde sessions sharing a `(cwd, git-branch)` cluster). Unlike the
//! article/video/thread distillers, a session's value is asymmetric: setup and
//! decisions sit at the HEAD, conclusions and gotchas at the TAIL, while the
//! middle is exploration. So the primary long-thread strategy is **head+tail
//! windowing** to `SessionConfig.token_cap` (design doc: Distillation > Input),
//! NOT sentence-boundary chunking.
//!
//! Routing mirrors the sibling map-reduce distillers, applied to the *windowed*
//! body: at or below `SINGLE_CALL_TOKEN_THRESHOLD` a single `distill-session`
//! call runs; above it, the `distill-session-chunk` / `distill-session-reduce`
//! map-reduce path runs. With the default `token_cap == SINGLE_CALL_TOKEN_
//! THRESHOLD` (12K), windowing always brings a thread down to the single-call
//! path; the chunk/reduce path is live only when an operator raises `token_cap`
//! above the threshold (tested explicitly).
//!
//! Truncation is NEVER silent to the model. When the export flagged
//! `body-truncated` (clyde cut the tail), or head+tail windowing dropped the
//! middle, the assembled prompt carries an explicit `[TRANSCRIPT TRUNCATED]`
//! marker so the LLM knows it is not seeing the whole transcript.
//!
//! The prompt contract (see `borg/patterns/distill-session*.md`) extracts
//! KNOWLEDGE - decisions made, approaches rejected and why, gotchas learned,
//! reusable patterns - and forbids narration / activity ledgers (the conductor
//! anti-pattern the design calls out).
//!
//! Embedding policy (design doc): only the distilled note is embedded; the
//! staged transcript is trace-recallable, never embedded. So this distiller
//! leaves `Distilled.transcript` `None` (like Article/Repo) - the verbatim
//! body lives in the staged `body.txt`, not the note.

use async_trait::async_trait;
use chrono::Utc;
use eyre::Result;
use futures::stream::{self, StreamExt};
use vault::distilled::{Claim, Distilled, DistilledMeta, KindPayload, Link, SessionPayload, ValidationMeta};

use crate::parse::{
    PatternYaml, ReduceYaml, approx_tokens, build_reduce_input, compose_capture_input, select_reduce_claims,
    strip_fences,
};
use crate::video::{CHUNK_TOKEN_TARGET, SINGLE_CALL_TOKEN_THRESHOLD, chunk_transcript};

use crate::{
    DistillExtractor, DistillInputs, FabricCaller, FabricRequest, enforce_bounds, fallback_distilled, max_claims,
    validate::MAX_SUMMARY_CHARS,
};

const ID: &str = "distill-session-v1";
const PATTERN: &str = "distill-session";
const PATTERN_CHUNK: &str = "distill-session-chunk";
const PATTERN_REDUCE: &str = "distill-session-reduce";

/// The explicit marker the assembled prompt carries when the transcript the
/// model sees is not the whole thing (design doc Phase 4: "truncation is never
/// silent to the model").
pub const TRUNCATION_MARKER: &str = "[TRANSCRIPT TRUNCATED]";

/// 4 chars per token, the shared English-prose rule of thumb (`parse::CHARS_PER_TOKEN`).
const CHARS_PER_TOKEN: usize = 4;
/// Head share of the windowing budget: a session's decisions/setup lead, so the
/// head gets the larger slice; the tail (conclusions/gotchas) gets the rest.
const HEAD_BUDGET_NUM: usize = 3;
const HEAD_BUDGET_DEN: usize = 5;
/// Default parallelism for chunk distillation on the (rare) map-reduce path.
const DEFAULT_CHUNK_CONCURRENCY: usize = 4;

/// Deterministic Stage-0 metadata for a harvested session thread. Mirrors
/// `VideoMetadata`/`RepoMetadata`: the distiller reads it to attach
/// `KindPayload::Session` and to know whether the export truncated the body.
/// Every field is known before distillation (from the clustered
/// `SessionRecord`s), so none of it is LLM-derived.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionMetadata {
    /// `<org>/<repo>` anchor for the thread's primary session, verbatim from
    /// clyde's export `repo` field. `None` when the cwd has no repo anchor.
    pub repo: Option<String>,
    /// Member session ids in `created` order (primary first per the harvest
    /// clustering). Feed the note's `## Sessions` footer as `clyde://` refs.
    pub session_ids: Vec<String>,
    /// Total messages across every member session.
    pub msg_count: u32,
    /// ISO 8601 timestamp of the earliest member session.
    pub date_start: Option<String>,
    /// ISO 8601 timestamp of the latest member session.
    pub date_end: Option<String>,
    /// The export flagged `body-truncated` (clyde cut the transcript tail).
    /// Drives the `[TRANSCRIPT TRUNCATED]` marker so truncation is never silent.
    pub body_truncated: bool,
}

/// Tunables for the session distiller. `token_cap` is the head+tail windowing
/// budget (design doc: `harvest.token-cap`); `model` inherits `llm.model` when
/// empty, resolved by borg at construction (Phase 5).
#[derive(Debug, Clone)]
pub struct SessionConfig {
    pub model: String,
    pub max_chars: usize,
    pub timeout_secs: u64,
    /// Head+tail windowing budget in approximate tokens (design doc:
    /// `harvest.token-cap`, default 12000).
    pub token_cap: usize,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            model: String::new(),
            max_chars: 32_000,
            timeout_secs: 60,
            token_cap: 12_000,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SessionDistiller<F: FabricCaller + Clone> {
    fabric: F,
    config: SessionConfig,
}

impl<F: FabricCaller + Clone> SessionDistiller<F> {
    pub fn new(fabric: F, config: SessionConfig) -> Self {
        Self { fabric, config }
    }
}

#[async_trait]
impl<F: FabricCaller + Clone> DistillExtractor for SessionDistiller<F> {
    fn id(&self) -> &'static str {
        ID
    }

    async fn distill(&self, inputs: DistillInputs<'_>) -> Result<Distilled> {
        let transcript = inputs.transcript;
        let meta = inputs.session_metadata;
        let export_truncated = meta.map(|m| m.body_truncated).unwrap_or(false);
        log::debug!(
            "SessionDistiller::distill: transcript_len={} token_cap={} export_truncated={} has_metadata={} session_ids={}",
            transcript.len(),
            self.config.token_cap,
            export_truncated,
            meta.is_some(),
            meta.map(|m| m.session_ids.len()).unwrap_or(0),
        );

        // Head+tail window to the token cap, then mark truncation so the model
        // never silently sees a partial transcript.
        let (windowed, windowed_truncated) = window_head_tail(transcript, self.config.token_cap);
        let marked = ensure_truncation_marker(windowed, export_truncated, windowed_truncated);
        let token_estimate = approx_tokens(marked.len());

        let (distilled, chunk_count) = if token_estimate <= SINGLE_CALL_TOKEN_THRESHOLD {
            (self.distill_short(&marked, inputs.capture_note).await?, 1usize)
        } else {
            let chunks = chunk_transcript(&marked, CHUNK_TOKEN_TARGET);
            let chunk_count = chunks.len().max(1);
            (self.distill_long(&marked, chunks).await?, chunk_count)
        };

        let mut bounded = enforce_bounds(distilled.take(), max_claims(chunk_count));
        debug_assert!(bounded.summary.chars().count() <= MAX_SUMMARY_CHARS);
        bounded.tags.iter_mut().for_each(|t| *t = t.to_lowercase());
        // Embedding policy: the staged body.txt is the archive; the note is
        // never a transcript carrier for sessions (never embedded).
        bounded.transcript = None;
        attach_session_payload(&mut bounded, meta);
        Ok(bounded)
    }
}

/// Small newtype so `distill_short`/`distill_long` can hand back a `Distilled`
/// the outer `distill` moves into `enforce_bounds` without a clone.
struct Built(Distilled);
impl Built {
    fn take(self) -> Distilled {
        self.0
    }
}

impl<F: FabricCaller + Clone> SessionDistiller<F> {
    /// Single-call path: one `distill-session` call over the windowed body.
    async fn distill_short(&self, body: &str, capture_note: Option<&str>) -> Result<Built> {
        if body.trim().is_empty() {
            return Ok(Built(fallback_distilled(
                ID,
                "empty-transcript",
                body,
                None,
                &self.config.model,
            )));
        }
        let input = compose_capture_input(body, capture_note);
        let raw = match self.call_fabric(PATTERN, &input).await {
            Ok(text) => text,
            Err(reason) => {
                return Ok(Built(fallback_distilled(ID, &reason, body, None, &self.config.model)));
            }
        };
        let yaml_body = strip_fences(&raw);
        let parsed: PatternYaml = match crate::parse::parse_pattern_yaml(yaml_body) {
            Ok(p) => p,
            Err(err) => {
                log::warn!("SessionDistiller: yaml parse failed: {err}; using fallback");
                return Ok(Built(fallback_distilled(
                    ID,
                    "yaml-parse-error",
                    body,
                    Some(yaml_body),
                    &self.config.model,
                )));
            }
        };
        let summary = parsed.summary.unwrap_or_default().trim().to_string();
        if summary.is_empty() {
            log::warn!("SessionDistiller: empty summary; using missing-summary fallback");
            return Ok(Built(fallback_distilled(
                ID,
                "missing-summary",
                body,
                Some(yaml_body),
                &self.config.model,
            )));
        }
        let slug = clean_slug(parsed.slug.as_deref());
        let claims = collect_claims(parsed.claims);
        let tags = collect_tags(parsed.tags);
        let links = collect_links(parsed.links);
        let input_tokens = approx_tokens(body.len()) as u32;
        let output_tokens = approx_tokens(raw.len()) as u32;
        Ok(Built(Distilled {
            summary,
            tldr: None,
            slug,
            enumeration: None,
            key_ideas: Vec::new(),
            claims,
            tags,
            links,
            kind_specific: None,
            meta: build_meta(
                &self.config.model,
                input_tokens,
                output_tokens,
                ValidationMeta::default(),
            ),
            transcript: None,
        }))
    }

    /// Map-reduce path for a windowed body still above the single-call
    /// threshold (only reachable when `token_cap` is configured above it).
    /// Chunks distill in parallel via `distill-session-chunk`; a single
    /// `distill-session-reduce` synthesizes the summary and SELECTS the final
    /// claims from the pooled chunk claims (mirrors video/thread).
    async fn distill_long(&self, body: &str, chunks: Vec<String>) -> Result<Built> {
        log::debug!(
            "SessionDistiller::distill_long: chunks={} threshold_tokens={} target_tokens={}",
            chunks.len(),
            SINGLE_CALL_TOKEN_THRESHOLD,
            CHUNK_TOKEN_TARGET
        );
        if chunks.is_empty() {
            return Ok(Built(fallback_distilled(
                ID,
                "empty-transcript",
                body,
                None,
                &self.config.model,
            )));
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
                    (idx, fabric.call(request).await)
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
                    log::warn!("SessionDistiller: chunk fabric call failed: {err}");
                    any_chunk_failed = true;
                    continue;
                }
            };
            output_chars += raw.len();
            let parsed: PatternYaml = match crate::parse::parse_pattern_yaml(strip_fences(&raw)) {
                Ok(p) => p,
                Err(err) => {
                    log::warn!("SessionDistiller: chunk yaml parse failed: {err}");
                    any_chunk_failed = true;
                    continue;
                }
            };
            if let Some(s) = parsed.summary.clone()
                && !s.trim().is_empty()
            {
                chunk_summaries.push(s.trim().to_string());
            }
            combined_claims.extend(collect_claims(parsed.claims));
            combined_links.extend(collect_links(parsed.links));
            combined_tags.extend(collect_tags(parsed.tags));
        }

        let mut seen_tags = std::collections::HashSet::new();
        combined_tags.retain(|t| seen_tags.insert(t.clone()));

        if chunk_summaries.is_empty() {
            log::warn!("SessionDistiller: all chunks failed; using chunk-failures fallback");
            return Ok(Built(fallback_distilled(
                ID,
                "chunk-failures",
                body,
                None,
                &self.config.model,
            )));
        }

        let joined = chunk_summaries.join("\n\n");
        let reduce_input = build_reduce_input(&chunk_summaries, &combined_claims, &[], None);
        let mut anchors_stripped: u32 = 0;
        let (summary, claims, reduce_selection_failed, slug) = match self
            .call_fabric(PATTERN_REDUCE, &reduce_input)
            .await
        {
            Ok(raw) => match crate::parse::parse_pattern_yaml::<ReduceYaml>(strip_fences(&raw)) {
                Ok(parsed) => {
                    let slug = clean_slug(parsed.slug.as_deref());
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
                        Some(selected) => (summary, selected, false, slug),
                        None => {
                            log::warn!("SessionDistiller: reduce selected no claims; falling back to chunk merge");
                            (summary, combined_claims.clone(), true, slug)
                        }
                    }
                }
                Err(err) => {
                    log::warn!("SessionDistiller: reduce yaml parse failed: {err}; falling back to concat + merge");
                    (joined.clone(), combined_claims.clone(), true, None)
                }
            },
            Err(reason) => {
                log::warn!("SessionDistiller: reduce fabric call failed ({reason}); falling back to concat + merge");
                (joined.clone(), combined_claims.clone(), true, None)
            }
        };

        let mut validation = ValidationMeta::default();
        if reduce_selection_failed {
            validation.fallback_reason = Some("reduce-selection-failed".to_string());
        } else if any_chunk_failed {
            validation.fallback_reason = Some("partial-chunk-failure".to_string());
        }
        validation.anchors_stripped = anchors_stripped;
        let input_tokens = approx_tokens(body.len()) as u32;
        let output_tokens = approx_tokens(output_chars) as u32;
        Ok(Built(Distilled {
            summary,
            tldr: None,
            slug,
            enumeration: None,
            key_ideas: Vec::new(),
            claims,
            tags: combined_tags,
            links: combined_links,
            kind_specific: None,
            meta: build_meta(&self.config.model, input_tokens, output_tokens, validation),
            transcript: None,
        }))
    }

    /// Map timeout vs error into a stable fallback reason (mirrors siblings).
    async fn call_fabric(&self, pattern: &str, input: &str) -> std::result::Result<String, String> {
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
                log::warn!("SessionDistiller::call_fabric: pattern={pattern} reason={reason} err={msg}");
                Err(reason)
            }
        }
    }
}

/// Build the `DistilledMeta` shared by both paths.
fn build_meta(model: &str, input_tokens: u32, output_tokens: u32, validation: ValidationMeta) -> DistilledMeta {
    DistilledMeta {
        extractor: ID.to_string(),
        model: if model.is_empty() { "default".to_string() } else { model.to_string() },
        input_tokens,
        output_tokens,
        produced_at: Utc::now().to_rfc3339(),
        validation,
    }
}

/// Normalize a distiller-emitted slug: trim, lowercase, and drop it when empty.
/// The pattern already asks for lowercase-kebab; this is defensive normalization
/// only. Filename-safety (illegal chars, length) is the publish path's job via
/// `hygiene::sanitize_filename` — this helper keeps the raw subject intact.
fn clean_slug(slug: Option<&str>) -> Option<String> {
    slug.map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty())
}

fn collect_claims(claims: Option<Vec<crate::parse::PatternClaim>>) -> Vec<Claim> {
    claims
        .unwrap_or_default()
        .into_iter()
        .map(|c| c.into_claim())
        .filter(|c| !c.text.is_empty())
        .collect()
}

fn collect_tags(tags: Option<Vec<String>>) -> Vec<String> {
    tags.unwrap_or_default()
        .into_iter()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .collect()
}

fn collect_links(links: Option<Vec<crate::parse::PatternLink>>) -> Vec<Link> {
    links
        .unwrap_or_default()
        .into_iter()
        .map(|l| Link {
            url: l.url.trim().to_string(),
            label: l.label.filter(|s| !s.is_empty()),
        })
        .filter(|l| !l.url.is_empty())
        .collect()
}

/// Attach `KindPayload::Session` from the deterministic Stage-0 metadata (even
/// on a fallback, so cortex/render always see the session bookkeeping - mirrors
/// the thread distiller attaching its platform on fallback). No-op when the
/// caller supplied no metadata (cortex backfill / a bare test).
fn attach_session_payload(distilled: &mut Distilled, meta: Option<&SessionMetadata>) {
    let Some(m) = meta else {
        return;
    };
    distilled.kind_specific = Some(KindPayload::Session(SessionPayload {
        repo: m.repo.clone(),
        session_ids: m.session_ids.clone(),
        msg_count: m.msg_count,
        date_start: m.date_start.clone(),
        date_end: m.date_end.clone(),
    }));
}

/// Head+tail window a transcript to `max_tokens` (approx). Returns the windowed
/// body and whether truncation happened. When truncation happens the dropped
/// middle is replaced by the `[TRANSCRIPT TRUNCATED]` marker so the head and
/// tail stay legible to the model. Char-based throughout so a multibyte
/// codepoint can never straddle a cut (the `string_slice` footgun).
fn window_head_tail(transcript: &str, max_tokens: usize) -> (String, bool) {
    let max_chars = max_tokens.saturating_mul(CHARS_PER_TOKEN);
    let total_chars = transcript.chars().count();
    if max_chars == 0 || total_chars <= max_chars {
        return (transcript.to_string(), false);
    }
    // Reserve room for the marker inside the budget so the windowed body stays
    // at or under max_chars.
    let marker_len = TRUNCATION_MARKER.chars().count() + 4; // surrounding blank lines
    let budget = max_chars.saturating_sub(marker_len).max(2);
    let head_chars = (budget * HEAD_BUDGET_NUM / HEAD_BUDGET_DEN).max(1);
    let tail_chars = budget.saturating_sub(head_chars);
    let head: String = transcript.chars().take(head_chars).collect();
    let tail: String = transcript
        .chars()
        .skip(total_chars.saturating_sub(tail_chars))
        .collect();
    log::debug!(
        "SessionDistiller::window_head_tail: total_chars={total_chars} max_chars={max_chars} head_chars={head_chars} tail_chars={tail_chars}"
    );
    (format!("{head}\n\n{TRUNCATION_MARKER}\n\n{tail}"), true)
}

/// Ensure the body carries the `[TRANSCRIPT TRUNCATED]` marker when the model
/// is not seeing the whole transcript. Windowing already inserted it in the
/// middle; export truncation (clyde cut the tail) appends it at the end.
fn ensure_truncation_marker(body: String, export_truncated: bool, windowed_truncated: bool) -> String {
    if windowed_truncated || !export_truncated || body.contains(TRUNCATION_MARKER) {
        return body;
    }
    format!("{body}\n\n{TRUNCATION_MARKER}\n")
}

#[cfg(test)]
mod tests;
