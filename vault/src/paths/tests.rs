#![allow(clippy::unwrap_used)]

use super::*;
use std::path::PathBuf;
use std::sync::Mutex;
use tempfile::TempDir;

/// `set_current_dir` mutates process-global state; serialize the CWD-mutating
/// tests so a concurrent test's CWD cannot bleed into `resolve_vault_root`'s
/// marker walk (which would make the marker-less test find a marker and fail).
static CWD_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn cli_override_wins_when_both_set() {
    let cli = PathBuf::from("/tmp/cli-vault");
    let resolved = resolve_vault_root(Some(&cli), Some("/tmp/config-vault")).unwrap();
    assert_eq!(resolved, cli);
}

#[test]
fn config_wins_when_cli_is_none() {
    let resolved = resolve_vault_root(None, Some("/tmp/config-vault")).unwrap();
    assert_eq!(resolved, PathBuf::from("/tmp/config-vault"));
}

#[test]
fn config_value_expands_tilde() {
    let resolved = resolve_vault_root(None, Some("~/vault")).unwrap();
    let home = std::env::var("HOME").unwrap();
    assert_eq!(resolved, PathBuf::from(home).join("vault"));
}

#[test]
fn cwd_with_obsidian_marker_wins_when_both_none() {
    let _cwd = CWD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = TempDir::new().unwrap();
    std::fs::create_dir(tmp.path().join(".obsidian")).unwrap();
    // Canonicalize the tmp path because std::env::current_dir() returns the canonical form
    // on macOS where /tmp is a symlink to /private/tmp.
    let expected = std::fs::canonicalize(tmp.path()).unwrap();

    let prev = std::env::current_dir().unwrap();
    std::env::set_current_dir(tmp.path()).unwrap();
    let resolved = resolve_vault_root(None, None);
    std::env::set_current_dir(prev).unwrap();

    let r = resolved.unwrap();
    assert_eq!(std::fs::canonicalize(&r).unwrap(), expected);
}

#[test]
fn cwd_without_marker_errors() {
    let _cwd = CWD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = TempDir::new().unwrap();

    let prev = std::env::current_dir().unwrap();
    std::env::set_current_dir(tmp.path()).unwrap();
    let resolved = resolve_vault_root(None, None);
    std::env::set_current_dir(prev).unwrap();

    let err = resolved.unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("vault root not set"), "unexpected: {msg}");
    assert!(msg.contains("--vault"), "unexpected: {msg}");
    assert!(msg.contains(".obsidian/"), "unexpected: {msg}");
}

#[test]
fn config_root_lands_under_dirs_config_dir() {
    let root = config_root();
    let expected_parent = dirs::config_dir().unwrap();
    assert_eq!(root, expected_parent.join("sb"));
}

#[test]
fn expand_tilde_expands_leading_tilde() {
    let expanded = expand_tilde("~/foo/bar");
    let home = std::env::var("HOME").unwrap();
    assert_eq!(expanded, PathBuf::from(home).join("foo/bar"));
}

#[test]
fn expand_tilde_passes_absolute_path_through() {
    let expanded = expand_tilde("/etc/passwd");
    assert_eq!(expanded, PathBuf::from("/etc/passwd"));
}

#[test]
fn expand_tilde_passes_relative_path_through() {
    let expanded = expand_tilde("relative/path");
    assert_eq!(expanded, PathBuf::from("relative/path"));
}

#[test]
fn deserialize_tilde_pathbuf_expands_tilde_in_yaml() {
    #[derive(serde::Deserialize)]
    struct Wrapper {
        #[serde(deserialize_with = "super::deserialize_tilde_pathbuf")]
        path: PathBuf,
    }
    let yaml = "path: ~/.local/share/borg/stages\n";
    let parsed: Wrapper = serde_yaml::from_str(yaml).unwrap();
    let home = std::env::var("HOME").unwrap();
    assert_eq!(parsed.path, PathBuf::from(home).join(".local/share/borg/stages"));
}

