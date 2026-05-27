use super::*;
use crate::gems::{Gem, InteractionTurn, Review};
use crate::workitem::{WorkItem, WorkItemStatus};
use chrono::TimeZone;

fn ts(year: i32, month: u32, day: u32) -> chrono::DateTime<chrono::Utc> {
    chrono::Utc
        .with_ymd_and_hms(year, month, day, 12, 0, 0)
        .single()
        .expect("valid timestamp")
}

fn fixture_workitem() -> WorkItem {
    WorkItem {
        id: 42,
        slug: "rename-portrait-to-spectrum".to_string(),
        title: "Rename portrait to spectrum".to_string(),
        repos: vec!["scottidler/second-brain".to_string()],
        status: WorkItemStatus::Active,
        created_at: ts(2026, 5, 20),
        updated_at: ts(2026, 5, 26),
        dormant_since: None,
        sessions_count: 3,
        modes_present: vec!["reject".to_string()],
    }
}

fn fixture_turn(seq: usize) -> InteractionTurn {
    InteractionTurn {
        ai_says: format!("AI says turn {seq}"),
        ai_turn_uuid: format!("ai-{seq}"),
        user_says: format!("Scott replies turn {seq}"),
        user_turn_uuid: format!("u-{seq}"),
        tags: vec!["reject".to_string()],
    }
}

fn fixture_gem(id: i64, task: &str, tags: Vec<String>) -> Gem {
    Gem {
        id,
        workitem_id: 42,
        session_uuid: "session-abcdef123".to_string(),
        task: task.to_string(),
        context_loaded: vec!["facet/src/extract/portrait.rs".to_string()],
        context_missing: vec![],
        interaction: vec![fixture_turn(1), fixture_turn(2)],
        review: Review {
            accepted: Some("rename landed".to_string()),
            rejected: None,
            verified_manually: Some("cargo check passed".to_string()),
            rewrote_by_hand: None,
        },
        tags,
        why_it_matters: "renames cascade".to_string(),
        extractor_model: "claude-sonnet-4-6".to_string(),
        extracted_at: ts(2026, 5, 26),
    }
}

#[test]
fn fresh_render_includes_frontmatter_header_index_and_one_section_per_gem() {
    let wi = fixture_workitem();
    let gems = vec![
        fixture_gem(1, "rename portrait to spectrum", vec!["reject".to_string()]),
        fixture_gem(
            2,
            "wire spectrum into CLI",
            vec!["frame".to_string(), "verify".to_string()],
        ),
    ];
    let body = render_prism_to_string(&wi, &gems, None);
    assert!(body.contains("type: facet-prism"));
    assert!(body.contains("facet-gem-count: 2"));
    // Tag mix entries
    assert!(body.contains("facet-tag-mix:"));
    assert!(body.contains("tag: reject"));
    assert!(body.contains("count: 1"));
    // Headers
    assert!(body.contains("# Rename portrait to spectrum"));
    assert!(body.contains("## Gem Index"));
    // Per-gem sections
    assert!(body.contains("## Gem 1: rename portrait to spectrum"));
    assert!(body.contains("## Gem 2: wire spectrum into CLI"));
    // Four-part anatomy sub-sections per gem
    assert!(body.contains("### Task"));
    assert!(body.contains("### Context"));
    assert!(body.contains("### Interaction"));
    assert!(body.contains("### Review"));
    // Fencepost markers
    assert!(body.contains("<!-- facet:auto:begin frontmatter -->"));
    assert!(body.contains("<!-- facet:auto:begin gem-index -->"));
    assert!(body.contains("<!-- facet:auto:begin gem:1 -->"));
    assert!(body.contains("<!-- facet:auto:begin gem:2 -->"));
}

#[test]
fn empty_gem_list_still_renders_a_well_formed_note() {
    let wi = fixture_workitem();
    let body = render_prism_to_string(&wi, &[], None);
    assert!(body.contains("type: facet-prism"));
    assert!(body.contains("facet-gem-count: 0"));
    assert!(body.contains("*(no gems yet)*"));
    assert!(body.contains("- **Gem count:** 0"));
}

#[test]
fn frontmatter_includes_facet_tag_mix_in_count_desc_order() {
    let wi = fixture_workitem();
    let gems = vec![
        fixture_gem(1, "a", vec!["reject".to_string()]),
        fixture_gem(2, "b", vec!["reject".to_string()]),
        fixture_gem(3, "c", vec!["verify".to_string()]),
    ];
    let body = render_prism_to_string(&wi, &gems, None);
    let mix_start = body.find("facet-tag-mix:").expect("tag mix present");
    let reject_idx = body[mix_start..].find("tag: reject").expect("reject in mix");
    let verify_idx = body[mix_start..].find("tag: verify").expect("verify in mix");
    assert!(
        reject_idx < verify_idx,
        "reject (count 2) should appear before verify (count 1)"
    );
}

#[test]
fn merge_preserves_operator_content_outside_fenceposts() {
    let wi = fixture_workitem();
    let gems = vec![fixture_gem(1, "rename", vec!["reject".to_string()])];
    let existing = format!(
        "{}\n\n## Operator notes\n\nI added this by hand.\n",
        render_prism_to_string(&wi, &gems, None)
    );
    let regenerated = render_prism_to_string(&wi, &gems, Some(&existing));
    assert!(
        regenerated.contains("## Operator notes"),
        "operator content outside fenceposts must survive re-render; got:\n{regenerated}"
    );
    assert!(regenerated.contains("I added this by hand."));
}

#[test]
fn title_for_truncates_long_task_at_word_boundary() {
    let long = "a ".repeat(60);
    let wi = fixture_workitem();
    let gems = vec![fixture_gem(1, &long, vec![])];
    let body = render_prism_to_string(&wi, &gems, None);
    // Section heading must be present and end with the truncation marker
    let line = body
        .lines()
        .find(|l| l.starts_with("## Gem 1:"))
        .expect("gem 1 heading present");
    assert!(line.ends_with('…'), "truncated heading must end with ellipsis: {line}");
    assert!(line.chars().count() < 100, "truncated heading must be bounded");
}

#[test]
fn gem_section_lists_per_turn_tags() {
    let wi = fixture_workitem();
    let mut g = fixture_gem(1, "x", vec![]);
    g.interaction[0].tags = vec!["name-the-failure".to_string()];
    let body = render_prism_to_string(&wi, &[g], None);
    assert!(body.contains("**Turn 1** — `name-the-failure`"));
}

#[test]
fn render_prism_note_writes_atomically_to_disk() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let target = tmp.path().join("test-prism.md");
    let wi = fixture_workitem();
    let gems = vec![fixture_gem(1, "rename", vec!["reject".to_string()])];
    render_prism_note(&target, &wi, &gems).expect("render");
    let body = std::fs::read_to_string(&target).expect("read");
    assert!(body.contains("type: facet-prism"));
    assert!(body.contains("## Gem 1: rename"));
}
