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

    let renames = apply_naming(v.root(), &notes, &config, &crate::shutdown::StopHandle::never())
        .expect("apply")
        .written;
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
        &format!("---\ntitle: Holder\ndate: 2026-08-19\ntype: note\norigin: authored\ntags: []\n---\n\n{body}\n"),
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
        relink("[[old-name#^abc123|quote]]", "old-name", "new-name"),
        "[[new-name#^abc123|quote]]"
    );
    assert_eq!(relink("![[old-name]]", "old-name", "new-name"), "![[new-name]]");
    assert_eq!(
        relink("[[notes/Old-Name.md|x]]", "old-name", "new-name"),
        "[[notes/new-name.md|x]]",
        "case-insensitive match; the .md suffix and folder survive"
    );
}

#[test]
fn relink_leaves_a_caret_without_hash_alone_because_it_names_another_note() {
    // Obsidian's block ref is `#^id`; `[[old-name^abc]]` targets a note named
    // `old-name^abc`, which this rename does not touch.
    assert_eq!(
        relink("[[old-name^abc123|quote]]", "old-name", "new-name"),
        "[[old-name^abc123|quote]]"
    );
}

#[test]
fn relink_leaves_a_path_link_to_another_directory_alone() {
    // `notes/old-name.md` is renamed; `[[archive/old-name]]` is a different note.
    assert_eq!(
        relink("[[archive/old-name]] and [[notes/old-name]]", "old-name", "new-name"),
        "[[archive/old-name]] and [[notes/new-name]]"
    );
}

#[test]
fn relink_leaves_links_inside_code_alone() {
    assert_eq!(
        relink(
            "`[[old-name]]` and [[old-name]]\n\n```\n[[old-name]]\n```",
            "old-name",
            "new-name"
        ),
        "`[[old-name]]` and [[new-name]]\n\n```\n[[old-name]]\n```"
    );
}

#[test]
fn relink_rewrites_frontmatter_links_including_indented_yaml() {
    let v = crate::testutil::TestVault::new();
    v.add_note(
        "hub.md",
        "---\ntitle: Hub\nrelated: \"[[old-name]]\"\nmembers:\n    - \"[[notes/old-name|Old]]\"\n---\n\nbody [[old-name]]\n",
    );
    let notes = v.scan();
    let renames = vec![(
        std::path::PathBuf::from("notes/old-name.md"),
        std::path::PathBuf::from("notes/new-name.md"),
    )];
    update_wikilinks_batch(v.root(), &notes, &renames).expect("relink");
    assert_eq!(
        v.read("hub.md"),
        "---\ntitle: Hub\nrelated: \"[[new-name]]\"\nmembers:\n    - \"[[notes/new-name|Old]]\"\n---\n\nbody [[new-name]]\n"
    );
}

#[test]
fn relink_leaves_unrelated_targets_alone() {
    assert_eq!(
        relink("[[old-name-extended]] and [[other]]", "old-name", "new-name"),
        "[[old-name-extended]] and [[other]]"
    );
}

#[test]
fn relink_returns_the_unreadable_note_and_still_rewrites_the_rest() {
    let v = crate::testutil::TestVault::new();
    let fm = "---\ntitle: T\ndate: 2026-08-19\ntype: note\norigin: authored\ntags: []\n---\n\n";
    v.add_note("good.md", &format!("{fm}see [[old-name]]\n"));
    v.add_note("broken.md", &format!("{fm}see [[old-name]]\n"));
    let notes = v.scan();
    // Scanned fine, then becomes unreadable (invalid UTF-8) before the relink.
    std::fs::write(v.root().join("broken.md"), [0xff, 0xfe, 0x00]).expect("corrupt");
    let renames = vec![(PathBuf::from("notes/old-name.md"), PathBuf::from("notes/new-name.md"))];

    let out = update_wikilinks_batch(v.root(), &notes, &renames).expect("relink continues");

    assert_eq!(out.unreadable, vec![PathBuf::from("broken.md")]);
    assert_eq!(out.rewritten, vec!["good.md".to_string()]);
    assert!(v.read("good.md").contains("[[new-name]]"));
}

#[test]
fn apply_naming_reports_the_unreadable_note_and_the_lint_report_names_it() {
    let v = crate::testutil::TestVault::new();
    let fm = "---\ntitle: T\ndate: 2026-08-19\ntype: note\norigin: authored\ntags: []\n---\n\n";
    v.add_note("My Renamed Note.md", &format!("{fm}x\n"));
    v.add_note("linker.md", &format!("{fm}see [[My Renamed Note]]\n"));
    let notes = v.scan();
    std::fs::write(v.root().join("linker.md"), [0xff, 0xfe, 0x00]).expect("corrupt");
    let config = v.config().actions.naming;

    let applied = apply_naming(v.root(), &notes, &config, &crate::shutdown::StopHandle::never()).expect("apply");

    assert!(v.exists("my-renamed-note.md"), "the rename itself still landed");
    assert_eq!(applied.unreadable, vec![PathBuf::from("linker.md")]);

    let mut report = crate::report::Report::default();
    report.add_unreadable_after_rename("naming", &applied.unreadable);
    assert_eq!(report.violations.len(), 1);
    assert_eq!(report.violations[0].path, PathBuf::from("linker.md"));
    assert_eq!(report.violations[0].severity, crate::report::Severity::Warning);
}

