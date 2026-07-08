//! Deterministic, LLM-free thread/social note title construction.
//!
//! Mirrors the shape of `github::extract_repo_slugs`: pure, unit-testable,
//! no network/LLM calls. Consumed at the `is_thread` seam in
//! `borg/src/pipeline.rs` to replace the generic article-title extractor
//! for X/Reddit/HN thread notes, whose scraped page title can degenerate to
//! a bare numeric post ID (see design doc
//! `docs/design/2026-07-08-thread-title-generation.md`).

use vault::distilled::{Distilled, KindPayload};

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

/// Resolve a thread/social note's title from its distilled output.
///
/// Threads NEVER consult the scraped page title: `extract_article_title`'s
/// Strategy 3 degenerates to a bare numeric post ID for X/Reddit/HN status
/// URLs served by the browser-UA fallback fetcher (design doc
/// `docs/design/2026-07-08-thread-title-generation.md`). Instead:
///
/// - A successful distillation builds [`title_for_thread`] from the
///   LLM-extracted author plus the distilled `tldr`/`summary`.
/// - A fallback distillation goes straight to a generic `"<Platform> thread"`
///   (or `"Thread thread"` when the platform is unknown), logged at `warn!`.
///
/// The fallback branch is keyed on `meta.validation.fallback_reason` being set,
/// NOT on `kind_specific` being `None`. The design doc's original wiring assumed
/// a distiller fallback leaves `kind_specific` absent, but
/// `ThreadDistiller::distill` calls `attach_platform` UNCONDITIONALLY, so a
/// fabric-timeout / yaml-parse-error / missing-summary fallback exits with
/// `kind_specific = Some(Thread { author: None, platform, .. })` and a
/// `"[reason]\n\n<snippet>"` summary. `fallback_reason` is the typed,
/// authoritative "degraded distillation" signal; keying on it both (a) never
/// reads that summary, so the internal `[reason]` string can never leak into a
/// title, and (b) still recovers the platform label from the attached payload
/// so an x-platform fallback titles as `"X thread"`, not `"Thread thread"`.
pub fn thread_title(distilled: &Distilled, trace_id: &str) -> String {
    let payload = match &distilled.kind_specific {
        Some(KindPayload::Thread(t)) => Some(t),
        _ => None,
    };
    let is_fallback = distilled.meta.validation.fallback_reason.is_some();
    log::debug!(
        "thread_title: trace={trace_id} has_payload={} platform={:?} has_author={} has_tldr={} is_fallback={is_fallback}",
        payload.is_some(),
        payload.map(|t| t.platform.as_str()),
        payload.is_some_and(|t| t.author.is_some()),
        distilled.tldr.is_some(),
    );

    let built = if is_fallback {
        None
    } else {
        payload.and_then(|t| {
            title_for_thread(
                &t.platform,
                t.author.as_deref(),
                distilled.tldr.as_deref(),
                &distilled.summary,
            )
        })
    };

    built.unwrap_or_else(|| {
        let label = payload
            .map(|t| platform_label(&t.platform))
            .unwrap_or_else(|| "Thread".to_string());
        log::warn!(
            "[{trace_id}] thread_title: no usable author/snippet (is_fallback={is_fallback}); using generic '{label} thread' title"
        );
        format!("{label} thread")
    })
}

/// Title-selection seam at the end of the non-YouTube URL branch of the
/// pipeline. Threads route through [`thread_title`]; every other kind keeps the
/// `article_title` it arrived with (`owner/repo` for github repos, the scraped
/// title for plain articles) byte-identically -- no behavior change outside the
/// `is_thread` arm.
pub fn resolve_title(is_thread: bool, article_title: String, distilled: &Distilled, trace_id: &str) -> String {
    if is_thread { thread_title(distilled, trace_id) } else { article_title }
}

#[cfg(test)]
mod tests;
