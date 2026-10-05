use super::*;

#[test]
fn hotkey_config_defaults_to_localhost_8181() {
    let cfg = HotkeyConfig::default();
    assert_eq!(cfg.host, "localhost");
    assert_eq!(cfg.port, 8181);
}

#[test]
fn hotkey_config_parses_kebab_yaml_with_defaults_for_missing_keys() {
    let cfg: HotkeyConfig = serde_yaml::from_str("host: desk.lan\n").expect("parse");
    assert_eq!(cfg.host, "desk.lan");
    assert_eq!(cfg.port, 8181);
}

#[test]
fn client_auth_token_none_when_unconfigured() {
    assert_eq!(client_auth_token(None), None);
}

#[test]
fn client_auth_token_reads_file_reference() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("token");
    std::fs::write(&file, "  s3cret\n").expect("write");
    assert_eq!(
        client_auth_token(Some(file.to_str().expect("utf8 path"))),
        Some("s3cret".to_string())
    );
}

#[test]
fn client_auth_token_none_when_reference_unresolvable_or_empty() {
    assert_eq!(client_auth_token(Some("VAULT_DAEMON_TEST_UNSET_TOKEN_VAR")), None);
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("empty");
    std::fs::write(&file, "   \n").expect("write");
    assert_eq!(client_auth_token(Some(file.to_str().expect("utf8 path"))), None);
}

#[test]
fn request_timeout_defaults_to_ten_seconds() {
    assert_eq!(
        HotkeyConfig::default().request_timeout,
        std::time::Duration::from_secs(10)
    );
    let parsed: HotkeyConfig = serde_yaml::from_str("host: desk.lan\n").expect("parse");
    assert_eq!(parsed.request_timeout, std::time::Duration::from_secs(10));
}

#[test]
fn request_timeout_reads_kebab_case_humantime() {
    let parsed: HotkeyConfig = serde_yaml::from_str("request-timeout: 1s\n").expect("parse");
    assert_eq!(parsed.request_timeout, std::time::Duration::from_secs(1));
    let parsed: HotkeyConfig = serde_yaml::from_str("request-timeout: 1m 30s\n").expect("parse");
    assert_eq!(parsed.request_timeout, std::time::Duration::from_secs(90));
}

#[test]
fn a_bad_request_timeout_is_a_loud_error_naming_the_key() {
    let err = serde_yaml::from_str::<HotkeyConfig>("request-timeout: soon\n").expect_err("must reject");
    let msg = err.to_string();
    assert!(msg.contains("request-timeout") && msg.contains("soon"), "{msg}");
}
