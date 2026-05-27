use super::*;
use chrono::TimeZone;

fn ts(year: i32, month: u32, day: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(year, month, day, 0, 0, 0)
        .single()
        .expect("valid fixture timestamp")
}

fn narrative_fixture() -> Narrative {
    Narrative {
        id: 0,
        slug: "the-seven-rust-renames".to_string(),
        title: "The Seven Rust Renames".to_string(),
        thesis: "When the noun changes, the metaphor changes; chase it everywhere.".to_string(),
        body_md: "# The Seven Rust Renames\n\nOnce upon a time...".to_string(),
        gem_ids: vec![1, 2, 3, 5, 7],
        axes: NarrativeAxes {
            semantic_cluster_id: Some(17),
            mode_mix: vec![
                ("reject".to_string(), 3),
                ("name-the-failure".to_string(), 2),
                ("verify".to_string(), 2),
            ],
            time_window: Some((ts(2026, 4, 1), ts(2026, 5, 26))),
            repos: vec!["scottidler/second-brain".to_string()],
            workitem_ids: vec![42, 43, 51],
        },
        synthesised_at: Utc
            .with_ymd_and_hms(2026, 5, 26, 12, 0, 0)
            .single()
            .expect("valid fixture timestamp"),
        synthesiser_model: "claude-opus-4-7".to_string(),
        revision: 1,
    }
}

#[test]
fn narrative_roundtrips_through_json() {
    let n = narrative_fixture();
    let json = serde_json::to_string(&n).expect("serialize narrative");
    let back: Narrative = serde_json::from_str(&json).expect("deserialize narrative");
    assert_eq!(n, back);
}

#[test]
fn narrative_defaults_revision_to_one() {
    let raw = r#"{
        "slug": "x",
        "title": "X",
        "thesis": "x",
        "body_md": "x",
        "gem_ids": [1],
        "axes": {},
        "synthesised_at": "2026-05-26T12:00:00Z",
        "synthesiser_model": "opus"
    }"#;
    let n: Narrative = serde_json::from_str(raw).expect("deserialize narrative");
    assert_eq!(n.revision, 1);
    assert_eq!(n.id, 0);
    assert_eq!(n.axes, NarrativeAxes::default());
}

#[test]
fn archetype_serializes_as_kebab_case() {
    let json = serde_json::to_string(&Archetype::CrossSession).expect("serialize archetype");
    assert_eq!(json, "\"cross-session\"");
    let back: Archetype = serde_json::from_str("\"cross-session\"").expect("deserialize archetype");
    assert_eq!(back, Archetype::CrossSession);
}

#[test]
fn archetype_as_str_matches_frontmatter_values() {
    assert_eq!(Archetype::Session.as_str(), "session");
    assert_eq!(Archetype::CrossSession.as_str(), "cross-session");
}

#[test]
fn spectrum_status_serializes_as_kebab_case() {
    let json = serde_json::to_string(&SpectrumStatus::Rejected).expect("serialize status");
    assert_eq!(json, "\"rejected\"");
    let back: SpectrumStatus = serde_json::from_str("\"active\"").expect("deserialize status");
    assert_eq!(back, SpectrumStatus::Active);
}
