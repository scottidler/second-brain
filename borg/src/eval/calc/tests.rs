use super::*;

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
