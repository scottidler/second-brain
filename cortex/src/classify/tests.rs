use super::*;
use crate::testutil::NoteBuilder;

fn test_config() -> ClassifyConfig {
    ClassifyConfig::default()
}

#[test]
fn test_classify_by_tags_single_domain() {
    let note = NoteBuilder::new("inbox/test-note.md")
        .title("Test Note")
        .tags(&["rust", "cli"])
        .build();

    let config = test_config();
    let result = classify_by_tags(&note, &config);
    assert!(result.is_some());
    let result = result.expect("should classify");
    assert_eq!(result.domain, Domain::Tech);
    assert_eq!(result.confidence, ClassifyConfidence::High);
    assert_eq!(result.method, ClassifyMethod::Deterministic);
}

#[test]
fn test_classify_by_tags_no_match() {
    let note = NoteBuilder::new("inbox/test-note.md")
        .title("Test Note")
        .tags(&["random-tag", "unrelated"])
        .build();

    let config = test_config();
    let result = classify_by_tags(&note, &config);
    assert!(result.is_none());
}

#[test]
fn test_classify_by_tags_ambiguous_tie() {
    // Tags matching two domains equally
    let note = NoteBuilder::new("inbox/test-note.md")
        .title("Test Note")
        .tags(&["rust", "claude"]) // rust=tech, claude=ai
        .build();

    let config = test_config();
    let result = classify_by_tags(&note, &config);
    // Should return None on a tie (fall through to Tier 2)
    assert!(result.is_none());
}

#[test]
fn test_classify_by_source() {
    let note = NoteBuilder::new("inbox/test-note.md")
        .title("Test Note")
        .source("https://docs.rs/some-crate")
        .build();

    let mut config = test_config();
    config.source_domain_map.insert("tech".into(), vec!["docs.rs".into()]);

    let result = classify_by_source(&note, &config);
    assert!(result.is_some());
    let result = result.expect("should classify");
    assert_eq!(result.domain, Domain::Tech);
}

#[test]
fn test_classify_by_source_no_match() {
    let note = NoteBuilder::new("inbox/test-note.md")
        .title("Test Note")
        .source("https://random-site.example.com")
        .build();

    let config = test_config();
    let result = classify_by_source(&note, &config);
    assert!(result.is_none());
}

#[test]
fn test_filter_inbox_notes() {
    let inbox_note = NoteBuilder::new("inbox/test.md").title("Test").build();
    let notes_note = NoteBuilder::new("notes/other.md").title("Other").build();
    let notes = vec![inbox_note, notes_note];

    let filtered = filter_inbox_notes(&notes, false, false);
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].path.to_string_lossy(), "inbox/test.md");
}

#[test]
fn test_filter_skips_already_classified() {
    let mut note = NoteBuilder::new("inbox/test.md").title("Test").build();
    note.frontmatter
        .extra
        .insert("cortex-classified".to_string(), serde_yaml::Value::Bool(true));
    let notes = vec![note];

    let filtered = filter_inbox_notes(&notes, false, false);
    assert_eq!(filtered.len(), 0);

    // With force=true, should include it
    let filtered = filter_inbox_notes(&notes, true, false);
    assert_eq!(filtered.len(), 1);
}

#[test]
fn test_resolve_collision_no_conflict() {
    let path = PathBuf::from("/tmp/nonexistent-classify-test-12345.md");
    assert_eq!(resolve_collision(&path, None), path);
}

/// Phase 5 fix, `2026-08-15-harvest-note-identity-trace-keyed-replace.md`
/// prior attempt 4: a base-path collision with a DIFFERENT source correctly
/// mints `-2`, but the bug walked past every SAME-source numeric candidate
/// (`-5`, `-7` .. `-14` in the real `hv-e5d240` cohort) because
/// `existing_note_has_source` was only ever applied to the base path. This
/// asserts the repaired loop overwrites the first same-source `-N` candidate
/// it finds instead of minting `-N+1`.
#[test]
fn test_resolve_collision_overwrites_same_source_numeric_candidate() {
    let dir = tempfile::tempdir().expect("tempdir");
    let base = dir.path().join("note.md");
    let source = "clyde://8d6b6ef3-aaaa-bbbb-cccc-dddddddddddd";
    // Genuinely different sources (not a suffixed variant of `source`, which
    // `existing_note_has_source`'s substring `contains` would still match).
    let other_source = "clyde://eb65b08e-1111-2222-3333-444444444444";

    // Base path collides with a DIFFERENT source.
    std::fs::write(&base, format!("---\nsource: {other_source}\n---\nbody\n")).expect("write base");
    // -2 and -3 are also different sources (real siblings from other sessions).
    std::fs::write(
        dir.path().join("note-2.md"),
        format!("---\nsource: {other_source}\n---\nbody\n"),
    )
    .expect("write -2");
    std::fs::write(
        dir.path().join("note-3.md"),
        format!("---\nsource: {other_source}\n---\nbody\n"),
    )
    .expect("write -3");
    // -4 carries the SAME source: this is the candidate that must be
    // overwritten rather than skipped in favor of -5.
    std::fs::write(
        dir.path().join("note-4.md"),
        format!("---\nsource: {source}\n---\nold body\n"),
    )
    .expect("write -4");

    let resolved = resolve_collision(&base, Some(source));
    assert_eq!(resolved, dir.path().join("note-4.md"));
    // -5 must never have been consulted/created by this call.
    assert!(!dir.path().join("note-5.md").exists());
}

