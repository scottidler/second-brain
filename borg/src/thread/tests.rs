use super::*;

// --- platform_label ---

#[test]
fn platform_label_known_platforms() {
    assert_eq!(platform_label("x"), "X");
    assert_eq!(platform_label("reddit"), "Reddit");
    assert_eq!(platform_label("hn"), "Hacker News");
}

#[test]
fn platform_label_unknown_platform_capitalizes_defensively() {
    assert_eq!(platform_label("mastodon"), "Mastodon");
    assert_eq!(platform_label(""), "");
}

// --- title_snippet ---

#[test]
fn title_snippet_prefers_tldr_over_summary() {
    let snippet = title_snippet(Some("the tldr"), "the summary");
    assert_eq!(snippet.as_deref(), Some("the tldr"));
}

#[test]
fn title_snippet_falls_back_to_summary_when_tldr_absent() {
    let snippet = title_snippet(None, "the summary");
    assert_eq!(snippet.as_deref(), Some("the summary"));
}

#[test]
fn title_snippet_falls_back_to_summary_when_tldr_empty() {
    let snippet = title_snippet(Some(""), "the summary");
    assert_eq!(snippet.as_deref(), Some("the summary"));
}

#[test]
fn title_snippet_none_when_both_absent_or_empty() {
    assert_eq!(title_snippet(None, ""), None);
    assert_eq!(title_snippet(Some(""), ""), None);
}

#[test]
fn title_snippet_truncates_summary_at_word_boundary() {
    let long = "one two three four five six seven eight nine ten eleven twelve thirteen fourteen fifteen sixteen";
    let snippet = title_snippet(None, long).expect("snippet");
    assert!(snippet.chars().count() <= SNIPPET_MAX_CHARS);
    // Must not end mid-word: the truncated text must be a prefix of `long`
    // up to a space boundary, never a chopped word.
    assert!(long.starts_with(&snippet));
    assert!(!snippet.ends_with(' '));
}

#[test]
fn title_snippet_collapses_embedded_newlines_without_lowercasing() {
    let tldr = "Fjall is a Rust\nKV store\n\nbuilt by Peter Steinberger";
    let snippet = title_snippet(Some(tldr), "").expect("snippet");
    assert_eq!(snippet, "Fjall is a Rust KV store built by Peter Steinberger");
    assert!(!snippet.contains('\n'));
}

// --- title_for_thread (Phase 1 success criteria a-e) ---

#[test]
fn title_for_thread_author_handle_and_snippet_preserves_casing() {
    // (a)
    let title = title_for_thread("x", Some("@tom_doerr"), Some("Fjall is a Rust KV store"), "...");
    assert_eq!(title.as_deref(), Some("@tom_doerr on X: \"Fjall is a Rust KV store\""));
}

#[test]
fn title_for_thread_author_display_name_preserves_casing() {
    // (b)
    let title = title_for_thread("x", Some("Peter Steinberger"), Some("Fjall benchmarks"), "...");
    assert_eq!(title.as_deref(), Some("Peter Steinberger on X: \"Fjall benchmarks\""));
}

#[test]
fn title_for_thread_none_when_author_and_snippet_both_absent() {
    // (c)
    let title = title_for_thread("reddit", None, None, "");
    assert_eq!(title, None);
}

#[test]
fn title_for_thread_author_only_hacker_news() {
    // (d)
    let title = title_for_thread("hn", Some("dang"), None, "");
    assert_eq!(title.as_deref(), Some("dang on Hacker News"));
}

#[test]
fn title_for_thread_embedded_newlines_collapse_to_single_line_without_lowercasing() {
    // (e)
    let tldr = "Fjall is a Rust\nKV store built by\nPeter Steinberger";
    let title = title_for_thread("x", Some("@tom_doerr"), Some(tldr), "...").expect("title");
    assert_eq!(
        title,
        "@tom_doerr on X: \"Fjall is a Rust KV store built by Peter Steinberger\""
    );
    assert_eq!(title.lines().count(), 1);
}

#[test]
fn title_for_thread_snippet_only_uses_platform_label() {
    let title = title_for_thread("reddit", None, Some("a snippet with no author"), "");
    assert_eq!(title.as_deref(), Some("Reddit thread: \"a snippet with no author\""));
}

#[test]
fn title_for_thread_empty_author_string_treated_as_absent() {
    let title = title_for_thread("hn", Some(""), Some("a snippet"), "");
    assert_eq!(title.as_deref(), Some("Hacker News thread: \"a snippet\""));
}

#[test]
fn title_for_thread_is_pure_and_deterministic() {
    // Same inputs, called twice, produce byte-identical output - no hidden
    // state, no I/O, no LLM call.
    let a = title_for_thread("x", Some("@tom_doerr"), Some("stable snippet"), "...");
    let b = title_for_thread("x", Some("@tom_doerr"), Some("stable snippet"), "...");
    assert_eq!(a, b);
}
