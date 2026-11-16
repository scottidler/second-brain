use borg::config::Config;
use borg::extension::{self, extension_dir};

/// Regression guard: `keepalive: true` on the popup's POST silently breaks capture on
/// snap Firefox (the POST never reaches the daemon - zero receipts, zero daemon log).
/// This was removed in 1c3deb0, wrongly "restored per spec" in 4556577, and removed
/// again for good. The daemon is fire-and-forget (returns "Queued" in ~17ms), so the
/// POST completes before any focus-loss close and keepalive is unnecessary. If this test
/// fails, someone re-added keepalive - do NOT; see borg/clients/extension/popup.js header.
/// The fetch itself lives in fallback.js (`postWithFallback`), so both files are scanned.
#[test]
fn popup_js_must_not_use_fetch_keepalive() {
    let repo_root = extension::repo_root().expect("locate repo root");
    for file in ["popup.js", "fallback.js"] {
        let source = std::fs::read_to_string(extension_dir(&repo_root).join(file))
            .unwrap_or_else(|e| panic!("read {file}: {e}"));
        // Truncate each line at the first `//` so comments explaining the ban (header block
        // and trailing notes) do not trip the guard. URLs contain `//` but never the word
        // "keepalive".
        let code: String = source
            .lines()
            .map(|line| match line.find("//") {
                Some(i) => &line[..i],
                None => line,
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !code.contains("keepalive"),
            "{file} re-introduced `keepalive` on the fetch - it breaks capture on snap Firefox. \
             Remove it (see the popup.js header comment and commit 1c3deb0)."
        );
    }
}

#[test]
fn stage_materialises_manifest_schema_and_static_assets() {
    let tempdir = tempfile::TempDir::new().expect("create tempdir");
    let result =
        extension::stage(tempdir.path(), "0.0.0-test", &Config::default()).expect("stage extension into tempdir");

    // Confirm stage returned the directory we asked for.
    assert_eq!(result.target_dir, tempdir.path());

    // Manifest + schema present.
    let manifest_path = tempdir.path().join("manifest.json");
    assert!(manifest_path.exists(), "manifest.json missing from staged dir");
    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&manifest_path).expect("read manifest"))
            .expect("parse manifest as JSON");
    assert_eq!(manifest["manifest_version"].as_u64(), Some(3));
    assert_eq!(manifest["version"].as_str(), Some("0.0.0-test"));

    let schema_path = tempdir.path().join("ingest-schema.json");
    assert!(schema_path.exists(), "ingest-schema.json missing from staged dir");

    // Static assets copied from source tree verbatim.
    for asset in [
        "popup.html",
        "popup.js",
        "popup.css",
        "fallback.js",
        "daemon.js",
        "options.html",
        "options.js",
        "options.css",
        "icons/locutus-16.png",
        "icons/locutus-48.png",
        "icons/locutus-128.png",
    ] {
        let dst = tempdir.path().join(asset);
        assert!(dst.exists(), "static asset {asset} missing from staged dir");
    }

    // .amo-upload-uuid carries the AMO listing identity; must be byte-equal to source
    // or a future re-sign would create a brand-new AMO listing.
    let repo_root = extension::repo_root().expect("locate repo root");
    let source_amo = extension_dir(&repo_root).join(".amo-upload-uuid");
    let staged_amo = tempdir.path().join(".amo-upload-uuid");
    assert!(staged_amo.exists(), ".amo-upload-uuid missing from staged dir");
    assert_eq!(
        std::fs::read(&source_amo).expect("read source .amo-upload-uuid"),
        std::fs::read(&staged_amo).expect("read staged .amo-upload-uuid"),
        ".amo-upload-uuid must be byte-equal to source - drift would orphan the AMO listing"
    );
}

/// The endpoint list is baked in at stage time, so no script may read the legacy `endpoint`
/// storage key. The one allowed mention is the options page deleting the stale value.
#[test]
fn staged_scripts_never_read_the_endpoint_storage_key() {
    let tempdir = tempfile::TempDir::new().expect("create tempdir");
    extension::stage(tempdir.path(), "0.0.0-test", &Config::default()).expect("stage extension into tempdir");
    for name in ["popup.js", "options.js", "fallback.js"] {
        let source = std::fs::read_to_string(tempdir.path().join(name)).expect("read staged script");
        for quoted in ["\"endpoint\"", "'endpoint'"] {
            for (index, _) in source.match_indices(quoted) {
                let line = source[..index].rsplit('\n').next().unwrap_or("");
                let line_end = source[index..].find('\n').map_or(source.len(), |n| index + n);
                let full_line = format!("{line}{}", &source[index..line_end]);
                assert!(
                    full_line.contains("chrome.storage.local.remove(\"endpoint\")"),
                    "{name} uses the {quoted} storage key outside the cleanup remove(): {full_line}"
                );
            }
        }
    }
}

