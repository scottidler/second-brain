use regex::Regex;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use unicode_normalization::UnicodeNormalization;

use crate::config::NamingConfig;
use crate::report::{Fix, Report, Severity, Violation};
use crate::vault::Note;

/// Convert a filename to lowercase-hyphenated slug.
///
/// ASCII-folds accented Latin characters (`ü` -> `u`) so the suggestion agrees
/// with [`is_valid_slug`], which has always required ASCII. Before folding, a
/// name like `tobi-lütke-….md` was BOTH a violation and its own suggested fix:
/// the fixer proposed the byte-identical name, the executor saw the destination
/// already existed, and every daemon cycle logged a "would clobber" skip that
/// could never converge. Folding is the same one `vault::hygiene::sanitize_slug`
/// applies at ingest, so producer and validator agree on one alphabet.
pub fn to_slug(filename: &str) -> String {
    let stem = filename.strip_suffix(".md").unwrap_or(filename);
    let folded: String = stem
        .nfd()
        .filter(|c| !unicode_normalization::char::is_combining_mark(*c))
        .collect();

    let slug: String = folded
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else if c == ' ' || c == '_' || c == '-' {
                '-'
            } else {
                // Drop everything else: punctuation, and any non-ASCII char
                // folding could not reduce to ASCII.
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
pub fn apply_naming(vault_root: &Path, notes: &[Note], config: &NamingConfig) -> eyre::Result<NamingApplied> {
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
        return Ok(NamingApplied::default());
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

        // A no-op rename is not a clobber. Reaching the guard below with
        // `from == to` is how the Unicode-slug loop reported a conflict against
        // the very file it was renaming; short-circuit so a future divergence
        // between `to_slug` and `is_valid_slug` degrades to silence, not to a
        // warning every cycle forever.
        if abs_from == abs_to {
            log::debug!("naming: {} already matches its slug; nothing to rename", from.display());
            continue;
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
    written.extend(relinked.rewritten);
    log::debug!(
        "naming::apply_naming: renamed={} written={} unreadable={}",
        applied.len(),
        written.len(),
        relinked.unreadable.len()
    );
    Ok(NamingApplied {
        written,
        unreadable: relinked.unreadable,
    })
}

/// What `apply_naming` did: the paths it wrote, and the notes it could not
/// read to relink (their wikilinks to the renamed files are now stale).
#[derive(Debug, Default)]
pub struct NamingApplied {
    pub written: Vec<String>,
    pub unreadable: Vec<PathBuf>,
}

/// Result of a batch wikilink rewrite. `unreadable` notes were skipped, not
/// failed: erroring mid-loop would leave the rewrite half done after the
/// renames already landed, so the caller reports them instead.
#[derive(Debug, Default)]
pub(crate) struct Relinked {
    pub rewritten: Vec<String>,
    pub unreadable: Vec<PathBuf>,
}

/// Update wikilinks in all vault files for a batch of renames.
/// Single pass through all files. THE shared wikilink-rewrite for renames —
/// case-insensitive, handles `[[link]]` and `[[link|alias]]`, skips renamed
/// files, writes atomically. classify and migrate both delegate here (a consolidation
/// that replaced two weaker copies).
///
/// Returns the paths of the notes it actually rewrote (real byte changes
/// only) and the notes it could not read. Each unreadable note WARNs and is
/// collected for the caller to report.
pub(crate) fn update_wikilinks_batch(
    vault_root: &Path,
    notes: &[Note],
    renames: &[(PathBuf, PathBuf)],
) -> eyre::Result<Relinked> {
    if renames.is_empty() {
        return Ok(Relinked::default());
    }

    // One Resolver over the OLD paths: a link is rewritten only when it
    // resolves to a renamed note by Obsidian's rules, so `[[otherdir/x]]`
    // is left alone when `notes/x.md` is renamed.
    let from_paths: Vec<String> = renames
        .iter()
        .map(|(from, _)| from.to_string_lossy().into_owned())
        .collect();
    let resolver = vault::wikilink::Resolver::new(from_paths.iter().cloned());
    let new_stems: HashMap<&str, String> = from_paths
        .iter()
        .zip(renames)
        .filter_map(|(from, (_, to))| Some((from.as_str(), to.file_stem()?.to_str()?.to_string())))
        .collect();

    let mut out = Relinked::default();
    for note in notes {
        let abs_path = vault_root.join(&note.path);
        // Skip files that were renamed (they no longer exist at old path)
        if renames.iter().any(|(from, _)| *from == note.path) {
            continue;
        }

        let content = match std::fs::read_to_string(&abs_path) {
            Ok(c) => c,
            Err(e) => {
                log::warn!(
                    "naming: cannot read {} to update wikilinks after a rename: {e}",
                    abs_path.display()
                );
                out.unreadable.push(note.path.clone());
                continue;
            }
        };

        let new_content = relink(&content, &resolver, &new_stems);

        if new_content != content {
            vault::note::write_atomic(&abs_path, new_content.as_bytes())?;
            log::info!("updated wikilinks: {}", note.path.display());
            out.rewritten.push(note.path.to_string_lossy().to_string());
        }
    }

    Ok(out)
}

/// Every wikilink in a note file: the frontmatter's (hub-style
/// `related: "[[x]]"` values) and the body's. Frontmatter lines are parsed one
/// at a time with their YAML indent stripped, so an indented YAML value is
/// not mistaken for an indented code line.
fn file_links(content: &str) -> Vec<vault::wikilink::WikiLink<'_>> {
    match vault::frontmatter::split_raw(content) {
        Some((yaml, body)) => yaml
            .lines()
            .flat_map(|line| vault::wikilink::parse(line.trim_start()))
            .chain(vault::wikilink::parse(body))
            .collect(),
        None => vault::wikilink::parse(content).collect(),
    }
}

/// Byte offset of `inner` within `outer`; `inner` must be a subslice of it.
fn offset_in(outer: &str, inner: &str) -> usize {
    inner.as_ptr() as usize - outer.as_ptr() as usize
}

/// Rewrite every link that resolves to a renamed note by replacing only the
/// file stem inside its target, so the folder prefix, `.md`, `#heading`,
/// `#^block`, `|alias`, and `!` embed all survive byte for byte. Links in
/// code are not links and are left alone.
fn relink(content: &str, resolver: &vault::wikilink::Resolver, new_stems: &HashMap<&str, String>) -> String {
    let mut out = String::with_capacity(content.len());
    let mut last = 0;
    for link in file_links(content) {
        let Some(new_stem) = resolver.resolve(link.target).find_map(|from| new_stems.get(from)) else {
            continue;
        };
        let old_stem = vault::wikilink::file_stem(link.target);
        let start = offset_in(content, old_stem);
        out.push_str(&content[last..start]);
        out.push_str(new_stem);
        last = start + old_stem.len();
    }
    out.push_str(&content[last..]);
    out
}

#[cfg(test)]
mod tests;
