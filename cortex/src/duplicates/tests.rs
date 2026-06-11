use super::*;
use crate::testutil::TestVault;

#[test]
fn test_exact_duplicates_on_vault() {
    let v = TestVault::new();
    let notes = v.scan();
    let config = v.config().actions.duplicates;

    let report = lint_duplicates(&notes, &config);
    // duplicate-a.md and duplicate-b.md have identical bodies
    assert!(report.violations.iter().any(|vi| vi.rule == "duplicates.exact"
        && (vi.path.to_string_lossy().contains("duplicate-a") || vi.path.to_string_lossy().contains("duplicate-b"))));
}

#[test]
fn test_unique_notes_not_flagged() {
    let v = TestVault::new();
    let notes = v.scan();
    let config = v.config().actions.duplicates;

    let report = lint_duplicates(&notes, &config);
    // rust-guide.md and python-guide.md have different content
    assert!(
        !report
            .violations
            .iter()
            .any(|vi| vi.rule == "duplicates.exact" && vi.path.to_string_lossy() == "rust-guide.md")
    );
}

#[test]
fn test_empty_bodies_not_false_duplicates() {
    use crate::testutil::NoteBuilder;

    let notes = vec![
        NoteBuilder::new("empty-a.md").title("Empty A").body("").build(),
        NoteBuilder::new("empty-b.md").title("Empty B").body("   ").build(),
        NoteBuilder::new("empty-c.md").title("Empty C").body("\n\n").build(),
    ];
    let config = DuplicatesConfig {
        threshold: 0.85,
        same_type_only: false,
        exclude: Vec::new(),
    };

    let report = lint_duplicates(&notes, &config);
    assert!(
        report.violations.is_empty(),
        "empty body notes should not be flagged as duplicates"
    );
}

#[test]
fn test_exact_duplicates_have_fix_with_group() {
    let v = TestVault::new();
    let notes = v.scan();
    let config = v.config().actions.duplicates;

    let report = lint_duplicates(&notes, &config);
    let dupe_violations: Vec<_> = report
        .violations
        .iter()
        .filter(|vi| vi.rule == "duplicates.exact")
        .collect();

    assert!(!dupe_violations.is_empty());
    for vi in &dupe_violations {
        match &vi.fix {
            Some(Fix::SetCortexFields { fields }) => {
                assert!(fields.iter().any(|(k, v)| k == "cortex-duplicate" && v == "true"));
                assert!(fields.iter().any(|(k, _)| k == "cortex-duplicate-group"));
            }
            other => panic!("expected SetCortexFields fix, got {other:?}"),
        }
    }
}

#[test]
fn test_both_notes_in_duplicate_pair_tagged() {
    let v = TestVault::new();
    let notes = v.scan();
    let config = v.config().actions.duplicates;

    let report = lint_duplicates(&notes, &config);
    let dupe_paths: Vec<String> = report
        .violations
        .iter()
        .filter(|vi| vi.rule == "duplicates.exact")
        .map(|vi| vi.path.to_string_lossy().to_string())
        .collect();

    assert!(
        dupe_paths.iter().any(|p| p.contains("duplicate-a")),
        "duplicate-a should be tagged"
    );
    assert!(
        dupe_paths.iter().any(|p| p.contains("duplicate-b")),
        "duplicate-b should be tagged"
    );
}

#[test]
fn test_apply_duplicates_writes_frontmatter() {
    let v = TestVault::new();
    let notes = v.scan();
    let config = v.config().actions.duplicates;

    let changed = apply_duplicates(v.root(), &notes, &config).expect("apply");
    assert!(!changed.is_empty(), "should have applied duplicate fields");

    let content_a = v.read("duplicate-a.md");
    let content_b = v.read("duplicate-b.md");
    assert!(
        content_a.contains("cortex-duplicate:"),
        "duplicate-a should have cortex-duplicate field"
    );
    assert!(
        content_b.contains("cortex-duplicate:"),
        "duplicate-b should have cortex-duplicate field"
    );
    assert!(
        content_a.contains("cortex-duplicate-group:"),
        "duplicate-a should have cortex-duplicate-group field"
    );
}

#[test]
fn test_apply_duplicates_clears_stale_fields() {
    let v = TestVault::new();

    // Add a note with stale cortex-duplicate fields (not actually a duplicate)
    v.add_note(
            "formerly-duplicate.md",
            "---\ntitle: Formerly Duplicate\ndate: 2026-03-18\ntype: note\ndomain: tech\norigin: authored\ntags: []\ncortex-duplicate: true\ncortex-duplicate-group: dup-old\n---\nThis note is unique now.\n",
        );

    let notes = v.scan();
    let config = v.config().actions.duplicates;

    let changed = apply_duplicates(v.root(), &notes, &config).expect("apply");
    assert!(!changed.is_empty());

    let content = v.read("formerly-duplicate.md");
    assert!(
        !content.contains("cortex-duplicate:"),
        "stale cortex-duplicate field should be removed"
    );
    assert!(
        !content.contains("cortex-duplicate-group:"),
        "stale cortex-duplicate-group field should be removed"
    );
}

#[test]
fn test_excluded_paths_not_flagged_as_duplicates() {
    use crate::testutil::NoteBuilder;

    let notes = vec![
        NoteBuilder::new("daily/2024/01/2024-01-25.md")
            .title("2024-01-25")
            .body("brushing: false\ntyping: false\nspanish: false")
            .build(),
        NoteBuilder::new("daily/2024/01/2024-01-26.md")
            .title("2024-01-26")
            .body("brushing: false\ntyping: false\nspanish: false")
            .build(),
        NoteBuilder::new("notes/real-dupe-a.md")
            .title("Real Dupe A")
            .body("This is identical content for testing.")
            .build(),
        NoteBuilder::new("notes/real-dupe-b.md")
            .title("Real Dupe B")
            .body("This is identical content for testing.")
            .build(),
    ];
    let config = DuplicatesConfig {
        threshold: 0.85,
        same_type_only: false,
        exclude: vec!["daily/**".to_string()],
    };

    let report = lint_duplicates(&notes, &config);
    // Daily notes should NOT be flagged
    assert!(
        !report
            .violations
            .iter()
            .any(|v| v.path.to_string_lossy().contains("daily/")),
        "daily notes should be excluded from duplicate detection"
    );
    // Real dupes should still be flagged
    assert!(
        report
            .violations
            .iter()
            .any(|v| v.path.to_string_lossy().contains("real-dupe")),
        "non-excluded duplicates should still be detected"
    );
}

#[test]
fn test_cosine_similarity_identical() {
    let mut a = HashMap::new();
    a.insert("hello", 1.0);
    a.insert("world", 1.0);

    let score = cosine_similarity(&a, &a);
    assert!((score - 1.0).abs() < 0.001);
}

#[test]
fn test_cosine_similarity_orthogonal() {
    let mut a = HashMap::new();
    a.insert("hello", 1.0);

    let mut b = HashMap::new();
    b.insert("world", 1.0);

    let score = cosine_similarity(&a, &b);
    assert!((score - 0.0).abs() < 0.001);
}
