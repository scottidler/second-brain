#![allow(clippy::unwrap_used)]

use super::*;
use std::path::PathBuf;
use tempfile::TempDir;

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
