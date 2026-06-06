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
            },
            ModeReport {
                mode: "graph-hybrid".into(),
                means: means(0.64, 0.71, 0.58),
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
