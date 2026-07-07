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

/// Minimal `EvalReport` builder for render tests: fills the judge-axis fields
/// with a stub two-fixture aggregate and lets the caller override the
/// deterministic-metrics fields under test.
fn stub_report(
    listicle: Vec<ListicleMetric>,
    listicle_aggregate: Option<f64>,
    note_size: Vec<NoteSizeMetric>,
) -> EvalReport {
    EvalReport {
        judge_model: String::new(),
        total_fixtures: 2,
        total_judgments: 2,
        new_judgments: 2,
        truncated_judgments: 0,
        fallback_fixtures: 1,
        kinds: vec![KindReport::aggregate("article", &[s(2, 3, 3)])],
        overall: KindReport::aggregate(crate::eval::OVERALL_LABEL, &[s(2, 3, 3)]),
        calibration: None,
        listicle,
        listicle_aggregate,
        note_size,
    }
}

#[test]
fn render_contains_kinds_overall_and_uncalibrated_note() {
    let report = stub_report(vec![], None, vec![]);
    let text = report.render();
    assert!(text.contains("distillation eval"));
    assert!(text.contains("article"));
    assert!(text.contains("ALL"));
    assert!(text.contains("UNCALIBRATED"));
}

#[test]
fn render_shows_na_when_no_fixture_is_listicle_applicable() {
    let text = stub_report(vec![], None, vec![]).render();
    assert!(text.contains("listicle-survival: N/A"));
}

#[test]
fn render_shows_listicle_aggregate_and_per_fixture_rows() {
    let listicle = vec![ListicleMetric {
        fixture: "video/top-10-claude-code-skills-plugins-clis-april-2026".to_string(),
        score: 1.0,
    }];
    let text = stub_report(listicle, Some(1.0), vec![]).render();
    assert!(text.contains("listicle-survival: 1.000  (1 applicable fixture)"));
    assert!(text.contains("video/top-10-claude-code-skills-plugins-clis-april-2026"));
}

#[test]
fn render_reports_note_size_pass_count_and_failing_fixtures() {
    let note_size = vec![
        NoteSizeMetric {
            fixture: "video/a".to_string(),
            rendered_bytes: 2_000,
            within_ceiling: true,
        },
        NoteSizeMetric {
            fixture: "video/b".to_string(),
            rendered_bytes: 100_000,
            within_ceiling: false,
        },
    ];
    let text = stub_report(vec![], None, note_size).render();
    assert!(text.contains("note-size: 1/2 within the 65536-byte ceiling"));
    assert!(text.contains("FAIL video/b"));
    assert!(!text.contains("FAIL video/a"));
}
