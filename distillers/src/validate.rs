//! Validation rules every distiller's output runs through before publication.
//!
//! The pipeline never gates on validation: degraded `Distilled`s always
//! publish so the user can see something in the vault and the staged
//! artifact preserves enough breadcrumbs for replay.

use chrono::Utc;
use vault::distilled::{Distilled, DistilledMeta, ValidationMeta};

/// Maximum summary length before sentence-boundary truncation.
pub const MAX_SUMMARY_CHARS: usize = 2000;
/// Hard cap on tags. Canonical-tag filtering happens upstream.
pub const MAX_TAGS: usize = 7;

/// Base claim budget for a single-call (unchunked) distillation.
const CLAIMS_BASE: usize = 10;
/// Additional claims allowed per chunk beyond the first.
const CLAIMS_PER_CHUNK: usize = 2;
/// Hard ceiling on the claim budget regardless of chunk count. 24 narrow
/// claims already risks exceeding bge-small's 512-token window at embed time.
const CLAIMS_CEILING: usize = 24;

/// Size-aware claim budget: base 10, +2 per chunk beyond the first, hard
/// ceiling 24. Single-call kinds pass `chunk_count = 1` (cap stays 10);
/// chunked kinds (video / voicenote map-reduce) pass their real chunk count so
/// a long source keeps proportionally more claims. Replaces the former flat
/// `MAX_CLAIMS` constant.
pub fn max_claims(chunk_count: usize) -> usize {
    let extra = chunk_count.saturating_sub(1) * CLAIMS_PER_CHUNK;
    (CLAIMS_BASE + extra).min(CLAIMS_CEILING)
}

/// Apply bounds and per-kind anchor validation to a freshly parsed Distilled.
///
/// `max_claims` is the claim cap the caller computed for this distillation
/// (via [`max_claims`] with the appropriate chunk count) — `enforce_bounds`
/// cannot know the chunk count itself.
///
/// Mutates the payload in place and records truncation tags into
/// `meta.validation`. Returns the mutated payload for chaining.
pub fn enforce_bounds(mut distilled: Distilled, max_claims: usize) -> Distilled {
    if distilled.claims.len() > max_claims {
        let original = distilled.claims.len();
        distilled.claims.truncate(max_claims);
        distilled
            .meta
            .validation
            .bounds_truncations
            .push(format!("claims:{original}>{max_claims}"));
    }
    if distilled.tags.len() > MAX_TAGS {
        let original = distilled.tags.len();
        distilled.tags.truncate(MAX_TAGS);
        distilled
            .meta
            .validation
            .bounds_truncations
            .push(format!("tags:{original}>{MAX_TAGS}"));
    }
    if distilled.summary.chars().count() > MAX_SUMMARY_CHARS {
        let original = distilled.summary.chars().count();
        distilled.summary = truncate_at_sentence_boundary(&distilled.summary, MAX_SUMMARY_CHARS);
        distilled
            .meta
            .validation
            .bounds_truncations
            .push(format!("summary:{original}>{MAX_SUMMARY_CHARS}"));
    }
    distilled
}

/// Truncate at the latest sentence-ending punctuation within `max_chars`,
/// falling back to a hard char-boundary cut when no punctuation is found.
fn truncate_at_sentence_boundary(text: &str, max_chars: usize) -> String {
    let head: String = text.chars().take(max_chars).collect();
    let cutoff = head
        .char_indices()
        .rev()
        .find(|(_, c)| matches!(c, '.' | '!' | '?'))
        .map(|(idx, c)| idx + c.len_utf8());
    match cutoff {
        Some(end) => head[..end].trim_end().to_string(),
        None => head,
    }
}

/// Construct a fallback `Distilled` for distillers whose Fabric call failed
/// or produced unparseable output. The summary leads with the fallback tag
/// followed by a short snippet of the transcript so the user has something
/// to read while triaging. The full transcript is preserved in
/// `Distilled.transcript` so the renderer can emit it under `## Transcript`
/// and no user content is silently lost on hard-failure publishes.
///
/// This used to default `transcript = None` (with video/voicenote distillers
/// post-processing to override). That asymmetry caused real data loss during
/// the 2026-05-18 cortex backfill: article and repo distillers hit
/// `yaml-parse-error` for short or pattern-mismatched inputs, and the full
/// legacy body was replaced by the 280-char snippet alone. Untracked notes
/// could not be recovered. Preserving the full transcript universally
/// removes the gap; the only cost is slightly larger note files on
/// fresh-ingest failures (an acceptable trade vs. silent data loss).
pub fn fallback_distilled(
    extractor: &str,
    reason: &str,
    transcript_snippet: &str,
    raw_output: Option<&str>,
    model: &str,
) -> Distilled {
    let snippet: String = transcript_snippet.chars().take(280).collect();
    let summary = format!("[{reason}]\n\n{snippet}");
    let transcript = if transcript_snippet.is_empty() {
        None
    } else {
        Some(transcript_snippet.to_string())
    };
    Distilled {
        summary,
        tldr: None,
        enumeration: None,
        key_ideas: Vec::new(),
        claims: Vec::new(),
        tags: Vec::new(),
        links: Vec::new(),
        kind_specific: None,
        meta: DistilledMeta {
            extractor: extractor.to_string(),
            // The real model, NOT the failure reason (which lives in
            // validation.fallback_reason below). Empty → "default".
            model: if model.is_empty() { "default".to_string() } else { model.to_string() },
            input_tokens: 0,
            output_tokens: 0,
            produced_at: Utc::now().to_rfc3339(),
            validation: ValidationMeta {
                fallback_reason: Some(reason.to_string()),
                bounds_truncations: Vec::new(),
                anchors_stripped: 0,
                raw_output: raw_output.map(|s| s.to_string()),
            },
        },
        transcript,
    }
}

#[cfg(test)]
mod tests;
