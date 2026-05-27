use super::*;
use crate::ledger::workitems::NewWorkItem;
use chrono::TimeZone;

fn open() -> Ledger {
    Ledger::open_in_memory().expect("open in-memory")
}

fn ensure_workitem(l: &Ledger, slug: &str) -> i64 {
    let now = Utc::now();
    let id = l
        .insert_workitem(NewWorkItem {
            slug,
            title: "fixture work-item",
            created_at: now,
        })
        .expect("insert workitem");
    l.link_workitem_repo(id, "scottidler/second-brain").expect("link repo");
    id
}

fn turn_fixture(ai_uuid: &str, user_uuid: &str) -> InteractionTurn {
    InteractionTurn {
        ai_says: format!("ai says for {ai_uuid}"),
        ai_turn_uuid: ai_uuid.to_string(),
        user_says: format!("user says for {user_uuid}"),
        user_turn_uuid: user_uuid.to_string(),
        tags: vec!["reject".to_string()],
    }
}

fn ts() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 5, 26, 12, 0, 0)
        .single()
        .expect("valid fixture timestamp")
}

fn new_gem<'a>(workitem_id: i64, turns: &'a [InteractionTurn], review: &'a Review, tags: &'a [String]) -> NewGem<'a> {
    NewGem {
        workitem_id,
        session_uuid: "session-abc",
        task: "rename portrait to spectrum",
        context_loaded: &[],
        context_missing: &[],
        interaction: turns,
        review,
        tags,
        why_it_matters: "rename cascades",
        extractor_model: "claude-sonnet-4-6",
        extracted_at: ts(),
    }
}

#[test]
fn schema_apply_is_idempotent() {
    // Ledger::open_in_memory applies schema once; re-applying is a no-op
    // because every CREATE statement is IF NOT EXISTS.
    let l = open();
    crate::ledger::schema::apply(&l).expect("re-apply schema");
    crate::ledger::schema::apply(&l).expect("re-apply schema again");
}

#[test]
fn upsert_then_read_round_trip() {
    let l = open();
    let wid = ensure_workitem(&l, "rename-portrait-to-spectrum");
    let turns = vec![turn_fixture("ai-1", "u-1"), turn_fixture("ai-2", "u-2")];
    let review = Review {
        accepted: Some("ack".to_string()),
        rejected: None,
        verified_manually: None,
        rewrote_by_hand: None,
    };
    let tags = vec!["reject".to_string(), "name-the-failure".to_string()];
    let id = l.upsert_gem(new_gem(wid, &turns, &review, &tags)).expect("upsert");
    let g = l.gem_by_id(id).expect("read by id").expect("present");
    assert_eq!(g.workitem_id, wid);
    assert_eq!(g.tags, tags);
    assert_eq!(g.interaction.len(), 2);
    assert_eq!(g.interaction[0].ai_turn_uuid, "ai-1");
    assert_eq!(g.interaction[1].user_turn_uuid, "u-2");
    assert_eq!(g.review.accepted.as_deref(), Some("ack"));
}

#[test]
fn upsert_is_idempotent_on_same_content_hash() {
    let l = open();
    let wid = ensure_workitem(&l, "rename");
    let turns = vec![turn_fixture("ai-1", "u-1"), turn_fixture("ai-2", "u-2")];
    let review = Review::default();
    let tags = vec!["reject".to_string()];
    let id1 = l
        .upsert_gem(new_gem(wid, &turns, &review, &tags))
        .expect("first upsert");
    let id2 = l
        .upsert_gem(new_gem(wid, &turns, &review, &tags))
        .expect("second upsert");
    assert_eq!(id1, id2);
    let all = l.gems_for_workitem(wid).expect("list");
    assert_eq!(all.len(), 1);
}

#[test]
fn upsert_is_stable_against_turn_order() {
    // Same set of turn UUIDs but different ordering -> same content_hash,
    // same gem row (the boundary UUIDs may flip; that's fine, they're
    // not part of the key).
    let l = open();
    let wid = ensure_workitem(&l, "rename");
    let review = Review::default();
    let tags = vec!["reject".to_string()];
    let forward = vec![turn_fixture("ai-1", "u-1"), turn_fixture("ai-2", "u-2")];
    let reversed = vec![turn_fixture("ai-2", "u-2"), turn_fixture("ai-1", "u-1")];
    let id_fwd = l
        .upsert_gem(new_gem(wid, &forward, &review, &tags))
        .expect("upsert forward");
    let id_rev = l
        .upsert_gem(new_gem(wid, &reversed, &review, &tags))
        .expect("upsert reversed");
    assert_eq!(id_fwd, id_rev);
}

