use super::*;
use crate::server::RerankRun;

fn list(xs: &[&str]) -> Vec<String> {
    xs.iter().map(|s| s.to_string()).collect()
}

fn judged(xs: &[(&str, u8)]) -> Judgments {
    xs.iter().map(|(p, s)| (p.to_string(), *s)).collect()
}

#[test]
fn split_is_fnv_parity_of_the_id() {
    for id in ["q1", "q2", "mined-0123456789abcdef", "x"] {
        let want = if fnv1a(id).is_multiple_of(2) { Split::Tune } else { Split::Confirm };
        assert_eq!(Split::of(id), want, "{id}");
    }
    // Both halves are reachable.
    let ids: Vec<String> = (0..16).map(|i| format!("q{i}")).collect();
    assert!(ids.iter().any(|i| Split::of(i) == Split::Tune));
    assert!(ids.iter().any(|i| Split::of(i) == Split::Confirm));
}

#[test]
fn paired_ndcg_ignores_judgments_outside_the_pair_pool() {
    let a = list(&["x", "y"]);
    let b = list(&["y", "x"]);
    // `z` is relevant but surfaced only by some other target: it must not
    // raise this target's IDCG.
    let with_z = judged(&[("x", 3), ("y", 1), ("z", 3)]);
    let without_z = judged(&[("x", 3), ("y", 1)]);
    let s1 = paired_ndcg(&a, &b, &with_z, 10).expect("relevant pool");
    let s2 = paired_ndcg(&a, &b, &without_z, 10).expect("relevant pool");
    assert_eq!(s1, s2);
    assert!(
        (s1.ndcg_a - 1.0).abs() < 1e-12,
        "x first is ideal for the {{x, y}} pool"
    );
    assert!(s1.ndcg_b < 1.0);
}

#[test]
fn paired_ndcg_pool_is_top_k_only() {
    let a = list(&["x", "deep"]);
    let b = list(&["x"]);
    // `deep` sits below k=1, so it is not in the pool and adds nothing to IDCG.
    let s = paired_ndcg(&a, &b, &judged(&[("x", 1), ("deep", 3)]), 1).expect("pool");
    assert!((s.ndcg_a - 1.0).abs() < 1e-12 && (s.ndcg_b - 1.0).abs() < 1e-12);
}

#[test]
fn paired_ndcg_is_none_without_a_relevant_note() {
    assert_eq!(
        paired_ndcg(&list(&["x"]), &list(&["y"]), &judged(&[("x", 0), ("y", 0)]), 10),
        None
    );
}

#[test]
fn bootstrap_ci_is_reproducible_and_brackets_the_mean() {
    let deltas = [0.1, 0.2, -0.05, 0.3, 0.0, 0.15, 0.05, 0.25];
    let ci = bootstrap_ci(&deltas, BOOTSTRAP_RESAMPLES, BOOTSTRAP_SEED);
    assert_eq!(
        ci,
        bootstrap_ci(&deltas, BOOTSTRAP_RESAMPLES, BOOTSTRAP_SEED),
        "fixed seed"
    );
    let mean = deltas.iter().sum::<f64>() / deltas.len() as f64;
    assert!(ci.0 < mean && mean < ci.1, "{ci:?} vs {mean}");
    assert!(ci.0 > 0.0, "a clearly positive sample clears zero: {ci:?}");
    assert_ne!(ci, bootstrap_ci(&deltas, BOOTSTRAP_RESAMPLES, BOOTSTRAP_SEED + 1));
}

#[test]
fn bootstrap_ci_of_constant_deltas_is_that_constant() {
    assert_eq!(bootstrap_ci(&[0.0; 5], BOOTSTRAP_RESAMPLES, BOOTSTRAP_SEED), (0.0, 0.0));
    assert_eq!(bootstrap_ci(&[], BOOTSTRAP_RESAMPLES, BOOTSTRAP_SEED), (0.0, 0.0));
}

#[test]
fn mdd_is_insufficient_for_n_le_one_or_zero_variance() {
    assert_eq!(mdd(&[]), None);
    assert_eq!(mdd(&[0.4]), None);
    assert_eq!(mdd(&[0.2, 0.2, 0.2]), None);
    // sd of [0, 1] is sqrt(0.5); 2.8 * sqrt(0.5) / sqrt(2) = 1.4
    let m = mdd(&[0.0, 1.0]).expect("variance");
    assert!((m - 1.4).abs() < 1e-12, "{m}");
}

