use regex::Regex;
use std::path::{Path, PathBuf};

use crate::config::NamingConfig;
use crate::report::{Fix, Report, Severity, Violation};
use crate::vault::Note;

/// Convert a filename to lowercase-hyphenated slug.
pub fn to_slug(filename: &str) -> String {
    let stem = filename.strip_suffix(".md").unwrap_or(filename);

    let slug: String = stem
        .chars()
        .map(|c| {
            if c.is_alphanumeric() {
                c.to_lowercase().next().unwrap_or(c)
            } else if c == ' ' || c == '_' || c == '-' {
                '-'
            } else {
                // Drop non-alphanumeric, non-separator chars
                '\0'
            }
        })
        .filter(|c| *c != '\0')
        .collect();

    // Collapse multiple hyphens
    let mut result = String::with_capacity(slug.len());
    let mut prev_hyphen = false;
    for c in slug.chars() {
        if c == '-' {
            if !prev_hyphen && !result.is_empty() {
                result.push('-');
            }
            prev_hyphen = true;
        } else {
            result.push(c);
            prev_hyphen = false;
        }
    }

    // Trim trailing hyphen
    if result.ends_with('-') {
        result.pop();
    }

    result
}

/// Check if a filename matches lowercase-hyphenated convention.
fn is_valid_slug(stem: &str) -> bool {
    if stem.is_empty() {
        return false;
    }
    stem.chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !stem.starts_with('-')
        && !stem.ends_with('-')
        && !stem.contains("--")
}

/// Check if a path is exempt from naming rules.
fn is_exempt(path: &Path, exempt_patterns: &[String]) -> bool {
    let path_str = path.to_string_lossy();
    for pattern in exempt_patterns {
        if let Ok(re) = Regex::new(pattern)
            && re.is_match(&path_str)
        {
            return true;
        }
    }
    false
}

/// Run naming lint on all notes. Returns violations.
pub fn lint_naming(notes: &[Note], config: &NamingConfig) -> Report {
    let mut report = Report::default();

    for note in notes {
        if is_exempt(&note.path, &config.exempt_patterns) {
            continue;
        }

        let filename = match note.path.file_name().and_then(|f| f.to_str()) {
            Some(f) => f,
            None => continue,
        };

        let stem = filename.strip_suffix(".md").unwrap_or(filename);

        // Check lowercase-hyphenated
        if !is_valid_slug(stem) {
            let suggested = to_slug(filename);
            let new_filename = format!("{suggested}.md");
            let new_path = note
                .path
                .parent()
                .map(|p| p.join(&new_filename))
                .unwrap_or_else(|| PathBuf::from(&new_filename));

            report.add(Violation {
                path: note.path.clone(),
                rule: "naming.lowercase-hyphenated".to_string(),
                severity: Severity::Error,
                message: format!("filename '{stem}' is not lowercase-hyphenated, suggest '{suggested}'"),
                fix: Some(Fix::RenameFile {
                    from: note.path.clone(),
                    to: new_path,
                }),
            });
        }

        // Check max length
        if stem.len() > config.max_length as usize {
            report.add(Violation {
                path: note.path.clone(),
                rule: "naming.max-length".to_string(),
                severity: Severity::Warning,
                message: format!("filename length {} exceeds max {}", stem.len(), config.max_length),
                fix: None,
            });
        }
    }

    log::info!("naming lint complete: {} violation(s)", report.violations.len());
    report
}