#[test]
fn upsert_replaces_interaction_turns_on_revision() {
    // A re-extract over the same turn set with different per-turn tags
    // must update the interaction_turns rows (not append, not leave
    // stale rows).
    let l = open();
    let wid = ensure_workitem(&l, "rename");
    let mut turns_first = vec![turn_fixture("ai-1", "u-1"), turn_fixture("ai-2", "u-2")];
    turns_first[0].tags = vec!["frame".to_string()];
    let review = Review::default();
    let tags = vec!["reject".to_string()];
    let id = l
        .upsert_gem(new_gem(wid, &turns_first, &review, &tags))
        .expect("upsert first");

    let mut turns_second = turns_first.clone();
    turns_second[0].tags = vec!["reject".to_string(), "name-the-failure".to_string()];
    let id_again = l
        .upsert_gem(new_gem(wid, &turns_second, &review, &tags))
        .expect("upsert second");
    assert_eq!(id, id_again);

    let g = l.gem_by_id(id).expect("read").expect("present");
    assert_eq!(g.interaction.len(), 2);
    assert_eq!(
        g.interaction[0].tags,
        vec!["reject".to_string(), "name-the-failure".to_string()]
    );
}

#[test]
fn gem_by_content_hash_returns_none_when_missing() {
    let l = open();
    let wid = ensure_workitem(&l, "rename");
    let g = l
        .gem_by_content_hash(wid, "deadbeef")
        .expect("query gem_by_content_hash");
    assert!(g.is_none());
}

#[test]
fn gem_by_content_hash_round_trips() {
    let l = open();
    let wid = ensure_workitem(&l, "rename");
    let turns = vec![turn_fixture("ai-1", "u-1"), turn_fixture("ai-2", "u-2")];
    let review = Review::default();
    let tags = vec!["reject".to_string()];
    let id = l.upsert_gem(new_gem(wid, &turns, &review, &tags)).expect("upsert");

    let hash = {
        // Re-compute the same way the upsert path does.
        let mut uuids: Vec<&str> = turns
            .iter()
            .flat_map(|t| [t.ai_turn_uuid.as_str(), t.user_turn_uuid.as_str()])
            .collect();
        uuids.sort_unstable();
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        for (idx, u) in uuids.iter().enumerate() {
            if idx > 0 {
                h.update(b"|");
            }
            h.update(u.as_bytes());
        }
        hex::encode(h.finalize())
    };

    let g = l.gem_by_content_hash(wid, &hash).expect("lookup").expect("present");
    assert_eq!(g.id, id);
}

#[test]
fn upsert_rejects_empty_interaction() {
    let l = open();
    let wid = ensure_workitem(&l, "rename");
    let review = Review::default();
    let tags: Vec<String> = vec![];
    let err = l
        .upsert_gem(new_gem(wid, &[], &review, &tags))
        .expect_err("empty interaction must error");
    assert!(format!("{err:#}").contains("empty interaction"));
}

#[test]
fn gems_for_workitem_orders_by_extracted_at() {
    let l = open();
    let wid = ensure_workitem(&l, "rename");
    let review = Review::default();
    let tags: Vec<String> = vec![];

    let turns_a = vec![turn_fixture("ai-1", "u-1"), turn_fixture("ai-2", "u-2")];
    let mut new_a = new_gem(wid, &turns_a, &review, &tags);
    new_a.extracted_at = Utc
        .with_ymd_and_hms(2026, 5, 25, 12, 0, 0)
        .single()
        .expect("valid timestamp");
    let id_a = l.upsert_gem(new_a).expect("upsert a");

    let turns_b = vec![turn_fixture("ai-3", "u-3"), turn_fixture("ai-4", "u-4")];
    let mut new_b = new_gem(wid, &turns_b, &review, &tags);
    new_b.extracted_at = Utc
        .with_ymd_and_hms(2026, 5, 26, 12, 0, 0)
        .single()
        .expect("valid timestamp");
    let id_b = l.upsert_gem(new_b).expect("upsert b");

    let all = l.gems_for_workitem(wid).expect("list");
    assert_eq!(all.len(), 2);
    assert_eq!(all[0].id, id_a);
    assert_eq!(all[1].id, id_b);
}
