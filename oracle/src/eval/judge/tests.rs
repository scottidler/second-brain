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
