use super::*;
use vault::distilled::{Claim, Link};

fn meta(extractor: &str) -> DistilledMeta {
    DistilledMeta {
        extractor: extractor.to_string(),
        model: "test".to_string(),
        input_tokens: 0,
        output_tokens: 0,
        produced_at: "2026-05-16T14:03:22Z".to_string(),
        validation: ValidationMeta::default(),
    }
}

#[test]
fn enforce_bounds_truncates_excess_claims() {
    let cap = max_claims(1);
    let claims = (0..cap + 3)
        .map(|i| Claim {
            text: format!("claim {i}"),
            anchor: None,
            ..Default::default()
        })
        .collect();
    let distilled = Distilled {
        summary: "ok".to_string(),
        tldr: None,
        enumeration: None,
        key_ideas: Vec::new(),
        claims,
        tags: Vec::new(),
        links: Vec::new(),
        kind_specific: None,
        meta: meta("distill-article-v1"),
        transcript: None,
    };

    let bounded = enforce_bounds(distilled, cap);
    assert_eq!(bounded.claims.len(), cap);
    assert!(
        bounded
            .meta
            .validation
            .bounds_truncations
            .iter()
            .any(|t| t.starts_with("claims:"))
    );
}

#[test]
fn enforce_bounds_truncates_excess_tags() {
    let tags = (0..MAX_TAGS + 2).map(|i| format!("tag{i}")).collect();
    let distilled = Distilled {
        summary: "ok".to_string(),
        tldr: None,
        enumeration: None,
        key_ideas: Vec::new(),
        claims: Vec::new(),
        tags,
        links: Vec::new(),
        kind_specific: None,
        meta: meta("distill-article-v1"),
        transcript: None,
    };

    let bounded = enforce_bounds(distilled, max_claims(1));
    assert_eq!(bounded.tags.len(), MAX_TAGS);
    assert!(
        bounded
            .meta
            .validation
            .bounds_truncations
            .iter()
            .any(|t| t.starts_with("tags:"))
    );
}

#[test]
fn enforce_bounds_truncates_summary_at_sentence() {
    let mut summary = String::new();
    for _ in 0..400 {
        summary.push_str("Sentence here. ");
    }
    let distilled = Distilled {
        summary: summary.clone(),
        tldr: None,
        enumeration: None,
        key_ideas: Vec::new(),
        claims: Vec::new(),
        tags: Vec::new(),
        links: Vec::new(),
        kind_specific: None,
        meta: meta("distill-article-v1"),
        transcript: None,
    };

    let bounded = enforce_bounds(distilled, max_claims(1));
    assert!(bounded.summary.chars().count() <= MAX_SUMMARY_CHARS);
    assert!(
        bounded.summary.ends_with('.'),
        "summary should end on sentence boundary; got {:?}",
        &bounded.summary[bounded.summary.len().saturating_sub(20)..]
    );
}

#[test]
fn enforce_bounds_leaves_within_limit_payload_untouched() {
    let distilled = Distilled {
        summary: "short".to_string(),
        tldr: None,
        enumeration: None,
        key_ideas: Vec::new(),
        claims: vec![Claim {
            text: "one".to_string(),
            anchor: None,
            ..Default::default()
        }],
        tags: vec!["rust".to_string()],
        links: vec![Link {
            url: "https://example.com".to_string(),
            label: None,
        }],
        kind_specific: None,
        meta: meta("distill-article-v1"),
        transcript: None,
    };

    let bounded = enforce_bounds(distilled, max_claims(1));
    assert!(bounded.meta.validation.bounds_truncations.is_empty());
    assert_eq!(bounded.claims.len(), 1);
    assert_eq!(bounded.tags.len(), 1);
    assert_eq!(bounded.links.len(), 1);
}

#[test]
fn max_claims_scales_with_chunk_count_and_ceilings_at_24() {
    // Single-call kinds pass chunk_count = 1; cap stays 10 (preserves the old
    // flat MAX_CLAIMS behavior).
    assert_eq!(max_claims(1), 10);
    // chunk_count = 0 is defensive: saturating_sub keeps it at the base.
    assert_eq!(max_claims(0), 10);
    // +2 per chunk beyond the first.
    assert_eq!(max_claims(2), 12);
    assert_eq!(max_claims(3), 14);
    assert_eq!(max_claims(7), 22);
    // Hard ceiling at 24 (reached at 8 chunks) and held beyond.
    assert_eq!(max_claims(8), 24);
    assert_eq!(max_claims(20), 24);
    assert_eq!(max_claims(1000), 24);
}

#[test]
fn fallback_distilled_records_reason_and_raw_output() {
    let fb = fallback_distilled(
        "distill-article-v1",
        "fabric-timeout",
        "snippet of the transcript",
        Some("raw fabric stdout"),
        "claude-sonnet-test",
    );
    assert!(fb.summary.starts_with("[fabric-timeout]"));
    // model is the REAL model, NOT the failure reason (which lives in
    // validation.fallback_reason).
    assert_eq!(fb.meta.model, "claude-sonnet-test");
    assert_eq!(fb.meta.validation.fallback_reason.as_deref(), Some("fabric-timeout"));
    assert_eq!(fb.meta.validation.raw_output.as_deref(), Some("raw fabric stdout"));
    assert!(fb.claims.is_empty());
}

#[test]
fn fallback_distilled_preserves_transcript_so_no_user_content_is_lost() {
    // Universal preservation: on any hard failure, the full input transcript
    // survives in Distilled.transcript so render emits `## Transcript\n\n<body>`
    // and the user retains their content. Previously only video/voicenote
    // distillers post-processed to set this; article/repo/thread fallbacks
    // left it None, which caused real data loss during the 2026-05-18 cortex
    // backfill (2 untracked github notes overwritten with just a 280-char
    // snippet and no recovery path).
    let original = "The full legacy note body with multiple paragraphs.\n\nSecond paragraph contains more detail that must survive on yaml-parse-error.";
    let fb = fallback_distilled("distill-article-v1", "yaml-parse-error", original, None, "model-x");
    assert_eq!(fb.transcript.as_deref(), Some(original));
    // Summary still leads with the sentinel + 280-char snippet for triage.
    assert!(fb.summary.starts_with("[yaml-parse-error]"));
}

#[test]
fn fallback_distilled_empty_transcript_stays_none() {
    // Empty input -> no transcript to preserve; render skips the `## Transcript`
    // section entirely. Prevents a stray empty-headed block on truly empty inputs.
    let fb = fallback_distilled("distill-article-v1", "empty-transcript", "", None, "model-x");
    assert_eq!(fb.transcript, None);
}
