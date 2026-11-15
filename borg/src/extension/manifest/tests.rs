use super::*;
use crate::config::Config;

#[test]
fn version_threads_through_from_parameter() {
    let manifest = build_manifest("0.0.0-test", &Config::default());
    assert_eq!(manifest["version"].as_str(), Some("0.0.0-test"));
}

#[test]
fn manifest_version_is_3() {
    let manifest = build_manifest("0.0.0-test", &Config::default());
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
    let manifest = build_manifest("0.0.0-test", &Config::default());
    for key in [
        "manifest_version",
        "name",
        "version",
        "icons",
        "action",
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

#[test]
fn permissions_are_exactly_activetab_storage_notifications() {
    let manifest = build_manifest("0.0.0-test", &Config::default());
    let perms: Vec<&str> = manifest["permissions"]
        .as_array()
        .expect("permissions array")
        .iter()
        .map(|v| v.as_str().expect("string"))
        .collect();
    assert_eq!(perms, vec!["activeTab", "storage", "notifications"]);
}

#[test]
fn default_host_permissions_match_default_origins() {
    let manifest = build_manifest("0.0.0-test", &Config::default());
    let hosts: Vec<&str> = manifest["host_permissions"]
        .as_array()
        .expect("host_permissions array")
        .iter()
        .map(|v| v.as_str().expect("string"))
        .collect();
    assert_eq!(hosts, vec!["http://localhost/*", "http://*.lan/*", "http://*.local/*"]);
}

#[test]
fn gecko_id_is_obsidian_borg_scottidler() {
    let manifest = build_manifest("0.0.0-test", &Config::default());
    assert_eq!(
        manifest["browser_specific_settings"]["gecko"]["id"].as_str(),
        Some("obsidian-borg@scottidler")
    );
}

#[test]
fn gecko_strict_min_version_is_140() {
    let manifest = build_manifest("0.0.0-test", &Config::default());
    assert_eq!(
        manifest["browser_specific_settings"]["gecko"]["strict_min_version"].as_str(),
        Some("140.0")
    );
}

#[test]
fn execute_action_suggested_key_is_alt_shift_b() {
    let manifest = build_manifest("0.0.0-test", &Config::default());
    assert_eq!(
        manifest["commands"]["_execute_action"]["suggested_key"]["default"].as_str(),
        Some("Alt+Shift+B")
    );
}

#[test]
fn action_opens_popup_and_background_is_removed() {
    let manifest = build_manifest("0.0.0-test", &Config::default());
    // The toolbar action opens a fresh popup per click, so capture does not
    // depend on a long-lived background context (which can die mid-session).
    assert_eq!(manifest["action"]["default_popup"].as_str(), Some("popup.html"));
    assert!(
        manifest.get("background").is_none(),
        "background context must be removed; its presence reintroduces the crashable capture path"
    );
}

fn config_with_endpoints(endpoints: &[&str]) -> Config {
    let mut config = Config::default();
    config.extension.endpoints = Some(endpoints.iter().map(|s| s.to_string()).collect());
    config
}

#[test]
fn endpoint_host_is_added_to_host_permissions_and_csp_with_defaults_kept() {
    let config = config_with_endpoints(&["http://ripr.example.ts.net:8181", "http://ripr.lan:8181"]);
    let patterns = origin_patterns(&config);
    for default in DEFAULT_ORIGIN_PATTERNS {
        assert!(patterns.iter().any(|p| p == default), "{default} missing: {patterns:?}");
    }
    let manifest = build_manifest("0.0.0-test", &config);
    let hosts: Vec<&str> = manifest["host_permissions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(hosts.contains(&"http://ripr.example.ts.net/*"), "{hosts:?}");
    let csp = manifest["content_security_policy"]["extension_pages"].as_str().unwrap();
    assert!(csp.contains("http://ripr.example.ts.net:*"), "{csp}");
}

#[test]
fn endpoint_hosts_are_added_to_explicit_origin_patterns() {
    let mut config = config_with_endpoints(&["http://ripr.example.ts.net:8181"]);
    config.extension.origin_patterns = Some(vec!["borg.example.com".to_string()]);
    let patterns = origin_patterns(&config);
    assert_eq!(
        patterns,
        vec!["borg.example.com".to_string(), "ripr.example.ts.net".to_string()]
    );
}

#[test]
fn lan_endpoint_adds_no_pattern_because_wildcard_covers_it() {
    let patterns = origin_patterns(&config_with_endpoints(&["http://ripr.lan:8181"]));
    assert_eq!(patterns, origin_patterns(&Config::default()));
}

#[test]
fn endpoint_already_listed_exactly_is_not_duplicated() {
    let mut config = config_with_endpoints(&["http://borg.example.com:8181", "http://borg.example.com:9000"]);
    config.extension.origin_patterns = Some(vec!["borg.example.com".to_string()]);
    assert_eq!(origin_patterns(&config), vec!["borg.example.com".to_string()]);
}

#[test]
fn two_endpoints_on_one_new_host_add_one_pattern() {
    let config = config_with_endpoints(&["http://ripr.ts.net:8181", "http://ripr.ts.net:9000"]);
    let count = origin_patterns(&config).iter().filter(|p| *p == "ripr.ts.net").count();
    assert_eq!(count, 1);
}

#[test]
fn invalid_endpoints_add_no_pattern() {
    let patterns = origin_patterns(&config_with_endpoints(&["https://ripr.example.ts.net:8181"]));
    assert_eq!(patterns, origin_patterns(&Config::default()));
}

#[test]
fn unset_endpoints_leave_explicit_origin_patterns_untouched() {
    let mut config = Config::default();
    config.extension.origin_patterns = Some(vec!["borg.example.com".to_string()]);
    assert_eq!(origin_patterns(&config), vec!["borg.example.com".to_string()]);
}

#[test]
fn apex_endpoint_under_wildcard_is_added_explicitly() {
    let mut config = config_with_endpoints(&["http://example.com:8181"]);
    config.extension.origin_patterns = Some(vec!["*.example.com".to_string()]);
    assert_eq!(
        origin_patterns(&config),
        vec!["*.example.com".to_string(), "example.com".to_string()]
    );
    let manifest = build_manifest("0.0.0-test", &config);
    let hosts: Vec<&str> = manifest["host_permissions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(hosts.contains(&"http://example.com/*"), "{hosts:?}");
    let csp = manifest["content_security_policy"]["extension_pages"].as_str().unwrap();
    assert!(csp.contains("http://example.com:*"), "{csp}");
}

#[test]
fn permits_matches_exact_and_strict_subdomain_but_not_apex() {
    let patterns = vec!["*.lan".to_string(), "localhost".to_string()];
    assert!(permits(&patterns, "ripr.lan"));
    assert!(permits(&patterns, "localhost"));
    assert!(!permits(&patterns, "lan"));
    assert!(!permits(&patterns, "ripr.local"));
}
