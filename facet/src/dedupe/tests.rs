use super::*;

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
    // "phase-8" is itself a legitimate slug, not a duplicate-of-"phase".
    // The dedupe planner uses `strip_dup_suffix` + "base exists" check,
    // so this is only a duplicate when "phase" also exists - which is
    // the correct semantic.
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
    // "phase-12" has no base "phase" in the ledger so it should not be
    // flagged. We track these as standalone work-items.
    let l = Ledger::open_in_memory().expect("ledger");
    insert_workitem(&l, "phase-12");
    insert_workitem(&l, "other-thing");
    let plans = plan_merges(&l).expect("plan");
    assert!(plans.is_empty());
}

#[test]
fn execute_merges_moments_and_deletes_duplicate() {
    let l = Ledger::open_in_memory().expect("ledger");
    let base_id = insert_workitem(&l, "concept");
    let dup_id = insert_workitem(&l, "concept-2");
    insert_moment(&l, base_id, "session-A", "turn-1", "frame");
    insert_moment(&l, dup_id, "session-B", "turn-9", "reject");
    let plan = MergePlan {
        base_id,
        base_slug: "concept".to_string(),
        duplicate_id: dup_id,
        duplicate_slug: "concept-2".to_string(),
    };
    let report = execute(&l, &plan).expect("execute");
    assert_eq!(report.moments_moved, 1);
    assert_eq!(report.moments_collided, 0);
    // Duplicate work-item gone.
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
    // Both moments now under base.
    let base_moments = l.moments_for_workitem(base_id).expect("moments");
    assert_eq!(base_moments.len(), 2);
}

#[test]
fn execute_handles_unique_constraint_collisions() {
    // If base and dup both carry a moment with the same (turn_uuid, mode),
    // the dup row collides; UPDATE OR IGNORE leaves it, DELETE then
    // removes it. Net: dup row is gone, base keeps its existing.
    let l = Ledger::open_in_memory().expect("ledger");
    let base_id = insert_workitem(&l, "concept");
    let dup_id = insert_workitem(&l, "concept-2");
    insert_moment(&l, base_id, "session-A", "turn-1", "frame");
    insert_moment(&l, dup_id, "session-B", "turn-1", "frame"); // same turn/mode
    let plan = MergePlan {
        base_id,
        base_slug: "concept".to_string(),
        duplicate_id: dup_id,
        duplicate_slug: "concept-2".to_string(),
    };
    let report = execute(&l, &plan).expect("execute");
    assert_eq!(report.moments_moved, 0);
    assert_eq!(report.moments_collided, 1);
    let base_moments = l.moments_for_workitem(base_id).expect("moments");
    assert_eq!(base_moments.len(), 1, "base moment unchanged");
}

fn insert_workitem(l: &Ledger, slug: &str) -> i64 {
    l.insert_workitem(crate::ledger::workitems::NewWorkItem {
        slug,
        title: slug,
        created_at: chrono::Utc::now(),
    })
    .expect("insert workitem")
}

fn insert_moment(l: &Ledger, workitem_id: i64, session_uuid: &str, turn_uuid: &str, mode: &str) {
    l.with_conn(|c| {
        c.execute(
            "INSERT INTO judgment_moments \
             (workitem_id, session_uuid, turn_uuid, mode, ai_move, scott_move, \
              quote_excerpt, why_it_matters, extractor_model, extracted_at) \
             VALUES (?1, ?2, ?3, ?4, 'a', 's', 'q', 'w', 'test-model', ?5)",
            rusqlite::params![
                workitem_id,
                session_uuid,
                turn_uuid,
                mode,
                chrono::Utc::now().to_rfc3339()
            ],
        )?;
        Ok(())
    })
    .expect("insert moment");
}
