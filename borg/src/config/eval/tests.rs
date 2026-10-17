use super::*;

#[test]
fn limit_defaults_to_100_and_null_lifts_it() {
    assert_eq!(EvalConfig::default().limit, Some(DEFAULT_LIMIT));
    let config: EvalConfig = serde_yaml::from_str("schedule: \"*-*-* 05:00:00\"\n").unwrap();
    assert_eq!(config.limit, Some(100));
    let config: EvalConfig = serde_yaml::from_str("limit: null\n").unwrap();
    assert_eq!(config.limit, None);
}

/// A blank judge model would run fabric's default model under a trust gate
/// that cannot name it: a load error, never a judge.
#[test]
fn a_blank_judge_model_is_a_load_error() {
    for yaml in ["judge-models: ['']\n", "judge-models: [gemini-2.5-pro, '  ']\n"] {
        let config: EvalConfig = serde_yaml::from_str(yaml).unwrap();
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("is blank"), "{yaml}: {err}");
    }
    let named: EvalConfig = serde_yaml::from_str("judge-models: [gemini-2.5-pro]\n").unwrap();
    named.validate().unwrap();
    let emptied: EvalConfig = serde_yaml::from_str("judge-models: []\n").unwrap();
    emptied.validate().unwrap();
}
