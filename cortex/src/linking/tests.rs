use super::*;
use crate::testutil::TestVault;

#[test]
fn find_mention_handles_length_changing_lowercase() {
    // 'İ' (2 bytes) lowercases to "i̇" (3 bytes), so a `body.to_lowercase()`
    // match offset is NOT a valid index into `body`. The old code sliced
    // `body` with that offset and could panic / extract a shifted span.
    let body = "İstanbul notes mention Rust here.";
    let (context, surface) = LoweredBody::new(body).find_mention("Rust", "rust", 3).expect("match");
    // Surface must be the original-case word, not a byte-shifted slice.
    assert_eq!(surface, "Rust");
    assert!(context.contains("Rust"));
}

#[test]
fn find_mention_no_panic_on_multibyte_before_match() {
    // Many length-changing chars before the match must not panic.
    let body = format!("{} discusses Rust extensively", "İ".repeat(50));
    let result = LoweredBody::new(&body).find_mention("Rust", "rust", 3);
    assert_eq!(result.map(|(_, s)| s), Some("Rust".to_string()));
}

#[test]
fn test_concept_linking_on_vault() {
    let v = TestVault::new();
    let notes = v.scan();
    let config = v.config().actions.linking;

    let report = lint_linking(&notes, &config);
    // rust-guide.md body mentions "Python Guide" - should suggest linking
    assert!(
        report
            .violations
            .iter()
            .any(|vi| vi.path.to_string_lossy() == "rust-guide.md"
                && vi.rule == "linking.concept"
                && vi.message.contains("Python Guide"))
    );
}

#[test]
fn test_person_entity_on_vault() {
    let v = TestVault::new();
    let notes = v.scan();
    let config = v.config().actions.linking;

    let report = lint_linking(&notes, &config);
    // daily-standup.md mentions "John Smith"
    assert!(
        report
            .violations
            .iter()
            .any(|vi| vi.path.to_string_lossy() == "daily-standup.md" && vi.rule == "linking.person")
    );
}

#[test]
fn test_already_linked_not_suggested() {
    let v = TestVault::new();
    let notes = v.scan();
    let config = v.config().actions.linking;

    let report = lint_linking(&notes, &config);
    // python-guide.md already has [[rust-guide]] - should NOT suggest it again
    assert!(
        !report
            .violations
            .iter()
            .any(|vi| vi.path.to_string_lossy() == "python-guide.md"
                && vi.rule == "linking.concept"
                && vi.message.contains("rust-guide"))
    );
}

#[test]
fn test_insert_first_wikilink() {
    let content = "Working on obsidian-cortex and obsidian-cortex improvements.";
    let result = insert_first_wikilink(content, "obsidian-cortex", "obsidian-cortex");
    assert!(result.is_some());
    let result = result.expect("should have result");
    assert!(result.starts_with("Working on [[obsidian-cortex]]"));
    assert_eq!(result.matches("[[").count(), 1);
}

#[test]
fn test_insert_first_wikilink_skips_frontmatter() {
    let content = "---\ntitle: i replaced commands with one python script\ntype: article\n---\n\nThis article about python is great.";
    let result = insert_first_wikilink(content, "python", "python");
    assert!(result.is_some());
    let result = result.expect("should have result");
    // Must NOT modify frontmatter title
    assert!(result.contains("title: i replaced commands with one python script"));
    // Must wrap the body occurrence
    assert!(result.contains("about [[python]] is"));
}

#[test]
fn test_insert_first_wikilink_no_frontmatter() {
    let content = "Just a body with python mentioned.";
    let result = insert_first_wikilink(content, "python", "python");
    assert!(result.is_some());
    assert!(result.unwrap().contains("[[python]]"));
}

#[test]
fn test_extract_existing_links() {
    let body = "See [[note-a]] and [[note-b|display]].";
    let links = extract_existing_links(body);
    assert!(links.contains("note-a"));
    assert!(links.contains("note-b"));
}

// --- Phase 2: glossary concepts + piped alias links ---

#[test]
fn insert_first_wikilink_pipes_alias_to_slug() {
    let content = "We rely on Retrieval-Augmented Generation here.";
    let result = insert_first_wikilink(content, "rag", "Retrieval-Augmented Generation").expect("link");
    assert!(
        result.contains("[[rag|Retrieval-Augmented Generation]]"),
        "piped link preserves prose surface; got {result}"
    );
}

