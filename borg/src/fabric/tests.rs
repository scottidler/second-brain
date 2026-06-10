//! Regression tests for the chunking primitives. Both `split_with_overlap`
//! and `find_break_point` previously panicked on multi-byte input (a raw byte
//! cut inside a codepoint) and could stall / slice backwards for small chunk
//! sizes; the fixed-panic comments in `fabric.rs` cite the exact cases pinned
//! here.

use super::*;

// --- split_with_overlap ---

#[test]
fn split_short_text_returns_single_chunk() {
    let text = "short text under the limit";
    let chunks = split_with_overlap(text, 1000, 100);
    assert_eq!(chunks, vec![text.to_string()]);
}

#[test]
fn split_zero_chunk_size_returns_single_chunk() {
    // chunk_size == 0 would never advance; treated as "no chunking".
    let text = "some content here";
    let chunks = split_with_overlap(text, 0, 0);
    assert_eq!(chunks, vec![text.to_string()]);
}

#[test]
fn split_zero_overlap_reconstructs_original() {
    // With no overlap the concatenation of chunks is exactly the input - the
    // splitter must not drop or duplicate any bytes.
    let text = "Para one.\n\nPara two is a bit longer.\n\nPara three ends it here.";
    let chunks = split_with_overlap(text, 20, 0);
    assert!(chunks.len() > 1, "expected multiple chunks, got {}", chunks.len());
    assert_eq!(chunks.concat(), text);
}

#[test]
fn split_covers_text_edges_with_overlap() {
    let text = "a".repeat(1000);
    let chunks = split_with_overlap(&text, 100, 10);
    assert!(chunks.len() > 1);
    assert!(text.starts_with(chunks.first().expect("at least one chunk").as_str()));
    assert!(text.ends_with(chunks.last().expect("at least one chunk").as_str()));
}

#[test]
fn split_multibyte_does_not_panic_and_aligns_edges() {
    // Regression: a raw byte cut inside a multi-byte codepoint panicked. Each
    // char here is 4 bytes, so any byte-arithmetic cut lands mid-codepoint.
    let text = "😀😀😀😀😀😀😀😀😀😀😀😀😀😀😀😀😀😀😀😀";
    let chunks = split_with_overlap(text, 10, 3);
    assert!(!chunks.is_empty());
    // Reaching here without panicking is the core assertion; the edges must
    // still line up with the input.
    assert!(text.starts_with(chunks.first().expect("at least one chunk").as_str()));
    assert!(text.ends_with(chunks.last().expect("at least one chunk").as_str()));
}

#[test]
fn split_tiny_chunk_size_terminates_without_stall() {
    // Regression: find_break_point's 200-byte lookback can return an offset
    // <= start for small chunks; the loop must still advance and terminate.
    let text = "one two three four five six seven eight nine ten";
    let chunks = split_with_overlap(text, 5, 2);
    assert!(!chunks.is_empty());
    // Reaching here proves the loop advanced to completion (no infinite stall).
    assert!(text.starts_with(chunks.first().expect("at least one chunk").as_str()));
}

#[test]
fn split_tiny_chunk_multibyte_does_not_panic() {
    // Combined regression: small chunk + multibyte content exercises both the
    // mid-codepoint cut and the sub-start lookback in the same call.
    let text = "café ☕ résumé ñoño 日本語 emoji 😀 done now";
    let chunks = split_with_overlap(text, 4, 2);
    assert!(!chunks.is_empty());
}

// --- find_break_point ---

#[test]
fn break_point_prefers_paragraph() {
    let text = "first para.\n\nsecond para continues here";
    let bp = find_break_point(text, 0, text.len());
    assert_eq!(&text[..bp], "first para.\n\n");
}

#[test]
fn break_point_falls_back_to_sentence() {
    let text = "one sentence. another sentence without breaks";
    let bp = find_break_point(text, 0, text.len());
    assert_eq!(&text[..bp], "one sentence. ");
}

#[test]
fn break_point_falls_back_to_line() {
    let text = "line one\nline two with no sentence end";
    let bp = find_break_point(text, 0, text.len());
    assert_eq!(&text[..bp], "line one\n");
}

#[test]
fn break_point_falls_back_to_end_when_no_boundary() {
    let text = "nobreakshereatall";
    let bp = find_break_point(text, 0, text.len());
    assert_eq!(bp, text.len());
}

#[test]
fn break_point_snaps_midcodepoint_bounds_without_panic() {
    // Regression: callers pass byte-arithmetic offsets that can land inside a
    // multi-byte codepoint; both bounds must be floored to char boundaries
    // before slicing the search region.
    let text = "😀😀😀😀😀"; // 5 × 4 bytes = 20 bytes
    // search_start=1 and end=15 both land mid-codepoint.
    let bp = find_break_point(text, 1, 15);
    assert!(
        text.is_char_boundary(bp),
        "break point {bp} must sit on a char boundary"
    );
}
