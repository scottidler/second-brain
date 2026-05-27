use super::*;
use crate::narrative::NarrativeAxes;
use chrono::{TimeZone, Utc};

fn fixture() -> Narrative {
    Narrative {
        id: 1,
        slug: "the-three-rejections".to_string(),
        title: "The Three Rejections".to_string(),
        thesis: "Three plausible-but-wrong AI suggestions in one session.".to_string(),
        body_md: "## Setup\n\nWe started with a rename.\n\n## Complication\n\nThe rename cascaded.".to_string(),
        gem_ids: vec![10, 11, 12],
        axes: NarrativeAxes::default(),
        synthesised_at: Utc.with_ymd_and_hms(2026, 5, 26, 12, 0, 0).single().expect("ts"),
        synthesiser_model: "claude-opus-4-7".to_string(),
        revision: 1,
    }
}

#[test]
fn outline_contains_one_beat_per_gem_id() {
    let body = render_outline(&fixture());
    assert!(body.contains("## Beat 1: gem #10"));
    assert!(body.contains("## Beat 2: gem #11"));
    assert!(body.contains("## Beat 3: gem #12"));
}

#[test]
fn outline_has_title_and_takeaway_slides() {
    let body = render_outline(&fixture());
    assert!(body.contains("# The Three Rejections"));
    assert!(body.contains("## Takeaway"));
    assert!(body.contains("---"));
}

#[test]
fn body_first_paragraph_skips_leading_heading() {
    let p = body_first_paragraph("## Setup\n\nFirst paragraph here.\n\n## Complication");
    assert!(p.contains("First paragraph here."));
    assert!(!p.contains("Setup"));
}
