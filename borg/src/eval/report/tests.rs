use super::*;

fn s(cc: u8, av: u8, sf: u8) -> AxisScores {
    AxisScores {
        claim_coverage: cc,
        anchor_validity: av,
        summary_faithfulness: sf,
    }
}

#[test]
fn aggregate_means_and_composite() {
    let k = KindReport::aggregate("video", &[s(3, 3, 3), s(1, 1, 1)]);
    assert_eq!(k.n, 2);
    assert!((k.claim_coverage - 2.0).abs() < 1e-9);
    assert!((k.composite - 2.0).abs() < 1e-9);
}

#[test]
fn aggregate_empty_is_zero() {
    let k = KindReport::aggregate("empty", &[]);
    assert_eq!(k.n, 0);
    assert_eq!(k.composite, 0.0);
}

#[test]
fn calibration_panel_none_without_labels() {
    assert!(calibration_panel(&[]).is_none());
}

#[test]
fn calibration_panel_trust_gate() {
    // perfect agreement -> P/R 1.0 -> trustworthy
    let good = calibration_panel(&[(3, 3), (2, 2), (0, 0)]).expect("panel");
    assert!(good.trustworthy);
    // judge misses every human hit -> recall 0 -> below the gate
    let bad = calibration_panel(&[(3, 0), (2, 0), (3, 1)]).expect("panel");
    assert!(!bad.trustworthy);
}

#[test]
fn render_contains_kinds_overall_and_uncalibrated_note() {
    let report = EvalReport {
        judge_model: String::new(),
        total_fixtures: 2,
        total_judgments: 2,
        new_judgments: 2,
        truncated_judgments: 0,
        fallback_fixtures: 1,
        kinds: vec![KindReport::aggregate("article", &[s(2, 3, 3)])],
        overall: KindReport::aggregate(crate::eval::OVERALL_LABEL, &[s(2, 3, 3)]),
        calibration: None,
    };
    let text = report.render();
    assert!(text.contains("distillation eval"));
    assert!(text.contains("article"));
    assert!(text.contains("ALL"));
    assert!(text.contains("UNCALIBRATED"));
}
