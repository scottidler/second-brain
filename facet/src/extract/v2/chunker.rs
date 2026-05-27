//! Turn-range chunker for the v2 extractor.
//!
//! Splits a span of [`Turn`]s into chunks suitable for one fabric call
//! each. Per the design doc's Phase 3 sub-spec:
//!
//! - Split only on user-turn boundaries (never mid-AI-response).
//! - Max-turns-per-chunk cap (default 50; configurable).
//! - Overlap window (default 4 turns): when a chunk is split, the next
//!   chunk includes the last N turns of the prior chunk so an arc that
//!   crossed the boundary still has both halves visible in one of the
//!   two windows.
//! - The chunker is heuristic, not semantic; the LLM is responsible
//!   for recognising "this span doesn't contain a gem" and returning
//!   an empty array.

use crate::jsonl::{Role, Turn};

#[cfg(test)]
mod tests;

pub const DEFAULT_MAX_TURNS_PER_CHUNK: usize = 50;
pub const DEFAULT_OVERLAP_TURNS: usize = 4;

/// Split `turns` into chunks. Each chunk is at most `max_turns` long;
/// adjacent chunks overlap by `overlap` turns (clamped to `max_turns - 1`).
///
/// The split point is the LAST user-turn boundary at or before
/// `max_turns`. If no user-turn boundary exists within the window, the
/// chunker falls back to splitting at `max_turns` exactly (logged at
/// WARN; rare in practice because user turns are interleaved).
///
/// Returns owned `Vec<Vec<Turn>>` because the v2 extractor wants to
/// pass each chunk to a separate fabric task. Callers that hold onto
/// the original slice can wrap each `Vec<Turn>` in `Arc` if needed.
pub fn chunk_turns(turns: &[Turn], max_turns: usize, overlap: usize) -> Vec<Vec<Turn>> {
    log::debug!(
        "chunk_turns: total_turns={} max_turns={} overlap={}",
        turns.len(),
        max_turns,
        overlap,
    );

    if turns.is_empty() {
        return vec![];
    }
    if max_turns == 0 {
        log::warn!("chunk_turns: max_turns=0; returning single oversized chunk");
        return vec![turns.to_vec()];
    }
    if turns.len() <= max_turns {
        return vec![turns.to_vec()];
    }

    let overlap = overlap.min(max_turns.saturating_sub(1));
    let mut chunks = Vec::new();
    let mut start = 0usize;

    while start < turns.len() {
        let window_end = (start + max_turns).min(turns.len());
        if window_end == turns.len() {
            chunks.push(turns[start..window_end].to_vec());
            break;
        }
        let split = last_user_boundary_in(turns, start, window_end);
        let split_at = match split {
            Some(idx) => idx,
            None => {
                log::warn!(
                    "chunk_turns: no user-turn boundary in [{start}, {window_end}); \
                     falling back to hard split at {window_end}"
                );
                window_end
            }
        };
        chunks.push(turns[start..split_at].to_vec());
        let next_start = split_at.saturating_sub(overlap);
        if next_start <= start {
            log::warn!(
                "chunk_turns: forward-progress check failed (next_start={next_start} <= start={start}); \
                 forcing single-turn advance to avoid an infinite loop"
            );
            start += 1;
        } else {
            start = next_start;
        }
    }
    log::debug!("chunk_turns: produced {} chunk(s)", chunks.len());
    chunks
}

/// Index of the LAST turn in `turns[start..end]` whose role is User,
/// returned as an absolute index into `turns`. Returns `None` if the
/// window has no user turns.
///
/// The chunker splits just BEFORE this turn (so the user turn starts
/// the next chunk's first exchange), which means the index returned
/// here is the *exclusive* end of the chunk being closed.
fn last_user_boundary_in(turns: &[Turn], start: usize, end: usize) -> Option<usize> {
    if start >= end {
        return None;
    }
    // Walk backward from end-1 to start+1: the chunk being closed must
    // be at least one turn long, and the first turn at `start` cannot
    // be itself the boundary (that would produce an empty chunk).
    let mut i = end;
    while i > start + 1 {
        i -= 1;
        if matches!(turns[i].role, Role::User) {
            return Some(i);
        }
    }
    None
}
