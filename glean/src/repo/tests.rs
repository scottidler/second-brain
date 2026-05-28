#![allow(clippy::unwrap_used)]
use super::*;
use std::path::PathBuf;

#[test]
fn slug_from_repo_path_extracts_org_and_repo() {
    let p = PathBuf::from("/home/saidler/repos/scottidler/second-brain");
    assert_eq!(slug_from_repo_path(&p), Some("scottidler/second-brain".to_string()));
}

#[test]
fn slug_from_repo_path_handles_nested_paths() {
    let p = PathBuf::from("/home/saidler/repos/tatari-tv/philo/services/api");
    assert_eq!(slug_from_repo_path(&p), Some("tatari-tv/philo".to_string()));
}

#[test]
fn slug_from_repo_path_returns_none_without_prefix() {
    let p = PathBuf::from("/home/saidler/random/scottidler/second-brain");
    assert_eq!(slug_from_repo_path(&p), None);
}

#[test]
fn resolve_returns_none_for_non_repo_dir() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (path, slug) = resolve(tmp.path());
    assert!(path.is_none());
    assert!(slug.is_none());
}

#[test]
fn resolve_finds_git_ancestor() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("repos").join("acme").join("widget");
    std::fs::create_dir_all(root.join(".git")).expect("mkdir .git");
    let sub = root.join("src").join("nested");
    std::fs::create_dir_all(&sub).expect("mkdir nested");
    let (path, slug) = resolve(&sub);
    assert_eq!(path.as_deref(), Some(root.as_path()));
    assert_eq!(slug.as_deref(), Some("acme/widget"));
}
