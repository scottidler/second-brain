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
        // Per-note errors WARN and skip, never `?`-abort the whole run (a note
        // deleted between scan and apply is routine on a Syncthing'd vault).
        let content = match std::fs::read_to_string(&abs_path) {
            Ok(c) => c,
            Err(e) => {
                log::warn!("skipping scope fix for {}: {e}", note.path.display());
                continue;
            }
        };

        if let Some(new_content) = insert_frontmatter_fields(&content, &fields_to_set) {
            if let Err(e) = vault::note::write_atomic(&abs_path, new_content.as_bytes()) {
                log::warn!("skipping scope fix for {}: {e}", note.path.display());
                continue;
            }
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

        // Scalars are emitted RAW (no added quoting): cortex stores some fields
        // as strings that must render unquoted - e.g. `cortex-quality-issues`
        // holds the literal `[no-outbound-links]` (an inline-array-shaped
        // string the readers expect verbatim). Routing those through serde_yaml
        // would quote them (`'[...]'`) and break the convention. NON-scalar
        // values (sequences/maps) ARE serialized via serde_yaml - the old code
        // fell back to `format!("{other:?}")`, writing a Rust Debug repr
        // (`Sequence([String("a")])`) that is not parseable YAML.
        match value {
            serde_yaml::Value::String(s) => new_lines.push(format!("{key}: {s}")),
            serde_yaml::Value::Bool(b) => new_lines.push(format!("{key}: {b}")),
            serde_yaml::Value::Number(n) => new_lines.push(format!("{key}: {n}")),
            other => {
                let mut map = serde_yaml::Mapping::new();
                map.insert(serde_yaml::Value::String(key.clone()), other.clone());
                match serde_yaml::to_string(&serde_yaml::Value::Mapping(map)) {
                    Ok(yaml) => {
                        for line in yaml.trim_end_matches('\n').lines() {
                            new_lines.push(line.to_string());
                        }
                    }
                    Err(e) => {
                        log::warn!("scope: skipping unserializable frontmatter field {key}: {e}");
                    }
                }
            }
        }
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
mod tests;
