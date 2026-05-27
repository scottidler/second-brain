use super::*;
use crate::narrative::{NarrativeAxes, SpectrumStatus};
use chrono::{TimeZone, Utc};

fn narrative_fixture() -> Narrative {
    Narrative {
        id: 1,
        slug: "three-rejections-xs12345".to_string(),
        title: "The Three Rejections".to_string(),
        thesis: "Three plausible-but-wrong suggestions in one session.".to_string(),
        body_md: "## Setup\n\nFirst.\n\n## Complication\n\nSecond.\n\n## Resolution\n\nThird.".to_string(),
        gem_ids: vec![1, 2, 3],
        axes: NarrativeAxes::default(),
        synthesised_at: Utc.with_ymd_and_hms(2026, 5, 1, 12, 0, 0).single().expect("ts"),
        synthesiser_model: "claude-opus-4-7".to_string(),
        revision: 1,
    }
}

#[test]
fn fresh_render_includes_archetype_status_cluster_key_and_gem_ids() {
    let body = render_to_string(&narrative_fixture(), Archetype::CrossSession, "xs-deadbeef0", None);
    assert!(body.contains("type: facet-spectrum"));
    assert!(body.contains("facet-spectrum-status: active"));
    assert!(body.contains("facet-spectrum-archetype: cross-session"));
    assert!(body.contains("facet-spectrum-cluster-key: xs-deadbeef0"));
    assert!(body.contains("- 1"));
    assert!(body.contains("- 2"));
    assert!(body.contains("- 3"));
    assert!(body.contains("# The Three Rejections"));
    assert!(body.contains("Three plausible-but-wrong"));
}

#[test]
fn read_meta_parses_status_archetype_cluster_key_and_gem_ids() {
    let body = render_to_string(&narrative_fixture(), Archetype::Session, "session-abc", None);
    let meta = parse_meta_from_body(&body).expect("meta present");
    assert_eq!(meta.status, SpectrumStatus::Active);
    assert_eq!(meta.cluster_key, "session-abc");
    assert_eq!(meta.gem_ids, vec![1, 2, 3]);
    assert_eq!(meta.archetype, Some(Archetype::Session));
}

#[test]
fn read_meta_picks_up_operator_rejection() {
    let mut body = render_to_string(&narrative_fixture(), Archetype::Session, "k", None);
    body = body.replace("facet-spectrum-status: active", "facet-spectrum-status: rejected");
    let meta = parse_meta_from_body(&body).expect("meta");
    assert_eq!(meta.status, SpectrumStatus::Rejected);
}

#[test]
fn round_trip_to_disk_preserves_operator_content() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let target = tmp.path().join("spectrum.md");
    render_spectrum_note(&target, &narrative_fixture(), Archetype::CrossSession, "k").expect("render");
    let body = std::fs::read_to_string(&target).expect("read");
    let appended = format!("{body}\n\n## Operator margin notes\n\nAdded by hand.\n");
    std::fs::write(&target, &appended).expect("write");
    // Re-render with same narrative -> operator content survives.
    render_spectrum_note(&target, &narrative_fixture(), Archetype::CrossSession, "k").expect("re-render");
    let final_body = std::fs::read_to_string(&target).expect("read");
    assert!(final_body.contains("## Operator margin notes"));
    assert!(final_body.contains("Added by hand."));
}

#[test]
fn meta_defaults_when_keys_missing() {
    let raw = "---\ntitle: Foo\n---\n\n# body\n";
    let meta = parse_meta_from_body(raw).expect("meta");
    assert_eq!(meta.status, SpectrumStatus::Active);
    assert_eq!(meta.cluster_key, "");
    assert!(meta.gem_ids.is_empty());
    assert!(meta.archetype.is_none());
}
