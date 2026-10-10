use regex::Regex;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use unicode_normalization::UnicodeNormalization;

use crate::config::NamingConfig;
use crate::report::{Fix, Report, Severity, Violation};
use crate::shutdown::StopHandle;
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
///
/// `stop` is checked before every rename. The relink of renames already made
/// is mandatory cleanup: it runs when the loop stops, and when a rename or
/// `create_dir_all` fails mid-loop (the error is returned after it).
pub fn apply_naming(
    vault_root: &Path,
    notes: &[Note],
    config: &NamingConfig,
    stop: &StopHandle,
) -> eyre::Result<NamingApplied> {
    apply_naming_with(vault_root, notes, config, stop, |from, to| std::fs::rename(from, to))
}

/// `apply_naming` with the filesystem rename injected, so tests can stop or
/// fail the loop at a chosen rename.
fn apply_naming_with<R>(
    vault_root: &Path,
    notes: &[Note],
    config: &NamingConfig,
    stop: &StopHandle,
    rename: R,
) -> eyre::Result<NamingApplied>
where
    R: FnMut(&Path, &Path) -> std::io::Result<()>,
{
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

    let mut applied = Vec::new();
    let executed = execute_renames(vault_root, &renames, stop, rename, &mut applied);

    // Batch update all wikilinks across the vault for the renames that landed,
    // whatever ended the loop: a stop or an error after a completed rename
    // must not leave links pointing at the old path.
    let relinked = update_wikilinks_batch(vault_root, notes, &applied);
    let end = match executed {
        Ok(end) => end,
        Err(e) => {
            match &relinked {
                Ok(r) => log::warn!(
                    "naming: rename loop failed after {} completed rename(s); relinked {} note(s) before returning the error",
                    applied.len(),
                    r.rewritten.len()
                ),
                Err(re) => log::error!(
                    "naming: rename loop failed after {} completed rename(s), and their relink failed too: {re:#}",
                    applied.len()
                ),
            }
            return Err(e);
        }
    };
    let relinked = relinked?;

    let mut written: Vec<String> = applied.iter().map(|(_, to)| to.to_string_lossy().to_string()).collect();
    written.extend(relinked.rewritten);
    log::debug!(
        "naming::apply_naming: renamed={} written={} unreadable={} stopped={}",
        applied.len(),
        written.len(),
        relinked.unreadable.len(),
        end == RenameLoop::Stopped
    );
    Ok(NamingApplied {
        written,
        unreadable: relinked.unreadable,
        stopped: end == RenameLoop::Stopped,
    })
}

/// How the rename loop ended when it did not error.
#[derive(Debug, PartialEq, Eq)]
enum RenameLoop {
    Complete,
    Stopped,
}

/// Execute renames in order, pushing each one that lands onto `applied`.
/// Skip (never clobber) when the destination already exists: on a
/// Syncthing'd vault a real file could occupy the normalized name, and
/// `fs::rename` would silently destroy it. Only actually-applied renames feed
/// the wikilink rewrite and the returned set.
fn execute_renames<R>(
    vault_root: &Path,
    renames: &[(PathBuf, PathBuf)],
    stop: &StopHandle,
    mut rename: R,
    applied: &mut Vec<(PathBuf, PathBuf)>,
) -> eyre::Result<RenameLoop>
where
    R: FnMut(&Path, &Path) -> std::io::Result<()>,
{
    for (from, to) in renames {
        if stop.is_stopped() {
            log::info!(
                "naming: stopped at a check point before {}; relinking {} rename(s) already made",
                from.display(),
                applied.len()
            );
            return Ok(RenameLoop::Stopped);
        }
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

        rename(&abs_from, &abs_to)?;
        log::info!("renamed file: {} -> {}", from.display(), to.display());
        applied.push((from.clone(), to.clone()));
    }
    Ok(RenameLoop::Complete)
}

/// What `apply_naming` did: the paths it wrote, the notes it could not read
/// to relink (their wikilinks to the renamed files are now stale), and
/// whether a stop cut the loop short.
#[derive(Debug, Default)]
pub struct NamingApplied {
    pub written: Vec<String>,
    pub unreadable: Vec<PathBuf>,
    /// The loop stopped at a check point; renames after it were not tried.
    pub stopped: bool,
}

/// Result of a batch wikilink rewrite. `unreadable` notes were skipped, not
/// failed: erroring mid-loop would leave the rewrite half done after the
/// renames already landed, so the caller reports them instead.
#[derive(Debug, Default)]
pub(crate) struct Relinked {
    pub rewritten: Vec<String>,
    pub unreadable: Vec<PathBuf>,
}

/// Update wikilinks in all vault files for a batch of renames and moves.
/// Single pass through all files. THE shared wikilink-rewrite for renames —
/// case-insensitive, handles `[[link]]` and `[[link|alias]]`, skips renamed
/// files, writes atomically. classify and migrate both delegate here (a consolidation
/// that replaced two weaker copies). A rename (same folder, new stem) rewrites
/// the stem; a move to another folder also rewrites a path-qualified target
/// (`[[inbox/x]]`) to the new path, and leaves a bare `[[x]]` alone when the
/// stem is unchanged.
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
    let destinations: HashMap<&str, Destination> = from_paths
        .iter()
        .zip(renames)
        .filter_map(|(from, (old, new))| Some((from.as_str(), Destination::new(old, new)?)))
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

        let new_content = relink(&content, &resolver, &destinations);

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

/// Where a renamed or moved note now lives, in the two forms a link target
/// can name it by.
struct Destination {
    /// New file stem, case kept: what a bare or same-folder link names.
    stem: String,
    /// New vault-relative path, `/`-separated, `.md` stripped: what a
    /// path-qualified link names once the note changed folders.
    path: String,
    /// The note left its folder, so a path-qualified link's folder is stale.
    changed_folder: bool,
}

impl Destination {
    fn new(from: &Path, to: &Path) -> Option<Self> {
        let stem = to.file_stem()?.to_str()?.to_string();
        let path = to.with_extension("").to_str()?.to_string();
        Some(Self {
            stem,
            path,
            changed_folder: from.parent() != to.parent(),
        })
    }
}

/// Rewrite every link that resolves to a renamed or moved note. A bare
/// target, or any target of a same-folder rename, gets only its file stem
/// replaced, so the folder prefix, `.md`, `#heading`, `#^block`, `|alias`, and
/// `!` embed all survive byte for byte. A path-qualified target of a note that
/// changed folders gets its whole path replaced (its `.md` and everything
/// after the target still survive), or it would keep pointing at the old
/// folder. Links in code are not links and are left alone.
fn relink(content: &str, resolver: &vault::wikilink::Resolver, destinations: &HashMap<&str, Destination>) -> String {
    let mut out = String::with_capacity(content.len());
    let mut last = 0;
    for link in file_links(content) {
        let Some(dest) = resolver.resolve(link.target).find_map(|from| destinations.get(from)) else {
            continue;
        };
        let target = link.target.trim();
        let (old, new) = if dest.changed_folder && target.contains('/') {
            let stem = vault::wikilink::file_stem(target);
            let stem_end = offset_in(target, stem) + stem.len();
            (&target[..stem_end], dest.path.as_str())
        } else {
            (vault::wikilink::file_stem(link.target), dest.stem.as_str())
        };
        let start = offset_in(content, old);
        out.push_str(&content[last..start]);
        out.push_str(new);
        last = start + old.len();
    }
    out.push_str(&content[last..]);
    out
}

#[cfg(test)]
mod tests;
