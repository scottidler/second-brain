use super::*;
use crate::eval::metrics::MetricMeans;

fn means(ndcg: f64, recall: f64, mrr: f64) -> MetricMeans {
    MetricMeans {
        ndcg,
        recall,
        mrr,
        n_ndcg: 10,
        n_recall: 10,
        n_mrr: 10,
    }
}

fn sample_report(calibration: Option<CalibrationPanel>, inconclusive: bool) -> EvalReport {
    EvalReport {
        k: 10,
        judge_model: String::new(),
        total_queries: 10,
        modes: vec![
            ModeReport {
                mode: "hybrid".into(),
                means: means(0.60, 0.67, 0.55),
                latency: Latency::default(),
                rerank: None,
            },
            ModeReport {
                mode: "graph-hybrid".into(),
                means: means(0.64, 0.71, 0.58),
                latency: Latency::from_samples(vec![12.0, 8.0]),
                rerank: None,
            },
        ],
        lift_ndcg: 0.04,
        lift_recall: 0.04,
        lift_mrr: 0.03,
        truncated_judgments: 2,
        total_judgments: 120,
        ablation: AblationReport {
            queries_touching_fact: if inconclusive { 0 } else { 6 },
            total_queries: 10,
            ndcg_lift_vs_ablation: 0.02,
            inconclusive,
        },
        calibration,
        paired: None,
    }
}

#[test]
fn render_contains_core_sections() {
    let r = sample_report(None, false);
    let out = r.render();
    assert!(out.contains("relevance eval @ K=10"));
    assert!(out.contains("graph-hybrid"));
    assert!(out.contains("LIFT graph-hybrid vs hybrid"));
    assert!(out.contains("fact-layer ablation"));
    assert!(out.contains("UNCALIBRATED"));
}

#[test]
fn render_shows_inconclusive_ablation() {
    let out = sample_report(None, true).render();
    assert!(out.contains("INCONCLUSIVE - fact layer not exercised"));
}

#[test]
fn render_shows_calibration_panel() {
    let panel = CalibrationPanel {
        pairs: 40,
        exact_pct: 0.7,
        adjacent_pct: 0.95,
        boundary_precision: 0.8,
        boundary_recall: 0.75,
        kappa: 0.35,
        trustworthy: true,
    };
    let out = sample_report(Some(panel), false).render();
    assert!(out.contains("calibration (40 hand-labeled pairs)"));
    assert!(out.contains("TRUSTWORTHY"));
    assert!(out.contains("kappa 0.35"));
}

#[test]
fn latency_is_nearest_rank() {
    let l = Latency::from_samples((1..=20).map(f64::from).rev().collect());
    assert_eq!((l.p50_ms, l.p95_ms, l.n), (10.0, 19.0, 20));
    let one = Latency::from_samples(vec![42.0]);
    assert_eq!((one.p50_ms, one.p95_ms), (42.0, 42.0));
    assert_eq!(Latency::from_samples(Vec::new()), Latency::default());
}

#[test]
fn render_shows_latency_or_a_dash_when_untimed() {
    let out = sample_report(None, false).render();
    assert!(out.contains("p50 ms"), "{out}");
    let gh = out
        .lines()
        .find(|l| l.starts_with("graph-hybrid"))
        .expect("graph-hybrid row");
    assert!(gh.contains("8.0") && gh.contains("12.0"), "{gh}");
    let hy = out.lines().find(|l| l.starts_with("hybrid")).expect("hybrid row");
    assert!(hy.contains(" - "), "{hy}");
}

#[test]
fn rerank_cell_names_off_ran_and_not_run() {
    use crate::server::RerankRun::{Disabled, FailedOpen, Off, Ran};
    assert_eq!(RerankTally::from_runs([Off, Off].into_iter()).cell(), "off");
    assert_eq!(RerankTally::from_runs([Ran, Ran].into_iter()).cell(), "ran 2/2");
    assert_eq!(
        RerankTally::from_runs([Ran, FailedOpen, Disabled].into_iter()).cell(),
        "NOT RUN 2/3"
    );
}
