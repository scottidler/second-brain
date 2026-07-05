use super::*;
use vault::distilled::{Claim, ClaimKind};

#[test]
fn strips_yaml_fence() {
    let raw = "```yaml\nsummary: hi\n```";
    assert_eq!(strip_fences(raw), "summary: hi");
}

#[test]
fn strips_bare_fence() {
    let raw = "```\nsummary: hi\n```";
    assert_eq!(strip_fences(raw), "summary: hi");
}

#[test]
fn passes_through_unfenced() {
    let raw = "summary: hi\nclaims: []";
    assert_eq!(strip_fences(raw), "summary: hi\nclaims: []");
}

#[test]
fn unfenced_with_embedded_fence_is_not_truncated() {
    // Regression for the truncation bug: unfenced YAML whose content contains
    // an embedded code fence must NOT be cut at that fence.
    let raw = "summary: see the snippet\nclaims:\n  - text: \"```rust let x = 1; ```\"";
    let out = strip_fences(raw);
    assert!(
        out.contains("let x = 1;"),
        "embedded fence content was truncated: {out:?}"
    );
}

#[test]
fn fenced_with_trailing_prose_strips_to_close() {
    let raw = "```yaml\nsummary: hi\n```\nignored trailing";
    assert_eq!(strip_fences(raw), "summary: hi");
}

#[test]
fn strips_fence_with_surrounding_whitespace() {
    // Leading/trailing whitespace around the fence (LLMs add blank lines) is
    // trimmed before the fence is detected.
    let raw = "\n\n  ```yaml\nsummary: hi\n```  \n\n";
    assert_eq!(strip_fences(raw), "summary: hi");
}

#[test]
fn strips_fence_with_multiline_yaml_body() {
    // A multi-line body inside the fence is returned intact (only the fence
    // markers are removed), so every consumer's serde_yaml parse sees clean
    // YAML.
    let raw = "```yaml\nsummary: hi\nclaims:\n  - text: one\n  - text: two\n```";
    assert_eq!(strip_fences(raw), "summary: hi\nclaims:\n  - text: one\n  - text: two");
}

#[test]
fn preserves_colons_inside_unfenced_body() {
    // Colons inside values must survive untouched (no fence present).
    let raw = "summary: \"ratio is 3:1 at 12:00\"";
    assert_eq!(strip_fences(raw), "summary: \"ratio is 3:1 at 12:00\"");
}

#[test]
fn approx_tokens_uses_four_char_rule() {
    assert_eq!(approx_tokens(0), 0);
    assert_eq!(approx_tokens(4), 1);
    assert_eq!(approx_tokens(401), 100);
}

fn claim(text: &str, anchor: Option<&str>) -> Claim {
    Claim {
        text: text.to_string(),
        anchor: anchor.map(|s| s.to_string()),
        ..Default::default()
    }
}

fn pattern_claim(text: &str, anchor: Option<&str>) -> PatternClaim {
    PatternClaim {
        text: text.to_string(),
        anchor: anchor.map(|s| s.to_string()),
        kind: ClaimKind::default(),
        who: None,
        quote: None,
    }
}

#[test]
fn build_reduce_input_has_two_labeled_sections_with_anchor_prefixed_pool() {
    let summaries = vec!["First chunk summary.".to_string(), "Second chunk summary.".to_string()];
    let pool = vec![
        claim("An anchored claim.", Some("00:00:05")),
        claim("A claim without an anchor.", None),
    ];
    let input = build_reduce_input(&summaries, &pool);

    assert!(input.contains("## Chunk Summaries"));
    assert!(input.contains("## Claim Pool"));
    assert!(input.contains("First chunk summary.\n\nSecond chunk summary."));
    assert!(
        input.contains("[00:00:05] An anchored claim."),
        "anchored pool line: {input:?}"
    );
    assert!(
        input.contains("A claim without an anchor."),
        "anchorless pool line: {input:?}"
    );
    // The summaries section precedes the claim pool section.
    let summaries_at = input.find("## Chunk Summaries").expect("summaries heading present");
    let pool_at = input.find("## Claim Pool").expect("pool heading present");
    assert!(summaries_at < pool_at);
}

#[test]
fn build_reduce_input_normalizes_bracketed_pool_anchor() {
    // A pool claim whose anchor already carries brackets is not double-bracketed.
    let pool = vec![claim("Bracketed anchor claim.", Some("[00:01:00]"))];
    let input = build_reduce_input(&[], &pool);
    assert!(input.contains("[00:01:00] Bracketed anchor claim."), "{input:?}");
    assert!(!input.contains("[[00:01:00]]"));
}

#[test]
fn select_reduce_claims_keeps_pool_matching_anchor() {
    let pool = vec![claim("Pooled.", Some("00:25:00"))];
    let mut stripped = 0;
    let selected = select_reduce_claims(
        vec![pattern_claim("Selected late claim.", Some("00:25:00"))],
        &pool,
        &mut stripped,
    )
    .expect("non-empty selection");
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].anchor.as_deref(), Some("00:25:00"));
    assert_eq!(stripped, 0);
}

#[test]
fn select_reduce_claims_matches_across_bracket_normalization() {
    // Pool anchor bare, selected anchor bracketed — still a match.
    let pool = vec![claim("Pooled.", Some("00:25:00"))];
    let mut stripped = 0;
    let selected = select_reduce_claims(
        vec![pattern_claim("Selected.", Some("[00:25:00]"))],
        &pool,
        &mut stripped,
    )
    .expect("non-empty selection");
    assert_eq!(
        selected[0].anchor.as_deref(),
        Some("00:25:00"),
        "normalized to bracket-free form"
    );
    assert_eq!(stripped, 0);
}

#[test]
fn select_reduce_claims_strips_non_pool_anchor_and_counts() {
    let pool = vec![claim("Pooled.", Some("00:00:05"))];
    let mut stripped = 0;
    let selected = select_reduce_claims(
        vec![pattern_claim("Invented-anchor claim.", Some("09:09:09"))],
        &pool,
        &mut stripped,
    )
    .expect("non-empty selection");
    assert_eq!(selected.len(), 1, "claim text retained");
    assert!(selected[0].anchor.is_none(), "invented anchor stripped");
    assert_eq!(selected[0].text, "Invented-anchor claim.");
    assert_eq!(stripped, 1);
}

#[test]
fn select_reduce_claims_accepts_anchorless_synthesis() {
    // No anchor → accepted as a synthesis, no text-match gate against the pool.
    let pool = vec![
        claim("Pooled one.", Some("00:00:05")),
        claim("Pooled two.", Some("00:10:00")),
    ];
    let mut stripped = 0;
    let selected = select_reduce_claims(
        vec![pattern_claim("A brand-new synthesis spanning two pool claims.", None)],
        &pool,
        &mut stripped,
    )
    .expect("non-empty selection");
    assert_eq!(selected.len(), 1);
    assert!(selected[0].anchor.is_none());
    assert_eq!(
        stripped, 0,
        "an anchorless synthesis is not counted as a stripped anchor"
    );
}

#[test]
fn select_reduce_claims_empty_returns_none() {
    let pool = vec![claim("Pooled.", Some("00:00:05"))];
    let mut stripped = 0;
    assert!(select_reduce_claims(vec![], &pool, &mut stripped).is_none());
    // A claim with only whitespace text is skipped, yielding an empty selection.
    assert!(select_reduce_claims(vec![pattern_claim("   ", None)], &pool, &mut stripped).is_none());
    assert_eq!(stripped, 0);
}
