use super::*;

#[test]
fn limit_defaults_to_100_and_null_lifts_it() {
    assert_eq!(EvalConfig::default().limit, Some(DEFAULT_LIMIT));
    let config: EvalConfig = serde_yaml::from_str("schedule: \"*-*-* 05:00:00\"\n").unwrap();
    assert_eq!(config.limit, Some(100));
    let config: EvalConfig = serde_yaml::from_str("limit: null\n").unwrap();
    assert_eq!(config.limit, None);
}