// ---- stop check points and relink on stop or error ----

const FM: &str = "---\ntitle: T\ndate: 2026-10-01\ntype: note\norigin: authored\ntags: []\n---\n\n";

/// A vault of three badly named notes and one note linking to all three, in a
/// tempdir with no other notes, so its broken-link count starts at 0.
fn three_renames_vault() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tmpdir");
    for name in ["Alpha Note", "Beta Note", "Gamma Note"] {
        std::fs::write(dir.path().join(format!("{name}.md")), format!("{FM}{name} body\n")).unwrap();
    }
    std::fs::write(
        dir.path().join("refs.md"),
        format!("{FM}See [[Alpha Note]], [[Beta Note]] and [[Gamma Note]].\n"),
    )
    .unwrap();
    dir
}

/// Path order, so the rename order is Alpha, Beta, Gamma.
fn scan_sorted(root: &Path) -> Vec<Note> {
    let mut notes = crate::vault::scan_vault(root, &crate::config::VaultConfig::default()).expect("scan");
    notes.sort_by(|a, b| a.path.cmp(&b.path));
    notes
}

fn broken_link_count(root: &Path) -> usize {
    let notes = scan_sorted(root);
    crate::links::lint_broken_links(&notes, &notes, &crate::config::BrokenLinksConfig::default())
        .violations
        .len()
}

/// Flag set after rename 1 of 3: note 1 is renamed, notes 2 and 3 keep their
/// names, the referrer is relinked to note 1's new name, and the vault has no
/// broken wikilink.
#[test]
fn stop_after_rename_one_relinks_it_and_leaves_the_rest() {
    let dir = three_renames_vault();
    let root = dir.path();
    assert_eq!(broken_link_count(root), 0, "fixture starts clean");
    let notes = scan_sorted(root);
    let stop = StopHandle::never();
    let mut renames = 0;
    let rename = |from: &Path, to: &Path| {
        renames += 1;
        std::fs::rename(from, to)?;
        if renames == 1 {
            stop.stop();
        }
        Ok(())
    };

    let applied =
        apply_naming_with(root, &notes, &NamingConfig::default(), &stop, rename).expect("a stop is not an error");

    assert!(applied.stopped);
    assert!(root.join("alpha-note.md").exists(), "note 1 renamed");
    assert!(
        root.join("Beta Note.md").exists() && root.join("Gamma Note.md").exists(),
        "notes 2 and 3 untouched"
    );
    assert!(!root.join("beta-note.md").exists() && !root.join("gamma-note.md").exists());
    let refs = std::fs::read_to_string(root.join("refs.md")).unwrap();
    assert!(
        refs.contains("[[alpha-note]]"),
        "referrer relinked to note 1's new name:\n{refs}"
    );
    assert!(
        refs.contains("[[Beta Note]]") && refs.contains("[[Gamma Note]]"),
        "{refs}"
    );
    assert_eq!(broken_link_count(root), 0, "no broken wikilink after a stopped run");
    assert!(applied.written.contains(&"refs.md".to_string()));
}

/// A rename that fails on note 2 returns the error, and note 1's completed
/// rename is relinked before it returns.
#[test]
fn rename_error_on_note_two_relinks_note_one_before_returning_the_error() {
    let dir = three_renames_vault();
    let root = dir.path();
    let notes = scan_sorted(root);
    let mut renames = 0;
    let rename = |from: &Path, to: &Path| {
        renames += 1;
        if renames == 2 {
            return Err(std::io::Error::other("injected rename failure"));
        }
        std::fs::rename(from, to)
    };

    let err = apply_naming_with(root, &notes, &NamingConfig::default(), &StopHandle::never(), rename)
        .expect_err("the rename error is returned");

    assert!(format!("{err:#}").contains("injected rename failure"), "{err:#}");
    assert!(root.join("alpha-note.md").exists(), "note 1's rename landed");
    assert!(root.join("Beta Note.md").exists() && root.join("Gamma Note.md").exists());
    let refs = std::fs::read_to_string(root.join("refs.md")).unwrap();
    assert!(
        refs.contains("[[alpha-note]]"),
        "note 1 relinked before the error returned:\n{refs}"
    );
    assert_eq!(broken_link_count(root), 0);
}

/// A flag set before the first rename renames nothing and writes nothing.
#[test]
fn preset_stop_renames_nothing() {
    let dir = three_renames_vault();
    let root = dir.path();
    let notes = scan_sorted(root);
    let stop = StopHandle::never();
    stop.stop();

    let applied = apply_naming(root, &notes, &NamingConfig::default(), &stop).expect("apply");

    assert!(applied.stopped);
    assert!(applied.written.is_empty(), "{:?}", applied.written);
    assert!(root.join("Alpha Note.md").exists());
}
