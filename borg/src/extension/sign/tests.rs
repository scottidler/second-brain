use super::*;
use crate::extension::daemon::baked_config_hash;

fn config_with_endpoints(endpoints: &[&str]) -> Config {
    let mut config = Config::default();
    config.extension.endpoints = Some(endpoints.iter().map(|s| s.to_string()).collect());
    config
}

/// A fake repo root holding `borg/clients/extension/web-ext-artifacts/<version>.xpi`.
fn fake_repo(version: &str) -> (tempfile::TempDir, PathBuf) {
    let root = tempfile::TempDir::new().unwrap();
    let artifacts = extension::extension_dir(root.path()).join("web-ext-artifacts");
    std::fs::create_dir_all(&artifacts).unwrap();
    std::fs::write(artifacts.join(format!("obsidian-borg-{version}.xpi")), b"dummy").unwrap();
    (root, artifacts)
}

fn write_sidecar(artifacts: &Path, version: &str, config: &Config) {
    std::fs::write(
        sidecar_path(artifacts, version),
        format!("{}\n", baked_config_hash(config).unwrap()),
    )
    .unwrap();
}

#[test]
fn same_config_same_hash() {
    let a = config_with_endpoints(&["http://ripr.lan:8181"]);
    let b = config_with_endpoints(&["http://ripr.lan:8181"]);
    assert_eq!(baked_config_hash(&a).unwrap(), baked_config_hash(&b).unwrap());
}

#[test]
fn endpoints_change_alters_hash() {
    let a = config_with_endpoints(&["http://ripr.lan:8181"]);
    let b = config_with_endpoints(&["http://ripr.lan:8181", "http://other.lan:8181"]);
    assert_ne!(baked_config_hash(&a).unwrap(), baked_config_hash(&b).unwrap());
}

#[test]
fn timeout_change_alters_hash() {
    let a = Config::default();
    let mut b = Config::default();
    b.extension.attempt_timeout_ms = Some(750);
    assert_ne!(baked_config_hash(&a).unwrap(), baked_config_hash(&b).unwrap());
}

#[test]
fn origin_patterns_change_alters_hash() {
    let a = Config::default();
    let mut b = Config::default();
    b.extension.origin_patterns = Some(vec!["localhost".to_string(), "*.example".to_string()]);
    assert_ne!(baked_config_hash(&a).unwrap(), baked_config_hash(&b).unwrap());
}

#[test]
fn server_host_change_alters_hash() {
    let a = Config::default();
    let mut b = Config::default();
    b.server.host = "ripr.example.ts.net".to_string();
    assert_ne!(baked_config_hash(&a).unwrap(), baked_config_hash(&b).unwrap());
}

#[test]
fn invalid_config_is_an_error_not_a_hash() {
    let bad = config_with_endpoints(&[]);
    assert!(baked_config_hash(&bad).is_err());
}

#[test]
fn hash_input_manifest_has_blank_version() {
    // baked_config_hash renders at version "", so a version bump alone cannot change it.
    let manifest = extension::render_manifest("", &Config::default()).unwrap();
    let json: serde_json::Value = serde_json::from_str(&manifest).unwrap();
    assert_eq!(json["version"], "");
}

#[test]
fn reuse_with_matching_sidecar_returns_the_xpi() {
    let config = config_with_endpoints(&["http://ripr.lan:8181"]);
    let (root, artifacts) = fake_repo("9.9.9");
    write_sidecar(&artifacts, "9.9.9", &config);
    let result = run(root.path(), &config, "9.9.9").expect("matching sidecar reuses");
    assert!(result.xpi_path.ends_with("obsidian-borg-9.9.9.xpi"));
}

#[test]
fn reuse_with_changed_endpoints_is_config_mismatch() {
    let signed = config_with_endpoints(&["http://ripr.lan:8181"]);
    let now = config_with_endpoints(&["http://ripr.example.ts.net:8181", "http://ripr.lan:8181"]);
    let (root, artifacts) = fake_repo("9.9.9");
    write_sidecar(&artifacts, "9.9.9", &signed);
    let err = run(root.path(), &now, "9.9.9").unwrap_err();
    assert!(err.downcast_ref::<ConfigMismatch>().is_some(), "{err:#}");
    assert!(err.to_string().contains("bump the version to re-sign"), "{err}");
}

#[test]
fn reuse_with_missing_sidecar_is_config_mismatch() {
    let config = Config::default();
    let (root, _artifacts) = fake_repo("9.9.9");
    let err = run(root.path(), &config, "9.9.9").unwrap_err();
    assert!(err.downcast_ref::<ConfigMismatch>().is_some(), "{err:#}");
    assert!(err.to_string().contains("bump the version to re-sign"), "{err}");
}
