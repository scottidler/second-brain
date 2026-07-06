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
///
/// Returns the real, byte-changed paths this call actually wrote - the
/// daemon's oscillation fingerprint draws only from this, never from the
/// lint report's violation paths (`tags.non-canonical`/`tags.orphan` carry
/// `fix: None` and are never written here).
pub fn apply_tags(vault_root: &Path, notes: &[Note], config: &TagsConfig) -> eyre::Result<Vec<String>> {
    log::debug!(
        "tags::apply_tags: vault_root={} notes={}",
        vault_root.display(),
        notes.len()
    );
    let mut written = Vec::new();

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

        // Deduplicate after normalization, PRESERVING the user's first-seen
        // order. The old sort+dedup reordered the whole tag list on any fix.
        let before_dedup = new_tags.len();
        let mut seen = std::collections::HashSet::new();
        new_tags.retain(|t| seen.insert(t.clone()));
        if new_tags.len() != before_dedup {
            changed = true;
        }

        if changed {
            let abs_path = vault_root.join(&note.path);
            // Per-note errors WARN and skip rather than `?`-aborting the whole
            // run: a note deleted between scan and apply is routine on a
            // Syncthing'd vault.
            let content = match std::fs::read_to_string(&abs_path) {
                Ok(c) => c,
                Err(e) => {
                    log::warn!("skipping tag fix for {}: {e}", note.path.display());
                    continue;
                }
            };

            if let Some(new_content) = replace_tags_in_frontmatter(&content, &new_tags) {
                if let Err(e) = vault::note::write_atomic(&abs_path, new_content.as_bytes()) {
                    log::warn!("skipping tag fix for {}: {e}", note.path.display());
                    continue;
                }
                log::info!("updated tags: {}", note.path.display());
                written.push(note.path.to_string_lossy().to_string());
            }
        }
    }

    log::debug!("tags::apply_tags: written={}", written.len());
    Ok(written)
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
mod tests;
