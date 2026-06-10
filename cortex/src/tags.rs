use std::collections::HashMap;
use std::path::Path;

use crate::config::TagsConfig;
use crate::report::{Fix, Report, Severity, Violation};
use crate::vault::Note;

/// Run tag normalization lint on all notes.
pub fn lint_tags(notes: &[Note], config: &TagsConfig) -> Report {
    let mut report = Report::default();
    let mut tag_usage: HashMap<String, usize> = HashMap::new();

    for note in notes {
        if let Some(ref tags) = note.frontmatter.tags {
            for tag in tags {
                *tag_usage.entry(tag.clone()).or_insert(0) += 1;

                // Check if tag is an alias
                if let Some(canonical) = config.aliases.get(tag) {
                    report.add(Violation {
                        path: note.path.clone(),
                        rule: "tags.alias".to_string(),
                        severity: Severity::Warning,
                        message: format!("tag '{tag}' is an alias for '{canonical}'"),
                        fix: Some(Fix::ReplaceTag {
                            old: tag.clone(),
                            new: canonical.clone(),
                        }),
                    });
                    continue;
                }

                // Check if tag is lowercase-hyphenated
                if !is_valid_tag(tag) {
                    let normalized = normalize_tag(tag);
                    report.add(Violation {
                        path: note.path.clone(),
                        rule: "tags.format".to_string(),
                        severity: Severity::Warning,
                        message: format!("tag '{tag}' is not lowercase-hyphenated"),
                        fix: Some(Fix::ReplaceTag {
                            old: tag.clone(),
                            new: normalized,
                        }),
                    });
                    continue;
                }

                // Check if tag is in canonical list (if list is non-empty)
                if !config.canonical.is_empty() && !config.canonical.contains(tag) {
                    report.add(Violation {
                        path: note.path.clone(),
                        rule: "tags.non-canonical".to_string(),
                        severity: Severity::Info,
                        message: format!("tag '{tag}' is not in canonical list"),
                        fix: None,
                    });
                }
            }
        }
    }

    // Report orphan tags (used by only one note)
    for (tag, count) in &tag_usage {
        if *count == 1 {
            report.add(Violation {
                path: std::path::PathBuf::from("(vault-wide)"),
                rule: "tags.orphan".to_string(),
                severity: Severity::Info,
                message: format!("tag '{tag}' is used by only 1 note"),
                fix: None,
            });
        }
    }

    log::info!("tags lint complete: {} violation(s)", report.violations.len());
    report
}

/// Apply tag fixes: rewrite tag lists in frontmatter.
pub fn apply_tags(vault_root: &Path, notes: &[Note], config: &TagsConfig) -> eyre::Result<usize> {
    let mut fixed_count = 0;

    for note in notes {
        let tags = match &note.frontmatter.tags {
            Some(t) => t,
            None => continue,
        };

        let mut new_tags = tags.clone();
        let mut changed = false;

        for (i, tag) in tags.iter().enumerate() {
            // Resolve alias
            if let Some(canonical) = config.aliases.get(tag) {
                new_tags[i] = canonical.clone();
                changed = true;
                continue;
            }

            // Normalize format
            if !is_valid_tag(tag) {
                new_tags[i] = normalize_tag(tag);
                changed = true;
            }
        }

        // Deduplicate tags after normalization
        new_tags.sort();
        new_tags.dedup();
        if new_tags.len() != tags.len() {
            changed = true;
        }

        if changed {
            let abs_path = vault_root.join(&note.path);
            let content = std::fs::read_to_string(&abs_path)?;

            if let Some(new_content) = replace_tags_in_frontmatter(&content, &new_tags) {
                vault::note::write_atomic(&abs_path, new_content.as_bytes())?;
                log::info!("updated tags: {}", note.path.display());
                fixed_count += 1;
            }
        }
    }

    Ok(fixed_count)
}

/// Check if a tag is valid lowercase-hyphenated format (unicode-aware).
/// Accepts: lowercase letters, caseless scripts (Hebrew, CJK, etc.), digits, hyphens.
fn is_valid_tag(tag: &str) -> bool {
    if tag.is_empty() {
        return false;
    }
    tag.chars().all(|c| {
        if c == '-' || c.is_ascii_digit() {
            return true;
        }
        if !c.is_alphabetic() {
            return false;
        }
        // Accept lowercase OR caseless scripts (Hebrew, Arabic, CJK, etc.)
        c.is_lowercase() || !c.is_uppercase()
    }) && !tag.starts_with('-')
        && !tag.ends_with('-')
        && !tag.contains("--")
}

/// Normalize a tag to lowercase-hyphenated format.
fn normalize_tag(tag: &str) -> String {
    tag.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

/// Replace the `tags` entry in frontmatter YAML with an inline list of
/// `new_tags`, rewriting it to `tags: [a, b, ...]`.
///
/// Delegates to [`crate::scope::insert_frontmatter_fields`], whose
/// continuation-aware `remove_entry` handles BOTH indented (`  - tag`) and
/// column-0 block-sequence (`- tag`) list styles. The previous bespoke
/// continuation logic only consumed indented bullets, so a column-0 block
/// `tags:` list (present in real vault notes) had the new inline line
/// inserted while the old `- tag` bullets were left orphaned as siblings of
/// other keys - structurally invalid YAML that the cortex parser then read
/// as defaults, silently dropping the note from subsequent scans. The daemon
/// auto-applies sweep, so this corrupted notes unattended.
pub fn replace_tags_in_frontmatter(content: &str, new_tags: &[String]) -> Option<String> {
    let inline = format!("[{}]", new_tags.join(", "));
    crate::scope::insert_frontmatter_fields(content, &[("tags".to_string(), serde_yaml::Value::String(inline))])
}

#[cfg(test)]
mod tests {
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
}
