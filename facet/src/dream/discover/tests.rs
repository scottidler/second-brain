use super::*;
use crate::gems::{InteractionTurn, Review};
use crate::ledger::workitems::NewWorkItem;
use chrono::{TimeZone, Utc};

fn ts(year: i32, month: u32, day: u32, hour: u32) -> chrono::DateTime<chrono::Utc> {
    Utc.with_ymd_and_hms(year, month, day, hour, 0, 0).single().expect("ts")
}

fn make_gem(id: i64, task: &str, session: &str, at: chrono::DateTime<chrono::Utc>) -> Gem {
    Gem {
        id,
        workitem_id: 1,
        session_uuid: session.to_string(),
        task: task.to_string(),
        context_loaded: vec![],
        context_missing: vec![],
        interaction: vec![InteractionTurn {
            ai_says: "ai".to_string(),
            ai_turn_uuid: format!("ai-{id}"),
            user_says: "user".to_string(),
            user_turn_uuid: format!("u-{id}"),
            tags: vec![],
        }],
        review: Review::default(),
        tags: vec![],
        why_it_matters: "matters".to_string(),
        extractor_model: "sonnet".to_string(),
        extracted_at: at,
    }
}

fn ledger_with_workitem() -> crate::Ledger {
    let l = crate::Ledger::open_in_memory().expect("ledger");
    l.apply_facet_v2_schema().expect("schema");
    l.insert_workitem(NewWorkItem {
        slug: "wi",
        title: "wi",
        created_at: Utc::now(),
    })
    .expect("workitem");
    l
}

#[test]
fn semantic_duplicate_group_emits_for_repeated_task() {
    let gems = vec![
        make_gem(1, "rename portrait", "s1", ts(2026, 5, 1, 10)),
        make_gem(2, "rename portrait", "s2", ts(2026, 5, 2, 10)),
        make_gem(3, "unrelated", "s3", ts(2026, 5, 3, 10)),
    ];
    let dreams = find_semantic_duplicate_groups(&gems);
    assert_eq!(dreams.len(), 1);
    match &dreams[0] {
        Dream::SemanticDuplicateGroup { gem_ids, canonical } => {
            assert_eq!(*canonical, 1);
            assert_eq!(gem_ids, &vec![1, 2]);
        }
        _ => panic!("expected SemanticDuplicateGroup"),
    }
}

#[test]
fn cross_reference_emits_when_later_review_mentions_earlier_task() {
    let mut earlier = make_gem(1, "renaming the portrait module", "s1", ts(2026, 5, 1, 10));
    earlier.task = "renaming the portrait module".to_string();
    let mut later = make_gem(2, "wire spectrum into CLI", "s2", ts(2026, 5, 2, 10));
    later.review.accepted = Some("followed up on renaming the portrait module by wiring CLI dispatch".to_string());
    let dreams = find_cross_references(&[earlier, later]);
    assert_eq!(dreams.len(), 1);
    match &dreams[0] {
        Dream::CrossReference {
            from_gem,
            to_gem,
            relation,
        } => {
            assert_eq!(*from_gem, 2);
            assert_eq!(*to_gem, 1);
            assert_eq!(relation, "precursor");
        }
        _ => panic!("expected CrossReference"),
    }
}

#[test]
fn narrative_candidate_emits_for_session_without_narrative() {
    let l = ledger_with_workitem();
    let gems = vec![
        make_gem(1, "a", "s1", ts(2026, 5, 1, 10)),
        make_gem(2, "b", "s1", ts(2026, 5, 1, 11)),
        make_gem(3, "c", "s1", ts(2026, 5, 1, 12)),
    ];
    let dreams = find_narrative_candidates(&gems, &l).expect("find");
    assert_eq!(dreams.len(), 1);
    match &dreams[0] {
        Dream::NarrativeCandidate { gem_ids, .. } => assert_eq!(gem_ids.len(), 3),
        _ => panic!("expected NarrativeCandidate"),
    }
}

#[test]
fn narrative_candidate_suppressed_when_narrative_already_exists() {
    let l = ledger_with_workitem();
    let axes = crate::narrative::NarrativeAxes::default();
    l.upsert_narrative(crate::ledger::narratives::NewNarrative {
        cluster_key: "s1",
        archetype: crate::narrative::Archetype::Session,
        slug: "x",
        title: "X",
        thesis: "t",
        body_md: "b",
        gem_ids: &[1, 2, 3],
        axes: &axes,
        synthesised_at: Utc::now(),
        synthesiser_model: "opus",
    })
    .expect("narrative");
    let gems = vec![
        make_gem(1, "a", "s1", ts(2026, 5, 1, 10)),
        make_gem(2, "b", "s1", ts(2026, 5, 1, 11)),
        make_gem(3, "c", "s1", ts(2026, 5, 1, 12)),
    ];
    let dreams = find_narrative_candidates(&gems, &l).expect("find");
    assert!(dreams.is_empty());
}

#[test]
fn stale_spectrum_emits_when_new_gems_since_synthesis() {
    let l = ledger_with_workitem();
    let axes = crate::narrative::NarrativeAxes::default();
    l.upsert_narrative(crate::ledger::narratives::NewNarrative {
        cluster_key: "s1",
        archetype: crate::narrative::Archetype::Session,
        slug: "x",
        title: "X",
        thesis: "t",
        body_md: "b",
        gem_ids: &[1, 2],
        axes: &axes,
        synthesised_at: Utc::now(),
        synthesiser_model: "opus",
    })
    .expect("narrative");
    let gems = vec![
        make_gem(1, "a", "s1", ts(2026, 5, 1, 10)),
        make_gem(2, "b", "s1", ts(2026, 5, 1, 11)),
        make_gem(3, "c", "s1", ts(2026, 5, 2, 10)),
    ];
    let dreams = find_stale_spectra(&gems, &l).expect("find");
    assert_eq!(dreams.len(), 1);
    match &dreams[0] {
        Dream::StaleSpectrum { new_gem_ids_since, .. } => assert_eq!(new_gem_ids_since, &vec![3]),
        _ => panic!("expected StaleSpectrum"),
    }
}
