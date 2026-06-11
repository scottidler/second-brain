use super::*;
use crate::testutil::TestVault;

#[test]
fn test_alias_resolution_on_vault() {
    let v = TestVault::new();
    let notes = v.scan();
    let config = v.config().actions.tags;

    let report = lint_tags(&notes, &config);
    // ai-research.md has tags: [ai, k8s] which are aliases
    assert!(
        report
            .violations
            .iter()
            .any(|vi| vi.path.to_string_lossy() == "ai-research.md"
                && vi.rule == "tags.alias"
                && vi.message.contains("ai-llm"))
    );
    assert!(
        report
            .violations
            .iter()
            .any(|vi| vi.path.to_string_lossy() == "ai-research.md"
                && vi.rule == "tags.alias"
                && vi.message.contains("kubernetes"))
    );
}

#[test]
fn test_non_canonical_tag_on_vault() {
    let v = TestVault::new();
    let notes = v.scan();
    let config = v.config().actions.tags;

    let report = lint_tags(&notes, &config);
    // hobby-project.md has tag "obscure-hobby" not in canonical list
    assert!(
        report
            .violations
            .iter()
            .any(|vi| vi.path.to_string_lossy() == "hobby-project.md" && vi.rule == "tags.non-canonical")
    );
}

#[test]
fn test_normalize_tag() {
    assert_eq!(normalize_tag("Hello World"), "hello-world");
    assert_eq!(normalize_tag("AI/ML"), "ai-ml");
    assert_eq!(normalize_tag("already-valid"), "already-valid");
    assert_eq!(normalize_tag("UPPERCASE"), "uppercase");
}

#[test]
fn test_is_valid_tag() {
    assert!(is_valid_tag("rust"));
    assert!(is_valid_tag("ai-llm"));
    assert!(is_valid_tag("k8s"));
    assert!(!is_valid_tag("Bad"));
    assert!(!is_valid_tag("has space"));
    assert!(!is_valid_tag("-leading"));
    assert!(!is_valid_tag(""));
}

#[test]
fn test_apply_tags_resolves_aliases() {
    let v = TestVault::new();
    let notes = v.scan();
    let config = v.config().actions.tags;

    let count = apply_tags(v.root(), &notes, &config).expect("apply");
    assert!(count > 0);

    // ai-research.md should now have ai-llm and kubernetes instead of ai and k8s
    let content = v.read("ai-research.md");
    assert!(content.contains("ai-llm") || content.contains("kubernetes"));
}

#[test]
fn test_replace_tags_in_frontmatter() {
    let content = "---\ntitle: Test\ntags: [old-tag, bad]\ndate: 2026-01-01\n---\nBody\n";
    let new_tags = vec!["new-tag".to_string(), "good".to_string()];
    let result = replace_tags_in_frontmatter(content, &new_tags);
    assert!(result.is_some());
    let result = result.expect("should have result");
    assert!(result.contains("tags: [new-tag, good]"));
    assert!(result.contains("title: Test"));
}

#[test]
fn replace_tags_on_column0_block_list_does_not_orphan_bullets() {
    // Regression: a column-0 block-sequence `tags:` list got the inline
    // replacement inserted while the `- tag` bullets were left orphaned.
    let content = "---\ntitle: Test\ntags:\n- old-tag\n- bad\ndate: 2026-01-01\n---\nBody\n";
    let new_tags = vec!["new-tag".to_string(), "good".to_string()];
    let result = replace_tags_in_frontmatter(content, &new_tags).expect("rewrite");
    let fm_block = result.split("\n---\n").next().expect("frontmatter");
    for line in fm_block.lines() {
        assert!(
            !line.starts_with("- "),
            "orphan column-0 bullet survived: {line:?}\nfull fm:\n{fm_block}"
        );
    }
    assert!(result.contains("tags: [new-tag, good]"));
    assert!(result.contains("title: Test"));
    assert!(result.contains("date: 2026-01-01"));
}

#[test]
fn replace_tags_on_indented_block_list_does_not_orphan_bullets() {
    let content = "---\ntitle: Test\ntags:\n  - old-tag\n  - bad\ndate: 2026-01-01\n---\nBody\n";
    let new_tags = vec!["new-tag".to_string()];
    let result = replace_tags_in_frontmatter(content, &new_tags).expect("rewrite");
    let fm_block = result.split("\n---\n").next().expect("frontmatter");
    for line in fm_block.lines() {
        assert!(
            !line.trim_start().starts_with("- "),
            "orphan indented bullet survived: {line:?}\nfull fm:\n{fm_block}"
        );
    }
    assert!(result.contains("tags: [new-tag]"));
    assert!(result.contains("date: 2026-01-01"));
}
