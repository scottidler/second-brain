use chrono::Utc;

use super::*;
use crate::extract::JudgmentMoment;
use crate::workitem::WorkItemStatus;

fn workitem(slug: &str, repos: Vec<&str>) -> WorkItem {
    let now = Utc::now();
    WorkItem {
        id: 1,
        slug: slug.to_string(),
        title: format!("Title for {slug}"),
        repos: repos.into_iter().map(String::from).collect(),
        status: WorkItemStatus::Active,
        created_at: now,
        updated_at: now,
        dormant_since: None,
        sessions_count: 2,
        modes_present: vec![],
    }
}

fn moment(mode: &str, scott: &str, quote: &str) -> JudgmentMoment {
    JudgmentMoment {
        id: 0,
        workitem_id: 1,
        session_uuid: "sess-12345678".to_string(),
        turn_uuid: "t-uuid".to_string(),
        mode: mode.to_string(),
        ai_move: "AI suggested something".to_string(),
        scott_move: scott.to_string(),
        quote_excerpt: quote.to_string(),
        why_it_matters: "shows judgment".to_string(),
        extracted_at: Utc::now(),
        extractor_model: "sonnet".to_string(),
    }
}

#[test]
fn fresh_render_with_no_existing_file_is_well_formed() {
    let w = workitem("foo", vec!["me/foo"]);
    let out = render_to_string(&w, &[], None);
    assert!(out.contains("<!-- facet:auto:begin frontmatter -->"));
    assert!(out.contains("<!-- facet:auto:end frontmatter -->"));
    assert!(out.contains("<!-- facet:auto:begin header -->"));
    assert!(out.contains("<!-- facet:auto:begin section:frame -->"));
    assert!(out.contains("<!-- facet:auto:begin section:reject -->"));
    assert!(out.contains("<!-- facet:auto:begin section:other -->"));
    assert!(out.contains("<!-- facet:auto:begin footer -->"));
    assert!(out.contains("Title for foo"));
}

#[test]
fn re_render_with_no_operator_content_is_byte_identical() {
    let w = workitem("foo", vec!["me/foo"]);
    let moments = vec![moment("frame", "framed it", "this is the frame")];
    let first = render_to_string(&w, &moments, None);
    let second = render_to_string(&w, &moments, Some(&first));
    assert_eq!(first, second, "second render should be byte-identical");
}

#[test]
fn operator_content_between_auto_blocks_survives_rerender() {
    let w = workitem("foo", vec!["me/foo"]);
    let first = render_to_string(&w, &[], None);
    let inserted = first.replace(
        "<!-- facet:auto:end header -->\n\n",
        "<!-- facet:auto:end header -->\n\nMY OPERATOR PARAGRAPH\n\n",
    );
    let second = render_to_string(&w, &[], Some(&inserted));
    assert!(
        second.contains("MY OPERATOR PARAGRAPH"),
        "operator content lost: {second}"
    );
}

#[test]
fn new_mode_section_added_does_not_displace_operator_content() {
    let w = workitem("foo", vec!["me/foo"]);
    let first = render_to_string(&w, &[], None);
    let edited = first.replace(
        "<!-- facet:auto:end footer -->\n",
        "<!-- facet:auto:end footer -->\n\nOPERATOR APPENDED\n",
    );
    let with_moments = render_to_string(&w, &[moment("reject", "rejected", "no")], Some(&edited));
    assert!(with_moments.contains("OPERATOR APPENDED"));
    assert!(with_moments.contains("section:reject"));
}

#[test]
fn moment_in_open_vocabulary_mode_lands_in_other_section() {
    let w = workitem("foo", vec!["me/foo"]);
    let moments = vec![moment("re-scope", "scoped down", "do half first")];
    let out = render_to_string(&w, &moments, None);
    let start = out
        .find("<!-- facet:auto:begin section:other -->")
        .expect("other present");
    let end = out[start..]
        .find("<!-- facet:auto:end section:other -->")
        .expect("close present");
    let block = &out[start..start + end];
    assert!(
        block.contains("## Re Scope") || block.contains("## Re-Scope"),
        "section:other block did not contain humanised mode heading: BLOCK={block}"
    );
    assert!(block.contains("do half first"));
}

#[test]
fn empty_mode_section_emits_placeholder() {
    let w = workitem("foo", vec!["me/foo"]);
    let out = render_to_string(&w, &[], None);
    // Every scaffold section should mention "no moments yet" since there are none.
    assert!(out.matches("*(no moments yet)*").count() >= 6);
}

#[test]
fn write_atomic_creates_parent_dir_and_writes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let target = dir.path().join("a/b/c/note.md");
    write_atomic(&target, "hello").expect("write");
    let read = std::fs::read_to_string(&target).expect("read");
    assert_eq!(read, "hello");
}
