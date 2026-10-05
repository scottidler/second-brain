use super::*;
use crate::cli::checks::Severity;
use std::path::PathBuf;

#[test]
fn differing_paths_warn_and_name_both() {
    let f = mismatch_finding(&PathBuf::from("/a/tags.yml"), &PathBuf::from("/b/tags.yml")).expect("warn");
    assert_eq!(f.severity, Severity::Warn);
    assert!(f.message.contains("/a/tags.yml") && f.message.contains("/b/tags.yml"));
}

#[test]
fn matching_paths_are_silent() {
    assert!(mismatch_finding(&PathBuf::from("/a/tags.yml"), &PathBuf::from("/a/tags.yml")).is_none());
}
