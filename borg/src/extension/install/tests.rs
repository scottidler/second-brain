use super::*;
use serde_json::json;

#[test]
fn merge_policy_into_empty_creates_our_entry() {
    let merged = merge_policy(json!({}), "file:///tmp/x.xpi");
    let entry = &merged["policies"]["ExtensionSettings"][EXTENSION_ID];
    assert_eq!(entry["install_url"], "file:///tmp/x.xpi");
    assert_eq!(entry["installation_mode"], "force_installed");
    assert_eq!(entry["updates_disabled"], false);
    assert_eq!(entry["default_area"], "navbar");
}

#[test]
fn merge_policy_preserves_unrelated_top_level_policies() {
    let existing = json!({
        "policies": {
            "Certificates": { "ImportEnterpriseRoots": true },
            "ExtensionSettings": {
                "ublock0@raymondhill.net": {
                    "installation_mode": "force_installed",
                    "install_url": "https://addons.mozilla.org/firefox/downloads/latest/ublock-origin/latest.xpi"
                }
            }
        }
    });
    let merged = merge_policy(existing, "file:///x.xpi");
    assert_eq!(merged["policies"]["Certificates"]["ImportEnterpriseRoots"], true);
    assert_eq!(
        merged["policies"]["ExtensionSettings"]["ublock0@raymondhill.net"]["installation_mode"], "force_installed",
        "unrelated extension entry must be preserved"
    );
    assert_eq!(
        merged["policies"]["ExtensionSettings"][EXTENSION_ID]["install_url"],
        "file:///x.xpi"
    );
}

#[test]
fn merge_policy_handles_corrupt_or_missing_extension_settings() {
    let existing = json!({ "policies": { "ExtensionSettings": "garbage" } });
    let merged = merge_policy(existing, "file:///x.xpi");
    assert_eq!(
        merged["policies"]["ExtensionSettings"][EXTENSION_ID]["install_url"],
        "file:///x.xpi"
    );
}

#[test]
fn merge_policy_replaces_existing_obsidian_borg_entry() {
    let existing = json!({
        "policies": {
            "ExtensionSettings": {
                EXTENSION_ID: {
                    "installation_mode": "blocked"
                }
            }
        }
    });
    let merged = merge_policy(existing, "file:///x.xpi");
    let entry = &merged["policies"]["ExtensionSettings"][EXTENSION_ID];
    assert_eq!(entry["installation_mode"], "force_installed", "replaces stale entry");
    assert_eq!(entry["install_url"], "file:///x.xpi");
}

#[test]
fn strip_policy_removes_our_entry_and_keeps_others() {
    let existing = json!({
        "policies": {
            "ExtensionSettings": {
                EXTENSION_ID: { "installation_mode": "force_installed" },
                "other@ext": { "installation_mode": "blocked" }
            },
            "Certificates": { "ImportEnterpriseRoots": true }
        }
    });
    let stripped = strip_policy(existing);
    assert!(stripped["policies"]["ExtensionSettings"].get(EXTENSION_ID).is_none());
    assert_eq!(
        stripped["policies"]["ExtensionSettings"]["other@ext"]["installation_mode"],
        "blocked"
    );
    assert_eq!(stripped["policies"]["Certificates"]["ImportEnterpriseRoots"], true);
}

#[test]
fn strip_policy_collapses_empty_extension_settings() {
    let existing = json!({
        "policies": {
            "ExtensionSettings": {
                EXTENSION_ID: { "installation_mode": "force_installed" }
            }
        }
    });
    let stripped = strip_policy(existing);
    assert!(
        stripped["policies"].get("ExtensionSettings").is_none(),
        "ExtensionSettings should be removed when it would otherwise be empty"
    );
}

#[test]
fn policy_path_routes_per_install_type() {
    assert_eq!(
        policy_path(&FirefoxInstall::Tarball(PathBuf::from("/opt/firefox"))).expect("tarball policy_path"),
        PathBuf::from("/opt/firefox/distribution/policies.json")
    );
    assert_eq!(
        policy_path(&FirefoxInstall::AptOrDeb).expect("apt policy_path"),
        PathBuf::from("/etc/firefox/policies/policies.json")
    );
    assert!(policy_path(&FirefoxInstall::Snap).is_err());
    assert!(policy_path(&FirefoxInstall::Unknown).is_err());
}

#[test]
fn install_strategy_picks_policy_for_system_firefox() {
    assert!(matches!(
        install_strategy(&FirefoxInstall::Tarball(PathBuf::from("/opt/firefox"))).expect("tarball strategy"),
        InstallStrategy::PolicyFile { .. }
    ));
    assert!(matches!(
        install_strategy(&FirefoxInstall::AptOrDeb).expect("apt strategy"),
        InstallStrategy::PolicyFile { .. }
    ));
    assert!(matches!(
        install_strategy(&FirefoxInstall::Flatpak).expect("flatpak strategy"),
        InstallStrategy::PolicyFile { .. }
    ));
}