/// Apply naming fixes: rename files and update wikilinks.
///
/// Returns the real, byte-changed paths this call actually wrote: the NEW
/// path of every rename that landed, plus every other note whose wikilinks
/// were rewritten to follow a rename. This is the seam the daemon's
/// oscillation fingerprint (`LintApplyReport.written_paths`) draws from -
/// callers must never substitute the lint report's violation paths, which
/// include renames skipped as would-clobber.
pub fn apply_naming(vault_root: &Path, notes: &[Note], config: &NamingConfig) -> eyre::Result<Vec<String>> {
    log::debug!(
        "naming::apply_naming: vault_root={} notes={}",
        vault_root.display(),
        notes.len()
    );
    let report = lint_naming(notes, config);
    let mut renames: Vec<(PathBuf, PathBuf)> = Vec::new();

    // Collect all renames first
    for violation in &report.violations {
        if let Some(Fix::RenameFile { from, to }) = &violation.fix {
            renames.push((from.clone(), to.clone()));
        }
    }

    if renames.is_empty() {
        return Ok(Vec::new());
    }

    // Execute renames. Skip (never clobber) when the destination already
    // exists: on a Syncthing'd vault a real file could occupy the normalized
    // name, and `fs::rename` would silently destroy it. Only actually-applied
    // renames feed the wikilink rewrite and the returned set.
    let mut applied = Vec::new();
    for (from, to) in &renames {
        let abs_from = vault_root.join(from);
        let abs_to = vault_root.join(to);

        if let Some(parent) = abs_to.parent() {
            std::fs::create_dir_all(parent)?;
        }

        if abs_to.exists() {
            log::warn!(
                "skipping rename {} -> {}: destination already exists (would clobber)",
                from.display(),
                to.display()
            );
            continue;
        }

        std::fs::rename(&abs_from, &abs_to)?;
        log::info!("renamed file: {} -> {}", from.display(), to.display());
        applied.push((from.clone(), to.clone()));
    }

    // Batch update all wikilinks across the vault for the renames that landed.
    let relinked = update_wikilinks_batch(vault_root, notes, &applied)?;

    let mut written: Vec<String> = applied.iter().map(|(_, to)| to.to_string_lossy().to_string()).collect();
    written.extend(relinked);
    log::debug!(
        "naming::apply_naming: renamed={} relinked={}",
        applied.len(),
        written.len()
    );
    Ok(written)
}

/// Update wikilinks in all vault files for a batch of renames.
/// Single pass through all files. THE shared wikilink-rewrite for renames —
/// case-insensitive, handles `[[link]]` and `[[link|alias]]`, skips renamed
/// files, writes atomically. classify and migrate both delegate here (Phase 9
/// consolidation; replaced two weaker copies).
///
/// Returns the paths of the notes it actually rewrote (real byte changes
/// only) - the caller folds this into its own written-paths return.
pub(crate) fn update_wikilinks_batch(
    vault_root: &Path,
    notes: &[Note],
    renames: &[(PathBuf, PathBuf)],
) -> eyre::Result<Vec<String>> {
    if renames.is_empty() {
        return Ok(Vec::new());
    }

    // Build a map of old stem -> new stem (case-insensitive matching)
    let rename_map: Vec<(String, String)> = renames
        .iter()
        .filter_map(|(from, to)| {
            let old_stem = from.file_stem()?.to_str()?.to_string();
            let new_stem = to.file_stem()?.to_str()?.to_string();
            Some((old_stem, new_stem))
        })
        .collect();

    let mut written = Vec::new();
    for note in notes {
        let abs_path = vault_root.join(&note.path);
        // Skip files that were renamed (they no longer exist at old path)
        if renames.iter().any(|(from, _)| *from == note.path) {
            continue;
        }

        let content = match std::fs::read_to_string(&abs_path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let mut new_content = content.clone();

        for (old_stem, new_stem) in &rename_map {
            // Wikilinks are case-insensitive in Obsidian, match all case variants
            let pattern = format!(r"\[\[{}\]\]", regex::escape(old_stem));
            if let Ok(re) = Regex::new(&format!("(?i){pattern}")) {
                new_content = re.replace_all(&new_content, format!("[[{new_stem}]]")).to_string();
            }

            // Also handle [[link|display text]] format
            let pipe_pattern = format!(r"\[\[{}\|", regex::escape(old_stem));
            if let Ok(re) = Regex::new(&format!("(?i){pipe_pattern}")) {
                new_content = re.replace_all(&new_content, format!("[[{new_stem}|")).to_string();
            }
        }

        if new_content != content {
            vault::note::write_atomic(&abs_path, new_content.as_bytes())?;
            log::info!("updated wikilinks: {}", note.path.display());
            written.push(note.path.to_string_lossy().to_string());
        }
    }

    Ok(written)
}

#[cfg(test)]
mod tests;
