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

#[derive(Debug, Deserialize, Default, PartialEq)]
struct LoadTestConfig {
    #[serde(default)]
    name: String,
    #[serde(default)]
    derived: String,
}

impl Normalize for LoadTestConfig {
    fn normalize(&mut self) {
        self.derived = format!("from-{}", self.name);
    }
}

#[test]
fn load_config_explicit_path_parses_and_normalizes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("borg.yml");
    fs::write(&path, "name: desk\n").expect("write");
    let config: LoadTestConfig = load_config(Some(&path)).expect("load");
    assert_eq!(config.name, "desk");
    assert_eq!(config.derived, "from-desk", "normalize must run on the explicit path");
}

#[test]
fn load_config_explicit_path_errors_loudly_on_bad_yaml_and_missing_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let bad = dir.path().join("bad.yml");
    fs::write(&bad, "name: [unclosed\n").expect("write");
    let err = load_config::<LoadTestConfig>(Some(&bad)).expect_err("bad yaml must error");
    assert!(format!("{err:#}").contains("Failed to load config from"), "{err:#}");

    let missing = dir.path().join("missing.yml");
    let err = load_config::<LoadTestConfig>(Some(&missing)).expect_err("missing explicit file must error");
    assert!(format!("{err:#}").contains("missing.yml"), "{err:#}");
}

#[test]
fn load_first_existing_bad_yaml_errors_instead_of_falling_back() {
    let dir = tempfile::tempdir().expect("tempdir");
    let primary = dir.path().join("primary.yml");
    let fallback = dir.path().join("fallback.yml");
    fs::write(&primary, "name: [unclosed\n").expect("write");
    fs::write(&fallback, "name: lappy\n").expect("write");
    let err = load_first_existing::<LoadTestConfig>(&[primary, fallback]).expect_err("bad primary must error");
    assert!(format!("{err:#}").contains("primary.yml"), "{err:#}");
}

#[test]
fn load_first_existing_skips_missing_and_defaults_when_none_exist() {
    let dir = tempfile::tempdir().expect("tempdir");
    let missing = dir.path().join("missing.yml");
    let fallback = dir.path().join("fallback.yml");
    fs::write(&fallback, "name: lappy\n").expect("write");
    let config: LoadTestConfig = load_first_existing(&[missing.clone(), fallback]).expect("load fallback");
    assert_eq!(config.name, "lappy");
    let config: LoadTestConfig = load_first_existing(&[missing]).expect("defaults");
    assert_eq!(config, LoadTestConfig::default());
}