#[test]
fn install_strategy_errors_on_unknown_firefox() {
    assert!(install_strategy(&FirefoxInstall::Unknown).is_err());
}

#[test]
fn parse_default_profile_path_picks_default_flagged_profile() {
    // The realistic snap-Firefox profiles.ini from this machine.
    let ini = "[Profile0]\n\
               Name=default\n\
               IsRelative=1\n\
               Path=qokp77y8.default-1764019811616\n\
               Default=1\n\
               \n\
               [General]\n\
               StartWithLastProfile=1\n\
               Version=2\n";
    assert_eq!(
        parse_default_profile_path(ini).as_deref(),
        Some("qokp77y8.default-1764019811616")
    );
}

#[test]
fn parse_default_profile_path_prefers_default_over_first() {
    // Profile1 has Default=1 even though Profile0 comes first.
    let ini = "[Profile0]\n\
               Name=older\n\
               IsRelative=1\n\
               Path=aaaaaaaa.older\n\
               \n\
               [Profile1]\n\
               Name=current\n\
               IsRelative=1\n\
               Path=bbbbbbbb.current\n\
               Default=1\n";
    assert_eq!(parse_default_profile_path(ini).as_deref(), Some("bbbbbbbb.current"));
}

#[test]
fn parse_default_profile_path_falls_back_to_first_when_none_marked_default() {
    let ini = "[Profile0]\n\
               Name=only\n\
               IsRelative=1\n\
               Path=cccccccc.only\n";
    assert_eq!(parse_default_profile_path(ini).as_deref(), Some("cccccccc.only"));
}

#[test]
fn parse_default_profile_path_returns_none_when_no_profile_sections() {
    let ini = "[General]\nStartWithLastProfile=1\nVersion=2\n";
    assert_eq!(parse_default_profile_path(ini), None);
}

#[test]
fn parse_default_profile_path_ignores_install_section_keys() {
    // [Install<hash>] sections also contain Default= but should not be confused
    // for profile sections. Our parser only reads keys when the current section
    // name starts with "Profile".
    let ini = "[Install4F96D1932A9F858E]\n\
               Default=Profiles/aaaaaaaa.fake\n\
               Locked=1\n\
               \n\
               [Profile0]\n\
               Name=real\n\
               IsRelative=1\n\
               Path=bbbbbbbb.real\n\
               Default=1\n";
    assert_eq!(parse_default_profile_path(ini).as_deref(), Some("bbbbbbbb.real"));
}

#[test]
fn requires_sudo_for_system_paths_only() {
    assert!(requires_sudo(Path::new("/etc/firefox/policies/policies.json")));
    assert!(requires_sudo(Path::new("/opt/firefox/distribution/policies.json")));
    assert!(!requires_sudo(Path::new(
        "/home/saidler/.var/app/org.mozilla.firefox/.mozilla/firefox/policies/policies.json"
    )));
    assert!(!requires_sudo(Path::new("/tmp/policies.json")));
}

#[test]
fn build_policy_entry_has_required_fields() {
    let entry = build_policy_entry("file:///x.xpi");
    for key in ["installation_mode", "install_url", "updates_disabled", "default_area"] {
        assert!(entry.get(key).is_some(), "missing field {key}");
    }
}

#[test]
fn atomic_symlink_swap_writes_relative_target() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let artifacts = tmp.path();
    let versioned = artifacts.join("obsidian-borg-0.8.12.xpi");
    std::fs::write(&versioned, b"fake xpi").expect("write fake xpi");
    let latest = artifacts.join(LATEST_XPI_NAME);

    atomic_symlink_swap(&versioned, &latest).expect("swap");
    let resolved = std::fs::read_link(&latest).expect("readlink");
    assert_eq!(resolved, PathBuf::from("obsidian-borg-0.8.12.xpi"));

    // Swap again to a different version - must succeed (atomic-rename over existing symlink).
    let versioned_b = artifacts.join("obsidian-borg-0.8.13.xpi");
    std::fs::write(&versioned_b, b"newer fake xpi").expect("write newer");
    atomic_symlink_swap(&versioned_b, &latest).expect("re-swap");
    let resolved2 = std::fs::read_link(&latest).expect("readlink2");
    assert_eq!(resolved2, PathBuf::from("obsidian-borg-0.8.13.xpi"));
}

#[test]
fn install_url_uses_file_scheme_and_repo_relative_path() {
    let repo = PathBuf::from("/home/u/second-brain");
    let url = install_url_for(&repo);
    assert!(url.starts_with("file:///home/u/second-brain/borg/clients/extension/web-ext-artifacts/"));
    assert!(url.ends_with(LATEST_XPI_NAME));
}