#[test]
fn insert_first_wikilink_pipes_when_only_case_differs() {
    let content = "We use LangChain daily.";
    // surface "LangChain" differs from slug "langchain" only in case -> piped.
    let result = insert_first_wikilink(content, "langchain", "LangChain").expect("link");
    assert!(result.contains("[[langchain|LangChain]]"), "got {result}");
}

#[test]
fn insert_first_wikilink_plain_when_surface_equals_target() {
    let content = "About python here.";
    let result = insert_first_wikilink(content, "python", "python").expect("link");
    assert!(result.contains("[[python]]"));
    assert!(!result.contains('|'), "no pipe when surface == target");
}

fn glossary_config(concepts: &[&str], aliases: &[(&str, &str)]) -> LinkingConfig {
    let mut cfg = LinkingConfig {
        scan_for: vec!["concepts".to_string()],
        min_word_length: 3,
        ..Default::default()
    };
    cfg.entities.concepts = concepts.iter().map(|s| s.to_string()).collect();
    cfg.aliases = aliases.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    cfg
}

fn note_with_body(path: &str, body: &str) -> Note {
    Note {
        path: std::path::PathBuf::from(path),
        frontmatter: crate::vault::Frontmatter::default(),
        body: body.to_string(),
        raw: body.to_string(),
    }
}

#[test]
fn glossary_concept_is_flagged_for_linking() {
    let cfg = glossary_config(&["langchain"], &[]);
    let notes = vec![note_with_body("notes/x.md", "We use LangChain in production.")];
    let report = lint_linking(&notes, &cfg);
    assert!(
        report
            .violations
            .iter()
            .any(|v| v.rule == "linking.glossary" && v.message.contains("langchain")),
        "glossary concept mention flagged"
    );
}

#[test]
fn alias_is_flagged_as_piped_link() {
    let cfg = glossary_config(&[], &[("Retrieval-Augmented Generation", "rag")]);
    let notes = vec![note_with_body(
        "notes/x.md",
        "Retrieval-Augmented Generation is everywhere.",
    )];
    let report = lint_linking(&notes, &cfg);
    let v = report
        .violations
        .iter()
        .find(|v| v.rule == "linking.alias")
        .expect("alias violation");
    match &v.fix {
        Some(Fix::AddWikilink { target, surface, .. }) => {
            assert_eq!(target, "rag");
            assert_eq!(surface, "Retrieval-Augmented Generation");
        }
        other => panic!("expected AddWikilink, got {other:?}"),
    }
}

#[test]
fn glossary_does_not_double_link_existing() {
    let cfg = glossary_config(&["langchain"], &[]);
    // Body already links it -> no new violation.
    let notes = vec![note_with_body("notes/x.md", "We use [[langchain]] here.")];
    let report = lint_linking(&notes, &cfg);
    assert!(
        !report.violations.iter().any(|v| v.rule == "linking.glossary"),
        "already-linked concept is not re-flagged"
    );
}

#[test]
fn glossary_does_not_self_link_hub_note() {
    let cfg = glossary_config(&["langchain"], &[]);
    // The note IS the langchain hub note (stem == slug) -> never self-link.
    let notes = vec![note_with_body("notes/langchain.md", "LangChain is a framework.")];
    let report = lint_linking(&notes, &cfg);
    assert!(
        !report.violations.iter().any(|v| v.rule == "linking.glossary"),
        "a concept's own hub note is never self-linked"
    );
}

#[test]
fn load_glossary_missing_file_is_empty() {
    let g = load_glossary(std::path::Path::new("/nonexistent/glossary.yml")).expect("ok");
    assert!(g.concepts.is_empty());
    assert!(g.aliases.is_empty());
}

// --- Suite A: structure-aware guard (inside_structure) ---
//
// The mutation point is `insert_first_wikilink`; a `None` result means the only
// occurrence of the surface was structural and was correctly skipped. These
// pin the "never corrupt a URL/HTML/code/math/link span again" contract.

/// Assert the surface is NOT wrapped (every occurrence is structural).
fn assert_blocked(content: &str, target: &str, surface: &str) {
    assert_eq!(
        insert_first_wikilink(content, target, surface),
        None,
        "should NOT link {surface:?} inside structure: {content:?}"
    );
}

#[test]
fn guard_blocks_iframe_src() {
    assert_blocked(
        r#"<iframe width="854" height="480" src="https://www.youtube.com/embed/abcdefghijk"></iframe>"#,
        "youtube-com",
        "youtube.com",
    );
}

