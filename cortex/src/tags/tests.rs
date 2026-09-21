use std::collections::HashSet;

use super::*;
use crate::testutil::{NoteBuilder, TestVault};

/// The vocabulary `TestVault`'s fixtures were written against (P9: lint reads
/// `canonical-tags.yml`, not `actions.tags.canonical`, so tests build the
/// `CanonicalSet` shape directly rather than a `TagsConfig.canonical` list).
/// `max_per_note` 7 is well above every fixture's tag count (max 2), so it
/// never trips `tags.cap` by accident.
fn test_canon() -> CanonicalSet {
    CanonicalSet {
        all: [
            "rust",
            "python",
            "programming",
            "ai-llm",
            "kubernetes",
            "sre",
            "obsidian",
            "writing",
        ]
        .into_iter()
        .map(String::from)
        .collect(),
        no_segment: HashSet::new(),
        no_classifier: HashSet::new(),
        max_per_note: 7,
    }
}

#[test]
fn test_alias_resolution_on_vault() {
    let v = TestVault::new();
    let notes = v.scan();
    let config = v.config().actions.tags;
    let canon = test_canon();

    let report = lint_tags(&notes, &config, &canon);
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
    let canon = test_canon();

    let report = lint_tags(&notes, &config, &canon);
    // hobby-project.md has tag "obscure-hobby", not in canonical-tags.yml
    assert!(
        report
            .violations
            .iter()
            .any(|vi| vi.path.to_string_lossy() == "hobby-project.md" && vi.rule == "tags.non-canonical")
    );
}

/// P9 success criterion: on a three-note fixture, `sb cortex lint` reports
/// exactly one `tags.non-canonical`, one `tags.cap`, one `tags.format`
/// (the form variant - inline on disk). Each note is built to trip exactly
/// one of the three rules and none of the others.
#[test]
fn tags_schema_rules_fire_once_each() {
    let canon = CanonicalSet {
        all: ["rust", "cli", "ai", "llm"].into_iter().map(String::from).collect(),
        no_segment: HashSet::new(),
        no_classifier: HashSet::new(),
        max_per_note: 3,
    };
    let config = TagsConfig::default();

    let non_canonical = NoteBuilder::new("non-canonical.md")
        .tags(&["rust", "not-a-real-tag"])
        .raw("---\ntitle: Non-canonical\ntags:\n  - rust\n  - not-a-real-tag\n---\nBody\n")
        .build();
    let over_cap = NoteBuilder::new("over-cap.md")
        .tags(&["rust", "cli", "ai", "llm"])
        .raw("---\ntitle: Over cap\ntags:\n  - rust\n  - cli\n  - ai\n  - llm\n---\nBody\n")
        .build();
    let inline_form = NoteBuilder::new("inline-form.md")
        .tags(&["rust"])
        .raw("---\ntitle: Inline form\ntags: [rust]\n---\nBody\n")
        .build();
    let notes = [non_canonical, over_cap, inline_form];

    let report = lint_tags(&notes, &config, &canon);

    for rule in ["tags.non-canonical", "tags.cap", "tags.format"] {
        let count = report.violations.iter().filter(|v| v.rule == rule).count();
        assert_eq!(
            count, 1,
            "expected exactly one {rule}, got {count}: {:#?}",
            report.violations
        );
    }
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
    // P4: block form is now the single on-disk spelling.
    assert!(result.contains("tags:\n  - new-tag\n  - good"), "got:\n{result}");
    assert!(!result.contains("tags: ["), "inline form survived:\n{result}");
    assert!(result.contains("title: Test"));
}

/// Every `- ` bullet in a frontmatter block must sit under a key that opened a
/// list. Shared by the two orphan-bullet regressions below, which can no
/// longer simply assert "no bullets": block form means the rewritten `tags:`
/// legitimately has its own.
fn assert_no_orphan_bullets(fm_block: &str) {
    let mut under_list = false;
    for line in fm_block.lines() {
        if line.trim_start().starts_with("- ") {
            assert!(under_list, "orphan bullet survived: {line:?}\nfull fm:\n{fm_block}");
            continue;
        }
        under_list = line.ends_with(':');
    }
}

#[test]
fn replace_tags_on_column0_block_list_does_not_orphan_bullets() {
    // Regression: a column-0 block-sequence `tags:` list got the inline
    // replacement inserted while the `- tag` bullets were left orphaned.
    let content = "---\ntitle: Test\ntags:\n- old-tag\n- bad\ndate: 2026-01-01\n---\nBody\n";
    let new_tags = vec!["new-tag".to_string(), "good".to_string()];
    let result = replace_tags_in_frontmatter(content, &new_tags).expect("rewrite");
    let fm_block = result.split("\n---\n").next().expect("frontmatter");
    assert_no_orphan_bullets(fm_block);
    assert!(!result.contains("old-tag"), "stale value survived:\n{result}");
    assert!(!result.contains("- bad"), "stale value survived:\n{result}");
    assert!(result.contains("tags:\n  - new-tag\n  - good"), "got:\n{result}");
    assert!(result.contains("title: Test"));
    assert!(result.contains("date: 2026-01-01"));
}

#[test]
fn replace_tags_on_indented_block_list_does_not_orphan_bullets() {
    let content = "---\ntitle: Test\ntags:\n  - old-tag\n  - bad\ndate: 2026-01-01\n---\nBody\n";
    let new_tags = vec!["new-tag".to_string()];
    let result = replace_tags_in_frontmatter(content, &new_tags).expect("rewrite");
    let fm_block = result.split("\n---\n").next().expect("frontmatter");
    assert_no_orphan_bullets(fm_block);
    assert!(!result.contains("old-tag"), "stale value survived:\n{result}");
    assert!(!result.contains("- bad"), "stale value survived:\n{result}");
    assert_eq!(fm_block.matches("  - ").count(), 1, "expected one bullet:\n{result}");
    assert!(result.contains("tags:\n  - new-tag"), "got:\n{result}");
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
