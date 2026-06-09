use super::*;

#[test]
fn ascii_under_limit_borrows_unchanged() {
    assert_eq!(truncate("hello", 10), "hello");
    assert_eq!(truncate_with_ellipsis("hello", 10), "hello");
}

#[test]
fn ascii_over_limit_cuts_and_ellipsizes() {
    assert_eq!(truncate("hello world", 5), "hello");
    assert_eq!(truncate_with_ellipsis("hello world", 5), "hello...");
}

#[test]
fn exactly_at_limit_no_cut() {
    assert_eq!(truncate("hello", 5), "hello");
    assert_eq!(truncate_with_ellipsis("hello", 5), "hello");
}

#[test]
fn one_char_over_limit_cuts() {
    assert_eq!(truncate("hello!", 5), "hello");
    assert_eq!(truncate_with_ellipsis("hello!", 5), "hello...");
}

#[test]
fn spanish_accents_counted_as_chars_not_bytes() {
    // "ñ" and "á" are 2 bytes each; a byte-index cut at 5 would panic mid-codepoint.
    let s = "niñez áspera";
    assert_eq!(truncate(s, 5), "niñez");
    assert_eq!(truncate_with_ellipsis(s, 5), "niñez...");
}

#[test]
fn emoji_counted_as_chars_not_bytes() {
    // Each emoji is 4 bytes; cutting by byte index would split a codepoint.
    let s = "👍🎉🚀🔥";
    assert_eq!(truncate(s, 2), "👍🎉");
    assert_eq!(truncate_with_ellipsis(s, 2), "👍🎉...");
}

#[test]
fn cut_lands_mid_codepoint_in_byte_equivalent_position() {
    // The exact panic case: a 50-byte guard would slice &s[..50] mid-codepoint.
    // 30 multi-byte chars => 60 bytes; byte-50 cut would land inside a char.
    let s = "ñ".repeat(30);
    let out = truncate_with_ellipsis(&s, 25);
    assert_eq!(out, format!("{}...", "ñ".repeat(25)));
    assert_eq!(truncate(&s, 25), "ñ".repeat(25));
}

#[test]
fn max_chars_zero() {
    assert_eq!(truncate_with_ellipsis("hello", 0), "...");
    assert_eq!(truncate_with_ellipsis("", 0), "");
    assert_eq!(truncate("hello", 0), "");
    assert_eq!(truncate("", 0), "");
}

#[test]
fn empty_input() {
    assert_eq!(truncate_with_ellipsis("", 10), "");
    assert_eq!(truncate("", 10), "");
}
