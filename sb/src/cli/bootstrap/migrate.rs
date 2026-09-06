//! Migration from legacy ~/.config/{borg,cortex,obsidian-cortex,oracle,second-brain}
//! into the unified ~/.config/sb/ layout.
//!
//! Invariants:
//! - Never deletes a legacy file at migration time. Cleanup is a separate,
//!   explicit step: `sb bootstrap --prune-legacy-config [--apply]`
//!   (`prune_legacy` below), fail-closed on anything it does not recognize.
//! - Idempotent: a marker file `.migrated-to-sb` is dropped in each migrated
//!   legacy directory so a re-run noops.
//! - Conflict-safe: if the new path exists and content differs from the legacy
//!   source, refuse and surface a diff hint. The user resolves manually.
//! - YAML-aware: after copying, any value matching a legacy path literal inside
//!   the yaml is rewritten to the new sb/ location. Users who customised these
//!   paths to point somewhere off-tree are left alone.

use eyre::{Context, Result};
use std::path::{Path, PathBuf};

const MARKER: &str = ".migrated-to-sb";

const LEGACY_DIRS: &[&str] = &["borg", "cortex", "obsidian-cortex", "oracle", "second-brain"];

#[derive(Debug, Default)]
pub struct Report {
    pub lines: Vec<String>,
    pub had_conflicts: bool,
}

/// Anything in `~/.config/{borg,cortex,obsidian-cortex,oracle,second-brain}` looks
/// like a legacy layout. We do not check for specific files - the directories
/// alone are evidence.
pub fn legacy_detected() -> bool {
    let Some(config_root) = vault::paths::xdg_config_dir() else {
        return false;
    };
    LEGACY_DIRS.iter().any(|name| {
        let dir = config_root.join(name);
        // If the marker is present we have already migrated this dir; skip it
        // for detection purposes.
        if dir.join(MARKER).exists() {
            return false;
        }
        dir.exists() && has_relevant_content(&dir)
    })
}

fn has_relevant_content(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() {
            let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if name.ends_with(".yml") || name.ends_with(".yaml") {
                return true;
            }
        } else if path.is_dir() && path.file_name().and_then(|s| s.to_str()) == Some("patterns") {
            return true;
        }
    }
    false
}

pub fn migrate_legacy_layout() -> Result<Report> {
    let Some(config_root) = vault::paths::xdg_config_dir() else {
        eyre::bail!("xdg_config_dir() returned None");
    };
    let sb_root = vault::paths::config_root();
    std::fs::create_dir_all(&sb_root).with_context(|| format!("create {}", sb_root.display()))?;

    let mut report = Report::default();

    // Map of legacy_path -> new_path for individual files
    let plans: Vec<(PathBuf, PathBuf)> = vec![
        (config_root.join("borg").join("borg.yml"), vault::paths::borg_config()),
        (
            config_root.join("cortex").join("cortex.yml"),
            vault::paths::cortex_config(),
        ),
        (
            config_root.join("obsidian-cortex").join("obsidian-cortex.yml"),
            vault::paths::cortex_config(),
        ),
        (
            config_root.join("oracle").join("oracle.yml"),
            vault::paths::oracle_config(),
        ),
        (
            config_root.join("second-brain").join("canonical-tags.yml"),
            vault::paths::canonical_tags(),
        ),
        (
            config_root.join("second-brain").join("tag-mapping.yml"),
            vault::paths::tag_mapping(),
        ),
        (
            config_root.join("second-brain").join("tag-proposals.yml"),
            vault::paths::tag_proposals(),
        ),
    ];

    for (src, dst) in &plans {
        copy_file_with_rewrite(src, dst, &mut report)?;
    }

    let patterns_src = config_root.join("borg").join("patterns");
    let patterns_dst = vault::paths::patterns_dir();
    copy_patterns_dir(&patterns_src, &patterns_dst, &mut report)?;

    // Drop markers in the legacy directories so subsequent runs noop.
    for legacy_dir in LEGACY_DIRS {
        let dir = config_root.join(legacy_dir);
        if !dir.exists() {
            continue;
        }
        let marker = dir.join(MARKER);
        if !marker.exists() {
            // best-effort; if we can't write the marker, the next run just retries
            let _ = std::fs::write(&marker, b"sb bootstrap migrated this directory\n");
        }
    }

    Ok(report)
}

