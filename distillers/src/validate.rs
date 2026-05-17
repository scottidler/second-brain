//! Validation rules every distiller's output runs through before publication.
//!
//! The pipeline never gates on validation: degraded `Distilled`s always
//! publish so the user can see something in the vault and the staged
//! artifact preserves enough breadcrumbs for replay.

use chrono::Utc;
use vault::distilled::{Distilled, DistilledMeta, ValidationMeta};

/// Maximum summary length before sentence-boundary truncation.
pub const MAX_SUMMARY_CHARS: usize = 2000;
/// Hard cap on the number of claims any distiller may publish.
pub const MAX_CLAIMS: usize = 10;
/// Hard cap on tags. Canonical-tag filtering happens upstream.
pub const MAX_TAGS: usize = 7;

/// Apply bounds and per-kind anchor validation to a freshly parsed Distilled.
///
/// Mutates the payload in place and records truncation tags into
/// `meta.validation`. Returns the mutated payload for chaining.
pub fn enforce_bounds(mut distilled: Distilled) -> Distilled {
    if distilled.claims.len() > MAX_CLAIMS {
        let original = distilled.claims.len();
        distilled.claims.truncate(MAX_CLAIMS);
        distilled
            .meta
            .validation
            .bounds_truncations
            .push(format!("claims:{original}>{MAX_CLAIMS}"));
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
/// to read while triaging.
pub fn fallback_distilled(
    extractor: &str,
    reason: &str,
    transcript_snippet: &str,
    raw_output: Option<&str>,
) -> Distilled {
    let snippet: String = transcript_snippet.chars().take(280).collect();
    let summary = format!("[{reason}]\n\n{snippet}");
    Distilled {
        summary,
        claims: Vec::new(),
        tags: Vec::new(),
        links: Vec::new(),
        kind_specific: None,
        meta: DistilledMeta {
            extractor: extractor.to_string(),
            model: reason.to_string(),
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
        // fallback_distilled defaults to None. Non-URL callers (Image,
        // VoiceNote, Idea, Vocab) post-process to set transcript = Some
        // when they want verbatim preservation; URL callers leave it None.
        transcript: None,
    }
}

#[cfg(test)]
mod tests;
