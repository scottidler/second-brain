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
    let claims = (0..MAX_CLAIMS + 3)
        .map(|i| Claim {
            text: format!("claim {i}"),
            anchor: None,
        })
        .collect();
    let distilled = Distilled {
        summary: "ok".to_string(),
        claims,
        tags: Vec::new(),
        links: Vec::new(),
        kind_specific: None,
        meta: meta("distill-article-v1"),
        transcript: None,
    };

    let bounded = enforce_bounds(distilled);
    assert_eq!(bounded.claims.len(), MAX_CLAIMS);
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
        claims: Vec::new(),
        tags,
        links: Vec::new(),
        kind_specific: None,
        meta: meta("distill-article-v1"),
        transcript: None,
    };

    let bounded = enforce_bounds(distilled);
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
        claims: Vec::new(),
        tags: Vec::new(),
        links: Vec::new(),
        kind_specific: None,
        meta: meta("distill-article-v1"),
        transcript: None,
    };

    let bounded = enforce_bounds(distilled);
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
        claims: vec![Claim {
            text: "one".to_string(),
            anchor: None,
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

    let bounded = enforce_bounds(distilled);
    assert!(bounded.meta.validation.bounds_truncations.is_empty());
    assert_eq!(bounded.claims.len(), 1);
    assert_eq!(bounded.tags.len(), 1);
    assert_eq!(bounded.links.len(), 1);
}

#[test]
fn fallback_distilled_records_reason_and_raw_output() {
    let fb = fallback_distilled(
        "distill-article-v1",
        "fabric-timeout",
        "snippet of the transcript",
        Some("raw fabric stdout"),
    );
    assert!(fb.summary.starts_with("[fabric-timeout]"));
    assert_eq!(fb.meta.model, "fabric-timeout");
    assert_eq!(fb.meta.validation.fallback_reason.as_deref(), Some("fabric-timeout"));
    assert_eq!(fb.meta.validation.raw_output.as_deref(), Some("raw fabric stdout"));
    assert!(fb.claims.is_empty());
}