/// Basenames `migrate_legacy_layout`'s `plans` array copies out of a legacy
/// directory (the seven file targets above), regardless of which legacy
/// directory happened to carry them. A file by one of these names sitting
/// in a legacy dir is known migration residue, not a stranger.
const KNOWN_BASENAMES: &[&str] = &[
    "borg.yml",
    "cortex.yml",
    "obsidian-cortex.yml",
    "oracle.yml",
    "canonical-tags.yml",
    "tag-mapping.yml",
    "tag-proposals.yml",
];

/// Preview (default) or apply (`apply = true`) deletion of legacy config
/// directories that have already been migrated into `~/.config/sb/`.
///
/// Fail-closed per directory: a directory is only ever a delete candidate if
/// it carries the `.migrated-to-sb` marker (proof `migrate_legacy_layout` ran
/// against it) AND every file inside it is one of the known migration
/// artifacts (`KNOWN_BASENAMES`, a `patterns/**/*.md` fabric pattern, or the
/// marker itself). Any other file refuses the whole directory and names the
/// stranger - never a partial delete. Deletion goes through
/// `borg::rkvr::remove`, which archives via `rkvr rmrf` when installed
/// (recoverable) or falls back to a WARN + plain removal when not; this
/// function never calls `remove_dir_all` itself.
pub fn prune_legacy(apply: bool) -> Result<Report> {
    let Some(config_root) = vault::paths::xdg_config_dir() else {
        eyre::bail!("xdg_config_dir() returned None");
    };

    let mut report = Report::default();

    for legacy_dir in LEGACY_DIRS {
        let dir = config_root.join(legacy_dir);
        if !dir.exists() {
            continue;
        }

        if !dir.join(MARKER).exists() {
            report.had_conflicts = true;
            report.lines.push(format!(
                "refused: {} has no {MARKER} marker (never migrated; leave it alone)",
                dir.display()
            ));
            continue;
        }

        let files = list_files_recursive(&dir)?;
        let strangers: Vec<String> = files
            .iter()
            .filter(|f| !is_known_file(&dir, f))
            .map(|f| f.strip_prefix(&dir).unwrap_or(f).display().to_string())
            .collect();
        if !strangers.is_empty() {
            report.had_conflicts = true;
            report.lines.push(format!(
                "refused: {} contains unknown file(s): {}",
                dir.display(),
                strangers.join(", ")
            ));
            continue;
        }

        if apply {
            borg::rkvr::remove(std::slice::from_ref(&dir)).with_context(|| format!("remove {}", dir.display()))?;
            report.lines.push(format!("removed: {}", dir.display()));
        } else {
            report.lines.push(format!("would remove: {}", dir.display()));
        }
    }

    Ok(report)
}

/// Every file (not directory) under `dir`, recursively.
fn list_files_recursive(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        for entry in std::fs::read_dir(&current).with_context(|| format!("read {}", current.display()))? {
            let entry = entry.with_context(|| format!("read entry in {}", current.display()))?;
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                out.push(path);
            }
        }
    }
    Ok(out)
}

/// True if `file` (an absolute path under `dir`) is a recognized migration
/// artifact: the marker, a top-level file matching `KNOWN_BASENAMES`, or any
/// `.md` file under a `patterns/` subdirectory (borg's fabric patterns).
fn is_known_file(dir: &Path, file: &Path) -> bool {
    let Ok(rel) = file.strip_prefix(dir) else {
        return false;
    };
    if rel == Path::new(MARKER) {
        return true;
    }
    let mut parts = rel.components();
    let Some(first) = parts.next() else {
        return false;
    };
    if first.as_os_str() == "patterns" {
        return rel.extension().and_then(|e| e.to_str()) == Some("md");
    }
    // Anything else must be a top-level file (exactly one path component)
    // whose basename is one of the known migration targets.
    if parts.next().is_none()
        && let Some(name) = rel.to_str()
    {
        return KNOWN_BASENAMES.contains(&name);
    }
    false
}