#[test]
fn dir_size_sums_nested_files() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("a.txt"), vec![0u8; 10]).unwrap();
    std::fs::create_dir(tmp.path().join("sub")).unwrap();
    std::fs::write(tmp.path().join("sub").join("b.txt"), vec![0u8; 20]).unwrap();

    assert_eq!(dir_size(tmp.path()), 30);
}

#[test]
fn dir_size_of_missing_dir_is_zero() {
    let tmp = TempDir::new().unwrap();
    assert_eq!(dir_size(&tmp.path().join("does-not-exist")), 0);
}

#[test]
fn oracle_paths_land_under_the_sb_data_namespace() {
    // R1: the oracle DB moved from `~/.local/share/oracle/` into the same
    // `sb/` namespace borg's data already lives in. `legacy_oracle_dir` is
    // the pre-move location, and must NOT follow.
    let db = oracle_db_path();
    assert!(
        db.ends_with("sb/oracle/oracle.db"),
        "oracle_db_path() = {}",
        db.display()
    );
    let cache = oracle_eval_cache_path();
    assert!(
        cache.ends_with("sb/oracle/eval-cache.db"),
        "oracle_eval_cache_path() = {}",
        cache.display()
    );
    assert_eq!(db.parent(), cache.parent());

    let legacy = legacy_oracle_dir();
    assert!(legacy.ends_with("oracle"), "legacy_oracle_dir() = {}", legacy.display());
    assert!(
        !legacy.ends_with("sb/oracle"),
        "legacy_oracle_dir() must stay the pre-move path, got {}",
        legacy.display()
    );
    assert_ne!(legacy, db.parent().unwrap());
}

#[test]
fn all_config_files_land_under_config_root() {
    let root = config_root();
    assert_eq!(borg_config(), root.join("borg.yml"));
    assert_eq!(cortex_config(), root.join("cortex.yml"));
    assert_eq!(oracle_config(), root.join("oracle.yml"));
    assert_eq!(canonical_tags(), root.join("canonical-tags.yml"));
    assert_eq!(tag_mapping(), root.join("tag-mapping.yml"));
    assert_eq!(tag_proposals(), root.join("tag-proposals.yml"));
    assert_eq!(patterns_dir(), root.join("patterns"));
}

/// Build a directory that structurally looks like this workspace checkout.
fn fake_second_brain_checkout(root: &std::path::Path) {
    std::fs::write(root.join("Cargo.toml"), "[workspace]\n").unwrap();
    for member in ["cortex", "borg", "vault"] {
        std::fs::create_dir_all(root.join(member)).unwrap();
    }
}

#[test]
fn cli_override_pointing_at_the_source_tree_is_refused() {
    // 2026-08-15: `sb bootstrap` baked its CWD into the unit's --vault and
    // cortex rewrote 203 files of its own source. The override is the highest
    // precedence input, so it is exactly the one that must be checked.
    let tmp = TempDir::new().unwrap();
    fake_second_brain_checkout(tmp.path());

    let err = resolve_vault_root(Some(tmp.path()), None).unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("refusing to use the second-brain source tree"),
        "unexpected: {msg}"
    );
}

#[test]
fn config_value_pointing_at_the_source_tree_is_refused() {
    let tmp = TempDir::new().unwrap();
    fake_second_brain_checkout(tmp.path());

    let err = resolve_vault_root(None, Some(tmp.path().to_str().unwrap())).unwrap_err();
    assert!(
        format!("{err}").contains("refusing to use the second-brain source tree"),
        "config path must be checked too"
    );
}

#[test]
fn a_real_vault_directory_is_still_accepted() {
    // The guard is structural: a directory with none of the member crates is a
    // normal vault and must pass untouched.
    let tmp = TempDir::new().unwrap();
    let resolved = resolve_vault_root(Some(tmp.path()), None).unwrap();
    assert_eq!(resolved, tmp.path());
}

#[test]
fn a_vault_that_merely_shares_one_directory_name_is_accepted() {
    // Only the full structural match is a refusal; a vault with a `vault/`
    // folder and no Cargo manifest is not this repo.
    let tmp = TempDir::new().unwrap();
    std::fs::create_dir_all(tmp.path().join("vault")).unwrap();
    assert!(resolve_vault_root(Some(tmp.path()), None).is_ok());
}
