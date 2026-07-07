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

/// Maximum enumerated items kept before truncation. A genuine listicle rarely
/// exceeds a "Top 25"; anything past this is either LLM runaway or the section
/// being mistaken for the whole transcript. Bounds the `## Enumerated Points`
/// section so it can never approach the note-size ceiling (Phase 3 gate).
pub const MAX_ENUMERATION_ITEMS: usize = 30;
/// Per-item length cap (name + text combined) before that item's `text` is
/// truncated at a sentence boundary. Each item is meant to be one line.
pub const MAX_ENUM_ITEM_CHARS: usize = 400;
/// Maximum thematic key-idea bullets kept before truncation (April rule: 5-7,
/// with headroom for a rich source).
pub const MAX_KEY_IDEAS: usize = 10;
/// Per-key-idea length cap before sentence-boundary truncation.
pub const MAX_KEY_IDEA_CHARS: usize = 400;
/// The `> [!tldr]` callout is a single hook sentence; cap it so a runaway model
/// can't smuggle paragraphs into the callout.
pub const MAX_TLDR_CHARS: usize = 400;

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
    enforce_tldr_bound(&mut distilled);
    enforce_enumeration_bounds(&mut distilled);
    enforce_key_idea_bounds(&mut distilled);
    distilled
}

/// Cap the `tldr` callout hook (Phase 4). A one-sentence hook that runs long is
/// truncated at a sentence boundary; the truncation is recorded so an operator
/// sees the model produced an over-length tldr.
fn enforce_tldr_bound(distilled: &mut Distilled) {
    let Some(tldr) = distilled.tldr.as_ref() else {
        return;
    };
    let original = tldr.chars().count();
    if original > MAX_TLDR_CHARS {
        let truncated = truncate_at_sentence_boundary(tldr, MAX_TLDR_CHARS);
        distilled.tldr = Some(truncated);
        distilled
            .meta
            .validation
            .bounds_truncations
            .push(format!("tldr:{original}>{MAX_TLDR_CHARS}"));
    }
}

/// Cap the enumeration item count and each item's combined length (Phase 4).
/// The item count is capped at [`MAX_ENUMERATION_ITEMS`]; per-item overflow
/// truncates the item's `text` (never its `name`) at a sentence boundary. A
/// count cap that trips is recorded as `enumeration-items:N>MAX`; a per-item
/// text cut as `enum-item-text:idx:orig>MAX`.
fn enforce_enumeration_bounds(distilled: &mut Distilled) {
    let Some(enumeration) = distilled.enumeration.as_mut() else {
        return;
    };
    if enumeration.items.len() > MAX_ENUMERATION_ITEMS {
        let original = enumeration.items.len();
        enumeration.items.truncate(MAX_ENUMERATION_ITEMS);
        distilled
            .meta
            .validation
            .bounds_truncations
            .push(format!("enumeration-items:{original}>{MAX_ENUMERATION_ITEMS}"));
    }
    let mut item_cuts: Vec<String> = Vec::new();
    for (idx, item) in enumeration.items.iter_mut().enumerate() {
        let combined = item.name.chars().count() + item.text.chars().count();
        if combined > MAX_ENUM_ITEM_CHARS {
            // The name is a short title; keep it whole and trim the text so the
            // combined length fits, never cutting below zero.
            let text_budget = MAX_ENUM_ITEM_CHARS.saturating_sub(item.name.chars().count());
            let original = item.text.chars().count();
            item.text = truncate_at_sentence_boundary(&item.text, text_budget);
            item_cuts.push(format!("enum-item-text:{idx}:{original}>{text_budget}"));
        }
    }
    distilled.meta.validation.bounds_truncations.extend(item_cuts);
}

/// Cap the key-idea bullet count and each bullet's length (Phase 4).
fn enforce_key_idea_bounds(distilled: &mut Distilled) {
    if distilled.key_ideas.len() > MAX_KEY_IDEAS {
        let original = distilled.key_ideas.len();
        distilled.key_ideas.truncate(MAX_KEY_IDEAS);
        distilled
            .meta
            .validation
            .bounds_truncations
            .push(format!("key-ideas:{original}>{MAX_KEY_IDEAS}"));
    }
    let mut idea_cuts: Vec<String> = Vec::new();
    for (idx, idea) in distilled.key_ideas.iter_mut().enumerate() {
        let original = idea.chars().count();
        if original > MAX_KEY_IDEA_CHARS {
            *idea = truncate_at_sentence_boundary(idea, MAX_KEY_IDEA_CHARS);
            idea_cuts.push(format!("key-idea:{idx}:{original}>{MAX_KEY_IDEA_CHARS}"));
        }
    }
    distilled.meta.validation.bounds_truncations.extend(idea_cuts);
}

/// Mark an enumeration shortfall (Resolved Decision 2026-07-07): when the source
/// declared N items (`enumeration.declared_count == Some(n)`) but the distiller
/// recovered fewer (`items.len() < n`), the note still publishes but the receipt
/// is marked degraded so the miss surfaces via `sb doctor` (`degraded_24h`) and
/// `sb borg log --degraded`. A shortfall is NOT a fallback, so this sets its own
/// `validation.enumeration_shortfall` flag rather than reusing `fallback_reason`.
/// Idempotent; call after the enumeration is finalized (post-`enforce_bounds`,
/// so the count cap cannot itself manufacture a false shortfall — the cap only
/// trims a count ABOVE `declared_count`, never below it).
pub fn mark_enumeration_shortfall(distilled: &mut Distilled) {
    let Some(enumeration) = distilled.enumeration.as_ref() else {
        return;
    };
    let Some(declared) = enumeration.declared_count else {
        return;
    };
    let recovered = enumeration.items.len() as u32;
    if recovered < declared {
        log::warn!(
            "enumeration shortfall: declared_count={declared} recovered={recovered} \
             (publishing degraded; enumeration items fell short of the declared total)"
        );
        distilled.meta.validation.enumeration_shortfall = true;
    }
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
                enumeration_shortfall: false,
                raw_output: raw_output.map(|s| s.to_string()),
            },
        },
        transcript,
    }
}

#[cfg(test)]
mod tests;