/// Mirror-image control: when NO numeric candidate shares the source, the
/// loop still mints the first free `-N` slot exactly as before the fix.
#[test]
fn test_resolve_collision_mints_next_free_slot_when_no_candidate_matches_source() {
    let dir = tempfile::tempdir().expect("tempdir");
    let base = dir.path().join("note.md");
    let source = "clyde://8d6b6ef3-aaaa-bbbb-cccc-dddddddddddd";
    let other_source = "clyde://eb65b08e-1111-2222-3333-444444444444";

    std::fs::write(&base, format!("---\nsource: {other_source}\n---\nbody\n")).expect("write base");
    std::fs::write(
        dir.path().join("note-2.md"),
        format!("---\nsource: {other_source}\n---\nbody\n"),
    )
    .expect("write -2");

    let resolved = resolve_collision(&base, Some(source));
    assert_eq!(resolved, dir.path().join("note-3.md"));
}

#[test]
fn test_build_enrichment_fields() {
    let result = ClassifyResult {
        domain: Domain::Ai,
        confidence: ClassifyConfidence::High,
        method: ClassifyMethod::Deterministic,
        reason: "test".to_string(),
    };

    let fields = build_enrichment_fields(&result);
    assert!(
        fields
            .iter()
            .any(|(k, v)| k == "domain" && v == &serde_yaml::Value::String("ai".to_string()))
    );
    assert!(
        fields
            .iter()
            .any(|(k, v)| k == "status" && v == &serde_yaml::Value::String("unread".to_string()))
    );
    assert!(
        fields
            .iter()
            .any(|(k, v)| k == "cortex-classified" && v == &serde_yaml::Value::Bool(true))
    );
}

#[test]
fn test_classify_by_tags_compound_segment_match() {
    // Compound tags like "ai-agents" should match via segment "agents"
    let note = NoteBuilder::new("inbox/test-note.md")
        .title("AI Agents Article")
        .tags(&["ai-agents", "ai-strategy"])
        .build();

    let config = test_config();
    let result = classify_by_tags(&note, &config);
    assert!(result.is_some());
    let result = result.expect("should classify");
    assert_eq!(result.domain, Domain::Ai);
    assert_eq!(result.confidence, ClassifyConfidence::High);
}

#[test]
fn test_classify_by_tags_compound_claude_code() {
    // "claude-code" should match via segment "claude" -> ai domain
    let note = NoteBuilder::new("inbox/test-note.md")
        .title("Claude Code Tips")
        .tags(&["claude-code", "claudecode"])
        .build();

    let config = test_config();
    let result = classify_by_tags(&note, &config);
    assert!(result.is_some());
    let result = result.expect("should classify");
    assert_eq!(result.domain, Domain::Ai);
}

#[test]
fn test_classify_by_tags_compound_no_false_positive() {
    // Tags with no segment matching any trigger should still return None
    let note = NoteBuilder::new("inbox/test-note.md")
        .title("Random Article")
        .tags(&["career-advice", "hiring-trends"])
        .build();

    let config = test_config();
    let result = classify_by_tags(&note, &config);
    assert!(result.is_none());
}

#[test]
fn test_classify_by_tags_single_word_still_works() {
    // Exact single-word matches should still work
    let note = NoteBuilder::new("inbox/test-note.md")
        .title("Rust Article")
        .tags(&["rust"])
        .build();

    let config = test_config();
    let result = classify_by_tags(&note, &config);
    assert!(result.is_some());
    assert_eq!(result.expect("should classify").domain, Domain::Tech);
}

