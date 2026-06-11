use super::*;

#[test]
fn test_scan_config_default() {
    let config = ScanConfig::default();
    assert!(config.ignore.contains(&".git".to_string()));
    assert!(config.ignore.contains(&".obsidian".to_string()));
    // Audit quarantine must be excluded so oracle/cortex don't index
    // set-aside-pending-review notes as live knowledge.
    assert!(config.ignore.contains(&"quarantine".to_string()));
}

#[test]
fn test_llm_config_default() {
    let config = LlmConfig::default();
    assert_eq!(config.provider, "claude");
    assert_eq!(config.model, "claude-sonnet-4-6");
}

#[test]
fn test_resolve_secret_from_file() {
    let dir = std::env::temp_dir().join("vault-test-secret");
    fs::create_dir_all(&dir).expect("create dir");
    let file = dir.join("test-token");
    fs::write(&file, "  my-secret-value\n").expect("write");
    let result = resolve_secret(file.to_str().expect("path")).expect("resolve");
    assert_eq!(result, "my-secret-value");
    let _ = fs::remove_file(&file);
}

#[test]
fn test_resolve_secret_from_env() {
    let key = "VAULT_TEST_SECRET_42";
    unsafe { std::env::set_var(key, "env-secret-value") };
    let result = resolve_secret(key).expect("resolve");
    assert_eq!(result, "env-secret-value");
    unsafe { std::env::remove_var(key) };
}

#[test]
fn test_resolve_secret_missing() {
    let result = resolve_secret("NONEXISTENT_VAR_VAULT_TEST_999");
    assert!(result.is_err());
}
