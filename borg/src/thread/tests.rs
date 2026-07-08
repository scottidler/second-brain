use super::*;
use vault::distilled::{Distilled, DistilledMeta, ThreadPayload, ValidationMeta};

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

// --- thread_title / resolve_title (Phase 2 seam success criteria a-c) ---

/// A purely-numeric title is the exact bug this design exists to eliminate.
fn is_purely_numeric(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_digit())
}

/// Build a synthetic thread `Distilled` at the shape the pipeline seam sees.
fn thread_distilled(
    payload: Option<ThreadPayload>,
    tldr: Option<&str>,
    summary: &str,
    fallback_reason: Option<&str>,
) -> Distilled {
    Distilled {
        summary: summary.to_string(),
        tldr: tldr.map(str::to_string),
        kind_specific: payload.map(KindPayload::Thread),
        meta: DistilledMeta {
            validation: ValidationMeta {
                fallback_reason: fallback_reason.map(str::to_string),
                ..Default::default()
            },
            ..Default::default()
        },
        ..Default::default()
    }
}

#[test]
fn thread_title_success_path_matches_builder_and_is_non_numeric() {
    // (a) synthetic success-path Distilled with author + tldr -> the resulting
    // title is non-numeric and byte-identical to `title_for_thread`'s output.
    let payload = ThreadPayload {
        author: Some("@tom_doerr".into()),
        platform: "x".into(),
        ..Default::default()
    };
    let distilled = thread_distilled(
        Some(payload),
        Some("Fjall is a Rust KV store"),
        "a longer summary body",
        None,
    );

    let title = thread_title(&distilled, "test-trace");
    let expected = title_for_thread(
        "x",
        Some("@tom_doerr"),
        Some("Fjall is a Rust KV store"),
        "a longer summary body",
    )
    .expect("builder title");
    assert_eq!(title, expected);
    assert_eq!(title, "@tom_doerr on X: \"Fjall is a Rust KV store\"");
    assert!(!is_purely_numeric(&title));
}

#[test]
fn thread_title_fabric_timeout_fallback_is_generic_platform_and_never_leaks() {
    // (b) A fabric-timeout thread fallback. The design doc claims this shape
    // carries `kind_specific: None`, but that is FACTUALLY WRONG:
    // `ThreadDistiller::distill` calls `attach_platform` UNCONDITIONALLY, so the
    // real fabric-timeout Distilled exits with `kind_specific = Some(Thread{..})`
    // (author None, platform "x"), `fallback_reason = Some("fabric-timeout")`,
    // and a `"[fabric-timeout]\n\n<snippet>"` summary (from `fallback_distilled`).
    // Reproduce exactly that shape and assert the title is "X thread", not the
    // leaked internal reason string.
    let mut distilled = distillers::fallback_distilled(
        "distill-thread-v1",
        "fabric-timeout",
        "some tweet body about rust",
        None,
        "gpt-4o",
    );
    distilled.kind_specific = Some(KindPayload::Thread(ThreadPayload {
        platform: "x".into(),
        ..Default::default()
    }));
    assert!(
        distilled.summary.starts_with("[fabric-timeout]"),
        "fallback summary shape changed"
    );

    let title = thread_title(&distilled, "test-trace");
    assert_eq!(title, "X thread");
    assert!(!is_purely_numeric(&title));
    assert!(
        !title.contains("[fabric-timeout]"),
        "internal reason string leaked into title"
    );
    assert!(
        !title.contains('['),
        "no bracketed internal token may appear in a title"
    );
}

#[test]
fn thread_title_dispatch_error_kind_specific_none_is_generic_thread() {
    // The ONE path that actually yields `kind_specific: None`: the outer
    // `dispatch-error` fallback in `distill_for_publish_thread` (the dispatcher
    // itself errored, so `attach_platform` never ran). No platform is known ->
    // "Thread thread", still non-leaking.
    let distilled = distillers::fallback_distilled("distill-thread-v1", "dispatch-error", "body", None, "gpt-4o");
    assert!(distilled.kind_specific.is_none());

    let title = thread_title(&distilled, "test-trace");
    assert_eq!(title, "Thread thread");
    assert!(!title.contains("[dispatch-error]"));
    assert!(!title.contains('['));
}

#[test]
fn resolve_title_non_thread_passes_article_title_through_byte_identical() {
    // (c) A non-thread note (github repo `owner/repo`, plain article scraped
    // title, or even a numeric one) is byte-identical to the title it arrived
    // with -- `thread_title` is never consulted outside the `is_thread` arm.
    // The payload is deliberately populated to prove it is IGNORED for non-threads.
    let distilled = thread_distilled(
        Some(ThreadPayload {
            author: Some("@should_be_ignored".into()),
            platform: "x".into(),
            ..Default::default()
        }),
        Some("ignored tldr"),
        "ignored summary",
        None,
    );
    for original in ["owner/repo", "Some Scraped Article Title", "2067473155988332909"] {
        let out = resolve_title(false, original.to_string(), &distilled, "test-trace");
        assert_eq!(out, original, "non-thread title must pass through unchanged");
    }
}

#[test]
fn resolve_title_thread_replaces_numeric_article_title() {
    // The end-to-end point of the fix: a thread whose article-path title WAS
    // the bare numeric post ID gets it replaced by the built thread title.
    let payload = ThreadPayload {
        author: Some("@tom_doerr".into()),
        platform: "x".into(),
        ..Default::default()
    };
    let distilled = thread_distilled(Some(payload), Some("Fjall is a Rust KV store"), "body", None);

    let out = resolve_title(true, "2067473155988332909".to_string(), &distilled, "test-trace");
    assert_ne!(out, "2067473155988332909");
    assert!(!is_purely_numeric(&out));
    assert_eq!(out, "@tom_doerr on X: \"Fjall is a Rust KV store\"");
}
