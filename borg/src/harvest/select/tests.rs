#![allow(clippy::unwrap_used)]
use super::*;
use crate::harvest::contract::{EnrichStatus, SessionRecord};

fn base() -> SessionRecord {
    SessionRecord {
        session_id: "s1".to_string(),
        host: "desk".to_string(),
        scope: "work".to_string(),
        cwd: "/home/saidler/repos/tatari-tv/marquee".to_string(),
        project_dir: None,
        repo: Some("tatari-tv/marquee".to_string()),
        git_branch: Some("main".to_string()),
        created: "2026-07-01T00:00:00+00:00".to_string(),
        modified: "2026-07-01T01:00:00+00:00".to_string(),
        updated_at: None,
        duration_secs: None,
        dormant: true,
        title: "substantive engineering work".to_string(),
        first_prompt: "do the thing".to_string(),
        n_msgs: 50,
        model: None,
        summary: None,
        tags: vec![],
        enrich_status: Some(EnrichStatus::Ok),
        redaction_count: 0,
        transcript_path: None,
        staged_path: None,
        archived: false,
        repos_touched: None,
        files_touched: None,
        body: None,
        body_truncated: false,
        body_error: None,
    }
}

fn cfg(min: usize, patterns: &[&str]) -> SelectionConfig {
    SelectionConfig::compile(min, &patterns.iter().map(|s| s.to_string()).collect::<Vec<_>>()).unwrap()
}

#[test]
fn happy_path_selects() {
    assert!(evaluate_selection(&base(), &cfg(6, &[]), "hv-000001").is_ok());
}

#[test]
fn rejects_not_dormant() {
    let mut r = base();
    r.dormant = false;
    let rec = evaluate_selection(&r, &cfg(6, &[]), "hv-000001").unwrap_err();
    assert!(rec.reason.contains("not dormant"), "{}", rec.reason);
    assert_eq!(rec.gate, crate::types::GateId::Selection);
    assert_eq!(rec.source.as_deref(), Some("clyde://s1"));
}

#[test]
fn rejects_enrich_not_ok() {
    for (status, needle) in [
        (EnrichStatus::SkippedPersonal, "skipped-personal"),
        (EnrichStatus::SkippedEmpty, "skipped-empty"),
        (EnrichStatus::Failed, "failed"),
    ] {
        let mut r = base();
        r.enrich_status = Some(status);
        let rec = evaluate_selection(&r, &cfg(6, &[]), "hv-000001").unwrap_err();
        assert!(rec.reason.contains(needle), "{}", rec.reason);
    }
    // null enrich-status also rejects.
    let mut r = base();
    r.enrich_status = None;
    let rec = evaluate_selection(&r, &cfg(6, &[]), "hv-000001").unwrap_err();
    assert!(rec.reason.contains("null"), "{}", rec.reason);
}

#[test]
fn enrichment_does_not_imply_dormancy() {
    // enrich-status ok but NOT dormant -> still rejected (both required).
    let mut r = base();
    r.dormant = false;
    r.enrich_status = Some(EnrichStatus::Ok);
    assert!(evaluate_selection(&r, &cfg(6, &[]), "hv-000001").is_err());
}

#[test]
fn rejects_non_repo_cwd() {
    let mut r = base();
    r.repo = None;
    r.cwd = "/home/saidler".to_string();
    let rec = evaluate_selection(&r, &cfg(6, &[]), "hv-000001").unwrap_err();
    assert!(rec.reason.contains("is not a repo"), "{}", rec.reason);
}

#[test]
fn rejects_malformed_repo_slug() {
    let mut r = base();
    r.repo = Some("no-slash".to_string());
    let rec = evaluate_selection(&r, &cfg(6, &[]), "hv-000001").unwrap_err();
    assert!(rec.reason.contains("well-formed"), "{}", rec.reason);
}

#[test]
fn rejects_below_message_threshold() {
    let mut r = base();
    r.n_msgs = 3;
    let rec = evaluate_selection(&r, &cfg(6, &[]), "hv-000001").unwrap_err();
    assert!(rec.reason.contains("below message threshold"), "{}", rec.reason);
}

#[test]
fn rejects_excluded_title_pattern() {
    let mut r = base();
    r.title = "security-review: workflow audit".to_string();
    let rec = evaluate_selection(&r, &cfg(6, &["security-review"]), "hv-000001").unwrap_err();
    assert!(rec.reason.contains("excluded by pattern"), "{}", rec.reason);
}

#[test]
fn rejects_excluded_first_prompt_pattern() {
    let mut r = base();
    r.first_prompt = "sure".to_string();
    let rec = evaluate_selection(&r, &cfg(6, &["^sure$"]), "hv-000001").unwrap_err();
    assert!(rec.reason.contains("excluded by pattern"), "{}", rec.reason);
}

#[test]
fn invalid_exclude_regex_is_a_loud_config_error() {
    let err = SelectionConfig::compile(6, &["(".to_string()]).unwrap_err();
    assert!(format!("{err:#}").contains("invalid regex"));
}
