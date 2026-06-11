use super::*;
use crate::testutil::{NoteBuilder, TestVault};

fn default_config() -> QualityConfig {
    QualityConfig { min_word_count: 50 }
}

#[test]
fn test_empty_body_flagged_critical() {
    let notes = vec![
            NoteBuilder::new("empty.md")
                .title("Empty")
                .note_type("note")
                .body("")
                .build(),
            NoteBuilder::new("other.md")
                .title("Other")
                .note_type("note")
                .body("A real note with enough words to pass the stub check. More words needed here to reach fifty words total. Let's keep adding until we have enough content to avoid the stub threshold completely.")
                .build(),
        ];

    let report = lint_quality(&notes, &default_config());
    let empty_vi = report
        .violations
        .iter()
        .find(|v| v.path.to_string_lossy() == "empty.md");
    assert!(empty_vi.is_some(), "empty body should be flagged");
    assert!(empty_vi.unwrap().message.contains("empty-body"));
}

#[test]
fn test_stub_body_flagged() {
    let notes = vec![
        NoteBuilder::new("stub.md")
            .title("Stub")
            .note_type("note")
            .body("Just a few words.")
            .build(),
    ];

    let report = lint_quality(&notes, &default_config());
    assert!(report.violations.iter().any(|v| v.message.contains("stub-body")));
}

#[test]
fn test_no_inbound_links_flagged() {
    let notes = vec![
            NoteBuilder::new("orphan.md")
                .title("Orphan")
                .note_type("note")
                .body("A real note with enough words to pass. More words needed here to reach fifty words total. Let's keep adding until we have enough content to completely avoid the stub body threshold check.")
                .build(),
            NoteBuilder::new("other.md")
                .title("Other")
                .note_type("note")
                .body("This note links to [[something-else]] but not to orphan.")
                .build(),
        ];

    let report = lint_quality(&notes, &default_config());
    let orphan_vi = report
        .violations
        .iter()
        .find(|v| v.path.to_string_lossy() == "orphan.md");
    assert!(orphan_vi.is_some());
    assert!(orphan_vi.unwrap().message.contains("no-inbound-links"));
}

#[test]
fn test_inbound_link_not_flagged() {
    let notes = vec![
            NoteBuilder::new("target.md")
                .title("Target")
                .note_type("note")
                .body("A real note with enough words to pass the stub check. More words needed here to reach fifty words total. Let's keep adding until we have enough content to completely avoid threshold. See also [[other]].")
                .build(),
            NoteBuilder::new("referrer.md")
                .title("Referrer")
                .note_type("note")
                .body("See [[target]] for details. This has enough words to pass the stub check. More words needed here to reach fifty. Let's keep adding until we have enough content to completely avoid the threshold.")
                .build(),
        ];

    let report = lint_quality(&notes, &default_config());
    let target_issues: Vec<_> = report
        .violations
        .iter()
        .filter(|v| v.path.to_string_lossy() == "target.md")
        .collect();

    assert!(
        !target_issues.iter().any(|v| v.message.contains("no-inbound-links")),
        "target should not be flagged for no inbound links"
    );
}

#[test]
fn test_system_types_excluded() {
    let notes = vec![
        NoteBuilder::new("digest.md")
            .title("Daily Digest")
            .note_type("digest")
            .body("")
            .build(),
        NoteBuilder::new("review.md")
            .title("Weekly Review")
            .note_type("review")
            .body("")
            .build(),
    ];

    let report = lint_quality(&notes, &default_config());
    assert!(
        report.is_empty(),
        "system types should be excluded from quality scoring"
    );
}

#[test]
fn test_quality_level_computation() {
    let critical = vec![QualityIssue {
        name: "empty-body".to_string(),
        severity: IssueSeverity::Critical,
    }];
    assert_eq!(compute_level(&critical), QualityLevel::Low);

    let two_warnings = vec![
        QualityIssue {
            name: "stub-body".to_string(),
            severity: IssueSeverity::Warning,
        },
        QualityIssue {
            name: "no-inbound-links".to_string(),
            severity: IssueSeverity::Warning,
        },
    ];
    assert_eq!(compute_level(&two_warnings), QualityLevel::Low);

    let one_warning = vec![QualityIssue {
        name: "stub-body".to_string(),
        severity: IssueSeverity::Warning,
    }];
    assert_eq!(compute_level(&one_warning), QualityLevel::Medium);

    let info_only = vec![QualityIssue {
        name: "no-outbound-links".to_string(),
        severity: IssueSeverity::Info,
    }];
    assert_eq!(compute_level(&info_only), QualityLevel::Medium);
}

