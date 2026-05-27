use chrono::Utc;

use super::*;
use crate::workitem::WorkItemStatus;

fn workitem() -> WorkItem {
    let now = Utc::now();
    WorkItem {
        id: 42,
        slug: "loopr-v5-stage-eight".to_string(),
        title: "Loopr v5 stage eight".to_string(),
        repos: vec!["scottidler/loopr".to_string()],
        status: WorkItemStatus::Active,
        created_at: now,
        updated_at: now,
        dormant_since: None,
        sessions_count: 3,
        modes_present: vec!["frame".to_string()],
    }
}

#[test]
fn fresh_render_carries_facet_keys_and_managed_tags() {
    let w = workitem();
    let out = render(&w, &[], None);
    assert!(out.starts_with("---\n"));
    assert!(out.contains("facet-workitem-id: 42"));
    assert!(out.contains("facet-slug: loopr-v5-stage-eight"));
    assert!(out.contains("type: facet-workitem"));
    assert!(out.contains("- facet\n"));
    assert!(out.contains("- judgment\n"));
    // repo terminal segment becomes a tag
    assert!(out.contains("- loopr\n"));
}

#[test]
fn merge_preserves_operator_tags_and_keys() {
    let w = workitem();
    let existing = "---\ntitle: Loopr v5 stage eight\ntype: facet-workitem\ntags:\n  - facet\n  - judgment\n  - loopr\n  - my-custom-tag\nmy-key: my-value\n---\n";
    let out = render(&w, &[], Some(existing));
    assert!(out.contains("my-custom-tag"));
    assert!(out.contains("my-key: my-value"));
    assert!(out.contains("- facet\n"));
}

#[test]
fn facet_managed_keys_win_on_their_keys() {
    let w = workitem();
    let existing = "---\ntype: foo\nfacet-slug: stale-slug\nstatus: garbled\n---\n";
    let out = render(&w, &[], Some(existing));
    assert!(out.contains("type: facet-workitem"));
    assert!(out.contains("facet-slug: loopr-v5-stage-eight"));
    assert!(!out.contains("type: foo"));
    assert!(!out.contains("stale-slug"));
}
