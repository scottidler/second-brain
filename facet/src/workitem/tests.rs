use super::*;

#[test]
fn derive_slug_basic() {
    assert_eq!(derive_slug("Loopr v5 Stage Eight"), "loopr-v5-stage-eight");
}

#[test]
fn derive_slug_strips_punctuation() {
    assert_eq!(derive_slug("Loopr/v5: stage-eight!"), "loopr-v5-stage-eight");
}

#[test]
fn derive_slug_collapses_runs() {
    assert_eq!(derive_slug("foo   bar___baz"), "foo-bar-baz");
}

#[test]
fn derive_slug_trims_dashes() {
    assert_eq!(derive_slug("---foo bar---"), "foo-bar");
}

#[test]
fn derive_slug_empty_falls_back() {
    assert_eq!(derive_slug("!!!"), "untitled");
}

#[test]
fn derive_slug_truncates_at_80() {
    let long = "a".repeat(120);
    let out = derive_slug(&long);
    assert!(out.len() <= 80, "slug too long: {}", out.len());
}

#[test]
fn workitem_status_roundtrip() {
    use std::str::FromStr;
    for s in [
        WorkItemStatus::Active,
        WorkItemStatus::Dormant,
        WorkItemStatus::Archived,
    ] {
        let v = s.as_str();
        let parsed = WorkItemStatus::from_str(v).expect("parse");
        assert_eq!(s, parsed);
    }
}