fn endpoint_config() -> Config {
    let mut config = Config::default();
    config.extension.endpoints = Some(vec![
        "http://ripr.example.ts.net:8181".to_string(),
        "http://ripr.lan:8181".to_string(),
    ]);
    config
}

#[test]
fn staged_daemon_js_lists_endpoints_in_config_order_and_hosts_are_permitted() {
    let tempdir = tempfile::TempDir::new().expect("create tempdir");
    extension::stage(tempdir.path(), "0.0.0-test", &endpoint_config()).expect("stage");
    let daemon_js = std::fs::read_to_string(tempdir.path().join("daemon.js")).expect("read daemon.js");
    let tailnet = daemon_js
        .find("http://ripr.example.ts.net:8181")
        .expect("tailnet endpoint");
    let lan = daemon_js.find("http://ripr.lan:8181").expect("lan endpoint");
    assert!(tailnet < lan, "endpoints must keep config order:\n{daemon_js}");

    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(tempdir.path().join("manifest.json")).unwrap()).unwrap();
    let permissions: Vec<&str> = manifest["host_permissions"]
        .as_array()
        .expect("host_permissions")
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    for host in ["ripr.example.ts.net", "ripr.lan"] {
        let covered = permissions.iter().any(|p| {
            let pattern_host = p.trim_start_matches("http://").trim_end_matches("/*");
            pattern_host == host
                || pattern_host
                    .strip_prefix("*.")
                    .is_some_and(|suffix| host.ends_with(&format!(".{suffix}")))
        });
        assert!(covered, "{host} not covered by host_permissions {permissions:?}");
    }
}

#[test]
fn stage_fails_on_invalid_endpoints_before_writing() {
    let tempdir = tempfile::TempDir::new().expect("create tempdir");
    let mut config = Config::default();
    config.extension.endpoints = Some(vec!["https://ripr.lan:8181".to_string()]);
    let err = extension::stage(tempdir.path(), "0.0.0-test", &config)
        .unwrap_err()
        .to_string();
    assert!(err.contains("http"), "{err}");
    assert!(
        !tempdir.path().join("manifest.json").exists(),
        "nothing may be staged on a bad config"
    );
}

/// Regression guard: the toolbar click is a one-click capture. v0.17.0 (c1f1632) added a
/// note textarea and a Capture button, so the popup stopped posting on open and every
/// capture took a second keypress. Scott does not want that, ever. If this fails, someone
/// put an input step back in the popup - take it out.
#[test]
fn popup_captures_on_open_with_no_input_step() {
    let repo_root = extension::repo_root().expect("locate repo root");
    let dir = extension_dir(&repo_root);
    let html = std::fs::read_to_string(dir.join("popup.html")).expect("read popup.html");
    for tag in ["<textarea", "<input", "<button", "<form"] {
        assert!(
            !html.contains(tag),
            "popup.html has `{tag}`: the popup must capture on open"
        );
    }
    let js = std::fs::read_to_string(dir.join("popup.js")).expect("read popup.js");
    assert!(
        js.contains(r#"document.addEventListener("DOMContentLoaded", capture);"#),
        "popup.js must call capture() on DOMContentLoaded"
    );
}

#[test]
fn staged_html_loads_daemon_and_fallback_before_own_script() {
    let tempdir = tempfile::TempDir::new().expect("create tempdir");
    extension::stage(tempdir.path(), "0.0.0-test", &Config::default()).expect("stage");
    for (html, own) in [("popup.html", "popup.js"), ("options.html", "options.js")] {
        let body = std::fs::read_to_string(tempdir.path().join(html)).expect("read html");
        let at = |name: &str| {
            body.find(&format!("src=\"{name}\""))
                .unwrap_or_else(|| panic!("{html} does not load {name}"))
        };
        assert!(
            at("daemon.js") < at("fallback.js"),
            "{html}: daemon.js before fallback.js"
        );
        assert!(at("fallback.js") < at(own), "{html}: fallback.js before {own}");
    }
}
