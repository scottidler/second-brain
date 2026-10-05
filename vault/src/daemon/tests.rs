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
