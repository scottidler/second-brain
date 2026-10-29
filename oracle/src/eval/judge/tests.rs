use super::*;

#[test]
fn mock_judge_returns_default_for_unknown_pairs() {
    let j = MockJudge::new(1);
    assert_eq!(j.judge("q", "Some Note", "body").expect("judge"), 1);
}

#[test]
fn mock_judge_returns_override_for_known_pair() {
    let j = MockJudge::new(0).with("claude code", "Claude", 3);
    assert_eq!(j.judge("claude code", "Claude", "body").expect("judge"), 3);
    // a different query/title pair falls back to default
    assert_eq!(j.judge("other", "Claude", "body").expect("judge"), 0);
}

#[test]
fn mock_judge_clamps_scores_to_max() {
    let j = MockJudge::new(9).with("q", "T", 8);
    assert_eq!(j.judge("q", "T", "").expect("judge"), MAX_SCORE);
    assert_eq!(j.judge("nope", "X", "").expect("judge"), MAX_SCORE);
}

#[test]
fn parse_score_reads_bare_integer() {
    assert_eq!(parse_score("2").expect("ok"), 2);
    assert_eq!(parse_score("0\n").expect("ok"), 0);
}

#[test]
fn parse_score_reads_first_integer_amid_prose() {
    assert_eq!(parse_score("Score: 3").expect("ok"), 3);
    assert_eq!(parse_score("the relevance is 2 out of 3").expect("ok"), 2);
    assert_eq!(parse_score("3/3").expect("ok"), 3);
}

#[test]
fn parse_score_clamps_out_of_range() {
    assert_eq!(parse_score("7").expect("ok"), MAX_SCORE);
    assert_eq!(parse_score("256").expect("ok"), MAX_SCORE);
}

#[test]
fn parse_score_errors_without_integer() {
    let err = parse_score("no number here").expect_err("must error");
    assert!(format!("{err}").contains("no integer score"));
}

#[test]
fn judge_input_budget_leaves_the_full_note_budget_after_framing() {
    let note = "n".repeat(8_000);
    let input = format!("# QUERY\nq\n\n# NOTE TITLE\nt\n\n# NOTE\n{note}\n");
    let budget = judge_input_budget(&input, &note, 8_000);
    assert_eq!(budget, input.chars().count(), "a note at the cap reaches fabric whole");
    // A note over the cap is still cut by fabric.
    let long = "n".repeat(9_000);
    let input = format!("# QUERY\nq\n\n# NOTE\n{long}\n");
    assert!(judge_input_budget(&input, &long, 8_000) < input.chars().count());
}

#[test]
fn mock_judge_failing_pair_errors() {
    let j = MockJudge::new(2).failing("q", "T");
    assert!(j.judge("q", "T", "x").is_err());
    assert_eq!(j.judge("q", "U", "x").unwrap(), 2);
}