#[test]
fn test_apply_quality_on_vault() {
    let v = TestVault::new();
    let notes = v.scan();
    let config = default_config();

    let changed = apply_quality(v.root(), &notes, &config).expect("apply");
    assert!(!changed.is_empty(), "should have applied quality fields to some notes");

    // bare-note.md has no frontmatter at all, so it won't get fields written
    // but partial-frontmatter.md has a stub body and should be flagged
    let partial = v.read("partial-frontmatter.md");
    assert!(
        partial.contains("cortex-quality:"),
        "partial-frontmatter.md should have quality field"
    );
}

#[test]
fn test_failed_fetch_signature_detects_paraphrased_block() {
    assert!(has_failed_fetch_signature(
        "The provided input contains an error message indicating that access to the website is blocked."
    ));
    assert!(has_failed_fetch_signature(
        "This page contains only an error message and no real content."
    ));
    assert!(has_failed_fetch_signature(
        "Anonymous access to domain has been blocked due to suspected DDoS activity."
    ));
}

#[test]
fn test_failed_fetch_signature_not_triggered_on_clean_content() {
    assert!(!has_failed_fetch_signature(
        "Docker containers provide lightweight virtualisation. The article explains seven useful \
             containers for self-hosters who want to minimise overhead."
    ));
}

#[test]
fn test_failed_fetch_flagged_critical() {
    let body = "The provided input contains an error message indicating that access to the website \
                    is blocked. More words here to exceed the min_word_count threshold so the stub check \
                    does not interfere with the failed-fetch signal being the dominant issue on the note. \
                    Adding enough content to reach beyond fifty words total for the configured threshold.";
    let notes = vec![
        NoteBuilder::new("blocked.md")
            .title("Blocked")
            .note_type("note")
            .body(body)
            .build(),
    ];

    let report = lint_quality(&notes, &default_config());
    let vi = report
        .violations
        .iter()
        .find(|v| v.path.to_string_lossy() == "blocked.md")
        .expect("blocked.md should be flagged");
    assert!(vi.message.contains("failed-fetch"));
    // Critical issue → QualityLevel::Low
    assert!(vi.message.contains("low"));
}

#[test]
fn test_apply_quality_clears_stale_fields() {
    let v = TestVault::new();
    v.add_note(
            "was-bad.md",
            "---\ntitle: Was Bad\ndate: 2026-03-18\ntype: note\ndomain: tech\norigin: authored\ntags:\n  - rust\ncortex-quality: low\ncortex-quality-issues: \"[empty-body]\"\n---\nNow this note has a real body with plenty of words to pass the quality checks. It has enough content to not be a stub. It also has outbound links like [[rust-guide]] and a summary section.\n\n## Summary\n\nThis note is now high quality.\n",
        );

    let notes = v.scan();
    let config = default_config();

    apply_quality(v.root(), &notes, &config).expect("apply");

    let content = v.read("was-bad.md");
    // Note: it may still have quality fields if it fails other checks (no-inbound-links),
    // but the old "empty-body" issue should not persist since it now has content
    assert!(!content.contains("empty-body"), "stale empty-body issue should be gone");
}

/// Phase 2a determinism guard: parallel `lint_quality` produces violations in the same order
/// as the sequential implementation would over the same input slice. Concretely, the
/// `path`-keyed sequence of violations must equal the sequence the input slice's path
/// ordering would dictate.
#[test]
fn lint_quality_violations_ordered_by_input_slice_under_par_iter() {
    // Build 20 short-body notes whose names span the alphabet so the slice has a
    // non-trivial ordering. Each note is short enough to be flagged as a "stub".
    let mut notes = Vec::new();
    for tag in &[
        "alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "golf", "hotel", "india", "juliet", "kilo", "lima",
        "mike", "november", "oscar", "papa", "quebec", "romeo", "sierra", "tango",
    ] {
        let filename = format!("{tag}.md");
        notes.push(
            NoteBuilder::new(&filename)
                .title(tag)
                .note_type("note")
                .body("short stub")
                .build(),
        );
    }
    let config = default_config();

    let report = lint_quality(&notes, &config);

    // Every violation has a path; that path sequence must equal the (filtered) input order.
    let violation_paths: Vec<String> = report
        .violations
        .iter()
        .map(|v| v.path.to_string_lossy().to_string())
        .collect();
    // Walk the input slice in order, retaining only entries that appear in violation_paths.
    let expected: Vec<String> = notes
        .iter()
        .map(|n| n.path.to_string_lossy().to_string())
        .filter(|p| violation_paths.contains(p))
        .collect();
    assert_eq!(
        violation_paths, expected,
        "par_iter().filter_map().collect() must preserve input-slice order"
    );
    assert!(
        !report.violations.is_empty(),
        "test fixture should produce at least one violation"
    );
}