#[test]
fn guard_blocks_markdown_image_embed() {
    assert_blocked(
        "![](https://www.youtube.com/watch?v=abcdefghijk)",
        "youtube-com",
        "youtube.com",
    );
}

#[test]
fn guard_blocks_markdown_link_destination() {
    assert_blocked("[docs](https://github.com/torvalds/linux)", "github-com", "github.com");
}

#[test]
fn guard_blocks_autolink() {
    assert_blocked("see <https://youtube.com/x> here", "youtube-com", "youtube.com");
}

#[test]
fn guard_blocks_bare_scheme_url() {
    assert_blocked(
        "prose then https://youtube.com/watch?v=x end",
        "youtube-com",
        "youtube.com",
    );
}

#[test]
fn guard_blocks_bare_path_no_scheme() {
    // No scheme, no www - caught by the trailing "/" of a bare path.
    assert_blocked("youtube.com/watch?v=x", "youtube-com", "youtube.com");
    assert_blocked("see github.com/torvalds for code", "github-com", "github.com");
}

#[test]
fn guard_blocks_mailto_scheme() {
    assert_blocked("contact mailto:hi@youtube.com today", "youtube-com", "youtube.com");
}

#[test]
fn guard_blocks_reference_style_definition() {
    assert_blocked("[ref]: https://youtube.com/x", "youtube-com", "youtube.com");
}

#[test]
fn guard_blocks_inline_code() {
    assert_blocked("the host `youtube.com` is special", "youtube-com", "youtube.com");
}

#[test]
fn guard_blocks_fenced_code() {
    assert_blocked("```\nyoutube.com\n```", "youtube-com", "youtube.com");
}

#[test]
fn guard_blocks_indented_code() {
    assert_blocked("    github.com is here", "github-com", "github.com");
}

#[test]
fn guard_blocks_inline_math() {
    assert_blocked("the value $rust = 1$ holds", "rust", "rust");
}

