use super::*;
use crate::testutil::TestVault;

#[test]
fn test_scope_matches_by_tag_on_vault() {
    let v = TestVault::new();
    let notes = v.scan();
    let config = v.config().actions.scope;

    let report = lint_scope(&notes, &config);
    // daily-standup.md has tag "sre" - should match work scope rule
    assert!(
        report
            .violations
            .iter()
            .any(|vi| vi.path.to_string_lossy() == "daily-standup.md")
    );
}

#[test]
fn test_scope_source_contains_on_vault() {
    let v = TestVault::new();
    let notes = v.scan();
    let config = v.config().actions.scope;

    let report = lint_scope(&notes, &config);
    // work-meeting.md has source: granola-meeting-notes
    assert!(
        report
            .violations
            .iter()
            .any(|vi| vi.path.to_string_lossy() == "work-meeting.md")
    );
}

#[test]
fn test_scope_no_match_on_personal() {
    let v = TestVault::new();
    let notes = v.scan();
    let config = v.config().actions.scope;

    let report = lint_scope(&notes, &config);
    // rust-guide.md has no work tags, no granola source - should NOT match
    assert!(
        !report
            .violations
            .iter()
            .any(|vi| vi.path.to_string_lossy() == "rust-guide.md")
    );
}

#[test]
fn test_insert_frontmatter_fields() {
    let content = "---\ntitle: Test\ndate: 2026-01-01\n---\nBody\n";
    let fields = vec![
        ("scope".to_string(), serde_yaml::Value::String("work".to_string())),
        ("company".to_string(), serde_yaml::Value::String("tatari".to_string())),
    ];

    let result = insert_frontmatter_fields(content, &fields);
    assert!(result.is_some());
    let result = result.expect("should have result");
    assert!(result.contains("scope: work"));
    assert!(result.contains("company: tatari"));
}

#[test]
fn test_apply_scope_on_vault() {
    let v = TestVault::new();
    let notes = v.scan();
    let config = v.config().actions.scope;

    let count = apply_scope(v.root(), &notes, &config).expect("apply");
    assert!(count > 0);

    // daily-standup.md should now have scope: work
    let content = v.read("daily-standup.md");
    assert!(content.contains("scope: work"));
}

#[test]
fn test_remove_frontmatter_fields() {
    let content =
        "---\ntitle: Test\ndate: 2026-01-01\ncortex-duplicate: true\ncortex-duplicate-group: dup-abc\n---\nBody\n";
    let keys = vec!["cortex-duplicate".to_string(), "cortex-duplicate-group".to_string()];

    let result = remove_frontmatter_fields(content, &keys);
    assert!(result.is_some());
    let result = result.expect("should have result");
    assert!(!result.contains("cortex-duplicate"));
    assert!(result.contains("title: Test"));
    assert!(result.contains("Body"));
}

#[test]
fn test_remove_frontmatter_fields_no_match() {
    let content = "---\ntitle: Test\ndate: 2026-01-01\n---\nBody\n";
    let keys = vec!["cortex-duplicate".to_string()];

    let result = remove_frontmatter_fields(content, &keys);
    assert!(result.is_none(), "should return None when no fields removed");
}

#[test]
fn insert_field_replacing_multi_line_list_value_does_not_orphan_bullets() {
    // Regression: `cortex-quality-issues` previously stored as a
    // column-0 block sequence. Replacing it with the inline form via
    // insert_frontmatter_fields used to delete only the header line,
    // leaving `- foo` bullets stranded as siblings of other keys -
    // structurally invalid YAML that the cortex parser then silently
    // failed on (342 affected notes in the 2026-05-19 audit).
    let content = "---\ntitle: T\ntype: youtube\ncortex-quality-issues:\n- no-outbound-links\n- missing-summary\ndistilled: true\n---\nbody\n";
    let fields = vec![(
        "cortex-quality-issues".to_string(),
        serde_yaml::Value::String("[no-outbound-links, missing-summary]".to_string()),
    )];
    let out = insert_frontmatter_fields(content, &fields).expect("rewrite");
    let fm_block = out.split("\n---\n").next().expect("frontmatter");
    // The orphan bullets must be gone from the new frontmatter.
    for line in fm_block.lines() {
        assert!(
            !line.starts_with("- "),
            "orphan list-item bullet survived: {line:?}\nfull fm:\n{fm_block}"
        );
    }
    // The new inline form should be the single representation of the value.
    assert!(out.contains("cortex-quality-issues: [no-outbound-links, missing-summary]"));
}

#[test]
fn insert_field_replacing_indented_list_value_does_not_orphan_bullets() {
    // Same regression with the indented-list style (`  - foo` instead
    // of column-0 `- foo`). Common in user-authored frontmatter.
    let content = "---\ntitle: T\ntags:\n  - rust\n  - cli\ndistilled: true\n---\nbody\n";
    let fields = vec![("tags".to_string(), serde_yaml::Value::String("[rust, cli]".to_string()))];
    let out = insert_frontmatter_fields(content, &fields).expect("rewrite");
    let fm_block = out.split("\n---\n").next().expect("frontmatter");
    for line in fm_block.lines() {
        assert!(
            !line.starts_with(' ') || !line.trim_start().starts_with('-'),
            "orphan indented bullet survived: {line:?}\nfull fm:\n{fm_block}"
        );
    }
    assert!(out.contains("tags: [rust, cli]"));
}

#[test]
fn insert_field_preserves_unrelated_lists() {
    // The continuation-aware remove must affect ONLY the targeted key.
    // Other multi-line list values (e.g. `tags:`) must survive untouched.
    let content =
        "---\ntitle: T\ntags:\n- rust\n- cli\ncortex-quality-issues:\n- old-issue\ndistilled: true\n---\nbody\n";
    let fields = vec![(
        "cortex-quality-issues".to_string(),
        serde_yaml::Value::String("[new-issue]".to_string()),
    )];
    let out = insert_frontmatter_fields(content, &fields).expect("rewrite");
    assert!(out.contains("tags:\n- rust\n- cli"), "tags list got clobbered:\n{out}");
    assert!(out.contains("cortex-quality-issues: [new-issue]"));
}

#[test]
fn remove_field_removes_multi_line_list_value_cleanly() {
    let content = "---\ntitle: T\ncortex-quality-issues:\n- a\n- b\ndistilled: true\n---\nbody\n";
    let keys = vec!["cortex-quality-issues".to_string()];
    let out = remove_frontmatter_fields(content, &keys).expect("removed");
    // No orphan bullets, and the surrounding keys survive.
    for line in out.lines() {
        assert!(!line.starts_with("- "), "orphan bullet survived: {line:?}\n{out}");
    }
    assert!(out.contains("title: T"));
    assert!(out.contains("distilled: true"));
}
