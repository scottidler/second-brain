use super::*;
use chrono::Utc;

fn make_turn(uuid: &str, role: Role) -> Turn {
    Turn {
        uuid: uuid.to_string(),
        parent_uuid: None,
        timestamp: Utc::now(),
        role,
        content: vec![],
        model: None,
    }
}

fn alternating(n: usize) -> Vec<Turn> {
    (0..n)
        .map(|i| {
            let role = if i % 2 == 0 { Role::User } else { Role::Assistant };
            make_turn(&format!("t-{i}"), role)
        })
        .collect()
}

#[test]
fn empty_input_yields_empty_output() {
    let chunks = chunk_turns(&[], 50, 4);
    assert!(chunks.is_empty());
}

#[test]
fn short_input_returns_single_chunk() {
    let turns = alternating(10);
    let chunks = chunk_turns(&turns, 50, 4);
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].len(), 10);
}

#[test]
fn exact_max_returns_single_chunk() {
    let turns = alternating(50);
    let chunks = chunk_turns(&turns, 50, 4);
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].len(), 50);
}

#[test]
fn splits_on_user_boundary_within_window() {
    // 100 alternating turns, max 50, overlap 4. The chunker should
    // split at the LAST user-turn index <= 50 (which is index 48,
    // since indices 0, 2, 4, ..., 48 are user turns).
    let turns = alternating(100);
    let chunks = chunk_turns(&turns, 50, 4);
    assert!(chunks.len() >= 2);
    // First chunk ends just before turn-48 (last user index in window).
    assert_eq!(chunks[0].len(), 48);
    assert_eq!(chunks[0].last().expect("non-empty chunk").uuid, "t-47");
}

#[test]
fn overlap_window_is_applied_between_chunks() {
    let turns = alternating(100);
    let chunks = chunk_turns(&turns, 50, 4);
    // Second chunk should start at split_at - overlap = 48 - 4 = 44.
    assert!(chunks.len() >= 2);
    assert_eq!(chunks[1].first().expect("non-empty chunk").uuid, "t-44");
}

#[test]
fn overlap_clamped_to_max_minus_one() {
    // Overlap larger than the cap is clamped to cap - 1 so progress is
    // guaranteed.
    let turns = alternating(100);
    let chunks = chunk_turns(&turns, 10, 1000);
    // We don't care about exact partitioning here, only that the chunker
    // terminates and produces multiple chunks covering all turns.
    assert!(chunks.len() >= 2);
    assert!(chunks.iter().all(|c| !c.is_empty()));
}

#[test]
fn all_assistant_turns_falls_back_to_hard_split() {
    // No user turns in the window -> hard split at window_end with a
    // WARN log. The fallback is needed for malformed transcripts; we
    // assert the chunker doesn't infinite-loop.
    let turns: Vec<Turn> = (0..120)
        .map(|i| make_turn(&format!("a-{i}"), Role::Assistant))
        .collect();
    let chunks = chunk_turns(&turns, 50, 4);
    assert!(chunks.len() >= 2);
    let total_unique_uuids: std::collections::HashSet<&str> =
        chunks.iter().flat_map(|c| c.iter().map(|t| t.uuid.as_str())).collect();
    assert_eq!(total_unique_uuids.len(), 120);
}

#[test]
fn max_zero_returns_single_oversized_chunk() {
    // Degenerate config: emit a single chunk (and log WARN). Guards
    // against infinite-loop interpretations of "max=0 means no max."
    let turns = alternating(10);
    let chunks = chunk_turns(&turns, 0, 4);
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].len(), 10);
}
