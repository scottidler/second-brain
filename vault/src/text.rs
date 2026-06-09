//! UTF-8-safe string truncation helpers.
//!
//! The single home for character-accurate, panic-free truncation across the
//! workspace. Production code previously truncated user-controlled strings by
//! *byte* index behind a *byte-length* guard (`if s.len() > 50 { &s[..50] }`),
//! so any multi-byte codepoint straddling the cut boundary panicked the daemon.
//!
//! Both helpers count **characters**, not bytes, and never split a codepoint.
//! Modeled on the char-budgeted prior art at `distillers::validate`
//! (`char_indices().nth(max_chars)`), not on the byte-budgeted
//! `cortex::fabric::truncate_input` (`floor_char_boundary`, correct for a
//! token estimate but wrong for a character count).

/// Byte index of the `max_chars`-th character, or `None` when `input` has at
/// most `max_chars` characters (i.e. no cut is needed).
fn char_cut(input: &str, max_chars: usize) -> Option<usize> {
    input.char_indices().nth(max_chars).map(|(idx, _)| idx)
}

/// Truncate `input` to at most `max_chars` characters, appending the ASCII
/// ellipsis "..." only when a cut occurred. Always returns a valid UTF-8
/// string; never panics on a multi-byte boundary. `max_chars` counts
/// characters, not bytes. `max_chars == 0` yields "..." for non-empty input,
/// "" for empty input.
pub fn truncate_with_ellipsis(input: &str, max_chars: usize) -> String {
    log::trace!(
        "truncate_with_ellipsis: input_len={} max_chars={}",
        input.len(),
        max_chars
    );
    match char_cut(input, max_chars) {
        Some(end) => format!("{}...", &input[..end]),
        None => input.to_string(),
    }
}

/// Truncate `input` to at most `max_chars` characters without an ellipsis.
/// Borrows when `input` already fits; otherwise borrows the head slice cut at
/// the byte index of the `max_chars`-th character. `max_chars` counts
/// characters, not bytes.
pub fn truncate(input: &str, max_chars: usize) -> &str {
    log::trace!("truncate: input_len={} max_chars={}", input.len(), max_chars);
    match char_cut(input, max_chars) {
        Some(end) => &input[..end],
        None => input,
    }
}

#[cfg(test)]
mod tests;