#[test]
fn test_classify_by_tags_multi_segment_tag() {
    // "ai-coding-agents" has segments ["ai", "coding", "agents"]
    // Both "ai" and "agents" are triggers for ai domain - should give ai +1 (not +2)
    let note = NoteBuilder::new("inbox/test-note.md")
        .title("AI Coding Agents")
        .tags(&["ai-coding-agents"])
        .build();

    let config = test_config();
    let result = classify_by_tags(&note, &config);
    assert!(result.is_some());
    assert_eq!(result.expect("should classify").domain, Domain::Ai);
}

#[test]
fn test_classify_note_tags_win_over_source() {
    let note = NoteBuilder::new("inbox/test-note.md")
        .title("AI Article on GitHub")
        .tags(&["claude", "llm", "anthropic"])
        .source("https://github.com/anthropics/claude")
        .build();

    let mut config = test_config();
    config
        .source_domain_map
        .insert("tech".into(), vec!["github.com".into()]);

    // Tags say ai (3 matches), source says tech - tags should win because
    // classify_note tries tags first
    let fabric = FabricConfig::default();
    let result = classify_note(&note, &config, &fabric, None);
    assert!(result.is_some());
    assert_eq!(result.expect("should classify").domain, Domain::Ai);
}

#[test]
fn test_filter_unclassified_notes_selects_domainless_in_notes() {
    let domainless = NoteBuilder::new("notes/orphaned.md").title("Orphaned").build();
    let classified = NoteBuilder::new("notes/good.md").title("Good").domain("tech").build();
    let inbox = NoteBuilder::new("inbox/new.md").title("New").build();
    let notes = vec![domainless, classified, inbox];

    let filtered = filter_unclassified_notes(&notes);
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].path.to_string_lossy(), "notes/orphaned.md");
}

#[test]
fn test_filter_unclassified_notes_ignores_inbox() {
    let inbox_no_domain = NoteBuilder::new("inbox/test.md").title("Test").build();
    let notes = vec![inbox_no_domain];

    let filtered = filter_unclassified_notes(&notes);
    assert_eq!(filtered.len(), 0);
}

#[test]
fn test_catchup_classify_enriches_in_place() {
    use crate::testutil::TestVault;

    let vault = TestVault::new();
    // Add a domainless note in notes/ with classifiable tags
    vault.add_note(
            "notes/reingest-orphan.md",
            "---\ntitle: Reingest Orphan\ndate: 2026-03-20\ntype: link\ntags:\n  - rust\n  - cli\nsource: \"https://example.com/rust-guide\"\n---\nA reingested note that lost its domain.\n",
        );

    let notes = vault.scan();
    let config = vault.config();
    let (report, written) = apply_classify(
        vault.root(),
        &notes,
        &config.actions.classify,
        &config.fabric,
        false,
        false,
        None,
        None,
    )
    .unwrap();

    // The orphan was actually written, so it is in the written-paths list.
    assert!(
        written.iter().any(|p| p.contains("reingest-orphan")),
        "catch-up write should be surfaced in written_paths: {written:?}"
    );

    // The orphan should have been catch-up classified
    let content = vault.read("notes/reingest-orphan.md");
    assert!(content.contains("domain: tech"), "should have domain assigned");
    assert!(
        content.contains("cortex-classified: true"),
        "should be marked classified"
    );

    // Report should mention catch-up
    let violations: Vec<_> = report
        .violations
        .iter()
        .filter(|v| v.path.to_string_lossy().contains("reingest-orphan"))
        .collect();
    assert!(!violations.is_empty(), "should have a violation for the orphan");
    assert!(
        violations[0].message.contains("catch-up"),
        "violation message should mention catch-up"
    );
}

#[test]
fn test_lint_classify_includes_unclassified_notes() {
    let inbox_note = NoteBuilder::new("inbox/test.md").title("Test").tags(&["rust"]).build();
    let orphan = NoteBuilder::new("notes/orphan.md")
        .title("Orphan")
        .tags(&["rust"])
        .build();
    let notes = vec![inbox_note, orphan];

    let config = test_config();
    let fabric = FabricConfig::default();
    let report = lint_classify(&notes, &config, &fabric, None);
    // Both should produce violations
    let paths: Vec<String> = report
        .violations
        .iter()
        .map(|v| v.path.to_string_lossy().to_string())
        .collect();
    assert!(paths.iter().any(|p| p.contains("inbox/test.md")));
    assert!(paths.iter().any(|p| p.contains("notes/orphan.md")));
}
