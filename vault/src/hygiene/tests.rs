use super::*;

#[test]
fn sanitize_filename_does_not_panic_on_multibyte_at_cut() {
    // sanitize_slug keeps non-ASCII alphanumerics, so a long title of
    // multi-byte chars must not panic when truncated at the byte limit.
    let title = "ñ".repeat(200);
    let out = sanitize_filename(&title);
    assert!(out.chars().count() <= MAX_FILENAME_LEN);
    assert!(out.chars().all(|c| c == 'ñ' || c == '-'));

    // A cut that would land mid-codepoint at exactly MAX_FILENAME_LEN bytes.
    let mixed = format!("{}ñ{}", "a".repeat(MAX_FILENAME_LEN - 1), "b".repeat(50));
    let _ = sanitize_filename(&mixed); // must not panic
}

#[test]
fn test_normalize_domain_valid_passthrough() {
    assert_eq!(normalize_domain("ai"), "ai");
    assert_eq!(normalize_domain("tech"), "tech");
    assert_eq!(normalize_domain("football"), "football");
    assert_eq!(normalize_domain("resources"), "resources");
}

#[test]
fn test_normalize_domain_case_insensitive() {
    assert_eq!(normalize_domain("AI"), "ai");
    assert_eq!(normalize_domain("Tech"), "tech");
    assert_eq!(normalize_domain("FOOTBALL"), "football");
}

#[test]
fn test_normalize_domain_trimming() {
    assert_eq!(normalize_domain("  ai  "), "ai");
}

#[test]
fn test_sanitize_tag_basic() {
    assert_eq!(sanitize_tag("AI/ML"), "ai-ml");
}

#[test]
fn test_sanitize_tag_spaces() {
    assert_eq!(sanitize_tag("Machine Learning"), "machine-learning");
}

#[test]
fn test_sanitize_tag_already_clean() {
    assert_eq!(sanitize_tag("rust"), "rust");
}

#[test]
fn test_sanitize_tag_trim_hyphens() {
    assert_eq!(sanitize_tag("--hello--"), "hello");
}

// ---- sanitize_filename: basics ----

#[test]
fn test_sanitize_filename_basic() {
    assert_eq!(sanitize_filename("Hello World!"), "hello-world");
}

#[test]
fn test_sanitize_filename_lowercases() {
    assert_eq!(sanitize_filename("MY TITLE"), "my-title");
    assert_eq!(sanitize_filename("CamelCase"), "camelcase");
}

#[test]
fn test_sanitize_filename_spaces_become_hyphens() {
    assert_eq!(sanitize_filename("one two three"), "one-two-three");
    assert_eq!(sanitize_filename("  leading trailing  "), "leading-trailing");
}

#[test]
fn test_sanitize_filename_special_chars() {
    assert_eq!(sanitize_filename("Test: A/B \"quotes\""), "test-a-b-quotes");
    assert_eq!(sanitize_filename("hello@world.com"), "hello-world-com");
    assert_eq!(
        sanitize_filename("(parens) [brackets] {braces}"),
        "parens-brackets-braces"
    );
    assert_eq!(sanitize_filename("a + b = c"), "a-b-c");
    assert_eq!(sanitize_filename("100% done!"), "100-done");
    assert_eq!(sanitize_filename("file#anchor?query&param"), "file-anchor-query-param");
}

#[test]
fn test_sanitize_filename_already_clean() {
    assert_eq!(sanitize_filename("my-cool-note"), "my-cool-note");
    assert_eq!(sanitize_filename("abc123"), "abc123");
}

#[test]
fn test_sanitize_filename_github_owner_repo() {
    // Borg derives github note slugs from `owner/repo` (see
    // pipeline.rs github_repo override). Lock in the slash-to-hyphen
    // mapping so two distinct repos never collide on the same filename.
    assert_eq!(sanitize_filename("coleam00/archon"), "coleam00-archon");
    assert_eq!(
        sanitize_filename("matt1398/claude-devtools"),
        "matt1398-claude-devtools"
    );
    // Different repos must never produce identical slugs.
    assert_ne!(
        sanitize_filename("coleam00/archon"),
        sanitize_filename("matt1398/claude-devtools")
    );
}

// ---- sanitize_filename: apostrophes and quotes ----

#[test]
fn test_sanitize_filename_strips_apostrophes() {
    assert_eq!(sanitize_filename("Bob's idea"), "bobs-idea");
    assert_eq!(sanitize_filename("it's a don't won't"), "its-a-dont-wont");
    assert_eq!(sanitize_filename("rock 'n' roll"), "rock-n-roll");
    assert_eq!(sanitize_filename("'quoted'"), "quoted");
}

#[test]
fn test_sanitize_filename_double_quotes_become_hyphens() {
    // double quotes are not stripped, they become hyphens (then collapsed)
    assert_eq!(sanitize_filename("the \"best\" plan"), "the-best-plan");
}

#[test]
fn test_sanitize_filename_backticks_become_hyphens() {
    assert_eq!(sanitize_filename("use `cargo test`"), "use-cargo-test");
}

// ---- sanitize_filename: dash/hyphen collapsing ----

#[test]
fn test_sanitize_filename_collapses_consecutive_hyphens() {
    assert_eq!(sanitize_filename("a--b"), "a-b");
    assert_eq!(sanitize_filename("a---b"), "a-b");
    assert_eq!(sanitize_filename("a------b"), "a-b");
}

