use super::*;
use crate::config::Config;

#[test]
fn version_matches_crate_version() {
    let manifest = build_manifest(&Config::default());
    assert_eq!(manifest["version"].as_str(), Some(env!("CARGO_PKG_VERSION")));
}

#[test]
fn manifest_version_is_3() {
    let manifest = build_manifest(&Config::default());
    assert_eq!(manifest["manifest_version"].as_u64(), Some(3));
}

#[test]
fn host_permissions_and_csp_agree_on_origins() {
    let config = Config::default();
    let patterns = origin_patterns(&config);
    let host_perms = host_permissions(&patterns);
    let csp = csp_extension_pages(&patterns);
    for pat in &patterns {
        assert!(
            host_perms.contains(&format!("http://{pat}/*")),
            "host_permissions missing http://{pat}/*; got {host_perms:?}"
        );
        assert!(
            csp.contains(&format!("http://{pat}:*")),
            "CSP missing http://{pat}:*; got {csp:?}"
        );
    }
}

#[test]
fn default_patterns_present_with_default_config() {
    let patterns = origin_patterns(&Config::default());
    for default in DEFAULT_ORIGIN_PATTERNS {
        assert!(
            patterns.iter().any(|p| p == default),
            "default pattern {default} missing from {patterns:?}"
        );
    }
}

#[test]
fn explicit_origin_patterns_override_defaults() {
    let mut config = Config::default();
    config.extension.origin_patterns = Some(vec!["100.64.0.0/10".to_string(), "borg.tail-net.ts.net".to_string()]);
    let patterns = origin_patterns(&config);
    assert_eq!(
        patterns,
        vec!["100.64.0.0/10".to_string(), "borg.tail-net.ts.net".to_string()]
    );
}

#[test]
fn server_host_merged_with_defaults_when_not_covered_by_wildcard() {
    let mut config = Config::default();
    config.server.host = "borg.example.com".to_string();
    let patterns = origin_patterns(&config);
    assert!(
        patterns.contains(&"borg.example.com".to_string()),
        "expected server.host to be merged in; got {patterns:?}"
    );
}

#[test]
fn server_host_omitted_when_covered_by_wildcard_default() {
    let mut config = Config::default();
    config.server.host = "desk.lan".to_string();
    let patterns = origin_patterns(&config);
    assert!(
        !patterns.contains(&"desk.lan".to_string()),
        "desk.lan is covered by *.lan, should not be added: {patterns:?}"
    );
    assert!(patterns.contains(&"*.lan".to_string()));
}

#[test]
fn server_host_bind_addresses_are_ignored() {
    for bind in ["0.0.0.0", "127.0.0.1"] {
        let mut config = Config::default();
        config.server.host = bind.to_string();
        let patterns = origin_patterns(&config);
        assert!(
            !patterns.iter().any(|p| p == bind),
            "{bind} is a bind address, not a reachable host: {patterns:?}"
        );
    }
}

#[test]
fn empty_explicit_falls_back_to_defaults() {
    let mut config = Config::default();
    config.extension.origin_patterns = Some(vec![]);
    let patterns = origin_patterns(&config);
    for default in DEFAULT_ORIGIN_PATTERNS {
        assert!(patterns.iter().any(|p| p == default));
    }
}

#[test]
fn manifest_contains_required_top_level_keys() {
    let manifest = build_manifest(&Config::default());
    for key in [
        "manifest_version",
        "name",
        "version",
        "icons",
        "action",
        "background",
        "permissions",
        "host_permissions",
        "content_security_policy",
        "options_ui",
        "commands",
        "browser_specific_settings",
    ] {
        assert!(manifest.get(key).is_some(), "missing manifest key {key}");
    }
}

#[test]
fn csp_starts_with_default_src_self() {
    let csp = csp_extension_pages(&origin_patterns(&Config::default()));
    assert!(
        csp.starts_with("default-src 'self'; connect-src "),
        "unexpected CSP format: {csp:?}"
    );
}
