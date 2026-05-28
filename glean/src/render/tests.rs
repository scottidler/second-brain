#![allow(clippy::unwrap_used)]
use super::*;
use crate::types::{WorkItem, WorkItemKey};
use chrono::{TimeZone, Utc};

fn sample_work_item(uuids: &[&str], content_hash: &str) -> WorkItem {
    WorkItem {
        id: 0,
        key_type: WorkItemKey::DesignDoc,
        key_value: "docs/design/2026-05-27-glean.md".to_string(),
        repo_slug: Some("scottidler/second-brain".to_string()),
        content_hash: content_hash.to_string(),
        session_uuids: uuids.iter().map(|s| s.to_string()).collect(),
        time_start: Utc.timestamp_opt(1_700_000_000, 0).unwrap(),
        time_end: Utc.timestamp_opt(1_700_000_500, 0).unwrap(),
        aggregated_tags: vec!["design-doc".to_string()],
        materialized_at: Utc.timestamp_opt(1_700_000_900, 0).unwrap(),
    }
}

fn sample_distill_output() -> DistillOutput {
    DistillOutput {
        title: "design and ship the glean two-tier distiller".to_string(),
        tldr: "tldr line".to_string(),
        task: "task content".to_string(),
        context: "context content".to_string(),
        interaction: "interaction content".to_string(),
        review: "review content".to_string(),
    }
}

#[test]
fn render_chunk_writes_file_with_content_hash_in_frontmatter() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let wi = sample_work_item(&["u1", "u2"], "deadbeefcafe1234");
    let out = sample_distill_output();
    let path = render_chunk(tmp.path(), &wi, &out, "glean-distill-v1", "claude-opus-4-7").expect("render");
    let body = std::fs::read_to_string(&path).expect("read");
    assert!(body.contains("content-hash: deadbeefcafe1234"));
    assert!(body.contains("## Task"));
    assert!(body.contains("## Interaction"));
    assert!(body.contains("interaction content"));
}

#[test]
fn render_chunk_renames_on_slug_drift_preserving_operator_content() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut wi = sample_work_item(&["u1"], "stable000content");
    let out1 = DistillOutput {
        title: "first title".to_string(),
        ..sample_distill_output()
    };
    let path1 = render_chunk(tmp.path(), &wi, &out1, "glean-distill-v1", "claude-opus-4-7").expect("render1");

    // operator edits the postamble after the auto body
    let existing = std::fs::read_to_string(&path1).expect("read1");
    let mut tampered = existing.clone();
    tampered.push_str("\n## Operator Note\n\nThis was added by Scott.\n");
    std::fs::write(&path1, &tampered).expect("write tampered");

    // change title; content_hash stays the same so the file should be
    // renamed in place
    wi.materialized_at = Utc::now();
    let out2 = DistillOutput {
        title: "second title that differs".to_string(),
        ..sample_distill_output()
    };
    let path2 = render_chunk(tmp.path(), &wi, &out2, "glean-distill-v1", "claude-opus-4-7").expect("render2");
    assert_ne!(path1, path2);
    assert!(!path1.exists(), "old slug file should no longer exist");
    let final_body = std::fs::read_to_string(&path2).expect("read final");
    assert!(final_body.contains("Operator Note"));
    assert!(final_body.contains("This was added by Scott"));
}

#[test]
fn slugify_includes_hash_prefix() {
    let s = slugify("a long human title with spaces", "abcdef1234567890");
    assert!(s.ends_with("-abcdef12"));
    assert!(s.starts_with("a-long-human"));
}

#[test]
fn slugify_falls_back_when_title_is_empty() {
    let s = slugify("...", "abcdef1234567890");
    assert!(s.starts_with("glean-"));
}