fn copy_file_with_rewrite(src: &Path, dst: &Path, report: &mut Report) -> Result<()> {
    if !src.exists() {
        return Ok(());
    }
    let src_content = std::fs::read(src).with_context(|| format!("read {}", src.display()))?;
    let rewritten = rewrite_legacy_paths_in_yaml(src, &src_content);

    if dst.exists() {
        let dst_content = std::fs::read(dst).with_context(|| format!("read {}", dst.display()))?;
        if dst_content == rewritten {
            report
                .lines
                .push(format!("noop: {} already matches {}", dst.display(), src.display()));
        } else {
            report.had_conflicts = true;
            report.lines.push(format!(
                "conflict: {} differs from migrated form of {}; resolve manually before rerunning",
                dst.display(),
                src.display()
            ));
        }
        return Ok(());
    }

    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    std::fs::write(dst, &rewritten).with_context(|| format!("write {}", dst.display()))?;
    report
        .lines
        .push(format!("migrated: {} -> {}", src.display(), dst.display()));
    Ok(())
}

fn copy_patterns_dir(src: &Path, dst: &Path, report: &mut Report) -> Result<()> {
    if !src.exists() {
        return Ok(());
    }
    if dst.exists() {
        // Compare every file; if anything differs, conflict. If all match, noop.
        let mut all_match = true;
        for entry in std::fs::read_dir(src).with_context(|| format!("read {}", src.display()))? {
            let entry = entry?;
            let name = entry.file_name();
            let dst_file = dst.join(&name);
            if !dst_file.exists() {
                all_match = false;
                break;
            }
            let a = std::fs::read(entry.path())?;
            let b = std::fs::read(&dst_file)?;
            if a != b {
                all_match = false;
                break;
            }
        }
        if all_match {
            report.lines.push(format!(
                "noop: patterns dir {} already matches {}",
                dst.display(),
                src.display()
            ));
        } else {
            report.had_conflicts = true;
            report.lines.push(format!(
                "conflict: patterns dir {} differs from {}; resolve manually before rerunning",
                dst.display(),
                src.display()
            ));
        }
        return Ok(());
    }
    std::fs::create_dir_all(dst).with_context(|| format!("create {}", dst.display()))?;
    for entry in std::fs::read_dir(src).with_context(|| format!("read {}", src.display()))? {
        let entry = entry?;
        let dst_file = dst.join(entry.file_name());
        std::fs::copy(entry.path(), &dst_file)
            .with_context(|| format!("copy {} -> {}", entry.path().display(), dst_file.display()))?;
    }
    report
        .lines
        .push(format!("migrated patterns dir: {} -> {}", src.display(), dst.display()));
    Ok(())
}

/// Rewrite legacy `~/.config/{borg,second-brain,obsidian-cortex,oracle,cortex}` path
/// substrings inside YAML to the new `~/.config/sb/` location. Operates on raw
/// bytes so user-edited comments and anchors are preserved verbatim. Only literal
/// matches are rewritten - a custom user path stays untouched.
fn rewrite_legacy_paths_in_yaml(src: &Path, content: &[u8]) -> Vec<u8> {
    let Ok(text) = std::str::from_utf8(content) else {
        return content.to_vec();
    };
    let mut text = text.to_string();
    let pairs: &[(&str, &str)] = &[
        (
            "~/.config/second-brain/canonical-tags.yml",
            "~/.config/sb/canonical-tags.yml",
        ),
        ("~/.config/second-brain/tag-mapping.yml", "~/.config/sb/tag-mapping.yml"),
        (
            "~/.config/second-brain/tag-proposals.yml",
            "~/.config/sb/tag-proposals.yml",
        ),
        ("~/.config/borg/patterns/", "~/.config/sb/patterns/"),
        ("~/.config/borg/patterns", "~/.config/sb/patterns"),
    ];
    for (from, to) in pairs {
        text = text.replace(from, to);
    }
    log::debug!(
        "rewrite_legacy_paths_in_yaml: src={} bytes_in={} bytes_out={}",
        src.display(),
        content.len(),
        text.len()
    );
    text.into_bytes()
}

#[cfg(test)]
mod tests;
