use super::*;
use chrono::TimeZone;

fn turn(ai_uuid: &str, user_uuid: &str) -> InteractionTurn {
    InteractionTurn {
        ai_says: format!("ai response for {ai_uuid}"),
        ai_turn_uuid: ai_uuid.to_string(),
        user_says: format!("user reply for {user_uuid}"),
        user_turn_uuid: user_uuid.to_string(),
        tags: vec!["frame".to_string()],
    }
}

fn gem_fixture(turns: Vec<InteractionTurn>) -> Gem {
    Gem {
        id: 0,
        workitem_id: 42,
        session_uuid: "session-abc".to_string(),
        task: "rename portrait to spectrum".to_string(),
        context_loaded: vec!["facet/src/extract/portrait.rs".to_string()],
        context_missing: vec![],
        interaction: turns,
        review: Review {
            accepted: Some("ack".to_string()),
            rejected: None,
            verified_manually: None,
            rewrote_by_hand: None,
        },
        tags: vec!["reject".to_string(), "name-the-failure".to_string()],
        why_it_matters: "renames cascade".to_string(),
        extractor_model: "claude-sonnet-4-6".to_string(),
        extracted_at: Utc
            .with_ymd_and_hms(2026, 5, 26, 12, 0, 0)
            .single()
            .expect("valid fixture timestamp"),
    }
}

#[test]
fn content_hash_is_stable_across_calls() {
    let g = gem_fixture(vec![turn("ai-1", "u-1"), turn("ai-2", "u-2")]);
    let h1 = g.content_hash();
    let h2 = g.content_hash();
    assert_eq!(h1, h2);
}

#[test]
fn content_hash_is_sha256_hex_length() {
    let g = gem_fixture(vec![turn("ai-1", "u-1"), turn("ai-2", "u-2")]);
    assert_eq!(g.content_hash().len(), 64);
    assert!(g.content_hash().chars().all(|c| c.is_ascii_hexdigit()));
}

#[test]
fn content_hash_independent_of_turn_order() {
    // Two gems with the same set of turn UUIDs but in different insertion
    // order must produce the same content_hash. This is the property that
    // makes the hash stable across chunker-boundary shifts.
    let g_forward = gem_fixture(vec![turn("ai-1", "u-1"), turn("ai-2", "u-2")]);
    let g_reversed = gem_fixture(vec![turn("ai-2", "u-2"), turn("ai-1", "u-1")]);
    assert_eq!(g_forward.content_hash(), g_reversed.content_hash());
}

#[test]
fn content_hash_differs_for_different_turn_sets() {
    let g_a = gem_fixture(vec![turn("ai-1", "u-1"), turn("ai-2", "u-2")]);
    let g_b = gem_fixture(vec![turn("ai-1", "u-1"), turn("ai-3", "u-3")]);
    assert_ne!(g_a.content_hash(), g_b.content_hash());
}

#[test]
fn boundary_user_turn_uuids_returns_first_and_last() {
    let g = gem_fixture(vec![turn("ai-1", "u-1"), turn("ai-2", "u-2"), turn("ai-3", "u-3")]);
    let (first, last) = g
        .boundary_user_turn_uuids()
        .expect("non-empty interaction must yield boundaries");
    assert_eq!(first, "u-1");
    assert_eq!(last, "u-3");
}

#[test]
fn boundary_user_turn_uuids_none_for_empty_interaction() {
    let g = gem_fixture(vec![]);
    assert!(g.boundary_user_turn_uuids().is_none());
}

#[test]
fn gem_roundtrips_through_json() {
    let g = gem_fixture(vec![turn("ai-1", "u-1"), turn("ai-2", "u-2")]);
    let json = serde_json::to_string(&g).expect("serialize gem");
    let back: Gem = serde_json::from_str(&json).expect("deserialize gem");
    assert_eq!(g, back);
}

#[test]
fn gem_deserializes_v2_extractor_output() {
    // Minimal shape matching the v2 pattern's output. id defaults to 0
    // (not present in extractor output; ledger fills it on upsert).
    let raw = r#"{
        "workitem_id": 1,
        "session_uuid": "abc",
        "task": "rename a file",
        "context_loaded": ["file.rs"],
        "context_missing": [],
        "interaction": [
            {
                "ai_says": "proposing X",
                "ai_turn_uuid": "ai-1",
                "user_says": "no, do Y",
                "user_turn_uuid": "u-1",
                "tags": ["reject"]
            },
            {
                "ai_says": "doing Y",
                "ai_turn_uuid": "ai-2",
                "user_says": "ack",
                "user_turn_uuid": "u-2",
                "tags": ["verify"]
            }
        ],
        "review": {"accepted": "ack", "rejected": null, "verified_manually": null, "rewrote_by_hand": null},
        "tags": ["reject"],
        "why_it_matters": "rename pattern",
        "extractor_model": "claude-sonnet-4-6",
        "extracted_at": "2026-05-26T12:00:00Z"
    }"#;
    let g: Gem = serde_json::from_str(raw).expect("deserialize v2 extractor output");
    assert_eq!(g.id, 0);
    assert_eq!(g.interaction.len(), 2);
    assert_eq!(g.review.accepted.as_deref(), Some("ack"));
}
