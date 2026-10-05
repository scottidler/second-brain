use super::*;

fn links(body: &str) -> Vec<WikiLink<'_>> {
    parse(body).collect()
}

fn one(body: &str) -> WikiLink<'_> {
    let found = links(body);
    assert_eq!(found.len(), 1, "expected exactly one link in {body:?}, got {found:?}");
    found.into_iter().next().expect("one link")
}

fn resolved<'r>(resolver: &'r Resolver, target: &str) -> Vec<&'r str> {
    let mut paths: Vec<&str> = resolver.resolve(target).collect();
    paths.sort_unstable();
    paths
}

// ---- the table: one test per row ----

#[test]
fn bare_link_yields_its_target() {
    let link = one("[[a]]");
    assert_eq!(link.target, "a");
    assert_eq!(
        (link.heading, link.block, link.alias, link.embed),
        (None, None, None, false)
    );
}

#[test]
fn pipe_splits_target_and_alias() {
    let link = one("[[a|b]]");
    assert_eq!((link.target, link.alias), ("a", Some("b")));
    assert_eq!((link.heading, link.block), (None, None));
}

#[test]
fn hash_yields_a_heading() {
    let link = one("[[a#h]]");
    assert_eq!((link.target, link.heading, link.block), ("a", Some("h"), None));
}

#[test]
fn hash_caret_yields_a_block_not_a_heading() {
    let link = one("[[a#^x]]");
    assert_eq!((link.target, link.heading, link.block), ("a", None, Some("x")));
}

#[test]
fn heading_and_alias_together() {
    let link = one("[[a#h|b]]");
    assert_eq!((link.target, link.heading, link.alias), ("a", Some("h"), Some("b")));
}

#[test]
fn bang_marks_an_embed() {
    let link = one("![[e]]");
    assert_eq!(link.target, "e");
    assert!(link.embed);
}

#[test]
fn table_escaped_pipe_drops_the_backslash_from_the_target() {
    let link = one(r"| col | [[a\|b]] |");
    assert_eq!((link.target, link.alias), ("a", Some("b")));
}

#[test]
fn path_target_keeps_the_path_and_stems_to_the_last_segment() {
    let link = one("[[dir/a]]");
    assert_eq!(link.target, "dir/a");
    assert_eq!(link.stem(), "a");
}

#[test]
fn md_suffix_is_kept_in_the_target_and_stripped_from_the_stem() {
    let link = one("[[a.md]]");
    assert_eq!(link.target, "a.md");
    assert_eq!(link.stem(), "a");
}

#[test]
fn heading_only_is_a_same_note_link_with_an_empty_target() {
    let link = one("[[#h]]");
    assert_eq!((link.target, link.heading), ("", Some("h")));
}

#[test]
fn alias_keeps_every_pipe_after_the_first() {
    let link = one("[[a|b|c]]");
    assert_eq!((link.target, link.alias), ("a", Some("b|c")));
}

#[test]
fn nested_brackets_yield_only_the_inner_link() {
    let link = one("[[a [[b]] c]]");
    assert_eq!(link.target, "b");
}

#[test]
fn empty_link_yields_nothing() {
    assert!(links("[[]]").is_empty());
}

#[test]
fn unclosed_link_yields_nothing() {
    assert!(links("[[a").is_empty());
    assert!(links("[[a\nb]]").is_empty(), "a link does not span lines");
}

#[test]
fn alias_without_a_target_yields_nothing() {
    assert!(links("[[|b]]").is_empty());
}

#[test]
fn link_inside_a_backtick_fence_yields_nothing() {
    let body = "before\n```\n[[a]]\n```\n[[after]]\n";
    let targets: Vec<&str> = parse(body).map(|l| l.target).collect();
    assert_eq!(targets, ["after"]);
}

#[test]
fn link_inside_a_tilde_fence_yields_nothing() {
    let body = "~~~md\n[[a]]\n~~~\n[[after]]\n";
    let targets: Vec<&str> = parse(body).map(|l| l.target).collect();
    assert_eq!(targets, ["after"]);
}

#[test]
fn link_inside_a_single_backtick_span_yields_nothing() {
    assert!(links("see `[[a]]` here").is_empty());
}

#[test]
fn link_inside_a_double_backtick_span_yields_nothing() {
    assert!(links("see ``x ` [[a]]`` here").is_empty());
}

#[test]
fn link_on_an_indented_line_yields_nothing() {
    assert!(links("    [[a]]").is_empty());
    assert!(links("\t[[a]]").is_empty());
}

#[test]
fn span_covers_the_brackets() {
    let body = "x [[a#h|b]] y";
    let link = one(body);
    assert_eq!(link.span, 2..11);
    assert_eq!(&body[link.span], "[[a#h|b]]");
}

