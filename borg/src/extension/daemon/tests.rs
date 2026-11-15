use super::*;

fn with_endpoints(endpoints: &[&str]) -> Config {
    let mut config = Config::default();
    config.extension.endpoints = Some(endpoints.iter().map(|s| s.to_string()).collect());
    config
}

fn bases(config: &Config) -> Vec<String> {
    resolved_endpoints(config)
        .expect("valid endpoints")
        .iter()
        .map(endpoint_base)
        .collect()
}

#[test]
fn unset_endpoints_default_to_localhost() {
    assert_eq!(bases(&Config::default()), vec!["http://localhost:8181".to_string()]);
}

#[test]
fn unset_timeout_defaults_to_3000() {
    assert_eq!(attempt_timeout_ms(&Config::default()).unwrap(), 3000);
}

#[test]
fn explicit_timeout_is_returned() {
    let mut config = Config::default();
    config.extension.attempt_timeout_ms = Some(750);
    assert_eq!(attempt_timeout_ms(&config).unwrap(), 750);
}

#[test]
fn zero_timeout_is_an_error() {
    let mut config = Config::default();
    config.extension.attempt_timeout_ms = Some(0);
    let err = attempt_timeout_ms(&config).unwrap_err().to_string();
    assert!(err.contains("attempt-timeout-ms"), "{err}");
}

#[test]
fn endpoints_keep_config_order() {
    let config = with_endpoints(&["http://ripr.example.ts.net:8181", "http://ripr.lan:8181"]);
    assert_eq!(
        bases(&config),
        vec![
            "http://ripr.example.ts.net:8181".to_string(),
            "http://ripr.lan:8181".to_string()
        ]
    );
}

#[test]
fn trailing_slash_is_stripped() {
    let config = with_endpoints(&["http://ripr.lan:8181/"]);
    assert_eq!(bases(&config), vec!["http://ripr.lan:8181".to_string()]);
}

#[test]
fn empty_list_is_an_error() {
    let err = resolved_endpoints(&with_endpoints(&[])).unwrap_err().to_string();
    assert!(err.contains("empty"), "{err}");
}

#[test]
fn https_is_an_error() {
    let err = resolved_endpoints(&with_endpoints(&["https://ripr.lan:8181"]))
        .unwrap_err()
        .to_string();
    assert!(err.contains("http"), "{err}");
}

#[test]
fn unparseable_url_is_an_error() {
    let err = resolved_endpoints(&with_endpoints(&["not a url"]))
        .unwrap_err()
        .to_string();
    assert!(err.contains("not a valid URL"), "{err}");
}

#[test]
fn missing_host_is_an_error() {
    assert!(resolved_endpoints(&with_endpoints(&["http://"])).is_err());
}

#[test]
fn path_query_fragment_and_userinfo_are_each_an_error() {
    for bad in [
        "http://ripr.lan:8181/ingest",
        "http://ripr.lan:8181/?x=1",
        "http://ripr.lan:8181/?",
        "http://ripr.lan:8181/#frag",
        "http://user:pw@ripr.lan:8181",
        "http://user@ripr.lan:8181",
    ] {
        assert!(
            resolved_endpoints(&with_endpoints(&[bad])).is_err(),
            "{bad} must be rejected"
        );
    }
}

#[test]
fn one_bad_entry_fails_the_whole_list() {
    let config = with_endpoints(&["http://ripr.lan:8181", "https://ripr.example"]);
    assert!(resolved_endpoints(&config).is_err());
}
