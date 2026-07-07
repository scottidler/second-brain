use super::*;
use vault::distilled::EnumeratedItem;

/// Real committed fixtures, located the same way `eval::tests` locates them
/// (`CARGO_MANIFEST_DIR`-relative, independent of the process CWD).
fn fixtures_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../config/eval/distill-fixtures")
}

fn item(name: &str, anchor: Option<&str>) -> EnumeratedItem {
    EnumeratedItem {
        name: name.to_string(),
        text: format!("{name} description"),
        anchor: anchor.map(str::to_string),
    }
}

#[test]
fn perfect_agreement_gives_kappa_one() {
    let pairs = [(3u8, 3u8), (2, 2), (0, 0), (1, 1)];
    assert!((cohens_kappa(&pairs) - 1.0).abs() < 1e-9);
}

#[test]
fn kappa_zero_for_too_few_pairs() {
    assert_eq!(cohens_kappa(&[(3, 3)]), 0.0);
    assert_eq!(cohens_kappa(&[]), 0.0);
}

#[test]
fn boundary_precision_recall_perfect() {
    // judge exactly matches human at the >=2 hit boundary
    let pairs = [(3u8, 3u8), (2, 2), (1, 1), (0, 0)];
    let (p, r) = boundary_precision_recall(&pairs, 2);
    assert!((p - 1.0).abs() < 1e-9);
    assert!((r - 1.0).abs() < 1e-9);
}

#[test]
fn boundary_recall_penalizes_missed_hits() {
    // human calls two hits (3,2); judge only catches one -> recall 0.5
    let pairs = [(3u8, 3u8), (2, 1)];
    let (p, r) = boundary_precision_recall(&pairs, 2);
    assert!((p - 1.0).abs() < 1e-9, "judge's one positive is a true positive");
    assert!((r - 0.5).abs() < 1e-9);
}

#[test]
fn degenerate_denominators_are_vacuously_one() {
    // no human or judge hits at all -> both 1.0 (vacuous)
    let pairs = [(0u8, 0u8), (1, 1)];
    let (p, r) = boundary_precision_recall(&pairs, 2);
    assert!((p - 1.0).abs() < 1e-9);
    assert!((r - 1.0).abs() < 1e-9);
}

// --- listicle-survival ---------------------------------------------------

#[test]
fn listicle_survival_scores_zero_when_enumeration_is_missing_entirely() {
    // The exact regression this metric exists to catch: a "Top N" source
    // whose distilled artifact carries no enumeration at all.
    assert_eq!(listicle_survival(None), 0.0);
}

#[test]
fn listicle_survival_scores_zero_when_declared_count_is_absent() {
    let enumeration = Enumeration {
        lead_in: None,
        declared_count: None,
        items: vec![item("A", None), item("B", None)],
    };
    assert_eq!(listicle_survival(Some(&enumeration)), 0.0);
}

#[test]
fn listicle_survival_full_marks_when_all_declared_items_present() {
    let enumeration = Enumeration {
        lead_in: Some("lead-in".to_string()),
        declared_count: Some(10),
        items: (1..=10).map(|n| item(&format!("item {n}"), Some("00:00:01"))).collect(),
    };
    assert_eq!(listicle_survival(Some(&enumeration)), 1.0);
}

#[test]
fn listicle_survival_gives_partial_credit_on_shortfall() {
    let enumeration = Enumeration {
        lead_in: None,
        declared_count: Some(10),
        items: (1..=7).map(|n| item(&format!("item {n}"), None)).collect(),
    };
    assert!((listicle_survival(Some(&enumeration)) - 0.7).abs() < 1e-9);
}

#[test]
fn listicle_survival_clamps_an_over_count_to_full_marks() {
    // Over-delivering the declared count is not the failure mode this design
    // cares about (the LLM listed 11 for a declared 10) - still full marks.
    let enumeration = Enumeration {
        lead_in: None,
        declared_count: Some(10),
        items: (1..=11).map(|n| item(&format!("item {n}"), None)).collect(),
    };
    assert_eq!(listicle_survival(Some(&enumeration)), 1.0);
}

/// Break-the-code check (design doc Phase 7 success criterion): pointing the
/// metric at a currently-shipped video fixture - harvested from a published
/// note under the pre-restore pipeline, so it carries no `enumeration:` key
/// at all - must score 0. This proves the metric actually bites the
/// regression rather than passing vacuously.
#[test]
fn listicle_survival_scores_zero_against_current_shipped_video_fixture() {
    let fixtures = crate::eval::fixtures::load(&fixtures_dir()).expect("load committed fixtures");
    let fx = fixtures
        .iter()
        .find(|f| f.id == "video/there-are-only-5-safe-places-to-build-in-ai-right-now-are-yo")
        .expect("shipped video fixture present");
    assert!(
        fx.distilled.enumeration.is_none(),
        "fixture must genuinely lack enumeration"
    );
    assert_eq!(listicle_survival(fx.distilled.enumeration.as_ref()), 0.0);
}

/// Positive counterpart: the April-shape fixture (Phase 7) declares 10 items
/// and carries all 10 - full marks.
#[test]
fn listicle_survival_full_marks_against_april_shape_fixture() {
    let fixtures = crate::eval::fixtures::load(&fixtures_dir()).expect("load committed fixtures");
    let fx = fixtures
        .iter()
        .find(|f| f.id == "video/top-10-claude-code-skills-plugins-clis-april-2026")
        .expect("April-shape fixture present");
    let enumeration = fx
        .distilled
        .enumeration
        .as_ref()
        .expect("fixture declares an enumeration");
    assert_eq!(enumeration.declared_count, Some(10));
    assert_eq!(enumeration.items.len(), 10);
    assert_eq!(listicle_survival(fx.distilled.enumeration.as_ref()), 1.0);
}

// --- note-size -------------------------------------------------------------

#[test]
fn note_size_within_ceiling_passes_below_the_limit() {
    assert!(note_size_within_ceiling(MAX_NOTE_BYTES - 1));
}

#[test]
fn note_size_within_ceiling_fails_at_and_above_the_limit() {
    assert!(!note_size_within_ceiling(MAX_NOTE_BYTES));
    assert!(!note_size_within_ceiling(MAX_NOTE_BYTES + 1));
}

/// Break-the-code check for note-size: a synthetic fixture whose rendered
/// body IS a verbatim transcript leak (the exact class of note this ceiling
/// exists to catch) must fail the metric.
#[test]
fn note_size_within_ceiling_fails_on_a_simulated_transcript_leak() {
    let leaked = "x".repeat(MAX_NOTE_BYTES + 1);
    assert!(!note_size_within_ceiling(leaked.len()));
}

/// Positive counterpart: rendering the committed April-shape fixture's
/// `Distilled` (transcript-free, per `RenderOptions::for_url_publish`) stays
/// comfortably under the ceiling.
#[test]
fn note_size_within_ceiling_passes_on_april_shape_fixture_render() {
    let fixtures = crate::eval::fixtures::load(&fixtures_dir()).expect("load committed fixtures");
    let fx = fixtures
        .iter()
        .find(|f| f.id == "video/top-10-claude-code-skills-plugins-clis-april-2026")
        .expect("April-shape fixture present");
    let rendered = distillers::render(&fx.distilled, distillers::RenderOptions::for_url_publish(&fx.distilled));
    assert!(
        note_size_within_ceiling(rendered.body_markdown.len()),
        "rendered body was {} bytes, ceiling is {MAX_NOTE_BYTES}",
        rendered.body_markdown.len()
    );
}
