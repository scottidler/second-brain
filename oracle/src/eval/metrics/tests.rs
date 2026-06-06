use super::*;

fn judgments(pairs: &[(&str, u8)]) -> Judgments {
    pairs.iter().map(|(p, s)| (p.to_string(), *s)).collect()
}

fn paths(p: &[&str]) -> Vec<String> {
    p.iter().map(|s| s.to_string()).collect()
}

#[test]
fn pool_unions_and_dedups_sorted() {
    let a = paths(&["c", "a", "b"]);
    let b = paths(&["b", "d"]);
    assert_eq!(pool(&[a, b]), paths(&["a", "b", "c", "d"]));
}

#[test]
fn dcg_matches_hand_computation() {
    // j: a=3, b=0, c=2, d=1 ; ranked = [a,b,c], k=3
    // DCG = 7/log2(2) + 0/log2(3) + 3/log2(4) = 7 + 0 + 1.5 = 8.5
    let j = judgments(&[("a", 3), ("b", 0), ("c", 2), ("d", 1)]);
    let ranked = paths(&["a", "b", "c"]);
    assert!(
        (dcg_at_k(&ranked, &j, 3) - 8.5).abs() < 1e-9,
        "dcg = {}",
        dcg_at_k(&ranked, &j, 3)
    );
}

#[test]
fn idcg_uses_ideal_ordering() {
    // ideal order by rel desc: a(3), c(2), d(1)
    // IDCG@3 = 7/log2(2) + 3/log2(3) + 1/log2(4) = 7 + 1.892789 + 0.5
    let j = judgments(&[("a", 3), ("b", 0), ("c", 2), ("d", 1)]);
    let expected = 7.0 + 3.0 / 3.0_f64.log2() + 1.0 / 4.0_f64.log2();
    assert!((idcg_at_k(&j, 3) - expected).abs() < 1e-9);
}

#[test]
fn ndcg_is_dcg_over_idcg() {
    let j = judgments(&[("a", 3), ("b", 0), ("c", 2), ("d", 1)]);
    let ranked = paths(&["a", "b", "c"]);
    let expected = 8.5 / (7.0 + 3.0 / 3.0_f64.log2() + 1.0 / 4.0_f64.log2());
    let got = ndcg_at_k(&ranked, &j, 3).expect("has relevant");
    assert!((got - expected).abs() < 1e-9, "ndcg {got} vs {expected}");
}

#[test]
fn ndcg_none_when_no_relevant_in_pool() {
    let j = judgments(&[("a", 0), ("b", 0)]);
    let ranked = paths(&["a", "b"]);
    assert_eq!(ndcg_at_k(&ranked, &j, 3), None);
}

#[test]
fn recall_counts_hits_over_total_relevant() {
    // relevant (>=2): a, c ; top-3 [a,b,c] contains a,c -> 2/2 = 1.0
    let j = judgments(&[("a", 3), ("b", 0), ("c", 2), ("d", 1)]);
    let ranked = paths(&["a", "b", "c"]);
    assert_eq!(recall_at_k(&ranked, &j, 3, 2), Some(1.0));
}

#[test]
fn recall_partial_when_relevant_outside_topk() {
    // relevant: a, c, e ; top-2 [a,b] contains only a -> 1/3
    let j = judgments(&[("a", 3), ("b", 0), ("c", 2), ("e", 2)]);
    let ranked = paths(&["a", "b", "c", "e"]);
    let got = recall_at_k(&ranked, &j, 2, 2).expect("relevant exist");
    assert!((got - 1.0 / 3.0).abs() < 1e-9, "recall {got}");
}

#[test]
fn recall_none_when_no_relevant() {
    let j = judgments(&[("a", 1), ("b", 0)]);
    let ranked = paths(&["a", "b"]);
    assert_eq!(recall_at_k(&ranked, &j, 3, 2), None);
}

#[test]
fn reciprocal_rank_first_hit() {
    // first relevant (>=2) in [b,a,c] is a at rank 2 -> 0.5
    let j = judgments(&[("a", 3), ("b", 0), ("c", 2)]);
    let ranked = paths(&["b", "a", "c"]);
    assert_eq!(reciprocal_rank(&ranked, &j, 3, 2), Some(0.5));
}

#[test]
fn reciprocal_rank_zero_when_relevant_below_k() {
    // relevant c exists but is below k=1
    let j = judgments(&[("b", 0), ("c", 2)]);
    let ranked = paths(&["b", "c"]);
    assert_eq!(reciprocal_rank(&ranked, &j, 1, 2), Some(0.0));
}

#[test]
fn reciprocal_rank_none_when_no_relevant() {
    let j = judgments(&[("b", 0), ("c", 1)]);
    let ranked = paths(&["b", "c"]);
    assert_eq!(reciprocal_rank(&ranked, &j, 3, 2), None);
}

#[test]
fn aggregate_ignores_excluded_queries() {
    let scores = vec![
        QueryScores {
            ndcg: Some(1.0),
            recall: Some(1.0),
            rr: Some(1.0),
        },
        QueryScores {
            ndcg: None,
            recall: None,
            rr: None,
        }, // excluded
        QueryScores {
            ndcg: Some(0.0),
            recall: Some(0.0),
            rr: Some(0.0),
        },
    ];
    let m = aggregate(&scores);
    assert_eq!(m.n_ndcg, 2);
    assert!((m.ndcg - 0.5).abs() < 1e-9);
    assert_eq!(m.n_recall, 2);
    assert!((m.mrr - 0.5).abs() < 1e-9);
}