#[test]
fn guard_blocks_html_attribute() {
    assert_blocked(r#"<a href="https://github.com">x</a>"#, "github-com", "github.com");
}

#[test]
fn guard_blocks_html_comment() {
    assert_blocked("<!-- youtube.com note -->", "youtube-com", "youtube.com");
}

#[test]
fn guard_allows_plain_prose_mention() {
    let out = insert_first_wikilink("I prefer rust for systems work", "rust", "rust").expect("link");
    assert_eq!(out, "I prefer [[rust]] for systems work");
}

#[test]
fn guard_iterate_to_clean_skips_url_links_prose() {
    // First occurrence is in a URL (skip), second is prose (link).
    let out =
        insert_first_wikilink("https://example.com/rust then later I use rust daily", "rust", "rust").expect("link");
    assert!(
        out.contains("https://example.com/rust then"),
        "URL occurrence untouched: {out}"
    );
    assert!(out.contains("I use [[rust]] daily"), "prose occurrence linked: {out}");
}

#[test]
fn guard_does_not_misfire_on_non_link_brackets_and_parens() {
    // `] (` is NOT a markdown link `](`; the prose `rust` must still link and
    // the `[1, 2]` array must be untouched.
    let out = insert_first_wikilink("array [1, 2] (rust is great)", "rust", "rust").expect("link");
    assert!(out.contains("[1, 2]"), "array untouched: {out}");
    assert!(out.contains("[[rust]]"), "prose rust linked: {out}");
}

#[test]
fn guard_no_panic_on_multibyte_before_url() {
    // Length-changing chars before a structural URL must not panic, and the
    // URL domain must not be linked.
    assert_blocked("café — https://youtube.com/x", "youtube-com", "youtube.com");
}

#[test]
fn guard_idempotent_on_already_linked_prose() {
    // Re-running over a note whose only mention is already a wikilink is a no-op.
    assert_blocked("I use [[rust]] daily", "rust", "rust");
}

#[test]
fn guard_blocks_match_inside_existing_wikilink_display() {
    // "offense" inside the DISPLAY text of an existing wikilink (no prior `]]`)
    // must NOT be linked - that builds a broken nested wikilink. This is the
    // exact shape the live daemon was still creating.
    assert_blocked(
        "Join [[the-spread-offense|the Spread Offense]] today",
        "offense",
        "offense",
    );
}

#[test]
fn guard_blocks_match_inside_existing_wikilink_target() {
    // "offense" inside the TARGET slug of an existing wikilink must NOT be linked.
    assert_blocked("Join [[the-spread-offense|the Group]] today", "offense", "offense");
}

#[test]
fn guard_blocks_nested_inside_piped_display() {
    // The real regression form: linking "claude" inside an existing piped link.
    assert_blocked("Using [[claude-code|Claude Code]] daily", "claude", "claude");
}

// --- Phase 4: detection <-> mutation matcher convergence ---
//
// Before this phase, `find_mention` (detection, ASCII-only boundary) and
// `insert_first_wikilink` (mutation, regex `\b` + an independently-sliced
// body) could disagree on whether a mention was clean/appliable. A
// suggestion the daemon reported but could never apply left
// `new_content == content` -> no write -> the same suggestion re-reported
// forever (the perpetual `link: N files` phantom). These tests pin the
// convergence: both sides now call the SAME `is_clean_mention` predicate on
// the SAME (shared-splitter-derived) body text.

#[test]
fn find_mention_and_insert_agree_on_underscore_boundary() {
    // "bar" here is not a standalone word - it's embedded in "foo_bar".
    // The old ASCII-only boundary check in `find_mention` treated `_` as a
    // non-alphanumeric boundary and would have reported this as a clean
    // mention; the old regex `\bbar\b` in `insert_first_wikilink` treats `_`
    // as a word char (Unicode `\w`) and would never match here at all. Both
    // now agree: NOT a clean mention.
    let body = "note: foo_bar here";
    assert_eq!(LoweredBody::new(body).find_mention("bar", "bar", 3), None);
    assert_eq!(insert_first_wikilink(body, "bar", "bar"), None);
}

#[test]
fn find_mention_and_insert_agree_on_clean_occurrence() {
    // Sanity companion: a genuinely clean, word-bounded mention still agrees
    // (both find it) after routing through the shared predicate.
    let body = "note: a clean bar mention here";
    let found = LoweredBody::new(body).find_mention("bar", "bar", 3);
    assert_eq!(found.map(|(_, s)| s), Some("bar".to_string()));
    assert!(insert_first_wikilink(body, "bar", "bar").is_some());
}

#[test]
fn insert_first_wikilink_uses_shared_splitter_for_a_false_delimiter_line() {
    // A frontmatter value can itself contain a literal "---" line that is
    // NOT the real closing delimiter. The old ad hoc `find("\n---")` in this
    // function stopped at that FIRST match, mis-splitting mid-frontmatter and
    // leaking leftover frontmatter text ("type: article") into what it
    // thought was the body. The shared splitter
    // (`vault::frontmatter::split_raw`, which also produces `Note::body`)
    // requires the closing delimiter to be a full, otherwise-blank line, so
    // it correctly finds the SECOND "---" as the real close.
    let content = "---\ntitle: t\n---not-a-real-delimiter here\ntype: article\n---\n\nBody text is short.\n";
    // "article" appears only inside frontmatter under the correct split - it
    // must NOT be linked.
    assert_eq!(insert_first_wikilink(content, "article", "article"), None);
}

#[test]
fn every_lint_linking_suggestion_is_appliable() {
    // Success criterion (a): every `linking.*` violation, when applied,
    // changes bytes - `insert_first_wikilink` must return `Some` for every
    // (target, surface) pair `lint_linking` emits a `Fix::AddWikilink` for.
    let cfg = glossary_config(&["langchain", "rag"], &[("Retrieval-Augmented Generation", "rag")]);
    let notes = vec![
        note_with_body(
            "notes/a.md",
            "We use LangChain daily and rely on Retrieval-Augmented Generation for search.",
        ),
        note_with_body("notes/b.md", "A note with the word foo_langchain_bar embedded oddly."),
    ];
    let report = lint_linking(&notes, &cfg);
    assert!(
        !report.violations.is_empty(),
        "fixture should produce at least one suggestion"
    );
    assert!(
        !report
            .violations
            .iter()
            .any(|v| v.path.to_string_lossy() == "notes/b.md"),
        "an underscore-bounded mention must not be suggested (detection/mutation must agree it's unclean)"
    );

    for violation in &report.violations {
        if let Some(Fix::AddWikilink { target, surface, .. }) = &violation.fix {
            let note = notes.iter().find(|n| n.path == violation.path).expect("note exists");
            assert!(
                insert_first_wikilink(&note.raw, target, surface).is_some(),
                "suggestion for {surface:?} in {} must be appliable",
                violation.path.display()
            );
        }
    }
}

#[test]
fn two_consecutive_link_passes_converge_to_zero_writes() {
    // Success criterion (b): two consecutive link passes over an unchanged
    // vault produce zero writes the second time - the exact structural
    // invariant the daemon's oscillation fingerprint depends on.
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    std::fs::write(
        root.join("langchain.md"),
        "---\ntitle: LangChain\ntype: note\n---\nThe LangChain hub note.\n",
    )
    .expect("write note");
    std::fs::write(
        root.join("a.md"),
        "---\ntitle: A\ntype: note\n---\nWe use LangChain daily in production.\n",
    )
    .expect("write note");

    let cfg = glossary_config(&["langchain"], &[]);
    let vault_config = crate::config::VaultConfig {
        root_path: None,
        ignore: vec![".git".to_string(), ".obsidian".to_string()],
        exclude: Vec::new(),
        include: Vec::new(),
    };

    let notes = crate::vault::scan_vault(root, &vault_config).expect("scan vault");
    let written_first = apply_linking(root, &notes, &cfg).expect("apply linking");
    assert_eq!(
        written_first,
        vec!["a.md".to_string()],
        "first pass links the mention exactly once"
    );

    // Re-scan to observe the newly-written bytes (mirrors the daemon's
    // per-cycle rescan), then run the SAME pass again with no edits in
    // between.
    let notes2 = crate::vault::scan_vault(root, &vault_config).expect("scan vault");
    let written_second = apply_linking(root, &notes2, &cfg).expect("apply linking");
    assert!(
        written_second.is_empty(),
        "steady state: second pass writes nothing, got {written_second:?}"
    );
}

#[test]
fn guard_links_clean_prose_occurrence_after_one_inside_a_wikilink() {
    // Iterate-to-clean: the surface appears inside an existing wikilink AND later
    // in clean prose -> link only the clean prose occurrence.
    let out = insert_first_wikilink(
        "see [[the-spread-offense|the Spread Offense]] then plain offense here",
        "offense",
        "offense",
    )
    .expect("link");
    assert!(
        out.contains("[[the-spread-offense|the Spread Offense]]"),
        "existing link intact: {out}"
    );
    assert!(
        out.contains("plain [[offense]] here"),
        "clean prose occurrence linked: {out}"
    );
}

#[test]
fn apply_linking_is_add_only_never_removes_or_alters_content() {
    // Phase 13 acceptance (apply_linking add-only): the linker only ADDS
    // wikilinks; the original prose AND any pre-existing wikilink survive
    // around the insertion.
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    std::fs::write(
        root.join("langchain.md"),
        "---\ntitle: LangChain\ntype: note\n---\nhub\n",
    )
    .expect("w");
    let original_body = "We use LangChain daily. See [[existing-link|prior]] too.\n";
    std::fs::write(
        root.join("a.md"),
        format!("---\ntitle: A\ntype: note\n---\n{original_body}"),
    )
    .expect("w");

    let cfg = glossary_config(&["langchain"], &[]);
    let vault_config = crate::config::VaultConfig {
        root_path: None,
        ignore: vec![".git".to_string(), ".obsidian".to_string()],
        exclude: Vec::new(),
        include: Vec::new(),
    };
    let notes = crate::vault::scan_vault(root, &vault_config).expect("scan");
    apply_linking(root, &notes, &cfg).expect("apply");

    let after = std::fs::read_to_string(root.join("a.md")).expect("read");
    assert!(
        after.contains("[[langchain"),
        "the concept mention gained a wikilink: {after}"
    );
    // Nothing removed/altered: the pre-existing wikilink and surrounding prose survive.
    assert!(
        after.contains("[[existing-link|prior]]"),
        "pre-existing wikilink untouched: {after}"
    );
    assert!(after.contains("We use "), "leading prose preserved");
    assert!(after.contains(" daily."), "prose preserved");
    assert!(after.contains(" too."), "trailing prose preserved");
}

#[test]
fn apply_linking_across_growing_sweeps_never_removes_prior_links() {
    // Phase 6 (harvest-completion): extends the Phase 13 add-only guarantee
    // (single-call) and the two-pass convergence test (identical vault) to a
    // THIRD sweep where the vault GROWS - the exact shape every real nightly
    // daemon tick takes. A prior sweep's inserted wikilink must survive
    // byte-for-byte as a later sweep adds an unrelated link elsewhere: linking
    // never removes an existing edge/link across sweeps.
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    std::fs::write(
        root.join("langchain.md"),
        "---\ntitle: LangChain\ntype: note\n---\nhub\n",
    )
    .expect("w");
    std::fs::write(root.join("graphrag.md"), "---\ntitle: GraphRAG\ntype: note\n---\nhub\n").expect("w");
    std::fs::write(
        root.join("a.md"),
        "---\ntitle: A\ntype: note\n---\nWe use LangChain daily.\n",
    )
    .expect("w");

    let cfg = glossary_config(&["langchain", "graphrag"], &[]);
    let vault_config = crate::config::VaultConfig {
        root_path: None,
        ignore: vec![".git".to_string(), ".obsidian".to_string()],
        exclude: Vec::new(),
        include: Vec::new(),
    };

    // Sweep 1: a.md mentions langchain -> gets linked.
    let notes1 = crate::vault::scan_vault(root, &vault_config).expect("scan1");
    let written1 = apply_linking(root, &notes1, &cfg).expect("apply1");
    assert_eq!(written1, vec!["a.md".to_string()], "sweep 1 links a.md");
    let a_after_sweep1 = std::fs::read_to_string(root.join("a.md")).expect("read a after sweep1");
    assert!(a_after_sweep1.contains("[[langchain"), "sweep 1 landed the link");

    // Sweep 2: the vault GROWS - a brand new note mentions a DIFFERENT
    // glossary concept. a.md is untouched input to this sweep.
    std::fs::write(
        root.join("b.md"),
        "---\ntitle: B\ntype: note\n---\nA note about GraphRAG retrieval.\n",
    )
    .expect("w");
    let notes2 = crate::vault::scan_vault(root, &vault_config).expect("scan2");
    let written2 = apply_linking(root, &notes2, &cfg).expect("apply2");
    assert_eq!(
        written2,
        vec!["b.md".to_string()],
        "growth only links the NEW note; a.md is not re-written"
    );

    // a.md's sweep-1 link survives byte-for-byte across the growth sweep.
    let a_after_sweep2 = std::fs::read_to_string(root.join("a.md")).expect("read a after sweep2");
    assert_eq!(
        a_after_sweep2, a_after_sweep1,
        "a.md is byte-identical across the growth sweep - linking never removes a prior link"
    );
    let b_after = std::fs::read_to_string(root.join("b.md")).expect("read b");
    assert!(b_after.contains("[[graphrag"), "the new note gained its own link");
}

#[test]
fn concept_recall_every_glossary_concept_mentioned_gets_linked() {
    // Phase 13 acceptance (concept recall): over a small labeled corpus, the
    // fraction of known-concept mentions that actually land a wikilink bounds
    // the glossary/alias coverage gap. A concept IN the glossary must reach
    // recall 1.0; an out-of-glossary term is the coverage gap (not linked).
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    std::fs::write(
        root.join("langchain.md"),
        "---\ntitle: LangChain\ntype: note\n---\nhub\n",
    )
    .expect("w");
    std::fs::write(root.join("graphrag.md"), "---\ntitle: GraphRAG\ntype: note\n---\nhub\n").expect("w");
    // Three notes each mentioning an in-glossary concept, one mentioning an
    // out-of-glossary term.
    let corpus = [
        ("n1.md", "A note about LangChain internals.\n"),
        ("n2.md", "Another on GraphRAG retrieval.\n"),
        ("n3.md", "LangChain plus GraphRAG together.\n"),
        ("n4.md", "This one is about SomeUnknownThing only.\n"),
    ];
    for (name, body) in corpus {
        std::fs::write(root.join(name), format!("---\ntitle: {name}\ntype: note\n---\n{body}")).expect("w");
    }
    let cfg = glossary_config(&["langchain", "graphrag"], &[]);
    let vault_config = crate::config::VaultConfig {
        root_path: None,
        ignore: vec![".git".to_string(), ".obsidian".to_string()],
        exclude: Vec::new(),
        include: Vec::new(),
    };
    let notes = crate::vault::scan_vault(root, &vault_config).expect("scan");
    apply_linking(root, &notes, &cfg).expect("apply");

    // Recall over the three notes with an in-glossary mention: all linked.
    for name in ["n1.md", "n2.md", "n3.md"] {
        let body = std::fs::read_to_string(root.join(name)).expect("read");
        assert!(
            body.contains("[["),
            "in-glossary concept in {name} must be linked (recall): {body}"
        );
    }
    // The out-of-glossary term is the coverage gap - grow aliases, never loosen
    // determinism. n4 gets no link.
    let n4 = std::fs::read_to_string(root.join("n4.md")).expect("read");
    assert!(
        !n4.contains("[["),
        "out-of-glossary term is the coverage gap, not linked: {n4}"
    );
}
