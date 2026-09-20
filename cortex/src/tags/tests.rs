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

    let written = apply_tags(v.root(), &notes, &config).expect("apply");
    assert!(!written.is_empty());

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

#[test]
fn alias_free_ai_survives_apply_tags() {
    // P1 retires the `ai`/`ML`/`ml` -> `ai-llm` aliases in `cortex.yml` so the
    // P4 migration can write `ai` as a tag and the next daemon tick leaves it
    // alone. With the aliases gone, `apply_tags` must not rewrite `ai`.
    let v = TestVault::new();
    let notes = v.scan();
    let mut config = v.config().actions.tags;
    config.aliases.remove("ai");
    config.aliases.remove("ML");
    config.aliases.remove("ml");

    apply_tags(v.root(), &notes, &config).expect("apply");

    let content = v.read("ai-research.md");
    assert!(
        !content.contains("ai-llm"),
        "retired alias still rewrote `ai`:\n{content}"
    );
    // Form-agnostic on purpose: cortex still writes the inline list here and
    // only switches to block form in P4, so this must read both.
    assert!(
        frontmatter_tags(&content).contains(&"ai".to_string()),
        "`ai` did not survive apply_tags:\n{content}"
    );
}

/// Read a note's `tags` values out of its frontmatter in either on-disk form
/// (inline `tags: [a, b]` or a block list of `- a` lines).
fn frontmatter_tags(content: &str) -> Vec<String> {
    let fm = content.split("\n---\n").next().unwrap_or_default();
    let mut lines = fm.lines().skip_while(|l| !l.trim_start().starts_with("tags:"));
    let Some(head) = lines.next() else {
        return Vec::new();
    };
    let rest = head.trim_start().trim_start_matches("tags:").trim();
    if let Some(inner) = rest.strip_prefix('[').and_then(|r| r.strip_suffix(']')) {
        return inner
            .split(',')
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .collect();
    }
    lines
        .map_while(|l| l.trim().strip_prefix("- ").map(|t| t.trim().to_string()))
        .collect()
}
