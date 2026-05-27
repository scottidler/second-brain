use super::*;

fn open() -> Ledger {
    let l = Ledger::open_in_memory().expect("open");
    l.apply_facet_v2_schema().expect("v2 schema");
    l
}

fn axes_fixture() -> NarrativeAxes {
    NarrativeAxes {
        semantic_cluster_id: Some(7),
        mode_mix: vec![("reject".to_string(), 3), ("verify".to_string(), 1)],
        time_window: None,
        repos: vec!["scottidler/second-brain".to_string()],
        workitem_ids: vec![42],
    }
}

#[test]
fn upsert_then_read_round_trip() {
    let l = open();
    let axes = axes_fixture();
    let id = l
        .upsert_narrative(NewNarrative {
            cluster_key: "session-abc",
            archetype: Archetype::Session,
            slug: "three-rejections-sessionab",
            title: "The Three Rejections",
            thesis: "Plausible-but-wrong AI suggestions.",
            body_md: "## Setup",
            gem_ids: &[1, 2, 3],
            axes: &axes,
            synthesised_at: Utc::now(),
            synthesiser_model: "claude-opus-4-7",
        })
        .expect("upsert");
    let n = l
        .narrative_by_cluster_key("session-abc")
        .expect("read")
        .expect("present");
    assert_eq!(n.id, id);
    assert_eq!(n.title, "The Three Rejections");
    assert_eq!(n.gem_ids, vec![1, 2, 3]);
    assert_eq!(n.revision, 1);
    assert_eq!(n.axes.workitem_ids, vec![42]);
    assert_eq!(n.axes.semantic_cluster_id, Some(7));
}

#[test]
fn upsert_bumps_revision_on_second_call() {
    let l = open();
    let axes = axes_fixture();
    let new_payload = NewNarrative {
        cluster_key: "session-abc",
        archetype: Archetype::Session,
        slug: "a",
        title: "A",
        thesis: "T",
        body_md: "B",
        gem_ids: &[1, 2],
        axes: &axes,
        synthesised_at: Utc::now(),
        synthesiser_model: "opus",
    };
    let id1 = l.upsert_narrative(new_payload.clone()).expect("first");
    let id2 = l.upsert_narrative(new_payload.clone()).expect("second");
    assert_eq!(id1, id2);
    let n = l
        .narrative_by_cluster_key("session-abc")
        .expect("read")
        .expect("present");
    assert_eq!(n.revision, 2);
}

#[test]
fn missing_cluster_key_returns_none() {
    let l = open();
    let n = l.narrative_by_cluster_key("nope").expect("read");
    assert!(n.is_none());
}
