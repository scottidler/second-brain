use std::path::Path;

use crate::config::ScopeConfig;
use crate::report::{Fix, Report, Severity, Violation};
use crate::vault::Note;

/// Run scope classification on all notes.
pub fn lint_scope(notes: &[Note], config: &ScopeConfig) -> Report {
    let mut report = Report::default();

    for note in notes {
        for rule in &config.rules {
            if matches_rule(note, rule) {
                // Check if the note already has the scope fields set correctly
                for (key, value) in &rule.set {
                    let current = note.frontmatter.extra.get(key);

                    if current != Some(value) {
                        report.add(Violation {
                            path: note.path.clone(),
                            rule: format!("scope.{key}"),
                            severity: Severity::Warning,
                            message: format!("scope rule matched: should set {key}={value:?}"),
                            fix: Some(Fix::SetFrontmatter {
                                key: key.clone(),
                                value: value.clone(),
                            }),
                        });
                    }
                }
            }
        }
    }

    log::info!("scope lint complete: {} violation(s)", report.violations.len());
    report
}

/// Apply scope fixes: set frontmatter fields.
pub fn apply_scope(vault_root: &Path, notes: &[Note], config: &ScopeConfig) -> eyre::Result<usize> {
    let mut fixed_count = 0;

    for note in notes {
        let mut fields_to_set: Vec<(String, serde_yaml::Value)> = Vec::new();

        for rule in &config.rules {
            if matches_rule(note, rule) {
                for (key, value) in &rule.set {
                    let current = note.frontmatter.extra.get(key);
                    if current != Some(value) {
                        fields_to_set.push((key.clone(), value.clone()));
                    }
                }
            }
        }

        if fields_to_set.is_empty() {
            continue;
        }

        let abs_path = vault_root.join(&note.path);
        let content = std::fs::read_to_string(&abs_path)?;

        if let Some(new_content) = insert_frontmatter_fields(&content, &fields_to_set) {
            vault::note::write_atomic(&abs_path, new_content.as_bytes())?;
            log::info!("applied scope fields: {}", note.path.display());
            fixed_count += 1;
        }
    }

    Ok(fixed_count)
}

fn matches_rule(note: &Note, rule: &crate::config::ScopeRule) -> bool {
    let match_criteria = &rule.match_criteria;

    // Check tag-based matching
    if let Some(ref match_tags) = match_criteria.tags
        && let Some(ref note_tags) = note.frontmatter.tags
    {
        let has_match = match_tags.iter().any(|mt| note_tags.iter().any(|nt| nt == mt));
        if has_match {
            return true;
        }
    }

    // Check source-contains matching
    if let Some(ref source_pattern) = match_criteria.source_contains
        && let Some(ref source) = note.frontmatter.source
        && source.to_lowercase().contains(&source_pattern.to_lowercase())
    {
        return true;
    }

    false
}

/// Test whether `line` is the start of a frontmatter entry for `key` -
/// either `key:` exactly (multi-line value follows) or `key: value` /
/// `key:\t...` (inline value).
fn is_key_line(line: &str, key: &str) -> bool {
    let prefix = format!("{key}:");
    line == prefix || line.starts_with(&format!("{prefix} ")) || line.starts_with(&format!("{prefix}\t"))
}

/// True for lines that belong to the *previous* key as continuation:
/// indented (space or tab) OR a column-0 list-item bullet (`-` followed
/// by space or end-of-line, the column-0 block-sequence style YAML allows).
fn is_continuation(line: &str) -> bool {
    line.starts_with(' ') || line.starts_with('\t') || line == "-" || line.starts_with("- ")
}

/// Remove the first occurrence of `key` from `lines`, along with any
/// continuation lines (multi-line list / nested map). Stops at the next
/// top-level key, blank line, or end of input.
///
/// Without continuation-aware removal, replacing `cortex-quality-issues:`
/// (when the existing value is a multi-line list) deletes only the
/// `cortex-quality-issues:` header and leaves the `- foo` bullets orphaned
/// at the root of the frontmatter map - structurally invalid YAML that
/// the cortex parser then reads as defaults (no `type:`, no `source:`),
/// causing affected notes to be silently dropped from every subsequent
/// vault scan. See [[project_frontmatter_orphan_corruption]] for the
/// 2026-05-19 audit (342 affected notes).
pub(crate) fn remove_entry(lines: &mut Vec<String>, key: &str) {
    let mut i = 0;
    while i < lines.len() {
        if is_key_line(&lines[i], key) {
            lines.remove(i);
            while i < lines.len() && is_continuation(&lines[i]) {
                lines.remove(i);
            }
            return;
        }
        i += 1;
    }
}

/// Insert key-value pairs into frontmatter before the closing ---.
pub fn insert_frontmatter_fields(content: &str, fields: &[(String, serde_yaml::Value)]) -> Option<String> {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return None;
    }

    let after_opening = &trimmed[3..];
    let after_opening = after_opening.trim_start_matches(['\r', '\n']);
    let end_pos = after_opening.find("\n---")?;

    let fm_block = &after_opening[..end_pos];
    let rest = &after_opening[end_pos..];

    let mut new_lines: Vec<String> = fm_block.lines().map(String::from).collect();

    for (key, value) in fields {
        // Remove existing entry for this key, including any multi-line
        // continuation lines (list bullets, indented sub-fields).
        remove_entry(&mut new_lines, key);

        let value_str = match value {
            serde_yaml::Value::String(s) => s.clone(),
            serde_yaml::Value::Bool(b) => b.to_string(),
            serde_yaml::Value::Number(n) => n.to_string(),
            other => format!("{other:?}"),
        };
        new_lines.push(format!("{key}: {value_str}"));
    }

    let offset = content.len() - trimmed.len();
    let prefix = &content[..offset];
    let new_fm = new_lines.join("\n");

    Some(format!("{prefix}---\n{new_fm}{rest}"))
}

/// Remove frontmatter fields by key name (continuation-aware).
/// Returns None if no frontmatter found or no changes needed.
pub fn remove_frontmatter_fields(content: &str, keys: &[String]) -> Option<String> {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return None;
    }

    let after_opening = &trimmed[3..];
    let after_opening = after_opening.trim_start_matches(['\r', '\n']);
    let end_pos = after_opening.find("\n---")?;

    let fm_block = &after_opening[..end_pos];
    let rest = &after_opening[end_pos..];

    let mut new_lines: Vec<String> = fm_block.lines().map(String::from).collect();
    let original_len = new_lines.len();
    for key in keys {
        remove_entry(&mut new_lines, key);
    }
    if new_lines.len() == original_len {
        return None; // No fields were removed
    }

    let offset = content.len() - trimmed.len();
    let prefix = &content[..offset];
    let new_fm = new_lines.join("\n");

    Some(format!("{prefix}---\n{new_fm}{rest}"))
}

#[cfg(test)]
mod tests {
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
}
