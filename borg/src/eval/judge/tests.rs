use super::*;

#[test]
fn parse_clean_yaml_reply() {
    let reply = "claim-coverage: 2\nanchor-validity: 3\nsummary-faithfulness: 1\n";
    let s = parse_axis_scores(reply).expect("parse");
    assert_eq!(s.claim_coverage, 2);
    assert_eq!(s.anchor_validity, 3);
    assert_eq!(s.summary_faithfulness, 1);
}

#[test]
fn parse_tolerates_fences_prose_and_underscores() {
    let reply = "Here is my assessment:\n\n```yaml\nclaim_coverage = 3\nAnchor Validity: 2\nsummary-faithfulness:  0\n```\nDone.";
    let s = parse_axis_scores(reply).expect("parse");
    assert_eq!(s.claim_coverage, 3);
    assert_eq!(s.anchor_validity, 2);
    assert_eq!(s.summary_faithfulness, 0);
}

#[test]
fn parse_clamps_out_of_range_scores() {
    let reply = "claim-coverage: 9\nanchor-validity: 4\nsummary-faithfulness: 2";
    let s = parse_axis_scores(reply).expect("parse");
    assert_eq!(s.claim_coverage, MAX_SCORE);
    assert_eq!(s.anchor_validity, MAX_SCORE);
    assert_eq!(s.summary_faithfulness, 2);
}

#[test]
fn parse_errors_when_an_axis_is_missing() {
    let reply = "claim-coverage: 2\nanchor-validity: 3\n"; // no summary axis
    let err = parse_axis_scores(reply).expect_err("missing axis is an error");
    assert!(err.to_string().contains("summary"));
}

#[test]
fn composite_is_mean_of_axes() {
    let s = AxisScores {
        claim_coverage: 3,
        anchor_validity: 3,
        summary_faithfulness: 0,
    };
    assert!((s.composite() - 2.0).abs() < 1e-9);
}

#[test]
fn mock_judge_returns_per_kind_and_counts_calls() {
    let judge = MockJudge::new(AxisScores {
        claim_coverage: 1,
        anchor_validity: 1,
        summary_faithfulness: 1,
    })
    .with(
        "video",
        AxisScores {
            claim_coverage: 3,
            anchor_validity: 2,
            summary_faithfulness: 3,
        },
    );
    let v = judge.judge("video", "src", "note").expect("judge");
    assert_eq!(v.claim_coverage, 3);
    let a = judge.judge("article", "src", "note").expect("judge");
    assert_eq!(a.claim_coverage, 1); // falls back to default
    assert_eq!(judge.calls(), 2);
}
