//! Deterministic, LLM-free thread/social note title construction.
//!
//! Mirrors the shape of `github::extract_repo_slugs`: pure, unit-testable,
//! no network/LLM calls. Consumed at the `is_thread` seam in
//! `borg/src/pipeline.rs` to replace the generic article-title extractor
//! for X/Reddit/HN thread notes, whose scraped page title can degenerate to
//! a bare numeric post ID (see design doc
//! `docs/design/2026-07-08-thread-title-generation.md`).

/// Longest snippet quoted in a title before truncating at a word boundary.
const SNIPPET_MAX_CHARS: usize = 80;

/// Platform identifier -> human-readable label for the title. Unknown
/// platform identifiers pass through capitalized (defensive;
/// `vault::distilled::ThreadPayload::platform` is currently always one of
/// "x" | "reddit" | "hn").
pub fn platform_label(platform: &str) -> String {
    match platform {
        "x" => "X".to_string(),
        "reddit" => "Reddit".to_string(),
        "hn" => "Hacker News".to_string(),
        other => capitalize(other),
    }
}

/// Capitalize the first character of `s`, leaving the rest untouched.
/// Char-boundary-safe (`chars()`, never byte-slices a `&str`).
fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// Collapse all internal whitespace runs (including embedded newlines) to a
/// single space and trim the ends. Deliberately a LOCAL helper, NOT
/// `hygiene::normalize_text_input` - that also lowercases, which would mangle
/// an author handle's or a proper noun's casing in the title.
fn collapse_whitespace(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Truncate `s` (already whitespace-collapsed) to at most `max_chars`
/// characters, cutting at the nearest preceding word boundary rather than
/// mid-word. Char-boundary-safe throughout (`chars()`, never byte-slices).
fn truncate_at_word_boundary(s: &str, max_chars: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max_chars {
        return s.to_string();
    }
    let cut = chars[..max_chars]
        .iter()
        .rposition(|c| c.is_whitespace())
        .unwrap_or(max_chars);
    chars[..cut].iter().collect::<String>().trim_end().to_string()
}

/// Longest usable text to quote in the title: `tldr` when present and
/// non-empty (after whitespace-collapse), else the first ~80 chars of
/// `summary` at a word boundary, else `None`.
pub fn title_snippet(tldr: Option<&str>, summary: &str) -> Option<String> {
    if let Some(collapsed) = tldr.map(collapse_whitespace).filter(|s| !s.is_empty()) {
        return Some(collapsed);
    }
    let collapsed_summary = collapse_whitespace(summary);
    if collapsed_summary.is_empty() {
        return None;
    }
    Some(truncate_at_word_boundary(&collapsed_summary, SNIPPET_MAX_CHARS))
}

/// Build a thread note title from data the pipeline already has. `author` is
/// used VERBATIM, whatever shape the LLM extracted - an `@handle` or a
/// display name. This function does not normalize or choose between them.
///
/// Shapes, in priority order:
/// 1. author + snippet -> `"<author> on <Platform>: \"<snippet>\""`
/// 2. author only       -> `"<author> on <Platform>"`
/// 3. snippet only       -> `"<Platform> thread: \"<snippet>\""`
/// 4. neither            -> `None` (caller substitutes a generic platform-only title)
pub fn title_for_thread(platform: &str, author: Option<&str>, tldr: Option<&str>, summary: &str) -> Option<String> {
    let label = platform_label(platform);
    let author = author.filter(|s| !s.is_empty());
    let snippet = title_snippet(tldr, summary);

    match (author, snippet) {
        (Some(author), Some(snippet)) => Some(format!("{author} on {label}: \"{snippet}\"")),
        (Some(author), None) => Some(format!("{author} on {label}")),
        (None, Some(snippet)) => Some(format!("{label} thread: \"{snippet}\"")),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests;
