use super::*;

#[test]
fn flatten_wikilinks_renders_display_and_target() {
    assert_eq!(flatten_wikilinks("see [[claude|Claude]] now"), "see Claude now");
    assert_eq!(flatten_wikilinks("uses [[mcp]] here"), "uses mcp here");
    assert_eq!(flatten_wikilinks("no links at all"), "no links at all");
    assert_eq!(flatten_wikilinks("[[a|A]] and [[b]]"), "A and b");
}

#[test]
fn flatten_wikilinks_handles_unclosed_brackets() {
    assert_eq!(flatten_wikilinks("[[oops unclosed"), "[[oops unclosed");
}

#[test]
fn prepare_note_text_prefers_summary_without_truncation_flag() {
    let (text, truncated) = prepare_note_text("short summary", "very long body....", 5);
    assert!(text.starts_with("short"));
    assert!(!truncated, "summary path is never a truncation hazard");
}

#[test]
fn prepare_note_text_falls_back_to_body_and_flags_truncation() {
    let body = "abcdefghij"; // 10 chars
    let (text, truncated) = prepare_note_text("", body, 4);
    assert_eq!(text, "abcd");
    assert!(truncated);
}

#[test]
fn prepare_note_text_body_under_budget_not_truncated() {
    let (text, truncated) = prepare_note_text("", "abc", 10);
    assert_eq!(text, "abc");
    assert!(!truncated);
}

#[test]
fn cohens_kappa_perfect_agreement_is_one() {
    let pairs = vec![(3, 3), (0, 0), (2, 2), (1, 1)];
    assert!((cohens_kappa(&pairs) - 1.0).abs() < 1e-9);
}

#[test]
fn cohens_kappa_below_one_on_disagreement() {
    let pairs = vec![(3, 0), (0, 3), (2, 1), (1, 2)];
    assert!(cohens_kappa(&pairs) < 0.5);
}

#[test]
fn boundary_precision_recall_perfect() {
    // human hits: a(3),c(2) ; judge hits: same
    let pairs = vec![(3, 3), (0, 0), (2, 2), (1, 1)];
    let (p, r) = boundary_precision_recall(&pairs, 2);
    assert!((p - 1.0).abs() < 1e-9 && (r - 1.0).abs() < 1e-9);
}

#[test]
fn boundary_precision_recall_partial() {
    // human hits (>=2): items 0,1,2 (3 hits). judge calls hit on item0 only, plus
    // a false positive on item3.
    // pairs: (h,j)
    let pairs = vec![(3, 3), (2, 1), (2, 0), (0, 2)];
    let (p, r) = boundary_precision_recall(&pairs, 2);
    // judge_pos = items where j>=2: item0 (3), item3 (2) -> 2 ; tp = item0 -> 1
    // precision = 1/2 = 0.5
    // human_pos = items where h>=2: item0,1,2 -> 3 ; recall = 1/3
    assert!((p - 0.5).abs() < 1e-9, "precision {p}");
    assert!((r - 1.0 / 3.0).abs() < 1e-9, "recall {r}");
}