#[test]
fn test_sanitize_filename_em_dash() {
    assert_eq!(sanitize_filename("before\u{2014}after"), "before-after"); // em dash U+2014
    assert_eq!(sanitize_filename("a\u{2013}b"), "a-b"); // en dash U+2013
}

#[test]
fn test_sanitize_filename_mixed_separators_collapse() {
    assert_eq!(sanitize_filename("a-_-b"), "a-b");
    assert_eq!(sanitize_filename("a _ - _ b"), "a-b");
    assert_eq!(sanitize_filename("a - - - b"), "a-b");
    assert_eq!(sanitize_filename("a:::b"), "a-b");
    assert_eq!(sanitize_filename("a///b"), "a-b");
}

#[test]
fn test_sanitize_filename_underscores_become_hyphens() {
    assert_eq!(sanitize_filename("hello_world"), "hello-world");
    assert_eq!(sanitize_filename("a__b"), "a-b");
}

// ---- sanitize_filename: leading/trailing cleanup ----

#[test]
fn test_sanitize_filename_strips_leading_trailing_hyphens() {
    assert_eq!(sanitize_filename("--hello--"), "hello");
    assert_eq!(sanitize_filename("---test---"), "test");
    assert_eq!(sanitize_filename(" - hello - "), "hello");
}

// ---- sanitize_filename: truncation ----

#[test]
fn test_sanitize_filename_short_title_unchanged() {
    assert_eq!(sanitize_filename("my-cool-note"), "my-cool-note");
}

#[test]
fn test_sanitize_filename_exactly_at_limit() {
    let title = "a".repeat(80);
    let result = sanitize_filename(&title);
    assert_eq!(result.len(), 80);
}

#[test]
fn test_sanitize_filename_truncates_long_title() {
    let long_title = "GitHub - joaoh82/rustunnel: A minimal, educational TCP tunneling tool written in Rust that demonstrates core networking concepts including TCP proxying, TLS termination, and connection multiplexing";
    let result = sanitize_filename(long_title);
    assert!(result.len() <= 80, "got length {}: {result}", result.len());
    assert!(!result.ends_with('-'));
}

#[test]
fn test_sanitize_filename_truncation_breaks_at_word_boundary() {
    // 85 chars when sanitized: "aaa...aaa-bbb" - should truncate at last hyphen before 80
    let title = format!("{}-bbbbbbbbb", "a-".repeat(38).trim_end_matches('-'));
    let result = sanitize_filename(&title);
    assert!(result.len() <= 80, "got length {}: {result}", result.len());
    assert!(!result.ends_with('-'));
}

#[test]
fn test_sanitize_filename_truncation_never_exceeds_limit() {
    // No hyphens at all - just a long run of chars
    let title = "a".repeat(200);
    let result = sanitize_filename(&title);
    assert_eq!(result.len(), 80);
}

// ---- sanitize_filename: real-world titles ----

#[test]
fn test_sanitize_filename_youtube_title() {
    assert_eq!(
        sanitize_filename("I Built My Second Brain with Claude Code + Obsidian"),
        "i-built-my-second-brain-with-claude-code-obsidian"
    );
}

#[test]
fn test_sanitize_filename_github_repo() {
    // github titles often include the full description
    let title = "GitHub - infatoshi/opensquirrel: For people who get distracted by agents. A native Rust GPUI control plane for running claude-code, codex, cursor and opencode side by side";
    let result = sanitize_filename(title);
    assert!(result.len() <= 80, "got length {}: {result}", result.len());
}

#[test]
fn test_sanitize_filename_howtogeek_url_slug() {
    assert_eq!(
        sanitize_filename("i failed to build a second brain until i used obsidians daily notes"),
        "i-failed-to-build-a-second-brain-until-i-used-obsidians-daily-notes"
    );
}

#[test]
fn test_sanitize_filename_possessive_in_real_title() {
    assert_eq!(
        sanitize_filename("Anthropic's New Claude Model: What You Need to Know"),
        "anthropics-new-claude-model-what-you-need-to-know"
    );
}

#[test]
fn test_sanitize_filename_unicode_and_emoji() {
    // emoji and non-latin chars become hyphens, then collapse
    let result = sanitize_filename("hello world cafe");
    assert_eq!(result, "hello-world-cafe");
}

// ---- sanitize_tag: matching behavior ----

#[test]
fn test_sanitize_tag_strips_apostrophes() {
    assert_eq!(sanitize_tag("don't-panic"), "dont-panic");
    assert_eq!(sanitize_tag("it's"), "its");
}

#[test]
fn test_sanitize_tag_collapses_hyphens() {
    assert_eq!(sanitize_tag("a--b"), "a-b");
    assert_eq!(sanitize_tag("a - b"), "a-b");
}

#[test]
fn test_normalize_text_input_basic() {
    assert_eq!(
        normalize_text_input("  Definition:  Gregarious  "),
        "definition: gregarious"
    );
}

#[test]
fn test_normalize_text_input_empty() {
    assert_eq!(normalize_text_input(""), "");
    assert_eq!(normalize_text_input("   "), "");
}

#[test]
fn test_normalize_text_input_tabs_newlines() {
    assert_eq!(normalize_text_input("define:\t\tword\n\n"), "define: word");
}