#[test]
fn embed_span_includes_the_bang() {
    let body = "x ![[e]] y";
    let link = one(body);
    assert_eq!(link.span, 2..8);
    assert_eq!(&body[link.span], "![[e]]");
}

// ---- code-context edges ----

#[test]
fn spans_are_body_offsets_on_later_lines() {
    let body = "line one\r\nsé [[a]] and [[b]]\n";
    let found = links(body);
    assert_eq!(found.len(), 2);
    for link in &found {
        assert_eq!(&body[link.span.clone()][2..3], link.target);
    }
}

#[test]
fn fence_closes_only_on_same_char_at_least_as_long() {
    // A tilde line and a shorter backtick run do not close a 4-backtick fence.
    let body = "````\n~~~\n```\n[[in]]\n````\n[[out]]\n";
    let targets: Vec<&str> = parse(body).map(|l| l.target).collect();
    assert_eq!(targets, ["out"]);
}

#[test]
fn fence_line_with_an_info_string_does_not_close() {
    let body = "```\n```rust\n[[in]]\n```\n[[out]]\n";
    let targets: Vec<&str> = parse(body).map(|l| l.target).collect();
    assert_eq!(targets, ["out"]);
}

#[test]
fn unclosed_fence_runs_to_the_end() {
    assert!(links("```\n[[a]]\n").is_empty());
}

#[test]
fn triple_backtick_inline_span_is_not_a_fence() {
    let body = "```[[in]]``` then [[out]]\n[[next]]\n";
    let targets: Vec<&str> = parse(body).map(|l| l.target).collect();
    assert_eq!(targets, ["out", "next"]);
}

#[test]
fn unmatched_backtick_is_literal() {
    assert_eq!(one("a ` b [[c]]").target, "c");
}

#[test]
fn link_after_a_closed_span_is_a_link() {
    assert_eq!(one("`code` [[a]]").target, "a");
}

#[test]
fn three_space_indent_is_prose() {
    assert_eq!(one("   [[a]]").target, "a");
}

#[test]
fn several_links_come_out_in_body_order() {
    let targets: Vec<&str> = parse("[[a]] [[b|x]]\n- ![[c]]").map(|l| l.target).collect();
    assert_eq!(targets, ["a", "b", "c"]);
}

#[test]
fn stem_lowercases() {
    assert_eq!(stem("Dir/Note.MD"), "note");
    assert_eq!(stem("Note"), "note");
}

// ---- Resolver ----

fn vault() -> Resolver {
    Resolver::new([
        "dir/x.md",
        "a/dir/x.md",
        "otherdir/x.md",
        "notes/Solo.md",
        "one/readme.md",
        "two/README.md",
    ])
}

#[test]
fn path_target_matches_equal_and_component_suffix_paths_only() {
    assert_eq!(resolved(&vault(), "dir/x"), ["a/dir/x.md", "dir/x.md"]);
}

#[test]
fn path_target_with_md_suffix_resolves_the_same() {
    assert_eq!(resolved(&vault(), "dir/x.md"), ["a/dir/x.md", "dir/x.md"]);
}

#[test]
fn full_path_target_matches_one_note() {
    assert_eq!(resolved(&vault(), "a/dir/x"), ["a/dir/x.md"]);
}

#[test]
fn bare_target_matches_by_stem() {
    assert_eq!(resolved(&vault(), "solo"), ["notes/Solo.md"]);
}

#[test]
fn duplicated_stem_resolves_to_every_note_with_it() {
    assert_eq!(resolved(&vault(), "readme"), ["one/readme.md", "two/README.md"]);
    assert_eq!(resolved(&vault(), "x"), ["a/dir/x.md", "dir/x.md", "otherdir/x.md"]);
}

#[test]
fn resolution_is_case_insensitive() {
    assert_eq!(resolved(&vault(), "SOLO"), ["notes/Solo.md"]);
    assert_eq!(resolved(&vault(), "Dir/X"), ["a/dir/x.md", "dir/x.md"]);
}

#[test]
fn unknown_and_empty_targets_resolve_to_nothing() {
    assert!(resolved(&vault(), "missing").is_empty());
    assert!(
        resolved(&vault(), "ir/x").is_empty(),
        "suffix must start at a component boundary"
    );
    assert!(resolved(&vault(), "").is_empty());
}

#[test]
fn parsed_link_resolves_through_its_target() {
    let resolver = vault();
    let link = one("see [[Dir/X#Heading|alias]]");
    assert_eq!(resolved(&resolver, link.target), ["a/dir/x.md", "dir/x.md"]);
}
