use super::*;
use crate::gems::{InteractionTurn, Review};
use crate::ledger::gems::NewGem;

#[test]
fn strip_suffix_two_digits() {
    assert_eq!(
        strip_dup_suffix("phase-8-implementation-2"),
        Some("phase-8-implementation")
    );
}

#[test]
fn strip_suffix_single_digit() {
    assert_eq!(strip_dup_suffix("foo-3"), Some("foo"));
}

#[test]
fn strip_suffix_multi_digit() {
    assert_eq!(strip_dup_suffix("research-12"), Some("research"));
}

#[test]
fn no_suffix_returns_none() {
    assert_eq!(strip_dup_suffix("phase-eight"), None);
    assert_eq!(strip_dup_suffix("plain"), None);
    assert_eq!(strip_dup_suffix(""), None);
    assert_eq!(strip_dup_suffix("-3"), None);
}

#[test]
fn embedded_digit_is_not_a_suffix() {
    assert_eq!(strip_dup_suffix("phase-8"), Some("phase"));
}

#[test]
fn plan_finds_only_real_duplicates() {
    let l = Ledger::open_in_memory().expect("ledger");
    insert_workitem(&l, "phase-8");
    insert_workitem(&l, "phase-8-2");
    insert_workitem(&l, "phase-eight");
    let plans = plan_merges(&l).expect("plan");
    assert_eq!(plans.len(), 1);
    assert_eq!(plans[0].base_slug, "phase-8");
    assert_eq!(plans[0].duplicate_slug, "phase-8-2");
}

#[test]
fn plan_skips_suffix_without_base() {
    let l = Ledger::open_in_memory().expect("ledger");
    insert_workitem(&l, "phase-12");
    insert_workitem(&l, "other-thing");
    let plans = plan_merges(&l).expect("plan");
    assert!(plans.is_empty());
}

#[test]
fn execute_merges_gems_and_deletes_duplicate() {
    let l = Ledger::open_in_memory().expect("ledger");
    let base_id = insert_workitem(&l, "concept");
    let dup_id = insert_workitem(&l, "concept-2");
    insert_gem(&l, base_id, "session-A", "ai-1", "u-1");
    insert_gem(&l, dup_id, "session-B", "ai-2", "u-2");
    let plan = MergePlan {
        base_id,
        base_slug: "concept".to_string(),
        duplicate_id: dup_id,
        duplicate_slug: "concept-2".to_string(),
    };
    let report = execute(&l, &plan).expect("execute");
    assert_eq!(report.gems_moved, 1);
    assert_eq!(report.gems_collided, 0);
    let dup_remaining: i64 = l
        .with_conn(|c| {
            Ok(c.query_row(
                "SELECT COUNT(*) FROM work_items WHERE id = ?1",
                rusqlite::params![dup_id],
                |r| r.get(0),
            )?)
        })
        .expect("count");
    assert_eq!(dup_remaining, 0);
    let base_gems = l.gems_for_workitem(base_id).expect("gems");
    assert_eq!(base_gems.len(), 2);
}

#[test]
fn execute_handles_unique_constraint_collisions() {
    // If base and dup both carry a gem with the same content_hash,
    // the dup row collides; UPDATE OR IGNORE leaves it, DELETE then
    // removes it. Net: dup row is gone, base keeps its existing.
    let l = Ledger::open_in_memory().expect("ledger");
    let base_id = insert_workitem(&l, "concept");
    let dup_id = insert_workitem(&l, "concept-2");
    // Identical interaction UUIDs produce identical content_hash.
    insert_gem(&l, base_id, "session-A", "ai-x", "u-x");
    insert_gem(&l, dup_id, "session-B", "ai-x", "u-x");
    let plan = MergePlan {
        base_id,
        base_slug: "concept".to_string(),
        duplicate_id: dup_id,
        duplicate_slug: "concept-2".to_string(),
    };
    let report = execute(&l, &plan).expect("execute");
    assert_eq!(report.gems_moved, 0);
    assert_eq!(report.gems_collided, 1);
    let base_gems = l.gems_for_workitem(base_id).expect("gems");
    assert_eq!(base_gems.len(), 1, "base gem unchanged");
}

fn insert_workitem(l: &Ledger, slug: &str) -> i64 {
    l.insert_workitem(crate::ledger::workitems::NewWorkItem {
        slug,
        title: slug,
        created_at: chrono::Utc::now(),
    })
    .expect("insert workitem")
}

fn insert_gem(l: &Ledger, workitem_id: i64, session_uuid: &str, ai_uuid: &str, user_uuid: &str) {
    let turn = InteractionTurn {
        ai_says: "ai".to_string(),
        ai_turn_uuid: ai_uuid.to_string(),
        user_says: "user".to_string(),
        user_turn_uuid: user_uuid.to_string(),
        tags: vec![],
    };
    let review = Review {
        accepted: None,
        rejected: None,
        verified_manually: None,
        rewrote_by_hand: None,
    };
    let context_loaded: Vec<String> = vec![];
    let context_missing: Vec<String> = vec![];
    let interaction = vec![turn];
    let tags: Vec<String> = vec![];
    l.upsert_gem(NewGem {
        workitem_id,
        session_uuid,
        task: "task",
        context_loaded: &context_loaded,
        context_missing: &context_missing,
        interaction: &interaction,
        review: &review,
        tags: &tags,
        why_it_matters: "why",
        extractor_model: "test-model",
        extracted_at: chrono::Utc::now(),
    })
    .expect("upsert gem");
}
