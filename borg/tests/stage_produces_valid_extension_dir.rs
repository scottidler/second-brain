use borg::config::Config;
use borg::extension::{self, extension_dir};

/// Regression guard: `keepalive: true` on the popup's POST silently breaks capture on
/// snap Firefox (the POST never reaches the daemon - zero receipts, zero daemon log).
/// This was removed in 1c3deb0, wrongly "restored per spec" in 4556577, and removed
/// again for good. The daemon is fire-and-forget (returns "Queued" in ~17ms), so the
/// POST completes before any focus-loss close and keepalive is unnecessary. If this test
/// fails, someone re-added keepalive - do NOT; see borg/clients/extension/popup.js header.
#[test]
fn popup_js_must_not_use_fetch_keepalive() {
    let repo_root = extension::repo_root().expect("locate repo root");
    let popup_js = std::fs::read_to_string(extension_dir(&repo_root).join("popup.js")).expect("read popup.js");
    // Truncate each line at the first `//` so comments explaining the ban (header block and
    // trailing notes) do not trip the guard. URLs contain `//` but never the word "keepalive".
    let code: String = popup_js
        .lines()
        .map(|line| match line.find("//") {
            Some(i) => &line[..i],
            None => line,
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !code.contains("keepalive"),
        "popup.js re-introduced `keepalive` on the fetch - it breaks capture on snap Firefox. \
         Remove it (see the popup.js header comment and commit 1c3deb0)."
    );
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