#[test]
fn verdict_follows_the_pre_registered_rule() {
    let varied = [0.1, 0.2];
    assert_eq!(verdict(&varied, (0.01, 0.3), 100.0, None), Verdict::Ship);
    assert_eq!(
        verdict(&varied, (0.0, 0.3), 100.0, None),
        Verdict::Hold,
        "lower bound must be > 0"
    );
    assert_eq!(verdict(&varied, (-0.1, 0.3), 100.0, None), Verdict::Hold);
    assert_eq!(
        verdict(&varied, (0.01, 0.3), 1600.0, Some(1500)),
        Verdict::Hold,
        "over budget"
    );
    assert_eq!(verdict(&varied, (0.01, 0.3), 1500.0, Some(1500)), Verdict::Ship);
    assert_eq!(verdict(&[0.5], (0.5, 0.5), 1.0, None), Verdict::Insufficient);
    assert_eq!(verdict(&[0.5, 0.5], (0.5, 0.5), 1.0, None), Verdict::Insufficient);
}

#[test]
fn summarize_means_deltas_and_latency() {
    let scores = [
        PairedScore {
            ndcg_a: 0.5,
            ndcg_b: 0.7,
        },
        PairedScore {
            ndcg_a: 0.6,
            ndcg_b: 0.6,
        },
    ];
    let row = summarize(
        "configured",
        Split::Confirm,
        &scores,
        vec![10.0, 20.0],
        vec![30.0, 40.0],
        None,
    );
    assert_eq!(row.n, 2);
    assert!((row.ndcg_a - 0.55).abs() < 1e-12);
    assert!((row.ndcg_b - 0.65).abs() < 1e-12);
    assert!((row.delta - 0.1).abs() < 1e-12);
    assert_eq!(row.latency_b.p95_ms, 40.0);
    assert!(row.mdd.is_some());
    let empty = summarize("graph", Split::Tune, &[], vec![], vec![], None);
    assert_eq!((empty.n, empty.verdict, empty.mdd), (0, Verdict::Insufficient, None));
}

#[test]
fn render_names_every_column_and_the_gate() {
    let row = summarize(
        "configured",
        Split::Confirm,
        &[PairedScore {
            ndcg_a: 0.5,
            ndcg_b: 0.7,
        }],
        vec![1.0],
        vec![2.0],
        None,
    );
    let report = PairedReport {
        compare_db: None,
        compare_config: Some("/tmp/arm.yml".into()),
        latency_budget_ms: None,
        rerank_a: RerankTally::from_runs([RerankRun::Off].into_iter()),
        rerank_b: RerankTally::from_runs([RerankRun::Ran].into_iter()),
        rows: vec![row],
    };
    let out = report.render();
    for col in [
        "nDCG A",
        "nDCG B",
        "delta",
        "95% CI",
        "MDD",
        "verdict",
        "/tmp/arm.yml",
        "NOT gated",
    ] {
        assert!(out.contains(col), "{col} missing:\n{out}");
    }
    assert!(out.contains("configured rerank: arm A off, arm B ran 1/1"), "{out}");
    assert!(!out.contains("WARNING"), "{out}");
    let line = out.lines().find(|l| l.starts_with("configured")).expect("row");
    assert!(line.contains("confirm") && line.contains("insufficient"), "{line}");
}

#[test]
fn render_warns_when_arm_b_rerank_did_not_run() {
    let report = PairedReport {
        compare_db: None,
        compare_config: None,
        latency_budget_ms: Some(1500),
        rerank_a: RerankTally::from_runs([RerankRun::Off, RerankRun::Off].into_iter()),
        rerank_b: RerankTally::from_runs([RerankRun::Ran, RerankRun::Disabled].into_iter()),
        rows: Vec::new(),
    };
    let out = report.render();
    assert!(out.contains("arm B NOT RUN 1/2"), "{out}");
    assert!(
        out.contains("WARNING: arm B has rerank enabled but it did NOT run on 1/2"),
        "{out}"
    );
}
