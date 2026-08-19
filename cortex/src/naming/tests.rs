use super::*;

#[test]
fn test_to_slug_basic() {
    assert_eq!(to_slug("Hello World.md"), "hello-world");
    assert_eq!(to_slug("My_Note.md"), "my-note");
    assert_eq!(to_slug("already-valid.md"), "already-valid");
}

#[test]
fn test_to_slug_special_chars() {
    assert_eq!(to_slug("Hello World!.md"), "hello-world");
    assert_eq!(to_slug("Test (1).md"), "test-1");
    assert_eq!(to_slug("A   B   C.md"), "a-b-c");
}

#[test]
fn test_to_slug_preserves_numbers() {
    assert_eq!(to_slug("note-123.md"), "note-123");
    assert_eq!(to_slug("2026-03-16-daily.md"), "2026-03-16-daily");
}

#[test]
fn test_is_valid_slug() {
    assert!(is_valid_slug("hello-world"));
    assert!(is_valid_slug("note-123"));
    assert!(is_valid_slug("a"));

    assert!(!is_valid_slug("Hello-World"));
    assert!(!is_valid_slug("hello_world"));
    assert!(!is_valid_slug("-leading"));
    assert!(!is_valid_slug("trailing-"));
    assert!(!is_valid_slug("double--hyphen"));
    assert!(!is_valid_slug(""));
}

#[test]
fn test_lint_naming_on_vault() {
    let v = crate::testutil::TestVault::new();
    let notes = v.scan();
    let config = v.config().actions.naming;

    let report = lint_naming(&notes, &config);
    // "My Awesome Note.md" should be flagged
    assert!(
        report
            .violations
            .iter()
            .any(|v| v.rule == "naming.lowercase-hyphenated" && v.path.to_string_lossy().contains("My Awesome Note"))
    );
    // Valid slugs should NOT be flagged
    assert!(
        !report
            .violations
            .iter()
            .any(|v| v.path.to_string_lossy() == "rust-guide.md")
    );
}

#[test]
fn test_lint_naming_max_length() {
    let v = crate::testutil::TestVault::new();
    v.add_note(
        &format!("{}.md", "a".repeat(100)),
        "---\ntitle: Long\n---\nLong name.\n",
    );
    let notes = v.scan();
    let config = v.config().actions.naming;

    let report = lint_naming(&notes, &config);
    assert!(report.violations.iter().any(|v| v.rule == "naming.max-length"));
}

#[test]
fn test_lint_naming_exempt() {
    let v = crate::testutil::TestVault::new();
    v.add_note("system/Bad Name.md", "---\ntitle: Bad\n---\nExempt.\n");
    let notes = v.scan();
    let config = NamingConfig {
        style: "lowercase-hyphenated".to_string(),
        max_length: 80,
        exempt_patterns: vec!["^system/".to_string()],
    };

    let report = lint_naming(&notes, &config);
    assert!(
        !report
            .violations
            .iter()
            .any(|v| v.path.to_string_lossy().contains("system/Bad Name"))
    );
}

#[test]
fn test_apply_naming_renames_files() {
    let v = crate::testutil::TestVault::new();
    let notes = v.scan();
    let config = v.config().actions.naming;

    let renames = apply_naming(v.root(), &notes, &config).expect("apply");
    assert!(!renames.is_empty());
    // "My Awesome Note.md" should be renamed to "my-awesome-note.md"
    assert!(v.exists("my-awesome-note.md"));
    assert!(!v.exists("My Awesome Note.md"));
}

// ---- ASCII folding: the "would clobber" self-rename loop ----

#[test]
fn to_slug_ascii_folds_accented_latin() {
    assert_eq!(
        to_slug("tobi-lütke-made-a-20-year-old-codebase-53-faster-overnight-heres-how.md"),
        "tobi-lutke-made-a-20-year-old-codebase-53-faster-overnight-heres-how"
    );
    assert_eq!(to_slug("michael-labbé.md"), "michael-labbe");
    assert_eq!(to_slug("tom-dörr.md"), "tom-dorr");
}

#[test]
fn to_slug_output_is_always_a_valid_slug() {
    // The loop this fixes: `is_valid_slug` rejected the name, `to_slug` handed
    // back the SAME name, so the violation could never be fixed. Every
    // suggestion must now pass the validator that asked for it.
    for name in [
        "tobi-lütke-made-a-20-year-old-codebase.md",
        "real-time-metaprogramming-michael-labbé-handmade-network.md",
        "Mixed Case With Ümlauts.md",
    ] {
        let slug = to_slug(name);
        assert!(is_valid_slug(&slug), "to_slug({name:?}) -> {slug:?} is not valid");
    }
}

#[test]
fn to_slug_drops_non_latin_it_cannot_fold() {
    // cortex's fixer is ASCII-only by design; vault::hygiene owns the
    // ingest-side fallback that keeps such a title from becoming empty.
    assert_eq!(to_slug("日本語-notes.md"), "notes");
}

// ---- post-rename wikilink rewrite: every link shape ----

/// Rewrite one file's links for a single rename and hand back the new bytes.
/// Uses the shared TestVault so the note list comes from a real scan.
fn relink(body: &str, from: &str, to: &str) -> String {
    let v = crate::testutil::TestVault::new();
    let holder = "link-holder.md";
    v.add_note(
        holder,
        &format!("---\ntitle: Holder\ndate: 2026-08-19\ntype: note\ndomain: tech\norigin: authored\ntags: []\n---\n\n{body}\n"),
    );
    let notes = v.scan();
    let renames = vec![(
        std::path::PathBuf::from(format!("notes/{from}.md")),
        std::path::PathBuf::from(format!("notes/{to}.md")),
    )];
    update_wikilinks_batch(v.root(), &notes, &renames).expect("relink");
    // Body only: the assertions are about link markup, not frontmatter.
    let written = v.read(holder);
    written
        .rsplit_once("---\n")
        .map(|(_, body)| body.trim().to_string())
        .unwrap_or(written)
}

#[test]
fn relink_rewrites_bare_and_piped_targets() {
    assert_eq!(
        relink("see [[old-name]] here", "old-name", "new-name"),
        "see [[new-name]] here"
    );
    assert_eq!(
        relink("see [[old-name|Old Name]] here", "old-name", "new-name"),
        "see [[new-name|Old Name]] here"
    );
}

#[test]
fn relink_preserves_a_path_form_prefix() {
    // The hub-body shape that dangled after the ASCII-fold rename.
    assert_eq!(
        relink("([[notes/old-name|Old Name]])", "old-name", "new-name"),
        "([[notes/new-name|Old Name]])"
    );
    assert_eq!(
        relink("[[notes/old-name]]", "old-name", "new-name"),
        "[[notes/new-name]]"
    );
}

#[test]
fn relink_preserves_heading_block_and_embed_syntax() {
    assert_eq!(
        relink("[[old-name#Summary]]", "old-name", "new-name"),
        "[[new-name#Summary]]"
    );
    assert_eq!(
        relink("[[old-name^abc123|quote]]", "old-name", "new-name"),
        "[[new-name^abc123|quote]]"
    );
    assert_eq!(relink("![[old-name]]", "old-name", "new-name"), "![[new-name]]");
}

#[test]
fn relink_leaves_unrelated_targets_alone() {
    assert_eq!(
        relink("[[old-name-extended]] and [[other]]", "old-name", "new-name"),
        "[[old-name-extended]] and [[other]]"
    );
}
