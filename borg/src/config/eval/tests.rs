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

#[test]
fn claims_cap_and_timeout_default_to_400k_and_90s_and_zero_is_a_load_error() {
    let config = EvalConfig::default();
    assert_eq!(config.claims_source_max_chars, 400_000);
    assert_eq!(config.claims_timeout_secs, 90);
    let set: EvalConfig = serde_yaml::from_str("claims-source-max-chars: 1000\nclaims-timeout-secs: 45\n").unwrap();
    assert_eq!((set.claims_source_max_chars, set.claims_timeout_secs), (1000, 45));
    set.validate().unwrap();
    for (yaml, needle) in [
        ("claims-source-max-chars: 0\n", "claims-source-max-chars"),
        ("claims-timeout-secs: 0\n", "claims-timeout-secs"),
    ] {
        let config: EvalConfig = serde_yaml::from_str(yaml).unwrap();
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains(needle), "{yaml}: {err}");
    }
}
